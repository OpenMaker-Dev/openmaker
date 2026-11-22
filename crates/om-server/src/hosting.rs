//! The files a server hosts: loaded at startup, and closed and opened again
//! by the administrator (`ServerHandle::close_file`, `open_file`), each in a
//! [`Slot`] that keeps its id, name and path meanwhile.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use om_crypto::account::{StoredPassword, StoredPasswordError};
use om_data::host::security::{set_access, BuiltInSet, SetAccess};
use om_hbam::HbamFile;

use crate::accounts::{AccountKind, AccountStatus};
use crate::admin::FileStatus;
use crate::config::{HostedFile, Privileges};
use crate::eventlog::{Event, EventLog, SaveOutcome};
use crate::filelock::FileLock;
use crate::locks::LockTable;
use crate::store::{Journal, Store};
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
    /// No credential or security-catalog bytes leave the host.
    pub fn authenticated_info(&self) -> om_data::host::AccountInfo {
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
    /// Where the file lives on disk.
    pub path: PathBuf,
    /// The file's contents: every download, query and write goes here.
    /// Taken through [`Hosted::read_store`] and [`Hosted::write_store`],
    /// which keeps `accounts` following it.
    store: RwLock<Store>,
    /// The locks clients hold on it.
    pub locks: Mutex<LockTable>,
    /// The connections to tell about changes.
    pub registrations: Mutex<Vec<Registration>>,
    /// Recently rendered container thumbnails (op96).
    pub thumbnails: Mutex<thumbnail::Cache>,
    /// `mInternalCreation` (`[2, 254]`), the second half of the Login nonce.
    pub internal_creation: u32,
    /// The accounts in the file's security catalog as the store holds it
    /// at a schema generation ([`Hosted::accounts`]).
    accounts: RwLock<Accounts>,
    /// Deferred field work [`Store::recover`] finished in memory and not
    /// saved yet: a local file keeps it provisional until an eligible
    /// account logs in ([`Hosted::commit_recovery`]); closing before that
    /// leaves the file as it was.
    pending_recovery: std::sync::atomic::AtomicBool,
    /// Where a failed save is recorded and raised as an alert.
    events: EventLog,
}

/// A file's accounts, decoded from its store at the store's schema
/// generation `generation` ([`Store::schema_generation`]): a change of the
/// security catalog (an op13 upload, a catalog member replaced, one put
/// back) moves the generation, and [`StoreWriteGuard`] decodes them again.
struct Accounts {
    generation: u64,
    list: Arc<[Arc<FileAccount>]>,
}

impl Accounts {
    fn of(store: &Store) -> Self {
        Accounts {
            generation: store.schema_generation(),
            list: file_accounts(store).into_iter().map(Arc::new).collect(),
        }
    }
}

/// The store of a [`Hosted`] file held for writing ([`Hosted::write_store`]).
/// When it is let go, the file's accounts are decoded again if the write
/// changed the schema, so a password or account changed over the wire
/// counts from the next login on.
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
        // Under the store's write lock still, so no login sees the store
        // and the accounts disagree.
        let mut accounts = self
            .hosted
            .accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if accounts.generation != self.store.schema_generation() {
            *accounts = Accounts::of(&self.store);
        }
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
    /// id `id`. Deferred field work a stop interrupted is finished (and left
    /// unsaved) before the file is served (schema-path.md "Implementation
    /// notes (recalculation)"). A local file comes with its lock and whether
    /// it is read-only. A file FileMaker Server calls insecure is refused
    /// unless `insecure_files` (a local file never is). A failed save is
    /// recorded in `events`.
    fn load(
        id: u32,
        hosted: &HostedFile,
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
                store.set_read_only();
            }
        }
        let mut environment =
            || crate::recalc::record_environment("", crate::recalc::random_uuid());
        let recovered = store.recover(&mut environment);
        let accounts = RwLock::new(Accounts::of(&store));
        Ok(Hosted {
            id,
            name: hosted.name.clone(),
            path: hosted.path.clone(),
            store: RwLock::new(store),
            locks: Mutex::new(LockTable::default()),
            registrations: Mutex::new(Vec::new()),
            thumbnails: Mutex::new(thumbnail::Cache::default()),
            internal_creation,
            accounts,
            pending_recovery: std::sync::atomic::AtomicBool::new(recovered),
            events: events.clone(),
        })
    }

    /// Save what [`Store::recover`] finished when the file was loaded, if
    /// it is still unsaved: an eligible account logged in to a local file.
    pub fn commit_recovery(&self) {
        use std::sync::atomic::Ordering;
        if self.pending_recovery.swap(false, Ordering::AcqRel) {
            info!(
                "{:?}: saving the deferred field work finished on opening",
                self.name
            );
            self.save();
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
    /// otherwise: for work that must not wait on a request.
    pub fn try_write_store(&self) -> Option<StoreWriteGuard<'_>> {
        let store = match self.store.try_write() {
            Ok(store) => store,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(StoreWriteGuard {
            hosted: self,
            store,
        })
    }

    /// The file's contents for writing ([`Hosted::read_store`]).
    pub fn write_store(&self) -> StoreWriteGuard<'_> {
        let store = self.store.write().unwrap_or_else(|poisoned| {
            warn!("{:?}: a request panicked holding the store", self.name);
            poisoned.into_inner()
        });
        StoreWriteGuard {
            hosted: self,
            store,
        }
    }

    /// Whether a request panicked holding the store.
    #[cfg(test)]
    pub(crate) fn store_is_poisoned(&self) -> bool {
        self.store.is_poisoned()
    }

    /// The accounts in the file's security catalog, as its store holds
    /// it now.
    pub fn accounts(&self) -> Arc<[Arc<FileAccount>]> {
        self.accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .list
            .clone()
    }

    /// The file's lock table, also after a request panicked holding it.
    pub fn lock_table(&self) -> MutexGuard<'_, LockTable> {
        self.locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the file is hosted read-only, including after refused deferred work.
    pub fn is_read_only(&self) -> bool {
        self.read_store().is_read_only()
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

    /// Write the file back to disk if its contents changed. A failure of
    /// the disk is logged and the changes stay in memory for the next
    /// attempt; this is for changes that may wait (reserved record ids and
    /// serial numbers, indexes built on demand, schema and container
    /// uploads). Every such change is checked against the tree before it is
    /// made, so it cannot leave what the file cannot hold. If a field index
    /// the file cannot hold is in the tree anyway, it is taken out
    /// ([`Store::drop_unwritable_indexes`]; the next find that reads it
    /// builds it again), logged, and the save tried again. Anything else the
    /// file cannot hold is kept as it is and logged, and saves fail rather
    /// than delete data. Record commits are saved with
    /// [`Store::save_commit`], which takes them back when the file cannot be
    /// written.
    pub fn save(&self) {
        let mut store = self.write_store();
        if store.is_read_only() {
            // Indexes built on demand and the like stay in memory.
            return;
        }
        let _ = self.save_held(&mut store);
    }

    /// [`Self::save`] of `store`, this file's, which the caller holds:
    /// whether the disk now holds every change, or the last error. A change
    /// that must be on disk before it is answered (an issued record id,
    /// #927) is taken back when this fails.
    pub fn save_held(&self, store: &mut Store) -> std::io::Result<()> {
        if store.is_read_only() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ReadOnlyFilesystem,
                "the file is hosted read-only",
            ));
        }
        let started = Instant::now();
        let error = match store.save(&self.path) {
            Ok(true) => {
                debug!("{:?}: saved in {:?}", self.name, started.elapsed());
                return Ok(());
            }
            Ok(false) => return Ok(()),
            Err(e) => e,
        };
        let (dropped, kept) = store.drop_unwritable_indexes();
        if !kept.is_empty() {
            error!(
                "{:?}: saving to {} failed ({error}): the file cannot hold what the tree holds \
                 here, which is kept as it is and not saved: {}",
                self.name,
                self.path.display(),
                crate::store::describe(&kept)
            );
            self.events.alert(Event::save_failed(
                &self.name,
                &error,
                SaveOutcome::Unwritable,
            ));
        }
        if dropped.is_empty() {
            if kept.is_empty() {
                error!(
                    "{:?}: saving to {} failed, unsaved changes are kept in memory: {error}",
                    self.name,
                    self.path.display()
                );
                self.events
                    .alert(Event::save_failed(&self.name, &error, SaveOutcome::Kept));
            }
            return Err(error);
        }
        error!(
            "{:?}: saving to {} failed ({error}): took out field indexes the file cannot hold, \
             which the next find that reads them builds again: {}",
            self.name,
            self.path.display(),
            crate::store::describe(&dropped)
        );
        if !kept.is_empty() {
            return Err(error);
        }
        if let Err(e) = store.save(&self.path) {
            error!(
                "{:?}: saving to {} failed, unsaved changes are kept in memory: {e}",
                self.name,
                self.path.display()
            );
            self.events
                .alert(Event::save_failed(&self.name, &e, SaveOutcome::Kept));
            return Err(e);
        }
        Ok(())
    }

    /// Set the password of account `name` from `old` to `new`, as
    /// FileMaker's `SetPassword` sets the signed-in account's own
    /// (local-files.md 2.4): the stored password `06` with a new random
    /// salt, the status `0a` stamped `now` to the second with its other
    /// flags kept and no change due, and the checksum `04` again; the
    /// member's other keys are kept. The new one must have at most 100
    /// characters; an account that may not manage itself (not full access,
    /// nor a set managing accounts outside [Full Access]) is checked as
    /// `SetPassword` checks it: its set must let it change its own
    /// password, and the new one must have at least the set's minimum
    /// length (`0d`) and differ from the old one. A `manager` caller (a
    /// script running with full access) skips those checks as one that
    /// may manage the account does. One commit, saved before this returns;
    /// a failed save takes it back. With `stamp` the account and the
    /// catalog's index get FileMaker's modification stamps
    /// ([`Self::rewrite_account`]).
    pub fn change_password(
        &self,
        account: AccountRef<'_>,
        old: &str,
        new: &str,
        manager: bool,
        stamp: Option<&ModStamp<'_>>,
        now: jiff::civil::DateTime,
    ) -> Result<(), PasswordChangeError> {
        use om_data::host::privilege_set::Options;
        if new.chars().count() > 100 {
            return Err(PasswordChangeError::TooLong);
        }
        let mut store = self.write_store();
        let Some(record) = find_account(&store, account) else {
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
        let current = AccountStatus::decode(member(&view, ACCOUNT_STATUS))
            .ok_or(PasswordChangeError::Refused)?;
        if !current.is_active() {
            return Err(PasswordChangeError::Inactive);
        }
        let stored = view.child(ACCOUNT_PASSWORD).and_then(|n| n.assembled());
        let Some(stored) = stored
            .as_deref()
            .and_then(|bytes| StoredPassword::parse(bytes).ok())
            .filter(|password| password.verify(old))
        else {
            return Err(PasswordChangeError::Refused);
        };
        let set_ref = member_owned(&view, ACCOUNT_PRIVILEGES);
        let set = set_ref
            .as_deref()
            .and_then(|r| r.split_first())
            .filter(|(length, key)| usize::from(**length) == key.len())
            .and_then(|(_, key)| {
                store.node(&[
                    PRIVILEGE_SETS_PATH[0],
                    PRIVILEGE_SETS_PATH[1],
                    PRIVILEGE_SETS_PATH[2],
                    key,
                ])
            })
            .map(crate::store::view);
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
            if new.chars().count() < usize::from(minimum) {
                return Err(PasswordChangeError::TooShort(minimum));
            }
            // Unchanged when it hashes as the current one: a text that only
            // decomposes alike (`é`, `e` U+0301) is the same password.
            if stored.verify(new) {
                return Err(PasswordChangeError::Unchanged);
            }
        }
        let status = current.password_set(now.date(), seconds(now)).encode();
        let salt = random_salt().map_err(PasswordChangeError::Unwritten)?;
        let password = StoredPassword::encode(new, salt);
        self.rewrite_account(&mut store, &record, Some(password), status, stamp)
            .map_err(PasswordChangeError::Unwritten)
    }

    /// Reset Account Password (136) for the account named `name`, as
    /// FileMaker's `DoResetAccount` writes it (local-files.md 2.4): the
    /// stored password `06` with a new random salt, the status `0a`
    /// stamped `now` to the second with its other flags kept and a change
    /// due at the next login when `expire`, and the checksum `04`
    /// ([`Self::rewrite_account`], with `stamp`). Only a FileMaker-file
    /// account has a password to reset (206, as `SetPassword` refuses
    /// another kind); more than 100 characters is 11. The caller has
    /// checked that the session may manage accounts.
    pub fn reset_account_password(
        &self,
        name: &str,
        new: &str,
        expire: bool,
        stamp: Option<&ModStamp<'_>>,
        now: jiff::civil::DateTime,
    ) -> Result<(), AccountEditError> {
        if new.chars().count() > 100 {
            return Err(AccountEditError::TooLong);
        }
        let mut store = self.write_store();
        let record =
            find_account(&store, AccountRef::Name(name)).ok_or(AccountEditError::Missing)?;
        let view = crate::store::view(&record);
        if AccountKind::decode(member(&view, ACCOUNT_KIND)) != Some(AccountKind::FileMakerFile) {
            return Err(AccountEditError::NotFileAccount);
        }
        let current = AccountStatus::decode(member(&view, ACCOUNT_STATUS))
            .ok_or_else(|| AccountEditError::Unwritten("its status does not decode".into()))?;
        let status = encoded(current.password_reset(now.date(), seconds(now), expire))
            .map_err(AccountEditError::Unwritten)?;
        let salt = random_salt().map_err(AccountEditError::Unwritten)?;
        let password = StoredPassword::encode(new, salt);
        self.rewrite_account(&mut store, &record, Some(password), status, stamp)
            .map_err(AccountEditError::Unwritten)
    }

    /// Enable Account (137): the account named `name` made active or
    /// inactive, as FileMaker's `SetEnabled` sets it (local-files.md 2.4):
    /// only the status's inactive bit changes (an account without a status
    /// gets 1/1/1024's), and the checksum `04` with it
    /// ([`Self::rewrite_account`], with `stamp`). Whether it changed: an
    /// account already in that state is left as it is. Refused (3) for an
    /// account an OAuth identity provider or Claris ID signs in, and for
    /// making a [Full Access] account inactive, which Claris documents the
    /// step does not do. The caller has checked that the session may
    /// manage accounts.
    pub fn enable_account(
        &self,
        name: &str,
        active: bool,
        stamp: Option<&ModStamp<'_>>,
    ) -> Result<bool, AccountEditError> {
        let mut store = self.write_store();
        let record =
            find_account(&store, AccountRef::Name(name)).ok_or(AccountEditError::Missing)?;
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
        self.rewrite_account(&mut store, &record, None, status, stamp)
            .map(|()| true)
            .map_err(AccountEditError::Unwritten)
    }

    /// Delete Account (135): the account named `name` removed, as
    /// FileMaker's `DoDeleteAccount` removes it (local-files.md 2.4 and 3):
    /// its member, and from the catalog's index its name list entry, its
    /// place in the display order (written again from 1) and its `07`
    /// entry, and one from the count; the last key issued stays. One
    /// commit, saved before this returns, with the index stamped when
    /// `stamp` is given. As Claris documents, a [Full Access] account is
    /// not deleted, nor an OAuth or Claris ID one; [Guest] is not either
    /// (all 3, inferred). An index that does not hold the accounts as
    /// [`index_without`] reads it is not rewritten. The caller has checked
    /// that the session may manage accounts.
    pub fn delete_account(
        &self,
        name: &str,
        stamp: Option<&ModStamp<'_>>,
    ) -> Result<(), AccountEditError> {
        let mut store = self.write_store();
        let record =
            find_account(&store, AccountRef::Name(name)).ok_or(AccountEditError::Missing)?;
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
        let mut accounts = store
            .node(&ACCOUNTS_PATH)
            .cloned()
            .ok_or_else(|| AccountEditError::Unwritten("no account catalog".into()))?;
        let index = store.node(&ACCOUNT_INDEX_PATH).cloned().ok_or_else(|| {
            AccountEditError::Unwritten("the account catalog has no index".into())
        })?;
        let members: Vec<Vec<u8>> = accounts
            .children
            .iter()
            .map(|a| a.segment.clone())
            .collect();
        let mut index =
            index_without(index, &record.segment, &members).map_err(AccountEditError::Unwritten)?;
        if let Some(stamp) = stamp {
            index = crate::authoring::stamped(index, stamp.user, stamp.account, stamp.time);
        }
        accounts.children.retain(|a| a.segment != record.segment);
        let path = |segments: &[&[u8]]| segments.iter().map(|s| s.to_vec()).collect();
        let mut journal = store.begin_commit();
        store.replace_members_in(
            &mut journal,
            vec![
                (path(&ACCOUNTS_PATH), accounts),
                (path(&ACCOUNT_INDEX_PATH), index),
            ],
        );
        self.save_commit(&mut store, journal)
            .map_err(|e| AccountEditError::Unwritten(e.to_string()))
    }

    /// Write account `record` again with `password` (its own when `None`)
    /// and `status`, and the checksum `04` they give, in one commit saved
    /// before this returns; its other members are kept. With `stamp`, the
    /// account and the account catalog's index `[23, 1, 1]` get the
    /// modification stamps FileMaker writes on every catalog commit
    /// (`IncrementModCount`, schema-path.md 8.1), so a client holding a
    /// copy of the catalog sees that it is stale.
    ///
    /// Refused, changing nothing, when the account stores a checksum other
    /// than the one its current members give: FileMaker Pro refuses a file
    /// whose account checksum is wrong (808), so an account this server
    /// does not read as FileMaker does is not rewritten. One without a
    /// checksum, which FileMaker always writes, gets one.
    fn rewrite_account(
        &self,
        store: &mut Store,
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
        let version = store
            .node(&[&[2], &[5]])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| <[u8; 2]>::try_from(v).ok())
            .map(u16::from_be_bytes);
        let access_uuid = store
            .node(&[&[23], &[4], &[1], &[0x10]])
            .and_then(|n| n.value.as_deref());
        let creation = store.node(&[&[2], &[254]]).and_then(|n| n.value.as_deref());
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
            let index = store
                .node(&ACCOUNT_INDEX_PATH)
                .cloned()
                .ok_or("the account catalog has no index")?;
            changed = crate::authoring::stamped(changed, stamp.user, stamp.account, stamp.time);
            members.push((
                ACCOUNT_INDEX_PATH.iter().map(|s| s.to_vec()).collect(),
                crate::authoring::stamped(index, stamp.user, stamp.account, stamp.time),
            ));
        }
        members.insert(0, (path, changed));
        let mut journal = store.begin_commit();
        store.replace_members_in(&mut journal, members);
        self.save_commit(store, journal).map_err(|e| e.to_string())
    }

    /// [`Store::save_commit`] for this file, logged.
    pub fn save_commit(&self, store: &mut Store, journal: Journal) -> std::io::Result<()> {
        let started = Instant::now();
        let saved = store.save_commit(&self.path, journal);
        match &saved {
            Ok(()) => debug!("{:?}: saved in {:?}", self.name, started.elapsed()),
            Err(e) => {
                error!(
                    "{:?}: saving to {} failed, the commit was taken back: {e}",
                    self.name,
                    self.path.display()
                );
                self.events
                    .alert(Event::save_failed(&self.name, e, SaveOutcome::TakenBack));
            }
        }
        saved
    }

    /// The file's account named `name` (case-insensitive, as FileMaker matches).
    pub fn account(&self, name: &str) -> Option<Arc<FileAccount>> {
        let name = name.to_lowercase();
        self.accounts()
            .iter()
            .find(|a| a.name.to_lowercase() == name)
            .cloned()
    }

    /// The file's account named `name` if `password` is the password the
    /// file stores for it. An account whose password this server cannot
    /// check is refused, as is one without a name ([Guest]'s is a
    /// reference to localized text, not text). A password is hashed once
    /// whatever the name, so that the time a login takes does not tell
    /// whether the name is an account. Whether the account may log in is
    /// [`FileAccount::eligibility`].
    pub fn verify_password(&self, name: &str, password: &str) -> Option<Arc<FileAccount>> {
        /// What a password is checked against when there is no stored one.
        static NONE: std::sync::LazyLock<StoredPassword> = std::sync::LazyLock::new(|| {
            StoredPassword::parse(&StoredPassword::encode("", [0; 10]))
                .expect("an encoded password reads back")
        });
        let account = self.account(name);
        let stored = match account.as_ref().map(|account| &account.password) {
            Some(Ok(stored)) => stored,
            Some(Err(reason)) => {
                info!(
                    "{:?}: account {name:?}: its password cannot be checked ({reason:?})",
                    self.name
                );
                &NONE
            }
            None => &NONE,
        };
        let matched = std::hint::black_box(stored.verify(password));
        account.filter(|account| matched && account.password.is_ok())
    }
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
}

/// Why Reset Account Password or Enable Account changed nothing.
#[derive(Debug)]
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
}

impl AccountEditError {
    /// FileMaker's error code; `None` for an account or file that could not
    /// be written.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Missing => Some(213),
            Self::NotFileAccount => Some(206),
            Self::TooLong => Some(11),
            Self::Unsupported | Self::FullAccess => Some(3),
            Self::Unwritten(_) => None,
        }
    }
}

impl PasswordChangeError {
    /// FileMaker's error code; `None` for a file that could not be written.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Refused => Some(212),
            Self::NotAllowed => Some(206),
            Self::TooShort(_) => Some(208),
            Self::Unchanged => Some(209),
            Self::TooLong => Some(11),
            Self::Inactive => Some(210),
            Self::Unwritten(_) => None,
        }
    }
}

/// An account named by its record key under `[23, 1, 5]`, which a rename
/// does not change, or by its name.
#[derive(Clone, Copy, Debug)]
pub enum AccountRef<'a> {
    Key(&'a [u8]),
    Name(&'a str),
}

/// Who changed a catalog, and when: the modification stamp FileMaker
/// writes (schema-path.md 8.1), `time` being the host time key of 3.1.
#[derive(Clone, Copy, Debug)]
pub struct ModStamp<'a> {
    pub user: &'a str,
    pub account: &'a str,
    pub time: &'a [u8],
}

/// The record of `account` as `store` holds it: by key, or by name as
/// FileMaker matches names, ignoring case. [Guest], whose name is not
/// text, is found only by its key.
fn find_account(store: &Store, account: AccountRef<'_>) -> Option<om_hbam::OwnedNode> {
    let accounts = store.node(&ACCOUNTS_PATH)?;
    match account {
        AccountRef::Key(key) => accounts.children.iter().find(|a| a.segment == key),
        AccountRef::Name(name) => {
            let wanted = name.to_lowercase();
            accounts.children.iter().find(|a| {
                crate::store::view(a)
                    .child(ACCOUNT_NAME)
                    .and_then(|n| n.text()?.ok())
                    .is_some_and(|text| text.to_lowercase() == wanted)
            })
        }
    }
    .cloned()
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

/// A new random salt for a stored password.
fn random_salt() -> Result<[u8; om_crypto::account::SALT_LEN], String> {
    let mut salt = [0u8; om_crypto::account::SALT_LEN];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut salt)
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

/// The accounts in `store`'s security catalog.
fn file_accounts(store: &Store) -> Vec<FileAccount> {
    let Some(node) = store.node(&ACCOUNTS_PATH).map(crate::store::view) else {
        return Vec::new();
    };
    let sets = store.node(&PRIVILEGE_SETS_PATH).map(crate::store::view);
    node.children
        .iter()
        // A Login reply names the account by its record key as a key path
        // with one-byte lengths (`[len][key]`, network-protocol.md 5.6.5):
        // an account whose key is too long for that is not admitted.
        .filter(|account| account.segment.0.len() < usize::from(u8::MAX))
        .filter_map(|account| {
            let name = account.child(ACCOUNT_NAME)?.text()?.ok()?;
            let password = match account.child(ACCOUNT_PASSWORD).and_then(|n| n.assembled()) {
                Some(bytes) => {
                    StoredPassword::parse(&bytes).map_err(PasswordUnavailable::Unsupported)
                }
                None => Err(PasswordUnavailable::Missing),
            };
            // The set the account names: its reference is a key string, a
            // length and then the set's record key.
            let reference = member(account, ACCOUNT_PRIVILEGES);
            let set = reference
                .and_then(|r| r.split_first())
                .filter(|(length, key)| usize::from(**length) == key.len())
                .and_then(|(_, key)| sets.as_ref()?.child(key));
            Some(FileAccount {
                name,
                key: account.segment.0.to_vec(),
                password,
                kind: AccountKind::decode(member(account, ACCOUNT_KIND)),
                status: AccountStatus::decode(member(account, ACCOUNT_STATUS)),
                access: set.and_then(|set| set_access(set).ok()),
                password_days: set.and_then(password_days),
            })
        })
        .collect()
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
    state: Mutex<SlotState>,
}

#[derive(Debug)]
struct SlotState {
    status: FileStatus,
    /// The file, while it is served: open, paused or closing.
    file: Option<Arc<Hosted>>,
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
            state: Mutex::new(SlotState {
                status: FileStatus::Open,
                file: Some(hosted),
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
    pub(crate) fn finish_close_if_drained(&self) -> bool {
        let mut state = self.state();
        let drained = state
            .file
            .as_ref()
            .is_none_or(|file| Arc::strong_count(file) == 1);
        if state.status != FileStatus::Closing || !drained {
            return false;
        }
        state.status = FileStatus::Closed;
        state.file = None;
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
}

impl Hosting {
    /// Open every file, recording each in `events`. Ids are assigned in
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
    ) -> Result<Self, HostingError> {
        let mut hosting = Hosting {
            slots: Vec::new(),
            configured: files.to_vec(),
            insecure_files,
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
            match load_hosted(id, hosted, insecure_files, events) {
                Ok(loaded) => {
                    events.record(Event::file_opened(&loaded.name, loaded.is_read_only()));
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
    /// at startup, the file records its failed saves in `events`.
    pub(crate) fn reopen(&self, id: u32, events: &EventLog) -> Result<(), ReopenError> {
        let slot = self.slot(id).ok_or(ReopenError::UnknownFile)?;
        slot.change(FileStatus::Closed, FileStatus::Opening)
            .map_err(ReopenError::NotAllowed)?;
        let loaded = load_hosted(
            id,
            &HostedFile {
                name: slot.name.clone(),
                path: slot.path.clone(),
            },
            self.insecure_files,
            events,
        );
        let mut state = slot.state();
        match loaded {
            Ok(hosted) => {
                state.status = FileStatus::Open;
                state.file = Some(hosted);
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
    /// never written. Deferred work recovered on loading stays unsaved until
    /// an eligible account logs in ([`Hosted::commit_recovery`]).
    pub(crate) fn local(
        hosted: &HostedFile,
        file: &HbamFile,
        lock: Option<FileLock>,
        read_only: bool,
    ) -> Result<Self, HostingError> {
        let configured = vec![hosted.clone()];
        let hosted = Hosted::load(
            1,
            hosted,
            file,
            Some((lock, read_only)),
            false,
            &EventLog::default(),
        )?;
        Ok(Hosting {
            slots: vec![Slot::serving(Arc::new(hosted))],
            configured,
            insecure_files: false,
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

/// Load the file `hosted` describes with server id `id`, saving first what
/// [`Store::recover`] finished: a server's files are recovered as they are
/// loaded. An insecure file is refused, before anything is written, unless
/// `insecure_files`. A failed save, that one included, is recorded in
/// `events`.
fn load_hosted(
    id: u32,
    hosted: &HostedFile,
    insecure_files: bool,
    events: &EventLog,
) -> Result<Arc<Hosted>, HostingError> {
    let file = read_regular_file(&hosted.path)
        .map_err(om_hbam::Error::from)
        .and_then(HbamFile::from_bytes)
        .map_err(|error| HostingError::Open {
            name: hosted.name.clone(),
            error,
        })?;
    let loaded = Arc::new(Hosted::load(
        id,
        hosted,
        &file,
        None,
        insecure_files,
        events,
    )?);
    drop(file);
    loaded.commit_recovery();
    Ok(loaded)
}

/// The bytes of the regular file at `path`, read through the one handle
/// whose type was checked: opened without blocking (a FIFO named as a
/// database would otherwise hold startup before the server listens), and
/// refused unless it is a regular file, so that one swapped in after a
/// check cannot be read instead.
fn read_regular_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(data)
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
            let hosting = Hosting::load(&files, false, &EventLog::default());
            let _ = done.send(hosting.map(|h| h.by_name(fixture::FILE).is_some()));
        });
        let loaded = wait.recv_timeout(std::time::Duration::from_secs(30));
        let _ = std::fs::remove_file(&fifo);
        assert!(matches!(loaded, Ok(Ok(true))), "the good file is hosted");
    }

    /// A password changed over the wire (#732's review: FileMaker Pro's
    /// Manage Security uploads the account's record with op13,
    /// `Store::put_subtree`) counts from the next login on, and the old
    /// one no more.
    #[test]
    fn logins_follow_the_security_catalog_as_the_store_holds_it() {
        let file = fixture::TempFile::new("hosting-accounts", &[], true);
        let hosted_file = HostedFile {
            name: fixture::FILE.into(),
            path: file.0.clone(),
        };
        let hosting = Hosting::load(&[hosted_file], false, &EventLog::default()).unwrap();
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
        assert!(hosted.write_store().put_subtree(&path, record));
        assert!(!clerk(fixture::CLERK_PASSWORD));
        assert!(clerk("changed"));
    }
}
