//! Schedules: FileMaker Server's "FileMaker Script" and "Backup" schedules
//! (Claris FileMaker Server Help, "Running FileMaker scripts", "Specifying
//! schedule details", "Examples of repeating schedules"; script-runtime.md
//! "Implementation notes (schedules)"; docs/re/backups.md).
//!
//! A [`Schedule`] runs its [`ScheduleTask`] on its [`Repeat`] while it is
//! enabled: a script ([`ScriptTask`]) or a backup ([`BackupTask`], run by
//! `crate::backups`, which describes it).
//!
//! A script task runs a script of a hosted file, named by its name, as an
//! account, with an optional text parameter. Each run is a Perform Script
//! on Server session of its own (`crate::scripts`): the same window, rules,
//! privileges (the account's), limits, abort and shutdown, with the
//! schedule's time limit added when
//! it ends runs at their time limit. Runs of different schedules execute
//! at the same time (FileMaker's second task queue); a start that comes
//! while the schedule's last run still executes is skipped, as is every
//! start the server was down for. The scheduler follows a [`Clock`]: the
//! server's own ([`SystemClock`]) or one a test moves ([`ManualClock`]).
//!
//! Each schedule keeps its last run (error, result, how it ended), which
//! [`crate::ServerHandle::schedules`] lists along with the next run time;
//! [`crate::ServerHandle::run_schedule`] runs one now. Shutdown stops the
//! scheduler before it aborts the executing scripts, so no schedule starts
//! during shutdown and shutdown waits for the runs it aborted.
//!
//! The event log has FileMaker's lines where Claris gives their text (a
//! backup's are in `crate::backups`):
//! `Schedule "N" has started FileMaker script "S".` when a run starts,
//! `Schedule "N" has exceeded its time limit.` (a warning: the run goes
//! on) and `Schedule "N" aborted; time limit exceeded.` (an error: the run
//! is ended), and the step errors as for every server script; each run
//! then ends with one line of OpenMaker's own wording: how it completed,
//! or why it did not start or was aborted.

mod calendar;
mod clock;
mod file;

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jiff::{Timestamp, Zoned};
use log::{error, info, warn};

pub use calendar::{Days, Repeat, Times};
pub use clock::{Clock, ClockWaker, ManualClock, SystemClock};
pub use file::{parse_schedules, ScheduleFileError};

use crate::backups::{self, Backups};
use crate::connection::Shared;
use crate::engine::ReadError;
use crate::eventlog::{Event, ScheduleAbort};
use crate::hosting::{Eligibility, FileAccount, Hosted};
use crate::scripts::{self, AbortReason, ScriptRef, Slot};

/// "Unable to open file": the run's file is not hosted (script oracle
/// s36: a file that cannot be opened).
const FILE_UNOPENABLE: i32 = 802;
/// "Invalid user account and/or password": the account cannot log in to
/// the file; also a disabled account (script oracle s36).
const LOGIN_FAILED: i32 = 212;
/// Why a run whose file's accounts could not be read is aborted, with
/// [`FILE_UNOPENABLE`] (OpenMaker's choice).
const UNREADABLE_ACCOUNTS: &str = "its file's accounts could not be read";
/// "Unknown error": the run's thread failed (a bug), or could not start.
const UNKNOWN_ERROR: i32 = -1;
/// The server is shutting down (FileMaker's 820, as for scripts).
const SHUTTING_DOWN: i32 = 820;

/// The longest the scheduler sleeps before it looks at its clock again,
/// so that a change of the system time is noticed.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// A schedule: what it does, when, and whether it runs on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// Its name, unique among the server's schedules (without case).
    pub name: String,
    /// What a run does.
    pub task: ScheduleTask,
    /// When it runs.
    pub repeat: Repeat,
    /// Whether it runs on its own.
    pub enabled: bool,
}

/// What a schedule does when it runs (FileMaker's schedule types).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleTask {
    /// Run a script of a hosted file ("FileMaker Script").
    Script(ScriptTask),
    /// Back up hosted files ("Backup").
    Backup(BackupTask),
}

/// A FileMaker script schedule's task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTask {
    /// The hosted file, by the name clients open it by (without case).
    pub file: String,
    /// The script, by name: matched without case, the first of that name
    /// in the file's script list (script oracle s36).
    pub script: String,
    /// The account the script runs as, with its privileges: an account of
    /// the file that may log in to it (`hosting::Eligibility`). Its name
    /// as written here is `Get ( AccountName )` (script oracle s36). No
    /// password is given: the schedules come from the server's
    /// administrator, who has the files themselves (FileMaker Server keeps
    /// the account's password with each schedule and logs in with it).
    pub account: String,
    /// The script parameter, as text ("5 + 5" is the text, not 10); empty
    /// for none.
    pub parameter: String,
    /// How long a run may take before the event log says so.
    pub time_limit: Option<Duration>,
    /// End a run at [`ScriptTask::time_limit`] (FileMaker's "End the
    /// schedule if the timeout is reached"); without it, a run past its
    /// time limit is logged as a warning and goes on.
    pub end_on_time_limit: bool,
}

/// A backup schedule's task (re/backups.md 6; the Admin API's
/// `backupType`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupTask {
    /// Which hosted files a run copies.
    pub files: BackupFiles,
    /// The folder its backups go in (FileMaker's target folder); the
    /// server's backup folder when absent ([`crate::Server::with_backup_folder`]).
    pub folder: Option<PathBuf>,
    /// How many of its backups that are not preserved it keeps, 1 to
    /// [`backups::MAX_KEEP`]: after a run, the oldest past this are deleted.
    pub keep: u8,
}

/// The hosted files a backup schedule copies (FileMaker's `ALL_DB`,
/// `DB_IN_FOLDER` and `SINGLE_DB`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupFiles {
    /// Every hosted file.
    All,
    /// The files in this folder or a folder below it.
    Folder(PathBuf),
    /// The file clients open by this name (without case).
    File(String),
}

impl Schedule {
    /// Why this schedule cannot run as given, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("a schedule needs a name".into());
        }
        match &self.task {
            ScheduleTask::Script(task) => task.check()?,
            ScheduleTask::Backup(task) => {
                backups::check_schedule_name(&self.name)?;
                task.check()?
            }
        }
        self.repeat.check()
    }
}

impl ScriptTask {
    /// Why this task cannot run as given, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        for (what, value) in [
            ("file", &self.file),
            ("script", &self.script),
            ("account", &self.account),
        ] {
            if value.is_empty() {
                return Err(format!("a schedule needs a {what}"));
            }
        }
        if self.time_limit.is_some_and(|t| t.is_zero()) {
            return Err("the time limit must be longer than zero".into());
        }
        Ok(())
    }
}

impl BackupTask {
    /// Why this task cannot run as given, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        if !(1..=backups::MAX_KEEP).contains(&self.keep) {
            return Err(format!(
                "a backup schedule keeps 1 to {} backups",
                backups::MAX_KEEP
            ));
        }
        match &self.files {
            BackupFiles::All => {}
            BackupFiles::Folder(folder) if folder.as_os_str().is_empty() => {
                return Err("a backup of a folder needs the folder".into())
            }
            BackupFiles::File(name) if name.is_empty() => {
                return Err("a backup of a file needs the file's name".into())
            }
            BackupFiles::Folder(_) | BackupFiles::File(_) => {}
        }
        if self
            .folder
            .as_ref()
            .is_some_and(|f| f.as_os_str().is_empty())
        {
            return Err("the backup folder is empty".into());
        }
        Ok(())
    }
}

/// Errors defining or operating schedules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// A schedule that cannot run as given.
    Invalid { schedule: String, reason: String },
    /// Two schedules of the same name.
    Duplicate(String),
    /// No schedule of that name.
    NotFound(String),
    /// The schedule's last run has not ended.
    Running(String),
    /// The schedule is not a backup schedule.
    NotBackup(String),
    /// A backup folder that may not hold backups: it holds a hosted file
    /// or its container data, or lies in a hosted file's container data.
    BackupFolder { folder: PathBuf, reason: String },
    /// The server is shutting down.
    ShuttingDown,
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScheduleError::Invalid { schedule, reason } => {
                write!(f, "schedule {schedule:?}: {reason}")
            }
            ScheduleError::Duplicate(name) => write!(f, "schedule {name:?} is defined twice"),
            ScheduleError::NotFound(name) => write!(f, "no schedule {name:?}"),
            ScheduleError::Running(name) => write!(f, "schedule {name:?} is running"),
            ScheduleError::NotBackup(name) => write!(f, "schedule {name:?} is not a backup"),
            ScheduleError::BackupFolder { reason, .. } => write!(f, "{reason}"),
            ScheduleError::ShuttingDown => write!(f, "the server is shutting down"),
        }
    }
}

impl std::error::Error for ScheduleError {}

/// A schedule's run that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastRun {
    /// When it started and ended, by the scheduler's clock.
    pub started: Zoned,
    pub ended: Zoned,
    /// Started by [`crate::ServerHandle::run_schedule`] or
    /// [`crate::ServerHandle::back_up_now`].
    pub manual: bool,
    /// Whether the script ran, or the backup began copying; when not,
    /// [`LastRun::error`] says why: 802 the file is not hosted (a backup:
    /// no hosted file is chosen), 212 the account cannot log in, 812 the
    /// server runs as many scripts as it may, 820 it is shutting down.
    pub ran: bool,
    /// The script's final `Get ( LastError )` (4 after an unknown step
    /// ended it, 104 for a missing script, 1 when aborted after no error),
    /// as Perform Script on Server returns it; -1 when the run failed
    /// inside the server. A backup's: 0, 1 when it was cancelled, or why
    /// it failed (`crate::backups`).
    pub error: i32,
    /// The top script's Exit Script result as text; a backup's folder
    /// name once it is complete.
    pub result: String,
    /// Why the run was aborted, if it was: a backup cancelled by the
    /// administrator ([`AbortReason::Admin`]) or by shutdown.
    pub aborted: Option<AbortReason>,
    /// It ran past the schedule's time limit.
    pub exceeded_time_limit: bool,
}

impl LastRun {
    /// FileMaker's "Succeeded": the script ran to its end with error 0
    /// (OpenMaker's reading; Claris shows Succeeded or Failed without
    /// saying when).
    pub fn succeeded(&self) -> bool {
        self.ran && self.error == 0 && self.aborted.is_none()
    }
}

/// A schedule as the administrator sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleStatus {
    pub schedule: Schedule,
    /// When it runs next on its own: none while it is disabled, after its
    /// end, or before the server serves.
    pub next_run: Option<Zoned>,
    /// When its executing run started.
    pub running_since: Option<Zoned>,
    pub last_run: Option<LastRun>,
    /// Its runs that ended (also those that did not start).
    pub runs: u64,
    /// Starts skipped because the last run had not ended.
    pub skipped: u64,
}

/// The scheduler: every schedule's state and the thread that starts their
/// runs on time.
pub(crate) struct Scheduler {
    state: Mutex<State>,
    /// Signaled when the scheduler should look at its clock again, when it
    /// has, and when a run ends.
    changed: Condvar,
    /// The backups that backup schedules' runs make. Its lock is taken
    /// after the state's, never before.
    backups: Arc<Backups>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler").finish_non_exhaustive()
    }
}

struct State {
    clock: Arc<dyn Clock>,
    entries: Vec<Entry>,
    /// The scheduler's thread, while it runs.
    thread: Option<JoinHandle<()>>,
    started: bool,
    stopped: bool,
    /// Something changed since the thread last looked at its clock.
    rung: bool,
    /// The clock time the thread last started the due runs at.
    seen: Option<Timestamp>,
}

struct Entry {
    schedule: Schedule,
    next: Option<Zoned>,
    running: Option<Zoned>,
    last: Option<LastRun>,
    runs: u64,
    skipped: u64,
}

impl Scheduler {
    pub fn new() -> Arc<Self> {
        Arc::new(Scheduler {
            state: Mutex::new(State {
                clock: Arc::new(SystemClock),
                entries: Vec::new(),
                thread: None,
                started: false,
                stopped: false,
                rung: false,
                seen: None,
            }),
            changed: Condvar::new(),
            backups: Arc::new(Backups::default()),
        })
    }

    /// The backups backup schedules make.
    pub fn backups(&self) -> &Arc<Backups> {
        &self.backups
    }

    /// The state. A run's thread that panicked never holds the lock (it
    /// records its end after the run), and every change leaves the state
    /// whole, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        self.lock().clock = clock;
    }

    /// Replace the schedules, after checking them (each valid, names
    /// unique without case). A file or account the server does not know
    /// is only logged: a run then fails with 802 or 212.
    pub fn install(&self, schedules: Vec<Schedule>, shared: &Shared) -> Result<(), ScheduleError> {
        for (i, schedule) in schedules.iter().enumerate() {
            schedule.check().map_err(|reason| ScheduleError::Invalid {
                schedule: schedule.name.clone(),
                reason,
            })?;
            let name = schedule.name.to_lowercase();
            if schedules[..i].iter().any(|s| s.name.to_lowercase() == name) {
                return Err(ScheduleError::Duplicate(schedule.name.clone()));
            }
            match &schedule.task {
                ScheduleTask::Script(task) => match shared.hosting.by_name(&task.file) {
                    None => warn!(
                        "schedule {:?}: the server does not host {:?}",
                        schedule.name, task.file
                    ),
                    Some(hosted) => match log_in(shared, &hosted, &task.account) {
                        Ok(Some(_)) => {}
                        Ok(None) => warn!(
                            "schedule {:?}: the account {:?} cannot log in to {:?}",
                            schedule.name, task.account, task.file
                        ),
                        Err(e) => warn!(
                            "schedule {:?}: the accounts of {:?} could not be read: {e}",
                            schedule.name, task.file
                        ),
                    },
                },
                ScheduleTask::Backup(task) => {
                    let folder = task.folder.clone().or_else(|| self.backups.folder());
                    if let Some(folder) = folder {
                        backups::check_folder(shared, &folder).map_err(|reason| {
                            ScheduleError::Invalid {
                                schedule: schedule.name.clone(),
                                reason,
                            }
                        })?;
                    }
                    match backups::chosen(shared, &task.files) {
                        Ok(files) if files.is_empty() => warn!(
                            "schedule {:?}: no hosted file is in its backup",
                            schedule.name
                        ),
                        Ok(_) => {}
                        // A file closed while it was looked at; each run
                        // chooses again.
                        Err(why) => warn!("schedule {:?}: {why}", schedule.name),
                    }
                }
            }
        }
        let mut state = self.lock();
        let now = state.clock.now();
        let started = state.started;
        state.entries = schedules
            .into_iter()
            .map(|schedule| Entry {
                next: next_run(&schedule, &now).filter(|_| started),
                schedule,
                running: None,
                last: None,
                runs: 0,
                skipped: 0,
            })
            .collect();
        drop(state);
        self.ring();
        Ok(())
    }

    /// Start the thread that runs the schedules on time (once; not after
    /// [`Scheduler::stop`]).
    pub fn start(self: &Arc<Self>, shared: &Arc<Shared>) {
        let clock = {
            let mut state = self.lock();
            if state.started || state.stopped {
                return;
            }
            state.started = true;
            let now = state.clock.now();
            for entry in &mut state.entries {
                entry.next = next_run(&entry.schedule, &now);
                match &entry.next {
                    Some(next) => info!(
                        "Schedule {:?} runs next at {}",
                        entry.schedule.name,
                        next.datetime()
                    ),
                    None if entry.schedule.enabled => {
                        info!("Schedule {:?} has no run ahead", entry.schedule.name)
                    }
                    None => {}
                }
            }
            let scheduler = self.clone();
            let shared = shared.clone();
            let thread = std::thread::Builder::new()
                .name("schedules".into())
                .spawn(move || scheduler.serve(&shared));
            match thread {
                Ok(thread) => state.thread = Some(thread),
                Err(e) => {
                    error!("schedules cannot run: {e}");
                    return;
                }
            }
            state.clock.clone()
        };
        // Outside the state's lock: a manual clock wakes the scheduler
        // under its own.
        clock.watch(ClockWaker(Arc::downgrade(self)));
    }

    /// Stop starting runs, for good, and end the scheduler's thread. Runs
    /// already executing go on (shutdown aborts them with every script).
    pub fn stop(&self) {
        let thread = {
            let mut state = self.lock();
            state.stopped = true;
            for entry in &mut state.entries {
                entry.next = None;
            }
            state.thread.take()
        };
        self.changed.notify_all();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    /// Have the thread look at its clock again.
    pub fn ring(&self) {
        self.lock().rung = true;
        self.changed.notify_all();
    }

    pub fn statuses(&self) -> Vec<ScheduleStatus> {
        self.lock()
            .entries
            .iter()
            .map(|e| ScheduleStatus {
                schedule: e.schedule.clone(),
                next_run: e.next.clone(),
                running_since: e.running.clone(),
                last_run: e.last.clone(),
                runs: e.runs,
                skipped: e.skipped,
            })
            .collect()
    }

    /// Run schedule `name` now, also when it is disabled, recording the
    /// administrator's run; its next run on its own stays as it was. A
    /// backup schedule's run is an on-demand backup.
    pub fn run_now(
        self: &Arc<Self>,
        shared: &Arc<Shared>,
        name: &str,
    ) -> Result<(), ScheduleError> {
        self.start_now(shared, name, false)
    }

    /// Run backup schedule `name` now: an on-demand backup.
    pub fn back_up_now(
        self: &Arc<Self>,
        shared: &Arc<Shared>,
        name: &str,
    ) -> Result<(), ScheduleError> {
        self.start_now(shared, name, true)
    }

    /// The backup schedules' names and tasks.
    pub fn backup_schedules(&self) -> Vec<(String, BackupTask)> {
        self.lock()
            .entries
            .iter()
            .filter_map(|e| match &e.schedule.task {
                ScheduleTask::Backup(task) => Some((e.schedule.name.clone(), task.clone())),
                ScheduleTask::Script(_) => None,
            })
            .collect()
    }

    /// [`Scheduler::run_now`], only for a backup schedule when
    /// `backup_only`.
    fn start_now(
        self: &Arc<Self>,
        shared: &Arc<Shared>,
        name: &str,
        backup_only: bool,
    ) -> Result<(), ScheduleError> {
        let mut state = self.lock();
        if state.stopped {
            return Err(ScheduleError::ShuttingDown);
        }
        let index = state.index(name)?;
        if backup_only && !matches!(state.entries[index].schedule.task, ScheduleTask::Backup(_)) {
            return Err(ScheduleError::NotBackup(
                state.entries[index].schedule.name.clone(),
            ));
        }
        if state.entries[index].running.is_some() {
            return Err(ScheduleError::Running(
                state.entries[index].schedule.name.clone(),
            ));
        }
        let now = state.clock.now();
        let name = &state.entries[index].schedule.name;
        shared.events.record(Event::schedule_run_now(name));
        self.launch(&mut state, index, shared, now, true);
        Ok(())
    }

    /// Enable or disable schedule `name`; returns its name as it is
    /// written.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<String, ScheduleError> {
        let mut state = self.lock();
        let index = state.index(name)?;
        let now = state.clock.now();
        let run_on_time = state.started && !state.stopped;
        let entry = &mut state.entries[index];
        entry.schedule.enabled = enabled;
        entry.next = next_run(&entry.schedule, &now).filter(|_| run_on_time);
        let name = entry.schedule.name.clone();
        drop(state);
        self.ring();
        Ok(name)
    }

    /// Wait, at most `timeout`, until no run executes and, with a clock
    /// that moves only when told to, the scheduler serves and has started
    /// every run due at its time. Returns whether that happened.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            let now = state.clock.now();
            let caught_up = state.stopped
                || state.clock.until(&now).is_some()
                || (state.started && !state.rung && state.seen == Some(now.timestamp()));
            if caught_up && state.entries.iter().all(|e| e.running.is_none()) {
                return true;
            }
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
    }

    /// The scheduler's thread: start the runs that are due, then sleep
    /// until the next is due or something changes.
    fn serve(self: Arc<Self>, shared: &Arc<Shared>) {
        let mut state = self.lock();
        loop {
            if state.stopped {
                return;
            }
            state.rung = false;
            let now = state.clock.now();
            self.start_due(&mut state, shared, &now);
            state.seen = Some(now.timestamp());
            self.changed.notify_all();
            let wait = state
                .entries
                .iter()
                .filter_map(|e| e.next.as_ref())
                .min_by_key(|next| next.timestamp())
                .and_then(|next| state.clock.until(next))
                .unwrap_or(MAX_SLEEP)
                .clamp(Duration::from_millis(1), MAX_SLEEP);
            state = self
                .changed
                .wait_timeout_while(state, wait, |s| !s.rung && !s.stopped)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Start every enabled schedule whose next run is due at `now`, or
    /// skip it when its last run has not ended; then move its next run
    /// past `now` (missed runs are not made up).
    fn start_due(self: &Arc<Self>, state: &mut State, shared: &Arc<Shared>, now: &Zoned) {
        for index in 0..state.entries.len() {
            let entry = &mut state.entries[index];
            if !entry
                .next
                .as_ref()
                .is_some_and(|next| next.timestamp() <= now.timestamp())
            {
                continue;
            }
            entry.next = next_run(&entry.schedule, now);
            if entry.running.is_some() {
                entry.skipped += 1;
                warn!(
                    "Schedule {:?} skipped its run at {}: its last run has not ended",
                    entry.schedule.name,
                    now.datetime()
                );
                shared
                    .events
                    .record(Event::schedule_skipped(&entry.schedule.name));
                continue;
            }
            self.launch(state, index, shared, now.clone(), false);
        }
    }

    /// Start a run of entry `index` on a thread of its own. Its script
    /// slot is taken here, under the state's lock, so a run is either
    /// refused by a shutdown that began or aborted and waited for by it.
    fn launch(
        self: &Arc<Self>,
        state: &mut State,
        index: usize,
        shared: &Arc<Shared>,
        started: Zoned,
        manual: bool,
    ) {
        let entry = &mut state.entries[index];
        let schedule = entry.schedule.clone();
        let refused = {
            let started = started.clone();
            move |error: i32| LastRun {
                started: started.clone(),
                ended: started.clone(),
                manual,
                ran: false,
                error,
                result: String::new(),
                aborted: None,
                exceeded_time_limit: false,
            }
        };
        let job = match &schedule.task {
            ScheduleTask::Script(task) => match Slot::take(
                shared,
                &task.file,
                task.script.clone(),
                &task.account,
                task.time_limit.filter(|_| task.end_on_time_limit),
            ) {
                Ok(slot) => Job::Script(task.clone(), slot),
                Err(code) => {
                    let why = match code {
                        820 => "the server is shutting down",
                        _ => "the server runs as many scripts as it may",
                    };
                    error!(
                        "Schedule {:?} aborted; {why} (error {code}).",
                        schedule.name
                    );
                    let why = ScheduleAbort::Refused(why, i32::from(code));
                    shared
                        .events
                        .record(Event::schedule_aborted(&schedule.name, why));
                    entry.last = Some(refused(i32::from(code)));
                    entry.runs += 1;
                    return;
                }
            },
            ScheduleTask::Backup(task) => {
                match self.backups.enter(&schedule.name, started.clone(), manual) {
                    Some(ticket) => Job::Backup(task.clone(), ticket),
                    None => {
                        error!(
                            "Schedule {:?} aborted; the server is shutting down (error \
                             {SHUTTING_DOWN}).",
                            schedule.name
                        );
                        entry.last = Some(refused(SHUTTING_DOWN));
                        entry.runs += 1;
                        return;
                    }
                }
            }
        };
        entry.running = Some(started.clone());
        let scheduler = self.clone();
        let events = shared.events.clone();
        let shared = shared.clone();
        let name = schedule.name.clone();
        let thread = std::thread::Builder::new()
            .name(format!("schedule {}", schedule.name))
            .spawn(move || {
                let ended = std::panic::catch_unwind(AssertUnwindSafe(|| match job {
                    Job::Script(task, slot) => run(&shared, &schedule.name, &task, slot),
                    Job::Backup(task, ticket) => {
                        backups::run(&shared, &scheduler.backups, ticket, &task).into()
                    }
                }))
                .unwrap_or_else(|_| {
                    error!(
                        "Schedule {:?} aborted; the server failed running it.",
                        schedule.name
                    );
                    let why = ScheduleAbort::Refused("the server failed running it", UNKNOWN_ERROR);
                    shared
                        .events
                        .record(Event::schedule_aborted(&schedule.name, why));
                    Ended::failed(UNKNOWN_ERROR)
                });
                scheduler.finish(&shared, &schedule.name, started, manual, ended);
            });
        if let Err(e) = thread {
            error!("Schedule {name:?} aborted; it cannot start: {e}.");
            let why = ScheduleAbort::Refused("it cannot start", UNKNOWN_ERROR);
            events.record(Event::schedule_aborted(&name, why));
            let entry = &mut state.entries[index];
            entry.running = None;
            entry.last = Some(refused(UNKNOWN_ERROR));
            entry.runs += 1;
        }
    }

    /// Record how a run of schedule `name` ended, and in the event log when
    /// it runs next on its own, before the run is seen to end.
    fn finish(&self, shared: &Shared, name: &str, started: Zoned, manual: bool, ended: Ended) {
        let mut state = self.lock();
        let now = state.clock.now();
        let mut next = None;
        if let Ok(index) = state.index(name) {
            let entry = &mut state.entries[index];
            entry.running = None;
            entry.runs += 1;
            entry.last = Some(LastRun {
                started,
                ended: now,
                manual,
                ran: ended.ran,
                error: ended.error,
                result: ended.result,
                aborted: ended.aborted,
                exceeded_time_limit: ended.exceeded_time_limit,
            });
            next = entry
                .next
                .clone()
                .map(|next| (entry.schedule.name.clone(), next));
        }
        if let Some((name, next)) = next {
            shared.events.record(Event::schedule_next(&name, &next));
        }
        // Publish completion only after the run's last event is queued.
        drop(state);
        self.changed.notify_all();
    }
}

impl State {
    fn index(&self, name: &str) -> Result<usize, ScheduleError> {
        let lower = name.to_lowercase();
        self.entries
            .iter()
            .position(|e| e.schedule.name.to_lowercase() == lower)
            .ok_or_else(|| ScheduleError::NotFound(name.to_string()))
    }
}

/// When `schedule` runs next after `now`, if it is enabled.
fn next_run(schedule: &Schedule, now: &Zoned) -> Option<Zoned> {
    schedule
        .enabled
        .then(|| schedule.repeat.next_after(now))
        .flatten()
}

/// `hosted`'s account `account` as it is now, if it may log in today, as a
/// client's login with its password could
/// (`hosting::FileAccount::server_eligibility`): not an account whose
/// stored password cannot be checked, which no client signs in as; nor an
/// inactive account (212, script oracle s36), one whose password must be
/// changed, or one in a set the server does not enforce. A run keeps the
/// account as it was at this login, as a client's session does. The
/// account is looked up in the accounts of a reading of the file
/// (`engine::Read::derived`); accounts that could not be read are an
/// error.
fn log_in(
    shared: &Shared,
    hosted: &Hosted,
    account: &str,
) -> Result<Option<Arc<FileAccount>>, ReadError> {
    let found = shared
        .engine
        .read(hosted)
        .serve_with_accounts(|read| read.derived.account(account))?;
    Ok(found.filter(|account| {
        let custom_sets = shared.custom_sets.load(Ordering::Relaxed);
        account.password.is_ok()
            && account.server_eligibility(jiff::Zoned::now().date(), custom_sets)
                == Eligibility::Eligible
    }))
}

/// What a run's thread does, with what it took to start.
enum Job {
    Script(ScriptTask, Slot),
    Backup(BackupTask, backups::Ticket),
}

/// How a run's thread ended.
struct Ended {
    ran: bool,
    error: i32,
    result: String,
    aborted: Option<AbortReason>,
    exceeded_time_limit: bool,
}

impl From<backups::Outcome> for Ended {
    fn from(outcome: backups::Outcome) -> Self {
        Ended {
            ran: outcome.copied,
            error: outcome.error,
            result: outcome.folder.unwrap_or_default(),
            aborted: outcome.cancelled,
            exceeded_time_limit: false,
        }
    }
}

impl Ended {
    fn failed(error: i32) -> Self {
        Ended {
            ran: false,
            error,
            result: String::new(),
            aborted: None,
            exceeded_time_limit: false,
        }
    }
}

/// One run of `schedule`, as fmsased runs a schedule
/// (`SMSession::RunScriptAndClose`): the file (802 when it is not hosted),
/// the account's login (212), then the script in a session of its own
/// (112 without a window, 104 without a script of that name, else its
/// final error and result).
fn run(shared: &Arc<Shared>, name: &str, schedule: &ScriptTask, slot: Slot) -> Ended {
    let events = &shared.events;
    events.record(Event::schedule_running(name));
    let Some(hosted) = shared.hosting.by_name(&schedule.file) else {
        error!(
            "Schedule {name:?} aborted; the file {:?} is not hosted (error {FILE_UNOPENABLE}).",
            schedule.file
        );
        let why = ScheduleAbort::NotHosted(&schedule.file);
        events.record(Event::schedule_aborted(name, why));
        return Ended::failed(FILE_UNOPENABLE);
    };
    let account = match log_in(shared, &hosted, &schedule.account) {
        Ok(Some(account)) => account,
        Ok(None) => {
            error!(
                "Schedule {name:?} aborted; the account {:?} cannot log in to {:?} (error \
                 {LOGIN_FAILED}).",
                schedule.account, hosted.name
            );
            events.record(Event::schedule_aborted(name, ScheduleAbort::Account));
            return Ended::failed(LOGIN_FAILED);
        }
        // As a client's login to it is answered (`LOGIN_UNREADABLE_FILE`):
        // not as an account that cannot log in. OpenMaker's choice.
        Err(e) => {
            error!(
                "Schedule {name:?} aborted; the accounts of {:?} could not be read ({e}) \
                 (error {FILE_UNOPENABLE}).",
                hosted.name
            );
            let why = ScheduleAbort::Refused(UNREADABLE_ACCOUNTS, FILE_UNOPENABLE);
            events.record(Event::schedule_aborted(name, why));
            return Ended::failed(FILE_UNOPENABLE);
        }
    };
    info!(
        "Schedule {name:?} has started FileMaker script {:?}.",
        schedule.script
    );
    events.record(Event::schedule_script(name, &schedule.script));
    // A time limit that does not end the run: a warning once it passes.
    let exceeded = Arc::new(AtomicBool::new(false));
    let (done, ending) = mpsc::channel::<()>();
    let watch = schedule
        .time_limit
        .filter(|_| !schedule.end_on_time_limit)
        .and_then(|limit| {
            let exceeded = exceeded.clone();
            let name = name.to_owned();
            let events = events.clone();
            std::thread::Builder::new()
                .name(format!("schedule {name} time limit"))
                .spawn(move || {
                    if let Err(RecvTimeoutError::Timeout) = ending.recv_timeout(limit) {
                        exceeded.store(true, Ordering::Release);
                        warn!("Schedule {name:?} has exceeded its time limit.");
                        events.record(Event::schedule_over_time(&name));
                    }
                })
                .ok()
        });
    let outcome = scripts::run(
        shared,
        &hosted,
        // `Get ( AccountName )` is the name as the schedule has it.
        &schedule.account,
        &account.key,
        account.privileges(),
        account.access.clone(),
        ScriptRef::Name(&schedule.script),
        &schedule.parameter,
        &slot,
    );
    drop(done);
    if let Some(watch) = watch {
        let _ = watch.join();
    }
    let aborted = slot.aborted();
    drop(slot);
    let error = i32::from(outcome.error);
    let own_limit = schedule
        .time_limit
        .filter(|_| schedule.end_on_time_limit)
        .is_some_and(|limit| limit <= shared.scripts.limits().duration);
    match aborted {
        Some(AbortReason::TimeLimit) if own_limit => {
            error!("Schedule {name:?} aborted; time limit exceeded.");
            events.record(Event::schedule_aborted(name, ScheduleAbort::OwnTimeLimit));
        }
        Some(reason) => {
            warn!(
                "Schedule {name:?} aborted; {} (error {error}).",
                match reason {
                    AbortReason::Admin => "an administrator aborted its script",
                    AbortReason::FileClosed => "its file was closed",
                    AbortReason::Shutdown => "the server is shutting down",
                    AbortReason::StepLimit => "its script reached the server's step limit",
                    AbortReason::TimeLimit => "its script reached the server's time limit",
                }
            );
            let why = ScheduleAbort::Script(reason, error);
            events.record(Event::schedule_aborted(name, why));
        }
        None if error == 0 => info!("Schedule {name:?} completed (error 0)."),
        None => warn!("Schedule {name:?} completed (error {error})."),
    }
    if aborted.is_none() {
        events.record(Event::schedule_completed(name, error));
    }
    Ended {
        ran: true,
        error,
        result: outcome.result,
        aborted,
        exceeded_time_limit: exceeded.load(Ordering::Acquire)
            || (aborted == Some(AbortReason::TimeLimit) && own_limit),
    }
}
