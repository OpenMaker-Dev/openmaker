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
use std::sync::Arc;

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
}

struct WatchedFile {
    inner: Box<dyn File>,
    counted: bool,
    changes: Arc<AtomicU64>,
}

impl Watched {
    fn new(inner: Arc<dyn Fs>) -> Self {
        Self {
            inner,
            changes: Arc::default(),
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
            counted: path.starts_with(FOLDER),
            changes: self.changes.clone(),
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
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        if self.counted {
            self.changes.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.write_at(offset, data)
    }

    fn flush(&self) -> sys::Result<()> {
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
        Self {
            fs: Arc::new(Watched::new(Arc::new(sim.fs()))),
            random: sim.random(),
            sim,
        }
    }
}

impl Machine for Simulated {
    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }

    fn changes(&self) -> u64 {
        self.fs.changes.load(Ordering::Relaxed)
    }

    fn random(&self) -> &dyn Random {
        &self.random
    }

    fn restart(&mut self, power_loss: bool) {
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
    committed: u64,
    appended: Position,
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
            appended: Position::START,
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
    state: Option<State>,
    log: Option<Log>,
    model: Model,
    term: u64,
    /// The simulator's events when the current process's open began.
    opened_at: usize,
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
            state: None,
            log: None,
            model: Model::new(),
            term: 1,
            opened_at: 0,
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
        self.state = None;
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
            83..=91 => Action::Stop,
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
        }
    }

    /// The simulator's events from `from` on.
    fn events_since(&self, from: usize) -> Vec<sim::Event> {
        self.machine
            .sim()
            .map(|sim| sim.events()[from..].to_vec())
            .unwrap_or_default()
    }

    fn events_len(&self) -> usize {
        self.machine.sim().map_or(0, |sim| sim.events().len())
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

    /// The process ends, the power failing with it when `power_loss`.
    fn stop(&mut self, power_loss: bool) {
        self.trace.push(stamp(
            &*self.machine,
            format!("stop (power loss: {power_loss})"),
        ));
        self.log = None;
        self.state = None;
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
        let seed = self.seed;
        let fs = self.machine.fs();
        let began = self.events_len();
        let lock = match fs.lock() {
            Ok(lock) => lock,
            Err(e) => {
                assert_ne!(e.kind(), ErrorKind::Locked, "seed {seed}: {e}");
                self.fail();
                return None;
            }
        };
        let mut state = match State::open(&*fs, lock) {
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
        let before = self.machine.changes();
        let opened = Log::open(fs.clone(), &mut state, CONFIG, self.model.start);
        self.trace.push(stamp(
            &*self.machine,
            match &opened {
                Ok((_, opened)) => format!("open from {}: {opened:?}", self.model.start.index),
                Err(e) => format!("open from {}: {e}", self.model.start.index),
            },
        ));
        match opened {
            Ok((log, opened)) => {
                self.opened_at = began;
                self.state = Some(state);
                self.log = Some(log);
                return self.check_open(&opened);
            }
            Err(LogError::Damaged(damage)) => {
                self.check_damage(&damage, before);
                return Some(End::Damaged);
            }
            Err(LogError::Unreadable { .. }) => {
                self.reached.unreadable += 1;
                drop(state);
                self.fail();
            }
            Err(LogError::Disk(_) | LogError::State(_)) => {
                drop(state);
                self.fail();
            }
            Err(e) => panic!("seed {seed}: open: {e}"),
        }
        None
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

    /// The checks of the module docs after an open that succeeded.
    fn check_open(&mut self, opened: &Opened) -> Option<End> {
        let seed = self.seed;
        self.reached.opens += 1;
        if opened.kept > 0 {
            self.reached.kept_tails += 1;
        }
        if opened.cut.is_some() {
            self.reached.open_cuts += 1;
        }
        let durable = opened.durable;
        {
            let log = self.log.as_ref().expect("opened");
            let marker = self.state.as_ref().expect("opened").values().marker;
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
            let state_damaged = self
                .damaged_paths(self.opened_at)
                .contains(crate::state::FILE)
                || damaged.contains(crate::state::FILE);
            let log_damaged = damaged.iter().any(|p| p.starts_with("log/"));
            assert!(
                state_damaged && log_damaged && durable.index >= self.model.before_advance,
                "seed {seed}: the durable position went back from {} to {}",
                self.model.durable,
                durable.index
            );
            lost = true;
        }
        // Read back from the start, against the model.
        let from = self.model.start.index + 1;
        let read: Result<Vec<Entry>, LogError> = self.quiet(|run| {
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
        self.model.acked.split_off(&(durable.index + 1));
        self.model.maybe.clear();
        self.model.durable = durable.index;
        self.model.before_advance = durable.index;
        self.model.appended = durable;
        let committed = self.log.as_ref().expect("opened").committed();
        self.model.committed = committed;
        let last_term = self
            .model
            .acked
            .last_key_value()
            .map_or(self.model.start.term, |(_, e)| e.term);
        self.term = self.term.max(last_term);
        if lost {
            return Some(End::Lost);
        }
        None
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
        let committed =
            with_committed.then(|| self.model.appended.index.saturating_sub(self.plan.below(4)));
        let log = self.log.as_mut().expect("open");
        let segments = log.segments().count;
        let flushed = log.flush_batch(self.state.as_mut().expect("open"), committed);
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
                }
            }
            Err(e) => {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                assert!(matches!(log.append(1, 0, 0, b""), Err(LogError::Poisoned)));
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    fn set_committed(&mut self) {
        let seed = self.seed;
        let committed = self.model.durable;
        let log = self.log.as_mut().expect("open");
        let set = log.set_committed(self.state.as_mut().expect("open"), committed);
        self.trace.push(stamp(
            &*self.machine,
            format!("set committed {committed}: {set:?}"),
        ));
        let log = self.log.as_mut().expect("open");
        match set {
            Ok(()) => {
                self.model.committed = self.model.committed.max(committed);
                assert_eq!(log.committed(), self.model.committed, "seed {seed}");
            }
            Err(e) => {
                assert!(log.is_poisoned(), "seed {seed}: {e}");
                self.reached.poisoned += 1;
                self.fail();
            }
        }
    }

    fn cut(&mut self, back: u64) {
        let seed = self.seed;
        // A cut at or before the committed position, or the checkpoint
        // (which covers only committed entries), is refused with nothing
        // changed (section 7.1). The committed position the marker records
        // can go back one advance with a lost copy of `state`; the
        // checkpoint cannot.
        let floor = self.model.committed.max(self.model.start.index);
        if floor > 0 && self.plan.below(3) == 0 {
            let from = 1 + self.plan.below(floor);
            let log = self.log.as_mut().expect("open");
            let refused = log.cut(self.state.as_mut().expect("open"), from);
            assert!(
                matches!(refused, Err(LogError::Refused(_))),
                "seed {seed}: a cut from {from} at or before {floor} gave {refused:?}"
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
        let log = self.log.as_mut().expect("open");
        let cut = log.cut(self.state.as_mut().expect("open"), from);
        self.trace
            .push(stamp(&*self.machine, format!("cut from {from}: {cut:?}")));
        match cut {
            Ok(position) => {
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
                assert_eq!(position, expected, "seed {seed}");
                assert_eq!(log.appended(), position, "seed {seed}");
                if from <= durable_before {
                    self.reached.cuts_of_durable += 1;
                    self.model.before_advance = from - 1;
                } else if from <= last {
                    self.reached.in_memory_cuts += 1;
                }
                self.model.acked.split_off(&from);
                self.model.maybe.split_off(&from);
                self.model.durable = self.model.durable.min(from - 1);
                self.model.appended = position;
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
        if c > self.model.start.index {
            match self.model.position(c) {
                Some(position) => self.model.start = position,
                None => assert_eq!(self.device, Device::Lying, "seed {seed}: no entry {c}"),
            }
        }
        let start = self.model.start.index;
        let log = self.log.as_mut().expect("open");
        let first = log.first_index();
        let retired = log.retire(self.state.as_mut().expect("open"), start + 1);
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
        let log = self.log.as_ref().expect("open");
        let read: Result<Vec<Entry>, LogError> = log.read(from).and_then(|r| r.collect());
        match read {
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
            ("damage reports", reached.damaged),
        ] {
            assert!(n > 0, "no {what}: {reached:?}");
        }
        assert!(ends.get("Open").copied().unwrap_or(0) > 150, "{ends:?}");
    }
    eprintln!("{ends:?} {reached:?}");
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
fn open_store(sim: &Sim, start: Position) -> Result<(State, Log, Opened), LogError> {
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let mut state = match State::open(&*fs, fs.lock().map_err(LogError::Disk)?) {
        Ok(state) => state,
        Err(StateError::Missing) => {
            State::create(&*fs, &sim.random(), fs.lock().map_err(LogError::Disk)?)
                .map_err(LogError::State)?
        }
        Err(e) => return Err(LogError::State(e)),
    };
    let (log, opened) = Log::open(fs, &mut state, CONFIG, start)?;
    Ok((state, log, opened))
}

/// Append `batches` batches of `per` entries with payloads of `len` bytes,
/// flushing each.
fn fill(log: &mut Log, state: &mut State, batches: usize, per: usize, len: usize) {
    for b in 0..batches {
        for i in 0..per {
            let payload = vec![(b * per + i) as u8; len];
            log.append(1, 2, 3, &payload).unwrap();
        }
        log.flush_batch(state, None).unwrap();
    }
}

/// Where a segment file's entries are, by the format's description
/// (`docs/server-storage-format.md` section 3) rather than the log's code:
/// the header's first index at bytes 48 to 56 of 128; batches from byte
/// 128, each a 96-byte header beginning `OMBATCH\0` with its entry count at
/// 12 to 16 and its first index at 16 to 24; entries of 64 bytes and their
/// payload, whose length is at 28 to 32. Returns, for each entry, its index,
/// offset and size, and for each batch its offset.
fn layout(bytes: &[u8]) -> (Vec<(u64, u64, u64)>, Vec<u64>) {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    assert_eq!(&bytes[0..8], b"OMLOGSG\0");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 1);
    let mut next = u64_at(48);
    let (mut entries, mut batches) = (Vec::new(), Vec::new());
    let mut at = 128usize;
    while at + 96 <= bytes.len() && &bytes[at..at + 8] == b"OMBATCH\0" && u64_at(at + 16) == next {
        batches.push(at as u64);
        let count = u32_at(at + 12);
        at += 96;
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

#[test]
fn a_batch_that_does_not_fit_continues_in_the_next_segment_as_a_second_batch() {
    let sim = Sim::new(3);
    let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
    // 1536-byte segments and entries of 364 bytes: a batch of two leaves
    // room for one more entry, not two.
    fill(&mut log, &mut state, 1, 2, 300);
    fill(&mut log, &mut state, 1, 2, 300);
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
    log.flush_batch(&mut state, None).unwrap();
    // Entries never span segments.
    for &(_, offset, size) in first.iter().chain(&second) {
        assert!(offset + size <= CONFIG.segment_size);
    }
    // The segment header names the store, its number and size, and the
    // entry before its first.
    let header = device_bytes(&sim, &format::segment_path(2));
    assert_eq!(&header[16..32], &state.values().server_id.0);
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
        let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, &mut state, 8, 2, 300);
        assert!(log.segments().count >= 4, "{:?}", log.segments());
        let last = log.durable();
        drop((log, state));
        sim.restart();
        (sim, last)
    };
    let (sim, last) = build();
    let seg2 = format::segment_path(2);
    let (entries2, _) = layout(&device_bytes(&sim, &seg2));
    let first2 = entries2[0].0;
    let last_seg = format::segment_path(
        sim.image()
            .iter()
            .filter(|(p, _)| p.starts_with("log/"))
            .count() as u64,
    );
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
        let mut state = State::open(&*fs, fs.lock().unwrap()).unwrap();
        let marker = state.values().marker;
        match Log::open(fs, &mut state, CONFIG, Position::START) {
            Err(LogError::Damaged(damage)) => {
                assert_eq!(damage.kind, kind, "{what}: {damage:?}");
                if segment != 0 {
                    assert_eq!(damage.segment, segment, "{what}: {damage:?}");
                }
                if let Some(index) = index {
                    assert_eq!(damage.index, index, "{what}: {damage:?}");
                }
            }
            other => panic!("{what}: {:?}", other.map(|(_, opened)| opened)),
        }
        // Nothing was cut, and the marker stands.
        assert_eq!(log_files(&sim), files, "{what}");
        assert_eq!(state.values().marker, marker, "{what}");
    }

    // The same damage before the entry a checkpoint covers is still found
    // when the walk passes it, and not when it does not.
    let (sim, last) = build();
    sim.damage(&format::segment_path(1), 128 + 96 + 70, 1)
        .unwrap();
    let (_, log, opened) = open_store(&sim, last).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(opened.durable, last);
    drop(log);
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
fn what_follows_the_marker_is_kept_when_whole_and_cut_when_not() {
    // A batch written whole whose marker was never recorded: the power
    // failed as `state` was written, and kept nothing unflushed.
    let build = |damaged: bool| {
        let sim = Sim::new(5);
        let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
        // The tail fits in the current segment (a part in a new segment
        // that `state` never listed is not part of the log).
        fill(&mut log, &mut state, 2, 2, 100);
        let marker = log.durable();
        for i in 0..3u8 {
            log.append(2, 0, 0, &[i; 50]).unwrap();
        }
        let tail = log.appended();
        // The batch's synchronous write, then state's write.
        sim.set_crash_fates(Fates::LoseAll);
        sim.place(sim.steps() + 2, Fault::Crash);
        assert!(log.flush_batch(&mut state, None).is_err());
        assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
        assert!(log.is_poisoned());
        drop((log, state));
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
    let (state, log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.kept, tail.index - marker.index);
    assert_eq!(opened.durable, tail);
    assert_eq!(state.values().marker.index, tail.index);
    assert_eq!(log.read(1).unwrap().count() as u64, tail.index);
    drop((log, state));

    let (sim, marker, _) = build(true);
    let (mut state, mut log, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, marker);
    let cut = opened.cut.expect("the damaged batch is cut");
    assert_eq!(cut.index, marker.index + 1);
    // The cut holds: after a power failure that keeps nothing unflushed, the
    // next open finds the same end and nothing more to cut.
    drop((log, state));
    sim.crash(Fates::LoseAll);
    sim.restart();
    let (mut state2, mut log2, opened) = open_store(&sim, Position::START).unwrap();
    assert_eq!(opened.durable, marker);
    assert_eq!(opened.cut, None);
    log2.append(3, 0, 0, b"after the cut").unwrap();
    let after = log2.flush_batch(&mut state2, None).unwrap();
    assert_eq!(after.index, marker.index + 1);
    (state, log) = (state2, log2);
    let read: Vec<Entry> = log.read(1).unwrap().map(Result::unwrap).collect();
    assert_eq!(read.last().unwrap().payload, b"after the cut");
    let _ = &mut state;
}

#[test]
fn a_full_disk_refuses_the_append_before_anything_is_written() {
    let sim = Sim::new(6);
    // Room for `state`, a segment and the next one, not a third.
    sim.set_capacity(2 * crate::state::SLOT as u64 + 2 * CONFIG.segment_size + 600);
    let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
    let mut refused = None;
    for i in 0..40u32 {
        let before = (log.appended(), log.batch_len());
        match log.append(1, 0, 0, &[i as u8; 300]) {
            Ok(_) => {
                log.flush_batch(&mut state, None).unwrap();
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
    // What was durable reads back, and an append that fits in the current
    // segment still goes in... there is none here, so retire to make room:
    // a checkpoint at the last entry lets the first segment go.
    assert_eq!(log.read(1).unwrap().count() as u64, last.index);
    assert!(log.segments().count >= 2);
    let retired = log.retire(&mut state, last.index + 1).unwrap();
    assert!(retired > 0);
    log.append(1, 0, 0, &[9; 300]).unwrap();
    assert_eq!(
        log.flush_batch(&mut state, None).unwrap().index,
        last.index + 1
    );
}

#[test]
fn a_failed_write_or_flush_stops_the_log() {
    for fault in [Fault::FailWrite, Fault::FailFlush] {
        let sim = Sim::new(7);
        let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, &mut state, 2, 2, 50);
        log.append(1, 0, 0, b"x").unwrap();
        sim.place(sim.steps() + 1, fault);
        assert!(log.flush_batch(&mut state, None).is_err(), "{fault:?}");
        assert_eq!(sim.fired().get(&fault), Some(&1));
        assert!(log.is_poisoned());
        assert!(matches!(log.append(1, 0, 0, b""), Err(LogError::Poisoned)));
        assert!(matches!(
            log.flush_batch(&mut state, None),
            Err(LogError::Poisoned)
        ));
        assert!(matches!(log.cut(&mut state, 5), Err(LogError::Poisoned)));
        assert!(matches!(log.retire(&mut state, 1), Err(LogError::Poisoned)));
        assert!(matches!(log.prepare(), Err(LogError::Poisoned)));
        assert!(log.read(1).is_err());
        drop((log, state));
        // The store opens again; the entries reported durable are there.
        sim.restart();
        let (_, log, opened) = open_store(&sim, Position::START).unwrap();
        assert!(opened.durable.index >= 4, "{fault:?}: {opened:?}");
        assert_eq!(log.read(1).unwrap().count() as u64, opened.durable.index);
    }
}

#[test]
fn an_open_that_repairs_its_tail_survives_a_crash_at_every_step() {
    // A batch of one entry whose write failed: whole in the system's cache
    // and not on the device, or torn. The process stops without a power
    // failure, so the next open reads the cache.
    let setup = |seed: u64| {
        let sim = Sim::new(seed);
        let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
        fill(&mut log, &mut state, 2, 2, 30);
        let marker = log.durable();
        let tail = log.append(1, 0, 0, &[7; 4]).unwrap();
        sim.place(sim.steps() + 1, Fault::FailWrite);
        assert!(log.flush_batch(&mut state, None).is_err());
        drop((log, state));
        let whole = sim.events().iter().rev().find_map(|e| match e {
            sim::Event::WriteFailed { written, .. } => Some(*written == 96 + 64 + 4),
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
                let (_, _, opened) = open_store(&sim, Position::START).unwrap();
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
                    let (_, log, opened) = open_store(&sim, Position::START)
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
    let (mut state, mut log, _) = open_store(&sim, Position::START).unwrap();
    fill(&mut log, &mut state, 4, 2, 20);
    log.set_committed(&mut state, 2).unwrap();
    for from in [1, 2] {
        assert!(matches!(
            log.cut(&mut state, from),
            Err(LogError::Refused(_))
        ));
    }
    let at4: Entry = log.read(4).unwrap().next().unwrap().unwrap();
    drop((log, state));
    // Opened from a checkpoint at entry 4, whose committed position the
    // marker no longer records as high (a lost copy of `state` can take it
    // back one advance): the checkpoint's entries are committed all the
    // same.
    sim.restart();
    let (mut state, mut log, _) = open_store(&sim, at4.position()).unwrap();
    assert_eq!(log.committed(), 2);
    for from in [3, 4] {
        assert!(matches!(
            log.cut(&mut state, from),
            Err(LogError::Refused(_))
        ));
    }
    // A retirement that no longer needs entry 6 raises the floor to 6.
    log.retire(&mut state, 7).unwrap();
    assert!(matches!(log.cut(&mut state, 6), Err(LogError::Refused(_))));
    assert_eq!(log.durable().index, 8);
    assert_eq!(log.cut(&mut state, 7).unwrap().index, 6);
}
