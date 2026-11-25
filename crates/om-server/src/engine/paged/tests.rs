//! Tests of the store engine's file (the module doc of `super`): two
//! engines give one answer to every read of the same file; an export of
//! the imported tree is the file engine's export byte for byte, and
//! imports again to the same root hash; the page file keeps an import
//! across a checkpoint and a reopen, and fails the reads a damaged page
//! reaches; and what an import refuses leaves nothing behind.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use om_hbam::container::{PageHeader, PAGE_HEADER_LEN};
use om_hbam::{HbamFile, OwnedNode};
use om_store::page::{Hash, MemStore, Numbering, PageKind, StoreError, PAGE_SIZE};
use om_store::pagefile::{Config, EntryId, PageFile, Purpose, Reservation, SpaceError};
use om_store::sim::Sim;
use om_store::sys::Fs;

use super::*;
use crate::anchored::FileAt;
use crate::derived::{self, Derived};
use crate::engine::export::{write_checked, Destination, Source};
use crate::engine::view::wire;
use crate::scripts::fixture;
use crate::store::Store;

/// Two views read as one: every call is answered by both, the answers are
/// compared (owned, so that a lent answer and a copy compare alike), and
/// the first view's is given. Calls are counted.
pub(super) struct Both<'a> {
    pub file: &'a dyn View,
    pub paged: &'a dyn View,
    pub calls: Cell<u64>,
}

impl<'a> Both<'a> {
    pub fn new(file: &'a dyn View, paged: &'a dyn View) -> Self {
        Both {
            file,
            paged,
            calls: Cell::new(0),
        }
    }

    fn count(&self) {
        self.calls.set(self.calls.get() + 1);
    }
}

fn owned_child(child: Child<'_>) -> Child<'static> {
    Child {
        segment: Cow::Owned(child.segment.into_owned()),
        head: child.head.into_owned(),
    }
}

fn owned_step(step: Step<'_>) -> Step<'static> {
    Step {
        depth: step.depth,
        segment: Cow::Owned(step.segment.into_owned()),
        head: step.head.into_owned(),
    }
}

impl View for Both<'_> {
    fn version(&self) -> Version {
        self.file.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        self.count();
        let a = self.file.node(path).map(|h| h.map(Head::into_owned));
        let b = self.paged.node(path).map(|h| h.map(Head::into_owned));
        assert_eq!(a, b, "node {path:02x?}");
        a
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        self.count();
        let a = self
            .file
            .subtree(path, limit)
            .map(|n| n.map(Cow::into_owned));
        let b = self
            .paged
            .subtree(path, limit)
            .map(|n| n.map(Cow::into_owned));
        assert!(a == b, "subtree {path:02x?} within {limit}");
        a.map(|n| n.map(Cow::Owned))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError> {
        self.count();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let ra = self.file.children_page(path, from, direction, max, &mut a);
        let rb = self.paged.children_page(path, from, direction, max, &mut b);
        let a: Vec<Child<'static>> = a.into_iter().map(owned_child).collect();
        let b: Vec<Child<'static>> = b.into_iter().map(owned_child).collect();
        assert_eq!(
            (&ra, &a),
            (&rb, &b),
            "children of {path:02x?} from {from:02x?} {direction:?}, {max} a page"
        );
        out.extend(a);
        ra
    }

    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'v, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        self.count();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let ra = self
            .file
            .subtrees_page(path, from, direction, max, limit, &mut a);
        let rb = self
            .paged
            .subtrees_page(path, from, direction, max, limit, &mut b);
        let a: Vec<OwnedNode> = a.into_iter().map(Cow::into_owned).collect();
        let b: Vec<OwnedNode> = b.into_iter().map(Cow::into_owned).collect();
        assert!(
            ra == rb && a == b,
            "subtrees of {path:02x?} from {from:02x?} {direction:?}, {max} a page within {limit}"
        );
        out.extend(a.into_iter().map(Cow::Owned));
        ra
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        self.count();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let ra = self.file.walk_page(path, after, max, &mut a);
        let rb = self.paged.walk_page(path, after, max, &mut b);
        let a: Vec<Step<'static>> = a.into_iter().map(owned_step).collect();
        let b: Vec<Step<'static>> = b.into_iter().map(owned_step).collect();
        assert_eq!(
            (&ra, &a),
            (&rb, &b),
            "walk of {path:02x?} after {after:02x?}, {max} a page"
        );
        out.extend(a);
        ra
    }

    /// The file engine's: versions are each engine's own (its writes, or
    /// the tree's entries), and are checked apart.
    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.file.changed_since(path, since)
    }
}

/// A small xorshift generator, so that a seed replays.
pub(super) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The paths of a walk's steps, below the walk's own node.
fn paths_of(steps: &[Step<'_>]) -> Vec<Vec<Vec<u8>>> {
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut out = Vec::new();
    for step in steps {
        path.truncate(step.depth.saturating_sub(1));
        if step.depth > 0 {
            path.push(step.segment.to_vec());
        }
        out.push(path.clone());
    }
    out
}

fn refs(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// A bound of a listing: none, or a segment of a child of `node`, or one
/// beside them, included or not.
fn bound<'a>(rng: &mut Rng, segments: &'a [Vec<u8>]) -> Bound<&'a [u8]> {
    if segments.is_empty() || rng.below(4) == 0 {
        return Bound::Unbounded;
    }
    let segment = segments[rng.below(segments.len())].as_slice();
    if rng.below(2) == 0 {
        Bound::Included(segment)
    } else {
        Bound::Excluded(segment)
    }
}

/// A limit for a subtree: none, its size as the file engine's tree counts
/// it or one byte less, or a small one.
fn limit(rng: &mut Rng, file: &dyn View, path: &[&[u8]]) -> usize {
    let held = || {
        file.subtree(path, NO_LIMIT)
            .ok()
            .flatten()
            .map_or(64, |n| om_hbam::tree::memory(&n))
    };
    match rng.below(5) {
        0 => NO_LIMIT,
        1 => held(),
        2 => held().saturating_sub(1),
        3 => rng.below(2_000),
        _ => held() + rng.below(100),
    }
}

/// `count` reads of the kinds readers make, at paths of the file and
/// beside them, through `both`; each answered by both views and compared.
pub(super) fn seeded_reads(both: &Both<'_>, paths: &[Vec<Vec<u8>>], seed: u64, count: usize) {
    let mut rng = Rng(seed | 1);
    let view: &dyn View = both;
    for _ in 0..count {
        let mut path = paths[rng.below(paths.len())].clone();
        match rng.below(8) {
            // A missing child, and a sibling that is not there.
            0 => path.push(vec![rng.next() as u8]),
            1 if !path.is_empty() => {
                let last = path.last_mut().expect("a segment");
                last.push(rng.next() as u8);
            }
            _ => {}
        }
        let at = refs(&path);
        // The children's segments, from the file engine's view.
        let segments: Vec<Vec<u8>> = both
            .file
            .children(&at, Bound::Unbounded, Direction::Forward)
            .map(|c| {
                c.expect("the file engine's reads do not fail")
                    .segment
                    .to_vec()
            })
            .chain([vec![0], vec![0x80, 0x01], vec![0xff]])
            .collect();
        let direction = if rng.below(2) == 0 {
            Direction::Forward
        } else {
            Direction::Backward
        };
        let max = match rng.below(4) {
            0 => 1,
            1 => 2 + rng.below(3),
            _ => 1 + rng.below(300),
        };
        match rng.below(7) {
            0 => {
                let _ = view.node(&at);
            }
            1 => {
                let limit = limit(&mut rng, both.file, &at);
                let _ = view.subtree(&at, limit);
            }
            2 => {
                let from = bound(&mut rng, &segments);
                let mut out = Vec::new();
                let _ = view.children_page(&at, from, direction, max, &mut out);
            }
            3 => {
                let from = bound(&mut rng, &segments);
                let limit = match rng.below(3) {
                    0 => NO_LIMIT,
                    _ => rng.below(4_000),
                };
                let mut out = Vec::new();
                let _ = view.subtrees_page(&at, from, direction, max, limit, &mut out);
            }
            4 => {
                // A listing read to its end, a page at a time.
                let from = bound(&mut rng, &segments);
                let _ = view.children(&at, from, direction).count();
            }
            5 => {
                // A walk in pages of `max`, resumed where each ended.
                let mut after: Option<Vec<Vec<u8>>> = None;
                for _ in 0..(1 + rng.below(6)) {
                    let mut out = Vec::new();
                    match view.walk_page(&at, after.as_deref(), max, &mut out) {
                        Ok(Some(resume)) => after = Some(resume),
                        _ => break,
                    }
                }
            }
            _ => {
                let _ = wire(view, &at);
            }
        }
    }
}

/// What one comparison of a file read, and how many calls both views
/// answered.
#[derive(Debug, Default)]
pub(super) struct Compared {
    pub nodes: usize,
    pub calls: u64,
}

/// Every read of the file the file engine loaded (`store`) and of its
/// import (`paged`) answered alike, through [`Both`]: its derived state
/// computed through both (catalog, accounts, locale, date order), a walk
/// of the whole tree, each node, each list's children and subtrees both
/// ways and its wire form, and `seeded` reads more.
pub(super) fn compare_reads(
    store: &Store,
    paged: &dyn View,
    page_zero: &[u8],
    seed: u64,
    seeded: usize,
) -> Compared {
    let file = store.tree_view();
    let both = Both::new(&file, paged);
    let view: &dyn View = &both;
    let catalog = derived::read_catalog(view).expect("the catalog reads");
    let mut mine = Derived::new(view, catalog, store.derived().name()).expect("derived state");
    mine.set_page_zero(page_zero);
    assert!(
        mine.same_as(store.derived(), true),
        "the derived state differs"
    );
    let steps: Vec<Step> = view
        .walk(&[])
        .collect::<Result<_, _>>()
        .expect("the walk reads");
    let paths = paths_of(&steps);
    let many = paths.len() > 5_000;
    for (step, path) in steps.iter().zip(&paths) {
        let at = refs(path);
        let _ = view.node(&at);
        if step.head.list {
            for direction in [Direction::Forward, Direction::Backward] {
                let _ = view.children(&at, Bound::Unbounded, direction).count();
            }
            // Every list's subtrees and wire form, but in the largest
            // files only those of lists of up to a few hundred nodes.
            let small = !many
                || file
                    .subtree(&at, NO_LIMIT)
                    .ok()
                    .flatten()
                    .is_some_and(|n| om_hbam::tree::memory_within(&n, 64 << 10).is_some());
            if small {
                let _ = view
                    .subtrees(&at, Bound::Unbounded, Direction::Backward, NO_LIMIT)
                    .count();
                let _ = wire(view, &at);
            }
        }
    }
    seeded_reads(&both, &paths, seed, seeded);
    Compared {
        nodes: steps.len() - 1,
        calls: both.calls.get(),
    }
}

/// A folder for one test's files in the build's target folder, removed
/// with what is in it when dropped.
pub(super) struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/om-paged-tests")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    /// A new file `name` in the folder, for reading and writing.
    pub fn create(&self, name: &str) -> File {
        File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.0.join(name))
            .unwrap()
    }

    /// The folder, opened as a hosted file's is.
    pub fn folder(&self) -> crate::anchored::Folder {
        FileAt::open(&self.0.join("any.fmp12"))
            .unwrap()
            .folder_now()
            .unwrap()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The file engine's written export of the file `store` loaded, as its
/// compacted copy writes it.
pub(super) fn file_engine_export(store: &Store, scratch: &Scratch, name: &str) -> Vec<u8> {
    let Source::Written { change_number } = store.compacted_source() else {
        unreachable!("a compacted copy is written")
    };
    let mut file = scratch.create(name);
    write_checked(
        &store.tree_view(),
        store.derived().page_zero(),
        change_number,
        &mut file,
        Progress::none(),
    )
    .unwrap();
    let bytes = std::fs::read(scratch.0.join(name)).unwrap();
    std::fs::remove_file(scratch.0.join(name)).unwrap();
    bytes
}

/// The store engine's export of `view`, with `page_zero`.
pub(super) fn paged_export(
    view: &dyn View,
    page_zero: &[u8],
    scratch: &Scratch,
    name: &str,
) -> Vec<u8> {
    let mut file = scratch.create(name);
    export(view, page_zero, Target::File(&mut file), Progress::none()).unwrap();
    let bytes = std::fs::read(scratch.0.join(name)).unwrap();
    std::fs::remove_file(scratch.0.join(name)).unwrap();
    bytes
}

pub(super) fn tree_name() -> TreeName {
    TreeName::new(b"file:1".to_vec()).unwrap()
}

/// Import `bytes` into a store in memory, in `batch`es from entry 1.
pub(super) fn import_into_memory(bytes: &[u8], batch: Batch) -> (MemStore, Imported) {
    let mut store = MemStore::new(Numbering::Sequential);
    let imported = import(Cursor::new(bytes), &mut store, &tree_name(), 1, batch).unwrap();
    (store, imported)
}

fn long_value(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The files of the tests' fixture (`tests/fixtures/tasks.rs`), in the
/// shapes the tests host them: by name, the bytes of each.
fn fixture_files() -> Vec<(&'static str, Vec<u8>)> {
    let mut out: Vec<(&'static str, OwnedNode)> = Vec::new();
    out.push(("bare", fixture::tree(&[], false)));
    let mut tasks = fixture::tree(&[], true);
    fixture::add_tasks(&mut tasks, 1..=40);
    out.push(("tasks", tasks.clone()));
    let mut both = tasks.clone();
    both.put(
        &[&[2], &[0x70]],
        OwnedNode::leaf(Vec::new(), long_value(2500)),
    );
    both.put(&[&[2], &[0x71]], OwnedNode::list(Vec::new(), Vec::new()));
    out.push(("a long value and an empty list", both));
    let mut calculated = tasks.clone();
    fixture::add_calculated(&mut calculated);
    out.push(("calculations", calculated));
    let mut photo = tasks.clone();
    fixture::add_photo(&mut photo);
    fixture::add_photo_calculations(&mut photo);
    out.push(("container data", photo));
    let mut themes = fixture::tree(&[], true);
    fixture::add_themes(&mut themes, &[1, 2]);
    out.push(("themes", themes));
    let mut indexed = tasks.clone();
    fixture::index_layouts(&mut indexed);
    fixture::index_scripts(&mut indexed);
    fixture::index_layout_definitions(&mut indexed);
    out.push(("design indexes", indexed));
    let mut accounts = tasks.clone();
    fixture::add_reader(&mut accounts);
    fixture::add_custom_clerk(&mut accounts);
    fixture::add_local_accounts(&mut accounts);
    fixture::put_data_entry_only(&mut accounts);
    out.push(("accounts", accounts));
    out.push((
        "scripts",
        fixture::tree(&[fixture::server_work(), fixture::failing()], true),
    ));
    let mut many = fixture::tree(&[], true);
    fixture::add_tasks(&mut many, 1..=3000);
    out.push(("3,000 records", many));
    out.into_iter()
        .map(|(name, root)| (name, written(&root)))
        .collect()
}

/// `root` written as an `.fmp12`.
fn written(root: &OwnedNode) -> Vec<u8> {
    om_hbam::writer::rebuild(root, &om_hbam::new_superblock("Tasks").unwrap()).unwrap()
}

/// The file engine's store of `bytes`.
pub(super) fn loaded(bytes: &[u8]) -> Store {
    Store::load(
        &HbamFile::from_bytes(bytes.to_vec()).unwrap(),
        fixture::FILE,
    )
    .unwrap()
}

/// Batches small enough that every fixture takes several.
const SMALL: Batch = Batch {
    nodes: 97,
    bytes: 64 << 10,
};

/// Two engines, one answer (docs/server-storage.md 12, "Both engines
/// behind one server", for reads): for every fixture file, each read the
/// comparison makes is answered alike by the file engine's view of the
/// loaded file and the store engine's of its import; an export of the
/// import is the file engine's byte for byte; and that export imported
/// again gives the same root hash.
#[test]
fn two_engines_answer_every_read_of_the_fixture_files_alike() {
    let scratch = Scratch::new("fixtures");
    let mut calls = 0;
    for (n, (name, bytes)) in fixture_files().into_iter().enumerate() {
        let store = loaded(&bytes);
        let (memory, imported) = import_into_memory(&bytes, SMALL);
        assert!(imported.entries.1 > imported.entries.0, "{name}: batches");
        let view = PagedView::open(&memory, imported.root).unwrap();
        let compared = compare_reads(&store, &view, &imported.page_zero, n as u64 + 1, 1_500);
        assert_eq!(compared.nodes as u64, imported.nodes, "{name}");
        calls += compared.calls;
        let theirs = file_engine_export(&store, &scratch, "file.fmp12");
        let ours = paged_export(&view, &imported.page_zero, &scratch, "paged.fmp12");
        assert!(theirs == ours, "{name}: the exports differ");
        let (_, again) = import_into_memory(&ours, SMALL);
        assert_eq!(again.root.hash, imported.root.hash, "{name}");
        println!(
            "{name}: {} nodes, {} entries, {} calls compared",
            imported.nodes,
            imported.entries.1 - imported.entries.0 + 1,
            compared.calls
        );
    }
    println!("{calls} calls compared in all");
}

/// [`View::changed_since`] from the tree's stamps: a version after an
/// entry that changed a node is later, and answers yes for that node and
/// the root; a version answers no for itself.
#[test]
fn a_change_is_seen_since_the_version_before_it() {
    let (_, bytes) = fixture_files().swap_remove(1);
    let (memory, imported) = import_into_memory(&bytes, SMALL);
    let before = PagedView::open(&memory, imported.root).unwrap();
    let record = [
        om_hbam::slot::uint_segment(fixture::TABLE),
        vec![5],
        om_hbam::serial::encode(7),
        om_hbam::slot::uint_segment(fixture::NAME),
    ];
    assert!(before.node(&refs(&record)).unwrap().is_some());
    let later = apply_unit(
        &memory,
        &tree_name(),
        Some(imported.root),
        imported.entries.1 + 1,
        &[Edit::leaf(record.to_vec(), b"renamed".to_vec())],
    )
    .unwrap();
    let after = PagedView::open(&memory, later).unwrap();
    assert!(after.version() > before.version());
    let since = before.version();
    for path in [&record[..], &record[..3], &record[..1], &[]] {
        assert!(
            after.changed_since(&refs(path), since).unwrap(),
            "{path:02x?}"
        );
        assert!(!after.changed_since(&refs(path), after.version()).unwrap());
        assert!(!before.changed_since(&refs(path), since).unwrap());
    }
    // The old version still reads as it was.
    assert_ne!(
        before.node(&refs(&record)).unwrap(),
        after.node(&refs(&record)).unwrap()
    );
}

/// The page file on the simulated disk: where an import goes when the
/// store's units are the page file's, each reserved, with a checkpoint
/// whenever the pages waiting for one reach the budget.
pub(super) struct OnPageFile<'f> {
    pub pf: &'f PageFile,
    pub checkpoints: u64,
}

impl<'f> OnPageFile<'f> {
    fn reserve(&mut self, entry: u64, pages: u64) -> Result<Reservation<'f>, tree::Error> {
        loop {
            match self.pf.reserve(entry, pages, Purpose::Entry) {
                Ok(reservation) => return Ok(reservation),
                Err(SpaceError::CheckpointNeeded { .. }) => self.checkpoint()?,
                Err(e) => return Err(tree::Error::Store(StoreError::Failed(e.to_string()))),
            }
        }
    }

    /// A checkpoint of what the import committed. Its entries are in no
    /// log, so their term and hash are none.
    pub fn checkpoint(&mut self) -> Result<(), tree::Error> {
        let entry = EntryId {
            index: self.pf.newest_entry(),
            term: 0,
            hash: Hash::default(),
        };
        self.pf
            .checkpoint(entry, None)
            .map_err(|e| tree::Error::Store(StoreError::Failed(e.to_string())))?;
        self.checkpoints += 1;
        Ok(())
    }
}

impl Units for OnPageFile<'_> {
    fn has_tree(&self, tree: &TreeName) -> bool {
        self.pf.root(tree).is_some()
    }

    fn apply(
        &mut self,
        tree: &TreeName,
        root: Option<Root>,
        entry: u64,
        edits: &[Edit],
    ) -> Result<Root, tree::Error> {
        // A unit that runs out changed nothing: it is applied again with
        // a larger reservation.
        let mut pages = 16 + edits.len() as u64 / 8;
        loop {
            let reservation = self.reserve(entry, pages)?;
            match apply_unit(&reservation, tree, root, entry, edits) {
                Err(tree::Error::Store(StoreError::Full)) => pages = 2 * pages + 64,
                done => return done,
            }
        }
    }

    fn drop_tree(&mut self, tree: &TreeName, entry: u64) -> Result<(), tree::Error> {
        let reservation = self.reserve(entry, 4)?;
        drop_unit(&reservation, tree)
    }
}

/// A page file small enough in memory that an import checkpoints along the
/// way and reads its pages back from the disk.
pub(super) fn small_config() -> Config {
    Config {
        cache_bytes: 256 << 10,
        dirty_budget: 8,
        reserve: 16,
        growth: 64,
    }
}

/// A leaf page of the tree at `root`, in the middle of it.
fn middle_leaf(pf: &PageFile, root: Root) -> u64 {
    let mut page = pf.read_page(root.page, &root.hash).unwrap();
    while page.kind() == PageKind::Index {
        let (number, hash) = page.references()[page.count() / 2];
        page = pf.read_page(number, &hash).unwrap();
    }
    page.number()
}

/// The page file on the simulated disk: an import (checkpointed along the
/// way), a checkpoint, the process gone, the page file opened again and
/// the checkpoint pinned, and its tree read and exported as the file
/// engine reads and exports the file. Then a leaf of the tree damaged on
/// the device: the reads that reach it fail and those that do not read,
/// and an export fails and leaves nothing under its name.
#[test]
fn the_page_file_keeps_an_import_and_fails_the_reads_a_damaged_page_reaches() {
    let (_, bytes) = fixture_files().pop().unwrap();
    let store = loaded(&bytes);
    let scratch = Scratch::new("page-file");
    let expected = file_engine_export(&store, &scratch, "file.fmp12");
    let sim = Sim::new(11);
    let config = small_config();
    let imported = {
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        let pf = PageFile::create(&fs, &lock, config).unwrap();
        let mut units = OnPageFile {
            pf: &pf,
            checkpoints: 0,
        };
        let imported = import(Cursor::new(&bytes), &mut units, &tree_name(), 1, SMALL).unwrap();
        assert!(units.checkpoints > 0, "the budget forced checkpoints");
        units.checkpoint().unwrap();
        imported
    };
    sim.restart();
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    let (pf, _) = PageFile::open(&fs, &lock, config).unwrap();
    let pin = pf.pin(pf.checkpoint_entry().1.index).unwrap();
    let root = pin
        .roots()
        .iter()
        .find(|(name, _)| *name == tree_name())
        .map(|(_, root)| *root)
        .unwrap();
    assert_eq!(root, imported.root);
    let view = PagedView::open(&pf, root).unwrap();
    let compared = compare_reads(&store, &view, &imported.page_zero, 5, 500);
    let ours = paged_export(&view, &imported.page_zero, &scratch, "paged.fmp12");
    assert!(ours == expected, "the page file's export differs");
    let walked: Vec<Vec<Vec<u8>>> = {
        let steps: Vec<Step> = (&view as &dyn View).walk(&[]).map(Result::unwrap).collect();
        paths_of(&steps)
    };
    let leaf = middle_leaf(&pf, root);
    drop(pin);
    drop(pf);
    drop(lock);
    sim.restart();
    sim.damage(om_store::pagefile::FILE, leaf * PAGE_SIZE as u64 + 100, 8)
        .unwrap();
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    let (pf, _) = PageFile::open(&fs, &lock, config).unwrap();
    let pin = pf.pin(pf.checkpoint_entry().1.index).unwrap();
    let view = PagedView::open(&pf, pin.roots()[0].1).unwrap();
    let dynamic: &dyn View = &view;
    let mut read = 0;
    let mut failed = None;
    for step in dynamic.walk(&[]) {
        match step {
            Ok(_) => read += 1,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    assert!(
        matches!(failed, Some(ReadError::Unreadable { .. })),
        "{failed:?}"
    );
    assert!(
        read > 1 && read < walked.len(),
        "{read} of {}",
        walked.len()
    );
    // The first node the walk did not reach is on the damaged page.
    let reached = refs(&walked[read]);
    assert!(matches!(
        dynamic.node(&reached),
        Err(ReadError::Unreadable { .. })
    ));
    assert!(dynamic.subtree(&[], NO_LIMIT).is_err());
    assert!(dynamic.node(&refs(&walked[1])).unwrap().is_some());
    assert!(dynamic
        .node(&refs(&walked[walked.len() - 1]))
        .unwrap()
        .is_some());
    let folder = scratch.folder();
    let to = Destination {
        folder: &folder,
        name: OsStr::new("damaged.fmp12"),
        permissions: None,
    };
    let refused = export(
        dynamic,
        &imported.page_zero,
        Target::Folder(&to),
        Progress::none(),
    );
    assert!(
        matches!(
            refused,
            Err(ExportError::Read(ReadError::Unreadable { .. }))
        ),
        "{refused:?}"
    );
    assert!(
        !scratch.names().iter().any(|n| n == "damaged.fmp12"),
        "{:?}",
        scratch.names()
    );
    println!(
        "{} nodes, {} calls compared; the damaged leaf {leaf} stopped the walk after {read}",
        imported.nodes, compared.calls
    );
}

/// A file of 4,096-byte pages: page 0, an index root no reader reads, and
/// leaf pages 2, 3, ... with these bodies, chained in that order (as
/// `om_hbam::stream`'s tests make them).
fn file_of(bodies: &[&[u8]]) -> Vec<u8> {
    let mut out = om_hbam::new_superblock("Stream").unwrap();
    let mut page = |level: u16, prev: u32, next: u32, body: &[u8]| {
        let header = PageHeader {
            log_seq_num: 0,
            prev,
            next,
            level,
            free_space: (4096 - PAGE_HEADER_LEN - body.len()) as u16,
            mod_count: 1,
        };
        let start = out.len();
        out.extend(header.to_bytes());
        out.extend(body);
        out.resize(start + 4096, 0);
    };
    let last = bodies.len() as u32 + 1;
    page(1, 0, last, &[]);
    for (i, body) in bodies.iter().enumerate() {
        let number = i as u32 + 2;
        let next = if number == last { 0 } else { number + 1 };
        page(0, if i == 0 { 0 } else { number - 1 }, next, body);
    }
    out
}

/// The leaf chunk of `key` holding `value`.
fn leaf(key: &[u8], value: &[u8]) -> Vec<u8> {
    let code = om_hbam::slot::minimal_code(false, key.len(), Some(value.len())).unwrap();
    om_hbam::slot::encode_chunk(code, key, value).unwrap()
}

/// A root list of leaves `a` to `e`, then `after`.
fn leaves_then(after: &[u8]) -> Vec<u8> {
    let mut body = vec![0x80];
    for key in b'a'..=b'e' {
        body.extend(leaf(&[key], &[key, key]));
    }
    body.extend(after);
    body
}

/// What an import refuses, it refuses with nothing left behind: the trees
/// of the store are as before, after one batch or several were committed.
#[test]
fn an_import_refuses_what_it_cannot_keep_and_leaves_nothing() {
    let one = Batch {
        nodes: 1,
        bytes: 1 << 20,
    };
    // A key segment of 220 bytes: the reader reads it, the writer writes
    // at most 219 (hbam-writer.md 2.2).
    let mut long = leaf(&[b'z'; 220], b"v");
    long.push(0x40);
    let unwritable = file_of(&[&leaves_then(&long)]);
    // A page whose bytes decode as no chunk, as an encrypted file's pages
    // do (file-format.md 6): it stands in for one.
    let mut noise = leaves_then(&[]);
    let mut rng = Rng(5);
    noise.extend((0..2_000).map(|_| rng.next() as u8));
    let undecodable = file_of(&[&noise]);
    // Page 0 the writer does not write: not 4,096 bytes.
    let mut short = file_of(&[&leaves_then(&[0x40])]);
    short.truncate(4096 * 2 + 100);
    for (what, bytes, batch) in [
        ("unwritable, one batch", &unwritable, Batch::default()),
        ("unwritable, five batches first", &unwritable, one),
        ("undecodable", &undecodable, one),
        ("cut short", &short, one),
    ] {
        let mut store = MemStore::new(Numbering::Sequential);
        let kept = TreeName::new(b"kept".to_vec()).unwrap();
        let before = apply_unit(&store, &kept, None, 1, &[Edit::list(vec![vec![1]])]).unwrap();
        let refused = import(Cursor::new(bytes), &mut store, &tree_name(), 2, batch);
        match what {
            "cut short" | "undecodable" => {
                assert!(
                    matches!(refused, Err(ImportError::File(_))),
                    "{what}: {refused:?}"
                )
            }
            _ => assert!(
                matches!(&refused, Err(ImportError::Unwritable { path, .. }) if path == &[vec![b'z'; 220]]),
                "{what}: {refused:?}"
            ),
        }
        assert_eq!(store.trees(), vec![(kept.clone(), before)], "{what}");
    }
    // A tree of that name is never written over.
    let good = file_of(&[&leaves_then(&[0x40])]);
    let (mut store, imported) = import_into_memory(&good, one);
    assert!(matches!(
        import(Cursor::new(&good), &mut store, &tree_name(), 100, one),
        Err(ImportError::Exists)
    ));
    assert_eq!(store.trees(), vec![(tree_name(), imported.root)]);
    // Past the store's bounds (docs/server-storage.md 6.11), which no file
    // the reader reads can be: a value of 64 KiB.
    let value = Node::Value(vec![0; om_store::page::MAX_VALUE + 1]);
    assert!(matches!(
        check(&[vec![1]], &value),
        Err(ImportError::Bounds {
            why: tree::Refusal::ValueTooLong(_),
            ..
        })
    ));
}

/// A list that carries an explicit empty value (#754): the reader gives
/// it so and the file engine keeps it so, and so does the import, so that
/// both views answer it alike. Its export is what the file engine's writer
/// writes, a bare list, and imports again with another root hash.
#[test]
fn a_list_with_an_empty_value_reads_as_the_file_engine_holds_it() {
    let bytes = file_of(&[&[0x80, 0x26, 0x05, 0x00, 0x80, 0x01, 0x01, 0x61, 0x40, 0x40]]);
    let file = HbamFile::from_bytes(bytes.clone()).unwrap();
    let root = file.key_tree().unwrap().root.to_owned_node();
    let page_zero = file.superblock().raw.to_vec();
    let tree_view = crate::engine::view::TreeView::new(&root, 0);
    let (memory, imported) = import_into_memory(&bytes, Batch::default());
    let view = PagedView::open(&memory, imported.root).unwrap();
    let both = Both::new(&tree_view, &view);
    let dynamic: &dyn View = &both;
    assert_eq!(
        dynamic.node(&[&[5]]).unwrap(),
        Some(Head {
            list: true,
            value: Some(Cow::Owned(Vec::new()))
        })
    );
    let steps: Vec<Step> = dynamic.walk(&[]).map(Result::unwrap).collect();
    assert_eq!(steps.len(), 3);
    let scratch = Scratch::new("empty-value");
    let change_number = om_hbam::writer::change_number(&page_zero).unwrap();
    let mut theirs = scratch.create("file.fmp12");
    write_checked(
        &tree_view,
        &page_zero,
        change_number,
        &mut theirs,
        Progress::none(),
    )
    .unwrap();
    let ours = paged_export(&view, &imported.page_zero, &scratch, "paged.fmp12");
    assert!(std::fs::read(scratch.0.join("file.fmp12")).unwrap() == ours);
    let (again_store, again) = import_into_memory(&ours, Batch::default());
    let again_view = PagedView::open(&again_store, again.root).unwrap();
    assert_eq!(
        again_view.node(&[&[5]]).unwrap(),
        Some(Head {
            list: true,
            value: None
        })
    );
    assert_ne!(again.root.hash, imported.root.hash);
}

/// The FileMaker templates (the 66 Mac templates `om-hbam`'s ignored tests
/// read, git-ignored extractions): each read alike through both engines,
/// exported byte for byte as the file engine exports it and imported again
/// to the same root hash, and kept by the page file on the simulated disk
/// across a checkpoint and a reopen, its export the same bytes.
///
/// ```sh
/// cargo test -p om-server --lib -- --ignored filemakers_templates_through_the_store_engine --nocapture
/// ```
#[test]
#[ignore = "requires the git-ignored extracted templates (re/extracted/mac)"]
fn filemakers_templates_through_the_store_engine() {
    const MAC_TEMPLATES: &str = "../../re/extracted/mac/FileMaker Pro 26/FileMaker \
                                 Pro.app/Contents/Resources/DynamicTemplates";
    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the templates are present") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "fmp12") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(MAC_TEMPLATES),
        &mut found,
    );
    found.sort();
    assert_eq!(found.len(), 66);
    let scratch = Scratch::new("templates");
    let (mut nodes, mut calls, mut refused) = (0, 0, Vec::new());
    for (n, path) in found.iter().enumerate() {
        let shown = path.display();
        let bytes = std::fs::read(path).unwrap();
        let store = loaded(&bytes);
        let mut memory = MemStore::new(Numbering::Sequential);
        let imported = match import(
            Cursor::new(&bytes),
            &mut memory,
            &tree_name(),
            1,
            Batch::default(),
        ) {
            Err(e) => {
                assert!(store.is_read_only(), "{shown}: {e}");
                refused.push(format!("{shown}: {e}"));
                continue;
            }
            Ok(imported) => imported,
        };
        let view = PagedView::open(&memory, imported.root).unwrap();
        let compared = compare_reads(&store, &view, &imported.page_zero, n as u64 + 1, 300);
        nodes += compared.nodes;
        calls += compared.calls;
        let theirs = file_engine_export(&store, &scratch, "file.fmp12");
        let ours = paged_export(&view, &imported.page_zero, &scratch, "paged.fmp12");
        assert!(theirs == ours, "{shown}: the exports differ");
        let (_, again) = import_into_memory(&ours, Batch::default());
        assert_eq!(again.root.hash, imported.root.hash, "{shown}");
        // The page file on the simulated disk, across a reopen.
        let sim = Sim::new(n as u64 + 1);
        sim.set_capacity(1 << 33);
        {
            let fs = sim.fs();
            let lock = fs.lock().unwrap();
            let pf = PageFile::create(&fs, &lock, small_config()).unwrap();
            let mut units = OnPageFile {
                pf: &pf,
                checkpoints: 0,
            };
            import(
                Cursor::new(&bytes),
                &mut units,
                &tree_name(),
                1,
                Batch::default(),
            )
            .unwrap();
            units.checkpoint().unwrap();
        }
        sim.restart();
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        let (pf, _) = PageFile::open(&fs, &lock, small_config()).unwrap();
        let pin = pf.pin(pf.checkpoint_entry().1.index).unwrap();
        let root = pin.roots()[0].1;
        // The page file numbers its pages its own way.
        assert_eq!(root.hash, imported.root.hash, "{shown}");
        let on_disk = PagedView::open(&pf, root).unwrap();
        let reopened = paged_export(&on_disk, &imported.page_zero, &scratch, "reopened.fmp12");
        assert!(
            reopened == theirs,
            "{shown}: the page file's export differs"
        );
    }
    println!(
        "{} files, {} refused, {nodes} nodes, {calls} calls compared: {refused:?}",
        found.len(),
        refused.len()
    );
}
