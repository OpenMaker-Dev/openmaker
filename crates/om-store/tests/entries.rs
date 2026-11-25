//! Entries applied to the trees, against the reference model (issue #727,
//! "How to check").
//!
//! Every random test is seeded and replays exactly. `OM_STORE_SEEDS` (how
//! many seeds, default 2) and `OM_STORE_ENTRIES` (entries per seed, default
//! 300) raise them; `OM_STORE_SEED` runs one seed. The ignored
//! `long_random_entries_match_the_model` runs 16 seeds of 1,500 entries
//! unless those say otherwise; run it with `--release`.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use om_hbam::OwnedNode;
use om_store::apply::{
    apply, Applied, Error, FileRow, LockRow, Position, Refusal, ServerRow, SessionRow, StoreTree,
    TreeId,
};
use om_store::entry::{
    entry_hash, Change, Entry, Feature, FileEvent, Kind, LockEdit, Mark, Members, Op, Payload,
    PayloadError, Purpose, ServerId, Session, SessionEvent, MAX_LOCK_KEY, MAX_NAME, MAX_PAGE0,
    MAX_REPLY,
};
use om_store::model::{self, File, Model};
use om_store::page::{
    EntryPages, Fault, Hash, MemEntry, MemStore, Numbering, Page, PageKind, PageNo, PageStore,
    Release, StoreError, TreeName, MAX_VALUE,
};
use om_store::pagefile::{Config as PageConfig, EntryId, PageFile, Purpose as PagePurpose};
use om_store::sim::{Fates, Rates, Sim};
use om_store::sys::{FolderLock, Fs};
use om_store::tree::{self, check_edits, key, Edit, Node, Root, Tree, MAX_DEPTH};

type Path = Vec<Vec<u8>>;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn percent(&mut self, p: usize) -> bool {
        self.below(100) < p
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

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
    (1..=env("OM_STORE_SEEDS", default)).collect()
}

/// An entry with its bytes, sealed after the model's last entry.
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

    /// The same entry, sealed after `prev` with these fields.
    fn raw(
        prev: &Position,
        index: u64,
        term: u64,
        kind: u16,
        file: u64,
        payload: Vec<u8>,
    ) -> Built {
        let hash = entry_hash(&prev.hash, index, term, kind, file, &payload);
        Built {
            index,
            term,
            kind,
            file,
            payload,
            hash,
        }
    }
}

fn seal(last: &Position, term: u64, file: u64, payload: &Payload) -> Built {
    let bytes = payload
        .encode()
        .expect("the generator makes entries within the bounds");
    Built::raw(
        last,
        last.index + 1,
        term,
        payload.kind().code(),
        file,
        bytes,
    )
}

const SERVERS: [ServerId; 3] = [[1; 16], [0; 16], [0xfe; 16]];

/// What the generated entries reached, summed over seeds.
#[derive(Debug, Default)]
struct Reached {
    kinds: BTreeMap<Kind, u64>,
    deep_puts: u64,
    overflow_values: u64,
    long_segments: u64,
    both_puts: u64,
    empty_lists: u64,
    big_deletes: u64,
    root_deletes: u64,
    lock_sets: u64,
    lock_removes: u64,
    file_releases: u64,
    closed_with_many_locks: u64,
    starts_that_closed: u64,
    stale_opens: u64,
    stale_closes: u64,
    stale_starts: u64,
    files_added: u64,
    files_removed: u64,
    files_removed_with_locks: u64,
    files_renamed: u64,
    names_reused: u64,
    replies: u64,
    notices: u64,
}

impl Reached {
    /// Every count is above zero.
    fn assert_all(&self) {
        let counts = [
            ("deep puts", self.deep_puts),
            ("values that spill", self.overflow_values),
            ("long segments", self.long_segments),
            ("puts of both", self.both_puts),
            ("puts of empty lists", self.empty_lists),
            ("deletes of large subtrees", self.big_deletes),
            ("deletes at the root", self.root_deletes),
            ("locks set", self.lock_sets),
            ("locks removed", self.lock_removes),
            ("a file's locks released", self.file_releases),
            (
                "sessions closed with many locks",
                self.closed_with_many_locks,
            ),
            ("starts that closed sessions", self.starts_that_closed),
            ("stale openings", self.stale_opens),
            ("closings of no open session", self.stale_closes),
            ("starts with no higher count", self.stale_starts),
            ("files added", self.files_added),
            ("files removed", self.files_removed),
            (
                "files removed with locks held",
                self.files_removed_with_locks,
            ),
            ("files renamed", self.files_renamed),
            ("names reused", self.names_reused),
            ("replies", self.replies),
            ("notices", self.notices),
        ];
        for kind in Kind::ALL {
            assert!(
                self.kinds.get(&kind).copied().unwrap_or(0) > 0,
                "no {kind:?} entry"
            );
        }
        for (what, n) in counts {
            assert!(n > 0, "the run reached no {what}: {self:?}");
        }
    }
}

/// Generates valid entries from the model's state.
struct Gen {
    rng: Rng,
    term: u64,
    names: u64,
    removed_names: Vec<Vec<u8>>,
    reached: Reached,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen {
            rng: Rng(seed),
            term: 1,
            names: 0,
            removed_names: Vec::new(),
            reached: Reached::default(),
        }
    }

    /// The next entry after the model's last.
    fn next(&mut self, model: &Model) -> Built {
        let (file, payload) = self.payload(model);
        *self.reached.kinds.entry(payload.kind()).or_default() += 1;
        if payload.kind() == Kind::Empty {
            // A new leader's first entry, in a new term.
            self.term += 1;
        }
        seal(&model.applied, self.term, file, &payload)
    }

    fn payload(&mut self, model: &Model) -> (u64, Payload) {
        let roll = self.rng.below(100);
        if model.files.is_empty() || (model.files.len() < 3 && roll < 8) {
            return self.added(model);
        }
        match roll {
            0..=54 => self.change(model),
            55..=74 => (0, Payload::Session(self.session(model))),
            75..=77 if model.files.len() < 6 => self.added(model),
            78..=79 if model.files.len() > 1 => self.removed(model),
            80..=82 => self.renamed(model),
            83..=87 => {
                let file = if self.rng.percent(50) {
                    0
                } else {
                    *self
                        .rng
                        .pick(&model.files.keys().copied().collect::<Vec<_>>())
                };
                let purpose =
                    *self
                        .rng
                        .pick(&[Purpose::RestorePoint, Purpose::Release, Purpose::Comparison]);
                let n = self.rng.range(1, 40);
                (
                    file,
                    Payload::Mark(Mark {
                        purpose,
                        name: self.rng.bytes(n),
                    }),
                )
            }
            88..=92 => (0, Payload::Empty),
            93..=94 => (0, Payload::Members(Members::FormatVersion(1))),
            _ => self.change(model),
        }
    }

    fn fresh_name(&mut self, model: &Model) -> Vec<u8> {
        if !self.removed_names.is_empty() && self.rng.percent(30) {
            let name = self.rng.pick(&self.removed_names).clone();
            if !model.files.values().any(|f| f.name == name) {
                self.reached.names_reused += 1;
                return name;
            }
        }
        self.names += 1;
        let mut name = format!("file {}", self.names).into_bytes();
        if self.rng.percent(10) {
            // Names are bytes: a zero byte, or one as long as allowed.
            name.push(0);
            if self.rng.percent(30) {
                name.resize(MAX_NAME, b'n');
            }
        }
        name
    }

    fn added(&mut self, model: &Model) -> (u64, Payload) {
        self.reached.files_added += 1;
        let name = self.fresh_name(model);
        let flags = self.rng.below(2) as u32;
        let len = if self.rng.percent(80) {
            MAX_PAGE0
        } else {
            self.rng.below(100)
        };
        let page0 = self.rng.bytes(len);
        (
            model.applied.index + 1,
            Payload::File(FileEvent::Added { name, flags, page0 }),
        )
    }

    fn removed(&mut self, model: &Model) -> (u64, Payload) {
        let ids: Vec<u64> = model.files.keys().copied().collect();
        let id = *self.rng.pick(&ids);
        self.reached.files_removed += 1;
        if model.locks.keys().any(|l| l.file == id) {
            self.reached.files_removed_with_locks += 1;
        }
        self.removed_names.push(model.files[&id].name.clone());
        (id, Payload::File(FileEvent::Removed))
    }

    fn renamed(&mut self, model: &Model) -> (u64, Payload) {
        let ids: Vec<u64> = model.files.keys().copied().collect();
        let id = *self.rng.pick(&ids);
        self.reached.files_renamed += 1;
        self.removed_names.push(model.files[&id].name.clone());
        let name = self.fresh_name(model);
        (id, Payload::File(FileEvent::Renamed { name }))
    }

    fn many_locks(model: &Model, s: &Session) -> bool {
        model.locks.keys().filter(|l| l.session == *s).count() >= 40
    }

    fn session(&mut self, model: &Model) -> SessionEvent {
        let server = *self.rng.pick(&SERVERS);
        let row = model.servers.get(&server).copied();
        let open: Vec<Session> = model.sessions.keys().copied().collect();
        match self.rng.below(100) {
            // A server starts: usually with a higher count.
            0..=11 => {
                let recorded = row.map_or(0, |r| r.0);
                let count = match self.rng.below(10) {
                    0 => recorded,
                    1 => recorded.saturating_sub(1),
                    _ => recorded + 1 + self.rng.below(2) as u64,
                };
                // Format 5.6: a first start is applied whatever its count.
                if row.is_none() || count > recorded {
                    let closing: Vec<&Session> =
                        open.iter().filter(|s| s.server == server).collect();
                    if !closing.is_empty() {
                        self.reached.starts_that_closed += 1;
                    }
                    if closing.iter().any(|s| Self::many_locks(model, s)) {
                        self.reached.closed_with_many_locks += 1;
                    }
                } else {
                    self.reached.stale_starts += 1;
                }
                SessionEvent::Started { server, count }
            }
            // A session opens: usually the next connection of the start.
            12..=64 => {
                let (start, highest) = row.unwrap_or((1, 0));
                let session = if self.rng.percent(80) {
                    Session {
                        server,
                        start,
                        connection: highest + 1 + self.rng.below(3) as u64,
                    }
                } else {
                    self.reached.stale_opens += 1;
                    if self.rng.percent(50) && start > 0 {
                        Session {
                            server,
                            start: start - 1,
                            connection: highest + 5,
                        }
                    } else {
                        Session {
                            server,
                            start,
                            connection: highest.saturating_sub(self.rng.below(2) as u64),
                        }
                    }
                };
                if row.is_none() {
                    self.reached.stale_opens += 1;
                }
                SessionEvent::Opened(session)
            }
            // A session closes: usually an open one.
            _ => {
                if !open.is_empty() && self.rng.percent(85) {
                    let s = *self.rng.pick(&open);
                    if Self::many_locks(model, &s) {
                        self.reached.closed_with_many_locks += 1;
                    }
                    SessionEvent::Closed(s)
                } else {
                    self.reached.stale_closes += 1;
                    SessionEvent::Closed(Session {
                        server,
                        start: 9,
                        connection: 9_999,
                    })
                }
            }
        }
    }

    fn change(&mut self, model: &Model) -> (u64, Payload) {
        let ids: Vec<u64> = model.files.keys().copied().collect();
        let id = *self.rng.pick(&ids);
        let open: Vec<(Session, u64)> = model
            .sessions
            .iter()
            .map(|(s, (seq, _))| (*s, *seq))
            .collect();
        let mut change = Change::default();
        if !open.is_empty() && self.rng.percent(70) {
            let (session, seq) = *self.rng.pick(&open);
            change.op = Some(Op {
                session,
                seq: seq + 1,
            });
            let n = if self.rng.percent(5) {
                self.rng.range(1_000, 3_000)
            } else {
                self.rng.below(60)
            };
            change.reply = self.rng.bytes(n);
            self.reached.replies += 1;
        }
        for _ in 0..self.rng.below(4) {
            let n = self.rng.below(40);
            change.notices.push(self.rng.bytes(n));
            self.reached.notices += 1;
        }
        if !open.is_empty() {
            let sessions: Vec<Session> = open.iter().map(|(s, _)| *s).collect();
            self.locks(model, id, &sessions, &mut change.locks);
        }
        self.edits(&model.files[&id], &mut change.edits);
        (id, Payload::Change(change))
    }

    fn locks(&mut self, model: &Model, file: u64, open: &[Session], out: &mut Vec<LockEdit>) {
        // The locks of the file as the entry leaves them so far.
        let mut held: Vec<(Session, Vec<u8>, Vec<u8>)> = model
            .locks
            .keys()
            .filter(|l| l.file == file)
            .map(|l| (l.session, l.key.clone(), l.user.clone()))
            .collect();
        let many = self.rng.percent(5);
        let n = if many {
            self.rng.range(40, 120)
        } else {
            self.rng.below(5)
        };
        let taker = *self.rng.pick(open);
        for i in 0..n {
            let roll = self.rng.below(100);
            if many || roll < 55 {
                let (session, key) = if many {
                    (
                        taker,
                        format!("record {i} {}", self.rng.next()).into_bytes(),
                    )
                } else {
                    let mut key = vec![self.rng.below(12) as u8, 0, 5];
                    if self.rng.percent(2) {
                        key.resize(MAX_LOCK_KEY, 0);
                    }
                    (*self.rng.pick(open), key)
                };
                let user = vec![b'u', self.rng.below(2) as u8];
                let n = self.rng.range(3, 30);
                let value = self.rng.bytes(n);
                held.retain(|h| !(h.0 == session && h.1 == key && h.2 == user));
                held.push((session, key.clone(), user.clone()));
                self.reached.lock_sets += 1;
                out.push(LockEdit::Set {
                    session,
                    key,
                    user,
                    value,
                });
            } else if roll < 92 && !held.is_empty() {
                let at = self.rng.below(held.len());
                let (session, key, user) = held.swap_remove(at);
                self.reached.lock_removes += 1;
                out.push(LockEdit::Remove { session, key, user });
            } else {
                let session = *self.rng.pick(open);
                held.retain(|h| h.0 != session);
                self.reached.file_releases += 1;
                out.push(LockEdit::ReleaseFile { session });
            }
        }
    }

    fn segment(&mut self) -> Vec<u8> {
        match self.rng.below(100) {
            0..=39 => vec![*self.rng.pick(&[0u8, 1, 2, 0xff, b'a', b'b'])],
            40..=79 => {
                let n = self.rng.range(1, 4);
                self.rng.bytes(n)
            }
            80..=95 => Vec::new(),
            _ => {
                self.reached.long_segments += 1;
                let n = self.rng.range(300, 1_500);
                self.rng.bytes(n)
            }
        }
    }

    fn node(&mut self) -> Node {
        let value = |g: &mut Gen| {
            let n = match g.rng.below(100) {
                0..=3 => {
                    g.reached.overflow_values += 1;
                    g.rng.range(5_000, 20_000)
                }
                4 => {
                    g.reached.overflow_values += 1;
                    MAX_VALUE
                }
                _ => g.rng.below(60),
            };
            g.rng.bytes(n)
        };
        match self.rng.below(100) {
            0..=69 => Node::Value(value(self)),
            70..=89 => {
                self.reached.empty_lists += 1;
                Node::List
            }
            _ => {
                self.reached.both_puts += 1;
                Node::Both(value(self))
            }
        }
    }

    fn edits(&mut self, file: &File, out: &mut Vec<Edit>) {
        let paths: Vec<&Path> = file.nodes.keys().collect();
        let mode = self.rng.below(100);
        if mode < 5 {
            // A large subtree in one entry.
            let at = if !paths.is_empty() && self.rng.percent(25) {
                let p = (*self.rng.pick(&paths)).clone();
                p[..p.len().min(3)].to_vec()
            } else {
                vec![self.segment()]
            };
            out.push(Edit::list(at.clone()));
            for j in 0..self.rng.range(200, 500) as u16 {
                let mut path = at.clone();
                path.push(j.to_be_bytes().to_vec());
                let n = self.rng.below(120);
                out.push(Edit::leaf(path, self.rng.bytes(n)));
            }
        } else if mode < 9 {
            // A deep path, its missing ancestors made by the edit.
            let depth = self.rng.range(40, MAX_DEPTH);
            let path: Path = (0..depth).map(|_| vec![self.rng.below(3) as u8]).collect();
            self.reached.deep_puts += 1;
            let node = self.node();
            out.push(Edit::Put { path, node });
        } else if mode < 10 {
            self.reached.root_deletes += 1;
            out.push(Edit::delete(Vec::new()));
        } else {
            for _ in 0..self.rng.range(1, 8) {
                let edit = if self.rng.percent(78) || paths.is_empty() {
                    let mut path = if !paths.is_empty() && self.rng.percent(80) {
                        (*self.rng.pick(&paths)).clone()
                    } else {
                        Vec::new()
                    };
                    path.push(self.segment());
                    if path.len() >= 48 {
                        self.reached.deep_puts += 1;
                    }
                    let node = self.node();
                    Edit::Put { path, node }
                } else if self.rng.percent(85) {
                    let path = if self.rng.percent(30) {
                        // Near the top, where large subtrees are.
                        let p = (*self.rng.pick(&paths)).clone();
                        p[..1].to_vec()
                    } else {
                        (*self.rng.pick(&paths)).clone()
                    };
                    let under = file.nodes.keys().filter(|p| p.starts_with(&path)).count();
                    if under >= 200 {
                        self.reached.big_deletes += 1;
                    }
                    Edit::delete(path)
                } else {
                    Edit::delete(vec![self.segment(), self.segment()])
                };
                if check_edits(std::slice::from_ref(&edit)).is_ok() {
                    out.push(edit);
                }
            }
        }
    }
}

/// The roots the page store holds for the state's trees.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Roots {
    store: Root,
    files: BTreeMap<u64, Root>,
}

impl Roots {
    /// Every tree with its root, the store's first.
    fn iter(&self) -> impl Iterator<Item = (TreeId, Root)> + '_ {
        std::iter::once((TreeId::Store, self.store))
            .chain(self.files.iter().map(|(id, r)| (TreeId::File(*id), *r)))
    }
}

fn roots_of<S: PageStore + ?Sized>(store: &S) -> Roots {
    let mut roots = Roots {
        store: Root {
            page: 0,
            hash: Hash::default(),
        },
        files: BTreeMap::new(),
    };
    for (name, root) in store.trees() {
        match TreeId::from_name(name.as_bytes()).expect("a tree the state names") {
            TreeId::Store => roots.store = root,
            TreeId::File(id) => {
                roots.files.insert(id, root);
            }
        }
    }
    roots
}

fn walk_all<S: PageStore + ?Sized>(store: &S, root: Root) -> Vec<(Path, Node)> {
    let tree = Tree::open(store, root).unwrap();
    let root_path: &[Vec<u8>] = &[];
    tree.walk(root_path)
        .map(|item| {
            let item = item.unwrap();
            (item.key.path(), item.node)
        })
        .collect()
}

/// Everything the state holds, read from the trees, against the model.
fn compare<S: PageStore + ?Sized>(store: &S, model: &Model, at: &str) {
    let roots = &roots_of(store);
    let state = StoreTree::of(store).unwrap();
    assert_eq!(
        state.applied().unwrap(),
        model.applied,
        "{at}: last applied"
    );
    assert_eq!(state.format_version().unwrap(), model.format, "{at}");
    let files = state.files().unwrap();
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
    assert_eq!(
        roots.files.keys().collect::<Vec<_>>(),
        model.files.keys().collect::<Vec<_>>(),
        "{at}: file trees"
    );
    for (id, file) in &model.files {
        assert_eq!(state.file_named(&file.name).unwrap(), Some(*id), "{at}");
        let got = walk_all(store, roots.files[id]);
        let want: Vec<(Path, Node)> = file
            .nodes
            .iter()
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect();
        if got != want {
            let first = got.iter().zip(&want).position(|(g, w)| g != w);
            panic!(
                "{at}: file {id}: {} nodes in the tree, {} in the model, first difference at {first:?}",
                got.len(),
                file.nodes.len()
            );
        }
    }
    let servers: BTreeMap<ServerId, (u64, u64)> = state
        .servers()
        .unwrap()
        .into_iter()
        .map(|(s, ServerRow { start, connection })| (s, (start, connection)))
        .collect();
    assert_eq!(servers, model.servers, "{at}: servers");
    let sessions: BTreeMap<Session, (u64, Vec<u8>)> = state
        .sessions()
        .unwrap()
        .into_iter()
        .map(|(s, SessionRow { seq, reply })| (s, (seq, reply)))
        .collect();
    assert!(sessions == model.sessions, "{at}: sessions");
    // The whole name index: no name left behind by a rename or a removal.
    let names: BTreeMap<Vec<u8>, u64> = state.names().unwrap().into_iter().collect();
    let want: BTreeMap<Vec<u8>, u64> = model
        .files
        .iter()
        .map(|(id, f)| (f.name.clone(), *id))
        .collect();
    assert!(
        names == want,
        "{at}: {} names, {} files",
        names.len(),
        want.len()
    );
    let mut locks = state.locks().unwrap();
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
    assert!(
        locks == want,
        "{at}: {} locks, {} in the model",
        locks.len(),
        want.len()
    );
}

/// One seed: generated entries applied to the trees and to the model, the
/// whole state compared after every entry, and what each unit of work did
/// checked against what the store saw.
fn run_against_the_model(seed: u64, entries: u64, reached: &mut Reached) {
    let store = MemStore::new(Numbering::Sequential);
    let mut model = Model::new();
    let mut gen = Gen::new(seed);
    for n in 0..entries {
        let built = gen.next(&model);
        let at = format!("seed {seed} entry {} ({n})", built.index);
        store.take_released();
        let before = roots_of(&store);
        let applied = apply(&store, &built.entry()).unwrap_or_else(|e| panic!("{at}: {e}"));
        let outcome = model
            .apply(&built.entry())
            .unwrap_or_else(|e| panic!("{at}: the model refused: {e}"));
        assert_eq!(applied.opened, outcome.opened, "{at}");
        assert_eq!(applied.closed, outcome.closed, "{at}");
        // The trees the store holds now: the file added made, the file
        // removed dropped, its root released by the store.
        let after = roots_of(&store);
        let made: Vec<u64> = after
            .files
            .keys()
            .filter(|id| !before.files.contains_key(id))
            .copied()
            .collect();
        assert_eq!(made, outcome.added.into_iter().collect::<Vec<_>>(), "{at}");
        let released = store.take_released();
        if let Some(id) = outcome.removed {
            let root = before.files[&id];
            assert!(!after.files.contains_key(&id), "{at}");
            assert!(
                released.contains(&(root.page, root.hash, Release::Dropped)),
                "{at}"
            );
        }
        compare(&store, &model, &at);
    }
    let nodes: usize = model.files.values().map(|f| f.nodes.len()).sum();
    eprintln!(
        "seed {seed}: {} files, {nodes} nodes, {} sessions, {} locks, {} pages stored",
        model.files.len(),
        model.sessions.len(),
        model.locks.len(),
        store.len()
    );
    add(reached, gen.reached);
}

fn add(into: &mut Reached, from: Reached) {
    for (k, n) in from.kinds {
        *into.kinds.entry(k).or_default() += n;
    }
    into.deep_puts += from.deep_puts;
    into.overflow_values += from.overflow_values;
    into.long_segments += from.long_segments;
    into.both_puts += from.both_puts;
    into.empty_lists += from.empty_lists;
    into.big_deletes += from.big_deletes;
    into.root_deletes += from.root_deletes;
    into.lock_sets += from.lock_sets;
    into.lock_removes += from.lock_removes;
    into.file_releases += from.file_releases;
    into.closed_with_many_locks += from.closed_with_many_locks;
    into.starts_that_closed += from.starts_that_closed;
    into.stale_opens += from.stale_opens;
    into.stale_closes += from.stale_closes;
    into.stale_starts += from.stale_starts;
    into.files_added += from.files_added;
    into.files_removed += from.files_removed;
    into.files_removed_with_locks += from.files_removed_with_locks;
    into.files_renamed += from.files_renamed;
    into.names_reused += from.names_reused;
    into.replies += from.replies;
    into.notices += from.notices;
}

/// The issue's first check: seeded sequences of entries of every kind,
/// applied to the trees over `MemStore` and to the model, the whole state
/// compared after every entry.
#[test]
fn random_entries_match_the_model() {
    let entries = env("OM_STORE_ENTRIES", 300);
    let mut reached = Reached::default();
    for seed in seeds(2) {
        run_against_the_model(seed, entries, &mut reached);
    }
    eprintln!("{reached:?}");
    reached.assert_all();
}

/// The same, longer: 16 seeds of 1,500 entries unless `OM_STORE_SEEDS` and
/// `OM_STORE_ENTRIES` say otherwise.
#[test]
#[ignore = "long: 16 seeds of 1,500 entries by default (OM_STORE_SEEDS, OM_STORE_ENTRIES); run with --release"]
fn long_random_entries_match_the_model() {
    let entries = env("OM_STORE_ENTRIES", 1_500);
    let mut reached = Reached::default();
    for seed in seeds(16) {
        run_against_the_model(seed, entries, &mut reached);
    }
    eprintln!("{reached:?}");
    reached.assert_all();
}

/// The issue's second check: the same sequence over page stores that number
/// pages differently gives the same root hash for every tree after every
/// entry.
#[test]
fn root_hashes_do_not_depend_on_page_numbers() {
    let entries = env("OM_STORE_ENTRIES", 300);
    for seed in seeds(2) {
        let stores = [
            MemStore::new(Numbering::Sequential),
            MemStore::new(Numbering::Scattered(seed.wrapping_mul(7919))),
            MemStore::trusting(Numbering::Scattered(seed)),
        ];
        let mut model = Model::new();
        let mut gen = Gen::new(seed);
        for _ in 0..entries {
            let built = gen.next(&model);
            model.apply(&built.entry()).unwrap();
            for store in &stores {
                apply(store, &built.entry()).unwrap();
            }
            let roots: Vec<Roots> = stores.iter().map(roots_of).collect();
            let hashes: Vec<Vec<(TreeId, Hash)>> = roots
                .iter()
                .map(|r| r.iter().map(|(t, root)| (t, root.hash)).collect())
                .collect();
            assert_eq!(hashes[0], hashes[1], "seed {seed} entry {}", built.index);
            assert_eq!(hashes[0], hashes[2], "seed {seed} entry {}", built.index);
            assert_ne!(roots[0].store.page, roots[1].store.page);
        }
    }
}

/// A small state with a file, a session holding locks and a second session,
/// and its entries.
struct Fixture {
    store: MemStore,
    model: Model,
    file: u64,
    alice: Session,
    bob: Session,
    term: u64,
}

impl Fixture {
    fn new() -> Fixture {
        let mut f = Fixture {
            store: MemStore::new(Numbering::Sequential),
            model: Model::new(),
            file: 0,
            alice: Session {
                server: SERVERS[0],
                start: 1,
                connection: 1,
            },
            bob: Session {
                server: SERVERS[0],
                start: 1,
                connection: 2,
            },
            term: 2,
        };
        f.push(
            1,
            Payload::File(FileEvent::Added {
                name: b"Invoices".to_vec(),
                flags: 0,
                page0: vec![7; MAX_PAGE0],
            }),
        );
        f.file = 1;
        f.push(
            0,
            Payload::Session(SessionEvent::Started {
                server: SERVERS[0],
                count: 1,
            }),
        );
        f.push(0, Payload::Session(SessionEvent::Opened(f.alice)));
        f.push(0, Payload::Session(SessionEvent::Opened(f.bob)));
        let mut edits = vec![Edit::list(vec![b"T".to_vec(), vec![5]])];
        for r in 0..300u16 {
            edits.push(Edit::leaf(
                vec![b"T".to_vec(), vec![5], r.to_be_bytes().to_vec()],
                vec![r as u8; 40],
            ));
        }
        let change = Change {
            op: Some(Op {
                session: f.alice,
                seq: 1,
            }),
            reply: b"ok".to_vec(),
            notices: vec![b"changed".to_vec()],
            locks: vec![
                LockEdit::Set {
                    session: f.alice,
                    key: b"rec 1".to_vec(),
                    user: b"t0".to_vec(),
                    value: b"9 1 Alice".to_vec(),
                },
                LockEdit::Set {
                    session: f.alice,
                    key: b"rec 2".to_vec(),
                    user: b"t0".to_vec(),
                    value: b"9 1 Alice".to_vec(),
                },
            ],
            edits,
        };
        f.push(1, Payload::Change(change));
        f
    }

    fn seal(&self, file: u64, payload: &Payload) -> Built {
        seal(&self.model.applied, self.term, file, payload)
    }

    fn push(&mut self, file: u64, payload: Payload) -> Applied {
        let built = self.seal(file, &payload);
        self.push_built(&built)
    }

    fn push_built(&mut self, built: &Built) -> Applied {
        let applied = apply(&self.store, &built.entry()).unwrap();
        self.model.apply(&built.entry()).unwrap();
        applied
    }

    /// What the store holds, to show that a refusal left it as it was.
    fn footprint(&self) -> (Vec<u64>, Vec<u64>, usize, usize) {
        (
            self.store.numbers(),
            self.store.taken(),
            self.store.written().len(),
            self.store.released().len(),
        )
    }

    /// `built` is refused with `why` by `apply` and by the model, and
    /// neither the store nor the model changes.
    fn refused(&mut self, built: &Built, why: Refusal) {
        let before = self.footprint();
        let model = self.model.clone();
        let got = apply(&self.store, &built.entry());
        assert_eq!(got, Err(Error::Refused(why.clone())), "{built:?}");
        assert_eq!(self.footprint(), before, "a refusal wrote to the store");
        assert_eq!(self.model.apply(&built.entry()), Err(why), "the model");
        assert!(self.model == model, "a refusal changed the model");
        compare(&self.store, &self.model, "after a refusal");
    }
}

fn change_of(session: Option<(Session, u64)>, locks: Vec<LockEdit>, edits: Vec<Edit>) -> Payload {
    Payload::Change(Change {
        op: session.map(|(session, seq)| Op { session, seq }),
        reply: Vec::new(),
        notices: Vec::new(),
        locks,
        edits,
    })
}

/// An entry repeated, skipped or out of order, with a broken chain or a term
/// that goes back, and every entry that cannot be right: each refused by
/// `apply` and the model alike, with nothing changed.
#[test]
fn entries_that_cannot_be_right_are_refused() {
    let mut f = Fixture::new();
    let last = f.model.applied;
    let next = f.seal(0, &Payload::Empty);

    // Order and chain.
    let mut repeated = next.clone();
    repeated.index = last.index;
    repeated.hash = entry_hash(&last.hash, last.index, f.term, repeated.kind, 0, &[]);
    f.refused(
        &repeated,
        Refusal::NotNext {
            last: last.index,
            index: last.index,
        },
    );
    let skipped = Built::raw(
        &last,
        last.index + 2,
        f.term,
        Kind::Empty.code(),
        0,
        Vec::new(),
    );
    f.refused(
        &skipped,
        Refusal::NotNext {
            last: last.index,
            index: last.index + 2,
        },
    );
    let old = Built::raw(&last, 2, f.term, Kind::Empty.code(), 0, Vec::new());
    f.refused(
        &old,
        Refusal::NotNext {
            last: last.index,
            index: 2,
        },
    );
    let mut broken = next.clone();
    broken.hash.0[0] ^= 1;
    f.refused(
        &broken,
        Refusal::Chain {
            expected: next.hash,
            found: broken.hash,
        },
    );
    // The right fields under another chain's hash.
    let elsewhere = Built::raw(
        &Position {
            hash: Hash([9; 32]),
            ..last
        },
        last.index + 1,
        f.term,
        Kind::Empty.code(),
        0,
        Vec::new(),
    );
    f.refused(
        &elsewhere,
        Refusal::Chain {
            expected: next.hash,
            found: elsewhere.hash,
        },
    );
    let behind = Built::raw(
        &last,
        last.index + 1,
        f.term - 1,
        Kind::Empty.code(),
        0,
        Vec::new(),
    );
    f.refused(
        &behind,
        Refusal::TermBehind {
            last: f.term,
            term: f.term - 1,
        },
    );

    // Kinds and payloads.
    let unknown = Built::raw(&last, last.index + 1, f.term, 99, 0, Vec::new());
    f.refused(&unknown, Refusal::Payload(PayloadError::UnknownKind(99)));
    let mut bytes = Payload::Session(SessionEvent::Closed(f.alice))
        .encode()
        .unwrap();
    bytes.pop();
    let short = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Session.code(),
        0,
        bytes,
    );
    f.refused(&short, Refusal::Payload(PayloadError::Short));
    let trailing = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Empty.code(),
        0,
        vec![0],
    );
    f.refused(&trailing, Refusal::Payload(PayloadError::Trailing(1)));
    let mut group = Payload::Change(Change::default()).encode().unwrap();
    group[1] = 1;
    let group = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Change.code(),
        f.file,
        group,
    );
    f.refused(
        &group,
        Refusal::Payload(PayloadError::Unsupported(Feature::Group)),
    );
    let clone = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::File.code(),
        last.index + 1,
        vec![1, 4],
    );
    f.refused(
        &clone,
        Refusal::Payload(PayloadError::Unsupported(Feature::Clone)),
    );
    let members = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Members.code(),
        0,
        vec![1, 2],
    );
    f.refused(
        &members,
        Refusal::Payload(PayloadError::Unsupported(Feature::MembersList)),
    );
    let version = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Mark.code(),
        0,
        vec![2, 1, 1, 0, b'x'],
    );
    f.refused(&version, Refusal::Payload(PayloadError::Version(2)));

    // The file field.
    let none = f.seal(0, &change_of(None, Vec::new(), Vec::new()));
    f.refused(
        &none,
        Refusal::FileField {
            kind: Kind::Change,
            file: 0,
        },
    );
    let missing = f.seal(77, &change_of(None, Vec::new(), Vec::new()));
    f.refused(&missing, Refusal::NoSuchFile(77));
    let session = f.seal(f.file, &Payload::Session(SessionEvent::Closed(f.bob)));
    f.refused(
        &session,
        Refusal::FileField {
            kind: Kind::Session,
            file: f.file,
        },
    );
    let empty = f.seal(f.file, &Payload::Empty);
    f.refused(
        &empty,
        Refusal::FileField {
            kind: Kind::Empty,
            file: f.file,
        },
    );
    let members = f.seal(f.file, &Payload::Members(Members::FormatVersion(1)));
    f.refused(
        &members,
        Refusal::FileField {
            kind: Kind::Members,
            file: f.file,
        },
    );
    let mark = f.seal(
        77,
        &Payload::Mark(Mark {
            purpose: Purpose::Release,
            name: b"v1".to_vec(),
        }),
    );
    f.refused(&mark, Refusal::NoSuchFile(77));
    let added = Payload::File(FileEvent::Added {
        name: b"Other".to_vec(),
        flags: 0,
        page0: Vec::new(),
    });
    let wrong_id = f.seal(f.file, &added);
    f.refused(
        &wrong_id,
        Refusal::FileField {
            kind: Kind::File,
            file: f.file,
        },
    );
    let taken = f.seal(
        last.index + 1,
        &Payload::File(FileEvent::Added {
            name: b"Invoices".to_vec(),
            flags: 0,
            page0: Vec::new(),
        }),
    );
    f.refused(&taken, Refusal::NameTaken(b"Invoices".to_vec()));
    let renamed = f.seal(
        f.file,
        &Payload::File(FileEvent::Renamed {
            name: b"Invoices".to_vec(),
        }),
    );
    f.refused(&renamed, Refusal::NameTaken(b"Invoices".to_vec()));
    let gone = f.seal(5, &Payload::File(FileEvent::Removed));
    f.refused(&gone, Refusal::NoSuchFile(5));
    let no_file = f.seal(0, &Payload::File(FileEvent::Removed));
    f.refused(
        &no_file,
        Refusal::FileField {
            kind: Kind::File,
            file: 0,
        },
    );

    // Sessions and sequence numbers.
    let stranger = Session {
        server: SERVERS[1],
        start: 1,
        connection: 1,
    };
    let closed = f.seal(
        f.file,
        &change_of(Some((stranger, 1)), Vec::new(), Vec::new()),
    );
    f.refused(&closed, Refusal::SessionNotOpen(stranger));
    for seq in [0, 1, 3] {
        let wrong = f.seal(
            f.file,
            &change_of(Some((f.alice, seq)), Vec::new(), Vec::new()),
        );
        f.refused(
            &wrong,
            Refusal::Sequence {
                session: f.alice,
                last: 1,
                seq,
            },
        );
    }
    let lock = f.seal(
        f.file,
        &change_of(
            None,
            vec![LockEdit::ReleaseFile { session: stranger }],
            Vec::new(),
        ),
    );
    f.refused(&lock, Refusal::SessionNotOpen(stranger));

    // Locks: a remove of a lock not held, also after this entry released
    // or removed it.
    let remove = |session: Session, key: &[u8]| LockEdit::Remove {
        session,
        key: key.to_vec(),
        user: b"t0".to_vec(),
    };
    for (locks, edit) in [
        (vec![remove(f.bob, b"rec 1")], 0),
        (vec![remove(f.alice, b"rec 3")], 0),
        (
            vec![
                LockEdit::ReleaseFile { session: f.alice },
                remove(f.alice, b"rec 1"),
            ],
            1,
        ),
        (
            vec![remove(f.alice, b"rec 1"), remove(f.alice, b"rec 1")],
            1,
        ),
    ] {
        let wrong = f.seal(f.file, &change_of(None, locks, Vec::new()));
        f.refused(&wrong, Refusal::NoSuchLock { edit });
    }
    // In another file, alice holds nothing.
    let other = f.model.applied.index + 1;
    f.push(
        other,
        Payload::File(FileEvent::Added {
            name: b"Other".to_vec(),
            flags: 0,
            page0: Vec::new(),
        }),
    );
    assert!(
        roots_of(&f.store).files.contains_key(&other),
        "no file made"
    );
    let elsewhere = f.seal(
        other,
        &change_of(None, vec![remove(f.alice, b"rec 1")], Vec::new()),
    );
    f.refused(&elsewhere, Refusal::NoSuchLock { edit: 0 });

    // Tree format versions.
    let higher = f.seal(0, &Payload::Members(Members::FormatVersion(2)));
    f.refused(&higher, Refusal::UnknownFormatVersion(2));
    // Only version 1 is known, so no lower version can be named yet: 0 is
    // unknown.
    let zero = f.seal(0, &Payload::Members(Members::FormatVersion(0)));
    f.refused(&zero, Refusal::UnknownFormatVersion(0));

    // An edit beyond the tree's bounds.
    let mut deep = Payload::Change(Change {
        edits: vec![Edit::leaf(vec![vec![1]; MAX_DEPTH], vec![1])],
        ..Change::default()
    })
    .encode()
    .unwrap();
    let at = deep.len() - (MAX_DEPTH * 3 + 4 + 1 + 1);
    assert_eq!(deep[at], MAX_DEPTH as u8);
    deep[at] = MAX_DEPTH as u8 + 1;
    deep.splice(at + 1..at + 1, [1, 0, 1]);
    let last = f.model.applied;
    let deep = Built::raw(
        &last,
        last.index + 1,
        f.term,
        Kind::Change.code(),
        f.file,
        deep,
    );
    f.refused(
        &deep,
        Refusal::Payload(PayloadError::Edit {
            edit: 0,
            why: tree::Refusal::TooDeep(MAX_DEPTH + 1),
        }),
    );

    // After all of it, the next entry is still taken.
    let next = f.seal(0, &Payload::Empty);
    f.push_built(&next);
    compare(&f.store, &f.model, "after the refusals");
}

/// A state whose tree format version this build does not know, a later
/// one or 0 (review of #732), is applied no further, by `apply` or the
/// model.
#[test]
fn a_state_of_an_unknown_tree_format_version_takes_no_entry() {
    for version in [0, 2] {
        let mut f = Fixture::new();
        let next = f.seal(0, &Payload::Empty);
        // Write the version as another build might have; the entry index is
        // the tree's own, not the log's.
        let index = f.model.applied.index;
        let name = TreeId::Store.name();
        let mut unit = f.store.begin().unwrap();
        let root = tree::apply(
            &f.store,
            &mut unit,
            f.store.root(&name),
            index + 1,
            &[Edit::leaf(vec![vec![0x02]], vec![version])],
        )
        .unwrap();
        unit.set_root(&name, root);
        unit.commit().unwrap();
        let before = f.footprint();
        let why = Refusal::UnknownFormatVersion(version);
        assert_eq!(
            apply(&f.store, &next.entry()),
            Err(Error::Refused(why.clone()))
        );
        assert_eq!(f.footprint(), before);
        f.model.format = version;
        assert_eq!(f.model.apply(&next.entry()), Err(why));
    }
}

/// The issue's last check: an entry whose unit of work is dropped leaves
/// nothing. Each call the store gets while applying an entry that changes
/// three trees fails in turn; every failure leaves the store as it was, and
/// the entry then applies as on an undisturbed store.
#[test]
fn a_dropped_unit_of_work_leaves_nothing() {
    // Entries that each write to more than one tree.
    let entries = |f: &Fixture| -> Vec<(u64, Payload)> {
        vec![
            (
                f.file,
                Payload::Change(Change {
                    op: Some(Op {
                        session: f.alice,
                        seq: 2,
                    }),
                    reply: b"done".to_vec(),
                    notices: Vec::new(),
                    locks: vec![LockEdit::Remove {
                        session: f.alice,
                        key: b"rec 1".to_vec(),
                        user: b"t0".to_vec(),
                    }],
                    edits: vec![
                        Edit::delete(vec![b"T".to_vec()]),
                        Edit::leaf(vec![b"U".to_vec(), vec![1]], vec![3; 9_000]),
                    ],
                }),
            ),
            (f.file, Payload::File(FileEvent::Removed)),
        ]
    };
    let reference = {
        let mut f = Fixture::new();
        for (file, payload) in entries(&f) {
            f.push(file, payload);
        }
        roots_of(&f.store)
    };
    let mut f = Fixture::new();
    for (file, payload) in entries(&f) {
        let built = f.seal(file, &payload);
        let mut failures = 0;
        let applied = loop {
            let before = f.footprint();
            f.store.fail_call(Some(failures));
            let got = apply(&f.store, &built.entry());
            f.store.fail_call(None);
            match got {
                Ok(applied) => break applied,
                Err(Error::Tree(_)) => {
                    assert_eq!(f.footprint(), before, "failure at call {failures}");
                    compare(&f.store, &f.model, "after a failure");
                    failures += 1;
                }
                Err(e) => panic!("{e}"),
            }
        };
        eprintln!("{failures} calls failed in turn");
        assert!(failures > 10, "only {failures} calls");
        f.model.apply(&built.entry()).unwrap();
        let _ = applied;
        compare(&f.store, &f.model, "after the entry");
    }
    let hashes = |r: &Roots| r.iter().map(|(t, root)| (t, root.hash)).collect::<Vec<_>>();
    assert_eq!(hashes(&roots_of(&f.store)), hashes(&reference));
}

/// A session's close and a server's start release the session's locks, in
/// every file, and a file's removal releases every session's locks on it.
#[test]
fn closing_releases_locks() {
    let mut f = Fixture::new();
    let other = f.model.applied.index + 1;
    f.push(
        other,
        Payload::File(FileEvent::Added {
            name: b"Other".to_vec(),
            flags: 1,
            page0: Vec::new(),
        }),
    );
    let set = |session: Session, key: &[u8]| LockEdit::Set {
        session,
        key: key.to_vec(),
        user: b"t0".to_vec(),
        value: b"9 1".to_vec(),
    };
    f.push(
        other,
        change_of(None, vec![set(f.alice, b"a"), set(f.bob, b"b")], Vec::new()),
    );
    f.push(f.file, change_of(None, vec![set(f.bob, b"c")], Vec::new()));
    let state = |f: &Fixture| StoreTree::of(&f.store).unwrap().locks().unwrap();
    assert_eq!(state(&f).len(), 5);
    // A logout from one file.
    f.push(
        other,
        change_of(
            None,
            vec![LockEdit::ReleaseFile { session: f.bob }],
            Vec::new(),
        ),
    );
    assert_eq!(state(&f).len(), 4);
    // Alice's close takes her locks in both files.
    let closed = f.push(0, Payload::Session(SessionEvent::Closed(f.alice)));
    assert_eq!(closed.closed, vec![f.alice]);
    assert!(state(&f).iter().all(|l| l.session == f.bob));
    // The first file's removal takes bob's lock on it.
    f.push(f.file, Payload::File(FileEvent::Removed));
    assert!(state(&f).is_empty());
    // A second start of the server closes bob.
    f.push(other, change_of(None, vec![set(f.bob, b"d")], Vec::new()));
    let started = f.push(
        0,
        Payload::Session(SessionEvent::Started {
            server: SERVERS[0],
            count: 2,
        }),
    );
    assert_eq!(started.closed, vec![f.bob]);
    assert!(state(&f).is_empty());
    assert!(StoreTree::of(&f.store)
        .unwrap()
        .sessions()
        .unwrap()
        .is_empty());
    // A delayed opening of the earlier start changes nothing.
    let late = Session {
        connection: 7,
        ..f.bob
    };
    assert_eq!(
        f.push(0, Payload::Session(SessionEvent::Opened(late)))
            .opened,
        None
    );
    // Review of #732: a server's first start is applied whatever its count,
    // 0 included, and its first session then opens.
    let first = Session {
        server: SERVERS[2],
        start: 0,
        connection: 1,
    };
    f.push(
        0,
        Payload::Session(SessionEvent::Started {
            server: SERVERS[2],
            count: 0,
        }),
    );
    assert_eq!(
        f.push(0, Payload::Session(SessionEvent::Opened(first)))
            .opened,
        Some(first)
    );
    // A second start with the same count changes nothing.
    let again = f.push(
        0,
        Payload::Session(SessionEvent::Started {
            server: SERVERS[2],
            count: 0,
        }),
    );
    assert!(again.closed.is_empty());
    compare(&f.store, &f.model, "at the end");
}

fn round_trip(kind: Kind, payload: &Payload) {
    let bytes = payload.encode().unwrap();
    assert_eq!(Payload::decode(kind.code(), &bytes).as_ref(), Ok(payload));
}

/// Every payload kind round-trips, at its bounds too.
#[test]
fn payloads_round_trip() {
    let s = Session {
        server: [0; 16],
        start: u64::MAX,
        connection: 1,
    };
    let change = Change {
        op: Some(Op { session: s, seq: 3 }),
        reply: vec![1; MAX_REPLY],
        notices: vec![Vec::new(), vec![0, 1, 2]],
        locks: vec![
            LockEdit::Set {
                session: s,
                key: vec![0; MAX_LOCK_KEY],
                user: Vec::new(),
                value: vec![2; MAX_VALUE],
            },
            LockEdit::Remove {
                session: s,
                key: Vec::new(),
                user: vec![1],
            },
            LockEdit::ReleaseFile { session: s },
        ],
        edits: vec![
            Edit::list(vec![vec![]]),
            Edit::leaf(vec![vec![0]; MAX_DEPTH], vec![9; MAX_VALUE]),
            Edit::Put {
                path: vec![b"a".to_vec(), vec![0xff; 30_000]],
                node: Node::Both(Vec::new()),
            },
            Edit::delete(Vec::new()),
            Edit::delete(vec![b"a".to_vec()]),
        ],
    };
    for payload in [
        Payload::Change(change),
        Payload::Change(Change::default()),
        Payload::Session(SessionEvent::Opened(s)),
        Payload::Session(SessionEvent::Closed(s)),
        Payload::Session(SessionEvent::Started {
            server: [3; 16],
            count: 0,
        }),
        Payload::File(FileEvent::Added {
            name: vec![0; MAX_NAME],
            flags: 1,
            page0: vec![5; MAX_PAGE0],
        }),
        Payload::File(FileEvent::Removed),
        Payload::File(FileEvent::Renamed { name: vec![b'x'] }),
        Payload::Members(Members::FormatVersion(1)),
        Payload::Mark(Mark {
            purpose: Purpose::Comparison,
            name: b"weekly".to_vec(),
        }),
        Payload::Empty,
    ] {
        round_trip(payload.kind(), &payload);
    }
}

/// Malformed payloads of every kind are refused, each with its reason, and
/// encoding refuses what decoding would.
#[test]
fn malformed_payloads_are_refused() {
    let session = [9u8; 32];
    let cases: Vec<(Kind, Vec<u8>, PayloadError)> = vec![
        (Kind::Change, vec![], PayloadError::Short),
        (Kind::Change, vec![2], PayloadError::Version(2)),
        (Kind::Change, vec![0], PayloadError::Version(0)),
        (
            Kind::Change,
            [vec![1], vec![0; 8], vec![2]].concat(),
            PayloadError::Tag {
                field: "op",
                tag: 2,
            },
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                1u32.to_le_bytes().to_vec(),
                vec![7],
                vec![0; 12],
            ]
            .concat(),
            PayloadError::ReplyWithoutOp,
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 4],
                u32::MAX.to_le_bytes().to_vec(),
            ]
            .concat(),
            PayloadError::TooMany {
                field: "notices",
                count: u32::MAX as usize,
                max: 1 << 16,
            },
        ),
        (
            // A count the bytes cannot hold.
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 4],
                1_000u32.to_le_bytes().to_vec(),
                vec![0; 4],
            ]
            .concat(),
            PayloadError::Short,
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 8],
                1u32.to_le_bytes().to_vec(),
                vec![4],
                session.to_vec(),
            ]
            .concat(),
            PayloadError::Tag {
                field: "lock edit",
                tag: 4,
            },
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 8],
                1u32.to_le_bytes().to_vec(),
                vec![2],
                session.to_vec(),
                ((MAX_LOCK_KEY + 1) as u16).to_le_bytes().to_vec(),
            ]
            .concat(),
            PayloadError::TooLong {
                field: "lock key",
                len: MAX_LOCK_KEY + 1,
                max: MAX_LOCK_KEY,
            },
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 12],
                1u32.to_le_bytes().to_vec(),
                vec![5],
            ]
            .concat(),
            PayloadError::Tag {
                field: "edit",
                tag: 5,
            },
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 12],
                1u32.to_le_bytes().to_vec(),
                vec![1, 0],
            ]
            .concat(),
            PayloadError::Edit {
                edit: 0,
                why: tree::Refusal::RootPut,
            },
        ),
        (
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 12],
                1u32.to_le_bytes().to_vec(),
                vec![2, 1, 1, 0, b'a'],
                ((MAX_VALUE + 1) as u32).to_le_bytes().to_vec(),
            ]
            .concat(),
            PayloadError::Edit {
                edit: 0,
                why: tree::Refusal::ValueTooLong(MAX_VALUE + 1),
            },
        ),
        (
            // Two segments of 65,535 bytes: a path longer than 64 KiB.
            Kind::Change,
            [
                vec![1],
                vec![0; 8],
                vec![0],
                vec![0; 12],
                1u32.to_le_bytes().to_vec(),
                vec![4, 2],
                u16::MAX.to_le_bytes().to_vec(),
                vec![1; 65_535],
                u16::MAX.to_le_bytes().to_vec(),
                vec![1; 65_535],
            ]
            .concat(),
            PayloadError::Edit {
                edit: 0,
                why: tree::Refusal::KeyTooLong(2 * 65_537),
            },
        ),
        (
            Kind::Session,
            vec![1, 4],
            PayloadError::Tag {
                field: "session event",
                tag: 4,
            },
        ),
        (
            Kind::Session,
            [vec![1, 1], vec![0; 31]].concat(),
            PayloadError::Short,
        ),
        (
            Kind::Session,
            [vec![1, 3], vec![0; 25]].concat(),
            PayloadError::Trailing(1),
        ),
        (
            Kind::File,
            vec![1, 0],
            PayloadError::Tag {
                field: "file event",
                tag: 0,
            },
        ),
        (Kind::File, vec![1, 3, 0, 0], PayloadError::EmptyName),
        (
            Kind::File,
            [vec![1, 3], ((MAX_NAME + 1) as u16).to_le_bytes().to_vec()].concat(),
            PayloadError::TooLong {
                field: "name",
                len: MAX_NAME + 1,
                max: MAX_NAME,
            },
        ),
        (
            Kind::File,
            vec![1, 1, 1, 0, b'a', 2, 0, 0, 0, 0, 0],
            PayloadError::Flags(2),
        ),
        (
            Kind::File,
            [
                vec![1, 1, 1, 0, b'a', 0, 0, 0, 0],
                ((MAX_PAGE0 + 1) as u16).to_le_bytes().to_vec(),
            ]
            .concat(),
            PayloadError::TooLong {
                field: "page 0",
                len: MAX_PAGE0 + 1,
                max: MAX_PAGE0,
            },
        ),
        (Kind::File, vec![1, 2, 0], PayloadError::Trailing(1)),
        (
            Kind::Members,
            vec![1, 3],
            PayloadError::Tag {
                field: "members event",
                tag: 3,
            },
        ),
        (Kind::Members, vec![1, 1], PayloadError::Short),
        (
            Kind::Mark,
            vec![1, 0, 1, 0, b'a'],
            PayloadError::Tag {
                field: "mark purpose",
                tag: 0,
            },
        ),
        (Kind::Mark, vec![1, 1, 0, 0], PayloadError::EmptyName),
        (Kind::Empty, vec![1], PayloadError::Trailing(1)),
    ];
    for (kind, bytes, why) in cases {
        assert_eq!(
            Payload::decode(kind.code(), &bytes),
            Err(why.clone()),
            "{kind:?} {:?}",
            &bytes[..bytes.len().min(40)]
        );
    }
    assert_eq!(Payload::decode(0, &[]), Err(PayloadError::UnknownKind(0)));
    assert_eq!(Payload::decode(7, &[]), Err(PayloadError::UnknownKind(7)));
    let huge = vec![0u8; om_store::entry::MAX_PAYLOAD + 1];
    assert!(matches!(
        Payload::decode(Kind::Change.code(), &huge),
        Err(PayloadError::TooLong {
            field: "payload",
            ..
        })
    ));

    // Encoding holds to the same bounds.
    let refused = [
        Payload::Change(Change {
            reply: vec![1],
            ..Change::default()
        }),
        Payload::Change(Change {
            edits: vec![Edit::list(Vec::new())],
            ..Change::default()
        }),
        Payload::Change(Change {
            edits: vec![Edit::leaf(vec![vec![1]; MAX_DEPTH + 1], Vec::new())],
            ..Change::default()
        }),
        Payload::File(FileEvent::Renamed { name: Vec::new() }),
        Payload::File(FileEvent::Added {
            name: b"a".to_vec(),
            flags: 4,
            page0: Vec::new(),
        }),
        Payload::File(FileEvent::Added {
            name: b"a".to_vec(),
            flags: 0,
            page0: vec![0; MAX_PAGE0 + 1],
        }),
    ];
    for payload in refused {
        assert!(payload.encode().is_err(), "{payload:?}");
    }
    // Review of #732: notices each within their bound, past the payload's
    // together, are refused before anything is copied.
    let notices = vec![vec![0u8; om_store::entry::MAX_NOTICE]; 1025];
    let big = Payload::Change(Change {
        notices,
        ..Change::default()
    });
    assert!(matches!(
        big.encode(),
        Err(PayloadError::TooLong {
            field: "payload",
            ..
        })
    ));
}

/// Random bytes, and valid payloads cut, extended and flipped, never make
/// decoding panic; whatever decodes encodes to the same bytes.
#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x5eed);
    let mut gen = Gen::new(77);
    let mut model = Model::new();
    let mut valid = Vec::new();
    for _ in 0..60 {
        let built = gen.next(&model);
        model.apply(&built.entry()).unwrap();
        valid.push((built.kind, built.payload));
    }
    let mut decoded = 0;
    for i in 0..20_000 {
        let (kind, bytes) = if i % 2 == 0 {
            let kind = rng.range(0, 7) as u16;
            let n = rng.below(120);
            let mut bytes = rng.bytes(n);
            if rng.percent(80) && !bytes.is_empty() {
                bytes[0] = 1;
            }
            (kind, bytes)
        } else {
            let (kind, bytes) = rng.pick(&valid).clone();
            let mut bytes = bytes;
            match rng.below(4) {
                0 => {
                    let n = rng.below(bytes.len() + 1);
                    bytes.truncate(n);
                }
                1 => {
                    let n = rng.range(1, 8);
                    bytes.extend(rng.bytes(n));
                }
                _ => {
                    for _ in 0..rng.range(1, 3) {
                        if !bytes.is_empty() {
                            let at = rng.below(bytes.len());
                            bytes[at] ^= 1 << rng.below(8);
                        }
                    }
                }
            }
            (kind, bytes)
        };
        if let Ok(payload) = Payload::decode(kind, &bytes) {
            decoded += 1;
            assert_eq!(payload.encode().unwrap(), bytes);
        }
    }
    assert!(decoded > 100, "{decoded}");
}

/// Edits leave exactly the tree the file engine's in-memory tree
/// (`om_hbam::OwnedNode`) holds after the same change, node kinds and empty
/// lists included: the model's rules against `OwnedNode::put` and
/// `OwnedNode::remove`, on random edits over a small alphabet so that edits
/// meet often. (The trees match the model in the tests above.)
#[test]
fn edits_leave_what_owned_node_holds() {
    fn flatten(node: &OwnedNode, path: &mut Path, out: &mut Vec<(Path, Node)>) {
        for child in &node.children {
            path.push(child.segment.clone());
            let kind = if child.push || !child.children.is_empty() {
                match &child.value {
                    None => Node::List,
                    Some(v) => Node::Both(v.clone()),
                }
            } else {
                Node::Value(child.value.clone().unwrap_or_default())
            };
            out.push((path.clone(), kind));
            flatten(child, path, out);
            path.pop();
        }
    }
    // Review of #732: a put under a leaf whose value is empty, which the
    // file engine's `holds_value` lets through, leaves an empty list: the
    // empty value goes, as the file writer writes a list's empty value as
    // nothing (`OwnedNode::ensure`).
    {
        let mut nodes = BTreeMap::new();
        let mut owned = OwnedNode::list(Vec::new(), Vec::new());
        owned.put(&[b"a"], OwnedNode::leaf(Vec::new(), Vec::new()));
        model::put(&mut nodes, &[b"a".to_vec()], Node::Value(Vec::new()));
        assert!(!om_hbam::writer::holds_value(owned.child(b"a").unwrap()));
        owned.put(&[b"a", b"b"], OwnedNode::leaf(Vec::new(), vec![1]));
        model::put(
            &mut nodes,
            &[b"a".to_vec(), b"b".to_vec()],
            Node::Value(vec![1]),
        );
        let mut got = Vec::new();
        flatten(&owned, &mut Vec::new(), &mut got);
        assert_eq!(got[0], (vec![b"a".to_vec()], Node::List));
        let want: Vec<(Path, Node)> = nodes.into_iter().collect();
        assert_eq!(got, want);
    }
    // Review of #758: through the page tree, a put below each kind of
    // ancestor leaves what `OwnedNode` leaves. A list that holds an
    // explicit empty value keeps it and its other children; a leaf with an
    // empty value becomes a list; a leaf with a value becomes both.
    {
        let store = MemStore::new(Numbering::Sequential);
        let name = TreeName::new(b"t".to_vec()).unwrap();
        let both_empty = OwnedNode {
            segment: Vec::new(),
            value: Some(Vec::new()),
            push: true,
            children: Vec::new(),
        };
        let leaf = |v: &[u8]| OwnedNode::leaf(Vec::new(), v.to_vec());
        let steps: Vec<(Vec<&[u8]>, OwnedNode, Node)> = vec![
            (vec![b"a"], both_empty, Node::Both(Vec::new())),
            (vec![b"a", b"x"], leaf(&[1]), Node::Value(vec![1])),
            (vec![b"a", b"y"], leaf(&[2]), Node::Value(vec![2])),
            (vec![b"b"], leaf(&[]), Node::Value(Vec::new())),
            (vec![b"b", b"c"], leaf(&[3]), Node::Value(vec![3])),
            (vec![b"d"], leaf(&[4]), Node::Value(vec![4])),
            (vec![b"d", b"e"], leaf(&[5]), Node::Value(vec![5])),
        ];
        let mut owned = OwnedNode::list(Vec::new(), Vec::new());
        let mut root = None;
        for (entry, (path, node, kind)) in (1..).zip(steps) {
            owned.put(&path, node);
            let edit = Edit::Put {
                path: path.iter().map(|s| s.to_vec()).collect(),
                node: kind,
            };
            let mut unit = store.begin().unwrap();
            let new = tree::apply(&store, &mut unit, root, entry, &[edit]).unwrap();
            match root {
                None => unit.add_tree(&name, new),
                Some(_) => unit.set_root(&name, new),
            }
            unit.commit().unwrap();
            root = Some(new);
        }
        let tree = Tree::open(&store, root.unwrap()).unwrap();
        let held: Vec<(Path, Node)> = tree
            .walk::<Vec<u8>>(&[])
            .map(|item| {
                let item = item.unwrap();
                (item.key.path(), item.node)
            })
            .collect();
        let mut got = Vec::new();
        flatten(&owned, &mut Vec::new(), &mut got);
        assert_eq!(held, got);
        let kind_of = |path: &[&[u8]]| {
            let path: Path = path.iter().map(|s| s.to_vec()).collect();
            held.iter()
                .find(|(p, _)| *p == path)
                .map(|(_, n)| n.clone())
        };
        assert_eq!(kind_of(&[b"a"]), Some(Node::Both(Vec::new())));
        assert_eq!(kind_of(&[b"a", b"x"]), Some(Node::Value(vec![1])));
        assert_eq!(kind_of(&[b"b"]), Some(Node::List));
        assert_eq!(kind_of(&[b"d"]), Some(Node::Both(vec![4])));
        assert_eq!(held.len(), 7);
    }
    for seed in seeds(2) {
        let mut rng = Rng(seed ^ 0x0e);
        let mut nodes = BTreeMap::new();
        let mut owned = OwnedNode::list(Vec::new(), Vec::new());
        let mut kinds = BTreeSet::new();
        for step in 0..3_000 {
            let depth = rng.range(1, 5);
            let path: Path = (0..depth)
                .map(|_| match rng.below(4) {
                    0 => Vec::new(),
                    1 => vec![0],
                    2 => vec![0, 0],
                    _ => vec![1],
                })
                .collect();
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let n = rng.below(3);
            let value = rng.bytes(n);
            match rng.below(10) {
                0..=3 => {
                    owned.put(&refs, OwnedNode::leaf(Vec::new(), value.clone()));
                    model::put(&mut nodes, &path, Node::Value(value));
                }
                4..=5 => {
                    owned.put(&refs, OwnedNode::list(Vec::new(), Vec::new()));
                    model::put(&mut nodes, &path, Node::List);
                }
                6 => {
                    let both = OwnedNode {
                        segment: Vec::new(),
                        value: Some(value.clone()),
                        push: true,
                        children: Vec::new(),
                    };
                    owned.put(&refs, both);
                    model::put(&mut nodes, &path, Node::Both(value));
                }
                7..=8 => {
                    owned.remove(&refs);
                    model::delete(&mut nodes, &path);
                }
                _ => {
                    // Delete at the root: every node goes, the root stays.
                    owned.children.clear();
                    model::delete(&mut nodes, &[]);
                }
            }
            let mut got = Vec::new();
            flatten(&owned, &mut Vec::new(), &mut got);
            let want: Vec<(Path, Node)> =
                nodes.iter().map(|(p, n)| (p.clone(), n.clone())).collect();
            assert_eq!(got, want, "seed {seed} step {step}");
            for (_, n) in &want {
                kinds.insert(match n {
                    Node::List => 0,
                    Node::Value(_) => 1,
                    Node::Both(_) => 2,
                });
            }
        }
        assert_eq!(kinds.len(), 3, "every kind of node was made");
    }
}

/// The encoded length the store's tree gives a lock row stays within the
/// tree's bound at the largest key and user (format section 5.3).
#[test]
fn the_largest_lock_fits_the_tree() {
    let mut f = Fixture::new();
    let key = vec![0u8; MAX_LOCK_KEY];
    let user = vec![0u8; om_store::entry::MAX_LOCK_USER];
    let path_len = key::encoded_len(&[
        vec![7],
        vec![0; 16],
        vec![0; 16],
        vec![0; 8],
        [vec![0; 2], key.clone(), user.clone()].concat(),
    ]);
    // Format section 5.3 states it.
    assert_eq!(path_len, 34_911);
    assert!(path_len <= om_store::page::MAX_KEY);
    f.push(
        f.file,
        change_of(
            None,
            vec![LockEdit::Set {
                session: f.alice,
                key,
                user,
                value: vec![0; MAX_VALUE],
            }],
            Vec::new(),
        ),
    );
    compare(&f.store, &f.model, "a large lock");
}

/// A `MemStore` that counts the overflow pages read through it.
struct Counting {
    inner: MemStore,
    overflow: Cell<u64>,
}

impl PageStore for Counting {
    type Entry<'a> = MemEntry<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let page = self.inner.read(number, hash)?;
        if page.kind() == PageKind::Overflow {
            self.overflow.set(self.overflow.get() + 1);
        }
        Ok(page)
    }

    fn trees(&self) -> Vec<(TreeName, Root)> {
        self.inner.trees()
    }

    fn root(&self, name: &TreeName) -> Option<Root> {
        self.inner.root(name)
    }

    fn begin(&self) -> Result<MemEntry<'_>, StoreError> {
        self.inner.begin()
    }
}

/// Review of #732: a server's start finds the sessions it closes from their
/// lists alone, one at a time. It reads none of their replies, however
/// large, and holds one listed row at once, so what one start reads and
/// holds does not grow with the replies it deletes. A file's removal looks
/// through the sessions that hold lock lists one at a time too, holding
/// nothing for the many that have none for the file.
#[test]
fn a_start_reads_no_reply_of_the_sessions_it_closes() {
    let store = Counting {
        inner: MemStore::new(Numbering::Sequential),
        overflow: Cell::new(0),
    };
    let mut model = Model::new();
    let server = SERVERS[0];
    let mut push = |file: u64, payload: Payload| {
        let built = seal(&model.applied, 1, file, &payload);
        let applied = apply(&store, &built.entry()).unwrap();
        model.apply(&built.entry()).unwrap();
        applied
    };
    for (id, name) in [(1, b"f"), (2, b"g")] {
        push(
            id,
            Payload::File(FileEvent::Added {
                name: name.to_vec(),
                flags: 0,
                page0: Vec::new(),
            }),
        );
    }
    push(
        0,
        Payload::Session(SessionEvent::Started { server, count: 1 }),
    );
    let sessions = 40;
    for connection in 1..=sessions {
        let session = Session {
            server,
            start: 1,
            connection,
        };
        push(0, Payload::Session(SessionEvent::Opened(session)));
        push(
            1,
            Payload::Change(Change {
                op: Some(Op { session, seq: 1 }),
                reply: vec![connection as u8; MAX_REPLY],
                ..Change::default()
            }),
        );
        // A lock on the second file, released: the session keeps an empty
        // list of locks.
        let set = LockEdit::Set {
            session,
            key: b"k".to_vec(),
            user: b"u".to_vec(),
            value: b"v".to_vec(),
        };
        let release = LockEdit::ReleaseFile { session };
        push(2, change_of(None, vec![set, release], Vec::new()));
    }
    // The first file's removal lists the one server and every session, and
    // holds two rows at once: a session within its server.
    let removed = push(1, Payload::File(FileEvent::Removed));
    assert_eq!(removed.listed.rows, 1 + sessions);
    assert_eq!(removed.listed.held, 2, "rows held at once by the removal");
    store.overflow.set(0);
    let started = push(
        0,
        Payload::Session(SessionEvent::Started { server, count: 2 }),
    );
    assert_eq!(started.closed.len(), sessions as usize);
    assert_eq!(store.overflow.get(), 0, "overflow pages read by the start");
    assert_eq!(started.listed.rows, sessions);
    assert_eq!(started.listed.held, 1, "rows held at once by the start");
    compare(&store, &model, "after the start");
}

/// Review of #732: the model checks and makes one change's lock edits in
/// linear steps too. Bob's locks sort between Alice's and Carol's; each
/// release of Bob's touches none of Carol's growing run, and a release of
/// Carol's takes one step per lock it removes and leaves Alice's.
#[test]
fn the_model_takes_lock_edits_in_linear_steps() {
    let mut f = Fixture::new();
    let carol = Session {
        connection: 3,
        ..f.alice
    };
    f.push(0, Payload::Session(SessionEvent::Opened(carol)));
    let n = 5_000u64;
    let mut locks = Vec::new();
    for i in 0..n {
        locks.push(LockEdit::Set {
            session: carol,
            key: i.to_be_bytes().to_vec(),
            user: b"t0".to_vec(),
            value: b"v".to_vec(),
        });
        locks.push(LockEdit::ReleaseFile { session: f.bob });
    }
    let edits = locks.len() as u64;
    let built = f.seal(f.file, &change_of(None, locks, Vec::new()));
    let outcome = f.model.apply(&built.entry()).unwrap();
    assert!(
        outcome.steps <= 2 * edits,
        "{} steps for {edits} lock edits",
        outcome.steps
    );
    assert_eq!(
        f.model.locks.keys().filter(|l| l.session == carol).count() as u64,
        n
    );
    let release = f.seal(
        f.file,
        &change_of(
            None,
            vec![LockEdit::ReleaseFile { session: carol }],
            Vec::new(),
        ),
    );
    let outcome = f.model.apply(&release.entry()).unwrap();
    assert!(
        outcome.steps <= n + 2,
        "{} steps to release {n} locks",
        outcome.steps
    );
    assert!(f.model.locks.keys().all(|l| l.session != carol));
    assert_eq!(
        f.model
            .locks
            .keys()
            .filter(|l| l.session == f.alice)
            .count(),
        2
    );
}

/// What the runs on the page file reached, summed over seeds.
#[derive(Debug, Default)]
struct OnDisk {
    entries: u64,
    extended: u64,
    checkpoints: u64,
    kept: u64,
    dropped: u64,
    crashes: u64,
    power_losses: u64,
    reopens: u64,
    replayed: u64,
    compared: u64,
}

/// A running process: the page file, with the store folder's lock.
struct OnDiskProcess {
    pf: PageFile,
    _lock: FolderLock,
}

/// Applies `built` through the page file: one reservation, extended while
/// the entry runs out (the unit that ran out is dropped, and `apply` begins
/// another).
fn apply_on_disk(pf: &PageFile, built: &Built, disk: &mut OnDisk) -> Result<(), String> {
    let res = pf
        .reserve(built.index, 4, PagePurpose::Entry)
        .map_err(|e| e.to_string())?;
    loop {
        match apply(&res, &built.entry()) {
            Ok(_) => return Ok(()),
            Err(Error::Tree(tree::Error::Store(StoreError::Full))) => {
                disk.extended += 1;
                res.extend(256).map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The checkpoint's entry for the model's last entry.
fn entry_id(p: &Position) -> EntryId {
    EntryId {
        index: p.index,
        term: p.term,
        hash: p.hash,
    }
}

/// The model after the log's entries up to `index`.
fn model_at(log: &[Built], index: u64) -> Model {
    let mut model = Model::new();
    for built in log.iter().take_while(|b| b.index <= index) {
        model.apply(&built.entry()).unwrap();
    }
    model
}

/// Opens the store after a stop, checks the checkpoint it opened against
/// the model at that entry, applies the log's later entries again and
/// checks the whole state; a crash meanwhile stops it and it starts over.
fn reopen_on_disk(
    sim: &Sim,
    rates: Rates,
    log: &[Built],
    model: &Model,
    disk: &mut OnDisk,
) -> OnDiskProcess {
    // The comparisons read without faults; the rest runs under `rates`.
    let quiet = |sim: &Sim, f: &mut dyn FnMut()| {
        sim.set_rates(Rates::default());
        f();
        sim.set_rates(rates);
    };
    loop {
        sim.restart();
        disk.reopens += 1;
        let fs = sim.fs();
        let lock = match fs.lock() {
            Ok(lock) => lock,
            Err(e) => {
                assert!(sim.halted(), "lock: {e}");
                disk.crashes += 1;
                continue;
            }
        };
        let (pf, opened) = match PageFile::open(&fs, &lock, PageConfig::default()) {
            Ok(opened) => opened,
            Err(e) => {
                assert!(sim.halted(), "open: {e}");
                disk.crashes += 1;
                continue;
            }
        };
        let at = opened.entry.index;
        let prefix = model_at(log, at);
        quiet(sim, &mut || compare(&pf, &prefix, "the checkpoint opened"));
        disk.compared += 1;
        let mut crashed = false;
        for built in log.iter().filter(|b| b.index > at) {
            if let Err(e) = apply_on_disk(&pf, built, disk) {
                assert!(sim.halted(), "replay of {}: {e}", built.index);
                crashed = true;
                break;
            }
            disk.replayed += 1;
        }
        if crashed {
            disk.crashes += 1;
            continue;
        }
        quiet(sim, &mut || compare(&pf, model, "after the replay"));
        disk.compared += 1;
        return OnDiskProcess { pf, _lock: lock };
    }
}

/// A crash drawn about once in this many steps of the disk.
const ON_DISK_RATES: Rates = Rates {
    crash: 150,
    fail_flush: 0,
    fail_write: 0,
    no_space: 0,
    fail_read: 0,
    damage: 0,
    misplace: 0,
    lose_write: 0,
    writeback: 0,
};

fn run_on_the_page_file(seed: u64, steps: u64, reached: &mut Reached, disk: &mut OnDisk) {
    let sim = Sim::new(seed);
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    let mut proc = Some(OnDiskProcess {
        pf: PageFile::create(&fs, &lock, PageConfig::default()).unwrap(),
        _lock: lock,
    });
    sim.set_rates(ON_DISK_RATES);
    let mut gen = Gen::new(seed);
    let mut rng = Rng(seed ^ 0xd15c);
    let mut log: Vec<Built> = Vec::new();
    let mut model = Model::new();
    let mut names = 0u64;
    for _ in 0..steps {
        let Some(p) = proc.as_ref() else {
            proc = Some(reopen_on_disk(&sim, ON_DISK_RATES, &log, &model, disk));
            continue;
        };
        let pf = &p.pf;
        let mut stop = false;
        match rng.below(100) {
            0..=79 => {
                // The entry is appended to the log, then applied: a crash
                // meanwhile leaves it for the replay.
                let built = gen.next(&model);
                model.apply(&built.entry()).unwrap();
                log.push(built.clone());
                disk.entries += 1;
                match apply_on_disk(pf, &built, disk) {
                    Ok(()) => {}
                    Err(e) if e.contains("checkpoint needed") => {
                        // Applied after the checkpoint, at the replay.
                        stop = true;
                    }
                    Err(e) => {
                        assert!(sim.halted(), "entry {}: {e}", built.index);
                        disk.crashes += 1;
                        stop = true;
                    }
                }
            }
            80..=89 => {
                let keep = rng.percent(25).then(|| {
                    names += 1;
                    format!("k{names}")
                });
                match pf.checkpoint(entry_id(&model.applied), keep.as_deref()) {
                    Ok(_) => {
                        disk.checkpoints += 1;
                        disk.kept += u64::from(keep.is_some());
                        sim.set_rates(Rates::default());
                        compare(pf, &model, "at a checkpoint");
                        sim.set_rates(ON_DISK_RATES);
                        disk.compared += 1;
                    }
                    Err(e) => {
                        assert!(sim.halted(), "checkpoint: {e}");
                        disk.crashes += 1;
                        stop = true;
                    }
                }
            }
            90..=93 => {
                let listed = pf.restore_points();
                if let Some((rp, false)) = listed.first() {
                    pf.drop_restore_point(&rp.name).unwrap();
                    disk.dropped += 1;
                }
            }
            _ => {
                sim.crash(Fates::Drawn);
                disk.power_losses += 1;
                stop = true;
            }
        }
        if stop {
            proc = None;
        }
    }
    // Without faults: a last stop, and the whole state again.
    sim.set_rates(Rates::default());
    drop(proc);
    let p = reopen_on_disk(&sim, Rates::default(), &log, &model, disk);
    let pf = &p.pf;
    let next = model.applied;
    pf.checkpoint(entry_id(&next), None).unwrap();
    compare(pf, &model, "at the end");
    add(reached, std::mem::take(&mut gen.reached));
}

/// The page file and `apply` fit (#735): entries of every kind (files
/// added, renamed and removed; changes; sessions; marks), applied through
/// `apply` on the page file itself, on the simulated disk, with
/// checkpoints, restore points kept and dropped, crashes drawn at any step
/// and power losses. Every tree and the store's tables match the model at
/// each checkpoint, at the checkpoint each reopen takes, and after the
/// replay of the log.
#[test]
fn entries_through_apply_on_the_page_file_match_the_model() {
    let steps = env("OM_STORE_ENTRIES", 300);
    let mut reached = Reached::default();
    let mut disk = OnDisk::default();
    for seed in seeds(3) {
        run_on_the_page_file(seed, steps, &mut reached, &mut disk);
    }
    eprintln!("{disk:?}\n{reached:?}");
    if std::env::var("OM_STORE_SEED").is_err() {
        let counts = [
            disk.entries,
            disk.checkpoints,
            disk.kept,
            disk.dropped,
            disk.crashes,
            disk.extended,
            disk.power_losses,
            disk.reopens,
            disk.replayed,
        ];
        assert!(counts.iter().all(|&n| n > 0), "{disk:?}");
        for kind in [Kind::Change, Kind::File, Kind::Session, Kind::Mark] {
            assert!(
                reached.kinds.get(&kind).copied().unwrap_or(0) > 0,
                "{kind:?}: {reached:?}"
            );
        }
    }
}

/// The same, longer.
#[test]
#[ignore = "long: 12 seeds of 2,000 steps on the simulated disk by default (OM_STORE_SEEDS, OM_STORE_ENTRIES); run with --release"]
fn long_entries_through_apply_on_the_page_file_match_the_model() {
    let steps = env("OM_STORE_ENTRIES", 2_000);
    let mut reached = Reached::default();
    let mut disk = OnDisk::default();
    for seed in seeds(12) {
        run_on_the_page_file(seed, steps, &mut reached, &mut disk);
    }
    eprintln!("{disk:?}\n{reached:?}");
}
