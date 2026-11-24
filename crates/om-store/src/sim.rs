//! A simulated machine for the store engine (`docs/server-storage.md`
//! section 12): its disk, clock and randomness in memory, driven by a seed,
//! with crashes and storage faults injected at chosen steps or drawn from
//! the seed. [`Sim`] is the machine; [`SimFs`], [`SimClock`] and
//! [`SimRandom`] implement the traits of [`crate::sys`] on it.
//!
//! # The disk
//!
//! For every file the simulator keeps what the device holds (durable),
//! what reads see (the system's cache), and the writes the device has not
//! been given yet (pending), in order. For every folder it keeps its
//! durable entries, its cached entries and its pending creates, renames
//! and removes. A process sees the cache.
//!
//! - A write goes to the cache and to the pending list; a write that
//!   reaches past the end also records the new size, which settles on its
//!   own. On a file opened for synchronous writes it goes to the device
//!   before it returns, over any pending write of the same bytes.
//! - A flush that succeeds gives the device every pending write of the
//!   file, in order. A folder flush does the same with the folder's
//!   pending changes.
//! - **A crash** (a power loss) settles every pending write and folder
//!   change on its own, in order: kept, lost, or, for a write, kept in
//!   part: a prefix cut at any byte, or any subset of its 512-byte sectors.
//!   A size change is kept or lost. The cache is then gone, and a file or
//!   folder no durable entry reaches is gone with it. A crash keeps
//!   everything durable as it was; it never invents bytes no write held.
//! - **A flush that fails** settles what it covered at once in the same
//!   way, and the cache keeps what was written or drops it (drawn). The
//!   pending list is then empty, so no later flush gives the device what
//!   the failed one lost: a later successful flush does not vouch for it
//!   (PostgreSQL's 2018 finding; section 6.12).
//! - **A folder flush that fails** gives the device the folder's pending
//!   changes up to a point drawn from the seed, in order, and leaves the
//!   rest pending; the cached entries stay. Folder changes are file system
//!   metadata, which journaling file systems (ext4, XFS, btrfs) commit in
//!   order and, after an error, stop committing rather than drop: what a
//!   failed folder flush did not write is written by a later one or
//!   settled by a crash, never silently lost.
//! - **A stop** ([`Sim::restart`] without a crash) ends the process and
//!   keeps the cache and the pending writes, as the system does.
//!
//! # Faults
//!
//! A step is one call of a [`Fs`] or [`File`] method. [`Sim::place`] puts
//! a [`Fault`] at an exact step: it fires at the first call at or after
//! that step that it applies to. [`Rates`] draw faults at every step. What
//! each fault did is recorded as an [`Event`].
//!
//! After a crash every call fails with [`ErrorKind::Halted`] until
//! [`Sim::restart`]. Handles and the lock belong to the process that made
//! them: after a restart they fail with [`ErrorKind::Halted`] too.
//!
//! # Replay
//!
//! A run is a function of its seed, the faults placed and the calls made:
//! every choice comes from streams derived from the seed, maps are ordered
//! by key, and nothing reads a clock, an address or another thread. The
//! same seed and calls give the same events and the same bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::sys::{
    self, Clock, Error, ErrorKind, File, FileOptions, FolderLock, Fs, Op, Random, Result,
};

/// The unit a crash keeps or loses a part of a write by, from the file's
/// start.
pub const SECTOR: u64 = 512;

/// The disk's size unless [`Sim::set_capacity`] says otherwise. Writes
/// past it fail with [`ErrorKind::NoSpace`], which also keeps a test from
/// asking for more memory than it has.
pub const DEFAULT_CAPACITY: u64 = 1 << 30;

/// The longest run of bytes a drawn [`Fault::Damage`] changes.
const MAX_DAMAGE: u64 = 16;

/// The wall clock's start: 2026-01-01T00:00:00Z.
const WALL_START: Duration = Duration::from_secs(1_767_225_600);

/// The store folder's node.
const ROOT: u64 = 0;

/// A seeded generator (SplitMix64): the simulator's source of choices, and
/// a source of deterministic plans for tests.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self::stream(seed, 0)
    }

    /// The generator `stream` of `seed`: streams of one seed are
    /// independent of each other.
    pub fn stream(seed: u64, stream: u64) -> Self {
        let mut mix = Rng { state: seed };
        let state = mix.next_u64() ^ stream.wrapping_mul(0xD1B5_4A32_D192_ED03);
        Rng { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `n` (0 when `n` is 0).
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }

    /// True once in `n` draws on average; never when `n` is 0.
    pub fn one_in(&mut self, n: u32) -> bool {
        n != 0 && self.below(u64::from(n)) == 0
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

/// A fault the simulator injects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fault {
    /// The power fails during the call: what it was doing is pending, and
    /// the crash settles it with everything else not flushed.
    Crash,
    /// A file or folder flush fails (module docs).
    FailFlush,
    /// A write fails after writing a part of its data, possibly none or
    /// all of it: a failed or short write. An allocation fails part way.
    FailWrite,
    /// The disk is full. A write or allocation fails after doing a part
    /// of its work, never all; a create fails with nothing done.
    NoSpace,
    /// A read fails.
    FailRead,
    /// Bytes the device holds change: up to 16 bytes of a file drawn from
    /// those on the device. It happens before the call, which then runs.
    /// Reads see the damage once the cache does not hold that file's
    /// bytes: at once for a file with nothing unflushed, otherwise after a
    /// crash.
    Damage,
    /// A write reaches the device at another place in its file, a whole
    /// number of sectors away; the cache has it in the right place.
    Misplace,
    /// The device drops a write it reported done; the cache has it.
    LoseWrite,
}

impl Fault {
    /// Whether the fault can fire in operation `op`.
    pub fn applies(self, op: Op) -> bool {
        match self {
            Fault::Crash | Fault::Damage => true,
            Fault::FailFlush => matches!(op, Op::Flush | Op::FlushFolder),
            Fault::FailWrite => matches!(op, Op::Write | Op::Allocate),
            Fault::NoSpace => {
                matches!(op, Op::Write | Op::Allocate | Op::Create | Op::CreateFolder)
            }
            Fault::FailRead => op == Op::Read,
            Fault::Misplace | Fault::LoseWrite => op == Op::Write,
        }
    }
}

/// How often each fault is drawn: once in so many steps it applies to, on
/// average; 0 never.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rates {
    pub crash: u32,
    pub fail_flush: u32,
    pub fail_write: u32,
    pub no_space: u32,
    pub fail_read: u32,
    pub damage: u32,
    pub misplace: u32,
    pub lose_write: u32,
}

impl Rates {
    fn of(&self, fault: Fault) -> u32 {
        match fault {
            Fault::Crash => self.crash,
            Fault::FailFlush => self.fail_flush,
            Fault::FailWrite => self.fail_write,
            Fault::NoSpace => self.no_space,
            Fault::FailRead => self.fail_read,
            Fault::Damage => self.damage,
            Fault::Misplace => self.misplace,
            Fault::LoseWrite => self.lose_write,
        }
    }
}

/// How a crash settles what was not flushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fates {
    /// Each write and change on its own, drawn from the seed.
    Drawn,
    /// All of it reached the device.
    KeepAll,
    /// None of it did.
    LoseAll,
}

/// How many pending writes and folder changes a crash or failed flush
/// kept, lost and kept in part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    pub kept: u32,
    pub lost: u32,
    pub partial: u32,
    /// Left pending (a failed folder flush).
    pub pending: u32,
}

/// What the simulator did that a process cannot see directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The power failed, in a call (`op`) or between calls.
    Crash {
        step: u64,
        op: Option<Op>,
        path: String,
        settled: Settled,
    },
    /// The process ended without a crash.
    Stopped {
        step: u64,
    },
    FlushFailed {
        step: u64,
        path: String,
        settled: Settled,
        cache_dropped: bool,
    },
    /// A write or allocation failed after `written` bytes.
    WriteFailed {
        step: u64,
        op: Op,
        path: String,
        written: u64,
    },
    /// The disk was full, after `written` bytes.
    NoSpace {
        step: u64,
        op: Op,
        path: String,
        written: u64,
    },
    ReadFailed {
        step: u64,
        path: String,
    },
    Damaged {
        step: u64,
        path: String,
        offset: u64,
        len: u64,
    },
    Misplaced {
        step: u64,
        path: String,
        offset: u64,
        landed: u64,
        len: u64,
    },
    Lost {
        step: u64,
        path: String,
        offset: u64,
        len: u64,
    },
}

/// The simulated machine. Clones share it.
#[derive(Clone)]
pub struct Sim {
    world: Arc<Mutex<World>>,
}

impl Sim {
    /// A machine with an empty store folder and no drawn faults.
    pub fn new(seed: u64) -> Self {
        Self::with_rates(seed, Rates::default())
    }

    /// A machine that draws faults at `rates`.
    pub fn with_rates(seed: u64, rates: Rates) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, Node::Folder(FolderNode::default()));
        let world = World {
            rates,
            faults: Rng::stream(seed, 1),
            app: Rng::stream(seed, 2),
            placed: Vec::new(),
            step: 0,
            epoch: 0,
            halted: false,
            lock: None,
            nodes,
            next_node: ROOT + 1,
            capacity: DEFAULT_CAPACITY,
            events: Vec::new(),
            wall: WALL_START,
            monotonic: Duration::ZERO,
        };
        Self {
            world: Arc::new(Mutex::new(world)),
        }
    }

    fn world(&self) -> MutexGuard<'_, World> {
        lock(&self.world)
    }

    /// The store folder, as the running process sees it.
    pub fn fs(&self) -> SimFs {
        SimFs {
            world: self.world.clone(),
        }
    }

    pub fn clock(&self) -> SimClock {
        SimClock {
            world: self.world.clone(),
        }
    }

    pub fn random(&self) -> SimRandom {
        SimRandom {
            world: self.world.clone(),
        }
    }

    /// Fire `fault` at the first call at or after `step` (counted from 1)
    /// that it applies to.
    pub fn place(&self, step: u64, fault: Fault) {
        self.world().placed.push((step, fault));
    }

    /// Draw faults at `rates` from now on (`Rates::default()`: none).
    pub fn set_rates(&self, rates: Rates) {
        self.world().rates = rates;
    }

    /// The steps taken so far.
    pub fn steps(&self) -> u64 {
        self.world().step
    }

    /// Whether the power has failed and no process runs.
    pub fn halted(&self) -> bool {
        self.world().halted
    }

    /// Cut the power now, between calls. Nothing happens when it is off.
    pub fn crash(&self, fates: Fates) {
        let mut world = self.world();
        if !world.halted {
            world.power_loss(None, "", fates);
        }
    }

    /// Start a new process: after a crash, on the disk as the crash left
    /// it; otherwise the running process stops first and the cache stays.
    /// Earlier handles and the lock are gone.
    pub fn restart(&self) {
        let mut world = self.world();
        if !world.halted {
            let step = world.step;
            world.events.push(Event::Stopped { step });
        }
        world.epoch += 1;
        world.halted = false;
        world.lock = None;
    }

    /// What happened so far.
    pub fn events(&self) -> Vec<Event> {
        self.world().events.clone()
    }

    /// Change `len` bytes from `offset` of what the device holds of the
    /// file `path` (as [`Fault::Damage`] does, at a chosen place). Bytes
    /// past the end are not changed.
    pub fn damage(&self, path: &str, offset: u64, len: u64) -> Result<()> {
        let mut world = self.world();
        let names = sys::names(path, Op::Read)?;
        let node = world
            .find_durable(&names)
            .filter(|&id| matches!(world.nodes.get(&id), Some(Node::File(_))))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, Op::Read, path))?;
        world.damage_at(node, path, offset, len);
        Ok(())
    }

    /// The disk's size in bytes.
    pub fn set_capacity(&self, bytes: u64) {
        self.world().capacity = bytes;
    }

    /// What the device holds: every folder (`None`) and file (its bytes)
    /// a durable entry reaches, by path, in order.
    pub fn image(&self) -> Vec<(String, Option<Vec<u8>>)> {
        let world = self.world();
        world
            .durable_paths()
            .into_iter()
            .map(|(path, id)| {
                let bytes = match world.nodes.get(&id) {
                    Some(Node::File(file)) => Some(file.durable.clone()),
                    _ => None,
                };
                (path, bytes)
            })
            .collect()
    }

    /// Let time pass on both clocks.
    pub fn advance(&self, by: Duration) {
        let mut world = self.world();
        world.wall = world.wall.saturating_add(by);
        world.monotonic = world.monotonic.saturating_add(by);
    }

    /// Set the wall clock, which may go back (the monotonic one does not).
    pub fn set_wall(&self, wall: Duration) {
        self.world().wall = wall;
    }
}

/// The simulated store folder (module docs). Not bound to a process: its
/// calls are the running process's.
#[derive(Clone)]
pub struct SimFs {
    world: Arc<Mutex<World>>,
}

/// The simulated clocks.
#[derive(Clone)]
pub struct SimClock {
    world: Arc<Mutex<World>>,
}

/// Random bytes from the seed.
#[derive(Clone)]
pub struct SimRandom {
    world: Arc<Mutex<World>>,
}

impl Clock for SimClock {
    fn wall(&self) -> Duration {
        lock(&self.world).wall
    }

    fn monotonic(&self) -> Duration {
        lock(&self.world).monotonic
    }
}

impl Random for SimRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<()> {
        lock(&self.world).app.fill(buf);
        Ok(())
    }
}

fn lock(world: &Mutex<World>) -> MutexGuard<'_, World> {
    world.lock().unwrap_or_else(|e| e.into_inner())
}

impl Fs for SimFs {
    fn lock(&self) -> Result<FolderLock> {
        let mut world = lock(&self.world);
        let epoch = world.epoch;
        let fault = world.begin(Op::Lock, epoch, "")?;
        if fault == Some(Fault::Crash) {
            return Err(world.crashed(Op::Lock, ""));
        }
        if world.lock == Some(epoch) {
            return Err(Error::new(ErrorKind::Locked, Op::Lock, ""));
        }
        world.lock = Some(epoch);
        Ok(FolderLock::new(SimLock {
            world: self.world.clone(),
            epoch,
        }))
    }

    fn create(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>> {
        let node = lock(&self.world).create(path, Op::Create)?;
        Ok(self.file(node, path, options))
    }

    fn open(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>> {
        let node = lock(&self.world).open(path)?;
        Ok(self.file(node, path, options))
    }

    fn create_folder(&self, path: &str) -> Result<()> {
        lock(&self.world).create(path, Op::CreateFolder).map(|_| ())
    }

    fn flush_folder(&self, path: &str) -> Result<()> {
        lock(&self.world).flush_folder(path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        lock(&self.world).rename(from, to)
    }

    fn remove(&self, path: &str) -> Result<()> {
        lock(&self.world).remove(path)
    }

    fn list(&self, path: &str) -> Result<Vec<String>> {
        lock(&self.world).list(path)
    }
}

impl SimFs {
    fn file(&self, node: u64, path: &str, options: FileOptions) -> Box<dyn File> {
        let epoch = lock(&self.world).epoch;
        Box::new(SimFile {
            world: self.world.clone(),
            node,
            epoch,
            sync: options.sync_writes,
            path: path.to_owned(),
        })
    }
}

/// The store folder's lock, held by process `epoch`.
struct SimLock {
    world: Arc<Mutex<World>>,
    epoch: u64,
}

impl Drop for SimLock {
    fn drop(&mut self) {
        let mut world = lock(&self.world);
        if world.epoch == self.epoch && world.lock == Some(self.epoch) {
            world.lock = None;
        }
    }
}

/// An open simulated file, of process `epoch`.
struct SimFile {
    world: Arc<Mutex<World>>,
    node: u64,
    epoch: u64,
    sync: bool,
    path: String,
}

impl File for SimFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut world = lock(&self.world);
        let fault = world.begin(Op::Read, self.epoch, &self.path)?;
        match fault {
            Some(Fault::Crash) => Err(world.crashed(Op::Read, &self.path)),
            Some(Fault::FailRead) => {
                let step = world.step;
                world.events.push(Event::ReadFailed {
                    step,
                    path: self.path.clone(),
                });
                Err(Error::new(ErrorKind::Read, Op::Read, &self.path))
            }
            _ => {
                let cache = &world.file(self.node).cache;
                let len = cache.len() as u64;
                if offset >= len {
                    return Ok(0);
                }
                let start = offset as usize;
                let n = buf.len().min(cache.len() - start);
                buf[..n].copy_from_slice(&cache[start..start + n]);
                Ok(n)
            }
        }
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut world = lock(&self.world);
        let fault = world.begin(Op::Write, self.epoch, &self.path)?;
        let result = world.write(self.node, &self.path, self.sync, offset, data, fault);
        if fault == Some(Fault::Crash) {
            return Err(world.crashed(Op::Write, &self.path));
        }
        result
    }

    fn flush(&self) -> Result<()> {
        let mut world = lock(&self.world);
        let fault = world.begin(Op::Flush, self.epoch, &self.path)?;
        match fault {
            Some(Fault::Crash) => Err(world.crashed(Op::Flush, &self.path)),
            Some(Fault::FailFlush) => {
                let World { nodes, faults, .. } = &mut *world;
                let Some(Node::File(file)) = nodes.get_mut(&self.node) else {
                    unreachable!("an open file's node exists while its process runs");
                };
                let settled = settle_file(file, faults, Fates::Drawn);
                let cache_dropped = faults.below(2) == 0;
                if cache_dropped {
                    file.cache = file.durable.clone();
                }
                let step = world.step;
                world.events.push(Event::FlushFailed {
                    step,
                    path: self.path.clone(),
                    settled,
                    cache_dropped,
                });
                Err(Error::new(ErrorKind::Flush, Op::Flush, &self.path))
            }
            _ => {
                let World { nodes, faults, .. } = &mut *world;
                if let Some(Node::File(file)) = nodes.get_mut(&self.node) {
                    settle_file(file, faults, Fates::KeepAll);
                }
                Ok(())
            }
        }
    }

    fn allocate(&self, size: u64) -> Result<()> {
        let mut world = lock(&self.world);
        let fault = world.begin(Op::Allocate, self.epoch, &self.path)?;
        let result = world.allocate(self.node, &self.path, self.sync, size, fault);
        if fault == Some(Fault::Crash) {
            return Err(world.crashed(Op::Allocate, &self.path));
        }
        result
    }

    fn size(&self) -> Result<u64> {
        let mut world = lock(&self.world);
        let fault = world.begin(Op::Size, self.epoch, &self.path)?;
        if fault == Some(Fault::Crash) {
            return Err(world.crashed(Op::Size, &self.path));
        }
        Ok(world.file(self.node).cache.len() as u64)
    }
}

/// The machine's state.
struct World {
    rates: Rates,
    /// Faults and how they settle.
    faults: Rng,
    /// What [`SimRandom`] gives the process.
    app: Rng,
    /// Faults placed by step, in the order placed.
    placed: Vec<(u64, Fault)>,
    step: u64,
    /// The running process; handles of earlier ones fail.
    epoch: u64,
    /// The power has failed and no process runs.
    halted: bool,
    /// The process that holds the store folder's lock.
    lock: Option<u64>,
    nodes: BTreeMap<u64, Node>,
    next_node: u64,
    capacity: u64,
    events: Vec<Event>,
    wall: Duration,
    monotonic: Duration,
}

enum Node {
    File(FileNode),
    Folder(FolderNode),
}

#[derive(Default)]
struct FileNode {
    durable: Vec<u8>,
    cache: Vec<u8>,
    pending: Vec<FileOp>,
}

#[derive(Default)]
struct FolderNode {
    durable: BTreeMap<String, u64>,
    cache: BTreeMap<String, u64>,
    pending: Vec<FolderOp>,
}

/// A write the device has not been given yet.
enum FileOp {
    /// The file grows to this size (with zeros).
    Extend(u64),
    /// Bytes at an offset. A write past the end follows the `Extend` that
    /// makes room for it; without that, only its part within the file
    /// lands.
    Write(u64, Vec<u8>),
}

/// A folder change the device has not been given yet. Each names the node
/// it concerns, and lands only where the entry still is that node.
enum FolderOp {
    Link(String, u64),
    Unlink(String, u64),
    Rename { from: String, to: String, node: u64 },
}

impl World {
    /// Count a step of process `epoch` and choose its fault. Damage happens
    /// here; any other fault is the caller's to carry out.
    fn begin(&mut self, op: Op, epoch: u64, path: &str) -> Result<Option<Fault>> {
        if self.halted || epoch != self.epoch {
            return Err(Error::new(ErrorKind::Halted, op, path));
        }
        self.step += 1;
        if self.take(Fault::Damage) {
            self.damage_drawn();
        }
        for fault in [
            Fault::Crash,
            Fault::FailFlush,
            Fault::FailWrite,
            Fault::NoSpace,
            Fault::FailRead,
            Fault::Misplace,
            Fault::LoseWrite,
        ] {
            if fault.applies(op) && self.take(fault) {
                return Ok(Some(fault));
            }
        }
        Ok(None)
    }

    fn take(&mut self, fault: Fault) -> bool {
        let step = self.step;
        if let Some(i) = self
            .placed
            .iter()
            .position(|&(at, placed)| at <= step && placed == fault)
        {
            self.placed.remove(i);
            return true;
        }
        self.faults.one_in(self.rates.of(fault))
    }

    /// A crash during `op`: the power fails.
    fn crashed(&mut self, op: Op, path: &str) -> Error {
        self.power_loss(Some(op), path, Fates::Drawn);
        Error::new(ErrorKind::Halted, op, path)
    }

    fn power_loss(&mut self, op: Option<Op>, path: &str, fates: Fates) {
        let mut settled = Settled::default();
        let World { nodes, faults, .. } = self;
        for node in nodes.values_mut() {
            let s = match node {
                Node::File(file) => {
                    let s = settle_file(file, faults, fates);
                    file.cache = file.durable.clone();
                    s
                }
                Node::Folder(folder) => {
                    let s = settle_folder(folder, faults, fates);
                    folder.cache = folder.durable.clone();
                    s
                }
            };
            settled.kept += s.kept;
            settled.lost += s.lost;
            settled.partial += s.partial;
        }
        // What no durable entry reaches is gone.
        let reachable: BTreeSet<u64> = self
            .durable_paths()
            .into_iter()
            .map(|(_, id)| id)
            .chain([ROOT])
            .collect();
        self.nodes.retain(|id, _| reachable.contains(id));
        self.halted = true;
        self.lock = None;
        let step = self.step;
        self.events.push(Event::Crash {
            step,
            op,
            path: path.to_owned(),
            settled,
        });
    }

    fn file(&self, node: u64) -> &FileNode {
        match self.nodes.get(&node) {
            Some(Node::File(file)) => file,
            _ => unreachable!("an open file's node exists while its process runs"),
        }
    }

    fn file_mut(&mut self, node: u64) -> &mut FileNode {
        match self.nodes.get_mut(&node) {
            Some(Node::File(file)) => file,
            _ => unreachable!("an open file's node exists while its process runs"),
        }
    }

    fn folder_mut(&mut self, node: u64) -> &mut FolderNode {
        match self.nodes.get_mut(&node) {
            Some(Node::Folder(folder)) => folder,
            _ => unreachable!("a folder node found by its path"),
        }
    }

    /// The node at `names`, as the process sees the folders.
    fn find(&self, names: &[&str]) -> Option<u64> {
        let mut id = ROOT;
        for name in names {
            match self.nodes.get(&id) {
                Some(Node::Folder(folder)) => id = *folder.cache.get(*name)?,
                _ => return None,
            }
        }
        Some(id)
    }

    /// The node at `names` on the device.
    fn find_durable(&self, names: &[&str]) -> Option<u64> {
        let mut id = ROOT;
        for name in names {
            match self.nodes.get(&id) {
                Some(Node::Folder(folder)) => id = *folder.durable.get(*name)?,
                _ => return None,
            }
        }
        Some(id)
    }

    /// The folder at `path`, as the process sees it.
    fn find_folder(&self, path: &str, op: Op) -> Result<u64> {
        let names = sys::names(path, op)?;
        match self.find(&names) {
            Some(id) if matches!(self.nodes.get(&id), Some(Node::Folder(_))) => Ok(id),
            Some(_) => Err(Error::new(ErrorKind::Other, op, path)),
            None => Err(Error::new(ErrorKind::NotFound, op, path)),
        }
    }

    /// Every path a durable entry reaches, depth first in name order.
    fn durable_paths(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), ROOT)];
        while let Some((path, id)) = stack.pop() {
            if let Some(Node::Folder(folder)) = self.nodes.get(&id) {
                for (name, &child) in folder.durable.iter().rev() {
                    let child_path = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}/{name}")
                    };
                    stack.push((child_path, child));
                }
            }
            if id != ROOT {
                out.push((path, id));
            }
        }
        out
    }

    /// The bytes the files take: what the cache or the device holds,
    /// whichever is more.
    fn used(&self) -> u64 {
        self.nodes
            .values()
            .map(|node| match node {
                Node::File(file) => file.cache.len().max(intended_len(file) as usize) as u64,
                Node::Folder(_) => 0,
            })
            .sum()
    }

    fn create(&mut self, path: &str, op: Op) -> Result<u64> {
        let (folder_path, name) = sys::split(path, op)?;
        let epoch = self.epoch;
        let fault = self.begin(op, epoch, path)?;
        let made = (|| {
            let parent = self.find_folder(folder_path, op)?;
            if self.folder_mut(parent).cache.contains_key(name) {
                return Err(Error::new(ErrorKind::AlreadyExists, op, path));
            }
            if fault == Some(Fault::NoSpace) {
                let step = self.step;
                self.events.push(Event::NoSpace {
                    step,
                    op,
                    path: path.to_owned(),
                    written: 0,
                });
                return Err(Error::new(ErrorKind::NoSpace, op, path));
            }
            let id = self.next_node;
            self.next_node += 1;
            let node = match op {
                Op::CreateFolder => Node::Folder(FolderNode::default()),
                _ => Node::File(FileNode::default()),
            };
            self.nodes.insert(id, node);
            let folder = self.folder_mut(parent);
            folder.cache.insert(name.to_owned(), id);
            folder.pending.push(FolderOp::Link(name.to_owned(), id));
            Ok(id)
        })();
        if fault == Some(Fault::Crash) {
            return Err(self.crashed(op, path));
        }
        made
    }

    fn open(&mut self, path: &str) -> Result<u64> {
        let names = sys::names(path, Op::Open)?;
        if names.is_empty() {
            return Err(Error::new(ErrorKind::InvalidPath, Op::Open, path));
        }
        let epoch = self.epoch;
        if self.begin(Op::Open, epoch, path)? == Some(Fault::Crash) {
            return Err(self.crashed(Op::Open, path));
        }
        match self.find(&names) {
            Some(id) if matches!(self.nodes.get(&id), Some(Node::File(_))) => Ok(id),
            Some(_) => Err(Error::new(ErrorKind::Other, Op::Open, path)),
            None => Err(Error::new(ErrorKind::NotFound, Op::Open, path)),
        }
    }

    fn flush_folder(&mut self, path: &str) -> Result<()> {
        sys::names(path, Op::FlushFolder)?;
        let epoch = self.epoch;
        let fault = self.begin(Op::FlushFolder, epoch, path)?;
        if fault == Some(Fault::Crash) {
            return Err(self.crashed(Op::FlushFolder, path));
        }
        let id = self.find_folder(path, Op::FlushFolder)?;
        let World { nodes, faults, .. } = self;
        let Some(Node::Folder(folder)) = nodes.get_mut(&id) else {
            unreachable!("found as a folder");
        };
        if fault == Some(Fault::FailFlush) {
            // A prefix reaches the device; the rest stays pending.
            let reached = faults.below(folder.pending.len() as u64 + 1) as usize;
            let rest = folder.pending.split_off(reached);
            let mut settled = settle_folder(folder, faults, Fates::KeepAll);
            settled.pending = rest.len() as u32;
            folder.pending = rest;
            let step = self.step;
            self.events.push(Event::FlushFailed {
                step,
                path: path.to_owned(),
                settled,
                cache_dropped: false,
            });
            return Err(Error::new(ErrorKind::Flush, Op::FlushFolder, path));
        }
        settle_folder(folder, faults, Fates::KeepAll);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let (from_folder, from_name) = sys::split(from, Op::Rename)?;
        let (to_folder, to_name) = sys::split(to, Op::Rename)?;
        if from_folder != to_folder {
            return Err(Error::new(ErrorKind::InvalidPath, Op::Rename, to));
        }
        let epoch = self.epoch;
        let fault = self.begin(Op::Rename, epoch, from)?;
        let renamed = (|| {
            let parent = self.find_folder(from_folder, Op::Rename)?;
            let folder = match self.nodes.get(&parent) {
                Some(Node::Folder(folder)) => folder,
                _ => unreachable!("found as a folder"),
            };
            let node = *folder
                .cache
                .get(from_name)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, Op::Rename, from))?;
            let is_folder = |id: Option<&u64>| {
                id.is_some_and(|id| matches!(self.nodes.get(id), Some(Node::Folder(_))))
            };
            if is_folder(Some(&node)) || is_folder(folder.cache.get(to_name)) {
                return Err(Error::new(ErrorKind::Other, Op::Rename, from));
            }
            if from_name == to_name {
                return Ok(());
            }
            let folder = self.folder_mut(parent);
            folder.cache.remove(from_name);
            folder.cache.insert(to_name.to_owned(), node);
            folder.pending.push(FolderOp::Rename {
                from: from_name.to_owned(),
                to: to_name.to_owned(),
                node,
            });
            Ok(())
        })();
        if fault == Some(Fault::Crash) {
            return Err(self.crashed(Op::Rename, from));
        }
        renamed
    }

    fn remove(&mut self, path: &str) -> Result<()> {
        let (folder_path, name) = sys::split(path, Op::Remove)?;
        let epoch = self.epoch;
        let fault = self.begin(Op::Remove, epoch, path)?;
        let removed = (|| {
            let parent = self.find_folder(folder_path, Op::Remove)?;
            let folder = match self.nodes.get(&parent) {
                Some(Node::Folder(folder)) => folder,
                _ => unreachable!("found as a folder"),
            };
            let node = *folder
                .cache
                .get(name)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, Op::Remove, path))?;
            if let Some(Node::Folder(target)) = self.nodes.get(&node) {
                if !target.cache.is_empty() {
                    return Err(Error::new(ErrorKind::Other, Op::Remove, path));
                }
            }
            let folder = self.folder_mut(parent);
            folder.cache.remove(name);
            folder.pending.push(FolderOp::Unlink(name.to_owned(), node));
            Ok(())
        })();
        if fault == Some(Fault::Crash) {
            return Err(self.crashed(Op::Remove, path));
        }
        removed
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>> {
        sys::names(path, Op::List)?;
        let epoch = self.epoch;
        if self.begin(Op::List, epoch, path)? == Some(Fault::Crash) {
            return Err(self.crashed(Op::List, path));
        }
        let id = self.find_folder(path, Op::List)?;
        match self.nodes.get(&id) {
            Some(Node::Folder(folder)) => Ok(folder.cache.keys().cloned().collect()),
            _ => unreachable!("found as a folder"),
        }
    }

    fn write(
        &mut self,
        node: u64,
        path: &str,
        sync: bool,
        offset: u64,
        data: &[u8],
        fault: Option<Fault>,
    ) -> Result<()> {
        let len = data.len() as u64;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| Error::new(ErrorKind::Write, Op::Write, path))?;
        let growth = end.saturating_sub(self.file(node).cache.len() as u64);
        if growth > 0 && self.used().saturating_add(growth) > self.capacity {
            let step = self.step;
            self.events.push(Event::NoSpace {
                step,
                op: Op::Write,
                path: path.to_owned(),
                written: 0,
            });
            return Err(Error::new(ErrorKind::NoSpace, Op::Write, path));
        }
        let step = self.step;
        match fault {
            Some(Fault::FailWrite) => {
                let written = self.faults.below(len + 1);
                self.put(node, offset, &data[..written as usize], false, Some(offset));
                self.events.push(Event::WriteFailed {
                    step,
                    op: Op::Write,
                    path: path.to_owned(),
                    written,
                });
                Err(Error::new(ErrorKind::Write, Op::Write, path))
            }
            Some(Fault::NoSpace) => {
                let written = self.faults.below(len);
                self.put(node, offset, &data[..written as usize], false, Some(offset));
                self.events.push(Event::NoSpace {
                    step,
                    op: Op::Write,
                    path: path.to_owned(),
                    written,
                });
                Err(Error::new(ErrorKind::NoSpace, Op::Write, path))
            }
            Some(Fault::Misplace) => {
                let landed = self.landing(node, offset, len);
                self.put(node, offset, data, sync, Some(landed));
                self.events.push(Event::Misplaced {
                    step,
                    path: path.to_owned(),
                    offset,
                    landed,
                    len,
                });
                Ok(())
            }
            Some(Fault::LoseWrite) => {
                self.put(node, offset, data, sync, None);
                self.events.push(Event::Lost {
                    step,
                    path: path.to_owned(),
                    offset,
                    len,
                });
                Ok(())
            }
            // A write cut by a crash has not returned: it is pending,
            // whatever the file's options.
            Some(Fault::Crash) => {
                self.put(node, offset, data, false, Some(offset));
                Ok(())
            }
            _ => {
                self.put(node, offset, data, sync, Some(offset));
                Ok(())
            }
        }
    }

    /// Write `data` at `offset` in the cache, and for the device at
    /// `device` (nowhere when `None`): on it now when `durable`, otherwise
    /// as pending.
    fn put(&mut self, node: u64, offset: u64, data: &[u8], durable: bool, device: Option<u64>) {
        if data.is_empty() {
            return;
        }
        let file = self.file_mut(node);
        put(&mut file.cache, offset, data);
        let Some(at) = device else {
            return;
        };
        if durable {
            trim(&mut file.pending, at, data.len() as u64);
            put(&mut file.durable, at, data);
        } else {
            let end = at + data.len() as u64;
            if end > intended_len(file) {
                file.pending.push(FileOp::Extend(end));
            }
            file.pending.push(FileOp::Write(at, data.to_vec()));
        }
    }

    /// Where a misplaced write of `len` bytes at `offset` lands: a whole
    /// number of sectors away, within the file where it can be.
    fn landing(&mut self, node: u64, offset: u64, len: u64) -> u64 {
        let span = (self.file(node).cache.len() as u64).max(offset + len);
        let first = offset % SECTOR;
        let count = (span - len - first) / SECTOR + 1;
        if count <= 1 {
            return offset + SECTOR * (1 + len / SECTOR);
        }
        let own = (offset - first) / SECTOR;
        let mut i = self.faults.below(count - 1);
        if i >= own {
            i += 1;
        }
        first + i * SECTOR
    }

    fn allocate(
        &mut self,
        node: u64,
        path: &str,
        sync: bool,
        size: u64,
        fault: Option<Fault>,
    ) -> Result<()> {
        let len = self.file(node).cache.len() as u64;
        if size <= len {
            return Ok(());
        }
        let growth = size - len;
        let step = self.step;
        if self.used().saturating_add(growth) > self.capacity {
            self.events.push(Event::NoSpace {
                step,
                op: Op::Allocate,
                path: path.to_owned(),
                written: 0,
            });
            return Err(Error::new(ErrorKind::NoSpace, Op::Allocate, path));
        }
        match fault {
            Some(kind @ (Fault::FailWrite | Fault::NoSpace)) => {
                let written = self.faults.below(growth);
                self.extend(node, len + written, false);
                let (event, error) = if kind == Fault::NoSpace {
                    let event = Event::NoSpace {
                        step,
                        op: Op::Allocate,
                        path: path.to_owned(),
                        written,
                    };
                    (event, ErrorKind::NoSpace)
                } else {
                    let event = Event::WriteFailed {
                        step,
                        op: Op::Allocate,
                        path: path.to_owned(),
                        written,
                    };
                    (event, ErrorKind::Write)
                };
                self.events.push(event);
                Err(Error::new(error, Op::Allocate, path))
            }
            Some(Fault::Crash) => {
                self.extend(node, size, false);
                Ok(())
            }
            _ => {
                self.extend(node, size, sync);
                Ok(())
            }
        }
    }

    fn extend(&mut self, node: u64, size: u64, durable: bool) {
        let file = self.file_mut(node);
        if (file.cache.len() as u64) < size {
            file.cache.resize(size as usize, 0);
        }
        if durable {
            if (file.durable.len() as u64) < size {
                file.durable.resize(size as usize, 0);
            }
        } else if size > intended_len(file) {
            file.pending.push(FileOp::Extend(size));
        }
    }

    /// A drawn [`Fault::Damage`]: some bytes of a file on the device.
    fn damage_drawn(&mut self) {
        let files: Vec<(String, u64)> = self
            .durable_paths()
            .into_iter()
            .filter(|(_, id)| {
                matches!(self.nodes.get(id), Some(Node::File(file)) if !file.durable.is_empty())
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let (path, id) = files[self.faults.below(files.len() as u64) as usize].clone();
        let size = self.file(id).durable.len() as u64;
        let offset = self.faults.below(size);
        let len = 1 + self.faults.below(MAX_DAMAGE.min(size - offset));
        self.damage_at(id, &path, offset, len);
    }

    fn damage_at(&mut self, id: u64, path: &str, offset: u64, len: u64) {
        let World { nodes, faults, .. } = self;
        let Some(Node::File(file)) = nodes.get_mut(&id) else {
            return;
        };
        let size = file.durable.len() as u64;
        let end = offset.saturating_add(len).min(size);
        // The cache holds nothing of its own for this file: as if it had
        // been evicted, reads see the device.
        let evicted = file.pending.is_empty() && file.cache == file.durable;
        for i in offset..end {
            let mask = 1 + faults.below(255) as u8;
            file.durable[i as usize] ^= mask;
            if evicted {
                file.cache[i as usize] ^= mask;
            }
        }
        if offset < end {
            let step = self.step;
            self.events.push(Event::Damaged {
                step,
                path: path.to_owned(),
                offset,
                len: end - offset,
            });
        }
    }
}

/// The size `file` has on the device once its pending writes land.
fn intended_len(file: &FileNode) -> u64 {
    file.pending
        .iter()
        .filter_map(|op| match op {
            FileOp::Extend(size) => Some(*size),
            FileOp::Write(..) => None,
        })
        .fold(file.durable.len() as u64, u64::max)
}

/// Write `data` at `offset`, extending `bytes` with zeros as needed.
fn put(bytes: &mut Vec<u8>, offset: u64, data: &[u8]) {
    let start = offset as usize;
    let end = start + data.len();
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[start..end].copy_from_slice(data);
}

/// Write the part of `data` at `offset` that lies within `bytes`.
fn put_within(bytes: &mut [u8], offset: u64, data: &[u8]) {
    let size = bytes.len() as u64;
    if offset >= size {
        return;
    }
    let n = ((size - offset) as usize).min(data.len());
    let start = offset as usize;
    bytes[start..start + n].copy_from_slice(&data[..n]);
}

/// Cut `len` bytes at `at` out of every pending write: a synchronous write
/// of them has reached the device after those.
fn trim(pending: &mut Vec<FileOp>, at: u64, len: u64) {
    let end = at + len;
    let mut out = Vec::with_capacity(pending.len());
    for op in pending.drain(..) {
        match op {
            FileOp::Write(offset, data) => {
                let op_end = offset + data.len() as u64;
                if op_end <= at || offset >= end {
                    out.push(FileOp::Write(offset, data));
                    continue;
                }
                if offset < at {
                    out.push(FileOp::Write(
                        offset,
                        data[..(at - offset) as usize].to_vec(),
                    ));
                }
                if op_end > end {
                    out.push(FileOp::Write(end, data[(end - offset) as usize..].to_vec()));
                }
            }
            extend => out.push(extend),
        }
    }
    *pending = out;
}

/// How one pending change settles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    Keep,
    Lose,
    Part,
}

fn fate(rng: &mut Rng, fates: Fates, can_part: bool) -> Fate {
    match fates {
        Fates::KeepAll => Fate::Keep,
        Fates::LoseAll => Fate::Lose,
        Fates::Drawn => match rng.below(if can_part { 3 } else { 2 }) {
            0 => Fate::Keep,
            1 => Fate::Lose,
            _ => Fate::Part,
        },
    }
}

/// Give the device `file`'s pending writes, each as `fates` says.
fn settle_file(file: &mut FileNode, rng: &mut Rng, fates: Fates) -> Settled {
    let mut settled = Settled::default();
    for op in std::mem::take(&mut file.pending) {
        let can_part = matches!(&op, FileOp::Write(_, data) if data.len() > 1);
        let fate = fate(rng, fates, can_part);
        match fate {
            Fate::Keep => settled.kept += 1,
            Fate::Lose => settled.lost += 1,
            Fate::Part => settled.partial += 1,
        }
        match (op, fate) {
            (_, Fate::Lose) => {}
            (FileOp::Extend(size), _) => {
                if file.durable.len() < size as usize {
                    file.durable.resize(size as usize, 0);
                }
            }
            (FileOp::Write(offset, data), Fate::Keep) => {
                put_within(&mut file.durable, offset, &data)
            }
            (FileOp::Write(offset, data), Fate::Part) => {
                if rng.below(2) == 0 {
                    // A prefix, cut at any byte.
                    let cut = 1 + rng.below(data.len() as u64 - 1) as usize;
                    put_within(&mut file.durable, offset, &data[..cut]);
                } else {
                    // Any of its sectors.
                    let mut start = 0usize;
                    while start < data.len() {
                        let at = offset + start as u64;
                        let next = ((at / SECTOR + 1) * SECTOR - offset) as usize;
                        let stop = next.min(data.len());
                        if rng.below(2) == 0 {
                            put_within(&mut file.durable, at, &data[start..stop]);
                        }
                        start = stop;
                    }
                }
            }
        }
    }
    settled
}

/// Give the device `folder`'s pending changes, each as `fates` says.
fn settle_folder(folder: &mut FolderNode, rng: &mut Rng, fates: Fates) -> Settled {
    let mut settled = Settled::default();
    for op in std::mem::take(&mut folder.pending) {
        if fate(rng, fates, false) == Fate::Lose {
            settled.lost += 1;
            continue;
        }
        settled.kept += 1;
        let entries = &mut folder.durable;
        match op {
            FolderOp::Link(name, node) => {
                entries.insert(name, node);
            }
            FolderOp::Unlink(name, node) => {
                if entries.get(&name) == Some(&node) {
                    entries.remove(&name);
                }
            }
            FolderOp::Rename { from, to, node } => {
                if entries.get(&from) == Some(&node) {
                    entries.remove(&from);
                    entries.insert(to, node);
                }
            }
        }
    }
    settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::{TempFolder, LOCK_FILE};

    /// What a short sequence of calls returns, in words.
    fn contract(fs: &dyn Fs) -> Vec<String> {
        let mut out = Vec::new();
        let kind = |r: Result<()>| format!("{:?}", r.map_err(|e| e.kind()));
        let lock = fs.lock().unwrap();
        out.push(kind(fs.lock().map(drop)));
        fs.create_folder("log").unwrap();
        out.push(kind(fs.create_folder("log")));
        let a = fs.create("log/a", FileOptions::PLAIN).unwrap();
        out.push(kind(fs.create("log/a", FileOptions::PLAIN).map(drop)));
        a.write_at(10, b"hello").unwrap();
        let mut buf = [9u8; 20];
        let n = a.read_at(0, &mut buf).unwrap();
        out.push(format!("{} {n} {buf:?}", a.size().unwrap()));
        a.allocate(100).unwrap();
        a.allocate(50).unwrap();
        out.push(format!("{}", a.size().unwrap()));
        a.flush().unwrap();
        let b = fs.create("log/b", FileOptions::SYNC).unwrap();
        b.write_at(0, b"x").unwrap();
        fs.flush_folder("log").unwrap();
        out.push(format!("{:?}", fs.list("log").unwrap()));
        fs.rename("log/b", "log/a").unwrap();
        out.push(format!("{:?}", fs.list("log").unwrap()));
        let now = fs.open("log/a", FileOptions::PLAIN).unwrap();
        out.push(format!("{}", now.size().unwrap()));
        // The replaced file's handle still reads it.
        out.push(format!("{}", a.size().unwrap()));
        out.push(kind(fs.rename("log/a", "a")));
        out.push(kind(fs.open("log/missing", FileOptions::PLAIN).map(drop)));
        out.push(kind(fs.remove("log")));
        fs.remove("log/a").unwrap();
        fs.remove("log").unwrap();
        let mut root = fs.list("").unwrap();
        root.retain(|name| name != LOCK_FILE);
        out.push(format!("{root:?}"));
        for path in ["../x", "a//b", "/a", "a/./b", ""] {
            out.push(kind(fs.create(path, FileOptions::PLAIN).map(drop)));
        }
        drop(lock);
        out.push(kind(fs.lock().map(drop)));
        out
    }

    #[test]
    fn the_simulated_file_system_answers_as_the_real_one() {
        let folder = TempFolder::new("contract");
        let real = contract(&folder.fs());
        assert_eq!(contract(&Sim::new(1).fs()), real);
        assert!(real[0].contains("Locked"), "{real:?}");
    }

    #[test]
    fn a_crash_keeps_what_was_flushed_and_any_part_of_the_rest() {
        let mut seen = BTreeSet::new();
        for seed in 0..64 {
            let sim = Sim::new(seed);
            let fs = sim.fs();
            fs.create_folder("log").unwrap();
            fs.flush_folder("").unwrap();
            let a = fs.create("log/a", FileOptions::PLAIN).unwrap();
            fs.flush_folder("log").unwrap();
            a.write_at(0, &[1; 2048]).unwrap();
            a.flush().unwrap();
            // Not flushed: an overwrite, a write past the end, a new file.
            a.write_at(0, &[2; 2048]).unwrap();
            a.write_at(2048, &[3; 100]).unwrap();
            fs.create("log/b", FileOptions::PLAIN).unwrap();
            sim.crash(Fates::Drawn);
            assert_eq!(
                fs.list("log").unwrap_err().kind(),
                ErrorKind::Halted,
                "nothing runs until a restart"
            );
            sim.restart();
            let a = fs.open("log/a", FileOptions::PLAIN).unwrap();
            let size = a.size().unwrap();
            assert!(size == 2048 || size == 2148, "{size}");
            let mut bytes = vec![0; size as usize];
            a.read_at(0, &mut bytes).unwrap();
            // Every byte is the flushed one or one written over it.
            assert!(bytes[..2048].iter().all(|&b| b == 1 || b == 2));
            assert!(bytes[2048..].iter().all(|&b| b == 0 || b == 3));
            let mixed = bytes[..2048].contains(&1) && bytes[..2048].contains(&2);
            seen.insert(if mixed {
                "part"
            } else if bytes[0] == 1 {
                "old"
            } else {
                "new"
            });
            let names = fs.list("log").unwrap();
            seen.insert(if names.len() == 2 { "b kept" } else { "b lost" });
        }
        assert_eq!(seen.len(), 5, "{seen:?}");

        // The extremes.
        for (fates, byte, names) in [(Fates::KeepAll, 2, 2), (Fates::LoseAll, 1, 1)] {
            let sim = Sim::new(0);
            let fs = sim.fs();
            let a = fs.create("a", FileOptions::PLAIN).unwrap();
            fs.flush_folder("").unwrap();
            a.write_at(0, &[1; 600]).unwrap();
            a.flush().unwrap();
            a.write_at(0, &[2; 600]).unwrap();
            fs.create("b", FileOptions::PLAIN).unwrap();
            sim.crash(fates);
            sim.restart();
            let mut bytes = [0; 600];
            fs.open("a", FileOptions::PLAIN)
                .unwrap()
                .read_at(0, &mut bytes)
                .unwrap();
            assert!(bytes.iter().all(|&b| b == byte), "{fates:?}");
            assert_eq!(fs.list("").unwrap().len(), names, "{fates:?}");
        }
    }

    #[test]
    fn a_failed_flush_is_not_vouched_for_by_a_later_one() {
        let mut lost = 0;
        for seed in 0..32 {
            let sim = Sim::new(seed);
            let fs = sim.fs();
            let a = fs.create("a", FileOptions::PLAIN).unwrap();
            fs.flush_folder("").unwrap();
            a.write_at(0, b"first").unwrap();
            sim.place(sim.steps() + 1, Fault::FailFlush);
            assert_eq!(a.flush().unwrap_err().kind(), ErrorKind::Flush);
            a.write_at(100, b"second").unwrap();
            a.flush().unwrap();
            sim.crash(Fates::LoseAll);
            sim.restart();
            let mut bytes = [0; 106];
            let a = fs.open("a", FileOptions::PLAIN).unwrap();
            assert_eq!(a.read_at(0, &mut bytes).unwrap(), 106);
            assert_eq!(&bytes[100..], b"second");
            if &bytes[..5] != b"first" {
                lost += 1;
            }
        }
        assert!(lost > 0, "a failed flush always kept its data");
    }
}
