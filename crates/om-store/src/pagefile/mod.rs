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
//! through. Each [`Unit`] it begins is one tree's changes: numbers come
//! from the free pages and are held for the unit; written pages, releases
//! and the numbers it did not use take effect together on commit, and
//! nothing does on drop. Committed pages stay in memory until a checkpoint
//! writes them, and are served from there; they count against a budget,
//! past which a reservation is refused with
//! [`SpaceError::CheckpointNeeded`].
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
//! reaches it ([`map`] module docs); dying pages become free when
//! [`PageFile::reclaim`] finds them so (bounded work per call), or at once
//! on commit when the page never reached the disk and no kept version
//! reaches it. Pins live in memory; after a restart, what the superblocks
//! on disk name is what is kept.

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
pub use meta::{MetaFault, MAX_RESTORE_NAME, MAX_TREES};
pub use space::entry_bound;
pub use superblock::{pair_slots, Invalid, Superblock, COPY, SLOT, SLOTS};

use cache::Cache;
use map::{Child, Kept, Map, Released};
use meta::Meta;

use crate::page::{
    self, EntryPages, Fault, Hash, Page, PageFault, PageKind, PageNo, PageStore, Release,
    StoreError, PAGE_SIZE,
};
use crate::sys::{self, ErrorKind, File, FileOptions, FolderLock, Fs};
use crate::tree::{self, Root};

/// The file's name in the store folder.
pub const FILE: &str = "pages";

/// The name a new page file is written under before it is renamed into
/// place.
pub const NEW_FILE: &str = "pages.new";

const PAGE: u64 = PAGE_SIZE as u64;

/// The most pages one write of a checkpoint carries.
const RUN: usize = 64;

/// An entry of the log, as a checkpoint names the last one it includes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EntryId {
    pub index: u64,
    pub term: u64,
    pub hash: Hash,
}

/// The name of one of the state's trees: 1 to 255 bytes. The page file
/// gives names no meaning; the engine names one tree per hosted file and
/// one for the rest of the state (section 6.2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeName(Vec<u8>);

impl TreeName {
    /// The longest name, in bytes.
    pub const MAX: usize = 255;

    pub fn new(bytes: impl Into<Vec<u8>>) -> Option<TreeName> {
        let bytes = bytes.into();
        (!bytes.is_empty() && bytes.len() <= Self::MAX).then_some(TreeName(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for TreeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TreeName({})", self.0.escape_ascii())
    }
}

impl fmt::Display for TreeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.escape_ascii())
    }
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
    /// trees ([`PageFile::trees_of`]).
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
    /// A root given cannot be read; nothing was written.
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
                write!(f, "the root of tree {tree} cannot be read: {fault}")
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

/// A newer checkpoint than the one opened, and why it was not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassedOver {
    pub number: u64,
    pub why: Rejection,
}

/// What an open found and did (section 6.7, step 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The checkpoint used.
    pub checkpoint: u64,
    pub entry: EntryId,
    /// What each slot held.
    pub slots: [SlotSeen; SLOTS],
    /// Newer checkpoints tried first, and why each was not used.
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
}

/// Why the page file could not be opened or created.
#[derive(Debug)]
pub enum OpenError {
    /// The store folder has no `pages`.
    Missing,
    /// `pages` exists (creation).
    Exists,
    /// No slot holds a checkpoint that can be used: what each slot held,
    /// and why each checkpoint with a valid copy was passed over.
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
            OpenError::Disk(e) => write!(f, "{e}"),
            OpenError::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

impl std::error::Error for OpenError {}

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

/// A version kept from reuse while the pin lives (section 6.7). Pins are
/// in memory only.
pub struct Pin {
    pins: Arc<Mutex<BTreeMap<Version, usize>>>,
    version: Version,
}

impl Pin {
    /// The entry pinned.
    pub fn entry(&self) -> u64 {
        self.version.entry
    }
}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pin({})", self.version.entry)
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut pins = lock(&self.pins);
        if let Some(n) = pins.get_mut(&self.version) {
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
    newest: Newest,
    newest_meta: BTreeSet<PageNo>,
    older: Option<Older>,
    /// The restore points the next checkpoint lists.
    listed: Vec<RestorePoint>,
    /// The checkpoint being written.
    in_progress: Option<Version>,
    next_number: u64,
    /// The entry of the last unit committed (or of the newest checkpoint).
    last_unit: u64,
    /// The trees that exist now, by name: those of the newest checkpoint,
    /// and those made, dropped and renamed since through a reservation. A
    /// checkpoint names exactly these, and the room kept for it is sized
    /// from them.
    trees: BTreeSet<TreeName>,
    /// The bytes their names take.
    names: usize,
    /// A write or flush failed: nothing more is written (section 6.12).
    failed: bool,
    cursor: PageNo,
}

/// The page file (module docs).
pub struct PageFile {
    file: Box<dyn File>,
    config: Config,
    inner: Mutex<Inner>,
    pins: Arc<Mutex<BTreeMap<Version, usize>>>,
    /// Held by a checkpoint and by growth of the file, so that neither runs
    /// alongside the other.
    io: Mutex<()>,
}

impl fmt::Debug for PageFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageFile")
            .field("space", &self.space())
            .finish()
    }
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

/// Reads and checks page `number` of a file of `end` pages against `hash`.
/// A failure of the read itself other than the device's (the process
/// stopping) is `Err(Err(..))`: it says nothing of the page.
fn load(
    file: &dyn File,
    end: PageNo,
    number: PageNo,
    hash: &Hash,
) -> Result<Arc<Page>, Result<PageFault, sys::Error>> {
    let fault = |fault| PageFault {
        page: number,
        hash: *hash,
        fault,
    };
    let bytes = match raw(file, number, end) {
        Ok(bytes) => bytes,
        Err(RawRead::Io(e)) if e.kind() != ErrorKind::Read => return Err(Err(e)),
        Err(r) => return Err(Ok(fault(r.fault()))),
    };
    Page::verify(number, hash, bytes)
        .map(Arc::new)
        .map_err(|f| Ok(fault(f)))
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
        let mut listed = self.listed.clone();
        listed.push(spare);
        let len = meta::stream_len(
            trees,
            names,
            &listed,
            &self.newest.restore_points,
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
            Purpose::Entry => meta + config.reserve,
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
        ))
    }

    fn assemble(
        file: Box<dyn File>,
        config: Config,
        map: Map,
        newest: Newest,
        older: Option<Older>,
        next_number: u64,
    ) -> PageFile {
        let capacity = (config.cache_bytes / PAGE) as usize;
        let newest_meta = newest.meta_pages.iter().map(|(n, _)| *n).collect();
        let listed = newest.restore_points.clone();
        let last_unit = newest.sb.entry.index;
        let trees: BTreeSet<TreeName> = newest.trees.iter().map(|(n, _)| n.clone()).collect();
        let names = trees.iter().map(|n| n.as_bytes().len()).sum();
        PageFile {
            file,
            config,
            inner: Mutex::new(Inner {
                map,
                dirty: BTreeMap::new(),
                held: BTreeSet::new(),
                cache: Cache::new(capacity),
                promised: 0,
                newest,
                newest_meta,
                older,
                listed,
                in_progress: None,
                next_number,
                last_unit,
                trees,
                names,
                failed: false,
                cursor: 0,
            }),
            pins: Arc::default(),
            io: Mutex::new(()),
        }
    }

    /// Open the page file (section 6.7, step 2): read the four slots one at
    /// a time (one that cannot be read does not stop the open), and of the
    /// two newest checkpoints with a valid copy, newest first, use the
    /// first whose meta stream reads back and agrees with it and whose tree
    /// roots read back with the hashes it names. Then, before returning:
    ///
    /// 1. its copy is written into both slots of its pair, each flushed, so
    ///    that the device holds it twice (an earlier process's failed write
    ///    or flush can leave the system's cache showing a copy the device
    ///    does not hold, and a crash between a checkpoint's two copies
    ///    leaves an older checkpoint's copy in the second slot);
    /// 2. in the other pair, the predecessor's copy is written into both
    ///    slots, each flushed, when one of them holds it; otherwise both are
    ///    cleared. Either way a slot that held anything else (a newer
    ///    checkpoint passed over, whose pages the store will now reuse, a
    ///    stale copy, or bytes that could not be read) is overwritten before
    ///    any page is.
    ///
    /// The open itself frees nothing. Roots are read and checked one at a
    /// time and let go: what an open holds in memory is the meta stream and
    /// its table of trees, however many there are.
    pub fn open(
        fs: &dyn Fs,
        lock: &FolderLock,
        config: Config,
    ) -> Result<(PageFile, Opened), OpenError> {
        check_lock(fs, lock)?;
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
        for (s, seen) in slots.iter_mut().enumerate() {
            let mut buf = vec![0u8; SLOT];
            let n = match file.read_at((s * SLOT) as u64, &mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Read => {
                    *seen = SlotSeen::Unreadable(e.kind());
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
        let mut passed_over = Vec::new();
        let mut chosen = None;
        for number in numbers {
            let (slot, sb) = copies
                .iter()
                .enumerate()
                .find_map(|(s, c)| c.filter(|sb| sb.number == number).map(|sb| (s, sb)))
                .expect("a copy of each number");
            match Loaded::read(&*file, file_pages, &sb) {
                Ok(loaded) => {
                    chosen = Some((slot, sb, loaded));
                    break;
                }
                Err(Ok(why)) => passed_over.push(PassedOver { number, why }),
                Err(Err(e)) => return Err(e.into()),
            }
        }
        let Some((slot, sb, loaded)) = chosen else {
            return Err(OpenError::NoCheckpoint { slots, passed_over });
        };

        // Its copy into both slots of its pair, each flushed.
        let pair = superblock::pair_of(slot);
        let copy = superblock::encode(&sb);
        let mut rewritten = Vec::new();
        for s in pair_slots(pair) {
            file.write_at((s * SLOT) as u64, &copy)?;
            file.flush()?;
            rewritten.push(s);
        }
        // The other pair: the predecessor's copy in both slots when one of
        // them holds it; otherwise both cleared. A slot that held anything
        // else (a checkpoint passed over, a stale copy, bytes that could
        // not be read) is overwritten before any page is: a checkpoint
        // whose pages the store may now reuse must never be chosen later.
        // The predecessor is the checkpoint whose number and meta stream
        // the chosen one names: a number alone can belong to another
        // checkpoint that never reached the disk whole.
        let predecessor = loaded.meta.predecessor;
        let older_sb = pair_slots(1 - pair).into_iter().find_map(|s| {
            copies[s].filter(|c| predecessor.is_some_and(|(n, _, m)| c.number == n && c.meta == m))
        });
        let mut cleared = Vec::new();
        for s in pair_slots(1 - pair) {
            let other = !blank[s] && (copies[s].is_none() || copies[s] != older_sb);
            match older_sb {
                Some(older) if copies[s] == Some(older) => continue,
                Some(older) => {
                    file.write_at((s * SLOT) as u64, &superblock::encode(&older))?;
                    rewritten.push(s);
                }
                None if blank[s] => continue,
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
        };
        let pf = PageFile::assemble(file, config, loaded.map, newest, older, next_number);
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
        self.lock().trees.iter().cloned().collect()
    }

    /// The trees of the newest checkpoint on disk, by name.
    pub fn checkpoint_trees(&self) -> Vec<(TreeName, Root)> {
        self.lock().newest.trees.clone()
    }

    /// The entry of the newest version: the last unit committed, or the
    /// newest checkpoint's.
    pub fn newest_entry(&self) -> u64 {
        self.lock().last_unit
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

    /// Set aside `pages` pages for entry `entry` (section 6.5): the caller's
    /// upper bound for applying it ([`entry_bound`]). An entry is refused
    /// with nothing reserved when its pages, the room for a checkpoint's
    /// meta stream and (for [`Purpose::Entry`]) the reserve do not fit even
    /// after the file grows; the store stays readable and can checkpoint.
    pub fn reserve(
        &self,
        entry: u64,
        pages: u64,
        purpose: Purpose,
    ) -> Result<Reservation<'_>, SpaceError> {
        self.set_aside(entry, pages, purpose)?;
        Ok(Reservation {
            file: self,
            entry,
            purpose,
            remaining: AtomicU64::new(pages),
            taken: AtomicU64::new(0),
        })
    }

    /// Adds `pages` to what is promised for `entry` (the checks of
    /// [`PageFile::reserve`]). When the free pages fall short it first frees
    /// what it can ([`PageFile::reclaim`], which reads pages if it must),
    /// then grows the file: so the space a sequence of entries needs does
    /// not depend on when the caller reclaims, and recovery, which applies
    /// the same entries, has at least the free pages the first run had
    /// (format section 4.8).
    fn set_aside(&self, entry: u64, pages: u64, purpose: Purpose) -> Result<(), SpaceError> {
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
                if entry <= inner.newest.sb.entry.index {
                    return Err(SpaceError::Refused(
                        "an entry at or before the newest checkpoint's",
                    ));
                }
                if entry < inner.last_unit {
                    return Err(SpaceError::Refused("an entry before the last one applied"));
                }
                let budget = self.config.dirty_budget;
                if inner.dirty.len() as u64 >= budget {
                    let dirty = inner.live_dirty();
                    if dirty >= budget {
                        return Err(SpaceError::CheckpointNeeded { dirty, budget });
                    }
                }
                let floor = inner.floor(purpose, &self.config);
                let available = inner.map.free_count().saturating_sub(inner.promised);
                if available >= pages.saturating_add(floor) {
                    inner.promised += pages;
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
                self.reclaim_for(deficit);
                continue;
            }
            let _io = lock(&self.io);
            self.grow(deficit)?;
            grown += 1;
        }
    }

    /// Reclaims, in one pass over the dying pages at most, until `pages`
    /// more are free.
    fn reclaim_for(&self, pages: u64) {
        let (start, mut left) = {
            let inner = self.lock();
            (inner.map.free_count(), inner.map.pending().len())
        };
        while left > 0 {
            let done = self.reclaim(left.min(4096));
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
                return Err(SpaceError::Refused("a file larger than offsets reach"));
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

    /// Keep entry `entry`'s version from reuse while the pin lives. The
    /// entry must be the newest version's (every unit of it committed, and
    /// none of a later entry; an entry that changed nothing has the version
    /// of the one before it), or one a checkpoint, restore point or pin
    /// keeps now: an older version may be partly reused already.
    pub fn pin(&self, entry: u64) -> Result<Pin, &'static str> {
        let inner = self.lock();
        let kept = self.kept(&inner);
        if entry < inner.last_unit
            && !kept
                .checkpoints
                .iter()
                .chain(&kept.pins)
                .any(|v| v.entry == entry)
        {
            return Err("a version that is neither the newest nor kept");
        }
        let version = Version::at(entry);
        *lock(&self.pins).entry(version).or_default() += 1;
        Ok(Pin {
            pins: self.pins.clone(),
            version,
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
    pub fn reclaim(&self, budget: usize) -> Reclaimed {
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
            // The same bytes whether the cache holds them or not.
            match self.read_page(n, &hash) {
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

    /// Make a checkpoint of the newest version, at `entry`, whose trees are
    /// `trees` (every tree of the state; their roots in the newest
    /// version), kept as restore point `keep_as` when given (section 6.7):
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
        trees: &[(TreeName, Root)],
        keep_as: Option<&str>,
    ) -> Result<Checkpointed, CheckpointError> {
        let _io = lock(&self.io);
        let mut trees = trees.to_vec();
        trees.sort_by(|a, b| a.0.cmp(&b.0));
        if trees.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(CheckpointError::Refused("a tree named twice"));
        }
        if trees.len() > MAX_TREES {
            return Err(CheckpointError::Refused("too many trees"));
        }
        if let Some(name) = keep_as {
            if name.is_empty() || name.len() > MAX_RESTORE_NAME {
                return Err(CheckpointError::Refused(
                    "a restore point name of 1 to 64 bytes",
                ));
            }
        }
        // Roots not in memory are read and checked first.
        for (name, root) in &trees {
            let in_memory = self
                .lock()
                .dirty
                .get(&root.page)
                .is_some_and(|p| p.hash() == root.hash);
            let page = if in_memory {
                None
            } else {
                Some(self.read_page(root.page, &root.hash).map_err(|fault| {
                    CheckpointError::Root {
                        tree: name.clone(),
                        fault: PageFault {
                            page: root.page,
                            hash: root.hash,
                            fault,
                        },
                    }
                })?)
            };
            if page.is_some_and(|p| p.kind() == PageKind::Overflow || p.written() > entry.index) {
                return Err(CheckpointError::Refused(
                    "a root that is not a tree's at the entry",
                ));
            }
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
            self.plan(&mut inner, entry, trees, keep_as)?
        };
        if let Err(e) = self.write_out(&plan) {
            let mut inner = self.lock();
            inner.failed = true;
            inner.in_progress = None;
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
        Ok(done)
    }

    fn plan(
        &self,
        inner: &mut Inner,
        entry: EntryId,
        trees: Vec<(TreeName, Root)>,
        keep_as: Option<&str>,
    ) -> Result<Plan, CheckpointError> {
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
            return refuse(
                "an entry before the newest checkpoint's, or another entry at its index",
            );
        }
        if keep_as.is_some_and(|name| inner.listed.iter().any(|rp| rp.name == name)) {
            return refuse("a restore point of that name is listed");
        }
        if trees.len() != inner.trees.len() || trees.iter().any(|(n, _)| !inner.trees.contains(n)) {
            return refuse(
                "the trees given are not the trees that exist (made and dropped through a \
                 reservation)",
            );
        }
        let Some(after) = inner.next_number.checked_add(1) else {
            return refuse("checkpoint numbers are exhausted");
        };
        for (_, root) in &trees {
            if !inner.map.in_use(root.page) || inner.newest_meta.contains(&root.page) {
                return refuse("a root that is not a page of the newest version");
            }
        }
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
            return refuse("a page written since the last checkpoint is reached by no tree given");
        }
        let given: BTreeSet<PageNo> = trees.iter().map(|(_, r)| r.page).collect();
        if inner
            .newest
            .trees
            .iter()
            .any(|(_, r)| inner.map.in_use(r.page) && !given.contains(&r.page))
        {
            return refuse("a tree of the newest checkpoint is neither given nor dropped");
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
                        break;
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

impl Loaded {
    /// Reads and checks the checkpoint `sb` names. `Err(Ok(..))` rejects
    /// the checkpoint; `Err(Err(..))` is a failure of the open itself (the
    /// process stopped), which says nothing of the checkpoint.
    fn read(
        file: &dyn File,
        file_pages: PageNo,
        sb: &Superblock,
    ) -> Result<Loaded, Result<Rejection, sys::Error>> {
        let mut stopped = None;
        let read = meta::read_stream(sb.meta, file_pages, |n, h| {
            load(file, file_pages, n, h).map_err(|e| match e {
                Ok(fault) => fault,
                Err(e) => {
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
            return Err(Err(e));
        }
        let (stream, meta_pages) = read.map_err(|f| Ok(Rejection::Meta(f)))?;
        let reject = |r| Err(Ok(r));
        let mut meta = match Meta::decode(&stream) {
            Ok(meta) => meta,
            Err(why) => return reject(Rejection::Meta(MetaFault::Malformed(why))),
        };
        if meta.number != sb.number || meta.entry != sb.entry || meta.tree_format != sb.tree_format
        {
            return reject(Rejection::Mismatch(
                "the meta stream names another checkpoint than its superblock",
            ));
        }
        if meta
            .predecessor
            .is_some_and(|(n, v, _)| n >= sb.number || v.entry > sb.entry.index)
        {
            return reject(Rejection::Mismatch("a predecessor after the checkpoint"));
        }
        for rp in &mut meta.restore_points {
            if rp.meta.0 == 0 {
                rp.meta = sb.meta;
            }
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
        // Each root is read, checked and let go: nothing here holds memory
        // in proportion to the trees beyond the table itself.
        for (tree, root) in &meta.trees {
            let page = match load(file, file_pages, root.page, &root.hash) {
                Ok(page) => page,
                Err(Err(e)) => return Err(Err(e)),
                Err(Ok(fault)) => {
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

    /// The pages the units committed so far wrote. Applying an entry to the
    /// same trees writes the same number of pages on every server and on
    /// every replay (tree format version 1 gives one shape per sequence of
    /// edits), so an entry can carry this count and recovery reserve
    /// exactly it.
    pub fn taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    /// Sets aside `pages` more for the same entry, as
    /// [`PageFile::reserve`] would: for a unit that ran out (its apply
    /// failed with [`StoreError::Full`] and changed nothing) to be applied
    /// again.
    pub fn extend(&self, pages: u64) -> Result<(), SpaceError> {
        self.file.set_aside(self.entry, pages, self.purpose)?;
        self.remaining.fetch_add(pages, Ordering::Relaxed);
        Ok(())
    }

    /// The page file.
    pub fn file(&self) -> &PageFile {
        self.file
    }

    /// Makes tree `name`, empty, at this entry ([`tree::create`]), and adds
    /// it to the trees that exist, which the next checkpoint must name.
    /// Making a tree takes its root page and the room its name takes in
    /// the tables of the two checkpoints the floor keeps room for, both
    /// from this reservation: reserve 3 pages for each. Refused, with
    /// nothing done, when a tree of that name exists or the reservation is
    /// short.
    pub fn create_tree(&self, name: &TreeName) -> Result<Root, tree::Error> {
        let room = self.table_room(None, Some(name))?;
        match tree::create(self, self.entry) {
            Ok(root) => Ok(root),
            Err(e) => {
                self.give_table_room(None, Some(name), room);
                Err(e)
            }
        }
    }

    /// Drops tree `name`, whose root at this entry is `root`: the root is
    /// released with all it reaches (taken down later, a page at a time),
    /// and the name leaves the trees that exist.
    pub fn drop_tree(&self, name: &TreeName, root: Root) -> Result<(), StoreError> {
        if !self.file.lock().trees.contains(name) {
            return Err(store_failed(format!("no tree {name} exists")));
        }
        let mut unit = self.begin()?;
        unit.release(root.page, &root.hash, Release::Dropped)?;
        unit.commit()?;
        let mut inner = self.file.lock();
        if inner.trees.remove(name) {
            inner.names -= name.as_bytes().len();
        }
        Ok(())
    }

    /// Gives tree `from` the name `to` at this entry; its pages stay as they
    /// are. A longer name's room in the table comes from this reservation.
    pub fn rename_tree(&self, from: &TreeName, to: &TreeName) -> Result<(), StoreError> {
        if !self.file.lock().trees.contains(from) {
            return Err(store_failed(format!("no tree {from} exists")));
        }
        self.table_room(Some(from), Some(to)).map_err(|e| match e {
            tree::Error::Store(e) => e,
            e => store_failed(e.to_string()),
        })?;
        Ok(())
    }

    /// Moves tree names in the table (`from` out, `to` in), taking from
    /// this reservation the pages by which that raises the room a
    /// checkpoint's meta stream needs. Returns them.
    fn table_room(
        &self,
        from: Option<&TreeName>,
        to: Option<&TreeName>,
    ) -> Result<u64, tree::Error> {
        let mut inner = self.file.lock();
        if inner.failed {
            return Err(tree::Error::Store(store_failed(FAILED)));
        }
        if to.is_some_and(|t| inner.trees.contains(t) && Some(t) != from) {
            return Err(tree::Error::Store(store_failed(
                "a tree of that name exists".to_string(),
            )));
        }
        let len = |n: Option<&TreeName>| n.map_or(0, |n| n.as_bytes().len());
        let count = inner.trees.len() - usize::from(from.is_some()) + usize::from(to.is_some());
        if count > MAX_TREES {
            return Err(tree::Error::Store(store_failed(format!(
                "more than {MAX_TREES} trees"
            ))));
        }
        let names = inner.names - len(from) + len(to);
        let before = 2 * inner.meta_room(inner.trees.len(), inner.names);
        let room = (2 * inner.meta_room(count, names)).saturating_sub(before);
        // A tree made needs its root page from the reservation as well.
        let needed = room + u64::from(to.is_some() && from.is_none());
        if self.remaining.load(Ordering::Relaxed) < needed {
            return Err(tree::Error::Store(StoreError::Full));
        }
        self.remaining.fetch_sub(room, Ordering::Relaxed);
        inner.promised -= room;
        if let Some(f) = from {
            inner.trees.remove(f);
        }
        if let Some(t) = to {
            inner.trees.insert(t.clone());
        }
        inner.names = names;
        Ok(room)
    }

    /// Undoes [`Reservation::table_room`].
    fn give_table_room(&self, from: Option<&TreeName>, to: Option<&TreeName>, room: u64) {
        let mut inner = self.file.lock();
        let len = |n: Option<&TreeName>| n.map_or(0, |n| n.as_bytes().len());
        if let Some(t) = to {
            inner.trees.remove(t);
        }
        if let Some(f) = from {
            inner.trees.insert(f.clone());
        }
        inner.names = inner.names - len(to) + len(from);
        self.remaining.fetch_add(room, Ordering::Relaxed);
        inner.promised += room;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut inner = self.file.lock();
        inner.promised -= self.remaining.load(Ordering::Relaxed);
    }
}

impl PageStore for PageFile {
    type Entry<'a> = Unit<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        self.read_page(number, hash)
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

    fn begin(&self) -> Result<Unit<'_>, StoreError> {
        if self.file.lock().failed {
            return Err(StoreError::Failed(FAILED.into()));
        }
        Ok(Unit {
            file: self.file,
            remaining: &self.remaining,
            taken: &self.taken,
            entry: self.entry,
            allocated: BTreeSet::new(),
            pages: BTreeMap::new(),
            released: Vec::new(),
            ahead: BTreeMap::new(),
            committed: false,
        })
    }
}

/// One tree's changes for one entry ([`EntryPages`]).
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
    committed: bool,
}

struct Releasing {
    number: PageNo,
    hash: Hash,
    how: Release,
    /// The page's references, for a shared page that is copied.
    references: Option<Vec<(PageNo, Hash)>>,
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

    fn commit(mut self) -> Result<(), StoreError> {
        let file = self.file;
        let mut inner = file.lock();
        if inner.failed {
            return Err(store_failed(FAILED));
        }
        if self.entry <= inner.newest.sb.entry.index {
            return Err(store_failed(
                "an entry at or before the newest checkpoint's",
            ));
        }
        if self.entry < inner.last_unit {
            return Err(store_failed(format!(
                "entry {} committed after entry {}",
                self.entry, inner.last_unit
            )));
        }
        // The releases are checked in order against the counts they will
        // meet, so that a page copied while shared has its references.
        let mut seen = BTreeSet::new();
        let mut ahead: BTreeMap<PageNo, u32> = BTreeMap::new();
        for r in &self.released {
            if !seen.insert(r.number)
                || self.allocated.contains(&r.number)
                || inner.held.contains(&r.number)
                || inner.newest_meta.contains(&r.number)
                || !inner.map.in_use(r.number)
            {
                return Err(store_failed(format!(
                    "page {} released though it is not in use",
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
        for r in &self.released {
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
        self.committed = true;
        let kept = file.kept(&inner);
        let mut twice = Vec::new();
        inner.settle(died, &kept, &mut twice);
        Ok(())
    }
}

impl Drop for Unit<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut inner = self.file.lock();
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
    pub(crate) fn clone_for_test(&self, name: &TreeName, root: PageNo) {
        let mut inner = self.lock();
        inner.map.share(root);
        inner.names += name.as_bytes().len();
        inner.trees.insert(name.clone());
    }

    pub(crate) fn count_for_test(&self, n: PageNo) -> u32 {
        self.lock().map.count(n)
    }
}
