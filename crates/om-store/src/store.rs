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
//! explicit "accept the loss" calls ([`Store::accept_damage`],
//! [`Store::give_up_unreadable_end`]) are the only way past a damaged log
//! and an unreadable end.

#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;

use crate::apply::{self, Applied, Position, StoreTree, TreeId};
use crate::entry::{entry_hash, Entry, Payload, PayloadError, ServerId, SessionEvent};
use crate::log::{self, CutDone, DamageAccepted, Log, LogError, UnreadableEnd};
use crate::page::{Hash, StoreError, TreeName};
use crate::pagefile::{
    self, CheckpointError, Checkpointed, EntryId, PageFile, Pin, Purpose, Reclaimed, RestorePoint,
    SpaceError,
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
    /// A failure after the entry's unit committed, or of a flush: the store
    /// has stopped (the reason). Its outcome is unknown to the caller:
    /// open the store again, and the durable log says.
    Failed(String),
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
            WriteError::Failed(why) => write!(
                f,
                "the store stopped: {why}; what was not flushed has an unknown outcome, and \
                 the store must be opened again"
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
}

impl fmt::Display for CheckpointFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointFailed::Stopped(why) => {
                write!(f, "the store stopped earlier ({why}); open it again")
            }
            CheckpointFailed::Refused(why) => write!(f, "checkpoint refused: {why}"),
            CheckpointFailed::Failed(why) => write!(f, "the store stopped: {why}"),
        }
    }
}

impl std::error::Error for CheckpointFailed {}

/// Why the store could not be opened (section 6.12), with what the
/// administrator can do ([`OpenError::remedy`]). Nothing the open could not
/// do is worked around.
#[derive(Debug)]
pub enum OpenError {
    /// The folder holds no store: no `state` (create one).
    Missing,
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
    /// An entry the log holds cannot be applied again: the store cannot
    /// produce the state it confirmed (a page that does not check, or an
    /// entry that cannot be right).
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
}

impl OpenError {
    /// What the administrator can do (section 6.12 and 11).
    pub fn remedy(&self) -> &'static str {
        match self {
            OpenError::Missing => "create the store",
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
            OpenError::Pages(_) => "restore the store from a backup",
            OpenError::LogDamaged(_) => {
                "restore the store from a backup, or accept the loss of what the log no longer \
                 holds (Store::accept_damage)"
            }
            OpenError::UnreadableEnd { .. } => {
                "open the store again later, restore it from a backup, or accept the loss of what \
                 may follow the place (Store::give_up_unreadable_end)"
            }
            OpenError::Log(LogError::Unreadable { .. }) => {
                "open the store again later, or restore it from a backup"
            }
            OpenError::Log(_) => "open the store again, or restore it from a backup",
            OpenError::Replay { .. } => "restore the store from a backup",
            OpenError::Unreadable(_) => "open the store again later, or restore it from a backup",
            OpenError::NoSpace(_) => "free space on the disk, then open the store again",
            OpenError::Start(_) => "open the store again",
            OpenError::Refused(_) => "a defect of the caller",
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Missing => f.write_str("the folder holds no store"),
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
        }?;
        write!(f, " ({})", self.remedy())
    }
}

impl std::error::Error for OpenError {}

/// Why this start's own entry was not written ([`OpenError::Start`]).
#[derive(Debug)]
pub struct StartFailed {
    /// The entry, when the log took it before the failure: a later open
    /// may find it durable.
    pub written: Option<(Position, Payload)>,
    pub error: WriteError,
}

/// What was given up to open the store, by one of the two explicit calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GaveUp {
    /// The loss after damage through the marker ([`Store::accept_damage`]).
    Damage(DamageAccepted),
    /// Whatever may follow an unreadable end
    /// ([`Store::give_up_unreadable_end`]).
    UnreadableEnd(CutDone),
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
    /// Checkpoints the replay took, its pages waiting for one past their
    /// budget (format section 4.8).
    pub replay_checkpoints: u64,
    /// The entry published after the replay: the durable one.
    pub recovered: Position,
    /// This start's own `session` entry, published.
    pub started: Position,
    /// The copy of `state` this open passed over, damaged or unreadable,
    /// when it did.
    pub passed_over_state: Option<SlotFault>,
    /// A copy of `state` was passed over and the log held no whole batch
    /// after the marker: the marker can have been one advance behind, and
    /// the last batch this server confirmed may be lost (section 6.12).
    pub last_batch_may_be_lost: bool,
    /// What an explicit call gave up to open the store.
    pub gave_up: Option<GaveUp>,
}

/// How an open treats the log.
#[derive(Clone, Copy, Debug)]
enum How {
    Normal,
    AcceptDamage,
    GiveUp(log::Position),
}

/// A view of a published version (section 6.6): its entry, and the roots of
/// its trees, kept by a pin for as long as the view lives. It holds the
/// page file, not the store.
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

    /// The version's trees, by name, with their roots.
    pub fn roots(&self) -> &[(TreeName, Root)] {
        self.version.pin.roots()
    }

    /// The root of one of the state's trees.
    pub fn root(&self, tree: TreeId) -> Option<Root> {
        let name = tree.name();
        self.roots()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
    }

    /// The page file the view reads from. Its pages are checked as they are
    /// read: a page that does not check is an error, never returned.
    pub fn pages(&self) -> &PageFile {
        &self.pages
    }

    /// The store's own tree as the view holds it (`None` before entry 1).
    pub fn store_tree(&self) -> Result<Option<StoreTree<'_, PageFile>>, apply::Error> {
        match self.root(TreeId::Store) {
            Some(root) => StoreTree::open(&*self.pages, root).map(Some),
            None => Ok(None),
        }
    }

    /// A file's tree as the view holds it, if the file exists.
    pub fn file_tree(&self, id: u64) -> Result<Option<Tree<'_, PageFile>>, tree::Error> {
        match self.root(TreeId::File(id)) {
            Some(root) => Tree::open(&*self.pages, root).map(Some),
            None => Ok(None),
        }
    }
}

/// The store on one server (module docs).
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
fn pin_version(pages: &PageFile, of: PinOf) -> Result<Pin, String> {
    let (pin, entry) = match of {
        PinOf::Newest(entry) => (pages.pin_newest(), entry),
        PinOf::OnDisk(entry) => (pages.pin(entry), entry),
    };
    let pin =
        pin.map_err(|why| format!("the version of entry {entry} could not be pinned: {why}"))?;
    if pin.entry() != entry {
        return Err(format!(
            "a pin for entry {entry} holds entry {}'s version",
            pin.entry()
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

/// Why an entry could not be applied, before anything of it took effect.
enum NotApplied {
    Space(SpaceError),
    Apply(apply::Error),
}

/// Applies `entry` through a reservation of the page file, extended while
/// the entry runs short (the unit that ran short changed nothing, and is
/// begun again): the entry's unit is committed when this returns `Ok`.
fn apply_entry(
    pages: &PageFile,
    entry: &Entry<'_>,
    purpose: Purpose,
) -> Result<(Applied, u32), NotApplied> {
    let res = pages
        .reserve(entry.index, FIRST_RESERVATION, purpose)
        .map_err(NotApplied::Space)?;
    let mut extended = 0u32;
    let mut size = FIRST_RESERVATION;
    loop {
        match apply::apply(&res, entry) {
            Ok(applied) => return Ok((applied, extended)),
            Err(apply::Error::Tree(tree::Error::Store(StoreError::Full))) => {
                res.extend(size).map_err(NotApplied::Space)?;
                size = size.saturating_mul(2);
                extended += 1;
            }
            Err(e) => return Err(NotApplied::Apply(e)),
        }
    }
}

impl Store {
    /// Makes a new store in a folder that holds none, and opens it: `state`
    /// with a new server id drawn from `random`, the page file's first
    /// checkpoint, an empty log, and this start's `session` entry. A page
    /// file left by a creation that did not reach `state` is replaced
    /// (without `state` nothing was ever confirmed); a folder with `state`
    /// holds a store already ([`OpenError::Exists`]).
    pub fn create(
        fs: Arc<dyn Fs>,
        random: &dyn Random,
        config: Config,
    ) -> Result<(Store, Opened), OpenError> {
        let disk = |e: sys::Error| OpenError::Disk(e.to_string());
        let lock = fs.lock().map_err(disk)?;
        match fs.open(crate::state::FILE, sys::FileOptions::PLAIN) {
            Ok(_) => return Err(OpenError::Exists),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(disk(e)),
        }
        // The page file first: `state` last, so that a creation cut short
        // leaves no `state`, and is begun again.
        match fs.remove(pagefile::FILE) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(disk(e)),
        }
        drop(PageFile::create(&*fs, &lock, config.pages).map_err(OpenError::Pages)?);
        drop(State::create(&*fs, random, lock).map_err(OpenError::State)?);
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
    /// administrator's command of section 6.12 on a single server
    /// ([`Log::accept_damage`]). What was given up is in
    /// [`Opened::gave_up`]. Refused when there is no such loss.
    pub fn accept_damage(fs: Arc<dyn Fs>, config: Config) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::AcceptDamage)
    }

    /// Opens a store whose log ends at a place it cannot read, giving up
    /// whatever may follow that place: the administrator's command of
    /// section 6.12 on a single server ([`Log::give_up_unreadable_end`]).
    /// `last` is the last entry the open verified, as
    /// [`OpenError::UnreadableEnd`] reported it; the call is refused when
    /// the log does not end so at that entry.
    pub fn give_up_unreadable_end(
        fs: Arc<dyn Fs>,
        config: Config,
        last: log::Position,
    ) -> Result<(Store, Opened), OpenError> {
        Store::start(fs, config, How::GiveUp(last))
    }

    fn start(fs: Arc<dyn Fs>, config: Config, how: How) -> Result<(Store, Opened), OpenError> {
        // 1. The lock and `state`.
        let lock = fs.lock().map_err(|e| OpenError::Disk(e.to_string()))?;
        let state = match State::open(&*fs, lock) {
            Ok(state) => state,
            Err(StateError::Missing) => return Err(OpenError::Missing),
            Err(e) => return Err(OpenError::State(e)),
        };
        let passed_over_state = state.opened().and_then(|o| o.other.err());
        let values = *state.values();
        // 2. The page file's checkpoint.
        let (pages, checkpoint) =
            PageFile::open(&*fs, state.folder_lock(), config.pages).map_err(OpenError::Pages)?;
        let start = checkpoint.entry;
        let start = log::Position {
            index: start.index,
            term: start.term,
            hash: start.hash.0,
        };
        // 3. The log from that checkpoint, checked against the marker.
        let failed = |failed: Box<log::OpenFailed>| match failed.error {
            LogError::Damaged(d) => OpenError::LogDamaged(d),
            e => OpenError::Log(e),
        };
        let (mut log, log_opened, gave_up) = match how {
            How::AcceptDamage => {
                let (log, opened, accepted) =
                    Log::accept_damage(fs.clone(), state, config.log, start).map_err(failed)?;
                (log, opened, Some(GaveUp::Damage(accepted)))
            }
            How::Normal | How::GiveUp(_) => {
                let (log, opened) =
                    Log::open(fs.clone(), state, config.log, start).map_err(failed)?;
                (log, opened, None)
            }
        };
        let mut log_opened = log_opened;
        let mut gave_up = gave_up;
        match (how, log_opened.unreadable_end) {
            (How::GiveUp(named), Some(_)) => {
                let done = log
                    .give_up_unreadable_end(named.index, named.hash)
                    .map_err(OpenError::Log)?;
                log_opened.unreadable_end = None;
                gave_up = Some(GaveUp::UnreadableEnd(done));
            }
            (How::GiveUp(_), None) => {
                return Err(OpenError::Refused(
                    "the log does not end at a place it cannot read",
                ))
            }
            (_, Some(end)) => {
                return Err(OpenError::UnreadableEnd {
                    end,
                    last: log.durable(),
                })
            }
            (_, None) => {}
        }
        // A copy of `state` passed over, and no whole batch after the
        // marker to make up for the advance it can have held
        // (server-storage.md 6.12 as revised for lost acknowledgements).
        let last_batch_may_be_lost = passed_over_state.is_some() && log_opened.kept == 0;
        let pages = Arc::new(pages);
        // 4. The entries after the checkpoint, each once, in order, through
        // the durable entry; nothing freed in the log meanwhile.
        let durable = log.durable();
        let mut replayed = 0u64;
        let mut replay_checkpoints = 0u64;
        let mut last = position_of(start);
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
        let pin = pin_version(&pages, PinOf::Newest(last.index)).map_err(OpenError::Disk)?;
        let term = values.term.max(durable.term).max(1);
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
        };
        // 5. This start's own entry (section 7.3), the first write.
        let start_count = store.log.state().values().start_count;
        let payload = Payload::Session(SessionEvent::Started {
            server: store.server,
            count: start_count,
        });
        let started = match store.append(0, &payload) {
            Ok(appended) => appended.position,
            Err(error) => {
                return Err(OpenError::Start(Box::new(StartFailed {
                    written: None,
                    error,
                })))
            }
        };
        if let Err(error) = store.flush() {
            return Err(OpenError::Start(Box::new(StartFailed {
                written: Some((started, payload)),
                error,
            })));
        }
        Ok((
            store,
            Opened {
                start_count,
                checkpoint,
                log: log_opened,
                replayed,
                replay_checkpoints,
                recovered: last,
                started,
                passed_over_state,
                last_batch_may_be_lost,
                gave_up,
            },
        ))
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

    /// The term entries are written in.
    pub fn term(&self) -> u64 {
        self.term
    }

    /// The page file, for reading its counts.
    pub fn pages(&self) -> &PageFile {
        &self.pages
    }

    /// The log's segments in use, as `state` lists them.
    pub fn log_segments(&self) -> crate::state::Segments {
        self.log.segments()
    }

    /// Appends an entry with this payload, of file `file` (0 for none):
    /// checks it, applies it to the working state and adds it to the log's
    /// open batch (module docs). It is published by the next flush.
    pub fn append(&mut self, file: u64, payload: &Payload) -> Result<Appended, WriteError> {
        self.usable()?;
        let bytes = payload.encode().map_err(WriteError::Payload)?;
        let kind = payload.kind().code();
        let term = self.term;
        // Whatever the log would refuse is refused now, before the unit.
        match self.log.ready_for(term, bytes.len()) {
            Ok(()) => {}
            Err(e) if self.log.is_poisoned() => return Err(self.stop(e.to_string())),
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
        let (applied, extended) = match apply_entry(&self.pages, &entry, Purpose::Entry) {
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
            Err(why) => return Err(self.stop(why)),
        };
        match self.log.append(term, kind, file, &bytes) {
            Ok(p) if p.index == index && p.hash == hash.0 => {}
            Ok(_) => {
                return Err(self.stop(
                    "the log placed the entry elsewhere, or hashed it otherwise, than it was \
                     applied",
                ))
            }
            Err(e) => {
                return Err(self.stop(format!("the log refused an entry already applied: {e}")))
            }
        }
        let position = applied.position;
        self.pending.push(Unpublished {
            version: Version {
                position,
                pin: Arc::new(pin),
            },
            applied,
        });
        Ok(Appended { position, extended })
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
            Err(e) => return Err(self.stop(format!("the log's flush failed: {e}"))),
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
            .map_err(WriteError::Failed)?;
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
                return Err(CheckpointFailed::Failed(why));
            }
            Err(e) => return Err(CheckpointFailed::Refused(e.to_string())),
        };
        let retired = match self.log.retire(self.oldest_needed()) {
            Ok(n) => n,
            Err(e) => {
                let why = format!("the log's retirement failed: {e}");
                self.stopped = Some(why.clone());
                return Err(CheckpointFailed::Failed(why));
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
    /// checkpoint on disk or restore point listed (section 6.7, "What is
    /// kept").
    fn oldest_needed(&self) -> u64 {
        let checkpoints = self
            .pages
            .kept_checkpoints()
            .into_iter()
            .map(|c| c.entry.index);
        let points = self.restore_points().into_iter().map(|rp| rp.entry.index);
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
