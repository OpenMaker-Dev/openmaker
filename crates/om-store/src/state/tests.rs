//! The `state` file under seeded schedules of crashes and storage faults
//! (`docs/server-storage.md` section 12), and the same scenario on the real
//! file system.
//!
//! Two oracles check every open. The **slot oracle** follows, from the
//! writes, flushes and reads the state code makes and the faults the
//! simulator records, which copies each slot can hold, and checks that the
//! copy read is one of them, with the highest number. The **record
//! oracle** checks what the design promises of a confirmed update: no start
//! count is issued twice, a term never goes back, a vote is never forgotten
//! or changed within its term, a repair record, its required end and the
//! segment run are never lost, and the marker goes back by at most one
//! advance.
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds of each schedule, for
//! long runs; `OM_STORE_SEED=<s>` runs seed `s` alone, to replay a failure
//! (a failing run prints its seed, the simulator's events and its steps).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::os::{OsFs, OsRandom, TempFolder};
use crate::sim::{Event, Fates, Fault, Rates, Rng, Sim, SimFs, SimRandom};
use crate::sys::Random;

/// The seeds of a schedule: `default` of them, scaled (module docs).
fn seeds(default: u64) -> Vec<u64> {
    let read = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(seed) = read("OM_STORE_SEED") {
        return vec![seed];
    }
    (0..default.saturating_mul(read("OM_STORE_SCALE").unwrap_or(1))).collect()
}

/// A copy as written.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Written {
    number: u64,
    values: Values,
}

/// A call the state code made on `state` or `state.new`, as recorded.
#[derive(Clone, Debug)]
enum Call {
    /// A write at the start of one slot or more, with the copy each held.
    Write {
        path: String,
        slots: Vec<(usize, Option<Written>)>,
        ok: bool,
    },
    Flush {
        path: String,
        ok: bool,
    },
    Read {
        path: String,
        slot: usize,
        ok: bool,
    },
    Rename {
        from: String,
        to: String,
        ok: bool,
    },
    Remove {
        path: String,
        ok: bool,
    },
}

/// The calls, each with the step it took.
type Log = Arc<Mutex<Vec<(u64, Call)>>>;

fn watched(path: &str) -> bool {
    path == FILE || path == NEW_FILE
}

/// The simulated store folder, recording what the state code does to its
/// files. A call that took no step (its process had stopped) did nothing
/// and is not recorded.
struct Recorder {
    inner: SimFs,
    sim: Sim,
    log: Log,
}

impl Recorder {
    fn record(sim: &Sim, log: &Log, before: u64, call: Call) {
        let after = sim.steps();
        if after > before {
            log.lock().unwrap().push((after, call));
        }
    }

    fn file(&self, path: &str, inner: Box<dyn File>) -> Box<dyn File> {
        if !watched(path) {
            return inner;
        }
        Box::new(RecordedFile {
            inner,
            path: path.to_owned(),
            sim: self.sim.clone(),
            log: self.log.clone(),
        })
    }
}

impl Fs for Recorder {
    fn create_root(&self) -> sys::Result<()> {
        self.inner.create_root()
    }

    fn lock(&self) -> sys::Result<FolderLock> {
        self.inner.lock()
    }

    fn owns(&self, lock: &FolderLock) -> bool {
        self.inner.owns(lock)
    }

    fn create(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        let file = self.inner.create(path, options)?;
        Ok(self.file(path, file))
    }

    fn open(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        let file = self.inner.open(path, options)?;
        Ok(self.file(path, file))
    }

    fn create_folder(&self, path: &str) -> sys::Result<()> {
        self.inner.create_folder(path)
    }

    fn flush_folder(&self, path: &str) -> sys::Result<()> {
        self.inner.flush_folder(path)
    }

    fn rename(&self, from: &str, to: &str) -> sys::Result<()> {
        let before = self.sim.steps();
        let renamed = self.inner.rename(from, to);
        let call = Call::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
            ok: renamed.is_ok(),
        };
        Self::record(&self.sim, &self.log, before, call);
        renamed
    }

    fn remove(&self, path: &str) -> sys::Result<()> {
        let before = self.sim.steps();
        let removed = self.inner.remove(path);
        if watched(path) {
            let call = Call::Remove {
                path: path.to_owned(),
                ok: removed.is_ok(),
            };
            Self::record(&self.sim, &self.log, before, call);
        }
        removed
    }

    fn list(&self, path: &str) -> sys::Result<Vec<String>> {
        self.inner.list(path)
    }
}

struct RecordedFile {
    inner: Box<dyn File>,
    path: String,
    sim: Sim,
    log: Log,
}

impl File for RecordedFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
        let before = self.sim.steps();
        let read = self.inner.read_at(offset, buf);
        let call = Call::Read {
            path: self.path.clone(),
            slot: offset as usize / SLOT,
            ok: read.is_ok(),
        };
        Recorder::record(&self.sim, &self.log, before, call);
        read
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        assert!(
            offset as usize % SLOT == 0
                && (data.len() == COPY
                    || data.len() == HEADER + BODY_V1 + HASH
                    || data.len() == HEADER + BODY_V2 + HASH
                    || data.len() % SLOT == 0),
            "state writes copies at the start of slots"
        );
        let slots = data
            .chunks(SLOT)
            .enumerate()
            .map(|(i, bytes)| {
                let copy = match decode(bytes) {
                    Ok(Decoded::Valid(number, values, _)) => Some(Written { number, values }),
                    _ => None,
                };
                (offset as usize / SLOT + i, copy)
            })
            .collect();
        let before = self.sim.steps();
        let written = self.inner.write_at(offset, data);
        let call = Call::Write {
            path: self.path.clone(),
            slots,
            ok: written.is_ok(),
        };
        Recorder::record(&self.sim, &self.log, before, call);
        written
    }

    fn flush(&self) -> sys::Result<()> {
        let before = self.sim.steps();
        let flushed = self.inner.flush();
        let call = Call::Flush {
            path: self.path.clone(),
            ok: flushed.is_ok(),
        };
        Recorder::record(&self.sim, &self.log, before, call);
        flushed
    }

    fn allocate(&self, size: u64) -> sys::Result<()> {
        self.inner.allocate(size)
    }

    fn size(&self) -> sys::Result<u64> {
        self.inner.size()
    }
}

/// What one slot of a file can hold, by the slot oracle (module docs).
#[derive(Clone, Debug, Default)]
struct Slot {
    /// What the device may hold (`None`: no valid copy).
    device: Vec<Option<Written>>,
    /// Writes not given to the device yet, in order, each marked when it
    /// failed part way.
    pending: Vec<(Option<Written>, bool)>,
    /// Damage reached the slot while writes were pending: writeback may
    /// have put them on the device before it, and a flush does not rewrite
    /// them.
    damaged_pending: bool,
}

impl Slot {
    fn add(&mut self, copy: Option<Written>) {
        if !self.device.contains(&copy) {
            self.device.push(copy);
        }
    }

    fn write(&mut self, copy: Option<Written>, torn: bool) {
        self.pending.push((copy, torn));
        self.damaged_pending = false;
    }

    fn flushed(&mut self) {
        match self.pending.last().copied() {
            None => {}
            Some((copy, false)) => {
                self.device = vec![copy];
                if self.damaged_pending {
                    self.add(None);
                }
            }
            Some((_, true)) => self.unsettled(),
        }
        self.pending.clear();
        self.damaged_pending = false;
    }

    /// A crash or a failed flush: each pending write may have landed,
    /// whole or in part, or not.
    fn unsettled(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        for (copy, _) in std::mem::take(&mut self.pending) {
            self.add(copy);
        }
        self.add(None);
        self.damaged_pending = false;
    }

    fn written_back(&mut self) {
        for (copy, _) in self.pending.clone() {
            self.add(copy);
        }
        if !self.pending.is_empty() {
            self.add(None);
        }
    }

    fn damaged(&mut self) {
        self.add(None);
        if !self.pending.is_empty() {
            self.damaged_pending = true;
        }
    }

    /// What a read of the slot can return.
    fn candidates(&self) -> Vec<Option<Written>> {
        let mut out = self.device.clone();
        for &(copy, torn) in &self.pending {
            for c in [Some(copy), torn.then_some(None)].into_iter().flatten() {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }
}

/// The slot oracle's files, by path.
#[derive(Default)]
struct Slots {
    files: BTreeMap<String, [Slot; 2]>,
    /// What each read of `state` in the current open could return
    /// (`None`: the read failed).
    reads: [Option<Option<Vec<Option<Written>>>>; 2],
    calls: usize,
    events: usize,
}

/// A call or an event, in the order the slot oracle takes them in.
enum Item<'a> {
    Call(&'a Call),
    Event(&'a Event),
}

impl Slots {
    fn file(&mut self, path: &str) -> &mut [Slot; 2] {
        self.files.entry(path.to_owned()).or_default()
    }

    /// Take in the calls and events since the last time, in the order they
    /// happened: what the simulator did before a step's call, the call,
    /// then what happened in it or after it.
    fn follow(&mut self, log: &Log, events: &[Event]) {
        let calls: Vec<(u64, Call)> = log.lock().unwrap()[self.calls..].to_vec();
        self.calls += calls.len();
        let new_events = &events[self.events..];
        self.events = events.len();
        let mut items: Vec<((u64, u8), Item)> = Vec::new();
        for (step, call) in &calls {
            items.push(((*step, 1), Item::Call(call)));
        }
        for event in new_events {
            let key = match event {
                Event::Damaged { step, .. }
                | Event::WrittenBack { step, .. }
                | Event::FolderWrittenBack { step, .. }
                | Event::Evicted { step, .. } => (*step, 0),
                Event::Crash { step, .. }
                | Event::Stopped { step }
                | Event::FlushFailed { step, .. }
                | Event::WriteFailed { step, .. }
                | Event::NoSpace { step, .. }
                | Event::ReadFailed { step, .. }
                | Event::Misplaced { step, .. }
                | Event::Lost { step, .. } => (*step, 2),
            };
            items.push((key, Item::Event(event)));
        }
        items.sort_by_key(|(key, _)| *key);
        for (_, item) in items {
            match item {
                Item::Call(call) => self.call(call),
                Item::Event(event) => self.event(event),
            }
        }
    }

    fn call(&mut self, call: &Call) {
        match call {
            Call::Write { path, slots, ok } => {
                for &(slot, copy) in slots {
                    self.file(path)[slot].write(copy, !ok);
                }
            }
            Call::Flush { path, ok: true } => self.file(path).iter_mut().for_each(Slot::flushed),
            Call::Flush { path, ok: false } => self.file(path).iter_mut().for_each(Slot::unsettled),
            Call::Read { path, slot, ok } if path == FILE => {
                let candidates = ok.then(|| self.file(FILE)[*slot].candidates());
                self.reads[*slot] = Some(candidates);
            }
            Call::Read { .. } => {}
            Call::Rename { from, to, ok } => {
                // Done, or cut short by a crash that may keep it: the name
                // leads to the same slots.
                if from == NEW_FILE && to == FILE {
                    let slots = self.files.get(NEW_FILE).cloned().unwrap_or_default();
                    if *ok {
                        self.files.remove(NEW_FILE);
                    }
                    self.files.insert(FILE.to_owned(), slots);
                }
            }
            Call::Remove { path, ok: true } => {
                self.files.remove(path);
            }
            Call::Remove { .. } => {}
        }
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::Crash { .. } => {
                for slots in self.files.values_mut() {
                    slots.iter_mut().for_each(Slot::unsettled);
                }
            }
            Event::Damaged {
                path, offset, len, ..
            } if watched(path) => {
                for (i, slot) in self.file(path).iter_mut().enumerate() {
                    let hashed = (i * SLOT) as u64..(i * SLOT + COPY) as u64;
                    if *offset < hashed.end && offset + len > hashed.start {
                        slot.damaged();
                    }
                }
            }
            Event::WrittenBack {
                path, offset, len, ..
            } if watched(path) => {
                for (i, slot) in self.file(path).iter_mut().enumerate() {
                    let range = (i * SLOT) as u64..((i + 1) * SLOT) as u64;
                    if *offset < range.end && offset + len > range.start {
                        slot.written_back();
                    }
                }
            }
            _ => {}
        }
    }
}

/// A machine the scenario runs on.
trait Machine {
    fn fs(&self) -> &dyn Fs;
    fn random(&self) -> &dyn Random;
    /// The process ends. With `power_loss`, the power fails first where
    /// the machine can do that.
    fn restart(&mut self, power_loss: bool);
    /// The simulator and the recorded calls, on a simulated machine.
    fn sim(&self) -> Option<(&Sim, &Log)>;
}

struct Simulated {
    sim: Sim,
    fs: Recorder,
    random: SimRandom,
}

impl Simulated {
    fn new(seed: u64, rates: Rates) -> Self {
        let sim = Sim::with_rates(seed, rates);
        Self {
            fs: Recorder {
                inner: sim.fs(),
                sim: sim.clone(),
                log: Log::default(),
            },
            random: sim.random(),
            sim,
        }
    }
}

impl Machine for Simulated {
    fn fs(&self) -> &dyn Fs {
        &self.fs
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

    fn sim(&self) -> Option<(&Sim, &Log)> {
        Some((&self.sim, &self.fs.log))
    }
}

struct Real {
    _folder: TempFolder,
    fs: OsFs,
    random: OsRandom,
}

impl Machine for Real {
    fn fs(&self) -> &dyn Fs {
        &self.fs
    }

    fn random(&self) -> &dyn Random {
        &self.random
    }

    /// The process "ends" by dropping its state; a real power failure is
    /// not available.
    fn restart(&mut self, _power_loss: bool) {}

    fn sim(&self) -> Option<(&Sim, &Log)> {
        None
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// Its last open succeeded.
    Open,
    /// An open found no copy it could use, where the slot oracle allowed
    /// that.
    NoCopy,
}

/// Which promises a schedule's device keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Device {
    /// It may crash, fail and damage, but never lies about a write: both
    /// oracles hold.
    Honest,
    /// It may also misplace or drop a write it reported done: the copy read
    /// is one that was written, never a mixture.
    Lying,
}

/// The record oracle's view of the last confirmed copy.
#[derive(Clone, Copy, Debug)]
struct Confirmed {
    values: Values,
    /// Written to one slot only (a marker advance).
    once: bool,
    /// The copy confirmed before it.
    previous: Values,
}

/// Counts over many runs, to show the schedules reach what they test.
#[derive(Debug, Default)]
struct Reached {
    opens: u64,
    other_slot_lost: u64,
    settled: u64,
    no_copy: u64,
    refused: u64,
    failed_updates: u64,
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

/// One run of the scenario: `actions` steps of updates, stops and power
/// failures drawn from `plan`, then a last open with no faults.
struct Run<'a> {
    machine: &'a mut dyn Machine,
    device: Device,
    seed: u64,
    plan: Rng,
    slots: Slots,
    confirmed: Option<Confirmed>,
    /// Updates attempted since the last confirmation.
    attempts: Vec<Values>,
    /// The highest start count an open or a creation returned.
    issued: u64,
    /// The simulator's events when the last open or creation that
    /// succeeded began.
    trusted_from: usize,
    /// Every copy the state code wrote.
    written: Vec<Written>,
    /// An open returned a mark, and no update has begun to settle it since:
    /// every open returns one.
    marked: bool,
    state: Option<State>,
    reached: &'a mut Reached,
    /// What the run did, for a failure's report.
    log: Vec<String>,
}

/// A failing run prints what the machine did, to read with the seed.
impl Drop for Run<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some((sim, _)) = self.machine.sim() {
                eprintln!("seed {}: the machine's events:", self.seed);
                for event in sim.events() {
                    eprintln!("  {event:?}");
                }
            }
            eprintln!("the run's steps:");
            for line in &self.log {
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
        reached: &'a mut Reached,
    ) -> Self {
        Self {
            machine,
            device,
            seed,
            plan: Rng::stream(seed, 7),
            slots: Slots::default(),
            confirmed: None,
            attempts: Vec::new(),
            issued: 0,
            trusted_from: 0,
            written: Vec::new(),
            marked: false,
            state: None,
            reached,
            log: Vec::new(),
        }
    }

    fn go(mut self, actions: usize) -> End {
        for _ in 0..actions {
            if self.state.is_none() {
                if let Some(end) = self.start() {
                    return self.end(end);
                }
                continue;
            }
            match self.plan.below(10) {
                0..=6 => self.update(),
                7 | 8 => self.stop(false),
                _ => self.stop(true),
            }
        }
        // Recovery without drawn faults: the store opens, or the faults
        // destroyed both slots where the slot oracle allowed that.
        if let Some((sim, _)) = self.machine.sim() {
            sim.set_rates(Rates::default());
        }
        self.stop(true);
        for _ in 0..3 {
            if let Some(end) = self.start() {
                return self.end(end);
            }
            if self.state.is_some() {
                return self.end(End::Open);
            }
        }
        panic!("seed {}: the store did not open without faults", self.seed);
    }

    fn end(self, end: End) -> End {
        if let Some((sim, _)) = self.machine.sim() {
            let sim = sim.clone();
            self.reached.add_fired(&sim);
        }
        end
    }

    /// The process ends, the power failing with it when `power_loss`.
    fn stop(&mut self, power_loss: bool) {
        self.log.push(format!("stop (power loss: {power_loss})"));
        self.state = None;
        self.machine.restart(power_loss);
        self.follow();
    }

    /// After a failure: the store stops at once (section 6.12), and the
    /// power may fail too.
    fn fail(&mut self) {
        let power_loss = self.plan.below(2) == 0;
        self.stop(power_loss);
    }

    /// Bring the slot oracle up to date.
    fn follow(&mut self) {
        if let Some((sim, log)) = self.machine.sim() {
            let events = sim.events();
            for (_, call) in &log.lock().unwrap()[self.slots.calls..] {
                if let Call::Write { slots, .. } = call {
                    self.written
                        .extend(slots.iter().filter_map(|(_, copy)| *copy));
                }
            }
            self.slots.follow(log, &events);
        }
    }

    /// Take the lock and open `state`, or create it where there is none.
    fn start(&mut self) -> Option<End> {
        let seed = self.seed;
        let lock = match self.machine.fs().lock() {
            Ok(lock) => lock,
            Err(e) => {
                assert_ne!(e.kind(), ErrorKind::Locked, "seed {seed}: {e}");
                self.fail();
                return None;
            }
        };
        match self.machine.fs().lock() {
            Err(e) if e.kind() == ErrorKind::Locked => {}
            Err(_) => {
                drop(lock);
                self.fail();
                return None;
            }
            Ok(_) => panic!("seed {seed}: a second lock of the store folder was granted"),
        }
        self.follow();
        self.slots.reads = [None, None];
        let began = self.machine.sim().map_or(0, |(sim, _)| sim.events().len());
        let opened = State::open(self.machine.fs(), lock);
        self.follow();
        self.log.push(match &opened {
            Ok(state) => format!("open: {:?} -> {:?}", state.opened(), state.copy()),
            Err(e) => format!("open: {e}"),
        });
        match opened {
            Ok(state) => {
                self.check_open(&state);
                self.check_mark(&state);
                self.reached.opens += 1;
                if state.opened().is_some_and(|o| o.other.is_err()) {
                    self.reached.other_slot_lost += 1;
                }
                self.confirm(&state, false);
                self.issued = state.values().start_count;
                self.trusted_from = began;
                self.state = Some(state);
            }
            Err(StateError::Missing) => {
                assert!(
                    self.confirmed.is_none(),
                    "seed {seed}: state is missing after it was confirmed"
                );
                let created = self
                    .machine
                    .fs()
                    .lock()
                    .map_err(StateError::Disk)
                    .and_then(|lock| State::create(self.machine.fs(), self.machine.random(), lock));
                self.follow();
                self.log.push(match &created {
                    Ok(state) => format!("create -> {:?}", state.copy()),
                    Err(e) => format!("create: {e}"),
                });
                match created {
                    Ok(state) => {
                        assert_eq!(state.values().start_count, 1, "seed {seed}");
                        assert_eq!(self.issued, 0, "seed {seed}: created twice");
                        self.confirm(&state, false);
                        self.issued = 1;
                        self.trusted_from = began;
                        self.state = Some(state);
                    }
                    Err(StateError::Disk(_)) => self.fail(),
                    Err(e) => panic!("seed {seed}: create: {e}"),
                }
            }
            Err(e @ (StateError::NoValidCopy { .. } | StateError::Unreadable { .. })) => {
                self.check_no_copy(&e);
                self.reached.no_copy += 1;
                return Some(End::NoCopy);
            }
            Err(StateError::Disk(_)) => self.fail(),
            Err(e) => panic!("seed {seed}: open: {e}"),
        }
        None
    }

    /// The slot oracle allows an open to find no copy.
    fn check_no_copy(&self, error: &StateError) {
        let seed = self.seed;
        if self.machine.sim().is_none() {
            panic!("seed {seed}: {error} on a file system without faults");
        }
        if self.device == Device::Lying {
            return;
        }
        for (slot, read) in self.slots.reads.iter().enumerate() {
            match read {
                Some(Some(candidates)) => assert!(
                    candidates.contains(&None),
                    "seed {seed}: {error}, though slot {slot} could only hold {candidates:?}"
                ),
                Some(None) => assert!(
                    matches!(error, StateError::Unreadable { .. }),
                    "seed {seed}: slot {slot} was unreadable, yet {error}"
                ),
                None => panic!("seed {seed}: slot {slot} was not read"),
            }
        }
        // Crashes, failed writes and failed flushes put at most one slot at
        // risk at a time: the state code writes a slot only while the other
        // holds a copy it flushed itself. So no slot is left only when the
        // device damaged one since both were last made whole by an open.
        if matches!(error, StateError::NoValidCopy { .. }) {
            let (sim, _) = self.machine.sim().expect("simulated");
            let damaged = sim.events()[self.trusted_from..].iter().any(|event| {
                matches!(event, Event::Damaged { path, offset, len, .. }
                if watched(path) && (0..2).any(|i| {
                    *offset < (i * SLOT + COPY) as u64 && offset + len > (i * SLOT) as u64
                }))
            });
            assert!(
                damaged,
                "seed {seed}: {error}, though only crashes and failures the store reports \
                 happened since its last open"
            );
        }
    }

    fn check_open(&mut self, state: &State) {
        let seed = self.seed;
        let opened = *state.opened().expect("an open");
        let values = *state.values();
        // The copy as read: the open raised the start count by one.
        let read = Written {
            number: opened.number,
            values: Values {
                start_count: values.start_count - 1,
                mark: opened.mark_read,
                ..values
            },
        };
        match (self.machine.sim().is_some(), self.device) {
            (true, Device::Lying) => {
                assert!(
                    self.written.contains(&read),
                    "seed {seed}: read {read:?}, which was never written"
                );
                return;
            }
            (true, Device::Honest) => self.check_slots(&opened, &read),
            (false, _) => {}
        }
        let Some(confirmed) = self.confirmed else {
            // Only a creation, and opens, that did not return can have
            // made it.
            let initial = Values {
                start_count: read.values.start_count,
                vote: None,
                term: 0,
                segments: Segments { first: 1, count: 0 },
                marker: Marker {
                    index: 0,
                    hash: [0; 32],
                    committed: 0,
                },
                cut_count: 0,
                repair: None,
                ..read.values
            };
            assert_eq!(read.values, initial, "seed {seed}");
            return;
        };
        assert_eq!(values.server_id, confirmed.values.server_id, "seed {seed}");
        if self.machine.sim().is_none() {
            // No faults: exactly what was confirmed.
            assert_eq!(read.values, confirmed.values, "seed {seed}");
        }
        self.check_record(&read.values, &confirmed);
    }

    /// The mark (module docs): an open that passed over a copy returns one;
    /// and once an open returned one, every open returns it until an update
    /// that settles it begins. (A mark whose write was lost whole, after a
    /// read of the slot failed for a while, leaves both copies to be read:
    /// nothing was passed over, and nothing is required.)
    fn check_mark(&mut self, state: &State) {
        let seed = self.seed;
        if self.device == Device::Lying {
            return;
        }
        let opened = state.opened().expect("an open");
        let mark = state.values().mark;
        if opened.other.is_err() {
            assert!(
                mark.is_some(),
                "seed {seed}: an open passed over a copy and left no mark"
            );
        }
        if self.marked {
            assert!(
                mark.is_some(),
                "seed {seed}: an open returned a mark, nothing settled it, and this open \
                 returned none"
            );
        }
        if let Some(read) = opened.mark_read {
            assert_eq!(mark, Some(read), "seed {seed}: the mark changed at an open");
        }
        self.marked |= mark.is_some();
    }

    /// The slot oracle (module docs).
    fn check_slots(&self, opened: &Opened, read: &Written) {
        let seed = self.seed;
        let candidates = |slot: usize| match &self.slots.reads[slot] {
            Some(Some(candidates)) => Some(candidates.clone()),
            Some(None) => None,
            None => panic!("seed {seed}: slot {slot} was not read"),
        };
        let mine = candidates(opened.slot)
            .unwrap_or_else(|| panic!("seed {seed}: slot {} read, yet unreadable", opened.slot));
        assert!(
            mine.contains(&Some(*read)),
            "seed {seed}: read {read:?} from slot {}, which could hold only {mine:?}",
            opened.slot
        );
        let other = 1 - opened.slot;
        match (opened.other, candidates(other)) {
            (Ok(number), Some(theirs)) => {
                assert!(
                    theirs.iter().flatten().any(|w| w.number == number),
                    "seed {seed}: slot {other} read as copy {number}, \
                     which could hold only {theirs:?}"
                );
                assert!(
                    opened.number > number || (opened.number == number && opened.slot == 0),
                    "seed {seed}: copy {} taken over copy {number}",
                    opened.number
                );
            }
            (Err(SlotFault::Invalid(_)), Some(theirs)) => assert!(
                theirs.contains(&None),
                "seed {seed}: slot {other} invalid, though it could hold only {theirs:?}"
            ),
            (Err(SlotFault::Unreadable(_)), None) => {}
            (other_read, theirs) => {
                panic!("seed {seed}: the other slot read as {other_read:?}; oracle {theirs:?}")
            }
        }
    }

    /// The record oracle (module docs): `read` against what was confirmed
    /// and attempted since.
    fn check_record(&self, read: &Values, confirmed: &Confirmed) {
        let seed = self.seed;
        let c = &confirmed.values;
        let attempted = |f: &dyn Fn(&Values) -> bool| self.attempts.iter().any(f);
        assert!(
            read.start_count + 1 > self.issued,
            "seed {seed}: start count {} issued again (issued up to {})",
            read.start_count + 1,
            self.issued
        );
        assert!(
            read.term >= c.term,
            "seed {seed}: term went back: {read:?} after {c:?}"
        );
        if read.term == c.term && c.vote.is_some() {
            assert_eq!(read.vote, c.vote, "seed {seed}: a vote was lost or changed");
        }
        assert!(
            read.repair == c.repair || attempted(&|a| a.repair == read.repair),
            "seed {seed}: repair record {:?} after {:?}",
            read.repair,
            c.repair
        );
        assert!(
            read.cut_count == c.cut_count || attempted(&|a| a.cut_count == read.cut_count),
            "seed {seed}: cut count {} after {}",
            read.cut_count,
            c.cut_count
        );
        assert!(
            read.segments == c.segments || attempted(&|a| a.segments == read.segments),
            "seed {seed}: segments {:?} after {:?}",
            read.segments,
            c.segments
        );
        let one_back = confirmed.once && read.marker == confirmed.previous.marker;
        assert!(
            read.marker == c.marker || one_back || attempted(&|a| a.marker == read.marker),
            "seed {seed}: marker {:?} after {:?} (confirmed once: {})",
            read.marker,
            c.marker,
            confirmed.once
        );
    }

    /// `state` returned `Ok`: what it holds is confirmed.
    fn confirm(&mut self, state: &State, once: bool) {
        let values = *state.values();
        let previous = self.confirmed.map_or(values, |c| c.values);
        self.confirmed = Some(Confirmed {
            values,
            once,
            previous,
        });
        self.attempts.clear();
    }

    fn update(&mut self) {
        let seed = self.seed;
        let before = *self.state.as_ref().expect("open").values();
        // A change that breaks a rule is refused, with nothing written.
        if before.term > 0 && self.plan.below(12) == 0 {
            let state = self.state.as_mut().expect("open");
            let refused = state.update(|v| v.term -= 1);
            assert!(
                matches!(refused, Err(StateError::Refused(_))),
                "seed {seed}"
            );
            assert_eq!(*state.values(), before, "seed {seed}");
            self.reached.refused += 1;
        }
        let next = self.change(&before);
        if before.mark.is_some() && next.mark.is_none() {
            self.marked = false;
            self.reached.settled += 1;
        }
        self.attempts.push(next);
        let calls = self.machine.sim().map(|(_, log)| log.lock().unwrap().len());
        let state = self.state.as_mut().expect("open");
        let updated = state.update(|v| *v = next);
        self.log.push(match &updated {
            Ok(()) => format!("update -> {:?}", state.copy()),
            Err(e) => format!("update: {e}"),
        });
        // The copies it wrote, against the rule in words.
        let expected = copies(&before, &next);
        if let (Some((_, log)), Some(calls), Ok(())) = (self.machine.sim(), calls, &updated) {
            let written = log.lock().unwrap()[calls..]
                .iter()
                .filter(|(_, call)| matches!(call, Call::Write { .. }))
                .count();
            assert_eq!(
                written, expected,
                "seed {seed}: an update from {before:?} to {next:?} wrote {written} copies"
            );
        }
        self.follow();
        match updated {
            Ok(()) if expected == 0 => {
                self.attempts.pop();
            }
            Ok(()) => {
                let state = self.state.take().expect("open");
                assert_eq!(*state.values(), next, "seed {seed}");
                self.confirm(&state, expected == 1);
                self.state = Some(state);
            }
            Err(StateError::Disk(_)) => {
                // What reached the device is unknown: nothing more is
                // written through this state.
                let state = self.state.as_mut().expect("open");
                assert!(
                    matches!(state.update(|_| {}), Err(StateError::Failed)),
                    "seed {seed}"
                );
                self.reached.failed_updates += 1;
                self.fail();
            }
            Err(e) => panic!("seed {seed}: update: {e}"),
        }
    }

    /// A change a running server could make: mostly the marker moving on.
    fn change(&mut self, before: &Values) -> Values {
        let mut v = *before;
        // A mark is settled, in both slots, once what it asks for is done.
        if v.mark.is_some() && self.plan.below(3) == 0 {
            v.mark = None;
            return v;
        }
        let id = |plan: &mut Rng| {
            let mut id = [0u8; 16];
            plan.fill(&mut id);
            ServerId(id)
        };
        match self.plan.below(12) {
            0..=3 => {
                // A batch was flushed.
                v.marker.index += 1 + self.plan.below(50);
                self.plan.fill(&mut v.marker.hash);
                v.marker.committed = v.marker.committed.max(v.marker.index - 1);
            }
            4 => {
                v.term += 1;
                v.vote = None;
                if self.plan.below(2) == 0 {
                    v.vote = Some(id(&mut self.plan));
                }
            }
            5 if v.vote.is_none() => v.vote = Some(id(&mut self.plan)),
            5 => v.marker.committed += 1,
            6 if v.segments.count > 0 && self.plan.below(2) == 0 => {
                // The oldest segment is retired.
                v.segments.first += 1;
                v.segments.count -= 1;
            }
            6 => v.segments.count += 1,
            7 => {
                v.repair = match v.repair {
                    None => Some(Repair {
                        required_end: v.marker.index,
                    }),
                    Some(_) => None,
                }
            }
            8 => {
                // A follower cuts entries: the marker goes back, and the
                // cut count goes up.
                v.marker.index = v.marker.index.saturating_sub(1 + self.plan.below(5));
                self.plan.fill(&mut v.marker.hash);
                v.cut_count += 1;
            }
            9 => {
                // A new segment listed with the batch that needed it.
                v.segments.count += 1;
                v.marker.index += 1;
                self.plan.fill(&mut v.marker.hash);
            }
            // A cut and a new entry at the same index: not an advance.
            10 => self.plan.fill(&mut v.marker.hash),
            // Nothing changes.
            _ => {}
        }
        v
    }
}

/// How many copies an update from `before` to `after` writes, by the rule
/// in words (`docs/server-storage-format.md` 1.3), kept apart from
/// `state.rs`'s own test of it so that an error there is not mirrored
/// here: none when nothing changes; one when only the marker changes, its
/// index and committed position do not go down, and at an unchanged index
/// its hash does not change; two otherwise.
fn copies(before: &Values, after: &Values) -> usize {
    if before == after {
        return 0;
    }
    let (was, now) = (&before.marker, &after.marker);
    let only_the_marker = before.server_id == after.server_id
        && before.start_count == after.start_count
        && before.term == after.term
        && before.vote == after.vote
        && before.segments == after.segments
        && before.cut_count == after.cut_count
        && before.repair == after.repair
        && before.mark == after.mark;
    let forward = now.index >= was.index
        && now.committed >= was.committed
        && (now.index != was.index || now.hash == was.hash);
    if only_the_marker && forward {
        1
    } else {
        2
    }
}

/// The faults the store answers for: crashes, failed flushes and writes,
/// a full disk, failed reads, damage and writeback.
const FAULTS: Rates = Rates {
    crash: 25,
    fail_flush: 12,
    fail_write: 20,
    no_space: 40,
    fail_read: 30,
    damage: 60,
    misplace: 0,
    lose_write: 0,
    writeback: 8,
};

#[test]
fn state_survives_drawn_crashes_and_faults() {
    let mut reached = Reached::default();
    let mut ends = [0u32; 2];
    for seed in seeds(300) {
        let mut machine = Simulated::new(seed, FAULTS);
        let end = Run::new(&mut machine, Device::Honest, seed, &mut reached).go(60);
        ends[usize::from(end == End::NoCopy)] += 1;
    }
    // The schedules reach what they are meant to test.
    if seeds(300).len() >= 100 {
        reached.assert_fired(&FAULTS);
        assert!(reached.other_slot_lost > 0, "{reached:?}");
        assert!(reached.settled > 0, "{reached:?}");
        assert!(reached.failed_updates > 0, "{reached:?}");
        assert!(reached.refused > 0, "{reached:?}");
        assert!(ends[0] > ends[1], "{ends:?}");
    }
}

#[test]
fn state_survives_a_crash_at_every_step() {
    let mut reached = Reached::default();
    for seed in seeds(4) {
        let steps = {
            let mut machine = Simulated::new(seed, Rates::default());
            Run::new(&mut machine, Device::Honest, seed, &mut reached).go(14);
            machine.sim.steps()
        };
        for step in 1..=steps {
            for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                let mut machine = Simulated::new(seed ^ (step << 20), Rates::default());
                machine.sim.set_crash_fates(fates);
                machine.sim.place(step, Fault::Crash);
                let end = Run::new(&mut machine, Device::Honest, seed, &mut reached).go(14);
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
}

#[test]
fn state_reads_a_written_copy_after_misplaced_and_lost_writes() {
    let rates = Rates {
        misplace: 15,
        lose_write: 15,
        ..FAULTS
    };
    let mut reached = Reached::default();
    for seed in seeds(150) {
        let mut machine = Simulated::new(seed, rates);
        Run::new(&mut machine, Device::Lying, seed, &mut reached).go(60);
    }
    if seeds(150).len() >= 100 {
        reached.assert_fired(&rates);
    }
}

#[test]
fn state_scenario_runs_on_the_real_file_system() {
    let mut reached = Reached::default();
    for seed in 0..3 {
        let folder = TempFolder::new("state");
        let mut machine = Real {
            fs: folder.fs(),
            _folder: folder,
            random: OsRandom::new(),
        };
        let end = Run::new(&mut machine, Device::Honest, seed, &mut reached).go(30);
        assert_eq!(end, End::Open);
    }
    assert!(reached.opens > 0);
}

#[test]
fn a_seed_replays_exactly() {
    let run = |seed: u64| {
        let mut machine = Simulated::new(seed, FAULTS);
        Run::new(&mut machine, Device::Honest, seed, &mut Reached::default()).go(60);
        (machine.sim.events(), machine.sim.image())
    };
    let (events, image) = run(11);
    assert!(events.len() > 3, "{events:?}");
    assert_eq!(run(11), (events.clone(), image.clone()));
    assert_ne!(run(12).0, events);
}

/// Open `state` on `sim`, taking the lock.
fn open(sim: &Sim) -> Result<State, StateError> {
    let fs = sim.fs();
    State::open(&fs, fs.lock()?)
}

/// Damage the hashed bytes of `slot` on the device.
fn damage(sim: &Sim, slot: usize) {
    sim.damage(FILE, (slot * SLOT + 40) as u64, 2).unwrap();
}

#[test]
fn open_refuses_what_it_cannot_trust() {
    let sim = Sim::new(5);
    assert!(matches!(open(&sim), Err(StateError::Missing)));
    let fs = sim.fs();
    drop(State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap());

    // Both damaged: the open fails and says so.
    damage(&sim, 0);
    damage(&sim, 1);
    match open(&sim) {
        Err(
            e @ StateError::NoValidCopy {
                first: Invalid::Hash,
                second: Invalid::Hash,
            },
        ) => assert!(e
            .to_string()
            .contains("neither copy of the state file is valid")),
        other => panic!("{other:?}"),
    }

    // Neither slot readable: a different error, so that the caller can tell
    // a failing disk from lost copies.
    let sim = Sim::new(6);
    let fs = sim.fs();
    drop(State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap());
    let at = sim.steps() + 1;
    for _ in 0..2 * READ_ATTEMPTS {
        sim.place(at, Fault::FailRead);
    }
    assert!(matches!(
        open(&sim),
        Err(StateError::Unreadable {
            first: SlotFault::Unreadable(ErrorKind::Read),
            second: SlotFault::Unreadable(ErrorKind::Read),
        })
    ));

    // A copy of a newer format, whole: refused rather than passed over.
    let mut copy = encode(99, state_values());
    copy[8] = 4;
    let end = HEADER + BODY_V3;
    let hash = digest::digest(&digest::SHA256, &copy[..end]);
    copy[end..end + HASH].copy_from_slice(hash.as_ref());
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &copy).unwrap();
    drop(file);
    assert!(matches!(
        open(&sim),
        Err(StateError::NewerFormat { version: 4 })
    ));
}

/// An open that passes over a copy writes the mark into the slot it passed
/// over, flushed, before it writes over the copy it read: a crash at any of
/// its steps, settled three ways, leaves a copy that the next open reads,
/// and that open returns the mark (with the marker of the copy read). Then
/// an update settles it in both slots.
#[test]
fn a_crash_at_every_step_of_an_open_that_passed_over_a_copy_keeps_the_mark() {
    // Copy 3, the marker at 5, in slot 0; copy 2, the marker at 0, in
    // slot 1. Damaging slot 1 leaves the marker at 5; damaging slot 0, the
    // marker at 0, one advance lost.
    let build = |seed: u64, damaged: usize| {
        let sim = Sim::new(seed);
        let fs = sim.fs();
        let mut state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
        state
            .update(|v| {
                v.marker = Marker {
                    index: 5,
                    hash: [5; 32],
                    committed: 5,
                }
            })
            .unwrap();
        drop(state);
        damage(&sim, damaged);
        sim.restart();
        sim
    };
    for damaged in 0..2 {
        let mark = Some(Mark {
            marker: if damaged == 1 { 5 } else { 0 },
        });
        let steps = {
            let sim = build(0, damaged);
            let begin = sim.steps();
            let state = open(&sim).unwrap();
            assert_eq!(state.values().mark, mark);
            sim.steps() - begin
        };
        for step in 1..=steps {
            for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                for seed in 0..4 {
                    let sim = build(seed, damaged);
                    sim.set_crash_fates(fates);
                    sim.place(sim.steps() + step, Fault::Crash);
                    assert!(open(&sim).is_err());
                    sim.restart();
                    let case = format!("slot {damaged}, step {step}, {fates:?}, seed {seed}");
                    let mut state = open(&sim).unwrap_or_else(|e| panic!("{case}: {e}"));
                    assert_eq!(state.values().mark, mark, "{case}");
                    // Settled: both slots hold copies without it.
                    state.update(|v| v.mark = None).unwrap();
                    drop(state);
                    let file = sim.fs().open(FILE, FileOptions::PLAIN).unwrap();
                    for slot in 0..2 {
                        let mut bytes = vec![0u8; SLOT];
                        file.read_at((slot * SLOT) as u64, &mut bytes).unwrap();
                        let Ok(Decoded::Valid(_, values, _)) = decode(&bytes) else {
                            panic!("{case}: slot {slot} holds no copy after the settling update")
                        };
                        assert_eq!(values.mark, None, "{case}: slot {slot}");
                    }
                }
            }
        }
    }
}

/// A copy of version 1, before the cut count, is read with a cut count of
/// 0, and the next update writes the current version; a copy of version 2,
/// before the mark, is read with none. No update sets a mark.
#[test]
fn a_version_1_copy_is_read_with_no_cuts() {
    let sim = Sim::new(12);
    let fs = sim.fs();
    let state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
    let values = *state.values();
    drop(state);
    // Version 1: the same fields without the cut count.
    let copy = copy_v1(5, &values);
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &copy).unwrap();
    file.write_at(SLOT as u64, &copy).unwrap();
    drop(file);
    let mut state = open(&sim).unwrap();
    assert_eq!(state.values().cut_count, 0);
    assert_eq!(state.values().server_id, values.server_id);
    state.update(|v| v.cut_count = 1).unwrap();
    assert!(matches!(
        state.update(|v| v.cut_count = 0),
        Err(StateError::Refused(_))
    ));
    drop(state);
    let mut state = open(&sim).unwrap();
    assert_eq!(state.values().cut_count, 1);
    assert!(matches!(
        state.update(|v| v.mark = Some(Mark { marker: 1 })),
        Err(StateError::Refused(_))
    ));
    // Version 2: the same fields without the mark.
    let values = *state.values();
    drop(state);
    let mut copy = encode(9, &values);
    copy.truncate(HEADER + BODY_V2);
    copy[8..10].copy_from_slice(&2u16.to_le_bytes());
    copy[12..16].copy_from_slice(&(BODY_V2 as u32).to_le_bytes());
    let hash = digest::digest(&digest::SHA256, &copy);
    copy.extend_from_slice(hash.as_ref());
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &copy).unwrap();
    file.write_at(SLOT as u64, &copy).unwrap();
    drop(file);
    let state = open(&sim).unwrap();
    assert_eq!(state.values().mark, None);
    assert_eq!(state.values().cut_count, 1);
}

/// A copy of version 1, as the device holds it: `values`, numbered
/// `number`, without the cut count.
fn copy_v1(number: u64, values: &Values) -> Vec<u8> {
    let mut copy = encode(number, values);
    copy.truncate(HEADER + BODY_V1);
    copy[8..10].copy_from_slice(&1u16.to_le_bytes());
    copy[12..16].copy_from_slice(&(BODY_V1 as u32).to_le_bytes());
    let hash = digest::digest(&digest::SHA256, &copy);
    copy.extend_from_slice(hash.as_ref());
    copy
}

/// The second review's second finding: the first open of a version 1
/// `state` by this code writes the copy it chose back as the bytes it
/// read, and version 2 only as new copies, so that a crash at any step of
/// it leaves a valid copy and loses nothing.
#[test]
fn a_version_1_state_survives_a_crash_at_every_step_of_its_first_open() {
    // Two valid copies: the newer recording committed position 4, the
    // older 1.
    let build = |seed: u64| {
        let sim = Sim::new(seed);
        let fs = sim.fs();
        let state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
        let mut older = *state.values();
        drop(state);
        older.term = 2;
        older.marker = Marker {
            index: 10,
            hash: [5; 32],
            committed: 1,
        };
        let newer = Values {
            marker: Marker {
                committed: 4,
                ..older.marker
            },
            ..older
        };
        let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
        file.write_at(0, &copy_v1(5, &older)).unwrap();
        file.write_at(SLOT as u64, &copy_v1(6, &newer)).unwrap();
        file.flush().unwrap();
        drop(file);
        sim.restart();
        (sim, newer)
    };
    let steps = {
        let (sim, _) = build(0);
        let begin = sim.steps();
        open(&sim).unwrap();
        sim.steps() - begin
    };
    for seed in 0..6 {
        for step in 1..=steps {
            for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                let (sim, newer) = build(seed);
                sim.set_crash_fates(fates);
                sim.place(sim.steps() + step, Fault::Crash);
                assert!(open(&sim).is_err());
                assert_eq!(sim.fired().get(&Fault::Crash), Some(&1));
                sim.restart();
                let state = open(&sim)
                    .unwrap_or_else(|e| panic!("seed {seed}, step {step}, {fates:?}: {e}"));
                let values = state.values();
                assert_eq!(
                    values.marker, newer.marker,
                    "seed {seed}, step {step}, {fates:?}: the newest copy was lost"
                );
                assert_eq!(values.term, newer.term);
                assert_eq!(values.cut_count, 0);
                assert!(values.start_count > newer.start_count);
            }
        }
    }
}

#[test]
fn one_lost_slot_costs_at_most_one_marker_advance() {
    let sim = Sim::new(7);
    let fs = sim.fs();
    let mut state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
    let voter = ServerId([9; 16]);
    state
        .update(|v| {
            v.term = 3;
            v.vote = Some(voter);
        })
        .unwrap();
    state.update(|v| v.marker.index = 10).unwrap();
    drop(state);

    // The slot with the newest marker advance damaged: one advance back,
    // the term and vote kept (written to both slots), a fresh start count.
    let mut state = open(&sim).unwrap();
    assert_eq!(state.values().marker.index, 10);
    assert_eq!(state.values().start_count, 2);
    state.update(|v| v.marker.index = 11).unwrap();
    let (newest, slot) = state.copy();
    drop(state);
    damage(&sim, slot);
    let state = open(&sim).unwrap();
    assert_eq!(state.opened().unwrap().number, newest - 1);
    assert_eq!(
        state.opened().unwrap().other,
        Err(SlotFault::Invalid(Invalid::Hash))
    );
    let values = *state.values();
    assert_eq!((values.term, values.vote), (3, Some(voter)));
    assert_eq!(values.marker.index, 10);
    assert_eq!(values.start_count, 3);
    let (_, slot) = state.copy();
    drop(state);

    // Damage to either slot after an open loses nothing: the start count
    // was written to both.
    damage(&sim, slot);
    let state = open(&sim).unwrap();
    assert_eq!(state.values().start_count, 4);
    assert_eq!(state.values().marker.index, 10);
    drop(state);

    // A read that fails is tried again: both slots are read.
    sim.place(sim.steps() + 1, Fault::FailRead);
    let state = open(&sim).unwrap();
    assert!(state.opened().unwrap().other.is_ok());
    assert_eq!(state.values().start_count, 5);
    drop(state);

    // One slot that fails every read: the other is used.
    for _ in 0..READ_ATTEMPTS {
        sim.place(sim.steps() + 1, Fault::FailRead);
    }
    let state = open(&sim).unwrap();
    assert!(matches!(
        state.opened().unwrap().other,
        Err(SlotFault::Unreadable(ErrorKind::Read))
    ));
    assert_eq!(state.values().start_count, 6);
}

/// The sequence the first review found (seed 5): an update cut short by a
/// failed write and a crash, an open, then damage to a slot that open
/// wrote. No start count may be issued twice.
#[test]
fn a_start_count_is_never_issued_twice_after_a_torn_write_and_damage() {
    for damaged in 0..2 {
        let sim = Sim::new(5);
        let fs = sim.fs();
        let mut issued = vec![];
        let mut state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
        issued.push(state.values().start_count);
        sim.place(sim.steps() + 1, Fault::FailWrite);
        assert!(state.update(|v| v.term = 1).is_err());
        drop(state);
        sim.crash(Fates::KeepAll);
        sim.restart();
        let state = open(&sim).unwrap();
        issued.push(state.values().start_count);
        let (_, last) = state.copy();
        drop(state);
        damage(&sim, if damaged == 0 { last } else { 1 - last });
        let state = open(&sim).unwrap();
        issued.push(state.values().start_count);
        assert!(issued.windows(2).all(|w| w[0] < w[1]), "{issued:?}");
    }
}

/// A flush that fails can leave the cache showing a copy the device does
/// not hold. An open after it, cut by a crash at any step, must not lose
/// both slots: it writes the copy it read back before it writes over the
/// other slot.
#[test]
fn an_open_after_a_failed_flush_survives_a_crash_at_every_step() {
    let voter = ServerId([4; 16]);
    for seed in seeds(40) {
        for step in 1..=12 {
            let sim = Sim::new(seed);
            let fs = sim.fs();
            let mut state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
            state
                .update(|v| {
                    v.term = 1;
                    v.vote = Some(voter);
                })
                .unwrap();
            // The flush of a marker advance fails; the process stops and
            // the cache stays.
            sim.place(sim.steps() + 2, Fault::FailFlush);
            assert!(state.update(|v| v.marker.index = 5).is_err());
            drop(state);
            sim.restart();
            sim.place(sim.steps() + step, Fault::Crash);
            let mut opened = Err(StateError::Failed);
            for _ in 0..3 {
                opened = open(&sim);
                match &opened {
                    Ok(_) => break,
                    Err(StateError::Disk(_)) => sim.restart(),
                    Err(e) => panic!("seed {seed}, crash at step {step} of the open: {e}"),
                }
            }
            let state = opened.unwrap();
            assert_eq!((state.values().term, state.values().vote), (1, Some(voter)));
        }
    }
}

/// A creation whose folder flush fails can leave `state`'s name only in
/// the cache. The next open makes it durable before it returns.
#[test]
fn an_open_makes_the_name_of_state_durable() {
    let mut pending = 0;
    for seed in seeds(30) {
        let sim = Sim::new(seed);
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        // create: open, remove, create, write, flush, rename, folder flush.
        sim.place(sim.steps() + 7, Fault::FailFlush);
        assert!(State::create(&fs, &sim.random(), lock).is_err());
        pending += u32::from(folder_flush_left_pending(&sim) > 0);
        sim.restart();
        let state = open(&sim).unwrap();
        assert_eq!(state.values().start_count, 2);
        drop(state);
        sim.crash(Fates::LoseAll);
        sim.restart();
        let state = open(&sim).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(state.values().start_count, 3);
    }
    assert!(
        pending > 0,
        "no failed folder flush left the rename pending"
    );
}

/// How many changes the last failed folder flush left pending.
fn folder_flush_left_pending(sim: &Sim) -> u32 {
    sim.events()
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::FlushFailed { path, settled, .. } if path.is_empty() => Some(settled.pending),
            _ => None,
        })
        .expect("a failed folder flush")
}

#[test]
fn one_lock_makes_one_writer() {
    let sim = Sim::new(8);
    let fs = sim.fs();
    let state = State::create(&fs, &sim.random(), fs.lock().unwrap()).unwrap();
    // While a state lives, its lock is held: no second one is granted.
    assert_eq!(fs.lock().unwrap_err().kind(), ErrorKind::Locked);
    drop(state);

    // Another store folder's lock is refused.
    let other = Sim::new(9);
    let foreign = other.fs().lock().unwrap();
    assert!(matches!(
        State::open(&fs, foreign),
        Err(StateError::Refused(_))
    ));

    // A lock of an earlier process is refused.
    let stale = fs.lock().unwrap();
    sim.restart();
    assert!(matches!(
        State::open(&fs, stale),
        Err(StateError::Refused(_))
    ));
    assert!(open(&sim).is_ok());

    // Across the two implementations: neither takes the other's lock,
    // however the two were made.
    let folder = TempFolder::new("owner");
    let os = folder.fs();
    let sim = Sim::new(10);
    let sim_fs = sim.fs();
    let mut real = State::create(&os, &OsRandom::new(), os.lock().unwrap()).unwrap();
    assert!(matches!(
        State::open(&os, sim_fs.lock().unwrap()),
        Err(StateError::Refused(_))
    ));
    drop(State::create(&sim_fs, &sim.random(), sim_fs.lock().unwrap()).unwrap());
    let other = OsFs::new(folder.fs().root().join("other"));
    other.create_root().unwrap();
    assert!(matches!(
        State::open(&sim_fs, other.lock().unwrap()),
        Err(StateError::Refused(_))
    ));
    // The real state is still the one writer of its folder.
    real.update(|v| v.term = 1).unwrap();
    assert_eq!(os.lock().unwrap_err().kind(), ErrorKind::Locked);
}

fn state_values() -> &'static Values {
    static VALUES: Values = Values {
        server_id: ServerId([7; 16]),
        start_count: 1,
        term: 0,
        vote: None,
        segments: Segments { first: 1, count: 0 },
        marker: Marker {
            index: 0,
            hash: [0; 32],
            committed: 0,
        },
        cut_count: 0,
        repair: None,
        mark: None,
    };
    &VALUES
}
