//! The page file under seeded schedules of entries, checkpoints, restore
//! points, pins, reclaims, restarts and storage faults on the simulated
//! disk (`docs/server-storage.md` section 12), the same scenario on the
//! real file system, a crash at every step of a checkpoint, a restore
//! point kept and dropped and an open, and targeted cases.
//!
//! **The oracle** shares no code with the page file. It keeps, for every
//! version it holds live (the newest, each pin, each checkpoint the run
//! attempted, by number), the content of every tree as a plain map, and
//! the log of entries appended. After steps and at every restart it
//! checks:
//!
//! - every live version (the newest, the pins, the checkpoints the page
//!   file keeps on disk and the restore points it lists) reads exactly its
//!   model, through the trees' own reads, and its pages are checked whole
//!   by `tree::check`;
//! - no page such a version reaches is free;
//! - once dying pages are drained, the free pages plus the pages the live
//!   versions reach (meta streams included) are every page of the file:
//!   nothing leaked and nothing was counted twice;
//! - an open uses a checkpoint the run made, the newest confirmed one or a
//!   later one that was being written (or, on a dishonest device or a read
//!   that failed during the open, an older one, with the reason), and every
//!   restore point confirmed and not dropped is still listed.
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds, `OM_STORE_ENTRIES=<n>`
//! sets the steps of each run, `OM_STORE_SEED=<s>` runs seed `s` alone; a
//! failing run prints its seed, its steps and the simulator's last events.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::os::TempFolder;
use crate::page::{MemStore, Numbering};
use crate::sim::{Fates, Fault as SimFault, Rates, Rng, Sim};
use crate::tree::{self, key, Edit, Node, Tree};

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn seeds(default: u64) -> Vec<u64> {
    if let Ok(seed) = std::env::var("OM_STORE_SEED") {
        return vec![seed.parse().expect("OM_STORE_SEED is a number")];
    }
    (0..default.saturating_mul(env("OM_STORE_SCALE", 1))).collect()
}

fn steps(default: u64) -> usize {
    env("OM_STORE_ENTRIES", default) as usize
}

fn config() -> Config {
    Config {
        cache_bytes: 24 * PAGE,
        dirty_budget: 400,
        reserve: 8,
        growth: 16,
    }
}

fn tree_name(i: usize) -> TreeName {
    TreeName::new(format!("t{i}")).unwrap()
}

fn entry_id(index: u64) -> EntryId {
    if index == 0 {
        return EntryId::default();
    }
    EntryId {
        index,
        term: 1,
        hash: Hash::of(&index.to_le_bytes()),
    }
}

/// A tree's content: encoded key to node, in key order.
type Content = BTreeMap<Vec<u8>, Node>;

fn remove_under(content: &mut Content, k: &[u8]) {
    let gone: Vec<Vec<u8>> = match key::subtree_end(k) {
        Some(end) => content
            .range(k.to_vec()..end)
            .map(|(k, _)| k.clone())
            .collect(),
        None => content.keys().cloned().collect(),
    };
    for g in gone {
        content.remove(&g);
    }
}

/// What an edit does to a key tree (`tree::Edit`'s docs).
fn model_apply(content: &mut Content, edit: &Edit) {
    match edit {
        Edit::Put { path, node } => {
            for d in 1..path.len() {
                let a = key::encode(&path[..d]);
                match content.get(&a) {
                    None => {
                        content.insert(a, Node::List);
                    }
                    Some(Node::Value(v)) => {
                        let v = v.clone();
                        content.insert(a, Node::Both(v));
                    }
                    Some(_) => {}
                }
            }
            let k = key::encode(path);
            remove_under(content, &k);
            content.insert(k, node.clone());
        }
        Edit::Delete { path } => {
            if path.is_empty() {
                content.clear();
            } else {
                remove_under(content, &key::encode(path));
            }
        }
    }
}

fn value(rng: &mut Rng) -> Vec<u8> {
    let len = match rng.below(24) {
        0 => 4_500 + rng.below(12_000),
        1..=3 => 600 + rng.below(1_500),
        _ => rng.below(100),
    };
    let mut v = vec![0u8; len as usize];
    rng.fill(&mut v);
    v
}

fn edits(rng: &mut Rng, n: u64) -> Vec<Edit> {
    (0..n)
        .map(|_| {
            let a = vec![b'a' + rng.below(5) as u8];
            let mut b = vec![0u8; 1 + rng.below(4) as usize];
            rng.fill(&mut b);
            match rng.below(30) {
                0 => Edit::delete(vec![a]),
                1 => Edit::list(vec![a]),
                2..=5 => Edit::delete(vec![a, b]),
                _ => Edit::leaf(vec![a, b], value(rng)),
            }
        })
        .collect()
}

/// The trees of a version, with their roots and contents.
#[derive(Clone, Debug, Default)]
struct Model {
    entry: EntryId,
    trees: BTreeMap<TreeName, (Root, Content)>,
}

impl Model {
    fn roots(&self) -> Vec<(TreeName, Root)> {
        self.trees
            .iter()
            .map(|(n, (r, _))| (n.clone(), *r))
            .collect()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Edits(Vec<Edit>),
    Create,
    /// The tree is dropped whole and made again, empty.
    Recreate,
}

#[derive(Clone, Debug)]
struct Logged {
    entry: EntryId,
    ops: Vec<(TreeName, Op)>,
    /// The pages its first whole application took.
    taken: Option<u64>,
}

/// The pages applying `ops` may take ([`entry_bound`]).
fn bound(store: &impl PageStore, model: &Model, ops: &[(TreeName, Op)]) -> Result<u64, String> {
    let mut total = 0;
    for (name, op) in ops {
        total += match op {
            Op::Edits(edits) => {
                let (root, _) = model.trees.get(name).ok_or("no such tree")?;
                let height = Tree::open(store, *root)
                    .map_err(|e| e.to_string())?
                    .height();
                entry_bound(edits, height)
            }
            // A root page, and room for the name in two checkpoints' tables.
            Op::Create | Op::Recreate => 3,
        };
    }
    Ok(total)
}

/// Applies `logged` through `res`, and to `model`.
fn apply_logged(res: &Reservation<'_>, model: &mut Model, logged: &Logged) -> Result<(), String> {
    let index = logged.entry.index;
    for (name, op) in &logged.ops {
        match op {
            Op::Edits(edits) => {
                let (root, content) = model.trees.get_mut(name).ok_or("no such tree")?;
                let new = tree::apply(res, *root, index, edits).map_err(|e| {
                    if e == tree::Error::Store(StoreError::Full) {
                        panic!("entry {index}: the reservation from entry_bound was too small");
                    }
                    e.to_string()
                })?;
                for e in edits {
                    model_apply(content, e);
                }
                *root = new;
            }
            Op::Create => {
                let root = res.create_tree(name).map_err(|e| e.to_string())?;
                model.trees.insert(name.clone(), (root, Content::new()));
            }
            Op::Recreate => {
                let (old, _) = model.trees.get(name).ok_or("no such tree")?;
                res.drop_tree(name, *old).map_err(|e| e.to_string())?;
                model.trees.remove(name);
                let root = res.create_tree(name).map_err(|e| e.to_string())?;
                model.trees.insert(name.clone(), (root, Content::new()));
            }
        }
    }
    model.entry = logged.entry;
    Ok(())
}

enum Disk {
    Sim(Sim),
    Real(TempFolder),
}

impl Disk {
    fn fs(&self) -> Box<dyn Fs> {
        match self {
            Disk::Sim(sim) => Box::new(sim.fs()),
            Disk::Real(folder) => Box::new(folder.fs()),
        }
    }

    fn sim(&self) -> Option<&Sim> {
        match self {
            Disk::Sim(sim) => Some(sim),
            Disk::Real(_) => None,
        }
    }

    /// The process ends; with `power_loss`, the power fails first where
    /// the disk can do that.
    fn restart(&self, power_loss: bool) {
        if let Disk::Sim(sim) = self {
            if power_loss {
                sim.crash(Fates::Drawn);
            }
            sim.restart();
        }
    }
}

/// A running process: its page file and the store folder's lock.
struct Process {
    pf: PageFile,
    _lock: FolderLock,
    _fs: Box<dyn Fs>,
}

fn create_proc(disk: &Disk, config: Config) -> Process {
    let fs = disk.fs();
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&*fs, &lock, config).unwrap();
    Process {
        pf,
        _lock: lock,
        _fs: fs,
    }
}

fn open_proc(disk: &Disk, config: Config) -> Result<(Process, Opened), OpenError> {
    let fs = disk.fs();
    let lock = fs.lock().map_err(OpenError::Disk)?;
    let (pf, opened) = PageFile::open(&*fs, &lock, config)?;
    Ok((
        Process {
            pf,
            _lock: lock,
            _fs: fs,
        },
        opened,
    ))
}

/// Which promises a schedule's device keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Device {
    /// No faults (the real file system).
    Clean,
    /// Crashes, failed and short writes, failed flushes and reads, a full
    /// disk, writeback: never a lie about what it holds.
    Honest,
    /// Also damage, misplaced and dropped writes: reads may fail, and must
    /// never return wrong data.
    Lying,
}

/// Counts over many runs, to show the schedules reach what they test.
#[derive(Debug, Default)]
struct Reached {
    entries: u64,
    full: u64,
    checkpoint_needed: u64,
    checkpoints: u64,
    checkpoints_failed: u64,
    restore_points: u64,
    restore_dropped: u64,
    pins: u64,
    reclaimed: u64,
    restarts: u64,
    opens: u64,
    opened_later: u64,
    fell_back: u64,
    no_checkpoint: u64,
    unrecoverable: u64,
    replayed: u64,
    recreated: u64,
    leak_checks: u64,
    /// Dying pages found held for a map on disk at a leak check.
    held: u64,
    read_faults: u64,
    fired: BTreeMap<SimFault, u64>,
}

impl Reached {
    fn assert_fired(&self, rates: &Rates) {
        let drawn = [
            (SimFault::Crash, rates.crash),
            (SimFault::FailFlush, rates.fail_flush),
            (SimFault::FailWrite, rates.fail_write),
            (SimFault::NoSpace, rates.no_space),
            (SimFault::FailRead, rates.fail_read),
            (SimFault::Damage, rates.damage),
            (SimFault::Misplace, rates.misplace),
            (SimFault::LoseWrite, rates.lose_write),
            (SimFault::Writeback, rates.writeback),
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

/// One run of the scenario (module docs).
struct Run<'r> {
    disk: Disk,
    device: Device,
    config: Config,
    seed: u64,
    plan: Rng,
    proc: Option<Process>,
    /// Every entry appended, in order.
    log: Vec<Logged>,
    newest: Model,
    /// Every checkpoint the run attempted, by number, with the version it
    /// was given.
    checkpoints: BTreeMap<u64, Model>,
    /// The newest checkpoint confirmed (or opened).
    confirmed: u64,
    pins: Vec<(Pin, Model)>,
    /// Restore points confirmed and not dropped: name to checkpoint.
    restore: BTreeMap<String, u64>,
    names: u64,
    /// The store could not be opened (a lying device destroyed both
    /// checkpoints).
    dead: bool,
    /// A test damaged the newest checkpoint on purpose: an open may use an
    /// older one.
    allow_fallback: bool,
    /// What the last reservation refused for a full disk wanted.
    last_full: Option<u64>,
    /// A checkpoint number and an entry a test operation notes, for its
    /// checks.
    mark: (u64, u64),
    /// Every checkpoint confirmed or opened, with its entry.
    seen: BTreeSet<(u64, u64)>,
    /// The checkpoint that was newest on disk when each checkpoint was
    /// written: what the page file must keep beside it.
    pred: BTreeMap<u64, Option<u64>>,
    /// The map after every step, when a test compares runs.
    images: Option<Vec<Vec<u8>>>,
    /// Replaying the entries after a checkpoint must never be refused for
    /// space.
    replay_must_fit: bool,
    /// A read failed during an open (one that succeeded or not) since the
    /// last checkpoint confirmed: such an open passes over the checkpoint
    /// it could not read and clears it (section 6.12).
    open_read_failed: bool,
    reached: &'r mut Reached,
    trace: Vec<String>,
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("seed {} ({:?}): the run's steps:", self.seed, self.device);
            for line in &self.trace {
                eprintln!("  {line}");
            }
            if let Some(sim) = self.disk.sim() {
                let events = sim.events();
                eprintln!("the machine's last events:");
                for event in &events[events.len().saturating_sub(40)..] {
                    eprintln!("  {event:?}");
                }
            }
        }
    }
}

impl<'r> Run<'r> {
    fn new(disk: Disk, device: Device, seed: u64, reached: &'r mut Reached) -> Self {
        Self::with(disk, device, seed, reached, config())
    }

    fn with(
        disk: Disk,
        device: Device,
        seed: u64,
        reached: &'r mut Reached,
        config: Config,
    ) -> Self {
        let proc = create_proc(&disk, config);
        let mut checkpoints = BTreeMap::new();
        checkpoints.insert(1, Model::default());
        let mut run = Run {
            disk,
            device,
            config,
            seed,
            plan: Rng::stream(seed, 11),
            proc: Some(proc),
            log: Vec::new(),
            newest: Model::default(),
            checkpoints,
            confirmed: 1,
            pins: Vec::new(),
            restore: BTreeMap::new(),
            names: 0,
            dead: false,
            allow_fallback: false,
            last_full: None,
            mark: (0, 0),
            seen: BTreeSet::new(),
            pred: [(1, None)].into(),
            images: None,
            replay_must_fit: false,
            open_read_failed: false,
            reached,
            trace: Vec::new(),
        };
        let first = Logged {
            entry: entry_id(1),
            ops: (0..3).map(|i| (tree_name(i), Op::Create)).collect(),
            taken: None,
        };
        run.append(first);
        run
    }

    fn pf(&self) -> &PageFile {
        &self.proc.as_ref().expect("a running process").pf
    }

    fn go(mut self, steps: usize, rates: Rates) -> Option<Vec<Vec<u8>>> {
        if let Some(sim) = self.disk.sim() {
            sim.set_rates(rates);
        }
        for step in 0..steps {
            if self.dead {
                break;
            }
            if self.proc.is_none() {
                self.reopen();
                continue;
            }
            if let Some(images) = self.images.as_mut() {
                let image = self.proc.as_ref().map(|p| p.pf.map_image());
                images.push(image.unwrap_or_default());
            }
            match self.plan.below(100) {
                0..=51 => self.entry(),
                52..=61 => {
                    let keep = self.plan.one_in(5);
                    self.checkpoint(keep);
                }
                62..=66 => self.pin(),
                67..=70 => self.unpin(),
                71..=73 => self.drop_restore_point(),
                74..=81 => self.reclaim(),
                82..=86 => self.check(step % 3 == 0),
                87..=92 => self.stop(false),
                _ => self.stop(true),
            }
        }
        if self.dead {
            self.end();
            return self.images.take();
        }
        // Recovery with no faults drawn: the store opens and every check
        // holds, with dying pages drained.
        if let Some(sim) = self.disk.sim() {
            sim.set_rates(Rates::default());
        }
        self.stop(self.device != Device::Clean);
        for _ in 0..3 {
            if self.proc.is_none() && !self.dead {
                self.reopen();
            }
        }
        if self.proc.is_none() && self.device == Device::Lying {
            // The device damaged a page that recovery needs: nothing is
            // served wrong, and the store cannot be brought back without
            // a repair (section 6.12).
            self.reached.unrecoverable += 1;
            self.dead = true;
        }
        if !self.dead {
            assert!(self.proc.is_some(), "the store did not open without faults");
            self.check(true);
            // Another checkpoint and restart: what the last one freed and
            // reused is still sound.
            self.entry();
            self.checkpoint(false);
            self.stop(self.device != Device::Clean);
            self.reopen();
            if self.proc.is_some() {
                self.check(true);
                // Two checkpoints later every map on disk records the
                // settled pages settled: nothing is held.
                if self.device != Device::Lying {
                    self.checkpoint(false);
                    self.checkpoint(false);
                    self.check(true);
                    assert!(self.pf().held_pages().is_empty(), "pages still held");
                }
            }
        }
        self.end();
        self.images.take()
    }

    fn end(&mut self) {
        if let Some(sim) = self.disk.sim() {
            for (fault, n) in sim.fired() {
                *self.reached.fired.entry(fault).or_default() += n;
            }
        }
    }

    fn append(&mut self, logged: Logged) {
        self.append_for(logged, Purpose::Entry);
    }

    fn append_for(&mut self, logged: Logged, purpose: Purpose) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let index = logged.entry.index;
        let pages = match bound(pf, &self.newest, &logged.ops) {
            Ok(pages) => pages,
            Err(e) => {
                self.trace.push(format!("entry {index}: bound failed: {e}"));
                self.allow_read_failure(&e);
                return self.fail();
            }
        };
        let outcome = match pf.reserve(index, pages, purpose) {
            Ok(res) => {
                self.trace.push(format!(
                    "entry {index}: {} trees, {pages} pages reserved",
                    logged.ops.len()
                ));
                self.log.push(logged.clone());
                self.reached.entries += 1;
                if logged.ops.iter().any(|(_, op)| matches!(op, Op::Recreate)) {
                    self.reached.recreated += 1;
                }
                match apply_logged(&res, &mut self.newest, &logged) {
                    Ok(()) => {
                        if let Some(last) = self.log.last_mut() {
                            last.taken = Some(res.taken());
                        }
                        Ok(())
                    }
                    Err(e) => Err(Err(e)),
                }
            }
            Err(e) => Err(Ok(e)),
        };
        match outcome {
            Ok(()) => {}
            Err(Ok(refused)) => self.refused(index, refused),
            Err(Err(e)) => {
                self.trace.push(format!("entry {index} failed: {e}"));
                self.allow_read_failure(&e);
                self.fail();
            }
        }
    }

    fn refused(&mut self, index: u64, error: SpaceError) {
        match error {
            SpaceError::Full { wanted, available } => {
                self.reached.full += 1;
                self.last_full = Some(wanted);
                self.trace.push(format!(
                    "entry {index}: full ({wanted} wanted, {available} available)"
                ));
                // Refused with nothing reserved; the store still reads and
                // can checkpoint.
                self.check(false);
                self.checkpoint(false);
            }
            SpaceError::CheckpointNeeded { .. } => {
                self.reached.checkpoint_needed += 1;
                self.trace.push(format!("entry {index}: checkpoint needed"));
                self.checkpoint(false);
            }
            SpaceError::Failed | SpaceError::Disk(_) => self.fail(),
            SpaceError::Refused(why) => panic!("reservation refused: {why}"),
        }
    }

    fn entry(&mut self) {
        if self.proc.is_none() {
            return;
        }
        let index = self.log.last().map_or(1, |l| l.entry.index + 1);
        let n = 1 + self.plan.below(2) as usize;
        let first = self.plan.below(3) as usize;
        let ops = (0..n)
            .map(|i| {
                let name = tree_name((first + i) % 3);
                let op = if self.plan.one_in(25) {
                    Op::Recreate
                } else {
                    let count = 1 + self.plan.below(6);
                    Op::Edits(edits(&mut self.plan, count))
                };
                (name, op)
            })
            .collect();
        self.append(Logged {
            entry: entry_id(index),
            ops,
            taken: None,
        });
    }

    /// An entry that changes all three trees.
    fn entry_all(&mut self) {
        let index = self.log.last().map_or(1, |l| l.entry.index + 1);
        let ops = (0..3)
            .map(|i| {
                let count = 2 + self.plan.below(4);
                (tree_name(i), Op::Edits(edits(&mut self.plan, count)))
            })
            .collect();
        self.append(Logged {
            entry: entry_id(index),
            ops,
            taken: None,
        });
    }

    /// The next entry's index.
    fn next_index(&self) -> u64 {
        self.log.last().map_or(1, |l| l.entry.index + 1)
    }

    /// Reopens until the store is open, then checks everything with dying
    /// pages drained; then a few entries, a checkpoint and a power loss,
    /// and the same again: what the first open and its frees left is
    /// sound.
    fn settle(&mut self) {
        for _ in 0..5 {
            if self.proc.is_none() {
                self.reopen();
            }
        }
        assert!(self.proc.is_some(), "the store did not open");
        self.check(true);
        for _ in 0..4 {
            self.entry_all();
        }
        self.checkpoint(false);
        self.stop(true);
        self.reopen();
        assert!(self.proc.is_some(), "the store did not open again");
        self.check(true);
    }

    /// A read the device failed is allowed where the device can fail
    /// reads; any other failure of an apply is a defect.
    fn allow_read_failure(&mut self, error: &str) {
        self.reached.read_faults += 1;
        let read = error.contains("unreadable");
        match self.device {
            Device::Clean => panic!("a failure on a disk without faults: {error}"),
            Device::Honest => assert!(read, "an honest disk gave: {error}"),
            Device::Lying => {}
        }
    }

    fn checkpoint(&mut self, keep: bool) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let number = pf.next_checkpoint();
        // Names come from a small pool, so that a name dropped is kept
        // again while the old restore point is still on disk.
        let listed: Vec<String> = pf
            .restore_points()
            .into_iter()
            .filter(|(_, dropped)| !dropped)
            .map(|(rp, _)| rp.name)
            .collect();
        let name = keep
            .then(|| {
                (1..=3)
                    .map(|i| format!("rp{i}"))
                    .find(|n| !listed.contains(n))
            })
            .flatten();
        self.names += u64::from(name.is_some());
        self.checkpoints.insert(number, self.newest.clone());
        self.pred.insert(number, Some(self.confirmed));
        let result = pf.checkpoint(self.newest.entry, &self.newest.roots(), name.as_deref());
        self.trace.push(format!(
            "checkpoint {number} at {} (keep {name:?}): {result:?}",
            self.newest.entry.index
        ));
        match result {
            Ok(done) => {
                assert_eq!(done.number, number);
                self.reached.checkpoints += 1;
                self.open_read_failed = false;
                self.confirmed = number;
                self.seen.insert((number, self.newest.entry.index));
                if let Some(name) = name {
                    self.reached.restore_points += 1;
                    self.restore.insert(name, number);
                }
            }
            Err(CheckpointError::Space(_)) => self.reached.checkpoints_failed += 1,
            Err(CheckpointError::Root { fault, .. }) => {
                self.allow_read_failure(&fault.to_string());
                self.fail();
            }
            Err(CheckpointError::Disk(_) | CheckpointError::Failed) => {
                self.reached.checkpoints_failed += 1;
                self.fail();
            }
            Err(CheckpointError::Refused(why)) => panic!("checkpoint refused: {why}"),
        }
    }

    fn pin(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pin = proc.pf.pin(self.newest.entry.index).expect("the newest");
        self.reached.pins += 1;
        self.trace.push(format!("pin {}", self.newest.entry.index));
        self.pins.push((pin, self.newest.clone()));
    }

    fn unpin(&mut self) {
        if !self.pins.is_empty() {
            let i = self.plan.below(self.pins.len() as u64) as usize;
            let (pin, _) = self.pins.remove(i);
            self.trace.push(format!("unpin {}", pin.entry()));
        }
    }

    fn drop_restore_point(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let listed: Vec<String> = proc
            .pf
            .restore_points()
            .into_iter()
            .filter(|(_, dropped)| !dropped)
            .map(|(rp, _)| rp.name)
            .collect();
        if listed.is_empty() {
            return;
        }
        let name = &listed[self.plan.below(listed.len() as u64) as usize];
        proc.pf.drop_restore_point(name).unwrap();
        self.trace.push(format!("drop restore point {name}"));
        self.reached.restore_dropped += 1;
        self.restore.remove(name);
    }

    fn reclaim(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let budget = 1 + self.plan.below(60) as usize;
        let done = proc.pf.reclaim(budget);
        self.reached.reclaimed += done.freed;
        assert!(
            done.twice.is_empty(),
            "pages released twice: {:?}",
            done.twice
        );
        for fault in &done.faults {
            let fault = fault.to_string();
            self.allow_read_failure(&fault);
        }
    }

    /// The process stops; the power fails with it when `power_loss`.
    fn stop(&mut self, power_loss: bool) {
        self.trace.push(format!("stop (power loss: {power_loss})"));
        self.pins.clear();
        self.proc = None;
        self.disk.restart(power_loss);
        self.reached.restarts += 1;
    }

    /// After a failure: the store stops at once (section 6.12), and the
    /// power may fail too.
    fn fail(&mut self) {
        let power_loss = self.device != Device::Clean && self.plan.one_in(2);
        self.stop(power_loss);
    }

    fn fired(&self, fault: SimFault) -> u64 {
        self.disk
            .sim()
            .map_or(0, |sim| sim.fired().get(&fault).copied().unwrap_or(0))
    }

    fn reopen(&mut self) {
        let reads_before = self.fired(SimFault::FailRead);
        let result = open_proc(&self.disk, self.config);
        self.open_read_failed |= self.fired(SimFault::FailRead) > reads_before;
        let (proc, opened) = match result {
            Ok(opened) => opened,
            Err(OpenError::Disk(e)) => {
                self.trace.push(format!("open failed: {e}"));
                assert_ne!(self.device, Device::Clean, "{e}");
                let power_loss = self.plan.one_in(2);
                return self.stop(power_loss);
            }
            Err(OpenError::NoCheckpoint { slots, passed_over }) => {
                self.trace
                    .push(format!("no checkpoint: {slots:?} {passed_over:?}"));
                self.reached.no_checkpoint += 1;
                // A read the device failed passes a checkpoint over; on an
                // honest disk the open is tried again.
                let unreadable = passed_over.iter().all(|p| {
                    matches!(
                        &p.why,
                        Rejection::Meta(MetaFault::Page(f)) | Rejection::Root { fault: f, .. }
                            if matches!(f.fault, Fault::Io(_))
                    )
                }) && slots.iter().any(|s| matches!(s, SlotSeen::Copy(_)));
                match self.device {
                    Device::Lying => self.dead = true,
                    Device::Honest if unreadable => {
                        let power_loss = self.plan.one_in(2);
                        self.stop(power_loss);
                    }
                    _ => panic!("no checkpoint: {slots:?} {passed_over:?}"),
                }
                return;
            }
            Err(e) => panic!("open: {e}"),
        };
        let read_failed = self.open_read_failed;
        self.reached.opens += 1;
        self.trace.push(format!("opened {opened:?}"));
        let number = opened.checkpoint;
        let model = self
            .checkpoints
            .get(&number)
            .unwrap_or_else(|| panic!("checkpoint {number} was never made"))
            .clone();
        assert_eq!(opened.entry, model.entry);
        match number.cmp(&self.confirmed) {
            std::cmp::Ordering::Greater => self.reached.opened_later += 1,
            std::cmp::Ordering::Less => {
                self.reached.fell_back += 1;
                assert!(
                    self.device == Device::Lying || read_failed || self.allow_fallback,
                    "checkpoint {number} opened, older than {} confirmed: {opened:?}",
                    self.confirmed
                );
            }
            std::cmp::Ordering::Equal => {}
        }
        if self.device != Device::Lying && !read_failed && !self.allow_fallback {
            assert!(
                opened.passed_over.is_empty(),
                "an honest disk lost a checkpoint: {opened:?}"
            );
        }
        assert_eq!(proc.pf.checkpoint_trees(), model.roots());
        self.check_slots(&opened);
        self.confirmed = number;
        self.seen.insert((number, model.entry.index));
        self.restore.retain(|_, at| *at <= number);
        self.newest = model;
        self.proc = Some(proc);
        // Recovery: every entry after the checkpoint, applied again.
        let after: Vec<Logged> = self
            .log
            .iter()
            .filter(|l| l.entry.index > self.newest.entry.index)
            .cloned()
            .collect();
        for logged in after {
            loop {
                let Some(proc) = self.proc.as_ref() else {
                    return;
                };
                let pf = &proc.pf;
                let index = logged.entry.index;
                // The pages the entry took when first applied: exactly what
                // applying it again takes.
                let pages = match logged.taken {
                    Some(taken) => taken,
                    None => match bound(pf, &self.newest, &logged.ops) {
                        Ok(pages) => pages,
                        Err(e) => {
                            self.allow_read_failure(&e);
                            return self.fail();
                        }
                    },
                };
                let outcome = match pf.reserve(index, pages, Purpose::Recovery) {
                    Ok(res) => match apply_logged(&res, &mut self.newest, &logged) {
                        Ok(()) => {
                            if let Some(taken) = logged.taken {
                                assert_eq!(res.taken(), taken, "entry {index} took other pages");
                            }
                            Ok(())
                        }
                        Err(e) => Err(Err(e)),
                    },
                    Err(e) => Err(Ok(e)),
                };
                match outcome {
                    Ok(()) => {}
                    Err(Err(e)) => {
                        self.trace.push(format!("replay of {index} failed: {e}"));
                        self.allow_read_failure(&e);
                        return self.fail();
                    }
                    Err(Ok(SpaceError::CheckpointNeeded { .. })) => {
                        // A checkpoint during recovery frees nothing.
                        self.checkpoint(false);
                        continue;
                    }
                    Err(Ok(SpaceError::Refused(why))) => panic!("replay refused: {why}"),
                    Err(Ok(e @ SpaceError::Full { .. })) if self.replay_must_fit => {
                        panic!("replaying entry {index} ran out of space: {e}")
                    }
                    Err(Ok(e)) => {
                        self.trace.push(format!("replay of {index}: {e}"));
                        assert_ne!(self.device, Device::Clean, "{e}");
                        return self.fail();
                    }
                }
                break;
            }
            self.reached.replayed += 1;
        }
        self.check(false);
    }

    /// After an open, the pair of the checkpoint used holds its copy in both
    /// slots, and the other pair holds its predecessor's copy in both slots,
    /// or nothing valid: the open wrote what the device may not have held,
    /// and cleared what must never be taken again. (Skipped when a slot
    /// cannot be read, and on a lying disk.)
    fn check_slots(&mut self, opened: &Opened) {
        if self.device == Device::Lying {
            // The device may have dropped what the open wrote.
            return;
        }
        let fs = self.disk.fs();
        let Ok(file) = fs.open(FILE, FileOptions::PLAIN) else {
            return;
        };
        let mut numbers = [None; SLOTS];
        for (slot, number) in numbers.iter_mut().enumerate() {
            let mut buf = vec![0u8; COPY];
            if file.read_at((slot * SLOT) as u64, &mut buf).is_err() {
                return;
            }
            if let Ok(superblock::Decoded::Valid(sb)) = superblock::decode(&buf) {
                *number = Some(sb.number);
            }
        }
        let pair = (0..SLOTS)
            .find(|&s| numbers[s] == Some(opened.checkpoint))
            .map(superblock::pair_of)
            .expect("the checkpoint used is on disk");
        for slot in pair_slots(pair) {
            assert_eq!(
                numbers[slot],
                Some(opened.checkpoint),
                "slot {slot}: {numbers:?}"
            );
        }
        for slot in pair_slots(1 - pair) {
            assert_eq!(
                numbers[slot], opened.predecessor,
                "slot {slot}: {numbers:?}"
            );
        }
    }

    /// Checks one version's trees against the model, adding the pages they
    /// reach to `reach`. False when a read failed (allowed on a disk with
    /// faults; reported as read faults).
    fn check_model(&mut self, model: &Model, reach: &mut BTreeSet<PageNo>, what: &str) -> bool {
        let pf = &self.proc.as_ref().unwrap().pf;
        let mut complete = true;
        let mut failures = Vec::new();
        for (name, (root, content)) in &model.trees {
            match tree::check(pf, *root) {
                Ok(report) => reach.extend(report.pages.iter().map(|(n, _)| *n)),
                Err(e) => {
                    failures.push(e.to_string());
                    complete = false;
                    continue;
                }
            }
            let tree = match Tree::open(pf, *root) {
                Ok(tree) => tree,
                Err(e) => {
                    failures.push(e.to_string());
                    complete = false;
                    continue;
                }
            };
            let mut expected = content.iter();
            let mut whole = true;
            for item in tree.walk(&[] as &[&[u8]]) {
                match item {
                    Ok(item) => {
                        let (k, node) = expected.next().unwrap_or_else(|| {
                            panic!("{what} {name}: an item the model does not have")
                        });
                        assert_eq!(item.key.as_bytes(), &k[..], "{what} {name}: a key");
                        assert_eq!(&item.node, node, "{what} {name}: a node");
                    }
                    Err(e) => {
                        failures.push(e.to_string());
                        whole = false;
                        break;
                    }
                }
            }
            if whole {
                assert!(expected.next().is_none(), "{what} {name}: items lost");
            } else {
                complete = false;
            }
        }
        for failure in failures {
            self.trace.push(format!("{what}: read failed: {failure}"));
            self.allow_read_failure(&failure);
        }
        complete
    }

    /// The oracle's checks (module docs); with `drain`, dying pages are
    /// drained first and nothing may have leaked.
    fn check(&mut self, drain: bool) {
        if self.proc.is_none() {
            return;
        }
        let mut reach = BTreeSet::new();
        let mut complete = true;
        let newest = self.newest.clone();
        complete &= self.check_model(&newest, &mut reach, "newest");
        let pinned: Vec<Model> = self.pins.iter().map(|(_, m)| m.clone()).collect();
        for model in &pinned {
            complete &= self.check_model(model, &mut reach, "pinned");
        }
        let pf = &self.proc.as_ref().unwrap().pf;
        let kept = pf.kept_checkpoints();
        let restore = pf.restore_points();
        if self.device != Device::Lying && !self.open_read_failed && !self.allow_fallback {
            // The newest on disk, and the one that was newest when it was
            // written.
            let mut expected = vec![self.confirmed];
            expected.extend(self.pred.get(&self.confirmed).copied().flatten());
            let numbers: Vec<u64> = kept.iter().map(|k| k.number).collect();
            assert_eq!(numbers, expected, "the checkpoints on disk");
        }
        let mut versions = Vec::new();
        for kc in &kept {
            versions.push((
                kc.number,
                kc.entry,
                kc.meta,
                format!("checkpoint {}", kc.number),
            ));
        }
        for (rp, _) in &restore {
            versions.push((
                rp.checkpoint,
                rp.entry,
                rp.meta,
                format!("restore point {}", rp.name),
            ));
        }
        for (number, entry, meta, what) in versions {
            let model = self
                .checkpoints
                .get(&number)
                .unwrap_or_else(|| panic!("{what}: never made"))
                .clone();
            assert_eq!(entry, model.entry, "{what}");
            let pf = &self.proc.as_ref().unwrap().pf;
            match (pf.trees_of(meta), pf.meta_pages_of(meta)) {
                (Ok(trees), Ok(pages)) => {
                    assert_eq!(trees, model.roots(), "{what}: its trees");
                    reach.extend(pages);
                }
                (a, b) => {
                    let failure = [a.err(), b.err()]
                        .into_iter()
                        .flatten()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ");
                    let failure = format!("{what}: {failure}");
                    complete = false;
                    self.allow_read_failure(&failure);
                }
            }
            complete &= self.check_model(&model, &mut reach, &what);
        }
        if self.device != Device::Lying {
            for (name, number) in &self.restore {
                assert!(
                    restore
                        .iter()
                        .any(|(rp, _)| rp.name == *name && rp.checkpoint == *number),
                    "restore point {name} (checkpoint {number}) is lost: {restore:?}"
                );
            }
        }
        let pf = &self.proc.as_ref().unwrap().pf;
        let space = pf.space();
        assert!(space.cached * PAGE <= self.config.cache_bytes, "{space:?}");
        for n in pf.free_pages() {
            assert!(
                !reach.contains(&n),
                "page {n} is free and a live version reaches it"
            );
        }
        if !drain || !complete || self.device == Device::Lying {
            return;
        }
        let mut drained = false;
        for _ in 0..50 {
            let done = pf.reclaim(100_000);
            self.reached.reclaimed += done.freed;
            assert!(
                done.twice.is_empty(),
                "pages released twice: {:?}",
                done.twice
            );
            if !done.faults.is_empty() {
                for fault in &done.faults {
                    self.allow_read_failure(&fault.to_string());
                }
                return;
            }
            if done.drained {
                drained = true;
                break;
            }
        }
        assert!(drained, "dying pages did not drain");
        for n in pf.free_pages() {
            assert!(
                !reach.contains(&n),
                "page {n} was freed and a live version reaches it"
            );
        }
        let space = pf.space();
        let held: BTreeSet<PageNo> = pf.held_pages().into_iter().collect();
        for n in pf.dying_pages() {
            assert!(
                reach.contains(&n) || held.contains(&n),
                "page {n} is dying, no kept version reaches it and nothing holds it"
            );
        }
        let held_only = held.difference(&reach).count() as u64;
        self.reached.held += held_only;
        assert_eq!(
            space.free + reach.len() as u64 + held_only,
            space.end - 1,
            "free {} + reached {} + held {held_only} against {} pages: a page leaked or was \
             counted twice ({space:?})",
            space.free,
            reach.len(),
            space.end - 1
        );
        self.reached.leak_checks += 1;
    }
}

fn honest() -> Rates {
    Rates {
        crash: 900,
        fail_flush: 500,
        fail_write: 700,
        no_space: 700,
        fail_read: 2_500,
        writeback: 60,
        ..Rates::default()
    }
}

fn lying() -> Rates {
    Rates {
        crash: 900,
        damage: 500,
        misplace: 300,
        lose_write: 300,
        writeback: 60,
        ..Rates::default()
    }
}

/// The issue's seeded schedules on an honest device: crashes and every
/// fault that does not lie, at every step.
#[test]
fn seeded_schedules_on_an_honest_disk() {
    let mut reached = Reached::default();
    for seed in seeds(40) {
        let run = Run::new(
            Disk::Sim(Sim::new(seed)),
            Device::Honest,
            seed,
            &mut reached,
        );
        run.go(steps(120), honest());
    }
    eprintln!("{reached:#?}");
    reached.assert_fired(&honest());
    if std::env::var("OM_STORE_SEED").is_err() {
        assert!(reached.leak_checks > 0 && reached.opened_later + reached.fell_back > 0);
        assert!(reached.full + reached.checkpoints_failed > 0, "{reached:?}");
        assert!(
            reached.restore_dropped > 0 && reached.recreated > 0,
            "{reached:?}"
        );
    }
}

/// The same schedules on a device that damages, misplaces and drops
/// writes: an open uses the newest checkpoint it can check, falls back
/// otherwise, and no read returns wrong data.
#[test]
fn seeded_schedules_on_a_lying_disk() {
    let mut reached = Reached::default();
    for seed in seeds(30) {
        let run = Run::new(
            Disk::Sim(Sim::new(seed ^ 0x5eed)),
            Device::Lying,
            seed,
            &mut reached,
        );
        run.go(steps(120), lying());
    }
    eprintln!("{reached:#?}");
    reached.assert_fired(&lying());
}

/// The same scenario body on the real file system, without faults.
#[test]
fn the_scenario_on_the_real_file_system() {
    let mut reached = Reached::default();
    for seed in seeds(2) {
        let folder = TempFolder::new("pagefile");
        let run = Run::new(Disk::Real(folder), Device::Clean, seed, &mut reached);
        run.go(steps(120), Rates::default());
    }
    assert!(reached.leak_checks > 0, "{reached:?}");
}

/// A run on a simulated disk with no faults drawn.
fn quiet_run(seed: u64, reached: &mut Reached) -> (Sim, Run<'_>) {
    let sim = Sim::new(seed);
    let run = Run::new(Disk::Sim(sim.clone()), Device::Honest, seed, reached);
    (sim, run)
}

fn crashes(sim: &Sim) -> u64 {
    sim.fired().get(&SimFault::Crash).copied().unwrap_or(0)
}

/// Runs `prefix`, then `op` with a crash placed at each of its steps in
/// turn, with every way a crash settles what was not flushed; after each,
/// the store must open on a checkpoint the run made, recover, pass every
/// check with dying pages drained, and stay sound through another
/// checkpoint and power loss. Returns how many crashes were placed.
fn crash_at_every_step(
    seed: u64,
    prefix: impl Fn(&mut Run<'_>),
    op: impl Fn(&mut Run<'_>),
    after: impl Fn(&mut Run<'_>),
) -> u64 {
    let mut reached = Reached::default();
    let (before, end) = {
        let (sim, mut run) = quiet_run(seed, &mut reached);
        prefix(&mut run);
        let before = sim.steps();
        op(&mut run);
        assert!(run.proc.is_some(), "the operation failed without a fault");
        (before, sim.steps())
    };
    assert!(end > before, "the operation took no step");
    let mut placed = 0;
    for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
        for step in before + 1..=end {
            let (sim, mut run) = quiet_run(seed, &mut reached);
            prefix(&mut run);
            assert_eq!(sim.steps(), before, "the prefix replays exactly");
            sim.set_crash_fates(fates);
            sim.place(step, SimFault::Crash);
            run.trace
                .push(format!("crash placed at step {step}, {fates:?}"));
            op(&mut run);
            assert_eq!(crashes(&sim), 1, "the crash at step {step} fired");
            if run.proc.is_some() {
                run.stop(false);
            }
            run.settle();
            after(&mut run);
            placed += 1;
        }
    }
    placed
}

fn prefix(run: &mut Run<'_>) {
    for _ in 0..6 {
        run.entry_all();
    }
    run.checkpoint(false);
    for _ in 0..6 {
        run.entry();
    }
    run.checkpoint(true);
    for _ in 0..4 {
        run.entry_all();
    }
    run.pin();
    run.entry_all();
}

#[test]
fn a_crash_at_every_step_of_a_checkpoint() {
    let placed = crash_at_every_step(1, prefix, |run| run.checkpoint(false), |_| {});
    assert!(placed >= 9, "{placed}");
}

/// A restore point kept (a checkpoint that lists itself) and dropped (the
/// next checkpoint omits it; the one after retires it): at every step, the
/// restore point is listed and reads its version until a confirmed
/// checkpoint retires it.
#[test]
fn a_crash_at_every_step_of_a_restore_point_kept_and_dropped() {
    let kept = crash_at_every_step(
        2,
        prefix,
        |run| run.checkpoint(true),
        |run| {
            let listed = run.pf().restore_points();
            assert!(listed.iter().any(|(rp, _)| rp.name == "rp1"), "{listed:?}");
        },
    );
    let dropped = crash_at_every_step(
        3,
        prefix,
        |run| {
            run.mark = (run.pf().next_checkpoint(), run.newest.entry.index);
            run.drop_restore_point();
            run.checkpoint(false);
            run.checkpoint(false);
        },
        |run| {
            // Once a checkpoint made after the drop has been on disk, and
            // another after it (settle makes one), no superblock lists it.
            // (A checkpoint that never reached the disk leaves its number
            // to the next one.)
            let (number, entry) = run.mark;
            if run.seen.iter().any(|&(n, e)| n >= number && e == entry) {
                let listed = run.pf().restore_points();
                assert!(listed.iter().all(|(rp, _)| rp.name != "rp1"), "{listed:?}");
            }
        },
    );
    assert!(kept >= 9 && dropped >= 18, "{kept} {dropped}");
}

#[test]
fn a_crash_at_every_step_of_an_open() {
    let placed = crash_at_every_step(
        4,
        |run| {
            prefix(run);
            run.checkpoint(false);
            run.entry_all();
            run.stop(true);
        },
        |run| run.reopen(),
        |_| {},
    );
    assert!(placed >= 10, "{placed}");
}

/// The newest checkpoint's root page damaged on the device: the open
/// passes over it, says why, and clears its slots before any page is
/// written; a crash at every step of that open leaves a store that opens
/// the same way.
#[test]
fn a_crash_at_every_step_of_an_open_that_passes_over_a_damaged_checkpoint() {
    let damaged = |run: &mut Run<'_>| {
        prefix(run);
        run.entry_all();
        run.checkpoint(false);
        let (number, _) = run.pf().checkpoint_entry();
        let root = run.pf().checkpoint_trees()[0].1;
        run.stop(false);
        let sim = run.disk.sim().unwrap().clone();
        sim.damage(FILE, root.page * PAGE + 300, 4).unwrap();
        run.allow_fallback = true;
        run.trace
            .push(format!("damaged the root of checkpoint {number}"));
    };
    let placed = crash_at_every_step(
        5,
        damaged,
        |run| run.reopen(),
        |run| assert!(run.reached.fell_back > 0),
    );
    assert!(placed >= 10, "{placed}");
}

/// Applying the same entries twice from the same checkpoint gives the
/// same map, byte for byte, and the same roots, page numbers included:
/// with no pins or reclaims, and with pins and reclaims at the same points
/// in both runs.
#[test]
fn applying_the_same_entries_twice_from_a_checkpoint_gives_the_same_map() {
    for seed in seeds(3) {
        let sim = Sim::new(seed);
        let disk = Disk::Sim(sim.clone());
        let config = Config {
            dirty_budget: 100_000,
            ..config()
        };
        let mut proc = create_proc(&disk, config);
        let mut rng = Rng::stream(seed, 9);
        let mut model = Model::default();
        let logged = |index: u64, rng: &mut Rng| Logged {
            entry: entry_id(index),
            ops: if index == 1 {
                (0..3).map(|i| (tree_name(i), Op::Create)).collect()
            } else {
                let t = rng.below(3) as usize;
                let count = 1 + rng.below(8);
                vec![(tree_name(t), Op::Edits(edits(rng, count)))]
            },
            taken: None,
        };
        let apply = |pf: &PageFile, model: &mut Model, l: &Logged, purpose| {
            let pages = bound(pf, model, &l.ops).unwrap();
            let res = pf.reserve(l.entry.index, pages, purpose).unwrap();
            apply_logged(&res, model, l).unwrap();
        };
        for index in 1..=20 {
            let l = logged(index, &mut rng);
            apply(&proc.pf, &mut model, &l, Purpose::Entry);
        }
        let checkpoint = proc
            .pf
            .checkpoint(model.entry, &model.roots(), None)
            .unwrap();
        let later: Vec<Logged> = (21..=70).map(|i| logged(i, &mut rng)).collect();
        for running in [false, true] {
            let mut results = Vec::new();
            for _ in 0..2 {
                drop(proc);
                sim.crash(Fates::LoseAll);
                sim.restart();
                let (p, opened) = open_proc(&disk, config).unwrap();
                assert_eq!(opened.checkpoint, checkpoint.number);
                let mut m = model.clone();
                let mut pins = Vec::new();
                for (i, l) in later.iter().enumerate() {
                    apply(&p.pf, &mut m, l, Purpose::Recovery);
                    if running && i % 9 == 4 {
                        pins.push(p.pf.pin(l.entry.index).unwrap());
                    }
                    if running && i % 7 == 6 {
                        p.pf.reclaim(5);
                        pins.truncate(1);
                    }
                }
                results.push((p.pf.map_image(), m.roots(), p.pf.space()));
                proc = p;
            }
            assert!(
                results[0] == results[1],
                "seed {seed}, running {running}: the maps differ"
            );
        }
    }
}

/// A full disk refuses a reservation before anything is appended; the
/// store still reads, checkpoints, drops a tree through the reserve, and
/// once the space is free again takes entries.
#[test]
fn a_full_disk_refuses_reservations_and_the_store_still_reads_and_checkpoints() {
    let mut reached = Reached::default();
    let (sim, mut run) = quiet_run(7, &mut reached);
    sim.set_capacity(16 << 20);
    let mut refused = false;
    for _ in 0..2_000 {
        let before = run.reached.full;
        let index = run.next_index();
        run.append(Logged {
            entry: entry_id(index),
            ops: vec![(
                tree_name(index as usize % 2),
                Op::Edits(
                    (0..4)
                        .map(|i| {
                            Edit::leaf(
                                vec![b"big".to_vec(), [index as u8, i].to_vec()],
                                vec![i; 9_000],
                            )
                        })
                        .collect(),
                ),
            )],
            taken: None,
        });
        assert!(run.proc.is_some(), "a full disk stopped the store");
        if run.reached.full > before {
            refused = true;
            break;
        }
    }
    assert!(refused, "the disk never filled");
    // The refusal checked every version and made a checkpoint.
    assert_eq!(run.reached.checkpoints_failed, 0);
    let confirmed = run.pf().checkpoint_entry().0;
    assert_eq!(confirmed, run.confirmed);
    // An entry is refused again, with nothing reserved.
    assert!(run.last_full.is_some());
    assert_eq!(run.pf().space().promised, 0, "nothing reserved");
    let index = run.next_index();
    // Dropping a tree draws on the reserve.
    run.append_for(
        Logged {
            entry: entry_id(index),
            ops: vec![(tree_name(0), Op::Recreate)],
            taken: None,
        },
        Purpose::Discard,
    );
    assert!(run.proc.is_some());
    run.checkpoint(false);
    run.checkpoint(false);
    run.check(true);
    let space = run.pf().space();
    assert!(space.free > space.floor + 20, "{space:?}");
    let before = run.reached.entries;
    let index = run.next_index();
    run.append(Logged {
        entry: entry_id(index),
        ops: vec![(
            tree_name(1),
            Op::Edits(vec![Edit::leaf(vec![b"small".to_vec()], vec![1; 8])]),
        )],
        taken: None,
    });
    assert_eq!(run.reached.entries, before + 1, "an entry is taken again");
    run.check(true);
}

/// Share counts at the level of the map (section 6.8): a tree cloned by
/// sharing its root, both changed, one dropped, then the other: every
/// version reads its model, nothing a live tree reaches is freed, and
/// nothing leaks, across a restart.
#[test]
fn share_counts_keep_a_cloned_tree_whole_until_both_are_dropped() {
    let mut reached = Reached::default();
    let (_sim, mut run) = quiet_run(8, &mut reached);
    let fill = |run: &mut Run<'_>, t: usize, n: u8, keys: u8| {
        let index = run.next_index();
        let edits = (0..keys)
            .map(|i| {
                let mut value = vec![n; 80];
                if i % 13 == 0 {
                    value.resize(6_000, i);
                }
                Edit::leaf(vec![vec![b'k', i], vec![n]], value)
            })
            .collect();
        run.append(Logged {
            entry: entry_id(index),
            ops: vec![(tree_name(t), Op::Edits(edits))],
            taken: None,
        });
    };
    for n in 0..6 {
        fill(&mut run, 0, n, 40);
    }
    run.checkpoint(false);
    // The clone: tree 3 is tree 0 as it is, sharing every page.
    let (root, content) = run.newest.trees[&tree_name(0)].clone();
    run.pf().clone_for_test(&tree_name(3), root.page);
    run.newest.trees.insert(tree_name(3), (root, content));
    run.checkpoint(false);
    assert_eq!(run.pf().count_for_test(root.page), 2);
    for n in 6..10 {
        fill(&mut run, 0, n, 2);
        fill(&mut run, 3, n + 50, 3);
    }
    assert!(
        run.pf().space().shared > 1,
        "counts passed down: {:?}",
        run.pf().space()
    );
    run.check(true);
    run.stop(true);
    run.reopen();
    run.check(true);
    // Tree 0 dropped: what tree 3 shares stays.
    let index = run.next_index();
    run.append(Logged {
        entry: entry_id(index),
        ops: vec![(tree_name(0), Op::Recreate)],
        taken: None,
    });
    run.checkpoint(false);
    run.checkpoint(false);
    run.check(true);
    // Tree 3 dropped too: everything goes.
    let index = run.next_index();
    run.append(Logged {
        entry: entry_id(index),
        ops: vec![(tree_name(3), Op::Recreate)],
        taken: None,
    });
    run.checkpoint(false);
    run.checkpoint(false);
    run.check(true);
    assert_eq!(run.pf().space().shared, 0);
    assert!(run.reached.leak_checks >= 4, "{}", run.reached.leak_checks);
}

/// Paths deep and long, values of every size: what an entry writes never
/// exceeds `entry_bound`, for entries of one edit up to a thousand.
#[test]
fn entry_bound_covers_what_an_entry_writes() {
    let store = MemStore::new(Numbering::Sequential);
    let mut root = tree::create(&store, 1).unwrap();
    let mut rng = Rng::new(5);
    let mut worst = 0f64;
    for entry in 2..400u64 {
        let n = match rng.below(12) {
            0 => 200 + rng.below(800),
            1..=3 => 10 + rng.below(50),
            _ => 1 + rng.below(4),
        };
        let edits: Vec<Edit> = (0..n)
            .map(|_| {
                let depth = 1 + rng.below(5) as usize;
                let path: Vec<Vec<u8>> = (0..depth)
                    .map(|_| {
                        let len = match rng.below(40) {
                            0 => 2_000 + rng.below(9_000),
                            _ => 1 + rng.below(6),
                        };
                        let mut s = vec![0u8; len as usize];
                        rng.fill(&mut s);
                        s[0] = b'a' + rng.below(3) as u8;
                        s
                    })
                    .collect();
                if tree::key::encoded_len(&path) > crate::page::MAX_KEY {
                    return Edit::delete(path[..1].to_vec());
                }
                match rng.below(8) {
                    0 => Edit::delete(path[..1.max(depth / 2)].to_vec()),
                    _ => Edit::leaf(path, value(&mut rng)),
                }
            })
            .collect();
        let height = Tree::open(&store, root).unwrap().height();
        store.take_written();
        root = tree::apply(&store, root, entry, &edits).unwrap();
        let written = store.take_written().len() as u64;
        let bound = entry_bound(&edits, height);
        assert!(
            written <= bound,
            "entry {entry}: {written} pages written, bound {bound}"
        );
        worst = worst.max(written as f64 / bound as f64);
    }
    assert!(worst > 0.0);
    let one = entry_bound(&[Edit::leaf(vec![b"a".to_vec()], b"x".to_vec())], 3);
    assert_eq!(one, 45, "the bound of one put into a tree of height 3");
}

/// A damaged, misplaced or missing page is reported by the read that
/// reaches it, by number and kind, and never served.
#[test]
fn a_damaged_misplaced_or_missing_page_is_reported_by_the_read_that_reaches_it() {
    let mut reached = Reached::default();
    let sim = Sim::new(9);
    let mut run = Run::new(Disk::Sim(sim.clone()), Device::Lying, 9, &mut reached);
    for n in 0..4u8 {
        let index = run.next_index();
        let edits = (0..60u8)
            .map(|i| Edit::leaf(vec![vec![b'k', i], vec![n]], vec![i; 300]))
            .collect();
        run.append(Logged {
            entry: entry_id(index),
            ops: vec![(tree_name(0), Op::Edits(edits))],
            taken: None,
        });
    }
    run.checkpoint(false);
    run.checkpoint(false);
    let (root, _) = run.newest.trees[&tree_name(0)].clone();
    let pages: Vec<PageNo> = tree::check(run.pf(), root)
        .unwrap()
        .pages
        .iter()
        .map(|(n, _)| *n)
        .filter(|&n| n != root.page)
        .collect();
    assert!(pages.len() >= 2, "tree 0 has pages below its root");
    let (victim, donor) = (pages[0], pages[pages.len() - 1]);
    run.stop(false);
    sim.damage(FILE, victim * PAGE + 1_000, 3).unwrap();
    run.reopen();
    let walk = |run: &Run<'_>| -> Result<usize, tree::Error> {
        let tree = Tree::open(run.pf(), root)?;
        let mut n = 0;
        for item in tree.walk(&[] as &[&[u8]]) {
            item?;
            n += 1;
        }
        Ok(n)
    };
    match walk(&run) {
        Err(tree::Error::Page(f)) => {
            assert_eq!((f.page, f.fault.clone()), (victim, Fault::Damaged), "{f}")
        }
        other => panic!("damage not reported: {other:?}"),
    }
    // The donor's bytes written over the victim's place: misplaced.
    let bytes = run.pf().read_raw(donor).unwrap();
    let fs = run.disk.fs();
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(victim * PAGE, &bytes).unwrap();
    file.flush().unwrap();
    drop(file);
    run.stop(false);
    run.reopen();
    match walk(&run) {
        Err(tree::Error::Page(f)) => assert_eq!(
            (f.page, f.fault.clone()),
            (victim, Fault::Misplaced { found: donor }),
            "{f}"
        ),
        other => panic!("misplacement not reported: {other:?}"),
    }
    // Beyond the file: missing.
    let end = run.pf().space().end;
    assert!(matches!(run.pf().read_raw(end + 3), Err(RawRead::Beyond)));
    assert_eq!(
        run.pf().read_page(end + 3, &Hash::default()).unwrap_err(),
        Fault::Missing
    );
    // A file that ends inside a page: a short read.
    let short = fs.create("short", FileOptions::PLAIN).unwrap();
    short.write_at(0, &vec![1u8; PAGE_SIZE + 100]).unwrap();
    assert!(matches!(
        raw(&*short, 1, 4),
        Err(RawRead::Short { read: 100 })
    ));
}

/// An open passes over a newest checkpoint whose root, meta stream or
/// both superblock copies are damaged, says why, and uses the one before
/// it, which is whole; the next checkpoint then takes the damaged one's
/// slots.
#[test]
fn an_open_passes_over_a_newest_checkpoint_it_cannot_check() {
    for case in 0..3 {
        let mut reached = Reached::default();
        let (sim, mut run) = quiet_run(10 + case, &mut reached);
        for _ in 0..8 {
            run.entry_all();
        }
        run.checkpoint(false);
        let older = run.confirmed;
        run.entry_all();
        run.checkpoint(false);
        let newest = run.confirmed;
        let kept = run.pf().kept_checkpoints();
        let root = run.pf().checkpoint_trees()[1].1;
        run.stop(false);
        match case {
            0 => sim.damage(FILE, root.page * PAGE + 64, 2).unwrap(),
            1 => sim.damage(FILE, kept[0].meta.0 * PAGE + 40, 2).unwrap(),
            _ => {
                let file = run.disk.fs().open(FILE, FileOptions::PLAIN).unwrap();
                for slot in 0..SLOTS {
                    let mut buf = vec![0u8; COPY];
                    file.read_at((slot * SLOT) as u64, &mut buf).unwrap();
                    if let Ok(superblock::Decoded::Valid(sb)) = superblock::decode(&buf) {
                        if sb.number == newest {
                            sim.damage(FILE, (slot * SLOT) as u64 + 30, 1).unwrap();
                        }
                    }
                }
            }
        }
        run.allow_fallback = true;
        run.reopen();
        assert_eq!(run.confirmed, older, "case {case}");
        let opened = run
            .trace
            .iter()
            .rev()
            .find(|l| l.starts_with("opened"))
            .unwrap()
            .clone();
        match case {
            0 => assert!(opened.contains("Root {"), "{opened}"),
            1 => assert!(opened.contains("Meta("), "{opened}"),
            _ => assert!(opened.contains("Invalid(Hash)"), "{opened}"),
        }
        if case == 2 {
            assert!(opened.contains("cleared: [0, 2]"), "{opened}");
        }
        run.check(true);
        run.entry_all();
        run.checkpoint(false);
        let made = (run.confirmed, run.newest.entry);
        run.stop(true);
        run.reopen();
        assert_eq!((run.confirmed, run.newest.entry), made, "case {case}");
        run.check(true);
    }
}

/// Past the budget of pages waiting in memory, a reservation is refused
/// with "checkpoint needed" until a checkpoint writes them; pages a pin
/// keeps after they left the newest version do not count.
#[test]
fn a_checkpoint_is_needed_past_the_budget_of_pages_in_memory() {
    let sim = Sim::new(21);
    let disk = Disk::Sim(sim.clone());
    let config = Config {
        dirty_budget: 12,
        ..config()
    };
    let proc = create_proc(&disk, config);
    let pf = &proc.pf;
    let mut model = Model::default();
    let mut rng = Rng::new(21);
    let mut index = 1;
    let apply = |model: &mut Model, ops: Vec<(TreeName, Op)>, index: &mut u64| {
        let logged = Logged {
            entry: entry_id(*index),
            ops,
            taken: None,
        };
        let pages = bound(pf, model, &logged.ops).unwrap();
        let res = pf.reserve(*index, pages, Purpose::Entry)?;
        apply_logged(&res, model, &logged).unwrap();
        *index += 1;
        Ok::<_, SpaceError>(())
    };
    apply(&mut model, vec![(tree_name(0), Op::Create)], &mut index).unwrap();
    let mut refused = false;
    for _ in 0..200 {
        let ops = vec![(tree_name(0), Op::Edits(edits(&mut rng, 6)))];
        match apply(&mut model, ops, &mut index) {
            Ok(()) => {}
            Err(SpaceError::CheckpointNeeded { dirty, budget }) => {
                assert!(dirty >= budget);
                refused = true;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(refused, "the budget was never reached");
    pf.checkpoint(model.entry, &model.roots(), None).unwrap();
    assert_eq!(pf.space().dirty, 0);
    // A pin keeps a version that is only in memory: when the tree is made
    // again, its pages leave the newest version and stay in memory for the
    // pin, and do not count against the budget.
    while pf.space().dirty < 10 {
        let ops = vec![(tree_name(0), Op::Edits(edits(&mut rng, 3)))];
        apply(&mut model, ops, &mut index).unwrap();
    }
    let pin = pf.pin(model.entry.index).unwrap();
    apply(&mut model, vec![(tree_name(0), Op::Recreate)], &mut index).unwrap();
    pf.checkpoint(model.entry, &model.roots(), None).unwrap();
    let kept = pf.space().dirty;
    assert!(kept >= 10, "the pinned pages stay in memory: {kept}");
    let mut over = false;
    for _ in 0..40 {
        let ops = vec![(tree_name(0), Op::Edits(edits(&mut rng, 2)))];
        match apply(&mut model, ops, &mut index) {
            Ok(()) => over |= pf.space().dirty >= config.dirty_budget,
            Err(SpaceError::CheckpointNeeded { dirty, .. }) => {
                assert!(dirty >= config.dirty_budget);
                pf.checkpoint(model.entry, &model.roots(), None).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(over, "reservations went on past the budget held by the pin");
    drop(pin);
    pf.reclaim(10_000);
    pf.checkpoint(model.entry, &model.roots(), None).unwrap();
    pf.reclaim(10_000);
    assert_eq!(pf.space().dirty, 0);
}

/// A unit dropped without a commit, or whose commit is refused, has had
/// no effect: the map is as it was, byte for byte, and every page it
/// released still reads.
#[test]
fn a_unit_dropped_or_refused_changes_nothing() {
    let mut reached = Reached::default();
    let (_sim, mut run) = quiet_run(22, &mut reached);
    for _ in 0..6 {
        run.entry_all();
    }
    run.checkpoint(false);
    let pf = run.pf();
    let (root, _) = run.newest.trees[&tree_name(0)].clone();
    let pages = tree::check(pf, root).unwrap().pages;
    assert!(pages.len() >= 2, "tree 0 has pages below its root");
    let (child, child_hash) = pages[1];
    let before = (pf.map_image(), pf.space());
    let index = run.next_index();
    let unit_page = |n: PageNo| {
        let mut b = crate::page::Builder::new(PageKind::Leaf, 0, index);
        b.item(crate::page::KIND_LIST, 3, 0, b"a\x00\x01", &[]);
        Arc::new(b.finish(n).unwrap())
    };
    {
        let res = pf.reserve(index, 4, Purpose::Entry).unwrap();
        // Dropped after allocating, writing and releasing.
        let mut unit = res.begin().unwrap();
        let n = unit.allocate().unwrap();
        unit.write(unit_page(n)).unwrap();
        unit.release(root.page, &root.hash, Release::Copied)
            .unwrap();
        unit.release(child, &child_hash, Release::Dropped).unwrap();
        drop(unit);
        assert_eq!(res.remaining(), 4);
        // Refused at commit: a page released twice.
        let mut unit = res.begin().unwrap();
        let n = unit.allocate().unwrap();
        unit.write(unit_page(n)).unwrap();
        unit.release(root.page, &root.hash, Release::Copied)
            .unwrap();
        unit.release(root.page, &root.hash, Release::Copied)
            .unwrap();
        assert!(unit.commit().is_err());
        assert_eq!(res.remaining(), 4);
    }
    assert!(
        (pf.map_image(), pf.space()) == before,
        "a unit that did not commit changed the map"
    );
    // Refused at commit: a checkpoint at the entry came in between.
    let res = pf.reserve(index, 4, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    unit.release(child, &child_hash, Release::Dropped).unwrap();
    let late = entry_id(index);
    let trees = run.newest.roots();
    let done = pf.checkpoint(late, &trees, None).unwrap();
    assert!(unit.commit().is_err());
    drop(res);
    let dying = pf.dying_pages();
    assert!(!dying.contains(&root.page) && !dying.contains(&child));
    run.newest.entry = late;
    run.checkpoints.insert(done.number, run.newest.clone());
    run.pred.insert(done.number, Some(run.confirmed));
    run.confirmed = done.number;
    run.check(true);
}

/// Review of #719 (P0): a restore point's name dropped and kept again names
/// a new restore point. The old one stays kept while the older checkpoint
/// on disk lists it: pages freed and reused meanwhile, a checkpoint cut
/// off after writing them, and an open that passes over the newest
/// checkpoint still read the first restore point whole.
#[test]
fn a_restore_point_name_kept_again_keeps_the_old_one_while_a_superblock_lists_it() {
    let mut reached = Reached::default();
    let marks = std::cell::Cell::new((0u64, 0u64, 0u64));
    let scenario = |run: &mut Run<'_>| {
        for _ in 0..6 {
            run.entry_all();
        }
        run.checkpoint(true);
        let first = run.confirmed;
        for _ in 0..6 {
            run.entry_all();
        }
        run.checkpoint(false);
        let older = run.confirmed;
        run.drop_restore_point();
        run.entry_all();
        run.checkpoint(true);
        let newest = run.confirmed;
        let listed = run.pf().restore_points();
        assert!(
            listed
                .iter()
                .filter(|(rp, _)| rp.name == "rp1")
                .map(|(rp, _)| rp.checkpoint)
                .eq([newest, first]),
            "both restore points named rp1 are kept: {listed:?}"
        );
        for _ in 0..20 {
            if run.pf().reclaim(100_000).drained {
                break;
            }
        }
        for _ in 0..10 {
            run.entry_all();
        }
        marks.set((first, older, newest));
    };
    let (before, end) = {
        let (sim, mut run) = quiet_run(31, &mut reached);
        scenario(&mut run);
        let before = sim.steps();
        run.checkpoint(false);
        assert!(run.proc.is_some());
        (before, sim.steps())
    };
    // The last four steps of a checkpoint write and flush its two copies:
    // cut it off at the first, after its pages are flushed.
    let (sim, mut run) = quiet_run(31, &mut reached);
    scenario(&mut run);
    assert_eq!(sim.steps(), before);
    let (first, older, newest) = marks.get();
    let root = run.checkpoints[&newest].trees[&tree_name(0)].0;
    sim.set_crash_fates(Fates::LoseAll);
    sim.place(end - 3, SimFault::Crash);
    run.checkpoint(false);
    assert_eq!(crashes(&sim), 1);
    if run.proc.is_some() {
        run.stop(false);
    }
    // The newest checkpoint's root damaged: the open takes the older one,
    // which lists the first rp1, and reads it (Run::check).
    sim.damage(FILE, root.page * PAGE + 64, 2).unwrap();
    run.allow_fallback = true;
    run.reopen();
    assert_eq!(run.confirmed, older);
    assert!(run
        .pf()
        .restore_points()
        .iter()
        .any(|(rp, _)| rp.name == "rp1" && rp.checkpoint == first));
    run.check(true);
}

/// Review of #719 (P1): recovery frees what normal running frees, so the
/// entries after a checkpoint, replayed, fit in the space the first run
/// had. The disk is filled to its limit, a tree dropped to make some room,
/// then a long tail of updates to a few keys with no checkpoint and no pin
/// (each update's old pages freed at once), a power loss, and the replay.
#[test]
fn replay_fits_in_the_space_the_first_run_had() {
    for seed in seeds(6) {
        let sim = Sim::new(seed);
        sim.set_capacity((6 + seed % 4) << 20);
        let mut reached = Reached::default();
        let mut run = Run::new(Disk::Sim(sim.clone()), Device::Honest, seed, &mut reached);
        run.replay_must_fit = true;
        for _ in 0..2_000 {
            let before = run.reached.full;
            let index = run.next_index();
            let edits = (0..4u8)
                .map(|i| {
                    Edit::leaf(
                        vec![b"big".to_vec(), [index as u8, i].to_vec()],
                        vec![i; 9_000],
                    )
                })
                .collect();
            run.append(Logged {
                entry: entry_id(index),
                ops: vec![(tree_name(index as usize % 2), Op::Edits(edits))],
                taken: None,
            });
            assert!(run.proc.is_some());
            if run.reached.full > before {
                break;
            }
        }
        assert!(run.reached.full > 0, "seed {seed}: the disk never filled");
        let index = run.next_index();
        run.append_for(
            Logged {
                entry: entry_id(index),
                ops: vec![(tree_name(0), Op::Recreate)],
                taken: None,
            },
            Purpose::Discard,
        );
        run.checkpoint(false);
        run.checkpoint(false);
        run.check(true);
        let mut rng = Rng::stream(seed, 5);
        let tail = 300;
        for _ in 0..tail {
            let index = run.next_index();
            let key = vec![b'k', rng.below(3) as u8];
            run.append(Logged {
                entry: entry_id(index),
                ops: vec![(
                    tree_name(2),
                    Op::Edits(vec![Edit::leaf(vec![key], value(&mut rng))]),
                )],
                taken: None,
            });
        }
        assert!(run.proc.is_some(), "seed {seed}: the tail fitted");
        let replayed = run.reached.replayed;
        run.stop(true);
        run.reopen();
        assert!(run.proc.is_some(), "seed {seed}: the store recovered");
        assert!(run.reached.replayed >= replayed + tail as u64);
        run.check(true);
    }
}

/// Review of #719 (P1): an open reads and checks each root and lets it go,
/// so a store of thousands of trees opens under a cache of four pages.
#[test]
fn a_store_of_many_trees_opens_under_a_small_cache() {
    let sim = Sim::new(40);
    let disk = Disk::Sim(sim.clone());
    let config = Config {
        cache_bytes: 4 * PAGE,
        ..config()
    };
    let proc = create_proc(&disk, config);
    let n = 3_000u64;
    let res = proc.pf.reserve(1, 3 * n, Purpose::Entry).unwrap();
    let mut trees: Vec<(TreeName, Root)> = (0..n)
        .map(|i| {
            let name = TreeName::new(format!("file/{i:05}")).unwrap();
            let root = res.create_tree(&name).unwrap();
            (name, root)
        })
        .collect();
    drop(res);
    trees.sort_by(|a, b| a.0.cmp(&b.0));
    proc.pf.checkpoint(entry_id(1), &trees, None).unwrap();
    drop(proc);
    sim.restart();
    let (proc, opened) = open_proc(&disk, config).unwrap();
    assert_eq!(opened.checkpoint, 2);
    assert!(proc.pf.space().cached <= 4);
    assert_eq!(proc.pf.checkpoint_trees(), trees);
    assert_eq!(proc.pf.tree_names().len(), n as usize);
    for (_, root) in trees.iter().step_by(500) {
        assert!(Tree::open(&proc.pf, *root)
            .unwrap()
            .get(&[b"x"])
            .unwrap()
            .is_none());
    }
    proc.pf.checkpoint(entry_id(1), &trees, None).unwrap();
    assert!(proc.pf.space().cached <= 4);
}

/// Review of #719 (P1): the room kept for a checkpoint counts the trees
/// that exist now, so trees made until reservations are refused still
/// leave room to checkpoint them all.
#[test]
fn trees_made_until_reservations_are_refused_still_leave_room_to_checkpoint() {
    let sim = Sim::new(41);
    sim.set_capacity(4 << 20);
    let disk = Disk::Sim(sim.clone());
    let config = Config {
        growth: 1,
        reserve: 0,
        ..config()
    };
    let proc = create_proc(&disk, config);
    let pf = &proc.pf;
    let mut trees = Vec::new();
    let mut index = 1;
    loop {
        let res = match pf.reserve(index, 3, Purpose::Entry) {
            Ok(res) => res,
            Err(SpaceError::Full { .. }) => break,
            Err(e) => panic!("{e}"),
        };
        let name = TreeName::new(format!("{index:0255}")).unwrap();
        match res.create_tree(&name) {
            Ok(root) => trees.push((name, root)),
            Err(tree::Error::Store(StoreError::Full)) => break,
            Err(e) => panic!("{e}"),
        }
        index += 1;
    }
    assert!(trees.len() > 100, "{} trees", trees.len());
    let done = pf.checkpoint(entry_id(index - 1), &trees, None).unwrap();
    assert!(done.meta_pages >= 3, "the table spans pages: {done:?}");
    // And again, with every tree's name in the table.
    pf.checkpoint(entry_id(index - 1), &trees, None).unwrap();
}

/// Review of #719 (P2): the map is a function of the checkpoint's map and
/// the calls made, never of the cache: the same schedule under caches of
/// 0, 4 and 512 pages gives the same map after every step.
#[test]
fn the_map_does_not_depend_on_the_cache() {
    for seed in seeds(3) {
        let mut runs = Vec::new();
        for pages in [0u64, 4, 512] {
            let mut reached = Reached::default();
            let config = Config {
                cache_bytes: pages * PAGE,
                ..config()
            };
            let mut run = Run::with(
                Disk::Sim(Sim::new(seed)),
                Device::Honest,
                seed,
                &mut reached,
                config,
            );
            run.images = Some(Vec::new());
            runs.push(run.go(steps(120), Rates::default()).unwrap());
        }
        for other in &runs[1..] {
            assert_eq!(other.len(), runs[0].len());
            if let Some(step) = (0..other.len()).find(|&i| other[i] != runs[0][i]) {
                panic!("seed {seed}: the maps differ after step {step}");
            }
        }
    }
}

/// The gate's state (armed, paused, released) and its signal.
type Gate = Arc<(Mutex<(bool, bool, bool)>, std::sync::Condvar)>;

/// An `Fs` over the simulator whose `pages` file, once armed, stops at its
/// next flush until released: it holds a checkpoint between its plan and
/// its install.
#[derive(Clone)]
struct Gated {
    inner: crate::sim::SimFs,
    gate: Gate,
}

struct GatedFile {
    inner: Box<dyn File>,
    gate: Gate,
}

impl File for GatedFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
        self.inner.read_at(offset, buf)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        self.inner.write_at(offset, data)
    }
    fn flush(&self) -> sys::Result<()> {
        let (state, cv) = &*self.gate;
        let mut s = state.lock().unwrap();
        // (armed, paused, released)
        if s.0 && !s.2 {
            s.1 = true;
            cv.notify_all();
            while !s.2 {
                s = cv.wait(s).unwrap();
            }
        }
        drop(s);
        self.inner.flush()
    }
    fn allocate(&self, size: u64) -> sys::Result<()> {
        self.inner.allocate(size)
    }
    fn size(&self) -> sys::Result<u64> {
        self.inner.size()
    }
}

impl Fs for Gated {
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
        self.inner.create(path, options)
    }
    fn open(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        let inner = self.inner.open(path, options)?;
        Ok(Box::new(GatedFile {
            inner,
            gate: self.gate.clone(),
        }))
    }
    fn create_folder(&self, path: &str) -> sys::Result<()> {
        self.inner.create_folder(path)
    }
    fn flush_folder(&self, path: &str) -> sys::Result<()> {
        self.inner.flush_folder(path)
    }
    fn rename(&self, from: &str, to: &str) -> sys::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove(&self, path: &str) -> sys::Result<()> {
        self.inner.remove(path)
    }
    fn list(&self, path: &str) -> sys::Result<Vec<String>> {
        self.inner.list(path)
    }
}

/// Review of #719 (P2): a restore point dropped while a checkpoint is being
/// written waits for it, and stays dropped: the checkpoint does not install
/// the list it started from over the drop.
#[test]
fn a_restore_point_dropped_during_a_checkpoint_stays_dropped() {
    let sim = Sim::new(42);
    let fs = Gated {
        inner: sim.fs(),
        gate: Arc::default(),
    };
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, config()).unwrap();
    let name = tree_name(0);
    let res = pf.reserve(1, 3, Purpose::Entry).unwrap();
    let root = res.create_tree(&name).unwrap();
    drop(res);
    let trees = vec![(name, root)];
    pf.checkpoint(entry_id(1), &trees, Some("a")).unwrap();
    pf.checkpoint(entry_id(1), &trees, None).unwrap();
    {
        let (state, _) = &*fs.gate;
        state.lock().unwrap().0 = true;
    }
    let dropped = std::thread::scope(|scope| {
        let writing = scope.spawn(|| pf.checkpoint(entry_id(1), &trees, None));
        let (state, cv) = &*fs.gate;
        let mut s = state.lock().unwrap();
        while !s.1 {
            s = cv.wait(s).unwrap();
        }
        drop(s);
        // The checkpoint holds its plan; the drop comes now, and must not
        // return before the checkpoint installs what it wrote.
        let dropping = scope.spawn(|| pf.drop_restore_point("a"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let early = dropping.is_finished();
        let mut s = state.lock().unwrap();
        s.2 = true;
        cv.notify_all();
        drop(s);
        writing.join().unwrap().unwrap();
        (early, dropping.join().unwrap())
    });
    assert_eq!(
        dropped,
        (false, Ok(())),
        "the drop waited for the checkpoint"
    );
    // The next checkpoint does not list it; the one after retires it.
    pf.checkpoint(entry_id(1), &trees, None).unwrap();
    pf.checkpoint(entry_id(1), &trees, None).unwrap();
    assert!(pf.restore_points().is_empty(), "{:?}", pf.restore_points());
}

/// Review of #719 (P2): a valid superblock copy at the highest checkpoint
/// number is a typed refusal before anything is written, never a wrap.
#[test]
fn a_checkpoint_number_at_its_limit_refuses_the_open() {
    let sim = Sim::new(43);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let res = proc.pf.reserve(1, 3, Purpose::Entry).unwrap();
    let root = res.create_tree(&tree_name(0)).unwrap();
    drop(res);
    proc.pf
        .checkpoint(entry_id(1), &[(tree_name(0), root)], None)
        .unwrap();
    let meta = proc.pf.kept_checkpoints()[0].meta;
    drop(proc);
    sim.restart();
    // Checkpoint 2 is in slots 1 and 3; a forged copy at the limit in
    // slot 0, valid, whose meta stream names another number.
    let forged = superblock::encode(&Superblock {
        number: u64::MAX,
        entry: entry_id(1),
        tree_format: page::FORMAT_VERSION,
        meta,
    });
    let fs = disk.fs();
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &forged).unwrap();
    file.flush().unwrap();
    let mut before = vec![0u8; PAGE_SIZE];
    file.read_at(0, &mut before).unwrap();
    drop(file);
    match open_proc(&disk, config()) {
        Err(OpenError::NumbersExhausted) => {}
        other => panic!("{:?}", other.map(|(_, o)| o)),
    }
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    let mut after = vec![0u8; PAGE_SIZE];
    file.read_at(0, &mut after).unwrap();
    assert!(before == after, "the open wrote nothing");
}

/// Review of #719 (P2): an allocation that fails other than for a full
/// disk (it can write) latches the page file's failure like a failed
/// write: every later reservation, growth and checkpoint is refused until
/// the page file is opened again. A full disk does not latch.
#[test]
fn a_failed_growth_latches_the_failure() {
    let sim = Sim::new(44);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let pf = &proc.pf;
    let res = pf.reserve(1, 3, Purpose::Entry).unwrap();
    let root = res.create_tree(&tree_name(0)).unwrap();
    drop(res);
    let trees = [(tree_name(0), root)];
    pf.checkpoint(entry_id(1), &trees, None).unwrap();
    // A full disk (more than the simulated disk's 1 GiB) refuses a
    // reservation and latches nothing.
    let big = 1 << 17;
    assert!(matches!(
        pf.reserve(2, big, Purpose::Entry),
        Err(SpaceError::Full { .. })
    ));
    assert!(!pf.failed());
    pf.checkpoint(entry_id(1), &trees, None).unwrap();
    // A failed allocation latches.
    sim.place(sim.steps() + 1, SimFault::FailWrite);
    assert!(matches!(
        pf.reserve(2, 64, Purpose::Entry),
        Err(SpaceError::Disk(_))
    ));
    assert!(pf.failed());
    assert!(matches!(
        pf.reserve(2, 1, Purpose::Entry),
        Err(SpaceError::Failed)
    ));
    assert!(matches!(
        pf.checkpoint(entry_id(1), &trees, None),
        Err(CheckpointError::Failed)
    ));
}

/// Review of #719 (P2): a dropped page that the maps on disk record
/// unsettled passes its hold to what it refers to. A tree of one leaf with
/// a spilled value is dropped; two checkpoints record the drop unsettled;
/// reclaim takes the leaf apart and then settles its overflow page, which
/// must stay held: a checkpoint cut off after writing reused pages, and an
/// open of the last checkpoint, still find the overflow page whole when
/// they take the drop apart again.
#[test]
fn a_held_page_passes_its_hold_to_what_it_refers_to() {
    let mut reached = Reached::default();
    let scenario = |run: &mut Run<'_>| {
        let index = run.next_index();
        run.append(Logged {
            entry: entry_id(index),
            ops: vec![(
                tree_name(0),
                Op::Edits(vec![Edit::leaf(vec![b"big".to_vec()], vec![7; 6_000])]),
            )],
            taken: None,
        });
        run.checkpoint(false);
        let index = run.next_index();
        run.append(Logged {
            entry: entry_id(index),
            ops: vec![(tree_name(0), Op::Recreate)],
            taken: None,
        });
        run.checkpoint(false);
        run.checkpoint(false);
        // The leaf, then its overflow page.
        for _ in 0..2 {
            let done = run.pf().reclaim(100_000);
            assert!(done.faults.is_empty() && done.twice.is_empty());
        }
        // New pages take the lowest free numbers.
        for _ in 0..4 {
            run.entry_all();
        }
    };
    let (before, end) = {
        let (sim, mut run) = quiet_run(45, &mut reached);
        scenario(&mut run);
        let before = sim.steps();
        run.checkpoint(false);
        assert!(run.proc.is_some());
        (before, sim.steps())
    };
    let (sim, mut run) = quiet_run(45, &mut reached);
    scenario(&mut run);
    assert_eq!(sim.steps(), before);
    sim.set_crash_fates(Fates::LoseAll);
    sim.place(end - 3, SimFault::Crash);
    run.checkpoint(false);
    assert_eq!(crashes(&sim), 1);
    if run.proc.is_some() {
        run.stop(false);
    }
    run.reopen();
    // Draining takes the drop apart again from the map on disk: every page
    // it reads must still be whole (an honest disk allows no other fault).
    run.check(true);
}
