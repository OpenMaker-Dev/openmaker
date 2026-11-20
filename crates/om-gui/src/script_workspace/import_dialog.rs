//! Import Records' import order (script-steps.md 11.3): the Import Field
//! Mapping dialog for a stored order, in the step's options. FileMaker reads
//! the data source when the order is specified, to list the source's
//! fields (Claris Help, "Import Records"); so does this dialog. The step's
//! first path that names an existing file (`scripting::files::existing`,
//! `file:` paths in this user's Documents; a path with a variable does not
//! resolve at design time) is parsed with the step's profile, as Import
//! Records parses it when it runs.
//!
//! Each source column gets a target field of the target table, or none, and
//! with "Update matching records in found set" may be a match key; then
//! the import action, "Add remaining data as new records", "Don't import
//! first record (contains field names)", "Perform auto-enter options while
//! importing" and, for a text source, the character set. OK answers the
//! import order (`84`) and the profile extension (`87`) as Import Records
//! with dialog writes them when it runs (`scripting::host::transfer`); the
//! other import flags, the per-field flags `f0` and the profile `86` stay as
//! stored. The controls and their order are the run-time dialog's; the
//! layout was not compared with FileMaker Pro's.

use std::sync::Arc;

use egui::{pos2, vec2, FontId, Rect, Sense, Stroke};
use om_data::host::{CalcSchema, FieldClass};
use om_transfer::import::{self, ImportRequest};
use om_transfer::spec::{import_flag, MapEntry};
use om_transfer::{
    FieldFlags, FileType, ImportAction, ImportCharset, ImportOptions, Key, Mapping, Profile,
    ProfileEx,
};

use super::pickers::{
    self, checkbox, dialog, left_label, popup_menu, radio, Picked, CONTROL_HEIGHT,
};
use crate::theme;

/// What OK answers: the import order, the profile extension (when one is
/// stored or the names row was set), and the source's profile when none
/// was stored (from the file's extension).
pub(crate) type Mapped = (ImportOptions, Option<ProfileEx>, Option<Profile>);

/// The largest source read at design time, where only its fields are
/// needed: the read and the parse stay short on the window's thread.
const DESIGN_BYTES: u64 = 16 << 20;

/// The profile of a source with none stored, by its file's extension: the
/// text and dBase types, whose profiles have no options (script-steps.md
/// 11.5). An Excel profile names a sheet and a file; how FileMaker fills
/// them for a new step is not established, so none is made.
fn profile_for(path: &std::path::Path) -> Option<Profile> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let file_type = match extension.as_str() {
        "tab" | "txt" => FileType::TabSeparated,
        "csv" => FileType::CommaSeparated,
        "mer" => FileType::Merge,
        "dbf" => FileType::DBase,
        _ => return None,
    };
    Some(Profile::new(file_type))
}

/// The step's import parts as stored, the source the dialog reads.
pub(crate) struct ImportParts {
    pub paths: Vec<String>,
    pub order: Option<ImportOptions>,
    pub profile: Option<Profile>,
    pub profile_ex: Option<ProfileEx>,
    pub field_flags: Option<FieldFlags>,
}

/// The import order a step gets when none is stored: add new records with
/// auto-enter, UTF-8, as Import Records assumes when it runs without one.
fn new_order(table: u64) -> ImportOptions {
    ImportOptions {
        table: Key::from_id(table),
        action: ImportAction::Add,
        flags: import_flag::AUTO_ENTER,
        charset: ImportCharset::Utf8,
        map: Vec::new(),
    }
}

/// The Import Field Mapping dialog of a step's import order.
pub(crate) struct ImportMapping {
    names: Option<Arc<CalcSchema>>,
    /// The source file, read once; parsed again when the character set
    /// changes.
    bytes: Vec<u8>,
    path: String,
    profile: Profile,
    /// Whether `profile` was made from the file's extension.
    inferred: bool,
    profile_ex: Option<ProfileEx>,
    field_flags: Option<FieldFlags>,
    columns: Vec<String>,
    /// The import order as edited; its map stays as stored, the answer
    /// changes only the entries of columns edited here.
    order: ImportOptions,
    /// The stored order (`None` for a new one).
    stored: Option<ImportOptions>,
    /// Per source column: the target field, or none.
    map: Vec<Option<u64>>,
    /// Per source column: a match key (with Update matching records).
    keys: Vec<bool>,
    /// Per source column: whether its target or key was changed here.
    touched: Vec<bool>,
    names_row: bool,
    /// The names row as the dialog opened.
    names_row_was: bool,
    /// Whether the source names its fields (merge, XML), so that an order
    /// arranged by matching names (`MATCH_NAMES`) maps by those names.
    named: bool,
    /// Why the source could not be parsed with the chosen character set.
    error: Option<String>,
}

impl ImportMapping {
    /// The dialog for `parts`, or why the source cannot be read.
    pub fn open(parts: ImportParts, names: Option<Arc<CalcSchema>>) -> Result<Self, String> {
        let documents = crate::scripting::files::Folders::of_this_user().documents;
        let path =
            crate::scripting::files::existing(&parts.paths, &documents, false).map_err(|_| {
                "Specify the data source first: the import order maps the fields of a file \
                 that exists on this computer."
                    .to_owned()
            })?;
        let inferred = parts.profile.is_none();
        let profile = match parts.profile {
            Some(profile) => profile,
            None => profile_for(&path).ok_or_else(|| {
                "OpenMaker takes a data source's type from its file's extension (.tab, .txt, \
                 .csv, .mer or .dbf) when the step stores none, so this source's import order \
                 cannot be specified here."
                    .to_owned()
            })?,
        };
        // An Excel 97-2004 source reads as Import Records reads it when it
        // runs (`om_transfer::import`'s BIFF8 reader).
        if matches!(
            profile.file_type,
            FileType::FileMaker | FileType::Folder | FileType::Odbc
        ) {
            return Err(format!(
                "OpenMaker does not read a {} data source, so its import order cannot be \
                 specified here.",
                profile.file_type.name()
            ));
        }
        let too_large = || {
            format!(
                "OpenMaker reads a data source of at most {} MB to list its fields here, and \
                 this one is larger.",
                DESIGN_BYTES >> 20
            )
        };
        let mut file = std::fs::File::open(&path)
            .map_err(|e| format!("The data source could not be read: {e}."))?;
        if file.metadata().is_ok_and(|m| m.len() > DESIGN_BYTES) {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(&mut file, DESIGN_BYTES + 1),
            &mut bytes,
        )
        .map_err(|e| format!("The data source could not be read: {e}."))?;
        if bytes.len() as u64 > DESIGN_BYTES {
            return Err(too_large());
        }
        let table = parts
            .order
            .as_ref()
            .and_then(|o| o.table.id())
            .or_else(|| {
                names
                    .as_ref()
                    .and_then(|n| n.occurrences_in_order().first().map(|o| o.id))
            })
            .unwrap_or(0);
        let stored = parts.order.clone();
        let order = parts.order.unwrap_or_else(|| new_order(table));
        let names_row = parts
            .profile_ex
            .as_ref()
            .map_or(profile.field_names != 0, |ex| ex.field_names_row >= 0);
        let mut dialog = Self {
            names,
            bytes,
            path: path.display().to_string(),
            profile,
            inferred,
            profile_ex: parts.profile_ex,
            field_flags: parts.field_flags,
            columns: Vec::new(),
            map: Vec::new(),
            keys: Vec::new(),
            touched: Vec::new(),
            names_row,
            names_row_was: names_row,
            named: false,
            error: None,
            order,
            stored,
        };
        dialog.read_columns()?;
        if dialog.columns.len() > usize::from(u16::MAX) {
            return Err(
                "The data source has more fields than an import order holds (65,535).".into(),
            );
        }
        if dialog.by_names() {
            // The fields the source's names match, as Import Records maps
            // them (om-transfer's import, `MATCH_NAMES`): a field's first
            // entry in the order says whether it is excluded or a key.
            let targets = dialog.targets();
            for (i, column) in dialog.columns.iter().enumerate() {
                let field = targets
                    .iter()
                    .find(|(_, name)| name.eq_ignore_ascii_case(column))
                    .map(|(id, _)| *id);
                let entry =
                    field.and_then(|id| dialog.order.map.iter().find(|e| e.field.id() == Some(id)));
                let excluded = entry.is_some_and(|e| {
                    matches!(e.mapping, Mapping::DontImport | Mapping::Unimportable)
                });
                dialog.map[i] = field.filter(|_| !excluded);
                dialog.keys[i] = entry.is_some_and(|e| e.mapping == Mapping::Match);
            }
        } else {
            dialog.map = (0..dialog.columns.len())
                .map(|i| {
                    let entry = dialog.order.map.get(i)?;
                    if !matches!(entry.mapping, Mapping::Import | Mapping::Match) {
                        return None;
                    }
                    entry.field.id()
                })
                .collect();
            dialog.keys = (0..dialog.columns.len())
                .map(|i| {
                    dialog
                        .order
                        .map
                        .get(i)
                        .is_some_and(|e| e.mapping == Mapping::Match)
                })
                .collect();
        }
        Ok(dialog)
    }

    /// The source's columns: their names, or the first record's values.
    fn read_columns(&mut self) -> Result<(), String> {
        let request = ImportRequest {
            options: self.order.clone(),
            profile: self.profile.clone(),
            profile_ex: self.profile_ex.clone(),
            field_flags: self.field_flags.clone(),
        };
        let source = import::parse(&request, &self.bytes).map_err(|e| {
            format!(
                "The data source could not be read as {} (error {}).",
                self.profile.file_type.name(),
                e.code
            )
        })?;
        self.named = source.names.is_some();
        self.columns = match source.names {
            Some(names) => names,
            None => source
                .rows
                .first()
                .map(|r| {
                    r.iter()
                        .map(|v| v.first().cloned().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default(),
        };
        self.map.resize(self.columns.len(), None);
        self.keys.resize(self.columns.len(), false);
        self.touched.resize(self.columns.len(), false);
        Ok(())
    }

    /// Whether the stored order maps this source by its field names.
    fn by_names(&self) -> bool {
        self.named && self.order.has(import_flag::MATCH_NAMES)
    }

    /// Choose the target table: every column's target is cleared, and
    /// counts as set here (none is taken back from the stored order).
    fn set_table(&mut self, table: u64) {
        if self.order.table.id() == Some(table) {
            return;
        }
        self.order.table = Key::from_id(table);
        self.map.iter_mut().for_each(|t| *t = None);
        self.keys.iter_mut().for_each(|k| *k = false);
        self.touched.iter_mut().for_each(|t| *t = true);
    }

    /// The target table's fields that take imported data, in creation
    /// order.
    fn targets(&self) -> Vec<(u64, String)> {
        let (Some(names), Some(table)) = (&self.names, self.order.table.id()) else {
            return Vec::new();
        };
        names
            .table_of(table)
            .map(|t| {
                t.fields
                    .values()
                    .filter(|f| {
                        f.class == FieldClass::Data
                            && f.value_type != om_data::calc::ValueType::Container
                    })
                    .map(|f| (f.id, f.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether OK can answer: a column mapped, and with Update matching
    /// records a mapped match key.
    fn complete(&self) -> bool {
        let mapped = |i: usize| self.map.get(i).copied().flatten().is_some();
        (0..self.columns.len()).any(mapped)
            && (self.order.action != ImportAction::UpdateMatching
                || (0..self.columns.len()).any(|i| mapped(i) && self.keys[i]))
    }

    /// The mapping kind of a mapped column under `action`.
    fn kind(&self, column: usize, action: ImportAction) -> Mapping {
        if action == ImportAction::UpdateMatching && self.keys[column] {
            Mapping::Match
        } else {
            Mapping::Import
        }
    }

    /// The import order, profile extension and profile as Import Records
    /// writes them after its dialog, changing only what was changed here:
    /// a column whose target or key was not touched keeps its stored entry
    /// (a Don't import entry's field key, an unimportable one), entries past
    /// the source's columns stay, and "matching names" (`MATCH_NAMES`) is
    /// cleared only when targets were set here. A new order gets an entry
    /// for every column.
    fn answer(&self) -> Mapped {
        let mut order = self.order.clone();
        let new = self.stored.is_none();
        let stored_table = self.stored.as_ref().map(|o| o.table.clone());
        let retargeted = stored_table.is_some_and(|t| t != order.table);
        let mapped = new || retargeted || self.touched.iter().any(|t| *t);
        // A name-matched order edited here becomes a custom one: every
        // column takes the target the names gave it, as shown.
        let all = new || retargeted || (mapped && self.by_names());
        let action_changed = self
            .stored
            .as_ref()
            .is_some_and(|o| o.action != order.action);
        if mapped || action_changed {
            let mut map = if retargeted {
                Vec::new()
            } else {
                order.map.clone()
            };
            while map.len() < self.columns.len() {
                map.push(MapEntry {
                    field: Key::none(),
                    mapping: Mapping::DontImport,
                });
            }
            for (column, entry) in map.iter_mut().enumerate().take(self.columns.len()) {
                if all || self.touched[column] {
                    *entry = match self.map[column] {
                        Some(id) => MapEntry {
                            field: Key::from_id(id),
                            mapping: self.kind(column, order.action),
                        },
                        None => MapEntry {
                            field: Key::none(),
                            mapping: Mapping::DontImport,
                        },
                    };
                } else if matches!(entry.mapping, Mapping::Import | Mapping::Match) {
                    entry.mapping = self.kind(column, order.action);
                }
            }
            order.map = map;
        }
        if mapped {
            order.flags &= !import_flag::MATCH_NAMES;
        }
        let names_row_at = |ex: Option<&ProfileEx>| {
            if self.names_row {
                ex.map_or(0, |ex| ex.field_names_row.max(0))
            } else {
                -1
            }
        };
        let ex = match &self.profile_ex {
            Some(stored) => Some(ProfileEx {
                field_names_row: if self.names_row == self.names_row_was {
                    stored.field_names_row
                } else {
                    names_row_at(Some(stored))
                },
                ..stored.clone()
            }),
            None if new || self.names_row != self.names_row_was => Some(ProfileEx {
                field_names_row: names_row_at(None),
                predefined: -1,
                delimiter: if self.profile.file_type == FileType::CommaSeparated {
                    ","
                } else {
                    "\t"
                }
                .into(),
            }),
            None => None,
        };
        (
            order,
            ex,
            (self.inferred && (new || mapped)).then(|| self.profile.clone()),
        )
    }

    fn set_flag(&mut self, flag: u8, on: bool) {
        if on {
            self.order.flags |= flag;
        } else {
            self.order.flags &= !flag;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<Mapped>> {
        let mut outcome = None;
        let complete = self.complete();
        let shown = dialog(
            ctx,
            "import_mapping",
            "Import Field Mapping",
            600.0,
            on_top,
            |ui| {
                let full = ui.available_width();
                let left = ui.cursor().left();
                let column_width = 240.0;
                let middle = 60.0;
                ui.add_space(10.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, 20.0), Sense::hover());
                left_label(
                    ui,
                    row,
                    &format!("Source: {}", self.path),
                    pickers::body(),
                    theme::CAPTION,
                );
                ui.add_space(4.0);
                // The target table.
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                left_label(
                    ui,
                    Rect::from_min_size(row.min, vec2(column_width + middle, row.height())),
                    "Source Fields",
                    FontId::new(theme::BODY, theme::semibold()),
                    theme::TEXT,
                );
                let occurrences: Vec<(u64, String)> = self
                    .names
                    .as_ref()
                    .map(|n| {
                        n.occurrences_in_order()
                            .into_iter()
                            .map(|o| (o.id, o.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let table = self.order.table.id();
                let current = occurrences
                    .iter()
                    .find(|(id, _)| Some(*id) == table)
                    .map_or_else(String::new, |(_, n)| n.clone());
                let items: Vec<String> = occurrences.iter().map(|(_, n)| n.clone()).collect();
                let popup = Rect::from_min_size(
                    pos2(left + column_width + middle, row.top()),
                    vec2(column_width, CONTROL_HEIGHT),
                );
                if let Some(i) = popup_menu(ui, popup, "Target", &current, &items, true) {
                    self.set_table(occurrences[i].0);
                }
                ui.add_space(6.0);
                // One row per source column: its name, the arrow, the target.
                let targets = self.targets();
                let mut items = vec!["Don't import".to_owned()];
                items.extend(targets.iter().map(|(_, n)| n.clone()));
                let matching = self.order.action == ImportAction::UpdateMatching;
                egui::ScrollArea::vertical()
                    .id_salt("import_mapping_rows")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for i in 0..self.columns.len() {
                            let (row, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
                            let name = if self.columns[i].is_empty() {
                                format!("({})", i + 1)
                            } else {
                                self.columns[i].replace(['\r', '\n'], " ")
                            };
                            left_label(
                                ui,
                                Rect::from_min_size(row.min, vec2(column_width, row.height())),
                                &name,
                                pickers::body(),
                                theme::TEXT,
                            );
                            let target = self.map[i];
                            let arrow = match target {
                                Some(_) if matching && self.keys[i] => "=",
                                Some(_) => "→",
                                None => "–",
                            };
                            left_label(
                                ui,
                                Rect::from_min_size(
                                    pos2(row.left() + column_width + 20.0, row.top()),
                                    vec2(middle - 20.0, row.height()),
                                ),
                                arrow,
                                pickers::body(),
                                theme::TEXT,
                            );
                            let shown = target
                                .and_then(|t| targets.iter().find(|(id, _)| *id == t))
                                .map_or("Don't import", |(_, n)| n.as_str());
                            let popup = Rect::from_min_size(
                                pos2(
                                    row.left() + column_width + middle,
                                    row.center().y - CONTROL_HEIGHT / 2.0,
                                ),
                                vec2(column_width, CONTROL_HEIGHT),
                            );
                            let label = format!("Target field for source column {}", i + 1);
                            if let Some(n) = popup_menu(ui, popup, &label, shown, &items, true) {
                                self.map[i] = n.checked_sub(1).map(|t| targets[t].0);
                                self.touched[i] = true;
                            }
                            if matching {
                                let check = Rect::from_min_size(
                                    pos2(row.left() + 2.0 * column_width + middle + 8.0, row.top()),
                                    vec2(80.0, row.height()),
                                );
                                let enabled = self.map[i].is_some();
                                if checkbox(ui, check, &mut self.keys[i], "Match", enabled) {
                                    self.touched[i] = true;
                                }
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.painter().hline(
                    left..=left + full,
                    ui.cursor().top(),
                    Stroke::new(1.0, theme::HAIRLINE),
                );
                ui.add_space(8.0);
                // The import action.
                let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
                left_label(
                    ui,
                    row,
                    "Import Action",
                    FontId::new(theme::BODY, theme::semibold()),
                    theme::TEXT,
                );
                for (action, text) in [
                    (ImportAction::Add, "Add new records"),
                    (
                        ImportAction::UpdateFoundSet,
                        "Update existing records in found set",
                    ),
                    (
                        ImportAction::UpdateMatching,
                        "Update matching records in found set",
                    ),
                ] {
                    let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
                    if radio(ui, row, self.order.action == action, text, true) {
                        self.order.action = action;
                    }
                }
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                let mut remaining = self.order.has(import_flag::ADD_REMAINING);
                let indent = Rect::from_min_max(pos2(row.left() + 24.0, row.top()), row.max);
                let updating = self.order.action != ImportAction::Add;
                if checkbox(
                    ui,
                    indent,
                    &mut remaining,
                    "Add remaining data as new records",
                    updating,
                ) {
                    self.set_flag(import_flag::ADD_REMAINING, remaining);
                }
                ui.add_space(6.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                checkbox(
                    ui,
                    row,
                    &mut self.names_row,
                    "Don't import first record (contains field names)",
                    true,
                );
                let (row, _) = ui.allocate_exact_size(vec2(full, 24.0), Sense::hover());
                let mut auto_enter = self.order.has(import_flag::AUTO_ENTER);
                if checkbox(
                    ui,
                    row,
                    &mut auto_enter,
                    "Perform auto-enter options while importing",
                    true,
                ) {
                    self.set_flag(import_flag::AUTO_ENTER, auto_enter);
                }
                if matches!(
                    self.profile.file_type,
                    FileType::TabSeparated | FileType::CommaSeparated | FileType::Merge
                ) {
                    let (row, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
                    left_label(
                        ui,
                        Rect::from_min_size(row.min, vec2(110.0, row.height())),
                        "Character set:",
                        pickers::body(),
                        theme::TEXT,
                    );
                    let items: Vec<String> = ImportCharset::ALL
                        .iter()
                        .map(|c| c.name().to_owned())
                        .collect();
                    let popup = Rect::from_min_size(
                        pos2(row.left() + 114.0, row.center().y - CONTROL_HEIGHT / 2.0),
                        vec2(220.0, CONTROL_HEIGHT),
                    );
                    if let Some(n) = popup_menu(
                        ui,
                        popup,
                        "Character set",
                        self.order.charset.name(),
                        &items,
                        true,
                    ) {
                        let was = self.order.charset;
                        self.order.charset = ImportCharset::ALL[n];
                        match self.read_columns() {
                            Ok(()) => self.error = None,
                            Err(e) => {
                                self.order.charset = was;
                                self.error = Some(e);
                            }
                        }
                    }
                }
                if let Some(error) = &self.error {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error)
                                .font(pickers::body())
                                .color(egui::Color32::from_rgb(0xC0, 0x1C, 0x28)),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(14.0);
                let (cancel, ok) = pickers::cancel_ok(ui, complete);
                if cancel {
                    outcome = Some(Picked::Cancel);
                } else if ok {
                    outcome = Some(Picked::Ok(self.answer()));
                }
            },
        );
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter && complete {
                outcome = Some(Picked::Ok(self.answer()));
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_file_maps_its_columns_into_the_order_import_records_writes() {
        let dir = std::env::temp_dir().join(format!("om-import-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source.tab");
        std::fs::write(&file, "First\tLast\tNotes\rAda\tLovelace\tx\r").unwrap();
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(&file)],
            order: None,
            profile: None,
            profile_ex: None,
            field_flags: None,
        };
        let mut dialog = ImportMapping::open(parts, None).unwrap();
        assert_eq!(dialog.columns, ["First", "Last", "Notes"]);
        dialog.order.table = Key::from_id(0xd00002);
        dialog.map = vec![Some(1), Some(2), None];
        dialog.touched = vec![true; 3];
        dialog.names_row = true;
        assert!(dialog.complete());
        // Matching needs a key.
        dialog.order.action = ImportAction::UpdateMatching;
        assert!(!dialog.complete());
        dialog.keys[1] = true;
        let (order, ex, profile) = dialog.answer();
        let ex = ex.unwrap();
        assert_eq!(profile.map(|p| p.file_type), Some(FileType::TabSeparated));
        assert_eq!(order.table.id(), Some(0xd00002));
        assert_eq!(
            order
                .map
                .iter()
                .map(|e| (e.field.id(), e.mapping))
                .collect::<Vec<_>>(),
            [
                (Some(1), Mapping::Import),
                (Some(2), Mapping::Match),
                (None, Mapping::DontImport)
            ]
        );
        assert!(order.has(import_flag::AUTO_ENTER));
        assert_eq!((ex.field_names_row, ex.predefined), (0, -1));
        assert_eq!(ex.delimiter, "\t");
        // Both encode as FileMaker reads them back.
        assert_eq!(ImportOptions::decode(&order.encode()).unwrap(), order);
        assert_eq!(ProfileEx::decode(&ex.encode()).unwrap(), ex);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A stored legacy step's Excel 97-2004 source lists its sheet's first
    /// row as the columns to map, read as Import Records reads it, and its
    /// profile (the sheet it names) is kept.
    #[test]
    fn an_excel_97_2004_source_maps_its_first_rows_cells() {
        let dir = std::env::temp_dir().join(format!("om-import-xls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source.xls");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../om-transfer/tests/data/xls/cells.xls");
        std::fs::copy(fixture, &file).unwrap();
        let mut profile = Profile::new(FileType::Excel2003);
        profile.body = om_transfer::spec::ProfileBody::Excel {
            sheet: 0,
            worksheet: String::new(),
            file: "source.xls".into(),
        };
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(&file)],
            order: None,
            profile: Some(profile.clone()),
            profile_ex: None,
            field_flags: None,
        };
        let dialog = ImportMapping::open(parts, None).unwrap();
        assert_eq!(dialog.columns, ["Text", "Number", "Flag", "When"]);
        assert_eq!(dialog.profile, profile);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unchanged_mapping_answers_the_stored_order() {
        let dir = std::env::temp_dir().join(format!("om-import-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source.csv");
        std::fs::write(&file, "a,b\r1,2\r").unwrap();
        // Arranged by matching names, a Don't import entry keeping its
        // field, an entry past the source's two columns.
        let stored = ImportOptions {
            table: Key::from_id(0xd00002),
            action: ImportAction::Add,
            flags: import_flag::AUTO_ENTER | import_flag::MATCH_NAMES,
            charset: ImportCharset::Utf8,
            map: vec![
                MapEntry {
                    field: Key::from_id(1),
                    mapping: Mapping::Import,
                },
                MapEntry {
                    field: Key::from_id(2),
                    mapping: Mapping::DontImport,
                },
                MapEntry {
                    field: Key::from_id(3),
                    mapping: Mapping::Import,
                },
            ],
        };
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(&file)],
            order: Some(stored.clone()),
            profile: Some(Profile::new(FileType::CommaSeparated)),
            profile_ex: None,
            field_flags: None,
        };
        let mut dialog = ImportMapping::open(parts, None).unwrap();
        assert_eq!(dialog.answer(), (stored.clone(), None, None));
        // One column retargeted: that entry changes, the others stay.
        dialog.map[1] = Some(4);
        dialog.touched[1] = true;
        let (order, _, _) = dialog.answer();
        assert_eq!(order.map[0], stored.map[0]);
        assert_eq!(order.map[1].field.id(), Some(4));
        assert_eq!(order.map[2], stored.map[2]);
        assert!(!order.has(import_flag::MATCH_NAMES));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_table_chosen_again_does_not_bring_cleared_targets_back() {
        let dir = std::env::temp_dir().join(format!("om-import-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source.tab");
        std::fs::write(&file, "1\t2\r").unwrap();
        let entry = |id| MapEntry {
            field: Key::from_id(id),
            mapping: Mapping::Import,
        };
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(&file)],
            order: Some(ImportOptions {
                map: vec![entry(1), entry(2)],
                ..new_order(0xd00002)
            }),
            profile: Some(Profile::new(FileType::TabSeparated)),
            profile_ex: None,
            field_flags: None,
        };
        let mut dialog = ImportMapping::open(parts, None).unwrap();
        dialog.set_table(0xd00003);
        dialog.set_table(0xd00002);
        dialog.map[0] = Some(1);
        let (order, _, _) = dialog.answer();
        assert_eq!(order.map[0], entry(1));
        assert_eq!(order.map[1].mapping, Mapping::DontImport);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_order_matched_by_names_is_shown_and_kept_by_those_names() {
        use om_data::calc::{Locale, ValueType};
        use om_data::host::FieldDef;
        let mut schema = CalcSchema::new("Import fixture", Locale::en_us());
        schema.add_table(1, "People");
        schema.add_occurrence(0xd00002, "People", 1);
        schema.add_field(1, FieldDef::data(1, "First", ValueType::Text));
        schema.add_field(1, FieldDef::data(2, "Last", ValueType::Text));
        let dir = std::env::temp_dir().join(format!("om-import-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source.mer");
        // A merge file names its fields: Last, then First.
        std::fs::write(&file, "\"Last\",\"First\"\r\"Lovelace\",\"Ada\"\r").unwrap();
        let entry = |id| MapEntry {
            field: Key::from_id(id),
            mapping: Mapping::Import,
        };
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(&file)],
            order: Some(ImportOptions {
                flags: import_flag::AUTO_ENTER | import_flag::MATCH_NAMES,
                map: vec![entry(1), entry(2)],
                ..new_order(0xd00002)
            }),
            profile: Some(Profile::new(FileType::Merge)),
            profile_ex: None,
            field_flags: None,
        };
        let mut dialog = ImportMapping::open(parts, Some(Arc::new(schema))).unwrap();
        assert_eq!(dialog.columns, ["Last", "First"]);
        // Shown by name, not by position.
        assert_eq!(dialog.map, [Some(2), Some(1)]);
        // Excluding the first column makes the order a custom one, which
        // keeps the second column's name target.
        dialog.map[0] = None;
        dialog.touched[0] = true;
        let (order, _, _) = dialog.answer();
        assert!(!order.has(import_flag::MATCH_NAMES));
        assert_eq!(order.map[0].mapping, Mapping::DontImport);
        assert_eq!(order.map[1], entry(1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_readable_source_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let parts = ImportParts {
            paths: vec![crate::scripting::files::file_line(
                &dir.path().join("om-source.tab"),
            )],
            order: None,
            profile: None,
            profile_ex: None,
            field_flags: None,
        };
        let error = ImportMapping::open(parts, None).err().unwrap();
        assert!(error.contains("Specify the data source first"), "{error}");
    }
}
