//! Related records of another file's table on a layout of the opened
//! file's own table (#1000): portal rows and related fields of a table a
//! related file owns, written by that file. Layouts of another file's table
//! are records.rs's (#193).
//!
//! Each write goes to the owning file's own provider through the table's
//! route (external.rs): the route is the file whose schema was imported
//! for the table, so the record is written in that file, under its
//! account and privilege set, by its record rules and locks. The opened
//! file's elevation does not apply to the owner. Field ids are the owner's
//! own (`om_data::host` keeps them when it imports a schema); only the
//! table id is the client's. Versions and the window's found set stay in
//! the client's table namespace, as for #193's writes.
//!
//! The window commits a record's related records inside a transaction
//! (record-writes.md). A routed write inside the opened file's transaction
//! holds the change in the owner's transaction: on the opened file's
//! connection the owner joins the opened file's transaction
//! (`begin_under`), its locks the transaction's, as FileMaker Pro holds two
//! files' records under one on FileMaker Server (write-path.md 1.5); on
//! another connection it opens its own there, with the same options. Commit
//! prepares every file before any record data goes, then commits the
//! related file first and the opened file last. The shared transaction
//! ends only after every file has settled, in each file that held part of
//! it, the opened file's last (a host may keep its locks by file). Commit
//! and Revert Transaction end every file's part.
//!
//! **A script's transaction may span files** (#1006). Claris documents
//! Open Transaction's changes as spanning tables of several files,
//! committed or reverted together. Commit Transaction prepares every file
//! (its rules, validation, versions, locks and reserved records, and the
//! uploads its commit sends) before any record data goes. A refusal there, in any file, reverts every file's
//! part and answers that error, nothing submitted: all or none. Once every
//! file is prepared they are submitted one by one, related files first, as
//! FileMaker Pro commits two files on one host (write-path.md 1.5). The
//! protocol commits each file with its own op70, so a connection lost, or
//! a host refusing a later file, after an earlier file's op70 leaves that
//! file committed: Commit Transaction answers the error with what became
//! of each file (`DataError::commit`), and the rest of the transaction
//! ends. Revert Transaction reverts every file's part.
//!
//! **The window's commit of a record may span files** (#1006). It is saved
//! file by file, related files first, as FileMaker Pro does on one host
//! (write-path.md 1.5), with no all-or-nothing claim.
//! - **Refused before any record data,** in any file, its record session
//!   stays as it was: every file's part goes back (#1095).
//! - **Partly applied:** a file committed or unknown while another's work
//!   is not. The provider keeps the save ([`Unsettled`]) and freezes the
//!   window until Revert Record discards what was not saved. Nothing is
//!   undone or sent again by itself.
//!
//! What FileMaker does when a later file refuses, and on two hosts, is
//! unmeasured.
//!
//! **Reports.** A commit reports what it did in each file whose records it
//! held: the files are told apart by connection and the host's id for the
//! file ([`FileKey`]), never by a name or a number alone. A failed commit's
//! error carries the report (`DataError::commit`), each file by an opaque
//! participant id.

use super::*;
use om_data::host::security::RecordAction;
use om_data::runtime::TransactionOptions;
use om_data::ScriptWrite;

use super::records::{CurrentSet, Origin};
use super::transaction::{CommitOutcome, Readback, TransactionKind};
use om_data::{FileCommit, FileOutcome};

/// The related files' transactions the opened file's transaction opened.
#[derive(Default)]
pub(crate) struct OwnerTransaction {
    /// The opened file's transaction's options, the owners' too.
    options: TransactionOptions,
    /// The related files whose transactions it opened, in the order writes
    /// reached them, each by a table of it (client-only id) to route by.
    owners: Vec<u64>,
    /// Related files on this file's connection whose part of the shared
    /// transaction ended without ending it (a staging rolled back,
    /// `rollback_owners`): their locks under it go when it ends
    /// ([`NativeProvider::end_members`]), as this file's own dropped
    /// records' do.
    settled: Vec<u64>,
    /// The records written there, for their versions after Commit.
    held: Vec<HeldRecord>,
}

impl OwnerTransaction {
    /// No related file's transaction was opened: the commit is this
    /// file's alone.
    pub(crate) fn untouched(&self) -> bool {
        self.owners.is_empty()
    }

    /// No staged related writes, including after reservations were parked.
    pub(crate) fn unstaged(&self) -> bool {
        self.held.is_empty()
    }

    /// The commit is this file's alone: [`Self::untouched`], and no related
    /// file's part of the shared transaction waits for its end (`settled`).
    pub(crate) fn alone(&self) -> bool {
        self.untouched() && self.settled.is_empty()
    }

    pub(crate) fn opened(options: TransactionOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// A table (client-only id) of each related file whose transaction this
    /// one opened, to route by: their providers' open transactions are its
    /// parts, set aside with it (`NativeProvider::set_aside`, #892).
    pub(crate) fn owner_tables(&self) -> &[u64] {
        &self.owners
    }

    /// The records of imported table `table` the related files hold for
    /// this transaction, in the order written: each as (record, the
    /// owner's table id, made by the write, deleted by it). Reads of the
    /// window show them (#892).
    pub(crate) fn held_of(&self, table: u64) -> impl Iterator<Item = (u64, u64, bool, bool)> + '_ {
        self.held
            .iter()
            .filter(move |held| held.table == table)
            .map(|held| (held.record, held.wire, held.new, held.deleted))
    }

    /// Every field of every record written here, by its client-only table
    /// id (`schema`'s, the opened file's) and the owner's record id, as
    /// unknown. In a save across files, the opened file's commit reads
    /// these records as the owners' prepared commits write them (#1167),
    /// so what its held records compute from them is not known to an
    /// owner's commit, which comes before. Marked unknown, it refuses a
    /// rule that reads it (3), rather than letting each file store a
    /// different account of the other.
    fn held_unknown(&self, schema: &om_data::host::CalcSchema) -> CallerUnknown {
        let mut unknown = CallerUnknown::new();
        for held in &self.held {
            let fields = schema
                .table(held.table)
                .map(|table| table.fields.keys().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            unknown
                .entry(held.table)
                .or_default()
                .entry(held.record)
                .or_default()
                .extend(fields);
        }
        unknown
    }
}

struct HeldRecord {
    /// The client-only table id, and the owner's own.
    table: u64,
    wire: u64,
    record: u64,
    deleted: bool,
    new: bool,
}

/// A file a commit writes (#1006): the connection it is open on and the
/// host's id for it there (`OpenReply::file_id`), which the handles holding
/// it on that connection share (#1016). Two hosts can number their files
/// and tables alike (file 1, table `80 01`), one host name can reach two
/// hosts, and one file can be open on two connections: no name or number
/// alone tells two files apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileKey {
    connection: om_session::ConnectionRef,
    file: u32,
}

/// A file of a commit, as its report names it.
#[derive(Clone)]
struct Part {
    /// `None`: not open on a connection.
    key: Option<FileKey>,
    /// As the user knows it (`Get ( FilePath )`'s form).
    name: String,
    /// Its transaction holds records: the report lists it.
    holds: bool,
}

/// A window's ordinary commit that may be partly applied (#1006): a file
/// committed, or its outcome is unknown, while another's work is not
/// applied. The provider keeps it until it is settled, the window frozen
/// for edits meanwhile ([`NativeProvider::refuse_while_unsettled`]): the work
/// never submitted, prepared, with its transaction and locks still held,
/// and the transaction they share still open. Nothing is sent again by
/// itself.
pub(crate) struct Unsettled {
    /// The window whose commit it was.
    window: u32,
    /// Each participant, in submission order.
    participants: Vec<Participant>,
    /// The shared transaction's held end, and the related files on this
    /// file's connection that share it.
    end: Option<(Vec<external::TableRoute>, u32)>,
    /// Its connection was lost: only its report is left, which stays, the
    /// window frozen, until it is discarded, whatever closes or reconnects
    /// meanwhile.
    lost: bool,
}

/// A file of a window's commit, as that commit left it.
struct Participant {
    /// The related file's route; `None` for this file.
    route: Option<external::TableRoute>,
    part: Part,
    outcome: FileOutcome,
    /// Its prepared work while not applied: never submitted, or the copy
    /// kept before it was sent with an unknown outcome. Its values are the
    /// ones computed once, never computed again.
    commit: Option<crate::transaction::PreparedCommit>,
    /// A related file's held records, for their versions once applied.
    held: Vec<HeldRecord>,
    /// A related file on another connection: its own transaction's held
    /// end, ended once every participant has settled.
    own_end: Option<u32>,
}

/// What a file's commit did, and its error unless it committed.
fn outcome(commit: CommitOutcome) -> (FileOutcome, Option<DataError>) {
    match commit {
        CommitOutcome::Committed => (FileOutcome::Committed, None),
        CommitOutcome::NotSubmitted(error) => (FileOutcome::NotSubmitted, Some(error)),
        CommitOutcome::Ambiguous(error) => (FileOutcome::Unknown, Some(error)),
    }
}

fn unavailable_owner() -> DataError {
    DataError::with_code(3, "The related file holding this table is unavailable.")
}

impl NativeProvider {
    /// The fields of imported table `table` Browse may edit through a
    /// relationship from a layout of this file's table: those its owner's
    /// rules let a user edit (`RecordPlan::editable_fields`), of the
    /// ordinary stored scalar fields Browse writes through the owner
    /// (`routed_browse_field`), limited by its own account's record and field
    /// access. None when the owner is read-only, its file is unavailable
    /// or its rules cannot be planned.
    pub(crate) fn routed_related_fields(
        &self,
        table: u64,
        here: &om_data::host::CalcSchema,
    ) -> BTreeSet<u64> {
        let Some(route) = self.external.route(table) else {
            return BTreeSet::new();
        };
        let Ok(RoutedEdits::Offered(names, table)) = self.routed_edits(route, here) else {
            return BTreeSet::new();
        };
        self.external
            .with_owner(route, |owner, wire| {
                owner.require_action(wire, RecordAction::Edit)?;
                Ok(table
                    .fields
                    .iter()
                    .filter(|field| {
                        // Containers take Insert rather than text entry, as
                        // for same-file related fields. The owner and ordinary
                        // stored-cell checks still decide availability.
                        (names.contains(&field.name)
                            || field.data_type == om_hbam::DataType::Container)
                            && Self::routed_browse_field(field)
                            && owner.account_info.set_access.as_ref().is_none_or(|access| {
                                access.modifies(
                                    &om_hbam::slot::uint_segment(wire),
                                    &om_hbam::slot::uint_segment(field.id),
                                )
                            })
                    })
                    .map(|field| field.id)
                    .collect())
            })
            .unwrap_or_default()
    }

    /// The fields of `route`'s owning table a user may edit, planned over
    /// its routed plan (#187) as its write is, opening nothing. A plan
    /// refused while a rule reads a related file this file has not opened,
    /// or a table whose fields are unread (pending in `here`, this file's
    /// schema), is offered from the fields' definitions
    /// (`rules::user_editable`): the write reads what waits and plans first
    /// (#1160; #1146 for this file's own tables).
    pub(crate) fn routed_edits(
        &self,
        route: external::TableRoute,
        here: &om_data::host::CalcSchema,
    ) -> Result<RoutedEdits, DataError> {
        let routed = self.routed_plan(route).ok();
        let answer = self.external.with_owner(route, |owner, wire| {
            if owner.read_only() {
                return Ok(None);
            }
            let file = match &routed {
                Some(plan) => std::sync::Arc::clone(&plan.schema),
                None => owner.calc_schema_cached()?,
            };
            let tree = download_tree(&owner.schema_downloads)?;
            let table = owner.catalog.table(wire).cloned().ok_or_else(|| {
                DataError::with_code(3, "The owning table definition is unavailable.")
            })?;
            let planned = om_data::rules::RecordPlan::from_file_schema(&table, &tree, &file);
            let reads: Vec<u64> = match &planned {
                Ok(_) => Vec::new(),
                Err(_) => crate::calc::rule_reads(&file, &table, &tree)
                    .into_iter()
                    .collect(),
            };
            Ok(Some((
                planned.ok().map(|plan| plan.editable_fields()),
                reads,
                table,
            )))
        })?;
        let Some((planned, reads, table)) = answer else {
            return Ok(RoutedEdits::ReadOnly);
        };
        if let Some(names) = planned {
            return Ok(RoutedEdits::Offered(names, table));
        }
        let waits = self.external.on_demand()
            && reads.iter().any(|own| {
                self.external
                    .imported_occurrence(route, *own)
                    .is_some_and(|ours| here.is_pending(ours))
            });
        if !waits {
            return Ok(RoutedEdits::Refused);
        }
        let names = table
            .fields
            .iter()
            .filter(|field| om_data::rules::user_editable(field))
            .map(|field| field.name.clone())
            .collect();
        Ok(RoutedEdits::Offered(names, table))
    }

    /// [`DataProvider::write_record`] of a related record of imported table
    /// `table` (module docs).
    pub(crate) fn write_routed_related(
        &mut self,
        write: &ScriptWrite,
        table: u64,
    ) -> Result<Record, DataError> {
        // A previewed new record names the owner's preview (preview.rs).
        let owned;
        let write = match write.prepared {
            Some(id) => {
                owned = ScriptWrite {
                    prepared: Some(self.routed_preview(id, table)?),
                    ..write.clone()
                };
                &owned
            }
            None => write,
        };
        if !self.in_transaction() {
            return match write.record {
                Some(record) => self.with_existing_record_owner(table, record, |owner, wire| {
                    routed_write(owner, wire, write)
                }),
                None => self
                    .with_new_record_owner(table, |owner, wire| routed_write(owner, wire, write)),
            };
        }
        let result = self.with_owner_transaction(table, write.record, |owner, wire| {
            routed_write(owner, wire, write)
        });
        self.collect_attachment_part(table);
        let (record, wire) = result?;
        let id = u64::try_from(record.id).map_err(|_| DataError::new("Invalid record ID."))?;
        let new = write.record.is_none() || self.related_reserved(table, record.id);
        self.owner_transaction.held.push(HeldRecord {
            table,
            wire,
            record: id,
            deleted: false,
            new,
        });
        Ok(record)
    }

    /// [`DataProvider::delete_related_record`] of a record of imported
    /// table `table` (module docs).
    pub(crate) fn delete_routed_related(
        &mut self,
        table: u64,
        record_id: i64,
    ) -> Result<(), DataError> {
        if !self.in_transaction() {
            return self.with_existing_record_owner(table, record_id, |owner, wire| {
                routed_delete(owner, wire, record_id)
            });
        }
        let ((), wire) = self.with_owner_transaction(table, Some(record_id), |owner, wire| {
            routed_delete(owner, wire, record_id)
        })?;
        let id = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        self.owner_transaction.held.push(HeldRecord {
            table,
            wire,
            record: id,
            deleted: true,
            new: false,
        });
        Ok(())
    }

    /// `operation` on the owner of imported table `table` inside its
    /// transaction, opened first when this is the transaction's first
    /// routed write. Record `record`'s version is the one the window
    /// showed, as [`Self::with_existing_record_owner`] checks it.
    fn with_owner_transaction<T>(
        &mut self,
        table: u64,
        record: Option<i64>,
        operation: impl FnOnce(&mut Self, u64) -> Result<T, DataError>,
    ) -> Result<(T, u64), DataError> {
        let route = self.external.route(table).ok_or_else(unavailable_owner)?;
        let id = record
            .map(|record| u64::try_from(record).map_err(|_| DataError::new("Invalid record ID.")))
            .transpose()?;
        let seen = match id {
            Some(id) => self.observed_version((table, id))?,
            None => None,
        };
        self.join_owner_transaction(table, route)?;
        self.with_owner_reading_root(route, |owner, wire| {
            owner.require_writable()?;
            owner.invalidate_calc_data();
            if let Some(id) = id {
                owner.mod_counts.remove(&(wire, id));
                if let Some(seen) = seen {
                    owner.mod_counts.insert((wire, id), seen);
                }
            }
            Ok((operation(owner, wire)?, wire))
        })
    }

    /// Open the related files the rules of `route`'s owning table read that
    /// this file has not opened yet (on demand), before the owner plans a
    /// write over its [`external::OwnerPlan`], which imports only files
    /// open here (#1160), and the tables they read whose fields are not
    /// read yet (#772). The owner's occurrences are asked for by this
    /// file's ids for them, as its import named them. Nothing is read
    /// while nothing in this file's schema waits
    /// ([`om_data::host::CalcSchema::waits`]).
    fn open_owner_rule_files(&mut self, route: external::TableRoute) -> Result<(), DataError> {
        let mut asked = BTreeSet::new();
        loop {
            if !self.external.on_demand() || !self.calc_schema_cached()?.waits() {
                return Ok(());
            }
            let reads = self.external.with_owner(route, |owner, wire| {
                let schema = owner.calc_schema_cached()?;
                let table = owner.catalog.table(wire).cloned().ok_or_else(|| {
                    DataError::with_code(3, "The owning table definition is unavailable.")
                })?;
                let tree = download_tree(&owner.schema_downloads)?;
                Ok(crate::calc::rule_reads(&schema, &table, &tree))
            })?;
            let here = self.calc_schema_cached()?;
            let ours: Vec<u64> = reads
                .into_iter()
                .filter_map(|own| self.external.imported_occurrence(route, own))
                .filter(|ours| here.is_pending(*ours) && asked.insert(*ours))
                .collect();
            if ours.is_empty() || !self.grow_related(ours)? {
                return Ok(());
            }
        }
    }

    /// The owning file of `route`'s [`external::OwnerPlan`], this file's
    /// own schema imported into it (#187).
    pub(crate) fn routed_plan(
        &self,
        route: external::TableRoute,
    ) -> Result<std::sync::Arc<external::OwnerPlan>, DataError> {
        self.external.owner_plan(
            route,
            || self.own_calc_schema(),
            self.calc.file_path.clone(),
        )
    }

    /// The opened file's own calculation schema from its table catalogs,
    /// built when first asked and kept (`own_schema`): every owner file a
    /// layout shows imports the same one.
    fn own_calc_schema(&self) -> Result<std::sync::Arc<om_data::host::CalcSchema>, DataError> {
        if let Some(schema) = self.own_schema.borrow().as_ref() {
            return Ok(std::sync::Arc::clone(schema));
        }
        let tree = download_tree(&self.schema_downloads)?;
        let schema = std::sync::Arc::new(crate::calc::calc_schema(
            &tree,
            &self.catalog,
            &self.calc.file_name,
        ));
        *self.own_schema.borrow_mut() = Some(std::sync::Arc::clone(&schema));
        Ok(schema)
    }

    /// `operation` on the owner of `route`, which plans its records over
    /// its [`external::OwnerPlan`] and reads this file's tables through this
    /// file's session ([`CallerReads`]) meanwhile (#187). The session is
    /// moved there for the call and always back, also when the owner is not
    /// reached. An unavailable plan or session refuses the operation.
    pub(crate) fn with_owner_reading_root<T>(
        &mut self,
        route: external::TableRoute,
        operation: impl FnOnce(&mut NativeProvider, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        self.with_owner_reading(route, None, operation)
    }

    /// [`Self::with_owner_reading_root`] for the owner's commit, which comes
    /// before this file's (#1006): this file's held records are read as
    /// this file's commit will write them, and what it may store otherwise
    /// is unknown there ([`CallerReads::pending`], preview.rs
    /// `root_as_committed`). `committing` is the commit's routed writes,
    /// which the commit took from the open transaction. Prepared peers are
    /// immutable snapshots; values of other held peers remain unknown.
    /// What this file's held records read of participants is still unknown
    /// there ([`OwnerTransaction::held_unknown`]).
    pub(crate) fn with_owner_reading_committed_root<T>(
        &mut self,
        route: external::TableRoute,
        committing: &OwnerTransaction,
        peers: &PreparedPeers,
        operation: impl FnOnce(&mut NativeProvider, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        self.with_owner_reading(route, Some((committing, peers)), operation)
    }

    /// `committing`: a commit's routed writes
    /// ([`Self::with_owner_reading_committed_root`]).
    fn with_owner_reading<T>(
        &mut self,
        route: external::TableRoute,
        committing: Option<(&OwnerTransaction, &PreparedPeers)>,
        operation: impl FnOnce(&mut NativeProvider, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        self.open_owner_rule_files(route)?;
        let plan = self.routed_plan(route)?;
        let (pending, mut unknown) = match committing {
            Some((open, _)) => {
                let schema = self.calc_schema_cached()?;
                self.root_as_committed(&plan, open.held_unknown(&schema))?
            }
            _ => (None, CallerUnknown::new()),
        };
        // Holding a peer does not prepare it. Its raw overlay is provisional:
        // every nondeleted held value is unknown until that peer's one final
        // preflight. In particular, never read its stale stored calculations.
        let open = committing.map_or(&self.owner_transaction, |(open, _)| open);
        let mut snapshots = BTreeMap::new();
        for other in &plan.others {
            if let Some(snapshot) = committing.and_then(|(_, peers)| peers.get(&other.file)) {
                snapshots.insert(other.file, std::sync::Arc::clone(snapshot));
                continue;
            }
            let route = open.owners.iter().find_map(|table| {
                (self.external.file_of_table(*table) == Some(other.file))
                    .then(|| self.external.route(*table))
                    .flatten()
            });
            let Some(route) = route else { continue };
            let (snapshot, peer_schema) = self.external.with_owner(route, |owner, _| {
                Ok((owner.held_overlay_with(None)?, owner.own_calc_schema()?))
            })?;
            if let Some(snapshot) = snapshot {
                snapshots.insert(other.file, snapshot);
            }
            for held in &open.held {
                if held.deleted || self.external.file_of_table(held.table) != Some(other.file) {
                    continue;
                }
                for (&mapped, &wire) in &other.tables {
                    if wire == held.wire {
                        let fields = peer_schema
                            .table(wire)
                            .map(|table| table.fields.keys().copied().collect())
                            .unwrap_or_default();
                        unknown
                            .entry(mapped)
                            .or_default()
                            .insert(held.record, fields);
                    }
                }
            }
        }
        let schema = self.calc_schema_cached()?;
        let globals = self.desktop_globals(&schema);
        if self.session.is_none() {
            return Err(unavailable_owner());
        }
        // A missing peer session must not turn a held-peer read into a
        // stored-data fallback. Return every session already lent on error.
        let mut others: Vec<(usize, OtherReads)> = Vec::new();
        for other in &plan.others {
            let Some((session, schema)) = self.external.lend_session(other.file) else {
                for (file, other) in others {
                    self.external.return_session(file, other.session);
                }
                return Err(unavailable_owner());
            };
            others.push((
                other.file,
                OtherReads {
                    session,
                    schema,
                    tables: other.tables.clone(),
                    pending: snapshots.remove(&other.file),
                },
            ));
        }
        let mut session = self.session.take();
        let result = self.external.with_owner(route, |owner, wire| {
            let root_session = session.take().ok_or_else(unavailable_owner)?;
            owner.routed_plan = Some(std::sync::Arc::clone(&plan));
            owner.caller = Some(CallerReads {
                session: root_session,
                schema,
                tables: plan.tables.clone(),
                globals,
                pending,
                unknown,
                others: std::mem::take(&mut others),
            });
            // The sessions lent come back also when `operation` panics: the
            // worker survives a panic, and this file and the others must
            // still have their sessions after it.
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(owner, wire)));
            if let Some(caller) = owner.caller.take() {
                session = Some(caller.session);
                others = caller.others;
            }
            owner.routed_plan = None;
            // An error stays the owner's, which `with_owner` names; a panic
            // is resumed once everything is given back.
            match outcome {
                Ok(result) => result.map(Ok),
                Err(panic) => Ok(Err(panic)),
            }
        });
        for (file, other) in others {
            self.external.return_session(file, other.session);
        }
        if let Some(session) = session {
            self.session = Some(session);
        }
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(error) => Err(error),
        }
    }

    /// The open file as a commit tells files apart ([`FileKey`]); `None`
    /// with no file open on a connection.
    pub(crate) fn file_key(&self) -> Option<FileKey> {
        let session = self.session.as_ref()?;
        Some(FileKey {
            connection: session.connection(),
            file: session.opened_file()?.reply.file_id,
        })
    }

    /// This file as a part of a commit.
    fn commit_part(&self) -> Part {
        let calc = &self.calc;
        Part {
            key: self.file_key(),
            name: calc
                .file_path
                .clone()
                .unwrap_or_else(|| format!("fmnet:/{}/{}", calc.host_name, calc.file_name)),
            holds: self.holds_any_record(),
        }
    }

    /// The related file owning imported table `table`, as a participant
    /// of a commit: its route and part.
    fn owner_participant(&self, table: u64) -> Result<(external::TableRoute, Part), DataError> {
        let route = self.external.route(table).ok_or_else(unavailable_owner)?;
        let part = self
            .external
            .with_owner(route, |owner, _| Ok(owner.commit_part()))?;
        Ok((route, part))
    }

    /// The id this provider's commits give the file `key` names
    /// (`om_data::ParticipantId`): the same in every commit while the
    /// provider is open, and not a host's id for the file.
    fn participant(&mut self, key: &FileKey) -> om_data::ParticipantId {
        let index = match self.participants.iter().position(|known| known == key) {
            Some(index) => index,
            None => {
                self.participants.push(key.clone());
                self.participants.len() - 1
            }
        };
        om_data::ParticipantId(index as u64 + 1)
    }

    /// `error`, of a commit that did what `parts` says in each file, in the
    /// order it reached them: the report of the files whose records it
    /// held, one entry per file. A file reached twice whose parts differ
    /// may be partly applied: unknown.
    fn reported(&mut self, mut error: DataError, parts: Vec<(Part, FileOutcome)>) -> DataError {
        let mut report: Vec<FileCommit> = Vec::new();
        for (part, outcome) in parts.into_iter().filter(|(part, _)| part.holds) {
            let participant = part.key.as_ref().map(|key| self.participant(key));
            let same = report
                .iter_mut()
                .find(|entry| participant.is_some() && entry.participant == participant);
            match same {
                Some(entry) if entry.outcome != outcome => entry.outcome = FileOutcome::Unknown,
                Some(_) => {}
                None => report.push(FileCommit {
                    participant,
                    file: part.name,
                    outcome,
                }),
            }
        }
        error.commit = report;
        error
    }

    /// Refuse a change in a window whose commit may be partly applied until
    /// it is settled (#1006): a change now could be lost to, or mixed into,
    /// the work that commit still holds. Reading stays available.
    pub(crate) fn refuse_while_unsettled(&self) -> Result<(), DataError> {
        // Its new record a reconnect could not make again, kept as it was
        // (#152): nothing writes until it is made again or discarded.
        if self.unrecreated_shown().is_some() {
            return Err(DataError::with_code(
                3,
                "This window's new record could not be made again on the new connection. Try \
                 again, or discard it, before it can be changed.",
            ));
        }
        if self
            .unsettled
            .iter()
            .any(|unsettled| unsettled.window == self.window)
        {
            return Err(DataError::with_code(
                3,
                "This record was saved in some of its files but not all of them. Revert \
                 Record discards what was not saved (the files already saved keep it) before \
                 it can be changed again.",
            ));
        }
        Ok(())
    }

    /// This file's part of the current window's unsettled save while it is
    /// not known to be applied: the records it holds, which the window's
    /// reads show (`shown_transactions`). Related records' display reads
    /// use `unsettled_related_record` for their final prepared cells.
    pub(crate) fn unsettled_shown(&self) -> Option<&crate::transaction::Transaction> {
        self.unsettled
            .iter()
            .find(|unsettled| unsettled.window == self.window)?
            .participants
            .iter()
            .find(|participant| participant.route.is_none())?
            .commit
            .as_ref()
            .map(crate::transaction::PreparedCommit::transaction)
    }

    /// A related record's final stored cells in this window's retained save.
    /// Imported table ids are resolved to the original file and wire table;
    /// a reconnected/replaced owner must never inherit the old intent.
    /// `Some(None)` is a prepared deletion. No rules or writes run here.
    pub(crate) fn unsettled_related_record(
        &self,
        table: u64,
        record: u64,
    ) -> Option<Option<om_hbam::Record>> {
        let unsettled = self
            .unsettled
            .iter()
            .find(|save| save.window == self.window)?;
        let route = self.external.route(table)?;
        let key = self
            .external
            .with_owner(route, |owner, _| Ok(owner.file_key()))
            .ok()
            .flatten()?;
        let participant = unsettled.participants.iter().find(|part| {
            part.route.is_some_and(|known| known.same_file(route))
                && part.part.key.as_ref() == Some(&key)
        })?;
        participant
            .commit
            .as_ref()?
            .written_record(route.table, record)
    }

    /// The current window's commit that may be partly applied, as its
    /// report ([`DataProvider::unsettled_save`]).
    pub(crate) fn unsettled_report(&mut self) -> Option<Vec<FileCommit>> {
        let at = self
            .unsettled
            .iter()
            .position(|unsettled| unsettled.window == self.window)?;
        let parts = Self::report_of(&self.unsettled[at].participants);
        Some(self.reported(DataError::new(""), parts).commit)
    }

    /// Every window's unsettled save, as its report
    /// ([`DataProvider::unsettled_saves`]).
    pub(crate) fn unsettled_reports(&mut self) -> Vec<(u32, Vec<FileCommit>)> {
        let parts: Vec<_> = self
            .unsettled
            .iter()
            .map(|unsettled| (unsettled.window, Self::report_of(&unsettled.participants)))
            .collect();
        parts
            .into_iter()
            .map(|(window, parts)| (window, self.reported(DataError::new(""), parts).commit))
            .collect()
    }

    fn report_of(participants: &[Participant]) -> Vec<(Part, FileOutcome)> {
        participants
            .iter()
            .map(|participant| (participant.part.clone(), participant.outcome))
            .collect()
    }

    /// The current window's unsettled save, taken out to work on; `Err`
    /// (3) without one.
    fn take_unsettled(&mut self) -> Result<Unsettled, DataError> {
        let at = self
            .unsettled
            .iter()
            .position(|unsettled| unsettled.window == self.window)
            .ok_or_else(|| {
                DataError::with_code(3, "This window has no partly saved record to settle.")
            })?;
        Ok(self.unsettled.remove(at))
    }

    /// [`DataProvider::reconcile_save`]: read back each file of the current
    /// window's unsettled save whose outcome is unknown, against the values
    /// its commit computed and what was there before
    /// (`NativeProvider::read_back`): applied, proven not applied, or
    /// unresolved. Nothing is sent but reads. A save then applied in every
    /// file is complete, and the window is no longer frozen.
    pub(crate) fn reconcile_unsettled(&mut self) -> Result<Vec<FileCommit>, DataError> {
        let mut unsettled = self.take_unsettled()?;
        for participant in &mut unsettled.participants {
            if participant.outcome != FileOutcome::Unknown {
                continue;
            }
            let Some(commit) = &participant.commit else {
                continue;
            };
            let read = match participant.route {
                None => self.read_back(commit),
                Some(route) => self
                    .external
                    .with_owner(route, |owner, _| Ok(owner.read_back(commit)))
                    .unwrap_or_else(|error| Readback::Unresolved(error.message)),
            };
            participant.outcome = match read {
                Readback::Applied => {
                    // Not sent again: its copy settles the container
                    // references as a successful submission does, once.
                    if let Some(commit) = participant.commit.take() {
                        match participant.route {
                            None => self.settle_applied(commit),
                            Some(route) => {
                                let _ = self.external.with_owner(route, |owner, _| {
                                    owner.settle_applied(commit);
                                    Ok(())
                                });
                            }
                        }
                    }
                    if let Some(route) = participant.route {
                        let held = std::mem::take(&mut participant.held);
                        self.applied_versions(route, &held);
                    }
                    FileOutcome::Committed
                }
                Readback::NotApplied => FileOutcome::NotApplied,
                Readback::Unresolved(_) => FileOutcome::Unresolved,
            };
        }
        self.invalidate_calc_data();
        let report = Self::report_of(&unsettled.participants);
        let complete = unsettled.participants.iter().all(|participant| {
            participant.outcome == FileOutcome::Committed || !participant.part.holds
        });
        if complete {
            self.settle(unsettled.participants, unsettled.end);
        } else {
            self.unsettled.push(unsettled);
        }
        Ok(self.reported(DataError::new(""), report).commit)
    }

    /// [`DataProvider::resume_save`]: send the current window's unsettled
    /// save's work not applied, in its order, and only that: work never
    /// submitted, and work read back as not applied, from the values its
    /// commit computed (nothing is computed again, no record created
    /// again). Refused (3) while a file's outcome is unknown or unresolved.
    /// A failure keeps the save, its report updated.
    pub(crate) fn resume_unsettled(&mut self) -> Result<(), DataError> {
        let mut unsettled = self.take_unsettled()?;
        let blocked = unsettled.participants.iter().any(|participant| {
            participant.part.holds
                && matches!(
                    participant.outcome,
                    FileOutcome::Unknown | FileOutcome::Unresolved
                )
        });
        if blocked {
            let report = Self::report_of(&unsettled.participants);
            self.unsettled.push(unsettled);
            let error = DataError::with_code(
                3,
                "A file's part of this save is unknown or unresolved: check the save again, or \
                 discard what was not saved.",
            );
            return Err(self.reported(error, report));
        }
        let mut failure = None;
        for participant in &mut unsettled.participants {
            if failure.is_some()
                || !matches!(
                    participant.outcome,
                    FileOutcome::NotSubmitted | FileOutcome::NotApplied
                )
            {
                continue;
            }
            failure = self.submit_participant(participant, true);
        }
        self.invalidate_calc_data();
        let report = Self::report_of(&unsettled.participants);
        match failure {
            None => {
                self.settle(unsettled.participants, unsettled.end);
                Ok(())
            }
            Some(error) => {
                self.unsettled.push(unsettled);
                Err(self.reported(error, report))
            }
        }
    }

    /// Discard window `window`'s commit that may be partly applied: the
    /// work not applied goes, sending nothing, and the transaction it held
    /// ends in each file, this file's last. What a file committed stays
    /// committed: nothing is deleted or undone. Whether one was there.
    pub(crate) fn discard_unsettled(&mut self, window: u32) -> bool {
        let Some(at) = self
            .unsettled
            .iter()
            .position(|unsettled| unsettled.window == window)
        else {
            return false;
        };
        let unsettled = self.unsettled.remove(at);
        self.settle(unsettled.participants, unsettled.end);
        self.invalidate_calc_data();
        true
    }

    /// Settle every window's partly applied save as the provider closes. On
    /// working connections: discarded, as Revert Record does. On a lost one
    /// (a reconnect): its report and its window's freeze stay, nothing is
    /// sent for the transactions the loss ended, and every file not known
    /// applied is unresolved, so only discarding settles it.
    pub(crate) fn close_unsettled(&mut self) {
        let lost = self.connection_lost().is_some();
        for unsettled in std::mem::take(&mut self.unsettled) {
            // A report kept from an earlier loss stays as it is.
            if unsettled.lost {
                self.unsettled.push(unsettled);
                continue;
            }
            // What the window is told, once its work is gone.
            let report: Vec<Participant> = unsettled
                .participants
                .iter()
                .map(|participant| Participant {
                    route: participant.route,
                    part: participant.part.clone(),
                    outcome: match participant.outcome {
                        FileOutcome::Committed => FileOutcome::Committed,
                        _ => FileOutcome::Unresolved,
                    },
                    commit: None,
                    held: Vec::new(),
                    own_end: None,
                })
                .collect();
            // Released where a connection still works; a lost one released
            // its locks with it.
            self.settle(unsettled.participants, unsettled.end);
            if lost {
                self.unsettled.push(Unsettled {
                    window: unsettled.window,
                    participants: report,
                    end: None,
                    lost: true,
                });
            }
        }
    }

    /// Hold the next write of imported table `table` in its owner's
    /// transaction: the one already open, or a new one, beside this file's
    /// and any other related file's (a script's transaction may span files,
    /// #1006). On this file's connection the owner joins this file's transaction
    /// (`begin_under`): its locks are the transaction's, as FileMaker Pro
    /// holds two files' records under one transaction on FileMaker Server
    /// (write-path.md 1.5, #1006). On another connection it opens its own
    /// there, of the same kind and options.
    pub(crate) fn join_owner_transaction(
        &mut self,
        table: u64,
        route: external::TableRoute,
    ) -> Result<(), DataError> {
        let opened = self
            .owner_transaction
            .owners
            .iter()
            .filter_map(|owner| self.external.route(*owner))
            .any(|open| open.same_file(route));
        if opened {
            return Ok(());
        }
        let options = self.owner_transaction.options;
        // Of the kind of this file's transaction, which its callers have
        // open (#1006).
        let kind = self
            .transaction_kind()
            .ok_or_else(|| DataError::with_code(3, "No transaction is open."))?;
        let shared = self.shared_transaction();
        self.external.with_owner(route, |owner, _| {
            match shared.as_ref().filter(|shared| owner.can_join(shared)) {
                Some(shared) => owner.begin_under(shared),
                None => owner.begin_kind(options, kind),
            }
        })?;
        self.owner_transaction.owners.push(table);
        Ok(())
    }

    /// Restore coordinator metadata for a validated existing participant. Its
    /// held edit and same-RID lock are already restored on the actual owner.
    pub(crate) fn restored_attachment_record(&mut self, table: u64, wire: u64, record: u64) {
        self.restored_mixed_record(table, wire, record, false);
    }

    pub(crate) fn restored_mixed_record(&mut self, table: u64, wire: u64, record: u64, new: bool) {
        if !self
            .owner_transaction
            .held
            .iter()
            .any(|h| h.table == table && h.wire == wire && h.record == record)
        {
            self.owner_transaction.held.push(HeldRecord {
                table,
                wire,
                record,
                new,
                deleted: false,
            });
        }
    }

    /// Whether this provider's open transaction holds a record of any of
    /// its file's tables.
    fn holds_any_record(&self) -> bool {
        self.catalog
            .tables
            .iter()
            .any(|table| self.holds_records_of(table.id))
    }

    /// Commit Transaction, with the related files' transactions routed
    /// writes opened (module docs). Every participant is prepared (its
    /// rules, validation and versions) before any record data goes; then
    /// the related files are submitted, this file last, as FileMaker Pro
    /// commits them (write-path.md 1.5). The transaction they share ends
    /// once, after every participant has settled. A failure's error
    /// reports what the commit did in each file whose records it held
    /// (`DataError::commit`, #1006). A refusal before any record data, in
    /// any file, ends every file's part with nothing submitted, so a
    /// script's transaction commits every file or none unless a later
    /// file's submission fails after an earlier one's (module docs).
    ///
    /// With `keep` (a window's record session, #1095), a refusal before any
    /// record data is submitted, in any participant, puts every
    /// participant's transaction back with its records held, as the session
    /// was: the window rolls back what it staged and keeps its changes.
    pub(crate) fn commit_with_owner(
        &mut self,
        skip_validation: bool,
        keep: bool,
    ) -> Result<(), DataError> {
        let kind = self.transaction_kind();
        let root = self.commit_part();
        let mut open = std::mem::take(&mut self.owner_transaction);
        let mut owners = Vec::new();
        let mut unreached = None;
        for table in &open.owners {
            match self.owner_participant(*table) {
                Ok(owner) => owners.push(owner),
                Err(error) => {
                    unreached.get_or_insert(error);
                }
            }
        }
        if owners.is_empty() && unreached.is_none() && open.settled.is_empty() {
            if keep {
                return self.commit_keeping(skip_validation);
            }
            return match outcome(self.commit(skip_validation)) {
                (_, None) => Ok(()),
                (done, Some(error)) => Err(self.reported(error, vec![(root, done)])),
            };
        }
        let refusal = unreached;
        let joined = self.joined_owners(&[&open.owners[..], &open.settled[..]].concat());
        // The transaction the related files on this connection joined ends
        // once in each file, after every participant has settled.
        let end = self.hold_transaction_end();
        let holders = |owners: &Vec<(external::TableRoute, Part)>, root: &Part| {
            owners
                .iter()
                .map(|(_, part)| part.clone())
                .chain(std::iter::once(root.clone()))
                .map(|part| (part, FileOutcome::NotSubmitted))
                .collect::<Vec<_>>()
        };
        if keep {
            if let Some(error) = refusal {
                // Nothing goes: the session stays as it was.
                let parts = holders(&owners, &root);
                self.owner_transaction = open;
                self.release_transaction_end();
                return Err(self.reported(error, parts));
            }
        }
        if let Some(error) = refusal {
            // No record data is submitted: every participant's part ends as
            // Revert ends it, then the transaction, this file's last.
            let _ = self.revert_owners(&open.owners);
            let _ = self.revert();
            if let Some(txn) = end {
                if let Some((members, _)) = &joined {
                    self.end_members(members, txn);
                }
                self.end_transaction(txn);
            }
            let parts = owners
                .into_iter()
                .map(|(_, part)| part)
                .chain(std::iter::once(root))
                .map(|part| (part, FileOutcome::NotSubmitted))
                .collect();
            return Err(self.reported(error, parts));
        }
        // A related file on another connection has its own transaction:
        // its end is held too, so its locks outlast an outcome left unknown
        // and the save can be read back there, then sent again (#1006). It
        // ends with the rest, once every participant has settled.
        let own_ends: Vec<Option<u32>> = owners
            .iter()
            .map(|(route, _)| {
                let member = joined
                    .as_ref()
                    .is_some_and(|(members, _)| members.iter().any(|m| m.same_file(*route)));
                if member {
                    return None;
                }
                self.external
                    .with_owner(*route, |owner, _| Ok(owner.hold_transaction_end()))
                    .ok()
                    .flatten()
            })
            .collect();
        // Plan dependencies before preparing anything. Acyclic peers can
        // be prepared once in dependency order, while this vector retains
        // the original submission/recovery order.
        let dependencies: Result<Vec<_>, _> = owners
            .iter()
            .map(|(route, _)| self.owner_dependencies(*route, &open))
            .collect();
        let mut failure = dependencies.as_ref().err().cloned();
        let mut prepared: Vec<_> = owners
            .into_iter()
            .zip(own_ends)
            .map(|((route, part), own_end)| (route, part, None, own_end))
            .collect();
        let mut peers = PreparedPeers::new();
        let mut remaining: BTreeSet<usize> = (0..prepared.len()).collect();
        if let Ok(dependencies) = dependencies {
            while failure.is_none() && !remaining.is_empty() {
                let ready = remaining.iter().copied().find(|at| {
                    dependencies[*at]
                        .iter()
                        .all(|file| peers.contains_key(file))
                });
                let Some(at) = ready else {
                    failure = Some(DataError::with_code(3,
                        "The save's held related files have cyclic rule dependencies. No record data was submitted."));
                    break;
                };
                let route = prepared[at].0;
                match self.with_owner_reading_committed_root(route, &open, &peers, |owner, _| {
                    if keep {
                        owner.prepare_keeping(skip_validation, None)
                    } else {
                        owner.prepare_commit(skip_validation, None)
                    }
                }) {
                    Ok(commit) => {
                        // Own-file ids, not the root's imported ids: every
                        // dependent maps these through its own OtherPlan.
                        let mut rows = om_data::rules::HeldRecords::default();
                        for (table, record, row) in commit.written() {
                            match row {
                                Some(row) => rows.hold(table, record, row),
                                None => rows.delete(table, record),
                            }
                        }
                        peers.insert(route.file_index(), std::sync::Arc::new(rows));
                        prepared[at].2 = Some(commit);
                        remaining.remove(&at);
                    }
                    Err(error) => failure = Some(error),
                }
            }
        }
        // This file's commit reads the related files' records the save
        // holds as their prepared commits write them, which comes before
        // it, not as stored (#1167).
        let others = self.participants_written(&prepared);
        let root_commit = match failure {
            // A related file refused: this file's transaction ends too,
            // releasing what it acquired; the op65s come at the end.
            Some(_) if !keep => {
                let _ = self.revert();
                None
            }
            Some(_) => None,
            None if keep => match self.prepare_keeping(skip_validation, others.as_ref()) {
                Ok(commit) => Some(commit),
                Err(error) => {
                    failure = Some(error);
                    None
                }
            },
            None => match self.prepare_commit(skip_validation, others.as_ref()) {
                Ok(commit) => Some(commit),
                Err(error) => {
                    failure = Some(error);
                    None
                }
            },
        };
        if keep {
            if let Some(error) = failure {
                // Refused before any record data: every participant's
                // transaction goes back, its records held, and the session
                // stays as it was (#1095).
                let mut parts = Vec::new();
                for (route, part, commit, own_end) in prepared {
                    let _ = self.external.with_owner(route, |owner, _| {
                        if let Some(commit) = commit {
                            owner.restore_prepared(commit);
                        }
                        if own_end.is_some() {
                            owner.release_transaction_end();
                        }
                        Ok(())
                    });
                    parts.push((part, FileOutcome::NotSubmitted));
                }
                if let Some(commit) = root_commit {
                    self.restore_prepared(commit);
                }
                parts.push((root, FileOutcome::NotSubmitted));
                self.owner_transaction = open;
                self.release_transaction_end();
                return Err(self.reported(error, parts));
            }
        }
        // The related files first, this file last. After a failure the rest
        // submit no record data: abandoned, or kept (below).
        let mut held = std::mem::take(&mut open.held);
        let mut participants = Vec::new();
        for (route, part, commit, own_end) in prepared {
            let (mine, rest): (Vec<_>, Vec<_>) = held.into_iter().partition(|held| {
                self.external
                    .route(held.table)
                    .is_some_and(|at| at.same_file(route))
            });
            held = rest;
            participants.push(Participant {
                route: Some(route),
                part,
                outcome: FileOutcome::NotSubmitted,
                commit,
                held: mine,
                own_end,
            });
        }
        participants.push(Participant {
            route: None,
            part: root,
            outcome: FileOutcome::NotSubmitted,
            commit: root_commit,
            held: Vec::new(),
            own_end: None,
        });
        for participant in &mut participants {
            if failure.is_some() {
                if participant.commit.is_none() {
                    // Not prepared: a refusal ended its transaction, or one
                    // before it left it open.
                    if let Some(route) = participant.route {
                        let _ = self.revert_owner(route);
                    }
                }
                continue;
            }
            failure = self.submit_participant(participant, kind == Some(TransactionKind::Record));
        }
        // Even a failed commit may have reached a host.
        self.invalidate_calc_data();
        let applied = participants.iter().any(|participant| {
            participant.part.holds
                && matches!(
                    participant.outcome,
                    FileOutcome::Committed | FileOutcome::Unknown
                )
        });
        let report: Vec<(Part, FileOutcome)> = participants
            .iter()
            .map(|participant| (participant.part.clone(), participant.outcome))
            .collect();
        let end = end.map(|txn| {
            let members = joined.map(|(members, _)| members).unwrap_or_default();
            (members, txn)
        });
        if let (Some(error), true, false) = (&failure, keep, applied) {
            // Failed with nothing applied: as refused before any record
            // data, every participant's part goes back, the session as it
            // was, a held new record kept (#1095).
            let error = error.clone();
            for participant in participants {
                open.held.extend(participant.held);
                let Some(commit) = participant.commit else {
                    continue;
                };
                match participant.route {
                    None => self.restore_prepared(commit),
                    Some(route) => {
                        let own = participant.own_end.is_some();
                        let _ = self.external.with_owner(route, |owner, _| {
                            owner.restore_prepared(commit);
                            if own {
                                owner.release_transaction_end();
                            }
                            Ok(())
                        });
                    }
                }
            }
            self.owner_transaction = open;
            self.release_transaction_end();
            return Err(self.reported(error, report));
        }
        if let Some(error) = failure {
            if kind == Some(TransactionKind::Record) && applied {
                // A window's commit that may be partly applied: kept, the
                // window frozen, until it is settled (#1006).
                let error = self.reported(error, report);
                self.unsettled.push(Unsettled {
                    window: self.window,
                    participants,
                    end,
                    lost: false,
                });
                return Err(error);
            }
            // Nothing applied, or a script's transaction: the rest goes.
            self.settle(participants, end);
            return Err(self.reported(error, report));
        }
        self.settle(participants, end);
        Ok(())
    }

    /// Dependencies of every held table in one participant, including
    /// validation lists and transitive calculation/relationship reads.
    fn owner_dependencies(
        &mut self,
        route: external::TableRoute,
        open: &OwnerTransaction,
    ) -> Result<BTreeSet<usize>, DataError> {
        let held_files: BTreeSet<usize> = open
            .owners
            .iter()
            .filter_map(|table| self.external.file_of_table(*table))
            .collect();
        self.with_owner_reading_root(route, |owner, _| {
            let plan = owner.routed_plan.clone().ok_or_else(unavailable_owner)?;
            // The owner's current work, not the root's write history: an
            // edit followed by deletion evaluates no outgoing table rules.
            let tables: BTreeSet<u64> = owner
                .held_in_order()
                .into_iter()
                .map(|(table, _)| table)
                .collect();
            let mut reads = BTreeSet::new();
            for table in &tables {
                let table = owner
                    .catalog
                    .table(*table)
                    .cloned()
                    .ok_or_else(unavailable_owner)?;
                // Refuse unresolved rules before any data upload.
                let rules = owner.plan_table(&table, None)?;
                reads.extend(rules.related_tables());
                reads.extend(validation_tables(&rules, &table));
                let tree = download_tree(&owner.schema_downloads)?;
                reads.extend(
                    crate::calc::rule_reads(&plan.schema, &table, &tree)
                        .into_iter()
                        .filter_map(|occurrence| {
                            plan.schema.table_of(occurrence).map(|table| table.id)
                        }),
                );
            }
            Ok(plan
                .others
                .iter()
                .filter(|other| {
                    held_files.contains(&other.file)
                        && other.tables.keys().any(|table| reads.contains(table))
                })
                .map(|other| other.file)
                .collect())
        })
    }

    /// The records the related files' prepared commits in `prepared` write,
    /// by this file's ids for their tables and their own record ids: what
    /// this file's commit, which comes after theirs, reads of them over the
    /// stored ones (#1167). They are final once prepared, so nothing in
    /// them is unknown. A table this file has no id for is read nowhere
    /// here. `None` when they hold none.
    fn participants_written(
        &self,
        prepared: &[(
            external::TableRoute,
            Part,
            Option<crate::transaction::PreparedCommit>,
            Option<u32>,
        )],
    ) -> Option<om_data::rules::HeldRecords> {
        let mut written = om_data::rules::HeldRecords::default();
        for (route, _, commit, _) in prepared {
            let Some(commit) = commit else {
                continue;
            };
            for (own, record, row) in commit.written() {
                let Some(table) = self.external.client_table(*route, own) else {
                    continue;
                };
                match row {
                    Some(row) => written.hold(table, record, row),
                    None => written.delete(table, record),
                }
            }
        }
        (!written.is_empty()).then_some(written)
    }

    /// Submit `participant`'s kept work, related files reading this file's
    /// records meanwhile: what it did, its error unless it committed. A copy
    /// is kept before it goes, for a save that may be partly applied: its
    /// outcome unknown, the copy is what is read back and sent again. One
    /// not submitted after all has ended its part (its container
    /// references released), so it cannot go again: unresolved.
    fn submit_participant(
        &mut self,
        participant: &mut Participant,
        ordinary: bool,
    ) -> Option<DataError> {
        let commit = participant.commit.take()?;
        let kept = commit.clone();
        let submit = |provider: &mut NativeProvider, commit| {
            outcome(if ordinary {
                provider.submit_keeping(commit)
            } else {
                provider.submit_commit(commit)
            })
        };
        let (done, failure, reached) = match participant.route {
            None => {
                let (done, failure) = submit(self, commit);
                (done, failure, true)
            }
            Some(route) => {
                let mut done = FileOutcome::NotSubmitted;
                let mut reached = false;
                // The owner's failure comes back as its error, which names
                // the file (`ExternalFiles::with_owner`).
                let sent = self.with_owner_reading_root(route, |owner, _| {
                    reached = true;
                    let (outcome, failure) = submit(owner, commit);
                    done = outcome;
                    failure.map_or(Ok(()), Err)
                });
                (done, sent.err(), reached)
            }
        };
        participant.outcome = done;
        match done {
            FileOutcome::Committed => {
                if let Some(route) = participant.route {
                    let held = std::mem::take(&mut participant.held);
                    self.applied_versions(route, &held);
                }
            }
            FileOutcome::Unknown => participant.commit = Some(kept),
            // Not submitted: an ordinary commit's part was left as it was
            // (`submit_keeping`), and one never reached was not touched, so
            // the copy can go back or go again. A script's part has ended.
            _ if ordinary || !reached => participant.commit = Some(kept),
            _ => participant.outcome = FileOutcome::Unresolved,
        }
        failure
    }

    /// Take the versions a related file's applied records have now, as the
    /// window's records read them.
    fn applied_versions(&mut self, route: external::TableRoute, held: &[HeldRecord]) {
        let versions = self
            .external
            .with_owner(route, |owner, _| {
                Ok(held
                    .iter()
                    .map(|held| {
                        (!held.deleted)
                            .then(|| owner.mod_counts.get(&(held.wire, held.record)).copied())
                            .flatten()
                    })
                    .collect::<Vec<_>>())
            })
            .unwrap_or_default();
        for (held, version) in held.iter().zip(versions) {
            self.wrote_version((held.table, held.record), version);
            if held.new && version.is_some() {
                self.found_add(held.table, held.record);
            }
            self.calc.page.invalidate_record(held.table, held.record);
        }
    }

    /// End a commit's participants: the work not applied goes, sending
    /// nothing (a copy kept of work whose outcome is unknown is dropped,
    /// its container references kept, as an ambiguous commit keeps them),
    /// then the shared transaction ends in each file, this file's last.
    fn settle(
        &mut self,
        participants: Vec<Participant>,
        end: Option<(Vec<external::TableRoute>, u32)>,
    ) {
        let own_ends: Vec<(external::TableRoute, u32)> = participants
            .iter()
            .filter_map(|participant| Some((participant.route?, participant.own_end?)))
            .collect();
        for participant in participants {
            let Some(commit) = participant.commit else {
                continue;
            };
            if !matches!(
                participant.outcome,
                FileOutcome::NotSubmitted | FileOutcome::NotApplied
            ) {
                continue;
            }
            match participant.route {
                None => self.abandon_commit(commit),
                Some(route) => {
                    let _ = self.external.with_owner(route, |owner, _| {
                        owner.abandon_commit(commit);
                        Ok(())
                    });
                }
            }
        }
        // Related files on other connections end their own transactions,
        // then the shared one ends in each file, this file's last.
        for (route, txn) in own_ends {
            let _ = self.external.with_owner(route, |owner, _| {
                owner.end_transaction(txn);
                Ok(())
            });
        }
        if let Some((members, txn)) = end {
            self.end_members(&members, txn);
            self.end_transaction(txn);
        }
    }

    /// Revert Transaction, with the related files' transactions routed
    /// writes opened: theirs first, then this file's, which ends the
    /// transaction they share.
    pub(crate) fn revert_with_owner(&mut self) -> Result<(), DataError> {
        // Explicit Revert settles asset receipts before ending transactions.
        // Close may instead have detached one, including a parked window's:
        // ending its locks must not discard its remaining ownership authority.
        if self
            .scalar_carry
            .as_ref()
            .is_none_or(|c| !c.has_assets() && c.window() == self.window)
        {
            self.scalar_carry = None;
        }
        self.scalar_refusal = None;
        let open = std::mem::take(&mut self.owner_transaction);
        let joined = self.joined_owners(&[&open.owners[..], &open.settled[..]].concat());
        // Every part settles before any op65: this file's end waits too.
        let end = self.hold_transaction_end();
        let owners = self.revert_owners(&open.owners);
        let root = self.revert();
        if let Some(txn) = end {
            if let Some((members, _)) = &joined {
                self.end_members(members, txn);
            }
            self.end_transaction(txn);
        }
        root?;
        owners
    }

    /// Revert the transactions of the related files `tables` route to, the
    /// first error kept.
    fn revert_owners(&mut self, tables: &[u64]) -> Result<(), DataError> {
        let mut reverted = Ok(());
        for table in tables {
            if let Some(route) = self.external.route(*table) {
                let owner = self.revert_owner(route);
                if reverted.is_ok() {
                    reverted = owner;
                }
            }
        }
        reverted
    }

    /// Revert the transaction of the related file `route` names, if one is
    /// open.
    fn revert_owner(&mut self, route: external::TableRoute) -> Result<(), DataError> {
        self.external.with_owner(route, |owner, _| {
            if owner.in_transaction() {
                owner.revert()
            } else {
                Ok(())
            }
        })
    }

    /// The related files of `tables` that joined this file's open
    /// transaction on its connection (`begin_under`), and its id.
    fn joined_owners(&self, tables: &[u64]) -> Option<(Vec<external::TableRoute>, u32)> {
        let shared = self.shared_transaction()?;
        // Each file once: one rolled back and staged again is in both lists.
        let mut joined: Vec<external::TableRoute> = Vec::new();
        for route in tables
            .iter()
            .filter_map(|table| self.external.route(*table))
        {
            let member = self
                .external
                .with_owner(route, |owner, _| Ok(owner.can_join(&shared)))
                .unwrap_or(false);
            if member && !joined.iter().any(|known| known.same_file(route)) {
                joined.push(route);
            }
        }
        Some((joined, Self::shared_txn(&shared)))
    }

    /// Set aside new related records from a refused save. The participants
    /// stay joined while any reservation survives, so a corrected draft can
    /// reclaim its id without another op71. Other staged edits are dropped.
    pub(crate) fn rollback_owners_parking(&mut self) {
        let mut parked = Vec::new();
        let mut routes: Vec<external::TableRoute> = Vec::new();
        for table in self.owner_transaction.owners.clone() {
            let Some(route) = self.external.route(table) else {
                continue;
            };
            if routes.iter().any(|known| known.same_file(route)) {
                continue;
            }
            routes.push(route);
            let kept = self
                .external
                .with_owner(route, |owner, _| Ok(owner.park_new_records()))
                .unwrap_or(false);
            if kept {
                parked.push(route);
            }
        }
        if parked.is_empty() {
            return self.rollback_owners();
        }
        // The parts stay as they are, their records parked: the window's
        // reads show none of them, and its drafts write them again.
        self.owner_transaction.held.clear();
    }

    pub(crate) fn scalar_owner_tables(&self) -> Option<Vec<u64>> {
        (self.owner_transaction.unstaged() && self.owner_transaction.settled.is_empty())
            .then(|| self.owner_transaction.owners.clone())
    }

    /// Whether `record` of imported table `table` is a new record its
    /// related file holds or has parked for the open transaction
    /// (`DataProvider::related_record_reserved`).
    pub(crate) fn related_reserved(&self, table: u64, record: i64) -> bool {
        let (Some(route), Ok(record)) = (self.external.route(table), u64::try_from(record)) else {
            return false;
        };
        self.external
            .with_owner(route, |owner, wire| Ok(owner.reserved_new(wire, record)))
            .unwrap_or(false)
    }

    /// Drop the staged participants and end their independent transactions.
    pub(crate) fn rollback_owners(&mut self) {
        let options = self.owner_transaction.options;
        let open = std::mem::replace(
            &mut self.owner_transaction,
            OwnerTransaction::opened(options),
        );
        let joined = self
            .joined_owners(&open.owners)
            .map(|(members, _)| members)
            .unwrap_or_default();
        let _ = self.revert_owners(&open.owners);
        let mut settled = open.settled;
        for table in open.owners {
            let Some(route) = self.external.route(table) else {
                continue;
            };
            let known = settled.iter().any(|other| {
                self.external
                    .route(*other)
                    .is_some_and(|other| other.same_file(route))
            });
            if !known && joined.iter().any(|member| member.same_file(route)) {
                settled.push(table);
            }
        }
        self.owner_transaction.settled = settled;
    }

    /// End shared transaction `txn` in each related file of `members`,
    /// whose parts of it have all settled, before this file ends it: once
    /// per file, OpenMaker's choice. A host may keep a transaction's locks by
    /// file, as OpenMaker Server does: this file's op65 would leave theirs
    /// held. Whether FileMaker Server's op65 reaches another file's locks is
    /// unmeasured; write-path.md 1.5's capture has no op65 for the
    /// transaction.
    fn end_members(&mut self, members: &[external::TableRoute], txn: u32) {
        for route in members {
            let _ = self.external.with_owner(*route, |owner, _| {
                owner.end_transaction(txn);
                Ok(())
            });
        }
    }

    /// Refuse deleting record `record_id` of this file's base table `table`
    /// when the account's privilege set does not allow it, also by a
    /// formula on the record as stored (200).
    pub(crate) fn require_delete(&mut self, table: u64, record_id: i64) -> Result<(), DataError> {
        self.require_action(table, RecordAction::Delete)?;
        let has_limit = self.account_info.set_access.as_ref().is_some_and(|access| {
            access
                .limit(&om_hbam::slot::uint_segment(table), RecordAction::Delete)
                .is_some()
        });
        if has_limit {
            let id = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
            let node = self
                .read_record(table, id)?
                .ok_or_else(|| DataError::with_code(101, "This record no longer exists."))?;
            let row = records::text_values(&records::stored_record(id, &node));
            self.require_limit(table, RecordAction::Delete, &row)?;
        }
        Ok(())
    }
}

/// A script's or the window's write of record `write.record` (`None`
/// creates one) of the owner's table `wire`, on the owner: its privilege
/// set, then its rules, a user's for what was typed. Inside its
/// transaction the change is held there.
fn routed_write(
    owner: &mut NativeProvider,
    wire: u64,
    write: &ScriptWrite,
) -> Result<Record, DataError> {
    let action = match write.record {
        // A new record a refused commit reserved is still one to create.
        Some(id) if u64::try_from(id).is_ok_and(|id| owner.reserved_new(wire, id)) => {
            RecordAction::Create
        }
        Some(_) => RecordAction::Edit,
        None => RecordAction::Create,
    };
    owner.require_action(wire, action)?;
    let held = owner.in_transaction();
    let (table, plan) = match held {
        // Held writes are planned at Commit; the definitions read at open
        // check the cells now, as `hold_script` checks them.
        true => (
            owner
                .catalog
                .table(wire)
                .ok_or_else(|| {
                    DataError::with_code(
                        om_data::rules::error::FIELD_MISSING,
                        "The owning table definition is unavailable.",
                    )
                })?
                .clone(),
            None,
        ),
        false => {
            let (table, plan) = owner.table_plan(wire)?;
            (table, Some(plan))
        }
    };
    NativeProvider::check_scalar_cells(&table, &write.cells)?;
    let mut cells = write.cells.clone();
    for cell in &write.typed {
        if let Some(Some(text)) = cells.get_mut(cell) {
            *text = owner.check_typed_entry(wire, *cell, text)?;
        }
    }
    // The window is the calling file's, no layout of the owner, and the
    // calling script's full access is its own file's: the owner's formulas
    // read no layout number and the owner session's own set and clock
    // (`write_context`, #1002). The record's number is the calling window's
    // portal row's, wherever the record is stored (#1038).
    let owner_context = Origin {
        layout: None,
        set: CurrentSet::Account,
        record_number: write.record_number,
        prepared: write.prepared,
    };
    match plan {
        Some(plan) => {
            let context = owner.origin_context(&plan, owner_context)?;
            owner.script_table_record(
                &table,
                &plan,
                (write.record, write.prepared),
                (&cells, &write.typed),
                write.skip_validation,
                context,
            )
        }
        None => owner.hold_script(wire, write.record, (&cells, &write.typed), owner_context),
    }
}

/// What [`NativeProvider::routed_edits`] gives: the owner is read-only;
/// the fields offered with the owning table; or its plan is refused.
pub(crate) enum RoutedEdits {
    ReadOnly,
    Offered(BTreeSet<String>, om_hbam::Table),
    Refused,
}

/// Tables read through validation lists, including the listed fields'
/// unstored calculations and every relationship match/sort field. Keep this
/// walk local to participant planning: it performs no hydration or evaluation.
fn validation_tables(plan: &om_data::rules::RecordPlan, table: &om_hbam::Table) -> BTreeSet<u64> {
    let schema = plan.schema();
    let mut pending = Vec::new();
    for field in &table.fields {
        let entry = om_data::rules::Entry::default();
        if let Some((occurrence, field)) = plan.value_list_field(field.id, entry) {
            if let Some(table) = schema.table_of(occurrence) {
                pending.push((table.id, field));
            }
        }
        let Some(list) = plan.related_value_list(field.id, entry) else {
            continue;
        };
        let om_data::meta::ValueSource::Field {
            primary,
            secondary,
            start,
        } = &list.source
        else {
            continue;
        };
        for spec in std::iter::once(primary).chain(secondary.as_ref()) {
            let Some(occurrence) = spec.occurrence else {
                continue;
            };
            if let Some(table) = schema.table_of(occurrence) {
                pending.push((table.id, spec.field));
            }
            for hop in start
                .and_then(|from| schema.path(from, occurrence))
                .unwrap_or_default()
            {
                let (Some(from), Some(to)) = (schema.table_of(hop.from), schema.table_of(hop.to))
                else {
                    continue;
                };
                for predicate in &hop.predicates {
                    pending.extend([(from.id, predicate.left), (to.id, predicate.right)]);
                }
                for sort in hop.sort.iter().flat_map(|sort| &sort.fields) {
                    if sort.field.occurrence.is_none_or(|at| at == hop.to) {
                        pending.push((to.id, sort.field.field));
                    }
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    while let Some((table, field)) = pending.pop() {
        if !seen.insert((table, field)) {
            continue;
        }
        if let Some(reads) = om_data::rules::field_reads(schema, table, field) {
            pending.extend(reads.own.into_iter().map(|field| (table, field)));
            pending.extend(reads.other);
        }
    }
    seen.into_iter().map(|(table, _)| table).collect()
}

/// What an owning file's formulas read of the opened file's tables while
/// the opened file writes its records (#187): the opened file's session,
/// moved here for the call and back, its schema, and the opened file's
/// tables as the owner's plan names them, to its own ids. The reads go
/// through the opened file's own session and account, so its record access
/// applies. Whether FileMaker reads them through the owning file's own
/// connection to the opened file is not measured.
pub(crate) struct CallerReads {
    pub(crate) session: Session,
    pub(crate) schema: std::sync::Arc<om_data::host::CalcSchema>,
    pub(crate) tables: BTreeMap<u64, u64>,
    /// The opened file's session values of its global fields, by its own
    /// table ids (`set_global`'s, which the host does not store): its
    /// globals read as its window reads them.
    pub(crate) globals: crate::calc::SessionGlobals,
    /// The opened file's unsaved records, by its own table ids, over its
    /// stored ones: its window's, given to a preview (preview.rs, #1063),
    /// or the records its transaction holds, as its commit will write them,
    /// given to a related file's commit, which comes before it (#1006).
    pub(crate) pending: Option<std::sync::Arc<om_data::rules::HeldRecords>>,
    /// Which of `pending`'s values the opened file's commit may not store as
    /// read now (its modification timestamp, which it draws itself, and
    /// what is computed from one), by the owner's ids for its tables and its
    /// own record ids: what a related file's commit refuses to read (3).
    pub(crate) unknown: CallerUnknown,
    /// The owner's other related files this file has open, by their index
    /// here, with their sessions lent for the call (#1160).
    pub(crate) others: Vec<(usize, OtherReads)>,
}

/// One of [`CallerReads::others`]: a related file's session, its own
/// schema, and its tables as the owner's plan names them, to their ids in
/// that file.
pub(crate) struct OtherReads {
    pub(crate) session: Session,
    pub(crate) schema: std::sync::Arc<om_data::host::CalcSchema>,
    pub(crate) tables: BTreeMap<u64, u64>,
    pub(crate) pending: Option<std::sync::Arc<om_data::rules::HeldRecords>>,
}

/// Immutable final preflight rows, keyed by the root's file index; each
/// snapshot uses that file's own table ids, including deletions.
type PreparedPeers = BTreeMap<usize, std::sync::Arc<om_data::rules::HeldRecords>>;

/// [`CallerReads::unknown`]: fields by table and record.
pub(crate) type CallerUnknown = crate::preview::Unknown;

/// `own`, the owning file's records, with the opened file's tables (`tables`,
/// the owner's ids to the opened file's) read from `root`, and the owner's
/// other related files' (`others`: each one's source and the owner's ids to
/// that file's, #1160).
pub(crate) struct CallerSource<'a> {
    pub(crate) own: &'a dyn om_data::host::CalcSource,
    pub(crate) root: &'a dyn om_data::host::CalcSource,
    pub(crate) tables: &'a BTreeMap<u64, u64>,
    pub(crate) others: &'a [(&'a dyn om_data::host::CalcSource, &'a BTreeMap<u64, u64>)],
}

impl CallerSource<'_> {
    fn of(&self, table: u64) -> (&dyn om_data::host::CalcSource, u64) {
        if let Some(&root) = self.tables.get(&table) {
            return (self.root, root);
        }
        for (source, tables) in self.others {
            if let Some(&own) = tables.get(&table) {
                return (*source, own);
            }
        }
        (self.own, table)
    }
}

impl om_data::host::CalcSource for CallerSource<'_> {
    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<std::sync::Arc<om_data::host::RecordValues>>, om_data::calc::CalcError> {
        let (source, table) = self.of(table);
        source.record(table, record)
    }

    fn records(&self, table: u64) -> Result<std::sync::Arc<[u64]>, om_data::calc::CalcError> {
        let (source, table) = self.of(table);
        source.records(table)
    }

    fn globals(
        &self,
        table: u64,
    ) -> Result<std::sync::Arc<om_data::host::RecordValues>, om_data::calc::CalcError> {
        let (source, table) = self.of(table);
        source.globals(table)
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        let (source, table) = self.of(table);
        source.prefetch_records(table, records);
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        let (source, table) = self.of(table);
        source.has_record(table, record)
    }

    fn lookup(
        &self,
        table: u64,
        field: u64,
        text: &str,
    ) -> Result<Option<std::sync::Arc<[u64]>>, om_data::calc::CalcError> {
        let (source, table) = self.of(table);
        source.lookup(table, field, text)
    }

    fn interrupted(&self) -> bool {
        self.own.interrupted()
    }

    fn pending(&self) -> om_data::calc::eval::Pending {
        self.own.pending()
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.own.need_occurrence(occurrence);
    }

    fn need_layout(&self, layout: u32) {
        self.own.need_layout(layout);
    }
}

/// Delete Portal Row's deletion of record `record_id` of the owner's table
/// `wire`, on the owner: its privilege set, then its deletion engine (held
/// inside its transaction).
fn routed_delete(owner: &mut NativeProvider, wire: u64, record_id: i64) -> Result<(), DataError> {
    owner.require_delete(wire, record_id)?;
    owner.delete_related_record(wire, record_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    // The fixture loads once in the crate (transaction.rs's tests).
    use crate::transaction::fixture::{TempFile, ACCOUNT, FILE, PASSWORD};
    use om_data::Connection;
    use om_protocol::MachineInfo;
    use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

    /// The Tasks file on a loopback host of its own, and its certificate.
    fn host(tag: &str) -> (TempFile, u16, Vec<u8>) {
        let file = TempFile::new(tag, &[], true);
        let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_pem = generated.cert.pem().into_bytes();
        let identity =
            Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
        let config = ServerConfig {
            identity: ServerIdentity {
                name: format!("owner-{tag}"),
                version_string: "OpenMaker test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: FILE.into(),
                path: file.0.clone(),
            }],
            insecure_files: false,
            record_dir: None,
        };
        let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || server.serve());
        (file, port, ca_pem)
    }

    /// A provider with the Tasks file open on a connection of its own.
    fn open(port: u16, ca_pem: &[u8]) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut provider = NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: ca_pem.to_vec(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        });
        provider
            .open(&Connection {
                host: "127.0.0.1".into(),
                port,
                file: FILE.into(),
                account: ACCOUNT.into(),
                password: PASSWORD.into(),
            })
            .unwrap();
        provider
    }

    /// A table known before its catalog was read need not have an
    /// occurrence. A table-only request must refuse that state, rather
    /// than return or cache a record decoded without field definitions.
    #[test]
    fn an_unread_table_without_an_occurrence_refuses_record_data() {
        let (_file, port, ca) = host("unread-no-occurrence");
        let mut provider = open(port, &ca);
        provider.external.set_on_demand(true);
        let mut schema = om_data::host::CalcSchema::new(FILE, om_data::calc::Locale::en_us());
        let table = crate::transaction::fixture::TABLE;
        schema.add_table(table, "Tasks");
        schema.set_fields_pending(table);
        provider.calc.schema = Some(std::sync::Arc::new(schema));
        let error = provider
            .read_calc_data(&[om_data::host::CalcNeed::Record { table, record: 1 }])
            .unwrap_err();
        assert_eq!(error.code, Some(3), "{error}");
        assert!(!provider.calc.page.records.contains_key(&(table, 1)));
        provider.close();
    }

    /// A file of a commit is its connection and the host's id for it there
    /// (#1006). One file name, numbered alike by two hosts, is two files;
    /// the same file over two connections to one host is two files too,
    /// each its own lock user.
    #[test]
    fn a_commits_file_is_its_connection_and_the_hosts_id_for_it() {
        let (_one_file, one_port, one_ca) = host("key-one");
        let (_other_file, other_port, other_ca) = host("key-other");
        let one = open(one_port, &one_ca);
        let again = open(one_port, &one_ca);
        let other = open(other_port, &other_ca);
        let key = |provider: &NativeProvider| provider.file_key().unwrap();
        assert_eq!(key(&one).file, key(&other).file, "numbered alike");
        assert_ne!(key(&one), key(&other), "two hosts");
        assert_ne!(key(&one), key(&again), "two connections");
        assert_eq!(key(&one), key(&one));
    }
}
