//! The desktop's container steps (containers.md, "Implementation notes
//! (desktop client)"): Insert File (131), Insert Picture (56), Insert PDF
//! (158), Insert Audio/Video (159) and Export Field Contents (132), also
//! as the Insert menu's Picture, Audio/Video, PDF and File and the Edit
//! menu's Export Field Contents run them.
//!
//! An inserted file is read from the first path of its path list that names
//! an existing file, with the path rules of the data file steps
//! ([`files`]): Insert File reads `file` paths, Insert Picture and Insert
//! PDF `image` paths and Insert Audio/Video `movie` paths, each family with
//! its platform forms (Claris, "Creating file paths"); paths of another
//! family are skipped. The file is typed by its name as FileMaker Server's
//! engine types a named file a field stores (script oracle s34,
//! `om_calc::functions::typed_file`) and goes into the container field
//! through the provider's op88/op87 path, or into Insert File's variable.
//! Errors (OpenMaker's choices where FileMaker's are not observed; the
//! steps do not run on FileMaker Server, so its engine cannot settle them):
//! 100 no path names a file, 826 an invalid path, 102 no active field, 413
//! a field that is not a container, 734 a file Insert Picture cannot show
//! as a picture, 700 a file of the wrong type for Insert PDF or Insert
//! Audio/Video, 2 a file over [`MAX_INSERT`].
//!
//! A step without a path asks for the file ([`Prompt::ContainerFile`]): in
//! the system's open panel on macOS (`crate::file_panel`), elsewhere and in
//! the GUI tests in an in-app dialog that takes a path, once
//! the field can take it: a stored container field of the window's own
//! table in Browse mode, on a connection that may change the record. The
//! dialog's answer applies only to what it was opened for
//! ([`DialogContext`]): Cancel, or a changed file, window, record or field,
//! changes nothing. Not done: "Store only a reference" (the stored form of
//! a reference is not specified, containers.md 12), which stops the script
//! as an unsupported step.
//!
//! Export Field Contents writes the field's main data stream, or its text
//! as UTF-16 (Claris: "a UTF-16 format text file"; little-endian with a
//! byte order mark, OpenMaker's choice), to the first path Export Records
//! could write to, creating folders only with "Create folders"; 800 when
//! none can be written. The data is read before any output is opened
//! (external data that cannot be read intact is refused, 1225), and the
//! file is staged beside the destination and renamed over it only once
//! complete, so a failure leaves an existing file as it was. Without a
//! path the dialog offers the container's file name, and asks before
//! replacing a file that exists.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use om_data::calc::{Container, Value, ValueType};
use om_data::host::{CalcSchema, FieldClass};
use om_data::runtime::{ErrorCode, FieldRef, HostError, HostResult, InsertFile, InsertKind};

use super::{data_error, files, note_failure, Mode, WorkerHost, FIELD_LOCKED};
use crate::scripting::{Answer, ContainerFileDialog, Prompt};

/// The largest file an Insert step reads (OpenMaker's limit: the data is
/// held in memory to upload).
pub(super) const MAX_INSERT: u64 = 1 << 30;

/// "Specified field has inappropriate field type".
const NOT_A_CONTAINER: i32 = 413;
/// "This file type cannot be displayed as a picture".
const NOT_A_PICTURE: i32 = 734;
/// "File is of the wrong file type for import".
const WRONG_FILE_TYPE: i32 = 700;
/// Out of memory.
const TOO_LARGE: i32 = 2;

/// A path line of `family` (`file`, `image`, `movie`) as a data-file path
/// (`file...`), or `None` for a line of another family. A line without a
/// prefix is the step's own family.
fn in_family(line: &str, family: &str) -> Option<String> {
    let trimmed = line.trim();
    let Some((prefix, rest)) = trimmed
        .split_once(':')
        .filter(|(p, _)| p.len() > 1 && p.chars().all(|c| c.is_ascii_alphabetic()))
    else {
        return Some(trimmed.to_owned());
    };
    let prefix = prefix.to_ascii_lowercase();
    let platform = prefix.strip_prefix(family)?;
    matches!(platform, "" | "mac" | "win" | "linux").then(|| format!("file{platform}:{rest}"))
}

/// What a container step's file dialog was opened for: the file, window,
/// record and field. The worker serves other windows while the dialog is
/// open (their writes, a schema change), and a peer file can have the same
/// layout, record and field ids, so the answer applies only while all of
/// this is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DialogContext {
    file: u64,
    epoch: u64,
    window: u32,
    uuid: String,
    mode: Mode,
    layout: Option<u32>,
    record: Option<i64>,
    occurrence: Option<u64>,
    focus: Option<String>,
    object: Option<u64>,
    /// The calculation schema the field was resolved in; Manage Database
    /// in another window reads it again.
    schema: Option<SchemaId>,
    /// The field (occurrence and id, with a calculation schema) and its
    /// name.
    field: Option<(u64, u64)>,
    name: String,
}

/// A calculation schema by identity: one read again is another.
#[derive(Clone, Debug)]
struct SchemaId(Arc<CalcSchema>);

impl PartialEq for SchemaId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SchemaId {}

impl WorkerHost<'_> {
    /// [`om_data::runtime::Fields::insert_file`].
    pub(super) fn insert_file_step(&mut self, request: &InsertFile) -> HostResult<Value> {
        if request.reference {
            return Err(HostError::Unsupported);
        }
        // A related container (a portal row's): the row's record takes the
        // file, committed with the window's record.
        if let Some(field) = request
            .target
            .map(|target| self.related_container_target(target.field))
            .transpose()?
            .flatten()
        {
            return self.insert_related(request, field);
        }
        let path = if request.paths.is_empty() {
            self.choose_inserted(request)?
        } else {
            let family = match request.kind {
                InsertKind::File => "file",
                InsertKind::Picture | InsertKind::Pdf => "image",
                InsertKind::AudioVideo => "movie",
            };
            let lines: Vec<String> = request
                .paths
                .iter()
                .filter_map(|line| in_family(line, family))
                .collect();
            let documents = self.s.file.data_files.folders.documents.clone();
            files::existing(&lines, &documents, false)?
        };
        let value = Value::Container(read_inserted(&path, request.kind)?);
        let Some(target) = request.target else {
            return Ok(value);
        };
        let name = self.target_name(target.field)?;
        if self.s.file.window.state.mode != Mode::Browse || !self.is_container_field(&name) {
            return Err(ErrorCode(NOT_A_CONTAINER).into());
        }
        self.set_container(&name, value.clone())?;
        Ok(value)
    }

    /// The file an Insert step without a path inserts, as the user chose
    /// it, once its field can take a file (module docs).
    fn choose_inserted(&mut self, request: &InsertFile) -> HostResult<PathBuf> {
        let title = match request.kind {
            InsertKind::File => "Insert File",
            InsertKind::Picture => "Insert Picture",
            InsertKind::Pdf => "Insert PDF",
            InsertKind::AudioVideo => "Insert Audio/Video",
        };
        let (field, context) = match request.target {
            Some(target) => {
                let name = self.insertable(target.field)?;
                let field = self.own_def(&name).zip(self.s.file.occurrence);
                let field = field.map(|(def, occurrence)| (occurrence, def.id));
                let context = self.dialog_context(field, &name)?;
                (self.field_label(field, &name), Some(context))
            }
            // Insert File into a variable changes no record.
            None => (String::new(), None),
        };
        let dialog = ContainerFileDialog {
            title: title.into(),
            field,
            export: false,
            path: String::new(),
            chosen: None,
        };
        let documents = self.s.file.data_files.folders.documents.clone();
        match self.choose_file(dialog, context.as_ref())? {
            Chosen::Native(path) => Ok(path),
            Chosen::Line(line) => files::existing(&[line], &documents, false).map_err(Into::into),
        }
    }

    /// The related field an Insert step's file goes into, or Export Field
    /// Contents writes, when it is one: `field`, or without one the field
    /// of the active portal row that has the focus (a container the user
    /// selected in a row).
    fn related_container_target(
        &mut self,
        field: Option<FieldRef>,
    ) -> HostResult<Option<FieldRef>> {
        let field = match field {
            Some(field) => field,
            None if self.s.file.window.state.focus.is_some() => return Ok(None),
            None => {
                let Some(object) = self.s.file.window.state.portal.and_then(|row| row.field) else {
                    return Ok(None);
                };
                let Some(layout) = self.s.file.window.state.layout else {
                    return Ok(None);
                };
                let Some(spec) = self
                    .render(layout)
                    .and_then(|render| render.layout.objects.get(object)?.field_spec().cloned())
                else {
                    return Ok(None);
                };
                let (Some(occurrence), Some(id)) =
                    (om_layout::occurrence_id(spec.table_key), spec.field_id())
                else {
                    return Ok(None);
                };
                FieldRef {
                    occurrence,
                    field: id,
                    repetition: u32::try_from(spec.repetition.max(1)).unwrap_or(1),
                }
            }
        };
        Ok(self.is_related(field.occurrence).then_some(field))
    }

    /// Insert into related container `field` ([`Self::related_container_target`]):
    /// checked before the user is asked (Browse mode, a stored container
    /// field Browse may edit in that table), stored in the related table's
    /// library, and written into the active row's record, the creation
    /// row's a new one, as Set Field writes a related field
    /// ([`Self::set_related`]). A reference the same edit took before is
    /// released.
    fn insert_related(&mut self, request: &InsertFile, field: FieldRef) -> HostResult<Value> {
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        let def = schema
            .field(field.occurrence, field.field)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?
            .clone();
        let table = schema
            .table_of(field.occurrence)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?
            .id;
        if self.s.file.window.state.mode != Mode::Browse || def.value_type != ValueType::Container {
            return Err(ErrorCode(NOT_A_CONTAINER).into());
        }
        if def.global || def.class != FieldClass::Data || field.repetition > 1 {
            return Err(ErrorCode(FIELD_LOCKED).into());
        }
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let capabilities = self
            .p()
            .record_capabilities(layout)
            .map_err(|e| self.failed(e))?;
        if !capabilities.can_edit_related(field.occurrence, field.field) {
            note_failure("This related record cannot be changed on this connection.".into());
            return Err(ErrorCode(FIELD_LOCKED).into());
        }
        let path = if request.paths.is_empty() {
            let reference = Some((field.occurrence, field.field));
            let context = self.dialog_context(reference, &def.name)?;
            let title = match request.kind {
                InsertKind::File => "Insert File",
                InsertKind::Picture => "Insert Picture",
                InsertKind::Pdf => "Insert PDF",
                InsertKind::AudioVideo => "Insert Audio/Video",
            };
            let dialog = ContainerFileDialog {
                title: title.into(),
                field: self.field_label(reference, &def.name),
                export: false,
                path: String::new(),
                chosen: None,
            };
            let documents = self.s.file.data_files.folders.documents.clone();
            match self.choose_file(dialog, Some(&context))? {
                Chosen::Native(path) => path,
                Chosen::Line(line) => files::existing(&[line], &documents, false)?,
            }
        } else {
            let family = match request.kind {
                InsertKind::File => "file",
                InsertKind::Picture | InsertKind::Pdf => "image",
                InsertKind::AudioVideo => "movie",
            };
            let lines: Vec<String> = request
                .paths
                .iter()
                .filter_map(|line| in_family(line, family))
                .collect();
            let documents = self.s.file.data_files.folders.documents.clone();
            files::existing(&lines, &documents, false)?
        };
        let container = read_inserted(&path, request.kind)?;
        let target = self.related_target(field.occurrence)?;
        let mixed = self.s.file.window.state.uncommitted.session
            && (self.s.file.window.state.uncommitted.new
                || self
                    .s
                    .file
                    .window
                    .state
                    .uncommitted
                    .attachment_field
                    .is_some())
            && matches!(
                target,
                super::portals::Target::Draft(_) | super::portals::Target::Record(_)
            )
            && !self.container_drafts().is_empty();
        if mixed {
            self.stage_mixed_attachment_rows()?;
        }
        let prepared = if let super::portals::Target::Record(record) = target {
            let open = &self.s.file.window.state.uncommitted;
            let version = match open
                .related
                .iter()
                .find(|d| d.table == table && d.record == Some(record))
            {
                Some(draft) => draft.version,
                None => self
                    .s
                    .file
                    .window
                    .state
                    .related_versions
                    .get(&(table, record))
                    .copied(),
            };
            let root = {
                let (window, provider) = self.wp();
                let record = window
                    .current(provider)
                    .map_err(data_error)?
                    .ok_or(ErrorCode::RECORD_MISSING)?
                    .1
                    .id;
                om_data::ContainerEditBaseline {
                    layout,
                    record,
                    version: window.state.record_version,
                }
            };
            let prepared = self
                .p()
                .prepare_related_container_edit(
                    field.occurrence,
                    table,
                    i64::try_from(record)
                        .map_err(|_| HostError::Error(ErrorCode::RECORD_MISSING))?,
                    version,
                    def.id,
                    Some(root),
                )
                .map_err(|e| self.failed(e))?;
            prepared.then_some((record, version))
        } else {
            None
        };
        if mixed {
            if let super::portals::Target::Draft(index) = target {
                let reserved = self.s.file.window.state.uncommitted.related[index]
                    .reserved
                    .ok_or(ErrorCode::UNAVAILABLE)?;
                if !self
                    .p()
                    .prepare_related_container_edit(
                        field.occurrence,
                        table,
                        reserved,
                        None,
                        def.id,
                        None,
                    )
                    .map_err(|e| self.failed(e))?
                {
                    return Err(ErrorCode::UNAVAILABLE.into());
                }
            }
        }
        let reference = self
            .p()
            .related_container_reference(table, def.id, &container)
            .map_err(|e| self.failed(e))?;
        let text = reference
            .clone()
            .unwrap_or_else(|| container.text.as_str().to_owned());
        let cell = super::writes::cell_of(def.id, 1);
        let replaced = self.related_draft_cell_at(target, table, cell);
        if let Err(error) = self.set_related_target(target, field.occurrence, table, cell, text) {
            if let Some(reference) = &reference {
                let _ = self.p().adjust_related_container(table, reference, -1);
            }
            return Err(error);
        }
        if let Some((record, version)) = prepared {
            let open = &mut self.s.file.window.state.uncommitted;
            open.related_attachments.insert((table, record), cell);
            if let Some(draft) = open
                .related
                .iter_mut()
                .find(|d| d.table == table && d.record == Some(record))
            {
                draft.version = version;
            }
        }
        if mixed {
            self.stage_mixed_attachment_rows()?;
            let window = self.s.file.window.id();
            let carried = self.p().carried_record_sessions();
            self.s.file.window.state.uncommitted.related_recoverable = carried.contains(&window);
        }
        if let Some(replaced) = replaced.filter(|r| om_data::containers::reference(r).is_some()) {
            let _ = self.p().adjust_related_container(table, &replaced, -1);
        }
        Ok(Value::Container(container))
    }

    /// Stage accepted drafts through the ordinary provider operation before
    /// another owner's acquisition. This holds rows; it never commits them.
    fn stage_mixed_attachment_rows(&mut self) -> HostResult {
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(ErrorCode::RECORD_MISSING)?;
        let root = self
            .s
            .file
            .window
            .state
            .record
            .ok_or(ErrorCode::RECORD_MISSING)?;
        let open = self.s.file.window.state.uncommitted.clone();
        if (!open.new && open.attachment_field.is_none())
            || !open.session
            || open.saved
            || open.related.iter().any(|d| d.deleted)
        {
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        let fields = self.s.file.window.state.drafts.clone();
        self.p()
            .write_record(&om_data::ScriptWrite {
                layout,
                table: None,
                record: Some(root),
                fields,
                cells: open.cells.clone(),
                typed: Default::default(),
                skip_validation: false,
                privilege_set: None,
                record_number: None,
                prepared: None,
                single_field_step: false,
            })
            .map_err(|e| self.failed(e))?;
        for (index, draft) in open.related.iter().enumerate() {
            let record = draft
                .record
                .map(i64::try_from)
                .transpose()
                .map_err(|_| HostError::Error(ErrorCode::RECORD_MISSING))?;
            let reserved = draft
                .reserved
                .filter(|id| self.p().related_record_reserved(draft.table, *id));
            if draft.reserved.is_some() && reserved.is_none() {
                return Err(ErrorCode::UNAVAILABLE.into());
            }
            if let Some(record) = record {
                let version = draft.version.ok_or(ErrorCode::UNAVAILABLE)?;
                self.p().retain_related_record(draft.table, record, version);
            }
            let written = self
                .p()
                .write_record(&om_data::ScriptWrite {
                    layout,
                    table: Some(draft.table),
                    record: record.or(reserved),
                    cells: draft.cells.clone(),
                    fields: Default::default(),
                    typed: draft.typed.clone(),
                    skip_validation: false,
                    privilege_set: None,
                    record_number: None,
                    prepared: draft
                        .prepared
                        .as_ref()
                        .filter(|_| reserved.is_none())
                        .map(|p| p.id),
                    single_field_step: false,
                })
                .map_err(|e| self.failed(e))?;
            if record.is_none() {
                self.s.file.window.state.uncommitted.related[index].reserved = Some(written.id);
            }
        }
        Ok(())
    }

    /// The field an Insert step without a path puts its file into, checked
    /// before the user is asked: a stored container field of the window's
    /// own table, in Browse mode, on a connection that may change it. The
    /// write itself checks again (the provider's library and record rules).
    fn insertable(&mut self, field: Option<FieldRef>) -> HostResult<String> {
        let name = self.target_name(field)?;
        if self.s.file.window.state.mode != Mode::Browse || !self.is_container_field(&name) {
            return Err(ErrorCode(NOT_A_CONTAINER).into());
        }
        if let Some(def) = self.own_def(&name) {
            if def.global || def.class != FieldClass::Data || def.value_type != ValueType::Container
            {
                return Err(ErrorCode(FIELD_LOCKED).into());
            }
        }
        let layout = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(HostError::Error(ErrorCode::RECORD_MISSING))?;
        let capabilities = self
            .p()
            .record_capabilities(layout)
            .map_err(|e| self.failed(e))?;
        if !capabilities.update {
            note_failure("This record cannot be changed on this connection.".into());
            return Err(ErrorCode(FIELD_LOCKED).into());
        }
        Ok(name)
    }

    /// [`om_data::runtime::Fields::export_field_contents`].
    pub(super) fn export_contents(
        &mut self,
        field: Option<FieldRef>,
        paths: &[String],
        create_folders: bool,
    ) -> HostResult {
        // Without a field: the active field, or the active portal row's
        // related field (a container selected in a row, `run_step`).
        let field = match field {
            Some(field) => field,
            None => match self.related_container_target(None)? {
                Some(field) => field,
                None => self.active_field()?,
            },
        };
        let value = om_data::runtime::Fields::field_value_of(self, field)?;
        // The data first: nothing is asked or opened for data that
        // cannot be read.
        let (bytes, name) = match &value {
            Value::Container(c) if c.is_unloaded() => {
                let stored = self
                    .file(|f| om_data::host::library_data(f.source, c))
                    .unwrap_or(Err(om_data::calc::CalcError::NOT_SUPPORTED))
                    .map_err(|e| HostError::Error(ErrorCode(e.code())))?;
                if stored.external && !stored.intact {
                    return Err(ErrorCode(om_data::calc::CalcError::NOT_SUPPORTED.code()).into());
                }
                (
                    stored.data.data().to_vec(),
                    Some(c.text.as_str().to_owned()),
                )
            }
            Value::Container(c) if !c.streams.is_empty() => {
                (c.data().to_vec(), Some(c.text.as_str().to_owned()))
            }
            other => (utf16(&self.text(other)), None),
        };
        if !paths.is_empty() {
            let path = self.export_path(paths, create_folders)?;
            return write_export(&path, &bytes);
        }
        let reference = Some((field.occurrence, field.field));
        let context = self.dialog_context(reference, "")?;
        let dialog = ContainerFileDialog {
            title: "Export Field Contents".into(),
            field: self.field_label(reference, &context.name),
            export: true,
            path: format!("file:{}", output_name(name.as_deref(), &context.name)),
            chosen: None,
        };
        let line = match self.choose_file(dialog, Some(&context))? {
            Chosen::Native(path) => files::file_line(&path),
            Chosen::Line(line) => line,
        };
        let documents = self.s.file.data_files.folders.documents.clone();
        let lines = [line];
        // Before anything is created (folders included): replacing a file
        // is the user's choice.
        if let Some(existing) = files::candidates(&lines, &documents)?
            .into_iter()
            .find(|p| p.exists())
        {
            if existing.is_dir() {
                return Err(ErrorCode(files::code::CANNOT_CREATE).into());
            }
            let shown = existing
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.confirm(
                "Export Field Contents",
                format!("\u{201c}{shown}\u{201d} already exists. Do you want to replace it?"),
                "Replace",
            )?;
            self.check_context(&context)?;
        }
        let path = self.export_path(&lines, create_folders)?;
        write_export(&path, &bytes)
    }

    /// The window's file, record and focus now, for a dialog about
    /// `field` (named `name` without a calculation schema).
    fn dialog_context(
        &mut self,
        field: Option<(u64, u64)>,
        name: &str,
    ) -> HostResult<DialogContext> {
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).map_err(data_error)?
        };
        let name = match (field, self.schema()) {
            (Some((occurrence, id)), Some(schema)) => schema
                .field(occurrence, id)
                .map(|def| def.name.clone())
                .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?,
            _ => name.to_owned(),
        };
        let state = &self.s.file.window.state;
        Ok(DialogContext {
            file: self.s.file.id,
            epoch: self.s.epoch,
            window: self.s.file.window.id(),
            uuid: state.uuid.clone(),
            mode: state.mode,
            layout: state.layout,
            record,
            occurrence: self.s.file.occurrence,
            focus: state.focus.clone(),
            object: state.object,
            schema: self.schema().cloned().map(SchemaId),
            field,
            name,
        })
    }

    /// Whether what a dialog was opened for is still the step's: `Err`
    /// with 3, changing nothing, once it is not.
    fn check_context(&mut self, context: &DialogContext) -> HostResult {
        if self.s.interrupted || self.connection_lost() {
            return Err(ErrorCode::CANCELED.into());
        }
        let now = self.dialog_context(context.field, &context.name).ok();
        if now.as_ref() != Some(context) {
            note_failure(
                "The window, record or field changed while the file was being chosen; \
                 nothing was changed."
                    .into(),
            );
            return Err(ErrorCode::UNAVAILABLE.into());
        }
        Ok(())
    }

    /// Show `dialog` and wait: the file the user chose, `Err` with 1 when
    /// cancelled (or the session was interrupted), and with 3 when
    /// `context` no longer holds.
    fn choose_file(
        &mut self,
        dialog: ContainerFileDialog,
        context: Option<&DialogContext>,
    ) -> HostResult<Chosen> {
        let Some(Answer::ContainerFile(Some(answer))) = self.ask(Prompt::ContainerFile(dialog))
        else {
            return Err(ErrorCode::CANCELED.into());
        };
        if let Some(context) = context {
            self.check_context(context)?;
        }
        if let Some(path) = answer.chosen {
            return Ok(Chosen::Native(path));
        }
        if answer.path.trim().is_empty() {
            return Err(ErrorCode::CANCELED.into());
        }
        Ok(Chosen::Line(files::chosen_path(&answer.path)))
    }

    /// The definition of the window's own field `name`.
    fn own_def(&self, name: &str) -> Option<om_data::host::FieldDef> {
        let occurrence = self.s.file.occurrence?;
        self.schema()?
            .table_of(occurrence)?
            .fields
            .values()
            .find(|f| f.name == name)
            .cloned()
    }

    /// `Occurrence::Field` as the dialog names `field` (`name`).
    fn field_label(&self, field: Option<(u64, u64)>, name: &str) -> String {
        let occurrence = field
            .map(|(occurrence, _)| occurrence)
            .or(self.s.file.occurrence)
            .and_then(|o| Some(self.schema()?.occurrence(o)?.name.clone()));
        match occurrence {
            Some(occurrence) => format!("{occurrence}::{name}"),
            None => name.to_owned(),
        }
    }

    /// The field that has the focus, as a field of the window's occurrence.
    fn active_field(&mut self) -> HostResult<FieldRef> {
        let name = self.target_name(None)?;
        let missing = || HostError::Error(ErrorCode::FIELD_MISSING);
        let occurrence = self.s.file.occurrence.ok_or_else(missing)?;
        let schema = self.schema().cloned().ok_or_else(missing)?;
        let table = schema.table_of(occurrence).ok_or_else(missing)?;
        let def = table
            .fields
            .values()
            .find(|f| f.name == name)
            .ok_or_else(missing)?;
        Ok(FieldRef {
            occurrence,
            field: def.id,
            repetition: 1,
        })
    }
}

/// The file a `file:` URL names (Insert from URL), as the user's own
/// permissions allow it, like the data file steps: 100 when it cannot be
/// read or is larger than a response may be.
pub(super) fn read_url_file(
    path: &str,
    request: &om_data::runtime::UrlRequest,
    url: String,
) -> Result<om_url::Fetched, ErrorCode> {
    // `/C:/x` on Windows.
    let local = match path.strip_prefix('/') {
        Some(rest) if cfg!(windows) && rest.get(1..2) == Some(":") => rest,
        _ => path,
    };
    let body = read_regular_file(Path::new(local), om_url::MAX_FILE, ErrorCode::FILE_MISSING)?;
    Ok(om_url::Fetched {
        url,
        body,
        container_variable: om_url::container_variable(request),
        ..Default::default()
    })
}

/// Open without waiting for a FIFO, check the opened object (not a
/// path that can be swapped), then bound the read even if the file grows.
fn read_regular_file(path: &Path, limit: u64, too_large: ErrorCode) -> Result<Vec<u8>, ErrorCode> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| ErrorCode::FILE_MISSING)?;
    let metadata = file.metadata().map_err(|_| ErrorCode::FILE_MISSING)?;
    if !metadata.is_file() {
        return Err(ErrorCode::FILE_MISSING);
    }
    if metadata.len() > limit {
        return Err(too_large);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ErrorCode::FILE_MISSING)?;
    if bytes.len() as u64 > limit {
        return Err(too_large);
    }
    Ok(bytes)
}

/// The file at `path` as Insert step `kind` stores it.
fn read_inserted(path: &Path, kind: InsertKind) -> Result<Container, ErrorCode> {
    let bytes = read_regular_file(path, MAX_INSERT, ErrorCode(TOO_LARGE))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A PDF's header, `%PDF-`, is within its first 1024 bytes (readers
    // accept it there): a file only named `.pdf` is not one.
    let pdf_header = bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-");
    let value = om_data::calc::functions::typed_file(&name, &name, bytes, None);
    match kind {
        InsertKind::Picture if value.path_type != Some("image") => {
            return Err(ErrorCode(NOT_A_PICTURE));
        }
        InsertKind::Pdf
            if !pdf_header
                || !value
                    .main_stream()
                    .is_some_and(|(kind, _)| kind.0 == *b"PDF ") =>
        {
            return Err(ErrorCode(WRONG_FILE_TYPE));
        }
        // What the Browse player plays: a `movie` path, or a file named as
        // audio or video (an `.aac` is stored as a file).
        InsertKind::AudioVideo if !crate::document::is_media(value.path_type, &name) => {
            return Err(ErrorCode(WRONG_FILE_TYPE));
        }
        _ => {}
    }
    Ok(value)
}

/// The file a container dialog's answer names.
enum Chosen {
    /// Typed in the in-app dialog: a path list line.
    Line(String),
    /// Chosen in the platform's open panel: the path as the panel named it.
    Native(PathBuf),
}

/// The file name Export Field Contents' dialog offers: the container's own
/// (its last path component), the field's for text.
fn output_name(declared: Option<&str>, field: &str) -> String {
    let last = |name: &str| {
        name.rsplit(['/', '\\', ':'])
            .next()
            .map(str::trim)
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .map(str::to_owned)
    };
    match declared {
        Some(declared) => last(declared).unwrap_or_else(|| "Untitled".into()),
        None => last(field).map_or_else(|| "Untitled.txt".into(), |n| format!("{n}.txt")),
    }
}

/// Write an export's bytes, replacing an existing file only once they are
/// all written (module docs): 800 when they cannot be.
fn write_export(path: &Path, bytes: &[u8]) -> HostResult {
    crate::report::replace_file(path, bytes)
        .map_err(|_| ErrorCode(files::code::CANNOT_CREATE).into())
}

/// Text as UTF-16 little-endian with a byte order mark.
fn utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_the_steps_family_are_data_file_paths() {
        assert_eq!(
            in_family("image:Photos/a.png", "image").as_deref(),
            Some("file:Photos/a.png")
        );
        assert_eq!(
            in_family("imagemac:/HD/a.png", "image").as_deref(),
            Some("filemac:/HD/a.png")
        );
        assert_eq!(
            in_family("MovieWin:/C:/a.mov", "movie").as_deref(),
            Some("filewin:/C:/a.mov")
        );
        assert_eq!(in_family("file:a.png", "image"), None);
        assert_eq!(in_family("image:a.png", "file"), None);
        assert_eq!(in_family("a.png", "image").as_deref(), Some("a.png"));
        assert_eq!(in_family("imagery:a.png", "image"), None);
    }

    #[test]
    fn inserted_files_are_typed_by_their_name() {
        let dir = std::env::temp_dir().join(format!("om-insert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("dot.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let text = dir.join("note.txt");
        std::fs::write(&text, b"hello").unwrap();
        let picture = read_inserted(&png, InsertKind::Picture).unwrap();
        assert_eq!(picture.text.as_str(), "dot.png");
        assert_eq!(&picture.streams[0].0 .0, b"PNGf");
        assert_eq!(picture.path_type, Some("image"));
        let file = read_inserted(&text, InsertKind::File).unwrap();
        assert_eq!(&file.streams[0].0 .0, b"FILE");
        assert_eq!(
            read_inserted(&text, InsertKind::Picture).unwrap_err(),
            ErrorCode(NOT_A_PICTURE)
        );
        assert_eq!(
            read_inserted(&dir.join("missing"), InsertKind::File).unwrap_err(),
            ErrorCode::FILE_MISSING
        );
        // A PDF is one by its header, not its name alone; audio and video
        // are what the Browse player plays, an `.aac` stored as a file.
        let pdf = dir.join("report.pdf");
        std::fs::write(&pdf, b"%PDF-1.7\n%%EOF\n").unwrap();
        assert!(read_inserted(&pdf, InsertKind::Pdf).is_ok());
        let named = dir.join("named.pdf");
        std::fs::write(&named, b"only text").unwrap();
        assert_eq!(
            read_inserted(&named, InsertKind::Pdf).unwrap_err(),
            ErrorCode(WRONG_FILE_TYPE)
        );
        let aac = dir.join("clip.aac");
        std::fs::write(&aac, b"\xff\xf1 not decoded").unwrap();
        assert!(read_inserted(&aac, InsertKind::AudioVideo).is_ok());
        assert_eq!(
            read_inserted(&text, InsertKind::AudioVideo).unwrap_err(),
            ErrorCode(WRONG_FILE_TYPE)
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(utf16("hé"), [0xff, 0xfe, b'h', 0, 0xe9, 0]);
    }

    #[test]
    fn exports_offer_the_stored_name_and_replace_whole_files() {
        assert_eq!(output_name(Some("photo2.png"), "Photo"), "photo2.png");
        assert_eq!(output_name(Some("../a\\b:c.png"), "Photo"), "c.png");
        assert_eq!(output_name(Some(".."), "Photo"), "Untitled");
        assert_eq!(output_name(Some(""), "Photo"), "Untitled");
        assert_eq!(output_name(None, "Notes"), "Notes.txt");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.bin");
        std::fs::write(&path, b"old contents").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::Permissions::from_mode(0o640);
            std::fs::set_permissions(&path, mode).unwrap();
        }
        write_export(&path, &[0, 0xff, b'\n']).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0, 0xff, b'\n']);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640, "a replaced file keeps its permissions");
        }
        // A folder in the way is refused and left alone; no staging file
        // stays behind.
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert!(matches!(
            write_export(&folder, b"x"),
            Err(HostError::Error(ErrorCode(files::code::CANNOT_CREATE)))
        ));
        assert!(folder.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn regular_file_reads_are_bounded_and_reject_other_object_types() {
        let dir = std::env::temp_dir().join(format!("om-regular-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data");
        std::fs::write(&path, b"hello").unwrap();
        assert_eq!(
            read_regular_file(&path, 5, ErrorCode(TOO_LARGE)).unwrap(),
            b"hello"
        );
        assert_eq!(
            read_regular_file(&path, 4, ErrorCode(TOO_LARGE)).unwrap_err(),
            ErrorCode(TOO_LARGE)
        );
        assert_eq!(
            read_regular_file(&dir, 5, ErrorCode(TOO_LARGE)).unwrap_err(),
            ErrorCode::FILE_MISSING
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg(unix)]
    fn local_file_reads_refuse_devices_and_fifos_without_waiting_for_a_writer() {
        assert_eq!(
            read_regular_file(Path::new("/dev/zero"), 16, ErrorCode::FILE_MISSING).unwrap_err(),
            ErrorCode::FILE_MISSING
        );
        let dir = std::env::temp_dir().join(format!("om-fifo-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        // No writer exists. Opening a blocking read would hang here.
        assert_eq!(
            read_regular_file(&fifo, 16, ErrorCode::FILE_MISSING).unwrap_err(),
            ErrorCode::FILE_MISSING
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
