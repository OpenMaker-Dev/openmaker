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
use crate::page::{Builder, MemStore, Numbering};
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
    /// The tree takes another name.
    Rename(TreeName),
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
            // A root page, and room for the name in the tables.
            Op::Create | Op::Recreate => 1 + TABLE_ROOM,
            Op::Rename(_) => TABLE_ROOM,
        };
    }
    Ok(total)
}

/// Applies `logged` through `res`, and to `model`.
fn apply_logged(res: &Reservation<'_>, model: &mut Model, logged: &Logged) -> Result<(), String> {
    apply_logged_with(res, model, logged, None)
}

/// Applies `logged` through one unit of `res`, the whole entry, committed
/// once; and to `model` when it commits. With `extend`, the reservation is
/// extended by that many pages before the entry's last tree.
fn apply_logged_with(
    res: &Reservation<'_>,
    model: &mut Model,
    logged: &Logged,
    extend: Option<u64>,
) -> Result<(), String> {
    let index = logged.entry.index;
    let mut next = model.clone();
    let mut unit = res.begin().map_err(|e| e.to_string())?;
    let last = logged.ops.len().saturating_sub(1);
    for (i, (name, op)) in logged.ops.iter().enumerate() {
        if let (Some(pages), true) = (extend, i == last) {
            res.extend(pages)
                .map_err(|e| format!("{EXTEND_REFUSED}: {e}"))?;
        }
        match op {
            Op::Edits(edits) => {
                let (root, content) = next.trees.get_mut(name).ok_or("no such tree")?;
                let new = tree::apply(res, &mut unit, Some(*root), index, edits).map_err(|e| {
                    if e == tree::Error::Store(StoreError::Full) {
                        panic!("entry {index}: the reservation from entry_bound was too small");
                    }
                    e.to_string()
                })?;
                if new != *root {
                    unit.set_root(name, new);
                }
                for e in edits {
                    model_apply(content, e);
                }
                *root = new;
            }
            Op::Create | Op::Recreate => {
                if matches!(op, Op::Recreate) {
                    unit.drop_tree(name);
                }
                let root =
                    tree::apply(res, &mut unit, None, index, &[]).map_err(|e| e.to_string())?;
                unit.add_tree(name, root);
                next.trees.insert(name.clone(), (root, Content::new()));
            }
            Op::Rename(to) => {
                unit.rename_tree(name, to);
                let tree = next.trees.remove(name).ok_or("no such tree")?;
                next.trees.insert(to.clone(), tree);
            }
        }
    }
    unit.commit().map_err(|e| e.to_string())?;
    next.entry = logged.entry;
    *model = next;
    Ok(())
}

/// Makes trees `names`, empty, as the whole of `res`'s entry: one unit,
/// committed. Each takes 5 pages of the reservation.
fn make_trees(res: &Reservation<'_>, names: &[TreeName]) -> Result<Vec<Root>, tree::Error> {
    let mut unit = res.begin()?;
    let mut roots = Vec::new();
    for name in names {
        let root = tree::apply(res, &mut unit, None, res.entry(), &[])?;
        unit.add_tree(name, root);
        roots.push(root);
    }
    unit.commit()?;
    Ok(roots)
}

fn make_tree(res: &Reservation<'_>, name: &TreeName) -> Result<Root, tree::Error> {
    Ok(make_trees(res, std::slice::from_ref(name))?[0])
}

/// Applies `edits` to tree `name`, as the whole of `res`'s entry.
fn edit_tree(res: &Reservation<'_>, name: &TreeName, edits: &[Edit]) -> Result<Root, tree::Error> {
    let root = res.root(name).expect("a tree of that name");
    let mut unit = res.begin()?;
    let new = tree::apply(res, &mut unit, Some(root), res.entry(), edits)?;
    if new != root {
        unit.set_root(name, new);
    }
    unit.commit()?;
    Ok(new)
}

/// Renames tree `from` to `to`, as the whole of `res`'s entry.
fn rename_tree(res: &Reservation<'_>, from: &TreeName, to: &TreeName) -> Result<(), StoreError> {
    let mut unit = res.begin()?;
    unit.rename_tree(from, to);
    unit.commit()
}

/// What a schedule's failed `Reservation::extend` reports.
const EXTEND_REFUSED: &str = "extend refused";

/// Whether `op` changes the set of trees.
fn names_trees(op: &Op) -> bool {
    !matches!(op, Op::Edits(_))
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

/// A running process: its page file and the store folder's lock; on the
/// simulator, its file system can stop a checkpoint after its first
/// superblock slot.
struct Process {
    pf: PageFile,
    _lock: FolderLock,
    _fs: Box<dyn Fs>,
    trip: Option<Arc<Mutex<Trip>>>,
}

impl Disk {
    /// A process's file system: on the simulator, one that can trip.
    fn proc_fs(&self) -> (Box<dyn Fs>, Option<Arc<Mutex<Trip>>>) {
        match self {
            Disk::Sim(sim) => {
                let fs = Gated::new(sim.fs());
                let trip = fs.trip.clone();
                (Box::new(fs), Some(trip))
            }
            Disk::Real(folder) => (Box::new(folder.fs()), None),
        }
    }
}

fn create_proc(disk: &Disk, config: Config) -> Process {
    let (fs, trip) = disk.proc_fs();
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&*fs, &lock, config).unwrap();
    Process {
        pf,
        _lock: lock,
        _fs: fs,
        trip,
    }
}

fn open_proc(disk: &Disk, config: Config) -> Result<(Process, Opened), OpenError> {
    let (fs, trip) = disk.proc_fs();
    let lock = fs.lock().map_err(OpenError::Disk)?;
    let (pf, opened) = PageFile::open(&*fs, &lock, config)?;
    Ok((
        Process {
            pf,
            _lock: lock,
            _fs: fs,
            trip,
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
    renamed: u64,
    same_entry: u64,
    /// Entries whose one unit changed the set of trees more than once.
    name_units: u64,
    /// Second units of an entry that committed one, refused.
    second_units: u64,
    /// Second reservations of an entry, refused.
    second_reservations: u64,
    /// Units while another was open (of the same entry or another),
    /// refused.
    other_entry_units: u64,
    /// A checkpoint and a reservation of its entry raced; of those, how
    /// many the reservation won (and the checkpoint was refused).
    races: u64,
    races_to_the_reservation: u64,
    /// Keeps stopped just after their first superblock slot; of those, how
    /// many raised the floor (the point crossed a page of the stream).
    keeps_stopped: u64,
    keeps_stopped_raising_the_floor: u64,
    /// Units naming an overflow page as a tree's root, refused at commit.
    bad_roots: u64,
    /// Entries reserved short and extended before their last tree.
    extended: u64,
    leak_checks: u64,
    /// Dying pages found held for a map on disk at a leak check.
    held: u64,
    read_faults: u64,
    fired: BTreeMap<SimFault, u64>,
}

impl Reached {
    /// The schedules reached every refusal of the contract (review of #719,
    /// third round). Which of a raced checkpoint and reservation wins, and
    /// whether a stopped keep raised the floor, are reported, not required.
    fn assert_contract(&self) {
        let reached = [
            self.name_units,
            self.second_units,
            self.second_reservations,
            self.other_entry_units,
            self.races,
            self.keeps_stopped,
            self.bad_roots,
            self.extended,
        ];
        assert!(reached.iter().all(|&n| n > 0), "{self:?}");
    }

    fn assert_fired(&self, rates: &Rates) {
        if std::env::var("OM_STORE_SEED").is_ok() {
            // One seed replayed need not reach every fault.
            return;
        }
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
    /// The faults the schedule draws.
    rates: Rates,
    /// Which entries are reserved short and extended: a stream of its own,
    /// so that the rest of a schedule does not depend on it.
    splits: Rng,
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
            rates: Rates::default(),
            splits: Rng::stream(seed, 13),
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
        self.rates = rates;
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
                71..=72 => self.drop_restore_point(),
                73..=74 => self.contract(),
                75..=81 => self.reclaim(),
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
        // Some entries of several trees are reserved short of their last
        // tree, and extended for it once the others are applied.
        let n = logged.ops.len();
        let split = if n >= 2 && self.splits.one_in(3) {
            bound(pf, &self.newest, &logged.ops[n - 1..]).ok()
        } else {
            None
        };
        let outcome = match pf.reserve(index, pages - split.unwrap_or(0), purpose) {
            Ok(res) => {
                self.trace.push(format!(
                    "entry {index}: {} trees, {pages} pages reserved (extended by {split:?})",
                    logged.ops.len()
                ));
                self.log.push(logged.clone());
                self.reached.entries += 1;
                if logged.ops.iter().any(|(_, op)| matches!(op, Op::Recreate)) {
                    self.reached.recreated += 1;
                }
                if logged.ops.iter().any(|(_, op)| matches!(op, Op::Rename(_))) {
                    self.reached.renamed += 1;
                }
                let changes: usize = logged
                    .ops
                    .iter()
                    .map(|(_, op)| match op {
                        Op::Recreate => 2,
                        op => usize::from(names_trees(op)),
                    })
                    .sum();
                if changes >= 2 {
                    self.reached.name_units += 1;
                }
                match apply_logged_with(&res, &mut self.newest, &logged, split) {
                    Ok(()) => {
                        if split.is_some() {
                            self.reached.extended += 1;
                        }
                        // One unit an entry: a second, once the first has
                        // committed, is refused.
                        let second = res.begin().err();
                        assert!(
                            matches!(&second, Some(StoreError::Failed(why)) if why.contains("has committed one")),
                            "a second unit of entry {index}: {second:?}"
                        );
                        self.reached.second_units += 1;
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
            Err(Err(e)) if e.starts_with(EXTEND_REFUSED) => {
                // Nothing applied (the unit was dropped); the process
                // stops, and the entry is applied again from the log.
                self.trace.push(format!("entry {index}: {e}"));
                self.fail();
            }
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
        let names: Vec<TreeName> = self.newest.trees.keys().cloned().collect();
        let n = (1 + self.plan.below(2) as usize).min(names.len());
        let first = self.plan.below(names.len() as u64) as usize;
        let ops = (0..n)
            .map(|i| {
                let name = names[(first + i) % names.len()].clone();
                let op = match self.plan.below(30) {
                    0 => Op::Recreate,
                    1 => {
                        self.names += 1;
                        Op::Rename(TreeName::new(format!("r{}", self.names)).unwrap())
                    }
                    _ => {
                        let count = 1 + self.plan.below(6);
                        Op::Edits(edits(&mut self.plan, count))
                    }
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

    /// One of the contract's refusals (review of #719, third round).
    fn contract(&mut self) {
        match self.plan.below(5) {
            0 => self.same_entry(),
            1 => self.one_at_a_time(),
            2 => self.race(),
            3 => self.bad_root(),
            _ => self.keep_stopped(),
        }
    }

    /// A second reservation of an entry is refused while the first is
    /// open, and a second unit while one is open, of the same entry or
    /// another; a unit dropped may be begun again.
    fn one_at_a_time(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let next = self.next_index();
        let Ok(first) = pf.reserve(next, 1, Purpose::Entry) else {
            return;
        };
        assert!(matches!(
            pf.reserve(next, 1, Purpose::Entry),
            Err(SpaceError::Refused(_))
        ));
        self.reached.second_reservations += 1;
        if let (Ok(later), Ok(unit)) = (pf.reserve(next + 1, 1, Purpose::Entry), first.begin()) {
            assert!(
                first.begin().is_err(),
                "a second unit of {next} while one is open"
            );
            assert!(
                later.begin().is_err(),
                "a unit of {} while one of {next} is open",
                next + 1
            );
            self.reached.other_entry_units += 1;
            drop(unit);
            assert!(first.begin().is_ok(), "a unit dropped is begun again");
        }
        self.trace
            .push(format!("one reservation and one entry's units at {next}"));
    }

    /// A checkpoint of the newest version and a reservation of its entry,
    /// asked for at once on two threads: one of them is refused. Either
    /// way a checkpoint of the newest version is taken (the refused one is
    /// asked for again), so the run does not depend on which won.
    fn race(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let space = pf.space();
        // Only where the reservation needs no reclaim (which reads) and no
        // growth: the other thread then does no I/O.
        if space.free.saturating_sub(space.promised) < space.floor + 2
            || space.dirty + 2 >= self.config.dirty_budget
        {
            return;
        }
        let at = self.newest.entry;
        let number = pf.next_checkpoint();
        self.checkpoints.insert(number, self.newest.clone());
        self.pred.insert(number, Some(self.confirmed));
        let start = std::sync::Barrier::new(2);
        let start = &start;
        let (reserved, result) = std::thread::scope(|scope| {
            let (done, wait) = std::sync::mpsc::channel::<()>();
            let racer = scope.spawn(move || {
                start.wait();
                let res = pf.reserve(at.index, 1, Purpose::Entry);
                let reserved = res.is_ok();
                // Held until the checkpoint has returned.
                let _ = wait.recv();
                drop(res);
                reserved
            });
            start.wait();
            let result = pf.checkpoint(at, None);
            done.send(()).unwrap();
            (racer.join().unwrap(), result)
        });
        self.reached.races += 1;
        self.trace.push(format!(
            "race at {}: reserved {reserved}, checkpoint {result:?}",
            at.index
        ));
        if reserved {
            assert!(
                matches!(result, Err(CheckpointError::Refused(_))),
                "a checkpoint and a reservation of its entry both succeeded: {result:?}"
            );
            self.reached.races_to_the_reservation += 1;
            self.checkpoint_as(None);
        } else {
            self.record_checkpoint(number, None, result);
        }
    }

    /// A unit that names an overflow page as a tree's root is refused at
    /// commit, whole: nothing of it takes effect, and the entry is not
    /// taken (its reservation is given back).
    fn bad_root(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let index = self.next_index();
        let Ok(res) = pf.reserve(index, 1 + TABLE_ROOM, Purpose::Entry) else {
            return;
        };
        let Ok(mut unit) = res.begin() else {
            return;
        };
        let Ok(n) = unit.allocate() else {
            return;
        };
        let mut builder = page::Builder::new(PageKind::Overflow, 0, index);
        builder.data(b"not a tree");
        let page = builder.finish(n).unwrap();
        let root = Root {
            page: n,
            hash: page.hash(),
        };
        unit.write(Arc::new(page)).unwrap();
        unit.add_tree(&TreeName::new(format!("bad{index}")).unwrap(), root);
        let before = pf.trees();
        let refused = unit.commit();
        assert!(
            matches!(&refused, Err(StoreError::Failed(why)) if why.contains("not a tree's page")),
            "an overflow page named as a root: {refused:?}"
        );
        assert_eq!(pf.trees(), before, "the refused unit changed nothing");
        self.reached.bad_roots += 1;
        self.trace
            .push(format!("an overflow root refused at {index}"));
    }

    /// A checkpoint that keeps a restore point, of a name of a drawn length,
    /// stops just after its first superblock slot; the store opens on it.
    fn keep_stopped(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let Some(trip) = proc.trip.clone() else {
            return;
        };
        let floor = proc.pf.space().floor;
        self.names += 1;
        // A name of a drawn length (padded), unique in the run.
        let len = 1 + self.plan.below(MAX_RESTORE_NAME as u64) as usize;
        let mut name = format!("k{}", self.names);
        while name.len() < len {
            name.push('x');
        }
        {
            let mut t = trip.lock().unwrap();
            t.stop_after_slot = true;
            t.slot_written = false;
        }
        self.checkpoint_as(Some(name));
        let stopped = trip.lock().unwrap().stopped;
        if let Some(proc) = self.proc.as_ref() {
            // Refused or failed before its first slot, in a process that
            // goes on: the trip is put away.
            assert!(!stopped);
            *proc.trip.as_ref().unwrap().lock().unwrap() = Trip::default();
            return;
        }
        if stopped {
            self.reached.keeps_stopped += 1;
            self.reopen();
            if let Some(proc) = self.proc.as_ref() {
                if proc.pf.space().floor > floor {
                    self.reached.keeps_stopped_raising_the_floor += 1;
                }
            }
        }
    }

    /// A checkpoint is of one exact version (review of #719, second round):
    /// a reservation at or before the newest checkpoint's entry is refused,
    /// and a checkpoint is refused while a reservation for an entry it
    /// would cover is open.
    fn same_entry(&mut self) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let (_, at) = pf.checkpoint_entry();
        if at.index > 0 {
            assert!(
                matches!(
                    pf.reserve(at.index, 1, Purpose::Entry),
                    Err(SpaceError::Refused(_))
                ),
                "a reservation at the checkpoint's entry {}",
                at.index
            );
        }
        let next = self.next_index();
        let held = pf.reserve(next, 1, Purpose::Entry);
        if held.is_ok() {
            let refused = pf.checkpoint(entry_id(next), None);
            assert!(
                matches!(refused, Err(CheckpointError::Refused(_))),
                "a checkpoint at {next} with its reservation open: {refused:?}"
            );
        }
        self.reached.same_entry += 1;
        self.trace.push(format!("same-entry checks at {next}"));
    }

    /// An entry that changes all three trees.
    fn entry_all(&mut self) {
        let index = self.log.last().map_or(1, |l| l.entry.index + 1);
        let names: Vec<TreeName> = self.newest.trees.keys().cloned().collect();
        let ops = names
            .into_iter()
            .map(|name| {
                let count = 2 + self.plan.below(4);
                (name, Op::Edits(edits(&mut self.plan, count)))
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
        self.checkpoint_as(name);
    }

    /// A checkpoint of the newest version, kept as restore point `name`
    /// when given.
    fn checkpoint_as(&mut self, name: Option<String>) {
        let Some(proc) = self.proc.as_ref() else {
            return;
        };
        let pf = &proc.pf;
        let number = pf.next_checkpoint();
        self.checkpoints.insert(number, self.newest.clone());
        self.pred.insert(number, Some(self.confirmed));
        let result = pf.checkpoint(self.newest.entry, name.as_deref());
        self.record_checkpoint(number, name, result);
    }

    /// What checkpoint `number` of the newest version, kept as `name` when
    /// given, gave.
    fn record_checkpoint(
        &mut self,
        number: u64,
        name: Option<String>,
        result: Result<Checkpointed, CheckpointError>,
    ) {
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
        // The trees the page file holds are the newest version's.
        assert_eq!(
            pf.trees(),
            self.newest.roots(),
            "the trees the page file holds"
        );
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
        assert!(reached.renamed > 0 && reached.same_entry > 0, "{reached:?}");
        reached.assert_contract();
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
    if std::env::var("OM_STORE_SEED").is_err() {
        assert!(reached.renamed > 0 && reached.same_entry > 0, "{reached:?}");
        reached.assert_contract();
    }
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
        let checkpoint = proc.pf.checkpoint(model.entry, None).unwrap();
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
    run.pf().clone_for_test(&tree_name(3), root);
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
    let in_unit = |root, entry, edits: &[Edit]| {
        let mut unit = store.begin().unwrap();
        let new = tree::apply(&store, &mut unit, root, entry, edits).unwrap();
        unit.commit().unwrap();
        new
    };
    let mut root = in_unit(None, 1, &[]);
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
        root = in_unit(Some(root), entry, &edits);
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
    pf.checkpoint(model.entry, None).unwrap();
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
    pf.checkpoint(model.entry, None).unwrap();
    let kept = pf.space().dirty;
    assert!(kept >= 10, "the pinned pages stay in memory: {kept}");
    let mut over = false;
    for _ in 0..40 {
        let ops = vec![(tree_name(0), Op::Edits(edits(&mut rng, 2)))];
        match apply(&mut model, ops, &mut index) {
            Ok(()) => over |= pf.space().dirty >= config.dirty_budget,
            Err(SpaceError::CheckpointNeeded { dirty, .. }) => {
                assert!(dirty >= config.dirty_budget);
                pf.checkpoint(model.entry, None).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(over, "reservations went on past the budget held by the pin");
    drop(pin);
    pf.reclaim(10_000);
    pf.checkpoint(model.entry, None).unwrap();
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
    // Review of #719 (second round, P0): a checkpoint is of one exact
    // version. It is refused while a reservation for an entry it covers is
    // open; once it has fixed its entry, a reservation at or before it is
    // refused, so no unit of a covered entry commits after it.
    let res = pf.reserve(index, 4, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    unit.release(child, &child_hash, Release::Dropped).unwrap();
    let late = entry_id(index);
    assert!(matches!(
        pf.checkpoint(late, None),
        Err(CheckpointError::Refused(_))
    ));
    drop(unit);
    drop(res);
    let done = pf.checkpoint(late, None).unwrap();
    assert!(matches!(
        pf.reserve(index, 4, Purpose::Entry),
        Err(SpaceError::Refused(_))
    ));
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
        // Restore points kept, to be dropped during the tail with no
        // checkpoint after: the room a reservation leaves must not shrink.
        run.checkpoint_as(Some(format!("a{seed}")));
        run.checkpoint_as(Some(format!("b{seed}")));
        run.checkpoint(false);
        run.check(true);
        let mut rng = Rng::stream(seed, 5);
        let tail = 300;
        for step in 0..tail {
            if step == tail / 3 || step == 2 * tail / 3 {
                let name = if step == tail / 3 { "a" } else { "b" };
                let name = format!("{name}{seed}");
                run.pf().drop_restore_point(&name).unwrap();
                run.restore.remove(&name);
            }
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
    let res = proc.pf.reserve(1, 5 * n, Purpose::Entry).unwrap();
    let names: Vec<TreeName> = (0..n)
        .map(|i| TreeName::new(format!("file/{i:05}")).unwrap())
        .collect();
    let roots = make_trees(&res, &names).unwrap();
    let mut trees: Vec<(TreeName, Root)> = names.into_iter().zip(roots).collect();
    drop(res);
    trees.sort_by(|a, b| a.0.cmp(&b.0));
    proc.pf.checkpoint(entry_id(1), None).unwrap();
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
    proc.pf.checkpoint(entry_id(1), None).unwrap();
    assert!(proc.pf.space().cached <= 4);
}

/// Review of #719 (P1): the room kept for a checkpoint counts the trees
/// that exist now, so trees made until reservations are refused still
/// leave room to checkpoint them all.
#[test]
fn trees_made_until_reservations_are_refused_still_leave_room_to_checkpoint() {
    let sim = Sim::new(41);
    sim.set_capacity(24 << 20);
    let disk = Disk::Sim(sim.clone());
    let config = Config {
        growth: 1,
        reserve: 0,
        dirty_budget: 1 << 20,
        ..config()
    };
    let proc = create_proc(&disk, config);
    let pf = &proc.pf;
    let mut trees = Vec::new();
    let mut index = 1;
    loop {
        let res = match pf.reserve(index, 5, Purpose::Entry) {
            Ok(res) => res,
            Err(SpaceError::Full { .. }) => break,
            Err(e) => panic!("{e}"),
        };
        let name = TreeName::new(format!("{index:0255}")).unwrap();
        match make_tree(&res, &name) {
            Ok(root) => trees.push((name, root)),
            Err(tree::Error::Store(StoreError::Full)) => break,
            Err(e) => panic!("{e}"),
        }
        index += 1;
    }
    assert!(trees.len() > 1_000, "{} trees", trees.len());
    let done = pf.checkpoint(entry_id(index - 1), None).unwrap();
    assert!(done.meta_pages >= 20, "the table spans pages: {done:?}");
    // And again, with every tree's name in the table.
    pf.checkpoint(entry_id(index - 1), None).unwrap();
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

/// What a `Gated` file does besides passing calls through.
#[derive(Default)]
struct Trip {
    /// Superblock copies are watched: once one is written (and so
    /// flushed before the next write), every later write fails as if the
    /// process had stopped there.
    stop_after_slot: bool,
    slot_written: bool,
    stopped: bool,
    /// The next write panics, as a `File` implementation might.
    panic_write: bool,
    /// Reads of this page fail, as a device's would.
    fail_read: Option<PageNo>,
}

/// An `Fs` over the simulator whose `pages` file, once armed, stops at its
/// next flush until released: it holds a checkpoint between its plan and
/// its install. It can also stop a checkpoint after its first superblock
/// slot, or panic.
#[derive(Clone)]
struct Gated {
    inner: crate::sim::SimFs,
    gate: Gate,
    trip: Arc<Mutex<Trip>>,
}

impl Gated {
    fn new(inner: crate::sim::SimFs) -> Self {
        Gated {
            inner,
            gate: Arc::default(),
            trip: Arc::default(),
        }
    }

    fn wrap(&self, inner: Box<dyn File>) -> Box<dyn File> {
        Box::new(GatedFile {
            inner,
            gate: self.gate.clone(),
            trip: self.trip.clone(),
        })
    }

    /// Arms the gate, at the next flush.
    fn arm(&self) {
        let (state, _) = &*self.gate;
        *state.lock().unwrap() = (true, false, false);
    }

    /// Waits until the call `call` runs has stopped at the gate; a test
    /// fails, rather than waits for good, when the call ends first.
    fn paused<T: std::fmt::Debug>(&self, call: &std::thread::ScopedJoinHandle<'_, T>) {
        let (state, cv) = &*self.gate;
        let mut s = state.lock().unwrap();
        while !s.1 {
            assert!(
                !call.is_finished(),
                "the call ended without reaching the gate"
            );
            s = cv
                .wait_timeout(s, std::time::Duration::from_millis(20))
                .unwrap()
                .0;
        }
    }

    fn release(&self) {
        let (state, cv) = &*self.gate;
        state.lock().unwrap().2 = true;
        cv.notify_all();
    }
}

/// Releases the gate when dropped, so that a test that fails while a call
/// waits at the gate ends instead of waiting for it.
struct Opens<'a>(&'a Gated);

impl Drop for Opens<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct GatedFile {
    inner: Box<dyn File>,
    gate: Gate,
    trip: Arc<Mutex<Trip>>,
}

impl GatedFile {
    fn wait(&self) {
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
    }
}

impl File for GatedFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
        if let Some(n) = self.trip.lock().unwrap().fail_read {
            let at = n * PAGE;
            if offset <= at && at < offset + buf.len() as u64 {
                return Err(sys::Error::new(ErrorKind::Read, sys::Op::Read, "pages"));
            }
        }
        self.inner.read_at(offset, buf)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        let mut t = self.trip.lock().unwrap();
        if t.panic_write {
            t.panic_write = false;
            drop(t);
            panic!("a write that panics");
        }
        let slot = offset < (SLOTS * SLOT) as u64;
        if t.stopped || (t.stop_after_slot && slot && t.slot_written) {
            t.stopped = true;
            return Err(sys::Error::new(ErrorKind::Halted, sys::Op::Write, "pages"));
        }
        if t.stop_after_slot && slot {
            t.slot_written = true;
        }
        drop(t);
        self.inner.write_at(offset, data)
    }
    fn flush(&self) -> sys::Result<()> {
        self.wait();
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
        Ok(self.wrap(self.inner.create(path, options)?))
    }
    fn open(&self, path: &str, options: FileOptions) -> sys::Result<Box<dyn File>> {
        Ok(self.wrap(self.inner.open(path, options)?))
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
    let fs = Gated::new(sim.fs());
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, config()).unwrap();
    let name = tree_name(0);
    let res = pf.reserve(1, 5, Purpose::Entry).unwrap();
    make_tree(&res, &name).unwrap();
    drop(res);
    pf.checkpoint(entry_id(1), Some("a")).unwrap();
    pf.checkpoint(entry_id(1), None).unwrap();
    {
        let (state, _) = &*fs.gate;
        state.lock().unwrap().0 = true;
    }
    let dropped = std::thread::scope(|scope| {
        let writing = scope.spawn(|| pf.checkpoint(entry_id(1), None));
        fs.paused(&writing);
        let (state, cv) = &*fs.gate;
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
    pf.checkpoint(entry_id(1), None).unwrap();
    pf.checkpoint(entry_id(1), None).unwrap();
    assert!(pf.restore_points().is_empty(), "{:?}", pf.restore_points());
}

/// Review of #719 (P2): a valid superblock copy at the highest checkpoint
/// number is a typed refusal before anything is written, never a wrap.
#[test]
fn a_checkpoint_number_at_its_limit_refuses_the_open() {
    let sim = Sim::new(43);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let res = proc.pf.reserve(1, 5, Purpose::Entry).unwrap();
    make_tree(&res, &tree_name(0)).unwrap();
    drop(res);
    proc.pf.checkpoint(entry_id(1), None).unwrap();
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
    let res = pf.reserve(1, 5, Purpose::Entry).unwrap();
    make_tree(&res, &tree_name(0)).unwrap();
    drop(res);
    pf.checkpoint(entry_id(1), None).unwrap();
    // A full disk (more than the simulated disk's 1 GiB) refuses a
    // reservation and latches nothing.
    let big = 1 << 17;
    assert!(matches!(
        pf.reserve(2, big, Purpose::Entry),
        Err(SpaceError::Full { .. })
    ));
    assert!(!pf.failed());
    pf.checkpoint(entry_id(1), None).unwrap();
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
        pf.checkpoint(entry_id(1), None),
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

/// Review of #719 (second round, P0): while a checkpoint at entry E is
/// being written, a reservation at E is refused, and units of later
/// entries go on; the checkpoint opened afterwards is exactly version E.
#[test]
fn a_checkpoint_being_written_takes_no_unit_of_its_entry() {
    let sim = Sim::new(46);
    let fs = Gated::new(sim.fs());
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, config()).unwrap();
    let name = tree_name(0);
    let res = pf.reserve(1, 5, Purpose::Entry).unwrap();
    let root = make_tree(&res, &name).unwrap();
    drop(res);
    let edits = |value: u8| [Edit::leaf(vec![b"k".to_vec()], vec![value; 10])];
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let _ = root;
    let at_two = edit_tree(&res, &name, &edits(2)).unwrap();
    drop(res);
    // Entry 3's reservation is taken before the checkpoint (one short of
    // space would wait for the checkpoint's writes); its unit commits while
    // they are under way.
    let later = pf.reserve(3, 64, Purpose::Entry).unwrap();
    {
        let (state, _) = &*fs.gate;
        state.lock().unwrap().0 = true;
    }
    let at_three = std::thread::scope(|scope| {
        let pf = &pf;
        let writing = scope.spawn(move || pf.checkpoint(entry_id(2), None));
        fs.paused(&writing);
        let (state, cv) = &*fs.gate;
        let release = || {
            let mut s = state.lock().unwrap();
            s.2 = true;
            cv.notify_all();
        };
        // The checkpoint has fixed entry 2: nothing of entry 2 is taken
        // now, and entry 3 goes on. A reservation that waited for the
        // checkpoint instead of being refused would wait for good: it is
        // made on a thread of its own and given 20 seconds.
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let refused = matches!(
                pf.reserve(2, 4, Purpose::Entry),
                Err(SpaceError::Refused(_))
            );
            let _ = tx.send(refused);
        });
        let refused = rx.recv_timeout(std::time::Duration::from_secs(20));
        if refused != Ok(true) {
            release();
            panic!("a reservation at the entry being checkpointed: {refused:?}");
        }
        let at_three = edit_tree(&later, &name, &edits(3)).unwrap();
        release();
        writing.join().unwrap().unwrap();
        at_three
    });
    drop(later);
    assert_eq!(pf.kept_checkpoints()[0].entry, entry_id(2));
    let read = |pf: &PageFile, root| Tree::open(pf, root).unwrap().get(&[b"k"]).unwrap();
    assert_eq!(read(&pf, at_three), Some(Node::Value(vec![3; 10])));
    drop(pf);
    sim.restart();
    let (pf, opened) = PageFile::open(&sim.fs(), &sim.fs().lock().unwrap(), config()).unwrap();
    assert_eq!(opened.entry, entry_id(2));
    assert_eq!(pf.checkpoint_trees(), vec![(name, at_two)]);
    assert_eq!(read(&pf, at_two), Some(Node::Value(vec![2; 10])));
}

/// Review of #719 (second round, P1): a rename is a unit of its entry. A
/// checkpoint at the entry before it is refused (a unit of a later entry
/// was committed), and after a restart the rename is applied again from
/// the log.
#[test]
fn a_rename_is_ordered_with_its_entry_and_replayed() {
    let mut reached = Reached::default();
    let (_sim, mut run) = quiet_run(48, &mut reached);
    for _ in 0..3 {
        run.entry_all();
    }
    run.checkpoint(false);
    let before = run.newest.clone();
    let index = run.next_index();
    let renamed = TreeName::new(b"renamed".to_vec()).unwrap();
    run.append(Logged {
        entry: entry_id(index),
        ops: vec![(tree_name(0), Op::Rename(renamed.clone()))],
        taken: None,
    });
    assert!(run.newest.trees.contains_key(&renamed));
    // The checkpoint of the entry before the rename is refused: a unit of
    // a later entry has committed.
    assert!(matches!(
        run.pf().checkpoint(before.entry, None),
        Err(CheckpointError::Refused(_))
    ));
    run.stop(true);
    run.reopen();
    assert!(run.pf().tree_names().contains(&renamed));
    run.checkpoint(false);
    run.stop(true);
    run.reopen();
    assert!(run
        .pf()
        .checkpoint_trees()
        .iter()
        .any(|(n, _)| *n == renamed));
    run.check(true);
}

/// Review of #719 (second round, P1): dropping restore points without a
/// checkpoint does not lower the room a reservation leaves (their pages are
/// still protected by the superblocks on disk), so an entry that just fits
/// before a restart fits when it is applied again after one.
#[test]
fn replay_after_restore_points_are_dropped_fits() {
    let sim = Sim::new(47);
    let mut reached = Reached::default();
    let config = Config {
        reserve: 0,
        ..config()
    };
    let mut run = Run::with(
        Disk::Sim(sim.clone()),
        Device::Honest,
        47,
        &mut reached,
        config,
    );
    run.replay_must_fit = true;
    // The longest names, so that the records a reopened page file would
    // count twice span more pages than any rounding hides.
    for i in 0..250 {
        run.entry();
        run.checkpoint_as(Some(format!("p{i:063}")));
    }
    run.checkpoint(false);
    run.checkpoint(false);
    for _ in 0..20 {
        if run.pf().reclaim(1 << 20).drained {
            break;
        }
    }
    // The disk holds the file as it is, and no more.
    sim.set_capacity(run.pf().space().end * PAGE);
    for (rp, _) in run.pf().restore_points() {
        run.pf().drop_restore_point(&rp.name).unwrap();
        run.restore.remove(&rp.name);
    }
    // An entry taking exactly what may be taken now: trees made (5 pages
    // each) and renamed (4 each).
    let space = run.pf().space();
    let avail = space.free - space.promised - space.floor;
    assert!(avail >= 20, "{space:?}");
    let made = avail % 4;
    let renames = (avail - 5 * made) / 4;
    let index = run.next_index();
    let mut ops: Vec<(TreeName, Op)> = (0..made)
        .map(|i| (TreeName::new(format!("n{i}")).unwrap(), Op::Create))
        .collect();
    let mut current = run.newest.trees.keys().next().unwrap().clone();
    for i in 0..renames {
        let to = TreeName::new(format!("m{i}")).unwrap();
        ops.push((current, Op::Rename(to.clone())));
        current = to;
    }
    let entries = run.reached.entries;
    run.append(Logged {
        entry: entry_id(index),
        ops,
        taken: None,
    });
    assert_eq!(run.reached.entries, entries + 1, "the entry fitted");
    assert_eq!(run.log.last().unwrap().taken, Some(avail));
    run.stop(true);
    run.reopen();
    assert!(run.proc.is_some());
    run.check(true);
}

/// Review of #719 (second round, P1): the writer keeps every limit the
/// reader enforces, by the same constants: restore points and trees past
/// the limit are refused before anything is written, names past theirs
/// cannot be made, a configuration that cannot be honoured is refused, and
/// what was written opens.
#[test]
fn the_writer_keeps_every_limit_the_reader_enforces() {
    let sim = Sim::new(49);
    let disk = Disk::Sim(sim.clone());
    assert!(matches!(
        PageFile::create(
            &*disk.fs(),
            &disk.fs().lock().unwrap(),
            Config {
                reserve: u64::MAX,
                ..config()
            }
        ),
        Err(OpenError::Refused(_))
    ));
    let proc = create_proc(&disk, config());
    let pf = &proc.pf;
    // Trees: as many as a checkpoint names, and no more.
    let res = pf
        .reserve(1, 5 * MAX_TREES as u64 + 5, Purpose::Entry)
        .unwrap();
    let mut names: Vec<TreeName> = (0..MAX_TREES)
        .map(|i| TreeName::new(format!("t{i:05}")).unwrap())
        .collect();
    names.push(TreeName::new(b"one-more".to_vec()).unwrap());
    assert!(make_trees(&res, &names).is_err(), "one tree too many");
    names.pop();
    make_trees(&res, &names).unwrap();
    drop(res);
    pf.checkpoint(entry_id(1), None).unwrap();
    drop(proc);
    sim.restart();
    let (proc, _) = open_proc(&disk, config()).unwrap();
    assert_eq!(proc.pf.tree_names().len(), MAX_TREES);
    drop(proc);

    // Restore points, on a store of one tree: as many as a checkpoint
    // lists, and no more.
    let sim = Sim::new(51);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let pf = &proc.pf;
    let res = pf.reserve(1, 5, Purpose::Entry).unwrap();
    make_tree(&res, &tree_name(0)).unwrap();
    drop(res);
    // Names: the longest there are, and no longer.
    assert!(TreeName::new(vec![b'x'; TreeName::MAX + 1]).is_none());
    let long = "x".repeat(MAX_RESTORE_NAME + 1);
    assert!(matches!(
        pf.checkpoint(entry_id(1), Some(&long)),
        Err(CheckpointError::Refused(_))
    ));
    for i in 0..MAX_RESTORE_POINTS {
        pf.checkpoint(entry_id(1), Some(&format!("p{i}"))).unwrap();
    }
    let next = pf.next_checkpoint();
    assert!(matches!(
        pf.checkpoint(entry_id(1), Some("one-more")),
        Err(CheckpointError::Refused(_))
    ));
    assert_eq!(pf.next_checkpoint(), next, "nothing was written");
    pf.checkpoint(entry_id(1), None).unwrap();
    drop(proc);
    sim.restart();
    let (proc, _) = open_proc(&disk, config()).unwrap();
    assert_eq!(proc.pf.restore_points().len(), MAX_RESTORE_POINTS);
}

/// Review of #719 (second round, answer 5): a reclaim reads a dying page
/// from the file even when it is cached, so a read error stops that page
/// the same whether or not a reader cached it: the page stays dying,
/// unsettled, the error is reported, and the next reclaim settles it.
#[test]
fn a_read_error_during_reclaim_does_not_depend_on_the_cache() {
    let mut reached = Reached::default();
    let (sim, mut run) = quiet_run(50, &mut reached);
    run.entry_all();
    run.checkpoint(false);
    let (old, _) = run.newest.trees[&tree_name(0)].clone();
    let index = run.next_index();
    run.append(Logged {
        entry: entry_id(index),
        ops: vec![(tree_name(0), Op::Recreate)],
        taken: None,
    });
    run.checkpoint(false);
    // A reader caches the dropped root.
    run.pf().read_page(old.page, &old.hash).unwrap();
    assert!(run.pf().dying_pages().contains(&old.page));
    sim.place(sim.steps() + 1, SimFault::FailRead);
    let done = run.pf().reclaim(1 << 20);
    assert!(
        done.faults.iter().any(|f| f.page == old.page),
        "the read failed although the page was cached: {done:?}"
    );
    assert!(run.pf().dying_pages().contains(&old.page));
    let done = run.pf().reclaim(1 << 20);
    assert!(done.faults.is_empty(), "{done:?}");
    run.check(true);
}

/// A page file on a `Gated` file system over `sim`, with the given trees
/// made at entry 1 and checkpointed there.
fn gated(
    sim: &Sim,
    config: Config,
    names: &[TreeName],
) -> (Gated, PageFile, Vec<(TreeName, Root)>) {
    let fs = Gated::new(sim.fs());
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, config).unwrap();
    let res = pf
        .reserve(1, 5 * names.len() as u64, Purpose::Entry)
        .unwrap();
    let roots = make_trees(&res, names).unwrap();
    let trees: Vec<(TreeName, Root)> = names.iter().cloned().zip(roots).collect();
    drop(res);
    pf.checkpoint(entry_id(1), None).unwrap();
    drop(lock);
    (fs, pf, trees)
}

/// One reservation an entry, and one unit an entry, one at a time: a
/// second unit while one is open is refused, of the same entry or another;
/// a unit dropped may be begun again; once an entry's unit has committed,
/// no other unit of it begins.
#[test]
fn one_reservation_and_one_unit_an_entry() {
    let sim = Sim::new(55);
    let (_fs, pf, _) = gated(&sim, config(), &[tree_name(0)]);
    let first = pf.reserve(2, 4, Purpose::Entry).unwrap();
    assert!(matches!(
        pf.reserve(2, 4, Purpose::Entry),
        Err(SpaceError::Refused(_))
    ));
    let next = pf.reserve(3, 4, Purpose::Entry).unwrap();
    let unit = first.begin().unwrap();
    assert!(
        first.begin().is_err(),
        "a second unit of entry 2 while one is open"
    );
    assert!(
        next.begin().is_err(),
        "a unit of entry 3 while one of 2 is open"
    );
    drop(unit);
    // A unit dropped may be begun again; once one has committed, no other.
    let renamed = TreeName::new(b"renamed".to_vec()).unwrap();
    rename_tree(&first, &tree_name(0), &renamed).unwrap();
    let again = first.begin().err();
    assert!(
        matches!(&again, Some(StoreError::Failed(why)) if why.contains("has committed one")),
        "{again:?}"
    );
    let back = TreeName::new(b"back".to_vec()).unwrap();
    rename_tree(&next, &renamed, &back).unwrap();
    assert_eq!(pf.tree_names(), vec![back]);
    drop((first, next));
    // A reservation dropped, the entry may be reserved again.
    let again = pf.reserve(4, 4, Purpose::Entry).unwrap();
    drop(again);
    pf.reserve(4, 4, Purpose::Entry).unwrap();
}

/// Review of #719 (third round, P1 4): a checkpoint left by a panic (a
/// `File` that panics while it writes) latches the page file's failure:
/// what its plan changed in memory is not used again; reservations and
/// checkpoints are refused until the page file is opened again, which
/// finds the checkpoint before it.
#[test]
fn a_checkpoint_left_by_a_panic_latches_the_failure() {
    let sim = Sim::new(56);
    let (fs, pf, trees) = gated(&sim, config(), &[tree_name(0)]);
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let edits = [Edit::leaf(vec![b"k".to_vec()], vec![2; 10])];
    let _ = trees;
    edit_tree(&res, &tree_name(0), &edits).unwrap();
    drop(res);
    fs.trip.lock().unwrap().panic_write = true;
    let left = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pf.checkpoint(entry_id(2), None)
    }));
    assert!(left.is_err(), "the write panicked");
    assert!(pf.failed(), "a checkpoint left by a panic latches");
    assert!(matches!(
        pf.reserve(3, 1, Purpose::Entry),
        Err(SpaceError::Failed)
    ));
    assert!(matches!(
        pf.checkpoint(entry_id(2), None),
        Err(CheckpointError::Failed)
    ));
    drop(pf);
    sim.restart();
    let (_, opened) = PageFile::open(&sim.fs(), &sim.fs().lock().unwrap(), config()).unwrap();
    assert_eq!(opened.entry, entry_id(1));
}

/// The floor with one more restore point listed, of name `name`.
fn floor_with_one_more(pf: &PageFile, name: &str) -> u64 {
    let mut inner = pf.lock();
    inner.listed.push(RestorePoint {
        name: name.to_owned(),
        checkpoint: u64::MAX,
        entry: EntryId::default(),
        version: Version {
            entry: u64::MAX,
            step: u64::MAX,
        },
        meta: (0, Hash::default()),
    });
    let floor = inner.floor(Purpose::Entry, &pf.config);
    inner.listed.pop();
    floor
}

/// Applies, through `res`, as the whole entry (one unit), trees made (5
/// pages each) and renamed (4 each) that take exactly `pages`, starting
/// the renames from `from`.
fn take_exactly(res: &Reservation<'_>, pages: u64, from: &TreeName) {
    let made = pages % 4;
    let renames = (pages - 5 * made) / 4;
    let mut unit = res.begin().unwrap();
    for i in 0..made {
        let root = tree::apply(res, &mut unit, None, res.entry(), &[]).unwrap();
        unit.add_tree(&TreeName::new(format!("n{i}")).unwrap(), root);
    }
    let mut current = from.clone();
    for i in 0..renames {
        let to = TreeName::new(format!("m{i}")).unwrap();
        unit.rename_tree(&current, &to);
        current = to;
    }
    unit.commit().unwrap();
}

/// Review of #719 (third round, P1 5): the room kept for meta streams
/// counts the restore point a checkpoint keeps from its first check. An
/// entry that takes exactly what may be taken while that checkpoint is
/// written fits again after a stop just after the checkpoint's first
/// superblock slot, when the reopened page file lists the point and keeps
/// a spare of its own; and a keep is refused while a reservation is open,
/// since that reservation was judged without the point.
#[test]
fn a_keep_stopped_after_its_first_slot_leaves_room_to_replay() {
    let sim = Sim::new(54);
    let config = Config {
        reserve: 0,
        ..config()
    };
    let name = tree_name(0);
    let (fs, pf, _) = gated(&sim, config, std::slice::from_ref(&name));
    // Room to take: the file grows by a reservation given back; then the
    // disk holds the file as it is, and no more.
    drop(pf.reserve(2, 256, Purpose::Entry).unwrap());
    sim.set_capacity(pf.space().end * PAGE);
    // Points of the longest names, until one more raises the floor.
    let long = |i: usize| format!("{i:064}");
    let mut i = 0;
    while floor_with_one_more(&pf, &long(i)) == pf.space().floor {
        pf.checkpoint(entry_id(1), Some(&long(i))).unwrap();
        i += 1;
        assert!(i < MAX_RESTORE_POINTS, "no keep raised the floor");
    }
    for _ in 0..20 {
        if pf.reclaim(1 << 20).drained {
            break;
        }
    }
    let end = pf.space().end;
    let ahead = pf.reserve(2, 1, Purpose::Entry).unwrap();
    assert!(matches!(
        pf.checkpoint(entry_id(1), Some(&long(i))),
        Err(CheckpointError::Refused(_))
    ));
    drop(ahead);
    let number = pf.next_checkpoint();
    fs.arm();
    let taken = std::thread::scope(|scope| {
        let keeping = scope.spawn(|| pf.checkpoint(entry_id(1), Some(&long(i))));
        fs.paused(&keeping);
        let gate = Opens(&fs);
        // Planned and its pages written: entry 2 takes all there is.
        let space = pf.space();
        let avail = space.free - space.promised - space.floor;
        assert!(avail >= 15, "{space:?}");
        let res = pf.reserve(2, avail, Purpose::Entry).unwrap();
        take_exactly(&res, avail, &name);
        let taken = res.taken();
        drop(res);
        fs.trip.lock().unwrap().stop_after_slot = true;
        drop(gate);
        let stopped = keeping.join().unwrap();
        assert!(
            matches!(stopped, Err(CheckpointError::Disk(_))),
            "{stopped:?}"
        );
        assert_eq!(taken, avail);
        taken
    });
    drop(pf);
    sim.restart();
    let (pf, opened) = PageFile::open(&sim.fs(), &sim.fs().lock().unwrap(), config).unwrap();
    assert_eq!(
        opened.checkpoint, number,
        "the keep's first copy was on disk"
    );
    assert_eq!(pf.space().end, end, "the file did not grow");
    let res = pf
        .reserve(2, taken, Purpose::Recovery)
        .expect("entry 2 fits again");
    take_exactly(&res, taken, &name);
    assert_eq!(res.taken(), taken);
}

/// A root a unit names is checked at its commit, as an open checks a
/// root: a tree's page (not an overflow page) the unit wrote. Named
/// otherwise, the unit is refused whole and nothing of it takes effect.
#[test]
fn a_unit_names_only_tree_pages_it_wrote_as_roots() {
    let sim = Sim::new(57);
    let (_fs, pf, trees) = gated(&sim, config(), &[tree_name(0)]);
    let bad = TreeName::new(b"bad".to_vec()).unwrap();
    let res = pf.reserve(2, 5, Purpose::Entry).unwrap();
    let overflow = |unit: &mut Unit<'_>| {
        let n = unit.allocate().unwrap();
        let mut builder = page::Builder::new(PageKind::Overflow, 0, 2);
        builder.data(b"not a tree");
        let page = builder.finish(n).unwrap();
        let root = Root {
            page: n,
            hash: page.hash(),
        };
        unit.write(Arc::new(page)).unwrap();
        root
    };
    let refused = |unit: Unit<'_>| {
        let why = unit.commit().err();
        assert!(
            matches!(&why, Some(StoreError::Failed(w)) if w.contains("not a tree's page")),
            "{why:?}"
        );
    };
    let before = pf.trees();
    // An overflow page made a tree's root, or given to a tree.
    let mut unit = res.begin().unwrap();
    let root = overflow(&mut unit);
    unit.add_tree(&bad, root);
    refused(unit);
    let mut unit = res.begin().unwrap();
    let root = overflow(&mut unit);
    unit.set_root(&tree_name(0), root);
    refused(unit);
    // A page the unit did not write.
    let mut unit = res.begin().unwrap();
    unit.add_tree(&bad, trees[0].1);
    refused(unit);
    assert_eq!(pf.trees(), before, "nothing of the refused units");
    // The entry is then made as it should be, and checkpointed.
    make_tree(&res, &bad).unwrap();
    drop(res);
    pf.checkpoint(entry_id(2), None).unwrap();
}

/// One unit carries a whole entry: it makes a tree, changes another, drops
/// a third and renames a fourth, and commits once; nothing of it is seen
/// before. The page file then holds exactly those trees, and so does the
/// checkpoint of that entry, opened again.
#[test]
fn one_unit_carries_a_whole_entry() {
    let sim = Sim::new(58);
    let names: Vec<TreeName> = (0..4).map(tree_name).collect();
    let (_fs, pf, trees) = gated(&sim, config(), &names);
    let made = TreeName::new(b"made".to_vec()).unwrap();
    let renamed = TreeName::new(b"renamed".to_vec()).unwrap();
    let res = pf.reserve(2, 128, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    let new_root = tree::apply(&res, &mut unit, None, 2, &[]).unwrap();
    unit.add_tree(&made, new_root);
    let edits = [Edit::leaf(vec![b"k".to_vec()], vec![7; 10])];
    let changed = tree::apply(&res, &mut unit, Some(trees[0].1), 2, &edits).unwrap();
    unit.set_root(&names[0], changed);
    unit.drop_tree(&names[2]);
    unit.rename_tree(&names[3], &renamed);
    assert_eq!(pf.trees(), trees, "nothing applied before the commit");
    unit.commit().unwrap();
    drop(res);
    let mut expected = vec![
        (names[0].clone(), changed),
        (names[1].clone(), trees[1].1),
        (made, new_root),
        (renamed, trees[3].1),
    ];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(pf.trees(), expected);
    assert!(pf.dying_pages().contains(&trees[2].1.page) || pf.is_free(trees[2].1.page));
    pf.checkpoint(entry_id(2), None).unwrap();
    drop(pf);
    sim.restart();
    let (pf, _) = PageFile::open(&sim.fs(), &sim.fs().lock().unwrap(), config()).unwrap();
    assert_eq!(pf.checkpoint_trees(), expected);
    assert_eq!(pf.trees(), expected);
    let read = Tree::open(&pf, changed).unwrap().get(&[b"k"]).unwrap();
    assert_eq!(read, Some(Node::Value(vec![7; 10])));
}

/// `Reservation::extend` is not judged against the budget of pages
/// waiting for a checkpoint: an entry reserved ahead, before the entry
/// before it took the pages in memory past the budget, extends its
/// reservation and finishes, and a checkpoint of it is then taken.
#[test]
fn an_entry_extended_past_the_budget_finishes() {
    let sim = Sim::new(60);
    let config = Config {
        dirty_budget: 16,
        ..config()
    };
    let names = [tree_name(0), tree_name(1)];
    let (_fs, pf, trees) = gated(&sim, config, &names);
    let edits = |from: u32, n: u32| -> Vec<Edit> {
        (from..from + n)
            .map(|i| Edit::leaf(vec![i.to_be_bytes().to_vec()], vec![7; 1500]))
            .collect()
    };
    let (first, second) = (edits(0, 400), edits(1000, 3));
    let height = |root| Tree::open(&pf, root).unwrap().height();
    let one = pf
        .reserve(2, entry_bound(&first, height(trees[0].1)), Purpose::Entry)
        .unwrap();
    let two = pf.reserve(3, 0, Purpose::Entry).unwrap();
    // Entry 2 takes the pages in memory past the budget.
    edit_tree(&one, &names[0], &first).unwrap();
    drop(one);
    assert!(pf.space().dirty >= 16, "entry 2 passed the budget");
    two.extend(entry_bound(&second, height(trees[1].1)))
        .expect("an extension is not judged against the budget");
    let root = edit_tree(&two, &names[1], &second).unwrap();
    drop(two);
    pf.checkpoint(entry_id(3), None).unwrap();
    let tree = Tree::open(&pf, root).unwrap();
    assert!(tree.get(&[&1000u32.to_be_bytes()]).unwrap().is_some());
}

/// Review of #744 (P0): a checkpoint reads back from the file every tree's
/// root that is not in memory before it writes anything, and is refused,
/// naming the tree, when one does not: its copies would replace the older
/// checkpoint's, leaving both checkpoints on disk naming a root that does
/// not read back. The store then opens on the checkpoint before.
#[test]
fn a_checkpoint_reads_the_roots_on_disk_back_before_it_writes() {
    let sim = Sim::new(61);
    let name = tree_name(0);
    let (fs, pf, trees) = gated(&sim, config(), std::slice::from_ref(&name));
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let edits = [Edit::leaf(vec![b"k".to_vec()], vec![2; 10])];
    let b = edit_tree(&res, &name, &edits).unwrap();
    drop(res);
    pf.checkpoint(entry_id(2), None).unwrap();
    // B is on disk now, and its page no longer reads back.
    fs.trip.lock().unwrap().fail_read = Some(b.page);
    let next = pf.next_checkpoint();
    let refused = pf.checkpoint(entry_id(2), None);
    assert!(
        matches!(&refused, Err(CheckpointError::Root { tree, .. }) if *tree == name),
        "{refused:?}"
    );
    assert_eq!(pf.next_checkpoint(), next, "nothing was written");
    assert!(!pf.failed());
    // The device damages it for good: the store opens on the checkpoint
    // before, which names A.
    drop(pf);
    sim.damage(FILE, b.page * PAGE + 100, 8).unwrap();
    sim.restart();
    let (pf, opened) = PageFile::open(&sim.fs(), &sim.fs().lock().unwrap(), config()).unwrap();
    assert_eq!(opened.entry, entry_id(1));
    assert_eq!(pf.trees(), trees);
}

/// Review of #744 (P1): a release of a page still in memory is checked
/// against its hash: a release with another hash is refused, and the unit
/// changes nothing.
#[test]
fn a_release_with_another_hash_is_refused() {
    let sim = Sim::new(62);
    let name = tree_name(0);
    let (_fs, pf, _) = gated(&sim, config(), std::slice::from_ref(&name));
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let big = [Edit::leaf(vec![b"k".to_vec()], vec![2; 20_000])];
    let root = edit_tree(&res, &name, &big).unwrap();
    drop(res);
    let overflow = overflow_page(&pf, root);
    let before = (pf.trees(), pf.map_image());
    let res = pf.reserve(3, 4, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    unit.release(overflow.0, &Hash::of(b"another page"), Release::Copied)
        .unwrap();
    let refused = unit.commit().err();
    assert!(
        matches!(&refused, Some(StoreError::Failed(why)) if why.contains("another hash")),
        "{refused:?}"
    );
    assert_eq!((pf.trees(), pf.map_image()), before, "nothing changed");
}

/// An overflow page of the tree `root` names.
fn overflow_page<S: PageStore + ?Sized>(store: &S, root: Root) -> (PageNo, Hash) {
    tree::check(store, root)
        .unwrap()
        .pages
        .into_iter()
        .find(|(n, h)| store.read(*n, h).unwrap().kind() == PageKind::Overflow)
        .expect("an overflow page")
}

/// A tree call of a table-driven case (below).
#[derive(Clone, Copy, Debug)]
enum TreeCall {
    /// A tree made, empty.
    Make(&'static str),
    /// A tree's edits applied, and its new root given.
    Edit(&'static str),
    /// A tree replaced by an empty one: its old root released as given,
    /// and, released as copied, what the old root referred to, which the
    /// new root does not keep.
    Replace(&'static str, Release),
    /// A tree given a second root, an empty one, in the unit.
    SetAgain(&'static str),
    /// A tree given the root it has.
    Same(&'static str),
    Drop(&'static str),
    Rename(&'static str, &'static str),
    /// A tree made with another tree's root.
    MakeFrom(&'static str, &'static str),
    /// A tree's root released, and no new root given.
    ReleaseRoot(&'static str),
    /// Tree `a`'s overflow page released with another hash.
    WrongHash,
    /// Tree `a`'s overflow page released twice.
    ReleaseTwice,
    /// Another unit, begun while this one is open, makes a tree.
    OtherUnitMakes(&'static str),
    /// Sol's sequence: a leaf and an index page over it, both written by
    /// the unit, made the roots of two trees (the first named for the leaf).
    MakeInside(&'static str, &'static str),
    /// A tree made whose root, an index page the unit writes, refers to
    /// the root of a tree the unit leaves alone.
    MakeAbove(&'static str, &'static str),
}

/// Writes, in `unit`, an index page whose one child is `child`.
fn index_over<E: EntryPages>(unit: &mut E, entry: u64, child: Root, stamp: u64) -> Root {
    let number = unit.allocate().unwrap();
    let mut builder = Builder::new(PageKind::Index, 1, entry);
    builder.child(child.page, &child.hash, stamp, None);
    let page = builder.finish(number).unwrap();
    let root = Root {
        page: number,
        hash: page.hash(),
    };
    unit.write(Arc::new(page)).unwrap();
    root
}

/// Makes trees `a` and `b`, empty, as entry `entry`: one unit.
fn make_a_and_b<S: PageStore + ?Sized>(store: &S, entry: u64) {
    let mut unit = store.begin().unwrap();
    for name in ["a", "b"] {
        let root = tree::apply(store, &mut unit, None, entry, &[]).unwrap();
        unit.add_tree(&TreeName::new(name).unwrap(), root);
    }
    unit.commit().unwrap();
}

/// Gives tree `a` a large value as entry `entry`: one unit. Returns its
/// overflow page.
fn grow_a<S: PageStore + ?Sized>(store: &S, entry: u64) -> (PageNo, Hash) {
    let a = TreeName::new("a").unwrap();
    let old = store.root(&a).unwrap();
    let big = [Edit::leaf(vec![b"k".to_vec()], vec![2; 20_000])];
    let mut unit = store.begin().unwrap();
    let root = tree::apply(store, &mut unit, Some(old), entry, &big).unwrap();
    unit.set_root(&a, root);
    unit.commit().unwrap();
    overflow_page(store, root)
}

/// Runs `calls` in one unit of `store`, as entry `entry`, and commits it;
/// another unit is begun from `other`, as entry `entry + 1`.
fn run_tree_calls<S: PageStore + ?Sized, O: PageStore + ?Sized>(
    store: &S,
    other: &O,
    entry: u64,
    calls: &[TreeCall],
    overflow: (PageNo, Hash),
) -> Result<(), String> {
    let name = |n: &str| TreeName::new(n).unwrap();
    let mut current: BTreeMap<TreeName, Root> = store.trees().into_iter().collect();
    let mut unit = store.begin().map_err(|e| e.to_string())?;
    let edits = [Edit::leaf(vec![b"e".to_vec()], vec![3; 10])];
    for call in calls {
        match *call {
            TreeCall::Make(n) => {
                let root =
                    tree::apply(store, &mut unit, None, entry, &[]).map_err(|e| e.to_string())?;
                unit.add_tree(&name(n), root);
                current.insert(name(n), root);
            }
            TreeCall::Edit(n) => {
                let old = current[&name(n)];
                let root = tree::apply(store, &mut unit, Some(old), entry, &edits)
                    .map_err(|e| e.to_string())?;
                unit.set_root(&name(n), root);
                current.insert(name(n), root);
            }
            TreeCall::Replace(n, how) => {
                let root =
                    tree::apply(store, &mut unit, None, entry, &[]).map_err(|e| e.to_string())?;
                let old = current[&name(n)];
                unit.release(old.page, &old.hash, how)
                    .map_err(|e| e.to_string())?;
                if how == Release::Copied {
                    let page = store.read(old.page, &old.hash).map_err(|e| e.to_string())?;
                    for (number, hash) in page.references() {
                        unit.release(number, &hash, Release::Dropped)
                            .map_err(|e| e.to_string())?;
                    }
                }
                unit.set_root(&name(n), root);
                current.insert(name(n), root);
            }
            TreeCall::SetAgain(n) => {
                let root =
                    tree::apply(store, &mut unit, None, entry, &[]).map_err(|e| e.to_string())?;
                unit.set_root(&name(n), root);
                current.insert(name(n), root);
            }
            TreeCall::Same(n) => unit.set_root(&name(n), current[&name(n)]),
            TreeCall::Drop(n) => {
                unit.drop_tree(&name(n));
                current.remove(&name(n));
            }
            TreeCall::Rename(f, t) => {
                unit.rename_tree(&name(f), &name(t));
                if let Some(root) = current.remove(&name(f)) {
                    current.insert(name(t), root);
                }
            }
            TreeCall::MakeFrom(n, other) => unit.add_tree(&name(n), current[&name(other)]),
            TreeCall::ReleaseRoot(n) => {
                let old = current[&name(n)];
                unit.release(old.page, &old.hash, Release::Copied)
                    .map_err(|e| e.to_string())?;
            }
            TreeCall::WrongHash => unit
                .release(overflow.0, &Hash::of(b"another page"), Release::Copied)
                .map_err(|e| e.to_string())?,
            TreeCall::ReleaseTwice => {
                for _ in 0..2 {
                    unit.release(overflow.0, &overflow.1, Release::Copied)
                        .map_err(|e| e.to_string())?;
                }
            }
            TreeCall::MakeInside(n, of) => {
                let leaf =
                    tree::apply(store, &mut unit, None, entry, &[]).map_err(|e| e.to_string())?;
                let top = index_over(&mut unit, entry, leaf, entry);
                unit.add_tree(&name(n), leaf);
                unit.add_tree(&name(of), top);
            }
            TreeCall::MakeAbove(n, other) => {
                let below = current[&name(other)];
                let stamp = store
                    .read(below.page, &below.hash)
                    .map_err(|e| e.to_string())?
                    .written();
                let top = index_over(&mut unit, entry, below, stamp);
                unit.add_tree(&name(n), top);
            }
            TreeCall::OtherUnitMakes(n) => {
                let mut second = other.begin().map_err(|e| e.to_string())?;
                let root = tree::apply(other, &mut second, None, entry + 1, &[])
                    .map_err(|e| e.to_string())?;
                second.add_tree(&name(n), root);
                second.commit().map_err(|e| e.to_string())?;
            }
        }
    }
    unit.commit().map_err(|e| e.to_string())
}

/// Review of #744 (P1 and P2): `MemStore` and the page file accept and
/// refuse the same units of tree calls: within one unit a tree's root is
/// given at most once (`add_tree` or `set_root`), and only a page the unit
/// wrote; `drop_tree` names a tree that existed when the unit began and
/// that the unit gave no root; a tree replaced whole may release its old
/// root either way; a page is released once, with its hash; one unit is
/// open at a time (round 2); no page the unit writes refers to a page that
/// is a tree's root once the unit takes effect (round 3). A unit both
/// accept is one the page file can checkpoint.
#[test]
fn memstore_and_the_page_file_accept_and_refuse_the_same_units() {
    use TreeCall::*;
    let cases: &[(&[TreeCall], bool)] = &[
        (&[Make("c")], true),
        (&[Make("a")], false),
        (&[Drop("a")], true),
        (&[Drop("c")], false),
        (&[Rename("a", "c")], true),
        (&[Rename("a", "b")], false),
        (&[Edit("a")], true),
        (&[Edit("a"), SetAgain("a")], false),
        (&[Make("c"), Drop("c")], false),
        (&[Edit("a"), Drop("a")], false),
        (&[Replace("a", Release::Dropped)], true),
        (&[Replace("a", Release::Copied)], true),
        (&[Same("a")], false),
        (&[Rename("a", "c"), Edit("c")], true),
        (&[Edit("a"), Rename("a", "c")], true),
        (&[Drop("a"), Make("a")], true),
        (&[ReleaseRoot("a")], false),
        (&[MakeFrom("c", "b")], false),
        (&[WrongHash], false),
        (&[ReleaseTwice], false),
        (&[OtherUnitMakes("c"), Drop("c")], false),
        (&[MakeInside("c", "d")], false),
        (&[MakeAbove("c", "b")], false),
    ];
    for (i, (calls, ok)) in cases.iter().enumerate() {
        // The page file: one reservation an entry.
        let sim = Sim::new(70 + i as u64);
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        let pf = PageFile::create(&fs, &lock, config()).unwrap();
        make_a_and_b(&pf.reserve(1, 64, Purpose::Entry).unwrap(), 1);
        let overflow = grow_a(&pf.reserve(2, 64, Purpose::Entry).unwrap(), 2);
        let res = pf.reserve(3, 64, Purpose::Entry).unwrap();
        let other = pf.reserve(4, 64, Purpose::Entry).unwrap();
        let on_file = run_tree_calls(&res, &other, 3, calls, overflow);
        drop((res, other));
        // What both stores accept, the page file can also checkpoint.
        if on_file.is_ok() {
            if let Err(e) = pf.checkpoint(entry_id(3), None) {
                panic!("the page file cannot checkpoint {calls:?}: {e}");
            }
        }
        // `MemStore`.
        let mem = MemStore::new(Numbering::Sequential);
        make_a_and_b(&mem, 1);
        let overflow = grow_a(&mem, 2);
        let in_memory = run_tree_calls(&mem, &mem, 3, calls, overflow);
        assert_eq!(
            on_file.is_ok(),
            *ok,
            "the page file, {calls:?}: {on_file:?}"
        );
        assert_eq!(in_memory.is_ok(), *ok, "MemStore, {calls:?}: {in_memory:?}");
    }
}

/// Review of #744, round 2 (P0): a pin is of a version the page file has.
/// Sol's sequence: entry 1 commits root A; entry 2's unit stages B; a pin
/// of entry 2 before that unit commits is refused (it would get A, which
/// entry 2's commit may free, and a later pin of entry 2 would be handed
/// A again). The newest version is entry 1's until then; once entry 2
/// commits, every pin of it has B, and two pins of it agree. An entry
/// after the last unit is refused too: it has no version of its own.
#[test]
fn a_pin_is_of_a_version_the_page_file_has() {
    let sim = Sim::new(63);
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, config()).unwrap();
    let name = tree_name(0);
    let a = make_tree(&pf.reserve(1, 5, Purpose::Entry).unwrap(), &name).unwrap();
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    let edits = [Edit::leaf(vec![b"k".to_vec()], vec![2; 10])];
    let b = tree::apply(&res, &mut unit, Some(a), 2, &edits).unwrap();
    unit.set_root(&name, b);
    assert!(pf.pin(2).is_err(), "a pin of an entry whose unit is open");
    let newest = pf.pin_newest().unwrap();
    assert_eq!(
        (newest.entry(), newest.roots()),
        (1, &[(name.clone(), a)][..])
    );
    unit.commit().unwrap();
    drop(res);
    let first = pf.pin(2).unwrap();
    let second = pf.pin(2).unwrap();
    let newest = pf.pin_newest().unwrap();
    for pin in [&first, &second, &newest] {
        assert_eq!((pin.entry(), pin.roots()), (2, &[(name.clone(), b)][..]));
    }
    assert!(pf.pin(3).is_err(), "an entry after the last unit");
    let read = Tree::open(&pf, b).unwrap().get(&[b"k"]).unwrap();
    assert_eq!(read, Some(Node::Value(vec![2; 10])));
}

/// Review of #744 (P1): a pin of an older checkpoint carries the roots of
/// its trees, read from its meta stream as the pin is taken; they stay with
/// the pin, and its trees stay readable, after later checkpoints and
/// reclaims have reused that meta stream's pages. A read of that stream
/// the device fails refuses the pin with the device's error (round 3).
#[test]
fn a_pin_of_an_older_checkpoint_carries_its_roots() {
    let sim = Sim::new(64);
    let name = tree_name(0);
    // No cache: the pin reads the older meta stream from the device.
    let config = Config {
        cache_bytes: 0,
        ..config()
    };
    let (fs, pf, trees) = gated(&sim, config, std::slice::from_ref(&name));
    let edits = |v: u8| [Edit::leaf(vec![b"k".to_vec()], vec![v; 10])];
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let first = edit_tree(&res, &name, &edits(1)).unwrap();
    drop(res);
    let _ = trees;
    pf.checkpoint(entry_id(2), None).unwrap();
    let res = pf.reserve(3, 64, Purpose::Entry).unwrap();
    edit_tree(&res, &name, &edits(2)).unwrap();
    drop(res);
    pf.checkpoint(entry_id(3), None).unwrap();
    // Entry 2's checkpoint is the older one on disk now. The device fails
    // to read its meta stream: the pin is refused with that cause.
    let meta = pf.kept_checkpoints()[1].meta;
    fs.trip.lock().unwrap().fail_read = Some(meta.0);
    match pf.pin(2) {
        Err(PinError::Meta(MetaFault::Page(PageFault {
            page,
            fault: Fault::Io(_),
            ..
        }))) => assert_eq!(page, meta.0),
        other => panic!("{other:?}"),
    }
    fs.trip.lock().unwrap().fail_read = None;
    let pin = pf.pin(2).unwrap();
    assert_eq!(pin.roots(), &[(name.clone(), first)][..]);
    for index in 4..12 {
        let res = pf.reserve(index, 64, Purpose::Entry).unwrap();
        edit_tree(&res, &name, &edits(index as u8)).unwrap();
        drop(res);
        pf.checkpoint(entry_id(index), None).unwrap();
        while !pf.reclaim(1 << 20).drained {}
    }
    assert_eq!(pin.roots(), &[(name, first)][..]);
    let read = Tree::open(&pf, first).unwrap().get(&[b"k"]).unwrap();
    assert_eq!(read, Some(Node::Value(vec![1; 10])));
}

/// Rewrites the tree table of the newest checkpoint on disk, as #719 could
/// write it from roots its caller gave: the meta stream rebuilt into the
/// same pages, and the superblock's copies pointed at it.
fn forge_newest_table(pf: &PageFile, edit: impl FnOnce(&mut Vec<(TreeName, Root)>)) {
    let (sb, pair) = {
        let inner = pf.lock();
        (inner.newest.sb, inner.newest.pair)
    };
    let root = forge_table(pf, sb, edit);
    let copy = superblock::encode(&Superblock { meta: root, ..sb });
    for s in pair_slots(pair) {
        pf.file.write_at((s * SLOT) as u64, &copy).unwrap();
    }
    pf.file.flush().unwrap();
}

/// Rewrites the tree table in the meta stream of checkpoint `sb`, into the
/// same pages; returns the stream's new first page.
fn forge_table(
    pf: &PageFile,
    sb: Superblock,
    edit: impl FnOnce(&mut Vec<(TreeName, Root)>),
) -> (PageNo, Hash) {
    let (stream, pages) = pf.read_meta(sb.meta).unwrap();
    let mut meta = Meta::decode(&stream).unwrap();
    edit(&mut meta.trees);
    meta.trees.sort_by(|a, b| a.0.cmp(&b.0));
    let stream = meta.encode();
    let numbers: Vec<PageNo> = pages.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        meta::pages_for(stream.len()),
        numbers.len(),
        "the table fits"
    );
    let (written, root) = meta::build(&stream, &numbers, sb.number);
    for page in written {
        pf.file
            .write_at(page.number() * PAGE, page.bytes())
            .unwrap();
    }
    pf.file.flush().unwrap();
    root
}

/// Review of #744, round 2 (P0): a checkpoint whose table names one root
/// page for two trees, which the map does not record as shared, is
/// damage: open passes it over and falls back to the checkpoint before.
#[test]
fn a_checkpoint_naming_one_root_for_two_trees_is_passed_over() {
    let sim = Sim::new(65);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let (t0, t1) = (tree_name(0), tree_name(1));
    let a = make_tree(&proc.pf.reserve(1, 5, Purpose::Entry).unwrap(), &t0).unwrap();
    proc.pf.checkpoint(entry_id(1), None).unwrap();
    proc.pf.checkpoint(entry_id(1), None).unwrap();
    assert_eq!(proc.pf.checkpoint_entry().0, 3);
    forge_newest_table(&proc.pf, |trees| trees.push((t1, a)));
    drop(proc);
    sim.restart();
    let (proc, opened) = open_proc(&disk, config()).unwrap();
    assert_eq!(opened.checkpoint, 2);
    assert_eq!(
        opened.passed_over,
        vec![PassedOver {
            number: 3,
            why: Rejection::Mismatch(SHARED_ROOT),
        }]
    );
    assert_eq!(proc.pf.trees(), vec![(t0, a)]);
}

/// Review of #744, round 2 (P0): the same table reached in memory (as a
/// page file that loaded it before open refused it would hold it): a
/// checkpoint of it is refused before it reads or writes anything. Sol's
/// sequence goes on: one of the two names is dropped, which marks the
/// page dying while the other still names it; every checkpoint is then
/// refused, and the checkpoints on disk stay the fallback.
#[test]
fn a_checkpoint_of_two_trees_on_one_root_is_refused() {
    let sim = Sim::new(66);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let pf = &proc.pf;
    let (t0, t1) = (tree_name(0), tree_name(1));
    let a = make_tree(&pf.reserve(1, 5, Purpose::Entry).unwrap(), &t0).unwrap();
    pf.checkpoint(entry_id(1), None).unwrap();
    {
        let mut inner = pf.lock();
        inner.trees.insert(t1.clone(), a);
        *inner.root_pages.entry(a.page).or_default() += 1;
    }
    let next = pf.next_checkpoint();
    match pf.checkpoint(entry_id(1), None) {
        Err(CheckpointError::Refused(why)) => assert_eq!(why, SHARED_ROOT),
        other => panic!("{other:?}"),
    }
    let res = pf.reserve(2, 5, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    unit.drop_tree(&t1);
    unit.commit().unwrap();
    drop(res);
    assert!(!pf.lock().map.in_use(a.page), "the page t0 names is dying");
    for _ in 0..2 {
        match pf.checkpoint(entry_id(2), None) {
            Err(CheckpointError::Refused(why)) => {
                assert_eq!(why, "a root that is not a page of the newest version")
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(pf.next_checkpoint(), next, "nothing written");
    drop(proc);
    sim.restart();
    let (proc, opened) = open_proc(&disk, config()).unwrap();
    assert_eq!(opened.checkpoint, 2);
    assert_eq!(proc.pf.trees(), vec![(t0, a)]);
}

/// Review of #744, round 2 (P0): a table #719 could write that open cannot
/// tell from a good one without walking every tree: one tree's root is a
/// page inside another tree. Once that tree is dropped and taken apart,
/// the first tree's root is dying; a checkpoint is then refused before it
/// replaces the checkpoint on disk, which stays the fallback.
#[test]
fn a_checkpoint_of_a_root_that_died_is_refused() {
    let sim = Sim::new(67);
    let disk = Disk::Sim(sim.clone());
    let proc = create_proc(&disk, config());
    let (x, y) = (tree_name(0), tree_name(1));
    let res = proc.pf.reserve(1, 64, Purpose::Entry).unwrap();
    make_tree(&res, &y).unwrap();
    drop(res);
    let res = proc.pf.reserve(2, 64, Purpose::Entry).unwrap();
    let edits: Vec<Edit> = (0..60u8)
        .map(|i| Edit::leaf(vec![format!("k{i:02}").into_bytes()], vec![i; 1_000]))
        .collect();
    let root = edit_tree(&res, &y, &edits).unwrap();
    drop(res);
    let top = proc.pf.read(root.page, &root.hash).unwrap();
    assert_eq!(top.kind(), PageKind::Index);
    let child = top.child(0);
    let inside = Root {
        page: child.number,
        hash: child.hash,
    };
    proc.pf.checkpoint(entry_id(2), None).unwrap();
    forge_newest_table(&proc.pf, |trees| trees.push((x.clone(), inside)));
    drop(proc);
    sim.restart();
    let (proc, opened) = open_proc(&disk, config()).unwrap();
    assert_eq!(opened.checkpoint, 2, "open cannot see it");
    let pf = &proc.pf;
    let res = pf.reserve(3, 5, Purpose::Entry).unwrap();
    let mut unit = res.begin().unwrap();
    unit.drop_tree(&y);
    unit.commit().unwrap();
    drop(res);
    while !pf.reclaim(1 << 20).drained {}
    assert!(!pf.lock().map.in_use(inside.page), "x's root is dying");
    let next = pf.next_checkpoint();
    for _ in 0..2 {
        match pf.checkpoint(entry_id(3), None) {
            Err(CheckpointError::Refused(why)) => {
                assert_eq!(why, "a root that is not a page of the newest version")
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(pf.next_checkpoint(), next, "nothing written");
    drop(proc);
    sim.restart();
    let (proc, opened) = open_proc(&disk, config()).unwrap();
    assert_eq!(opened.checkpoint, 2);
    assert_eq!(proc.pf.root(&x), Some(inside));
}

/// Review of #744, round 3: a page of the newest checkpoint's meta stream
/// that is dying (what a table #719 wrote with a root inside another tree
/// can lead to, once that page is taken for the meta stream and the tree
/// still referring to it is taken down) refuses the checkpoint instead of
/// stopping the process in the plan.
#[test]
fn a_checkpoint_whose_meta_page_is_dying_is_refused() {
    let sim = Sim::new(68);
    let (_fs, pf, _) = gated(&sim, config(), &[tree_name(0)]);
    let (n, h) = pf.lock().newest.meta_pages[0];
    pf.lock()
        .map
        .die(n, h, None, Version::at(2), false)
        .unwrap();
    let next = pf.next_checkpoint();
    match pf.checkpoint(entry_id(1), None) {
        Err(CheckpointError::Refused(why)) => {
            assert_eq!(
                why,
                "a page of the newest checkpoint's meta stream is not in use"
            )
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(pf.next_checkpoint(), next);
    assert!(!pf.failed());
}

/// Review of #744, round 3: a pin of an older checkpoint applies to its
/// table the rule open applies to the newest: one root page named by two
/// trees, which the map does not record as shared, refuses the pin, as a
/// stream that breaks a rule of the reader's.
#[test]
fn a_pin_refuses_a_table_with_one_root_for_two_trees() {
    let sim = Sim::new(69);
    let (t0, t1) = (tree_name(0), tree_name(1));
    let (_fs, pf, trees) = gated(&sim, config(), std::slice::from_ref(&t0));
    let a = trees[0].1;
    let res = pf.reserve(2, 64, Purpose::Entry).unwrap();
    let edits = [Edit::leaf(vec![b"k".to_vec()], vec![2; 10])];
    edit_tree(&res, &t0, &edits).unwrap();
    drop(res);
    pf.checkpoint(entry_id(2), None).unwrap();
    // Entry 1's checkpoint is the older one; its table, rewritten as #719
    // could have written it, names tree 0's root for tree 1 too.
    let older = pf.lock().older.unwrap().sb;
    let meta = forge_table(&pf, older, |trees| trees.push((t1, a)));
    pf.lock().older.as_mut().unwrap().sb.meta = meta;
    match pf.pin(1) {
        Err(PinError::Meta(MetaFault::Malformed(why))) => assert_eq!(why, SHARED_ROOT),
        other => panic!("{other:?}"),
    }
}
