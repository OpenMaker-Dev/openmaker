//! The log under seeded schedules of appends, batch flushes, cuts,
//! retirements, stops and power failures, with storage faults drawn at every
//! step or a crash placed at each one (`docs/server-storage.md` section 12),
//! and the same scenario on the real file system.
//!
//! The **model** knows the log only through the calls the scenario makes
//! and what they return: which entries an append returned, which a batch
//! flush reported durable, which a cut removed, and which calls failed. It
//! does not share the log's code: an entry's hash is computed here from the
//! format's description (`docs/server-storage-format.md` 3.4). After every
//! open it checks that:
//!
//! - every entry reported durable is read back, whole, in order, with the
//!   same bytes, and the durable position did not go back;
//! - what follows is a run of entries that were appended and not reported
//!   durable (or cut by a call that failed), each whole and in order, and
//!   nothing else;
//! - the hash chain of what is read is unbroken from the open's start;
//! - the marker in `state` is the durable position the open returned;
//! - the log reports damage only where the device damaged a segment, says
//!   which, and changes nothing when it does.
//!
//! A cut that removed what the marker covered before lowering it, a marker
//! recorded before its batch was written, or a segment removed while
//! `state` listed it, leaves a log that a later open finds damaged with no
//! damage done by the device, which fails the last check.
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds of each schedule, for
//! long runs; `OM_STORE_SEED=<s>` runs seed `s` alone, to replay a failure
//! (a failing run prints its seed, the simulator's events and its steps).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ring::digest;

use super::*;
use crate::os::{OsRandom, TempFolder};
use crate::sim::{self, Fates, Fault, Rates, Rng, Sim, SimRandom};
use crate::state::{State, StateError};
use crate::sys::Random;

/// Small segments, so that the schedules cross many of them.
const CONFIG: Config = Config {
    segment_size: 1536,
    max_payload: 600,
};

/// The seeds of a schedule: `default` of them, scaled (module docs).
fn seeds(default: u64) -> Vec<u64> {
    let read = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(seed) = read("OM_STORE_SEED") {
        return vec![seed];
    }
    (0..default.saturating_mul(read("OM_STORE_SCALE").unwrap_or(1))).collect()
}

/// The hash `entry` must have after `prev`, by the format's description:
/// SHA-256 over the previous hash, then the index, term and file as 8
/// bytes, the kind as 2, two zero bytes, the payload's length as 4, and the
/// payload.
fn expected_hash(prev: &[u8; 32], entry: &Entry) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(prev);
    ctx.update(&entry.index.to_le_bytes());
    ctx.update(&entry.term.to_le_bytes());
    ctx.update(&entry.file.to_le_bytes());
    ctx.update(&entry.kind.to_le_bytes());
    ctx.update(&[0, 0]);
    ctx.update(&(entry.payload.len() as u32).to_le_bytes());
    ctx.update(&entry.payload);
    ctx.finish().as_ref().try_into().unwrap()
}

/// A store folder that counts the calls that change the log's files, so
/// that a test can show an open that found damage changed nothing.
struct Watched {
    inner: Arc<dyn Fs>,
    changes: Arc<AtomicU64>,
    /// The largest read any of its files was asked for.
    largest_read: Arc<AtomicU64>,
    /// Places the device cannot read: a read of the file that covers the
    /// offset fails, every time.
    bad: Arc<Mutex<Vec<(String, u64)>>>,
    /// How many reads failed on one of those places.
    bad_reads: Arc<AtomicU64>,
    /// The next write to a log file reaches the system's cache only, whole,
    /// and fails: a write the device did not take, whose bytes a process
    /// that goes on without a power loss still reads.
    cache_only: Arc<std::sync::atomic::AtomicBool>,
    /// The next flush or write of a log file fails, as the simulator fails
    /// one (simulated machines only).
    fail_next: Arc<std::sync::atomic::AtomicBool>,
    rereads: Arc<Mutex<Rereads>>,
    sim: Option<Sim>,
}

/// Second reads that differ: when on, a read of bytes already read since
/// the last [`Rereads::begin`] comes back with one of them changed, the
/// byte and the change drawn from the seed. An operation of the log reads
/// each place once and acts only on what it checked (format section 3.11),
/// so it never meets one; code that reads a place again and uses what it
/// read then would use bytes it did not check.
#[derive(Default)]
struct Rereads {
    draw: Option<Rng>,
    /// The ranges read since the last `begin`, by path.
    read: BTreeMap<String, Vec<(u64, u64)>>,
    /// How many reads came back changed.
    changed: u64,
}

impl Rereads {
    /// A new operation begins: what was read before it may be read again.
    fn begin(&mut self) {
        self.read.clear();
    }

    /// `buf`, just read at `offset` of `path`: one byte changed when it
    /// overlaps bytes read before.
    fn after_read(&mut self, path: &str, offset: u64, buf: &mut [u8]) {
        let Some(draw) = self.draw.as_mut() else {
            return;
        };
        if buf.is_empty() {
            return;
        }
        let range = (offset, offset + buf.len() as u64);
        let ranges = self.read.entry(path.to_owned()).or_default();
        let overlap = ranges.iter().find_map(|&(a, b)| {
            let (lo, hi) = (a.max(range.0), b.min(range.1));
            (lo < hi).then_some((lo, hi))
        });
        if let Some((lo, hi)) = overlap {
            let at = lo + draw.below(hi - lo);
            buf[(at - offset) as usize] ^= 1 + draw.below(255) as u8;
            self.changed += 1;
        }
        ranges.push(range);
    }
}

struct WatchedFile {
    inner: Box<dyn File>,
    inner_fs: Arc<dyn Fs>,
    path: String,
    counted: bool,
    changes: Arc<AtomicU64>,
    largest_read: Arc<AtomicU64>,
    bad: Arc<Mutex<Vec<(String, u64)>>>,
    bad_reads: Arc<AtomicU64>,
    cache_only: Arc<std::sync::atomic::AtomicBool>,
    fail_next: Arc<std::sync::atomic::AtomicBool>,
    rereads: Arc<Mutex<Rereads>>,
    sim: Option<Sim>,
}

impl WatchedFile {
    /// The simulator fails this call, if a failure was armed.
    fn armed(&self, fault: Fault) {
        if self.counted && self.fail_next.swap(false, Ordering::Relaxed) {
            if let Some(sim) = &self.sim {
                sim.place(sim.steps() + 1, fault);
            }
        }
    }
}

impl Watched {
    fn new(inner: Arc<dyn Fs>) -> Self {
        Self {
            inner,
            changes: Arc::default(),
            largest_read: Arc::default(),
            bad: Arc::default(),
            bad_reads: Arc::default(),
            cache_only: Arc::default(),
            fail_next: Arc::default(),
            rereads: Arc::default(),
            sim: None,
        }
    }

    fn count(&self, path: &str) {
        if path.starts_with(FOLDER) {
            self.changes.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn file(&self, path: &str, inner: Box<dyn File>) -> Box<dyn File> {
        Box::new(WatchedFile {
            inner,
            inner_fs: self.inner.clone(),
            path: path.to_owned(),
            counted: path.starts_with(FOLDER),
            changes: self.changes.clone(),
            largest_read: self.largest_read.clone(),
            bad: self.bad.clone(),
            bad_reads: self.bad_reads.clone(),
            cache_only: self.cache_only.clone(),
            fail_next: self.fail_next.clone(),
            rereads: self.rereads.clone(),
            sim: self.sim.clone(),
        })
    }
}

impl Fs for Watched {
    fn create_root(&self) -> sys::Result<()> {
        self.inner.create_root()
    }

    fn lock(&self) -> sys::Result<sys::FolderLock> {
        self.inner.lock()
    }

    fn owns(&self, lock: &sys::FolderLock) -> bool {
        self.inner.owns(lock)
    }

    fn create(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        self.count(path);
        let file = self.inner.create(path, options)?;
        Ok(self.file(path, file))
    }

    fn open(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        let file = self.inner.open(path, options)?;
        Ok(self.file(path, file))
    }

    fn create_folder(&self, path: &str) -> sys::Result<()> {
        self.count(path);
        self.inner.create_folder(path)
    }

    fn flush_folder(&self, path: &str) -> sys::Result<()> {
        self.inner.flush_folder(path)
    }

    fn rename(&self, from: &str, to: &str) -> sys::Result<()> {
        self.count(from);
        self.count(to);
        self.inner.rename(from, to)
    }

    fn remove(&self, path: &str) -> sys::Result<()> {
        self.count(path);
        self.inner.remove(path)
    }

    fn list(&self, path: &str) -> sys::Result<Vec<String>> {
        self.inner.list(path)
    }
}

impl File for WatchedFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
        self.largest_read
            .fetch_max(buf.len() as u64, Ordering::Relaxed);
        let end = offset + buf.len() as u64;
        let bad = self
            .bad
            .lock()
            .unwrap()
            .iter()
            .any(|(path, at)| *path == self.path && (offset..end).contains(at));
        if bad {
            self.bad_reads.fetch_add(1, Ordering::Relaxed);
            return Err(sys::Error::new(ErrorKind::Read, sys::Op::Read, &self.path));
        }
        let n = self.inner.read_at(offset, buf)?;
        self.rereads
            .lock()
            .unwrap()
            .after_read(&self.path, offset, &mut buf[..n]);
        Ok(n)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        if self.counted {
            self.changes.fetch_add(1, Ordering::Relaxed);
            if self.cache_only.swap(false, Ordering::Relaxed) {
                // Through a handle without synchronous writes: the bytes
                // stay pending in the cache, and the write fails.
                let plain = self.inner_fs.open(&self.path, FileOptions::PLAIN)?;
                plain.write_at(offset, data)?;
                return Err(sys::Error::new(
                    ErrorKind::Write,
                    sys::Op::Write,
                    &self.path,
                ));
            }
        }
        self.armed(Fault::FailWrite);
        self.inner.write_at(offset, data)
    }

    fn flush(&self) -> sys::Result<()> {
        self.armed(Fault::FailFlush);
        self.inner.flush()
    }

    fn allocate(&self, size: u64) -> sys::Result<()> {
        if self.counted {
            self.changes.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.allocate(size)
    }

    fn size(&self) -> sys::Result<u64> {
        self.inner.size()
    }
}

/// A machine the scenario runs on.
trait Machine {
    fn fs(&self) -> Arc<dyn Fs>;
    /// How many calls so far changed the log's files.
    fn changes(&self) -> u64;
    /// The next write to a log file reaches the cache only, then fails
    /// (simulated machines only).
    fn arm_cache_only_write(&self) -> bool;
    /// The next flush or write of a log file fails (simulated machines
    /// only).
    fn arm_failure(&self);
    /// An operation that reads begins (see [`Rereads`]).
    fn begin_reads(&self);
    /// The device can no longer read the byte at `offset` of `path`, or,
    /// with `None`, can read every byte again.
    fn set_bad(&self, place: Option<(String, u64)>);
    /// How many reads failed on such a byte.
    fn bad_reads(&self) -> u64;
    fn random(&self) -> &dyn Random;
    /// The process ends; with `power_loss` the power fails first, where the
    /// machine can do that.
    fn restart(&mut self, power_loss: bool);
    fn sim(&self) -> Option<&Sim>;
}

struct Simulated {
    sim: Sim,
    fs: Arc<Watched>,
    random: SimRandom,
}

impl Simulated {
    fn new(seed: u64, rates: Rates) -> Self {
        let sim = Sim::with_rates(seed, rates);
        let mut watched = Watched::new(Arc::new(sim.fs()));
        watched.sim = Some(sim.clone());
        Self {
            fs: Arc::new(watched),
            random: sim.random(),
            sim,
        }
    }

    /// A second read of the same bytes in one operation differs in one
    /// byte, drawn from `seed` (see [`Rereads`]).
    fn with_rereads_changed(self, seed: u64) -> Self {
        self.fs.rereads.lock().unwrap().draw = Some(Rng::stream(seed, 11));
        self
    }

    /// How many second reads came back changed.
    fn rereads_changed(&self) -> u64 {
        self.fs.rereads.lock().unwrap().changed
    }
}

impl Machine for Simulated {
    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }

    fn changes(&self) -> u64 {
        self.fs.changes.load(Ordering::Relaxed)
    }

    fn arm_cache_only_write(&self) -> bool {
        self.fs.cache_only.store(true, Ordering::Relaxed);
        true
    }

    fn arm_failure(&self) {
        self.fs.fail_next.store(true, Ordering::Relaxed);
    }

    fn begin_reads(&self) {
        self.fs.rereads.lock().unwrap().begin();
    }

    fn set_bad(&self, place: Option<(String, u64)>) {
        *self.fs.bad.lock().unwrap() = place.into_iter().collect();
    }

    fn bad_reads(&self) -> u64 {
        self.fs.bad_reads.load(Ordering::Relaxed)
    }

    fn random(&self) -> &dyn Random {
        &self.random
    }

    fn restart(&mut self, power_loss: bool) {
        self.fs.cache_only.store(false, Ordering::Relaxed);
        self.fs.fail_next.store(false, Ordering::Relaxed);
        if power_loss {
            self.sim.crash(Fates::Drawn);
        }
        self.sim.restart();
    }

    fn sim(&self) -> Option<&Sim> {
        Some(&self.sim)
    }
}

struct Real {
    _folder: TempFolder,
    fs: Arc<Watched>,
    random: OsRandom,
}

impl Real {
    fn new(tag: &str) -> Self {
        let folder = TempFolder::new(tag);
        Self {
            fs: Arc::new(Watched::new(Arc::new(folder.fs()))),
            _folder: folder,
            random: OsRandom::new(),
        }
    }
}

impl Machine for Real {
    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }

    fn changes(&self) -> u64 {
        self.fs.changes.load(Ordering::Relaxed)
    }

    fn arm_cache_only_write(&self) -> bool {
        false
    }

    fn arm_failure(&self) {}

    fn begin_reads(&self) {
        self.fs.rereads.lock().unwrap().begin();
    }

    fn set_bad(&self, place: Option<(String, u64)>) {
        *self.fs.bad.lock().unwrap() = place.into_iter().collect();
    }

    fn bad_reads(&self) -> u64 {
        self.fs.bad_reads.load(Ordering::Relaxed)
    }

    fn random(&self) -> &dyn Random {
        &self.random
    }

    /// The process "ends" by dropping the log and the state; a real power
    /// failure is not available.
    fn restart(&mut self, _power_loss: bool) {}

    fn sim(&self) -> Option<&Sim> {
        None
    }
}

/// A line of a run's trace, with the simulator's step count.
fn stamp(machine: &dyn Machine, line: String) -> String {
    format!("[{}] {line}", machine.sim().map_or(0, Sim::steps))
}

/// Which promises the device keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Device {
    /// It may crash, fail and damage, but never lies about a write.
    Honest,
    /// It may also misplace or drop a write it reported done: what is read
    /// is still only what was appended, chained.
    Lying,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// Its last open succeeded and passed the checks.
    Open,
    /// An open reported damage the device did.
    Damaged,
    /// `state` had no valid copy (the device damaged it).
    NoState,
    /// Damage to both `state` and the log lost the last advance (format
    /// section 1.5, "What is not covered").
    Lost,
}

/// What the model knows (module docs).
struct Model {
    /// Entries reported durable, by index.
    acked: BTreeMap<u64, Entry>,
    /// The durable position last reported.
    durable: u64,
    /// The durable position before the last report that moved the marker
    /// with one copy of `state`: losing that copy goes back to it.
    before_advance: u64,
    /// Entries that may or may not be found after a restart: appended and
    /// not reported durable, or cut by a call that failed.
    maybe: BTreeMap<u64, Vec<Entry>>,
    /// Every entry ever appended, by index (for a lying device).
    ever: BTreeMap<u64, Vec<Entry>>,
    /// The checkpoint the next open reads from.
    start: Position,
    /// The highest committed position the scenario, as the leader, has
    /// given the log, in calls that succeeded or failed. What it gives is
    /// never past the log's own last entry (`Log::set_committed`), but a
    /// power loss can take entries back below it.
    committed: u64,
    /// The highest committed position given since the last open, in calls
    /// that succeeded: the log refuses every cut at or before it.
    told: u64,
    appended: Position,
    /// Cuts of durable entries reported done, and those also attempted by
    /// calls that failed: the cut count `state` holds is between them.
    cuts: u64,
    cuts_attempted: u64,
    /// `state` was made and confirmed.
    state_made: bool,
}

impl Model {
    fn new() -> Self {
        Self {
            acked: BTreeMap::new(),
            durable: 0,
            before_advance: 0,
            maybe: BTreeMap::new(),
            ever: BTreeMap::new(),
            start: Position::START,
            committed: 0,
            told: 0,
            appended: Position::START,
            cuts: 0,
            cuts_attempted: 0,
            state_made: false,
        }
    }

    fn position(&self, index: u64) -> Option<Position> {
        if index == self.start.index {
            return Some(self.start);
        }
        self.acked.get(&index).map(Entry::position)
    }
}

/// Counts over many runs, to show the schedules reach what they test.
#[derive(Debug, Default)]
struct Reached {
    opens: u64,
    kept_tails: u64,
    open_cuts: u64,
    cuts_of_durable: u64,
    in_memory_cuts: u64,
    switches: u64,
    retired: u64,
    prepared: u64,
    no_space: u64,
    batch_full: u64,
    poisoned: u64,
    retried_opens: u64,
    unreadable_ends: u64,
    given_up: u64,
    given_up_for_good: u64,
    cut_past_unreadable: u64,
    lost_slots: u64,
    tails_in_cache: u64,
    unconfirmed: u64,
    reappended: u64,
    damaged: u64,
    no_state: u64,
    lost: u64,
    unreadable: u64,
    fired: BTreeMap<Fault, u64>,
}

impl Reached {
    fn add_fired(&mut self, sim: &Sim) {
        for (fault, n) in sim.fired() {
            *self.fired.entry(fault).or_default() += n;
        }
    }

    /// Every fault `rates` draws had an effect somewhere.
    fn assert_fired(&self, rates: &Rates) {
        let drawn = [
            (Fault::Crash, rates.crash),
            (Fault::FailFlush, rates.fail_flush),
            (Fault::FailWrite, rates.fail_write),
            (Fault::NoSpace, rates.no_space),
            (Fault::FailRead, rates.fail_read),
            (Fault::Damage, rates.damage),
            (Fault::Misplace, rates.misplace),
            (Fault::LoseWrite, rates.lose_write),
            (Fault::Writeback, rates.writeback),
        ];
        for (fault, rate) in drawn {
            if rate != 0 {
                assert!(
                    self.fired.get(&fault).copied().unwrap_or(0) > 0,
                    "{fault:?} never fired: {self:?}"
                );
            }
        }
    }
}

/// A step of a schedule.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// Append `n` entries, with large payloads when `big`.
    Append {
        n: u32,
        big: bool,
    },
    /// Flush the batch, giving a committed position when `committed`.
    Flush {
        committed: bool,
    },
    SetCommitted,
    /// Cut from `back` entries before the last appended one.
    Cut {
        back: u64,
    },
    /// Take a checkpoint at the committed position and retire what it
    /// covers.
    Retire,
    Prepare,
    Read,
    Stop,
    Crash,
    /// The device loses the slot of `state` that holds its newest copy,
    /// and the process stops: the next open finds the marker one advance
    /// back (simulated machines only).
    LoseStateSlot,
    /// A batch's write reaches the system's cache only, whole, and fails;
    /// the process stops without a power loss, and the next open meets a
    /// failed flush or write somewhere (simulated machines only).
    TailInCacheOnly,
}

/// A schedule that does every operation at least once, for the crash at
/// every step.
const SCRIPT: &[Action] = &[
    Action::Append { n: 3, big: false },
    Action::Flush { committed: false },
    Action::Append { n: 4, big: true },
    Action::Flush { committed: true },
    Action::Prepare,
    Action::Append { n: 2, big: false },
    Action::Flush { committed: false },
    Action::SetCommitted,
    Action::Append { n: 2, big: false },
    Action::Flush { committed: false },
    Action::Append { n: 2, big: true },
    Action::Flush { committed: false },
    Action::Cut { back: 2 },
    Action::Append { n: 2, big: false },
    Action::Flush { committed: true },
    Action::Retire,
    Action::Append { n: 2, big: false },
    Action::Cut { back: 0 },
    Action::Flush { committed: false },
    Action::Stop,
    Action::Append { n: 5, big: true },
    Action::Flush { committed: false },
    Action::Read,
    Action::Crash,
    Action::Append { n: 1, big: false },
    Action::Flush { committed: false },
];

/// One run of the scenario.
struct Run<'a> {
    machine: &'a mut dyn Machine,
    device: Device,
    seed: u64,
    plan: Rng,
    rates: Rates,
    log: Option<Log>,
    /// A tail was held in the cache only, and the opens after it have not
    /// yet succeeded.
    cache_tail_pending: bool,
    /// What followed an unreadable place that stays unreadable was given
    /// up, when the device had failed this many reads on it: no later read
    /// may reach it, until an open succeeds and the place reads again.
    given_up_place: Option<u64>,
    model: Model,
    term: u64,
    reached: &'a mut Reached,
    trace: Vec<String>,
}

/// A failing run prints what the machine did, to read with the seed.
impl Drop for Run<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(sim) = self.machine.sim() {
                eprintln!("seed {}: the machine's events:", self.seed);
                for event in sim.events() {
                    eprintln!("  {event:?}");
                }
            }
            eprintln!("the run's steps:");
            for line in &self.trace {
                eprintln!("  {line}");
            }
        }
    }
}

impl<'a> Run<'a> {
    fn new(
        machine: &'a mut dyn Machine,
        device: Device,
        seed: u64,
        rates: Rates,
        reached: &'a mut Reached,
    ) -> Self {
        Self {
            machine,
            device,
            seed,
            plan: Rng::stream(seed, 9),
            rates,
            log: None,
            cache_tail_pending: false,
            given_up_place: None,
            model: Model::new(),
            term: 1,
            reached,
            trace: Vec::new(),
        }
    }

    /// `actions` steps drawn from the plan, then a last open without
    /// faults.
    fn go(self, actions: usize) -> End {
        self.drive(actions, None)
    }

    /// The steps of `script`, then a last open without faults.
    fn script(self, script: &[Action]) -> End {
        self.drive(script.len(), Some(script))
    }

    fn drive(mut self, actions: usize, script: Option<&[Action]>) -> End {
        for i in 0..actions {
            if self.log.is_none() {
                if let Some(end) = self.start() {
                    return self.end(end);
                }
                if self.log.is_none() {
                    continue;
                }
            }
            let action = match script {
                Some(script) => script[i],
                None => self.draw(),
            };
            self.perform(action);
        }
        // Recovery without drawn faults.
        self.rates = Rates::default();
        if let Some(sim) = self.machine.sim() {
            sim.set_rates(Rates::default());
        }
        self.stop(true);
        for _ in 0..3 {
            if let Some(end) = self.start() {
                return self.end(end);
            }
            if self.log.is_some() {
                return self.end(End::Open);
            }
        }
        panic!("seed {}: the log did not open without faults", self.seed);
    }

    fn end(mut self, end: End) -> End {
        if let Some(sim) = self.machine.sim() {
            let sim = sim.clone();
            self.reached.add_fired(&sim);
        }
        match end {
            End::Damaged => self.reached.damaged += 1,
            End::NoState => self.reached.no_state += 1,
            End::Lost => self.reached.lost += 1,
            End::Open => {}
        }
        self.log = None;
        end
    }

    fn draw(&mut self) -> Action {
        match self.plan.below(100) {
            0..=36 => Action::Append {
                n: 1 + self.plan.below(4) as u32,
                big: self.plan.below(4) == 0,
            },
            37..=56 => Action::Flush {
                committed: self.plan.below(2) == 0,
            },
            57..=60 => Action::SetCommitted,
            61..=67 => Action::Cut {
                back: self.plan.below(6),
            },
            68..=72 => Action::Retire,
            73..=76 => Action::Prepare,
            77..=82 => Action::Read,
            83..=90 => Action::Stop,
            91 => Action::LoseStateSlot,
            92 => Action::TailInCacheOnly,
            _ => Action::Crash,
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Append { n, big } => {
                for _ in 0..n {
                    if self.log.is_none() || !self.append(big) {
                        break;
                    }
                }
            }
            Action::Flush { committed } => self.flush(committed),
            Action::SetCommitted => self.set_committed(),
            Action::Cut { back } => self.cut(back),
            Action::Retire => self.retire(),
            Action::Prepare => self.prepare(),
            Action::Read => self.read_check(),
            Action::Stop => self.stop(false),
            Action::Crash => self.stop(true),
            Action::LoseStateSlot => self.lose_state_slot(),
            Action::TailInCacheOnly => self.tail_in_cache_only(),
        }
    }

    /// The simulator's events from `from` on.
    fn events_since(&self, from: usize) -> Vec<sim::Event> {
        self.machine
            .sim()
            .map(|sim| sim.events()[from..].to_vec())
            .unwrap_or_default()
    }

    /// The paths the device damaged, from event `from` on.
    fn damaged_paths(&self, from: usize) -> BTreeSet<String> {
        self.events_since(from)
            .into_iter()
            .filter_map(|event| match event {
                sim::Event::Damaged { path, .. } => Some(path),
                _ => None,
            })
            .collect()
    }

    /// No drawn faults while `f` checks.
    fn quiet<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        if let Some(sim) = self.machine.sim() {
            sim.set_rates(Rates::default());
        }
        let out = f(self);
        if let Some(sim) = self.machine.sim() {
            sim.set_rates(self.rates);
        }
        out
    }

    fn tail_in_cache_only(&mut self) {
        if self.machine.sim().is_none() {
            return;
        }
        if !self.append(false) || self.log.is_none() {
            return;
        }
        // The batch's write: whole in the cache, not on the device, and it
        // fails. The entries stay in `maybe`.
        self.machine.arm_cache_only_write();
        let flushed = self.log.as_mut().expect("open").flush_batch(None);
        self.trace.push(stamp(
            &*self.machine,
            format!("flush, its write held in the cache: {flushed:?}"),
        ));
        if flushed.is_ok() {
            return;
        }
        self.reached.tails_in_cache += 1;
        // The process stops and the cache stays; the next open's first
        // flush or write of a log file, the one that makes the tail durable,
        // fails. The open is then tried again with its state, or by a new
        // process, and power losses follow.
        self.stop(false);
        self.machine.arm_failure();
        self.cache_tail_pending = true;
    }

    fn lose_state_slot(&mut self) {
        let Some(sim) = self.machine.sim().cloned() else {
            return;
        };
        let (_, slot) = self.log.as_ref().expect("open").state().copy();
        self.trace
            .push(stamp(&*self.machine, format!("lose state slot {slot}")));
        self.stop(false);
        let at = (slot * crate::state::SLOT + 40) as u64;
        sim.damage(crate::state::FILE, at, 2)
            .expect("state is on the device");
        self.reached.lost_slots += 1;
    }

    /// The process ends, the power failing with it when `power_loss`.
    fn stop(&mut self, power_loss: bool) {
        self.trace.push(stamp(
            &*self.machine,
            format!("stop (power loss: {power_loss})"),
        ));
        self.log = None;
        self.machine.restart(power_loss);
    }

    /// After a failure the store stops at once (section 6.12), and the
    /// power may fail too.
    fn fail(&mut self) {
        let power_loss = self.plan.below(2) == 0;
        self.stop(power_loss);
    }

    /// Take the lock, open `state` (or make it), open the log, and check
    /// what the open found.
    fn start(&mut self) -> Option<End> {
        // After a tail held in the cache only, the opens that follow run
        // without drawn faults, so that the failure armed for them is the
        // one met (the scenario's drawn rates come back after).
        let quiet = self.cache_tail_pending;
        if quiet {
            if let Some(sim) = self.machine.sim() {
                sim.set_rates(Rates::default());
            }
        }
        let end = self.start_once();
        if quiet {
            if let Some(sim) = self.machine.sim() {
                sim.set_rates(self.rates);
            }
        }
        end
    }

    fn start_once(&mut self) -> Option<End> {
        let seed = self.seed;
        let fs = self.machine.fs();
        let lock = match fs.lock() {
            Ok(lock) => lock,
            Err(e) => {
                assert_ne!(e.kind(), ErrorKind::Locked, "seed {seed}: {e}");
                self.fail();
                return None;
            }
        };
        self.machine.begin_reads();
        let state = match State::open(&*fs, lock) {
            Ok(state) => state,
            Err(StateError::Missing) => {
                assert!(
                    !self.model.state_made,
                    "seed {seed}: state is missing after it was made"
                );
                let made = fs
                    .lock()
                    .map_err(StateError::Disk)
                    .and_then(|lock| State::create(&*fs, self.machine.random(), lock));
                match made {
                    Ok(state) => state,
                    Err(StateError::Disk(_)) => {
                        self.fail();
                        return None;
                    }
                    Err(e) => panic!("seed {seed}: create: {e}"),
                }
            }
            Err(e @ StateError::NoValidCopy { .. }) => {
                let damaged = self.damaged_paths(0).contains(crate::state::FILE);
                assert!(
                    self.device == Device::Lying || damaged,
                    "seed {seed}: {e}, though the device never damaged state"
                );
                return Some(End::NoState);
            }
            Err(StateError::Disk(_) | StateError::Unreadable { .. }) => {
                self.fail();
                return None;
            }
            Err(e) => panic!("seed {seed}: open state: {e}"),
        };
        self.model.state_made = true;
        let mut state = state;
        let mut tries = 0;
        loop {
            // What this attempt writes, not an earlier one's.
            let before = self.machine.changes();
            self.machine.begin_reads();
            let opened = Log::open(fs.clone(), state, CONFIG, self.model.start);
            if let Some(reads) = self.given_up_place {
                assert_eq!(
                    self.machine.bad_reads(),
                    reads,
                    "seed {}: an open read past the entry after which an unreadable place was \
                     given up",
                    self.seed
                );
                if opened.is_ok() {
                    self.given_up_place = None;
                    self.machine.set_bad(None);
                }
            }
            self.trace.push(stamp(
                &*self.machine,
                match &opened {
                    Ok((_, opened)) => format!("open from {}: {opened:?}", self.model.start.index),
                    Err(e) => format!("open from {}: {e}", self.model.start.index),
                },
            ));
            match opened {
                Ok((log, opened)) => {
                    self.log = Some(log);
                    let end = self.check_open(&opened);
                    let after_cache_tail = std::mem::replace(&mut self.cache_tail_pending, false);
                    if end.is_none()
                        && self.log.is_some()
                        && (after_cache_tail || (tries > 0 && self.plan.below(2) == 0))
                    {
                        // What a retried open relied on must hold across a
                        // power loss: append, flush, and lose the power.
                        self.perform(Action::Append { n: 2, big: false });
                        if self.log.is_some() {
                            self.flush(false);
                        }
                        self.stop(true);
                    }
                    return end;
                }
                Err(failed) => {
                    // The log hands `state` back.
                    let OpenFailed { error, state: back } = *failed;
                    let retryable = matches!(
                        error,
                        LogError::Unreadable { .. } | LogError::Disk(_) | LogError::State(_)
                    );
                    let running = self.machine.sim().is_none_or(|sim| !sim.halted());
                    if retryable && self.cache_tail_pending && running && self.plan.below(2) == 0 {
                        // A new process, the system's cache kept.
                        drop(back);
                        self.stop(false);
                        return None;
                    }
                    if retryable
                        && running
                        && tries < 2
                        && (self.cache_tail_pending || self.plan.below(2) == 0)
                    {
                        // Tried again with the same state, in the same
                        // process.
                        tries += 1;
                        self.reached.retried_opens += 1;
                        state = back;
                        continue;
                    }
                    drop(back);
                    match error {
                        LogError::Damaged(damage) => {
                            self.check_damage(&damage, before);
                            return Some(End::Damaged);
                        }
                        LogError::Unreadable { .. } => {
                            self.reached.unreadable += 1;
                            self.fail();
                        }
                        LogError::Disk(_) | LogError::State(_) => self.fail(),
                        e => panic!("seed {seed}: open: {e}"),
                    }
                    return None;
                }
            }
        }
    }

    /// Damage is reported only where the device damaged a segment, and the
    /// open changed nothing.
    fn check_damage(&mut self, damage: &Damage, changes_before: u64) {
        let seed = self.seed;
        assert!(
            self.machine.sim().is_some(),
            "seed {seed}: {damage:?} on a file system without faults"
        );
        assert_eq!(
            self.machine.changes(),
            changes_before,
            "seed {seed}: an open that found {damage:?} changed the log"
        );
        if self.device == Device::Honest {
            let path = format::segment_path(damage.segment);
            assert!(
                self.damaged_paths(0).contains(&path),
                "seed {seed}: {damage:?}, though the device never damaged {path}"
            );
        }
    }

    /// The checks of the module docs after an open that succeeded. After
    /// one that ended at a place it could not read they all run too, and
    /// the model keeps what may lie past that place.
    fn check_open(&mut self, opened: &Opened) -> Option<End> {
        let seed = self.seed;
        self.reached.opens += 1;
        let unreadable = opened.unreadable_end.is_some();
        if unreadable {
            // The log ends, for this open, where the device failed to read;
            // a confirmed batch may lie past it (a lost marker advance), so
            // appends wait.
            self.reached.unreadable_ends += 1;
            let log = self.log.as_mut().expect("opened");
            assert!(
                matches!(log.append(1, 0, 0, b""), Err(LogError::EndUnreadable(_))),
                "seed {seed}"
            );
        }
        if opened.kept > 0 {
            self.reached.kept_tails += 1;
        }
        if opened.cut.is_some() {
            self.reached.open_cuts += 1;
        }
        assert!(
            !(unreadable && opened.cut.is_some()),
            "seed {seed}: an open that ended unreadable cut: {opened:?}"
        );
        let durable = opened.durable;
        {
            let log = self.log.as_ref().expect("opened");
            let marker = log.state().values().marker;
            assert_eq!(
                (marker.index, marker.hash),
                (durable.index, durable.hash),
                "seed {seed}"
            );
            assert_eq!(log.durable(), durable, "seed {seed}");
            assert_eq!(log.appended(), durable, "seed {seed}");
        }
        let lying = self.device == Device::Lying;
        let mut lost = false;
        if durable.index < self.model.durable && !lying {
            let damaged = self.damaged_paths(0);
            let state_lost = damaged.contains(crate::state::FILE)
                || self.events_since(0).iter().any(|event| {
                    matches!(event, sim::Event::ReadFailed { path, .. } if path == crate::state::FILE)
                });
            if unreadable {
                // A copy of `state` lost (damaged, or unreadable at an open,
                // which `state` passes over like a lost one) took the marker
                // back, and the open could not read past it: what was
                // reported durable after it may lie past that place, and
                // the model still expects it.
                assert!(
                    state_lost,
                    "seed {seed}: the durable position went back from {} to {} at an unreadable \
                     end",
                    self.model.durable, durable.index
                );
            } else {
                // A copy of `state` lost together with a damaged tail: the
                // double fault format section 1.5 leaves uncovered, which
                // loses at most the last advance.
                let log_damaged = damaged.iter().any(|p| p.starts_with("log/"));
                assert!(
                    state_lost && log_damaged && durable.index >= self.model.before_advance,
                    "seed {seed}: the durable position went back from {} to {}",
                    self.model.durable,
                    durable.index
                );
                lost = true;
            }
        }
        // Read back from the start, against the model.
        let from = self.model.start.index + 1;
        let read: Result<Vec<Entry>, LogError> = self.quiet(|run| {
            run.machine.begin_reads();
            run.log
                .as_ref()
                .expect("opened")
                .read(from)
                .and_then(|reader| reader.collect())
        });
        let entries = match read {
            Ok(entries) => entries,
            Err(e @ (LogError::Disk(_) | LogError::Unreadable { .. })) => {
                // A placed crash or read failure in the check: the process
                // ends, and the next open checks again.
                self.trace
                    .push(stamp(&*self.machine, format!("check read: {e}")));
                self.stop(false);
                return None;
            }
            Err(LogError::Damaged(damage)) => {
                // The device damaged what the open had read, and the cache
                // dropped it since.
                let path = format::segment_path(damage.segment);
                assert!(
                    self.device == Device::Lying || self.damaged_paths(0).contains(&path),
                    "seed {seed}: reading back after the open found {damage:?}, though the \
                     device never damaged {path}"
                );
                return Some(End::Damaged);
            }
            Err(e) => panic!("seed {seed}: reading back after the open: {e}"),
        };
        let mut prev = self.model.start.hash;
        let mut next = from;
        for entry in &entries {
            assert_eq!(entry.index, next, "seed {seed}: entries out of order");
            assert_eq!(
                entry.hash,
                expected_hash(&prev, entry),
                "seed {seed}: the chain breaks at {}",
                entry.index
            );
            if lying {
                assert!(
                    self.model
                        .ever
                        .get(&entry.index)
                        .is_some_and(|all| all.contains(entry)),
                    "seed {seed}: entry {} was never appended",
                    entry.index
                );
            } else if entry.index <= self.model.durable && !lost {
                assert_eq!(
                    Some(entry),
                    self.model.acked.get(&entry.index),
                    "seed {seed}: durable entry {} differs",
                    entry.index
                );
            } else if entry.index <= self.model.durable {
                assert!(
                    self.model.acked.get(&entry.index) == Some(entry),
                    "seed {seed}: entry {} differs from the durable one",
                    entry.index
                );
            } else {
                assert!(
                    self.model
                        .maybe
                        .get(&entry.index)
                        .is_some_and(|all| all.contains(entry)),
                    "seed {seed}: entry {} after the durable ones was not appended there",
                    entry.index
                );
            }
            prev = entry.hash;
            next += 1;
        }
        assert_eq!(
            next - 1,
            durable.index.max(self.model.start.index),
            "seed {seed}: the read ended before the durable position"
        );
        // The model takes what the open returned.
        for entry in entries {
            self.model.acked.insert(entry.index, entry);
        }
        if unreadable {
            // Nothing past the unreadable place was cut: what the model was
            // owed there, or may find there, stays.
            self.model.maybe = self.model.maybe.split_off(&(durable.index + 1));
            self.model.before_advance = self.model.before_advance.min(durable.index - opened.kept);
            self.model.durable = self.model.durable.max(durable.index);
        } else {
            self.model.acked.split_off(&(durable.index + 1));
            self.model.maybe.clear();
            self.model.durable = durable.index;
            // An open that kept a tail moved the marker over it with one
            // advance of `state`.
            self.model.before_advance = durable.index - opened.kept;
        }
        self.model.appended = durable;
        // The recorded committed position is never more than the scenario
        // gave, and is not confirmed until it gives one again; the cut count
        // is what the cuts reported done and attempted allow.
        let log = self.log.as_ref().expect("opened");
        assert!(
            lying || log.committed() <= self.model.committed,
            "seed {seed}: committed {} recorded, though only {} was given",
            log.committed(),
            self.model.committed
        );
        assert!(!log.committed_confirmed(), "seed {seed}");
        self.model.told = 0;
        let cut_count = log.state().values().cut_count;
        assert_eq!(opened.cut_count, cut_count, "seed {seed}");
        assert!(
            lying || (self.model.cuts..=self.model.cuts_attempted).contains(&cut_count),
            "seed {seed}: cut count {cut_count}, though {} cuts were done and {} attempted",
            self.model.cuts,
            self.model.cuts_attempted
        );
        self.model.cuts = cut_count;
        self.model.cuts_attempted = cut_count;
        let last_term = self
            .model
            .acked
            .range(..=durable.index)
            .next_back()
            .map_or(self.model.start.term, |(_, e)| e.term);
        self.term = self.term.max(last_term);
        if lost {
            return Some(End::Lost);
        }
        if let Some(end) = opened.unreadable_end {
            self.past_unreadable_end(durable, end);
        }
        None
    }

    /// The log ends, for this open, at a place it could not read, after
    /// `durable`. The scenario opens it again, gives up what may follow (the
    /// administrator accepting the loss), or cuts past it as a leader
    /// would.
    fn past_unreadable_end(&mut self, durable: Position, end: UnreadableEnd) {
        let seed = self.seed;
        let choice = self.plan.below(3);
        if choice == 0 {
            self.stop(false);
            return;
        }
        // The place stays unreadable (a failed sector): once what follows
        // it is given up, nothing may read it again, until a cut takes the
        // log back into the segment it is in. A device that drops writes
        // can drop the fresh segment's header.
        let stays = self.machine.sim().is_some() && self.device == Device::Honest;
        if stays {
            self.machine
                .set_bad(Some((format::segment_path(end.segment), end.offset)));
        }
        self.machine.begin_reads();
        let log = self.log.as_mut().expect("opened");
        let done = if choice == 1 {
            // Giving up needs no committed position, and is refused for any
            // other entry than the last one verified.
            assert!(!log.committed_confirmed(), "seed {seed}");
            let mut other = durable.hash;
            other[0] ^= 1;
            assert!(
                matches!(
                    log.give_up_unreadable_end(durable.index, other),
                    Err(LogError::Refused(_))
                ),
                "seed {seed}"
            );
            log.give_up_unreadable_end(durable.index, durable.hash)
        } else {
            // As a leader: confirm, then cut from the entry after the
            // durable one. A cut further on removes nothing and leaves the
            // end unreadable: a cut never removes anything before its index.
            let given = self.model.committed.min(durable.index);
            if let Err(e) = log.set_committed(given) {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.machine.set_bad(None);
                self.fail();
                return;
            }
            self.model.told = self.model.told.max(given);
            let further = durable.index + 2 + self.plan.below(2);
            let nothing = log.cut(further);
            assert!(
                matches!(&nothing, Ok(done) if *done == CutDone::nothing(durable)),
                "seed {seed}: a cut from {further}: {nothing:?}"
            );
            assert!(
                matches!(log.append(1, 0, 0, b""), Err(LogError::EndUnreadable(_))),
                "seed {seed}"
            );
            log.cut(durable.index + 1)
        };
        self.trace.push(stamp(
            &*self.machine,
            format!("past the unreadable end after {}: {done:?}", durable.index),
        ));
        let log = self.log.as_mut().expect("opened");
        if stays && done.is_err() {
            self.machine.set_bad(None);
        }
        match done {
            Ok(done) => {
                if stays {
                    self.given_up_place = Some(self.machine.bad_reads());
                    self.reached.given_up_for_good += 1;
                }
                assert_eq!(done.last, durable, "seed {seed}");
                assert!(
                    done.unreadable_end.is_some() && done.fence.is_some(),
                    "seed {seed}: {done:?}"
                );
                assert_eq!((done.unflushed, done.durable), (0, 0), "seed {seed}");
                assert!(
                    self.device == Device::Lying || done.cut_count == Some(self.model.cuts + 1),
                    "seed {seed}: {done:?} after {} cuts",
                    self.model.cuts
                );
                if choice == 1 {
                    self.reached.given_up += 1;
                } else {
                    self.reached.cut_past_unreadable += 1;
                }
                // Whatever followed is gone, the loss accepted, and what was
                // committed past it with it.
                self.model.acked.split_off(&(durable.index + 1));
                self.model.maybe.split_off(&(durable.index + 1));
                self.model.durable = durable.index;
                self.model.committed = self.model.committed.min(durable.index);
                self.model.before_advance = durable.index;
                self.model.cuts += 1;
                self.model.cuts_attempted = self.model.cuts_attempted.max(self.model.cuts);
            }
            Err(e @ (LogError::NoSpace(_) | LogError::Disk(_))) if !log.is_poisoned() => {
                // Refused before anything listed changed.
                self.trace
                    .push(stamp(&*self.machine, format!("give up refused: {e}")));
                self.stop(false);
            }
            Err(e) => {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                // Given up or not: what followed may be found again, or not.
                self.model.cuts_attempted += 1;
                for (index, entry) in self.model.acked.split_off(&(durable.index + 1)) {
                    self.model.maybe.entry(index).or_default().push(entry);
                }
                self.model.durable = durable.index;
                self.model.before_advance = self.model.before_advance.min(durable.index);
                self.fail();
            }
        }
    }

    fn payload(&mut self, big: bool) -> Vec<u8> {
        let len = if big {
            200 + self.plan.below(u64::from(CONFIG.max_payload) - 199)
        } else {
            match self.plan.below(10) {
                0..=3 => self.plan.below(9),
                4..=8 => 9 + self.plan.below(100),
                _ => 100 + self.plan.below(300),
            }
        };
        let mut payload = vec![0u8; len as usize];
        self.plan.fill(&mut payload);
        payload
    }

    /// Append one entry; false when the schedule should stop appending.
    fn append(&mut self, big: bool) -> bool {
        let seed = self.seed;
        if self.plan.below(15) == 0 {
            self.term += 1;
        }
        let payload = self.payload(big);
        let kind = self.plan.below(6) as u16;
        let file = self.plan.below(4);
        let term = self.term;
        // Now and then, an append that breaks a rule is refused with
        // nothing added.
        if self.plan.below(25) == 0 {
            let log = self.log.as_mut().expect("open");
            let too_large = vec![0u8; CONFIG.max_payload as usize + 1];
            assert!(
                matches!(
                    log.append(term, kind, file, &too_large),
                    Err(LogError::TooLarge { .. })
                ),
                "seed {seed}"
            );
            if term > 1 && self.model.appended.term == term {
                assert!(
                    matches!(
                        log.append(term - 1, kind, file, &payload),
                        Err(LogError::Refused(_))
                    ),
                    "seed {seed}"
                );
            }
            assert_eq!(log.appended(), self.model.appended, "seed {seed}");
        }
        self.append_entry(term, kind, file, payload)
    }

    /// Append one entry with these fields; false when the schedule should
    /// stop appending.
    fn append_entry(&mut self, term: u64, kind: u16, file: u64, payload: Vec<u8>) -> bool {
        let seed = self.seed;
        let log = self.log.as_mut().expect("open");
        let appended = log.append(term, kind, file, &payload);
        match appended {
            Ok(position) => {
                let entry = Entry {
                    index: self.model.appended.index + 1,
                    term,
                    kind,
                    file,
                    payload,
                    hash: position.hash,
                };
                assert_eq!(position.index, entry.index, "seed {seed}");
                assert_eq!(position.term, term, "seed {seed}");
                assert_eq!(
                    position.hash,
                    expected_hash(&self.model.appended.hash, &entry),
                    "seed {seed}: append returned another hash"
                );
                self.model.appended = position;
                self.model
                    .maybe
                    .entry(entry.index)
                    .or_default()
                    .push(entry.clone());
                self.model.ever.entry(entry.index).or_default().push(entry);
                if log.spare_ready() {
                    self.reached.prepared += 1;
                }
                true
            }
            Err(LogError::BatchFull) => {
                self.reached.batch_full += 1;
                assert_eq!(log.appended(), self.model.appended, "seed {seed}");
                self.flush(false);
                false
            }
            Err(e @ (LogError::NoSpace(_) | LogError::Disk(_))) if !log.is_poisoned() => {
                // Refused before anything was written; the log goes on.
                self.trace
                    .push(stamp(&*self.machine, format!("append refused: {e}")));
                self.reached.no_space += 1;
                assert_eq!(log.appended(), self.model.appended, "seed {seed}");
                false
            }
            Err(e) => {
                self.trace
                    .push(stamp(&*self.machine, format!("append: {e}")));
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
                false
            }
        }
    }

    fn flush(&mut self, with_committed: bool) {
        let seed = self.seed;
        // A leader gives at least the commit index it gave before, and the
        // log is given the lower of that and its own last entry (Raft's
        // follower rule; every entry here matches the scenario's).
        let last = self.model.appended.index;
        let committed = with_committed.then(|| {
            let heard = last.saturating_sub(self.plan.below(4));
            heard.max(self.model.committed).min(last)
        });
        let log = self.log.as_mut().expect("open");
        if self.plan.below(10) == 0 {
            // One past the last entry is refused, with nothing written.
            let past = last + 1 + self.plan.below(3);
            let batch = log.batch_len();
            assert!(
                matches!(log.flush_batch(Some(past)), Err(LogError::Refused(_))),
                "seed {seed}"
            );
            assert!(!log.is_poisoned(), "seed {seed}");
            assert_eq!(log.batch_len(), batch, "seed {seed}");
            assert_eq!(log.appended(), self.model.appended, "seed {seed}");
        }
        let segments = log.segments().count;
        let flushed = log.flush_batch(committed);
        self.trace
            .push(format!("flush ({committed:?}): {flushed:?}"));
        match flushed {
            Ok(position) => {
                assert_eq!(position, self.model.appended, "seed {seed}");
                if log.segments().count > segments {
                    self.reached.switches += 1;
                }
                let durable = position.index;
                for index in self.model.durable + 1..=durable {
                    let entry = self
                        .model
                        .maybe
                        .remove(&index)
                        .and_then(|mut all| all.pop())
                        .expect("appended");
                    self.model.acked.insert(index, entry);
                }
                if durable > self.model.durable {
                    self.model.before_advance = self.model.durable;
                }
                self.model.durable = durable;
                if let Some(c) = committed {
                    self.model.committed = self.model.committed.max(c);
                    self.model.told = self.model.told.max(c);
                    assert!(log.committed_confirmed(), "seed {seed}");
                }
            }
            Err(e) => {
                // The leader knows the position it gave, whether or not
                // `state` recorded it.
                if let Some(c) = committed {
                    self.model.committed = self.model.committed.max(c);
                }
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                assert!(matches!(log.append(1, 0, 0, b""), Err(LogError::Poisoned)));
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    fn set_committed(&mut self) {
        let seed = self.seed;
        let log = self.log.as_mut().expect("open");
        // Everything durable is committed, and no more can be given.
        let committed = log.durable().index;
        if self.plan.below(5) == 0 {
            assert!(
                matches!(log.set_committed(committed + 1), Err(LogError::Refused(_))),
                "seed {seed}"
            );
            assert!(!log.is_poisoned(), "seed {seed}");
        }
        let set = log.set_committed(committed);
        self.trace.push(stamp(
            &*self.machine,
            format!("set committed {committed}: {set:?}"),
        ));
        let log = self.log.as_mut().expect("open");
        match set {
            Ok(()) => {
                self.model.committed = self.model.committed.max(committed);
                self.model.told = self.model.told.max(committed);
                assert!(log.committed() >= committed, "seed {seed}");
                assert!(log.committed_confirmed(), "seed {seed}");
            }
            Err(e) => {
                self.model.committed = self.model.committed.max(committed);
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    fn cut(&mut self, back: u64) {
        let seed = self.seed;
        // Until the scenario, as the leader, gives a committed position
        // since the open, every cut is refused: the one recorded can be one
        // advance behind (a lost copy of `state`).
        let log = self.log.as_mut().expect("open");
        if !log.committed_confirmed() {
            let refused = log.cut(self.model.appended.index.max(1));
            assert!(
                matches!(refused, Err(LogError::CommittedUnconfirmed)),
                "seed {seed}: a cut before a committed position was given: {refused:?}"
            );
            assert_eq!(log.appended(), self.model.appended, "seed {seed}");
            self.reached.unconfirmed += 1;
            let given = self.model.committed.min(log.durable().index);
            if let Err(e) = log.set_committed(given) {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
                return;
            }
            self.model.told = self.model.told.max(given);
        }
        // A cut at or before a committed position given since the open, or
        // the checkpoint (which covers only committed entries), is refused
        // with nothing changed (section 7.1). The scenario, as the leader,
        // never asks for one at or before the committed position it gave.
        let refused_floor = self.model.told.max(self.model.start.index);
        let floor = self.model.committed.max(self.model.start.index);
        if refused_floor > 0 && self.plan.below(3) == 0 {
            let from = 1 + self.plan.below(refused_floor);
            let log = self.log.as_mut().expect("open");
            let refused = log.cut(from);
            if !matches!(refused, Err(LogError::Refused(_))) && self.device == Device::Lying {
                // A device that drops writes can leave the log's own floor
                // behind the model's (a retirement it refused): start again
                // from what the device holds.
                self.stop(false);
                return;
            }
            assert!(
                matches!(refused, Err(LogError::Refused(_))),
                "seed {seed}: a cut from {from} at or before {refused_floor} gave {refused:?}"
            );
            assert_eq!(log.appended(), self.model.appended, "seed {seed}");
            assert!(!log.is_poisoned(), "seed {seed}");
        }
        let last = self.model.appended.index;
        let from = last.saturating_sub(back).max(1);
        if from <= floor {
            return;
        }
        let durable_before = self.model.durable;
        if from <= durable_before {
            // A cut of durable entries can take the log back into the
            // segment of a place given up, past which a walk then reads
            // again: the place reads again too.
            self.given_up_place = None;
            self.machine.set_bad(None);
        }
        self.machine.begin_reads();
        let log = self.log.as_mut().expect("open");
        let cut = log.cut(from);
        self.trace
            .push(stamp(&*self.machine, format!("cut from {from}: {cut:?}")));
        match cut {
            Ok(done) => {
                let position = done.last;
                // What the cut says it did.
                if self.device == Device::Honest {
                    let (unflushed, durable, cut_count) = if from > last {
                        (0, 0, None)
                    } else if from > durable_before {
                        (last - from + 1, 0, None)
                    } else {
                        (
                            last - durable_before,
                            durable_before - from + 1,
                            Some(self.model.cuts + 1),
                        )
                    };
                    assert_eq!(
                        (done.unflushed, done.durable, done.cut_count),
                        (unflushed, durable, cut_count),
                        "seed {seed}: cut from {from}: {done:?}"
                    );
                    assert_eq!(
                        (done.unreadable_end, done.fence),
                        (None, None),
                        "seed {seed}"
                    );
                }
                let expected = if from > last {
                    self.model.appended
                } else if from - 1 > durable_before {
                    self.model.maybe[&(from - 1)]
                        .last()
                        .expect("appended")
                        .position()
                } else {
                    self.model.position(from - 1).unwrap_or_else(|| {
                        // A device that drops writes can take the log back
                        // behind what the model holds.
                        assert_eq!(self.device, Device::Lying, "seed {seed}: no entry {from}");
                        position
                    })
                };
                if position != expected && self.device == Device::Lying {
                    // The device dropped writes the model counts on: start
                    // again from what it holds.
                    self.stop(false);
                    return;
                }
                assert_eq!(position, expected, "seed {seed}");
                assert_eq!(log.appended(), position, "seed {seed}");
                let removed: Vec<Entry> = self.model.acked.split_off(&from).into_values().collect();
                self.model.maybe.split_off(&from);
                self.model.durable = self.model.durable.min(from - 1);
                self.model.appended = position;
                if from > durable_before {
                    if from <= last {
                        self.reached.in_memory_cuts += 1;
                    }
                    return;
                }
                self.reached.cuts_of_durable += 1;
                self.model.before_advance = from - 1;
                self.model.cuts += 1;
                self.model.cuts_attempted = self.model.cuts_attempted.max(self.model.cuts);
                // Sometimes the leader sends some of the cut entries again,
                // exactly: nothing written before the cut may come back
                // with them.
                if self.plan.below(2) == 0 {
                    let n = 1 + self.plan.below(removed.len() as u64) as usize;
                    self.reached.reappended += 1;
                    for entry in removed.into_iter().take(n) {
                        if !self.append_entry(entry.term, entry.kind, entry.file, entry.payload) {
                            break;
                        }
                        if self.plan.below(3) == 0 {
                            self.flush(false);
                            if self.log.is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            // A device that drops writes can take the log back behind what
            // the model holds.
            Err(LogError::Refused(_)) if self.device == Device::Lying => {}
            Err(e @ (LogError::Unreadable { .. } | LogError::Disk(_) | LogError::Damaged(_)))
                if !log.is_poisoned() =>
            {
                // Finding the entry failed before anything was written.
                assert_eq!(log.appended(), self.model.appended, "seed {seed}: {e}");
                self.fail();
            }
            Err(e) => {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                // The cut may or may not have happened: the entries from
                // `from` on may be found again, or not.
                if from <= durable_before {
                    self.model.cuts_attempted += 1;
                }
                for (index, entry) in self.model.acked.split_off(&from) {
                    self.model.maybe.entry(index).or_default().push(entry);
                }
                self.model.durable = self.model.durable.min(from - 1);
                self.model.before_advance = self.model.before_advance.min(from - 1);
                self.fail();
            }
        }
    }

    fn retire(&mut self) {
        let seed = self.seed;
        // A checkpoint at the committed position, which the next open
        // reads from, then the segments it makes unneeded.
        let c = self.model.committed.min(self.model.durable);
        let mut checkpoint = self.model.start;
        if c > checkpoint.index {
            match self.model.position(c) {
                Some(position) => checkpoint = position,
                None => assert_eq!(self.device, Device::Lying, "seed {seed}: no entry {c}"),
            }
        }
        // The checkpoint exists before the retirement that relies on it.
        self.model.start = checkpoint;
        let start = checkpoint.index;
        let log = self.log.as_mut().expect("open");
        let first = log.first_index();
        let retired = log.retire(start + 1);
        self.trace.push(stamp(
            &*self.machine,
            format!("retire before {}: {retired:?}", start + 1),
        ));
        let log = self.log.as_mut().expect("open");
        match retired {
            Ok(n) => {
                self.reached.retired += n;
                assert!(log.first_index() <= start + 1, "seed {seed}");
                assert!(log.first_index() >= first, "seed {seed}");
            }
            // A device that drops writes can take the log back behind the
            // checkpoint the model took.
            Err(LogError::Refused(_)) if self.device == Device::Lying => {}
            Err(e) => {
                self.trace
                    .push(stamp(&*self.machine, format!("retire: {e}")));
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    fn prepare(&mut self) {
        let seed = self.seed;
        let log = self.log.as_mut().expect("open");
        match log.prepare() {
            Ok(()) => assert!(log.spare_ready(), "seed {seed}"),
            Err(LogError::NoSpace(_)) => self.reached.no_space += 1,
            Err(LogError::Disk(e)) if !log.is_poisoned() => {
                assert_eq!(e.kind(), ErrorKind::Write, "seed {seed}");
            }
            Err(e) => {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    /// Read every durable entry back while the log runs.
    fn read_check(&mut self) {
        let seed = self.seed;
        let from = self.model.start.index + 1;
        self.machine.begin_reads();
        let log = self.log.as_ref().expect("open");
        let read: Result<Vec<Entry>, LogError> = log.read(from).and_then(|r| r.collect());
        match read {
            Ok(entries) if self.device == Device::Lying => {
                for entry in &entries {
                    assert!(
                        self.model
                            .ever
                            .get(&entry.index)
                            .is_some_and(|all| all.contains(entry)),
                        "seed {seed}: entry {} was never appended",
                        entry.index
                    );
                }
            }
            Ok(entries) => {
                let durable: Vec<&Entry> = self.model.acked.range(from..).map(|(_, e)| e).collect();
                assert_eq!(entries.iter().collect::<Vec<_>>(), durable, "seed {seed}");
            }
            Err(LogError::Unreadable { .. }) => self.reached.unreadable += 1,
            Err(LogError::Damaged(damage)) => {
                let path = format::segment_path(damage.segment);
                assert!(
                    self.device == Device::Lying || self.damaged_paths(0).contains(&path),
                    "seed {seed}: a read found {damage:?}, though the device never damaged {path}"
                );
                self.stop(false);
            }
            Err(LogError::Disk(e)) => {
                self.trace.push(stamp(&*self.machine, format!("read: {e}")));
                self.stop(false);
            }
            Err(e) => panic!("seed {seed}: read: {e}"),
        }
    }
}

/// The faults the log answers for.
const FAULTS: Rates = Rates {
    crash: 60,
    fail_flush: 25,
    fail_write: 40,
    no_space: 60,
    fail_read: 80,
    damage: 1500,
    misplace: 0,
    lose_write: 0,
    writeback: 10,
};

#[test]
fn the_log_survives_drawn_crashes_and_faults() {
    let mut reached = Reached::default();
    let mut ends = BTreeMap::new();
    for seed in seeds(250) {
        let mut machine = Simulated::new(seed, FAULTS);
        let end = Run::new(&mut machine, Device::Honest, seed, FAULTS, &mut reached).go(70);
        *ends.entry(format!("{end:?}")).or_insert(0u32) += 1;
    }
    // The schedules reach what they are meant to test.
    if seeds(250).len() >= 100 {
        reached.assert_fired(&FAULTS);
        for (what, n) in [
            ("kept tails", reached.kept_tails),
            ("open cuts", reached.open_cuts),
            ("cuts of durable entries", reached.cuts_of_durable),
            ("in-memory cuts", reached.in_memory_cuts),
            ("segment switches", reached.switches),
            ("retired segments", reached.retired),
            ("refused appends", reached.no_space),
            ("full batches", reached.batch_full),
            ("poisoned logs", reached.poisoned),
            ("cuts refused as unconfirmed", reached.unconfirmed),
            ("opens tried again with their state", reached.retried_opens),
            ("lost state slots", reached.lost_slots),
            ("tails held in the cache only", reached.tails_in_cache),
            (
                "opens that ended where a read failed",
                reached.unreadable_ends,
            ),
            ("unreadable ends given up", reached.given_up),
            (
                "unreadable places given up for good",
                reached.given_up_for_good,
            ),
            ("cuts past an unreadable end", reached.cut_past_unreadable),
            ("cut entries appended again", reached.reappended),
            ("damage reports", reached.damaged),
        ] {
            assert!(n > 0, "no {what}: {reached:?}");
        }
        assert!(ends.get("Open").copied().unwrap_or(0) > 150, "{ends:?}");
    }
    eprintln!("{ends:?} {reached:?}");
}

/// The drawn scenario on a device whose second read of the same bytes, in
/// one operation, differs from the first in one byte: no entry is confirmed
/// or returned whose bytes do not match its hash, and no field returned
/// differs from the bytes the hash was checked over (format section 3.11,
/// "Whatever is checked is what is used"). The log reads each place once,
/// so it never meets such a read; code that reads a place again and acts on
/// it fails the model's checks.
#[test]
fn the_log_acts_only_on_what_it_checked_when_a_second_read_differs() {
    let mut reached = Reached::default();
    let mut changed = 0;
    for seed in seeds(100) {
        let mut machine = Simulated::new(seed, FAULTS).with_rereads_changed(seed);
        Run::new(&mut machine, Device::Honest, seed, FAULTS, &mut reached).go(70);
        changed += machine.rereads_changed();
    }
    if seeds(100).len() >= 100 {
        assert!(reached.kept_tails > 0 && reached.opens > 100, "{reached:?}");
    }
    assert_eq!(
        changed, 0,
        "the log read the same bytes twice in one operation"
    );
}

#[test]
fn the_log_survives_a_crash_at_every_step() {
    let mut reached = Reached::default();
    for seed in seeds(3) {
        let steps = {
            let mut machine = Simulated::new(seed, Rates::default());
            let mut base = Reached::default();
            let end = Run::new(
                &mut machine,
                Device::Honest,
                seed,
                Rates::default(),
                &mut base,
            )
            .script(SCRIPT);
            assert_eq!(end, End::Open, "seed {seed}");
            assert!(
                base.switches > 0
                    && base.cuts_of_durable > 0
                    && base.in_memory_cuts > 0
                    && base.retired > 0,
                "seed {seed}: the script does not reach every operation: {base:?}"
            );
            machine.sim.steps()
        };
        for step in 1..=steps {
            for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                let mut machine = Simulated::new(seed ^ (step << 20), Rates::default());
                machine.sim.set_crash_fates(fates);
                machine.sim.place(step, Fault::Crash);
                let end = Run::new(
                    &mut machine,
                    Device::Honest,
                    seed,
                    Rates::default(),
                    &mut reached,
                )
                .script(SCRIPT);
                assert_eq!(
                    end,
                    End::Open,
                    "seed {seed}, crash at step {step}, {fates:?}"
                );
                assert_eq!(
                    machine.sim.fired().get(&Fault::Crash),
                    Some(&1),
                    "seed {seed}: the crash placed at step {step} did not fire"
                );
            }
        }
    }
    assert!(reached.kept_tails > 0, "{reached:?}");
}

#[test]
fn the_log_returns_only_appended_entries_from_a_lying_device() {
    let rates = Rates {
        misplace: 30,
        lose_write: 30,
        ..FAULTS
    };
    let mut reached = Reached::default();
    for seed in seeds(120) {
        let mut machine = Simulated::new(seed, rates);
        Run::new(&mut machine, Device::Lying, seed, rates, &mut reached).go(70);
    }
    if seeds(120).len() >= 100 {
        reached.assert_fired(&rates);
    }
}

#[test]
fn the_log_scenario_runs_on_the_real_file_system() {
    let mut reached = Reached::default();
    for seed in 0..3 {
        let mut machine = Real::new("log");
        let end = Run::new(
            &mut machine,
            Device::Honest,
            seed,
            Rates::default(),
            &mut reached,
        )
        .go(60);
        assert_eq!(end, End::Open);
        let mut machine = Real::new("log-script");
        let end = Run::new(
            &mut machine,
            Device::Honest,
            seed,
            Rates::default(),
            &mut reached,
        )
        .script(SCRIPT);
        assert_eq!(end, End::Open);
    }
    assert!(reached.switches > 0 && reached.cuts_of_durable > 0 && reached.retired > 0);
}

#[test]
fn a_seed_replays_exactly() {
    let run = |seed: u64| {
        let mut machine = Simulated::new(seed, FAULTS);
        Run::new(
            &mut machine,
            Device::Honest,
            seed,
            FAULTS,
            &mut Reached::default(),
        )
        .go(70);
        (machine.sim.events(), machine.sim.image())
    };
    let (events, image) = run(21);
    assert!(events.len() > 3, "{events:?}");
    assert_eq!(run(21), (events.clone(), image));
    assert_ne!(run(22).0, events);
}

/// A store on `sim`: `state` opened, or made where there is none, and the
/// log opened from `start`.
fn open_store(sim: &Sim, start: Position) -> Result<(Log, Opened), LogError> {
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let state = match State::open(&*fs, fs.lock().map_err(LogError::Disk)?) {
        Ok(state) => state,
        Err(StateError::Missing) => {
            State::create(&*fs, &sim.random(), fs.lock().map_err(LogError::Disk)?)
                .map_err(LogError::State)?
        }
        Err(e) => return Err(LogError::State(e)),
    };
    Log::open(fs, state, CONFIG, start).map_err(|failed| failed.error)
}

/// Append `batches` batches of `per` entries with payloads of `len` bytes,
/// flushing each.
fn fill(log: &mut Log, batches: usize, per: usize, len: usize) {
    for b in 0..batches {
        for i in 0..per {
            let payload = vec![(b * per + i) as u8; len];
            log.append(1, 2, 3, &payload).unwrap();
        }
        log.flush_batch(None).unwrap();
    }
}

/// Where a segment file's entries are, by the format's description
/// (`docs/server-storage-format.md` section 3) rather than the log's code:
/// the header's first index at bytes 48 to 56 of 128; batches from byte
/// 128, each a 136-byte header beginning `OMBATCH\0` with its entry count
/// at 12 to 16 and its first index at 16 to 24; entries of 64 bytes and
/// their payload, whose length is at 28 to 32. Returns, for each entry, its
/// index, offset and size, and for each batch its offset.
fn layout(bytes: &[u8]) -> (Vec<(u64, u64, u64)>, Vec<u64>) {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    assert_eq!(&bytes[0..8], b"OMLOGSG\0");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 1);
    let mut next = u64_at(48);
    let (mut entries, mut batches) = (Vec::new(), Vec::new());
    let mut at = 128usize;
    while at + 136 <= bytes.len() && &bytes[at..at + 8] == b"OMBATCH\0" && u64_at(at + 16) == next {
        batches.push(at as u64);
        let count = u32_at(at + 12);
        at += 136;
        for _ in 0..count {
            let size = 64 + u64::from(u32_at(at + 28));
            entries.push((next, at as u64, size));
            at += size as usize;
            next += 1;
        }
    }
    (entries, batches)
}

/// What the device holds of `path`.
fn device_bytes(sim: &Sim, path: &str) -> Vec<u8> {
    sim.image()
        .into_iter()
        .find(|(p, _)| p == path)
        .and_then(|(_, bytes)| bytes)
        .unwrap_or_else(|| panic!("{path} is not on the device"))
}

/// The log's files as the device holds them.
fn log_files(sim: &Sim) -> Vec<(String, Option<Vec<u8>>)> {
    sim.image()
        .into_iter()
        .filter(|(path, _)| path.starts_with(FOLDER))
        .collect()
}

/// The path of the last segment file on the device.
fn last_seg_path(sim: &Sim) -> String {
    sim.image()
        .into_iter()
        .filter(|(p, _)| p.starts_with("log/"))
        .map(|(p, _)| p)
        .max()
        .unwrap()
}

#[test]
fn a_batch_that_does_not_fit_continues_in_the_next_segment_as_a_second_batch() {
    let sim = Sim::new(3);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    // 1536-byte segments and entries of 364 bytes: a batch of two leaves
    // room for one more entry, not two.
    fill(&mut log, 1, 2, 300);
    fill(&mut log, 1, 2, 300);
    assert_eq!(log.segments().count, 2);
    let (first, first_batches) = layout(&device_bytes(&sim, &format::segment_path(1)));
    let (second, second_batches) = layout(&device_bytes(&sim, &format::segment_path(2)));
    // The second batch's first entry fit after the first batch; its second
    // went to segment 2 as a batch of its own, after the header that names
    // index 4 as the segment's first.
    assert_eq!(first.iter().map(|e| e.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(first_batches.len(), 2);
    assert_eq!(second.iter().map(|e| e.0).collect::<Vec<_>>(), vec![4]);
    assert_eq!(second_batches, vec![128]);
    // A batch too large for what is left of the current segment and the
    // whole next one is refused: the caller flushes first.
    let mut refused = false;
    for _ in 0..5 {
        match log.append(1, 0, 0, &[0; 500]) {
            Ok(_) => {}
            Err(LogError::BatchFull) => {
                refused = true;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(refused);
    log.flush_batch(None).unwrap();
    // Entries never span segments.
    for &(_, offset, size) in first.iter().chain(&second) {
        assert!(offset + size <= CONFIG.segment_size);
    }
    // The segment header names the store, its number and size, and the
    // entry before its first.
    let header = device_bytes(&sim, &format::segment_path(2));
    assert_eq!(&header[16..32], &log.state().values().server_id.0);
    assert_eq!(u64::from_le_bytes(header[32..40].try_into().unwrap()), 2);
    assert_eq!(
        u64::from_le_bytes(header[40..48].try_into().unwrap()),
        CONFIG.segment_size
    );
    let read: Vec<Entry> = log.read(1).unwrap().map(Result::unwrap).collect();
    assert_eq!(&header[64..96], &read[2].hash);
    assert_eq!(u64::from_le_bytes(header[56..64].try_into().unwrap()), 1);
}

#[test]
fn damage_through_the_marker_is_reported_where_it_is_and_nothing_is_cut() {
    // A log of several segments, its marker at its last entry.
    let build = || {
        let sim = Sim::new(4);
        let (mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, 8, 2, 300);
        assert!(log.segments().count >= 4, "{:?}", log.segments());
        let last = log.durable();
        drop(log);
        sim.restart();
        (sim, last)
    };
    let (sim, last) = build();
    let seg2 = format::segment_path(2);
    let (entries2, _) = layout(&device_bytes(&sim, &seg2));
    let first2 = entries2[0].0;
    let last_seg = last_seg_path(&sim);
    let (entries_last, _) = layout(&device_bytes(&sim, &last_seg));
    let marker_entry = *entries_last.last().unwrap();
    assert_eq!(marker_entry.0, last.index);

    type Harm = Box<dyn Fn(&Sim)>;
    let cases: Vec<(&str, Harm, DamageKind, u64, Option<u64>)> = vec![
        (
            "a byte of an entry in the middle of the log",
            Box::new(move |sim: &Sim| {
                sim.damage(&format::segment_path(2), entries2[0].1 + 70, 1)
                    .unwrap()
            }),
            DamageKind::EndsEarly,
            2,
            Some(first2),
        ),
        (
            "the marker's entry",
            Box::new(move |sim: &Sim| {
                sim.damage(&last_seg_path(sim), marker_entry.1 + 40, 2)
                    .unwrap()
            }),
            DamageKind::EndsEarly,
            0,
            Some(marker_entry.0),
        ),
        (
            "a segment's header",
            Box::new(|sim: &Sim| sim.damage(&format::segment_path(3), 50, 1).unwrap()),
            DamageKind::Header(HeaderFault::Hash),
            3,
            None,
        ),
        (
            "a listed segment's file",
            Box::new(|sim: &Sim| {
                let fs = sim.fs();
                fs.remove(&format::segment_path(2)).unwrap();
                fs.flush_folder(FOLDER).unwrap();
            }),
            DamageKind::Missing,
            2,
            None,
        ),
    ];
    for (what, harm, kind, segment, index) in cases {
        let (sim, _) = build();
        harm(&sim);
        let files = log_files(&sim);
        let fs: Arc<dyn Fs> = Arc::new(sim.fs());
        let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
        let marker = state.values().marker;
        match Log::open(fs, state, CONFIG, Position::START).map_err(|failed| *failed) {
            Err(OpenFailed {
                error: LogError::Damaged(damage),
                state,
            }) => {
                assert_eq!(damage.kind, kind, "{what}: {damage:?}");
                if segment != 0 {
                    assert_eq!(damage.segment, segment, "{what}: {damage:?}");
                }
                if let Some(index) = index {
                    assert_eq!(damage.index, index, "{what}: {damage:?}");
                }
                // The state comes back, its marker standing.
                assert_eq!(state.values().marker, marker, "{what}");
            }
            other => panic!("{what}: {:?}", other.map(|(_, opened)| opened)),
        }
        // Nothing was cut.
        assert_eq!(log_files(&sim), files, "{what}");
    }

    // The same damage before the entry a checkpoint covers is still found
    // when the walk passes it, and not when it does not.
    let (sim, last) = build();
    sim.damage(
        &format::segment_path(1),
        SEGMENT_HEADER + BATCH_HEADER + 70,
        1,
    )
    .unwrap();
    let (log, opened) = open_store(&sim, last).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(opened.durable, last);
    drop(log);
}

#[test]
fn what_follows_the_marker_is_kept_when_whole_and_cut_when_not() {
    // A batch written whole whose marker was never recorded: the power
    // failed as `state` was written, and kept nothing unflushed.
    let build = |damaged: bool| {
        let sim = Sim::new(5);
        let (mut log, _) = open_store(&sim, Position::START).unwrap();
        // The tail fits in the current segment (a part in a new segment
        // that `state` never listed is not part of the log).
        fill(&mut log, 2, 2, 100);
        let marker = log.durable();
        for i in 0..3u8 {
            log.append(2, 0, 0, &[i; 50]).unwrap();
        }
        let tail = log.appended();
        // The batch's synchronous write, then state's write.
        sim.set_crash_fates(Fates::LoseAll);
        sim.place(sim.steps() + 2, Fault::Crash);
        assert!(log.flush_batch(None).is_err());
        assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
        assert!(log.is_poisoned());
        drop(log);
        sim.restart();
        if damaged {
            let path = last_seg_path(&sim);
            let (entries, _) = layout(&device_bytes(&sim, &path));
            let middle = entries.iter().find(|e| e.0 == marker.index + 2).unwrap();
            sim.damage(&path, middle.1 + 40, 1).unwrap();
        }
        (sim, marker, tail)
    };

    let (sim, marker, tail) = build(false);
    let (log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.kept, tail.index - marker.index);
    assert_eq!(opened.durable, tail);
    assert_eq!(log.state().values().marker.index, tail.index);
    assert_eq!(log.read(1).unwrap().count() as u64, tail.index);
    drop(log);

    let (sim, marker, _) = build(true);
    let (log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, marker);
    let cut = opened.cut.expect("the damaged batch is cut");
    assert_eq!(cut.index, marker.index + 1);
    // The cut holds: after a power failure that keeps nothing unflushed, the
    // next open finds the same end.
    drop(log);
    sim.crash(Fates::LoseAll);
    sim.restart();
    let (mut log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, marker);
    assert_eq!(opened.cut.map(|c| c.index), Some(marker.index + 1));
    log.append(3, 0, 0, b"after the cut").unwrap();
    let after = log.flush_batch(None).unwrap();
    assert_eq!(after.index, marker.index + 1);
    let read: Vec<Entry> = log.read(1).unwrap().map(Result::unwrap).collect();
    assert_eq!(read.last().unwrap().payload, b"after the cut");
}

#[test]
fn a_full_disk_refuses_the_append_before_anything_is_written() {
    let sim = Sim::new(6);
    // Room for `state`, a segment and the next one, not a third.
    sim.set_capacity(2 * crate::state::SLOT as u64 + 2 * CONFIG.segment_size + 600);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    let mut refused = None;
    for i in 0..40u32 {
        let before = (log.appended(), log.batch_len());
        match log.append(1, 0, 0, &[i as u8; 300]) {
            Ok(_) => {
                log.flush_batch(None).unwrap();
            }
            Err(LogError::NoSpace(_)) => {
                assert_eq!((log.appended(), log.batch_len()), before);
                assert!(!log.is_poisoned());
                refused = Some(before.0);
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let last = refused.expect("the disk filled");
    // What was durable reads back. A checkpoint at the last entry lets the
    // first segment go, which makes room for the next.
    assert_eq!(log.read(1).unwrap().count() as u64, last.index);
    assert!(log.segments().count >= 2);
    let retired = log.retire(last.index + 1).unwrap();
    assert!(retired > 0);
    log.append(1, 0, 0, &[9; 300]).unwrap();
    assert_eq!(log.flush_batch(None).unwrap().index, last.index + 1);

    // The sixth review's second finding: a segment number whose run `state`
    // could not list is refused the same way, before anything is written,
    // and the log goes on. First a run that starts at the largest number,
    // then one whose next segment would be it.
    for first in [u64::MAX, u64::MAX - 1] {
        let sim = Sim::new(6);
        let fs: Arc<dyn Fs> = Arc::new(sim.fs());
        let mut state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
        state
            .update(|v| v.segments = Segments { first, count: 0 })
            .unwrap();
        let (mut log, _) = Log::open(fs, state, CONFIG, Position::START).unwrap();
        let mut flushed = Position::START;
        let refused = loop {
            let before = (log.appended(), log.batch_len());
            match log.append(1, 0, 0, &[7; 300]) {
                Ok(_) if before.1 == 1 => flushed = log.flush_batch(None).unwrap(),
                Ok(_) => {}
                Err(LogError::Refused(_)) => break before,
                Err(e) => panic!("{e}"),
            }
        };
        assert!(!log.is_poisoned());
        assert_eq!((log.appended(), log.batch_len()), refused);
        assert!(matches!(log.prepare(), Err(LogError::Refused(_))));
        assert_eq!(log.flush_batch(None).unwrap(), refused.0);
        assert_eq!(flushed == Position::START, first == u64::MAX);
        drop(log);
        sim.restart();
        let (_, opened) = open_store(&sim, Position::START).unwrap();
        assert_eq!(opened.durable, refused.0);
    }
}

#[test]
fn a_failed_write_or_flush_stops_the_log() {
    for fault in [Fault::FailWrite, Fault::FailFlush] {
        let sim = Sim::new(7);
        let (mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, 2, 2, 50);
        log.append(1, 0, 0, b"x").unwrap();
        sim.place(sim.steps() + 1, fault);
        assert!(log.flush_batch(None).is_err(), "{fault:?}");
        assert_eq!(sim.fired().get(&fault), Some(&1));
        assert!(log.is_poisoned());
        assert!(matches!(log.append(1, 0, 0, b""), Err(LogError::Poisoned)));
        assert!(matches!(log.flush_batch(None), Err(LogError::Poisoned)));
        assert!(matches!(log.cut(5), Err(LogError::Poisoned)));
        assert!(matches!(log.retire(1), Err(LogError::Poisoned)));
        assert!(matches!(log.prepare(), Err(LogError::Poisoned)));
        assert!(matches!(
            log.update_state(|v| v.term += 1),
            Err(LogError::Poisoned)
        ));
        assert!(log.read(1).is_err());
        drop(log);
        // The store opens again; the entries reported durable are there.
        sim.restart();
        let (log, opened) = open_store(&sim, Position::START).unwrap();
        assert!(opened.durable.index >= 4, "{fault:?}: {opened:?}");
        assert_eq!(log.read(1).unwrap().count() as u64, opened.durable.index);
    }

    // The sixth review's second finding: a `state` update refused after the
    // batch was written (here the copy number at its limit, which nothing
    // checks before) stops the log as a failed write does.
    let sim = Sim::new(7);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    fill(&mut log, 1, 1, 10);
    drop(log);
    sim.restart();
    set_copy_numbers(&sim, u64::MAX - 2);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    assert_eq!(log.state().copy().0, u64::MAX);
    log.append(1, 0, 0, b"x").unwrap();
    assert!(matches!(
        log.flush_batch(None),
        Err(LogError::State(StateError::Refused(_)))
    ));
    assert!(log.is_poisoned());
    assert!(matches!(log.append(1, 0, 0, b""), Err(LogError::Poisoned)));
    assert!(matches!(log.flush_batch(None), Err(LogError::Poisoned)));
}

/// Give the newest copy of `state` on `sim` the copy number `newest`, and
/// the other `newest - 1`, by the format's description
/// (`docs/server-storage-format.md` 1.1): the number at bytes 16 to 24 of a
/// copy, the body's length at 12 to 16, and SHA-256 over the 24 bytes
/// before the body and the body, after it.
fn set_copy_numbers(sim: &Sim, newest: u64) {
    let fs = sim.fs();
    let file = fs.open(crate::state::FILE, FileOptions::PLAIN).unwrap();
    let slot = crate::state::SLOT;
    let mut copies: Vec<(u64, Vec<u8>)> = (0..2)
        .map(|i| {
            let mut bytes = vec![0u8; slot];
            file.read_at((i * slot) as u64, &mut bytes).unwrap();
            (u64::from_le_bytes(bytes[16..24].try_into().unwrap()), bytes)
        })
        .collect();
    let newer = usize::from(copies[1].0 > copies[0].0);
    for (i, (_, bytes)) in copies.iter_mut().enumerate() {
        let number = if i == newer { newest } else { newest - 1 };
        bytes[16..24].copy_from_slice(&number.to_le_bytes());
        let end = 24 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let hash = digest::digest(&digest::SHA256, &bytes[..end]);
        bytes[end..end + 32].copy_from_slice(hash.as_ref());
        file.write_at((i * slot) as u64, bytes).unwrap();
    }
    file.flush().unwrap();
}

#[test]
fn an_open_that_repairs_its_tail_survives_a_crash_at_every_step() {
    // A batch of one entry whose write failed: whole in the system's cache
    // and not on the device, or torn. The process stops without a power
    // failure, so the next open reads the cache.
    let setup = |seed: u64| {
        let sim = Sim::new(seed);
        let (mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, 2, 2, 30);
        let marker = log.durable();
        let tail = log.append(1, 0, 0, &[7; 4]).unwrap();
        sim.place(sim.steps() + 1, Fault::FailWrite);
        assert!(log.flush_batch(None).is_err());
        drop(log);
        let whole = sim.events().iter().rev().find_map(|e| match e {
            sim::Event::WriteFailed { written, .. } => {
                Some(*written == BATCH_HEADER + ENTRY_OVERHEAD + 4)
            }
            _ => None,
        });
        sim.restart();
        (sim, marker, tail, whole.expect("the write failed"))
    };
    let mut cases = BTreeMap::new();
    for seed in 0..3000 {
        let (_, _, _, whole) = setup(seed);
        cases.entry(whole).or_insert_with(Vec::new).push(seed);
        if cases.values().all(|seeds| seeds.len() >= 2) && cases.len() == 2 {
            break;
        }
    }
    assert_eq!(cases.len(), 2, "no write left the batch whole in the cache");
    let mut kept = 0;
    for (whole, seeds) in cases {
        for seed in seeds.into_iter().take(2) {
            let steps = {
                let (sim, _, tail, _) = setup(seed);
                let begin = sim.steps();
                let (_, opened) = open_store(&sim, Position::START).unwrap();
                assert_eq!(opened.durable == tail, whole, "seed {seed}");
                sim.steps() - begin
            };
            for step in 1..=steps {
                for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                    let (sim, marker, tail, _) = setup(seed);
                    sim.set_crash_fates(fates);
                    sim.place(sim.steps() + step, Fault::Crash);
                    assert!(open_store(&sim, Position::START).is_err());
                    assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
                    sim.restart();
                    let (log, opened) = open_store(&sim, Position::START)
                        .unwrap_or_else(|e| panic!("seed {seed}, step {step}, {fates:?}: {e}"));
                    assert!(
                        opened.durable == marker || (whole && opened.durable == tail),
                        "seed {seed}, step {step}, {fates:?}: {opened:?}"
                    );
                    kept += u32::from(opened.durable == tail);
                    let read: Vec<Entry> = log.read(1).unwrap().map(Result::unwrap).collect();
                    assert_eq!(read.len() as u64, opened.durable.index);
                }
            }
        }
    }
    assert!(kept > 0);
}

#[test]
fn a_cut_never_reaches_committed_or_checkpointed_entries() {
    let sim = Sim::new(8);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    fill(&mut log, 4, 2, 20);
    log.set_committed(2).unwrap();
    for from in [1, 2] {
        assert!(matches!(log.cut(from), Err(LogError::Refused(_))));
    }
    let at4: Entry = log.read(4).unwrap().next().unwrap().unwrap();
    drop(log);
    // Opened from a checkpoint at entry 4, whose committed position the
    // marker records lower: the checkpoint's entries are committed all the
    // same.
    sim.restart();
    let (mut log, _) = open_store(&sim, at4.position()).unwrap();
    assert_eq!(log.committed(), 2);
    log.set_committed(2).unwrap();
    for from in [3, 4] {
        assert!(matches!(log.cut(from), Err(LogError::Refused(_))));
    }
    // A retirement that no longer needs entry 6 raises the floor to 6.
    log.retire(7).unwrap();
    assert!(matches!(log.cut(6), Err(LogError::Refused(_))));
    assert_eq!(log.durable().index, 8);
    assert_eq!(log.cut(7).unwrap().last.index, 6);
    // A `state` this log did not write, whose committed position is past
    // its marker: nothing is repaired, nothing breaks, and the cut is
    // refused.
    let mut state = log.into_state();
    state.update(|v| v.marker.committed = u64::MAX).unwrap();
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let (mut log, opened) = Log::open(fs, state, CONFIG, at4.position()).unwrap();
    assert_eq!(opened.recorded_committed, u64::MAX);
    log.set_committed(6).unwrap();
    log.flush_batch(Some(6)).unwrap();
    assert_eq!(log.committed(), u64::MAX);
    assert!(matches!(log.cut(6), Err(LogError::Refused(_))));
    assert!(!log.is_poisoned());
}

/// Entries 1, 2 and 3, each flushed as a batch of its own, entry 1
/// committed: the store of the cut tests.
fn three_batches(sim: &Sim) -> (Log, Vec<Position>) {
    let (mut log, _) = open_store(sim, Position::START).unwrap();
    let mut entries = Vec::new();
    for i in 1..=3u8 {
        entries.push(log.append(1, 0, 0, &[i; 40]).unwrap());
        log.flush_batch(None).unwrap();
    }
    log.set_committed(1).unwrap();
    (log, entries)
}

/// The first review's first sequence: after a cut from 2 and the same entry
/// 2 appended again, byte for byte, the batch of entry 3 written before the
/// cut follows it on disk and chains from it. It is not part of the log.
#[test]
fn a_cut_holds_when_the_cut_entries_are_appended_again() {
    let sim = Sim::new(9);
    let (mut log, entries) = three_batches(&sim);
    assert_eq!(log.cut(2).unwrap().last, entries[0]);
    assert_eq!(log.state().values().cut_count, 1);
    let again = log.append(1, 0, 0, &[2; 40]).unwrap();
    assert_eq!(again, entries[1]);
    log.flush_batch(None).unwrap();
    drop(log);
    for power_loss in [false, true] {
        if power_loss {
            sim.crash(Fates::LoseAll);
        }
        sim.restart();
        let (log, opened) = open_store(&sim, Position::START).unwrap();
        assert_eq!(opened.durable, entries[1], "power loss: {power_loss}");
        assert_eq!(opened.kept, 0);
        assert!(opened.cut.is_some(), "the batch of entry 3 is cut");
        assert_eq!(log.read(1).unwrap().count(), 2);
        drop(log);
    }
    // A batch written after the cut, whose marker the power failure lost,
    // carries the current cut count: it is kept.
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    let three = log.append(2, 0, 0, b"a new entry 3").unwrap();
    sim.set_crash_fates(Fates::LoseAll);
    sim.place(sim.steps() + 2, Fault::Crash);
    assert!(log.flush_batch(None).is_err());
    assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
    drop(log);
    sim.restart();
    let (_, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.kept, 1);
    assert_eq!(opened.durable, three);
}

/// A cut is done once the first slot of its `state` update is flushed: a
/// crash at any step of it leaves the cut undone or done, never entry 2
/// back after that point; and nothing a cut removed comes back after a stop
/// and a power loss, whatever the system's cache held (the first review's
/// second and seventh findings).
#[test]
fn a_cut_is_done_once_state_records_it() {
    let steps = {
        let sim = Sim::new(10);
        let (mut log, _) = three_batches(&sim);
        let begin = sim.steps();
        log.cut(2).unwrap();
        sim.steps() - begin
    };
    // The cut removes no file here, so its last five steps are `state`'s
    // two writes and flushes, then the flush of `log/`.
    let first_flushed = steps - 3;
    for step in 1..=steps {
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            let sim = Sim::new(10);
            let (mut log, entries) = three_batches(&sim);
            sim.set_crash_fates(fates);
            sim.place(sim.steps() + step, Fault::Crash);
            assert!(log.cut(2).is_err());
            assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
            drop(log);
            sim.restart();
            let (log, opened) = open_store(&sim, Position::START).unwrap();
            let done = opened.durable == entries[0];
            assert!(
                done || opened.durable == entries[2],
                "step {step}, {fates:?}: {opened:?}"
            );
            if step > first_flushed {
                assert!(done, "step {step}, {fates:?}: the cut was undone");
            }
            assert_eq!(opened.cut_count, u64::from(done));
            drop(log);
            // A stop and a power loss bring nothing back.
            sim.restart();
            let (log, again) = open_store(&sim, Position::START).unwrap();
            drop(log);
            sim.crash(Fates::LoseAll);
            sim.restart();
            let (_, last) = open_store(&sim, Position::START).unwrap();
            assert_eq!(again.durable, opened.durable);
            assert_eq!(last.durable, opened.durable);
        }
    }
    // The second slot's write fails: the log stops, and the cut is done.
    let sim = Sim::new(10);
    let (mut log, entries) = three_batches(&sim);
    sim.place(sim.steps() + steps - 2, Fault::FailWrite);
    assert!(log.cut(2).is_err());
    assert_eq!(sim.fired().get(&Fault::FailWrite), Some(&1));
    assert!(log.is_poisoned());
    drop(log);
    sim.restart();
    let (log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, entries[0]);
    drop(log);
    sim.crash(Fates::LoseAll);
    sim.restart();
    let (_, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, entries[0]);
}

/// The first review's third sequence: the committed position the marker
/// records goes back one advance when the newest copy of `state` is lost,
/// so a cut waits for the caller to give it again.
#[test]
fn a_cut_waits_for_a_committed_position_given_since_the_open() {
    let sim = Sim::new(11);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    for i in 1..=4u8 {
        log.append(1, 0, 0, &[i; 10]).unwrap();
        log.flush_batch((i == 4).then_some(1)).unwrap();
    }
    let at1: Entry = log.read(1).unwrap().next().unwrap().unwrap();
    log.set_committed(4).unwrap();
    let (_, newest) = log.state().copy();
    drop(log);
    sim.damage(
        crate::state::FILE,
        (newest * crate::state::SLOT + 40) as u64,
        2,
    )
    .unwrap();
    sim.restart();
    let (mut log, opened) = open_store(&sim, at1.position()).unwrap();
    assert_eq!(opened.durable.index, 4);
    assert_eq!(opened.recorded_committed, 1);
    assert!(!log.committed_confirmed());
    assert!(matches!(log.cut(2), Err(LogError::CommittedUnconfirmed)));
    // A committed position is never past the log's own entries: one past
    // the last entry a flush makes durable, or past the durable entry, is
    // refused with nothing written, and the log goes on.
    let five = log.append(1, 0, 0, &[5; 10]).unwrap();
    assert!(matches!(
        log.flush_batch(Some(6)),
        Err(LogError::Refused(_))
    ));
    assert!(matches!(log.set_committed(5), Err(LogError::Refused(_))));
    assert!(!log.is_poisoned() && !log.committed_confirmed());
    assert_eq!((log.durable().index, log.batch_len()), (4, 1));
    assert_eq!(log.flush_batch(Some(4)).unwrap(), five);
    assert_eq!(log.committed(), 4);
    log.set_committed(5).unwrap();
    assert!(matches!(log.cut(2), Err(LogError::Refused(_))));
    assert_eq!(log.durable().index, 5);
}

/// The first review's fourth sequence: one store has one log. A second
/// `Log::open` needs the `State` the first one owns, and a second `State`
/// needs the lock it holds; a log whose process has stopped writes
/// nothing.
#[test]
fn one_store_has_one_log() {
    let sim = Sim::new(12);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    log.prepare().unwrap();
    assert_eq!(sim.fs().lock().unwrap_err().kind(), ErrorKind::Locked);
    assert!(open_store(&sim, Position::START).is_err());
    sim.restart();
    // The first entry needs a segment, which it would write.
    assert!(matches!(
        log.append(1, 0, 0, b"x"),
        Err(LogError::Refused(_))
    ));
    assert!(matches!(
        log.flush_batch(Some(1)),
        Err(LogError::Refused(_))
    ));
    assert!(matches!(log.set_committed(1), Err(LogError::Refused(_))));
    assert!(matches!(log.prepare(), Err(LogError::Refused(_))));
    assert!(matches!(
        log.update_state(|v| v.term = 1),
        Err(LogError::Refused(_))
    ));
    drop(log);
    // The term and vote change through the log; its own fields do not.
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    log.update_state(|v| v.term = 3).unwrap();
    assert_eq!(log.state().values().term, 3);
    assert!(matches!(
        log.update_state(|v| v.cut_count += 1),
        Err(LogError::Refused(_))
    ));
    assert!(matches!(
        log.update_state(|v| v.marker.committed += 1),
        Err(LogError::Refused(_))
    ));
    assert_eq!(log.into_state().values().term, 3);
}

/// The first review's fifth sequence: an entry whose fixed fields declare a
/// payload past the format's bound is not valid, and nothing is read for
/// it, however large its segment says it is; one past the configured bound
/// and within the format's is read.
#[test]
fn an_entry_declaring_a_huge_payload_is_refused_before_anything_is_read_for_it() {
    let folder = TempFolder::new("log-huge");
    let watched = Arc::new(Watched::new(Arc::new(folder.fs())));
    let fs: Arc<dyn Fs> = watched.clone();
    let open = |config: Config| {
        let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
        Log::open(fs.clone(), state, config, Position::START).unwrap()
    };
    // Written under a larger configuration: a segment of 4096 bytes and an
    // entry of 1500.
    let larger = Config {
        segment_size: 4096,
        max_payload: 2000,
    };
    let state = State::create(&*fs, &OsRandom::new(), fs.lock().unwrap()).unwrap();
    let (mut log, _) = Log::open(fs.clone(), state, larger, Position::START).unwrap();
    let one = log.append(1, 0, 0, &[1; 1500]).unwrap();
    log.flush_batch(None).unwrap();
    let store = log.state().values().server_id.0;
    drop(log);
    let (log, opened) = open(CONFIG);
    assert_eq!(opened.durable, one);
    let read: Vec<Entry> = log.read(1).unwrap().map(Result::unwrap).collect();
    assert_eq!(read[0].payload.len(), 1500);
    drop(log);

    // Segment 1 made a gigabyte long (a sparse file), with a batch after
    // entry 1 whose entry declares a payload of most of it.
    let file = fs
        .open(&format::segment_path(1), FileOptions::PLAIN)
        .unwrap();
    let size = MAX_SEGMENT;
    file.write_at(size - 1, &[0]).unwrap();
    let header = format::SegmentHeader {
        store,
        number: 1,
        size,
        first: 1,
        prev_term: 0,
        prev_hash: ZERO_HASH,
    };
    file.write_at(0, &header.encode()).unwrap();
    let at = SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD + 1500;
    let payload = size - at - BATCH_HEADER - ENTRY_OVERHEAD;
    let batch = format::BatchHeader {
        count: 1,
        first: 2,
        length: ENTRY_OVERHEAD + payload,
        cut_count: 0,
        prev: one.hash,
        last: [0; 32],
    };
    let mut fixed = [0u8; 32];
    fixed[0..8].copy_from_slice(&2u64.to_le_bytes());
    fixed[8..16].copy_from_slice(&1u64.to_le_bytes());
    fixed[28..32].copy_from_slice(&(payload as u32).to_le_bytes());
    file.write_at(at, &batch.encode()).unwrap();
    file.write_at(at + BATCH_HEADER, &fixed).unwrap();
    drop(file);
    watched.largest_read.store(0, Ordering::Relaxed);
    let (log, opened) = open(CONFIG);
    assert_eq!(opened.durable, one);
    assert_eq!(opened.cut.map(|c| c.index), Some(2));
    assert_eq!(log.read(1).unwrap().count(), 1);
    let largest = watched.largest_read.load(Ordering::Relaxed);
    assert!(largest <= 64 * 1024, "a read of {largest} bytes");
}

/// The first review's sixth sequence: indexes at the top of the range.
#[test]
fn indexes_at_the_top_of_the_range() {
    let sim = Sim::new(13);
    let start = Position {
        index: MAX_INDEX - 2,
        term: 5,
        hash: [7; 32],
    };
    let (mut log, opened) = open_store(&sim, start).unwrap();
    assert_eq!(opened.durable, start);
    assert_eq!(log.first_index(), MAX_INDEX - 1);
    log.append(5, 0, 0, b"a").unwrap();
    let last = log.append(5, 0, 0, b"b").unwrap();
    assert_eq!(last.index, MAX_INDEX);
    assert!(matches!(
        log.append(5, 0, 0, b"c"),
        Err(LogError::Refused(_))
    ));
    log.flush_batch(None).unwrap();
    drop(log);
    sim.restart();
    let (log, opened) = open_store(&sim, start).unwrap();
    assert_eq!(opened.durable, last);
    assert_eq!(log.read(u64::MAX).unwrap().count(), 0);
    let read: Vec<Entry> = log.read(MAX_INDEX).unwrap().map(Result::unwrap).collect();
    assert_eq!(read.len(), 1);
    assert_eq!(log.first_index(), MAX_INDEX - 1);
    drop(log);
    sim.restart();
    let past = Position {
        index: u64::MAX,
        ..start
    };
    assert!(matches!(open_store(&sim, past), Err(LogError::Refused(_))));
    // The batch mark, read as an index, is past the bound, so no entry can
    // begin with it: the fifth review's case, a log opened from two before
    // it, cannot be built. Nor can a log whose marker is past the bound be
    // opened.
    let mark = u64::from_le_bytes(*b"OMBATCH\0");
    assert!(mark > MAX_INDEX);
    for index in [mark - 2, MAX_INDEX + 1] {
        sim.restart();
        let from = Position { index, ..start };
        assert!(matches!(open_store(&sim, from), Err(LogError::Refused(_))));
    }
    sim.restart();
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let mut state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    state.update(|v| v.marker.index = MAX_INDEX + 1).unwrap();
    let opened = Log::open(fs, state, CONFIG, start).map_err(|failed| failed.error);
    assert!(matches!(opened, Err(LogError::Refused(_))));
}

/// The second review's first finding: an open whose flush of the store
/// folder fails can leave `log/` made only in the system's cache. An open
/// tried again with the `state` handed back, in the same process, finds the
/// folder; it must make its name durable all the same before a segment is
/// listed in it.
#[test]
fn an_open_tried_again_after_its_folder_flush_failed_makes_log_durable() {
    let mut tried = 0;
    for seed in 0..200 {
        let sim = Sim::new(seed);
        let fs: Arc<dyn Fs> = Arc::new(sim.fs());
        let state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
        // The open lists `log/`, makes it, then flushes the store folder.
        sim.place(sim.steps() + 1, Fault::FailFlush);
        let failed = *Log::open(fs.clone(), state, CONFIG, Position::START).unwrap_err();
        assert!(
            matches!(failed.error, LogError::Disk(_)),
            "{}",
            failed.error
        );
        let left = sim.events().iter().rev().find_map(|e| match e {
            sim::Event::FlushFailed { path, settled, .. } if path.is_empty() => {
                Some(settled.pending)
            }
            _ => None,
        });
        if left != Some(1) {
            continue;
        }
        tried += 1;
        let (mut log, _) = Log::open(fs.clone(), failed.state, CONFIG, Position::START)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let one = log.append(1, 0, 0, b"one").unwrap();
        log.flush_batch(None).unwrap();
        drop(log);
        sim.crash(Fates::LoseAll);
        sim.restart();
        let (_, opened) =
            open_store(&sim, Position::START).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(opened.durable, one, "seed {seed}");
        if tried == 3 {
            break;
        }
    }
    assert!(tried > 0, "no failed flush left `log/` unflushed");
}

/// What follows the firm position is judged by what its batch header
/// settles before anything else is read, and each read is of what the
/// record being decided on takes, so that a place the device cannot read is
/// met only by the record it belongs to. A read that fails after the firm
/// position ends the log for that open only: nothing is cut or raised, and
/// appends wait for an open that reads past it. At or before the firm
/// position a read that fails is the caller's to see.
#[test]
fn an_unreadable_place_after_the_firm_position_ends_the_log_for_that_open() {
    let sim = Sim::new(15);
    let watched = Arc::new(Watched::new(Arc::new(sim.fs())));
    let fs: Arc<dyn Fs> = watched.clone();
    let big = Config {
        segment_size: 1 << 20,
        max_payload: 300 << 10,
    };
    let open = || {
        let lock = fs.lock().unwrap();
        let state = match State::open(&*fs, lock) {
            Ok(state) => state,
            Err(StateError::Missing) => {
                State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap()
            }
            Err(e) => panic!("{e}"),
        };
        Log::open(fs.clone(), state, big, Position::START)
    };
    let (mut log, _) = open().unwrap();
    let one = log.append(1, 0, 0, b"one").unwrap();
    log.flush_batch(None).unwrap();
    log.append(1, 0, 0, &vec![2; 256 << 10]).unwrap();
    log.flush_batch(None).unwrap();
    log.set_committed(1).unwrap();
    assert_eq!(log.cut(2).unwrap().last, one);
    drop(log);
    sim.restart();
    let second = SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD + 3;
    let payload = second + BATCH_HEADER + format::ENTRY_FIXED;
    let seg1 = format::segment_path(1);

    // The batch written before the cut is judged by its header alone, with
    // a place its payload holds that cannot be read: near its start, and
    // deep inside.
    for bad in [payload + 8 * 1024, payload + 200_000] {
        watched.bad.lock().unwrap().push((seg1.clone(), bad));
        let (log, opened) = open().unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert_eq!(opened.durable, one);
        assert_eq!(opened.cut.map(|c| c.why), Some(CutWhy::OlderCut));
        assert_eq!(opened.unreadable_end, None);
        drop(log);
        sim.restart();
        watched.bad.lock().unwrap().clear();
    }

    // A batch of the current count whose payload cannot be read, its marker
    // never recorded, ends the log for this open: nothing is cut, the cut
    // count stays, and appends wait.
    let (mut log, _) = open().unwrap();
    let again = log.append(2, 0, 0, &vec![3; 256 << 10]).unwrap();
    sim.set_crash_fates(Fates::LoseAll);
    sim.place(sim.steps() + 2, Fault::Crash);
    assert!(log.flush_batch(None).is_err());
    assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
    drop(log);
    sim.restart();
    watched
        .bad
        .lock()
        .unwrap()
        .push((seg1.clone(), payload + 200_000));
    let (mut log, opened) = open().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(opened.durable, one);
    assert_eq!(opened.cut, None);
    assert_eq!(opened.cut_count, 1);
    let end = opened
        .unreadable_end
        .expect("the log ends where it could not read");
    assert_eq!((end.segment, end.index), (1, 2));
    assert!(matches!(
        log.append(2, 0, 0, b"x"),
        Err(LogError::EndUnreadable(_))
    ));
    drop(log);
    // An open that reads past it takes the batch as any whole tail.
    sim.restart();
    watched.bad.lock().unwrap().clear();
    let (mut log, opened) = open().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(opened.durable, again);
    assert_eq!(opened.kept, 1);
    assert_eq!(opened.unreadable_end, None);

    // At or before the firm position, the open fails and says so.
    let three = log.append(2, 0, 0, b"three").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    sim.restart();
    watched.bad.lock().unwrap().push((
        seg1,
        SEGMENT_HEADER + BATCH_HEADER + format::ENTRY_FIXED + 1,
    ));
    match open() {
        Err(failed) => assert!(
            matches!(failed.error, LogError::Unreadable { segment: 1, .. }),
            "{}",
            failed.error
        ),
        Ok((_, opened)) => panic!("{opened:?}"),
    }
    watched.bad.lock().unwrap().clear();
    let (_, opened) = open().unwrap();
    assert_eq!(opened.durable, three);
}

/// The final review's second finding: a confirmed batch whose marker
/// advance was lost with the newest copy of `state`, and which the device
/// fails to read once, is not lost: the open ends the log before it for
/// that open only, and the next open that can read it takes it back.
#[test]
fn a_confirmed_batch_behind_a_lost_marker_and_a_failed_read_comes_back() {
    let sim = Sim::new(17);
    let watched = Arc::new(Watched::new(Arc::new(sim.fs())));
    let fs: Arc<dyn Fs> = watched.clone();
    let big = Config {
        segment_size: 1 << 20,
        max_payload: 300 << 10,
    };
    let state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
    let (mut log, _) = Log::open(fs.clone(), state, big, Position::START).unwrap();
    let one = log.append(1, 0, 0, b"one").unwrap();
    log.flush_batch(None).unwrap();
    let two = log.append(1, 0, 0, &vec![2; 256 << 10]).unwrap();
    log.flush_batch(None).unwrap();
    let (_, newest) = log.state().copy();
    drop(log);
    sim.damage(
        crate::state::FILE,
        (newest * crate::state::SLOT + 40) as u64,
        2,
    )
    .unwrap();
    sim.restart();
    let bad = SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD + 3 + BATCH_HEADER + 100_000;
    watched
        .bad
        .lock()
        .unwrap()
        .push((format::segment_path(1), bad));
    let open = || {
        let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
        Log::open(fs.clone(), state, big, Position::START).unwrap()
    };
    let (mut log, opened) = open();
    assert_eq!(opened.durable, one);
    assert!(opened.unreadable_end.is_some());
    assert!(matches!(
        log.append(2, 0, 0, b"x"),
        Err(LogError::EndUnreadable(_))
    ));
    drop(log);
    sim.restart();
    watched.bad.lock().unwrap().clear();
    let (log, opened) = open();
    assert_eq!(opened.durable, two);
    assert_eq!(log.read(1).unwrap().count(), 2);
}

/// A batch whole in the system's cache only, after a write the device did
/// not finish.
fn tail_in_cache(seed: u64) -> (Sim, Position, Position, bool) {
    let sim = Sim::new(seed);
    let (mut log, _) = open_store(&sim, Position::START).unwrap();
    fill(&mut log, 2, 2, 30);
    let marker = log.durable();
    let tail = log.append(1, 0, 0, &[7; 4]).unwrap();
    sim.place(sim.steps() + 1, Fault::FailWrite);
    assert!(log.flush_batch(None).is_err());
    drop(log);
    let whole = sim.events().iter().rev().find_map(|e| match e {
        sim::Event::WriteFailed { written, .. } => {
            Some(*written == BATCH_HEADER + ENTRY_OVERHEAD + 4)
        }
        _ => None,
    });
    sim.restart();
    (sim, marker, tail, whole.expect("the write failed"))
}

/// The final review's first finding: a tail an open finds whole in the
/// system's cache only is written again before the marker moves over it.
/// A fault at any step of that open, then an open tried again with the
/// same `State` or by a new process, then a power loss: whatever the
/// opens confirmed is there.
#[test]
fn a_tail_held_only_in_the_cache_is_written_again_before_the_marker_covers_it() {
    let seeds: Vec<u64> = (0..3000)
        .filter(|&seed| tail_in_cache(seed).3)
        .take(2)
        .collect();
    assert!(
        !seeds.is_empty(),
        "no write left the batch whole in the cache"
    );
    for seed in seeds {
        let steps = {
            let (sim, _, tail, _) = tail_in_cache(seed);
            let begin = sim.steps();
            let (_, opened) = open_store(&sim, Position::START).unwrap();
            assert_eq!(opened.durable, tail, "seed {seed}");
            sim.steps() - begin
        };
        for step in 1..=steps {
            for fault in [Fault::FailFlush, Fault::FailWrite, Fault::FailRead] {
                for fresh in [false, true] {
                    let (sim, marker, tail, _) = tail_in_cache(seed);
                    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
                    sim.place(sim.steps() + step, fault);
                    let what = format!("seed {seed}, step {step}, {fault:?}, fresh {fresh}");
                    let state = State::open(&*fs, fs.lock().unwrap());
                    let opened = match state {
                        Ok(state) => match Log::open(fs.clone(), state, CONFIG, Position::START) {
                            Ok((_, opened)) => Some(opened),
                            Err(failed) if !fresh => {
                                // Tried again with the state handed back.
                                let OpenFailed { state, .. } = *failed;
                                Log::open(fs.clone(), state, CONFIG, Position::START)
                                    .ok()
                                    .map(|(_, opened)| opened)
                            }
                            Err(_) => None,
                        },
                        Err(_) => None,
                    };
                    let confirmed = if let Some(opened) = opened {
                        opened.durable
                    } else {
                        // A new process, the cache kept, until one opens (the
                        // fault placed may fire in the first of them).
                        let mut opened = None;
                        for _ in 0..3 {
                            sim.restart();
                            if let Ok((_, o)) = open_store(&sim, Position::START) {
                                opened = Some(o);
                                break;
                            }
                        }
                        opened.unwrap_or_else(|| panic!("{what}: no open")).durable
                    };
                    sim.crash(Fates::LoseAll);
                    let mut last = Err(LogError::Poisoned);
                    for _ in 0..3 {
                        sim.restart();
                        last = open_store(&sim, Position::START);
                        if last.is_ok() {
                            break;
                        }
                    }
                    let (log, opened) = last.unwrap_or_else(|e| panic!("{what}: {e}"));
                    assert!(
                        opened.durable.index >= confirmed.index,
                        "{what}: {:?} after {confirmed:?}",
                        opened.durable
                    );
                    assert!(opened.durable == marker || opened.durable == tail, "{what}");
                    assert_eq!(log.read(1).unwrap().count() as u64, opened.durable.index);
                }
            }
        }
    }
}

/// A batch header after the end whose index and cut count fit but which
/// does not name the last entry as the one before it is not a batch:
/// nothing past it is read, and nothing is cut.
#[test]
fn a_batch_header_that_does_not_chain_is_passed_over_unread() {
    let sim = Sim::new(16);
    let watched = Arc::new(Watched::new(Arc::new(sim.fs())));
    let fs: Arc<dyn Fs> = watched.clone();
    let big = Config {
        segment_size: 1 << 20,
        max_payload: 300 << 10,
    };
    let state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
    let (mut log, _) = Log::open(fs.clone(), state, big, Position::START).unwrap();
    let one = log.append(1, 0, 0, b"one").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    sim.restart();
    let payload = 200u64 << 10;
    let at = SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD + 3;
    let header = format::BatchHeader {
        count: 1,
        first: 2,
        length: ENTRY_OVERHEAD + payload,
        cut_count: 0,
        prev: [9; 32],
        last: [0; 32],
    };
    let mut fixed = [0u8; 32];
    fixed[0..8].copy_from_slice(&2u64.to_le_bytes());
    fixed[8..16].copy_from_slice(&1u64.to_le_bytes());
    fixed[28..32].copy_from_slice(&(payload as u32).to_le_bytes());
    let file = fs
        .open(&format::segment_path(1), FileOptions::PLAIN)
        .unwrap();
    file.write_at(at, &header.encode()).unwrap();
    file.write_at(at + BATCH_HEADER, &fixed).unwrap();
    drop(file);
    watched.largest_read.store(0, Ordering::Relaxed);
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    let (_, opened) = Log::open(fs.clone(), state, big, Position::START).unwrap();
    assert_eq!(opened.durable, one);
    assert_eq!(opened.cut, None);
    assert!(watched.largest_read.load(Ordering::Relaxed) <= 64 * 1024);
}

/// Segments of 1024 bytes, for the cases of the end rule.
const SMALL: Config = Config {
    segment_size: 1024,
    max_payload: 600,
};

/// A store on `sim` seen through `watched`: `state` opened, or made where
/// there is none, and the log opened from the start with `config`.
fn open_watched(
    sim: &Sim,
    watched: &Arc<Watched>,
    config: Config,
) -> Result<(Log, Opened), Box<OpenFailed>> {
    open_watched_from(sim, watched, config, Position::START)
}

/// As [`open_watched`], the log opened from `start`.
fn open_watched_from(
    sim: &Sim,
    watched: &Arc<Watched>,
    config: Config,
    start: Position,
) -> Result<(Log, Opened), Box<OpenFailed>> {
    let fs: Arc<dyn Fs> = watched.clone();
    let state = match State::open(&*fs, fs.lock().unwrap()) {
        Ok(state) => state,
        Err(StateError::Missing) => State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap(),
        Err(e) => panic!("{e}"),
    };
    Log::open(fs, state, config, start)
}

/// A log of 1024-byte segments written in `batches` (each a list of
/// payload lengths), every batch's marker recorded but, unless `recorded`,
/// the last one's: its write is on the device and the power failed before
/// `state` was updated. A first batch whose marker is lost would be in a
/// segment `state` never listed, so then an entry is written and cut
/// first: segment 1 is listed, the cut count is 1, and the batches begin
/// at its first place. Returns the machine and every entry appended.
fn small_log(
    seed: u64,
    batches: &[&[usize]],
    recorded: bool,
) -> (Sim, Arc<Watched>, Vec<Position>) {
    let sim = Sim::new(seed);
    let watched = Arc::new(Watched::new(Arc::new(sim.fs())));
    let (mut log, _) = open_watched(&sim, &watched, SMALL).unwrap();
    if batches.len() == 1 && !recorded {
        log.append(1, 0, 0, b"cut").unwrap();
        log.flush_batch(None).unwrap();
        log.set_committed(0).unwrap();
        log.cut(1).unwrap();
    }
    let mut entries = Vec::new();
    for (i, batch) in batches.iter().enumerate() {
        for &len in *batch {
            entries.push(log.append(1, 0, 0, &vec![7; len]).unwrap());
        }
        if i + 1 == batches.len() && !recorded {
            sim.set_crash_fates(Fates::LoseAll);
            sim.place(sim.steps() + 2, Fault::Crash);
            assert!(log.flush_batch(None).is_err());
            assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
        } else {
            log.flush_batch(None).unwrap();
        }
    }
    drop(log);
    sim.restart();
    sim.set_crash_fates(Fates::Drawn);
    (sim, watched, entries)
}

/// The third review's fourth finding: where the log ends is decided from
/// what must be there, and a read fails only where a decision needs the
/// bytes it could not read (format section 3.6). The review's segment of
/// 1024 bytes with byte 512 unreadable: after a whole batch whose last
/// entry ends at 494, nothing is read past the next header's mark.
#[test]
fn the_end_is_decided_by_what_must_be_there() {
    let seg1 = format::segment_path(1);
    let open = |sim: &Sim, watched: &Arc<Watched>, bad: &[u64]| {
        *watched.bad.lock().unwrap() = bad.iter().map(|&at| (seg1.clone(), at)).collect();
        open_watched(sim, watched, SMALL)
    };
    for recorded in [true, false] {
        // Two entries in one batch, payloads of 102 and 0 bytes: the second
        // from 430 to 494. Nothing looks for a header in its place past its
        // fixed bytes, and nothing past 494 is read but the next mark.
        let (sim, watched, entries) = small_log(41, &[&[102, 0]], recorded);
        let (layout, _) = layout(&device_bytes(&sim, &seg1));
        assert_eq!(&layout[1..], &[(2, 430, 64)]);
        let (log, opened) = open(&sim, &watched, &[512]).unwrap();
        assert_eq!(opened.durable, entries[1], "recorded: {recorded}");
        assert_eq!(opened.kept, if recorded { 0 } else { 2 });
        assert_eq!((opened.unreadable_end, opened.cut), (None, None));
        assert_eq!(log.read(1).unwrap().map(Result::unwrap).count(), 2);
        drop(log);

        // One entry of 166 bytes of payload, ending at 494.
        let (sim, watched, entries) = small_log(42, &[&[166]], recorded);
        let (log, opened) = open(&sim, &watched, &[512]).unwrap();
        assert_eq!(opened.durable, entries[0], "recorded: {recorded}");
        assert_eq!((opened.unreadable_end, opened.cut), (None, None));
        drop(log);
        sim.restart();
        // The next header's mark itself cannot be read: the log ends there
        // for this open, at the first byte of the read that failed.
        let (log, opened) = open(&sim, &watched, &[496]).unwrap();
        assert_eq!(opened.durable, entries[0]);
        assert_eq!(
            opened.unreadable_end,
            Some(UnreadableEnd {
                segment: 1,
                offset: 494,
                index: 2
            })
        );
        drop(log);
    }

    // Fewer bytes left than a header and one empty entry: no batch can
    // begin, and nothing is read.
    let (sim, watched, entries) = small_log(43, &[&[600]], false);
    let (_, opened) = open(&sim, &watched, &[930]).unwrap();
    assert_eq!(opened.durable, entries[0]);
    assert_eq!((opened.unreadable_end, opened.cut), (None, None));

    // Entries 1 and 2 as batches of their own, then a cut from 2: entry 2's
    // batch, at 338, carries the older cut count. Its header's start shows
    // that, so the rest of it is not read.
    let (sim, watched, entries) = small_log(44, &[&[10], &[10]], true);
    let (mut log, _) = open(&sim, &watched, &[]).unwrap();
    log.set_committed(1).unwrap();
    assert_eq!(log.cut(2).unwrap().last, entries[0]);
    drop(log);
    sim.restart();
    let (log, opened) = open(&sim, &watched, &[338 + 100]).unwrap();
    assert_eq!(opened.durable, entries[0]);
    assert_eq!(opened.cut.map(|c| c.why), Some(CutWhy::OlderCut));
    assert_eq!(opened.unreadable_end, None);
    drop(log);
    sim.restart();
    // A batch written there after the cut, its marker lost: a header of the
    // current count, whose hash cannot be read, decides nothing.
    let (mut log, _) = open(&sim, &watched, &[]).unwrap();
    log.append(2, 0, 0, &[8; 10]).unwrap();
    sim.set_crash_fates(Fates::LoseAll);
    sim.place(sim.steps() + 2, Fault::Crash);
    assert!(log.flush_batch(None).is_err());
    drop(log);
    sim.restart();
    let (_, opened) = open(&sim, &watched, &[338 + 100]).unwrap();
    assert_eq!(opened.durable, entries[0]);
    assert_eq!(opened.cut, None);
    assert_eq!(
        opened.unreadable_end,
        Some(UnreadableEnd {
            segment: 1,
            offset: 338 + format::BATCH_START,
            index: 2
        })
    );
}

/// Entry 1 durable, entry 2's batch at 338 written and its marker lost,
/// and byte 340, in that batch's mark, unreadable: the open ends the log
/// after entry 1.
fn unreadable_after_one(seed: u64) -> (Sim, Arc<Watched>, Position, Log, Opened) {
    let (sim, watched, entries) = small_log(seed, &[&[10], &[10]], false);
    watched
        .bad
        .lock()
        .unwrap()
        .push((format::segment_path(1), 340));
    let (log, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, entries[0]);
    assert!(opened.unreadable_end.is_some());
    (sim, watched, entries[0], log, opened)
}

/// The third review's fifth point and the review's third answer: what may
/// follow an unreadable end is given up with the last entry the open
/// verified, and no committed position. The log continues in a fresh
/// segment, so the next open does not stop at that place again though it
/// stays unreadable, and what followed never comes back.
#[test]
fn giving_up_an_unreadable_end_fences_what_may_follow() {
    let (sim, watched, one, mut log, opened) = unreadable_after_one(51);
    assert!(matches!(
        log.give_up_unreadable_end(one.index, [0; 32]),
        Err(LogError::Refused(_))
    ));
    assert!(matches!(
        log.give_up_unreadable_end(2, one.hash),
        Err(LogError::Refused(_))
    ));
    assert!(!log.committed_confirmed());
    let done = log.give_up_unreadable_end(one.index, one.hash).unwrap();
    assert_eq!(
        done,
        CutDone {
            last: one,
            unflushed: 0,
            durable: 0,
            cut_count: Some(1),
            unreadable_end: opened.unreadable_end,
            fence: Some(2),
            removed: Vec::new(),
        }
    );
    assert_eq!(log.segments().count, 2);
    assert!(matches!(
        log.give_up_unreadable_end(one.index, one.hash),
        Err(LogError::Refused(_))
    ));
    let two = log.append(2, 0, 0, b"two").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    for bad in [true, false] {
        if !bad {
            watched.bad.lock().unwrap().clear();
            sim.crash(Fates::LoseAll);
        }
        sim.restart();
        let (log, opened) = open_watched(&sim, &watched, SMALL).unwrap();
        assert_eq!(opened.durable, two, "the place unreadable: {bad}");
        assert_eq!((opened.unreadable_end, opened.cut), (None, None));
        assert_eq!(opened.cut_count, 1);
        let read: Vec<Position> = log
            .read(1)
            .unwrap()
            .map(|e| e.unwrap().position())
            .collect();
        assert_eq!(read, [one, two]);
        drop(log);
    }
    // Nothing to give up on a log that ends where it can read.
    sim.restart();
    let (mut log, _) = open_watched(&sim, &watched, SMALL).unwrap();
    assert!(matches!(
        log.give_up_unreadable_end(two.index, two.hash),
        Err(LogError::Refused(_))
    ));
    drop(log);
    // A device that drops the fresh segment's header write leaves the space
    // the segment was made with, zeros: `state` lists a segment with no
    // valid header, which is damage, and the open changes nothing.
    sim.restart();
    let fs: Arc<dyn Fs> = watched.clone();
    let file = fs
        .open(&format::segment_path(2), FileOptions::PLAIN)
        .unwrap();
    file.write_at(0, &[0; SEGMENT_HEADER as usize]).unwrap();
    file.flush().unwrap();
    drop(file);
    let changes = watched.changes.load(Ordering::Relaxed);
    match open_watched(&sim, &watched, SMALL) {
        Err(failed) => assert!(
            matches!(
                failed.error,
                LogError::Damaged(Damage {
                    kind: DamageKind::Header(HeaderFault::Magic),
                    segment: 2,
                    ..
                })
            ),
            "{}",
            failed.error
        ),
        Ok((_, opened)) => panic!("{opened:?}"),
    }
    assert_eq!(watched.changes.load(Ordering::Relaxed), changes);
}

/// Giving up is done once the first slot of its `state` update is flushed:
/// a crash at any step of it leaves it undone (the open ends unreadable
/// again, and it can be given up again) or done, never anything else.
#[test]
fn giving_up_survives_a_crash_at_every_step() {
    let steps = {
        let (sim, _, one, mut log, _) = unreadable_after_one(52);
        let begin = sim.steps();
        log.give_up_unreadable_end(one.index, one.hash).unwrap();
        sim.steps() - begin
    };
    let mut done_after = 0;
    for step in 1..=steps {
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            let (sim, watched, one, mut log, _) = unreadable_after_one(52);
            sim.set_crash_fates(fates);
            sim.place(sim.steps() + step, Fault::Crash);
            assert!(log.give_up_unreadable_end(one.index, one.hash).is_err());
            assert_eq!(sim.fired().get(&Fault::Crash), Some(&2));
            drop(log);
            sim.restart();
            let what = format!("step {step}, {fates:?}");
            let (mut log, opened) =
                open_watched(&sim, &watched, SMALL).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(opened.durable, one, "{what}");
            let done = opened.unreadable_end.is_none();
            assert_eq!(opened.cut_count, u64::from(done), "{what}");
            // The update's second slot: written after the first one's flush
            // returned.
            if step > steps - 2 {
                assert!(done, "{what}: given up, then undone");
            }
            if done {
                done_after += 1;
            } else {
                log.give_up_unreadable_end(one.index, one.hash)
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
            }
            let two = log.append(2, 0, 0, b"two").unwrap();
            log.flush_batch(None).unwrap();
            drop(log);
            sim.crash(Fates::LoseAll);
            sim.restart();
            let (_, opened) = open_watched(&sim, &watched, SMALL).unwrap();
            assert_eq!(opened.durable, two, "{what}");
            assert_eq!(opened.unreadable_end, None, "{what}");
        }
    }
    assert!(done_after > 0);
}

/// The fourth review's fourth point and the fifth's second: on a log that
/// ends unreadable, a cut from the entry after the durable one gives up
/// what may follow, and says so; one further on removes nothing, since a
/// cut never removes anything before its index, and the end stays
/// unreadable; one at or before the durable entry ends the log before the
/// place.
#[test]
fn a_cut_past_an_unreadable_end_gives_up_what_may_follow() {
    let (sim, watched, one, mut log, opened) = unreadable_after_one(53);
    assert!(matches!(log.cut(2), Err(LogError::CommittedUnconfirmed)));
    // No committed position past the durable entry is taken.
    assert!(matches!(log.set_committed(2), Err(LogError::Refused(_))));
    assert!(!log.is_poisoned());
    assert!(!log.committed_confirmed());
    log.set_committed(1).unwrap();
    for further in [3, 5] {
        assert_eq!(log.cut(further).unwrap(), CutDone::nothing(one));
        assert!(matches!(
            log.append(2, 0, 0, b"x"),
            Err(LogError::EndUnreadable(_))
        ));
    }
    let done = log.cut(2).unwrap();
    assert_eq!(done.last, one);
    assert_eq!(done.cut_count, Some(1));
    assert_eq!(done.unreadable_end, opened.unreadable_end);
    assert_eq!(done.fence, Some(2));
    let two = log.append(2, 0, 0, b"two").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    sim.restart();
    let (_, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, two);
    assert_eq!(opened.unreadable_end, None);
    // Entries 1 and 2 durable, 3's batch past the marker unreadable: a cut
    // from 2 ends the log before the place; the next open stops at entry
    // 2's batch, of an older cut count, before it reaches the place.
    let (sim, watched, entries) = small_log(54, &[&[10], &[10], &[10]], false);
    let bad = (format::segment_path(1), 548 + 2);
    watched.bad.lock().unwrap().push(bad);
    let (mut log, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, entries[1]);
    assert!(opened.unreadable_end.is_some());
    log.set_committed(1).unwrap();
    let done = log.cut(2).unwrap();
    assert_eq!(
        (done.last, done.durable, done.cut_count, done.fence),
        (entries[0], 1, Some(1), None)
    );
    assert_eq!(done.unreadable_end, opened.unreadable_end);
    log.append(2, 0, 0, b"x").unwrap();
    drop(log);
    sim.restart();
    let (_, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, entries[0]);
    assert_eq!(opened.unreadable_end, None);
}

/// The fifth review's first finding: a retirement shortens the run from the
/// front, and giving up an unreadable end in the last segment is accepted
/// after it. Three segments of 1024 bytes, entries 1 to 6 two to a segment,
/// entry 7's batch past the marker in the third, and its mark unreadable.
#[test]
fn an_unreadable_end_is_given_up_after_a_retirement() {
    let batch: &[usize] = &[100];
    let (sim, watched, entries) = small_log(
        55,
        &[batch, batch, batch, batch, batch, batch, &[10]],
        false,
    );
    watched
        .bad
        .lock()
        .unwrap()
        .push((format::segment_path(3), 728 + 2));
    let (mut log, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, entries[5]);
    assert_eq!(
        opened.unreadable_end.map(|e| (e.segment, e.offset)),
        Some((3, 728))
    );
    assert_eq!(log.segments().count, 3);
    // A checkpoint at entry 4: segments 1 and 2 are no longer needed.
    let four = entries[3];
    assert_eq!(log.retire(5).unwrap(), 2);
    let done = log
        .give_up_unreadable_end(entries[5].index, entries[5].hash)
        .unwrap();
    assert_eq!(done.fence, Some(4));
    assert_eq!(log.segments(), Segments { first: 3, count: 2 });
    let seven = log.append(2, 0, 0, b"seven").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    sim.restart();
    let (log, opened) = open_watched_from(&sim, &watched, SMALL, four).unwrap();
    assert_eq!(opened.durable, seven);
    assert_eq!(opened.unreadable_end, None);
    let read: Vec<Position> = log
        .read(5)
        .unwrap()
        .map(|e| e.unwrap().position())
        .collect();
    assert_eq!(read, [entries[4], entries[5], seven]);
}

/// The fifth review's third finding: a place given up is read again once
/// a later cut of durable entries takes the log back into its segment. What
/// was written before that cut never returns; the place is read like any
/// other: where the log's end falls on it, the open ends unreadable again
/// and it is given up again; where the log has grown over it, an open that
/// cannot read it at or before the marker fails `Unreadable`, and one that
/// can reads every entry. The test's device keeps failing reads of the
/// place after it is written, until the test says otherwise; a real device
/// takes a write over a failed sector.
#[test]
fn a_place_given_up_is_read_again_after_a_cut_back_into_its_segment() {
    // Entries 1 and 2 in segment 1, the second ending at 548, where the
    // next batch's mark cannot be read.
    let (sim, watched, entries) = small_log(56, &[&[10], &[10]], true);
    let (one, two) = (entries[0], entries[1]);
    let seg1 = format::segment_path(1);
    let set_bad = |bad: bool| {
        *watched.bad.lock().unwrap() = if bad {
            vec![(seg1.clone(), 548)]
        } else {
            vec![]
        };
    };
    let reopen = || {
        sim.restart();
        open_watched(&sim, &watched, SMALL)
    };
    let read_all = |log: &Log| -> Vec<Position> {
        log.read(1)
            .unwrap()
            .map(|e| e.unwrap().position())
            .collect()
    };
    set_bad(true);
    let (mut log, opened) = open_watched(&sim, &watched, SMALL).unwrap();
    assert_eq!(opened.durable, two);
    assert!(opened.unreadable_end.is_some());
    log.give_up_unreadable_end(two.index, two.hash).unwrap();
    let three = log.append(1, 0, 0, b"three").unwrap();
    log.flush_batch(None).unwrap();

    // A cut from 2 drops the fresh segment and continues at 338, in
    // segment 1. Entry 2 again, of the same size: the log's end falls on
    // the place again.
    log.set_committed(1).unwrap();
    let done = log.cut(2).unwrap();
    assert_eq!((done.last, done.fence, done.removed), (one, None, vec![2]));
    let two_b = log.append(2, 0, 0, &[2; 10]).unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    let (mut log, opened) = reopen().unwrap();
    assert_eq!(opened.durable, two_b);
    assert_eq!(
        opened.unreadable_end.map(|e| (e.segment, e.offset)),
        Some((1, 548))
    );
    log.give_up_unreadable_end(two_b.index, two_b.hash).unwrap();
    let three_b = log.append(2, 0, 0, b"three again").unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    let (mut log, opened) = reopen().unwrap();
    assert_eq!((opened.durable, opened.cut_count), (three_b, 3));
    assert_eq!(read_all(&log), [one, two_b, three_b]);

    // Cut back again, and a batch that covers the place.
    log.set_committed(1).unwrap();
    log.cut(2).unwrap();
    let two_c = log.append(3, 0, 0, &[3; 200]).unwrap();
    log.flush_batch(None).unwrap();
    drop(log);
    match reopen() {
        Err(failed) => assert!(
            matches!(failed.error, LogError::Unreadable { segment: 1, .. }),
            "{}",
            failed.error
        ),
        Ok((_, opened)) => panic!("{opened:?}"),
    }
    set_bad(false);
    let (log, opened) = reopen().unwrap();
    assert_eq!((opened.durable, opened.cut_count), (two_c, 4));
    assert_eq!(opened.unreadable_end, None);
    assert_eq!(read_all(&log), [one, two_c]);
    assert!(three.index == 3 && three_b.index == 3);
}

/// What a byte of a segment file belongs to, for the sweep below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A segment's header: every entry of the segment, from `first`.
    SegmentHeader(u64),
    /// A batch's header: every entry of the batch, from `first`.
    BatchHeader(u64),
    Entry(u64),
}

impl Owner {
    fn index(self) -> u64 {
        match self {
            Owner::SegmentHeader(i) | Owner::BatchHeader(i) | Owner::Entry(i) => i,
        }
    }
}

/// A small log on the simulated disk, laid out byte by byte: its files,
/// its entries, and what each byte of its listed segments belongs to (by
/// the format's description, sections 3.2 to 3.4; the log has no cut).
struct Laid {
    config: Config,
    state: Vec<u8>,
    segments: Vec<(String, Vec<u8>)>,
    entries: Vec<(Position, Vec<u8>)>,
    /// Per segment, the byte ranges and what they belong to.
    owners: Vec<Vec<(usize, usize, Owner)>>,
}

impl Laid {
    fn new() -> Laid {
        let sim = Sim::new(80);
        let fs: Arc<dyn Fs> = Arc::new(sim.fs());
        let state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
        let config = Config {
            segment_size: 1024,
            max_payload: 64,
        };
        let (mut log, _) = Log::open(fs.clone(), state, config, Position::START).unwrap();
        let mut entries = Vec::new();
        for batch in 0..5u8 {
            for i in 0..3u8 {
                let payload = vec![batch * 3 + i + 1; 32];
                let at = log.append(1, 1, 0, &payload).unwrap();
                entries.push((at, payload));
            }
            let last = entries.last().unwrap().0.index;
            log.flush_batch(Some(last)).unwrap();
        }
        let run = log.segments();
        drop(log);
        sim.restart();
        let image = sim.image();
        let bytes = |path: &str| {
            image
                .iter()
                .find(|(p, _)| p == path)
                .and_then(|(_, b)| b.clone())
                .unwrap()
        };
        let segments: Vec<(String, Vec<u8>)> = (run.first..run.first + run.count)
            .map(|n| (segment_path(n), bytes(&segment_path(n))))
            .collect();
        let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let mut owners = Vec::new();
        for (_, b) in &segments {
            let mut own = vec![(0, 128, Owner::SegmentHeader(u64_at(b, 48)))];
            let mut at = 128;
            while at + 136 <= b.len() && &b[at..at + 8] == b"OMBATCH\0" {
                let count = u32::from_le_bytes(b[at + 12..at + 16].try_into().unwrap());
                let first = u64_at(b, at + 16);
                own.push((at, at + 136, Owner::BatchHeader(first)));
                let mut entry = at + 136;
                for _ in 0..count {
                    let len = u32::from_le_bytes(b[entry + 28..entry + 32].try_into().unwrap());
                    let size = 64 + len as usize;
                    own.push((entry, entry + size, Owner::Entry(u64_at(b, entry))));
                    entry += size;
                }
                at = entry;
            }
            owners.push(own);
        }
        let held: usize = owners
            .iter()
            .flatten()
            .filter(|(_, _, o)| matches!(o, Owner::Entry(_)))
            .count();
        assert_eq!(held, entries.len(), "every entry laid out once");
        Laid {
            config,
            state: bytes(crate::state::FILE),
            segments,
            entries,
            owners,
        }
    }

    /// The segment and offset where entry `index` begins.
    fn place_of(&self, index: u64) -> (usize, usize) {
        for (seg, own) in self.owners.iter().enumerate() {
            for &(start, _, owner) in own {
                if owner == Owner::Entry(index) {
                    return (seg, start);
                }
            }
        }
        panic!("entry {index} laid out")
    }

    /// What bytes `from..to` of segment `seg` belong to.
    fn touched(&self, seg: usize, from: usize, to: usize) -> Vec<Owner> {
        self.owners[seg]
            .iter()
            .filter(|(start, end, _)| *start < to && from < *end)
            .map(|(_, _, owner)| *owner)
            .collect()
    }

    /// A fresh disk holding the log, segment `seg` changed by `harm`.
    fn disk(&self, seg: usize, harm: impl Fn(&mut Vec<u8>)) -> Sim {
        let sim = Sim::new(81);
        let fs = sim.fs();
        fs.create_root().unwrap();
        fs.create_folder(FOLDER).unwrap();
        let write = |path: &str, bytes: &[u8]| {
            let file = fs.create(path, FileOptions::PLAIN).unwrap();
            file.write_at(0, bytes).unwrap();
            file.flush().unwrap();
        };
        write(crate::state::FILE, &self.state);
        for (at, (path, bytes)) in self.segments.iter().enumerate() {
            let mut bytes = bytes.clone();
            if at == seg {
                harm(&mut bytes);
            }
            write(path, &bytes);
        }
        fs.flush_folder(FOLDER).unwrap();
        fs.flush_folder("").unwrap();
        sim
    }
}

/// The entries from `from` the log reads back, by index, with their
/// payloads.
fn read_back(log: &Log, from: u64) -> Vec<(u64, Vec<u8>)> {
    log.read(from)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.index, e.payload)
        })
        .collect()
}

/// One case of the sweep: segment `seg` of `laid` damaged over `from..to`
/// by `harm`, the log opened from entry `start`. What it checks is the
/// sweep's doc; a failure says the case.
fn damaged_case(
    laid: &Laid,
    start: u64,
    seg: usize,
    from: usize,
    to: usize,
    harm: impl Fn(&mut Vec<u8>),
) -> &'static str {
    let case = format!("start {start}, segment {seg}, bytes {from}..{to}");
    let last = laid.entries.last().unwrap().0;
    let start_at = if start == 0 {
        Position::START
    } else {
        laid.entries[start as usize - 1].0
    };
    let config = laid.config;
    let want = |from: u64, through: u64| -> Vec<(u64, Vec<u8>)> {
        laid.entries
            .iter()
            .filter(|(p, _)| p.index >= from && p.index <= through)
            .map(|(p, payload)| (p.index, payload.clone()))
            .collect()
    };
    let touched = laid.touched(seg, from, to);
    let successor = laid.place_of(start + 1);
    // Wholly before the entry after the start; begun before it.
    let before = (seg, to) <= successor;
    let begun_before = (seg, from) < successor;
    // The first entry after the start the damage reaches.
    let first_after = touched
        .iter()
        .map(|o| o.index())
        .filter(|&i| i > start)
        .min();
    let sim = laid.disk(seg, harm);
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let damaged: Vec<(String, Vec<u8>)> = sim
        .image()
        .into_iter()
        .filter(|(p, _)| p.starts_with("log/"))
        .map(|(p, b)| (p, b.unwrap()))
        .collect();
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    match Log::open(fs.clone(), state, config, start_at) {
        Ok((log, opened)) => {
            // It opens with everything, and reports damage it walked past.
            assert!(
                first_after.is_none(),
                "{case}: opened though entry {first_after:?} after the start is damaged"
            );
            assert_eq!(opened.durable, last, "{case}: opened short");
            assert_eq!(
                read_back(&log, start + 1),
                want(start + 1, last.index),
                "{case}"
            );
            let walked = opened.walked_before_start.map(|n| {
                laid.segments
                    .iter()
                    .position(|(p, _)| *p == segment_path(n))
                    .unwrap()
            });
            let reported = touched
                .iter()
                .filter_map(|o| match o {
                    Owner::Entry(i) if *i <= start && walked == Some(seg) => Some(*i),
                    _ => None,
                })
                .min();
            assert_eq!(
                opened.before_start.map(|f| f.index),
                reported,
                "{case}: the chain before the start"
            );
            "opened"
        }
        Err(failed) => {
            assert!(
                matches!(failed.error, LogError::Damaged(_)),
                "{case}: {}",
                failed.error
            );
            let OpenFailed { state, .. } = *failed;
            drop(state);
            sim.restart();
            let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
            let planned = Log::plan_damage(&*fs, &state, config, start_at);
            match planned {
                Err(refused) => {
                    // A refusal is allowed for damage that begins before the
                    // entry after the start; it changes nothing.
                    assert!(
                        begun_before,
                        "{case}: refused ({refused}) damage after the start"
                    );
                    let now: Vec<(String, Vec<u8>)> = sim
                        .image()
                        .into_iter()
                        .filter(|(p, _)| p.starts_with("log/"))
                        .map(|(p, b)| (p, b.unwrap()))
                        .collect();
                    assert!(now == damaged, "{case}: the plan changed the log");
                    "refused"
                }
                Ok(plan) => {
                    let (log, opened, accepted) =
                        Log::accept_damage(fs.clone(), state, config, start_at, &plan)
                            .unwrap_or_else(|e| panic!("{case}: {}", e.failed.error));
                    // The effect is the plan: what it keeps, the run, where
                    // the open ends; and that open removed nothing.
                    assert_eq!(accepted.plan, plan, "{case}: the plan carried out");
                    assert_eq!(log.segments(), plan.listed, "{case}: the run");
                    assert_eq!(opened.durable, plan.opens_at, "{case}: where it ends");
                    assert!(opened.removed.is_empty(), "{case}: the open removed files");
                    // Wholly before the entry after the start: nothing after
                    // the start given up. Reaching it or past it: kept every
                    // entry before the first the damage reaches.
                    let kept = if before {
                        last.index
                    } else {
                        first_after.map_or(last.index, |t| t - 1)
                    };
                    assert_eq!(accepted.kept.index, kept, "{case}: kept");
                    assert_eq!(accepted.marker, last.index, "{case}");
                    // Nothing removed: every file's bytes are still in the
                    // log's folder, under its own name or the one given.
                    let now: Vec<Vec<u8>> = sim
                        .image()
                        .into_iter()
                        .filter(|(p, _)| p.starts_with("log/"))
                        .filter_map(|(_, b)| b)
                        .collect();
                    for (path, bytes) in &damaged {
                        assert!(now.contains(bytes), "{case}: {path} lost");
                    }
                    drop(log);
                    // The next open agrees, and reads back what was kept.
                    sim.restart();
                    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
                    let (log, opened) = Log::open(fs.clone(), state, config, start_at)
                        .unwrap_or_else(|e| panic!("{case}: {}", e.error));
                    assert_eq!(opened.durable.index, kept, "{case}: opened after");
                    assert!(
                        opened.removed.is_empty(),
                        "{case}: the next open removed files"
                    );
                    assert_eq!(read_back(&log, start + 1), want(start + 1, kept), "{case}");
                    "accepted"
                }
            }
        }
    }
}

/// Accepting a loss never gives up an entry it could have checked, over
/// every byte of a small log: each byte of every listed segment changed in
/// turn, and then 16 bytes zeroed from each, with the start at a batch's
/// first entry, inside a batch, and at a batch's (and its segment's) last.
/// Damage wholly before the entry after the start: the open opens with
/// everything (and reports what it walked past), or it refuses and the
/// acceptance refuses too or keeps everything. Damage reaching it or past
/// it: the acceptance keeps every entry before the first it reaches, and
/// says so. A refusal changes nothing; an acceptance removes no byte; the
/// open after it agrees and reads back every entry kept. `stride` thins
/// the bytes for the default run.
fn sweep_damage(stride: usize) -> BTreeMap<&'static str, u64> {
    let laid = Laid::new();
    let mut outcomes = BTreeMap::new();
    for start in [4, 5, 6] {
        for (seg, (_, bytes)) in laid.segments.iter().enumerate() {
            for at in (0..bytes.len()).step_by(stride) {
                let outcome = damaged_case(&laid, start, seg, at, at + 1, |b| b[at] ^= 0x5a);
                *outcomes.entry(outcome).or_insert(0) += 1;
                let end = (at + 16).min(bytes.len());
                let outcome = damaged_case(&laid, start, seg, at, end, |b| b[at..end].fill(0));
                *outcomes.entry(outcome).or_insert(0) += 1;
            }
        }
    }
    outcomes
}

#[test]
fn accepting_a_loss_gives_up_no_entry_it_could_check_whatever_byte_is_damaged() {
    let outcomes = sweep_damage(5);
    eprintln!("{outcomes:?}");
    for kind in ["opened", "refused", "accepted"] {
        assert!(
            outcomes.get(kind).copied().unwrap_or(0) > 0,
            "no {kind}: {outcomes:?}"
        );
    }
}

#[test]
#[ignore = "long: every byte of the small log's three 1 KiB segments, changed and zeroed, for three starts (18,432 cases); run with --release"]
fn long_accepting_a_loss_gives_up_no_entry_it_could_check_whatever_byte_is_damaged() {
    eprintln!("{:?}", sweep_damage(1));
}

/// Opens `laid`'s log on `sim` from entry `start`.
fn open_laid(sim: &Sim, laid: &Laid, start: u64) -> Result<(Log, Opened), Box<OpenFailed>> {
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    Log::open(fs, state, laid.config, laid.entries[start as usize - 1].0)
}

/// A read that reaches the entry the log was opened from checks it against
/// that entry's hash: an older, locally valid version of it (the same
/// length and predecessor) on disk is damage there, though the open, which
/// reaches the start by framing, goes on and reports it.
#[test]
fn a_read_at_the_checkpoints_entry_checks_it_against_the_checkpoint() {
    let laid = Laid::new();
    let start = 4u64;
    let prev = laid.entries[start as usize - 2].0;
    let (bytes, _) = format::encode_entry(&prev.hash, start, 1, 1, 0, &[0xee; 32]);
    let (seg, offset) = laid.place_of(start);
    let sim = laid.disk(seg, |b| {
        b[offset..offset + bytes.len()].copy_from_slice(&bytes)
    });
    let (log, opened) = open_laid(&sim, &laid, start).unwrap_or_else(|e| panic!("{}", e.error));
    assert_eq!(opened.before_start.map(|f| f.index), Some(start));
    let read = log.read(start).unwrap().next().unwrap();
    assert!(
        matches!(
            read,
            Err(LogError::Damaged(Damage {
                kind: DamageKind::Start,
                ..
            }))
        ),
        "{read:?}"
    );
    // From after it, the read anchors at the checkpoint, as the open does.
    assert_eq!(
        read_back(&log, start + 1).len(),
        laid.entries.len() - start as usize
    );
}

/// An acceptance carries out the plan it is given or refuses with nothing
/// changed: the log changed since the plan, or the names it would give
/// what it sets aside are spent.
#[test]
fn an_acceptance_refuses_a_plan_the_log_no_longer_matches() {
    let laid = Laid::new();
    let start = 6u64;
    let at = laid.entries[start as usize - 1].0;
    // The first segment's header damaged: the plan sets it aside.
    let sim = laid.disk(0, |b| b[40] ^= 1);
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    let plan = Log::plan_damage(&*fs, &state, laid.config, at).unwrap();
    assert!(
        matches!(plan.files[0].fate, Fate::SetAside { .. }),
        "{plan:?}"
    );
    // Then the last segment is lost: the plan no longer holds.
    let log_files = |sim: &Sim| -> Vec<(String, Option<Vec<u8>>)> {
        sim.image()
            .into_iter()
            .filter(|(p, _)| p.starts_with("log/"))
            .collect()
    };
    fs.remove(&laid.segments[2].0).unwrap();
    fs.flush_folder(FOLDER).unwrap();
    let before = log_files(&sim);
    let refused = Log::accept_damage(fs.clone(), state, laid.config, at, &plan);
    let Err(refused) = refused else {
        panic!("accepted a stale plan")
    };
    assert!(matches!(refused.failed.error, LogError::PlanChanged));
    assert!(refused.decided.is_none());
    assert!(log_files(&sim) == before, "the refusal changed the log");
    // The names spent: refused too, with nothing changed.
    let sim = laid.disk(0, |b| b[40] ^= 1);
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let taken = format!("{FOLDER}/{GIVEN_UP_PREFIX}{}.x", u64::MAX);
    drop(fs.create(&taken, FileOptions::PLAIN).unwrap());
    fs.flush_folder(FOLDER).unwrap();
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    let plan = Log::plan_damage(&*fs, &state, laid.config, at).unwrap();
    let before = log_files(&sim);
    let refused = Log::accept_damage(fs.clone(), state, laid.config, at, &plan);
    let Err(refused) = refused else {
        panic!("accepted with the names spent")
    };
    assert!(
        matches!(refused.failed.error, LogError::Refused(_)),
        "{}",
        refused.failed.error
    );
    assert!(log_files(&sim) == before, "the refusal changed the log");
}

/// The plan is what the open after the acceptance does: a listed segment
/// after the marker whose header (valid in itself) does not continue from
/// the marker's entry is one that open would drop, so the plan sets it
/// aside rather than list it kept, and nothing is removed.
#[test]
fn the_plan_sets_aside_what_the_open_after_it_would_drop() {
    let laid = Laid::new();
    let start = 6u64;
    let last = laid.entries.last().unwrap().0;
    // The last segment cut back to its batches: short, so that the open
    // refuses and the acceptance has work.
    let end = laid.owners[2].last().unwrap().1;
    let sim = laid.disk(2, |b| b.truncate(end));
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let mut state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    // A listed segment after it, with a whole batch chained from the
    // marker's entry, and a header naming another predecessor.
    let run = state.values().segments;
    let number = run.first + run.count;
    let (entry, hash) = format::encode_entry(&last.hash, last.index + 1, 1, 1, 0, &[0x77; 32]);
    let header = format::SegmentHeader {
        store: state.values().server_id.0,
        number,
        size: laid.config.segment_size,
        first: last.index + 1,
        prev_term: last.term,
        prev_hash: [7; 32],
    };
    let batch = format::BatchHeader {
        count: 1,
        first: last.index + 1,
        length: entry.len() as u64,
        cut_count: state.values().cut_count,
        prev: last.hash,
        last: hash,
    };
    let mut bytes = header.encode().to_vec();
    bytes.extend_from_slice(&batch.encode());
    bytes.extend_from_slice(&entry);
    bytes.resize(laid.config.segment_size as usize, 0);
    let file = fs
        .create(&segment_path(number), FileOptions::PLAIN)
        .unwrap();
    file.write_at(0, &bytes).unwrap();
    file.flush().unwrap();
    fs.flush_folder(FOLDER).unwrap();
    state
        .update(|v| {
            v.segments = Segments {
                first: run.first,
                count: run.count + 1,
            }
        })
        .unwrap();
    let at = laid.entries[start as usize - 1].0;
    assert!(Log::open(fs.clone(), state, laid.config, at).is_err());
    sim.restart();
    let state = State::open(&*fs, fs.lock().unwrap()).unwrap();
    let plan = Log::plan_damage(&*fs, &state, laid.config, at).unwrap();
    let fate = plan.files.iter().find(|f| f.number == number).unwrap();
    assert!(matches!(fate.fate, Fate::SetAside { .. }), "{plan:?}");
    assert_eq!(plan.listed.first + plan.listed.count, number, "{plan:?}");
    let (log, opened, accepted) = Log::accept_damage(fs.clone(), state, laid.config, at, &plan)
        .unwrap_or_else(|e| panic!("{}", e.failed.error));
    assert_eq!(accepted.plan, plan);
    assert_eq!(opened.durable, last);
    assert!(opened.removed.is_empty());
    assert_eq!(log.segments(), plan.listed);
    let moved = accepted
        .given_up
        .iter()
        .find(|g| g.number == number)
        .and_then(|g| g.moved_to.clone())
        .unwrap();
    let kept: Vec<u8> = sim
        .image()
        .into_iter()
        .find(|(p, _)| *p == moved)
        .and_then(|(_, b)| b)
        .unwrap();
    assert!(kept == bytes, "the set-aside segment kept whole");
}
