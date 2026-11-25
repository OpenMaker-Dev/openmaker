//! The files a server hosts: loaded at startup, and closed and opened again
//! by the administrator (`ServerHandle::close_file`, `open_file`), each in a
//! [`Slot`] that keeps its id, name and path meanwhile.

use std::borrow::Cow;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use om_crypto::account::{StoredPassword, StoredPasswordError};
use om_data::host::security::{set_access, BuiltInSet, SetAccess};
use om_hbam::HbamFile;

use crate::accounts::{AccountKind, AccountStatus};
use crate::admin::FileStatus;
use crate::anchored::{FileAt, FileId};
use crate::config::{HostedFile, Privileges};
use crate::connection::host_time_key_at;
use crate::engine::{
    AccountStamp, ChangePassword, DeleteAccount, Direction, EnableAccount, Environment, FileEngine,
    Operation, Password, ReadError, Reply, ResetPassword, Transaction, View, Writer, NO_LIMIT,
};
use crate::eventlog::{Event, EventLog, SaveOutcome};
use crate::filelock::FileLock;
use crate::locks::LockTable;
use crate::store::{Journal, SaveError, Store};
use crate::thumbnail;

/// An account defined in a file's security catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileAccount {
    /// The account name (`[23, 1, 5, id, 16]`).
    pub name: String,
    /// The account's record key under `[23, 1, 5]`. A successful Login reply
    /// carries it, and FileMaker Pro derives the session's privileges from that
    /// account (spec 5.6.5).
    pub key: Vec<u8>,
    /// The account's password as the file stores it (`[23, 1, 5, id, 06]`,
    /// `om_crypto::account`), when it is one this server can check: logins
    /// authenticate against it ([`Hosted::verify_password`]).
    pub password: Result<StoredPassword, PasswordUnavailable>,
    /// The account's kind (`02`), `None` when it cannot be read.
    pub kind: Option<AccountKind>,
    /// The account's status (`0a`): whether it is active and its password
    /// due a change; `None` when it cannot be read.
    pub status: Option<AccountStatus>,
    /// The privilege set rules the host enforces for the account: a
    /// verified built-in set, or a custom set every member of which it
    /// enforces (`om_data::host::security::set_access`, docs/re/local-files.md
    /// 2.3); `None` for a set it cannot enforce or read.
    pub access: Option<SetAccess>,
    /// How many days a password lasts under the account's privilege set
    /// (`[23, 2, 5, P, 0c]`, absent or 0 for no limit); `None` when the
    /// set or the value cannot be read.
    pub password_days: Option<u16>,
}

impl FileAccount {
    /// Facts about a file account after its own password authenticated it.
    /// No credential or security-catalog bytes leave the host. They check
    /// an empty password against the account's verifier, which is no read:
    /// never with a reading of the file held (debug builds check it).
    pub fn authenticated_info(&self) -> om_data::host::AccountInfo {
        crate::engine::assert_no_reading_held("an account's verifier check");
        let account_type =
            (self.kind == Some(AccountKind::FileMakerFile)).then(|| "FileMaker File".to_owned());
        let password_expiry = match (self.password.as_ref(), self.password_days, self.status) {
            (Ok(password), _, _) if password.verify("") => Some(-1),
            (_, Some(0), _) => Some(-1),
            (_, Some(days), Some(status)) => {
                let date = status.date;
                om_data::calc::Date::from_ymd(
                    i64::from(date.year()),
                    date.month() as u32,
                    date.day() as u32,
                )
                .map(|date| date.day_number() + i64::from(days))
            }
            _ => None,
        };
        om_data::host::AccountInfo {
            account_type,
            password_expiry,
            set_access: self.access.clone(),
        }
    }

    /// Whether the account may log in to the file on `today`, hosted or
    /// opened through a client's private host ([`crate::LocalHost`]): only
    /// an active account of the file itself with no password change due,
    /// in a privilege set whose every limit the host enforces (a verified
    /// built-in set, or a custom set `set_access` admits), so that a client
    /// that ignores the set's limits still gets no more than it allows
    /// (docs/re/local-files.md 2.2, 2.3). Checked the way FileMaker Pro
    /// checks a typed password, after the password itself, and for a
    /// schedule's account.
    pub fn eligibility(&self, today: jiff::civil::Date) -> Eligibility {
        if self.key == GUEST_KEY {
            return Eligibility::Guest;
        }
        match self.kind {
            Some(AccountKind::FileMakerFile) => {}
            Some(AccountKind::External(_)) => return Eligibility::External,
            None => return Eligibility::UnreadableSettings,
        }
        let Some(status) = self.status else {
            return Eligibility::UnreadableSettings;
        };
        if !status.is_active() {
            return Eligibility::Inactive;
        }
        if status.must_change_password() {
            return Eligibility::PasswordChangeDue;
        }
        // A lifetime that cannot be read could have run out.
        match self.password_days {
            Some(days) if status.password_expired(days, today) => {
                return Eligibility::PasswordChangeDue;
            }
            None => return Eligibility::UnreadableSettings,
            _ => {}
        }
        if self.access.is_none() {
            return Eligibility::UnsupportedPrivileges;
        }
        Eligibility::Eligible
    }

    /// [`FileAccount::eligibility`] for a client of a network server: an
    /// account in a custom set is refused too, as a set the host cannot
    /// enforce, since the server does not yet hold a client that sends its
    /// own requests to every member of one (its scripts, view-only fields,
    /// value lists and container uploads, #734). `custom_sets` admits it
    /// (a test fixture, `Server::admitting_custom_sets`). A file's private
    /// host uses [`FileAccount::eligibility`]: its one client is the
    /// desktop that opened the file, which applies those limits itself.
    pub fn server_eligibility(&self, today: jiff::civil::Date, custom_sets: bool) -> Eligibility {
        let eligibility = self.eligibility(today);
        let custom = matches!(self.access, Some(SetAccess::Custom(_)));
        if eligibility == Eligibility::Eligible && custom && !custom_sets {
            return Eligibility::UnsupportedPrivileges;
        }
        eligibility
    }

    /// Whether the account is in the verified built-in [Full Access] set.
    pub fn full_access(&self) -> bool {
        self.access.as_ref().is_some_and(SetAccess::full_access)
    }

    /// What the account may do. A file admits only its active accounts in
    /// a set the host enforces ([`Eligibility`]): one in a verified
    /// built-in set has that set's privileges; one in a custom set is
    /// `Unknown` here, so that it changes no design and its server scripts
    /// fail closed, and its record limits are checked where clients read
    /// and write records.
    pub fn privileges(&self) -> Privileges {
        match self.access {
            Some(SetAccess::BuiltIn(BuiltInSet::FullAccess)) => Privileges::FullAccess,
            Some(SetAccess::BuiltIn(BuiltInSet::DataEntryOnly)) => Privileges::DataEntry,
            Some(SetAccess::BuiltIn(BuiltInSet::ReadOnlyAccess)) => Privileges::ReadOnly,
            _ => Privileges::Unknown,
        }
    }
}

/// Whether an account of a file may log in to it
/// ([`FileAccount::eligibility`]). An account the host cannot hold to its
/// privilege set's limits is refused before any session exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eligibility {
    /// An active account of the file itself, no password change due, in
    /// a set the host enforces.
    Eligible,
    /// The [Guest] account (record `01`), which no typed password opens.
    Guest,
    /// An account another service authenticates ([`AccountKind::External`]).
    External,
    /// An account the file keeps from logging in.
    Inactive,
    /// The password must be changed first: the account is marked so, or
    /// its privilege set's password lifetime has run out.
    PasswordChangeDue,
    /// A kind, status or password lifetime in a form OpenMaker does not
    /// read: it could be any of the above.
    UnreadableSettings,
    /// A privilege set the host cannot enforce every member of, or one
    /// that cannot be read.
    UnsupportedPrivileges,
}

/// The record key of the [Guest] account (FileMaker's `KIndex`).
const GUEST_KEY: [u8; 1] = [1];

/// Why an account's stored password cannot be checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordUnavailable {
    /// The account record has no password key.
    Missing,
    /// A form `om_crypto::account` does not read.
    Unsupported(StoredPasswordError),
}

/// A connection's registration for change notifications on a file (op63,
/// write-path.md 6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration {
    pub connection: u32,
    /// The client's session id notifications are addressed to.
    pub address: u32,
    /// The attention codes the client wants.
    pub mask: u32,
    /// The client's own handle for the file, named in each notification.
    pub client_file: i32,
}

/// A hosted file, opened and ready to serve.
pub struct Hosted {
    /// The server's id for the file: the Open reply's `file_id`, which later
    /// transactions on the file carry in their header.
    pub id: u32,
    /// The name clients open it by.
    pub name: String,
    /// Where the file lives on disk, as it was given.
    pub path: PathBuf,
    /// The file's folder as it was when the file was first hosted: it is
    /// read and saved only there ([`FileAt`], #468).
    pub(crate) at: FileAt,
    /// The file's contents: every download, query and write goes here.
    /// Taken through [`Hosted::read_store`] and [`Hosted::write_store`],
    /// which keeps `published` following it.
    store: RwLock<Store>,
    /// The locks clients hold on it: changed only by the engine
    /// ([`Hosted::locks`]), read by anyone ([`Hosted::lock_table`]).
    locks: Mutex<LockTable>,
    /// The connections to tell about changes.
    pub registrations: Mutex<Vec<Registration>>,
    /// Recently rendered container thumbnails (op96).
    pub thumbnails: Mutex<thumbnail::Cache>,
    /// `mInternalCreation` (`[2, 254]`), the second half of the Login nonce.
    pub internal_creation: u32,
    /// Where the deferred field work the file records as pending stands
    /// ([`Hosted::recover`]): finished and saved, a server's file's as it
    /// is loaded, a local file's when an account it admits first logs in,
    /// so that closing before that leaves the file as it was.
    recovery: Mutex<Recovery>,
    /// Why the file takes no more changes, once a save's outcome was
    /// unknown (`Store::writes_refused`): kept here too, so that the
    /// administrator's list of files reads it without waiting for the
    /// store ([`Hosted::writes_refused`]). Set once; hosting the file again
    /// makes a new `Hosted`.
    unknown: std::sync::OnceLock<String>,
    /// Where a failed save is recorded and raised as an alert.
    events: EventLog,
    /// A test's switch that fails one of a script session's reads of the
    /// file ([`ReadFault`]).
    #[cfg(test)]
    pub(crate) read_fault: ReadFault,
}

/// A test's switch that fails the n-th read a script session makes of the
/// file (`scripts::host::SessionReads::begin`), counted from when it is
/// set, or every read from the n-th on. It keeps what the file held when
/// the first of them failed (its tree and its waiting set), and what the
/// session did after it (each step it entered, each operation it
/// submitted), for the test to compare with what the session left.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ReadFault {
    /// The read to fail, 0 for none.
    pub fail_at: std::sync::atomic::AtomicU64,
    /// Whether every read after it fails too, as storage that stays broken.
    pub after: std::sync::atomic::AtomicBool,
    /// The reads counted.
    pub reads: std::sync::atomic::AtomicU64,
    /// Whether the read failed.
    pub failed: std::sync::atomic::AtomicBool,
    /// The file's tree when the read failed.
    pub at_failure: Mutex<Option<om_hbam::OwnedNode>>,
    /// The file's waiting set when the read failed (`Waiting::snapshot`).
    pub waiting_at_failure: Mutex<Option<WaitingSnapshot>>,
    /// What the session did after the read failed, in order: `step` for a
    /// step it entered, and each operation or draft it submitted by name.
    pub since: Mutex<Vec<String>>,
}

/// The indexes and member keys a file's waiting set holds.
#[cfg(test)]
pub(crate) type WaitingSnapshot = (Vec<crate::store::IndexId>, Vec<crate::store::MemberKey>);

#[cfg(test)]
impl ReadFault {
    /// Fail read `n` from now on (0 for none), counting from 0, and with
    /// `after` every read after it too.
    pub fn fail(&self, n: u64, after: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.reads.store(0, Relaxed);
        self.fail_at.store(n, Relaxed);
        self.after.store(after, Relaxed);
        self.failed.store(false, Relaxed);
        *self.at_failure.lock().unwrap() = None;
        *self.waiting_at_failure.lock().unwrap() = None;
        self.since.lock().unwrap().clear();
    }

    /// The tree kept when the read failed, if one did.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn tree_at_failure(&self) -> Option<om_hbam::OwnedNode> {
        self.at_failure.lock().unwrap().clone()
    }

    /// Note `what` the session did, once the read failed.
    pub fn note(&self, what: impl FnOnce() -> String) {
        if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
            self.since.lock().unwrap().push(what());
        }
    }
}

/// Where a file's deferred field work stands ([`Hosted::recover`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Recovery {
    /// Still to be finished and saved.
    Due,
    /// Begun and not ended: a run a panic left stays so.
    Begun,
    /// Ended with an outcome: saved, nothing to do, taken back (the file
    /// read-only) or unknown (the file taking no more changes); or found
    /// left by a panic and the file made read-only.
    Ended,
}

/// The store of a [`Hosted`] file held for writing ([`Hosted::write_store`]).
/// When it is let go, the file's accounts follow a write that changed the
/// schema and did not decode them itself, as an operation does
/// (`Store::follow_accounts`), and the version and its derived state are
/// published for the readers that do not take the store ([`Published`]).
pub struct StoreWriteGuard<'a> {
    hosted: &'a Hosted,
    store: RwLockWriteGuard<'a, Store>,
}

impl std::ops::Deref for StoreWriteGuard<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

impl std::ops::DerefMut for StoreWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

impl Drop for StoreWriteGuard<'_> {
    fn drop(&mut self) {
        // Under the store's write lock still, so that no reader sees the
        // store and the accounts disagree: a write outside an operation
        // (the setup of a file, a test's) has its accounts decoded here.
        self.store.follow_accounts(None);
        // A save left by an unwind after its rename refuses writes from
        // then on without reporting it (`Store::save`): the administrator's
        // list learns it here. Nothing is logged: this may run in an unwind.
        if let Some(why) = self.store.writes_refused() {
            let _ = self.hosted.unknown.set(why.to_owned());
        }
    }
}

/// A hosted file's lock table held for reading ([`Hosted::lock_table`]):
/// it derefs to the table, never mutably, so code outside the engine
/// cannot change a lock.
pub(crate) struct LockTableView<'a>(MutexGuard<'a, LockTable>);

impl std::ops::Deref for LockTableView<'_> {
    type Target = LockTable;
    fn deref(&self) -> &LockTable {
        &self.0
    }
}

impl std::fmt::Debug for Hosted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hosted")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Hosted {
    /// The hosted file `hosted`, whose contents `file` holds, with server
    /// id `id`, as the file holds it: deferred field work a stop
    /// interrupted is finished by [`Hosted::recover`]. A local file comes
    /// with its lock and whether it is read-only. A file FileMaker Server
    /// calls insecure is refused unless `insecure_files` (a local file
    /// never is). A failed save is recorded in `events`.
    fn load(
        id: u32,
        hosted: &HostedFile,
        at: FileAt,
        file: &HbamFile,
        local: Option<(Option<FileLock>, bool)>,
        insecure_files: bool,
        events: &EventLog,
    ) -> Result<Self, HostingError> {
        let open = |error| HostingError::Open {
            name: hosted.name.clone(),
            error,
        };
        if local.is_none() && !insecure_files {
            if let Some(why) = insecurity(file).map_err(open)? {
                return Err(HostingError::Insecure {
                    name: hosted.name.clone(),
                    why,
                });
            }
        }
        let internal_creation = file.internal_creation().map_err(open)?.ok_or_else(|| {
            HostingError::NoInternalCreation {
                name: hosted.name.clone(),
            }
        })?;
        let mut store = Store::load(file, &hosted.name).map_err(open)?;
        if let Some((lock, local_read_only)) = local {
            store.serve_desktop();
            if let Some(lock) = lock {
                store.set_lock(lock);
            }
            if local_read_only {
                store.set_read_only(crate::store::ReadOnlyCause::Opened);
            }
        }
        Ok(Hosted {
            id,
            name: hosted.name.clone(),
            path: hosted.path.clone(),
            at,
            store: RwLock::new(store),
            locks: Mutex::new(LockTable::default()),
            registrations: Mutex::new(Vec::new()),
            thumbnails: Mutex::new(thumbnail::Cache::default()),
            internal_creation,
            recovery: Mutex::new(Recovery::Due),
            unknown: std::sync::OnceLock::new(),
            events: events.clone(),
            #[cfg(test)]
            read_fault: ReadFault::default(),
        })
    }

    /// Finish and save, with `engine`, the deferred field work the file
    /// records as pending (schema-path.md "Implementation notes
    /// (recalculation)"; [`Operation::FinishDeferredWork`]), once: a
    /// server's file as it is loaded, before it is served, a local file
    /// when an account it admits first logs in, so that a local file closed
    /// before that is left as it was. The work and its save are one
    /// operation: when the save fails, the work is taken back, the file is
    /// served as its disk holds it, read-only, and the failure is logged and
    /// alerted; when the save's outcome is unknown, the file keeps the work
    /// and takes no more changes (docs/server-storage.md 5).
    /// The check and the operation are one step, under the state's mutex: a
    /// login that comes while another login's recovery runs waits for it to
    /// end (finished, taken back or unknown) before it is answered, so no
    /// login to a local file is answered, and no record served to it,
    /// before the work ended. The work is marked ended only once the
    /// operation has an outcome: one a panic left (the engine took back
    /// what it did, so the file is as its disk holds it, the work pending)
    /// is found begun by the next login, which gets the file read-only, as
    /// when the work cannot be done, never as if it were recovered.
    pub(crate) fn recover(&self, engine: &FileEngine) {
        let mut state = self
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *state {
            Recovery::Ended => return,
            Recovery::Begun => {
                error!(
                    "{:?}: the deferred field work finished on opening was left part way; \
                     the file is served read-only as its disk holds it",
                    self.name
                );
                self.events.alert(Event::hosted_read_only(
                    &self.name,
                    "finishing its deferred field work was left part way",
                ));
                engine.hold_read_only(self, crate::store::ReadOnlyCause::DeferredWork);
                *state = Recovery::Ended;
                return;
            }
            Recovery::Due => {}
        }
        *state = Recovery::Begun;
        let reply = engine.submit(self, Operation::FinishDeferredWork).reply;
        *state = Recovery::Ended;
        match reply {
            Reply::FinishDeferredWork(Ok(true)) => info!(
                "{:?}: saved the deferred field work finished on opening",
                self.name
            ),
            Reply::FinishDeferredWork(Ok(false)) => {}
            Reply::FinishDeferredWork(Err(error)) if error.unknown => warn!(
                "{:?}: the deferred field work finished on opening may or may not be on \
                 disk: {error}",
                self.name
            ),
            Reply::FinishDeferredWork(Err(error)) => warn!(
                "{:?}: the deferred field work finished on opening was taken back, and the \
                 file is served read-only: {error}",
                self.name
            ),
            other => warn!("{:?}: the engine answered {other:?}", self.name),
        }
    }

    /// The file's contents for reading. A request that panicked while it
    /// held the store poisons its lock; the next one still gets the store
    /// (a commit puts back its changes when it panics, [`Store::guarded`]),
    /// so the file keeps serving every connection rather than none until
    /// the server restarts.
    pub fn read_store(&self) -> RwLockReadGuard<'_, Store> {
        self.store.read().unwrap_or_else(|poisoned| {
            warn!("{:?}: a request panicked holding the store", self.name);
            poisoned.into_inner()
        })
    }

    /// [`Hosted::write_store`] when no one holds the store, `None`
    /// otherwise: for work that must not wait on a request. Only the engine
    /// has a [`Writer`].
    pub(crate) fn try_write_store(&self, _: &Writer) -> Option<StoreWriteGuard<'_>> {
        let mut store = match self.store.try_write() {
            Ok(store) => store,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        store.begin_writing();
        Some(StoreWriteGuard {
            hosted: self,
            store,
        })
    }

    /// The file's contents for writing ([`Hosted::read_store`]). Only the
    /// engine has a [`Writer`]: every change to the file is an operation
    /// it runs (docs/server-storage.md 5).
    pub(crate) fn write_store(&self, _: &Writer) -> StoreWriteGuard<'_> {
        self.store_for_writing()
    }

    /// The file's contents for writing: the engine's ([`Hosted::write_store`])
    /// and this module's test switches'.
    fn store_for_writing(&self) -> StoreWriteGuard<'_> {
        let mut store = self.store.write().unwrap_or_else(|poisoned| {
            warn!("{:?}: a request panicked holding the store", self.name);
            poisoned.into_inner()
        });
        // Views from now on read a new version (`engine::View::version`).
        store.begin_writing();
        StoreWriteGuard {
            hosted: self,
            store,
        }
    }

    /// Make every commit's save of this file fail, or work again, as
    /// storage refusing it would (`ServerHandle::failing_commit_saves`, a
    /// test's switch).
    #[cfg(debug_assertions)]
    pub(crate) fn fail_commit_saves(&self, fail: bool) {
        self.store_for_writing().fail_commit_saves = fail;
    }

    /// A script session's read of the file is about to be made: counted,
    /// and failed when it is the one [`ReadFault`] was set to fail, the
    /// file's tree kept as it is then.
    #[cfg(test)]
    pub(crate) fn script_read(&self) -> Result<(), ReadError> {
        use std::sync::atomic::Ordering::Relaxed;
        let fault = &self.read_fault;
        let n = fault.reads.fetch_add(1, Relaxed) + 1;
        let at = fault.fail_at.load(Relaxed);
        if at == 0 || n < at || (n > at && !fault.after.load(Relaxed)) {
            return Ok(());
        }
        if n == at {
            let store = self.read_store();
            *fault.at_failure.lock().unwrap() = store.node(&[]).cloned();
            // What reads left waiting is kept in debug builds only.
            #[cfg(debug_assertions)]
            {
                *fault.waiting_at_failure.lock().unwrap() = Some(store.waiting_for_tests());
            }
            fault.failed.store(true, Relaxed);
        }
        Err(ReadError::Unreadable {
            path: Vec::new(),
            why: format!("a test failed read {n}"),
        })
    }

    /// Make every save of this file fail at `fault`, or work again with
    /// `None` (`ServerHandle::failing_saves`, a test's switch).
    #[cfg(debug_assertions)]
    pub(crate) fn fail_saves(&self, fault: Option<crate::store::SaveFault>) {
        self.store_for_writing().save_fault = fault;
    }

    /// Why the file takes no more changes until it is hosted again: a
    /// save's outcome was unknown ([`crate::store::SaveError::Unknown`], or
    /// a save left by an unwind after its rename), copied from the store as
    /// a write lets go of it. Read without waiting for the store.
    pub fn writes_refused(&self) -> Option<&str> {
        self.unknown.get().map(String::as_str)
    }

    /// Whether a request panicked holding the store.
    #[cfg(test)]
    pub(crate) fn store_is_poisoned(&self) -> bool {
        self.store.is_poisoned()
    }

    /// The accounts in the file's security catalog as the store holds
    /// them, none when they could not be read: a test's. Every reader of
    /// the accounts reads them through a reading (`engine::Read::derived`).
    #[cfg(test)]
    pub fn accounts(&self) -> Arc<[Arc<FileAccount>]> {
        match self.read_store().derived().accounts() {
            Ok(accounts) => accounts.clone(),
            Err(_) => Arc::new([]),
        }
    }

    /// The file's lock table, to read, also after a request panicked
    /// holding it: only the engine changes it ([`Hosted::locks`]).
    pub(crate) fn lock_table(&self) -> LockTableView<'_> {
        LockTableView(
            self.locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The file's lock table, to change: only the engine has a [`Writer`].
    pub(crate) fn locks(&self, _: &Writer) -> &Mutex<LockTable> {
        &self.locks
    }

    /// The connections registered for the file's change notifications,
    /// also after a request panicked holding them: each change is one push
    /// or retain, which leaves the list whole, and a session's end must
    /// still remove its own.
    pub fn registrations(&self) -> MutexGuard<'_, Vec<Registration>> {
        self.registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Store `journal`, the change an operation made to `store`, this
    /// file's, which the engine holds for writing: the file is written if
    /// it holds changes the disk lacks ([`Store::save_commit`] for a
    /// commit, [`Store::save_change`] for another change; each also writes
    /// the changes others left waiting). Logged, and alerted in the event
    /// log unless it is a [`Saving::QuietCommit`] (a window's found set): a
    /// save that did not write the file put back what the change did when
    /// it had effects (otherwise what waited stays waiting), and one whose
    /// outcome is unknown leaves the file taking no more changes, which is
    /// alerted once, quiet or not.
    pub(crate) fn save_change(
        &self,
        store: &mut Store,
        journal: Journal,
        saving: Saving,
    ) -> Result<(), SaveError> {
        let started = Instant::now();
        let effects = journal.changed();
        let quiet = saving == Saving::QuietCommit;
        let saved = match saving {
            Saving::Commit | Saving::QuietCommit => store.save_commit(&self.at, journal),
            Saving::Change => store.save_change(&self.at, journal),
        };
        match &saved {
            Ok(()) => debug!("{:?}: saved in {:?}", self.name, started.elapsed()),
            Err(SaveError::Unknown(e)) => {
                let why = store
                    .writes_refused()
                    .map_or_else(|| e.to_string(), str::to_owned);
                if self.unknown.set(why).is_ok() {
                    error!(
                        "{:?}: saving to {} could not be confirmed on disk ({e}): the file \
                         keeps the change in memory and takes no more changes until it is \
                         hosted again",
                        self.name,
                        self.path.display()
                    );
                    self.events
                        .alert(Event::save_failed(&self.name, e, SaveOutcome::Unknown));
                }
            }
            Err(SaveError::NotWritten(e)) if quiet => {
                debug!(
                    "{:?}: saving to {} failed: {e}",
                    self.name,
                    self.path.display()
                );
            }
            Err(SaveError::NotWritten(e)) if effects => {
                error!(
                    "{:?}: saving to {} failed, the change was taken back: {e}",
                    self.name,
                    self.path.display()
                );
                self.events
                    .alert(Event::save_failed(&self.name, e, SaveOutcome::TakenBack));
            }
            Err(SaveError::NotWritten(e)) => {
                error!(
                    "{:?}: saving to {} failed, unsaved changes are kept in memory: {e}",
                    self.name,
                    self.path.display()
                );
                self.events
                    .alert(Event::save_failed(&self.name, e, SaveOutcome::Kept));
            }
        }
        saved
    }

    /// The file's account named `name` ([`account_named`],
    /// [`Hosted::accounts`]): a test's.
    #[cfg(test)]
    pub fn account(&self, name: &str) -> Option<Arc<FileAccount>> {
        account_named(&self.accounts(), name)
    }

    /// [`check_password`] of the account named `name`
    /// ([`Hosted::account`]).
    #[cfg(test)]
    pub fn verify_password(&self, name: &str, password: &str) -> Option<Arc<FileAccount>> {
        check_password(&self.name, name, self.account(name), password)
    }
}

/// The account of `accounts` named `name`, matched case-insensitively, as
/// FileMaker matches account names: the first in key order when two names
/// differ only in case.
pub fn account_named(accounts: &[Arc<FileAccount>], name: &str) -> Option<Arc<FileAccount>> {
    let name = name.to_lowercase();
    accounts
        .iter()
        .find(|a| a.name.to_lowercase() == name)
        .cloned()
}

/// `account`, the account of the file named `file` that a login named
/// `name` ([`account_named`]), if `password` is the password the file
/// stores for it. An account whose password this server cannot check is
/// refused, as is one without a name ([Guest]'s is a reference to
/// localized text, not text). A password is hashed once whatever the name,
/// so that the time a login takes does not tell whether the name is an
/// account. Whether the account may log in is [`FileAccount::eligibility`].
pub fn check_password(
    file: &str,
    name: &str,
    account: Option<Arc<FileAccount>>,
    password: &str,
) -> Option<Arc<FileAccount>> {
    /// What a password is checked against when there is no stored one.
    static NONE: std::sync::LazyLock<StoredPassword> = std::sync::LazyLock::new(|| {
        StoredPassword::parse(&StoredPassword::encode("", [0; 10]))
            .expect("an encoded password reads back")
    });
    let stored = match account.as_ref().map(|account| &account.password) {
        Some(Ok(stored)) => stored,
        Some(Err(reason)) => {
            info!("{file:?}: account {name:?}: its password cannot be checked ({reason:?})");
            &NONE
        }
        None => &NONE,
    };
    let matched = std::hint::black_box(stored.verify(password));
    account.filter(|account| matched && account.password.is_ok())
}

/// What [`Hosted::save_change`] stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Saving {
    /// A commit: op70, an authoring edit, a server script's commit and Set
    /// Next Serial Value, an account edit.
    Commit,
    /// A window's saved found set, whose failed save is not alerted.
    QuietCommit,
    /// Any other change.
    Change,
}

/// FileMaker's error for a save that failed (OpenMaker's choice; how
/// FileMaker Server reports a failed write is not observed): 809
/// "Disk/volume is full" for a full disk, a quota or a size limit, 810
/// "Disk/volume is locked" for a read-only file system, else 800 "Unable to
/// create file on disk" (the save writes a new copy of the file).
pub fn save_error(error: &std::io::Error) -> u16 {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded | ErrorKind::FileTooLarge => 809,
        ErrorKind::ReadOnlyFilesystem => 810,
        _ => 800,
    }
}

/// Change Password: set the password of `op.account` from `old` to `new`,
/// as FileMaker's `SetPassword` sets the signed-in account's own
/// (local-files.md 2.4): the stored password `06` with a new salt from
/// `environment` (none: "no random salt", and nothing changes), the status `0a` stamped with the environment's time to
/// the second with its other flags kept and no change due, and the
/// checksum `04` again; the member's other keys are kept. The new one must
/// have at most 100 characters; an account that may not manage itself
/// (not full access, nor a set managing accounts outside [Full Access]) is
/// checked as `SetPassword` checks it: its set must let it change its own
/// password, and the new one must have at least the set's minimum length
/// (`0d`) and differ from the old one. A `manager` caller (a script
/// running with full access) skips those checks as one that may manage
/// the account does. Each password is dropped, and so wiped, once it is
/// no longer needed: the old one once it is checked, the new one once it
/// is hashed. One commit in `tx`, which the engine saves or takes back.
/// With a stamp the account and the catalog's index get FileMaker's
/// modification stamps ([`rewrite_account`]).
pub(crate) fn change_password(
    op: ChangePassword,
    old: Password,
    new: Password,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<(), PasswordChangeError> {
    use om_data::host::privilege_set::Options;
    let ChangePassword {
        account,
        manager,
        stamp,
    } = op;
    if new.text().chars().count() > 100 {
        return Err(PasswordChangeError::TooLong);
    }
    let now = environment.now();
    let Some(record) = find_account(tx, &account) else {
        return Err(PasswordChangeError::Refused);
    };
    let view = crate::store::view(&record);
    let kind = AccountKind::decode(member(&view, ACCOUNT_KIND));
    if kind != Some(AccountKind::FileMakerFile) {
        return Err(PasswordChangeError::Refused);
    }
    // FileMaker's check of the old password gives 210 for an inactive
    // account before it tests the password (local-files.md 2.2), and
    // Change Password accepts only 0 or 211.
    let current =
        AccountStatus::decode(member(&view, ACCOUNT_STATUS)).ok_or(PasswordChangeError::Refused)?;
    if !current.is_active() {
        return Err(PasswordChangeError::Inactive);
    }
    let stored = view.child(ACCOUNT_PASSWORD).and_then(|n| n.assembled());
    let Some(stored) = stored
        .as_deref()
        .and_then(|bytes| StoredPassword::parse(bytes).ok())
        .filter(|password| password.verify(old.text()))
    else {
        return Err(PasswordChangeError::Refused);
    };
    drop(old);
    let set_ref = member_owned(&view, ACCOUNT_PRIVILEGES);
    let set = set_ref
        .as_deref()
        .and_then(|r| r.split_first())
        .filter(|(length, key)| usize::from(**length) == key.len())
        .and_then(|(_, key)| {
            let path = [
                PRIVILEGE_SETS_PATH[0],
                PRIVILEGE_SETS_PATH[1],
                PRIVILEGE_SETS_PATH[2],
                key,
            ];
            tx.subtree(&path, NO_LIMIT)
                .ok()
                .flatten()
                .map(Cow::into_owned)
        });
    let set = set.as_ref().map(crate::store::view);
    let Some(set) = set else {
        return Err(PasswordChangeError::NotAllowed);
    };
    // An account that may manage itself (full access, or a set that
    // manages accounts outside [Full Access]) skips the checks.
    let full = manager || set.segment.0 == FULL_ACCESS_SET;
    let decoded = om_data::host::privilege_set::decode(&set);
    let manages = full
        || decoded
            .as_ref()
            .is_some_and(|d| d.options.has(Options::MANAGE_ACCOUNTS));
    if !manages {
        if !decoded.is_some_and(|d| d.options.has(Options::CHANGE_PASSWORD)) {
            return Err(PasswordChangeError::NotAllowed);
        }
        let minimum = match member(&set, SET_PASSWORD_MINIMUM) {
            Some(&[high, low]) => u16::from_be_bytes([high, low]),
            _ => 0,
        };
        if new.text().chars().count() < usize::from(minimum) {
            return Err(PasswordChangeError::TooShort(minimum));
        }
        // Unchanged when it hashes as the current one: a text that only
        // decomposes alike (`é`, `e` U+0301) is the same password.
        if stored.verify(new.text()) {
            return Err(PasswordChangeError::Unchanged);
        }
    }
    let civil = now.datetime();
    let status = current.password_set(civil.date(), seconds(civil)).encode();
    let salt = salt(environment).map_err(PasswordChangeError::Unwritten)?;
    let password = StoredPassword::encode(new.text(), salt);
    drop(new);
    let time = stamp.as_ref().map(|_| host_time_key_at(&now));
    let stamp = mod_stamp(stamp.as_ref(), time.as_deref());
    rewrite_account(tx, &record, Some(password), status, stamp.as_ref())
        .map_err(PasswordChangeError::Unwritten)
}

/// Reset Account Password (136) for the account named `op.account`, as
/// FileMaker's `DoResetAccount` writes it (local-files.md 2.4): the stored
/// password `06`, `new`, with a new salt from `environment` (none: "no
/// random salt", and nothing changes), the status `0a`
/// stamped with the environment's time to the second with its other flags
/// kept and a change due at the next login when `op.expire`, and the
/// checksum `04` ([`rewrite_account`], with the stamp). Only a
/// FileMaker-file account has a password to reset (206, as `SetPassword`
/// refuses another kind); more than 100 characters is 11. `new` is
/// dropped, and so wiped, once it is hashed. The caller has checked that
/// the session may manage accounts.
pub(crate) fn reset_account_password(
    op: ResetPassword,
    new: Password,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<(), AccountEditError> {
    let ResetPassword {
        account,
        expire,
        stamp,
    } = op;
    if new.text().chars().count() > 100 {
        return Err(AccountEditError::TooLong);
    }
    let now = environment.now();
    let record = find_account(tx, &AccountRef::Name(account)).ok_or(AccountEditError::Missing)?;
    let view = crate::store::view(&record);
    if AccountKind::decode(member(&view, ACCOUNT_KIND)) != Some(AccountKind::FileMakerFile) {
        return Err(AccountEditError::NotFileAccount);
    }
    let current = AccountStatus::decode(member(&view, ACCOUNT_STATUS))
        .ok_or_else(|| AccountEditError::Unwritten("its status does not decode".into()))?;
    let civil = now.datetime();
    let status = encoded(current.password_reset(civil.date(), seconds(civil), expire))
        .map_err(AccountEditError::Unwritten)?;
    let salt = salt(environment).map_err(AccountEditError::Unwritten)?;
    let password = StoredPassword::encode(new.text(), salt);
    drop(new);
    let time = stamp.as_ref().map(|_| host_time_key_at(&now));
    let stamp = mod_stamp(stamp.as_ref(), time.as_deref());
    rewrite_account(tx, &record, Some(password), status, stamp.as_ref())
        .map_err(AccountEditError::Unwritten)
}

/// Enable Account (137): the account named `op.account` made active or
/// inactive, as FileMaker's `SetEnabled` sets it (local-files.md 2.4):
/// only the status's inactive bit changes (an account without a status
/// gets 1/1/1024's), and the checksum `04` with it ([`rewrite_account`],
/// with the stamp). Whether it changed: an account already in that state
/// is left as it is. Refused (3) for an account an OAuth identity provider
/// or Claris ID signs in, and for making a [Full Access] account inactive,
/// which Claris documents the step does not do. The caller has checked
/// that the session may manage accounts.
pub(crate) fn enable_account(
    op: EnableAccount,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<bool, AccountEditError> {
    let EnableAccount {
        account,
        active,
        stamp,
    } = op;
    let record = find_account(tx, &AccountRef::Name(account)).ok_or(AccountEditError::Missing)?;
    let view = crate::store::view(&record);
    // The file's own accounts and external server groups (kind 1);
    // OAuth identities are `provider << 3 | 2` (local-files.md 2.2).
    match AccountKind::decode(member(&view, ACCOUNT_KIND)) {
        Some(AccountKind::FileMakerFile | AccountKind::External(1)) => {}
        _ => return Err(AccountEditError::Unsupported),
    }
    if !active && account_set(&view) == Some(FULL_ACCESS_SET) {
        return Err(AccountEditError::FullAccess);
    }
    let current = AccountStatus::decode(member(&view, ACCOUNT_STATUS))
        .ok_or_else(|| AccountEditError::Unwritten("its status does not decode".into()))?;
    let wanted = current.enabled(active);
    if wanted == current {
        return Ok(false);
    }
    let status = encoded(wanted).map_err(AccountEditError::Unwritten)?;
    let time = stamp.as_ref().map(|_| host_time_key_at(&environment.now()));
    let stamp = mod_stamp(stamp.as_ref(), time.as_deref());
    rewrite_account(tx, &record, None, status, stamp.as_ref())
        .map(|()| true)
        .map_err(AccountEditError::Unwritten)
}

/// Delete Account (135): the account named `op.account` removed, as
/// FileMaker's `DoDeleteAccount` removes it (local-files.md 2.4 and 3):
/// its member, and from the catalog's index its name list entry, its
/// place in the display order (written again from 1) and its `07` entry,
/// and one from the count; the last key issued stays. One commit in `tx`,
/// which the engine saves or takes back, with the index stamped when a
/// stamp is given. As Claris documents, a [Full Access] account is not
/// deleted, nor an OAuth or Claris ID one; [Guest] is not either (all 3,
/// inferred). An index that does not hold the accounts as
/// [`index_without`] reads it is not rewritten. The caller has checked
/// that the session may manage accounts.
pub(crate) fn delete_account(
    op: DeleteAccount,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> Result<(), AccountEditError> {
    let DeleteAccount { account, stamp } = op;
    let record = find_account(tx, &AccountRef::Name(account)).ok_or(AccountEditError::Missing)?;
    let view = crate::store::view(&record);
    // [Guest] (key 1) is the file's own; OAuth and Claris ID
    // identities are not the step's (Claris, "Delete Account").
    let deletable = matches!(
        AccountKind::decode(member(&view, ACCOUNT_KIND)),
        Some(AccountKind::FileMakerFile | AccountKind::External(1))
    );
    if record.segment == [1] || !deletable {
        return Err(AccountEditError::Unsupported);
    }
    if account_set(&view) == Some(FULL_ACCESS_SET) {
        return Err(AccountEditError::FullAccess);
    }
    // The catalog's members by key, listed: their records are not read.
    let view: &dyn View = tx;
    let members: Vec<Vec<u8>> = view
        .children(&ACCOUNTS_PATH, Bound::Unbounded, Direction::Forward)
        .map(|child| child.map(|child| child.segment.into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| AccountEditError::Unwritten(e.to_string()))?;
    if members.is_empty() && View::node(tx, &ACCOUNTS_PATH).ok().flatten().is_none() {
        return Err(AccountEditError::Unwritten("no account catalog".into()));
    }
    let index = tx
        .subtree(&ACCOUNT_INDEX_PATH, NO_LIMIT)
        .ok()
        .flatten()
        .map(Cow::into_owned)
        .ok_or_else(|| AccountEditError::Unwritten("the account catalog has no index".into()))?;
    let mut index =
        index_without(index, &record.segment, &members).map_err(AccountEditError::Unwritten)?;
    if let Some(stamp) = &stamp {
        let time = host_time_key_at(&environment.now());
        index = crate::authoring::stamped(index, &stamp.user, &stamp.account, &time);
    }
    let path =
        |segments: &[&[u8]]| -> Vec<Vec<u8>> { segments.iter().map(|s| s.to_vec()).collect() };
    let mut member = path(&ACCOUNTS_PATH);
    member.push(record.segment.clone());
    tx.begin_commit();
    if let Err(reason) = tx.remove(member) {
        tx.refuse(reason);
    }
    if let Err(reason) = tx.replace(path(&ACCOUNT_INDEX_PATH), index) {
        tx.refuse(reason);
    }
    Ok(())
}

/// Write account `record` again with `password` (its own when `None`) and
/// `status`, and the checksum `04` they give, in the commit of `tx`; its
/// other members are kept. With `stamp`, the account and the account
/// catalog's index `[23, 1, 1]` get the modification stamps FileMaker
/// writes on every catalog commit (`IncrementModCount`, schema-path.md
/// 8.1), so a client holding a copy of the catalog sees that it is stale.
///
/// Refused, changing nothing, when the account stores a checksum other
/// than the one its current members give: FileMaker Pro refuses a file
/// whose account checksum is wrong (808), so an account this server does
/// not read as FileMaker does is not rewritten. One without a checksum,
/// which FileMaker always writes, gets one.
fn rewrite_account(
    tx: &mut Transaction<'_>,
    record: &om_hbam::OwnedNode,
    password: Option<Vec<u8>>,
    status: Vec<u8>,
    stamp: Option<&ModStamp<'_>>,
) -> Result<(), String> {
    let view = crate::store::view(record);
    let current = view
        .child(ACCOUNT_PASSWORD)
        .and_then(|n| n.assembled())
        .ok_or("it stores no password")?;
    let name = view
        .child(ACCOUNT_NAME)
        .and_then(|n| n.text()?.ok())
        .ok_or("its name is not text")?;
    let description = view.child(ACCOUNT_DESCRIPTION).and_then(|n| n.text()?.ok());
    let kind = member(&view, ACCOUNT_KIND).and_then(|v| v.first().copied());
    let set = member_owned(&view, ACCOUNT_PRIVILEGES);
    let value = |tx: &Transaction<'_>, path: &[&[u8]]| -> Option<Vec<u8>> {
        View::node(tx, path)
            .ok()
            .flatten()
            .and_then(|head| head.value)
            .map(Cow::into_owned)
    };
    let version = value(tx, &[&[2], &[5]])
        .and_then(|v| <[u8; 2]>::try_from(v.as_slice()).ok())
        .map(u16::from_be_bytes);
    let access_uuid = value(tx, &[&[23], &[4], &[1], &[0x10]]);
    let creation = value(tx, &[&[2], &[254]]);
    let (access_uuid, creation) = (access_uuid.as_deref(), creation.as_deref());
    let checksum = |password: &[u8], status: Option<&[u8]>| {
        crate::accounts::checksum(&crate::accounts::ChecksumInput {
            version,
            access_uuid,
            creation,
            key: &record.segment,
            kind,
            name: &name,
            password,
            description: description.as_deref(),
            status,
            set: set.as_deref(),
        })
    };
    if let Some(stored) = member(&view, ACCOUNT_CHECKSUM) {
        if stored != checksum(&current, member(&view, ACCOUNT_STATUS)) {
            return Err("its checksum is not the one its members give".into());
        }
    }
    let sum = checksum(password.as_deref().unwrap_or(&current), Some(&status));
    let mut changed = record.clone();
    if let Some(password) = password {
        changed.put(
            &[ACCOUNT_PASSWORD],
            om_hbam::OwnedNode::leaf(Vec::new(), password),
        );
    }
    changed.put(
        &[ACCOUNT_STATUS],
        om_hbam::OwnedNode::leaf(Vec::new(), status),
    );
    changed.put(
        &[ACCOUNT_CHECKSUM],
        om_hbam::OwnedNode::leaf(Vec::new(), sum.to_vec()),
    );
    let path: Vec<Vec<u8>> = ACCOUNTS_PATH
        .iter()
        .map(|s| s.to_vec())
        .chain([record.segment.clone()])
        .collect();
    let mut members = Vec::new();
    if let Some(stamp) = stamp {
        let index = tx
            .subtree(&ACCOUNT_INDEX_PATH, NO_LIMIT)
            .ok()
            .flatten()
            .map(Cow::into_owned)
            .ok_or("the account catalog has no index")?;
        changed = crate::authoring::stamped(changed, stamp.user, stamp.account, stamp.time);
        members.push((
            ACCOUNT_INDEX_PATH.iter().map(|s| s.to_vec()).collect(),
            crate::authoring::stamped(index, stamp.user, stamp.account, stamp.time),
        ));
    }
    members.insert(0, (path, changed));
    // One commit; a member the file cannot hold fails it, the others kept
    // out with it.
    tx.begin_commit();
    for (path, node) in members {
        if let Err(reason) = tx.replace(path, node) {
            tx.refuse(reason);
        }
    }
    Ok(())
}

/// The modification stamp `stamp` names, at host time key `time`
/// ([`ModStamp`]).
fn mod_stamp<'a>(stamp: Option<&'a AccountStamp>, time: Option<&'a [u8]>) -> Option<ModStamp<'a>> {
    Some(ModStamp {
        user: &stamp?.user,
        account: &stamp?.account,
        time: time?,
    })
}

/// Key of the account records in the security catalog: `[23, 1, 5]`.
const ACCOUNTS_PATH: [&[u8]; 3] = [&[23], &[1], &[5]];

/// Key of the account catalog's index (local-files.md 3).
const ACCOUNT_INDEX_PATH: [&[u8]; 3] = [&[23], &[1], &[1]];

/// Key of an account record's name.
const ACCOUNT_NAME: &[u8] = &[16];

/// Key of an account record's password (`om_crypto::account`).
const ACCOUNT_PASSWORD: &[u8] = &[6];

/// Key of an account record's kind ([`AccountKind`]).
const ACCOUNT_KIND: &[u8] = &[2];

/// Key of an account record's status ([`AccountStatus`]).
const ACCOUNT_STATUS: &[u8] = &[0x0a];

/// Key of an account record's privilege set reference: a key string
/// naming a member of `[23, 2, 5]` (session-identity.md 1).
const ACCOUNT_PRIVILEGES: &[u8] = &[0x0b];

/// Key of the privilege set records: `[23, 2, 5]`.
const PRIVILEGE_SETS_PATH: [&[u8]; 3] = [&[23], &[2], &[5]];

/// Key of a privilege set's password lifetime in days: a big-endian u16.
const SET_PASSWORD_DAYS: &[u8] = &[0x0c];

/// Key of a privilege set's minimum password length: a big-endian u16.
const SET_PASSWORD_MINIMUM: &[u8] = &[0x0d];

/// Key of an account record's description.
const ACCOUNT_DESCRIPTION: &[u8] = &[3];

/// Key of an account record's checksum ([`crate::accounts::checksum`]).
const ACCOUNT_CHECKSUM: &[u8] = &[4];

/// [`member`], owned.
fn member_owned(record: &om_hbam::Node<'_>, key: &[u8]) -> Option<Vec<u8>> {
    member(record, key).map(<[u8]>::to_vec)
}

/// Why [`Hosted::change_password`] changed nothing, as FileMaker's
/// `SetPassword` refuses (local-files.md 2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordChangeError {
    /// No FileMaker-file account of that name whose password is the old
    /// one (212).
    Refused,
    /// Its privilege set does not let it change its own password (206).
    NotAllowed,
    /// Shorter than its set's minimum length (208, with the minimum).
    TooShort(u16),
    /// The new password is the current one (209).
    Unchanged,
    /// More than 100 characters (11).
    TooLong,
    /// The account is inactive (210).
    Inactive,
    /// The file could not be written, and nothing changed.
    Unwritten(String),
    /// The file was written but could not be confirmed on disk: the
    /// password may or may not be the new one there, and the file takes no
    /// more changes until it is hosted again (docs/server-storage.md 5).
    OutcomeUnknown(String),
}

/// Why Reset Account Password or Enable Account changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountEditError {
    /// No account has that name (213, inferred: FileMaker's code for an
    /// account it does not find there is not observed).
    Missing,
    /// Not a FileMaker-file account, which alone has a password here (206).
    NotFileAccount,
    /// More than 100 characters (11).
    TooLong,
    /// An account the step does not change: one an OAuth identity
    /// provider or Claris ID signs in, or [Guest] for Delete Account (3,
    /// inferred).
    Unsupported,
    /// Making a [Full Access] account inactive (3, inferred: Claris
    /// documents that Enable Account does not; its code is not observed),
    /// or deleting one (3, OpenMaker's choice).
    FullAccess,
    /// The account could not be rewritten, or the file could not be
    /// written, and nothing changed.
    Unwritten(String),
    /// The file was written but could not be confirmed on disk: the edit
    /// may or may not be in it, and the file takes no more changes until it
    /// is hosted again (docs/server-storage.md 5).
    OutcomeUnknown(String),
}

impl AccountEditError {
    /// FileMaker's error code; `None` for an account or file that could not
    /// be written, or whose save could not be confirmed.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Missing => Some(213),
            Self::NotFileAccount => Some(206),
            Self::TooLong => Some(11),
            Self::Unsupported | Self::FullAccess => Some(3),
            Self::Unwritten(_) | Self::OutcomeUnknown(_) => None,
        }
    }
}

impl PasswordChangeError {
    /// FileMaker's error code; `None` for a file that could not be written,
    /// or whose save could not be confirmed.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Refused => Some(212),
            Self::NotAllowed => Some(206),
            Self::TooShort(_) => Some(208),
            Self::Unchanged => Some(209),
            Self::TooLong => Some(11),
            Self::Inactive => Some(210),
            Self::Unwritten(_) | Self::OutcomeUnknown(_) => None,
        }
    }
}

/// An account named by its record key under `[23, 1, 5]`, which a rename
/// does not change, or by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountRef {
    Key(Vec<u8>),
    Name(String),
}

/// Who changed a catalog, and when: the modification stamp FileMaker
/// writes (schema-path.md 8.1), `time` being the host time key of 3.1.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModStamp<'a> {
    pub user: &'a str,
    pub account: &'a str,
    pub time: &'a [u8],
}

/// The record of `account` as `view` holds it, whole: by key, or by name
/// as FileMaker matches names, ignoring case, the catalog `[23, 1, 5]`
/// listed and each account's name read. [Guest], whose name is not text,
/// is found only by its key.
fn find_account(view: &dyn View, account: &AccountRef) -> Option<om_hbam::OwnedNode> {
    let key = match account {
        AccountRef::Key(key) => key.clone(),
        AccountRef::Name(name) => {
            let wanted = name.to_lowercase();
            view.children(&ACCOUNTS_PATH, Bound::Unbounded, Direction::Forward)
                .map_while(Result::ok)
                .map(|child| child.segment.into_owned())
                .find(|key| {
                    let path = [
                        ACCOUNTS_PATH[0],
                        ACCOUNTS_PATH[1],
                        ACCOUNTS_PATH[2],
                        key,
                        ACCOUNT_NAME,
                    ];
                    view.subtree(&path, NO_LIMIT)
                        .ok()
                        .flatten()
                        .and_then(|name| crate::store::view(&name).text()?.ok())
                        .is_some_and(|text| text.to_lowercase() == wanted)
                })?
        }
    };
    let path = [ACCOUNTS_PATH[0], ACCOUNTS_PATH[1], ACCOUNTS_PATH[2], &key];
    view.subtree(&path, NO_LIMIT)
        .ok()
        .flatten()
        .map(Cow::into_owned)
}

/// The key of the privilege set account `view` is in (`0b`, a key string).
fn account_set<'a>(view: &om_hbam::Node<'a>) -> Option<&'a [u8]> {
    member(view, ACCOUNT_PRIVILEGES)
        .and_then(|r| r.split_first())
        .filter(|(length, key)| usize::from(**length) == key.len())
        .map(|(_, key)| key)
}

/// The account catalog's index `index` without account `key`
/// (local-files.md 3): its name list entry and its place in the display
/// order removed, the order numbered again from 1, its `07` entry removed
/// and the count one less. Why not, when the index is not one this
/// server reads as FileMaker keeps it for `members` (the accounts before
/// the deletion): its name list and its display order (positions 1 to n
/// as four bytes) must each name every member once, as a key string; the
/// count must be the number of members; and the account's `07` entry, if
/// it has one, must be an empty leaf, so nothing unread is dropped.
fn index_without(
    mut index: om_hbam::OwnedNode,
    key: &[u8],
    members: &[Vec<u8>],
) -> Result<om_hbam::OwnedNode, String> {
    let key_string = |key: &[u8]| {
        let mut named = vec![key.len() as u8];
        named.extend_from_slice(key);
        named
    };
    let mut wanted: Vec<Vec<u8>> = members.iter().map(|m| key_string(m)).collect();
    wanted.sort();
    let names_each_member = |node: &om_hbam::OwnedNode| {
        let mut named: Vec<Vec<u8>> = node
            .children
            .iter()
            .map(|n| n.value.clone().unwrap_or_default())
            .collect();
        named.sort();
        named == wanted && node.children.iter().all(|n| n.children.is_empty())
    };
    let named = key_string(key);
    let names = |n: &om_hbam::OwnedNode| n.value.as_deref() == Some(&named[..]);
    let list = index.child_mut(&[1]).ok_or("the index has no name list")?;
    if !names_each_member(list) {
        return Err("its name list does not name each account once".into());
    }
    list.children.retain(|n| !names(n));
    let order = index
        .child_mut(&[3])
        .ok_or("the index has no display order")?;
    let contiguous = order
        .children
        .iter()
        .enumerate()
        .all(|(i, n)| u32::try_from(i + 1).is_ok_and(|p| n.segment == p.to_be_bytes()));
    if !contiguous || !names_each_member(order) {
        return Err("its display order does not place each account once, from 1".into());
    }
    order.children.retain(|n| !names(n));
    for (position, place) in order.children.iter_mut().enumerate() {
        place.segment = u32::try_from(position + 1)
            .map_err(|_| "too many places")?
            .to_be_bytes()
            .to_vec();
    }
    if let Some(cache) = index.child_mut(&[7]) {
        if let Some(entry) = cache.child(key) {
            if !entry.children.is_empty() || entry.value.as_deref().is_some_and(|v| !v.is_empty()) {
                return Err("its 07 entry holds data this server does not read".into());
            }
        }
        cache.children.retain(|n| n.segment != key);
    }
    let count = index
        .child(&[4])
        .and_then(|n| n.value.as_deref())
        .and_then(|v| <[u8; 4]>::try_from(v).ok())
        .map(u32::from_be_bytes)
        .ok_or("the index has no count")?;
    if usize::try_from(count).ok() != Some(members.len()) || count == 0 {
        return Err("its count is not the number of accounts".into());
    }
    index.set_child(om_hbam::OwnedNode::leaf(
        vec![4],
        (count - 1).to_be_bytes().to_vec(),
    ));
    Ok(index)
}

/// `status` as FileMaker stores it, refused when the value would not read
/// back as `status`: flags added to a fraction of a second the file
/// already held can pass the six digits the value keeps, and a status cut
/// back would lose them.
fn encoded(status: AccountStatus) -> Result<Vec<u8>, String> {
    let value = status.encode();
    if AccountStatus::decode(Some(&value)) == Some(status) {
        Ok(value)
    } else {
        Err("its status cannot hold the flags".into())
    }
}

/// The seconds since midnight of `now`, to the second.
fn seconds(now: jiff::civil::DateTime) -> u32 {
    let time = now.time();
    time.hour() as u32 * 3600 + time.minute() as u32 * 60 + time.second() as u32
}

/// A new salt for a stored password, from `environment`'s random source,
/// or why there is none: a source that fails refuses the edit, which
/// changes nothing ([`Environment::try_fill_random`]).
fn salt(environment: &dyn Environment) -> Result<[u8; om_crypto::account::SALT_LEN], String> {
    let mut salt = [0u8; om_crypto::account::SALT_LEN];
    environment
        .try_fill_random(&mut salt)
        .map_err(|_| "no random salt".to_string())?;
    Ok(salt)
}

/// The value of `record`'s member `key`: `None` when it has none, and an
/// empty value, which nothing decodes, for one that is not a plain value.
fn member<'a>(record: &om_hbam::Node<'a>, key: &[u8]) -> Option<&'a [u8]> {
    record.child(key).map(|n| match n.value {
        Some(value) if n.children.is_empty() => value,
        _ => &[],
    })
}

/// How many days a password lasts under `set` (0: no limit).
fn password_days(set: &om_hbam::Node) -> Option<u16> {
    match member(set, SET_PASSWORD_DAYS) {
        None => Some(0),
        Some(&[high, low]) => Some(u16::from_be_bytes([high, low])),
        Some(_) => None,
    }
}

/// Key of the account catalog's own members (local-files.md 3): `[23, 1, 1]`.
const ACCOUNT_CATALOG_PATH: [&[u8]; 3] = [&[23], &[1], &[1]];

/// Key of the account catalog's stored password for File Options' "Log in
/// using" (`KRequests`, local-files.md 3.1); its `16` names the account.
const LOGIN_USING_PASSWORD: &[u8] = &[6];

/// The record key of the built-in [Full Access] set (FileMaker's `KIndex`).
const FULL_ACCESS_SET: &[u8] = &[1];

/// Key of a privilege set's other privileges (session-identity.md 2.3), and
/// the bit of `kCanModifyAccounts` ("Manage accounts that don't have Full
/// Access") once the stored value is XORed with `0x6000`.
const SET_OPTIONS: &[u8] = &[0x0a];
const MANAGES_ACCOUNTS: u16 = 0x0010;

/// Why FileMaker Server would not host a file: anyone could open it with
/// full access (docs/re/local-files.md 3.2). An active account in the
/// [Full Access] set, or in a set that manages accounts, is [Guest], has
/// the empty password, or is the account File Options log in as with the
/// password the file stores for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Insecure {
    /// [Guest] (record `01`).
    Guest,
    /// The account with this name opens with the empty password.
    EmptyPassword(String),
    /// File Options log in as the account with this name, with its
    /// password, which the file stores.
    StoredPassword(String),
}

impl std::fmt::Display for Insecure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Insecure::Guest => f.write_str("[Guest] has full access or manages accounts"),
            Insecure::EmptyPassword(name) => write!(
                f,
                "{name:?}, which has full access or manages accounts, has no password"
            ),
            Insecure::StoredPassword(name) => write!(
                f,
                "it logs in as {name:?}, which has full access or manages accounts, with the \
                 password it stores"
            ),
        }
    }
}

/// Whether `file` is one FileMaker Server would not host ([`Insecure`]),
/// read as FileMaker reads it (`DBUserAccountCatalog::IsInsecure`): an
/// account whose status OpenMaker cannot read counts as active, and an
/// account of another kind has no password to try.
fn insecurity(file: &HbamFile) -> om_hbam::Result<Option<Insecure>> {
    let Some(accounts) = file.subtree(&ACCOUNTS_PATH)? else {
        return Ok(None);
    };
    let sets = file.subtree(&PRIVILEGE_SETS_PATH)?;
    let text = |node: Option<&om_hbam::Node>| {
        node.and_then(om_hbam::Node::text)
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    // The account File Options log in as, and the password they store.
    let header = file.subtree(&ACCOUNT_CATALOG_PATH)?;
    let (logs_in_as, stored_login) = header.as_ref().map_or_else(Default::default, |header| {
        (
            text(header.child(ACCOUNT_NAME)).to_lowercase(),
            text(header.child(LOGIN_USING_PASSWORD)),
        )
    });
    for account in &accounts.children {
        let status = AccountStatus::decode(member(account, ACCOUNT_STATUS));
        if status.is_some_and(|status| !status.is_active()) {
            continue;
        }
        let Some((_, set)) = member(account, ACCOUNT_PRIVILEGES)
            .and_then(|r| r.split_first())
            .filter(|(length, key)| usize::from(**length) == key.len())
        else {
            continue;
        };
        let manages_accounts = || {
            let options = sets
                .as_ref()?
                .child(set)
                .and_then(|s| member(s, SET_OPTIONS))?;
            let &[high, low] = options else {
                return None;
            };
            Some(u16::from_be_bytes([high, low]) ^ 0x6000)
        };
        if set != FULL_ACCESS_SET && manages_accounts().is_none_or(|o| o & MANAGES_ACCOUNTS == 0) {
            continue;
        }
        if account.segment.0 == GUEST_KEY {
            return Ok(Some(Insecure::Guest));
        }
        if AccountKind::decode(member(account, ACCOUNT_KIND)) != Some(AccountKind::FileMakerFile) {
            continue;
        }
        let Some(stored) = account
            .child(ACCOUNT_PASSWORD)
            .and_then(|n| n.assembled())
            .and_then(|bytes| StoredPassword::parse(&bytes).ok())
        else {
            continue;
        };
        let name = text(account.child(ACCOUNT_NAME));
        if stored.verify("") {
            return Ok(Some(Insecure::EmptyPassword(name)));
        }
        if !logs_in_as.is_empty()
            && logs_in_as == name.to_lowercase()
            && stored.verify(&stored_login)
        {
            return Ok(Some(Insecure::StoredPassword(name)));
        }
    }
    Ok(None)
}

/// The accounts in the security catalog of the file `view` reads, in key
/// order: each account record of `[23, 1, 5]`, read whole one at a time,
/// with the privilege set it names from `[23, 2, 5]`, read whole once.
pub(crate) fn read_accounts(view: &dyn View) -> Result<Vec<FileAccount>, ReadError> {
    let sets = view.subtree(&PRIVILEGE_SETS_PATH, NO_LIMIT)?;
    let sets = sets.as_deref().map(crate::store::view);
    let mut accounts = Vec::new();
    for account in view.subtrees(
        &ACCOUNTS_PATH,
        Bound::Unbounded,
        Direction::Forward,
        NO_LIMIT,
    ) {
        // A copy the view made of the record, its password verifier with
        // it, is wiped once decoded (the test view's; the file engine's
        // view lends its tree).
        let account = crate::engine::Wiped(account?);
        let account = &account.0;
        // A Login reply names the account by its record key as a key path
        // with one-byte lengths (`[len][key]`, network-protocol.md 5.6.5):
        // an account whose key is too long for that is not admitted.
        if account.segment.len() >= usize::from(u8::MAX) {
            continue;
        }
        let account = crate::store::view(account);
        let decoded = (|| {
            let name = account.child(ACCOUNT_NAME)?.text()?.ok()?;
            let password = match account.child(ACCOUNT_PASSWORD).and_then(|n| n.assembled()) {
                Some(std::borrow::Cow::Borrowed(bytes)) => {
                    StoredPassword::parse(bytes).map_err(PasswordUnavailable::Unsupported)
                }
                // A verifier stored in pieces, assembled into a copy, which
                // is wiped once parsed.
                Some(std::borrow::Cow::Owned(bytes)) => {
                    let bytes = zeroize::Zeroizing::new(bytes);
                    StoredPassword::parse(&bytes).map_err(PasswordUnavailable::Unsupported)
                }
                None => Err(PasswordUnavailable::Missing),
            };
            // The set the account names: its reference is a key string, a
            // length and then the set's record key.
            let reference = member(&account, ACCOUNT_PRIVILEGES);
            let set = reference
                .and_then(|r| r.split_first())
                .filter(|(length, key)| usize::from(**length) == key.len())
                .and_then(|(_, key)| sets.as_ref()?.child(key));
            Some(FileAccount {
                name,
                key: account.segment.0.to_vec(),
                password,
                kind: AccountKind::decode(member(&account, ACCOUNT_KIND)),
                status: AccountStatus::decode(member(&account, ACCOUNT_STATUS)),
                access: set.and_then(|set| set_access(set).ok()),
                password_days: set.and_then(password_days),
            })
        })();
        accounts.extend(decoded);
    }
    Ok(accounts)
}

/// Errors loading the hosted files.
#[derive(Debug)]
pub enum HostingError {
    Open {
        name: String,
        error: om_hbam::Error,
    },
    NoInternalCreation {
        name: String,
    },
    DuplicateName {
        name: String,
    },
    /// The file another name already hosts (a link to it, or another hard
    /// link): two stores of one file would each save over the other's
    /// changes.
    SameFile {
        name: String,
        hosted_as: String,
    },
    /// A file FileMaker Server would not host, and the server does not host
    /// insecure files ([`crate::ServerConfig::insecure_files`]).
    Insecure {
        name: String,
        why: Insecure,
    },
}

impl std::fmt::Display for HostingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostingError::Open { name, error } => write!(f, "opening {name}: {error}"),
            HostingError::NoInternalCreation { name } => {
                write!(f, "{name}: no mInternalCreation at [2, 254]")
            }
            HostingError::DuplicateName { name } => write!(f, "{name} is hosted twice"),
            HostingError::SameFile { name, hosted_as } => {
                write!(f, "{name} is the file already hosted as {hosted_as}")
            }
            HostingError::Insecure { name, why } => write!(
                f,
                "{name} is not password protected: {why} (hosting it needs --allow-insecure-files)"
            ),
        }
    }
}

impl std::error::Error for HostingError {}

/// A close of a hosted file under way (`ServerHandle::close_file`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileClose {
    /// The administrator's text for the file's clients.
    pub text: String,
    /// When the close began.
    pub started: Instant,
    /// How long the file's clients have to close it.
    pub grace: Duration,
}

/// A hosted file's place on the server: its id, name and path stay while
/// it is closed and opened again; its state and, while it is served, its
/// contents change.
#[derive(Debug)]
pub struct Slot {
    /// The server's id for the file ([`Hosted::id`]).
    pub id: u32,
    /// The name clients open it by.
    pub name: String,
    /// Where it lives on disk.
    pub path: PathBuf,
    /// Its folder as it was when it was first hosted: a reopen reads the
    /// file only there ([`FileAt`]).
    at: FileAt,
    state: Mutex<SlotState>,
}

#[derive(Debug)]
struct SlotState {
    status: FileStatus,
    /// The file, while it is served: open, paused or closing.
    file: Option<Arc<Hosted>>,
    /// The last saved identity while closed or opening, for backup protection.
    closed_file: Option<FileId>,
    /// The close under way, while the file is closing.
    closing: Option<FileClose>,
    /// Why the file is closed: its last open failed.
    error: Option<String>,
}

impl Slot {
    fn serving(hosted: Arc<Hosted>) -> Self {
        Slot {
            id: hosted.id,
            name: hosted.name.clone(),
            path: hosted.path.clone(),
            at: hosted.at.clone(),
            state: Mutex::new(SlotState {
                status: FileStatus::Open,
                file: Some(hosted),
                closed_file: None,
                closing: None,
                error: None,
            }),
        }
    }

    /// The state, also after a thread panicked holding it (each change
    /// leaves it whole).
    fn state(&self) -> MutexGuard<'_, SlotState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What state the file is in.
    pub fn status(&self) -> FileStatus {
        self.state().status
    }

    /// The file, while it is served (open, paused or closing).
    pub fn served(&self) -> Option<Arc<Hosted>> {
        self.state().file.clone()
    }

    /// Where the slot saves and reopens its file, also while it is closed.
    pub(crate) fn file_at(&self) -> &FileAt {
        &self.at
    }

    /// The file on disk this slot's engine last read or saved for it
    /// ([`FileEngine::file_on_disk`]), also after the file is closed.
    pub(crate) fn saved_file(&self, engine: &FileEngine) -> Option<FileId> {
        let (served, closed) = {
            let state = self.state();
            (state.file.clone(), state.closed_file)
        };
        match served {
            Some(file) => engine.file_on_disk(&file),
            None => closed,
        }
    }

    /// Why the file is closed, when its last open failed.
    pub fn error(&self) -> Option<String> {
        self.state().error.clone()
    }

    /// The close under way, while the file is closing.
    pub(crate) fn closing(&self) -> Option<FileClose> {
        self.state().closing.clone()
    }

    /// Move from `from` to `to`, or answer the status that forbids it.
    pub(crate) fn change(&self, from: FileStatus, to: FileStatus) -> Result<(), FileStatus> {
        let mut state = self.state();
        if state.status != from {
            return Err(state.status);
        }
        state.status = to;
        Ok(())
    }

    /// Begin closing an open file.
    pub(crate) fn start_close(&self, close: FileClose) -> Result<(), FileStatus> {
        let mut state = self.state();
        if state.status != FileStatus::Open {
            return Err(state.status);
        }
        state.status = FileStatus::Closing;
        state.closing = Some(close);
        Ok(())
    }

    /// Let a new user have the file (a client's session, a server script,
    /// a schedule's run), if it is open or paused: `admit` runs with the
    /// file under the slot's lock, so a close cannot finish between the
    /// check and the user's taking it ([`Slot::finish_close_if_drained`]).
    /// Otherwise the status that refuses it.
    pub(crate) fn admit<T>(&self, admit: impl FnOnce(&Arc<Hosted>) -> T) -> Result<T, FileStatus> {
        let state = self.state();
        match (&state.file, state.status) {
            (Some(file), FileStatus::Open | FileStatus::Paused) => Ok(admit(file)),
            (_, status) => Err(status),
        }
    }

    /// Finish closing the file once nothing but this slot holds it: no
    /// session has it open, and no server script, schedule run or request
    /// uses its store. Checked under the slot's lock, from which alone the
    /// file is handed out ([`Slot::admit`]): once it is closed no one holds
    /// the old store, so it can never save over the file opened again.
    /// Returns whether it closed.
    pub(crate) fn finish_close_if_drained(&self, engine: &FileEngine) -> bool {
        let mut state = self.state();
        let drained = state
            .file
            .as_ref()
            .is_none_or(|file| Arc::strong_count(file) == 1);
        if state.status != FileStatus::Closing || !drained {
            return false;
        }
        state.status = FileStatus::Closed;
        state.closed_file = state
            .file
            .take()
            .and_then(|file| engine.file_on_disk(&file));
        state.closing = None;
        true
    }

    /// Give up a close that was begun: the file is open again.
    pub(crate) fn cancel_close(&self) {
        let mut state = self.state();
        if state.status == FileStatus::Closing {
            state.status = FileStatus::Open;
            state.closing = None;
        }
    }
}

/// The set of hosted files.
#[derive(Debug, Default)]
pub struct Hosting {
    slots: Vec<Slot>,
    /// Every file the server was given, in id order, those that failed to
    /// open at startup (and have no slot) included: what backups must never
    /// delete ([`Hosting::configured`]).
    configured: Vec<HostedFile>,
    /// Host files FileMaker Server calls insecure
    /// ([`crate::ServerConfig::insecure_files`]), also when one is opened
    /// again.
    insecure_files: bool,
    /// Held while a file is opened again, so that two reopens cannot both
    /// find their file unhosted and serve one file twice
    /// ([`HostingError::SameFile`]).
    reopening: Mutex<()>,
}

impl Hosting {
    /// Open every file, recording each in `events`, its deferred field
    /// work finished and saved with `engine`. Ids are assigned in
    /// order, starting at 1. A file that cannot be opened is left out (it
    /// gets no [`Slot`]), its id unused, and raises an alert, as FileMaker
    /// Server goes on with the files it can open; when none opens, the
    /// first file's error is returned (OpenMaker's choice: a server with
    /// nothing to host is a mistake to say at once). An insecure file
    /// ([`Insecure`]) is one that cannot be opened unless `insecure_files`.
    pub(crate) fn load(
        files: &[HostedFile],
        insecure_files: bool,
        events: &EventLog,
        engine: &FileEngine,
    ) -> Result<Self, HostingError> {
        let mut hosting = Hosting {
            slots: Vec::new(),
            configured: files.to_vec(),
            insecure_files,
            reopening: Mutex::new(()),
        };
        let mut first_error = None;
        let mut names = std::collections::HashSet::new();
        for (index, hosted) in files.iter().enumerate() {
            if !names.insert(hosted.name.to_lowercase()) {
                return Err(HostingError::DuplicateName {
                    name: hosted.name.clone(),
                });
            }
            events.record(Event::file_opening(&hosted.name));
            let id = u32::try_from(index + 1).expect("fewer than 2^32 files");
            let loaded = FileAt::open(&hosted.path)
                .map_err(|e| HostingError::Open {
                    name: hosted.name.clone(),
                    error: om_hbam::Error::from(e),
                })
                .and_then(|at| {
                    let taken = |file: FileId| hosted_as(&hosting.slots, file, id, engine);
                    load_hosted(id, hosted, &at, insecure_files, events, &taken, engine)
                });
            match loaded {
                Ok(loaded) => {
                    let read_only = engine.read_only(&loaded).is_some();
                    events.record(Event::file_opened(&loaded.name, read_only));
                    hosting.slots.push(Slot::serving(loaded));
                }
                Err(e) => {
                    error!("{e}: not hosted");
                    events.alert(Event::file_open_failed(&hosted.name, &e));
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if hosting.slots.is_empty() => Err(e),
            _ => Ok(hosting),
        }
    }

    /// Load slot `id`'s file from disk again, with the checks it had at
    /// startup, and serve it: the administrator may have replaced it while
    /// it was closed. Only a closed file is opened; while it loads it is
    /// opening. A file that fails to load stays closed with the reason. As
    /// at startup, the file records its failed saves in `events`, and its
    /// deferred field work is finished and saved with `engine`.
    pub(crate) fn reopen(
        &self,
        id: u32,
        events: &EventLog,
        engine: &FileEngine,
    ) -> Result<(), ReopenError> {
        let slot = self.slot(id).ok_or(ReopenError::UnknownFile)?;
        slot.change(FileStatus::Closed, FileStatus::Opening)
            .map_err(ReopenError::NotAllowed)?;
        // One reopen at a time, until the file is served: another one
        // looking for a file hosted twice sees this one (#468).
        let _reopening = self
            .reopening
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Only from the folder it was first hosted from: a folder of the
        // path swapped for a link, or moved, since then fails the reopen
        // (#468).
        let loaded = load_hosted(
            id,
            &HostedFile {
                name: slot.name.clone(),
                path: slot.path.clone(),
            },
            &slot.at,
            self.insecure_files,
            events,
            &|file| hosted_as(&self.slots, file, id, engine),
            engine,
        );
        let mut state = slot.state();
        match loaded {
            Ok(hosted) => {
                state.status = FileStatus::Open;
                state.file = Some(hosted);
                state.closed_file = None;
                state.error = None;
                Ok(())
            }
            Err(e) => {
                let reason = e.to_string();
                state.status = FileStatus::Closed;
                state.error = Some(reason.clone());
                Err(ReopenError::Load(reason))
            }
        }
    }

    /// Host one file a local client opened, already read from its locked
    /// descriptor: `lock` (none where the file cannot be locked, and then
    /// `read_only`) stays with the file's store, and a `read_only` file is
    /// never written. Its deferred work is finished and saved when an
    /// account it admits first logs in ([`Hosted::recover`]).
    pub(crate) fn local(
        hosted: &HostedFile,
        file: &HbamFile,
        lock: Option<FileLock>,
        read_only: bool,
    ) -> Result<Self, HostingError> {
        let configured = vec![hosted.clone()];
        // The path the desktop opened and locked, as it named it.
        let at = FileAt::open_as_named(&hosted.path).map_err(|e| HostingError::Open {
            name: hosted.name.clone(),
            error: om_hbam::Error::from(e),
        })?;
        let hosted = Hosted::load(
            1,
            hosted,
            at,
            file,
            Some((lock, read_only)),
            false,
            &EventLog::default(),
        )?;
        Ok(Hosting {
            slots: vec![Slot::serving(Arc::new(hosted))],
            configured,
            insecure_files: false,
            reopening: Mutex::new(()),
        })
    }

    /// Every hosted file's slot, served or not, in id order.
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Every file the server was given, in id order: each slot's file, and
    /// each that failed to open at startup.
    pub(crate) fn configured(&self) -> &[HostedFile] {
        &self.configured
    }

    /// The slot of the file with server id `id`.
    pub fn slot(&self, id: u32) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id == id)
    }

    /// The slot of the file clients open as `name` (case-insensitive).
    pub fn slot_named(&self, name: &str) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|slot| slot.name.to_lowercase() == name.to_lowercase())
    }

    /// The file clients open as `name` (case-insensitive), for a new user
    /// of it ([`Slot::admit`]): only while it is open or paused.
    pub fn by_name(&self, name: &str) -> Option<Arc<Hosted>> {
        self.slot_named(name)?.admit(Arc::clone).ok()
    }

    /// The file with server id `id`, for a new user of it: only while it is
    /// open or paused.
    pub fn by_id(&self, id: u32) -> Option<Arc<Hosted>> {
        self.slot(id)?.admit(Arc::clone).ok()
    }

    /// Every served file (open, paused or closing), in id order: for work
    /// on what is already held, such as releasing a session's locks.
    pub fn files(&self) -> Vec<Arc<Hosted>> {
        self.slots.iter().filter_map(Slot::served).collect()
    }
}

/// Load the file `hosted` describes with server id `id`, finishing and
/// saving first, with `engine`, the deferred field work it records as
/// pending: a server's files are recovered as they are loaded. An insecure
/// file is refused, before anything is written, unless `insecure_files`. A
/// file with anything at its save's copy name (`<file>.omsave`, an earlier
/// save's leftover or not) is hosted as any file is, and an alert names
/// the entry: each of its saves fails, the change taken back, until the
/// administrator removes it; nothing removes it here
/// (docs/server-storage.md 5). A failed save, that of the deferred work
/// included, is recorded in `events`.
fn load_hosted(
    id: u32,
    hosted: &HostedFile,
    at: &FileAt,
    insecure_files: bool,
    events: &EventLog,
    taken: &dyn Fn(FileId) -> Option<String>,
    engine: &FileEngine,
) -> Result<Arc<Hosted>, HostingError> {
    let (bytes, on_disk) = read_regular_file(at).map_err(|e| HostingError::Open {
        name: hosted.name.clone(),
        error: om_hbam::Error::from(e),
    })?;
    // Before anything is saved, recovery included.
    if let Some(hosted_as) = on_disk.and_then(taken) {
        return Err(HostingError::SameFile {
            name: hosted.name.clone(),
            hosted_as,
        });
    }
    let file = HbamFile::from_bytes(bytes).map_err(|error| HostingError::Open {
        name: hosted.name.clone(),
        error,
    })?;
    let loaded = Arc::new(Hosted::load(
        id,
        hosted,
        at.clone(),
        &file,
        None,
        insecure_files,
        events,
    )?);
    // Before any save: saves write only where the path still leads to
    // this file (#468).
    engine.loaded_from(&loaded, on_disk);
    drop(file);
    // Looked at in the file's folder, without following a link; one that
    // cannot be looked at counts.
    let copy_in_the_way = !matches!(
        at.folder()
            .and_then(|folder| crate::store::copy_beside(&folder, at.name())),
        Ok(false)
    );
    if copy_in_the_way {
        let copy = crate::store::save_copy_path(at.path());
        error!(
            "{:?}: something is at {}, where its saves write their new copy (an earlier \
             save's leftover?); its changes are not saved until that is removed",
            hosted.name,
            copy.display()
        );
        events.alert(Event::save_copy_in_the_way(
            &hosted.name,
            &copy.display().to_string(),
        ));
    }
    loaded.recover(engine);
    Ok(loaded)
}

/// The bytes of the regular file `at`, from the folder it was first hosted
/// from, and its device and inode, read through the one handle whose type
/// was checked: opened without
/// blocking (a FIFO named as a database would otherwise hold startup before
/// the server listens), and refused unless it is a regular file, so that
/// one swapped in after a check cannot be read instead.
fn read_regular_file(at: &FileAt) -> std::io::Result<(Vec<u8>, Option<FileId>)> {
    use std::io::Read as _;
    let mut file = at.folder()?.open_regular(at.name())?;
    let on_disk = crate::anchored::file_identity(&file)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok((data, on_disk))
}

/// The name of the slot other than `id` among `slots` that serves the file
/// with device and inode `file` ([`FileEngine::file_on_disk`]).
fn hosted_as(slots: &[Slot], file: FileId, id: u32, engine: &FileEngine) -> Option<String> {
    slots
        .iter()
        .filter(|slot| slot.id != id)
        .find(|slot| {
            slot.served()
                .is_some_and(|served| engine.file_on_disk(&served) == Some(file))
        })
        .map(|slot| slot.name.clone())
}

/// Why [`Hosting::reopen`] did not open a file.
#[derive(Debug)]
pub(crate) enum ReopenError {
    UnknownFile,
    /// The file is not closed.
    NotAllowed(FileStatus),
    /// It failed to load, for this reason.
    Load(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::fixture;

    /// A status whose fraction of a second leaves no room for a flag is not
    /// written cut back to six digits, which would lose its flags (#83).
    #[test]
    fn a_status_that_cannot_hold_its_flags_is_not_written() {
        let status = AccountStatus {
            date: jiff::civil::date(2026, 11, 19),
            seconds: 1,
            microseconds: 500_000,
        };
        assert!(encoded(status).is_ok());
        assert!(encoded(status.enabled(false)).is_err());
    }

    /// A FIFO named as a database is refused without waiting for a writer,
    /// and the other files are hosted: a blocking open would hold startup
    /// before the server listens (#430's review).
    #[cfg(unix)]
    #[test]
    fn a_fifo_among_the_files_does_not_hold_startup() {
        let file = fixture::TempFile::new("hosting-fifo", &[], true);
        let fifo = file.0.with_extension("pipe.fmp12");
        // Keep the fixture in-process so it cannot inherit another test's lock.
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let files = [
            HostedFile {
                name: "Pipe.fmp12".into(),
                path: fifo.clone(),
            },
            HostedFile {
                name: fixture::FILE.into(),
                path: file.0.clone(),
            },
        ];
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let hosting =
                Hosting::load(&files, false, &EventLog::default(), &FileEngine::default());
            let _ = done.send(hosting.map(|h| h.by_name(fixture::FILE).is_some()));
        });
        let loaded = wait.recv_timeout(std::time::Duration::from_secs(30));
        let _ = std::fs::remove_file(&fifo);
        assert!(matches!(loaded, Ok(Ok(true))), "the good file is hosted");
    }

    /// The review's P0 on recovery: deferred work a local file holds is
    /// finished at the first admitted login, and a login that comes while
    /// it runs is answered only once it ended, as is the first: whichever
    /// checks first does the work, under the lock the other waits on.
    #[test]
    fn a_login_while_recovery_runs_waits_for_it() {
        use om_hbam::slot::uint_segment;
        use om_hbam::OwnedNode;
        let mut tree = fixture::tree(&[], true);
        fixture::add_tasks(&mut tree, 1..=3);
        let deferred = [uint_segment(fixture::TABLE), vec![3], vec![1], vec![0xfd]];
        let path: Vec<&[u8]> = deferred.iter().map(Vec::as_slice).collect();
        let work = OwnedNode::leaf(uint_segment(fixture::COUNT), 0x40u32.to_be_bytes().to_vec());
        tree.put(&path, OwnedNode::list(vec![0xfd], vec![work]));
        let disk = fixture::TempFile::from_tree("hosting-recovery-race", &tree);
        let hbam = HbamFile::open(&disk.0).unwrap();
        let hosted = HostedFile {
            name: fixture::FILE.into(),
            path: disk.0.clone(),
        };
        let hosting = Hosting::local(&hosted, &hbam, None, false).unwrap();
        let file = hosting.by_name(fixture::FILE).unwrap();
        assert!(
            file.read_store().node(&path).is_some(),
            "not before a login"
        );
        let engine = FileEngine::default();
        let (done, answered) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            // The work cannot run until the store is let go of.
            let held = file.store_for_writing();
            for login in 0..2 {
                let (file, engine, done) = (&file, &engine, done.clone());
                scope.spawn(move || {
                    file.recover(engine);
                    done.send(login).unwrap();
                });
            }
            assert!(
                answered.recv_timeout(Duration::from_millis(300)).is_err(),
                "no login is answered before the work ended"
            );
            drop(held);
        });
        assert_eq!(answered.try_iter().count(), 2);
        assert!(file.read_store().node(&path).is_none(), "the work is done");
    }

    /// A password changed over the wire (#732's review: FileMaker Pro's
    /// Manage Security uploads the account's record with op13,
    /// `engine::schema::upload_to_catalog`) counts from the next login on, and the old
    /// one no more.
    #[test]
    fn logins_follow_the_security_catalog_as_the_store_holds_it() {
        let file = fixture::TempFile::new("hosting-accounts", &[], true);
        let hosted_file = HostedFile {
            name: fixture::FILE.into(),
            path: file.0.clone(),
        };
        let hosting = Hosting::load(
            &[hosted_file],
            false,
            &EventLog::default(),
            &FileEngine::default(),
        )
        .unwrap();
        let hosted = hosting.by_name(fixture::FILE).unwrap();
        let clerk = |password| hosted.verify_password(fixture::CLERK, password).is_some();
        assert!(clerk(fixture::CLERK_PASSWORD));
        let path: [&[u8]; 4] = [&[23], &[1], &[5], &[3]];
        let mut record = hosted.read_store().node(&path).unwrap().clone();
        record.put(
            &[ACCOUNT_PASSWORD],
            om_hbam::OwnedNode::leaf(Vec::new(), StoredPassword::encode("changed", [9; 10])),
        );
        let path: Vec<Vec<u8>> = path.iter().map(|segment| segment.to_vec()).collect();
        assert!(hosted.store_for_writing().put_subtree(&path, record));
        assert!(!clerk(fixture::CLERK_PASSWORD));
        assert!(clerk("changed"));
    }
}
