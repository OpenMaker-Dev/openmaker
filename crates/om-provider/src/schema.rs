//! Native schema reads, and Manage Database's saves to the host.
//!
//! Manage Database edits the schema as FileMaker Pro does
//! (docs/re/schema-path.md 2): opening it takes the schema lock `[08]`
//! and the three catalogs' locks, so a second user is told the schema is
//! in use (303); each change is made on this client's copy of the
//! catalogs (`om_data::schema_draft`) and nothing is sent; OK saves them
//! with FileMaker Pro's own transaction (each changed catalog's lock
//! upgraded, the catalog uploaded whole with op13 and released with
//! attention 2 so the other clients reload it, op73 after each field
//! catalog, DeleteTable for deleted tables) and then reads every uploaded
//! catalog back: the save is reported only when the host stores what was
//! sent. Cancel releases the locks. A catalog another client changed since
//! it was read is not overwritten (306).
//!
//! The host must be OpenMaker Server or a file opened from this computer,
//! and the account one it lets change the design (asked through the
//! authoring extension, `crate::authoring`). FileMaker Server is sent no
//! schema change: the transaction was replayed there by the RE probe but
//! OpenMaker's catalogs have not been checked against it.

use std::collections::BTreeMap;

use om_data::calc_field::{self, CalculationField, FormulaSettings};
use om_data::field_options::AutoEnterValue;
use om_data::meta::JoinOp;
use om_data::schema_draft::{CatalogPath, CommitPlan, SchemaCatalogs, SchemaDraft, Stamp};
use om_data::{
    DataError, DatabaseSchema, FieldKind, OccurrenceBox, SchemaCapabilities, SchemaChange,
    SchemaField, SchemaFieldKind, SchemaMatch, SchemaOccurrence, SchemaRelationship, SchemaTable,
};
use om_hbam::{Catalog, KeySegment, KeyTree, OwnedNode, SlotReader};
use om_protocol::authoring::catalog_revision;
use om_protocol::write::{LockReply, LOCK_WRITE};

use crate::authoring::Authoring;
use crate::{download_node, download_tree, failure, key_bytes, NativeProvider};

/// Said with every refused or failed schema change.
const UNCHANGED: &str = "No server definitions were changed.";

/// Lock types (write-path.md 4.1): exclusive, exclusive-reject (a table
/// being deleted) and "any" (to release a table lock whatever its type).
const LOCK_EXCLUSIVE: u16 = 10;
const LOCK_DELETE: u16 = 11;
const LOCK_ANY: u16 = 100;
/// The schema lock path `[08]` (`KGlobal`) and a table's records `[T, 05]`.
const SCHEMA_LOCK: u8 = 0x08;
const RECORDS: u8 = 0x05;
/// Attention codes of the releases (schema-path.md 9, 2.3a): a catalog
/// that changed, the schema lock after a save, a table's records.
const ATTENTION_CATALOG: u32 = 0x2;
const ATTENTION_SCHEMA: u32 = 0x3;
const ATTENTION_TABLE: u32 = 0x201;
/// How long FileMaker Pro waits for a table lock when it saves (2 s) and
/// for the schema lock (forever; the host answers at once regardless).
const TABLE_LOCK_TIMEOUT: u32 = 2;
const FOREVER: u32 = u32::MAX;
/// FileMaker's errors: 3 unsupported, 303 "Database schema is in use by
/// another user", 306 "Record modification ID does not match".
const UNSUPPORTED: i32 = 3;
const SCHEMA_IN_USE: i32 = 303;
const CHANGED_SINCE: i32 = 306;
/// DeleteTable (schema-path.md 3.4).
const DELETE_TABLE: u8 = 0x32;

/// A database schema and the exact field catalogs used to decode it.
/// Each table's fields and catalog form one observation; this does not
/// claim a transaction covering the entire database's metadata.
pub struct DatabaseSchemaSnapshot {
    /// The decoded tables, fields, occurrences and relationships.
    pub schema: DatabaseSchema,
    /// Stored field catalogs, keyed by table id. Missing catalogs are absent.
    pub field_catalogs: BTreeMap<u64, OwnedNode>,
}

/// Manage Database's open edit: the locks it holds on the host, as
/// `(path, transaction)`, and its copy of the catalogs.
pub(crate) struct SchemaEdit {
    schema_lock: u32,
    catalog_locks: Vec<(CatalogPath, u32)>,
    draft: SchemaDraft,
    capabilities: SchemaCapabilities,
}

impl NativeProvider {
    /// Read the schema together with the field catalogs that produced it.
    /// Revision tokens derived from these catalogs describe the returned
    /// fields even when another client edits the schema immediately after.
    pub fn database_schema_snapshot(&mut self) -> Result<DatabaseSchemaSnapshot, DataError> {
        let schema = self.read_database_schema()?;
        let mut field_catalogs = BTreeMap::new();
        if !schema.tables.is_empty() {
            let tree = download_tree(&self.schema_downloads)?;
            for table in &schema.tables {
                if let Some(catalog) = tree.at(&[table.id, 3]) {
                    field_catalogs.insert(table.id, catalog.to_owned_node());
                }
            }
        }
        Ok(DatabaseSchemaSnapshot {
            schema,
            field_catalogs,
        })
    }

    pub(crate) fn read_database_schema(&mut self) -> Result<DatabaseSchema, DataError> {
        self.require_open()?;
        let session = self.session.as_mut().expect("open session");
        let Some(root) = download_node(session, vec![key_bytes(3)])? else {
            // A metadata-only file need not have a table catalog.
            self.catalog.tables.clear();
            self.catalog.table_occurrences.clear();
            self.drop_calc_schema();
            self.invalidate_calc_data();
            self.schema_downloads.clear();
            *self.own_schema.get_mut() = None;
            return Ok(DatabaseSchema {
                relationships_decoded: true,
                ..Default::default()
            });
        };
        let mut downloads = vec![root];
        let tables = Catalog::from_tree(&download_tree(&downloads)?)
            .map_err(|error| failure("decode database tables", error))?;
        for table in &tables.tables {
            if let Some(fields) = download_node(session, vec![key_bytes(table.id), vec![3]])? {
                downloads.push(fields);
            }
        }
        let catalog = Catalog::from_tree(&download_tree(&downloads)?)
            .map_err(|error| failure("decode database fields", error))?;
        self.catalog.tables = catalog.tables;
        self.catalog.table_occurrences = catalog.table_occurrences;
        self.drop_calc_schema();
        self.invalidate_calc_data();
        self.schema_downloads = downloads;
        *self.own_schema.get_mut() = None;
        // Only calculation fields' formulas need the file's functions and
        // locale, which the calculation schema reads.
        let calculations = self.catalog.tables.iter().any(|table| {
            table
                .fields
                .iter()
                .any(|field| field.kind == om_hbam::FieldKind::Calculation)
        });
        let formulas = if calculations {
            self.formula_settings()
        } else {
            FormulaSettings::default()
        };
        schema_from_tree_with(&download_tree(&self.schema_downloads)?, &formulas)
    }

    /// The file's decimal separator and functions, which its schema's
    /// formulas are shown and compiled with. Without a calculation schema
    /// (a file that cannot be read for calculations) formulas show with a
    /// decimal point and no functions of the file.
    fn formula_settings(&mut self) -> FormulaSettings {
        self.calc_schema_cached()
            .map(|schema| FormulaSettings::from_calc_schema(&schema))
            .unwrap_or_default()
    }

    /// The schema as Manage Database shows it before it is opened for
    /// editing: [`Self::read_database_schema`] with what may be saved.
    pub(crate) fn database_schema_for_editing(&mut self) -> Result<DatabaseSchema, DataError> {
        let mut schema = self.read_database_schema()?;
        schema.capabilities = self.schema_capabilities();
        Ok(schema)
    }

    /// What Manage Database may save on this connection: tables, fields and
    /// occurrences where the host lets the account change the design;
    /// nothing otherwise, with the reason.
    fn schema_capabilities(&mut self) -> SchemaCapabilities {
        let read_only = |reason: &str| SchemaCapabilities {
            unavailable_reason: Some(format!(
                "Schema definitions are read from the server. {reason}"
            )),
            ..Default::default()
        };
        if self.read_only() {
            return read_only("This file is open read-only.");
        }
        // OpenMaker Server lets an account upload catalogs exactly when it
        // lets it author fields: both need [Full Access] on the host.
        match self.field_authoring() {
            Ok(Authoring::Available { .. }) => SchemaCapabilities {
                tables: true,
                fields: true,
                occurrences: true,
                relationships: false,
                unavailable_reason: None,
            },
            Ok(Authoring::Denied(reason)) => read_only(&reason),
            Ok(Authoring::Unavailable(_)) => read_only(
                "OpenMaker saves schema changes to OpenMaker Server and to files opened from \
                 this computer; saving them to FileMaker Server has not been verified yet.",
            ),
            Err(e) => read_only(&format!(
                "The host could not be asked what may be saved: {e}"
            )),
        }
    }

    /// Open Manage Database for editing: take the schema lock and the
    /// catalogs' locks, then read the catalogs under them. Read-only
    /// (no locks) where nothing may be saved.
    pub(crate) fn begin_schema_edit_on_host(&mut self) -> Result<DatabaseSchema, DataError> {
        self.require_open()?;
        if self.schema_edit.is_some() {
            return self.schema_edit_view();
        }
        let capabilities = self.schema_capabilities();
        if !(capabilities.tables || capabilities.fields || capabilities.occurrences) {
            let mut schema = self.read_database_schema()?;
            schema.capabilities = capabilities;
            return Ok(schema);
        }
        let schema_lock = self.take_transaction();
        self.lock(&[vec![SCHEMA_LOCK]], schema_lock, LOCK_WRITE, FOREVER)?;
        let mut catalog_locks = Vec::new();
        for which in [
            CatalogPath::Tables,
            CatalogPath::Occurrences,
            CatalogPath::Relationships,
        ] {
            let txn = self.take_transaction();
            if let Err(e) = self.lock(&which.path(), txn, LOCK_WRITE, 0) {
                self.release(&catalog_locks, schema_lock, 0);
                return Err(e);
            }
            catalog_locks.push((which, txn));
        }
        let base = match self.read_schema_catalogs() {
            Ok(base) => base,
            Err(e) => {
                self.release(&catalog_locks, schema_lock, 0);
                return Err(e);
            }
        };
        let formulas = self.formula_settings();
        let empty = self.empty_tables(&base);
        self.schema_edit = Some(SchemaEdit {
            schema_lock,
            catalog_locks,
            // A file on this computer is hosted by OpenMaker's own engine,
            // which converts stored values for a type change (#441).
            draft: SchemaDraft::new(base)
                .with_formulas(formulas)
                .with_empty_tables(empty)
                .with_host_conversion(self.local.is_some()),
            capabilities,
        });
        self.schema_edit_view()
    }

    /// The tables of a file on this computer that hold no record, by key: a
    /// field of theirs may change its type to a container with no value to
    /// leave as it is (#1268), which the
    /// save checks again under the tables' locks
    /// ([`NativeProvider::check_still_empty`]). None for a hosted file:
    /// another client there could add a record meanwhile. A table whose
    /// records cannot be read counts as holding some.
    fn empty_tables(&mut self, base: &SchemaCatalogs) -> std::collections::BTreeSet<Vec<u8>> {
        let mut empty = std::collections::BTreeSet::new();
        if self.local.is_none() {
            return empty;
        }
        for table in base.fields.keys() {
            let Some(id) = om_hbam::slot::be_uint(table) else {
                continue;
            };
            let session = self.session.as_mut().expect("open session");
            if crate::holds_no_record(session, id).unwrap_or(false) && !self.holds_records_of(id) {
                empty.insert(table.clone());
            }
        }
        empty
    }

    /// Under the locks the save holds on the records of the tables whose
    /// fields change (`[T, 05]`, which no other connection's record lock,
    /// and so no other connection's new record, can share): the tables
    /// `edit` relies on holding no record hold none, in the host's record
    /// list and in this connection's open transaction. Otherwise the save
    /// is refused, since a value would be left unconverted (#441).
    fn check_still_empty(&mut self, edit: &SchemaEdit, plan: &CommitPlan) -> Result<(), DataError> {
        for table in &edit.draft.relies_on_empty_tables() {
            // Its fields change, so the save locked its records; were it
            // not locked, emptiness could not be made sure of.
            let id = om_hbam::slot::be_uint(table)
                .filter(|_| plan.field_tables.contains(table))
                .ok_or_else(|| {
                    DataError::with_code(
                        UNSUPPORTED,
                        format!(
                            "OpenMaker cannot make sure the table {table:02x?} still holds no \
                             record. {UNCHANGED}"
                        ),
                    )
                })?;
            let session = self.session.as_mut().expect("open session");
            if !crate::holds_no_record(session, id)? || self.holds_records_of(id) {
                return Err(DataError::with_code(
                    CHANGED_SINCE,
                    format!(
                        "A record was added to a table since Manage Database read it, and this \
                         change gives a field of it a type that needs it to hold no value \
                         (#1268). {UNCHANGED} Cancel and open \
                         Manage Database again."
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Stage `change` on the open edit. Nothing is sent.
    pub(crate) fn stage_schema_change(
        &mut self,
        change: SchemaChange,
    ) -> Result<DatabaseSchema, DataError> {
        self.require_open()?;
        let edit = self.schema_edit.as_mut().ok_or_else(|| {
            DataError::with_code(UNSUPPORTED, "Manage Database is not open for editing.")
        })?;
        if !edit.capabilities.tables {
            return Err(DataError::with_code(
                UNSUPPORTED,
                edit.capabilities
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "This schema cannot be changed here.".into()),
            ));
        }
        edit.draft.apply(&change)?;
        self.schema_edit_view()
    }

    /// Drop the staged changes and release the edit's locks.
    pub(crate) fn cancel_schema_edit(&mut self) -> Result<(), DataError> {
        if let Some(edit) = self.schema_edit.take() {
            if self.session.is_some() {
                self.release(&edit.catalog_locks, edit.schema_lock, 0);
            }
        }
        Ok(())
    }

    /// The draft as the dialog shows it.
    fn schema_edit_view(&self) -> Result<DatabaseSchema, DataError> {
        let edit = self.schema_edit.as_ref().expect("an open edit");
        let mut schema = schema_from_catalogs(edit.draft.catalogs(), edit.draft.formulas())?;
        schema.capabilities = edit.capabilities.clone();
        Ok(schema)
    }

    /// Save the staged changes with FileMaker Pro's transaction
    /// (schema-path.md 2.3, 2.3a) and read them back.
    pub(crate) fn commit_schema_edit(&mut self) -> Result<DatabaseSchema, DataError> {
        self.require_open()?;
        let Some(edit) = self.schema_edit.take() else {
            return self.database_schema_for_editing();
        };
        if !edit.draft.is_changed() {
            self.release(&edit.catalog_locks, edit.schema_lock, 0);
            return self.database_schema_for_editing();
        }
        match self.save_draft(&edit) {
            Ok(()) => {
                // The schema lock goes with attention 3, which tells the
                // other clients the schema changed (schema-path.md 2.3a).
                self.release(&edit.catalog_locks, edit.schema_lock, ATTENTION_SCHEMA);
                let mut schema = self.database_schema_for_editing()?;
                verify_saved(&edit.draft, &schema)?;
                schema.capabilities = self.schema_capabilities();
                Ok(schema)
            }
            // Nothing was sent: the edit stays open with its locks and
            // changes, for another OK or Cancel.
            Err(Failure::NotSent(error)) => {
                self.schema_edit = Some(edit);
                Err(error)
            }
            Err(Failure::Partial(error)) => {
                self.release(&edit.catalog_locks, edit.schema_lock, ATTENTION_SCHEMA);
                Err(error)
            }
        }
    }

    fn save_draft(&mut self, edit: &SchemaEdit) -> Result<(), Failure> {
        let time = self
            .session
            .as_mut()
            .expect("open session")
            .host_timestamp()
            .map_err(|e| {
                Failure::NotSent(DataError::new(format!(
                    "The host's clock could not be read: {e}. {UNCHANGED}"
                )))
            })?;
        let stamp = Stamp {
            user: om_data::host::machine::user_name(),
            account: self.account_name.clone(),
            time,
        };
        let mut failed = None;
        let mut uuid = || {
            let mut bytes = [0u8; 16];
            if let Err(e) = getrandom::getrandom(&mut bytes) {
                failed = Some(e);
            }
            bytes
        };
        let plan = edit.draft.finish(&stamp, &mut uuid);
        if let Some(e) = failed {
            return Err(Failure::NotSent(DataError::new(format!(
                "No random UUIDs could be made: {e}. {UNCHANGED}"
            ))));
        }
        // The field catalogs FileMaker Pro holds while the dialog shows
        // them; locked now, before anything is checked or sent.
        let mut field_locks = Vec::new();
        let mut table_locks = Vec::new();
        let outcome = (|| {
            let not_sent = |e: DataError| Failure::NotSent(e);
            for table in &plan.field_tables {
                let which = CatalogPath::Fields(table.clone());
                let txn = self.take_transaction();
                self.lock(&which.path(), txn, LOCK_WRITE, 0)
                    .map_err(not_sent)?;
                field_locks.push((which, txn));
            }
            self.check_unchanged(edit.draft.base(), &plan)
                .map_err(not_sent)?;
            // The tables whose fields change, so no record of theirs is
            // being edited while the catalog is replaced.
            for table in &plan.field_tables {
                let path = vec![table.clone(), vec![RECORDS]];
                self.lock(&path, 0, LOCK_WRITE, TABLE_LOCK_TIMEOUT)
                    .map_err(not_sent)?;
                table_locks.push(path);
            }
            self.check_still_empty(edit, &plan).map_err(not_sent)?;
            self.send_plan(&plan, edit, &field_locks)
        })();
        let session = self.session.as_mut().expect("open session");
        for (which, txn) in &field_locks {
            let path = which.path();
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let _ = session.unlock(&refs, *txn, LOCK_WRITE, 0);
        }
        for path in &table_locks {
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let attention = if outcome.is_ok() { ATTENTION_TABLE } else { 0 };
            let _ = session.unlock(&refs, 0, LOCK_ANY, attention);
        }
        outcome
    }

    /// Refuse the save, sending nothing, when the host's copy of a catalog
    /// the plan replaces is no longer the one the draft was made from.
    fn check_unchanged(
        &mut self,
        base: &SchemaCatalogs,
        plan: &CommitPlan,
    ) -> Result<(), DataError> {
        for (which, _) in &plan.catalogs {
            let before = match which {
                CatalogPath::Tables => base.tables.as_ref(),
                CatalogPath::Occurrences => base.occurrences.as_ref(),
                CatalogPath::Relationships => base.relationships.as_ref(),
                CatalogPath::Fields(table) => base.fields.get(table),
            };
            let now = self.download_catalog(which)?;
            let same = match (before, &now) {
                (Some(before), Some(now)) => catalog_revision(before) == catalog_revision(now),
                (None, None) => true,
                _ => false,
            };
            if !same {
                return Err(DataError::with_code(
                    CHANGED_SINCE,
                    format!(
                        "Another user changed the schema since Manage Database read it. \
                         {UNCHANGED} Cancel and open Manage Database again."
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Send the plan: each catalog upgraded, uploaded and released in
    /// FileMaker Pro's order, op73 after each field catalog, then the
    /// deleted tables.
    fn send_plan(
        &mut self,
        plan: &CommitPlan,
        edit: &SchemaEdit,
        field_locks: &[(CatalogPath, u32)],
    ) -> Result<(), Failure> {
        let mut sent = Vec::new();
        // Before the first upload's op13 nothing has changed; after it,
        // the error says what the host stored.
        let partial = |sent: &[String], e: DataError| {
            if sent.is_empty() {
                Failure::NotSent(DataError {
                    message: format!("{} {UNCHANGED}", e.message),
                    ..e
                })
            } else {
                Failure::Partial(DataError::new(format!(
                    "The schema was saved only in part ({} stored) before this failed: {} \
                     Open Manage Database again to see what the host has.",
                    sent.join(", "),
                    e.message
                )))
            }
        };
        for (which, catalog) in &plan.catalogs {
            let txn = edit
                .catalog_locks
                .iter()
                .chain(field_locks)
                .find(|(w, _)| w == which)
                .map(|(_, txn)| *txn)
                .expect("every uploaded catalog is locked");
            let path = which.path();
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let session = self.session.as_mut().expect("open session");
            let upgraded = session
                .upgrade_lock(&refs, txn, LOCK_WRITE, LOCK_EXCLUSIVE)
                .map_err(|e| failure("upgrade a catalog lock", e));
            match upgraded {
                Ok(LockReply::Granted { .. }) => {}
                Ok(other) => return Err(partial(&sent, refused_lock(&other, "a catalog"))),
                Err(e) => return Err(partial(&sent, e)),
            }
            let uploaded = session
                .upload_to_host(&refs, catalog)
                .map_err(|e| failure("upload a catalog", e));
            // Released whatever happened; attention 2 tells the other
            // clients to read it again.
            let _ = session.unlock(&refs, txn, LOCK_EXCLUSIVE, ATTENTION_CATALOG);
            if let Err(e) = uploaded {
                // The upload may have been stored in part.
                sent.push(format!("perhaps {}", catalog_name(which)));
                return Err(partial(&sent, e));
            }
            sent.push(catalog_name(which));
            if let CatalogPath::Fields(table) = which {
                session
                    .update_table(table)
                    .map_err(|e| partial(&sent, failure("ask the host to update the table", e)))?;
            }
        }
        for table in &plan.deleted_tables {
            self.delete_table_on_host(table)
                .map_err(|e| partial(&sent, e))?;
            sent.push(format!("the deletion of table {table:02x?}"));
        }
        Ok(())
    }

    /// DeleteTable `table` under its exclusive-reject lock (schema-path.md
    /// 3.4, 4.4). OpenMaker sends it after the table catalog no longer
    /// lists the table, so a failure leaves unreachable records, not a
    /// table without fields (FileMaker Pro sends it first).
    fn delete_table_on_host(&mut self, table: &[u8]) -> Result<(), DataError> {
        let txn = self.take_transaction();
        let path = vec![table.to_vec()];
        self.lock(&path, txn, LOCK_DELETE, FOREVER)?;
        let session = self.session.as_mut().expect("open session");
        let file_id = session
            .opened_file()
            .map(|f| f.reply.file_id)
            .ok_or_else(|| DataError::new("No hosted file is open."))?;
        let mut segment = vec![table.len() as u8];
        segment.extend(table);
        let leaf = |key: u8, value: Vec<u8>| OwnedNode::leaf(vec![key], value);
        let instruction = OwnedNode::list(
            om_hbam::serial::encode(1),
            vec![
                leaf(0x00, vec![DELETE_TABLE]),
                leaf(0x01, segment),
                leaf(0x04, file_id.to_be_bytes().to_vec()),
                leaf(
                    0x07,
                    om_crypto::query_access_key(1, session.connection_key(), file_id).to_vec(),
                ),
                leaf(0x0a, txn.to_be_bytes().to_vec()),
            ],
        );
        let program = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(vec![1], vec![instruction])],
        )
        .to_wire()
        .expect("a small program");
        let result = session.query(&program);
        let refs = [table];
        let _ = session.unlock(&refs, txn, LOCK_DELETE, 0);
        let reply = result.map_err(|e| failure("delete a table", e))?;
        let tree = KeyTree::from_slots(SlotReader::new(&reply.slots))
            .map_err(|e| failure("decode DeleteTable's answer", e))?;
        if let Some(error) = tree.root.child(&[0x00]) {
            return Err(DataError::new(format!(
                "The host refused to delete table {table:02x?} ({:02x?}).",
                error.value
            )));
        }
        Ok(())
    }

    /// Lock `path` for `txn`; a conflict is FileMaker's 303 naming who
    /// holds it.
    fn lock(
        &mut self,
        path: &[Vec<u8>],
        txn: u32,
        lock_type: u16,
        timeout: u32,
    ) -> Result<(), DataError> {
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let reply = self
            .session
            .as_mut()
            .expect("open session")
            .lock_typed(&refs, txn, lock_type, timeout)
            .map_err(|e| failure("lock the schema", e))?;
        match reply {
            LockReply::Granted { .. } => Ok(()),
            other => Err(refused_lock(&other, &lock_what(path))),
        }
    }

    /// Release the edit's catalog locks and its schema lock.
    fn release(&mut self, catalogs: &[(CatalogPath, u32)], schema_lock: u32, attention: u32) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        for (which, txn) in catalogs {
            let path = which.path();
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let _ = session.unlock(&refs, *txn, LOCK_WRITE, 0);
        }
        let _ = session.unlock(&[&[SCHEMA_LOCK]], schema_lock, LOCK_WRITE, attention);
    }

    /// The host's copy of a catalog now, `None` when it has none.
    fn download_catalog(&mut self, which: &CatalogPath) -> Result<Option<OwnedNode>, DataError> {
        let session = self.session.as_mut().expect("open session");
        let path = which.path();
        let Some(download) = download_node(session, path.clone())? else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&download))?;
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        Ok(tree.node(&refs).map(|n| n.to_owned_node()))
    }

    /// The catalogs as the host stores them, read under the edit's locks.
    fn read_schema_catalogs(&mut self) -> Result<SchemaCatalogs, DataError> {
        let schema = self.read_database_schema()?;
        let tree = download_tree(&self.schema_downloads)?;
        let at = |path: &[u64]| tree.at(path).map(|n| n.to_owned_node());
        let mut fields = BTreeMap::new();
        for table in &schema.tables {
            if let Some(catalog) = at(&[table.id, 3]) {
                fields.insert(key_bytes(table.id), catalog);
            }
        }
        Ok(SchemaCatalogs {
            tables: at(&[3, 0x10]),
            occurrences: at(&[3, 0x11]),
            relationships: at(&[3, 0xfb]),
            fields,
        })
    }

    /// The connection's next transaction id ([`Self::new_transaction`]).
    fn take_transaction(&mut self) -> u32 {
        if let Some(session) = self.session.as_mut() {
            return session.take_transaction();
        }
        let txn = self.next_transaction;
        self.next_transaction = self.next_transaction.wrapping_add(1).max(1);
        txn
    }
}

/// Why a save failed: before anything was sent, or after the host stored
/// part of it.
enum Failure {
    NotSent(DataError),
    Partial(DataError),
}

/// The error for a lock the host did not grant.
fn refused_lock(reply: &LockReply, what: &str) -> DataError {
    match reply {
        LockReply::Refused { conflict, .. } if !conflict.users.is_empty() => DataError::with_code(
            SCHEMA_IN_USE,
            format!("{what} is in use by {}.", conflict.users.trim()),
        ),
        _ => DataError::with_code(SCHEMA_IN_USE, format!("{what} is in use by another user.")),
    }
}

fn lock_what(path: &[Vec<u8>]) -> String {
    match path {
        [only] if only == &[SCHEMA_LOCK] => "The database schema".into(),
        [_, records] if records == &[RECORDS] => "A record of a table whose fields changed".into(),
        _ => "The database schema".into(),
    }
}

fn catalog_name(which: &CatalogPath) -> String {
    match which {
        CatalogPath::Tables => "the tables".into(),
        CatalogPath::Occurrences => "the table occurrences".into(),
        CatalogPath::Relationships => "the relationships".into(),
        CatalogPath::Fields(table) => format!("the fields of table {table:02x?}"),
    }
}

/// The schema a draft's catalogs decode to, as the host would store them.
fn schema_from_catalogs(
    catalogs: &SchemaCatalogs,
    formulas: &FormulaSettings,
) -> Result<DatabaseSchema, DataError> {
    let wire = catalogs
        .to_root()
        .to_wire()
        .ok_or_else(|| DataError::new("The schema is too large to show."))?;
    let tree = KeyTree::from_slots(SlotReader::with_path(&wire, Vec::<KeySegment<'_>>::new()))
        .map_err(|e| failure("decode the edited schema", e))?;
    schema_from_tree_with(&tree, formulas)
}

/// After a save, the host's schema must be the draft's: the same tables,
/// fields and occurrences with the same names, types, comments,
/// annotations, display-names flags and field options. A host that stored
/// nothing (one that refused the uploads) or something else is reported,
/// not taken for a save.
fn verify_saved(draft: &SchemaDraft, saved: &DatabaseSchema) -> Result<(), DataError> {
    let expected = schema_from_catalogs(draft.catalogs(), draft.formulas())?;
    let tables = |s: &DatabaseSchema| {
        s.tables
            .iter()
            .map(|t| {
                let fields: Vec<_> = t
                    .fields
                    .iter()
                    .map(|f| {
                        (
                            f.id,
                            f.name.clone(),
                            f.kind,
                            f.data_type,
                            f.comment.clone(),
                            f.annotation.clone(),
                            f.display_names_enabled,
                            f.options.clone(),
                            f.calculation.clone(),
                            f.summary,
                        )
                    })
                    .collect();
                (t.id, t.name.clone(), t.comment.clone(), fields)
            })
            .collect::<Vec<_>>()
    };
    let occurrences = |s: &DatabaseSchema| {
        s.occurrences
            .iter()
            .map(|o| (o.id, o.name.clone(), o.table_id))
            .collect::<Vec<_>>()
    };
    let relationships = |s: &DatabaseSchema| {
        s.relationships
            .iter()
            .map(|r| (r.id, r.left_occurrence, r.right_occurrence))
            .collect::<Vec<_>>()
    };
    if tables(&expected) != tables(saved)
        || occurrences(&expected) != occurrences(saved)
        || relationships(&expected) != relationships(saved)
    {
        return Err(DataError::new(
            "The host does not read back the schema that was sent, so the changes may not \
             have been saved. Open Manage Database again to see what the host has.",
        ));
    }
    Ok(())
}

/// Decode the same database definitions from a downloaded catalog or a local
/// HBAM file. Unknown relationship definitions remain explicitly undecoded.
/// Formulas show with a decimal point and no custom functions.
pub fn schema_from_tree(tree: &om_hbam::KeyTree<'_>) -> Result<DatabaseSchema, DataError> {
    schema_from_tree_with(tree, &FormulaSettings::default())
}

/// [`schema_from_tree`], with calculation formulas shown with the file's
/// decimal separator and functions.
pub fn schema_from_tree_with(
    tree: &om_hbam::KeyTree<'_>,
    formulas: &FormulaSettings,
) -> Result<DatabaseSchema, DataError> {
    let catalog =
        Catalog::from_tree(tree).map_err(|error| failure("decode database schema", error))?;
    let mut schema = database_schema_from_catalog(&catalog);
    read_schema_details(tree, &mut schema);
    read_calculations(tree, &mut schema, formulas);
    let (relationships, decoded) = read_relationships(tree, &schema);
    schema.relationships = relationships;
    schema.relationships_decoded = decoded;
    Ok(schema)
}

/// Read comments, advanced field metadata and auto-enter text from their
/// stored members (file-metadata.md 1.1-1.2, schema-path.md 5.5).
fn read_schema_details(tree: &om_hbam::KeyTree<'_>, schema: &mut DatabaseSchema) {
    let text = |path: &[u64]| {
        tree.at(path)
            .and_then(|n| n.text())
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let member = |path: &[u64]| tree.at(path).and_then(|n| n.text()).and_then(Result::ok);
    for table in &mut schema.tables {
        table.comment = text(&[3, 0x10, 5, table.id, 3]);
        for field in &mut table.fields {
            field.comment = text(&[table.id, 3, 5, field.id, 3]);
            field.annotation = text(&[table.id, 3, 5, field.id, 0x30]);
            // The advanced flags are a big-endian u32 (file-metadata.md
            // 1.2); a value of any other width is not read as set.
            field.display_names_enabled = tree
                .at(&[table.id, 3, 5, field.id, 0x29])
                .and_then(|node| <[u8; 4]>::try_from(node.value?).ok())
                .is_some_and(|flags| u32::from_be_bytes(flags) & 0x2 != 0);
            // The next serial value `0c` and the auto-entered data `0d`.
            match field.options.as_mut().map(|o| &mut o.auto_enter.value) {
                Some(AutoEnterValue::Serial(serial)) => {
                    serial.next =
                        member(&[table.id, 3, 5, field.id, 0x0c]).unwrap_or_else(|| "1".into());
                }
                Some(AutoEnterValue::Data(data)) => {
                    *data = member(&[table.id, 3, 5, field.id, 0x0d]).unwrap_or_default();
                }
                _ => {}
            }
        }
    }
}

/// Each calculation field's formula `[T, 3, 5, F, 05]` and options
/// (calc-encoding.md 1, schema-path.md 5.10): its text printed with the
/// schema's names, its context occurrence and its definition. A formula
/// without its text, or whose context or streams do not read, leaves the
/// field without a calculation to edit.
fn read_calculations(
    tree: &om_hbam::KeyTree<'_>,
    schema: &mut DatabaseSchema,
    formulas: &FormulaSettings,
) {
    let names = formulas.names(schema);
    for table in &mut schema.tables {
        for field in &mut table.fields {
            if field.kind == SchemaFieldKind::Summary {
                field.summary = tree
                    .at(&[table.id, 3, 5, field.id, 9])
                    .and_then(|n| n.assembled())
                    .map(|v| om_data::meta::SummaryDef::decode(&v));
                continue;
            }
            if let Some(options) = field.options.as_mut() {
                options.read_members(&TreeMembers {
                    tree,
                    path: [table.id, 3, 5, field.id],
                    names: &names,
                    decimal_comma: formulas.decimal_comma,
                });
            }
            if field.kind != SchemaFieldKind::Calculation {
                continue;
            }
            let value = |child: u64| {
                tree.at(&[table.id, 3, 5, field.id, 5, child])
                    .and_then(|n| n.assembled())
                    .map(|v| v.into_owned())
            };
            let definition = tree
                .at(&[table.id, 3, 5, field.id, 2])
                .and_then(|n| n.assembled())
                .map(|v| v.into_owned());
            let context = value(4).and_then(|key| om_data::formula::Key(key).id());
            let text = value(5).and_then(|formatted| {
                calc_field::formula_text(&formatted, &names, formulas.decimal_comma)
            });
            field.calculation = match (definition, context, text, value(6)) {
                (Some(definition), Some(context), Some(text), Some(_)) => {
                    CalculationField::decode(&definition, text, context)
                }
                _ => None,
            };
        }
    }
}

/// A normal field's member `[T, 3, 5, F]`, read for its options' formulas,
/// value list, range and message (schema-path.md 5.13).
struct TreeMembers<'a, 't> {
    tree: &'a om_hbam::KeyTree<'t>,
    path: [u64; 4],
    names: &'a om_data::formula::NameTable,
    decimal_comma: bool,
}

impl TreeMembers<'_, '_> {
    fn node(&self, key: u8) -> Option<&om_hbam::Node<'_>> {
        let [table, catalog, members, field] = self.path;
        self.tree
            .at(&[table, catalog, members, field, u64::from(key)])
    }
}

impl om_data::field_options::OptionMembers for TreeMembers<'_, '_> {
    fn text(&self, key: u8) -> Option<String> {
        self.node(key)?.text()?.ok()
    }

    fn bytes(&self, key: u8) -> Option<Vec<u8>> {
        Some(self.node(key)?.assembled()?.into_owned())
    }

    fn formula(&self, key: u8) -> Option<om_data::field_options::OptionFormula> {
        let node = self.node(key)?;
        let part = |child: u64| Some(node.get(child)?.assembled()?.into_owned());
        let context = om_data::formula::Key(part(4)?).id()?;
        let text =
            part(5).and_then(|f| calc_field::formula_text(&f, self.names, self.decimal_comma));
        Some(om_data::field_options::OptionFormula { text, context })
    }
}

fn database_schema_from_catalog(catalog: &Catalog) -> DatabaseSchema {
    let tables = catalog
        .tables
        .iter()
        .map(|table| SchemaTable {
            id: table.id,
            name: table.name.clone(),
            comment: String::new(),
            fields: table.fields.iter().map(schema_field).collect(),
        })
        .collect();
    let occurrences = catalog
        .table_occurrences
        .iter()
        .map(|occurrence| SchemaOccurrence {
            id: occurrence.id,
            name: occurrence.name.clone(),
            table_id: occurrence
                .base_table()
                .filter(|id| catalog.table(*id).is_some()),
            graph: occurrence.graph_box().map(|graph| OccurrenceBox {
                x: graph.x,
                y: graph.y,
                width: graph.width,
                height: graph.height,
                list_height: graph.list_height,
                display: graph.display,
                color: graph.color,
            }),
        })
        .collect();
    let unavailable_reason = "Schema definitions are read from the file; this read saves nothing.";
    DatabaseSchema {
        tables,
        occurrences,
        relationships_decoded: false,
        capabilities: SchemaCapabilities {
            unavailable_reason: Some(unavailable_reason.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn schema_field(field: &om_hbam::Field) -> SchemaField {
    let kind = match field.kind {
        om_hbam::FieldKind::Normal => SchemaFieldKind::Normal,
        om_hbam::FieldKind::Calculation => SchemaFieldKind::Calculation,
        om_hbam::FieldKind::Summary => SchemaFieldKind::Summary,
        om_hbam::FieldKind::Unknown(value) => SchemaFieldKind::Unknown(value),
    };
    let data_type = match field.data_type {
        om_hbam::DataType::Text | om_hbam::DataType::Unknown(_) => FieldKind::Text,
        om_hbam::DataType::Number => FieldKind::Number,
        om_hbam::DataType::Date => FieldKind::Date,
        om_hbam::DataType::Time => FieldKind::Time,
        om_hbam::DataType::Timestamp => FieldKind::Timestamp,
        om_hbam::DataType::Container => FieldKind::Container,
    };
    let options = field.decoded_options();
    SchemaField {
        id: field.id,
        name: field.name.clone(),
        kind,
        data_type,
        comment: String::new(),
        annotation: String::new(),
        display_names_enabled: false,
        unique: options
            .as_ref()
            .is_some_and(|o| o.validation_unique() || o.auto_entry_serial()),
        unstored: options.is_some_and(|o| o.unstored_calculation()),
        options: (kind == SchemaFieldKind::Normal)
            .then(|| om_data::field_options::FieldOptions::decode(&field.options, None, None))
            .flatten(),
        calculation: None,
        summary: None,
        definition: (field.options.len() == 26).then(|| field.options.clone()),
    }
}

/// Relationship definitions `[3, 251, 5, id, 2]` and their join predicates
/// `[.., 3, n]` (file-metadata.md 4). Each predicate's operator is shown by
/// its symbol (4.2, [`JoinOp`]). Reject truncated keys, operator codes
/// above 6 (FileMaker's own read fails on them) and references outside the
/// downloaded schema.
fn read_relationships(
    tree: &om_hbam::KeyTree<'_>,
    schema: &DatabaseSchema,
) -> (Vec<SchemaRelationship>, bool) {
    let Some(entries) = tree.at(&[3, 251, 5]) else {
        return (vec![], true);
    };
    let mut decoded = true;
    let mut relationships = vec![];
    for entry in &entries.children {
        let relation = (|| {
            let id = entry.id()?;
            let definition = entry.get(2)?.assembled()?;
            let mut input = definition.as_ref();
            let left_occurrence = read_key(&mut input)?;
            let right_occurrence = read_key(&mut input)?;
            // An occurrence of a deleted table keeps its relationships, as
            // FileMaker Pro keeps them (schema-path.md 5.8, E7); their
            // fields on that side cannot be checked.
            let table = |id: u64| {
                schema
                    .occurrences
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| o.table_id.and_then(|t| schema.table(t)))
            };
            let left_table = table(left_occurrence)?;
            let right_table = table(right_occurrence)?;
            let mut matches = vec![];
            for predicate in &entry.get(3)?.children {
                let bytes = predicate.assembled()?;
                let (&code, rest) = bytes.split_first()?;
                let operator = JoinOp::from_code(code)?;
                let mut rest = rest;
                let left_field = read_key(&mut rest)?;
                let right_field = read_key(&mut rest)?;
                let has = |table: Option<&SchemaTable>, field: u64| {
                    table.is_none_or(|t| t.fields.iter().any(|f| f.id == field))
                };
                if !rest.is_empty()
                    || !has(left_table, left_field)
                    || !has(right_table, right_field)
                {
                    return None;
                }
                matches.push(SchemaMatch {
                    left_field,
                    right_field,
                    comparison: operator.symbol().into(),
                    operator: code,
                });
            }
            // The option word ends the definition, after both sort specs
            // (file-metadata.md 4.1).
            let options = definition
                .len()
                .checked_sub(4)
                .and_then(|at| definition.get(at..))
                .and_then(|word| <[u8; 4]>::try_from(word).ok())
                .map(u32::from_be_bytes)?;
            // The right occurrence's records' order, then the left's; an
            // order that does not read is shown as none and kept as stored.
            let sorts = input.get(..input.len().checked_sub(4)?)?;
            let orders = om_data::scripts::specs::SortSpec::read(sorts)
                .map(|(right_order, used)| {
                    let left_order = om_data::scripts::specs::SortSpec::read(&sorts[used..])
                        .filter(|(_, n)| used + n == sorts.len())
                        .map(|(order, _)| order);
                    [left_order, Some(right_order)]
                })
                .unwrap_or_default();
            Some(SchemaRelationship {
                id,
                left_occurrence,
                right_occurrence,
                matches,
                options,
                orders,
            })
        })();
        if let Some(relation) = relation {
            relationships.push(relation);
        } else {
            decoded = false;
        }
    }
    (relationships, decoded)
}

/// One catalog id written as a key string, `[u8 total][u8 len][segment]`,
/// advancing `input` past it.
fn read_key(input: &mut &[u8]) -> Option<u64> {
    let (&length, rest) = input.split_first()?;
    let key = rest.get(..usize::from(length))?;
    let remaining = rest.get(usize::from(length)..)?;
    let (&key_length, number) = key.split_first()?;
    if usize::from(key_length) != number.len() {
        return None;
    }
    let value = om_hbam::slot::be_uint(number)?;
    *input = remaining;
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_serial_keys_are_rejected_without_advancing() {
        let mut input = &[4, 3, 0xd0, 0, 2, 7][..];
        assert_eq!(read_key(&mut input), Some(0xd00002));
        assert_eq!(input, &[7]);
        for bytes in [
            &[4, 3, 0xd0][..],
            &[3, 1, 2, 3][..],
            &[0][..],
            &[10, 9, 0, 0, 0, 0, 0, 0, 0, 0, 1][..],
        ] {
            let mut input = bytes;
            assert_eq!(read_key(&mut input), None);
            assert_eq!(input, bytes);
        }
    }

    #[test]
    fn graph_contains_real_occurrence_and_field_identities() {
        use om_hbam::{slot::KeySegment, KeyTree, Node};
        fn node(
            segment: &'static [u8],
            value: Option<&'static [u8]>,
            children: Vec<Node<'static>>,
        ) -> Node<'static> {
            Node {
                segment: KeySegment(segment),
                value,
                push: !children.is_empty(),
                children,
            }
        }
        // Relationship 1 with these join predicates `[.., 3, n]`.
        let graph = |predicates: Vec<Node<'static>>| KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[3],
                    None,
                    vec![node(
                        &[251],
                        None,
                        vec![node(
                            &[5],
                            None,
                            vec![node(
                                &[1],
                                None,
                                vec![
                                    node(
                                        &[2],
                                        Some(&[
                                            4, 3, 0xd0, 0, 2, 4, 3, 0xd0, 0, 3, 0, 0, 0, 0, 0, 0,
                                            0, 0, 0, 0, 0, 16,
                                        ]),
                                        vec![],
                                    ),
                                    node(&[3], None, predicates),
                                ],
                            )],
                        )],
                    )],
                )],
            ),
        };
        // Operators 0 (=), 2 (<) and 6 (cartesian), each on the same fields.
        let tree = graph(vec![
            node(&[1], Some(&[0, 2, 1, 9, 2, 1, 10]), vec![]),
            node(&[2], Some(&[2, 2, 1, 9, 2, 1, 10]), vec![]),
            node(&[3], Some(&[6, 2, 1, 9, 2, 1, 10]), vec![]),
        ]);
        let field = |id| SchemaField {
            id,
            name: format!("Field {id}"),
            kind: SchemaFieldKind::Normal,
            data_type: FieldKind::Text,
            comment: String::new(),
            annotation: String::new(),
            display_names_enabled: false,
            unique: false,
            unstored: false,
            options: None,
            calculation: None,
            summary: None,
            definition: None,
        };
        let schema = DatabaseSchema {
            tables: vec![
                SchemaTable {
                    id: 0x8002,
                    name: "Contacts".into(),
                    comment: String::new(),
                    fields: vec![field(9)],
                },
                SchemaTable {
                    id: 0x8003,
                    name: "Addresses".into(),
                    comment: String::new(),
                    fields: vec![field(10)],
                },
            ],
            occurrences: vec![
                SchemaOccurrence {
                    id: 0xd00002,
                    name: "Contacts".into(),
                    table_id: Some(0x8002),
                    graph: None,
                },
                SchemaOccurrence {
                    id: 0xd00003,
                    name: "Addresses".into(),
                    table_id: Some(0x8003),
                    graph: None,
                },
            ],
            ..Default::default()
        };
        let (relations, decoded) = read_relationships(&tree, &schema);
        assert!(decoded);
        assert_eq!(
            relations,
            vec![SchemaRelationship {
                id: 1,
                options: 0x10,
                left_occurrence: 0xd00002,
                right_occurrence: 0xd00003,
                matches: [("=", 0), ("<", 2), ("×", 6)]
                    .into_iter()
                    .map(|(symbol, operator)| SchemaMatch {
                        left_field: 9,
                        right_field: 10,
                        comparison: symbol.into(),
                        operator,
                    })
                    .collect(),
                // Two empty orders, "Keep records in sorted order" set.
                orders: [0, 1].map(|_| {
                    Some(om_data::scripts::specs::SortSpec {
                        keep_sorted: true,
                        blanks_last: false,
                        fields: vec![],
                    })
                }),
            }]
        );
        let (relations, decoded) = read_relationships(&tree, &DatabaseSchema::default());
        assert!(!decoded);
        assert!(relations.is_empty());
        // Code 7 exists only in memory; FileMaker's own read fails on it.
        let (relations, decoded) = read_relationships(
            &graph(vec![node(&[1], Some(&[7, 2, 1, 9, 2, 1, 10]), vec![])]),
            &schema,
        );
        assert!(!decoded);
        assert!(relations.is_empty());
    }

    #[test]
    fn saved_field_options_must_match_the_draft() {
        use om_data::field_options::{AutoEnterValue, Serial};

        let mut draft = SchemaDraft::new(Default::default());
        draft
            .apply(&SchemaChange::CreateTable {
                name: "Probe".into(),
                comment: String::new(),
            })
            .unwrap();
        let table_id = schema_from_catalogs(draft.catalogs(), draft.formulas())
            .unwrap()
            .tables[0]
            .id;
        draft
            .apply(&SchemaChange::CreateField {
                table_id,
                name: "Name".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let initial = schema_from_catalogs(draft.catalogs(), draft.formulas()).unwrap();
        let field_id = initial.tables[0].fields[0].id;
        for (wanted, stale) in [
            (
                AutoEnterValue::Data("wanted".into()),
                AutoEnterValue::Data("old".into()),
            ),
            (
                AutoEnterValue::Serial(Serial {
                    next: "10".into(),
                    increment: 2,
                    ..Default::default()
                }),
                AutoEnterValue::Serial(Serial {
                    next: "9".into(),
                    increment: 1,
                    ..Default::default()
                }),
            ),
        ] {
            let current = schema_from_catalogs(draft.catalogs(), draft.formulas()).unwrap();
            let mut options = current.tables[0].fields[0].options.clone().unwrap();
            options.auto_enter.value = wanted;
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id,
                    field_id,
                    options: Box::new(options),
                })
                .unwrap();
            let saved = schema_from_catalogs(draft.catalogs(), draft.formulas()).unwrap();
            assert!(verify_saved(&draft, &saved).is_ok());
            let mut not_saved = saved;
            not_saved.tables[0].fields[0]
                .options
                .as_mut()
                .unwrap()
                .auto_enter
                .value = stale;
            assert!(verify_saved(&draft, &not_saved).is_err());
        }
        // A host that lost or changed a field's annotation or display-names
        // flag did not save the draft either.
        let saved = schema_from_catalogs(draft.catalogs(), draft.formulas()).unwrap();
        let changes: [fn(&mut om_data::SchemaField); 2] = [
            |field| field.annotation = "Stale".into(),
            |field| field.display_names_enabled = true,
        ];
        for change in changes {
            let mut not_saved = saved.clone();
            change(&mut not_saved.tables[0].fields[0]);
            assert!(verify_saved(&draft, &not_saved).is_err());
        }
    }

    #[test]
    fn readable_catalog_does_not_claim_native_write_or_relationship_support() {
        let schema = database_schema_from_catalog(&Catalog::default());
        assert!(!schema.capabilities.fields);
        assert!(!schema.relationships_decoded);
    }
}
