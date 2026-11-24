//! The `state` file under seeded schedules of crashes and storage faults
//! (`docs/server-storage.md` section 12), and the same scenario on the real
//! file system.
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds of each schedule, for
//! long runs; `OM_STORE_SEED=<s>` runs seed `s` alone, to replay a failure
//! (a failing run prints its seed, the simulator's events and its steps).

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

/// A machine the scenario runs on.
trait Machine {
    fn fs(&self) -> &dyn Fs;
    fn random(&self) -> &dyn Random;
    /// The process ends. With `power_loss`, the power fails first where
    /// the machine can do that.
    fn restart(&mut self, power_loss: bool);
    /// What the machine did that the process cannot see.
    fn events(&self) -> Vec<Event>;
    /// No more drawn faults.
    fn calm(&mut self);
}

struct Simulated {
    sim: Sim,
    fs: SimFs,
    random: SimRandom,
}

impl Simulated {
    fn new(seed: u64, rates: Rates) -> Self {
        let sim = Sim::with_rates(seed, rates);
        Self {
            fs: sim.fs(),
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

    fn events(&self) -> Vec<Event> {
        self.sim.events()
    }

    fn calm(&mut self) {
        self.sim.set_rates(Rates::default());
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

    /// The process "ends" by dropping its state and lock; a real power
    /// failure is not available.
    fn restart(&mut self, _power_loss: bool) {}

    fn events(&self) -> Vec<Event> {
        Vec::new()
    }

    fn calm(&mut self) {}
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// Its last open succeeded.
    Open,
    /// An open found no valid copy, where the faults allowed that.
    NoValidCopy,
}

/// What the oracle expects of the next open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// Only crashes and failures the store reports: the last confirmed
    /// copy or a later one that was being written.
    Exact,
    /// The device damaged `state` since the call that confirmed the copy
    /// began: an older copy may be read, or none.
    DamagedConfirmed,
    /// The device misplaced or dropped a write of `state`: a copy that
    /// was once written, never a mixture, or none.
    AnyWritten,
}

/// The copy the last open, create or update that returned `Ok` made.
#[derive(Clone, Copy, Debug)]
struct Confirmed {
    core: Values,
    number: u64,
    /// How much the open that made it raised the start count (0 for an
    /// update or a creation).
    raised: u64,
    /// The machine's events when the call that confirmed it began.
    events: usize,
}

/// The oracle's record of a run.
#[derive(Default)]
struct Model {
    confirmed: Option<Confirmed>,
    /// The values (start count cleared) of updates attempted since.
    attempts: Vec<Values>,
    /// Copies attempted since: each takes one number.
    writes: u64,
    /// The values (start count cleared) of every copy attempted.
    history: Vec<Values>,
    /// The highest start count an open or a creation returned.
    max_start: u64,
}

/// What the oracle compares: everything but the start count.
fn core(values: &Values) -> Values {
    Values {
        start_count: 0,
        ..*values
    }
}

fn touches_state(path: &str) -> bool {
    path == FILE || path == NEW_FILE
}

/// Counts over many runs, to show the schedules reach what they test.
#[derive(Debug, Default)]
struct Reached {
    opens: u64,
    fallbacks: u64,
    no_valid_copy: u64,
    refused: u64,
    failed_updates: u64,
}

/// One run of the scenario: `actions` steps of updates, stops and power
/// failures drawn from `plan`, then a last open with no faults.
struct Run<'a> {
    machine: &'a mut dyn Machine,
    seed: u64,
    plan: Rng,
    model: Model,
    state: Option<State>,
    lock: Option<FolderLock>,
    reached: &'a mut Reached,
    /// What the run did, for a failure's report.
    log: Vec<String>,
}

/// A failing run prints what the machine did, to read with the seed.
impl Drop for Run<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("seed {}: the machine's events:", self.seed);
            for event in self.machine.events() {
                eprintln!("  {event:?}");
            }
            eprintln!("the run's steps:");
            for line in &self.log {
                eprintln!("  {line}");
            }
        }
    }
}

impl<'a> Run<'a> {
    fn new(machine: &'a mut dyn Machine, seed: u64, reached: &'a mut Reached) -> Self {
        Self {
            machine,
            seed,
            plan: Rng::stream(seed, 7),
            model: Model::default(),
            state: None,
            lock: None,
            reached,
            log: Vec::new(),
        }
    }

    fn go(mut self, actions: usize) -> End {
        for _ in 0..actions {
            if self.state.is_none() {
                if let Some(end) = self.start() {
                    return end;
                }
                continue;
            }
            match self.plan.below(10) {
                0..=6 => self.update(),
                7 | 8 => self.stop(false),
                _ => self.stop(true),
            }
        }
        // Recovery without faults: the store opens, or the faults
        // destroyed both copies where the oracle allowed that.
        self.machine.calm();
        self.stop(true);
        for _ in 0..3 {
            if let Some(end) = self.start() {
                return end;
            }
            if self.state.is_some() {
                return End::Open;
            }
        }
        panic!("seed {}: the store did not open without faults", self.seed);
    }

    /// The process ends, the power failing with it when `power_loss`.
    fn stop(&mut self, power_loss: bool) {
        self.log.push(format!("stop (power loss: {power_loss})"));
        self.state = None;
        self.lock = None;
        self.machine.restart(power_loss);
    }

    /// After a failure: the store stops at once (section 6.12), and the
    /// power may fail too.
    fn fail(&mut self) {
        let power_loss = self.plan.below(2) == 0;
        self.stop(power_loss);
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
        self.model.writes += 1;
        let mark = self.machine.events().len();
        let opened = State::open(self.machine.fs(), &lock);
        self.log.push(match &opened {
            Ok(state) => format!("open: {:?} -> {:?}", state.opened(), state.copy()),
            Err(e) => format!("open: {e}"),
        });
        match opened {
            Ok(state) => {
                self.check_open(&state);
                self.reached.opens += 1;
                if state.opened().is_some_and(|o| o.other.is_err()) {
                    self.reached.fallbacks += 1;
                }
                let raised = match state.opened() {
                    Some(o) if o.other.is_err() => 2,
                    _ => 1,
                };
                self.confirm(&state, raised, mark);
                self.state = Some(state);
                self.lock = Some(lock);
            }
            Err(StateError::Missing) => {
                assert!(
                    self.model.confirmed.is_none(),
                    "seed {seed}: state is missing after it was confirmed"
                );
                let created = State::create(self.machine.fs(), self.machine.random(), &lock);
                self.log.push(match &created {
                    Ok(state) => format!("create -> {:?}", state.copy()),
                    Err(e) => format!("create: {e}"),
                });
                match created {
                    Ok(state) => {
                        assert_eq!(state.values().start_count, 1, "seed {seed}");
                        self.confirm(&state, 0, mark);
                        self.state = Some(state);
                        self.lock = Some(lock);
                    }
                    Err(StateError::Disk(_)) => {
                        drop(lock);
                        self.fail();
                    }
                    Err(e) => panic!("seed {seed}: create: {e}"),
                }
            }
            Err(StateError::NoValidCopy { .. }) => {
                let expect = self.expect();
                assert_ne!(
                    expect,
                    Expect::Exact,
                    "seed {seed}: no valid copy, though the device kept the confirmed one; {:?}",
                    self.model.confirmed
                );
                self.reached.no_valid_copy += 1;
                return Some(End::NoValidCopy);
            }
            Err(StateError::Disk(_)) => {
                drop(lock);
                self.fail();
            }
            Err(e) => panic!("seed {seed}: open: {e}"),
        }
        None
    }

    /// What the next open may find, from the device's faults since the
    /// confirmed copy.
    fn expect(&self) -> Expect {
        if self.lied() {
            return Expect::AnyWritten;
        }
        let Some(confirmed) = self.model.confirmed else {
            return Expect::Exact;
        };
        // Damage to either slot counts: an open that wrote the copy it
        // read back and flushed it may then overwrite the confirmed copy's
        // slot, and the copy it read is the one that holds.
        let damaged = self.machine.events()[confirmed.events..]
            .iter()
            .any(|event| matches!(event, Event::Damaged { path, .. } if path == FILE));
        if damaged {
            Expect::DamagedConfirmed
        } else {
            Expect::Exact
        }
    }

    /// Whether the device has misplaced or dropped a write of `state`.
    fn lied(&self) -> bool {
        self.machine.events().iter().any(|e| match e {
            Event::Misplaced { path, .. } | Event::Lost { path, .. } => touches_state(path),
            _ => false,
        })
    }

    fn check_open(&mut self, state: &State) {
        let seed = self.seed;
        let opened = *state.opened().expect("an open");
        let values = *state.values();
        let Some(confirmed) = self.model.confirmed else {
            // Only a creation that did not return can have made it.
            assert_eq!(values.term, 0, "seed {seed}");
            assert_eq!(values.vote, None, "seed {seed}");
            assert_eq!(values.marker.index, 0, "seed {seed}");
            assert_eq!(values.repair, None, "seed {seed}");
            assert!(values.start_count >= 2, "seed {seed}: {values:?}");
            return;
        };
        assert_eq!(values.server_id, confirmed.core.server_id, "seed {seed}");
        let read = core(&values);
        let expect = self.expect();
        let newest = confirmed.number + self.model.writes;
        match expect {
            Expect::Exact => {
                assert!(
                    (confirmed.number..=newest).contains(&opened.number),
                    "seed {seed}: copy {} read; confirmed {}, {} written since",
                    opened.number,
                    confirmed.number,
                    self.model.writes
                );
                assert!(
                    read == confirmed.core || self.model.attempts.contains(&read),
                    "seed {seed}: read {read:?}, confirmed {:?}, attempted since {:?}",
                    confirmed.core,
                    self.model.attempts
                );
            }
            Expect::DamagedConfirmed => {
                assert!(
                    (confirmed.number - 1..=newest).contains(&opened.number),
                    "seed {seed}: copy {} read after damage; confirmed {}",
                    opened.number,
                    confirmed.number
                );
                assert!(
                    self.model.history.contains(&read),
                    "seed {seed}: read {read:?}"
                );
            }
            Expect::AnyWritten => {
                assert!(
                    self.model.history.contains(&read),
                    "seed {seed}: read {read:?}"
                );
            }
        }
        // The start count never goes back. With the confirmed copy damaged
        // it holds unless that copy was itself written by an open that
        // found the other copy invalid (the documented limit).
        let start_checked = match expect {
            Expect::Exact => true,
            Expect::DamagedConfirmed => confirmed.raised != 2,
            Expect::AnyWritten => false,
        };
        if start_checked {
            assert!(
                values.start_count > self.model.max_start,
                "seed {seed}: start count {} after {}",
                values.start_count,
                self.model.max_start
            );
        }
    }

    /// `state` returned `Ok`: what it holds is confirmed.
    fn confirm(&mut self, state: &State, raised: u64, mark: usize) {
        let (number, _) = state.copy();
        let start = state.values().start_count;
        self.model.max_start = if self.lied() {
            start
        } else {
            self.model.max_start.max(start)
        };
        let core = core(state.values());
        if !self.model.history.contains(&core) {
            self.model.history.push(core);
        }
        self.model.confirmed = Some(Confirmed {
            core,
            number,
            raised,
            events: mark,
        });
        self.model.attempts.clear();
        self.model.writes = 0;
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
        self.model.attempts.push(core(&next));
        self.model.history.push(core(&next));
        self.model.writes += 1;
        let state = self.state.as_mut().expect("open");
        let mark = self.machine.events().len();
        let updated = state.update(|v| *v = next);
        self.log.push(match &updated {
            Ok(()) => format!("update -> {:?}", state.copy()),
            Err(e) => format!("update: {e}"),
        });
        match updated {
            Ok(()) => {
                assert_eq!(*state.values(), next, "seed {seed}");
                let state = self.state.take().expect("open");
                self.confirm(&state, 0, mark);
                self.state = Some(state);
            }
            Err(StateError::Disk(_)) => {
                // What reached the device is unknown: nothing more is
                // written through this state.
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

    /// A change a running server could make.
    fn change(&mut self, before: &Values) -> Values {
        let mut v = *before;
        let id = |plan: &mut Rng| {
            let mut id = [0u8; 16];
            plan.fill(&mut id);
            ServerId(id)
        };
        match self.plan.below(6) {
            0 => {
                // A batch was flushed.
                v.marker.index += 1 + self.plan.below(50);
                self.plan.fill(&mut v.marker.hash);
                v.marker.committed = v.marker.committed.max(v.marker.index - 1);
            }
            1 => {
                v.term += 1;
                v.vote = None;
                if self.plan.below(2) == 0 {
                    v.vote = Some(id(&mut self.plan));
                }
            }
            2 if v.vote.is_none() => v.vote = Some(id(&mut self.plan)),
            2 => v.marker.committed += 1,
            3 if v.segments.count > 0 && self.plan.below(2) == 0 => {
                // The oldest segment is retired.
                v.segments.first += 1;
                v.segments.count -= 1;
            }
            3 => v.segments.count += 1,
            4 => {
                v.repair = match v.repair {
                    None => Some(Repair {
                        required_end: v.marker.index,
                    }),
                    Some(_) => None,
                }
            }
            _ => {
                // A follower cuts entries: the marker goes back.
                v.marker.index = v.marker.index.saturating_sub(1 + self.plan.below(5));
                self.plan.fill(&mut v.marker.hash);
            }
        }
        v
    }
}

/// The faults the store answers for: crashes, failed flushes and writes,
/// a full disk, failed reads and damage.
const FAULTS: Rates = Rates {
    crash: 25,
    fail_flush: 12,
    fail_write: 20,
    no_space: 40,
    fail_read: 30,
    damage: 60,
    misplace: 0,
    lose_write: 0,
};

#[test]
fn state_survives_drawn_crashes_and_faults() {
    let mut reached = Reached::default();
    let mut ends = [0u32; 2];
    for seed in seeds(300) {
        let mut machine = Simulated::new(seed, FAULTS);
        let end = Run::new(&mut machine, seed, &mut reached).go(60);
        ends[usize::from(end == End::NoValidCopy)] += 1;
    }
    // The schedules reach what they are meant to test.
    if seeds(300).len() >= 100 {
        assert!(reached.fallbacks > 0, "{reached:?}");
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
            Run::new(&mut machine, seed, &mut reached).go(14);
            machine.sim.steps()
        };
        for step in 1..=steps {
            // How the crash settles what was not flushed, three ways.
            for fates in 0..3 {
                let mut machine =
                    Simulated::new(seed ^ (step << 20) ^ (fates << 40), Rates::default());
                machine.sim.place(step, Fault::Crash);
                let end = Run::new(&mut machine, seed, &mut reached).go(14);
                assert_eq!(end, End::Open, "seed {seed}, crash at step {step}");
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
        Run::new(&mut machine, seed, &mut reached).go(60);
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
        let end = Run::new(&mut machine, seed, &mut reached).go(30);
        assert_eq!(end, End::Open);
    }
    assert!(reached.opens > 0);
}

#[test]
fn a_seed_replays_exactly() {
    let run = |seed: u64| {
        let mut machine = Simulated::new(seed, FAULTS);
        Run::new(&mut machine, seed, &mut Reached::default()).go(60);
        (machine.sim.events(), machine.sim.image())
    };
    let (events, image) = run(11);
    assert!(events.len() > 3, "{events:?}");
    assert_eq!(run(11), (events.clone(), image.clone()));
    assert_ne!(run(12).0, events);
}

#[test]
fn open_refuses_what_it_cannot_trust() {
    let sim = Sim::new(5);
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    assert!(matches!(State::open(&fs, &lock), Err(StateError::Missing)));

    let mut state = State::create(&fs, &sim.random(), &lock).unwrap();
    for index in 1..=3 {
        state.update(|v| v.marker.index = index).unwrap();
    }
    let (newest, slot) = state.copy();
    let start = state.values().start_count;
    drop(state);
    let at = |slot: usize| (slot * SLOT + 40) as u64;

    // Damage to the older copy: the newest is read.
    sim.damage(FILE, at(1 - slot), 4).unwrap();
    let state = State::open(&fs, &lock).unwrap();
    let opened = *state.opened().unwrap();
    assert_eq!((opened.number, opened.slot), (newest, slot));
    assert_eq!(opened.other, Err(Invalid::Hash));
    assert_eq!(state.values().marker.index, 3);
    assert_eq!(state.values().start_count, start + 2);
    let mut state = state;
    state.update(|v| v.marker.index = 4).unwrap();
    let (newest, slot) = state.copy();
    let start = state.values().start_count;
    drop(state);

    // Damage to the newest: the one before it is read, and the start count
    // still rises past the lost copy's.
    sim.damage(FILE, at(slot), 4).unwrap();
    let state = State::open(&fs, &lock).unwrap();
    assert_eq!(state.opened().unwrap().number, newest - 1);
    assert!(state.values().start_count > start);
    let (_, slot) = state.copy();
    drop(state);

    // Both damaged: the open fails and says so.
    sim.damage(FILE, at(slot), 1).unwrap();
    sim.damage(FILE, at(1 - slot), 1).unwrap();
    match State::open(&fs, &lock) {
        Err(e @ StateError::NoValidCopy { .. }) => {
            assert!(matches!(
                e,
                StateError::NoValidCopy {
                    first: Invalid::Hash,
                    second: Invalid::Hash
                }
            ));
            assert!(e
                .to_string()
                .contains("neither copy of the state file is valid"));
        }
        other => panic!("{other:?}"),
    }

    // A copy of a newer format, whole: refused rather than passed over.
    let mut copy = encode(99, state_values());
    copy[8] = 2;
    let end = HEADER + BODY_V1;
    let hash = digest::digest(&digest::SHA256, &copy[..end]);
    copy[end..end + HASH].copy_from_slice(hash.as_ref());
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &copy).unwrap();
    assert!(matches!(
        State::open(&fs, &lock),
        Err(StateError::NewerFormat { version: 2 })
    ));
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
        repair: None,
    };
    &VALUES
}
