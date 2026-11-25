//! Backups of the hosted files: FileMaker Server's backup schedules, Back
//! Up Now, retention and preserved backups (docs/re/backups.md; what
//! OpenMaker does and its limits: docs/admin-console.md, "Backups").
//!
//! **A backup** is a folder in its schedule's target folder, laid out as
//! FileMaker Server lays one out (re 2): `<schedule>_YYYY-MM-DD_HHMM`
//! (the run's start, the scheduler's local time) holding FileMaker's
//! `backup.config` (label, start time in UTC, preserved, id) and
//! `Databases/` with a copy of each chosen file. OpenMaker Server has no
//! database folder of its own: `Databases/` holds the files as they lie
//! below the deepest folder that holds every hosted file, so that copying
//! it back restores them (inf). A file's externally stored container data
//! (`RC_Data_FMS/<file>/` beside it) is copied beside its copy, where
//! FileMaker Server puts it (re 2.1, read in its code). Links and special
//! files in it are left out and logged, while a container folder that is
//! a link, or one that cannot be read, fails the run (only a missing one
//! means "none"). Every container file is copied on every run: FileMaker
//! links those unchanged since the last backup (re 2.3, #493).
//!
//! **Through folder handles** (`fs`): each operation opens its target
//! folder once, from the path resolved then, a name at a time without
//! following links, and lists, renames, reads markers and configs, and
//! deletes through that handle, never through a link. A change to the
//! configured path, or to a folder above it, during the operation cannot
//! redirect it. Container data is read the same way. On systems without
//! these calls (Windows) every backup operation fails as unsupported.
//!
//! **A backup folder holds only backups**: one that holds a hosted file or
//! its container data, or lies in a hosted file's container data folder,
//! is refused when the server is set up ([`check_folder`]); a target that
//! comes to lie in container data is not used; and no backup folder that
//! hosted data has come to lie in is deleted (retention, Delete, the
//! cleanup of unfinished backups). Folders are compared by device and
//! inode numbers, found where the server reads the hosted files, links
//! followed (`fs::Protected`).
//!
//! **Consistency.** OpenMaker Server writes each change of a file as a
//! complete new file renamed over the old one, and never writes a file in
//! place (`crate::store::Store::save`). A run opens each file while it
//! holds the file's store for reading, so that no save is half done, and
//! lets go at once ([`crate::engine::FileEngine::stored`]): the descriptor
//! names that version of the file whatever later saves rename over it, so
//! the copy made from it is a snapshot of one save. The copy is an export
//! of that file (`crate::engine::export::copy`): written under a temporary
//! name in its folder of the backup, checked against the file (its length
//! and SHA-256), then renamed. The store is held only for that open call,
//! never while the file is copied. Clients see no pause (FileMaker Server
//! pauses each file at the end of its copy, re 3): a client's write waits
//! at most for one `open`, and a save in progress delays the backup, not
//! the client. Changes a failed save left in memory are not in the copy;
//! the run logs a warning.
//!
//! **Unchanged files are linked**, as FileMaker does (re 2.3): when the
//! file has not been saved since this schedule's last complete backup made
//! in this process (`Store::saves`), its entry is a hard link to that
//! backup's copy, while that copy is still the file the run made. After a
//! restart, the first run of each schedule copies every file.
//!
//! **A run** writes `<name>_InProgress`, flushes every file and folder,
//! and renames it to `<name>` once it is complete, so a crash never leaves
//! a backup that looks finished. A run that fails or is cancelled removes
//! its folder (FileMaker's Keep Cancelled Backups off). The folder carries
//! a marker naming the process writing it ([`MARKER`]) until it is
//! complete; the schedule's next run removes an `_InProgress` folder whose
//! marker, exactly in this server's form, names a process that is gone,
//! and leaves any other alone. If the target folder cannot be flushed
//! after the rename, the run fails, the backup stays, and no older backup
//! is deleted. After a run that completed, it deletes the schedule's
//! oldest backups past its keep count among those that are not preserved
//! ([`BackupTask::keep`]): only folders of that schedule's name with a
//! `backup.config`, in the target folder. A folder is removed through the
//! handle it was checked on, its contents first and then its name, which
//! fails unless the name still holds an empty folder. One run copies at a
//! time across the server (FileMaker's Parallel Backup off); a run due
//! meanwhile waits for its turn.
//!
//! **Preserved state and labels** live in each backup's `backup.config`,
//! where FileMaker keeps them; the backups are listed from the target
//! folders each time, so a folder copied in or out is seen as it is.
//!
//! The process log has FileMaker's event log lines (re 4) where they hold:
//! `Schedule "S" running.`, `Starting to back up N database(s) to folder
//! "...".`, `Starting to back up database "F", current status: Normal`,
//! the hard-link line, `Backup of "F" completed, current status: Normal`,
//! `Deleted backup folder "...".` and `Schedule "S" completed.`; failures
//! and cancels are OpenMaker's own lines.

mod config;
mod fs;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use jiff::civil::DateTime;
use jiff::{Timestamp, Zoned};
use log::{debug, error, info, warn};

use crate::anchored::{FileAt, FileId};
use crate::connection::Shared;
use crate::engine::{export, Destination, ExportError, Progress};
use crate::hosting::{save_error, Hosted};
use crate::schedules::{BackupFiles, BackupTask};
use crate::scripts::AbortReason;

pub use config::MAX_LABEL_CHARS;
use config::{Config, AUTO_BACKUP, IN_PROGRESS, ON_DEMAND};
use fs::{CopyError, Folder, Identity, Kind, Protected};

/// The most backups a schedule keeps (FileMaker's limit, 99).
pub const MAX_KEEP: u8 = 99;

/// The longest backup schedule name in characters (FileMaker's) and in
/// bytes, which keeps its folder names within a file system's 255.
const MAX_SCHEDULE_CHARS: usize = 100;
const MAX_SCHEDULE_BYTES: usize = 200;

/// The most entries read from one target folder to find its backups.
const MAX_FOLDER_ENTRIES: usize = 100_000;

/// The folder a backup holds its copies in.
const DATABASES: &str = "Databases";

/// The folder beside a hosted file that holds its externally stored
/// container data (re/containers.md 8.1).
const CONTAINER_FOLDER: &str = "RC_Data_FMS";

/// A run's errors (OpenMaker's choice of FileMaker codes; FileMaker
/// Server's for backups are unmeasured): 1 "User canceled action" for a
/// cancel, 802 "Unable to open file" when no hosted file is chosen or one
/// cannot be read, and a save's codes when the backup cannot be written
/// ([`save_error`]: 800, 809 for a full disk, 810 for a read-only one).
/// A run that panics fails with -1 (`crate::schedules`).
const CANCELLED: i32 = 1;
const UNOPENABLE: i32 = 802;
const CANNOT_WRITE: i32 = 800;

/// A backup, as the administrator sees it (the Backups tab, re 4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupInfo {
    /// Its id (`backup.config`'s 32 hex digits), which names it to
    /// [`crate::ServerHandle::preserve_backup`] and the others.
    pub id: String,
    /// The schedule its folder name names.
    pub schedule: String,
    /// Its folder's name, `<schedule>_YYYY-MM-DD_HHMM`.
    pub folder: String,
    /// Its folder.
    pub path: PathBuf,
    /// When its run started, as its folder name says (the server's local
    /// time, to the minute).
    pub started: DateTime,
    /// When its run started, as its `backup.config` says, if that reads.
    pub time: Option<Timestamp>,
    /// "Auto-backup", "On-demand", or the administrator's.
    pub label: String,
    /// Kept from automatic deletion.
    pub preserved: bool,
    /// The bytes of its files (a file it shares with other backups by a
    /// hard link counts in each).
    pub size: u64,
}

/// A backup run under way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupProgress {
    pub schedule: String,
    /// When it started, by the schedules' clock.
    pub started: Zoned,
    /// Back Up Now (or Run Now) started it.
    pub on_demand: bool,
    /// It waits for another backup to finish.
    pub waiting: bool,
    /// How many files it backs up, and how many it has done.
    pub files: usize,
    pub files_done: usize,
    /// The bytes it has copied (a linked file copies none).
    pub bytes_copied: u64,
}

/// Errors managing backups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupError {
    /// No backup has this id.
    NotFound(String),
    /// Several backups have this id (a folder copied within the backup
    /// folders): none is changed.
    Ambiguous(String),
    /// Only a preserved backup is deleted by hand.
    NotPreserved(String),
    /// A label that cannot be one.
    InvalidLabel(String),
    /// The backup could not be changed.
    Io(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::NotFound(id) => write!(f, "no backup {id}"),
            BackupError::Ambiguous(id) => write!(f, "several backups are {id}"),
            BackupError::NotPreserved(id) => write!(f, "backup {id} is not preserved"),
            BackupError::InvalidLabel(reason) => write!(f, "{reason}"),
            BackupError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BackupError {}

/// Why `name` cannot name a backup schedule: it begins each backup's
/// folder name, so it is a valid folder name on Linux, macOS and Windows
/// (FileMaker: "a valid folder name, with no characters or words reserved
/// by the operating system"), of at most 100 characters.
pub(crate) fn check_schedule_name(name: &str) -> Result<(), String> {
    if name.chars().count() > MAX_SCHEDULE_CHARS || name.len() > MAX_SCHEDULE_BYTES {
        return Err(format!(
            "a backup schedule's name has at most {MAX_SCHEDULE_CHARS} characters"
        ));
    }
    let reserved = |c: char| c.is_control() || "/\\:*?\"<>|".contains(c);
    if name.chars().any(reserved) || name.starts_with('.') || name.trim() != name {
        return Err(
            "a backup schedule's name is a valid folder name: no control characters, \
             none of / \\ : * ? \" < > |, no spaces around it, not starting with ."
                .into(),
        );
    }
    Ok(())
}

/// The backups of a server: the default backup folder, the runs under
/// way, and what each schedule's last backup copied.
#[derive(Default)]
pub(crate) struct Backups {
    folder: RwLock<Option<PathBuf>>,
    state: Mutex<State>,
    /// Signaled when a run ends or gives up its turn, and on a cancel.
    changed: Condvar,
    /// Held while backup folders are deleted or their configs written:
    /// retention, and the administrator's changes.
    manage: Mutex<()>,
    /// A test fixture: the next run's flush of its target folder fails
    /// ([`crate::ServerHandle::failing_next_backup_flush`]).
    #[cfg(debug_assertions)]
    fail_next_flush: std::sync::atomic::AtomicBool,
    /// A test fixture: run before a checked backup folder is removed
    /// ([`crate::ServerHandle::before_backup_removal`]).
    #[cfg(debug_assertions)]
    #[allow(clippy::type_complexity)]
    removing: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

impl std::fmt::Debug for Backups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backups").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    /// Shut down: no run starts.
    closed: bool,
    next_run: u64,
    runs: Vec<Run>,
    /// A run has its turn to copy.
    copying: bool,
    /// Per schedule (its name in lower case): the copies its last complete
    /// backup made, by hosted file id.
    last: HashMap<String, HashMap<u32, Made>>,
}

/// A run under way.
struct Run {
    id: u64,
    schedule: String,
    started: Zoned,
    on_demand: bool,
    cancel: Arc<OnceLock<AbortReason>>,
    copying: bool,
    files: usize,
    files_done: usize,
    bytes_copied: u64,
}

/// A copy a backup made of a hosted file.
#[derive(Clone, Debug)]
struct Made {
    /// The file's [`crate::store::Store::saves`] when it was copied.
    saves: u64,
    /// The finished backup's folder name in its target folder.
    backup: String,
    /// The copy's folder below the backup's `Databases`, and its name.
    relative: PathBuf,
    file: OsString,
    identity: Identity,
}

impl Made {
    /// Where the copy is in its target folder.
    fn path(&self) -> PathBuf {
        Path::new(&self.backup)
            .join(DATABASES)
            .join(&self.relative)
            .join(&self.file)
    }
}

/// A run's place among the backups: registered while it lives, so that
/// it can be listed and cancelled.
pub(crate) struct Ticket {
    backups: Arc<Backups>,
    id: u64,
    schedule: String,
    started: Zoned,
    on_demand: bool,
    cancel: Arc<OnceLock<AbortReason>>,
}

impl Ticket {
    fn cancelled(&self) -> Option<AbortReason> {
        self.cancel.get().copied()
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.backups.lock();
        if let Some(at) = state.runs.iter().position(|r| r.id == self.id) {
            if state.runs.remove(at).copying {
                state.copying = false;
            }
        }
        drop(state);
        self.backups.changed.notify_all();
    }
}

/// How a run ended.
pub(crate) struct Outcome {
    /// It began copying.
    pub copied: bool,
    pub error: i32,
    /// The backup's folder name, when it is complete.
    pub folder: Option<String>,
    pub cancelled: Option<AbortReason>,
}

/// Why a run did not complete.
enum Failure {
    Cancelled(AbortReason),
    Failed { code: i32, message: String },
}

impl Failure {
    fn io(what: &dyn std::fmt::Display, e: &io::Error) -> Self {
        Failure::Failed {
            code: i32::from(save_error(e)),
            message: format!("{what}: {e}"),
        }
    }

    fn copy(what: &dyn std::fmt::Display, e: CopyError, ticket: &Ticket) -> Self {
        match e {
            CopyError::Stopped => {
                Failure::Cancelled(ticket.cancelled().unwrap_or(AbortReason::Admin))
            }
            CopyError::Io(e) => Failure::io(what, &e),
        }
    }

    /// A file's copy to `what` failed: stopped, or not read or written,
    /// with a save's codes, as a copy failed before; or not read back as
    /// the file (800).
    fn export(what: &dyn std::fmt::Display, e: ExportError, ticket: &Ticket) -> Self {
        match e {
            ExportError::Stopped => {
                Failure::Cancelled(ticket.cancelled().unwrap_or(AbortReason::Admin))
            }
            ExportError::Io(e) | ExportError::Source(e) => Failure::io(what, &e),
            e => Failure::Failed {
                code: CANNOT_WRITE,
                message: format!("{what}: {e}"),
            },
        }
    }
}

impl Backups {
    /// The state. Every change leaves it whole, so a poisoned lock is used
    /// as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn manage(&self) -> MutexGuard<'_, ()> {
        self.manage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The default backup folder: backup schedules without their own go
    /// there.
    pub fn folder(&self) -> Option<PathBuf> {
        self.folder
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_folder(&self, folder: PathBuf) {
        *self.folder.write().unwrap_or_else(PoisonError::into_inner) = Some(folder);
    }

    /// Make the next run's flush of its target folder fail (tests).
    #[cfg(debug_assertions)]
    pub fn fail_next_flush(&self) {
        self.fail_next_flush
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The failure [`Backups::fail_next_flush`] asked for, once.
    fn flush_failure(&self) -> Option<io::Error> {
        #[cfg(debug_assertions)]
        {
            if self
                .fail_next_flush
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                return Some(io::Error::other("the flush failed (test fixture)"));
            }
        }
        None
    }

    /// Register a run of `schedule` started at `started`; `None` once the
    /// server shuts down.
    pub fn enter(
        self: &Arc<Self>,
        schedule: &str,
        started: Zoned,
        on_demand: bool,
    ) -> Option<Ticket> {
        let mut state = self.lock();
        if state.closed {
            return None;
        }
        state.next_run += 1;
        let id = state.next_run;
        let cancel = Arc::new(OnceLock::new());
        state.runs.push(Run {
            id,
            schedule: schedule.to_string(),
            started: started.clone(),
            on_demand,
            cancel: cancel.clone(),
            copying: false,
            files: 0,
            files_done: 0,
            bytes_copied: 0,
        });
        Some(Ticket {
            backups: self.clone(),
            id,
            schedule: schedule.to_string(),
            started,
            on_demand,
            cancel,
        })
    }

    /// The runs under way, the one copying first.
    pub fn progress(&self) -> Vec<BackupProgress> {
        let state = self.lock();
        let mut runs: Vec<BackupProgress> = state
            .runs
            .iter()
            .map(|run| BackupProgress {
                schedule: run.schedule.clone(),
                started: run.started.clone(),
                on_demand: run.on_demand,
                waiting: !run.copying,
                files: run.files,
                files_done: run.files_done,
                bytes_copied: run.bytes_copied,
            })
            .collect();
        runs.sort_by_key(|run| run.waiting);
        runs
    }

    /// Cancel the backup that is copying (FileMaker's Cancel): it stops
    /// within a few megabytes and removes its folder. Returns whether one
    /// was copying.
    pub fn cancel(&self) -> bool {
        let state = self.lock();
        let mut cancelled = false;
        for run in state.runs.iter().filter(|run| run.copying) {
            cancelled |= run.cancel.set(AbortReason::Admin).is_ok();
        }
        drop(state);
        self.changed.notify_all();
        cancelled
    }

    /// Shut down: start no run from now on, and cancel every run, those
    /// waiting for their turn too.
    pub fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        for run in &state.runs {
            let _ = run.cancel.set(AbortReason::Shutdown);
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Wait up to `timeout` until no run is under way. Returns whether
    /// none is.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while !state.runs.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Wait for `ticket`'s turn to copy; the reason when it is cancelled
    /// first.
    fn take_turn(&self, ticket: &Ticket) -> Result<(), AbortReason> {
        let mut state = self.lock();
        loop {
            if let Some(reason) = ticket.cancelled() {
                return Err(reason);
            }
            if !state.copying {
                state.copying = true;
                if let Some(run) = state.runs.iter_mut().find(|r| r.id == ticket.id) {
                    run.copying = true;
                }
                return Ok(());
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn update(&self, ticket: &Ticket, change: impl FnOnce(&mut Run)) {
        if let Some(run) = self.lock().runs.iter_mut().find(|r| r.id == ticket.id) {
            change(run);
        }
    }

    /// The backups in the folders `folders` (each once), newest first.
    pub fn list(&self, shared: &Shared, folders: &[PathBuf]) -> Vec<BackupInfo> {
        let Ok(protected) = protected(shared) else {
            return Vec::new();
        };
        let mut backups: Vec<BackupInfo> = open_targets(folders, &protected)
            .iter()
            .flat_map(|target| {
                found_in(&target.folder)
                    .into_iter()
                    .map(|found| found.info(target))
                    .collect::<Vec<_>>()
            })
            .collect();
        backups.sort_by(|a, b| (b.started, b.time, &b.folder).cmp(&(a.started, a.time, &a.folder)));
        backups
    }

    /// Preserve the backup `id` in `folders`, or stop preserving it.
    pub fn set_preserved(
        &self,
        shared: &Shared,
        folders: &[PathBuf],
        id: &str,
        preserved: bool,
    ) -> Result<BackupInfo, BackupError> {
        let _manage = self.manage();
        let (target, mut found) = find(shared, folders, id)?;
        found.config.set_preserved(preserved);
        config::write(&found.folder, &found.config).map_err(|e| BackupError::Io(e.to_string()))?;
        let info = found.info(&target);
        info!(
            "Backup {:?} {}.",
            info.path.display(),
            if preserved {
                "preserved"
            } else {
                "no longer preserved"
            }
        );
        Ok(info)
    }

    /// Give the backup `id` in `folders` the label `label`.
    pub fn set_label(
        &self,
        shared: &Shared,
        folders: &[PathBuf],
        id: &str,
        label: &str,
    ) -> Result<BackupInfo, BackupError> {
        config::check_label(label).map_err(BackupError::InvalidLabel)?;
        let _manage = self.manage();
        let (target, mut found) = find(shared, folders, id)?;
        found.config.set_label(label);
        config::write(&found.folder, &found.config).map_err(|e| BackupError::Io(e.to_string()))?;
        let info = found.info(&target);
        info!("Backup {:?} labeled {label:?}.", info.path.display());
        Ok(info)
    }

    /// Delete the preserved backup `id` in `folders`, unless a file
    /// `shared` hosts, or its container data, lies in it.
    pub fn delete(
        &self,
        shared: &Shared,
        folders: &[PathBuf],
        id: &str,
    ) -> Result<(), BackupError> {
        let _manage = self.manage();
        let protected = protected(shared).map_err(|e| BackupError::Io(e.to_string()))?;
        if let Some(reason) = protected.removal_refused() {
            return Err(BackupError::Io(format!("no backup is deleted: {reason}")));
        }
        let (target, found) = find_in(&open_targets(folders, &protected), id)?;
        if !found.config.preserved() {
            return Err(BackupError::NotPreserved(found.config.id()));
        }
        let path = target.path.join(&found.name);
        if protected.lies_in(&found.folder).unwrap_or(true) {
            return Err(BackupError::Io(format!(
                "{} holds a file the server hosts, or its container data: not deleted",
                path.display()
            )));
        }
        self.before_removal();
        target
            .folder
            .remove(OsStr::new(&found.name), found.folder)
            .map_err(|e| BackupError::Io(format!("{}: {e}", path.display())))?;
        info!("Deleted backup folder \"{}/\".", path.display());
        Ok(())
    }

    /// Delete the oldest backups of `schedule` in `target` past `keep`,
    /// among those that are not preserved, never `made`, nor one that
    /// hosted data lies in.
    fn retain(&self, target: &Target, protected: &Protected, schedule: &str, keep: u8, made: &str) {
        if let Some(reason) = protected.removal_refused() {
            error!("Schedule {schedule:?}: no old backup is deleted: {reason}");
            return;
        }
        let _manage = self.manage();
        let mut kept: Vec<Found> = found_in(&target.folder)
            .into_iter()
            .filter(|found| found.schedule == schedule && !found.config.preserved())
            .collect();
        kept.sort_by_key(|found| (found.started, found.config.time()));
        let excess = kept.len().saturating_sub(usize::from(keep));
        for found in kept
            .into_iter()
            .take(excess)
            .filter(|found| found.name != made)
        {
            let path = target.path.join(&found.name);
            if protected.lies_in(&found.folder).unwrap_or(true) {
                error!(
                    "Schedule {schedule:?}: the old backup {} holds a file the server hosts, or \
                     its container data: not deleted",
                    path.display()
                );
                continue;
            }
            self.before_removal();
            match target.folder.remove(OsStr::new(&found.name), found.folder) {
                Ok(()) => info!("Deleted backup folder \"{}/\".", path.display()),
                Err(e) => warn!(
                    "Schedule {schedule:?}: cannot delete the old backup {}: {e}",
                    path.display()
                ),
            }
        }
    }

    /// Run the test hook set to run before a backup folder is removed.
    fn before_removal(&self) {
        #[cfg(debug_assertions)]
        {
            let mut hook = self.removing.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hook) = hook.as_mut() {
                hook();
            }
        }
    }

    /// Run `hook` each time a backup folder is about to be removed, after
    /// it was checked (tests).
    #[cfg(debug_assertions)]
    pub fn on_removal(&self, hook: Box<dyn FnMut() + Send>) {
        *self.removing.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }
}

/// A target folder, opened for one operation: what an operation lists,
/// renames and deletes is reached through its handle.
struct Target {
    /// Its path as configured, made absolute (for the log and the lists).
    path: PathBuf,
    folder: Folder,
}

/// The folders `folders`, each opened once ([`Folder::open_configured`]):
/// the same folder named twice once, and one that is missing, cannot be
/// opened or lies in container data left out.
fn open_targets(folders: &[PathBuf], protected: &Protected) -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();
    let mut seen = Vec::new();
    for path in folders {
        let folder = match Folder::open_configured(path) {
            Ok(folder) => folder,
            Err(e) => {
                debug!("{}: {e}", path.display());
                continue;
            }
        };
        let Ok(id) = folder.id() else {
            continue;
        };
        if seen.contains(&id) {
            continue;
        }
        if protected.in_container_data(&folder).unwrap_or(true) {
            warn!(
                "{}: lies in a hosted file's container data: not used as a backup folder",
                path.display()
            );
            continue;
        }
        seen.push(id);
        targets.push(Target {
            path: path.clone(),
            folder,
        });
    }
    targets
}

/// A backup found in a target folder, opened.
struct Found {
    name: String,
    schedule: String,
    started: DateTime,
    folder: Folder,
    config: Config,
}

impl Found {
    fn info(&self, target: &Target) -> BackupInfo {
        BackupInfo {
            id: self.config.id(),
            schedule: self.schedule.clone(),
            folder: self.name.clone(),
            path: target.path.join(&self.name),
            started: self.started,
            time: self.config.time(),
            label: self.config.label().to_string(),
            preserved: self.config.preserved(),
            size: self.folder.size(),
        }
    }
}

/// The backups in `target`: folders (not links) named
/// `<schedule>_YYYY-MM-DD_HHMM` holding a `backup.config` that reads.
/// Unfinished backups (`_InProgress`) are not among them.
fn found_in(target: &Folder) -> Vec<Found> {
    let Ok(entries) = target.entries() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (name, kind) in entries.into_iter().take(MAX_FOLDER_ENTRIES) {
        let Ok(name) = name.into_string() else {
            continue;
        };
        let Some((schedule, started)) = config::parse_folder_name(&name) else {
            continue;
        };
        if kind != Kind::Folder {
            continue;
        }
        let Ok(folder) = target.open(OsStr::new(&name)) else {
            continue;
        };
        match config::read(&folder) {
            Ok(config) => found.push(Found {
                schedule: schedule.to_string(),
                started,
                name,
                folder,
                config,
            }),
            Err(e) => debug!("{name}: not a backup: {e}"),
        }
    }
    found
}

/// The one backup with id `id` in `folders`, and its target.
fn find(shared: &Shared, folders: &[PathBuf], id: &str) -> Result<(Target, Found), BackupError> {
    let protected = protected(shared).map_err(|e| BackupError::Io(e.to_string()))?;
    find_in(&open_targets(folders, &protected), id)
}

/// The one backup with id `id` in `targets`, and its target (opened
/// again for the caller).
fn find_in(targets: &[Target], id: &str) -> Result<(Target, Found), BackupError> {
    let wanted = id.to_ascii_uppercase();
    let mut matches: Vec<(&Target, Found)> = targets
        .iter()
        .flat_map(|target| {
            found_in(&target.folder)
                .into_iter()
                .filter(|found| found.config.id() == wanted)
                .map(move |found| (target, found))
                .collect::<Vec<_>>()
        })
        .collect();
    match matches.len() {
        0 => Err(BackupError::NotFound(id.to_string())),
        1 => {
            let (target, found) = matches.remove(0);
            let folder = target
                .folder
                .duplicate()
                .map_err(|e| BackupError::Io(e.to_string()))?;
            Ok((
                Target {
                    path: target.path.clone(),
                    folder,
                },
                found,
            ))
        }
        _ => Err(BackupError::Ambiguous(id.to_string())),
    }
}

/// The folder a hosted file is read from: the folder its path names.
fn home_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The folders the server's files and their container data are in now,
/// where the server reads them (links followed): each file's folder, its
/// `RC_Data_FMS` and its `RC_Data_FMS/<file>` (`fs` module docs). A file's
/// folder is both the folder its path names and the folder of the file
/// that path leads to, which differ when the path is a link to a file
/// elsewhere. For a slot that was hosted through a file link, also
/// protect the location where the server saves and reopens the file.
///
/// Every file the server was given counts, whether it is open, paused,
/// closing or closed ([`crate::hosting::Slot`]), or failed to open at
/// startup: closing or failing does not make a file's folder deletable.
/// A file the server hosts (it has a slot) that cannot be found makes this
/// fail, and with it every deletion. A file that failed at startup and
/// cannot be found keeps what of it can be opened (its folder and
/// container data) and is recorded as unresolved: nothing is deleted
/// while it is ([`Protected::removal_refused`]), but backups are still
/// made and checked against what is known. An unverified saved location
/// also blocks removal, including when its slot is closed or opening.
fn protected(shared: &Shared) -> io::Result<Protected> {
    let mut protected = Protected::default();
    for configured in shared.hosting.configured() {
        let slot = shared.hosting.slot_named(&configured.name);
        match protect(&mut protected, &configured.path) {
            Ok(()) => {}
            Err(e) if slot.is_none() && absent(&e) => protected.add_unresolved(&configured.path),
            Err(e) => return Err(e),
        }
        if let Some(slot) = slot {
            let at = slot.file_at();
            // The saved location can differ from the configured link's
            // current target, and a closed slot still reopens there (#468).
            if at.place() != configured.path.as_path()
                && protect(&mut protected, at.place()).is_err()
            {
                // Incomplete protection prevents removal. A schedule can
                // still copy its own files outside the known container data.
                protected.add_unresolved(at.place());
            }
            // The last saved file may lie in a backup if its path no
            // longer leads to it, even while the slot is closed (#468).
            if !leads_to_saved(at, slot.saved_file()) {
                protected.add_unresolved(at.place());
            }
        }
    }
    Ok(protected)
}

/// Whether `at` still names the last saved file, where its identity is known.
fn leads_to_saved(at: &FileAt, expected: Option<FileId>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    at.folder_now()
        .and_then(|folder| folder.entry(at.name()))
        .is_ok_and(|entry| entry.is_some_and(|e| e.regular && e.id == Some(expected)))
}

/// A file or folder that is not there.
fn absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Record in `protected` the folders of the file at `path`: the folder its
/// path names and its container data first, then the folder the path
/// leads to.
fn protect(protected: &mut Protected, path: &Path) -> io::Result<()> {
    let home = Folder::open_following(home_of(path))?;
    protected.add_home(&home)?;
    match home.open_following_at(OsStr::new(CONTAINER_FOLDER)) {
        Ok(containers) => {
            protected.add_container(&containers)?;
            if let Some(stem) = path.file_stem() {
                match containers.open_following_at(stem) {
                    Ok(own) => protected.add_container(&own)?,
                    Err(e) if absent(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Err(e) if absent(&e) => {}
        Err(e) => return Err(e),
    }
    let real = std::fs::canonicalize(path)?;
    if let Some(parent) = real.parent() {
        protected.add_home(&Folder::open_following(parent)?)?;
    }
    Ok(())
}

/// Why `folder` cannot hold backups of `shared`'s files: retention deletes
/// old backups in it, so hosted data may not lie in it, nor it in a hosted
/// file's container data (OpenMaker's rule; FileMaker keeps backups in a
/// folder of their own). A folder beside the hosted files, or below the
/// folder that holds them, is fine. Compared by device and inode numbers,
/// links followed; a folder that does not exist yet by the nearest one
/// above it, and the names it would be made by.
#[cfg(unix)]
pub(crate) fn check_folder(shared: &Shared, folder: &Path) -> Result<(), String> {
    let cannot = |e: io::Error| format!("cannot check the backup folder {}: {e}", folder.display());
    let protected = protected(shared).map_err(cannot)?;
    let absolute = std::path::absolute(folder).map_err(cannot)?;
    let mut existing = absolute.as_path();
    let opened = loop {
        match Folder::open_following(existing) {
            Ok(opened) => break opened,
            Err(e) if e.kind() == io::ErrorKind::NotFound => match existing.parent() {
                Some(parent) => existing = parent,
                None => return Ok(()),
            },
            Err(e) => return Err(cannot(e)),
        }
    };
    let in_containers = protected.in_container_data(&opened).map_err(cannot)?
        // Made later inside a hosted file's folder as `RC_Data_FMS/...`.
        || (existing != absolute
            && protected.is_home(&opened).map_err(cannot)?
            && absolute
                .strip_prefix(existing)
                .ok()
                .and_then(|rest| rest.components().next())
                == Some(Component::Normal(OsStr::new(CONTAINER_FOLDER))));
    if in_containers {
        return Err(format!(
            "the backup folder {} lies in a hosted file's container data folder ({CONTAINER_FOLDER})",
            folder.display()
        ));
    }
    if existing == absolute && protected.lies_in(&opened).map_err(cannot)? {
        return Err(format!(
            "the backup folder {} holds a hosted file or its container data: old backups are \
             deleted from it, so give backups a folder of their own",
            folder.display()
        ));
    }
    Ok(())
}

/// Backups need descriptor-relative calls: they are refused when they
/// run, so there is nothing to check here.
#[cfg(not(unix))]
pub(crate) fn check_folder(_shared: &Shared, _folder: &Path) -> Result<(), String> {
    Ok(())
}

/// The hosted files `files` chooses, in id order: each only if it is open
/// or paused, taken under its slot's lock ([`crate::hosting::Slot::admit`]),
/// so that a backup never holds a file that is closing or closed. The run
/// holds what it takes until it ends, as a script does, so a close waits
/// for it.
pub(crate) fn chosen(shared: &Shared, files: &BackupFiles) -> Vec<Arc<Hosted>> {
    let slots = shared.hosting.slots();
    match files {
        BackupFiles::All => slots
            .iter()
            .filter_map(|slot| slot.admit(Arc::clone).ok())
            .collect(),
        BackupFiles::File(name) => shared.hosting.by_name(name).into_iter().collect(),
        BackupFiles::Folder(folder) => {
            let Ok(folder) = std::fs::canonicalize(folder) else {
                return Vec::new();
            };
            slots
                .iter()
                .filter(|slot| real_folder(&slot.path).is_ok_and(|real| real.starts_with(&folder)))
                .filter_map(|slot| slot.admit(Arc::clone).ok())
                .collect()
        }
    }
}

/// The folder that holds the file at `path`, links resolved.
fn real_folder(path: &Path) -> io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    Ok(path.parent().map(Path::to_path_buf).unwrap_or(path))
}

/// The deepest folder that holds every hosted file, open or not: a file
/// closing or opening again does not move the backups' layout.
fn database_root(shared: &Shared) -> PathBuf {
    let mut root: Option<PathBuf> = None;
    for slot in shared.hosting.slots() {
        let Ok(folder) = real_folder(&slot.path) else {
            continue;
        };
        root = Some(match root {
            None => folder,
            Some(root) => root
                .components()
                .zip(folder.components())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    root.unwrap_or_default()
}

/// The name FileMaker's event log gives a file: without its extension.
fn shown_name(hosted: &Hosted) -> String {
    Path::new(&hosted.name)
        .file_stem()
        .map_or_else(|| hosted.name.clone(), |s| s.to_string_lossy().into_owned())
}

/// One run of a backup schedule: wait for its turn, write the backup,
/// then apply the schedule's keep count.
pub(crate) fn run(
    shared: &Shared,
    backups: &Backups,
    ticket: Ticket,
    task: &BackupTask,
) -> Outcome {
    let name = ticket.schedule.clone();
    info!("Schedule {name:?} running.");
    let failed = |copied: bool, failure: Failure| {
        let (error, cancelled) = match failure {
            Failure::Cancelled(reason) => {
                warn!(
                    "Schedule {name:?} aborted; {} (error {CANCELLED}).",
                    match reason {
                        AbortReason::Shutdown => "the server is shutting down",
                        _ => "an administrator cancelled the backup",
                    }
                );
                (CANCELLED, Some(reason))
            }
            Failure::Failed { code, message } => {
                error!("Schedule {name:?} aborted; {message} (error {code}).");
                (code, None)
            }
        };
        Outcome {
            copied,
            error,
            folder: None,
            cancelled,
        }
    };
    let Some(target) = task.folder.clone().or_else(|| backups.folder()) else {
        return failed(
            false,
            Failure::Failed {
                code: CANNOT_WRITE,
                message: "no backup folder: give the schedule a target or the server one".into(),
            },
        );
    };
    let files = chosen(shared, &task.files);
    if files.is_empty() {
        return failed(
            false,
            Failure::Failed {
                code: UNOPENABLE,
                message: "no hosted file is in its backup".into(),
            },
        );
    }
    if let Err(reason) = backups.take_turn(&ticket) {
        return failed(false, Failure::Cancelled(reason));
    }
    backups.update(&ticket, |run| run.files = files.len());
    let path = std::path::absolute(&target).unwrap_or(target);
    let opened = (|| {
        make_target(&path).map_err(|e| Failure::io(&path.display(), &e))?;
        let protected = protected(shared).map_err(|e| Failure::io(&"the hosted files", &e))?;
        let folder =
            Folder::open_configured(&path).map_err(|e| Failure::io(&path.display(), &e))?;
        if protected
            .in_container_data(&folder)
            .map_err(|e| Failure::io(&path.display(), &e))?
        {
            return Err(Failure::Failed {
                code: CANNOT_WRITE,
                message: format!(
                    "{} lies in a hosted file's container data: no backup is made there",
                    path.display()
                ),
            });
        }
        Ok((
            Target {
                path: path.clone(),
                folder,
            },
            protected,
        ))
    })();
    let (target, protected) = match opened {
        Ok(opened) => opened,
        Err(failure) => return failed(false, failure),
    };
    match write_backup(shared, backups, &ticket, &target, &protected, &files) {
        Ok(folder) => {
            backups.retain(&target, &protected, &name, task.keep, &folder);
            info!("Schedule {name:?} completed.");
            Outcome {
                copied: true,
                error: 0,
                folder: Some(folder),
                cancelled: None,
            }
        }
        Err(failure) => failed(true, failure),
    }
}

/// Removes an unfinished backup's folder unless the run completes: its
/// contents through the run's own handle on it, then its name in the
/// target folder; not when hosted data has come to lie in it.
struct Unfinished<'a> {
    target: &'a Target,
    protected: &'a Protected,
    name: OsString,
    folder: Option<Folder>,
}

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        let Some(folder) = self.folder.take() else {
            return;
        };
        let path = self.target.path.join(&self.name);
        if self.protected.lies_in(&folder).unwrap_or(true) {
            error!(
                "the unfinished backup {} holds a file the server hosts, or its container data: \
                 not removed",
                path.display()
            );
            return;
        }
        match self.target.folder.remove(&self.name, folder) {
            Ok(()) => info!("Removed the unfinished backup {}", path.display()),
            Err(e) => warn!(
                "cannot remove the unfinished backup {}: {e}",
                path.display()
            ),
        }
    }
}

/// Write `ticket`'s backup of `files` in `target`; its folder name.
fn write_backup(
    shared: &Shared,
    backups: &Backups,
    ticket: &Ticket,
    target: &Target,
    protected: &Protected,
    files: &[Arc<Hosted>],
) -> Result<String, Failure> {
    let schedule = &ticket.schedule;
    let name = config::folder_name(schedule, ticket.started.datetime());
    let done = target.path.join(&name);
    let temporary_name = OsString::from(format!("{name}{IN_PROGRESS}"));
    let temporary_path = target.path.join(&temporary_name);
    let exists = |what: &str| Failure::Failed {
        code: CANNOT_WRITE,
        message: format!("{} {what}", done.display()),
    };
    if target
        .folder
        .stat(OsStr::new(&name))
        .map_err(|e| Failure::io(&done.display(), &e))?
        .is_some()
    {
        return Err(exists(
            "already exists: one backup of a schedule per minute",
        ));
    }
    remove_unfinished(backups, target, protected, schedule, writer(shared));
    let temporary = target
        .folder
        .make_new(&temporary_name)
        .map_err(|e| Failure::io(&temporary_path.display(), &e))?;
    let mut unfinished = Unfinished {
        target,
        protected,
        name: temporary_name.clone(),
        folder: Some(
            temporary
                .duplicate()
                .map_err(|e| Failure::io(&temporary_path.display(), &e))?,
        ),
    };
    write_marker(&temporary, writer(shared))
        .map_err(|e| Failure::io(&temporary_path.display(), &e))?;
    info!(
        "Starting to back up {} database(s) to folder \"{}/\".",
        files.len(),
        temporary_path.display()
    );
    let databases = temporary
        .make_new(OsStr::new(DATABASES))
        .map_err(|e| Failure::io(&temporary_path.display(), &e))?;
    // The folders made in `Databases`, by their path below it, to flush
    // once every copy is in them.
    let mut folders: Vec<(PathBuf, Folder)> = Vec::new();
    let root = database_root(shared);
    let last = backups
        .lock()
        .last
        .get(&schedule.to_lowercase())
        .cloned()
        .unwrap_or_default();
    let mut made = HashMap::new();
    let stop = || ticket.cancelled().is_some();
    let copied = |n: u64| backups.update(ticket, |run| run.bytes_copied += n);
    for hosted in files {
        if let Some(reason) = ticket.cancelled() {
            return Err(Failure::Cancelled(reason));
        }
        let unreadable = |e: &io::Error| Failure::Failed {
            code: UNOPENABLE,
            message: format!("{}: {e}", hosted.path.display()),
        };
        let real = real_folder(&hosted.path).map_err(|e| unreadable(&e))?;
        // Below the root, every part a plain name (the path is canonical).
        let relative: PathBuf = real
            .strip_prefix(&root)
            .unwrap_or(Path::new(""))
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .collect();
        let file_name = hosted
            .path
            .file_name()
            .ok_or_else(|| unreadable(&io::Error::other("no file name")))?;
        let dest_path = temporary_path.join(DATABASES).join(&relative);
        let into = folder_below(&databases, &relative, &mut folders)
            .map_err(|e| Failure::io(&dest_path.display(), &e))?;
        // The file as its last save left it, opened under the store's read
        // lock for that call alone, and copied after it.
        let source = shared.engine.stored(hosted).map_err(|e| unreadable(&e))?;
        if source.unsaved {
            warn!(
                "{:?}: the backup holds the file as last saved: changes the server could not \
                 save are not in it",
                hosted.name
            );
        }
        let shown = shown_name(hosted);
        let previous = last.get(&hosted.id).filter(|p| p.saves == source.saves);
        let linked = match previous {
            Some(previous) => link_previous(&target.folder, previous, &into, file_name)
                .map_err(|e| Failure::io(&dest_path.display(), &e))?,
            None => false,
        };
        let identity = if let (true, Some(previous)) = (linked, previous) {
            info!(
                "Backup of {shown:?} skipped because no changes were made since last backup; \
                 created hard link to last backup file \"{}\".",
                target.path.join(previous.path()).display()
            );
            previous.identity
        } else {
            info!("Starting to back up database {shown:?}, current status: Normal");
            let to = Destination {
                folder: &into,
                name: file_name,
                permissions: Some(source.permissions.clone()),
            };
            let progress = Progress {
                stop: &stop,
                written: &copied,
            };
            let exported = export::copy(&source, &to, progress)
                .map_err(|e| Failure::export(&dest_path.join(file_name).display(), e, ticket))?;
            let id = exported.id.ok_or_else(|| Failure::Failed {
                code: CANNOT_WRITE,
                message: format!(
                    "{}: the copy has no device and inode numbers to link it by",
                    dest_path.join(file_name).display()
                ),
            })?;
            info!("Backup of {shown:?} completed, current status: Normal");
            Identity::exported(id, exported.size)
        };
        made.insert(
            hosted.id,
            Made {
                saves: source.saves,
                backup: name.clone(),
                relative: relative.clone(),
                file: file_name.to_os_string(),
                identity,
            },
        );
        back_up_container_data(hosted, &into, &stop, &mut |n| copied(n))
            .map_err(|e| Failure::copy(&hosted.path.display(), e, ticket))?;
        backups.update(ticket, |run| run.files_done += 1);
    }
    let stamp = Config::new(
        if ticket.on_demand {
            ON_DEMAND
        } else {
            AUTO_BACKUP
        },
        ticket.started.timestamp(),
        config::new_id(),
    );
    config::write(&temporary, &stamp).map_err(|e| Failure::io(&temporary_path.display(), &e))?;
    // Every entry reaches the disk before the backup looks finished:
    // the deepest folders first.
    folders.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, folder) in folders.iter().chain([(PathBuf::new(), databases)].iter()) {
        folder
            .sync()
            .map_err(|e| Failure::io(&temporary_path.join(DATABASES).join(path).display(), &e))?;
    }
    temporary
        .sync()
        .map_err(|e| Failure::io(&temporary_path.display(), &e))?;
    if let Some(reason) = ticket.cancelled() {
        return Err(Failure::Cancelled(reason));
    }
    target
        .folder
        .rename_new(&temporary_name, OsStr::new(&name))
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => exists("appeared while the backup was written"),
            _ => Failure::io(&done.display(), &e),
        })?;
    unfinished.folder = None;
    drop(unfinished);
    if let Err(e) = temporary.remove_file(OsStr::new(MARKER)) {
        warn!("{}: cannot remove {MARKER}: {e}", done.display());
    }
    // Until the target folder is flushed, a crash may lose the rename: the
    // run fails, the backup stays as it is, and the schedule's earlier
    // backups are kept (no retention) and not linked to.
    let flushed = backups
        .flush_failure()
        .map_or_else(|| target.folder.sync(), Err);
    if let Err(e) = flushed {
        return Err(Failure::io(
            &format_args!(
                "{} is written, but flushing {} failed, so it may not survive a crash; \
                 it is kept and no older backup is deleted",
                done.display(),
                target.path.display()
            ),
            &e,
        ));
    }
    backups.lock().last.insert(schedule.to_lowercase(), made);
    Ok(name)
}

/// The folder `relative` below `databases`, made as needed, each made one
/// kept in `folders` to be flushed.
fn folder_below(
    databases: &Folder,
    relative: &Path,
    folders: &mut Vec<(PathBuf, Folder)>,
) -> io::Result<Folder> {
    let mut folder = databases.duplicate()?;
    let mut path = PathBuf::new();
    for part in relative.components() {
        let Component::Normal(name) = part else {
            continue;
        };
        path.push(name);
        folder = folder.make(name)?;
        if !folders.iter().any(|(p, _)| *p == path) {
            folders.push((path.clone(), folder.duplicate()?));
        }
    }
    Ok(folder)
}

/// Link `file` in `into` to the copy `previous` made, through `target`
/// without following links; whether it did.
fn link_previous(
    target: &Folder,
    previous: &Made,
    into: &Folder,
    file: &OsStr,
) -> io::Result<bool> {
    let opened = (|| {
        let mut folder = target
            .open(OsStr::new(&previous.backup))?
            .open(OsStr::new(DATABASES))?;
        for part in previous.relative.components() {
            if let Component::Normal(name) = part {
                folder = folder.open(name)?;
            }
        }
        Ok::<_, io::Error>(folder)
    })();
    match opened {
        Ok(from) => fs::link(&from, &previous.file, into, file, previous.identity),
        Err(e) => {
            debug!("no link to {}: {e}", previous.path().display());
            Ok(false)
        }
    }
}

/// Make the target folder if it is missing, owner-only (it holds whole
/// databases; OpenMaker's choice), with the folders above it.
fn make_target(target: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(target)
}

/// The marker file a run puts in its `_InProgress` folder, naming the
/// process that writes it (OpenMaker's own; FileMaker's is unknown): only a
/// folder that carries it and whose writer is gone is removed as an
/// unfinished backup. It is removed once the backup is complete.
const MARKER: &str = ".openmaker-in-progress";

/// The marker's first line.
const MARKER_HEADER: &str = "OpenMaker Server is writing this backup. If it stops first, its \
                             next run of the schedule removes this folder.";

/// The largest marker read: one this server writes is about 140 bytes.
const MAX_MARKER_BYTES: u64 = 256;

/// A process that writes backups: its id and when it started (nanoseconds
/// since the Unix epoch), which a marker records with the id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Writer {
    pid: u32,
    started: u128,
}

/// This server process as a [`Writer`].
fn writer(shared: &Shared) -> Writer {
    Writer {
        pid: std::process::id(),
        started: shared
            .started
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    }
}

/// The marker's text: the header, then `pid=` and `started=` lines.
fn marker_text(writer: Writer) -> String {
    format!(
        "{MARKER_HEADER}\npid={}\nstarted={}\n",
        writer.pid, writer.started
    )
}

/// Put the marker naming `writer` in the new folder `folder`, flushed.
fn write_marker(folder: &Folder, writer: Writer) -> io::Result<()> {
    use std::io::Write;
    let mut file = folder.create_file(OsStr::new(MARKER), 0o600)?;
    file.write_all(marker_text(writer).as_bytes())?;
    file.sync_all()?;
    folder.sync()
}

/// The writer the marker in `folder` names, when it is a regular file of
/// at most [`MAX_MARKER_BYTES`] holding exactly what [`marker_text`]
/// writes.
fn read_marker(folder: &Folder) -> Option<Writer> {
    use std::io::Read;
    let file = folder.read_file(OsStr::new(MARKER)).ok()?;
    if file.metadata().ok()?.len() > MAX_MARKER_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    parse_marker(&text)
}

/// The writer `text` names, when it is exactly a marker this server
/// writes.
fn parse_marker(text: &str) -> Option<Writer> {
    if text.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    let rest = text.strip_prefix(MARKER_HEADER)?.strip_prefix('\n')?;
    let (pid, rest) = rest.strip_prefix("pid=")?.split_once('\n')?;
    let started = rest.strip_prefix("started=")?.strip_suffix('\n')?;
    fn digits(text: &str) -> Option<&str> {
        text.bytes().all(|b| b.is_ascii_digit()).then_some(text)
    }
    let writer = Writer {
        pid: digits(pid)?.parse().ok()?,
        started: digits(started)?.parse().ok()?,
    };
    // Nothing else, and in the one form this server writes.
    (marker_text(writer) == text).then_some(writer)
}

/// Whether the backup `marked` writes is abandoned, seen from `me`: this
/// process's own (a run of the schedule is not under way: runs of a
/// schedule never overlap), an earlier process's under this one's id, or
/// a process that no longer runs. Another live process's is not; nor is
/// one whose process id the system has given to another process since,
/// which is left until that process ends.
fn abandoned(marked: Writer, me: Writer) -> bool {
    marked.pid == me.pid || !process_runs(marked.pid)
}

/// Whether process `pid` runs on this machine (a signal 0 reaches it, or
/// is refused for permission).
#[cfg(unix)]
fn process_runs(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    rustix::process::test_kill_process(pid) != Err(rustix::io::Errno::SRCH)
}

/// Without a way to ask, another process's backup is taken to be under way.
#[cfg(not(unix))]
fn process_runs(_pid: u32) -> bool {
    true
}

/// Remove the unfinished backups of `schedule` in `target` that runs which
/// never ended (the server stopped or crashed) left: `_InProgress` folders
/// carrying [`MARKER`] whose writer is gone ([`abandoned`]), unless hosted
/// data lies in one. One without a marker this server wrote is left alone
/// and logged: it may be anyone's.
fn remove_unfinished(
    backups: &Backups,
    target: &Target,
    protected: &Protected,
    schedule: &str,
    me: Writer,
) {
    if let Some(reason) = protected.removal_refused() {
        error!("Schedule {schedule:?}: no unfinished backup is removed: {reason}");
        return;
    }
    let Ok(entries) = target.folder.entries() else {
        return;
    };
    for (name, kind) in entries.into_iter().take(MAX_FOLDER_ENTRIES) {
        let Some(base) = name.to_str().and_then(|n| n.strip_suffix(IN_PROGRESS)) else {
            continue;
        };
        if kind != Kind::Folder
            || !config::parse_folder_name(base).is_some_and(|(s, _)| s == schedule)
        {
            continue;
        }
        let path = target.path.join(&name);
        let Ok(folder) = target.folder.open(&name) else {
            continue;
        };
        match read_marker(&folder) {
            None => warn!(
                "Schedule {schedule:?}: left the unfinished backup {} alone: it has no {MARKER} \
                 this server wrote",
                path.display()
            ),
            Some(marked) if !abandoned(marked, me) => info!(
                "Schedule {schedule:?}: left the unfinished backup {} alone: process {} still \
                 runs",
                path.display(),
                marked.pid
            ),
            Some(_) if protected.lies_in(&folder).unwrap_or(true) => error!(
                "Schedule {schedule:?}: the unfinished backup {} holds a file the server hosts, \
                 or its container data: not removed",
                path.display()
            ),
            Some(_) => {
                backups.before_removal();
                match target.folder.remove(&name, folder) {
                    Ok(()) => warn!(
                        "Schedule {schedule:?}: removed the unfinished backup {} an earlier run \
                         left",
                        path.display()
                    ),
                    Err(e) => warn!(
                        "Schedule {schedule:?}: cannot remove the unfinished backup {}: {e}",
                        path.display()
                    ),
                }
            }
        }
    }
}

/// Copy `hosted`'s externally stored container data, the folder
/// `RC_Data_FMS/<file name>` beside it, to the same place in `into`, the
/// folder of its copy, as FileMaker Server does (re 2.1). Every file is
/// copied, also one unchanged since the last backup, which FileMaker links
/// (re 2.3, #493). Only a missing folder means the file has none: a folder
/// that is a link (OpenMaker reads container data only through real
/// folders, `scripts::host::files`), or that cannot be read, fails the
/// backup rather than leave it without the data. Links and special files
/// below it are left out and logged.
fn back_up_container_data(
    hosted: &Hosted,
    into: &Folder,
    stop: &dyn Fn() -> bool,
    copied: &mut dyn FnMut(u64),
) -> Result<(), CopyError> {
    let Some(stem) = hosted.path.file_stem() else {
        return Ok(());
    };
    let copied_files = fs::copy_tree_beneath(
        home_of(&hosted.path),
        &[OsStr::new(CONTAINER_FOLDER), stem],
        into,
        stop,
        copied,
    )?;
    if let Some(files) = copied_files.filter(|files| *files > 0) {
        info!(
            "Backup of {:?}: {files} container file(s) copied",
            shown_name(hosted)
        );
    }
    Ok(())
}
