//! Import Records (35) and Export Records (36) in server scripts, through
//! `om_transfer`, with FileMaker Server's folders (oracle x01, x02 on
//! FileMaker Server 26.0.3; script-steps.md, "Implementation notes
//! (import/export)"):
//!
//! - Paths name files beneath the Documents and the session's temporary
//!   folders only, as [`super::files`] resolves and opens them; no usable
//!   path is error 800 for an export, 100 for an import.
//! - An export writes the first path whose folder exists (or is created,
//!   with "Create folders"), replacing a file there; an empty found set
//!   writes nothing. Save Records as Excel is error 3, as on FileMaker
//!   Server.
//! - An import reads the first path naming a file, up to
//!   `om_transfer::import::limit::SOURCE_BYTES`. Its rows wait in the
//!   session's transaction (or one of its own, written at the end in one
//!   commit), so a row the file's rules refuse is left out and the others
//!   are written together; the step then fails with 729. Each row gets
//!   the file's rules with the import's options (`Entry::import`) as it
//!   is staged and is written as they left it: neither the import's commit
//!   nor Commit Transaction applies its own options to it. A table whose
//!   rules the server cannot enforce refuses the step with 3.
//! - An aborted script, or one past its time limit, stops an import or
//!   export at the next row or record with error 1: the import's rows are
//!   dropped (its own transaction, or taken out of the script's) and the
//!   export writes no file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;

use om_calc::eval::{FieldId, Storage};
use om_calc::CalcError;
use om_data::rules::Entry;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, DataType, FieldKind, OwnedNode};
use om_protocol::write::CreateRecord;
use om_scripts::runtime::{
    ErrorCode, HostError, HostResult, Records, TransactionOptions, TransferCall,
};
use om_transfer::export::{
    self, ExportContext, ExportRecord, ExportRequest, ExportSource, FieldInfo,
};
use om_transfer::import::{self, FieldWrite, ImportRequest, ImportTarget, TargetField};
use om_transfer::spec::{ProfileBody, XmlSource};
use om_transfer::text::ExportValue;
use om_transfer::{
    ExportOptions, FieldFlags, FieldType, FileType, ImportOptions, Profile, ProfileEx,
    TransferError,
};

use super::{cell_id, Change, OpenRecord, Ruled, ServerHost, Transaction, Window};
use crate::{modcount, writes};

/// FileMaker errors of the folders.
const FILE_MISSING: i32 = 100;
const CANNOT_CREATE: i32 = 800;

fn host_error(e: TransferError) -> HostError {
    HostError::Error(ErrorCode(e.code))
}

/// A row's write error as the import takes it: a rule OpenMaker does not
/// support checking ([`HostError::Unsupported`], a value list it cannot
/// key, #571) ends the import; any other refuses the row.
fn transfer_error(e: HostError) -> TransferError {
    match e {
        HostError::Error(code) => TransferError::new(code.0, "the record could not be written"),
        HostError::Unsupported => {
            TransferError::unsupported("a rule of the record cannot be checked")
        }
        HostError::Pending => TransferError::new(3, "the record could not be written"),
    }
}

fn calc_error(e: CalcError) -> TransferError {
    TransferError::new(e.code(), "a value could not be read")
}

impl ServerHost {
    /// Export Records (module docs).
    pub(super) fn export_step(&mut self, call: &TransferCall<'_>) -> HostResult {
        if call.id == 143 {
            return Err(HostError::Unsupported);
        }
        // Every built-in set may export (session-identity.md 2.7).
        self.require_view()?;
        self.leave()?;
        let Some(order) = call.export_order.as_deref() else {
            return Err(ErrorCode(om_transfer::code::EXPORT_ORDER_INVALID).into());
        };
        let options = ExportOptions::decode(order).map_err(|_| ErrorCode::INVALID)?;
        let profile = match call.profile.as_deref() {
            Some(bytes) => Profile::decode(bytes).map_err(|_| ErrorCode::INVALID)?,
            None => Profile::new(FileType::TabSeparated),
        };
        if matches!(profile.file_type, FileType::FileMaker | FileType::Other(_)) {
            return Err(HostError::Unsupported);
        }
        let window = self.window()?.clone();
        if window.found.is_empty() {
            return Ok(());
        }
        let destination = self
            .destination(&call.paths, call.create_folders)
            .ok_or(ErrorCode(CANNOT_CREATE))?;
        let request = ExportRequest::new(options, profile);
        let mut source = ServerExport {
            host: self,
            window,
            groups: HashMap::new(),
            positions: HashMap::new(),
        };
        let bytes = export::export(&mut source, &request).map_err(host_error)?;
        if let Some(bytes) = bytes {
            destination
                .write(&bytes)
                .map_err(|_| ErrorCode(CANNOT_CREATE))?;
        }
        Ok(())
    }

    /// Import Records (module docs).
    pub(super) fn import_step(&mut self, call: &TransferCall<'_>) -> HostResult {
        self.require_edit()?;
        self.leave()?;
        // A folder of files, ODBC and FileMaker sources are not imported
        // here; FileMaker Server refuses folders too.
        if call.source_kind == 2 {
            return Err(HostError::Unsupported);
        }
        let Some(order) = call.import_order.as_deref() else {
            return Err(ErrorCode(om_transfer::code::IMPORT_ORDER_INVALID).into());
        };
        let options = ImportOptions::decode(order).map_err(|_| ErrorCode::INVALID)?;
        let profile = match call.profile.as_deref() {
            Some(bytes) => Profile::decode(bytes).map_err(|_| ErrorCode::INVALID)?,
            None => Profile::new(FileType::TabSeparated),
        };
        if matches!(
            profile.file_type,
            FileType::FileMaker | FileType::Folder | FileType::Odbc
        ) {
            return Err(HostError::Unsupported);
        }
        let profile_ex = call
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
        if paths.is_empty() {
            if let ProfileBody::XmlImport {
                xml: XmlSource::File(name),
                ..
            } = &profile.body
            {
                paths.push(name.clone());
            }
        }
        let file = self
            .open_for_reading(&paths)
            .ok_or(ErrorCode(FILE_MISSING))?;
        // One byte past the limit, so that the import refuses a larger
        // file without it being read whole.
        let mut bytes = Vec::new();
        file.take(import::limit::SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ErrorCode(FILE_MISSING))?;
        // A table whose rules the server cannot enforce refuses the step,
        // as other script writes to it, before any row is read (rather
        // than refusing every row, 729).
        if let Some(table) = options.table.id().and_then(|o| self.schema.table_of(o)) {
            self.plan(table)?;
        }
        let request = ImportRequest {
            options,
            profile,
            profile_ex,
            field_flags,
        };
        let own = self.transaction.is_none();
        if own {
            self.transaction = Some(Transaction::new(TransactionOptions {
                skip_validation: true,
                override_ess_locking: false,
                skip_auto_enter: true,
            }));
        }
        let locale = self.locale.clone();
        let mut target = ServerImport {
            host: self,
            undo: Vec::new(),
            touched: BTreeSet::new(),
            locked: Vec::new(),
        };
        let result = import::import(&mut target, &request, &bytes, &locale);
        match result {
            Err(e) => {
                // Nothing of an import that failed or was aborted is
                // written: its own transaction is dropped, or its rows
                // leave the script's.
                if own {
                    self.discard_transaction();
                } else {
                    target.take_back();
                }
                Err(host_error(e))
            }
            Ok(summary) => {
                if own {
                    // The rows were checked with the import's rules when
                    // they were staged ([`Ruled`]): the commit writes them
                    // as they are.
                    self.commit_transaction(true)?;
                }
                match summary.error_code() {
                    0 => Ok(()),
                    code => Err(ErrorCode(code).into()),
                }
            }
        }
    }

    /// A field's type and kind as transfers see them.
    fn transfer_type(data_type: DataType) -> FieldType {
        match data_type {
            DataType::Number => FieldType::Number,
            DataType::Date => FieldType::Date,
            DataType::Time => FieldType::Time,
            DataType::Timestamp => FieldType::Timestamp,
            DataType::Container => FieldType::Container,
            DataType::Text | DataType::Unknown(_) => FieldType::Text,
        }
    }

    fn transfer_kind(def: &om_hbam::Field) -> om_transfer::FieldKind {
        match def.kind {
            FieldKind::Calculation => om_transfer::FieldKind::Calculation,
            FieldKind::Summary => om_transfer::FieldKind::Summary,
            _ if Self::is_global(def) => om_transfer::FieldKind::Global,
            _ => om_transfer::FieldKind::Normal,
        }
    }
}

/// The session's window as an export reads it.
struct ServerExport<'h> {
    host: &'h mut ServerHost,
    window: Window,
    /// Group rows' summaries, computed once per (summary, break field)
    /// over the found set: each record's group value
    /// ([`ServerHost::group_summaries`]).
    groups: HashMap<(FieldId, FieldId), Vec<om_calc::Value>>,
    /// Each record's position in the found set.
    positions: HashMap<u64, usize>,
}

impl ServerExport<'_> {
    fn value(
        &self,
        occurrence: u64,
        table: u64,
        record: u64,
        field: u64,
        rep: u32,
    ) -> Result<ExportValue, TransferError> {
        let stored = matches!(
            self.host.field_info(occurrence, field).map(|i| i.storage),
            Some(Storage::Stored)
        );
        if stored {
            let cell = cell_id(field, rep).unwrap_or(field);
            return Ok(ExportValue::Stored(
                self.host.stored_text(table, record, cell),
            ));
        }
        self.host
            .read_field(occurrence, record, field, rep)
            .map(ExportValue::Computed)
            .map_err(calc_error)
    }
}

impl ExportSource for ServerExport<'_> {
    /// The script was aborted or ran out of time: the export stops and
    /// writes no file.
    fn interrupted(&mut self) -> bool {
        self.host.aborted()
    }

    fn context(&mut self) -> om_transfer::Result<ExportContext> {
        let host = &self.host;
        let created = jiff::Timestamp::now()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let version = host
            .get_value(super::selector::APPLICATION_VERSION)
            .to_text(&host.cx())
            .into_string();
        Ok(ExportContext {
            file_name: host.hosted.name.clone(),
            layout_name: String::new(),
            total_records: host.all_records(self.window.table).len() as u64,
            product_build: "08-26-2026".into(),
            product_name: "FileMaker".into(),
            product_version: version,
            date_format: date_format(&host.locale),
            time_format: "h:mm:ss a".into(),
            locale: host.locale.clone(),
            created,
        })
    }

    fn field(&mut self, occurrence: u64, field: u64) -> om_transfer::Result<FieldInfo> {
        let (_, def) = self
            .host
            .schema
            .field(occurrence, field)
            .ok_or_else(|| TransferError::new(102, "The field is missing."))?;
        let occurrence_name = self
            .host
            .schema
            .catalog
            .table_occurrences
            .iter()
            .find(|o| o.id == occurrence)
            .map(|o| o.name.clone())
            .unwrap_or_default();
        Ok(FieldInfo {
            name: def.name.clone(),
            occurrence: occurrence_name,
            related: occurrence != self.window.occurrence,
            ty: ServerHost::transfer_type(def.data_type),
            kind: ServerHost::transfer_kind(def),
            repetitions: ServerHost::repetitions(def),
            empty_ok: true,
        })
    }

    fn records(&mut self) -> om_transfer::Result<Vec<ExportRecord>> {
        let table = self.window.table;
        Ok(self
            .window
            .found
            .iter()
            .map(|&id| ExportRecord {
                id,
                mod_count: self.host.with_record(table, id, |node| {
                    modcount::serial_value(node.and_then(|n| n.child(&[0xfc])))
                }),
            })
            .collect())
    }

    fn sort_fields(&mut self) -> om_transfer::Result<Vec<(u64, u64)>> {
        Ok(self
            .window
            .sort
            .as_ref()
            .map(|spec| {
                spec.fields
                    .iter()
                    .filter_map(|e| Some((e.field.occurrence?, e.field.field)))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn values(
        &mut self,
        record: u64,
        occurrence: u64,
        field: u64,
    ) -> om_transfer::Result<Vec<Vec<ExportValue>>> {
        let (table, def) = self
            .host
            .schema
            .field(occurrence, field)
            .ok_or_else(|| TransferError::new(102, "The field is missing."))?;
        let reps = ServerHost::repetitions(def);
        let records = if occurrence == self.window.occurrence {
            vec![record]
        } else {
            self.host
                .related_set(self.window.occurrence, record, occurrence)
                .map_err(calc_error)?
        };
        records
            .into_iter()
            .map(|r| {
                (1..=reps)
                    .map(|rep| self.value(occurrence, table, r, field, rep))
                    .collect()
            })
            .collect()
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
        let Some((o, f)) = by else {
            // The grand summary, `GetSummary ( summary ; summary )`, as the
            // desktop reads it: a running total's total, a fraction's 1.
            let summary = FieldId { occurrence, field };
            let context = om_calc::eval::Context::record(self.window.occurrence, first);
            return om_data::summary::or_invalid(self.host.get_summary(&context, summary, summary))
                .map(ExportValue::Computed)
                .map_err(calc_error);
        };
        // The group's summary, as `GetSummary ( summary ; break )` on its
        // first record; every group's at once, so a grouped export reads the
        // found set once per summary rather than once per group.
        let key = (
            FieldId { occurrence, field },
            FieldId {
                occurrence: o,
                field: f,
            },
        );
        if !self.groups.contains_key(&key) {
            let values = self
                .host
                .group_summaries(key.0, key.1)
                .map_err(calc_error)?;
            self.groups.insert(key, values);
        }
        if self.positions.is_empty() {
            self.positions = self
                .window
                .found
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i))
                .collect();
        }
        Ok(self
            .positions
            .get(&first)
            .and_then(|&i| self.groups[&key].get(i))
            .cloned()
            .map_or_else(ExportValue::empty, ExportValue::Computed))
    }

    /// Each record's value as the window's found set shows it.
    fn record_summaries(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[u64],
    ) -> om_transfer::Result<Vec<ExportValue>> {
        let values = self
            .host
            .record_summaries(FieldId { occurrence, field }, records)
            .map_err(calc_error)?;
        Ok(values.into_iter().map(ExportValue::Computed).collect())
    }
}

/// The FMPXMLRESULT date format of a locale (`M/d/yyyy` in English).
fn date_format(locale: &om_calc::Locale) -> String {
    use om_calc::locale::DateOrder;
    let sep = locale.date_separator;
    match locale.date_order {
        DateOrder::DayMonthYear => format!("d{sep}M{sep}yyyy"),
        DateOrder::YearMonthDay => format!("yyyy{sep}M{sep}d"),
        DateOrder::MonthDayYear => format!("M{sep}d{sep}yyyy"),
    }
}

/// The session's records as an import writes them: rows join the
/// transaction ([`ServerHost::import_step`]), each checked with the
/// import's rules as it is staged and kept as they left it ([`Ruled`]).
struct ServerImport<'h> {
    host: &'h mut ServerHost,
    /// What the script's transaction held for each record before the
    /// import first staged it, to put back when the import fails.
    undo: Vec<((u64, u64), Option<Change>)>,
    touched: BTreeSet<(u64, u64)>,
    /// The records the import locked (new ids too).
    locked: Vec<(u64, u64)>,
}

impl ServerImport<'_> {
    fn table(&self, occurrence: u64) -> om_transfer::Result<u64> {
        self.host
            .schema
            .table_of(occurrence)
            .ok_or_else(|| TransferError::new(102, "The table occurrence is missing."))
    }

    /// Put an imported row's values into a record's cells.
    fn put(&self, record: &mut OpenRecord, writes: &[FieldWrite]) {
        let Some(table) = self.host.schema.catalog.table(record.table) else {
            return;
        };
        record.before_change();
        *record.context = self.host.step_context();
        for w in writes {
            let Some(def) = table.field(w.field) else {
                continue;
            };
            if !matches!(def.kind, FieldKind::Normal) || ServerHost::is_global(def) {
                continue;
            }
            let Some(cell) = cell_id(w.field, w.repetition) else {
                continue;
            };
            let key = uint_segment(cell);
            if w.text.is_empty() {
                record.node.remove(&[&key]);
            } else {
                record.node.set_child(super::values::cell(&key, &w.text));
            }
            record.changed_cell(cell);
        }
    }

    /// Check a row against the rules with the import's options
    /// ([`Entry::import`]: auto-entered values for the fields it does not
    /// set, "always" validation) and stage it in the transaction as they
    /// left it: neither the import's commit nor Commit Transaction applies
    /// other options to it.
    fn stage(
        &mut self,
        mut record: OpenRecord,
        writes: &[FieldWrite],
        auto_enter: bool,
        locked_here: bool,
    ) -> om_transfer::Result<()> {
        self.put(&mut record, writes);
        // A record the script changed earlier in its transaction gets the
        // import's rules for those changes too (OpenMaker's choice).
        let written = match self
            .host
            .apply_rules(&mut record, Entry::import(auto_enter))
        {
            Ok(written) => written,
            Err(e) => {
                if locked_here {
                    self.host.unlock_record(record.table, record.id);
                    self.locked.retain(|k| *k != (record.table, record.id));
                }
                return Err(transfer_error(e));
            }
        };
        if record.ruled.is_none() {
            record.ruled = Some(Box::new(Ruled {
                written,
                ..Ruled::default()
            }));
        }
        // Its node now holds the import's computed values: a preview from
        // before is not read again.
        record.preview.clear();
        let key = (record.table, record.id);
        let Some(transaction) = self.host.transaction.as_mut() else {
            return Err(TransferError::new(3, "The import has no transaction."));
        };
        let before = transaction.insert(key, Change::Write(record));
        if self.touched.insert(key) {
            self.undo.push((key, before));
        }
        Ok(())
    }

    /// Take the import's rows out of the script's transaction again: what
    /// it held before is back, and the locks the import took are released.
    fn take_back(self) {
        if let Some(transaction) = self.host.transaction.as_mut() {
            for (key, before) in self.undo.into_iter().rev() {
                match before {
                    Some(change) => {
                        transaction.insert(key, change);
                    }
                    None => {
                        transaction.remove(&key);
                    }
                }
            }
        }
        for (table, id) in self.locked {
            self.host.unlock_record(table, id);
        }
    }
}

impl ImportTarget for ServerImport<'_> {
    /// The script was aborted or ran out of time: the import stops at the
    /// next row and writes nothing.
    fn interrupted(&mut self) -> bool {
        self.host.aborted()
    }

    fn fields(&mut self, occurrence: u64) -> om_transfer::Result<Vec<TargetField>> {
        let table = self.table(occurrence)?;
        let def = self
            .host
            .schema
            .catalog
            .table(table)
            .ok_or_else(|| TransferError::new(106, "The table is missing."))?;
        Ok(def
            .fields
            .iter()
            .map(|f| TargetField {
                id: f.id,
                name: f.name.clone(),
                ty: ServerHost::transfer_type(f.data_type),
                kind: ServerHost::transfer_kind(f),
                repetitions: ServerHost::repetitions(f),
            })
            .collect())
    }

    fn found_set(&mut self, occurrence: u64) -> om_transfer::Result<Vec<u64>> {
        let table = self.table(occurrence)?;
        Ok(match self.host.window.as_ref() {
            Some(w) if w.table == table => w.found.clone(),
            _ => self.host.all_records(table),
        })
    }

    fn value(&mut self, occurrence: u64, record: u64, field: u64) -> om_transfer::Result<String> {
        let table = self.table(occurrence)?;
        Ok(self.host.stored_text(table, record, field))
    }

    fn create(
        &mut self,
        occurrence: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> om_transfer::Result<u64> {
        let table = self.table(occurrence)?;
        self.host.plan(table).map_err(transfer_error)?;
        let reply = writes::create_record(
            &self.host.hosted,
            self.host.user,
            &self.host.holder,
            &CreateRecord {
                table: vec![uint_segment(table)],
                count: 1,
                txn: self.host.user.txn,
            },
            writes::UNCAPPED,
            false,
        );
        let id = match reply.status {
            0 => reply
                .record
                .first()
                .and_then(|key| serial::decode(key))
                .ok_or_else(|| TransferError::new(-1, "No record id."))?,
            106 => return Err(TransferError::new(106, "The table is missing.")),
            _ => return Err(TransferError::new(301, "The record is in use.")),
        };
        let record = OpenRecord {
            table,
            id,
            node: OwnedNode::list(serial::encode(id), Vec::new()),
            new: true,
            changed: Default::default(),
            ruled: None,
            serials: BTreeMap::new(),
            context: Box::new(self.host.step_context()),
            relooked: false,
            relookup: false,
            created: None,
            preview: BTreeMap::new(),
        };
        self.locked.push((table, id));
        self.stage(record, writes, auto_enter, true)?;
        Ok(id)
    }

    fn update(
        &mut self,
        occurrence: u64,
        record: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> om_transfer::Result<()> {
        let table = self.table(occurrence)?;
        let staged = match self.host.pending(table, record) {
            // A Relookup the transaction holds for the record waits for
            // Commit Transaction's rules, which an import's own options
            // and values could not share without one overriding the other:
            // refused (3), the transaction failing, rather than either
            // half written (OpenMaker's limit, #647).
            Some(Change::Write(r)) if r.relookup => {
                return Err(TransferError::unsupported(
                    "The transaction holds a Relookup Field Contents of this record, \
                     which an import does not update.",
                ));
            }
            Some(Change::Write(r)) => Some(r.clone()),
            Some(Change::Delete) => return Err(TransferError::new(101, "The record is missing.")),
            None => None,
        };
        let (open, locked_here) = match staged {
            Some(r) => (r, false),
            None => {
                self.host
                    .lock_record(table, record)
                    .map_err(transfer_error)?;
                let Some(node) = self.host.committed_node(table, record) else {
                    self.host.unlock_record(table, record);
                    return Err(TransferError::new(101, "The record is missing."));
                };
                self.locked.push((table, record));
                (
                    OpenRecord {
                        table,
                        id: record,
                        node,
                        new: false,
                        changed: Default::default(),
                        ruled: None,
                        serials: BTreeMap::new(),
                        context: Box::new(self.host.step_context()),
                        relooked: false,
                        relookup: false,
                        created: None,
                        preview: BTreeMap::new(),
                    },
                    true,
                )
            }
        };
        self.stage(open, writes, auto_enter, locked_here)
    }

    fn set_found_set(&mut self, occurrence: u64, records: &[u64]) -> om_transfer::Result<()> {
        let table = self.table(occurrence)?;
        if let Some(w) = self.host.window.as_mut().filter(|w| w.table == table) {
            w.found = records.to_vec();
            w.current = 0;
            w.sort = None;
        }
        Ok(())
    }
}
