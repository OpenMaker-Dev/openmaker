//! Import Records (35), Export Records (36) and Save Records as Excel (143)
//! in the desktop client, through `om_transfer` (script-steps.md 11.3-11.5,
//! "Implementation notes (import/export)").
//!
//! Paths follow the data file steps ([`super::super::files`]): a path list
//! line of this platform, relative paths in the user's Documents folder,
//! `..` that climbs out of it refused (826). An export writes the first
//! path whose folder exists (or is created, with "Create folders"),
//! replacing a file; an import reads the first path naming a file (100
//! otherwise).
//!
//! "With dialog" shows FileMaker Pro's dialogs as one each: the output file
//! with its type, character set and export order (also when the step stores
//! no order); the source's field mapping with the import action and
//! options, then the Import Summary. Cancel is error 1. Save Records as
//! Excel writes a header row of field names on `Sheet1`; without a stored
//! order it exports the current layout's fields; with "Current record"
//! (`om_scripts::save_records::Records`) only the window's current record.
//!
//! Records are read through the host's calculation access (related
//! records, summaries, globals, unstored calculations) and the found set's
//! record pages (stored text as entered); imported rows are written with
//! the provider's import write (`DataProvider::import_record`), with the
//! import's entry options as on OpenMaker Server: every field type and
//! repetition, auto-enter only with "Perform auto-enter options", no "only
//! during data entry" validation; a row the rules refuse is left out (729
//! at the end).

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;

use om_data::calc::eval::{CalcHost, Context, FieldId};
use om_data::calc::Value;
use om_data::found::FoundSetChange;
use om_data::host::{CalcNeed, CalcSchema, FieldClass};
use om_data::runtime::{ErrorCode, HostError, HostResult, TransferCall};
use om_data::scripts::save_records::Records;
use om_transfer::export::{
    self, ExportContext, ExportRecord, ExportRequest, ExportSource, FieldInfo,
};
use om_transfer::import::{self, FieldWrite, ImportRequest, ImportTarget, TargetField};
use om_transfer::spec::{
    ExportField, FieldSpec, ImportAction, Key, MapEntry, Mapping, ProfileBody,
};
use om_transfer::text::ExportValue;
use om_transfer::{
    ExportCharset, ExportOptions, FieldFlags, FieldKind, FieldType, FileType, ImportCharset,
    ImportOptions, Profile, ProfileEx, TransferError,
};

use super::{data_error, WorkerHost};
use crate::scripting::files;
use crate::scripting::{Answer, ExportDialog, ImportDialog, ImportSourceDialog, Prompt};

/// The file types the export dialog offers.
const EXPORT_TYPES: [FileType; 7] = [
    FileType::TabSeparated,
    FileType::CommaSeparated,
    FileType::Merge,
    FileType::HtmlTable,
    FileType::XmlExport,
    FileType::ExcelExport,
    FileType::DBase,
];

fn field_type(def: &om_data::host::FieldDef) -> FieldType {
    use om_data::calc::ValueType;
    match def.value_type {
        ValueType::Text => FieldType::Text,
        ValueType::Number => FieldType::Number,
        ValueType::Date => FieldType::Date,
        ValueType::Time => FieldType::Time,
        ValueType::Timestamp => FieldType::Timestamp,
        ValueType::Container => FieldType::Container,
    }
}

fn field_kind(def: &om_data::host::FieldDef) -> FieldKind {
    match def.class {
        FieldClass::Calculation => FieldKind::Calculation,
        FieldClass::Summary => FieldKind::Summary,
        FieldClass::Data if def.global => FieldKind::Global,
        FieldClass::Data => FieldKind::Normal,
    }
}

fn calc_error(e: om_data::calc::CalcError) -> TransferError {
    TransferError::new(e.code(), "a value could not be read")
}

/// `Occurrence::Field` for a field of another occurrence, else the name.
fn label(schema: &CalcSchema, window: u64, occurrence: u64, field: u64) -> String {
    let name = schema
        .field(occurrence, field)
        .map_or_else(String::new, |f| f.name.clone());
    if occurrence == window {
        name
    } else {
        let to = schema
            .occurrence(occurrence)
            .map_or_else(String::new, |o| o.name.clone());
        format!("{to}::{name}")
    }
}

/// Open a file with the system's application ("Automatically open").
pub(super) fn open_with_system(path: &std::path::Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

impl WorkerHost<'_> {
    fn transfer_schema(&self) -> HostResult<std::sync::Arc<CalcSchema>> {
        self.schema().cloned().ok_or(HostError::Unsupported)
    }

    /// The fields an export dialog lists: the table's, then its related
    /// occurrences', as (occurrence, field, label).
    fn exportable(&self, schema: &CalcSchema, window: u64) -> Vec<(u64, u64, String)> {
        let mut out = Vec::new();
        let mut occurrences: Vec<u64> = vec![window];
        occurrences.extend(
            schema
                .occurrences_in_order()
                .iter()
                .map(|o| o.id)
                .filter(|o| *o != window && schema.path(window, *o).is_some()),
        );
        for o in occurrences {
            if let Some(table) = schema.table_of(o) {
                for f in table.fields.values() {
                    if f.value_type != om_data::calc::ValueType::Container {
                        out.push((o, f.id, label(schema, window, o, f.id)));
                    }
                }
            }
        }
        out
    }

    /// Export Records and Save Records as Excel (module docs).
    pub(super) fn export_step(&mut self, call: &TransferCall<'_>) -> HostResult {
        self.commit_window()?;
        let schema = self.transfer_schema()?;
        let occurrence = self.occurrence().ok_or(ErrorCode::RECORD_MISSING)?;
        let excel = call.id == 143;
        let mut current = excel && Records::of(call.step) == Records::Current;
        let mut options = match call.export_order.as_deref() {
            Some(bytes) if call.restore || !excel => {
                ExportOptions::decode(bytes).map_err(|_| ErrorCode::INVALID)?
            }
            _ => ExportOptions {
                fields: self.layout_export_fields(&schema),
                group_by: Vec::new(),
                formatting: false,
                charset: ExportCharset::Utf8,
                use_name_alias: Some(1),
                field_names: None,
            },
        };
        let mut profile = match call.profile.as_deref() {
            Some(bytes) => Profile::decode(bytes).map_err(|_| ErrorCode::INVALID)?,
            None => Profile::new(if excel {
                FileType::ExcelExport
            } else {
                FileType::TabSeparated
            }),
        };
        if excel {
            profile.file_type = FileType::ExcelExport;
        }
        let mut paths = call.paths.clone();
        if call.with_dialog || paths.is_empty() {
            let available = self.exportable(&schema, occurrence);
            let chosen = options
                .fields
                .iter()
                .filter_map(|f| f.field.ids())
                .filter_map(|(o, f)| available.iter().position(|a| a.0 == o && a.1 == f))
                .collect();
            let stem = self
                .s
                .file
                .context
                .file
                .trim_end_matches(".fmp12")
                .to_owned();
            // The first stored path this machine reads, as the resolver
            // would take it: another platform's line first is skipped.
            let shown = paths
                .iter()
                .find(|line| files::of_this_platform(line))
                .cloned()
                .unwrap_or_else(|| format!("file:{stem}.{}", profile.file_type.extension()));
            let dialog = ExportDialog {
                path: shown.clone(),
                file_type: profile.file_type,
                types: if excel {
                    vec![FileType::ExcelExport]
                } else {
                    EXPORT_TYPES.to_vec()
                },
                charset: options.charset,
                formatting: options.formatting,
                available: available.iter().map(|a| a.2.clone()).collect(),
                chosen,
                current_record: excel.then_some(current),
            };
            let Some(Answer::Export(Some(d))) = self.ask(Prompt::Export(dialog)) else {
                return Err(ErrorCode::CANCELED.into());
            };
            // A native path the user typed, in FileMaker's form, as the
            // import dialog passes its own: `/private/tmp/x.csv` as it is
            // would name the volume `private` on macOS (error 826). The
            // path list line shown stays as it is when the user left it, or
            // only changed the file type, which gives it that type's
            // extension (`app::scripts::transfer`): it may be a FileMaker
            // path without a prefix.
            let retyped = shown
                .rsplit_once('.')
                .map(|(stem, _)| format!("{stem}.{}", d.file_type.extension()));
            paths = vec![if d.path == shown || Some(&d.path) == retyped.as_ref() {
                d.path.clone()
            } else {
                files::chosen_path(&d.path)
            }];
            current = d.current_record == Some(true);
            profile = Profile::new(d.file_type);
            options.charset = d.charset;
            options.formatting = d.formatting;
            options.fields = d
                .chosen
                .iter()
                .filter_map(|i| available.get(*i))
                .map(|(o, f, _)| ExportField {
                    field: FieldSpec::new(*o, *f),
                    sum_by: FieldSpec::none(),
                })
                .collect();
            options.group_by.clear();
        }
        let (found, only) = {
            let (window, provider) = self.wp();
            let found = window.found_ids(provider).map_err(data_error)?;
            let only = if current {
                window.record_id(provider).map_err(data_error)?
            } else {
                None
            };
            (found, only)
        };
        // No records to write: no file, as for an empty found set.
        if found.is_empty() || (current && only.is_none()) {
            return Ok(());
        }
        let path = self.export_path(&paths, call.create_folders)?;
        let mut request = ExportRequest::new(options, profile);
        request.field_names_row |= excel;
        let sort = self.sorted_fields(&schema, occurrence);
        let mut source = GuiExport {
            host: self,
            schema,
            occurrence,
            found,
            only,
            sort,
            pages: HashMap::new(),
            groups: HashMap::new(),
            positions: HashMap::new(),
        };
        let bytes = export::export(&mut source, &request)
            .map_err(|e| HostError::Error(ErrorCode(e.code)))?;
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes)
                .map_err(|_| ErrorCode(om_transfer::code::CANNOT_CREATE))?;
            if call.open_automatically {
                open_with_system(&path);
            }
        }
        Ok(())
    }

    /// The current layout's fields, for Save Records as Excel without a
    /// stored order.
    fn layout_export_fields(&mut self, schema: &CalcSchema) -> Vec<ExportField> {
        let Some(layout) = self.s.file.window.state.layout else {
            return Vec::new();
        };
        schema
            .layouts()
            .iter()
            .find(|l| l.id == layout)
            .map(|l| {
                l.fields
                    .iter()
                    .map(|f| ExportField {
                        field: FieldSpec::new(f.occurrence, f.field),
                        sum_by: FieldSpec::none(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The found set's sort fields as (occurrence, field).
    fn sorted_fields(&self, schema: &CalcSchema, window: u64) -> Vec<(u64, u64)> {
        if !self.s.file.window.sorted {
            return Vec::new();
        }
        self.s
            .file
            .window
            .last_sort
            .iter()
            .filter_map(|s| {
                let (occurrence, name) = match s.field.split_once("::") {
                    Some((to, name)) => (schema.occurrence_named(to)?.id, name),
                    None => (window, s.field.as_str()),
                };
                let table = schema.table_of(occurrence)?;
                let field = table.fields.values().find(|f| f.name == name)?;
                Some((occurrence, field.id))
            })
            .collect()
    }

    /// Where an export writes (module docs).
    pub(super) fn export_path(
        &self,
        paths: &[String],
        create_folders: bool,
    ) -> HostResult<PathBuf> {
        let documents = self.s.file.data_files.folders.documents.clone();
        for path in files::candidates(paths, &documents)? {
            let Some(parent) = path.parent() else {
                continue;
            };
            if path.is_dir() {
                continue;
            }
            if parent.is_dir() || (create_folders && std::fs::create_dir_all(parent).is_ok()) {
                return Ok(path);
            }
        }
        Err(ErrorCode(om_transfer::code::CANNOT_CREATE).into())
    }

    /// Import Records (module docs).
    pub(super) fn import_step(&mut self, call: &TransferCall<'_>) -> HostResult {
        self.commit_window()?;
        let schema = self.transfer_schema()?;
        let window = self.occurrence().ok_or(ErrorCode::RECORD_MISSING)?;
        if call.source_kind == 2 {
            return Err(HostError::Unsupported);
        }
        let mut options = match call.import_order.as_deref() {
            Some(bytes) => ImportOptions::decode(bytes).map_err(|_| ErrorCode::INVALID)?,
            None => ImportOptions {
                table: Key::from_id(window),
                action: ImportAction::Add,
                flags: om_transfer::spec::import_flag::AUTO_ENTER,
                charset: ImportCharset::Utf8,
                map: Vec::new(),
            },
        };
        let mut profile = match call.profile.as_deref() {
            Some(bytes) => Profile::decode(bytes).map_err(|_| ErrorCode::INVALID)?,
            None => Profile::new(FileType::TabSeparated),
        };
        if matches!(
            profile.file_type,
            FileType::FileMaker | FileType::Folder | FileType::Odbc | FileType::Excel2003
        ) {
            return Err(HostError::Unsupported);
        }
        let mut profile_ex = call
            .profile_ex
            .as_deref()
            .and_then(|b| ProfileEx::decode(b).ok());
        let field_flags = call
            .field_flags
            .as_deref()
            .and_then(|b| FieldFlags::decode(b).ok());
        import::refuse_unsupported(&options, field_flags.as_ref())
            .map_err(|_| HostError::Unsupported)?;
        let mut paths = call.paths.clone();
        let target = options.table.id().unwrap_or(window);
        let target_name = format!(
            "{} — {}",
            self.s.file.context.file,
            schema
                .table_of(target)
                .map_or("Unknown table", |t| t.name.as_str())
        );
        let choose_source = paths.is_empty();
        if choose_source {
            // The menu uses the ordinary owned script worker. Refuse a
            // provider that only supports ordinary edits before offering
            // an import; every actual row still uses import_record.
            if !self.p().supports_record_import()
                || !ImportSourceDialog::TYPES.contains(&profile.file_type)
            {
                return Err(HostError::Unsupported);
            }
            let sheet = match &profile.body {
                ProfileBody::Excel { sheet, .. } => sheet.saturating_add(1),
                _ => 1,
            };
            let Some(Answer::ImportSource(Some(d))) =
                self.ask(Prompt::ImportSource(ImportSourceDialog {
                    path: String::new(),
                    target: target_name.clone(),
                    file_type: profile.file_type,
                    charset: options.charset,
                    sheet,
                }))
            else {
                return Err(ErrorCode::CANCELED.into());
            };
            if !ImportSourceDialog::TYPES.contains(&d.file_type) || d.sheet == 0 {
                return Err(ErrorCode::INVALID.into());
            }
            if d.file_type != profile.file_type {
                profile = Profile::new(d.file_type);
                profile_ex = None;
            }
            if d.file_type == FileType::Excel2007 {
                profile.body = ProfileBody::Excel {
                    sheet: d.sheet - 1,
                    worksheet: String::new(),
                    file: d.path.clone(),
                };
            }
            options.charset = d.charset;
            // The chooser accepts native absolute paths. Convert those at
            // this UI boundary; scripted path lists retain FileMaker syntax.
            paths.push(files::chosen_path(&d.path));
        }
        let documents = self.s.file.data_files.folders.documents.clone();
        let path = files::existing(&paths, &documents, false)?;
        // One byte past the limit, so that the import refuses a larger
        // file without it being read whole.
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|f| {
                f.take(import::limit::SOURCE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|_| ErrorCode::FILE_MISSING)?;
        if call.with_dialog || choose_source || options.map.is_empty() {
            let probe = ImportRequest {
                options: options.clone(),
                profile: profile.clone(),
                profile_ex: profile_ex.clone(),
                field_flags: field_flags.clone(),
            };
            let source =
                import::parse(&probe, &bytes).map_err(|e| HostError::Error(ErrorCode(e.code)))?;
            let columns: Vec<String> = match &source.names {
                Some(names) => names.clone(),
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
            let targets: Vec<(u64, String)> = schema
                .table_of(target)
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
                .unwrap_or_default();
            let map = (0..columns.len())
                .map(|i| {
                    let entry = options.map.get(i)?;
                    if !matches!(entry.mapping, Mapping::Import | Mapping::Match) {
                        return None;
                    }
                    let id = entry.field.id()?;
                    targets.iter().position(|t| t.0 == id)
                })
                .collect();
            let dialog = ImportDialog {
                path: path.display().to_string(),
                target: target_name,
                targets: targets.iter().map(|t| t.1.clone()).collect(),
                map,
                match_fields: (0..columns.len())
                    .map(|i| {
                        options
                            .map
                            .get(i)
                            .is_some_and(|e| e.mapping == Mapping::Match)
                    })
                    .collect(),
                columns,
                action: options.action,
                add_remaining: options.has(om_transfer::spec::import_flag::ADD_REMAINING),
                names_row: profile_ex
                    .as_ref()
                    .map_or(profile.field_names != 0, |ex| ex.field_names_row >= 0),
                auto_enter: options.has(om_transfer::spec::import_flag::AUTO_ENTER),
            };
            let Some(Answer::Import(Some(d))) = self.ask(Prompt::Import(dialog)) else {
                return Err(ErrorCode::CANCELED.into());
            };
            options.action = d.action;
            options.flags &= !(om_transfer::spec::import_flag::AUTO_ENTER
                | om_transfer::spec::import_flag::MATCH_NAMES
                | om_transfer::spec::import_flag::ADD_REMAINING);
            if d.auto_enter {
                options.flags |= om_transfer::spec::import_flag::AUTO_ENTER;
            }
            if d.add_remaining {
                options.flags |= om_transfer::spec::import_flag::ADD_REMAINING;
            }
            options.map = d
                .map
                .iter()
                .enumerate()
                .map(|(column, t)| match t.and_then(|i| targets.get(i)) {
                    Some((id, _)) => MapEntry {
                        field: Key::from_id(*id),
                        mapping: if d.action == ImportAction::UpdateMatching
                            && d.match_fields.get(column) == Some(&true)
                        {
                            Mapping::Match
                        } else {
                            Mapping::Import
                        },
                    },
                    None => MapEntry {
                        field: Key::none(),
                        mapping: Mapping::DontImport,
                    },
                })
                .collect();
            profile_ex = Some(ProfileEx {
                field_names_row: if d.names_row {
                    profile_ex
                        .as_ref()
                        .map_or(0, |ex| ex.field_names_row.max(0))
                } else {
                    -1
                },
                predefined: -1,
                delimiter: profile_ex.map_or_else(
                    || {
                        if profile.file_type == FileType::CommaSeparated {
                            ","
                        } else {
                            "\t"
                        }
                        .into()
                    },
                    |ex| ex.delimiter,
                ),
            });
        }
        let request = ImportRequest {
            options,
            profile,
            profile_ex,
            field_flags,
        };
        let locale = CalcHost::locale(self).clone();
        let summary = import::import(
            &mut GuiImport {
                host: self,
                schema,
                window,
            },
            &request,
            &bytes,
            &locale,
        )
        .map_err(|e| HostError::Error(ErrorCode(e.code)))?;
        if call.with_dialog || choose_source {
            self.ask(Prompt::ImportSummary {
                total: summary.total,
                added: summary.added,
                updated: summary.updated,
                skipped: summary.errors.len(),
            });
        }
        match summary.error_code() {
            0 => Ok(()),
            code => Err(ErrorCode(code).into()),
        }
    }
}

/// The window's found set as an export reads it.
struct GuiExport<'a, 'h> {
    host: &'a mut WorkerHost<'h>,
    schema: std::sync::Arc<CalcSchema>,
    occurrence: u64,
    found: Vec<i64>,
    /// The one record to write (Save Records as Excel of the current
    /// record); the found set otherwise. Summaries and pages still read
    /// the whole found set.
    only: Option<i64>,
    sort: Vec<(u64, u64)>,
    /// The found set's record pages: stored text by record and field name.
    pages: HashMap<i64, HashMap<String, String>>,
    /// Group rows' summaries, by (summary, break field): each record's
    /// group value, in found-set order (`FileCalc::group_summaries`).
    groups: HashMap<(FieldId, FieldId), Vec<Value>>,
    /// Each record's position in the found set.
    positions: HashMap<u64, usize>,
}

impl GuiExport<'_, '_> {
    /// Read the found set's pages once (stored text as entered).
    fn page_text(&mut self, record: i64, name: &str) -> Option<String> {
        if self.pages.is_empty() {
            let layout = self.host.s.file.window.state.layout?;
            let page = self
                .host
                .p()
                .records(
                    layout,
                    0,
                    u32::try_from(self.found.len()).unwrap_or(u32::MAX),
                )
                .ok()?;
            for r in page.records {
                let values = page
                    .fields
                    .iter()
                    .zip(r.values)
                    .map(|(f, v)| (f.name.clone(), v.unwrap_or_default()))
                    .collect();
                self.pages.insert(r.id, values);
            }
        }
        self.pages.get(&record)?.get(name).cloned()
    }
}

impl ExportSource for GuiExport<'_, '_> {
    fn context(&mut self) -> om_transfer::Result<ExportContext> {
        let cx = Context {
            occurrence: Some(self.occurrence),
            ..Context::new()
        };
        let version = self
            .host
            .get(1, &cx)
            .map(|v| self.host.text(&v))
            .unwrap_or_default();
        let total = self
            .host
            .get(19, &cx)
            .ok()
            .and_then(|v| self.host.text(&v).parse().ok())
            .unwrap_or(self.found.len() as u64);
        Ok(ExportContext {
            file_name: self.host.s.file.context.file.clone(),
            layout_name: String::new(),
            total_records: total,
            product_build: "08-26-2026".into(),
            product_name: "FileMaker".into(),
            product_version: version,
            date_format: "M/d/yyyy".into(),
            time_format: "h:mm:ss a".into(),
            locale: CalcHost::locale(&*self.host).clone(),
            created: utc_now(),
        })
    }

    fn field(&mut self, occurrence: u64, field: u64) -> om_transfer::Result<FieldInfo> {
        let def = self
            .schema
            .field(occurrence, field)
            .ok_or_else(|| TransferError::new(102, "The field is missing."))?;
        Ok(FieldInfo {
            name: def.name.clone(),
            occurrence: self
                .schema
                .occurrence(occurrence)
                .map_or_else(String::new, |o| o.name.clone()),
            related: occurrence != self.occurrence,
            ty: field_type(def),
            kind: field_kind(def),
            repetitions: def.repetitions.max(1),
            empty_ok: true,
        })
    }

    fn records(&mut self) -> om_transfer::Result<Vec<ExportRecord>> {
        let ids = match self.only {
            Some(id) => vec![id],
            None => self.found.clone(),
        };
        Ok(ids
            .into_iter()
            .map(|id| ExportRecord {
                id: id as u64,
                mod_count: 0,
            })
            .collect())
    }

    fn sort_fields(&mut self) -> om_transfer::Result<Vec<(u64, u64)>> {
        Ok(self.sort.clone())
    }

    fn values(
        &mut self,
        record: u64,
        occurrence: u64,
        field: u64,
    ) -> om_transfer::Result<Vec<Vec<ExportValue>>> {
        let def = self
            .schema
            .field(occurrence, field)
            .cloned()
            .ok_or_else(|| TransferError::new(102, "The field is missing."))?;
        let reps = def.repetitions.max(1);
        if occurrence != self.occurrence {
            let related = self
                .host
                .related_records(self.occurrence, record, occurrence)
                .map_err(calc_error)?;
            return related
                .into_iter()
                .map(|r| {
                    (1..=reps)
                        .map(|rep| {
                            self.host
                                .field_value(occurrence, r, field, rep)
                                .map(ExportValue::Computed)
                                .map_err(calc_error)
                        })
                        .collect()
                })
                .collect();
        }
        let mut out = Vec::with_capacity(reps as usize);
        for rep in 1..=reps {
            let stored = rep == 1 && def.class == FieldClass::Data && !def.global && reps == 1;
            if stored {
                if let Some(text) = self.page_text(record as i64, &def.name) {
                    out.push(ExportValue::Stored(text));
                    continue;
                }
            }
            out.push(
                self.host
                    .field_value(occurrence, record, field, rep)
                    .map(ExportValue::Computed)
                    .map_err(calc_error)?,
            );
        }
        Ok(vec![out])
    }

    fn summary(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[u64],
        by: Option<(u64, u64)>,
    ) -> om_transfer::Result<ExportValue> {
        let Some(&first) = records.first() else {
            return Ok(ExportValue::empty());
        };
        if let Some((bo, bf)) = by {
            // A group row's summary, as `GetSummary ( summary ; break )` on
            // its first record; every group's at once, so a grouped export
            // reads the found set once per summary, not once per group.
            let key = (
                FieldId { occurrence, field },
                FieldId {
                    occurrence: bo,
                    field: bf,
                },
            );
            if !self.groups.contains_key(&key) {
                let values = self
                    .host
                    .file(|f| f.group_summaries(key.0, key.1))
                    .unwrap_or(Err(om_data::calc::CalcError::NOT_SUPPORTED))
                    .map_err(calc_error)?;
                self.groups.insert(key, values);
            }
            if self.positions.is_empty() {
                self.positions = self
                    .found
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (*id as u64, i))
                    .collect();
            }
            return Ok(self
                .positions
                .get(&first)
                .and_then(|&i| self.groups[&key].get(i))
                .cloned()
                .map_or_else(ExportValue::empty, ExportValue::Computed));
        }
        let (bo, bf) = (occurrence, field);
        om_data::summary::or_invalid(self.host.summary(
            &Context {
                occurrence: Some(self.occurrence),
                record: Some(first),
                ..Context::new()
            },
            FieldId { occurrence, field },
            FieldId {
                occurrence: bo,
                field: bf,
            },
        ))
        .map(ExportValue::Computed)
        .map_err(calc_error)
    }

    /// Each record's value as the window's found set shows it, the current
    /// record's too when it is exported alone.
    fn record_summaries(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[u64],
    ) -> om_transfer::Result<Vec<ExportValue>> {
        let values = self
            .host
            .file(|f| f.record_summaries(FieldId { occurrence, field }, records))
            .unwrap_or(Err(om_data::calc::CalcError::NOT_SUPPORTED))
            .map_err(calc_error)?;
        Ok(values.into_iter().map(ExportValue::Computed).collect())
    }
}

/// The export's time in UTC, `2026-10-29T14:17:34Z`.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The provider's record writes as an import target.
struct GuiImport<'a, 'h> {
    host: &'a mut WorkerHost<'h>,
    schema: std::sync::Arc<CalcSchema>,
    /// The window's occurrence.
    window: u64,
}

impl GuiImport<'_, '_> {
    /// A layout showing `occurrence`: the window's, else the first.
    fn layout(&self, occurrence: u64) -> om_transfer::Result<u32> {
        if occurrence == self.window {
            if let Some(layout) = self.host.s.file.window.state.layout {
                return Ok(layout);
            }
        }
        self.schema
            .layouts()
            .iter()
            .find(|l| l.occurrence == Some(occurrence))
            .map(|l| l.id)
            .ok_or_else(|| TransferError::new(105, "No layout shows the target table."))
    }

    /// The writes as the record's cells: a field's id for its first
    /// repetition, `F * 256 + n` for repetition n (as the provider and the
    /// file store them); an empty value clears the cell.
    fn cells(writes: &[FieldWrite]) -> BTreeMap<u64, Option<String>> {
        writes
            .iter()
            .filter(|w| (1..=255).contains(&w.repetition))
            .map(|w| {
                let cell = match w.repetition {
                    1 => w.field,
                    n => w.field * 256 + u64::from(n),
                };
                (cell, (!w.text.is_empty()).then(|| w.text.clone()))
            })
            .collect()
    }

    fn same_table(&self, occurrence: u64) -> bool {
        let table = |o| self.schema.table_of(o).map(|t| t.id);
        table(occurrence).is_some() && table(occurrence) == table(self.window)
    }
}

fn write_error(e: om_data::DataError) -> TransferError {
    TransferError::new(e.code.unwrap_or(3), e.message)
}

impl ImportTarget for GuiImport<'_, '_> {
    fn fields(&mut self, occurrence: u64) -> om_transfer::Result<Vec<TargetField>> {
        let table = self
            .schema
            .table_of(occurrence)
            .ok_or_else(|| TransferError::new(102, "The table occurrence is missing."))?;
        Ok(table
            .fields
            .values()
            .map(|f| TargetField {
                id: f.id,
                name: f.name.clone(),
                ty: field_type(f),
                kind: field_kind(f),
                repetitions: f.repetitions.max(1),
            })
            .collect())
    }

    /// The window's found set; for another table than the window's, its
    /// records in creation order, as FileMaker Server updates them (oracle
    /// x04).
    fn found_set(&mut self, occurrence: u64) -> om_transfer::Result<Vec<u64>> {
        if !self.same_table(occurrence) {
            let table = self
                .schema
                .table_of(occurrence)
                .map(|t| t.id)
                .ok_or_else(|| TransferError::new(102, "The table occurrence is missing."))?;
            let data = self
                .host
                .p()
                .calc_data(&[CalcNeed::Records { table }])
                .map_err(write_error)?;
            return Ok(data
                .tables
                .get(&table)
                .map(|ids| ids.to_vec())
                .unwrap_or_default());
        }
        let (window, provider) = self.host.wp();
        let ids = window.found_ids(provider).map_err(write_error)?;
        Ok(ids.into_iter().map(|i| i as u64).collect())
    }

    fn value(&mut self, occurrence: u64, record: u64, field: u64) -> om_transfer::Result<String> {
        let v: Value = self
            .host
            .field_value(occurrence, record, field, 1)
            .map_err(calc_error)?;
        Ok(self.host.text(&v))
    }

    /// A new record through the provider's import write, with the import's
    /// entry options (`DataProvider::import_record`).
    fn create(
        &mut self,
        occurrence: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> om_transfer::Result<u64> {
        let layout = self.layout(occurrence)?;
        let record = self
            .host
            .p()
            .import_record(layout, None, &Self::cells(writes), auto_enter)
            .map_err(write_error)?;
        Ok(record as u64)
    }

    fn update(
        &mut self,
        occurrence: u64,
        record: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> om_transfer::Result<()> {
        let layout = self.layout(occurrence)?;
        self.host
            .p()
            .import_record(
                layout,
                Some(record as i64),
                &Self::cells(writes),
                auto_enter,
            )
            .map(|_| ())
            .map_err(write_error)
    }

    fn set_found_set(&mut self, occurrence: u64, records: &[u64]) -> om_transfer::Result<()> {
        if self.same_table(occurrence) {
            let ids = records.iter().map(|r| *r as i64).collect();
            self.host
                .change(FoundSetChange::Records(ids))
                .map_err(|_| TransferError::new(3, "The found set could not be shown."))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_times() {
        let t = utc_now();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }
}
