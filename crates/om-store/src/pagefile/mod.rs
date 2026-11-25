//! The page file (`docs/server-storage.md` sections 6.4 to 6.8): where the
//! trees keep their pages, the page cache, the map of free pages, and
//! checkpoints. The byte formats are section 4 of
//! `docs/server-storage-format.md`.
//!
//! `<store>/pages` begins with four superblock slots of 4 KiB (the first
//! 16 KiB, "page 0"); page `n` follows at byte `n * 16384`. A checkpoint
//! writes the pages of the state that are not on disk yet and a **meta
//! stream** of its own (the roots of the state's trees by name, the
//! restore points, and the map of free pages), flushes them, then writes
//! its superblock copy into both slots of the pair the newest checkpoint
//! does not hold, flushing each. The two newest checkpoints and every
//! restore point a superblock on disk lists are kept whole.
//!
//! **Changes** come through a [`Reservation`]: [`PageFile::reserve`] sets
//! aside, for one entry, the pages the caller says applying it can take,
//! and the reservation is the [`PageStore`] the trees apply the entry
//! through. The entry's one [`Unit`] carries everything it changes: every
//! tree's pages and releases, the trees it makes, drops and renames, and
//! each changed tree's new root ([`EntryPages`]). Numbers come from the
//! free pages and are held for the unit; all of it takes effect together
//! on commit, and nothing does on drop. Committed pages stay in memory
//! until a checkpoint writes them, and are served from there; they count
//! against a budget, past which a reservation is refused with
//! [`SpaceError::CheckpointNeeded`]. The page file holds each tree's name
//! and root ([`PageStore::trees`]), changed only by a unit's commit, which
//! checks every root it records; a checkpoint writes the trees as the
//! page file holds them, and the room kept for checkpoints is sized from
//! them.
//!
//! **Versions.** The page file holds one line of versions: units are
//! committed in entry order, each entry after the last checkpoint's, and a
//! checkpoint is of the newest version (every unit committed is of its
//! entry or earlier). Versions before the newest are kept only by
//! checkpoints and restore points on disk, the checkpoint being written,
//! and [`Pin`]s, which the caller takes for views, state copies and
//! readers of a restore point. A caller that applies entries before it
//! publishes them pins the published version, and checkpoints only once
//! everything applied is published (section 6.7: only published entries
//! are checkpointed).
//!
//! **Reuse.** A page leaves the newest version when a unit releases it (or
//! the subtree it was in, lazily). It is reused only when no kept version
//! reaches it (the `map` module's docs); dying pages become free when
//! [`PageFile::reclaim`] finds them so (bounded work per call), or at once
//! on commit when the page never reached the disk and no kept version
//! reaches it. Pins live in memory; after a restart, what the superblocks
//! on disk name is what is kept. Recovery frees the same way, so applying
//! the entries after a checkpoint again needs no more space than they
//! needed the first time (format section 4.8).

//! **Calls at the same time.** The page file serves one caller that
//! applies entries, in order, one entry at a time (it reserves the entry,
//! runs its one unit, and is done with it before the next; it may reserve
//! later entries ahead), one that checkpoints and reclaims, and readers at
//! any time. It enforces that contract by refusing what falls outside it,
//! and each check is made in the same hold of the `inner` lock as the
//! change it guards. Three locks order the calls: `inner`, held for short
//! steps and never across a read or write of the file; `io`, held by a
//! checkpoint for all of it, by growth of the file, by a reclaim and by a
//! restore point's drop; and the pin table.
//!
//! | Calls | Alongside? | What orders them, and the refusal |
//! |---|---|---|
//! | a read and anything | yes | a read sees committed pages only; its lookups take `inner`, its file read none |
//! | reserve and reserve | for different entries, yes; a second for an entry with one open, no | the checks, the promise of pages and the opening of the reservation are one hold of `inner`; the second is refused ([`SpaceError::Refused`]); one short of space reclaims and grows under `io`, one at a time, before that hold |
//! | a unit and a unit | no: one unit an entry, one open at a time | `inner`; a unit begun while another is open, or for an entry that has committed one, is refused ([`StoreError`]); a unit dropped or refused may be begun again |
//! | reserve, begin or commit, and a checkpoint at an earlier entry | yes; a reservation short of space waits for the checkpoint (`io`) | `inner`; a unit of a later entry committed before the checkpoint plans refuses the checkpoint ([`CheckpointError::Refused`]) |
//! | reserve, begin or commit, and a checkpoint at the same or a later entry | no | the checkpoint's first check fixes its entry, in the hold that checks it: the checkpoint is refused while a reservation for an entry it covers is open, and from then until it is installed or fails, every reservation, unit and commit at or before its entry is refused |
//! | a checkpoint that keeps a restore point, and an open reservation | no: the checkpoint is refused | the point counts in the room kept for meta streams from the first check, so a reservation opened after it is judged as a reopen on that checkpoint would judge it; one opened before was not |
//! | a checkpoint and a checkpoint, a reclaim or a restore point's drop | no: the second waits | `io`; so whether a reclaim settles a page before or after a checkpoint, and so the hold it gives, is the order of the two calls |
//! | a reclaim and a reserve, a unit, a read or a pin | yes | `inner` for each page's step; pages a reclaim frees are those no kept version reaches when it decides, under `inner` |
//! | a pin or unpin and anything | yes; a pin of an older checkpoint or a restore point waits for a checkpoint, a reclaim or a restore point's drop (`io`) | the pin table; a pin is of a version the page file has (the newest, by its entry or [`PageFile::pin_newest`]; a checkpoint or restore point on disk; one a pin keeps), and a pin of any other entry, such as one whose unit has not committed, is refused; a pin counts from when it is taken, and holds the roots of the version it pins ([`Pin::roots`]): from memory, or for an older checkpoint or a restore point from its meta stream, read while `io` is held so that no reclaim or checkpoint reuses those pages first; a pin of the version a checkpoint is writing is refused, and that checkpoint keeps its own version itself |
//! | open | nothing runs on a page file before its open returns; a second open (or creation) with the same lock is refused, writing nothing, until the first page file is dropped or its open has failed | one page file per store folder rests on two things: the store folder's lock, which the caller holds, keeps other processes out, and the lock's mark ([`FolderLock::take_page_file`]), taken from the start of an open until the page file is dropped, keeps out a second page file of the same process, which would write from its own view of the slots over the first's |
//!
//! So the caller that checkpoints has the caller that applies take no new
//! entry for the moment of a checkpoint's first check and plan (a unit of
//! a later entry committed then refuses the checkpoint), and holds no
//! reservation open when it keeps a restore point; both refusals repeat
//! for as long as the applier does not pause.
//!
//! A checkpoint clears what it fixed on every way out of it, an unwind
//! included; one left by a panic also latches the page file's failure
//! (what its plan changed in memory is not used: the page file is opened
//! again). A unit's commit checks every root it records as an open will:
//! a tree's page, not an overflow page, which the unit wrote.

mod cache;
mod map;
mod meta;
mod space;
mod superblock;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub use map::Version;
pub use meta::{MetaFault, MAX_RESTORE_NAME, MAX_RESTORE_POINTS, MAX_TREES};
pub use space::entry_bound;
pub use superblock::{pair_slots, Invalid, Superblock, COPY, SLOT, SLOTS};

use cache::Cache;
use map::{Child, Kept, Map, Released};
use meta::Meta;

use crate::page::{
    self, EntryPages, Fault, Hash, Page, PageFault, PageKind, PageNo, PageStore, Release,
    StoreError, PAGE_SIZE,
};
use crate::sys::{self, ErrorKind, File, FileOptions, FolderLock, Fs, PageFileMark};
use crate::tree::Root;

pub use crate::page::TreeName;

/// The file's name in the store folder.
pub const FILE: &str = "pages";

/// The name a new page file is written under before it is renamed into
/// place.
pub const NEW_FILE: &str = "pages.new";

const PAGE: u64 = PAGE_SIZE as u64;

/// The most pages one write of a checkpoint carries.
const RUN: usize = 64;

/// The pages a tree made or renamed takes from its reservation for its
/// record in the tables of trees: at most a data page and a directory page
/// more in each of the two meta streams the floor keeps room for.
pub const TABLE_ROOM: u64 = 4;

/// An entry of the log, as a checkpoint names the last one it includes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EntryId {
    pub index: u64,
    pub term: u64,
    pub hash: Hash,
}

/// A checkpoint kept beyond the two newest, by name (section 11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestorePoint {
    pub name: String,
    /// The checkpoint's number.
    pub checkpoint: u64,
    pub entry: EntryId,
    pub version: Version,
    /// The first page of the checkpoint's meta stream, which names its
    /// trees: a pin of the restore point reads them from it.
    pub meta: (PageNo, Hash),
}

/// What the page file is set up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The page cache's size in bytes (`--store-cache-mib`, section 11).
    pub cache_bytes: u64,
    /// The most pages of the newest version that may wait in memory for a
    /// checkpoint: once as many wait, a reservation is refused with
    /// [`SpaceError::CheckpointNeeded`] (section 6.11). The entries already
    /// reserved for can take it past the budget by what they write. Pages
    /// that left the newest version and stay in memory for a pin do not
    /// count: a checkpoint does not write them.
    pub dirty_budget: u64,
    /// The pages that only recovery, checkpoints and the discard of a
    /// group may take (section 6.7), beyond the room a checkpoint's meta
    /// stream needs.
    pub reserve: u64,
    /// The least the file grows by, in pages; it grows by an eighth of its
    /// size when that is more, up to 65,536 pages (1 GiB) a step.
    pub growth: u64,
}

impl Config {
    /// Refuses a configuration the page file cannot honour: a growth step
    /// of 1 to 65,536 pages, a budget of at least one page, and a reserve
    /// of at most 2^32 pages (64 TiB), so that no sum the page file makes
    /// with them can overflow.
    pub fn check(&self) -> Result<(), OpenError> {
        if !(1..=1 << 16).contains(&self.growth) || self.dirty_budget == 0 || self.reserve > 1 << 32
        {
            return Err(OpenError::Refused(
                "a configuration the page file cannot honour (growth of 1 to 65,536 pages, a \
                 budget of at least one page, a reserve of at most 2^32 pages)",
            ));
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cache_bytes: 256 << 20,
            dirty_budget: 16_384,
            reserve: 256,
            growth: 256,
        }
    }
}

/// What a reservation is for, which decides whether it may draw on the
/// reserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// An entry being appended: the reserve is left alone.
    Entry,
    /// An entry applied again after a restart (section 6.7, step 4), or
    /// one another server appended: it cannot be refused, so it may take
    /// the reserve.
    Recovery,
    /// The entry that discards a group (section 6.9).
    Discard,
}

/// Why space could not be reserved. Nothing was reserved, and the store is
/// still readable and can checkpoint.
#[derive(Debug)]
pub enum SpaceError {
    /// The disk is full: the file could not grow enough. `available` is
    /// what the purpose may take.
    Full { wanted: u64, available: u64 },
    /// Too many pages wait in memory for a checkpoint: checkpoint, then
    /// reserve again (section 6.11).
    CheckpointNeeded { dirty: u64, budget: u64 },
    /// An earlier write or flush of the page file failed; the store must
    /// stop and be opened again (section 6.12).
    Failed,
    /// The file could not grow for another reason.
    Disk(sys::Error),
    /// The call breaks a rule.
    Refused(&'static str),
}

impl fmt::Display for SpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpaceError::Full { wanted, available } => write!(
                f,
                "the disk is full: {wanted} pages wanted, {available} available"
            ),
            SpaceError::CheckpointNeeded { dirty, budget } => write!(
                f,
                "{dirty} pages wait for a checkpoint, at a budget of {budget}"
            ),
            SpaceError::Failed => f.write_str(FAILED),
            SpaceError::Disk(e) => write!(f, "{e}"),
            SpaceError::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

impl std::error::Error for SpaceError {}

const FAILED: &str = "an earlier write or flush of the page file failed; the store must stop and \
                      be opened again";

/// Why a checkpoint was not made.
#[derive(Debug)]
pub enum CheckpointError {
    /// The call breaks a rule; nothing was written.
    Refused(&'static str),
    /// A tree's root on disk does not read back; nothing was written.
    Root { tree: TreeName, fault: PageFault },
    /// No space for the checkpoint's meta stream; nothing was written.
    Space(SpaceError),
    /// A write or flush failed. What reached the disk is unknown, and the
    /// page file refuses further writes: the store stops and is opened
    /// again (section 6.12).
    Disk(sys::Error),
    /// An earlier write or flush failed.
    Failed,
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::Refused(why) => write!(f, "checkpoint refused: {why}"),
            CheckpointError::Root { tree, fault } => {
                write!(f, "the root of tree {tree} does not read back: {fault}")
            }
            CheckpointError::Space(e) => write!(f, "{e}"),
            CheckpointError::Disk(e) => write!(f, "{e}"),
            CheckpointError::Failed => f.write_str(FAILED),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// What a superblock slot held when the file was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotSeen {
    /// A valid copy of this checkpoint.
    Copy(u64),
    Invalid(Invalid),
    /// The read failed.
    Unreadable(ErrorKind),
}

/// Why a checkpoint whose superblock copy is valid was not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Its meta stream cannot be read, or breaks the format.
    Meta(MetaFault),
    /// The root of one of its trees does not read back with the hash it
    /// names.
    Root { tree: TreeName, fault: PageFault },
    /// Its meta stream does not agree with its superblock, or describes a
    /// map no checkpoint writes.
    Mismatch(&'static str),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::Meta(fault) => write!(f, "its meta stream: {fault}"),
            Rejection::Root { tree, fault } => write!(f, "the root of tree {tree}: {fault}"),
            Rejection::Mismatch(why) => write!(f, "{why}"),
        }
    }
}

/// A newer checkpoint than the one opened, why it was not used, and the
/// restore points given up with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassedOver {
    pub number: u64,
    pub why: Rejection,
    pub lost: Lost,
}

impl fmt::Display for PassedOver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "checkpoint {}: {}; giving it up loses {}",
            self.number, self.why, self.lost
        )
    }
}

/// The restore points given up with a checkpoint passed over, or that
/// would be: those it lists that were kept after the checkpoint the open
/// falls back to, which cannot list them. Each checkpoint lists the points
/// of the one before it that were not dropped, and the one it keeps, so
/// that is exactly what it lists and the one fallen back to does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lost {
    /// Its table of restore points was read: those points (name, and the
    /// number of the checkpoint each was kept at). Often none.
    Points(Vec<(String, u64)>),
    /// Its table of restore points could not be read: points kept after
    /// the checkpoint fallen back to, if any, are lost.
    Unknown,
}

impl fmt::Display for Lost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lost::Points(points) if points.is_empty() => f.write_str("no restore point"),
            Lost::Points(points) => {
                f.write_str("restore points ")?;
                for (i, (name, at)) in points.iter().enumerate() {
                    let sep = if i == 0 { "" } else { ", " };
                    write!(f, "{sep}{name:?} (kept at checkpoint {at})")?;
                }
                Ok(())
            }
            Lost::Unknown => f.write_str(
                "the restore points kept after the checkpoint fallen back to, which cannot be \
                 known (its table was not read)",
            ),
        }
    }
}

/// A checkpoint as an open saw it: its number, and the first page and hash
/// of its meta stream from its superblock copy. The hash covers the whole
/// stream (its number, entry, trees, restore points and map), so two
/// checkpoints of one number, as when a number comes again after the
/// checkpoint that had it was given up, have different identities unless
/// their contents are the same in every byte, at the same pages; then they
/// describe the same loss and the same landing, and nothing tells them
/// apart or needs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointId {
    pub number: u64,
    pub meta: (PageNo, Hash),
}

impl CheckpointId {
    fn of(sb: &Superblock) -> CheckpointId {
        CheckpointId {
            number: sb.number,
            meta: sb.meta,
        }
    }
}

impl fmt::Display for CheckpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "checkpoint {} (meta stream at page {}, {})",
            self.number, self.meta.0, self.meta.1
        )
    }
}

/// What a caller approves [`PageFile::open_giving_up`] to do: exactly what
/// an error of [`PageFile::open`] (or of that call) showed
/// ([`OpenError::give_up`]), with what goes with it. The call does exactly
/// this or writes nothing; whatever a caller records of it before the call,
/// it has recorded the loss, however many things were wrong at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GiveUp {
    /// The checkpoints given up, newest first, each with the restore points
    /// that go with it.
    pub checkpoints: Vec<GivenUp>,
    /// The superblock slots the device fails to read, written over unread:
    /// whatever each held goes, and with it any restore point kept after
    /// `onto` that only it listed.
    pub slots: Vec<usize>,
    /// The checkpoint the open is then on.
    pub onto: CheckpointId,
}

/// A checkpoint an approval gives up, and the restore points that go with
/// it: those it lists that were kept after the checkpoint landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GivenUp {
    pub checkpoint: CheckpointId,
    pub lost: Lost,
}

impl fmt::Display for GiveUp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("give up")?;
        for c in &self.checkpoints {
            write!(f, " {}, losing {};", c.checkpoint, c.lost)?;
        }
        for s in &self.slots {
            write!(
                f,
                " slot {s}, unread, losing whatever it held and any restore point kept after \
                 checkpoint {} that only it listed;",
                self.onto.number
            )?;
        }
        write!(f, " and open on {}", self.onto)
    }
}

/// The restore points lost with a checkpoint whose own table listed
/// `points` (`None` when it could not be read) when the open falls back
/// to checkpoint `below` (0 for none).
fn lost_after(points: Option<Vec<RestorePoint>>, below: u64) -> Lost {
    match points {
        Some(points) => Lost::Points(
            points
                .into_iter()
                .filter(|rp| rp.checkpoint > below)
                .map(|rp| (rp.name, rp.checkpoint))
                .collect(),
        ),
        None => Lost::Unknown,
    }
}

/// A read the device failed while an open chose a checkpoint (section
/// 6.7, step 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// Superblock slot `slot`, which could hold a copy of a checkpoint
    /// newer than any the open could read.
    Slot { slot: usize, error: String },
    /// A page of checkpoint `checkpoint`'s meta stream.
    Meta {
        checkpoint: u64,
        fault: Box<PageFault>,
    },
    /// The root of tree `tree` of checkpoint `checkpoint`, with what
    /// giving that checkpoint up would lose (its table was read). Giving up
    /// a checkpoint whose meta stream the device fails to read loses
    /// [`Lost::Unknown`].
    Root {
        checkpoint: u64,
        tree: TreeName,
        fault: Box<PageFault>,
        lost: Lost,
    },
}

impl fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFailure::Slot { slot, error } => write!(f, "superblock slot {slot}: {error}"),
            ReadFailure::Meta { checkpoint, fault } => write!(
                f,
                "the meta stream of checkpoint {checkpoint}: {fault}; giving it up loses {}",
                Lost::Unknown
            ),
            ReadFailure::Root {
                checkpoint,
                tree,
                fault,
                lost,
            } => write!(
                f,
                "the root of tree {tree} of checkpoint {checkpoint}: {fault}; giving it up loses \
                 {lost}"
            ),
        }
    }
}

/// What an open found and did (section 6.7, step 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The checkpoint used.
    pub checkpoint: u64,
    pub entry: EntryId,
    /// What each slot held.
    pub slots: [SlotSeen; SLOTS],
    /// Newer checkpoints passed over, why, and what was lost with each:
    /// only [`PageFile::open_giving_up`] passes one over.
    pub passed_over: Vec<PassedOver>,
    /// The checkpoint before it, still on disk and kept.
    pub predecessor: Option<u64>,
    /// Slots written with the copy of the checkpoint used or of its
    /// predecessor, so that each has two copies on the device.
    pub rewritten: Vec<usize>,
    /// Slots cleared: copies of a checkpoint passed over, or older than the
    /// predecessor.
    pub cleared: Vec<usize>,
    /// The restore points kept: name, and the number of the checkpoint
    /// each was taken at, which is what identifies it.
    pub restore_points: Vec<(String, u64)>,
    /// The file was shorter than the checkpoint's map says, and was
    /// extended.
    pub extended: bool,
    /// Slots the device failed to read that the open wrote over although
    /// they could have held a checkpoint newer than the one opened (only
    /// [`PageFile::open_giving_up`] does that): whatever they held is
    /// given up, with any restore point only it listed.
    pub given_up_slots: Vec<usize>,
}

/// Why the page file could not be opened or created.
#[derive(Debug)]
pub enum OpenError {
    /// The store folder has no `pages`.
    Missing,
    /// `pages` exists (creation).
    Exists,
    /// No slot holds a checkpoint that can be used (an open with no valid
    /// copy, or a call that gave up every checkpoint it tried): what each
    /// slot held, and why each checkpoint with a valid copy was passed
    /// over. Nothing was written.
    NoCheckpoint {
        slots: [SlotSeen; SLOTS],
        passed_over: Vec<PassedOver>,
    },
    /// A copy is of a superblock format newer than this code reads.
    NewerFormat {
        version: u16,
    },
    /// A checkpoint's trees are of a tree format newer than this code
    /// reads.
    NewerTrees {
        version: u8,
    },
    /// A valid copy has the highest checkpoint number there is: the next
    /// checkpoint could not have a higher one. Nothing was written.
    NumbersExhausted,
    /// The device failed a read the open needed to choose a checkpoint.
    /// Nothing was written and every slot is as it was: a read the device
    /// fails says nothing of the bytes, which a later read may return
    /// whole. Open again later; if the read keeps failing,
    /// [`PageFile::open_giving_up`], given `give_up`, gives up what cannot
    /// be read (`None`: nothing older could be opened).
    Unreadable {
        failure: ReadFailure,
        give_up: Option<Box<GiveUp>>,
        fallback: Option<Box<PassedOver>>,
    },
    /// The newest checkpoint with a valid copy does not check (its meta
    /// stream or a root): which, why, and what giving it up would lose.
    /// Nothing was written. [`PageFile::open_giving_up`], given `give_up`,
    /// gives it up (`None`: nothing older could be opened).
    ///
    /// For these two errors, what a caller records durably before it calls
    /// [`PageFile::open_giving_up`] is the structure (the text of the error
    /// says all of it too): which checkpoint or slot, why, and the approval
    /// with everything it would give up and lose. An approval is offered
    /// only when its landing opens; otherwise `give_up` is `None`, and
    /// `fallback` says why the checkpoint the open would fall back to
    /// cannot be used either (`None` when there is none).
    Damaged {
        passed: Box<PassedOver>,
        give_up: Option<Box<GiveUp>>,
        fallback: Option<Box<PassedOver>>,
    },
    Disk(sys::Error),
    Refused(&'static str),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Missing => f.write_str("the store folder has no page file"),
            OpenError::Exists => f.write_str("the store folder has a page file already"),
            OpenError::NoCheckpoint { slots, passed_over } => {
                write!(
                    f,
                    "no checkpoint in the page file can be used (slots: {slots:?}"
                )?;
                for p in passed_over {
                    write!(f, "; checkpoint {}: {}", p.number, p.why)?;
                }
                f.write_str(")")
            }
            OpenError::NewerFormat { version } => write!(
                f,
                "the page file's superblock has format version {version}, newer than this \
                 server reads ({})",
                superblock::VERSION
            ),
            OpenError::NewerTrees { version } => write!(
                f,
                "the page file holds trees of format version {version}, newer than this \
                 server reads ({})",
                page::FORMAT_VERSION
            ),
            OpenError::NumbersExhausted => f.write_str(
                "a superblock copy has the highest checkpoint number there is, so no checkpoint \
                 could follow it",
            ),
            OpenError::Unreadable {
                failure,
                give_up,
                fallback,
            } => {
                write!(
                    f,
                    "the device failed a read the open needs, so nothing was written: {failure}"
                )?;
                write_give_up(f, give_up.as_deref(), fallback.as_deref())
            }
            OpenError::Damaged {
                passed,
                give_up,
                fallback,
            } => {
                write!(
                    f,
                    "a checkpoint does not check, so nothing was written: {passed}"
                )?;
                write_give_up(f, give_up.as_deref(), fallback.as_deref())
            }
            OpenError::Disk(e) => write!(f, "{e}"),
            OpenError::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

fn write_give_up(
    f: &mut fmt::Formatter<'_>,
    give_up: Option<&GiveUp>,
    fallback: Option<&PassedOver>,
) -> fmt::Result {
    match (give_up, fallback) {
        (Some(give_up), _) => write!(f, "; to open without it: {give_up}"),
        (None, Some(fallback)) => {
            write!(f, "; nothing older can be opened either: {fallback}")
        }
        (None, None) => f.write_str("; there is nothing older to open"),
    }
}

impl std::error::Error for OpenError {}

impl OpenError {
    /// The approval that gets past this error: what
    /// [`PageFile::open_giving_up`] would give up, and the checkpoint it
    /// would then be on. `None` for another error, or when nothing older
    /// could be opened.
    pub fn give_up(&self) -> Option<&GiveUp> {
        match self {
            OpenError::Unreadable { give_up, .. } | OpenError::Damaged { give_up, .. } => {
                give_up.as_deref()
            }
            _ => None,
        }
    }
}

impl From<sys::Error> for OpenError {
    fn from(e: sys::Error) -> Self {
        OpenError::Disk(e)
    }
}

/// Why the bytes of a page could not be read, apart from bytes that do not
/// verify.
#[derive(Debug)]
pub enum RawRead {
    /// The number is 0 or beyond the file's pages.
    Beyond,
    /// The file ends inside the page.
    Short { read: usize },
    /// The read failed.
    Io(sys::Error),
}

impl RawRead {
    fn fault(self) -> Fault {
        match self {
            RawRead::Beyond => Fault::Missing,
            RawRead::Short { read } => Fault::Io(format!(
                "short read: the file ends {read} bytes into the page"
            )),
            RawRead::Io(e) => Fault::Io(e.to_string()),
        }
    }
}

/// What a checkpoint wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpointed {
    pub number: u64,
    /// Pages of the trees.
    pub pages: u64,
    /// Pages of its meta stream.
    pub meta_pages: u64,
    /// Bytes written, superblock copies included.
    pub bytes: u64,
}

/// What a call of [`PageFile::reclaim`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Dying pages looked at.
    pub examined: u64,
    /// Pages read to learn their birth or what they refer to.
    pub read: u64,
    pub freed: u64,
    /// Dying pages that could not be read: they stay dying, and what they
    /// refer to stays in use.
    pub faults: Vec<PageFault>,
    /// Pages found released twice (by a unit and as part of a subtree
    /// dropped): a defect of the caller, recorded and not freed twice.
    pub twice: Vec<PageNo>,
    /// Every dying page is settled and kept by a version still in use:
    /// nothing more can be freed until a version is let go.
    pub drained: bool,
}

/// The page file's counts, in pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Space {
    /// Pages `1..end` exist.
    pub end: u64,
    pub free: u64,
    /// Left the newest version, not free yet.
    pub dying: u64,
    /// Waiting in memory for a checkpoint.
    pub dirty: u64,
    /// Held by units that have not committed.
    pub held: u64,
    /// Set aside for reservations and not taken yet.
    pub promised: u64,
    /// Of `promised`, what the open groups hold for their later
    /// application.
    pub groups: u64,
    /// The reserve and the room for a checkpoint's meta stream.
    pub floor: u64,
    pub cached: u64,
    /// Pages shared by more than one tree.
    pub shared: u64,
}

/// A checkpoint on disk, kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptCheckpoint {
    pub number: u64,
    pub entry: EntryId,
    pub version: Version,
    pub meta: (PageNo, Hash),
}

/// The trees of a version, by name, with their roots.
type Roots = Arc<Vec<(TreeName, Root)>>;

/// The pins alive, by version: how many, and the roots of the version.
type Pins = Arc<Mutex<BTreeMap<Version, (usize, Roots)>>>;

/// A version kept from reuse while the pin lives (section 6.7), with the
/// roots of its trees. Pins are in memory only.
pub struct Pin {
    pins: Pins,
    version: Version,
    roots: Roots,
}

impl Pin {
    /// The entry pinned.
    pub fn entry(&self) -> u64 {
        self.version.entry
    }

    /// The trees of the version pinned, by name, with their roots.
    pub fn roots(&self) -> &[(TreeName, Root)] {
        &self.roots
    }
}

/// Why a pin was not taken.
#[derive(Debug)]
pub enum PinError {
    /// The page file does not have the version, or cannot hand over its
    /// roots now (the version a checkpoint is still writing).
    Refused(&'static str),
    /// The meta stream that holds the roots of an older checkpoint or a
    /// restore point could not be used: one of its pages the device failed
    /// to read, or whose bytes do not check ([`MetaFault::Page`], with the
    /// page's own fault), or a stream that reads and breaks the format or a
    /// rule of the reader's ([`MetaFault::Malformed`]).
    Meta(MetaFault),
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinError::Refused(why) => write!(f, "refused: {why}"),
            PinError::Meta(fault) => write!(f, "the version's meta stream: {fault}"),
        }
    }
}

impl std::error::Error for PinError {}

impl From<&'static str> for PinError {
    fn from(why: &'static str) -> Self {
        PinError::Refused(why)
    }
}

/// Where the roots of a version to pin come from.
enum Source {
    /// Memory.
    Roots(Roots),
    /// The meta stream of a checkpoint on disk.
    Meta((PageNo, Hash)),
}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pin({})", self.version.entry)
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut pins = lock(&self.pins);
        if let Some((n, _)) = pins.get_mut(&self.version) {
            *n -= 1;
            if *n == 0 {
                pins.remove(&self.version);
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while a lock was held leaves the state as consistent as any
    // single step leaves it; the store stops on the error that follows.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The newest checkpoint on disk.
#[derive(Clone, Debug)]
struct Newest {
    sb: Superblock,
    version: Version,
    pair: usize,
    meta_pages: Vec<(PageNo, Hash)>,
    trees: Vec<(TreeName, Root)>,
    restore_points: Vec<RestorePoint>,
    retiring: Vec<RestorePoint>,
}

/// The checkpoint before it, while it is on disk.
#[derive(Clone, Copy, Debug)]
struct Older {
    sb: Superblock,
    version: Version,
}

struct Inner {
    map: Map,
    /// Pages committed and not on disk yet.
    dirty: BTreeMap<PageNo, Arc<Page>>,
    /// Numbers taken by units that have not committed.
    held: BTreeSet<PageNo>,
    cache: Cache,
    promised: u64,
    /// Of `promised`, what the open groups hold for their later application
    /// (`docs/server-storage.md` section 6.9).
    groups: u64,
    newest: Newest,
    newest_meta: BTreeSet<PageNo>,
    older: Option<Older>,
    /// The restore points the next checkpoint lists.
    listed: Vec<RestorePoint>,
    /// The checkpoint being written, from its plan until it is installed or
    /// fails.
    in_progress: Option<Version>,
    /// The entry a checkpoint fixed at its first check, until it is
    /// installed or fails: nothing at or before it is reserved or committed
    /// meanwhile.
    fixed: Option<u64>,
    /// The restore point that checkpoint keeps: the room kept for meta
    /// streams counts it from the first check (format section 4.8).
    keeping: Option<RestorePoint>,
    next_number: u64,
    /// The entry of the last unit committed (or of the newest checkpoint).
    last_unit: u64,
    /// The trees of the newest version, by name, with their roots: those of
    /// the newest checkpoint, as units have changed them since. Only a
    /// unit's commit changes them; a checkpoint writes them as they are.
    trees: BTreeMap<TreeName, Root>,
    /// How many trees have each page as their root: a page a unit releases
    /// is checked against it.
    root_pages: BTreeMap<PageNo, u32>,
    /// The bytes their names take.
    names: usize,
    /// A write or flush failed: nothing more is written (section 6.12).
    failed: bool,
    /// The entries with an open reservation, one each.
    reservations: BTreeSet<u64>,
    /// A unit is open: one at a time.
    unit_open: bool,
    cursor: PageNo,
}

/// The page file (module docs).
pub struct PageFile {
    file: Box<dyn File>,
    config: Config,
    inner: Mutex<Inner>,
    pins: Pins,
    /// Held by a checkpoint and by growth of the file, so that neither runs
    /// alongside the other.
    io: Mutex<()>,
    /// The store folder's lock, taken by this page file until it is dropped
    /// ([`FolderLock::take_page_file`]).
    _mark: PageFileMark,
}

impl fmt::Debug for PageFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageFile")
            .field("space", &self.space())
            .finish()
    }
}

/// Takes the store folder's lock for one page file, refusing a second while
/// one is open or opening with the same lock.
fn take_page_file(lock: &FolderLock) -> Result<PageFileMark, OpenError> {
    lock.take_page_file().ok_or(OpenError::Refused(
        "a page file opened with this store folder's lock is open or opening: one page file \
         per store folder",
    ))
}

fn check_lock(fs: &dyn Fs, lock: &FolderLock) -> Result<(), OpenError> {
    if fs.owns(lock) {
        Ok(())
    } else {
        Err(OpenError::Refused(
            "the lock given is not this store folder's, or is no longer held",
        ))
    }
}

/// Reads the bytes of page `number` of a file of `end` pages.
fn raw(file: &dyn File, number: PageNo, end: PageNo) -> Result<Box<[u8]>, RawRead> {
    let Some(at) = number
        .checked_mul(PAGE)
        .filter(|_| number != 0 && number < end)
    else {
        return Err(RawRead::Beyond);
    };
    let mut buf = vec![0u8; PAGE_SIZE].into_boxed_slice();
    let n = file.read_at(at, &mut buf).map_err(RawRead::Io)?;
    if n < PAGE_SIZE {
        return Err(RawRead::Short { read: n });
    }
    Ok(buf)
}

/// Why [`load`] returned no page.
enum Unloaded {
    /// The bytes are not there (the file ends before them) or do not check.
    Bad(PageFault),
    /// The device failed the read: that says nothing of the bytes, which a
    /// later read may return whole.
    Device(PageFault),
    /// A failure of the read other than the device's (the process
    /// stopping): it says nothing of the page.
    Stopped(sys::Error),
}

/// Reads and checks page `number` of a file of `end` pages against `hash`.
fn load(file: &dyn File, end: PageNo, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Unloaded> {
    let fault = |fault| PageFault {
        page: number,
        hash: *hash,
        fault,
    };
    let bytes = match raw(file, number, end) {
        Ok(bytes) => bytes,
        Err(RawRead::Io(e)) if e.kind() == ErrorKind::Read => {
            return Err(Unloaded::Device(fault(Fault::Io(e.to_string()))))
        }
        Err(RawRead::Io(e)) => return Err(Unloaded::Stopped(e)),
        Err(r) => return Err(Unloaded::Bad(fault(r.fault()))),
    };
    Page::verify(number, hash, bytes)
        .map(Arc::new)
        .map_err(|f| Unloaded::Bad(fault(f)))
}

/// What the pages a dropped page refers to are, for the map.
fn children_of(page: &Page) -> Vec<Child> {
    match page.kind() {
        PageKind::Index => {
            let mut out: Vec<Child> = (0..page.count())
                .map(|i| {
                    let c = page.child(i);
                    Child {
                        number: c.number,
                        hash: c.hash,
                        birth: Some(Version::at(c.stamp)),
                        expand: true,
                    }
                })
                .collect();
            out.extend(
                page.references()
                    .into_iter()
                    .skip(page.count())
                    .map(|(number, hash)| Child {
                        number,
                        hash,
                        birth: None,
                        expand: false,
                    }),
            );
            out
        }
        PageKind::Leaf => page
            .references()
            .into_iter()
            .map(|(number, hash)| Child {
                number,
                hash,
                birth: None,
                expand: false,
            })
            .collect(),
        PageKind::Overflow => Vec::new(),
    }
}

impl Inner {
    /// A page's bytes if no checkpoint has written them yet. Nothing the
    /// map decides depends on the cache, which readers fill: the map stays
    /// a function of the changes made (format section 4.5).
    fn in_memory(&self, n: PageNo, hash: &Hash) -> Option<Arc<Page>> {
        self.dirty.get(&n).filter(|p| p.hash() == *hash).cloned()
    }

    /// The last entry a checkpoint covers, on disk or being written: no
    /// reservation and no unit at or before it is taken from the moment a
    /// checkpoint fixes its entry, at its first check (format section 4.6).
    fn cutoff(&self) -> u64 {
        self.newest.sb.entry.index.max(self.fixed.unwrap_or(0))
    }

    /// Pages in memory waiting for a checkpoint that the newest version
    /// reaches (pages that left it, kept for a pin, are not written by a
    /// checkpoint and do not count).
    fn live_dirty(&self) -> u64 {
        self.dirty
            .keys()
            .filter(|n| !self.map.pending().contains_key(n))
            .count() as u64
    }

    /// Learns a dying page's birth and expands it, from its bytes.
    fn learn(&mut self, n: PageNo, page: &Page, work: &mut Vec<PageNo>, twice: &mut Vec<PageNo>) {
        let Some(record) = self.map.pending_mut().get_mut(&n) else {
            return;
        };
        if record.birth.is_none() {
            record.birth = Some(Version::at(page.written()));
        }
        let carried = record.carried;
        if carried {
            // A map on disk needs these bytes until every checkpoint on
            // disk records the page settled.
            record.hold = record.hold.max(self.next_number);
            record.carried = false;
        }
        if record.expand {
            // What the page refers to is in use in that map, which will
            // need its bytes too: the children are carried.
            let (died, t) = self.map.expand(n, &children_of(page), carried);
            work.extend(died);
            twice.extend(t);
        }
    }

    /// Settles the dying pages in `work` as far as the pages not written
    /// yet allow, and frees those no kept version reaches and nothing
    /// holds. Returns how many were freed.
    fn settle(&mut self, mut work: Vec<PageNo>, kept: &Kept, twice: &mut Vec<PageNo>) -> u64 {
        let mut freed = 0;
        while let Some(n) = work.pop() {
            let Some(record) = self.map.pending().get(&n).copied() else {
                continue;
            };
            if record.expand || record.birth.is_none() {
                let Some(page) = self.in_memory(n, &record.hash) else {
                    continue;
                };
                self.learn(n, &page, &mut work, twice);
            }
            let record = self.map.pending()[&n];
            if kept.frees(&record) {
                self.free(n);
                freed += 1;
            }
        }
        freed
    }

    fn free(&mut self, n: PageNo) {
        self.map.free_pending(n);
        self.dirty.remove(&n);
        self.cache.remove(n);
    }

    /// The room a checkpoint taken now could need for its meta stream:
    /// the trees that exist now, the restore points listed and retiring
    /// plus one it may keep, and a map in which every page of the file is
    /// dying (the largest record a page can have). Commits, the taking
    /// down of dropped subtrees and reclaims never make the map larger than
    /// that, so space kept by this rule stays enough whatever is applied
    /// meanwhile; it is about 0.5% of the file.
    fn meta_room(&self, trees: usize, names: usize) -> u64 {
        let spare = RestorePoint {
            name: "x".repeat(MAX_RESTORE_NAME),
            checkpoint: 0,
            entry: EntryId::default(),
            version: Version::default(),
            meta: (0, Hash::default()),
        };
        // The next checkpoint lists `listed` and one it may keep, and
        // retires the newest checkpoint's points it does not list: at most
        // the union of the two, which a drop does not shrink (the dropped
        // point is still on disk) and a reopen does not grow (it rebuilds
        // `listed` from the newest checkpoint's list). The point a
        // checkpoint under way keeps counts from its first check: a reopen
        // after its first copy is on disk lists it, with a spare of its own.
        let mut points = self.newest.restore_points.clone();
        for rp in &self.listed {
            if !points.iter().any(|p| p.version == rp.version) {
                points.push(rp.clone());
            }
        }
        points.extend(self.keeping.clone());
        points.push(spare);
        let len = meta::stream_len(
            trees,
            names,
            &points,
            &[],
            self.map.end(),
            self.map.end() as usize,
            0,
        );
        // A page to spare, for trees made by other reservations meanwhile.
        meta::pages_for(len) as u64 + 1
    }

    /// The room every reservation leaves: two checkpoints' meta streams,
    /// since the next checkpoint retires nothing when the other pair is
    /// empty (after creation, or after an open that kept no predecessor),
    /// and the one after it frees the first's pages only once it is on
    /// disk; plus, for an ordinary entry, the reserve.
    fn floor(&self, purpose: Purpose, config: &Config) -> u64 {
        let meta = 2 * self.meta_room(self.trees.len(), self.names);
        match purpose {
            Purpose::Entry => meta.saturating_add(config.reserve),
            Purpose::Recovery | Purpose::Discard => meta,
        }
    }
}

impl PageFile {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// The versions kept from reuse now: the checkpoints on disk and the
    /// one being written, the restore points the superblocks on disk list,
    /// and the pins.
    fn kept(&self, inner: &Inner) -> Kept {
        let mut kept = Kept::default();
        let versions = &mut kept.checkpoints;
        versions.insert(inner.newest.version);
        for rp in &inner.newest.restore_points {
            versions.insert(rp.version);
        }
        if let Some(older) = &inner.older {
            versions.insert(older.version);
            for rp in &inner.newest.retiring {
                versions.insert(rp.version);
            }
        }
        if let Some(v) = inner.in_progress {
            versions.insert(v);
        }
        kept.pins = lock(&self.pins).keys().copied().collect();
        kept.on_disk = inner.older.map_or(inner.newest.sb.number, |o| {
            o.sb.number.min(inner.newest.sb.number)
        });
        kept
    }

    /// Set up the page file in a store folder that has none: an empty
    /// checkpoint, number 1, at entry 0, written to [`NEW_FILE`], flushed,
    /// renamed to [`FILE`] and the folder flushed, so that `pages` exists
    /// only whole. A [`NEW_FILE`] left by a creation that did not finish is
    /// replaced. `lock` must be the store folder's, held (by
    /// `crate::state::State`) for as long as the page file is used.
    pub fn create(fs: &dyn Fs, lock: &FolderLock, config: Config) -> Result<PageFile, OpenError> {
        check_lock(fs, lock)?;
        let mark = take_page_file(lock)?;
        config.check()?;
        match fs.open(FILE, FileOptions::PLAIN) {
            Ok(_) => return Err(OpenError::Exists),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        match fs.remove(NEW_FILE) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let entry = EntryId::default();
        let version = Version { entry: 0, step: 1 };
        let mut end = 1;
        let (meta, numbers) = loop {
            let meta = Meta {
                number: 1,
                entry,
                tree_format: page::FORMAT_VERSION,
                predecessor: None,
                trees: Vec::new(),
                restore_points: Vec::new(),
                retiring: Vec::new(),
                map: Map::new(end).image(&BTreeSet::new()),
            };
            let need = meta::pages_for(meta.encode().len()) as u64;
            if 1 + need == end {
                break (meta, (1..end).collect::<Vec<_>>());
            }
            end = 1 + need;
        };
        let stream = meta.encode();
        let (pages, root) = meta::build(&stream, &numbers, 1);
        let sb = Superblock {
            number: 1,
            entry,
            tree_format: page::FORMAT_VERSION,
            meta: root,
        };
        let mut bytes = vec![0u8; PAGE_SIZE];
        let copy = superblock::encode(&sb);
        for slot in pair_slots(0) {
            bytes[slot * SLOT..slot * SLOT + COPY].copy_from_slice(&copy);
        }
        let mut sorted = pages.clone();
        sorted.sort_by_key(|p| p.number());
        for page in &sorted {
            bytes.extend_from_slice(page.bytes());
        }
        let file = fs.create(NEW_FILE, FileOptions::PLAIN)?;
        file.write_at(0, &bytes)?;
        file.flush()?;
        drop(file);
        fs.rename(NEW_FILE, FILE)?;
        fs.flush_folder("")?;
        let file = fs.open(FILE, FileOptions::PLAIN)?;
        let meta_pages: Vec<(PageNo, Hash)> =
            sorted.iter().map(|p| (p.number(), p.hash())).collect();
        let newest = Newest {
            sb,
            version,
            pair: 0,
            meta_pages: meta_pages.clone(),
            trees: Vec::new(),
            restore_points: Vec::new(),
            retiring: Vec::new(),
        };
        Ok(PageFile::assemble(
            file,
            config,
            Map::new(end),
            newest,
            None,
            2,
            mark,
        ))
    }

    fn assemble(
        file: Box<dyn File>,
        config: Config,
        map: Map,
        newest: Newest,
        older: Option<Older>,
        next_number: u64,
        mark: PageFileMark,
    ) -> PageFile {
        let capacity = (config.cache_bytes / PAGE) as usize;
        let newest_meta = newest.meta_pages.iter().map(|(n, _)| *n).collect();
        let listed = newest.restore_points.clone();
        let last_unit = newest.sb.entry.index;
        let trees: BTreeMap<TreeName, Root> = newest.trees.iter().cloned().collect();
        let mut root_pages = BTreeMap::new();
        for root in trees.values() {
            *root_pages.entry(root.page).or_default() += 1;
        }
        let names = trees.keys().map(|n| n.as_bytes().len()).sum();
        PageFile {
            file,
            config,
            inner: Mutex::new(Inner {
                map,
                dirty: BTreeMap::new(),
                held: BTreeSet::new(),
                cache: Cache::new(capacity),
                promised: 0,
                groups: 0,
                newest,
                newest_meta,
                older,
                listed,
                in_progress: None,
                fixed: None,
                keeping: None,
                next_number,
                last_unit,
                trees,
                root_pages,
                names,
                failed: false,
                reservations: BTreeSet::new(),
                unit_open: false,
                cursor: 0,
            }),
            pins: Arc::default(),
            io: Mutex::new(()),
            _mark: mark,
        }
    }

    /// Open the page file (section 6.7, step 2) on the newest checkpoint,
    /// giving nothing up. It reads the four slots one at a time and opens
    /// the checkpoint with the highest-numbered valid copy when its meta
    /// stream reads back and agrees with it and its tree roots read back
    /// with the hashes it names. Otherwise it writes nothing and fails:
    ///
    /// - with [`OpenError::Unreadable`] when the device fails a read of a
    ///   slot (which could hold a checkpoint newer than any read) or of
    ///   that checkpoint's meta stream or roots: a read the device fails
    ///   says nothing of the bytes, which a later read may return whole;
    /// - with [`OpenError::Damaged`] when that checkpoint's bytes are not
    ///   there or do not check, with what giving it up would lose.
    ///
    /// Either error carries the approval [`PageFile::open_giving_up`]
    /// needs ([`OpenError::give_up`]); a caller records the error durably
    /// before it makes that call. Having opened, it writes, each slot
    /// flushed:
    ///
    /// 1. the checkpoint's copy into both slots of its pair, so that the
    ///    device holds it twice (an earlier process's failed write or flush
    ///    can leave the system's cache showing a copy the device does not
    ///    hold), never over a copy of the predecessor;
    /// 2. in the other pair, the predecessor's copy into both slots, when
    ///    one shows it; otherwise zeros over both. Both are written whatever
    ///    they show, as the opened pair's are: after an earlier process's
    ///    failed flush the cache can show a copy, or zeros, that the device
    ///    does not hold, and drop it later.
    ///
    /// What these keep: the checkpoint opened and the predecessor its meta
    /// stream names keep every copy they have. What goes is what the opened
    /// checkpoint had already retired: bytes that do not check (taken as
    /// damage that stays), and copies of older checkpoints, such as the one
    /// a crash between the opened checkpoint's two copies leaves in the
    /// second slot (pairs take checkpoints in turn). Such an older
    /// checkpoint's pages may still check, and its table may list a restore
    /// point the opened one dropped; but the opened checkpoint no longer
    /// keeps it. Its pages are retired with its last superblock copy and
    /// become reusable under the ordinary rules (an old meta page may still
    /// be dying, an unchanged tree page still in use), so it must never be
    /// taken later.
    /// Every slot was read and no checkpoint with a valid copy newer than
    /// the opened one exists, so nothing these writes remove is newer than
    /// the checkpoint opened. The open itself frees nothing. Roots are read
    /// and checked one at a time and let go: what an open holds in memory
    /// is the meta stream and its table of trees, however many there are.
    pub fn open(
        fs: &dyn Fs,
        lock: &FolderLock,
        config: Config,
    ) -> Result<(PageFile, Opened), OpenError> {
        PageFile::open_choosing(fs, lock, config, None)
    }

    /// Open the page file giving up exactly what `approval` names, or write
    /// nothing: the explicit call, for damage and for a read that keeps
    /// failing alike. `approval` is what an error showed
    /// ([`OpenError::give_up`]): the checkpoints to give up, each with the
    /// restore points that go with it, the slots to write over unread, and
    /// the checkpoint the open is then on, each checkpoint named by its
    /// identity ([`CheckpointId`]), which differs when its number comes
    /// again (unless its contents are the same in every byte, and then so
    /// is the loss). The caller records the error durably first; then the
    /// loss it recorded is the loss that can happen, however many things are
    /// wrong at once, whatever was damaged in between and whatever was
    /// written since, and a stop during this call loses no report.
    ///
    /// The call first learns, writing nothing, what it would give up and
    /// where it would land: every slot the device fails to read, and the two
    /// newest checkpoints with a valid copy, newest first, until one opens.
    /// Unless that is exactly `approval`, it is refused, writing nothing:
    /// with [`OpenError::Refused`] when nothing stops the open any more
    /// (`open` opens), and otherwise with the error for what `approval` does
    /// not cover (or for the last checkpoint it would give up), carrying the
    /// approval for what is so now. When it is, it writes as `open` does,
    /// over the slots of what it gives up too, and reports each checkpoint
    /// passed over with the restore points lost with it
    /// ([`PassedOver::lost`]) and each slot written over unread
    /// ([`Opened::given_up_slots`]).
    pub fn open_giving_up(
        fs: &dyn Fs,
        lock: &FolderLock,
        config: Config,
        approval: &GiveUp,
    ) -> Result<(PageFile, Opened), OpenError> {
        PageFile::open_choosing(fs, lock, config, Some(approval))
    }

    /// [`PageFile::open`], or with `accept` [`PageFile::open_giving_up`].
    fn open_choosing(
        fs: &dyn Fs,
        lock: &FolderLock,
        config: Config,
        accept: Option<&GiveUp>,
    ) -> Result<(PageFile, Opened), OpenError> {
        check_lock(fs, lock)?;
        let mark = take_page_file(lock)?;
        config.check()?;
        let file = match fs.open(FILE, FileOptions::PLAIN) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(OpenError::Missing),
            Err(e) => return Err(e.into()),
        };
        fs.flush_folder("")?;
        let size = file.size()?;
        let file_pages = size / PAGE;
        let mut slots = [SlotSeen::Invalid(Invalid::Short); SLOTS];
        let mut copies: [Option<Superblock>; SLOTS] = [None; SLOTS];
        // Slots read whole and holding only zeros (never written, or
        // cleared).
        let mut blank = [false; SLOTS];
        let mut unread = Vec::new();
        for (s, seen) in slots.iter_mut().enumerate() {
            let mut buf = vec![0u8; SLOT];
            let n = match file.read_at((s * SLOT) as u64, &mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Read => {
                    *seen = SlotSeen::Unreadable(e.kind());
                    unread.push((s, e.to_string()));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            blank[s] = n == SLOT && buf.iter().all(|&b| b == 0);
            match superblock::decode(&buf[..n]) {
                Ok(superblock::Decoded::Valid(sb)) => {
                    if sb.tree_format > page::FORMAT_VERSION {
                        return Err(OpenError::NewerTrees {
                            version: sb.tree_format,
                        });
                    }
                    *seen = SlotSeen::Copy(sb.number);
                    copies[s] = Some(sb);
                }
                Ok(superblock::Decoded::Newer(version)) => {
                    return Err(OpenError::NewerFormat { version })
                }
                Err(invalid) => *seen = SlotSeen::Invalid(invalid),
            }
        }
        let mut numbers: Vec<u64> = copies.iter().flatten().map(|sb| sb.number).collect();
        numbers.sort_unstable_by(|a, b| b.cmp(a));
        numbers.dedup();
        let highest = numbers.first().copied().unwrap_or(0);
        numbers.truncate(2);
        // The next checkpoint's number, before anything is written: a copy
        // at the limit is valid and must not make the open wrap.
        let Some(next_number) = highest.checked_add(1) else {
            return Err(OpenError::NumbersExhausted);
        };
        // The two newest checkpoints with a valid copy, newest first.
        let candidates: Vec<(usize, Superblock)> = numbers
            .iter()
            .map(|&number| {
                copies
                    .iter()
                    .enumerate()
                    .find_map(|(s, c)| c.filter(|sb| sb.number == number).map(|sb| (s, sb)))
                    .expect("a copy of each number")
            })
            .collect();
        // What the open would give up, and where it would land, learned
        // writing nothing: every slot the device fails to read, and the two
        // newest checkpoints with a valid copy, newest first, read until one
        // opens. When nothing stops it, that is the newest alone; reading on
        // is only for an error, to offer an approval that works.
        let mut blocked: Vec<(Superblock, Blocked)> = Vec::new();
        let mut chosen = None;
        for &(slot, sb) in &candidates {
            match Loaded::read(&*file, file_pages, &sb) {
                Ok(loaded) => {
                    chosen = Some((slot, sb, loaded));
                    break;
                }
                Err(NotLoaded::Stopped(e)) => return Err(e.into()),
                Err(NotLoaded::Rejected(why, points)) => {
                    blocked.push((sb, Blocked::Damaged(*why, points)))
                }
                Err(NotLoaded::Unreadable(unread, points)) => {
                    blocked.push((sb, Blocked::Unreadable(*unread, points)))
                }
            }
        }
        let below = chosen.as_ref().map_or(0, |(_, sb, _)| sb.number);
        // The approval for all of it, with what goes with it; there is one
        // only when its landing opens.
        let outcome = chosen.as_ref().map(|(_, sb, _)| GiveUp {
            checkpoints: blocked
                .iter()
                .map(|(b, why)| GivenUp {
                    checkpoint: CheckpointId::of(b),
                    lost: why.lost(below),
                })
                .collect(),
            slots: unread.iter().map(|(s, _)| *s).collect(),
            onto: CheckpointId::of(sb),
        });
        let stopped = !unread.is_empty() || !blocked.is_empty();
        let (slot, sb, loaded, passed_over, given_up_slots) = match (accept, chosen) {
            (None, Some((slot, sb, loaded))) if !stopped => {
                (slot, sb, loaded, Vec::new(), Vec::new())
            }
            (None, None) if !stopped => {
                return Err(OpenError::NoCheckpoint {
                    slots,
                    passed_over: Vec::new(),
                })
            }
            // The plain open gives nothing up: it names the first thing that
            // stops it, with the approval for all of it, and writes nothing.
            // Without an approval, `fallback` says why the checkpoint it
            // would fall back to cannot be used either.
            (None, _) => {
                let give_up = outcome.map(Box::new);
                let none_opens = give_up.is_none();
                if let Some((slot, error)) = unread.first().cloned() {
                    let fallback = blocked
                        .pop()
                        .filter(|_| none_opens)
                        .map(|(b, why)| Box::new(passed(&b, why, 0)));
                    let failure = ReadFailure::Slot { slot, error };
                    return Err(OpenError::Unreadable {
                        failure,
                        give_up,
                        fallback,
                    });
                }
                let mut blocked = blocked.into_iter();
                let (first, why) = blocked.next().expect("something stops the open");
                let fallback = blocked
                    .next()
                    .filter(|_| none_opens)
                    .map(|(b, why)| Box::new(passed(&b, why, 0)));
                return Err(blocked_error(&first, why, below, give_up, fallback));
            }
            (Some(_), None) => {
                // Nothing could be opened: there is nothing to approve.
                let passed_over = blocked
                    .into_iter()
                    .map(|(sb, why)| passed(&sb, why, 0))
                    .collect();
                return Err(OpenError::NoCheckpoint { slots, passed_over });
            }
            (Some(approval), Some((slot, sb, loaded))) => {
                let now = outcome.expect("a landing");
                if !stopped {
                    return Err(OpenError::Refused(
                        "nothing to give up stops the open: `PageFile::open` opens the newest \
                         checkpoint",
                    ));
                }
                if now != *approval {
                    // Not what was approved: what it gives up, what goes with
                    // it, or where it lands is another now. Refused with the
                    // obstacle the approval does not cover (or the last one),
                    // carrying the approval for what is so now.
                    let give_up = Some(Box::new(now));
                    if let Some((slot, error)) = unread
                        .iter()
                        .find(|(s, _)| !approval.slots.contains(s))
                        .cloned()
                    {
                        let failure = ReadFailure::Slot { slot, error };
                        return Err(OpenError::Unreadable {
                            failure,
                            give_up,
                            fallback: None,
                        });
                    }
                    let approved = |b: &Superblock| {
                        approval
                            .checkpoints
                            .iter()
                            .any(|g| g.checkpoint == CheckpointId::of(b))
                    };
                    let at = blocked
                        .iter()
                        .position(|(b, _)| !approved(b))
                        .or(blocked.len().checked_sub(1));
                    if let Some(at) = at {
                        let (first, why) = blocked.swap_remove(at);
                        return Err(blocked_error(&first, why, below, give_up, None));
                    }
                    let (slot, error) = unread.first().cloned().expect("something to give up");
                    let failure = ReadFailure::Slot { slot, error };
                    return Err(OpenError::Unreadable {
                        failure,
                        give_up,
                        fallback: None,
                    });
                }
                let passed_over = blocked
                    .into_iter()
                    .map(|(b, why)| passed(&b, why, below))
                    .collect();
                (slot, sb, loaded, passed_over, now.slots)
            }
        };

        let predecessor = loaded.meta.predecessor;
        let is_predecessor =
            |c: &Superblock| predecessor.is_some_and(|(n, _, m)| c.number == n && c.meta == m);
        // Its copy into both slots of its pair, each flushed. A plain open
        // never writes over a copy of the predecessor, which a file this
        // code wrote never holds in this pair (pairs take checkpoints in
        // turn).
        let pair = superblock::pair_of(slot);
        let copy = superblock::encode(&sb);
        let mut rewritten = Vec::new();
        for s in pair_slots(pair) {
            if accept.is_none() && copies[s].as_ref().is_some_and(is_predecessor) {
                continue;
            }
            file.write_at((s * SLOT) as u64, &copy)?;
            file.flush()?;
            rewritten.push(s);
        }
        // The other pair: the predecessor's copy in both slots when one of
        // them holds it; otherwise both cleared. A slot that held anything
        // else (a checkpoint passed over, a stale copy, bytes that could
        // not be read) is overwritten before any page is: a checkpoint
        // whose pages the store may now reuse must never be chosen later.
        // Both slots are written whatever they show, as the opened pair's
        // are: the system's cache can show a copy, or zeros, that the device
        // does not hold (an earlier process's failed flush, after which the
        // cache may drop what it showed and the device's older bytes come
        // back). The predecessor is the checkpoint whose number and meta
        // stream the chosen one names: a number alone can belong to another
        // checkpoint that never reached the disk whole.
        let older_sb = pair_slots(1 - pair)
            .into_iter()
            .find_map(|s| copies[s].filter(is_predecessor));
        let mut cleared = Vec::new();
        for s in pair_slots(1 - pair) {
            let other = !blank[s] && (copies[s].is_none() || copies[s] != older_sb);
            match older_sb {
                Some(older) => {
                    file.write_at((s * SLOT) as u64, &superblock::encode(&older))?;
                    rewritten.push(s);
                }
                None => file.write_at((s * SLOT) as u64, &[0u8; COPY])?,
            }
            file.flush()?;
            if other {
                cleared.push(s);
            }
        }
        let end = loaded.map.end();
        // `Loaded::read` checked that the pages' bytes fit a file offset.
        let extended = size < end * PAGE;
        if extended {
            file.allocate(end * PAGE)?;
            file.flush()?;
        }

        let older = match (older_sb, predecessor) {
            (Some(sb), Some((_, version, _))) => Some(Older { sb, version }),
            _ => None,
        };
        let mut restore_points: Vec<(String, u64)> = loaded
            .meta
            .restore_points
            .iter()
            .map(|rp| (rp.name.clone(), rp.checkpoint))
            .collect();
        if older.is_some() {
            restore_points.extend(
                loaded
                    .meta
                    .retiring
                    .iter()
                    .map(|rp| (rp.name.clone(), rp.checkpoint)),
            );
        }
        let newest = Newest {
            sb,
            version: Version {
                entry: sb.entry.index,
                step: sb.number,
            },
            pair,
            meta_pages: loaded.meta_pages,
            trees: loaded.meta.trees,
            restore_points: loaded.meta.restore_points,
            retiring: loaded.meta.retiring,
        };
        let opened = Opened {
            checkpoint: sb.number,
            entry: sb.entry,
            slots,
            passed_over,
            predecessor: older.map(|o| o.sb.number),
            rewritten,
            cleared,
            restore_points,
            extended,
            given_up_slots,
        };
        let pf = PageFile::assemble(file, config, loaded.map, newest, older, next_number, mark);
        Ok((pf, opened))
    }

    /// Whether a write or flush has failed (the store must stop).
    pub fn failed(&self) -> bool {
        self.lock().failed
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// The newest checkpoint on disk: its number and the entry it covers.
    pub fn checkpoint_entry(&self) -> (u64, EntryId) {
        let inner = self.lock();
        (inner.newest.sb.number, inner.newest.sb.entry)
    }

    /// The number the next checkpoint gets.
    pub fn next_checkpoint(&self) -> u64 {
        self.lock().next_number
    }

    /// The trees that exist now, by name (the next checkpoint names exactly
    /// these).
    pub fn tree_names(&self) -> Vec<TreeName> {
        self.lock().trees.keys().cloned().collect()
    }

    /// The trees of the newest checkpoint on disk, by name.
    pub fn checkpoint_trees(&self) -> Vec<(TreeName, Root)> {
        self.lock().newest.trees.clone()
    }

    /// The entry of the newest version: the last entry whose unit
    /// committed, or the newest checkpoint's when it is later.
    pub fn newest_entry(&self) -> u64 {
        newest_entry(&self.lock())
    }

    /// The checkpoints on disk, newest first.
    pub fn kept_checkpoints(&self) -> Vec<KeptCheckpoint> {
        let inner = self.lock();
        let mut out = vec![KeptCheckpoint {
            number: inner.newest.sb.number,
            entry: inner.newest.sb.entry,
            version: inner.newest.version,
            meta: inner.newest.sb.meta,
        }];
        if let Some(older) = &inner.older {
            out.push(KeptCheckpoint {
                number: older.sb.number,
                entry: older.sb.entry,
                version: older.version,
                meta: older.sb.meta,
            });
        }
        out
    }

    /// The restore points the superblocks on disk list (their pages are
    /// kept), with whether a drop is pending: a dropped one is kept until
    /// no superblock on disk lists it.
    pub fn restore_points(&self) -> Vec<(RestorePoint, bool)> {
        let inner = self.lock();
        let mut out: Vec<(RestorePoint, bool)> = inner
            .newest
            .restore_points
            .iter()
            .map(|rp| {
                let dropped = !inner.listed.iter().any(|l| l.version == rp.version);
                (rp.clone(), dropped)
            })
            .collect();
        if inner.older.is_some() {
            out.extend(inner.newest.retiring.iter().map(|rp| (rp.clone(), true)));
        }
        out
    }

    /// The trees named by the meta stream that begins at `meta` (a
    /// checkpoint's or a restore point's).
    pub fn trees_of(&self, meta: (PageNo, Hash)) -> Result<Vec<(TreeName, Root)>, MetaFault> {
        let (stream, _) = self.read_meta(meta)?;
        Ok(Meta::decode(&stream).map_err(MetaFault::Malformed)?.trees)
    }

    /// Every page of the meta stream that begins at `meta`.
    pub fn meta_pages_of(&self, meta: (PageNo, Hash)) -> Result<Vec<PageNo>, MetaFault> {
        Ok(self
            .read_meta(meta)?
            .1
            .into_iter()
            .map(|(n, _)| n)
            .collect())
    }

    fn read_meta(&self, meta: (PageNo, Hash)) -> Result<meta::Stream, MetaFault> {
        let end = self.lock().map.end();
        meta::read_stream(meta, end, |n, h| {
            self.read_page(n, h).map_err(|fault| PageFault {
                page: n,
                hash: *h,
                fault,
            })
        })
    }

    /// The page file's counts.
    pub fn space(&self) -> Space {
        let inner = self.lock();
        Space {
            end: inner.map.end(),
            free: inner.map.free_count(),
            dying: inner.map.pending().len() as u64,
            dirty: inner.dirty.len() as u64,
            held: inner.held.len() as u64,
            promised: inner.promised,
            groups: inner.groups,
            floor: inner.floor(Purpose::Entry, &self.config),
            cached: inner.cache.len() as u64,
            shared: inner.map.shares().len() as u64,
        }
    }

    /// The page cache's hits and misses so far.
    pub fn cache_counts(&self) -> (u64, u64) {
        self.lock().cache.counts()
    }

    /// Whether page `n` is free.
    pub fn is_free(&self, n: PageNo) -> bool {
        self.lock().map.is_free(n)
    }

    /// Every free page, in order.
    pub fn free_pages(&self) -> Vec<PageNo> {
        let inner = self.lock();
        (1..inner.map.end())
            .filter(|&n| inner.map.is_free(n))
            .collect()
    }

    /// The dying pages held for a map on disk that records them unsettled
    /// (their bytes stay until every checkpoint on disk records them
    /// settled).
    pub fn held_pages(&self) -> Vec<PageNo> {
        let inner = self.lock();
        let kept = self.kept(&inner);
        inner
            .map
            .pending()
            .iter()
            .filter(|(_, r)| r.settled() && r.hold > kept.on_disk)
            .map(|(n, _)| *n)
            .collect()
    }

    /// Every dying page, in order.
    pub fn dying_pages(&self) -> Vec<PageNo> {
        self.lock().map.pending().keys().copied().collect()
    }

    /// The map as a checkpoint taken now would write it
    /// (`docs/server-storage-format.md` section 4.4), for comparing two
    /// runs: the same calls from the same checkpoint give the same bytes.
    pub fn map_image(&self) -> Vec<u8> {
        let inner = self.lock();
        let image = inner.map.image(&inner.held);
        Meta {
            number: 0,
            entry: EntryId::default(),
            tree_format: 0,
            predecessor: None,
            trees: Vec::new(),
            restore_points: Vec::new(),
            retiring: Vec::new(),
            map: image,
        }
        .encode()
    }

    /// The bytes stored for page `number`, unchecked.
    pub fn read_raw(&self, number: PageNo) -> Result<Box<[u8]>, RawRead> {
        let end = self.lock().map.end();
        raw(&*self.file, number, end)
    }

    /// Page `number`, checked against `hash`: from memory when no
    /// checkpoint has written it yet, from the cache, or read from the file
    /// and checked by [`Page::verify`]. A number beyond the file is
    /// missing; a short or failed read is reported as unreadable; bytes
    /// that do not verify as what they are (damaged, misplaced, stale,
    /// malformed).
    pub fn read_page(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let end = {
            let mut inner = self.lock();
            if let Some(page) = inner.dirty.get(&number) {
                return if page.hash() == *hash {
                    Ok(page.clone())
                } else {
                    Err(Fault::Stale { found: page.hash() })
                };
            }
            if let Some(page) = inner.cache.get(number, hash) {
                return Ok(page);
            }
            inner.map.end()
        };
        let bytes = raw(&*self.file, number, end).map_err(RawRead::fault)?;
        let page = Arc::new(Page::verify(number, hash, bytes)?);
        self.lock().cache.insert(page.clone());
        Ok(page)
    }

    /// A page's bytes from memory when no checkpoint wrote it, otherwise
    /// read from the file and checked, never from the cache: what a reclaim
    /// decides (including whether a read fails) does not depend on what
    /// readers cached, and a checkpoint learns that a root on disk reads
    /// back, not that a copy of it was once cached.
    fn read_uncached(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let end = {
            let inner = self.lock();
            if let Some(page) = inner.in_memory(number, hash) {
                return Ok(page);
            }
            inner.map.end()
        };
        let bytes = raw(&*self.file, number, end).map_err(RawRead::fault)?;
        Ok(Arc::new(Page::verify(number, hash, bytes)?))
    }

    /// Set aside `pages` pages for entry `entry` (section 6.5): the caller's
    /// upper bound for applying it ([`entry_bound`]). An entry is refused
    /// with nothing reserved when its pages, the room for a checkpoint's
    /// meta stream and (for [`Purpose::Entry`]) the reserve do not fit even
    /// after the file grows; the store stays readable and can checkpoint.
    /// An entry has one open reservation at a time: a second, while the
    /// first is open, is refused.
    pub fn reserve(
        &self,
        entry: u64,
        pages: u64,
        purpose: Purpose,
    ) -> Result<Reservation<'_>, SpaceError> {
        self.set_aside(entry, pages, purpose, true)?;
        Ok(Reservation {
            file: self,
            entry,
            purpose,
            remaining: AtomicU64::new(pages),
            taken: AtomicU64::new(0),
            from_groups: AtomicU64::new(0),
        })
    }

    /// Holds `pages` for a group that entry `entry` opens or adds a part
    /// to (section 4.8): promised as a reservation's pages are, and checked
    /// the same way, but kept past the entry, until the entry that ends the
    /// group draws on them ([`PageFile::reserve_held`]). [`PageFile::unhold`]
    /// gives back a hold whose entry was not applied.
    pub fn hold(&self, entry: u64, pages: u64, purpose: Purpose) -> Result<(), SpaceError> {
        if pages == 0 {
            return Ok(());
        }
        self.set_aside(entry, pages, purpose, false)?;
        self.lock().groups += pages;
        Ok(())
    }

    /// Gives back `pages` the open groups held for an entry that was not
    /// applied. More than they hold is the store's own
    /// accounting gone wrong: a test build stops on it, and a release
    /// gives back what is held.
    pub fn unhold(&self, pages: u64) {
        let mut inner = self.lock();
        debug_assert!(
            pages <= inner.groups,
            "giving back {pages} pages of the {} the groups hold",
            inner.groups
        );
        let pages = pages.min(inner.groups);
        inner.groups -= pages;
        inner.promised -= pages;
    }

    /// A reservation for entry `entry`, one that ends groups (a close, a
    /// publication, a discard, an `empty` entry), of `pages` the groups
    /// hold: set aside already, so no room is looked for. Checked as
    /// [`PageFile::reserve`] is otherwise, the budget of pages waiting for
    /// a checkpoint included. An entry not applied gives them back to the
    /// groups ([`Reservation::keep_held`]).
    pub fn reserve_held(
        &self,
        entry: u64,
        pages: u64,
        purpose: Purpose,
    ) -> Result<Reservation<'_>, SpaceError> {
        let take = {
            let mut inner = self.lock();
            if inner.failed {
                return Err(SpaceError::Failed);
            }
            if entry <= inner.cutoff() {
                return Err(SpaceError::Refused(
                    "an entry at or before a checkpoint's (on disk or being written)",
                ));
            }
            if entry < inner.last_unit {
                return Err(SpaceError::Refused("an entry before the last one applied"));
            }
            if inner.reservations.contains(&entry) {
                return Err(SpaceError::Refused(
                    "an entry with a reservation open: one at a time",
                ));
            }
            // The budget of pages waiting for a checkpoint admits it as it
            // admits any reservation (`set_aside`).
            let budget = self.config.dirty_budget;
            if inner.dirty.len() as u64 >= budget {
                let dirty = inner.live_dirty();
                if dirty >= budget {
                    return Err(SpaceError::CheckpointNeeded { dirty, budget });
                }
            }
            // More than the groups hold is the store's own accounting gone
            // wrong (as in `unhold`).
            debug_assert!(
                pages <= inner.groups,
                "drawing {pages} pages of the {} the groups hold",
                inner.groups
            );
            let take = pages.min(inner.groups);
            inner.groups -= take;
            inner.reservations.insert(entry);
            take
        };
        Ok(Reservation {
            file: self,
            entry,
            purpose,
            remaining: AtomicU64::new(take),
            taken: AtomicU64::new(0),
            from_groups: AtomicU64::new(take),
        })
    }

    /// Adds `pages` to what is promised for `entry` (the checks of
    /// [`PageFile::reserve`]). When the free pages fall short it first frees
    /// what it can ([`PageFile::reclaim`], which reads pages if it must),
    /// then grows the file: so the space a sequence of entries needs does
    /// not depend on when the caller reclaims, and recovery, which applies
    /// the same entries, has at least the free pages the first run had
    /// (format section 4.8). With `open`, the reservation is opened in the
    /// same hold of the lock that checks it and promises its pages, so that
    /// a checkpoint sees it or it sees the checkpoint's entry fixed.
    fn set_aside(
        &self,
        entry: u64,
        pages: u64,
        purpose: Purpose,
        open: bool,
    ) -> Result<(), SpaceError> {
        let mut reclaimed = false;
        // The room kept for a checkpoint grows with the file, so growing
        // can leave a few pages to grow by again.
        let mut grown = 0;
        loop {
            let deficit = {
                let mut inner = self.lock();
                if inner.failed {
                    return Err(SpaceError::Failed);
                }
                if entry <= inner.cutoff() {
                    return Err(SpaceError::Refused(
                        "an entry at or before a checkpoint's (on disk or being written)",
                    ));
                }
                if entry < inner.last_unit {
                    return Err(SpaceError::Refused("an entry before the last one applied"));
                }
                if open && inner.reservations.contains(&entry) {
                    return Err(SpaceError::Refused(
                        "an entry with a reservation open: one at a time",
                    ));
                }
                // The budget admits entries; pages added to one already
                // admitted (`Reservation::extend`) may take memory past it.
                let budget = self.config.dirty_budget;
                if open && inner.dirty.len() as u64 >= budget {
                    let dirty = inner.live_dirty();
                    if dirty >= budget {
                        return Err(SpaceError::CheckpointNeeded { dirty, budget });
                    }
                }
                let floor = inner.floor(purpose, &self.config);
                let available = inner.map.free_count().saturating_sub(inner.promised);
                if available >= pages.saturating_add(floor) {
                    inner.promised += pages;
                    if open {
                        inner.reservations.insert(entry);
                    }
                    return Ok(());
                }
                if grown == 4 {
                    return Err(SpaceError::Full {
                        wanted: pages,
                        available: available.saturating_sub(floor),
                    });
                }
                pages.saturating_add(floor) - available
            };
            if !reclaimed {
                reclaimed = true;
                let _io = lock(&self.io);
                self.reclaim_for(deficit);
                continue;
            }
            let _io = lock(&self.io);
            self.grow(deficit)?;
            grown += 1;
        }
    }

    /// Reclaims, in one pass over the dying pages at most, until `pages`
    /// more are free. The caller holds `io`.
    fn reclaim_for(&self, pages: u64) {
        let (start, mut left) = {
            let inner = self.lock();
            (inner.map.free_count(), inner.map.pending().len())
        };
        while left > 0 {
            let done = self.reclaim_held(left.min(4096));
            left = left.saturating_sub(done.examined.max(1) as usize);
            if self.lock().map.free_count().saturating_sub(start) >= pages {
                return;
            }
        }
    }

    /// Grows the file by at least `at_least` pages; the caller holds `io`.
    /// A full disk refuses the growth and leaves the page file as it was;
    /// any other failure of the allocation (which can write) latches the
    /// page file's failure, as a failed write does.
    fn grow(&self, at_least: u64) -> Result<(), SpaceError> {
        let end = {
            let inner = self.lock();
            if inner.failed {
                return Err(SpaceError::Failed);
            }
            inner.map.end()
        };
        let step = at_least.max(self.config.growth).max((end / 8).min(1 << 16));
        for want in [step, at_least] {
            let Some(size) = end.checked_add(want).and_then(|e| e.checked_mul(PAGE)) else {
                continue;
            };
            match self.file.allocate(size) {
                Ok(()) => {
                    self.lock().map.grow(end + want);
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::NoSpace => continue,
                Err(e) => {
                    self.lock().failed = true;
                    return Err(SpaceError::Disk(e));
                }
            }
        }
        let inner = self.lock();
        Err(SpaceError::Full {
            wanted: at_least,
            available: inner.map.free_count().saturating_sub(inner.promised),
        })
    }

    /// Keep the newest version from reuse while the pin lives, and hand
    /// over the roots of its trees ([`Pin::roots`]) and its entry
    /// ([`Pin::entry`]): the last entry whose unit committed, or the newest
    /// checkpoint's when it is later. An entry that changed nothing has no
    /// version of its own: its reader pins this one.
    pub fn pin_newest(&self) -> Result<Pin, PinError> {
        let inner = self.lock();
        let entry = newest_entry(&inner);
        let roots = inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect();
        Ok(self.register(entry, Arc::new(roots))?)
    }

    /// Keep entry `entry`'s version from reuse while the pin lives, and
    /// hand over the roots of its trees ([`Pin::roots`]). The version must
    /// be one the page file has: the newest (`entry` is the last entry
    /// whose unit committed, or the newest checkpoint's), a checkpoint or
    /// restore point on disk, or one a pin keeps now. Any other entry is
    /// refused: an entry that changed nothing has no version of its own,
    /// and the page file cannot tell it from one whose unit has not
    /// committed yet ([`PageFile::pin_newest`] pins the newest without
    /// naming it). The roots come from memory, or, for an older checkpoint
    /// or a restore point, from its meta stream, read as part of taking the
    /// pin. A pin whose roots cannot be had is refused: the version a
    /// checkpoint is still writing ([`PinError::Refused`]), or a meta
    /// stream that does not read back or does not decode, with the cause
    /// ([`PinError::Meta`]). A table that names one root page for more
    /// trees than its map counts reaching it is refused as open refuses it.
    pub fn pin(&self, entry: u64) -> Result<Pin, PinError> {
        {
            let inner = self.lock();
            if let Source::Roots(roots) = self.pin_source(&inner, entry)? {
                return Ok(self.register(entry, roots)?);
            }
        }
        // An older checkpoint or a restore point: its table is read from
        // its meta stream while `io` is held, which a reclaim, a checkpoint
        // and a restore point's drop hold too, so none of them can free,
        // reuse or retire those pages before the pin keeps the version.
        let _io = lock(&self.io);
        let meta = match self.pin_source(&self.lock(), entry)? {
            Source::Roots(roots) => return Ok(self.register(entry, roots)?),
            Source::Meta(meta) => meta,
        };
        let (stream, _) = self.read_meta(meta).map_err(PinError::Meta)?;
        let meta =
            Meta::decode(&stream).map_err(|why| PinError::Meta(MetaFault::Malformed(why)))?;
        let shares: BTreeMap<PageNo, u32> =
            meta.map.shares.iter().map(|(n, s)| (*n, s.count)).collect();
        let count = |n| shares.get(&n).copied().unwrap_or(1);
        if root_named_past_its_count(meta.trees.iter().map(|(_, r)| r), count) {
            return Err(PinError::Meta(MetaFault::Malformed(SHARED_ROOT)));
        }
        let _inner = self.lock();
        Ok(self.register(entry, Arc::new(meta.trees))?)
    }

    /// Where the roots of entry `entry`'s version come from, if it may be
    /// pinned: the newest version (the last unit's entry), the newest
    /// checkpoint's, a version another pin holds, or an older checkpoint
    /// or a restore point on disk. The version a checkpoint is writing is
    /// refused: its roots are the checkpoint's own until it is on disk.
    fn pin_source(&self, inner: &Inner, entry: u64) -> Result<Source, &'static str> {
        if entry == inner.last_unit {
            let roots = inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect();
            return Ok(Source::Roots(Arc::new(roots)));
        }
        if entry == inner.newest.sb.entry.index {
            return Ok(Source::Roots(Arc::new(inner.newest.trees.clone())));
        }
        if let Some((_, (_, roots))) = lock(&self.pins).iter().find(|(v, _)| v.entry == entry) {
            return Ok(Source::Roots(roots.clone()));
        }
        if let Some(older) = inner.older.filter(|o| o.sb.entry.index == entry) {
            return Ok(Source::Meta(older.sb.meta));
        }
        let retiring = inner.older.map_or(&[][..], |_| &inner.newest.retiring[..]);
        if let Some(rp) = inner
            .newest
            .restore_points
            .iter()
            .chain(retiring)
            .find(|rp| rp.entry.index == entry)
        {
            return Ok(Source::Meta(rp.meta));
        }
        if inner.in_progress.is_some_and(|v| v.entry == entry) {
            return Err(
                "the version a checkpoint is writing: pin it once the checkpoint is on disk",
            );
        }
        Err(
            "a version the page file does not have: an entry after the last unit committed, \
             or one neither the newest nor kept",
        )
    }

    /// Adds a pin of entry `entry`'s version, whose trees are `roots`. Two
    /// pins of one entry are of one version, since only a version the page
    /// file has is pinned, so a second pin finds the roots the first
    /// registered equal to its own; if they ever differ the pin is refused
    /// rather than handed either.
    fn register(&self, entry: u64, roots: Roots) -> Result<Pin, &'static str> {
        let version = Version::at(entry);
        let mut pins = lock(&self.pins);
        let (n, held) = pins.entry(version).or_insert((0, roots.clone()));
        let same = Arc::ptr_eq(held, &roots) || **held == *roots;
        debug_assert!(same, "two pins of entry {entry} with different trees");
        if !same {
            return Err("two pins of one entry with different trees: a fault of the page file");
        }
        *n += 1;
        Ok(Pin {
            pins: self.pins.clone(),
            version,
            roots: held.clone(),
        })
    }

    /// Frees what can be freed among at most `budget` dying pages, taken in
    /// page order from where the last call stopped: a page's bytes are
    /// read when its birth or what it refers to is not known yet (a
    /// subtree dropped whole is taken apart here, a page at a time), and a
    /// page no kept version reaches and nothing holds is freed. What it
    /// decides depends on the pages it examines and their bytes, never on
    /// whether those were cached: the pages it discovers are settled only
    /// from pages not written yet, or when a later call examines them.
    ///
    /// A reclaim waits for a checkpoint being written, so that whether it
    /// settles a page before or after a checkpoint is decided by the order
    /// of the two calls, not by where the checkpoint's writes stand (a
    /// page settled while a checkpoint is planned gets the hold of the one
    /// after it). Dying pages are read from the file, never from the cache:
    /// a read error leaves the page dying, unsettled, with what it refers
    /// to untouched, is reported, and is met again by the next pass; the
    /// call goes on with the other pages, so that one damaged page cannot
    /// stop all reclaiming.
    pub fn reclaim(&self, budget: usize) -> Reclaimed {
        let _io = lock(&self.io);
        self.reclaim_held(budget)
    }

    /// [`PageFile::reclaim`], for a caller that holds `io`.
    fn reclaim_held(&self, budget: usize) -> Reclaimed {
        let mut out = Reclaimed::default();
        let (batch, whole) = {
            let mut inner = self.lock();
            let cursor = inner.cursor;
            let pending = inner.map.pending();
            let batch: Vec<(PageNo, Hash)> = pending
                .range(cursor..)
                .chain(pending.range(..cursor))
                .take(budget)
                .map(|(n, p)| (*n, p.hash))
                .collect();
            let whole = batch.len() == pending.len();
            inner.cursor = batch.last().map_or(0, |(n, _)| n + 1);
            (batch, whole)
        };
        for (n, hash) in batch {
            out.examined += 1;
            let needs_bytes = {
                let mut inner = self.lock();
                let kept = self.kept(&inner);
                out.freed += inner.settle(vec![n], &kept, &mut out.twice);
                inner
                    .map
                    .pending()
                    .get(&n)
                    .is_some_and(|r| r.hash == hash && !r.settled())
            };
            if !needs_bytes {
                continue;
            }
            out.read += 1;
            match self.read_uncached(n, &hash) {
                Ok(page) => {
                    let mut inner = self.lock();
                    if inner.map.pending().get(&n).is_some_and(|r| r.hash == hash) {
                        let mut work = Vec::new();
                        inner.learn(n, &page, &mut work, &mut out.twice);
                        work.push(n);
                        let kept = self.kept(&inner);
                        out.freed += inner.settle(work, &kept, &mut out.twice);
                    }
                }
                Err(fault) => out.faults.push(PageFault {
                    page: n,
                    hash,
                    fault,
                }),
            }
        }
        if whole {
            let inner = self.lock();
            let kept = self.kept(&inner);
            out.drained = inner
                .map
                .pending()
                .values()
                .all(|r| r.settled() && !kept.frees(r));
        }
        out
    }

    /// Stop keeping restore point `name`: the next checkpoint does not list
    /// it, and its pages are freed once no superblock on disk lists it,
    /// after the checkpoint after that.
    /// A drop waits for a checkpoint being written, which installs the list
    /// it started from.
    pub fn drop_restore_point(&self, name: &str) -> Result<(), &'static str> {
        let _io = lock(&self.io);
        let mut inner = self.lock();
        let before = inner.listed.len();
        inner.listed.retain(|rp| rp.name != name);
        if inner.listed.len() == before {
            return Err("no restore point of that name is listed");
        }
        Ok(())
    }

    /// Make a checkpoint of the newest version, at `entry`, of the trees as
    /// the page file holds them, kept as restore point `keep_as` when given
    /// (section 6.7). It takes no roots: units name every tree's root.
    ///
    /// 1. the pages the trees reach that are only in memory, and the
    ///    checkpoint's meta stream (its trees, its restore points, and the
    ///    map of free pages as of this checkpoint), are written, in page
    ///    order, to pages no kept version uses;
    /// 2. the file is flushed;
    /// 3. the superblock copy is written into the first slot of the pair
    ///    the newest checkpoint does not hold, and flushed;
    /// 4. then into the second slot of that pair, and flushed. The
    ///    checkpoint that pair held is then retired.
    ///
    /// A crash before step 3 has written its first slot leaves the two
    /// checkpoints before it as they were (step 1 wrote only pages neither
    /// uses); a crash during step 3 leaves the first slot torn or old and
    /// the second slot with the older checkpoint's copy; a crash during
    /// step 4 leaves the new checkpoint's first copy, whole and with its
    /// pages flushed. The refusals write nothing. A failed write or flush
    /// makes the page file refuse further writes.
    pub fn checkpoint(
        &self,
        entry: EntryId,
        keep_as: Option<&str>,
    ) -> Result<Checkpointed, CheckpointError> {
        let _io = lock(&self.io);
        if let Some(name) = keep_as {
            if name.is_empty() || name.len() > MAX_RESTORE_NAME {
                return Err(CheckpointError::Refused(
                    "a restore point name of 1 to 64 bytes",
                ));
            }
        }
        // The first check fixes the entry, and the point kept, in the same
        // hold of the lock: from here until the checkpoint is installed or
        // fails, nothing at or before the entry is reserved or committed,
        // and the room kept for meta streams counts the point.
        let _fixed = {
            let mut inner = self.lock();
            refusal(&inner, entry, keep_as)?;
            // A reservation open now was judged without the point counted;
            // replayed after a reopen on this checkpoint, it would meet a
            // floor that counts it (format section 4.8).
            if keep_as.is_some() && !inner.reservations.is_empty() {
                return Err(CheckpointError::Refused(
                    "a restore point is kept only while no reservation is open",
                ));
            }
            let number = inner.next_number;
            inner.fixed = Some(entry.index);
            inner.keeping = keep_as.map(|name| RestorePoint {
                name: name.to_owned(),
                checkpoint: number,
                entry,
                version: Version {
                    entry: entry.index,
                    step: number,
                },
                meta: (0, Hash::default()),
            });
            Fixed(self)
        };
        // Every tree's root that is not in memory is read back from the
        // file and checked before anything is written: this checkpoint's
        // copies replace the older checkpoint's, so the newest must be
        // usable first, or a root on disk that no longer reads back would
        // be named by both checkpoints left on disk.
        let roots: Vec<(TreeName, Root)> = {
            let inner = self.lock();
            inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect()
        };
        for (name, root) in &roots {
            self.read_uncached(root.page, &root.hash)
                .map_err(|fault| CheckpointError::Root {
                    tree: name.clone(),
                    fault: PageFault {
                        page: root.page,
                        hash: root.hash,
                        fault,
                    },
                })?;
        }
        // Short of room for the meta stream: free what can be freed first
        // (the checkpoint before last's meta stream, retired by the last).
        let short = {
            let inner = self.lock();
            let room = inner.meta_room(inner.trees.len(), inner.names);
            room.saturating_sub(inner.map.free_count().saturating_sub(inner.promised))
        };
        if short > 0 {
            self.reclaim_for(short);
        }
        let plan = {
            let mut inner = self.lock();
            self.plan(&mut inner, entry, keep_as)?
        };
        if let Err(e) = self.write_out(&plan) {
            self.lock().failed = true;
            return Err(CheckpointError::Disk(e));
        }
        let mut inner = self.lock();
        let done = Checkpointed {
            number: plan.sb.number,
            pages: plan.tree_pages.len() as u64,
            meta_pages: plan.meta_pages.len() as u64,
            bytes: (plan.tree_pages.len() + plan.meta_pages.len()) as u64 * PAGE + 2 * COPY as u64,
        };
        for page in plan.tree_pages.iter().chain(&plan.meta_pages) {
            if inner
                .dirty
                .get(&page.number())
                .is_some_and(|p| p.hash() == page.hash())
            {
                inner.dirty.remove(&page.number());
            }
            inner.cache.insert(page.clone());
        }
        let mut listed = plan.listed;
        for rp in &mut listed {
            if rp.checkpoint == plan.sb.number {
                rp.meta = plan.sb.meta;
            }
        }
        let old = std::mem::replace(
            &mut inner.newest,
            Newest {
                sb: plan.sb,
                version: plan.version,
                pair: plan.pair,
                meta_pages: plan
                    .meta_pages
                    .iter()
                    .map(|p| (p.number(), p.hash()))
                    .collect(),
                trees: plan.trees,
                restore_points: listed.clone(),
                retiring: plan.retiring,
            },
        );
        inner.newest_meta = inner.newest.meta_pages.iter().map(|(n, _)| *n).collect();
        inner.older = Some(Older {
            sb: old.sb,
            version: old.version,
        });
        inner.listed = listed;
        inner.in_progress = None;
        inner.fixed = None;
        inner.keeping = None;
        drop(inner);
        Ok(done)
    }

    fn plan(
        &self,
        inner: &mut Inner,
        entry: EntryId,
        keep_as: Option<&str>,
    ) -> Result<Plan, CheckpointError> {
        let refuse = |why| Err(CheckpointError::Refused(why));
        // Again, under the hold that plans: units of later entries may have
        // committed since the first check (nothing at or before the entry
        // can have).
        refusal(inner, entry, keep_as)?;
        // The trees as they are at the entry: a unit of a later entry
        // committed since the first check has refused the checkpoint above.
        let trees: Vec<(TreeName, Root)> =
            inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect();
        let Some(after) = inner.next_number.checked_add(1) else {
            return refuse("checkpoint numbers are exhausted");
        };
        // The pages to write: those in memory that the roots reach.
        let mut write: BTreeMap<PageNo, Arc<Page>> = BTreeMap::new();
        let mut stack: Vec<PageNo> = trees
            .iter()
            .filter(|(_, r)| inner.dirty.get(&r.page).is_some_and(|p| p.hash() == r.hash))
            .map(|(_, r)| r.page)
            .collect();
        while let Some(n) = stack.pop() {
            if write.contains_key(&n) {
                continue;
            }
            if inner.map.pending().contains_key(&n) {
                return refuse("a root reaches a page that left the newest version");
            }
            let page = inner.dirty[&n].clone();
            for (r, h) in page.references() {
                if !write.contains_key(&r) && inner.dirty.get(&r).is_some_and(|p| p.hash() == h) {
                    stack.push(r);
                }
            }
            write.insert(n, page);
        }
        if inner
            .dirty
            .keys()
            .any(|n| !write.contains_key(n) && !inner.map.pending().contains_key(n))
        {
            return refuse("a page written since the last checkpoint is reached by no tree");
        }
        // Dying pages in memory are taken apart now: after a restart only
        // the disk is left to read them from.
        let work: Vec<PageNo> = inner
            .map
            .pending()
            .iter()
            .filter(|(n, r)| r.expand && inner.dirty.contains_key(n))
            .map(|(n, _)| *n)
            .collect();
        let kept = self.kept(inner);
        let mut twice = Vec::new();
        inner.settle(work, &kept, &mut twice);

        let number = inner.next_number;
        let version = Version {
            entry: entry.index,
            step: number,
        };
        let mut listed = inner.listed.clone();
        if let Some(name) = keep_as {
            listed.push(RestorePoint {
                name: name.to_owned(),
                checkpoint: number,
                entry,
                version,
                meta: (0, Hash::default()),
            });
        }
        // By identity, not by name: a name dropped and kept again names a
        // new restore point, and the old one is kept while a superblock on
        // disk lists it.
        let retiring: Vec<RestorePoint> = inner
            .newest
            .restore_points
            .iter()
            .filter(|rp| !listed.iter().any(|l| l.version == rp.version))
            .cloned()
            .collect();
        // Room for the meta stream, counting the newest checkpoint's meta
        // pages, which die now.
        let names: usize = trees.iter().map(|(n, _)| n.as_bytes().len()).sum();
        let dying_meta = inner.newest.meta_pages.clone();
        let mut taken: Vec<PageNo> = Vec::new();
        loop {
            let len = meta::stream_len(
                trees.len(),
                names,
                &listed,
                &retiring,
                inner.map.end(),
                inner.map.pending().len() + dying_meta.len(),
                inner.map.shares().len(),
            );
            let need = meta::pages_for(len);
            if taken.len() >= need {
                debug_assert_eq!(taken.len(), need);
                break;
            }
            let want = (need - taken.len()) as u64;
            let available = inner.map.free_count().saturating_sub(inner.promised);
            if available < want {
                let end = inner.map.end();
                let short = want - available;
                let mut grown = None;
                for g in [short.max(self.config.growth), short] {
                    let Some(size) = end.checked_add(g).and_then(|e| e.checked_mul(PAGE)) else {
                        continue;
                    };
                    match self.file.allocate(size) {
                        Ok(()) => {
                            grown = Some(g);
                            break;
                        }
                        Err(e) if e.kind() == ErrorKind::NoSpace => continue,
                        Err(e) => {
                            // The allocation can write: a failure latches.
                            inner.failed = true;
                            for n in taken {
                                inner.map.give(n);
                            }
                            return Err(CheckpointError::Disk(e));
                        }
                    }
                }
                match grown {
                    Some(g) => inner.map.grow(end + g),
                    None => {
                        for n in taken {
                            inner.map.give(n);
                        }
                        return Err(CheckpointError::Space(SpaceError::Full {
                            wanted: want,
                            available,
                        }));
                    }
                }
            }
            for _ in 0..want {
                taken.push(inner.map.take_lowest().expect("pages available"));
            }
        }
        for (n, h) in &dying_meta {
            inner
                .map
                .die(*n, *h, Some(inner.newest.version), version, false)
                .expect("the newest checkpoint's meta pages are in use");
        }
        inner.map.carry_unsettled();
        let image = inner.map.image(&inner.held);
        let meta = Meta {
            number,
            entry,
            tree_format: page::FORMAT_VERSION,
            predecessor: Some((
                inner.newest.sb.number,
                inner.newest.version,
                inner.newest.sb.meta,
            )),
            trees: trees.clone(),
            restore_points: listed.clone(),
            retiring: retiring.clone(),
            map: image,
        };
        let stream = meta.encode();
        // What is written must be what an open accepts: the stream goes
        // through the reader's own checks before any byte of it is written.
        if let Err(why) = Meta::decode(&stream).and_then(|m| Map::from_image(&m.map).map(|_| ())) {
            // The plan has changed the map: the page file stops here.
            inner.failed = true;
            return Err(CheckpointError::Refused(why));
        }
        let (meta_pages, root) = meta::build(&stream, &taken, number);
        inner.in_progress = Some(version);
        inner.next_number = after;
        Ok(Plan {
            sb: Superblock {
                number,
                entry,
                tree_format: page::FORMAT_VERSION,
                meta: root,
            },
            version,
            pair: 1 - inner.newest.pair,
            tree_pages: write.into_values().collect(),
            meta_pages,
            trees,
            listed,
            retiring,
        })
    }

    fn write_out(&self, plan: &Plan) -> sys::Result<()> {
        let mut pages: Vec<&Arc<Page>> = plan.tree_pages.iter().chain(&plan.meta_pages).collect();
        pages.sort_by_key(|p| p.number());
        let mut i = 0;
        while i < pages.len() {
            let first = pages[i].number();
            let mut j = i + 1;
            while j < pages.len() && j - i < RUN && pages[j].number() == first + (j - i) as u64 {
                j += 1;
            }
            let mut bytes = Vec::with_capacity((j - i) * PAGE_SIZE);
            for page in &pages[i..j] {
                bytes.extend_from_slice(page.bytes());
            }
            self.file.write_at(first * PAGE, &bytes)?;
            i = j;
        }
        self.file.flush()?;
        let copy = superblock::encode(&plan.sb);
        for slot in pair_slots(plan.pair) {
            self.file.write_at((slot * SLOT) as u64, &copy)?;
            self.file.flush()?;
        }
        Ok(())
    }
}

/// Clears what a checkpoint fixed (its entry, the point it keeps, the
/// version it writes) on every way out of it. A checkpoint left by a panic
/// also latches the page file's failure: its plan may have changed the map
/// in memory, which must not be used again (the page file is opened again).
struct Fixed<'a>(&'a PageFile);

impl Drop for Fixed<'_> {
    fn drop(&mut self) {
        let mut inner = self.0.lock();
        inner.fixed = None;
        inner.keeping = None;
        inner.in_progress = None;
        if std::thread::panicking() {
            inner.failed = true;
        }
    }
}

/// What a checkpoint writes, decided under the lock.
struct Plan {
    sb: Superblock,
    version: Version,
    pair: usize,
    tree_pages: Vec<Arc<Page>>,
    meta_pages: Vec<Arc<Page>>,
    trees: Vec<(TreeName, Root)>,
    listed: Vec<RestorePoint>,
    retiring: Vec<RestorePoint>,
}

/// A checkpoint read at open.
struct Loaded {
    meta: Meta,
    map: Map,
    meta_pages: Vec<(PageNo, Hash)>,
}

/// Why a checkpoint tried could not be opened, short of the process
/// stopping: with its own table of restore points when that was read.
#[derive(Clone)]
enum Blocked {
    Damaged(Rejection, Option<Vec<RestorePoint>>),
    Unreadable(Unread, Option<Vec<RestorePoint>>),
}

impl Blocked {
    /// The restore points lost with it when the open falls back to
    /// checkpoint `below` (0 for none).
    fn lost(&self, below: u64) -> Lost {
        match self {
            Blocked::Damaged(_, points) | Blocked::Unreadable(_, points) => {
                lost_after(points.clone(), below)
            }
        }
    }
}

/// The error for checkpoint `sb`, blocked as `why`, when giving it up opens
/// on checkpoint `below` (0 for none), with the approval that gets past it.
fn blocked_error(
    sb: &Superblock,
    why: Blocked,
    below: u64,
    give_up: Option<Box<GiveUp>>,
    fallback: Option<Box<PassedOver>>,
) -> OpenError {
    let checkpoint = sb.number;
    match why {
        Blocked::Damaged(why, points) => OpenError::Damaged {
            passed: Box::new(PassedOver {
                number: checkpoint,
                why,
                lost: lost_after(points, below),
            }),
            give_up,
            fallback,
        },
        Blocked::Unreadable(Unread::Meta(fault), _) => OpenError::Unreadable {
            failure: ReadFailure::Meta {
                checkpoint,
                fault: Box::new(fault),
            },
            give_up,
            fallback,
        },
        Blocked::Unreadable(Unread::Root(tree, fault), points) => OpenError::Unreadable {
            failure: ReadFailure::Root {
                checkpoint,
                tree,
                fault: Box::new(fault),
                lost: lost_after(points, below),
            },
            give_up,
            fallback,
        },
    }
}

/// Checkpoint `sb`, blocked as `why`, passed over for checkpoint `below`.
fn passed(sb: &Superblock, why: Blocked, below: u64) -> PassedOver {
    let (why, points) = match why {
        Blocked::Damaged(why, points) => (why, points),
        Blocked::Unreadable(Unread::Meta(fault), points) => {
            (Rejection::Meta(MetaFault::Page(fault)), points)
        }
        Blocked::Unreadable(Unread::Root(tree, fault), points) => {
            (Rejection::Root { tree, fault }, points)
        }
    };
    PassedOver {
        number: sb.number,
        why,
        lost: lost_after(points, below),
    }
}

/// The read of a checkpoint the device failed.
#[derive(Clone)]
enum Unread {
    /// A page of its meta stream.
    Meta(PageFault),
    /// The root of one of its trees.
    Root(TreeName, PageFault),
}

/// Why [`Loaded::read`] did not load a checkpoint.
enum NotLoaded {
    /// Its bytes are not there, do not check, or break the format: the
    /// checkpoint is passed over. With its own table of restore points when
    /// that was read.
    Rejected(Box<Rejection>, Option<Vec<RestorePoint>>),
    /// The device failed a read of it, which says nothing of its bytes.
    Unreadable(Box<Unread>, Option<Vec<RestorePoint>>),
    /// A failure of the open itself (the process stopped), which says
    /// nothing of the checkpoint.
    Stopped(sys::Error),
}

impl Loaded {
    /// Reads and checks the checkpoint `sb` names.
    fn read(file: &dyn File, file_pages: PageNo, sb: &Superblock) -> Result<Loaded, NotLoaded> {
        let mut stopped = None;
        let mut device = None;
        let read = meta::read_stream(sb.meta, file_pages, |n, h| {
            load(file, file_pages, n, h).map_err(|e| match e {
                Unloaded::Bad(fault) => fault,
                Unloaded::Device(fault) => {
                    device = Some(fault.clone());
                    fault
                }
                Unloaded::Stopped(e) => {
                    let fault = PageFault {
                        page: n,
                        hash: *h,
                        fault: Fault::Io(e.to_string()),
                    };
                    stopped = Some(e);
                    fault
                }
            })
        });
        if let Some(e) = stopped {
            return Err(NotLoaded::Stopped(e));
        }
        let (stream, meta_pages) = match (read, device) {
            (Ok(read), _) => read,
            (Err(_), Some(fault)) => {
                return Err(NotLoaded::Unreadable(Box::new(Unread::Meta(fault)), None))
            }
            (Err(fault), None) => {
                return Err(NotLoaded::Rejected(Box::new(Rejection::Meta(fault)), None))
            }
        };
        let mut meta = match Meta::decode(&stream) {
            Ok(meta) => meta,
            Err(why) => {
                let why = Rejection::Meta(MetaFault::Malformed(why));
                return Err(NotLoaded::Rejected(Box::new(why), None));
            }
        };
        if meta.number != sb.number || meta.entry != sb.entry || meta.tree_format != sb.tree_format
        {
            // Not this checkpoint's table: its restore points are unknown.
            let why =
                Rejection::Mismatch("the meta stream names another checkpoint than its superblock");
            return Err(NotLoaded::Rejected(Box::new(why), None));
        }
        for rp in &mut meta.restore_points {
            if rp.meta.0 == 0 {
                rp.meta = sb.meta;
            }
        }
        // From here on the table read is the checkpoint's own: whatever
        // passes it over reports the restore points it listed.
        let points = meta.restore_points.clone();
        let reject = |why| Err(NotLoaded::Rejected(Box::new(why), Some(points.clone())));
        if meta
            .predecessor
            .is_some_and(|(n, v, _)| n >= sb.number || v.entry > sb.entry.index)
        {
            return reject(Rejection::Mismatch("a predecessor after the checkpoint"));
        }
        if meta.retiring.iter().any(|rp| rp.meta.0 == 0) {
            return reject(Rejection::Mismatch(
                "a restore point without its meta stream",
            ));
        }
        if meta.map.end.checked_mul(PAGE).is_none() {
            return reject(Rejection::Mismatch("a file larger than offsets reach"));
        }
        let map = match Map::from_image(&meta.map) {
            Ok(map) => map,
            Err(why) => return reject(Rejection::Mismatch(why)),
        };
        if meta_pages.iter().any(|(n, _)| !map.in_use(*n)) {
            return reject(Rejection::Mismatch(
                "a meta page the map does not count in use",
            ));
        }
        // A root page named by more trees than the map counts sharing it
        // (a clone shares its root; nothing writes clones yet): #719 could
        // write such a table from roots its caller gave, and dropping one
        // of the names would mark the page dying while another still names
        // it.
        if root_named_past_its_count(meta.trees.iter().map(|(_, r)| r), |n| map.count(n)) {
            return reject(Rejection::Mismatch(SHARED_ROOT));
        }
        // Each root is read, checked and let go: nothing here holds memory
        // in proportion to the trees beyond the table itself.
        for (tree, root) in &meta.trees {
            let page = match load(file, file_pages, root.page, &root.hash) {
                Ok(page) => page,
                Err(Unloaded::Stopped(e)) => return Err(NotLoaded::Stopped(e)),
                Err(Unloaded::Device(fault)) => {
                    let unread = Unread::Root(tree.clone(), fault);
                    return Err(NotLoaded::Unreadable(Box::new(unread), Some(points)));
                }
                Err(Unloaded::Bad(fault)) => {
                    return reject(Rejection::Root {
                        tree: tree.clone(),
                        fault,
                    })
                }
            };
            if page.kind() == PageKind::Overflow
                || page.written() > sb.entry.index
                || !map.in_use(root.page)
            {
                return reject(Rejection::Root {
                    tree: tree.clone(),
                    fault: PageFault {
                        page: root.page,
                        hash: root.hash,
                        fault: Fault::Malformed("not the root of a tree at the checkpoint"),
                    },
                });
            }
        }
        Ok(Loaded {
            meta,
            map,
            meta_pages,
        })
    }
}

/// Pages set aside for one entry (section 6.5), and the [`PageStore`] the
/// trees apply it through. Pages not taken when it is dropped are set
/// aside no longer.
pub struct Reservation<'f> {
    file: &'f PageFile,
    entry: u64,
    purpose: Purpose,
    remaining: AtomicU64,
    taken: AtomicU64,
    /// What it drew from the open groups' holds ([`PageFile::reserve_held`]).
    from_groups: AtomicU64,
}

impl fmt::Debug for Reservation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reservation")
            .field("entry", &self.entry)
            .field("remaining", &self.remaining())
            .finish()
    }
}

impl Reservation<'_> {
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The pages still set aside.
    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// The pages the entry took so far: those its units wrote, and the
    /// fixed room its trees made and renamed take in the tables. Applying
    /// an entry to the same trees takes the same number on every server and
    /// on every replay (tree format version 1 gives one shape per sequence
    /// of edits), so an entry can carry this count and recovery reserve
    /// exactly it.
    pub fn taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    /// Sets aside `pages` more for the same entry, as
    /// [`PageFile::reserve`] would: for a unit that ran out (its apply
    /// failed with [`StoreError::Full`] and changed nothing) to be applied
    /// again.
    pub fn extend(&self, pages: u64) -> Result<(), SpaceError> {
        self.file
            .set_aside(self.entry, pages, self.purpose, false)?;
        self.remaining.fetch_add(pages, Ordering::Relaxed);
        Ok(())
    }

    /// The page file.
    pub fn file(&self) -> &PageFile {
        self.file
    }

    /// The entry was not applied: what it drew from the open groups' holds,
    /// as far as it is still set aside, goes back to them.
    pub fn keep_held(&self) {
        let back = self
            .from_groups
            .swap(0, Ordering::Relaxed)
            .min(self.remaining.load(Ordering::Relaxed));
        self.remaining.fetch_sub(back, Ordering::Relaxed);
        self.file.lock().groups += back;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut inner = self.file.lock();
        inner.promised -= self.remaining.load(Ordering::Relaxed);
        inner.reservations.remove(&self.entry);
    }
}

impl PageStore for PageFile {
    type Entry<'a> = Unit<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        self.read_page(number, hash)
    }

    fn trees(&self) -> Vec<(TreeName, Root)> {
        let inner = self.lock();
        inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect()
    }

    fn root(&self, name: &TreeName) -> Option<Root> {
        self.lock().trees.get(name).copied()
    }

    /// Refused: changes go through a [`Reservation`].
    fn begin(&self) -> Result<Unit<'_>, StoreError> {
        Err(StoreError::Failed(
            "changes to the page file go through a reservation (PageFile::reserve)".into(),
        ))
    }
}

impl<'f> PageStore for Reservation<'f> {
    type Entry<'a>
        = Unit<'a>
    where
        Self: 'a;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        self.file.read_page(number, hash)
    }

    fn trees(&self) -> Vec<(TreeName, Root)> {
        self.file.trees()
    }

    fn root(&self, name: &TreeName) -> Option<Root> {
        self.file.root(name)
    }

    /// The entry's unit of work. One unit an entry, one at a time: refused
    /// while another unit is open, for an entry that has committed one (or
    /// before the last one that did), and for one a checkpoint covers. A
    /// unit dropped or refused may be begun again.
    fn begin(&self) -> Result<Unit<'_>, StoreError> {
        let mut inner = self.file.lock();
        if inner.failed {
            return Err(StoreError::Failed(FAILED.into()));
        }
        if inner.unit_open {
            return Err(store_failed("a unit while another is open: one at a time"));
        }
        if self.entry <= inner.cutoff() || self.entry <= inner.last_unit {
            return Err(store_failed(
                "a unit of an entry that has committed one, or one a checkpoint covers",
            ));
        }
        inner.unit_open = true;
        drop(inner);
        Ok(Unit {
            file: self.file,
            remaining: &self.remaining,
            taken: &self.taken,
            entry: self.entry,
            allocated: BTreeSet::new(),
            pages: BTreeMap::new(),
            released: Vec::new(),
            ahead: BTreeMap::new(),
            names: Vec::new(),
            committed: false,
        })
    }
}

/// Everything one entry changes ([`EntryPages`]): the pages of every tree
/// it changes, and the trees it makes, drops and renames and gives a new
/// root.
pub struct Unit<'a> {
    file: &'a PageFile,
    remaining: &'a AtomicU64,
    taken: &'a AtomicU64,
    entry: u64,
    allocated: BTreeSet<PageNo>,
    pages: BTreeMap<PageNo, Arc<Page>>,
    released: Vec<Releasing>,
    /// The counts this unit's earlier releases raise at commit: a shared
    /// page copied shares its references with the copy, and the tree
    /// releases a page before the pages it refers to.
    ahead: BTreeMap<PageNo, u32>,
    /// Changes to the trees, applied with the unit's pages.
    names: Vec<NameChange>,
    committed: bool,
}

/// A change to the trees, applied at commit.
#[derive(Clone, Debug)]
enum NameChange {
    Add(TreeName, Root),
    Drop(TreeName),
    Rename(TreeName, TreeName),
    SetRoot(TreeName, Root),
}

struct Releasing {
    number: PageNo,
    hash: Hash,
    how: Release,
    /// The page's references, for a shared page that is copied.
    references: Option<Vec<(PageNo, Hash)>>,
}

/// Why a checkpoint at `entry` of `trees` cannot be taken now, from the
/// page file's state alone: run before any page is read, so that a call the
/// page file refuses reads, reclaims and writes nothing, and again in the
/// plan.
fn refusal(inner: &Inner, entry: EntryId, keep_as: Option<&str>) -> Result<(), CheckpointError> {
    let refuse = |why| Err(CheckpointError::Refused(why));
    if inner.failed {
        return Err(CheckpointError::Failed);
    }
    let last = inner.newest.sb.entry;
    if entry.index < inner.last_unit {
        return refuse(
            "units of a later entry were committed: a checkpoint is of the newest version",
        );
    }
    if entry.index < last.index || (entry.index == last.index && entry != last) {
        return refuse("an entry before the newest checkpoint's, or another entry at its index");
    }
    if keep_as.is_some_and(|name| inner.listed.iter().any(|rp| rp.name == name)) {
        return refuse("a restore point of that name is listed");
    }
    if inner.listed.len() + usize::from(keep_as.is_some()) > MAX_RESTORE_POINTS {
        return refuse("more restore points than a checkpoint lists");
    }
    // A checkpoint is of one exact version: every unit of an entry it
    // covers is committed before it fixes its entry, and none after.
    if inner.reservations.range(..=entry.index).next().is_some() {
        return refuse("a reservation for an entry the checkpoint covers is open");
    }
    // The roots as the reader checks them at open (`Loaded::read`): each a
    // page in use in the newest version (not free, not dying, not a page
    // of the newest checkpoint's meta stream), named by no more trees than
    // the map counts sharing it. A unit cannot name another way, but a
    // table loaded from a checkpoint #719 wrote with roots from its caller
    // can (a tree's root that is a page inside another tree, which open
    // cannot see without walking every tree, dies when that tree is
    // dropped): such a checkpoint is refused before it replaces the
    // older one, which stays the fallback.
    if inner
        .trees
        .values()
        .any(|r| !inner.map.in_use(r.page) || inner.newest_meta.contains(&r.page))
    {
        return refuse("a root that is not a page of the newest version");
    }
    if inner
        .root_pages
        .iter()
        .any(|(n, trees)| *trees > inner.map.count(*n))
    {
        return refuse(SHARED_ROOT);
    }
    // The plan marks the newest checkpoint's meta pages dying. They are in
    // use unless a table #719 wrote aliased a page that was then freed and
    // taken for them, and the tree that still referred to it was taken
    // down (the map takes a child down by its number): refused here rather
    // than failing in the plan.
    if inner.newest_meta.iter().any(|n| !inner.map.in_use(*n)) {
        return refuse("a page of the newest checkpoint's meta stream is not in use");
    }
    Ok(())
}

/// Why a table is refused that names one root page for more trees than its
/// map counts reaching it.
const SHARED_ROOT: &str = "two trees name one root page, which the map does not record as shared";

/// Whether `roots` name one page for more trees than `count` says reach it
/// (a clone shares its root, with a share the map records; nothing writes
/// clones yet).
fn root_named_past_its_count<'r>(
    roots: impl Iterator<Item = &'r Root>,
    count: impl Fn(PageNo) -> u32,
) -> bool {
    let mut names_on: BTreeMap<PageNo, u32> = BTreeMap::new();
    for root in roots {
        *names_on.entry(root.page).or_default() += 1;
    }
    names_on.iter().any(|(n, names)| *names > count(*n))
}

/// Why a unit may not commit the pages it wrote, if it may not: one of
/// them refers to a page that is a tree's root once the unit takes effect
/// (a root the unit gives, or the root of a tree it leaves alone), and the
/// trees naming that page and the references to it are more than the map
/// counts reaching it. Dropping either tree would then free the page while
/// the other still reached it. Sharing a page between trees is what clones
/// will do, with a share the map records; nothing records one yet. The
/// other way round cannot happen: a root the unit gives is a page it wrote
/// under a number it allocated, which was free, and a page is freed only
/// once no version reaches it and what referred to it was taken down, so
/// no page that any version reaches refers to that number.
fn root_referred_to<'p>(
    inner: &Inner,
    view: &BTreeMap<TreeName, Option<Root>>,
    pages: impl Iterator<Item = &'p Arc<Page>>,
) -> Option<String> {
    // The trees naming each page once the unit takes effect: those named
    // now, less the old roots the unit replaces, drops or renames away,
    // plus the roots it gives.
    let mut change: BTreeMap<PageNo, i64> = BTreeMap::new();
    for (name, now) in view {
        if let Some(old) = inner.trees.get(name) {
            *change.entry(old.page).or_default() -= 1;
        }
        if let Some(root) = now {
            *change.entry(root.page).or_default() += 1;
        }
    }
    let names = |n: PageNo| {
        i64::from(inner.root_pages.get(&n).copied().unwrap_or(0))
            + change.get(&n).copied().unwrap_or(0)
    };
    // The references to such a page from the pages the unit writes.
    let mut referred: BTreeMap<PageNo, (i64, PageNo)> = BTreeMap::new();
    for page in pages {
        for (n, _) in page.references() {
            if names(n) > 0 {
                referred.entry(n).or_insert((0, page.number())).0 += 1;
            }
        }
    }
    referred
        .into_iter()
        .find(|(n, (refs, _))| names(*n) + refs > i64::from(inner.map.count(*n)))
        .map(|(n, (_, from))| {
            format!(
                "page {from} refers to page {n}, a tree's root, which the map does not record \
                 as shared"
            )
        })
}

/// The entry of the newest version: the last entry whose unit committed,
/// or the newest checkpoint's when it is later (an entry that changed
/// nothing has no version of its own).
fn newest_entry(inner: &Inner) -> u64 {
    inner.last_unit.max(inner.newest.sb.entry.index)
}

fn store_failed(why: impl Into<String>) -> StoreError {
    StoreError::Failed(why.into())
}

impl EntryPages for Unit<'_> {
    fn allocate(&mut self) -> Result<PageNo, StoreError> {
        let mut inner = self.file.lock();
        if inner.failed {
            return Err(store_failed(FAILED));
        }
        if self.remaining.load(Ordering::Relaxed) == 0 {
            return Err(StoreError::Full);
        }
        let n = inner.map.take_lowest().ok_or(StoreError::Full)?;
        self.remaining.fetch_sub(1, Ordering::Relaxed);
        inner.promised -= 1;
        inner.held.insert(n);
        self.allocated.insert(n);
        Ok(n)
    }

    fn write(&mut self, page: Arc<Page>) -> Result<(), StoreError> {
        let n = page.number();
        if !self.allocated.contains(&n) || self.pages.contains_key(&n) {
            return Err(store_failed(format!(
                "page {n} written without being allocated to the entry, or twice"
            )));
        }
        if page.written() != self.entry {
            return Err(store_failed(format!(
                "page {n} is stamped with entry {}, in a unit of entry {}",
                page.written(),
                self.entry
            )));
        }
        self.pages.insert(n, page);
        Ok(())
    }

    fn release(&mut self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError> {
        let count =
            self.file.lock().map.count(number) + self.ahead.get(&number).copied().unwrap_or(0);
        let references = if count > 1 && how == Release::Copied {
            let page = self
                .file
                .read_page(number, hash)
                .map_err(|f| store_failed(format!("page {number}: {f}")))?;
            let references = page.references();
            for (r, _) in &references {
                *self.ahead.entry(*r).or_default() += 1;
            }
            Some(references)
        } else {
            None
        };
        self.released.push(Releasing {
            number,
            hash: *hash,
            how,
            references,
        });
        Ok(())
    }

    fn add_tree(&mut self, name: &TreeName, root: Root) {
        self.names.push(NameChange::Add(name.clone(), root));
    }

    fn drop_tree(&mut self, name: &TreeName) {
        self.names.push(NameChange::Drop(name.clone()));
    }

    fn rename_tree(&mut self, from: &TreeName, to: &TreeName) {
        self.names
            .push(NameChange::Rename(from.clone(), to.clone()));
    }

    fn set_root(&mut self, name: &TreeName, root: Root) {
        self.names.push(NameChange::SetRoot(name.clone(), root));
    }

    fn commit(mut self) -> Result<(), StoreError> {
        let file = self.file;
        let mut inner = file.lock();
        if inner.failed {
            return Err(store_failed(FAILED));
        }
        if self.entry <= inner.cutoff() {
            return Err(store_failed(
                "an entry at or before a checkpoint's (on disk or being written)",
            ));
        }
        // The changes to the trees, checked in order against the trees as
        // the page file holds them (no other unit can change them while
        // this one is open). A root given must be a tree's page this unit
        // wrote; a tree given a new root must have had its old root
        // released by this unit; a dropped tree's root is released here;
        // no page this unit writes refers to a tree's root.
        let mut view: BTreeMap<TreeName, Option<Root>> = BTreeMap::new();
        let mut count = inner.trees.len();
        let mut room = 0u64;
        let mut drops: Vec<Releasing> = Vec::new();
        let mut replaced: BTreeMap<PageNo, Hash> = BTreeMap::new();
        let mut named: BTreeSet<PageNo> = BTreeSet::new();
        for change in &self.names {
            let current = |view: &BTreeMap<TreeName, Option<Root>>, n: &TreeName| match view.get(n)
            {
                Some(r) => *r,
                None => inner.trees.get(n).copied(),
            };
            let refused = |why: &str| Err(store_failed(format!("{why}: {change:?}")));
            let mut new_root = |root: &Root| -> Result<(), StoreError> {
                let tree_page = self
                    .pages
                    .get(&root.page)
                    .is_some_and(|p| p.hash() == root.hash && p.kind() != PageKind::Overflow);
                if !tree_page || !named.insert(root.page) {
                    return refused(
                        "a root that is not a tree's page this unit wrote, or named twice",
                    );
                }
                Ok(())
            };
            match change {
                NameChange::Add(n, root) => {
                    if current(&view, n).is_some() {
                        return refused("a tree of that name exists");
                    }
                    new_root(root)?;
                    view.insert(n.clone(), Some(*root));
                    count += 1;
                    room += TABLE_ROOM;
                }
                NameChange::Drop(n) => {
                    let Some(old) = current(&view, n) else {
                        return refused("no tree of that name");
                    };
                    if self.pages.contains_key(&old.page) {
                        return refused("a tree this unit made or gave a root is dropped by it");
                    }
                    drops.push(Releasing {
                        number: old.page,
                        hash: old.hash,
                        how: Release::Dropped,
                        references: None,
                    });
                    view.insert(n.clone(), None);
                    count -= 1;
                }
                NameChange::Rename(f, t) => {
                    let Some(root) = current(&view, f) else {
                        return refused("no tree of that name");
                    };
                    if current(&view, t).is_some() {
                        return refused("a tree of the new name exists");
                    }
                    view.insert(f.clone(), None);
                    view.insert(t.clone(), Some(root));
                    room += TABLE_ROOM;
                }
                NameChange::SetRoot(n, root) => {
                    let Some(old) = current(&view, n) else {
                        return refused("no tree of that name");
                    };
                    if self.pages.contains_key(&old.page) {
                        return refused("a tree this unit made or gave a root is given another");
                    }
                    new_root(root)?;
                    replaced.insert(old.page, old.hash);
                    view.insert(n.clone(), Some(*root));
                }
            }
            if count > MAX_TREES {
                return Err(store_failed(format!("more than {MAX_TREES} trees")));
            }
        }
        // A tree's root is released only as the old root of a tree this
        // unit gives a new one, and every such old root is released.
        for r in &self.released {
            let accounted = replaced.get(&r.number) == Some(&r.hash);
            if inner.root_pages.contains_key(&r.number) && !accounted {
                return Err(store_failed(format!(
                    "page {} released: the root of a tree this unit gives no new root",
                    r.number
                )));
            }
        }
        if let Some(old) = replaced
            .keys()
            .find(|n| !self.released.iter().any(|r| r.number == **n))
        {
            return Err(store_failed(format!(
                "page {old}, a tree's root, replaced but not released"
            )));
        }
        if let Some(why) = root_referred_to(&inner, &view, self.pages.values()) {
            return Err(store_failed(why));
        }
        // The releases, the dropped trees' roots last, are checked in order
        // against the counts they will meet, so that a page copied while
        // shared has its references.
        let mut seen = BTreeSet::new();
        let mut ahead: BTreeMap<PageNo, u32> = BTreeMap::new();
        for r in self.released.iter().chain(&drops) {
            if !seen.insert(r.number)
                || self.allocated.contains(&r.number)
                || inner.held.contains(&r.number)
                || inner.newest_meta.contains(&r.number)
                || !inner.map.in_use(r.number)
                || inner
                    .dirty
                    .get(&r.number)
                    .is_some_and(|p| p.hash() != r.hash)
            {
                return Err(store_failed(format!(
                    "page {} released though it is not in use, or with another hash",
                    r.number
                )));
            }
            let count = inner.map.count(r.number) + ahead.get(&r.number).copied().unwrap_or(0);
            if count > 1 && r.how == Release::Copied {
                let Some(references) = &r.references else {
                    return Err(store_failed(format!(
                        "page {} became shared while the entry was built",
                        r.number
                    )));
                };
                for (n, _) in references {
                    *ahead.entry(*n).or_default() += 1;
                }
            }
        }
        let unwritten = (self.allocated.len() - self.pages.len()) as u64;
        if self.remaining.load(Ordering::Relaxed) + unwritten < room {
            return Err(StoreError::Full);
        }
        // Everything is checked: from here on, nothing fails.
        self.taken
            .fetch_add(self.pages.len() as u64, Ordering::Relaxed);
        for (n, page) in std::mem::take(&mut self.pages) {
            inner.dirty.insert(n, page);
        }
        for n in std::mem::take(&mut self.allocated) {
            inner.held.remove(&n);
            if !inner.dirty.contains_key(&n) {
                inner.map.give(n);
                self.remaining.fetch_add(1, Ordering::Relaxed);
                inner.promised += 1;
            }
        }
        inner.last_unit = self.entry;
        let at = Version::at(self.entry);
        let mut died = Vec::new();
        for r in self.released.iter().chain(&drops) {
            let birth = inner
                .in_memory(r.number, &r.hash)
                .map(|p| Version::at(p.written()));
            let released = inner.map.release(
                r.number,
                r.hash,
                birth,
                at,
                r.how == Release::Dropped,
                r.references.as_deref(),
            );
            if released == Ok(Released::Died) {
                died.push(r.number);
            }
        }
        for (name, root) in view {
            if let Some(old) = inner.trees.remove(&name) {
                inner.names -= name.as_bytes().len();
                unroot(&mut inner.root_pages, old.page);
            }
            if let Some(root) = root {
                inner.names += name.as_bytes().len();
                *inner.root_pages.entry(root.page).or_default() += 1;
                inner.trees.insert(name, root);
            }
        }
        // The fixed room the trees' records take, from the reservation to
        // the floor (format section 4.8).
        self.remaining.fetch_sub(room, Ordering::Relaxed);
        self.taken.fetch_add(room, Ordering::Relaxed);
        inner.promised -= room;
        self.committed = true;
        let kept = file.kept(&inner);
        let mut twice = Vec::new();
        inner.settle(died, &kept, &mut twice);
        Ok(())
    }
}

/// One tree fewer has page `n` for its root.
fn unroot(root_pages: &mut BTreeMap<PageNo, u32>, n: PageNo) {
    if let Some(k) = root_pages.get_mut(&n) {
        *k -= 1;
        if *k == 0 {
            root_pages.remove(&n);
        }
    }
}

impl Drop for Unit<'_> {
    fn drop(&mut self) {
        let mut inner = self.file.lock();
        inner.unit_open = false;
        if self.committed {
            return;
        }
        for &n in &self.allocated {
            inner.held.remove(&n);
            inner.map.give(n);
            self.remaining.fetch_add(1, Ordering::Relaxed);
            inner.promised += 1;
        }
    }
}

#[cfg(test)]
impl PageFile {
    /// A clone of the tree whose root is `root`, named `name` (section 6.8),
    /// as far as the map goes: one more tree reaches the root page. The
    /// clone feature itself is not built; this lets tests drive share
    /// counts.
    pub(crate) fn clone_for_test(&self, name: &TreeName, root: Root) {
        let mut inner = self.lock();
        inner.map.share(root.page);
        inner.names += name.as_bytes().len();
        *inner.root_pages.entry(root.page).or_default() += 1;
        inner.trees.insert(name.clone(), root);
    }

    pub(crate) fn count_for_test(&self, n: PageNo) -> u32 {
        self.lock().map.count(n)
    }
}
