//! The store on one server (`docs/server-storage.md` sections 6.2, 6.5,
//! 6.6, 6.7 and 6.12): one store folder, opened by one process, joining
//! `state`, the log ([`crate::log`]), the page file ([`crate::pagefile`])
//! and applying entries ([`crate::apply`]). The order of every write, and
//! what a crash at each step leaves, are section 6 of
//! `docs/server-storage-format.md`.
//!
//! **Driven by its caller.** Nothing here runs on its own: no thread, no
//! timer, no background work. Every step is a call, so the simulated disk
//! can stop the store anywhere. The threaded front (an applying thread,
//! group commit across callers, background checkpoints, the limits of
//! section 6.11) is not built here.
//!
//! **Writing** (section 6.5). The caller brings an entry's file and
//! payload ([`Payload`]); the store gives it its index and term and:
//!
//! 1. asks the log whether it would take the entry ([`Log::ready_for`]),
//!    so that nothing the log refuses comes after the next step;
//! 2. reserves pages for it and applies it to the working state in its one
//!    unit of work ([`apply::apply`]): a reservation that runs short is
//!    extended and the unit begun again; a full disk refuses the entry
//!    with nothing changed and nothing appended;
//! 3. pins the version the committed unit made, the newest then
//!    ([`PageFile::pin_newest`]);
//! 4. appends it to the log.
//!
//! [`Store::flush`] writes the open batch and advances the marker, then
//! **publishes** every entry it made durable, in order: their versions
//! become the one views are taken of, and their notices and replies go
//! back to the caller. Several entries appended before one flush are all
//! published by it, or none. If anything fails after a unit committed (the
//! pin, the append, the flush, the marker), what is in memory is ahead of
//! what is durable and cannot be taken back: the store **stops**, every
//! later call is refused, and the caller opens it again, which rebuilds it
//! from what the durable log says.
//!
//! **Reading** (section 6.6). A [`View`] is the roots of a published entry
//! with a pin that keeps its pages; it holds the page file, not the store,
//! so it outlives later writes, checkpoints and the store itself. It never
//! reads the working state.
//!
//! **Checkpoints** (section 6.7) are of the published entry, taken only
//! when nothing is applied past it, so the caller takes them between
//! flushes. After one, the log is retired up to what the oldest kept
//! checkpoint or restore point still needs, never past it.
//!
//! **Opening** is section 6.7's steps with the pieces' own opens: the
//! store folder's lock and `state`; the page file's choice of checkpoint;
//! the log from that checkpoint's entry, checked against the marker; the
//! entries after the checkpoint applied again, each once, through the
//! durable entry, with recovery reservations; then this start's own
//! `session` entry (section 7.3), the first write of every start. Nothing
//! is freed in the log, and nothing reclaimed or checkpointed in the page
//! file, before the replay is done, but for one case: when the pages
//! waiting for a checkpoint pass their budget during the replay, the last
//! entry replayed, which is durable, is checkpointed and the replay goes
//! on. What cannot be opened is reported,
//! typed, with what the administrator can do ([`OpenError`]); the two
//! explicit "accept the loss" calls ([`Store::accept_damage`], which
//! carries out what [`Store::plan_damage`] showed, and
//! [`Store::give_up_unreadable_end`]) are the only way past a damaged log
//! and an unreadable end.
//!
//! **What is given up is recorded first** (section 6.12), in the store's
//! incident journal ([`crate::incident`]), flushed, before it is given up:
//! the page file's damage, which the store gives up by itself; a read of
//! the page file that keeps failing, given up by the administrator's call
//! ([`Store::give_up_pages`]); an accepted loss of the log, with where each
//! file goes; an unreadable end; and the last batch after a copy of
//! `state` passed over, before its mark is settled. One act is one record,
//! which says what was approved and about to be done. Every open that
//! succeeds reports the incidents no administrator has acknowledged
//! ([`Opened::incidents`]); an open that fails after recording one says so
//! ([`OpenError::AfterIncidents`]); [`Store::incidents`] reads the journal
//! and [`Store::acknowledge`] acknowledges everything that read showed (or,
//! with the store closed, [`Store::read_incidents`] and
//! [`Store::acknowledge_incidents`]).
//! The store keeps no restore points of its own: those the page file gives
//! up go with the checkpoint, named by its incident.

#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;

use crate::apply::{self, Applied, Position, StoreTree, TreeId};
use crate::entry::{
    entry_hash, Change, Entry, Group, GroupEvent, Payload, PayloadError, ServerId, SessionEvent,
    Step,
};
use crate::incident::{self, Incident, Journal, JournalError, PlanRecord, What};
use crate::log::{self, CutDone, DamageAccepted, Log, LogError, Recorded, UnreadableEnd};
use crate::page::{Hash, StoreError, TreeName};
use crate::pagefile::{
    self, CheckpointError, Checkpointed, EntryId, PageFile, Pin, PinError, Purpose, Reclaimed,
    RestorePoint, SpaceError,
};
use crate::state::{SlotFault, State, StateError};
use crate::sys::{self, ErrorKind, Fs, Random};
use crate::tree::{self, Root, Tree};

/// How the store is set up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub log: log::Config,
    pub pages: pagefile::Config,
}

/// The pages a reservation starts with, and the least it is extended by
/// when an entry runs short: the store's tree takes a few pages for every
/// entry, a file's tree a few for a small change. A larger entry extends
/// its reservation, doubling it each time.
const FIRST_RESERVATION: u64 = 8;

/// The most dying pages a checkpoint goes on to examine
/// ([`PageFile::reclaim`]).
const RECLAIM_AFTER_CHECKPOINT: usize = 4096;

/// A version of the state the store has published or will publish: the
/// entry, and the pin that keeps its pages.
#[derive(Clone, Debug)]
struct Version {
    position: Position,
    pin: Arc<Pin>,
}

/// An entry applied and appended, not yet published.
#[derive(Debug)]
struct Unpublished {
    version: Version,
    applied: Applied,
}

/// An entry appended: applied to the working state and in the log's open
/// batch. It is published by the flush that makes it durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appended {
    pub position: Position,
    /// How many times its reservation was extended.
    pub extended: u32,
    /// The pages its unit of work wrote, with the fixed room its trees
    /// take in the tables ([`pagefile::Reservation::taken`]).
    pub pages: u64,
}

/// An entry published: durable, in the version views are taken of. Its
/// notices and reply ([`Applied::notices`], [`Applied::reply`]) go out
/// now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub applied: Applied,
}

/// What a checkpoint did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub written: Checkpointed,
    /// The entry it is of: the published one.
    pub entry: Position,
    /// Log segments retired after it.
    pub retired: u64,
    /// Dying pages examined, and freed, after it.
    pub examined: u64,
    pub freed: u64,
}

/// Why a write was not done.
#[derive(Debug)]
pub enum WriteError {
    /// The store stopped after a failure (the reason); open it again.
    Stopped(String),
    /// The payload is beyond a bound; nothing changed.
    Payload(PayloadError),
    /// The entry cannot be right ([`apply::Refusal`]); nothing changed.
    Refused(apply::Refusal),
    /// The disk is full: the entry was refused, nothing was appended, and
    /// the store keeps reading and can checkpoint (section 6.12).
    Full(SpaceError),
    /// Too many pages wait for a checkpoint: flush, checkpoint, then write
    /// again (section 6.11). Nothing changed.
    CheckpointNeeded(SpaceError),
    /// The log would not take the entry (too large, a batch past two
    /// segments, no room for the next segment); nothing changed.
    Log(LogError),
    /// The entry could not be applied for a reason of this server's own (a
    /// page it cannot read); nothing changed.
    Apply(apply::Error),
    /// The call breaks a rule; nothing changed.
    Rule(&'static str),
    /// A restore point's version could not be had: its table of roots the
    /// device failed to read (`PinError::Meta` with a page whose fault is
    /// `Fault::Io`) or whose bytes do not check. Nothing changed, and the
    /// store goes on.
    Unreadable(PinError),
    /// A failure after the entry's unit committed, or of a flush: the store
    /// has stopped (the reason). Its outcome is unknown to the caller:
    /// open the store again, and the durable log says.
    Failed(String),
    /// As [`WriteError::Failed`], the disk having had no room for the
    /// write: free space before the store is opened again.
    NoSpace(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Stopped(why) => {
                write!(f, "the store stopped earlier ({why}); open it again")
            }
            WriteError::Payload(e) => write!(f, "the payload is refused: {e}"),
            WriteError::Refused(r) => write!(f, "the entry is refused: {r}"),
            WriteError::Full(e) => write!(f, "{e}"),
            WriteError::CheckpointNeeded(e) => write!(f, "{e}"),
            WriteError::Log(e) => write!(f, "{e}"),
            WriteError::Apply(e) => write!(f, "{e}"),
            WriteError::Rule(why) => write!(f, "refused: {why}"),
            WriteError::Unreadable(e) => write!(f, "the restore point's version: {e}"),
            WriteError::Failed(why) => write!(
                f,
                "the store stopped: {why}; what was not flushed has an unknown outcome, and \
                 the store must be opened again"
            ),
            WriteError::NoSpace(why) => write!(
                f,
                "the store stopped, the disk having no room: {why}; what was not flushed has \
                 an unknown outcome; free space, then open the store again"
            ),
        }
    }
}

impl std::error::Error for WriteError {}

/// Why a checkpoint was not taken.
#[derive(Debug)]
pub enum CheckpointFailed {
    /// The store stopped earlier.
    Stopped(String),
    /// The call breaks a rule, or the page file refused it; nothing was
    /// written.
    Refused(String),
    /// A write or flush failed: the store has stopped (section 6.12).
    Failed(String),
    /// The disk had no room for the checkpoint: nothing was written, or a
    /// write failed so and the store has stopped ([`Store::stopped`]).
    NoSpace(String),
}

impl fmt::Display for CheckpointFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointFailed::Stopped(why) => {
                write!(f, "the store stopped earlier ({why}); open it again")
            }
            CheckpointFailed::Refused(why) => write!(f, "checkpoint refused: {why}"),
            CheckpointFailed::Failed(why) => write!(f, "the store stopped: {why}"),
            CheckpointFailed::NoSpace(why) => write!(f, "no room on the disk: {why}"),
        }
    }
}

impl std::error::Error for CheckpointFailed {}

/// Why the store could not be opened (section 6.12), with what the
/// administrator can do ([`OpenError::remedy`]). Nothing the open could not
/// do is worked around.
#[derive(Debug)]
pub enum OpenError {
    /// The folder holds no store: no `state`, and nothing of a store but
    /// what a creation cut short left (create one).
    Missing,
    /// The folder holds a store whose `state` is gone: `found` is what of
    /// it is left. The server does not know what it confirmed, and nothing
    /// is created over it (section 6.12, "`state` has no valid copy").
    NoState { found: &'static str },
    /// The folder holds a store already (creation).
    Exists,
    /// `state` has no valid copy, or cannot be read, or is of a newer
    /// format: the server does not know what it confirmed.
    State(StateError),
    /// The page file cannot be opened: no checkpoint can be used, a newer
    /// format, the file missing.
    Pages(pagefile::OpenError),
    /// The log does not hold, whole, every entry through its marker.
    LogDamaged(log::Damage),
    /// The log ends at a place after its marker the device failed to read:
    /// what follows may have been confirmed. `last` is the last entry the
    /// open verified, which giving up names.
    UnreadableEnd {
        end: UnreadableEnd,
        last: log::Position,
    },
    /// The log could not be opened otherwise: a place at or before the
    /// marker the device failed to read, a newer format, a disk failure.
    Log(LogError),
    /// An entry the log holds cannot be applied again, or the state at
    /// entry `index` cannot be read: the store cannot produce the state it
    /// confirmed (a page that does not check, or an entry that cannot be
    /// right).
    Replay { index: u64, error: apply::Error },
    /// The device failed to read a page the replay needed.
    Unreadable(String),
    /// No space to apply an entry the log holds (section 6.11).
    NoSpace(SpaceError),
    /// The disk failed, or the process stopped, during the open.
    Disk(String),
    /// This start's own `session` entry could not be written.
    Start(Box<StartFailed>),
    /// The call breaks a rule.
    Refused(&'static str),
    /// The configuration cannot hold the store: refused before anything
    /// is created or opened.
    Config(String),
    /// An explicit call gave something up, and the open then failed
    /// ([`GaveUpThenFailed`]).
    AfterGivingUp(Box<GaveUpThenFailed>),
    /// The incident journal could not be read (a newer format) or written
    /// (the disk, or full of incidents not acknowledged): nothing that
    /// needed a record was given up.
    Journal(JournalError),
    /// The open recorded incidents ([`Incident`]) and then failed: they are
    /// durable, and the next open reports them too.
    AfterIncidents(Box<IncidentsThenFailed>),
}

/// The incidents an open recorded before it failed, and why it failed.
#[derive(Debug)]
pub struct IncidentsThenFailed {
    pub recorded: Vec<Incident>,
    pub error: OpenError,
}

/// What an explicit call gave up before the open failed, and why it failed
/// (`error`). With [`Recorded::Done`] what was given up stays given up, and
/// the next open starts from it without reporting it again; with
/// [`Recorded::Unknown`] the update of `state` that records it failed part
/// way, and the next open shows whether it holds.
#[derive(Debug)]
pub struct GaveUpThenFailed {
    pub gave_up: GaveUp,
    pub recorded: Recorded,
    pub error: OpenError,
}

impl OpenError {
    fn after(gave_up: GaveUp, recorded: Recorded, error: OpenError) -> OpenError {
        OpenError::AfterGivingUp(Box::new(GaveUpThenFailed {
            gave_up,
            recorded,
            error,
        }))
    }

    /// The incidents this error says the failed open recorded.
    pub fn recorded(&self) -> &[Incident] {
        match self {
            OpenError::AfterIncidents(failed) => &failed.recorded,
            _ => &[],
        }
    }
}

impl OpenError {
    /// What the administrator can do (section 6.12 and 11).
    pub fn remedy(&self) -> &'static str {
        match self {
            OpenError::Missing => "create the store",
            OpenError::NoState { .. } => {
                "restore the store from a backup: the server does not know what it confirmed"
            }
            OpenError::Exists => "open the store",
            OpenError::State(StateError::Disk(_)) | OpenError::Disk(_) => "open the store again",
            OpenError::State(_) => {
                "restore the store from a backup: the server does not know what it confirmed"
            }
            OpenError::Pages(pagefile::OpenError::NoCheckpoint { .. }) => {
                "open the store again (a read the device failed counts as a checkpoint that does \
                 not read back); if none can be read then either, restore it from a backup"
            }
            OpenError::Pages(pagefile::OpenError::Disk(_)) => "open the store again",
            OpenError::Pages(pagefile::OpenError::Unreadable {
                give_up: Some(_), ..
            }) => {
                "open the store again later; if the read keeps failing, give up what cannot be \
                 read with the approval this error carries (Store::give_up_pages)"
            }
            OpenError::Pages(pagefile::OpenError::Unreadable { .. }) => {
                "open the store again later; if the read keeps failing, restore the store from a \
                 backup: nothing older can be opened"
            }
            OpenError::Pages(_) => "restore the store from a backup",
            OpenError::LogDamaged(_) => {
                "restore the store from a backup, or see what accepting the loss would keep and \
                 give up (Store::plan_damage) and accept that plan (Store::accept_damage)"
            }
            OpenError::Log(LogError::PlanChanged) => {
                "plan the acceptance again (Store::plan_damage): the log changed since"
            }
            OpenError::Log(LogError::NotAsPlanned { .. }) => {
                "plan the acceptance again (Store::plan_damage): the log changed after the \
                 acceptance recorded its decision, and its open did nothing the plan did not say"
            }
            OpenError::UnreadableEnd { .. } => {
                "open the store again later, restore it from a backup, or accept the loss of what \
                 may follow the place (Store::give_up_unreadable_end)"
            }
            OpenError::Log(LogError::FramingBroken { .. }) => {
                "restore the store from a backup: what the log holds after the checkpoint cannot \
                 be found"
            }
            OpenError::Log(LogError::NoSpace(_)) => {
                "free space on the disk, then open the store again"
            }
            OpenError::Log(LogError::Unreadable { .. }) => {
                "open the store again later, or restore it from a backup"
            }
            OpenError::Log(_) => "open the store again, or restore it from a backup",
            OpenError::Replay { .. } => "restore the store from a backup",
            OpenError::Unreadable(_) => "open the store again later, or restore it from a backup",
            OpenError::NoSpace(_) => "free space on the disk, then open the store again",
            OpenError::Start(failed) if failed.disk_full() => {
                "free space on the disk, then open the store again"
            }
            OpenError::Start(_) => "open the store again",
            OpenError::Refused(_) => "a defect of the caller",
            OpenError::Config(_) => "correct the store's configuration",
            OpenError::AfterGivingUp(failed) => failed.error.remedy(),
            OpenError::Journal(JournalError::Full) => {
                "read the incidents (Store::read_incidents) and acknowledge them \
                 (Store::acknowledge_incidents), then open the store again"
            }
            OpenError::Journal(JournalError::NewerFormat { .. }) => {
                "open the store with a server that reads its incident journal"
            }
            OpenError::Journal(JournalError::TooLarge { .. }) => {
                "restore the store from a backup: what it would give up is too large to record \
                 (server-storage-format.md 7.7)"
            }
            OpenError::Journal(_) => "open the store again",
            OpenError::AfterIncidents(failed) => failed.error.remedy(),
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Missing => f.write_str("the folder holds no store"),
            OpenError::NoState { found } => write!(
                f,
                "the folder holds a store ({found}) whose `state` is gone"
            ),
            OpenError::Exists => f.write_str("the folder holds a store already"),
            OpenError::State(e) => write!(f, "{e}"),
            OpenError::Pages(e) => write!(f, "{e}"),
            OpenError::LogDamaged(d) => write!(f, "{}", LogError::Damaged(*d)),
            OpenError::UnreadableEnd { end, .. } => write!(f, "{}", LogError::EndUnreadable(*end)),
            OpenError::Log(e) => write!(f, "{e}"),
            OpenError::Replay { index, error } => {
                write!(
                    f,
                    "entry {index} of the log cannot be applied again: {error}"
                )
            }
            OpenError::Unreadable(e) => write!(f, "{e}"),
            OpenError::NoSpace(e) => {
                write!(f, "no space to apply the entries the log holds: {e}")
            }
            OpenError::Disk(e) => write!(f, "{e}"),
            OpenError::Start(failed) => {
                write!(
                    f,
                    "this start's own entry was not written: {}",
                    failed.error
                )
            }
            OpenError::Refused(why) => write!(f, "refused: {why}"),
            OpenError::Config(why) => write!(f, "the configuration: {why}"),
            OpenError::AfterGivingUp(failed) => {
                let GaveUpThenFailed {
                    gave_up,
                    recorded,
                    error,
                } = &**failed;
                let held = match recorded {
                    Recorded::Done => "which stands",
                    Recorded::Unknown => "which may or may not stand",
                };
                // The inner error names the remedy.
                return write!(f, "after giving up {gave_up:?}, {held}: {error}");
            }
            OpenError::Journal(e) => write!(f, "{e}"),
            OpenError::AfterIncidents(failed) => {
                f.write_str("after recording")?;
                for incident in &failed.recorded {
                    write!(f, " {incident};")?;
                }
                // The inner error names the remedy.
                return write!(f, " {}", failed.error);
            }
        }?;
        write!(f, " ({})", self.remedy())
    }
}

impl std::error::Error for OpenError {}

/// Why this start's own entry was not written ([`OpenError::Start`]).
#[derive(Debug)]
pub struct StartFailed {
    /// The `empty` entry that discarded the groups left open, when the log
    /// took one before the failure: a later open may find it durable.
    pub discarded: Option<Position>,
    /// The entry, when the log took it before the failure: a later open
    /// may find it durable.
    pub written: Option<(Position, Payload)>,
    pub error: WriteError,
}

impl StartFailed {
    /// Whether the disk had no room for the entry, after a checkpoint of
    /// the recovered version let go of what it could.
    pub fn disk_full(&self) -> bool {
        matches!(
            self.error,
            WriteError::Full(_) | WriteError::Log(LogError::NoSpace(_)) | WriteError::NoSpace(_)
        )
    }
}

/// What was given up to open the store, by one of the two explicit calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GaveUp {
    /// The loss after damage through the marker ([`Store::accept_damage`]).
    Damage(Box<DamageAccepted>),
    /// Whatever may follow the unreadable end `end`, after `last`, the last
    /// entry the open verified ([`Store::give_up_unreadable_end`]). `done`
    /// is what the log did: `None` only when the update of `state` that
    /// records it failed part way ([`Recorded::Unknown`]).
    UnreadableEnd {
        last: log::Position,
        end: UnreadableEnd,
        done: Option<Box<CutDone>>,
    },
}

/// What an open found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// This start's count (section 6.1).
    pub start_count: u64,
    /// The checkpoint the page file chose, and the newer ones it passed
    /// over.
    pub checkpoint: pagefile::Opened,
    /// What the log's open found.
    pub log: log::Opened,
    /// Entries applied again after the checkpoint.
    pub replayed: u64,
    /// Checkpoints the open took, the pages waiting for one past their
    /// budget (format section 4.8): of the last entry replayed, during the
    /// replay, or of the recovered entry, before this start's own entry.
    pub replay_checkpoints: u64,
    /// The entry published after the replay: the durable one.
    pub recovered: Position,
    /// The `empty` entry this start wrote before its own, which discarded
    /// the groups the recovered state had open (section 6.9), when it had
    /// any.
    pub discarded: Option<Position>,
    /// This start's own `session` entry, published.
    pub started: Position,
    /// The copy of `state` this open passed over, damaged or unreadable,
    /// when it did.
    pub passed_over_state: Option<SlotFault>,
    /// `state` marked a copy passed over (by this open or an earlier one)
    /// and the open found no whole batch after the later of the marker and
    /// the checkpoint's entry: the marker can have been one advance behind,
    /// and whatever this server confirmed after that entry may be lost
    /// (section 6.12). This open recorded it as an incident
    /// ([`incident::What::LastBatch`], or the field of an acceptance's
    /// [`incident::What::LogDamage`]) before it settled the mark.
    pub last_batch_may_be_lost: bool,
    /// What an explicit call gave up to open the store.
    pub gave_up: Option<GaveUp>,
    /// Where the chain of entries before the checkpoint's entry stops
    /// checking, in the segment the log's open walked to reach it
    /// ([`log::Opened::before_start`]): nothing was given up and the open
    /// went on, since this checkpoint does not need them.
    pub before_start: Option<log::ChainFault>,
    /// The checkpoint on disk before the newest, once the open is done (it
    /// may have taken checkpoints of its own), comes before that place: an
    /// open that falls back to it, the newest not readable, could not replay
    /// past it, which design 6.7 counts on.
    pub fallback_cannot_replay: bool,
    /// Every incident of the journal no administrator has acknowledged,
    /// those this open recorded among them, in order: what the store was
    /// approved to give up, each recorded before it acted
    /// ([`Store::acknowledge`]).
    pub incidents: Vec<Incident>,
    /// What the read of the journal found besides its records (a missing
    /// journal, places that do not check), until an acknowledgement.
    pub journal: incident::Found,
}

/// What an open found before the replay.
struct Opening {
    checkpoint: pagefile::Opened,
    log: log::Opened,
    passed_over_state: Option<SlotFault>,
    gave_up: Option<GaveUp>,
    journal: Journal,
    last_batch_may_be_lost: bool,
}

/// What an open's first steps leave.
struct FirstSteps {
    state: State,
    journal: Journal,
    pages: PageFile,
    checkpoint: pagefile::Opened,
    passed_over_state: Option<SlotFault>,
}

/// How an open treats the page file and the log.
#[derive(Clone, Copy, Debug)]
enum How<'a> {
    Normal,
    AcceptDamage(&'a log::DamagePlan),
    GiveUp(log::Position),
    GiveUpPages(&'a pagefile::GiveUp),
}

/// The most approvals an open records and gives the page file in turn,
/// each one the answer of the call before (the damage changed meanwhile).
const PAGE_ROUNDS: usize = 8;

/// A view of a published version (section 6.6): its entry, and the roots of
/// its trees, kept by a pin for as long as the view lives. It holds the
/// page file, not the store, and hands out nothing of it but checked reads
/// of its own version: no reads of the working state, no checkpoint.
///
/// ```compile_fail
/// fn pages(view: &om_store::store::View) {
///     let _ = view.pages();
/// }
/// ```
#[derive(Clone)]
pub struct View {
    pages: Arc<PageFile>,
    version: Version,
}

impl fmt::Debug for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "View({})", self.version.position.index)
    }
}

impl View {
    /// The entry the view is of.
    pub fn position(&self) -> Position {
        self.version.position
    }

    /// The version's trees a reader of the state opens, by name, with
    /// their roots: the store's tree and the files'. An open group's tree
    /// (`group:` and its id: a staged group's staging, or the file a group
    /// is making, which has no file's name until it is published) is not
    /// among them; of an open group, a view shows only the rows of its
    /// store tree (section 6.9).
    pub fn roots(&self) -> Vec<(TreeName, Root)> {
        self.version
            .pin
            .roots()
            .iter()
            .filter(|(name, _)| {
                !matches!(TreeId::from_name(name.as_bytes()), Some(TreeId::Group(_)))
            })
            .cloned()
            .collect()
    }

    /// The root of one of the state's trees a reader opens ([`View::roots`]).
    pub fn root(&self, tree: TreeId) -> Option<Root> {
        if let TreeId::Group(_) = tree {
            return None;
        }
        let name = tree.name();
        self.version
            .pin
            .roots()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
    }

    /// The store's own tree as the view holds it (`None` before entry 1).
    pub fn store_tree(&self) -> Result<Option<StoreTree<'_, PageFile>>, apply::Error> {
        match self.root(TreeId::Store) {
            Some(root) => StoreTree::open(&*self.pages, root).map(Some),
            None => Ok(None),
        }
    }

    /// A file's tree as the view holds it, if the file exists. A file a
    /// group is still making has no tree of that name until the group
    /// publishes it (section 6.9).
    pub fn file_tree(&self, id: u64) -> Result<Option<Tree<'_, PageFile>>, tree::Error> {
        match self.root(TreeId::File(id)) {
            Some(root) => Tree::open(&*self.pages, root).map(Some),
            None => Ok(None),
        }
    }
}

/// The store on one server (module docs). It hands out nothing of its page
/// file, log or `state` that would let a caller past its rules, only counts
/// and views.
///
/// ```compile_fail
/// fn pages(store: &om_store::store::Store) {
///     let _ = store.pages();
/// }
/// ```
///
/// ```compile_fail
/// fn log(store: &om_store::store::Store) {
///     let _ = store.log();
/// }
/// ```
pub struct Store {
    fs: Arc<dyn Fs>,
    config: Config,
    log: Log,
    pages: Arc<PageFile>,
    server: ServerId,
    /// The term this server writes entries in: on one server it never
    /// changes, and is never below the last entry's.
    term: u64,
    published: Version,
    pending: Vec<Unpublished>,
    stopped: Option<String>,
    journal: Journal,
    /// The groups open in the newest applied state: the log keeps room for
    /// the entries that would end them ([`kept_room`]).
    groups: u64,
}

/// The bytes the log keeps at the end of its segment while `groups` groups
/// are open (format section 6.3): a batch of its own for a discard of each,
/// and one for the next start's `empty` entry and its own entry, so that
/// none of them ever needs a segment a full disk cannot give. None while no
/// group is open.
fn kept_room(groups: u64) -> u64 {
    if groups == 0 {
        return 0;
    }
    let len = |p: &Payload| p.size().map_or(0, |n| n as u64);
    let discard = len(&Payload::Group(GroupEvent::Discard { id: 1 }));
    let start = len(&Payload::Session(SessionEvent::Started {
        server: [0; 16],
        count: 0,
    }));
    let entry = log::ENTRY_OVERHEAD;
    (log::BATCH_HEADER + 2 * entry + start)
        .saturating_add(groups.saturating_mul(log::BATCH_HEADER + entry + discard))
}

/// Whether an entry may take the room the log keeps ([`kept_room`]): one
/// that only gives back, a discard or an `empty` entry, and a start's own
/// entry.
fn takes_kept_room(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::Empty
            | Payload::Group(GroupEvent::Discard { .. })
            | Payload::Session(SessionEvent::Started { .. })
    )
}

/// How an applied entry changes the number of open groups.
fn groups_after(groups: u64, payload: &Payload) -> u64 {
    match payload {
        Payload::Change(Change {
            group:
                Some(Group {
                    step: Step::Open(_),
                    ..
                }),
            ..
        })
        | Payload::Group(GroupEvent::NewFile) => groups.saturating_add(1),
        Payload::Change(Change {
            group: Some(Group {
                step: Step::Close, ..
            }),
            ..
        })
        | Payload::Group(GroupEvent::Publish { .. } | GroupEvent::Discard { .. }) => {
            groups.saturating_sub(1)
        }
        Payload::Empty => 0,
        _ => groups,
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("published", &self.published.position.index)
            .field("pending", &self.pending.len())
            .field("stopped", &self.stopped)
            .finish()
    }
}

/// Which version the store pins.
enum PinOf {
    /// The newest version, right after entry `.0` was applied (or, at
    /// open, replayed or loaded from the checkpoint). Every entry the
    /// store applies commits a unit, since its unit always records the
    /// entry as the store tree's last applied, so the newest version is
    /// that entry's.
    Newest(u64),
    /// A restore point's version, which is on disk.
    OnDisk(u64),
}

/// The pin of a version, and the entry it must be of. Every pin the store
/// takes goes through here, so that how the page file hands out pins is
/// one place. The store never names an entry whose unit has not committed.
fn pin_version(pages: &PageFile, of: PinOf) -> Result<Pin, PinError> {
    let (pin, entry) = match of {
        PinOf::Newest(entry) => (pages.pin_newest()?, entry),
        PinOf::OnDisk(entry) => (pages.pin(entry)?, entry),
    };
    if pin.entry() != entry {
        return Err(PinError::Refused(
            "the pin holds another entry's version than the one asked for",
        ));
    }
    Ok(pin)
}

fn entry_id(p: &Position) -> EntryId {
    EntryId {
        index: p.index,
        term: p.term,
        hash: p.hash,
    }
}

fn position_of(p: log::Position) -> Position {
    Position {
        index: p.index,
        term: p.term,
        hash: Hash(p.hash),
    }
}

/// Whether a failure of the log came from a disk with no room.
fn log_no_space(e: &LogError) -> bool {
    match e {
        LogError::NoSpace(_) => true,
        LogError::Disk(e) | LogError::State(StateError::Disk(e)) => e.kind() == ErrorKind::NoSpace,
        _ => false,
    }
}

/// Whether applying failed because the device failed to read a page (not
/// because a page did not check).
fn device_read(e: &apply::Error) -> bool {
    matches!(
        e,
        apply::Error::Tree(tree::Error::Page(crate::page::PageFault {
            fault: crate::page::Fault::Io(_),
            ..
        }))
    )
}

/// Why the open could not read the rows of the state at entry `index` it
/// needs (the groups left open, section 6.9): the device failing a read is
/// [`OpenError::Unreadable`], as in the replay; a page that does not check,
/// or a row this build cannot read, is the state the store cannot produce
/// again, [`OpenError::Replay`] at that entry.
fn state_unread(index: u64, error: apply::Error) -> OpenError {
    match error {
        e if device_read(&e) => OpenError::Unreadable(e.to_string()),
        e @ (apply::Error::Tree(tree::Error::Page(_))
        | apply::Error::MissingRoot(_)
        | apply::Error::Malformed(_)) => OpenError::Replay { index, error: e },
        e => OpenError::Disk(format!("the groups left open: {e}")),
    }
}

/// Why an entry could not be applied, before anything of it took effect.
enum NotApplied {
    Space(SpaceError),
    Apply(apply::Error),
}

/// Applies `entry` through a reservation of the page file, extended while
/// the entry runs short (the unit that ran short changed nothing, and is
/// begun again): the entry's unit is committed when this returns `Ok`.
///
/// A group's entries (section 6.9) move the space its groups hold
/// ([`apply::group_space`]): the entry that opens a group holds the room
/// for its end, and a staged group's part the room for its later
/// application, before it is applied; the entry that ends groups (a close,
/// a publication, a discard, an `empty` entry) takes what they hold as its
/// reservation, extended like any entry's for the rest, and what it does
/// not take goes back to the free pages. An entry not applied leaves the
/// holds as they were.
fn apply_entry(
    pages: &PageFile,
    entry: &Entry<'_>,
    purpose: Purpose,
) -> Result<(Applied, u32, u64), NotApplied> {
    let space = apply::group_space(pages, entry).map_err(NotApplied::Apply)?;
    let purpose = match purpose {
        Purpose::Entry if space.ends => Purpose::Discard,
        purpose => purpose,
    };
    pages
        .hold(entry.index, space.hold, purpose)
        .map_err(NotApplied::Space)?;
    let reserved = if space.draw > 0 {
        pages.reserve_held(entry.index, space.draw, purpose)
    } else {
        pages.reserve(entry.index, FIRST_RESERVATION, purpose)
    };
    let res = match reserved {
        Ok(res) => res,
        Err(e) => {
            pages.unhold(space.hold);
            return Err(NotApplied::Space(e));
        }
    };
    let not_applied = |e| {
        res.keep_held();
        pages.unhold(space.hold);
        e
    };
    let mut extended = 0u32;
    let mut size = FIRST_RESERVATION;
    loop {
        match apply::apply(&res, entry) {
            Ok(applied) => return Ok((applied, extended, res.taken())),
            Err(apply::Error::Tree(tree::Error::Store(StoreError::Full))) => {
                res.extend(size)
                    .map_err(|e| not_applied(NotApplied::Space(e)))?;
                size = size.saturating_mul(2);
                extended += 1;
            }
            Err(e) => return Err(not_applied(NotApplied::Apply(e))),
        }
    }
}

/// The file a creation makes first and removes once `state` exists (format
/// section 6.1): a folder without `state` that holds it holds only what a
/// creation cut short left, and is created over.
pub const CREATING: &str = "creating";

/// Whether the file `path` exists.
fn exists(fs: &dyn Fs, path: &str) -> Result<bool, OpenError> {
    match fs.open(path, sys::FileOptions::PLAIN) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(OpenError::Disk(e.to_string())),
    }
}

/// What of a store a folder without `state` holds, apart from what a
/// creation cut short left: the log's folder (made only by an open, once
/// `state` exists), or a page file without the creation's marker.
fn store_left(fs: &dyn Fs) -> Result<Option<&'static str>, OpenError> {
    match fs.list(log::FOLDER) {
        Ok(_) => return Ok(Some("the log's folder")),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(OpenError::Disk(e.to_string())),
    }
    if exists(fs, pagefile::FILE)? && !exists(fs, CREATING)? {
        return Ok(Some("a page file, and no creation under way"));
    }
    Ok(None)
}

/// The entry of the checkpoint the page file opened.
fn start_of(checkpoint: &pagefile::Opened) -> log::Position {
    log::Position {
        index: checkpoint.entry.index,
        term: checkpoint.entry.term,
        hash: checkpoint.entry.hash.0,
    }
}

/// The open's error for the log's.
fn log_error(e: LogError) -> OpenError {
    match e {
        LogError::Damaged(d) => OpenError::LogDamaged(d),
        e => OpenError::Log(e),
    }
}

/// `error`, saying what the open recorded before it, if anything.
fn after_incidents(recorded: Vec<Incident>, error: OpenError) -> OpenError {
    if recorded.is_empty() {
        error
    } else {
        OpenError::AfterIncidents(Box::new(IncidentsThenFailed { recorded, error }))
    }
}

/// Removes the creation's marker, once `state` exists.
fn creation_done(fs: &dyn Fs) -> Result<(), OpenError> {
    let disk = |e: sys::Error| OpenError::Disk(e.to_string());
    match fs.remove(CREATING) {
        Ok(()) => fs.flush_folder("").map_err(disk),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(disk(e)),
    }
}

/// Checks `config` against what the store itself must be able to write:
/// the log's and the page file's own bounds, and this start's entry, which
/// every open writes. Before anything is created or opened.
fn check_config(config: &Config) -> Result<(), OpenError> {
    let refused = |why: String| OpenError::Config(why);
    config.log.check().map_err(|e| refused(e.to_string()))?;
    config.pages.check().map_err(|e| refused(e.to_string()))?;
    let start = Payload::Session(SessionEvent::Started {
        server: [0xff; 16],
        count: u64::MAX,
    })
    .encode()
    .map_err(|e| refused(e.to_string()))?;
    if start.len() > config.log.max_payload as usize {
        return Err(refused(format!(
            "the largest payload, {} bytes, is smaller than this start's own entry, {} bytes",
            config.log.max_payload,
            start.len()
        )));
    }
    Ok(())
}

impl Store {
    /// Makes a new store in a folder that holds none, and opens it: `state`
    /// with a new server id drawn from `random`, the page file's first
    /// checkpoint, an empty log, and this start's `session` entry. The
    /// creation's marker ([`CREATING`]) is made first and removed once
    /// `state` exists, so that a creation cut short is told from a store
    /// whose `state` is gone: what a creation cut short left is created
    /// over; a folder with `state` holds a store already
    /// ([`OpenError::Exists`]), and one that holds a store without `state`
    /// is refused ([`OpenError::NoState`]).
    pub fn create(
        fs: Arc<dyn Fs>,
        random: &dyn Random,
        config: Config,
    ) -> Result<(Store, Opened), OpenError> {
        check_config(&config)?;
        let disk = |e: sys::Error| OpenError::Disk(e.to_string());
        let lock = fs.lock().map_err(disk)?;
        if exists(&*fs, crate::state::FILE)? {
            return Err(OpenError::Exists);
        }
        if let Some(found) = store_left(&*fs)? {
            return Err(OpenError::NoState { found });
        }
        // The marker first, its name durable before anything else is made.
        if !exists(&*fs, CREATING)? {
            drop(fs.create(CREATING, sys::FileOptions::PLAIN).map_err(disk)?);
        }
        fs.flush_folder("").map_err(disk)?;
        // The page file, then `state`, then the marker removed.
        match fs.remove(pagefile::FILE) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(disk(e)),
        }
        drop(PageFile::create(&*fs, &lock, config.pages).map_err(OpenError::Pages)?);
        // The incident journal, holding its base record, its name durable
        // before `state` exists: from then on a store whose journal is
        // missing, or holds no record, lost it.
        Journal::create(&*fs, 0).map_err(OpenError::Journal)?;
        let state = State::create(&*fs, random, lock).map_err(OpenError::State)?;
        creation_done(&*fs)?;
        drop(state);
        // The rest is an open of the store just made.
        Store::open(fs, config)
    }

    /// Opens the store in the folder (section 6.7), or says why it cannot
    /// ([`OpenError`]).
    pub fn open(fs: Arc<dyn Fs>, config: Config) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::Normal)
    }

    /// Opens a store whose log does not hold, whole, every entry through
    /// its marker, accepting the loss of what it no longer holds: the
    /// administrator's command of section 6.12 on a single server. It
    /// carries out `plan`, what [`Store::plan_damage`] showed: the plan is
    /// made again from the checkpoint the store opens on, and the call
    /// refuses with nothing in the log changed when the two differ
    /// ([`LogError::PlanChanged`]; plan again). The rule is
    /// [`Log::plan_damage`]'s: kept, the chain from that checkpoint as far
    /// as it checks and the open's own walk goes; given up, the rest, set
    /// aside in `log/` and never removed; refused when the walk cannot say
    /// where the checkpoint's successor begins, or nothing would change.
    /// What was given up is in [`Opened::gave_up`], or, when the open fails
    /// after deciding, in [`OpenError::AfterGivingUp`].
    pub fn accept_damage(
        fs: Arc<dyn Fs>,
        config: Config,
        plan: &log::DamagePlan,
    ) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::AcceptDamage(plan))
    }

    /// Opens a store whose log ends at a place it cannot read, giving up
    /// whatever may follow that place: the administrator's command of
    /// section 6.12 on a single server ([`Log::give_up_unreadable_end`]).
    /// `last` is the last entry the open verified, as
    /// [`OpenError::UnreadableEnd`] reported it; the call is refused when
    /// the log does not end so at that entry. What was given up is reported
    /// as by [`Store::accept_damage`].
    pub fn give_up_unreadable_end(
        fs: Arc<dyn Fs>,
        config: Config,
        last: log::Position,
    ) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::GiveUp(last))
    }

    /// Opens a store whose page file does not open because the device fails
    /// a read its open needs, giving up what it cannot read: the
    /// administrator's explicit call (sections 6.7 and 6.12), given the
    /// approval [`OpenError::Pages`] carried
    /// ([`pagefile::OpenError::give_up`]). The approval is recorded in the
    /// incident journal, and flushed, before
    /// [`PageFile::open_giving_up`] is called with it, which does exactly
    /// that or nothing. When nothing stops the page file's open any more it
    /// opens plainly; when the call answers damage instead, the store gives
    /// that up by itself, recording it first; when it answers another read
    /// failure, that error is returned, to approve in turn.
    pub fn give_up_pages(
        fs: Arc<dyn Fs>,
        config: Config,
        approval: &pagefile::GiveUp,
    ) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::GiveUpPages(approval))
    }

    /// What accepting the loss would do ([`Log::plan_damage`]), from the
    /// checkpoint the store would open on: nothing is written to the log.
    /// The open's own first steps run as in any open (they raise `state`'s
    /// start count, settle the page file's slots, and give up what the
    /// store gives up by itself, recording it first). A listed segment that
    /// an earlier acceptance set aside, by its record in the journal, is
    /// named with where it went ([`log::Fate::MovedEarlier`]). Accept it
    /// with [`Store::accept_damage`], which refuses when the log no longer
    /// matches it.
    pub fn plan_damage(fs: Arc<dyn Fs>, config: Config) -> Result<log::DamagePlan, OpenError> {
        let mut recorded = Vec::new();
        let planned = (|| {
            let first = Store::first_steps(&fs, config, None, &mut recorded)?;
            let start = start_of(&first.checkpoint);
            Log::plan_damage(
                &*fs,
                &first.state,
                config.log,
                start,
                &first.journal.earlier_moves(),
            )
            .map_err(log_error)
        })();
        planned.map_err(|e| after_incidents(recorded, e))
    }

    /// The incidents of the store in the folder no administrator has
    /// acknowledged, and what the read of the journal found, read with the
    /// store folder's lock (so not while the store is open: then see
    /// [`Store::incidents`]). It needs nothing but the journal: a store whose
    /// log or page file cannot be read says what it gave up.
    pub fn read_incidents(fs: &Arc<dyn Fs>) -> Result<incident::Reading, OpenError> {
        let _lock = fs.lock().map_err(|e| OpenError::Disk(e.to_string()))?;
        incident::look(&**fs).map_err(OpenError::Journal)
    }

    /// Acknowledge, for the store in the folder, everything a read showed
    /// the administrator ([`Store::read_incidents`]): refused, with nothing
    /// written, unless the journal reads the same now; then it holds only a
    /// base record carrying the next number (format section 7.5). What the
    /// read could not see goes with it, unseen: a place the device failed
    /// to read at both reads (a record there is discarded), and the bytes
    /// past the read's limit (a change there that keeps the file's size
    /// does not refuse the call). With the store folder's lock, so not
    /// while the store is open: then see [`Store::acknowledge`]. Returns how
    /// many incidents the read decoded and were acknowledged.
    pub fn acknowledge_incidents(
        fs: &Arc<dyn Fs>,
        shown: &incident::Shown,
    ) -> Result<usize, OpenError> {
        let _lock = fs.lock().map_err(|e| OpenError::Disk(e.to_string()))?;
        incident::acknowledge_closed(&**fs, shown).map_err(OpenError::Journal)
    }

    /// An open's first steps: the configuration checked, the lock and
    /// `state`, a creation's marker settled, the incident journal read (a
    /// missing one recorded as such), and the page file's checkpoint. When
    /// the page file answers damage with an approval, or the administrator
    /// gave one (`approval`), it is recorded and flushed before
    /// [`PageFile::open_giving_up`] is called with it; what the call
    /// answers is handled the same way, up to [`PAGE_ROUNDS`] times.
    /// Everything recorded is pushed to `recorded`.
    fn first_steps(
        fs: &Arc<dyn Fs>,
        config: Config,
        approval: Option<&pagefile::GiveUp>,
        recorded: &mut Vec<Incident>,
    ) -> Result<FirstSteps, OpenError> {
        check_config(&config)?;
        // 1. The lock and `state`.
        let disk = |e: sys::Error| OpenError::Disk(e.to_string());
        let mut looks = 0;
        let state = loop {
            let lock = fs.lock().map_err(disk)?;
            match State::open(&**fs, lock) {
                Ok(state) => break state,
                Err(StateError::Missing) => {
                    // Without `state`: nothing, a creation cut short, or a
                    // store whose `state` is gone, looked at under the lock
                    // again. `State::open` let it go: another process may
                    // have made `state` meanwhile, and it is opened then.
                    let _lock = fs.lock().map_err(disk)?;
                    looks += 1;
                    if exists(&**fs, crate::state::FILE)? && looks < 3 {
                        continue;
                    }
                    return Err(match store_left(&**fs)? {
                        Some(found) => OpenError::NoState { found },
                        None => OpenError::Missing,
                    });
                }
                Err(e) => return Err(OpenError::State(e)),
            }
        };
        // A creation whose last step was cut short.
        creation_done(&**fs)?;
        let passed_over_state = state.opened().and_then(|o| o.other.err());
        // The incident journal: the store's own, read before anything that
        // can be given up is opened.
        let mut journal =
            Journal::open(&**fs, state.values().start_count).map_err(OpenError::Journal)?;
        if journal.found().missing {
            recorded.push(
                journal
                    .record(&**fs, What::JournalLost)
                    .map_err(OpenError::Journal)?,
            );
        }
        // 2. The page file's checkpoint: given up only as recorded.
        let mut giving: Option<(pagefile::GiveUp, bool, bool, String)> = approval.map(|a| {
            (
                a.clone(),
                false,
                true,
                "the administrator gave up what the device fails to read".to_owned(),
            )
        });
        let mut rounds = 0;
        let (pages, checkpoint) = loop {
            rounds += 1;
            if rounds > PAGE_ROUNDS {
                return Err(OpenError::Refused(
                    "the page file's damage changed at every attempt to give it up",
                ));
            }
            let opened = match &giving {
                None => PageFile::open(&**fs, state.folder_lock(), config.pages),
                Some((approval, damaged, by_administrator, why)) => {
                    let incident = journal
                        .record(
                            &**fs,
                            What::checkpoints(approval, *damaged, *by_administrator, why),
                        )
                        .map_err(OpenError::Journal)?;
                    recorded.push(incident);
                    PageFile::open_giving_up(&**fs, state.folder_lock(), config.pages, approval)
                }
            };
            match opened {
                Ok(opened) => break opened,
                // Nothing stops the plain open any more.
                Err(pagefile::OpenError::Refused(_)) if giving.is_some() => giving = None,
                // Damage the store gives up by itself, recorded first.
                Err(e @ pagefile::OpenError::Damaged { .. }) if e.give_up().is_some() => {
                    let approval = e.give_up().cloned().expect("checked");
                    giving = Some((approval, true, false, e.to_string()));
                }
                Err(e) => return Err(OpenError::Pages(e)),
            }
        };
        Ok(FirstSteps {
            state,
            journal,
            pages,
            checkpoint,
            passed_over_state,
        })
    }

    fn start(fs: Arc<dyn Fs>, config: Config, how: How) -> Result<(Store, Opened), OpenError> {
        let mut recorded = Vec::new();
        Store::start_recording(fs, config, how, &mut recorded)
            .map_err(|e| after_incidents(recorded, e))
    }

    fn start_recording(
        fs: Arc<dyn Fs>,
        config: Config,
        how: How,
        recorded: &mut Vec<Incident>,
    ) -> Result<(Store, Opened), OpenError> {
        let approval = match how {
            How::GiveUpPages(approval) => Some(approval),
            _ => None,
        };
        let FirstSteps {
            state,
            mut journal,
            pages,
            checkpoint,
            passed_over_state,
        } = Store::first_steps(&fs, config, approval, recorded)?;
        let start = start_of(&checkpoint);
        // 3. The log from that checkpoint, checked against the marker, and
        // what an explicit call gives up, each recorded before it is. From
        // the moment `state` records a decision, every error says what it
        // was.
        let failed = log_error;
        let mut last_batch_may_be_lost = false;
        let (mut log, mut log_opened, mut gave_up) = match how {
            How::AcceptDamage(plan) => {
                let earlier = journal.earlier_moves();
                let values = *state.values();
                // Why the journal refused, kept typed for the caller.
                let mut refused: Option<JournalError> = None;
                let mut record = |plan: &log::DamagePlan| -> Result<(), String> {
                    // A mark the acceptance does not make up for: whatever
                    // was confirmed after the later of the marker and the
                    // start may be lost too (section 6.12), a fact of the
                    // one record.
                    let firm = plan.marker.max(plan.start.index);
                    let last_batch_after =
                        (values.mark.is_some() && plan.opens_at.index <= firm).then_some(firm);
                    let recording = PlanRecord::of(plan).and_then(|plan| {
                        let what = What::LogDamage {
                            plan: Box::new(plan),
                            last_batch_after,
                        };
                        journal.record(&*fs, what)
                    });
                    match recording {
                        Ok(incident) => {
                            last_batch_may_be_lost |= last_batch_after.is_some();
                            recorded.push(incident);
                            Ok(())
                        }
                        Err(e) => {
                            let why = e.to_string();
                            refused = Some(e);
                            Err(why)
                        }
                    }
                };
                let accepted = Log::accept_damage(
                    fs.clone(),
                    state,
                    config.log,
                    start,
                    plan,
                    &earlier,
                    &mut record,
                );
                if let Some(e) = refused {
                    return Err(OpenError::Journal(e));
                }
                match accepted {
                    Ok((log, opened, accepted)) => {
                        (log, opened, Some(GaveUp::Damage(Box::new(accepted))))
                    }
                    Err(f) => {
                        let error = failed(f.failed.error);
                        return Err(match f.decided {
                            Some((accepted, recorded)) => OpenError::after(
                                GaveUp::Damage(Box::new(accepted)),
                                recorded,
                                error,
                            ),
                            None => error,
                        });
                    }
                }
            }
            How::Normal | How::GiveUp(_) | How::GiveUpPages(_) => {
                let (log, opened) = match Log::open(fs.clone(), state, config.log, start) {
                    Ok(opened) => opened,
                    Err(f) => match f.error {
                        // `state`'s mark, no whole batch after the later of
                        // the marker and the start: the possible loss
                        // recorded, once, and the open goes on whatever it
                        // finds after that entry, settling the mark.
                        LogError::LossNotRecorded { after } => {
                            let incident = journal
                                .record(&*fs, What::LastBatch { after })
                                .map_err(OpenError::Journal)?;
                            recorded.push(incident);
                            last_batch_may_be_lost = true;
                            Log::open_settling(fs.clone(), f.state, config.log, start)
                                .map_err(|f| failed(f.error))?
                        }
                        e => return Err(failed(e)),
                    },
                };
                (log, opened, None)
            }
        };
        match (how, log_opened.unreadable_end) {
            (How::GiveUp(named), Some(end)) => {
                let durable = log.durable();
                if named.index != durable.index || named.hash != durable.hash {
                    return Err(OpenError::Refused(
                        "the log does not end at that entry before a place it cannot read",
                    ));
                }
                let incident = journal
                    .record(&*fs, What::UnreadableEnd { last: durable, end })
                    .map_err(OpenError::Journal)?;
                recorded.push(incident);
                match log.give_up_unreadable_end(named.index, named.hash) {
                    Ok(done) => {
                        log_opened.unreadable_end = None;
                        gave_up = Some(GaveUp::UnreadableEnd {
                            last: named,
                            end,
                            done: Some(Box::new(done)),
                        });
                    }
                    // Its one `state` update failed part way.
                    Err(e @ LogError::State(_))
                        if !matches!(e, LogError::State(StateError::Refused(_))) =>
                    {
                        let gave_up = GaveUp::UnreadableEnd {
                            last: named,
                            end,
                            done: None,
                        };
                        return Err(OpenError::after(
                            gave_up,
                            Recorded::Unknown,
                            OpenError::Log(e),
                        ));
                    }
                    Err(e) => return Err(OpenError::Log(e)),
                }
            }
            (How::GiveUp(_), None) => {
                return Err(OpenError::Refused(
                    "the log does not end at a place it cannot read",
                ))
            }
            (_, Some(end)) => {
                let error = OpenError::UnreadableEnd {
                    end,
                    last: log.durable(),
                };
                // After an acceptance `state` recorded, the error says so.
                return Err(match gave_up {
                    Some(gave_up) => OpenError::after(gave_up, Recorded::Done, error),
                    None => error,
                });
            }
            (_, None) => {}
        }
        let opened = Opening {
            checkpoint,
            log: log_opened,
            passed_over_state,
            gave_up: gave_up.clone(),
            journal,
            last_batch_may_be_lost,
        };
        match Store::recover(fs, config, Arc::new(pages), log, opened) {
            Err(error) => Err(match gave_up {
                Some(gave_up) => OpenError::after(gave_up, Recorded::Done, error),
                None => error,
            }),
            done => done,
        }
    }

    /// The rest of an open, once the log is open: the replay, the pin of
    /// the recovered version, and this start's own entry.
    fn recover(
        fs: Arc<dyn Fs>,
        config: Config,
        pages: Arc<PageFile>,
        log: Log,
        opening: Opening,
    ) -> Result<(Store, Opened), OpenError> {
        let Opening {
            checkpoint,
            log: log_opened,
            passed_over_state,
            gave_up,
            journal,
            last_batch_may_be_lost,
        } = opening;
        let values = *log.state().values();
        let start = position_of(log::Position {
            index: checkpoint.entry.index,
            term: checkpoint.entry.term,
            hash: checkpoint.entry.hash.0,
        });
        // The groups the checkpoint's state has open hold the room for their
        // later application again, as their parts held it (section 6.9).
        let held: u64 = StoreTree::of(&*pages)
            .and_then(|state| state.groups())
            .map_err(|e| state_unread(start.index, e))?
            .iter()
            .map(|(_, g)| g.held)
            .sum();
        match pages.hold(start.index + 1, held, Purpose::Recovery) {
            Ok(()) => {}
            Err(e @ SpaceError::Full { .. }) => return Err(OpenError::NoSpace(e)),
            Err(e) => return Err(OpenError::Disk(e.to_string())),
        }
        // 4. The entries after the checkpoint, each once, in order, through
        // the durable entry; nothing freed in the log meanwhile.
        let durable = log.durable();
        let mut replayed = 0u64;
        let mut replay_checkpoints = 0u64;
        let mut last = start;
        if durable.index > start.index {
            for e in log.read(start.index + 1).map_err(OpenError::Log)? {
                let e = e.map_err(|e| match e {
                    LogError::Damaged(d) => OpenError::LogDamaged(d),
                    e => OpenError::Log(e),
                })?;
                let entry = Entry {
                    index: e.index,
                    term: e.term,
                    kind: e.kind,
                    file: e.file,
                    payload: &e.payload,
                    hash: Hash(e.hash),
                };
                let mut applied = apply_entry(&pages, &entry, Purpose::Recovery);
                if let Err(NotApplied::Space(SpaceError::CheckpointNeeded { .. })) = applied {
                    // The pages of the replay waiting past their budget: the
                    // entries replayed are durable, so the last of them is
                    // checkpointed (format section 4.8, a case it lists).
                    pages
                        .checkpoint(entry_id(&last), None)
                        .map_err(|e| OpenError::Disk(e.to_string()))?;
                    replay_checkpoints += 1;
                    applied = apply_entry(&pages, &entry, Purpose::Recovery);
                }
                match applied {
                    Ok(_) => {}
                    Err(NotApplied::Space(e @ SpaceError::Full { .. })) => {
                        return Err(OpenError::NoSpace(e))
                    }
                    Err(NotApplied::Space(e)) => return Err(OpenError::Disk(e.to_string())),
                    Err(NotApplied::Apply(error)) if device_read(&error) => {
                        return Err(OpenError::Unreadable(error.to_string()))
                    }
                    Err(NotApplied::Apply(error)) => {
                        return Err(OpenError::Replay {
                            index: e.index,
                            error,
                        })
                    }
                }
                last = Position {
                    index: e.index,
                    term: e.term,
                    hash: Hash(e.hash),
                };
                replayed += 1;
            }
        }
        if last.index != durable.index || last.hash.0 != durable.hash {
            return Err(OpenError::Refused(
                "the replay did not end at the durable entry",
            ));
        }
        let before_start = log_opened.before_start;
        // 5. The recovered version, pinned and published.
        let pin = pin_version(&pages, PinOf::Newest(last.index))
            .map_err(|e| OpenError::Disk(format!("the recovered version: {e}")))?;
        // Each start begins a new term (server-storage.md 6.9): above every
        // term `state` and the log recorded, made durable by this start's
        // first entries.
        let term = values
            .term
            .max(durable.term)
            .checked_add(1)
            .ok_or(OpenError::Refused("the term is at its greatest value"))?;
        let mut store = Store {
            fs,
            config,
            log,
            pages,
            server: values.server_id.0,
            term,
            published: Version {
                position: last,
                pin: Arc::new(pin),
            },
            pending: Vec::new(),
            stopped: None,
            journal,
            groups: 0,
        };
        // 6. This start's own entry (section 7.3), the first write. It is
        // part of the recovery for space: it may take the reserve, and when
        // the pages waiting for a checkpoint are past their budget, or the
        // disk has no room for the entry (the log's next segment, pages
        // past the reserve), the recovered version, which is durable, is
        // checkpointed first, which retires the log and frees dying pages
        // as far as the checkpoints and restore points on disk allow.
        // A restart discards every group left open, by the open's own
        // `empty` entry, before this start's entry (section 6.9).
        let groups_open = StoreTree::of(&*store.pages)
            .and_then(|state| state.groups())
            .map_err(|e| state_unread(last.index, e))?;
        store.groups = groups_open.len() as u64;
        let discarded = if groups_open.is_empty() {
            None
        } else {
            match store.append_own(&Payload::Empty, Purpose::Discard) {
                Ok((at, checkpoints)) => {
                    replay_checkpoints += checkpoints;
                    Some(at)
                }
                Err(error) => {
                    return Err(OpenError::Start(Box::new(StartFailed {
                        discarded: None,
                        written: None,
                        error,
                    })))
                }
            }
        };
        let start_count = values.start_count;
        let payload = Payload::Session(SessionEvent::Started {
            server: store.server,
            count: start_count,
        });
        let failed = |written, error| {
            OpenError::Start(Box::new(StartFailed {
                discarded,
                written,
                error,
            }))
        };
        let started = match store.append_own(&payload, Purpose::Recovery) {
            Ok((at, checkpoints)) => {
                replay_checkpoints += checkpoints;
                at
            }
            Err(error) => return Err(failed(None, error)),
        };
        if let Err(error) = store.flush() {
            return Err(failed(Some((started, payload)), error));
        }
        // What an open falling back from the newest checkpoint would meet:
        // the checkpoints on disk now, after any this open took.
        let incidents = store.journal.incidents().to_vec();
        let journal_found = store.journal.found().clone();
        let fallback_cannot_replay = before_start.is_some_and(|stop| {
            store
                .pages
                .kept_checkpoints()
                .get(1)
                .is_some_and(|older| older.entry.index < stop.index)
        });
        Ok((
            store,
            Opened {
                start_count,
                checkpoint,
                log: log_opened,
                replayed,
                replay_checkpoints,
                recovered: last,
                discarded,
                started,
                passed_over_state,
                last_batch_may_be_lost,
                gave_up,
                before_start,
                fallback_cannot_replay,
                incidents,
                journal: journal_found,
            },
        ))
    }

    /// The incidents no administrator has acknowledged, in order (what the
    /// store was approved to give up, each recorded before it acted), what
    /// the read found besides, and what it showed: the journal read again
    /// now, to show an administrator before [`Store::acknowledge`].
    pub fn incidents(&self) -> Result<incident::Reading, WriteError> {
        incident::look(&*self.fs)
            .map_err(|e| WriteError::Failed(format!("reading the incident journal: {e}")))
    }

    /// Acknowledge everything a read showed ([`Store::incidents`]): refused,
    /// with nothing written, unless the journal reads the same now (an
    /// incident recorded since, or a change the read can see, refuses it);
    /// then the journal holds only a base record carrying the next number,
    /// and no open reports those incidents again. What the read could not
    /// see goes with it, unseen: a place the device failed to read at both
    /// reads (a record there is discarded), and the bytes past the read's
    /// limit (a change there that keeps the file's size does not refuse the
    /// call). Returns how many incidents the read decoded and were
    /// acknowledged. A failed write or flush stops the store.
    pub fn acknowledge(&mut self, shown: &incident::Shown) -> Result<usize, WriteError> {
        self.usable()?;
        match self.journal.acknowledge(&*self.fs, shown) {
            Ok(n) => Ok(n),
            Err(JournalError::Refused(why)) => Err(WriteError::Rule(why)),
            Err(e) if self.journal.is_failed() => {
                Err(self.stop(format!("acknowledging incidents: {e}")))
            }
            Err(e) => Err(WriteError::Failed(format!("acknowledging incidents: {e}"))),
        }
    }

    fn usable(&self) -> Result<(), WriteError> {
        match &self.stopped {
            Some(why) => Err(WriteError::Stopped(why.clone())),
            None => Ok(()),
        }
    }

    /// The store stops (module docs): every later call is refused.
    fn stop(&mut self, why: impl Into<String>) -> WriteError {
        let why = why.into();
        self.stopped = Some(why.clone());
        WriteError::Failed(why)
    }

    /// The store stops for a failure of the log, keeping a full disk as the
    /// cause.
    fn stop_log(&mut self, why: String, e: &LogError) -> WriteError {
        self.stopped = Some(why.clone());
        if log_no_space(e) {
            WriteError::NoSpace(why)
        } else {
            WriteError::Failed(why)
        }
    }

    /// Whether the store has stopped, and why.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// The last entry published.
    pub fn published(&self) -> Position {
        self.published.position
    }

    /// The last entry appended (published or waiting for a flush).
    pub fn appended(&self) -> Position {
        position_of(self.log.appended())
    }

    /// How many entries wait for a flush.
    pub fn unpublished(&self) -> usize {
        self.pending.len()
    }

    /// The index the next entry appended gets: a file it adds has it for
    /// its id.
    pub fn next_index(&self) -> u64 {
        self.log.appended().index + 1
    }

    /// The server's id, from `state`.
    pub fn server(&self) -> ServerId {
        self.server
    }

    /// The term entries are written in: each start begins a new one, above
    /// every term `state` and the log recorded.
    pub fn term(&self) -> u64 {
        self.term
    }

    /// The page file's space: its size, and its free, held and dying
    /// pages.
    pub fn space(&self) -> pagefile::Space {
        self.pages.space()
    }

    /// The checkpoints on disk, newest first.
    pub fn checkpoints(&self) -> Vec<pagefile::KeptCheckpoint> {
        self.pages.kept_checkpoints()
    }

    /// The log's segments in use, as `state` lists them.
    pub fn log_segments(&self) -> crate::state::Segments {
        self.log.segments()
    }

    /// Appends an entry with this payload, of file `file` (0 for none):
    /// checks it, applies it to the working state and adds it to the log's
    /// open batch (module docs). It is published by the next flush.
    ///
    /// The store gives the entry its index and term; what a payload says
    /// about this server's own starts is the store's too, and refused
    /// ([`WriteError::Rule`]) from its caller: a `session` entry that says
    /// this server started (the store writes one at each open), and one that
    /// opens a session of this server in another start than the current one.
    pub fn append(&mut self, file: u64, payload: &Payload) -> Result<Appended, WriteError> {
        self.usable()?;
        if let Payload::Session(event) = payload {
            match event {
                SessionEvent::Started { server, .. } if *server == self.server => {
                    return Err(WriteError::Rule(
                        "this server's start entry is the store's to write, at each open",
                    ))
                }
                SessionEvent::Opened(s)
                    if s.server == self.server && s.start != self.start_count() =>
                {
                    return Err(WriteError::Rule(
                        "a session of this server is opened only in its current start",
                    ))
                }
                _ => {}
            }
        }
        self.append_as(file, payload, Purpose::Entry)
    }

    /// This start's count, from `state`.
    fn start_count(&self) -> u64 {
        self.log.state().values().start_count
    }

    /// Appends one of the open's own entries (format section 6.2, steps 5
    /// and 6), unflushed: refused because the pages waiting for a
    /// checkpoint are past their budget, or because the disk has no room,
    /// it flushes what the open appended before it, checkpoints that
    /// version, which is durable then, and appends the entry again, once.
    /// Returns where the entry went and the checkpoints taken.
    fn append_own(
        &mut self,
        payload: &Payload,
        purpose: Purpose,
    ) -> Result<(Position, u64), WriteError> {
        let mut checkpointed = false;
        loop {
            match self.append_as(0, payload, purpose) {
                Ok(appended) => return Ok((appended.position, u64::from(checkpointed))),
                Err(
                    WriteError::CheckpointNeeded(_)
                    | WriteError::Full(_)
                    | WriteError::Log(LogError::NoSpace(_)),
                ) if !checkpointed => {
                    checkpointed = true;
                    if !self.pending.is_empty() {
                        self.flush()?;
                    }
                    if let Err(e) = self.checkpoint(None) {
                        let why = format!("the checkpoint before the open's own entry: {e}");
                        return Err(match e {
                            CheckpointFailed::NoSpace(_) => WriteError::NoSpace(why),
                            _ => WriteError::Failed(why),
                        });
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// [`Store::append`] without its rules on the payload, reserving for
    /// `purpose`.
    fn append_as(
        &mut self,
        file: u64,
        payload: &Payload,
        purpose: Purpose,
    ) -> Result<Appended, WriteError> {
        self.usable()?;
        let bytes = payload.encode().map_err(WriteError::Payload)?;
        let kind = payload.kind().code();
        let term = self.term;
        // The room the log keeps for ending the groups open after this
        // entry, which an entry that only gives back may take.
        let kept = takes_kept_room(payload);
        self.log.keep(if kept {
            0
        } else {
            kept_room(groups_after(self.groups, payload).max(self.groups))
        });
        // Whatever the log would refuse is refused now, before the unit.
        let ready = if kept {
            self.log.ready_for_kept(term, bytes.len())
        } else {
            self.log.ready_for(term, bytes.len())
        };
        match ready {
            Ok(()) => {}
            Err(e) if self.log.is_poisoned() => return Err(self.stop_log(e.to_string(), &e)),
            Err(e) => return Err(WriteError::Log(e)),
        }
        let prev = self.log.appended();
        let index = prev.index + 1;
        let hash = entry_hash(&Hash(prev.hash), index, term, kind, file, &bytes);
        let entry = Entry {
            index,
            term,
            kind,
            file,
            payload: &bytes,
            hash,
        };
        let (applied, extended, pages) = match apply_entry(&self.pages, &entry, purpose) {
            Ok(done) => done,
            Err(NotApplied::Space(e @ SpaceError::Full { .. })) => return Err(WriteError::Full(e)),
            Err(NotApplied::Space(e @ SpaceError::CheckpointNeeded { .. })) => {
                return Err(WriteError::CheckpointNeeded(e))
            }
            Err(NotApplied::Space(SpaceError::Refused(why))) => return Err(WriteError::Rule(why)),
            Err(NotApplied::Space(e)) => return Err(self.stop(e.to_string())),
            Err(NotApplied::Apply(apply::Error::Refused(r))) => return Err(WriteError::Refused(r)),
            Err(NotApplied::Apply(e)) if self.pages.failed() => {
                return Err(self.stop(e.to_string()))
            }
            Err(NotApplied::Apply(e)) => return Err(WriteError::Apply(e)),
        };
        // The unit is committed: from here a failure stops the store.
        let pin = match pin_version(&self.pages, PinOf::Newest(index)) {
            Ok(pin) => pin,
            Err(e) => return Err(self.stop(format!("the version of entry {index}: {e}"))),
        };
        let logged = if kept {
            self.log.append_kept(term, kind, file, &bytes)
        } else {
            self.log.append(term, kind, file, &bytes)
        };
        match logged {
            Ok(p) if p.index == index && p.hash == hash.0 => {}
            Ok(_) => {
                return Err(self.stop(
                    "the log placed the entry elsewhere, or hashed it otherwise, than it was \
                     applied",
                ))
            }
            Err(e) => {
                let why = format!("the log refused an entry already applied: {e}");
                return Err(self.stop_log(why, &e));
            }
        }
        self.groups = groups_after(self.groups, payload);
        let position = applied.position;
        self.pending.push(Unpublished {
            version: Version {
                position,
                pin: Arc::new(pin),
            },
            applied,
        });
        Ok(Appended {
            position,
            extended,
            pages,
        })
    }

    /// Writes the log's open batch and advances the marker, then publishes
    /// every entry it made durable, in order, and returns them (module
    /// docs). On one server the committed position is the flush's last
    /// entry. A failure stops the store.
    pub fn flush(&mut self) -> Result<Vec<Published>, WriteError> {
        self.usable()?;
        let last = self.log.appended().index;
        let durable = match self.log.flush_batch(Some(last)) {
            Ok(durable) => durable,
            Err(e) => {
                let why = format!("the log's flush failed: {e}");
                return Err(self.stop_log(why, &e));
            }
        };
        if durable.index != last {
            return Err(self.stop("the flush made another entry durable than the last appended"));
        }
        let mut out = Vec::with_capacity(self.pending.len());
        for p in std::mem::take(&mut self.pending) {
            self.published = p.version;
            out.push(Published { applied: p.applied });
        }
        Ok(out)
    }

    /// A view of the published version (section 6.6).
    pub fn view(&self) -> Result<View, WriteError> {
        self.usable()?;
        Ok(View {
            pages: self.pages.clone(),
            version: self.published.clone(),
        })
    }

    /// A view of restore point `name`, read from its meta stream.
    pub fn restore_point(&self, name: &str) -> Result<View, WriteError> {
        self.usable()?;
        let point = self
            .pages
            .restore_points()
            .into_iter()
            .find(|(rp, dropped)| rp.name == name && !dropped)
            .map(|(rp, _)| rp)
            .ok_or(WriteError::Rule("no restore point of that name is listed"))?;
        let pin = pin_version(&self.pages, PinOf::OnDisk(point.entry.index))
            .map_err(WriteError::Unreadable)?;
        Ok(View {
            pages: self.pages.clone(),
            version: Version {
                position: Position {
                    index: point.entry.index,
                    term: point.entry.term,
                    hash: point.entry.hash,
                },
                pin: Arc::new(pin),
            },
        })
    }

    /// The restore points listed, by name, with their entries.
    pub fn restore_points(&self) -> Vec<RestorePoint> {
        self.pages
            .restore_points()
            .into_iter()
            .filter(|(_, dropped)| !dropped)
            .map(|(rp, _)| rp)
            .collect()
    }

    /// Stops keeping restore point `name` (section 11); its pages are freed
    /// once no checkpoint on disk lists it, and the log it needed is
    /// retired by the next checkpoint.
    pub fn drop_restore_point(&mut self, name: &str) -> Result<(), WriteError> {
        self.usable()?;
        self.pages
            .drop_restore_point(name)
            .map_err(WriteError::Rule)
    }

    /// Takes a checkpoint of the published entry (section 6.7), kept as
    /// restore point `keep_as` when given, then retires the log up to what
    /// the oldest checkpoint or restore point kept still needs, and frees
    /// what the page file can. Refused while entries are applied past the
    /// published one: flush first. A failed write stops the store.
    pub fn checkpoint(&mut self, keep_as: Option<&str>) -> Result<Checkpoint, CheckpointFailed> {
        if let Some(why) = &self.stopped {
            return Err(CheckpointFailed::Stopped(why.clone()));
        }
        if !self.pending.is_empty() {
            return Err(CheckpointFailed::Refused(
                "entries are applied past the published one: flush first".into(),
            ));
        }
        let entry = self.published.position;
        let written = match self.pages.checkpoint(entry_id(&entry), keep_as) {
            Ok(written) => written,
            Err(e @ (CheckpointError::Disk(_) | CheckpointError::Failed)) => {
                let why = format!("a checkpoint failed: {e}");
                self.stopped = Some(why.clone());
                return Err(match e {
                    CheckpointError::Disk(d) if d.kind() == ErrorKind::NoSpace => {
                        CheckpointFailed::NoSpace(why)
                    }
                    _ => CheckpointFailed::Failed(why),
                });
            }
            Err(e @ CheckpointError::Space(_)) => {
                return Err(CheckpointFailed::NoSpace(e.to_string()))
            }
            Err(e) => return Err(CheckpointFailed::Refused(e.to_string())),
        };
        let retired = match self.log.retire(self.oldest_needed()) {
            Ok(n) => n,
            Err(e) => {
                let why = format!("the log's retirement failed: {e}");
                self.stopped = Some(why.clone());
                return Err(if log_no_space(&e) {
                    CheckpointFailed::NoSpace(why)
                } else {
                    CheckpointFailed::Failed(why)
                });
            }
        };
        let reclaimed: Reclaimed = self.pages.reclaim(RECLAIM_AFTER_CHECKPOINT);
        Ok(Checkpoint {
            written,
            entry,
            retired,
            examined: reclaimed.examined,
            freed: reclaimed.freed,
        })
    }

    /// The oldest entry the log must keep: the one after the oldest
    /// checkpoint or restore point a superblock on disk names (section 6.7,
    /// "What is kept"). A dropped restore point counts until no superblock
    /// on disk lists it: an open that falls back to the older checkpoint
    /// lists it again.
    fn oldest_needed(&self) -> u64 {
        let checkpoints = self
            .pages
            .kept_checkpoints()
            .into_iter()
            .map(|c| c.entry.index);
        let points = self
            .pages
            .restore_points()
            .into_iter()
            .map(|(rp, _)| rp.entry.index);
        checkpoints
            .chain(points)
            .min()
            .unwrap_or(self.published.position.index)
            .saturating_add(1)
    }

    /// Frees what the page file can among `budget` dying pages.
    pub fn reclaim(&mut self, budget: usize) -> Result<Reclaimed, WriteError> {
        self.usable()?;
        Ok(self.pages.reclaim(budget))
    }

    /// The store folder, for the caller that reopens it.
    pub fn fs(&self) -> &Arc<dyn Fs> {
        &self.fs
    }

    /// How the store was set up.
    pub fn config(&self) -> Config {
        self.config
    }
}
