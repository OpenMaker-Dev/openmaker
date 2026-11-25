//! The store on the simulated disk (`docs/server-storage.md` section 12),
//! against the reference model ([`crate::model`]).
//!
//! The seeded test drives one store with entries of every kind, several
//! appended before a flush, views taken and held across writes and
//! checkpoints, checkpoints, restore points kept, read and dropped, the
//! log's retirement, stops, power losses and the faults the disk injects
//! (crashes, failed writes and flushes, a full disk, failed reads, damage,
//! writeback). The two "accept the loss" calls are made where the faults
//! lead there. After every open, flush, view and restore point read:
//!
//! - every published version equals the model at its entry (check 1), and
//!   a held view still reads what it read;
//! - no entry answered as stored is lost (check 2), within the faults
//!   section 6.12 says the store finds: a loss the store reports
//!   ([`Opened::last_batch_may_be_lost`]) or one the administrator accepted;
//! - no view ever showed an entry that was not durable (check 4): after
//!   every open, the durable entry is at or after every view taken;
//! - a restore point reads as the model at its entry (check 9);
//! - the store opens from every state the faults leave, or says why, and
//!   only where the disk did damage (check 11); a read that returns data
//!   returns the model's, and a page that does not check is an error
//!   (check 12).
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds; `OM_STORE_SEED=<s>`
//! runs seed `s` alone.

use std::collections::BTreeMap;

use super::*;
use crate::apply::{FileRow, LockRow, ServerRow, SessionRow};
use crate::entry::{
    Change, FileEvent, Kind, LockEdit, Mark, Members, Op, Purpose as MarkPurpose, Session,
};
use crate::model::{Model, Path};
use crate::sim::{self, Fates, Fault, Rates, Rng, Sim};
use crate::tree::{Edit, Node};

/// Small pieces, so that the schedules cross segments, fill the dirty
/// budget and grow the page file.
const CONFIG: Config = Config {
    log: log::Config {
        segment_size: 64 << 10,
        max_payload: 48 << 10,
    },
    pages: pagefile::Config {
        cache_bytes: 1 << 20,
        dirty_budget: 24,
        reserve: 32,
        growth: 32,
    },
};

/// The seeds of a schedule: `default` of them, scaled.
fn seeds(default: u64) -> Vec<u64> {
    let read = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(seed) = read("OM_STORE_SEED") {
        return vec![seed];
    }
    (0..default.saturating_mul(read("OM_STORE_SCALE").unwrap_or(1))).collect()
}

/// The simulated disk, with one place that fails every read until the next
/// open of the store succeeds: a sector gone bad, which the simulator's
/// read faults (one read each) do not make. Directed tests can also have
/// the folder's lock lost after so many more checks of it, and every write
/// fail under a path.
#[derive(Clone)]
struct Flaky {
    inner: sim::SimFs,
    bad: Arc<std::sync::Mutex<Option<(String, u64)>>>,
    lock_checks_left: Arc<std::sync::Mutex<Option<u64>>>,
    /// Writes under a path fail, with this kind, once so many more of them
    /// succeeded.
    fail_writes: Arc<std::sync::Mutex<Option<(String, u64, ErrorKind)>>>,
    /// Another process makes a store in the folder just before the lock
    /// is taken for the so manyth time from now.
    create_at_lock: Arc<std::sync::Mutex<Option<(u64, sim::SimRandom)>>>,
}

struct FlakyFile {
    inner: Box<dyn sys::File>,
    path: String,
    bad: Arc<std::sync::Mutex<Option<(String, u64)>>>,
    fail_writes: Arc<std::sync::Mutex<Option<(String, u64, ErrorKind)>>>,
}

impl sys::Fs for Flaky {
    fn create_root(&self) -> sys::Result<()> {
        self.inner.create_root()
    }
    fn lock(&self) -> sys::Result<sys::FolderLock> {
        let other = {
            let mut at = self.create_at_lock.lock().unwrap();
            match at.as_mut() {
                Some((0, _)) => at.take().map(|(_, random)| random),
                Some((n, _)) => {
                    *n -= 1;
                    None
                }
                None => None,
            }
        };
        if let Some(random) = other {
            let fs: Arc<dyn Fs> = Arc::new(self.inner.clone());
            drop(Store::create(fs, &random, CONFIG).expect("the other process's creation"));
        }
        self.inner.lock()
    }
    fn owns(&self, lock: &sys::FolderLock) -> bool {
        let mut left = self.lock_checks_left.lock().unwrap();
        match left.as_mut() {
            Some(0) => return false,
            Some(n) => *n -= 1,
            None => {}
        }
        self.inner.owns(lock)
    }
    fn create(&self, path: &str, options: sys::FileOptions) -> sys::Result<Box<dyn sys::File>> {
        let inner = self.inner.create(path, options)?;
        Ok(self.wrap(path, inner))
    }
    fn open(&self, path: &str, options: sys::FileOptions) -> sys::Result<Box<dyn sys::File>> {
        let inner = self.inner.open(path, options)?;
        Ok(self.wrap(path, inner))
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

impl Flaky {
    fn new(sim: &Sim) -> Flaky {
        Flaky {
            inner: sim.fs(),
            bad: Arc::new(std::sync::Mutex::new(None)),
            lock_checks_left: Arc::new(std::sync::Mutex::new(None)),
            fail_writes: Arc::new(std::sync::Mutex::new(None)),
            create_at_lock: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn wrap(&self, path: &str, inner: Box<dyn sys::File>) -> Box<dyn sys::File> {
        Box::new(FlakyFile {
            inner,
            path: path.to_owned(),
            bad: self.bad.clone(),
            fail_writes: self.fail_writes.clone(),
        })
    }
}

impl sys::File for FlakyFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
        let end = offset + buf.len() as u64;
        let bad = self
            .bad
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(p, at)| *p == self.path && (offset..end).contains(at));
        if bad {
            return Err(sys::Error::new(ErrorKind::Read, sys::Op::Read, &self.path));
        }
        self.inner.read_at(offset, buf)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> sys::Result<()> {
        let fail = match self.fail_writes.lock().unwrap().as_mut() {
            Some((prefix, left, kind)) if self.path.starts_with(prefix.as_str()) => {
                let fail = (*left == 0).then_some(*kind);
                *left = left.saturating_sub(1);
                fail
            }
            _ => None,
        };
        if let Some(kind) = fail {
            return Err(sys::Error::new(kind, sys::Op::Write, &self.path));
        }
        self.inner.write_at(offset, data)
    }
    fn flush(&self) -> sys::Result<()> {
        self.inner.flush()
    }
    fn allocate(&self, size: u64) -> sys::Result<()> {
        self.inner.allocate(size)
    }
    fn size(&self) -> sys::Result<u64> {
        self.inner.size()
    }
}

/// An entry as appended, for the model.
#[derive(Clone, Debug)]
struct Built {
    index: u64,
    term: u64,
    kind: u16,
    file: u64,
    payload: Vec<u8>,
    hash: Hash,
    /// The session it opened and those it closed, by the model, once it
    /// was applied to it.
    effects: Option<(Option<Session>, Vec<Session>)>,
}

impl Built {
    fn new(prev: &Position, term: u64, file: u64, payload: &Payload) -> Built {
        let bytes = payload.encode().expect("the generator stays within bounds");
        let index = prev.index + 1;
        let kind = payload.kind().code();
        let hash = entry_hash(&prev.hash, index, term, kind, file, &bytes);
        Built {
            index,
            term,
            kind,
            file,
            payload: bytes,
            hash,
            effects: None,
        }
    }

    fn entry(&self) -> Entry<'_> {
        Entry {
            index: self.index,
            term: self.term,
            kind: self.kind,
            file: self.file,
            payload: &self.payload,
            hash: self.hash,
        }
    }
}

/// Servers other than the store's, whose sessions entries open and close.
const SERVERS: [ServerId; 2] = [[1; 16], [2; 16]];

/// Entries of every kind, valid for the model they are drawn from.
struct Gen {
    rng: Rng,
    names: u64,
}

impl Gen {
    fn next(&mut self, model: &Model, next_index: u64) -> (u64, Payload) {
        let roll = self.rng.below(100);
        if model.files.is_empty() || (model.files.len() < 4 && roll < 6) {
            return self.added(next_index);
        }
        let ids: Vec<u64> = model.files.keys().copied().collect();
        let file = ids[self.rng.below(ids.len() as u64) as usize];
        match roll {
            6..=59 => (file, Payload::Change(self.change(model, file))),
            60..=74 => (0, Payload::Session(self.session(model))),
            75..=78 => {
                self.names += 1;
                let name = format!("renamed {}", self.names).into_bytes();
                (file, Payload::File(FileEvent::Renamed { name }))
            }
            79..=81 if ids.len() > 1 => (file, Payload::File(FileEvent::Removed)),
            82..=86 => {
                let n = 1 + self.rng.below(20) as usize;
                let mut name = vec![0u8; n];
                self.rng.fill(&mut name);
                let on = if self.rng.below(2) == 0 { 0 } else { file };
                (
                    on,
                    Payload::Mark(Mark {
                        purpose: MarkPurpose::RestorePoint,
                        name,
                    }),
                )
            }
            87..=90 => (0, Payload::Empty),
            91..=93 => (0, Payload::Members(Members::FormatVersion(1))),
            _ => (file, Payload::Change(self.change(model, file))),
        }
    }

    fn added(&mut self, next_index: u64) -> (u64, Payload) {
        self.names += 1;
        let mut page0 = vec![0u8; self.rng.below(64) as usize];
        self.rng.fill(&mut page0);
        (
            next_index,
            Payload::File(FileEvent::Added {
                name: format!("file {}", self.names).into_bytes(),
                flags: self.rng.below(2) as u32,
                page0,
            }),
        )
    }

    fn bytes(&mut self, max: u64) -> Vec<u8> {
        let mut out = vec![0u8; self.rng.below(max + 1) as usize];
        self.rng.fill(&mut out);
        out
    }

    fn session(&mut self, model: &Model) -> SessionEvent {
        let server = SERVERS[self.rng.below(2) as usize];
        match (model.servers.get(&server), self.rng.below(10)) {
            (None, _) | (Some(_), 0) => SessionEvent::Started {
                server,
                count: model.servers.get(&server).map_or(1, |r| r.0 + 1),
            },
            (Some(&(start, highest)), 1..=6) => SessionEvent::Opened(Session {
                server,
                start,
                connection: highest + 1,
            }),
            _ => {
                let open: Vec<Session> = model.sessions.keys().copied().collect();
                match open.len() {
                    0 => SessionEvent::Closed(Session {
                        server,
                        start: 9,
                        connection: 9,
                    }),
                    n => SessionEvent::Closed(open[self.rng.below(n as u64) as usize]),
                }
            }
        }
    }

    fn change(&mut self, model: &Model, file: u64) -> Change {
        let mut change = Change::default();
        let open: Vec<(Session, u64)> = model.sessions.iter().map(|(s, (q, _))| (*s, *q)).collect();
        if !open.is_empty() && self.rng.below(10) < 6 {
            let (session, seq) = open[self.rng.below(open.len() as u64) as usize];
            change.op = Some(Op {
                session,
                seq: seq + 1,
            });
            change.reply = self.bytes(40);
        }
        for _ in 0..self.rng.below(3) {
            change.notices.push(self.bytes(30));
        }
        if !open.is_empty() && self.rng.below(3) == 0 {
            let held: Vec<(Session, Vec<u8>, Vec<u8>)> = model
                .locks
                .keys()
                .filter(|l| l.file == file)
                .map(|l| (l.session, l.key.clone(), l.user.clone()))
                .collect();
            let session = open[self.rng.below(open.len() as u64) as usize].0;
            if !held.is_empty() && self.rng.below(3) == 0 {
                let (session, key, user) = held[self.rng.below(held.len() as u64) as usize].clone();
                change.locks.push(LockEdit::Remove { session, key, user });
            } else {
                change.locks.push(LockEdit::Set {
                    session,
                    key: vec![self.rng.below(8) as u8],
                    user: vec![b'u'],
                    value: self.bytes(10),
                });
            }
        }
        let paths: Vec<&Path> = model.files[&file].nodes.keys().collect();
        if self.rng.below(15) == 0 {
            // A large change: more pages than a first reservation holds,
            // values in overflow pages of their own.
            let top = vec![0xf0, self.rng.below(4) as u8];
            for j in 0..8 + self.rng.below(3) as u16 {
                let mut value = vec![0u8; 4500];
                self.rng.fill(&mut value);
                change.edits.push(Edit::leaf(
                    vec![top.clone(), j.to_be_bytes().to_vec()],
                    value,
                ));
            }
            return change;
        }
        for _ in 0..1 + self.rng.below(4) {
            let edit = if !paths.is_empty() && self.rng.below(5) == 0 {
                Edit::delete(paths[self.rng.below(paths.len() as u64) as usize].clone())
            } else {
                let mut path: Path = if !paths.is_empty() && self.rng.below(2) == 0 {
                    paths[self.rng.below(paths.len() as u64) as usize].clone()
                } else {
                    Vec::new()
                };
                path.truncate(6);
                path.push(vec![self.rng.below(16) as u8]);
                let value = if self.rng.below(30) == 0 {
                    self.bytes(6000)
                } else {
                    self.bytes(60)
                };
                Edit::leaf(path, value)
            };
            change.edits.push(edit);
        }
        change
    }
}

/// The trees a view holds, against the model: `Ok` when every read
/// returned the model's, `Err` naming the first read that failed. A read
/// that returns other data than the model's fails the test.
fn compare(view: &View, model: &Model, at: &str) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("{at}: {e}");
    let state = view.store_tree().map_err(|e| fail(&e))?;
    let Some(state) = state else {
        assert_eq!(model.applied, Position::START, "{at}: no store tree");
        return Ok(());
    };
    assert_eq!(
        state.applied().map_err(|e| fail(&e))?,
        model.applied,
        "{at}: last applied"
    );
    assert_eq!(view.position(), model.applied, "{at}: the view's entry");
    assert_eq!(state.format_version().map_err(|e| fail(&e))?, model.format);
    let files = state.files().map_err(|e| fail(&e))?;
    let want: Vec<(u64, FileRow)> = model
        .files
        .iter()
        .map(|(id, f)| {
            (
                *id,
                FileRow {
                    name: f.name.clone(),
                    flags: f.flags,
                    page0: f.page0.clone(),
                },
            )
        })
        .collect();
    assert!(files == want, "{at}: files");
    for (id, file) in &model.files {
        let tree = view
            .file_tree(*id)
            .map_err(|e| fail(&e))?
            .unwrap_or_else(|| panic!("{at}: no tree for file {id}"));
        let root: &[Vec<u8>] = &[];
        let mut got = Vec::new();
        for item in tree.walk(root) {
            let item = item.map_err(|e| fail(&e))?;
            got.push((item.key.path(), item.node));
        }
        let want: Vec<(Path, Node)> = file
            .nodes
            .iter()
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect();
        assert!(got == want, "{at}: file {id}'s tree");
    }
    let servers: BTreeMap<ServerId, (u64, u64)> = state
        .servers()
        .map_err(|e| fail(&e))?
        .into_iter()
        .map(|(s, ServerRow { start, connection })| (s, (start, connection)))
        .collect();
    assert_eq!(servers, model.servers, "{at}: servers");
    let sessions: BTreeMap<Session, (u64, Vec<u8>)> = state
        .sessions()
        .map_err(|e| fail(&e))?
        .into_iter()
        .map(|(s, SessionRow { seq, reply })| (s, (seq, reply)))
        .collect();
    assert!(sessions == model.sessions, "{at}: sessions");
    let mut locks = state.locks().map_err(|e| fail(&e))?;
    locks.sort();
    let mut want: Vec<LockRow> = model
        .locks
        .iter()
        .map(|(id, value)| LockRow {
            session: id.session,
            file: id.file,
            key: id.key.clone(),
            user: id.user.clone(),
            value: value.clone(),
        })
        .collect();
    want.sort();
    assert!(locks == want, "{at}: locks");
    Ok(())
}

/// The model after `record`'s entries up to `index`, from `from` (the
/// model at `from.applied`) when `record` still holds that entry, or from
/// the start. An entry the record replaced (a start's entry written again
/// after an accepted loss lowered the marker) has another hash.
fn model_at(record: &[Built], index: u64, from: Option<&Model>) -> Model {
    let holds = |m: &Model| match m.applied.index {
        0 => true,
        i => record
            .get(i as usize - 1)
            .is_some_and(|b| b.hash == m.applied.hash),
    };
    let mut model = match from {
        Some(m) if m.applied.index <= index && holds(m) => m.clone(),
        _ => Model::new(),
    };
    let from = model.applied.index;
    for built in record.iter().filter(|b| b.index > from && b.index <= index) {
        model
            .apply(&built.entry())
            .unwrap_or_else(|r| panic!("the model refuses entry {}: {r:?}", built.index));
    }
    model
}

/// Where the chain of entries up to `start` stops checking in a segment's
/// bytes, by the format's description (sections 3.2 to 3.5): from the
/// header's previous hash (bytes 64 to 96), through each batch header
/// that continues it (its mark, its first index at 16, its length at 24,
/// its count at 12, its previous hash at 40), a batch ending early where
/// such a header stands instead of an entry (a cut), and each entry's
/// hash, SHA-256 over the hash before it and its bytes before its own;
/// `start` must have `start_hash`. `None` when it checks through `start`.
fn chain_stops(bytes: &[u8], start: u64, start_hash: &[u8; 32]) -> Option<u64> {
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let header = |at: usize, next: u64, prev: &[u8; 32]| -> Option<(u32, usize)> {
        let fits = at + 136 <= bytes.len()
            && &bytes[at..at + 8] == b"OMBATCH\0"
            && u64_at(at + 16) == next
            && bytes[at + 40..at + 72] == prev[..];
        fits.then(|| (u32_at(at + 12), u64_at(at + 24) as usize))
    };
    if bytes.len() < 128 {
        return Some(0);
    }
    let mut next = u64_at(48);
    let mut prev: [u8; 32] = bytes[64..96].try_into().unwrap();
    let mut at = 128usize;
    while next <= start {
        let Some((count, length)) = header(at, next, &prev) else {
            return Some(next);
        };
        let mut entry = at + 136;
        at = entry + length;
        for _ in 0..count {
            if next > start {
                return None;
            }
            if header(entry, next, &prev).is_some() {
                at = entry;
                break;
            }
            if entry + 32 > bytes.len() {
                return Some(next);
            }
            let len = u32_at(entry + 28) as usize;
            let content = 32 + len;
            if u64_at(entry) != next || entry + content + 32 > bytes.len() {
                return Some(next);
            }
            let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
            digest.update(&prev);
            digest.update(&bytes[entry..entry + content]);
            let hash: [u8; 32] = digest.finish().as_ref().try_into().unwrap();
            if hash[..] != bytes[entry + content..entry + content + 32]
                || (next == start && hash != *start_hash)
            {
                return Some(next);
            }
            prev = hash;
            next += 1;
            entry += content + 32;
        }
    }
    None
}

/// Counts over many runs, to show the schedules reach what they test.
#[derive(Debug, Default)]
struct Reached {
    appended: u64,
    flushes: u64,
    published: u64,
    several_in_a_flush: u64,
    extended: u64,
    views: u64,
    held_view_checks: u64,
    checkpoints: u64,
    refused_checkpoints: u64,
    kept: u64,
    restore_reads: u64,
    dropped: u64,
    retired: u64,
    freed: u64,
    checkpoint_needed: u64,
    full: u64,
    stopped_after_failure: u64,
    stops: u64,
    power_losses: u64,
    opens: u64,
    replayed: u64,
    passed_over_checkpoints: u64,
    damage_accepted: u64,
    unreadable_given_up: u64,
    unreadable_ends: u64,
    passed_over_state: u64,
    last_batch_may_be_lost: u64,
    damaged_reads: u64,
    ended_unopenable: u64,
    space_freed: u64,
    fallback_reported: u64,
    chain_reported: u64,
    chain_damaged: u64,
    chain_excused: u64,
    kinds: BTreeMap<u16, u64>,
    fired: BTreeMap<Fault, u64>,
}

/// One seeded run.
struct Run {
    sim: Sim,
    fs: Arc<dyn Fs>,
    store: Option<Store>,
    plan: Rng,
    /// Draws for the calls that must be refused without changing anything,
    /// kept apart from `plan` so they do not move the run's other draws.
    asks: Rng,
    gen: Gen,
    /// Every entry appended and not lost, by index.
    record: Vec<Built>,
    /// The model at the published entry, and at the appended one.
    published: Model,
    appended: Model,
    /// The last entry answered as stored, and the one before the last
    /// flush that answered (a lost copy of `state` can take the marker
    /// back over that flush).
    acked: u64,
    before_last_flush: u64,
    /// The place that stays unreadable, if one does.
    bad: Arc<std::sync::Mutex<Option<(String, u64)>>>,
    /// The latest entry any view showed.
    viewed: u64,
    /// The lowest entry an open may end at for a loss an explicit call
    /// reported in an error, which stands, or may (`AfterGivingUp`): the
    /// loss is accepted from the moment `state` records it.
    excused: Option<u64>,
    views: Vec<(View, Model)>,
    /// Restore points kept, with the model at their entry, the number of
    /// the checkpoint that wrote them and the device's step then.
    points: BTreeMap<String, (Model, u64, u64)>,
    names: u64,
    reached: Reached,
    seed: u64,
    trace: Vec<String>,
    /// The device may misplace or drop a write it reported done: the
    /// store's guarantees do not hold, and the run checks only that every
    /// call returns, typed, without a panic.
    lying: bool,
    /// The fault rates of the run, put back after an open without faults.
    rates: Rates,
    /// The store cannot be opened again, for a reason the device explains.
    unopenable: bool,
    /// The disk's size, when the run gives it one.
    capacity: Option<u64>,
    /// The step the current attempt to open began at.
    opening: u64,
}

/// A failing run prints what it did, to read with the seed.
impl Drop for Run {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("seed {}: the run's steps:", self.seed);
            for line in &self.trace {
                eprintln!("  {line}");
            }
            for event in self.sim.events() {
                if !matches!(event, sim::Event::Stopped { .. }) {
                    eprintln!("  {event:?}");
                }
            }
        }
    }
}

impl Run {
    fn note(&mut self, line: String) {
        let step = self.sim.steps();
        self.trace.push(format!("[{step}] {line}"));
    }

    fn new(seed: u64, rates: Rates) -> Run {
        let lying = rates.misplace != 0 || rates.lose_write != 0;
        let sim = Sim::new(seed);
        let flaky = Flaky::new(&sim);
        let bad = flaky.bad.clone();
        let fs: Arc<dyn Fs> = Arc::new(flaky);
        let (store, opened) = Store::create(fs.clone(), &sim.random(), CONFIG).unwrap();
        // A small disk, which fills.
        let capacity = (seed % 4 == 3).then_some(3 << 20);
        if let Some(bytes) = capacity {
            sim.set_capacity(bytes);
        }
        sim.set_rates(rates);
        let mut run = Run {
            sim,
            fs,
            store: None,
            plan: Rng::stream(seed, 21),
            asks: Rng::stream(seed, 23),
            gen: Gen {
                rng: Rng::stream(seed, 22),
                names: 0,
            },
            record: Vec::new(),
            published: Model::new(),
            appended: Model::new(),
            acked: 0,
            before_last_flush: 0,
            bad,
            viewed: 0,
            excused: None,
            views: Vec::new(),
            points: BTreeMap::new(),
            names: 0,
            reached: Reached::default(),
            seed,
            trace: Vec::new(),
            lying,
            rates,
            unopenable: false,
            capacity,
            opening: 0,
        };
        run.opened(store, &opened);
        run
    }

    /// Whether the device damaged a file whose name begins with `prefix`.
    fn damaged(&self, prefix: &str) -> bool {
        self.sim
            .events()
            .iter()
            .any(|e| matches!(e, sim::Event::Damaged { path, .. } if path.starts_with(prefix)))
    }

    /// A read that failed is allowed where the device failed to read,
    /// damaged pages, or the process crashed during the read (the next call
    /// finds it stopped); never otherwise.
    fn read_failed(&mut self, why: String) {
        if self.lying {
            return;
        }
        assert!(
            why.contains("unreadable:") || self.damaged(pagefile::FILE) || self.sim.halted(),
            "seed {}: a read failed, though the device damaged no page: {why}",
            self.seed
        );
        self.reached.damaged_reads += 1;
    }

    /// The store stopped, or the process ends.
    fn stop(&mut self, power_loss: bool) {
        self.note(format!("stop, power loss {power_loss}"));
        self.store = None;
        self.views.clear();
        if power_loss {
            self.sim.crash(Fates::Drawn);
            self.reached.power_losses += 1;
        } else {
            self.reached.stops += 1;
        }
        self.sim.restart();
    }

    /// After an open: the checks of the module docs, and the model taken to
    /// the store's state.
    fn opened(&mut self, store: Store, opened: &Opened) {
        let seed = self.seed;
        *self.bad.lock().unwrap() = None;
        self.note(format!(
            "opened: checkpoint {} at {}, recovered {}, replayed {}, state passed over {:?}, \
             may be lost {}, gave up {:?}, log kept {} cut {:?}",
            opened.checkpoint.checkpoint,
            opened.checkpoint.entry.index,
            opened.recovered.index,
            opened.replayed,
            opened.passed_over_state,
            opened.last_batch_may_be_lost,
            opened.gave_up,
            opened.log.kept,
            opened.log.cut
        ));
        self.reached.opens += 1;
        self.reached.replayed += opened.replayed;
        self.reached.passed_over_checkpoints += opened.checkpoint.passed_over.len() as u64;
        if opened.passed_over_state.is_some() {
            self.reached.passed_over_state += 1;
        }
        if opened.last_batch_may_be_lost {
            self.reached.last_batch_may_be_lost += 1;
        }
        let durable = opened.recovered;
        // A loss an explicit call reports in this open: the store opens
        // exactly where it says (or, when nothing after the marker was
        // given up, at or after it, by whole batches after the marker).
        if let Some(gave_up) = &opened.gave_up {
            self.gave_up(gave_up);
        }
        let reported = match &opened.gave_up {
            Some(GaveUp::Damage(d)) if !self.lying => {
                if d.cut_count.is_some() {
                    assert_eq!(
                        durable.index, d.kept.index,
                        "seed {seed}: the loss reported"
                    );
                    assert_eq!(
                        durable.hash.0, d.kept.hash,
                        "seed {seed}: the loss reported"
                    );
                } else {
                    // Nothing after it was given up: it is the later of the
                    // marker and the checkpoint's entry (a lost copy of
                    // `state` can leave the marker behind the checkpoint).
                    let firm = d.marker.max(opened.checkpoint.entry.index);
                    assert_eq!(d.kept.index, firm, "seed {seed}: the loss reported");
                    assert!(
                        durable.index >= d.kept.index,
                        "seed {seed}: the loss reported"
                    );
                }
                Some(d.kept.index)
            }
            Some(GaveUp::UnreadableEnd { last, done, .. }) if !self.lying => {
                assert!(
                    done.is_some(),
                    "seed {seed}: a give-up that succeeded says what it did"
                );
                assert_eq!(durable.index, last.index, "seed {seed}: the end given up");
                assert_eq!(durable.hash.0, last.hash, "seed {seed}: the end given up");
                Some(last.index)
            }
            _ => None,
        };
        let floor = reported.into_iter().chain(self.excused.take()).min();
        let excused = |at: u64| self.lying || floor.is_some_and(|floor| at >= floor);
        // Check 2: nothing answered as stored is lost, but down to what an
        // explicit call reported giving up, or the last batch reported as
        // possibly lost.
        if durable.index < self.acked && !excused(durable.index) {
            assert!(
                opened.last_batch_may_be_lost && durable.index >= self.before_last_flush,
                "seed {seed}: entry {} was answered as stored, and the store opened at {}",
                self.acked,
                durable.index
            );
        }
        // Check 4: no view showed an entry that is not durable.
        if durable.index < self.viewed && !excused(durable.index) {
            assert!(
                opened.last_batch_may_be_lost,
                "seed {seed}: a view showed entry {}, and the store opened at {}",
                self.viewed, durable.index
            );
        }
        if self.lying {
            // A device that drops writes: what the store holds is not what
            // was written, and the model follows it as far as it can.
            self.record.truncate(durable.index as usize);
            let mut model = Model::new();
            for b in &self.record {
                if model.apply(&b.entry()).is_err() {
                    break;
                }
            }
            self.published = model;
            self.appended = self.published.clone();
            self.store = Some(store);
            return;
        }
        assert!(
            durable.index as usize <= self.record.len(),
            "seed {seed}: the store holds entry {} that was never appended",
            durable.index
        );
        self.record.truncate(durable.index as usize);
        if let Some(last) = self.record.last() {
            assert_eq!(last.hash, durable.hash, "seed {seed}: entry {}", last.index);
        }
        self.published = model_at(&self.record, durable.index, Some(&self.published));
        // This start's own entry.
        let start = Built::new(
            &durable,
            store.term(),
            0,
            &Payload::Session(SessionEvent::Started {
                server: store.server(),
                count: opened.start_count,
            }),
        );
        assert_eq!(
            start.hash, opened.started.hash,
            "seed {seed}: the start's entry"
        );
        self.published.apply(&start.entry()).unwrap();
        self.record.push(start);
        self.appended = self.published.clone();
        self.acked = opened.started.index;
        self.before_last_flush = durable.index;
        self.viewed = self.viewed.min(self.acked);
        // The chain before the checkpoint's entry, in the segment that holds
        // the entry after it, as a read shows it: what the open reports
        // must be where it stops checking (or, for a read the device failed,
        // no later), and nothing when it checks.
        let start = opened.checkpoint.entry;
        let header_first = |number: u64| {
            let path = format!("{}/{number:016x}", log::FOLDER);
            self.sim
                .visible(&path)
                .filter(|b| b.len() >= 128)
                .map(|b| (u64::from_le_bytes(b[48..56].try_into().unwrap()), b))
        };
        // The segment the open walked to its start, as it says.
        let anchor = opened
            .log
            .walked_before_start
            .and_then(header_first)
            .filter(|(first, _)| *first <= start.index);
        let stops = anchor
            .as_ref()
            .and_then(|(_, bytes)| chain_stops(bytes, start.index, &start.hash.0));
        // The device's damage shows in a read once the cache drops what it
        // held: a segment whose cache was dropped during this open may have
        // been read before it was.
        let changed = opened.log.walked_before_start.is_some_and(|n| {
            let path = format!("{}/{n:016x}", log::FOLDER);
            self.sim.events().iter().any(|e| {
                matches!(e, sim::Event::Evicted { step, path: p }
                    if *step >= self.opening && *p == path)
            })
        });
        match opened.before_start {
            Some(found) if found.unreadable => self.reached.chain_reported += 1,
            Some(_) => self.reached.chain_damaged += 1,
            None => {}
        }
        match (opened.before_start, stops) {
            _ if changed => self.reached.chain_excused += 1,
            (None, None) => {}
            (Some(found), _) if found.unreadable => {
                assert!(
                    stops.is_none_or(|at| found.index <= at),
                    "seed {seed}: a read failed at entry {}, after where the chain stops, {stops:?}",
                    found.index
                );
            }
            (Some(found), Some(at)) => assert_eq!(
                found.index, at,
                "seed {seed}: where the chain before the checkpoint's entry stops"
            ),
            (None, Some(at)) => {
                panic!(
                "seed {seed}: the chain before the checkpoint's entry {} stops checking at entry \
                 {at}, and the open reported nothing (the segment begins at {:?})",
                start.index,
                anchor.as_ref().map(|(first, _)| *first)
            )
            }
            (Some(found), None) => panic!(
                "seed {seed}: the open reported the chain stopping at {found:?}, and it checks"
            ),
        }
        let older = store.checkpoints().get(1).map(|c| c.entry.index);
        assert_eq!(
            opened.fallback_cannot_replay,
            opened
                .before_start
                .is_some_and(|f| older.is_some_and(|older| older < f.index)),
            "seed {seed}: the older checkpoint"
        );
        if opened.fallback_cannot_replay {
            self.reached.fallback_reported += 1;
        }
        // Restore points: every one a checkpoint at or before the one the
        // open used wrote is listed; those of a checkpoint it passed over
        // are gone with it.
        let listed: Vec<String> = store
            .restore_points()
            .into_iter()
            .map(|rp| rp.name)
            .collect();
        let used = opened.checkpoint.checkpoint;
        let events = self.sim.events();
        // An open that failed may have passed over, and cleared, the
        // checkpoint that wrote a point, for a read of the pages the device
        // failed (its number can then be used again).
        let pages_failed_since = |at: u64| {
            events.iter().any(|e| match e {
                sim::Event::ReadFailed { step, path } | sim::Event::Damaged { step, path, .. } => {
                    *step >= at && path == pagefile::FILE
                }
                sim::Event::Crash {
                    step,
                    op: Some(sys::Op::Read),
                    path,
                    ..
                } => *step >= at && path == pagefile::FILE,
                _ => false,
            })
        };
        let mut gone = Vec::new();
        self.points.retain(|name, (_, number, at)| {
            if listed.contains(name) {
                return true;
            }
            if *number <= used && !pages_failed_since(*at) {
                gone.push(format!("{name} of checkpoint {number}"));
            }
            false
        });
        assert!(
            gone.is_empty(),
            "seed {seed}: restore points {gone:?} are gone, though the open used checkpoint {used} \
             and the device failed no read of the pages"
        );
        self.store = Some(store);
        self.check_published("after the open");
    }

    /// An explicit call's decision, counted; the entry it kept.
    fn gave_up(&mut self, gave_up: &GaveUp) -> u64 {
        match gave_up {
            GaveUp::Damage(d) => {
                self.reached.damage_accepted += 1;
                d.kept.index
            }
            GaveUp::UnreadableEnd { last, .. } => {
                self.reached.unreadable_given_up += 1;
                last.index
            }
        }
    }

    /// The published version against the model.
    fn check_published(&mut self, at: &str) {
        if self.lying {
            return;
        }
        let Some(store) = &self.store else { return };
        let Ok(view) = store.view() else { return };
        if let Err(why) = compare(&view, &self.published, at) {
            self.read_failed(why);
        }
    }

    /// Opens the store again, until it opens or says why it cannot.
    fn open(&mut self) -> bool {
        let seed = self.seed;
        for attempt in 0..60 {
            if attempt == 40 {
                // A disk that keeps failing the open: without faults the
                // store opens, or fails for a reason the device explains.
                self.note("faults off".into());
                self.sim.set_rates(Rates::default());
            }
            self.sim.restart();
            self.opening = self.sim.steps();
            let opened = Store::open(self.fs.clone(), CONFIG);
            if let Err(e) = &opened {
                self.note(format!("open: {e}"));
            }
            let opened = match opened {
                Err(OpenError::LogDamaged(damage)) => {
                    assert!(
                        self.lying || self.damaged("log/"),
                        "seed {seed}: {damage:?}, though the device damaged no segment"
                    );
                    self.sim.restart();
                    Store::accept_damage(self.fs.clone(), CONFIG)
                }
                Err(OpenError::UnreadableEnd { end, last }) => {
                    self.reached.unreadable_ends += 1;
                    // The place stays unreadable: the give-up must not need
                    // to read it again.
                    *self.bad.lock().unwrap() =
                        Some((format!("{}/{:016x}", log::FOLDER, end.segment), end.offset));
                    self.sim.restart();
                    let given = Store::give_up_unreadable_end(self.fs.clone(), CONFIG, last);
                    if matches!(given, Err(OpenError::Refused(_))) {
                        // The place reads now: nothing to give up.
                        continue;
                    }
                    given
                }
                other => other,
            };
            if let Err(e) = &opened {
                self.note(format!("then: {e}"));
            }
            // A decision an explicit call recorded, or may have, before
            // the open failed: it excuses a loss down to what it kept.
            let opened = match opened {
                Err(OpenError::AfterGivingUp(failed)) => {
                    let GaveUpThenFailed {
                        gave_up,
                        recorded,
                        error,
                    } = *failed;
                    self.note(format!("gave up {gave_up:?} ({recorded:?}), then: {error}"));
                    let kept = self.gave_up(&gave_up);
                    self.excused = Some(self.excused.map_or(kept, |e| e.min(kept)));
                    Err(error)
                }
                other => other,
            };
            match opened {
                Ok((store, opened)) => {
                    if attempt >= 40 {
                        self.sim.set_rates(self.rates);
                    }
                    self.opened(store, &opened);
                    return true;
                }
                Err(OpenError::Start(failed)) if failed.written.is_some() => {
                    let (at, payload) = failed.written.expect("written");
                    // The start's own entry is in the log, and may be found
                    // durable by the next open.
                    let index = at.index as usize;
                    if self.lying && index > self.record.len() + 1 {
                        continue;
                    }
                    assert!(index >= 1 && index <= self.record.len() + 1, "seed {seed}");
                    self.record.truncate(index - 1);
                    let prev = self.record.last().map_or(Position::START, |b| Position {
                        index: b.index,
                        term: b.term,
                        hash: b.hash,
                    });
                    let built = Built::new(&prev, at.term, 0, &payload);
                    assert!(
                        self.lying || built.hash == at.hash,
                        "seed {seed}: the start's entry"
                    );
                    self.record.push(built);
                    continue;
                }
                // A full disk: the administrator frees space, as the
                // remedy says, and the store opens. (Elsewhere the device
                // drew a refusal for space, which passes.)
                Err(e) if e.remedy().starts_with("free space") => {
                    if let Some(bytes) = self.capacity {
                        let bytes = bytes + (1 << 20);
                        self.capacity = Some(bytes);
                        self.sim.set_capacity(bytes);
                        self.reached.space_freed += 1;
                    }
                    continue;
                }
                // The framing before the checkpoint's entry broken: the
                // acceptance refuses, and only a restore helps.
                Err(e @ OpenError::Log(LogError::FramingBroken { .. })) => {
                    assert!(self.lying || self.damaged("log/"), "seed {seed}: {e}");
                    self.reached.ended_unopenable += 1;
                    self.unopenable = true;
                    return false;
                }
                Err(
                    OpenError::Disk(_)
                    | OpenError::Start(_)
                    | OpenError::NoSpace(_)
                    | OpenError::State(StateError::Disk(_))
                    | OpenError::State(StateError::Unreadable { .. })
                    | OpenError::Log(_)
                    | OpenError::Unreadable(_)
                    | OpenError::Pages(pagefile::OpenError::Disk(_))
                    | OpenError::UnreadableEnd { .. }
                    | OpenError::LogDamaged(_),
                ) => continue,
                Err(e @ OpenError::Pages(pagefile::OpenError::NoCheckpoint { .. }))
                    if e.to_string().contains("unreadable:") =>
                {
                    // A read the device failed passed the checkpoint over.
                    continue;
                }
                Err(e @ OpenError::State(StateError::NoValidCopy { .. })) => {
                    assert!(
                        self.lying || self.damaged(crate::state::FILE),
                        "seed {seed}: {e}"
                    );
                    self.reached.ended_unopenable += 1;
                    self.unopenable = true;
                    return false;
                }
                Err(e @ (OpenError::Pages(_) | OpenError::Replay { .. })) => {
                    assert!(
                        self.lying || self.damaged(pagefile::FILE),
                        "seed {seed}: {e}"
                    );
                    self.reached.ended_unopenable += 1;
                    self.unopenable = true;
                    return false;
                }
                Err(_) if self.lying => {
                    self.reached.ended_unopenable += 1;
                    self.unopenable = true;
                    return false;
                }
                Err(e) => panic!("seed {seed}: {e}"),
            }
        }
        panic!("seed {seed}: the store does not open on a disk without faults")
    }

    /// A step of the schedule; false when the store cannot be opened again.
    fn step(&mut self) -> bool {
        if self.store.is_none() {
            return self.open();
        }
        match self.plan.below(100) {
            0..=39 => {
                for _ in 0..1 + self.plan.below(3) {
                    if !self.append() {
                        break;
                    }
                }
            }
            40..=59 => self.flush(),
            60..=66 => self.take_view(),
            67..=74 => self.check_views(),
            75..=80 => self.checkpoint(),
            81..=82 => self.drop_point(),
            83..=85 => self.read_point(),
            86..=89 => {
                if let Some(Ok(r)) = self.store.as_mut().map(|s| s.reclaim(64)) {
                    self.reached.freed += r.freed;
                }
            }
            90..=94 => self.stop(false),
            _ => self.stop(true),
        }
        true
    }

    fn append(&mut self) -> bool {
        let seed = self.seed;
        let store = self.store.as_mut().expect("open");
        let next = store.next_index();
        let (file, payload) = self.gen.next(&self.appended, next);
        let built = Built::new(&self.appended.applied, store.term(), file, &payload);
        if self.appended.clone().apply(&built.entry()).is_err() {
            return true;
        }
        let appended = store.append(file, &payload);
        self.note(format!(
            "append {}: {:?}",
            built.index,
            appended
                .as_ref()
                .map(|a| a.position.index)
                .map_err(|e| e.to_string())
        ));
        match appended {
            Ok(appended) => {
                assert_eq!(appended.position.hash, built.hash, "seed {seed}");
                self.reached.extended += u64::from(appended.extended);
                self.reached.appended += 1;
                *self.reached.kinds.entry(built.kind).or_default() += 1;
                let outcome = self.appended.apply(&built.entry()).unwrap();
                let mut built = built;
                built.effects = Some((outcome.opened, outcome.closed));
                self.record.truncate(built.index as usize - 1);
                self.record.push(built);
                true
            }
            Err(WriteError::CheckpointNeeded(_)) => {
                self.reached.checkpoint_needed += 1;
                self.flush();
                self.checkpoint();
                false
            }
            Err(WriteError::Full(_) | WriteError::Log(_)) => {
                self.reached.full += 1;
                false
            }
            Err(WriteError::Apply(e)) => {
                self.read_failed(e.to_string());
                false
            }
            Err(WriteError::Failed(_) | WriteError::NoSpace(_) | WriteError::Stopped(_)) => {
                self.reached.stopped_after_failure += 1;
                let power_loss = self.plan.below(2) == 0;
                self.stop(power_loss);
                false
            }
            Err(WriteError::Refused(_)) if self.lying => false,
            Err(e) => panic!("seed {seed}: append: {e}"),
        }
    }

    fn flush(&mut self) {
        let seed = self.seed;
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let before = self.published.applied.index;
        let flushed = store.flush();
        self.note(format!(
            "flush after {before}: {:?}",
            flushed.as_ref().map(|p| p.len()).map_err(|e| e.to_string())
        ));
        let store = self.store.as_mut().expect("open");
        match flushed {
            Ok(published) => {
                self.reached.flushes += 1;
                self.reached.published += published.len() as u64;
                if published.len() > 1 {
                    self.reached.several_in_a_flush += 1;
                }
                if !self.lying {
                    // Every entry waiting is answered, in order, with its
                    // notices and reply, and the last is the published one.
                    let waiting =
                        &self.record[before as usize..self.appended.applied.index as usize];
                    assert_eq!(
                        published.len(),
                        waiting.len(),
                        "seed {seed}: the flush answered {} of the {} entries waiting",
                        published.len(),
                        waiting.len()
                    );
                    for (p, built) in published.iter().zip(waiting) {
                        assert_eq!(p.applied.position.index, built.index, "seed {seed}");
                        assert_eq!(p.applied.position.hash, built.hash, "seed {seed}");
                        let payload = Payload::decode(built.kind, &built.payload).unwrap();
                        assert_eq!(p.applied.payload, payload, "seed {seed}: notices and reply");
                        // What it did to the sessions, as the model says; and
                        // what its checks listed, within the format's bound of
                        // one row held per level of listing (section 5.8).
                        if let Some((opened, closed)) = &built.effects {
                            assert_eq!(&p.applied.opened, opened, "seed {seed}: opened");
                            let mut got = p.applied.closed.clone();
                            let mut want = closed.clone();
                            got.sort_by_key(|s| (s.server, s.start, s.connection));
                            want.sort_by_key(|s| (s.server, s.start, s.connection));
                            assert_eq!(got, want, "seed {seed}: closed");
                        }
                        assert!(p.applied.listed.held <= 2, "seed {seed}: listed");
                    }
                    let last = published
                        .last()
                        .map_or(before, |p| p.applied.position.index);
                    assert_eq!(
                        store.published().index,
                        last,
                        "seed {seed}: the published entry"
                    );
                }
                // What is answered as stored is what the flush returned.
                if let Some(last) = published.last() {
                    self.before_last_flush = self.acked;
                    self.acked = last.applied.position.index;
                }
                self.published = self.appended.clone();
            }
            Err(WriteError::Failed(_) | WriteError::NoSpace(_) | WriteError::Stopped(_)) => {
                self.reached.stopped_after_failure += 1;
                let power_loss = self.plan.below(2) == 0;
                self.stop(power_loss);
            }
            Err(e) => panic!("seed {seed}: flush: {e}"),
        }
    }

    fn take_view(&mut self) {
        if self.lying {
            return;
        }
        let Some(store) = &self.store else { return };
        let Ok(view) = store.view() else { return };
        self.reached.views += 1;
        self.viewed = self.viewed.max(view.position().index);
        match compare(&view, &self.published, "a new view") {
            Ok(()) => {
                if self.views.len() < 3 {
                    self.views.push((view, self.published.clone()));
                } else {
                    let at = self.plan.below(3) as usize;
                    self.views[at] = (view, self.published.clone());
                }
            }
            Err(why) => self.read_failed(why),
        }
    }

    fn check_views(&mut self) {
        for i in 0..self.views.len() {
            self.reached.held_view_checks += 1;
            if let Err(why) = compare(&self.views[i].0, &self.views[i].1, "a held view") {
                self.read_failed(why);
            }
        }
        if !self.views.is_empty() && self.plan.below(2) == 0 {
            self.views.remove(0);
        }
    }

    fn checkpoint(&mut self) {
        let seed = self.seed;
        if self.store.as_ref().is_some_and(|s| s.unpublished() > 0) {
            // A checkpoint is only of a published entry: with entries
            // waiting for their flush the store refuses and changes nothing.
            if self.asks.below(3) == 0 {
                let store = self.store.as_mut().expect("open");
                let published = store.published();
                let segments = store.log_segments();
                let refused = store.checkpoint(None);
                assert!(
                    matches!(refused, Err(CheckpointFailed::Refused(_))),
                    "seed {seed}: a checkpoint with entries waiting: {:?}",
                    refused.map(|d| d.entry.index)
                );
                assert_eq!(store.published(), published, "seed {seed}");
                assert_eq!(store.log_segments(), segments, "seed {seed}");
                self.reached.refused_checkpoints += 1;
            }
            self.flush();
        }
        let keep = (self.plan.below(4) == 0 && self.points.len() < 3).then(|| {
            self.names += 1;
            format!("point {}", self.names)
        });
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let segments = store.log_segments();
        let done = store.checkpoint(keep.as_deref());
        self.note(format!(
            "checkpoint {keep:?}: {:?}",
            done.as_ref()
                .map(|d| d.entry.index)
                .map_err(|e| e.to_string())
        ));
        let store = self.store.as_mut().expect("open");
        match done {
            Ok(done) => {
                self.reached.checkpoints += 1;
                self.reached.retired += done.retired;
                self.reached.freed += done.freed;
                assert!(store.log_segments().first >= segments.first, "seed {seed}");
                if let Some(name) = keep {
                    self.reached.kept += 1;
                    let number = done.written.number;
                    let step = self.sim.steps();
                    self.points
                        .insert(name, (self.published.clone(), number, step));
                }
            }
            Err(CheckpointFailed::Refused(_)) => {}
            Err(CheckpointFailed::NoSpace(_)) if store.stopped().is_none() => {}
            Err(
                CheckpointFailed::Failed(_)
                | CheckpointFailed::Stopped(_)
                | CheckpointFailed::NoSpace(_),
            ) => {
                self.reached.stopped_after_failure += 1;
                let power_loss = self.plan.below(2) == 0;
                self.stop(power_loss);
            }
        }
    }

    fn drop_point(&mut self) {
        let Some(name) = self.points.keys().next().cloned() else {
            return;
        };
        let Some(store) = self.store.as_mut() else {
            return;
        };
        if store.drop_restore_point(&name).is_ok() {
            self.points.remove(&name);
            self.reached.dropped += 1;
        }
    }

    fn read_point(&mut self) {
        if self.lying {
            return;
        }
        let Some((name, (model, _, _))) = self.points.iter().next() else {
            return;
        };
        let Some(store) = &self.store else { return };
        let view = match store.restore_point(name) {
            Ok(view) => view,
            Err(WriteError::Unreadable(e)) => {
                // The restore point's table of roots did not read back.
                self.read_failed(e.to_string());
                return;
            }
            Err(e) => panic!("seed {}: restore point {name}: {e}", self.seed),
        };
        let result = compare(&view, model, "a restore point");
        self.reached.restore_reads += 1;
        if let Err(why) = result {
            self.read_failed(why);
        }
    }

    /// The end of a run: no faults, a stop, and the whole state again.
    fn finish(mut self, reached: &mut Reached) {
        self.sim.set_rates(Rates::default());
        self.rates = Rates::default();
        if self.store.is_some() {
            self.stop(false);
        }
        // Without faults the store opens, unless the device made it
        // unopenable earlier for a reason it explains.
        if !self.unopenable {
            assert!(
                self.open(),
                "seed {}: the last open, without faults",
                self.seed
            );
            self.check_published("at the end");
        }
        for (fault, n) in self.sim.fired() {
            *self.reached.fired.entry(fault).or_default() += n;
        }
        add(reached, std::mem::take(&mut self.reached));
    }
}

fn add(into: &mut Reached, from: Reached) {
    macro_rules! sum {
        ($($f:ident),*) => { $( into.$f += from.$f; )* };
    }
    sum!(
        appended,
        flushes,
        published,
        several_in_a_flush,
        extended,
        views,
        held_view_checks,
        checkpoints,
        refused_checkpoints,
        kept,
        restore_reads,
        dropped,
        retired,
        freed,
        checkpoint_needed,
        full,
        stopped_after_failure,
        stops,
        power_losses,
        opens,
        replayed,
        passed_over_checkpoints,
        damage_accepted,
        unreadable_given_up,
        unreadable_ends,
        passed_over_state,
        last_batch_may_be_lost,
        damaged_reads,
        ended_unopenable,
        space_freed,
        fallback_reported,
        chain_reported,
        chain_damaged,
        chain_excused
    );
    for (k, n) in from.kinds {
        *into.kinds.entry(k).or_default() += n;
    }
    for (k, n) in from.fired {
        *into.fired.entry(k).or_default() += n;
    }
}

/// The faults the store answers for (honest device).
const FAULTS: Rates = Rates {
    crash: 400,
    fail_flush: 300,
    fail_write: 400,
    no_space: 500,
    fail_read: 300,
    damage: 3000,
    misplace: 0,
    lose_write: 0,
    writeback: 40,
};

fn run_seeds(seeds: &[u64], steps: u64) -> Reached {
    let mut reached = Reached::default();
    for &seed in seeds {
        let mut run = Run::new(seed, FAULTS);
        for _ in 0..steps {
            if !run.step() {
                break;
            }
        }
        run.finish(&mut reached);
    }
    reached
}

#[test]
fn the_store_survives_drawn_faults_against_the_model() {
    let seeds = seeds(24);
    let reached = run_seeds(&seeds, 150);
    eprintln!("{reached:?}");
    if seeds.len() >= 24 {
        for (what, n) in [
            ("appended", reached.appended),
            ("flushes", reached.flushes),
            ("several in a flush", reached.several_in_a_flush),
            ("extended reservations", reached.extended),
            ("views", reached.views),
            ("held view checks", reached.held_view_checks),
            ("checkpoints", reached.checkpoints),
            (
                "checkpoints refused with entries waiting",
                reached.refused_checkpoints,
            ),
            ("restore points kept", reached.kept),
            ("restore points read", reached.restore_reads),
            ("restore points dropped", reached.dropped),
            ("segments retired", reached.retired),
            ("stops after a failure", reached.stopped_after_failure),
            ("stops", reached.stops),
            ("power losses", reached.power_losses),
            ("entries replayed", reached.replayed),
            ("pages freed", reached.freed),
            ("refusals for a checkpoint", reached.checkpoint_needed),
            ("refusals for a full disk", reached.full),
            ("losses accepted after damage", reached.damage_accepted),
            ("unreadable ends given up", reached.unreadable_given_up),
            ("copies of state passed over", reached.passed_over_state),
            ("last batches reported lost", reached.last_batch_may_be_lost),
            (
                "chains before the start reported damaged",
                reached.chain_damaged,
            ),
            (
                "reads failed before the start reported",
                reached.chain_reported,
            ),
            (
                "older checkpoints reported unable to replay",
                reached.fallback_reported,
            ),
        ] {
            assert!(n > 0, "no {what}: {reached:?}");
        }
        for kind in Kind::ALL {
            assert!(
                reached.kinds.get(&kind.code()).copied().unwrap_or(0) > 0,
                "no {kind:?}: {reached:?}"
            );
        }
    }
}

/// A device that misplaces or drops writes it reported done: the store's
/// guarantees do not hold (section 6.12, "What is not found"), and every
/// call still returns, typed, with no panic.
#[test]
fn the_store_returns_typed_answers_on_a_lying_device() {
    let rates = Rates {
        misplace: 300,
        lose_write: 300,
        ..FAULTS
    };
    let mut reached = Reached::default();
    for seed in seeds(12) {
        let mut run = Run::new(seed, rates);
        for _ in 0..150 {
            if !run.step() {
                break;
            }
        }
        run.finish(&mut reached);
    }
    eprintln!("{reached:?}");
    assert!(reached.fired.get(&Fault::LoseWrite).copied().unwrap_or(0) > 0);
}

/// A fixed script that does every operation of the store, with a crash
/// placed at each of its steps in turn, each settled three ways: what was
/// published is there after the next open, exactly, and nothing else is.
#[test]
fn the_store_survives_a_crash_at_every_step() {
    fn script(sim: &Sim, record: &mut Vec<Built>, acked: &mut usize) -> Result<(), String> {
        let fs: Arc<dyn Fs> = Arc::new(sim.fs());
        let (mut store, opened) = match Store::open(fs.clone(), CONFIG) {
            Ok(opened) => opened,
            Err(OpenError::Start(failed)) if failed.written.is_some() => {
                let StartFailed { written, error } = *failed;
                let (at, payload) = written.expect("written");
                // The start's entry reached the log: it may be durable.
                record.truncate(at.index as usize - 1);
                let prev = record.last().map_or(Position::START, |b| Position {
                    index: b.index,
                    term: b.term,
                    hash: b.hash,
                });
                record.push(Built::new(&prev, at.term, 0, &payload));
                return Err(error.to_string());
            }
            Err(e) => return Err(e.to_string()),
        };
        record.truncate(opened.recovered.index as usize);
        let start = Built::new(
            &opened.recovered,
            store.term(),
            0,
            &Payload::Session(SessionEvent::Started {
                server: store.server(),
                count: opened.start_count,
            }),
        );
        record.push(start);
        *acked = record.len();
        let mut model = model_at(record, record.len() as u64, None);
        let file = match model.files.keys().next() {
            Some(id) => *id,
            None => {
                let id = store.next_index();
                let payload = Payload::File(FileEvent::Added {
                    name: b"f".to_vec(),
                    flags: 0,
                    page0: Vec::new(),
                });
                let built = Built::new(&model.applied, store.term(), id, &payload);
                store.append(id, &payload).map_err(|e| e.to_string())?;
                model.apply(&built.entry()).unwrap();
                record.push(built);
                id
            }
        };
        let mut views = Vec::new();
        for round in 0..3u8 {
            for i in 0..3u8 {
                let payload = put(&[round, i], &[i; 300]);
                let built = Built::new(&model.applied, store.term(), file, &payload);
                store.append(file, &payload).map_err(|e| e.to_string())?;
                model.apply(&built.entry()).unwrap();
                record.push(built);
            }
            store.flush().map_err(|e| e.to_string())?;
            *acked = record.len();
            let view = store.view().map_err(|e| e.to_string())?;
            views.push((view, model.clone()));
            let keep = (round == 1).then_some("kept");
            store.checkpoint(keep).map_err(|e| e.to_string())?;
            for (view, at) in &views {
                compare(view, at, "a view held").map_err(|e| e.to_string())?;
            }
        }
        if store.restore_points().iter().any(|rp| rp.name == "kept") {
            store
                .drop_restore_point("kept")
                .map_err(|e| e.to_string())?;
        }
        store.checkpoint(None).map_err(|e| e.to_string())?;
        Ok(())
    }
    // A store made, with its first start's entry, which the record holds.
    let setup = |sim: &Sim| -> Vec<Built> {
        let (store, opened) = made(sim);
        let start = Built::new(
            &Position::START,
            store.term(),
            0,
            &Payload::Session(SessionEvent::Started {
                server: store.server(),
                count: opened.start_count,
            }),
        );
        assert_eq!(start.hash, opened.started.hash);
        drop(store);
        sim.restart();
        vec![start]
    };
    let base = {
        let sim = Sim::new(40);
        let mut record = setup(&sim);
        let begin = sim.steps();
        let mut acked = 0;
        script(&sim, &mut record, &mut acked).unwrap();
        (begin, sim.steps())
    };
    let mut crashes = 0;
    for step in base.0 + 1..=base.1 {
        let step = step - base.0;
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            let sim = Sim::new(40);
            let mut record = setup(&sim);
            let mut acked = record.len();
            sim.set_crash_fates(fates);
            sim.place(sim.steps() + step, Fault::Crash);
            if script(&sim, &mut record, &mut acked).is_ok() {
                continue;
            }
            crashes += 1;
            let what = format!("crash at step {step}, {fates:?}");
            sim.restart();
            let (store, opened) =
                Store::open(Arc::new(sim.fs()), CONFIG).unwrap_or_else(|e| panic!("{what}: {e}"));
            let durable = opened.recovered.index as usize;
            assert!(durable >= acked, "{what}: {durable} < {acked}");
            assert!(durable <= record.len(), "{what}");
            record.truncate(durable);
            let model = model_at(&record, durable as u64, None);
            let mut model = model;
            let start = Built::new(
                &opened.recovered,
                store.term(),
                0,
                &Payload::Session(SessionEvent::Started {
                    server: store.server(),
                    count: opened.start_count,
                }),
            );
            model.apply(&start.entry()).unwrap();
            compare(&store.view().unwrap(), &model, &what).unwrap();
        }
    }
    assert!(crashes > 0);
}

#[test]
#[ignore = "long: 400 seeds of 400 steps on the simulated disk (OM_STORE_SCALE multiplies the seeds); run with --release"]
fn long_the_store_survives_drawn_faults_against_the_model() {
    let reached = run_seeds(&seeds(400), 400);
    eprintln!("{reached:?}");
}

/// A store on `sim` with `CONFIG`, made where there is none.
fn made(sim: &Sim) -> (Store, Opened) {
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    Store::create(fs, &sim.random(), CONFIG).unwrap()
}

fn reopened(sim: &Sim) -> Result<(Store, Opened), OpenError> {
    sim.restart();
    Store::open(Arc::new(sim.fs()), CONFIG)
}

/// A change of file `file` putting `value` at `path`.
fn put(path: &[u8], value: &[u8]) -> Payload {
    Payload::Change(Change {
        edits: vec![Edit::leaf(vec![path.to_vec()], value.to_vec())],
        notices: vec![b"note".to_vec()],
        ..Change::default()
    })
}

fn add_file(store: &mut Store, name: &str) -> u64 {
    let id = store.next_index();
    store
        .append(
            id,
            &Payload::File(FileEvent::Added {
                name: name.as_bytes().to_vec(),
                flags: 0,
                page0: Vec::new(),
            }),
        )
        .unwrap();
    store.flush().unwrap();
    id
}

/// What a view reads at `path` of file `file`.
fn read(view: &View, file: u64, path: &[u8]) -> Option<Vec<u8>> {
    let tree = view.file_tree(file).unwrap()?;
    tree.get(&[path]).unwrap().map(|n| n.value().to_vec())
}

/// Decision 3: a failure after an entry's unit committed (here the log's
/// flush) stops the store; every later call is refused; opened again, the
/// store is what the durable log says.
#[test]
fn a_failure_after_the_unit_committed_stops_the_store() {
    for fault in [Fault::FailWrite, Fault::FailFlush] {
        let sim = Sim::new(31);
        let (mut store, _) = made(&sim);
        let file = add_file(&mut store, "f");
        store.append(file, &put(b"a", b"1")).unwrap();
        store.flush().unwrap();
        let durable = store.published();
        store.append(file, &put(b"b", b"2")).unwrap();
        store.append(file, &put(b"c", b"3")).unwrap();
        // The batch's write is synchronous; a flush fault meets the marker.
        sim.place(sim.steps() + 1, fault);
        let flushed = store.flush();
        assert!(
            matches!(flushed, Err(WriteError::Failed(_))),
            "{fault:?}: {flushed:?}"
        );
        assert!(store.stopped().is_some());
        assert!(matches!(
            store.append(file, &put(b"d", b"4")),
            Err(WriteError::Stopped(_))
        ));
        assert!(matches!(store.flush(), Err(WriteError::Stopped(_))));
        assert!(store.view().is_err());
        assert!(matches!(
            store.checkpoint(None),
            Err(CheckpointFailed::Stopped(_))
        ));
        drop(store);
        sim.crash(Fates::LoseAll);
        let (store, opened) = reopened(&sim).unwrap();
        // The batch written before the fault may be found whole; what was
        // published before it is there.
        assert!(opened.recovered.index >= durable.index, "{fault:?}");
        let view = store.view().unwrap();
        assert_eq!(read(&view, file, b"a"), Some(b"1".to_vec()));
        let b = read(&view, file, b"b");
        let c = read(&view, file, b"c");
        assert_eq!(
            b.is_some(),
            c.is_some(),
            "{fault:?}: one batch, whole or not"
        );
        assert_eq!(
            opened.recovered.index,
            durable.index + if b.is_some() { 2 } else { 0 }
        );
        assert_eq!(read(&view, file, b"d"), None);
    }

    // The folder's lock lost between the log's check before the unit and
    // its append (an entry that needs the next segment checks it in both):
    // the append is refused after the unit committed, and the store stops.
    let sim = Sim::new(31);
    let flaky = Flaky::new(&sim);
    let fs: Arc<dyn Fs> = Arc::new(flaky.clone());
    let (mut store, _) = Store::create(fs.clone(), &sim.random(), CONFIG).unwrap();
    let file = add_file(&mut store, "f");
    let big = vec![5u8; 40 << 10];
    store.append(file, &put(b"x", &big)).unwrap();
    store.flush().unwrap();
    let durable = store.published();
    *flaky.lock_checks_left.lock().unwrap() = Some(1);
    let failed = store.append(file, &put(b"y", &big));
    assert!(matches!(failed, Err(WriteError::Failed(_))), "{failed:?}");
    assert!(store.stopped().is_some());
    *flaky.lock_checks_left.lock().unwrap() = None;
    drop(store);
    sim.restart();
    let (store, opened) = Store::open(fs, CONFIG).unwrap();
    assert_eq!(opened.recovered, durable);
    assert_eq!(read(&store.view().unwrap(), file, b"y"), None);
}

/// A full disk refuses the entry with nothing appended, and the store
/// keeps reading and can checkpoint.
#[test]
fn a_full_disk_refuses_the_entry_and_the_store_keeps_reading() {
    let sim = Sim::new(32);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    store.checkpoint(None).unwrap();
    let used: u64 = sim
        .image()
        .iter()
        .map(|(_, b)| b.as_ref().map_or(0, |b| b.len() as u64))
        .sum();
    sim.set_capacity(used + (64 << 10));
    let big = vec![7u8; 6000];
    let mut refused = None;
    for i in 0..400u32 {
        let before = (store.appended(), store.unpublished());
        match store.append(file, &put(&i.to_be_bytes(), &big)) {
            Ok(_) => {
                if store.flush().is_err() {
                    panic!("a flush failed at entry {i}");
                }
            }
            Err(e @ (WriteError::Full(_) | WriteError::Log(_))) => {
                assert_eq!((store.appended(), store.unpublished()), before, "{e}");
                refused = Some(e);
                break;
            }
            Err(WriteError::CheckpointNeeded(_)) => {
                store.checkpoint(None).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
    let refused = refused.expect("the disk filled");
    assert!(store.stopped().is_none(), "{refused}");
    let view = store.view().unwrap();
    assert_eq!(read(&view, file, &0u32.to_be_bytes()), Some(big.clone()));
    store.checkpoint(None).unwrap();
}

/// When the newest checkpoint cannot be used, the open takes the one
/// before it and applies every entry after that one again, in order: the
/// log was kept from the oldest checkpoint.
#[test]
fn the_entries_after_the_older_checkpoint_are_applied_when_the_newest_cannot_be_used() {
    let sim = Sim::new(33);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    for i in 0..30u8 {
        store.append(file, &put(&[i], &[i; 200])).unwrap();
        if i % 3 == 2 {
            store.flush().unwrap();
        }
        if i == 10 || i == 20 {
            store.flush().unwrap();
            store.checkpoint(None).unwrap();
        }
    }
    store.flush().unwrap();
    let durable = store.published();
    let older = store.checkpoints()[1].entry.index;
    let newest = store.checkpoints()[0].number;
    drop(store);
    sim.restart();
    // Both copies of the newest checkpoint's superblock damaged, by the
    // format's description: the copy number at bytes 16 to 24 of a slot.
    let pages = sim
        .image()
        .into_iter()
        .find(|(p, _)| p == pagefile::FILE)
        .and_then(|(_, b)| b)
        .unwrap();
    let mut hit = 0;
    for slot in 0..pagefile::SLOTS {
        let at = slot * pagefile::SLOT;
        let number = u64::from_le_bytes(pages[at + 16..at + 24].try_into().unwrap());
        if number == newest {
            sim.damage(pagefile::FILE, (at + 100) as u64, 2).unwrap();
            hit += 1;
        }
    }
    assert_eq!(hit, 2);
    let (store, opened) = reopened(&sim).unwrap();
    assert_eq!(opened.checkpoint.entry.index, older);
    let invalid = opened
        .checkpoint
        .slots
        .iter()
        .filter(|s| matches!(s, pagefile::SlotSeen::Invalid(_)))
        .count();
    assert_eq!(invalid, 2, "{:?}", opened.checkpoint.slots);
    assert_eq!(opened.replayed, durable.index - older);
    assert_eq!(opened.recovered, durable);
    let view = store.view().unwrap();
    for i in 0..30u8 {
        assert_eq!(read(&view, file, &[i]), Some(vec![i; 200]), "entry {i}");
    }
}

/// A view held across a checkpoint and a restart of the writer side still
/// reads what it read: its pages are kept by its pin in the page file it
/// holds, and, after the restart, by the checkpoints on disk.
#[test]
fn a_view_is_held_across_a_checkpoint_and_a_restart() {
    let sim = Sim::new(34);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    for i in 0..5u8 {
        store.append(file, &put(&[i], b"old")).unwrap();
    }
    store.flush().unwrap();
    store.checkpoint(None).unwrap();
    let view = store.view().unwrap();
    for i in 0..5u8 {
        store.append(file, &put(&[i], b"new")).unwrap();
    }
    store.flush().unwrap();
    store.checkpoint(None).unwrap();
    assert_eq!(read(&view, file, &[3]), Some(b"old".to_vec()));
    // The writer side stops and opens again; the view stays.
    drop(store);
    let (mut store, _) = reopened(&sim).unwrap();
    for i in 0..5u8 {
        store.append(file, &put(&[i + 10], b"newer")).unwrap();
    }
    store.flush().unwrap();
    store.checkpoint(None).unwrap();
    for i in 0..5u8 {
        assert_eq!(read(&view, file, &[i]), Some(b"old".to_vec()), "{i}");
    }
    let now = store.view().unwrap();
    assert_eq!(read(&now, file, &[3]), Some(b"new".to_vec()));
}

/// A steady workload with checkpoints keeps the disk it uses bounded, and
/// the log's segments are retired.
#[test]
fn a_steady_workload_uses_bounded_space() {
    let sim = Sim::new(35);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    let mut ends = Vec::new();
    let mut segments = Vec::new();
    for round in 0..12u32 {
        for i in 0..40u32 {
            let key = (i % 20).to_be_bytes();
            store
                .append(file, &put(&key, &round.to_be_bytes().repeat(50)))
                .unwrap();
            store.flush().unwrap();
        }
        store.checkpoint(None).unwrap();
        ends.push(store.space().end);
        segments.push(store.log_segments().count);
    }
    let late = *ends[6..].iter().max().unwrap();
    assert!(
        late <= ends[5] + ends[5] / 4,
        "the page file keeps growing: {ends:?}"
    );
    assert!(
        segments.iter().all(|&n| n <= 3),
        "the log is not retired: {segments:?}"
    );
}

/// The two copies of the entry hash, the log's and `entry`'s, agree over
/// generated entries: the store checks each append's hash against the one
/// it applied, and this compares them directly.
#[test]
fn the_log_and_the_entries_hash_an_entry_alike() {
    let sim = Sim::new(36);
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    let state = State::create(&*fs, &sim.random(), fs.lock().unwrap()).unwrap();
    let config = log::Config {
        segment_size: 1 << 20,
        max_payload: 64 << 10,
    };
    let (mut log, _) = Log::open(fs, state, config, log::Position::START).unwrap();
    let mut rng = Rng::new(36);
    let mut prev = log::Position::START;
    for _ in 0..300 {
        let term = prev.term + rng.below(2);
        let kind = rng.below(10) as u16;
        let file = rng.next_u64();
        let mut payload = vec![0u8; rng.below(3000) as usize];
        rng.fill(&mut payload);
        let at = log.append(term, kind, file, &payload).unwrap();
        let want = entry_hash(&Hash(prev.hash), prev.index + 1, term, kind, file, &payload);
        assert_eq!(at.hash, want.0);
        prev = at;
    }
}

/// Accepting the loss after damage through the marker: the store opens at
/// the last entry the log still holds whole, says so, and the call is
/// refused when there is no such loss.
#[test]
fn accepting_the_loss_after_damage_opens_the_store_at_what_the_log_still_holds() {
    let sim = Sim::new(37);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    store.checkpoint(None).unwrap();
    let mut positions = Vec::new();
    for i in 0..12u8 {
        store.append(file, &put(&[i], &[i; 100])).unwrap();
        store.flush().unwrap();
        positions.push(store.published());
    }
    drop(store);
    sim.restart();
    // Nothing to accept yet.
    assert!(matches!(
        Store::accept_damage(Arc::new(sim.fs()), CONFIG),
        Err(OpenError::Log(LogError::Refused(_)))
    ));
    // Damage in the segment, at an entry past the middle (by the log's
    // format: the last segment's batches; here every entry's payload holds
    // its own byte repeated).
    let (path, bytes) = sim
        .image()
        .into_iter()
        .filter(|(p, _)| p.starts_with("log/"))
        .max()
        .map(|(p, b)| (p, b.unwrap()))
        .unwrap();
    let at = bytes.windows(100).position(|w| w == [8u8; 100]).unwrap();
    sim.damage(&path, at as u64 + 10, 4).unwrap();
    sim.restart();
    let damaged = Store::open(Arc::new(sim.fs()), CONFIG);
    assert!(
        matches!(damaged, Err(OpenError::LogDamaged(_))),
        "{damaged:?}"
    );
    sim.restart();
    let (store, opened) = Store::accept_damage(Arc::new(sim.fs()), CONFIG).unwrap();
    let Some(GaveUp::Damage(accepted)) = opened.gave_up else {
        panic!("{opened:?}")
    };
    // Entry `i` is at index positions[i]; the damaged payload is entry 8's.
    assert_eq!(accepted.kept.index, positions[7].index);
    assert_eq!(accepted.marker, positions[11].index);
    assert_eq!(opened.recovered.index, positions[7].index);
    let view = store.view().unwrap();
    assert_eq!(read(&view, file, &[7]), Some(vec![7; 100]));
    assert_eq!(read(&view, file, &[8]), None);
    drop(store);
    // It holds: the next open is an ordinary one.
    let (_, again) = reopened(&sim).unwrap();
    assert!(again.gave_up.is_none());
    assert!(again.recovered.index > positions[7].index);
}

/// A lost older segment gives up nothing the chain from the checkpoint the
/// store opens on still holds: accepting the loss keeps that chain whole
/// and drops only the segments before it, which the older checkpoint alone
/// needed.
#[test]
fn accepting_a_lost_older_segment_keeps_the_chain_from_the_checkpoint() {
    let sim = Sim::new(44);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    let first = store.log_segments().first;
    let mut written = 0u8;
    let mut write = |store: &mut Store, count: u8| {
        for _ in 0..count {
            store
                .append(file, &put(&[written], &[written; 6000]))
                .unwrap();
            store.flush().unwrap();
            written += 1;
        }
    };
    // The older checkpoint in the first segment; the newest after the
    // second segment began.
    write(&mut store, 3);
    store.checkpoint(None).unwrap();
    while store.log_segments().count < 2 {
        write(&mut store, 1);
    }
    let second_begins = store.published().index;
    write(&mut store, 2);
    store.checkpoint(None).unwrap();
    write(&mut store, 3);
    let durable = store.published();
    let newest = store.checkpoints()[0].entry.index;
    assert!(newest > second_begins);
    assert_eq!(
        store.log_segments().first,
        first,
        "the older checkpoint keeps it"
    );
    drop(store);
    sim.restart();
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    fs.remove(&format!("{}/{first:016x}", log::FOLDER)).unwrap();
    fs.flush_folder(log::FOLDER).unwrap();
    let damaged = Store::open(fs.clone(), CONFIG);
    assert!(
        matches!(&damaged, Err(OpenError::LogDamaged(d)) if d.kind == log::DamageKind::Missing),
        "{damaged:?}"
    );
    sim.restart();
    let (store, opened) = Store::accept_damage(fs.clone(), CONFIG).unwrap();
    assert_eq!(opened.checkpoint.entry.index, newest);
    let Some(GaveUp::Damage(accepted)) = opened.gave_up else {
        panic!("{opened:?}")
    };
    // Nothing confirmed was given up: only the segment before the chain.
    assert_eq!(accepted.kept.index, durable.index);
    assert_eq!(accepted.marker, durable.index);
    assert_eq!(accepted.cut_count, None);
    assert_eq!(accepted.listed_before.first, first);
    assert_eq!(accepted.listed.first, first + 1);
    assert_eq!(
        accepted.listed.first + accepted.listed.count,
        accepted.listed_before.first + accepted.listed_before.count
    );
    assert_eq!(opened.recovered, durable);
    let view = store.view().unwrap();
    for i in 0..written {
        assert_eq!(read(&view, file, &[i]), Some(vec![i; 6000]), "entry {i}");
    }
    drop(store);
    let (_, again) = reopened(&sim).unwrap();
    assert!(again.gave_up.is_none());
    assert_eq!(
        again.recovered.index,
        durable.index + 1,
        "the accept's start"
    );
}

/// A store whose `state` is gone is reported as one, to restore, and
/// nothing is created over it.
#[test]
fn a_store_without_state_is_reported_and_not_created_over() {
    let sim = Sim::new(45);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    store.append(file, &put(b"a", b"1")).unwrap();
    store.flush().unwrap();
    drop(store);
    sim.restart();
    let fs: Arc<dyn Fs> = Arc::new(sim.fs());
    fs.remove(crate::state::FILE).unwrap();
    fs.flush_folder("").unwrap();
    let before = sim.image();
    let opened = Store::open(fs.clone(), CONFIG);
    let Err(e @ OpenError::NoState { .. }) = opened else {
        panic!("{opened:?}")
    };
    assert!(e.remedy().contains("restore"), "{e}");
    sim.restart();
    let created = Store::create(fs.clone(), &sim.random(), CONFIG);
    assert!(
        matches!(created, Err(OpenError::NoState { .. })),
        "{created:?}"
    );
    assert!(sim.image() == before, "nothing was changed");
}

/// A creation cut short at any step leaves a folder that opens as the
/// store, or as none (with the creation's marker), never as a store
/// without `state`; a creation begun again then succeeds.
#[test]
fn a_creation_cut_short_at_any_step_is_created_over() {
    let steps = {
        let sim = Sim::new(46);
        let begin = sim.steps();
        made(&sim);
        sim.steps() - begin
    };
    let mut cut = 0;
    for step in 1..=steps {
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            let sim = Sim::new(46);
            sim.set_crash_fates(fates);
            sim.place(sim.steps() + step, Fault::Crash);
            let fs: Arc<dyn Fs> = Arc::new(sim.fs());
            if Store::create(fs.clone(), &sim.random(), CONFIG).is_ok() {
                continue;
            }
            cut += 1;
            let what = format!("a crash at step {step} of the creation, {fates:?}");
            sim.restart();
            match Store::open(fs.clone(), CONFIG) {
                Ok(_) => {}
                Err(OpenError::Missing) => {
                    sim.restart();
                    Store::create(fs, &sim.random(), CONFIG)
                        .unwrap_or_else(|e| panic!("{what}: {e}"));
                }
                Err(e) => panic!("{what}: {e}"),
            }
        }
    }
    assert!(cut > 0);
}

/// The store starts with the smallest budget of pages waiting for a
/// checkpoint, and on a disk too full for an ordinary entry: this start's
/// entry is part of the recovery.
#[test]
fn the_store_starts_with_the_smallest_budget_and_on_a_full_disk() {
    let config = Config {
        pages: pagefile::Config {
            dirty_budget: 1,
            ..CONFIG.pages
        },
        ..CONFIG
    };
    let sim = Sim::new(47);
    let (store, _) = Store::create(Arc::new(sim.fs()), &sim.random(), config).unwrap();
    drop(store);
    let mut checkpoints = 0;
    for round in 0..4 {
        sim.restart();
        let (store, opened) =
            Store::open(Arc::new(sim.fs()), config).unwrap_or_else(|e| panic!("open {round}: {e}"));
        checkpoints += opened.replay_checkpoints;
        drop(store);
    }
    assert!(checkpoints > 0, "the budget was never reached");

    let sim = Sim::new(48);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    store.checkpoint(None).unwrap();
    let used: u64 = sim
        .image()
        .iter()
        .map(|(_, b)| b.as_ref().map_or(0, |b| b.len() as u64))
        .sum();
    sim.set_capacity(used + (64 << 10));
    let big = vec![7u8; 6000];
    let mut full = false;
    for i in 0..400u32 {
        match store.append(file, &put(&i.to_be_bytes(), &big)) {
            Ok(_) => {
                store.flush().unwrap();
            }
            Err(WriteError::Full(_)) => {
                full = true;
                break;
            }
            Err(WriteError::CheckpointNeeded(_)) => {
                store.checkpoint(None).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(full, "the disk filled");
    drop(store);
    for round in 0..6 {
        let (store, _) = reopened(&sim).unwrap_or_else(|e| panic!("open {round}: {e}"));
        drop(store);
    }
}

/// A restore point dropped but still listed by the older checkpoint on
/// disk keeps the log it needs until no superblock lists it: an open that
/// falls back to that checkpoint lists the point again.
#[test]
fn the_log_keeps_what_a_restore_point_on_disk_needs() {
    let sim = Sim::new(49);
    let (mut store, _) = made(&sim);
    let file = add_file(&mut store, "f");
    let mut written = 0u32;
    let mut write = |store: &mut Store, count: u32| {
        for _ in 0..count {
            store
                .append(file, &put(&written.to_be_bytes(), &[1; 6000]))
                .unwrap();
            store.flush().unwrap();
            written += 1;
        }
    };
    write(&mut store, 2);
    let point = store.checkpoint(Some("p")).unwrap().entry.index;
    write(&mut store, 12);
    store.checkpoint(None).unwrap();
    write(&mut store, 12);
    store.checkpoint(None).unwrap();
    store.drop_restore_point("p").unwrap();
    write(&mut store, 12);
    store.checkpoint(None).unwrap();
    assert!(
        store
            .pages
            .restore_points()
            .iter()
            .any(|(rp, dropped)| rp.name == "p" && *dropped),
        "the older checkpoint still lists it"
    );
    assert!(
        store.log.read(point + 1).is_ok(),
        "the log was retired past the point's entry"
    );
    let newest = store.checkpoints()[0].number;
    drop(store);
    sim.restart();
    let pages = sim
        .image()
        .into_iter()
        .find(|(p, _)| p == pagefile::FILE)
        .and_then(|(_, b)| b)
        .unwrap();
    for slot in 0..pagefile::SLOTS {
        let at = slot * pagefile::SLOT;
        let number = u64::from_le_bytes(pages[at + 16..at + 24].try_into().unwrap());
        if number == newest {
            sim.damage(pagefile::FILE, (at + 100) as u64, 2).unwrap();
        }
    }
    let (store, opened) = reopened(&sim).unwrap();
    assert!(opened.checkpoint.checkpoint < newest);
    assert!(store.restore_points().iter().any(|rp| rp.name == "p"));
    assert!(store.log.read(point + 1).is_ok());
}

/// A loss accepted before the open fails is in the error, and stands: the
/// next open starts from it. When the update of `state` that records it
/// fails, the error says the decision may or may not stand, and the next
/// open shows which.
#[test]
fn a_loss_accepted_before_the_open_fails_is_reported() {
    let sim = Sim::new(50);
    let flaky = Flaky::new(&sim);
    let fs: Arc<dyn Fs> = Arc::new(flaky.clone());
    let (mut store, _) = Store::create(fs.clone(), &sim.random(), CONFIG).unwrap();
    let file = add_file(&mut store, "f");
    store.checkpoint(None).unwrap();
    let mut positions = Vec::new();
    for i in 0..12u8 {
        store.append(file, &put(&[i], &[i; 100])).unwrap();
        store.flush().unwrap();
        positions.push(store.published());
    }
    drop(store);
    let (path, bytes) = sim
        .image()
        .into_iter()
        .filter(|(p, _)| p.starts_with("log/"))
        .max()
        .map(|(p, b)| (p, b.unwrap()))
        .unwrap();
    let at = bytes.windows(100).position(|w| w == [8u8; 100]).unwrap();
    sim.damage(&path, at as u64 + 10, 4).unwrap();
    let fail_writes = |prefix: &str, after: u64| {
        *flaky.fail_writes.lock().unwrap() = Some((prefix.to_owned(), after, ErrorKind::Write));
    };
    // The update of `state` that records the decision fails, after the
    // writes of `state`'s own open (counted on an open of it alone).
    fail_writes(crate::state::FILE, 1000);
    sim.restart();
    drop(State::open(&*fs, fs.lock().unwrap()).unwrap());
    let opening = 1000 - flaky.fail_writes.lock().unwrap().as_ref().unwrap().1;
    fail_writes(crate::state::FILE, opening);
    sim.restart();
    let failed = Store::accept_damage(fs.clone(), CONFIG);
    let Err(OpenError::AfterGivingUp(after)) = &failed else {
        panic!("{failed:?}")
    };
    let GaveUp::Damage(decided) = &after.gave_up else {
        panic!("{failed:?}")
    };
    assert_eq!(after.recorded, Recorded::Unknown);
    assert_eq!(decided.kept.index, positions[7].index);
    *flaky.fail_writes.lock().unwrap() = None;
    // It did not stand: the next open finds the damage again.
    sim.restart();
    let again = Store::open(fs.clone(), CONFIG);
    assert!(matches!(again, Err(OpenError::LogDamaged(_))), "{again:?}");
    // `state` records it, and this start's entry then fails: it stands.
    // The segments only (named by their numbers), not what the acceptance
    // sets aside.
    fail_writes("log/0", 0);
    sim.restart();
    let failed = Store::accept_damage(fs.clone(), CONFIG);
    let Err(OpenError::AfterGivingUp(after)) = &failed else {
        panic!("{failed:?}")
    };
    let GaveUp::Damage(decided) = &after.gave_up else {
        panic!("{failed:?}")
    };
    assert_eq!(after.recorded, Recorded::Done);
    assert!(
        matches!(after.error, OpenError::Start(_)),
        "{}",
        after.error
    );
    assert_eq!(decided.kept.index, positions[7].index);
    *flaky.fail_writes.lock().unwrap() = None;
    sim.restart();
    let (store, opened) = Store::open(fs.clone(), CONFIG).unwrap();
    assert!(opened.gave_up.is_none());
    assert_eq!(opened.recovered.index, positions[7].index);
    let view = store.view().unwrap();
    assert_eq!(read(&view, file, &[7]), Some(vec![7; 100]));
    assert_eq!(read(&view, file, &[8]), None);
}

/// This server's start entry is the store's to write: a caller's is
/// refused, as is a session of this server opened in another start, and
/// the next start closes the sessions of this one.
#[test]
fn a_caller_cannot_write_this_servers_start() {
    let sim = Sim::new(51);
    let (mut store, opened) = made(&sim);
    let server = store.server();
    let start = opened.start_count;
    let started = store.append(
        0,
        &Payload::Session(SessionEvent::Started {
            server,
            count: start + 5,
        }),
    );
    assert!(matches!(started, Err(WriteError::Rule(_))), "{started:?}");
    let elsewhere = Session {
        server,
        start: start + 1,
        connection: 1,
    };
    let opened_elsewhere = store.append(0, &Payload::Session(SessionEvent::Opened(elsewhere)));
    assert!(
        matches!(opened_elsewhere, Err(WriteError::Rule(_))),
        "{opened_elsewhere:?}"
    );
    assert_eq!(store.unpublished(), 0, "nothing was appended");
    let session = Session {
        server,
        start,
        connection: 1,
    };
    store
        .append(0, &Payload::Session(SessionEvent::Opened(session)))
        .unwrap();
    store.flush().unwrap();
    drop(store);
    let (store, again) = reopened(&sim).unwrap();
    assert_eq!(again.start_count, start + 1);
    let view = store.view().unwrap();
    let tree = view.store_tree().unwrap().unwrap();
    assert!(!tree.is_open(&session).unwrap(), "the next start closed it");
    assert_eq!(tree.server(&server).unwrap().unwrap().start, start + 1);
}

/// A store whose log spans four segments, with the older checkpoint in the
/// first and the newest in the second, for the shapes of damage below.
struct Shaped {
    sim: Sim,
    flaky: Flaky,
    file: u64,
    /// The newest checkpoint's entry, and the last durable one.
    start: u64,
    durable: Position,
    /// The segments' files, in order.
    segments: Vec<String>,
    /// Each tag's entry: its index, its segment (by place in `segments`)
    /// and the offset of its payload there.
    entries: BTreeMap<u8, (u64, usize, u64)>,
}

const SHAPE_PAYLOAD: usize = 6000;

/// The shapes' pieces: [`CONFIG`]'s, with room for their large entries
/// waiting for a checkpoint.
const SHAPE_CONFIG: Config = Config {
    pages: pagefile::Config {
        dirty_budget: 4096,
        ..CONFIG.pages
    },
    ..CONFIG
};

impl Shaped {
    fn new() -> Shaped {
        let sim = Sim::new(60);
        let flaky = Flaky::new(&sim);
        let fs: Arc<dyn Fs> = Arc::new(flaky.clone());
        let (mut store, _) = Store::create(fs, &sim.random(), SHAPE_CONFIG).unwrap();
        let file = add_file(&mut store, "f");
        let mut indexes = BTreeMap::new();
        let mut tag = 0u8;
        let mut write = |store: &mut Store| {
            store
                .append(file, &put(&[tag], &[tag; SHAPE_PAYLOAD]))
                .unwrap();
            store.flush().unwrap();
            indexes.insert(tag, store.published().index);
            tag += 1;
        };
        for _ in 0..3 {
            write(&mut store);
        }
        store.checkpoint(None).unwrap();
        while store.log_segments().count < 2 {
            write(&mut store);
        }
        write(&mut store);
        store.checkpoint(None).unwrap();
        let start = store.checkpoints()[0].entry.index;
        while store.log_segments().count < 4 {
            write(&mut store);
        }
        write(&mut store);
        let durable = store.published();
        let first = store.log_segments().first;
        assert_eq!(store.log_segments().count, 4);
        drop(store);
        sim.restart();
        let segments: Vec<String> = (first..first + 4)
            .map(|n| format!("{}/{n:016x}", log::FOLDER))
            .collect();
        let image = sim.image();
        let mut entries = BTreeMap::new();
        for (&tag, &index) in &indexes {
            for (at, path) in segments.iter().enumerate() {
                let bytes = image
                    .iter()
                    .find(|(p, _)| p == path)
                    .and_then(|(_, b)| b.as_ref())
                    .unwrap();
                if let Some(offset) = bytes
                    .windows(SHAPE_PAYLOAD)
                    .position(|w| w == [tag; SHAPE_PAYLOAD])
                {
                    entries.insert(tag, (index, at, offset as u64));
                }
            }
        }
        assert_eq!(entries.len(), indexes.len(), "every entry found once");
        Shaped {
            sim,
            flaky,
            file,
            start,
            durable,
            segments,
            entries,
        }
    }

    fn fs(&self) -> Arc<dyn Fs> {
        Arc::new(self.flaky.clone())
    }

    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.sim
            .image()
            .into_iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, b)| b)
    }

    /// The tags of the entries in segment `at`, in order.
    fn tags_in(&self, at: usize) -> Vec<u8> {
        self.entries
            .iter()
            .filter(|(_, (_, seg, _))| *seg == at)
            .map(|(tag, _)| *tag)
            .collect()
    }

    fn index(&self, tag: u8) -> u64 {
        self.entries[&tag].0
    }

    /// The tag of the entry at `index`.
    fn tag_at(&self, index: u64) -> u8 {
        *self
            .entries
            .iter()
            .find(|(_, (i, _, _))| *i == index)
            .expect("an entry of the shape")
            .0
    }

    fn remove(&self, at: usize) {
        let fs = self.sim.fs();
        fs.remove(&self.segments[at]).unwrap();
        fs.flush_folder(log::FOLDER).unwrap();
    }

    /// Damage `len` bytes of entry `tag`'s value, `from` bytes into it.
    fn damage(&self, tag: u8, from: u64, len: u64) {
        let (_, at, offset) = self.entries[&tag];
        self.sim
            .damage(&self.segments[at], offset + from, len)
            .unwrap();
    }

    /// Damage the index in entry `tag`'s fixed fields, which begin with it
    /// (format section 3.4): found as the entry's index, little-endian,
    /// last before its payload.
    fn damage_head(&self, tag: u8) {
        let (index, at, offset) = self.entries[&tag];
        let bytes = self.bytes(&self.segments[at]).unwrap();
        let from = offset.saturating_sub(256) as usize;
        let head = bytes[from..offset as usize]
            .windows(8)
            .rposition(|w| w == index.to_le_bytes())
            .expect("the entry's index before its payload");
        let place = (from + head) as u64;
        self.sim.damage(&self.segments[at], place, 8).unwrap();
    }

    /// Where segment `at`'s batches end: after its last batch, by the
    /// format's description of a batch header (its mark, `OMBATCH\0`, and
    /// the length of its entries at bytes 24 to 32; 136 bytes in all).
    fn end_of_batches(&self, at: usize) -> u64 {
        let bytes = self.bytes(&self.segments[at]).unwrap();
        let mark = bytes
            .windows(8)
            .rposition(|w| w == b"OMBATCH\0")
            .expect("a batch");
        let length = u64::from_le_bytes(bytes[mark + 24..mark + 32].try_into().unwrap());
        mark as u64 + 136 + length
    }

    /// Segment `at` cut to its first `len` bytes.
    fn shorten(&self, at: usize, len: u64) {
        let bytes = self.bytes(&self.segments[at]).unwrap();
        let fs = self.sim.fs();
        let path = &self.segments[at];
        fs.remove(path).unwrap();
        let file = fs.create(path, sys::FileOptions::PLAIN).unwrap();
        file.write_at(0, &bytes[..len as usize]).unwrap();
        file.flush().unwrap();
        fs.flush_folder(log::FOLDER).unwrap();
    }

    /// Segment `at`'s header made one of a newer format (its hash kept
    /// right, by the format's description: the version at bytes 8 to 10,
    /// the hash over the first 96 bytes at 96 to 128).
    fn newer(&self, at: usize) {
        let mut header = self.bytes(&self.segments[at]).unwrap()[..128].to_vec();
        header[8..10].copy_from_slice(&2u16.to_le_bytes());
        let hash = ring::digest::digest(&ring::digest::SHA256, &header[..96]);
        header[96..128].copy_from_slice(hash.as_ref());
        let file = self
            .sim
            .fs()
            .open(&self.segments[at], sys::FileOptions::PLAIN)
            .unwrap();
        file.write_at(0, &header).unwrap();
        file.flush().unwrap();
    }

    fn number(&self, at: usize) -> u64 {
        u64::from_str_radix(&self.segments[at][log::FOLDER.len() + 1..], 16).unwrap()
    }
}

/// What a shape of damage leads to.
enum Want {
    /// The ordinary open opens the store, with nothing lost, and reports
    /// that the chain before the checkpoint's entry stops checking at entry
    /// `stops` (in the segment that holds the entry after it), past which
    /// the older checkpoint could not replay.
    Opens { stops: u64 },
    /// The ordinary open reports damage, and accepting the loss keeps
    /// through entry `kept`, the segments `listed` (by place), lengthens
    /// the segments `lengthened`, and copies the one whose tail it gives
    /// up, `tail`.
    Accepts {
        kept: u64,
        listed: Option<(usize, usize)>,
        lengthened: Vec<usize>,
        tail: Option<usize>,
    },
    /// The acceptance is refused with nothing changed.
    Refused(fn(&OpenError) -> bool),
}

/// Accepting the loss after damage, over the shapes the damage can take: a
/// hole before, at and after the segment of the checkpoint's successor;
/// several holes; the last segment missing; an entry that does not verify
/// before the checkpoint's entry, at it and after it; a file short only in
/// its padding, and one short into an entry; a segment of a newer format; a
/// read the device fails while deciding; the framing broken before the
/// checkpoint's entry. Whatever is kept opens; whatever is given up is
/// copied first, byte for byte; a refusal changes nothing.
#[test]
fn accepting_the_loss_over_every_shape_of_damage() {
    type Shape = (&'static str, fn(&Shaped), fn(&Shaped) -> Want);
    let shapes: Vec<Shape> = vec![
        (
            "a hole before",
            |s| s.remove(0),
            |s| Want::Accepts {
                kept: s.durable.index,
                listed: Some((1, 3)),
                lengthened: vec![],
                tail: None,
            },
        ),
        (
            "a hole at the checkpoint's successor",
            |s| s.remove(1),
            |s| Want::Accepts {
                kept: s.start,
                listed: None,
                lengthened: vec![],
                tail: None,
            },
        ),
        (
            "a hole after",
            |s| s.remove(2),
            |s| Want::Accepts {
                kept: s.index(*s.tags_in(1).last().unwrap()),
                listed: Some((0, 1)),
                lengthened: vec![],
                tail: Some(1),
            },
        ),
        (
            "several holes",
            |s| {
                s.remove(0);
                s.remove(2);
            },
            |s| Want::Accepts {
                kept: s.index(*s.tags_in(1).last().unwrap()),
                listed: Some((1, 1)),
                lengthened: vec![],
                tail: Some(1),
            },
        ),
        (
            "the last segment missing",
            |s| s.remove(3),
            |s| Want::Accepts {
                kept: s.index(*s.tags_in(2).last().unwrap()),
                listed: Some((0, 2)),
                lengthened: vec![],
                tail: Some(2),
            },
        ),
        (
            "an entry that does not verify before the checkpoint's",
            |s| s.damage(s.tags_in(1)[0], 100, 4),
            |s| Want::Opens {
                stops: s.index(s.tags_in(1)[0]),
            },
        ),
        (
            "the checkpoint's entry does not verify",
            |s| s.damage(s.tag_at(s.start), 100, 4),
            |s| Want::Opens { stops: s.start },
        ),
        (
            "an entry after the checkpoint's does not verify",
            |s| s.damage(s.tag_at(s.start + 2), 100, 4),
            |s| Want::Accepts {
                kept: s.start + 1,
                listed: Some((0, 1)),
                lengthened: vec![],
                tail: Some(1),
            },
        ),
        (
            "a file short only in its padding",
            |s| {
                let last = *s.tags_in(3).last().unwrap();
                let end = s.entries[&last].2 + SHAPE_PAYLOAD as u64 + 32;
                s.shorten(3, end);
            },
            |s| Want::Accepts {
                kept: s.durable.index,
                listed: Some((0, 3)),
                lengthened: vec![3],
                tail: None,
            },
        ),
        (
            "a file short into its last entry",
            |s| {
                let last = *s.tags_in(3).last().unwrap();
                s.shorten(3, s.entries[&last].2 + 100);
            },
            |s| Want::Accepts {
                kept: s.durable.index - 1,
                listed: Some((0, 3)),
                lengthened: vec![3],
                tail: Some(3),
            },
        ),
        (
            "a segment of a newer format",
            |s| s.newer(2),
            |_| Want::Refused(|e| matches!(e, OpenError::Log(LogError::NewerFormat { .. }))),
        ),
        (
            "a read the device fails while deciding",
            |s| {
                s.remove(0);
                let tag = s.tag_at(s.start + 3);
                let (_, at, offset) = s.entries[&tag];
                *s.flaky.bad.lock().unwrap() = Some((s.segments[at].clone(), offset + 10));
            },
            |_| Want::Refused(|e| matches!(e, OpenError::Log(LogError::Unreadable { .. }))),
        ),
        (
            "the framing broken before the checkpoint's entry",
            |s| s.damage_head(s.tags_in(1)[0]),
            |_| Want::Refused(|e| matches!(e, OpenError::Log(LogError::FramingBroken { .. }))),
        ),
    ];
    for (name, harm, want) in shapes {
        let s = Shaped::new();
        harm(&s);
        let want = want(&s);
        let before = s.sim.image();
        s.sim.restart();
        let ordinary = Store::open(s.fs(), SHAPE_CONFIG);
        match want {
            Want::Opens { stops } => {
                let (store, opened) = ordinary.unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(opened.recovered, s.durable, "{name}");
                assert!(opened.gave_up.is_none(), "{name}");
                let reported = opened
                    .before_start
                    .expect("the chain before the start reported");
                assert_eq!(reported.index, stops, "{name}: where the chain stops");
                assert_eq!(reported.segment, s.number(1), "{name}");
                assert!(!reported.unreadable, "{name}");
                assert!(
                    opened.fallback_cannot_replay,
                    "{name}: the older checkpoint"
                );
                let view = store.view().unwrap();
                for &tag in s.entries.keys() {
                    let value = read(&view, s.file, &[tag]);
                    assert_eq!(value, Some(vec![tag; SHAPE_PAYLOAD]), "{name}: {tag}");
                }
                drop(store);
                s.sim.restart();
                let refused = Store::accept_damage(s.fs(), SHAPE_CONFIG);
                assert!(
                    matches!(refused, Err(OpenError::Log(LogError::Refused(_)))),
                    "{name}: {refused:?}"
                );
            }
            Want::Refused(is) => {
                assert!(ordinary.is_err(), "{name}: the ordinary open opened");
                drop(ordinary);
                // The log as it is: its files and any acceptance's folder
                // (the store's own open writes `state` and the page file's
                // slots, as every open does).
                let log = |sim: &Sim| -> Vec<(String, Option<Vec<u8>>)> {
                    sim.image()
                        .into_iter()
                        .filter(|(p, _)| p.starts_with("log"))
                        .collect()
                };
                let at_refusal = log(&s.sim);
                s.sim.restart();
                let refused = Store::accept_damage(s.fs(), SHAPE_CONFIG);
                let Err(e) = &refused else {
                    panic!("{name}: accepted")
                };
                assert!(is(e), "{name}: {e}");
                assert!(
                    log(&s.sim) == at_refusal,
                    "{name}: the refusal changed the log"
                );
            }
            Want::Accepts {
                kept,
                listed,
                lengthened,
                tail,
            } => {
                assert!(ordinary.is_err(), "{name}: the ordinary open opened");
                drop(ordinary);
                s.sim.restart();
                let (store, opened) = Store::accept_damage(s.fs(), SHAPE_CONFIG)
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                let Some(GaveUp::Damage(d)) = &opened.gave_up else {
                    panic!("{name}: {opened:?}")
                };
                assert_eq!(d.kept.index, kept, "{name}: kept");
                assert_eq!(opened.recovered.index, kept, "{name}: opened at");
                let want_listed = listed.map(|(a, b)| (s.number(a), (b - a + 1) as u64));
                let got_listed = (d.listed.count > 0).then_some((d.listed.first, d.listed.count));
                assert_eq!(got_listed, want_listed, "{name}: listed");
                let lengthened: Vec<u64> = lengthened.iter().map(|&a| s.number(a)).collect();
                assert_eq!(d.lengthened, lengthened, "{name}: lengthened");
                assert_eq!(d.tail_copied, tail.map(|a| s.number(a)), "{name}: tail");
                // Every file given up is kept whole under its new name, and
                // the segment whose tail is given up is copied byte for byte.
                let original = |number: u64| {
                    before
                        .iter()
                        .find(|(p, _)| *p == format!("{}/{number:016x}", log::FOLDER))
                        .and_then(|(_, b)| b.clone())
                };
                for given in &d.given_up {
                    match &given.moved_to {
                        Some(path) => assert!(
                            s.bytes(path) == original(given.number),
                            "{name}: segment {} moved",
                            given.number
                        ),
                        None => assert!(
                            original(given.number).is_none(),
                            "{name}: segment {} was there and not moved",
                            given.number
                        ),
                    }
                }
                if let Some(number) = d.tail_copied {
                    let prefix = d.prefix.as_ref().expect("names for what was given up");
                    let copy = s.bytes(&format!("{prefix}{number:016x}.tail"));
                    assert!(copy == original(number), "{name}: the tail's copy");
                    assert!(d.tail_unreadable.is_empty(), "{name}");
                }
                // The entries kept read back; those given up do not.
                let view = store.view().unwrap();
                for (&tag, &(index, _, _)) in &s.entries {
                    let value = read(&view, s.file, &[tag]);
                    if index <= kept {
                        assert_eq!(value, Some(vec![tag; SHAPE_PAYLOAD]), "{name}: {tag}");
                    } else {
                        assert_eq!(value, None, "{name}: {tag}");
                    }
                }
                drop(store);
                s.sim.restart();
                let (_, again) =
                    Store::open(s.fs(), SHAPE_CONFIG).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(again.gave_up.is_none(), "{name}");
                assert_eq!(
                    again.recovered.index,
                    kept + 1,
                    "{name}: the acceptance's start"
                );
            }
        }
    }
}

/// An acceptance `state` recorded, after which the log's open meets an
/// unreadable end, says what it gave up in the error; giving up the end
/// then opens the store, and the acceptance stands.
#[test]
fn an_unreadable_end_after_a_recorded_acceptance_says_what_was_given_up() {
    let s = Shaped::new();
    s.remove(0);
    let end = s.end_of_batches(3);
    *s.flaky.bad.lock().unwrap() = Some((s.segments[3].clone(), end));
    s.sim.restart();
    let failed = Store::accept_damage(s.fs(), SHAPE_CONFIG);
    let Err(OpenError::AfterGivingUp(after)) = &failed else {
        panic!("{failed:?}")
    };
    let GaveUp::Damage(accepted) = &after.gave_up else {
        panic!("{failed:?}")
    };
    assert_eq!(after.recorded, Recorded::Done);
    assert_eq!(accepted.listed.first, s.number(1));
    let OpenError::UnreadableEnd { last, .. } = &after.error else {
        panic!("{failed:?}")
    };
    s.sim.restart();
    let (store, opened) = Store::give_up_unreadable_end(s.fs(), SHAPE_CONFIG, *last).unwrap();
    assert_eq!(opened.recovered, s.durable);
    assert_eq!(
        store.log_segments().first,
        s.number(1),
        "the acceptance stands"
    );
}

/// An open that finds no `state` while another process creates the store,
/// between its two holds of the lock, opens what was created; it never
/// says to restore it.
#[test]
fn an_open_during_a_creation_opens_what_was_created() {
    let sim = Sim::new(61);
    let flaky = Flaky::new(&sim);
    *flaky.create_at_lock.lock().unwrap() = Some((1, sim.random()));
    let opened = Store::open(Arc::new(flaky), CONFIG);
    assert!(opened.is_ok(), "{opened:?}");
}

/// A start whose own entry finds no room after its reservation (the log's
/// write, the marker's write) says to free space, and the store opens once
/// there is room.
#[test]
fn a_start_that_finds_no_room_says_to_free_space() {
    for (path, measure) in [("log/", false), (crate::state::FILE, true)] {
        let sim = Sim::new(62);
        let flaky = Flaky::new(&sim);
        let fs: Arc<dyn Fs> = Arc::new(flaky.clone());
        drop(Store::create(fs.clone(), &sim.random(), CONFIG).unwrap());
        let fail = |after: u64| {
            *flaky.fail_writes.lock().unwrap() = Some((path.to_owned(), after, ErrorKind::NoSpace));
        };
        // For `state`, the writes of its own open go first (counted on an
        // open of it alone).
        let skip = if measure {
            fail(1000);
            sim.restart();
            drop(State::open(&*fs, fs.lock().unwrap()).unwrap());
            1000 - flaky.fail_writes.lock().unwrap().as_ref().unwrap().1
        } else {
            0
        };
        fail(skip);
        sim.restart();
        let failed = Store::open(fs.clone(), CONFIG);
        let Err(e @ OpenError::Start(_)) = &failed else {
            panic!("{path}: {failed:?}")
        };
        assert!(e.remedy().starts_with("free space"), "{path}: {e}");
        *flaky.fail_writes.lock().unwrap() = None;
        sim.restart();
        Store::open(fs, CONFIG).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
}

/// A configuration whose largest payload cannot hold this start's own
/// entry is refused before anything is created or opened: no file made,
/// no start counted.
#[test]
fn a_configuration_too_small_for_the_start_is_refused_first() {
    let small = Config {
        log: log::Config {
            max_payload: 25,
            ..CONFIG.log
        },
        ..CONFIG
    };
    let sim = Sim::new(63);
    let created = Store::create(Arc::new(sim.fs()), &sim.random(), small);
    assert!(matches!(created, Err(OpenError::Config(_))), "{created:?}");
    assert!(sim.image().is_empty(), "nothing was created");
    let (store, opened) = made(&sim);
    drop(store);
    sim.restart();
    let before = sim.image();
    let refused = Store::open(Arc::new(sim.fs()), small);
    assert!(matches!(refused, Err(OpenError::Config(_))), "{refused:?}");
    assert!(sim.image() == before, "nothing was written");
    let (_, again) = reopened(&sim).unwrap();
    assert_eq!(
        again.start_count,
        opened.start_count + 1,
        "no start counted"
    );
}
