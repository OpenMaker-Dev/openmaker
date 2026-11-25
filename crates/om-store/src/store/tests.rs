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
/// read faults (one read each) do not make.
struct Flaky {
    inner: sim::SimFs,
    bad: Arc<std::sync::Mutex<Option<(String, u64)>>>,
}

struct FlakyFile {
    inner: Box<dyn sys::File>,
    path: String,
    bad: Arc<std::sync::Mutex<Option<(String, u64)>>>,
}

impl sys::Fs for Flaky {
    fn create_root(&self) -> sys::Result<()> {
        self.inner.create_root()
    }
    fn lock(&self) -> sys::Result<sys::FolderLock> {
        self.inner.lock()
    }
    fn owns(&self, lock: &sys::FolderLock) -> bool {
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
    fn wrap(&self, path: &str, inner: Box<dyn sys::File>) -> Box<dyn sys::File> {
        Box::new(FlakyFile {
            inner,
            path: path.to_owned(),
            bad: self.bad.clone(),
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
/// model at `from.applied`) or from the start.
fn model_at(record: &[Built], index: u64, from: Option<&Model>) -> Model {
    let mut model = match from {
        Some(m) if m.applied.index <= index => m.clone(),
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
    kinds: BTreeMap<u16, u64>,
    fired: BTreeMap<Fault, u64>,
}

/// One seeded run.
struct Run {
    sim: Sim,
    fs: Arc<dyn Fs>,
    store: Option<Store>,
    plan: Rng,
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
    /// An "accept the loss" call was made since the last open succeeded:
    /// one that failed after `state` recorded it has taken effect all the
    /// same (the loss is accepted from the moment `state` records it).
    accepting: bool,
    views: Vec<(View, Model)>,
    points: BTreeMap<String, Model>,
    names: u64,
    reached: Reached,
    seed: u64,
    trace: Vec<String>,
    /// The device may misplace or drop a write it reported done: the
    /// store's guarantees do not hold, and the run checks only that every
    /// call returns, typed, without a panic.
    lying: bool,
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
        let bad = Arc::new(std::sync::Mutex::new(None));
        let fs: Arc<dyn Fs> = Arc::new(Flaky {
            inner: sim.fs(),
            bad: bad.clone(),
        });
        let (store, opened) = Store::create(fs.clone(), &sim.random(), CONFIG).unwrap();
        if seed % 4 == 3 {
            // A small disk, which fills.
            sim.set_capacity(3 << 20);
        }
        sim.set_rates(rates);
        let mut run = Run {
            sim,
            fs,
            store: None,
            plan: Rng::stream(seed, 21),
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
            accepting: false,
            views: Vec::new(),
            points: BTreeMap::new(),
            names: 0,
            reached: Reached::default(),
            seed,
            trace: Vec::new(),
            lying,
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

    /// A read that failed is allowed where the device failed to read, or
    /// damaged pages; never otherwise.
    fn read_failed(&mut self, why: String) {
        if self.lying {
            return;
        }
        assert!(
            why.contains("unreadable:") || self.damaged(pagefile::FILE),
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
        let accepted =
            opened.gave_up.is_some() || std::mem::take(&mut self.accepting) || self.lying;
        // Check 2: nothing answered as stored is lost, but by a loss the
        // store reports or the administrator accepted.
        if durable.index < self.acked && !accepted {
            assert!(
                opened.last_batch_may_be_lost && durable.index >= self.before_last_flush,
                "seed {seed}: entry {} was answered as stored, and the store opened at {}",
                self.acked,
                durable.index
            );
        }
        // Check 4: no view showed an entry that is not durable.
        if !accepted && !opened.last_batch_may_be_lost {
            assert!(
                durable.index >= self.viewed,
                "seed {seed}: a view showed entry {}, and the store opened at {}",
                self.viewed,
                durable.index
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
        // Restore points that did not survive (an older checkpoint taken).
        let listed: Vec<String> = store
            .restore_points()
            .into_iter()
            .map(|rp| rp.name)
            .collect();
        self.points.retain(|name, _| listed.contains(name));
        self.store = Some(store);
        self.check_published("after the open");
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
        for _ in 0..40 {
            self.sim.restart();
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
                    self.accepting = true;
                    let accepted = Store::accept_damage(self.fs.clone(), CONFIG);
                    if accepted.is_ok() {
                        self.reached.damage_accepted += 1;
                    }
                    accepted
                }
                Err(OpenError::UnreadableEnd { end, last }) => {
                    self.reached.unreadable_ends += 1;
                    if self.plan.below(4) == 0 {
                        continue;
                    }
                    // The place stays unreadable: the give-up must not need
                    // to read it again.
                    *self.bad.lock().unwrap() =
                        Some((format!("{}/{:016x}", log::FOLDER, end.segment), end.offset));
                    self.sim.restart();
                    self.accepting = true;
                    let given = Store::give_up_unreadable_end(self.fs.clone(), CONFIG, last);
                    if given.is_ok() {
                        self.reached.unreadable_given_up += 1;
                    }
                    if matches!(given, Err(OpenError::Refused(_))) {
                        // The place reads now: nothing to give up.
                        continue;
                    }
                    given
                }
                other => other,
            };
            match opened {
                Ok((store, opened)) => {
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
                    return false;
                }
                Err(e @ (OpenError::Pages(_) | OpenError::Replay { .. })) => {
                    assert!(
                        self.lying || self.damaged(pagefile::FILE),
                        "seed {seed}: {e}"
                    );
                    self.reached.ended_unopenable += 1;
                    return false;
                }
                Err(_) if self.lying => {
                    self.reached.ended_unopenable += 1;
                    return false;
                }
                Err(e) => panic!("seed {seed}: {e}"),
            }
        }
        // A disk that keeps failing the open: say so, and stop the run.
        self.reached.ended_unopenable += 1;
        false
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
                self.appended.apply(&built.entry()).unwrap();
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
            Err(WriteError::Failed(_) | WriteError::Stopped(_)) => {
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
        let before = store.published().index;
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
                let record = if self.lying {
                    &[][..]
                } else {
                    &self.record[before as usize..]
                };
                for (p, built) in published.iter().zip(record) {
                    assert_eq!(p.applied.position.index, built.index, "seed {seed}");
                    assert_eq!(p.applied.position.hash, built.hash, "seed {seed}");
                    let payload = Payload::decode(built.kind, &built.payload).unwrap();
                    assert_eq!(p.applied.payload, payload, "seed {seed}: notices and reply");
                }
                if !published.is_empty() {
                    self.before_last_flush = self.acked;
                }
                self.acked = store.published().index;
                self.published = self.appended.clone();
            }
            Err(WriteError::Failed(_) | WriteError::Stopped(_)) => {
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
                    self.points.insert(name, self.published.clone());
                }
            }
            Err(CheckpointFailed::Refused(_)) => {}
            Err(CheckpointFailed::Failed(_) | CheckpointFailed::Stopped(_)) => {
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
        let Some((name, model)) = self.points.iter().next() else {
            return;
        };
        let Some(store) = &self.store else { return };
        let view = match store.restore_point(name) {
            Ok(view) => view,
            Err(WriteError::Failed(why)) => {
                // The restore point's meta stream did not read back.
                self.read_failed(why);
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
        if self.store.is_some() {
            self.stop(false);
        }
        if self.open() {
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
        ended_unopenable
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
    let older = store.pages().kept_checkpoints()[1].entry.index;
    let newest = store.pages().checkpoint_entry().0;
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
        ends.push(store.pages().space().end);
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
