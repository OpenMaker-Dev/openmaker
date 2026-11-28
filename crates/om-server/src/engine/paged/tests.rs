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
    /// The largest cost of a node's entry in an export's index
    /// (`om_hbam::writer::index_entry_cost`), and the path of that node.
    pub index_cost: (usize, Vec<Vec<u8>>),
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
    #[cfg(debug_assertions)]
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
    let index_cost = steps
        .iter()
        .zip(&paths)
        .skip(1)
        .map(|(step, path)| {
            (
                om_hbam::writer::index_entry_cost(path, step.head.list),
                path.clone(),
            )
        })
        .max()
        .unwrap_or_default();
    Compared {
        nodes: steps.len() - 1,
        calls: both.calls.get(),
        index_cost,
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

/// A page store that counts the pages read from it.
struct CountingReads<'s> {
    inner: &'s MemStore,
    reads: Cell<usize>,
}

impl PageStore for CountingReads<'_> {
    type Entry<'a>
        = om_store::page::MemEntry<'a>
    where
        Self: 'a;

    fn read(
        &self,
        number: om_store::page::PageNo,
        hash: &Hash,
    ) -> Result<std::sync::Arc<om_store::page::Page>, om_store::page::Fault> {
        self.reads.set(self.reads.get() + 1);
        self.inner.read(number, hash)
    }

    fn trees(&self) -> Vec<(TreeName, om_store::page::Root)> {
        self.inner.trees()
    }

    fn root(&self, name: &TreeName) -> Option<om_store::page::Root> {
        self.inner.root(name)
    }

    fn begin(&self) -> Result<Self::Entry<'_>, StoreError> {
        self.inner.begin()
    }
}

/// An index of Tasks' Name holding one entry of about fifty RID list nodes
/// (25 KB, over several pages): its path, the entry's key, and its records.
fn indexed_file() -> (
    OwnedNode,
    [&'static [u8]; 4],
    &'static [u8],
    om_hbam::RidList,
) {
    let mut root = fixture::tree(&[], true);
    let index: [&[u8]; 4] = [&[0x80, 0x01], &[1], &[1], &[0]];
    let key: &[u8] = b"\x01ship";
    let posting: om_hbam::RidList = (1..=200_000u64).step_by(3).collect();
    let mut at = index.to_vec();
    at.push(key);
    root.put(&at, posting.to_node(Vec::new()));
    (root, index, key, posting)
}

/// The store engine builds a find's index entry within the work its program
/// has left (`View::subtree_within`), counting each node as the reader
/// counts it (`om_query::program::Size::of_tree`): an entry exactly at the
/// limit in both counters is given; one node or one byte more is refused;
/// and with either counter spent, the other ample, it is refused before
/// its first node is kept, having read no page past the one a head read of
/// it reads. Through a view that keeps failed reads for a server script's
/// find (`Noted`), the refusal is no failed read.
#[test]
fn the_store_engine_refuses_an_entry_at_the_first_node_past_either_counter() {
    use om_query::program::Size;
    let (root, index, key, posting) = indexed_file();
    let (memory, imported) = import_into_memory(&written(&root), SMALL);
    let counting = CountingReads {
        inner: &memory,
        reads: Cell::new(0),
    };
    let view = PagedView::open(&counting, imported.root).unwrap();
    let mut path = index.to_vec();
    path.push(key);
    let exact = Size::of_tree(&posting.to_node(key.to_vec()));
    assert!(exact.nodes > 40, "{exact:?}");
    let given = view.subtree_within(&path, exact).unwrap().unwrap();
    assert_eq!(om_hbam::RidList::from_owned(&given).unwrap(), posting);
    let refused = |work: Size| {
        matches!(
            view.subtree_within(&path, work),
            Err(ReadError::TooLarge { .. })
        )
    };
    for short in [
        Size {
            nodes: exact.nodes - 1,
            ..exact
        },
        Size {
            bytes: exact.bytes - 1,
            ..exact
        },
    ] {
        assert!(refused(short), "{short:?}");
    }
    // What a head read of the entry reads: the seek.
    counting.reads.set(0);
    assert!(view.node(&path).unwrap().is_some());
    let seek = counting.reads.get();
    for spent in [
        Size {
            nodes: usize::MAX,
            bytes: 0,
        },
        Size {
            nodes: 0,
            bytes: usize::MAX,
        },
    ] {
        counting.reads.set(0);
        assert!(refused(spent), "{spent:?}");
        // No more: from the place the last read left, none at all (#767).
        assert!(counting.reads.get() <= seek, "{spent:?}");
    }
    // Read whole by a view that holds no place yet.
    let fresh = PagedView::open(&counting, imported.root).unwrap();
    counting.reads.set(0);
    assert!(fresh.subtree_within(&path, exact).unwrap().is_some());
    assert!(
        counting.reads.get() > seek,
        "the entry spans pages past the seek"
    );
    let noted = crate::engine::Noted::new(&view);
    assert!(matches!(
        noted.subtree_within(&path, Size::default()),
        Err(ReadError::TooLarge { .. })
    ));
    assert!(
        noted.failed().is_ok(),
        "a refusal at the reader's limit is no failed read"
    );
}

/// The file engine lends a find's index entry unchecked
/// (`View::subtree_within`), so that one whose lists hold room past their
/// length is given when its work fits, and its reader's count decides; and
/// each engine, and in a debug build the test view over each, gives the
/// reader the same outcome at, and either side of, the entry's work.
#[test]
fn the_file_engine_lends_an_entry_and_both_engines_answer_the_reader_alike() {
    use om_query::program::Size;
    let (mut root, index, key, posting) = indexed_file();
    let mut path = index.to_vec();
    path.push(key);
    // The entry's list with room for a thousand times its length.
    let entry = root.at_mut(&path).unwrap();
    let mut roomy = Vec::with_capacity(entry.children.len() * 1000);
    roomy.append(&mut entry.children);
    entry.children = roomy;
    let exact = Size::of_tree(&posting.to_node(key.to_vec()));
    let tree = crate::engine::TreeView::new(&root, 1);
    assert!(matches!(
        tree.subtree_within(&path, exact).unwrap(),
        Some(Cow::Borrowed(_))
    ));
    let (memory, imported) = import_into_memory(&written(&root), SMALL);
    let paged = PagedView::open(&memory, imported.root).unwrap();
    let reader = |view: &dyn View, work: Size| match view.subtree_within(&path, work) {
        Err(ReadError::TooLarge { .. }) => "refused",
        Ok(Some(node)) => {
            let size = Size::of_tree(&node);
            match size.nodes <= work.nodes && size.bytes <= work.bytes {
                true => "read",
                false => "refused",
            }
        }
        other => panic!("{other:?}"),
    };
    let at = |nodes: usize, bytes: usize| Size { nodes, bytes };
    for (work, outcome) in [
        (exact, "read"),
        (at(exact.nodes + 1, exact.bytes + 1), "read"),
        (at(exact.nodes - 1, exact.bytes), "refused"),
        (at(exact.nodes, exact.bytes - 1), "refused"),
        (at(exact.nodes, exact.bytes / 2), "refused"),
        (at(usize::MAX, 0), "refused"),
        (at(0, usize::MAX), "refused"),
        (Size::default(), "refused"),
    ] {
        let (file, store) = (reader(&tree, work), reader(&paged, work));
        assert_eq!((file, store), (outcome, outcome), "{work:?}");
        // The test view exists in debug builds only (`engine::copies`).
        #[cfg(debug_assertions)]
        {
            let file = reader(&crate::engine::copies::Copies::new(&tree, &[]), work);
            let store = reader(&crate::engine::copies::Copies::new(&paged, &[]), work);
            assert_eq!((file, store), (outcome, outcome), "test view, {work:?}");
        }
    }
}

/// An index entry read at a key a listing has just given is sought from the
/// listing's place, and the listing's next page from where the last
/// entry's walk ended (#767): listing an index of 3,000 keys in pages of
/// eight and reading each key's entry, as a find's listed index reads them,
/// reads each page of the index at most twice, once as the listing looks
/// past a page of keys and once as the entries' walks reach it, where
/// seeking every entry from the root read a way down the tree for each
/// key; and it gives what the file engine gives.
#[test]
fn a_listing_and_the_entries_it_gives_read_about_the_pages_of_one_walk() {
    use om_hbam::RidList;
    use om_query::program::Size;
    let mut root = fixture::tree(&[], true);
    let index: [&[u8]; 4] = [&[0x80, 0x01], &[1], &[1], &[0]];
    let keys: Vec<Vec<u8>> = (0..3_000u32)
        .map(|n| format!("\x01name {n:05}").into_bytes())
        .collect();
    for (n, key) in (1u64..).zip(&keys) {
        let mut at = index.to_vec();
        at.push(key);
        let posting: RidList = [n, n + 3_000].into_iter().collect();
        root.put(&at, posting.to_node(Vec::new()));
    }
    let (memory, imported) = import_into_memory(&written(&root), SMALL);
    let counting = CountingReads {
        inner: &memory,
        reads: Cell::new(0),
    };
    let height = Tree::open(&memory, imported.root).unwrap().height();
    let read = |view: &dyn View| -> Vec<OwnedNode> {
        let mut entries = Vec::new();
        let mut after: Option<Vec<u8>> = None;
        loop {
            let mut page = Vec::new();
            let from = after.as_deref().map_or(Bound::Unbounded, Bound::Excluded);
            let more = view
                .children_page(&index, from, Direction::Forward, 8, &mut page)
                .unwrap();
            for child in &page {
                let mut at = index.to_vec();
                at.push(&child.segment);
                let entry = view.subtree_within(&at, Size::UNLIMITED).unwrap();
                entries.push(entry.unwrap().into_owned());
            }
            after = page.last().map(|child| child.segment.to_vec());
            if !more || after.is_none() {
                return entries;
            }
        }
    };
    let listed = read(&PagedView::open(&counting, imported.root).unwrap());
    let reads = counting.reads.replace(0);
    assert_eq!(listed.len(), keys.len());
    assert_eq!(listed, read(&crate::engine::TreeView::new(&root, 1)));
    let view = PagedView::open(&counting, imported.root).unwrap();
    counting.reads.set(0);
    view.subtree(&index, NO_LIMIT).unwrap().unwrap();
    let walk = counting.reads.get();
    assert!(walk > 10, "{walk} pages: the index should span many leaves");
    assert!(
        reads <= 2 * walk + height,
        "{reads} pages read; a walk {walk}, height {height}"
    );
}

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
            "{name}: {} nodes, {} entries, {} calls compared, largest index entry {} bytes",
            imported.nodes,
            imported.entries.1 - imported.entries.0 + 1,
            compared.calls,
            compared.index_cost.0
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
    assert!(read < walked.len(), "{read} of {}", walked.len());
    // The walk gave whole pages before the one that reaches the damaged
    // page and nothing of that one: a node it did not give is on the
    // damaged page.
    let reached = walked[read..]
        .iter()
        .find(|path| matches!(dynamic.node(&refs(path)), Err(ReadError::Unreadable { .. })));
    assert!(
        reached.is_some(),
        "no node after the {read} given is unreadable"
    );
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
    let left = scratch.names().iter().any(|n| n == "damaged.fmp12");
    if cfg!(unix) {
        assert!(
            matches!(
                refused,
                Err(ExportError::Read(ReadError::Unreadable { .. }))
            ),
            "{refused:?}"
        );
        // Made unnamed (Linux with unnamed files), nothing is under its
        // name; made under its name, what was written stays there
        // (`crate::staged`), as for every export.
        assert_eq!(left, !unnamed_files(&folder), "{:?}", scratch.names());
    } else {
        // No folder descriptors: an export to a folder is unsupported.
        assert!(matches!(refused, Err(ExportError::Io(_))), "{refused:?}");
        assert!(!left);
    }
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

/// Whether exports into `folder` are made unnamed and linked to their
/// name once complete (`crate::staged`): only then does a failed export
/// leave nothing under its name.
fn unnamed_files(folder: &crate::anchored::Folder) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::staged::Staged::unnamed(folder.descriptor(), OsStr::new("probe")).is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = folder;
        false
    }
}

/// What an export would give back in another shape is refused, with its
/// path and nothing left: a list that carries an explicit empty value
/// (#754), which the file engine keeps in memory and its writer writes as a
/// bare list, and a value over 1,000 bytes held whole, which the writer
/// writes in pieces.
#[test]
fn what_an_export_would_reshape_is_refused() {
    let empty_value = file_of(&[&[0x80, 0x26, 0x05, 0x00, 0x80, 0x01, 0x01, 0x61, 0x40, 0x40]]);
    // The file engine holds it as the reader gives it.
    let file = HbamFile::from_bytes(empty_value.clone()).unwrap();
    let root = file.key_tree().unwrap().root.to_owned_node();
    assert_eq!(root.at(&[&[5]]).unwrap().value.as_deref(), Some(&[][..]));
    let mut whole = leaves_then(&leaf(b"v", &long_value(1001)));
    whole.push(0x40);
    let whole = file_of(&[&whole]);
    let mut at_the_limit = leaves_then(&leaf(b"v", &long_value(1000)));
    at_the_limit.push(0x40);
    let at_the_limit = file_of(&[&at_the_limit]);
    import_into_memory(&at_the_limit, Batch::default());
    for (bytes, path) in [(&empty_value, vec![vec![5]]), (&whole, vec![b"v".to_vec()])] {
        let mut store = MemStore::new(Numbering::Sequential);
        let refused = import(
            Cursor::new(bytes),
            &mut store,
            &tree_name(),
            1,
            Batch::default(),
        );
        assert!(
            matches!(&refused, Err(ImportError::Unwritable { path: p, .. }) if *p == path),
            "{refused:?}"
        );
        assert!(store.trees().is_empty());
    }
}

/// Page 0 as an export needs it: a change number at its largest, or with
/// less room than an export's pages take, is refused before anything is
/// written; one with just that room imports, and exports.
#[test]
fn a_page_zero_an_export_could_not_number_from_is_refused() {
    let mut body = leaves_then(&[]);
    body.push(0x40);
    let with = |scn: u64| {
        let mut bytes = file_of(&[&body]);
        bytes[0x802..0x80a].copy_from_slice(&scn.to_be_bytes());
        bytes
    };
    let room = u64::MAX - u64::from(u32::MAX);
    for scn in [u64::MAX, room + 1] {
        let mut store = MemStore::new(Numbering::Sequential);
        let refused = import(
            Cursor::new(&with(scn)),
            &mut store,
            &tree_name(),
            1,
            Batch::default(),
        );
        assert!(
            matches!(&refused, Err(ImportError::Unwritable { path, .. }) if path.is_empty()),
            "{scn}: {refused:?}"
        );
        assert!(store.trees().is_empty());
    }
    let (store, imported) = import_into_memory(&with(room), Batch::default());
    let view = PagedView::open(&store, imported.root).unwrap();
    let scratch = Scratch::new("change-number");
    let exported = paged_export(&view, &imported.page_zero, &scratch, "room.fmp12");
    // The export numbered its pages on from there: imported again, it is
    // refused, as the rule's boundary says (`import`, "What import
    // accepts, export gives back").
    let advanced = om_hbam::writer::change_number(&exported).unwrap();
    assert!(advanced > room, "{advanced}");
    let mut again = MemStore::new(Numbering::Sequential);
    let refused = import(
        Cursor::new(&exported),
        &mut again,
        &tree_name(),
        1,
        Batch::default(),
    );
    assert!(
        matches!(&refused, Err(ImportError::Unwritable { path, .. }) if path.is_empty()),
        "{refused:?}"
    );
}

/// The entries an import may take are worked out before anything is
/// written: from the last index an entry can have back, a file is refused
/// whole, never with a panic or a number that wraps; entry 0 is no entry.
#[test]
fn an_import_whose_entries_could_not_be_numbered_is_refused() {
    let mut body = leaves_then(&[]);
    body.push(0x40);
    let bytes = file_of(&[&body]);
    // Three pages of 4,096 bytes, and the two entries more.
    let needed = 3 * 4096 + 2;
    let one = Batch {
        nodes: 1,
        bytes: 1 << 20,
    };
    for first in [0, u64::MAX, u64::MAX - needed + 1] {
        let mut store = MemStore::new(Numbering::Sequential);
        let refused = import(Cursor::new(&bytes), &mut store, &tree_name(), first, one);
        assert!(
            matches!(refused, Err(ImportError::Entries { first: f, needed: n }) if f == first && n == needed),
            "{first}: {refused:?}"
        );
        assert!(store.trees().is_empty());
    }
    let mut store = MemStore::new(Numbering::Sequential);
    let first = u64::MAX - needed;
    let imported = import(Cursor::new(&bytes), &mut store, &tree_name(), first, one).unwrap();
    assert_eq!(imported.entries, (first, first + 4));
}

/// Where an import's batches go, recorded: each one's nodes and bytes.
struct Recording {
    store: MemStore,
    batches: Vec<(usize, usize)>,
}

impl Units for Recording {
    fn has_tree(&self, tree: &TreeName) -> bool {
        self.store.has_tree(tree)
    }

    fn apply(
        &mut self,
        tree: &TreeName,
        root: Option<Root>,
        entry: u64,
        edits: &[Edit],
    ) -> Result<Root, tree::Error> {
        let bytes = edits
            .iter()
            .map(|edit| match edit {
                Edit::Put { path, node } => key::encoded_len(path) + node.value().len(),
                Edit::Delete { path } => key::encoded_len(path),
            })
            .sum();
        self.batches.push((edits.len(), bytes));
        self.store.apply(tree, root, entry, edits)
    }

    fn drop_tree(&mut self, tree: &TreeName, entry: u64) -> Result<(), tree::Error> {
        self.store.drop_tree(tree, entry)
    }
}

/// A batch never passes its limits: the byte limit is checked before a
/// node joins it, and a node larger than the limit alone is a batch of its
/// own.
#[test]
fn a_batch_keeps_within_its_limits() {
    let mut body = leaves_then(&[]);
    body.push(0x40);
    let bytes = file_of(&[&body]);
    // Five leaves, each of a key `x` (3 bytes encoded) and 2 bytes.
    for (limit, expected) in [
        (10, vec![(2, 10), (2, 10), (1, 5)]),
        (7, vec![(1, 5); 5]),
        (3, vec![(1, 5); 5]),
    ] {
        let mut units = Recording {
            store: MemStore::new(Numbering::Sequential),
            batches: Vec::new(),
        };
        let batch = Batch {
            nodes: 4096,
            bytes: limit,
        };
        import(Cursor::new(&bytes), &mut units, &tree_name(), 1, batch).unwrap();
        assert_eq!(units.batches, expected, "a limit of {limit} bytes");
    }
    let mut units = Recording {
        store: MemStore::new(Numbering::Sequential),
        batches: Vec::new(),
    };
    let batch = Batch {
        nodes: 2,
        bytes: 1 << 20,
    };
    import(Cursor::new(&bytes), &mut units, &tree_name(), 1, batch).unwrap();
    assert_eq!(units.batches, vec![(2, 10), (2, 10), (1, 5)]);
}

/// A listing of children in pages, each from just past the last child the
/// page before gave, as a client pages them: the segments given, and the
/// error that ended it, if one did. `page` reads one page from a bound
/// onto its segments. A page that gives nothing and says more follow fails
/// the test: a reader ends there, and what follows is lost.
fn in_pages(
    mut page: impl FnMut(Bound<&[u8]>, &mut Vec<Vec<u8>>) -> Result<bool, ReadError>,
) -> (Vec<Vec<u8>>, Option<ReadError>) {
    let mut given: Vec<Vec<u8>> = Vec::new();
    loop {
        let mut got = Vec::new();
        let from = given
            .last()
            .map_or(Bound::Unbounded, |s| Bound::Excluded(s.as_slice()));
        match page(from, &mut got) {
            Err(e) => return (given, Some(e)),
            Ok(more) => {
                assert!(
                    !(more && got.is_empty()),
                    "an empty page said more follow after {given:02x?}"
                );
                given.extend(got);
                assert!(given.len() <= 100, "a listing that does not end");
                if !more {
                    return (given, None);
                }
            }
        }
    }
}

/// [`in_pages`] through [`View::children_page`], pages of `max`.
fn children_in_pages(
    view: &dyn View,
    path: &[&[u8]],
    max: usize,
) -> (Vec<Vec<u8>>, Option<ReadError>) {
    in_pages(|from, out| {
        let mut page = Vec::new();
        let more = view.children_page(path, from, Direction::Forward, max, &mut page)?;
        out.extend(page.iter().map(|child| child.segment.to_vec()));
        Ok(more)
    })
}

/// [`in_pages`] through [`View::subtrees_page`], pages of `max`.
fn subtrees_in_pages(
    view: &dyn View,
    path: &[&[u8]],
    max: usize,
) -> (Vec<Vec<u8>>, Option<ReadError>) {
    in_pages(|from, out| {
        let mut page = Vec::new();
        let more = view.subtrees_page(path, from, Direction::Forward, max, NO_LIMIT, &mut page)?;
        out.extend(page.iter().map(|node| node.segment.clone()));
        Ok(more)
    })
}

/// The look past a full page asks only whether more follow, and reads no
/// child's value (the module doc, "A read that fails partway"; #1200):
/// here the next child's value is on an overflow page that does not check.
/// An exact lookup of the key before it, a page of one from that key as a
/// find reads a field's index (`om_query`'s `listed_get`, through the
/// server's `ViewListing`), answers that key. Listed in pages of one,
/// children or subtrees, the child before it comes and the page says more
/// follow, and the next page, which reads the value, fails: nothing given
/// twice, nothing left out without an error. A page that holds the child
/// fails, the child before it in the page with it: that child was asked
/// for. And a read of the value that fails once fails the page that gives
/// the child, not the look, which does not read it. Backward the look finds
/// the next child from the last node under it, and does not seek the
/// child's own node, which the child's read seeks from there: with a node
/// under `a`, and `a`'s own value on an overflow page whose read fails
/// once, a page of one from the end gives `b` and says more follow, the
/// next page fails at `a`'s value, and the page after it gives `a`.
#[test]
fn a_look_ahead_reads_no_value_and_the_page_that_gives_it_fails() {
    let list = vec![b"l".to_vec()];
    let at = refs(&list);
    let under = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        list.iter()
            .cloned()
            .chain(segments.iter().map(|s| s.to_vec()))
            .collect()
    };
    // The list `l` and the nodes `edits` put under it; the store, the root
    // and the tree's overflow pages.
    let tree = |edits: &[Edit]| {
        let store = MemStore::new(Numbering::Sequential);
        let mut unit = vec![Edit::list(list.clone())];
        unit.extend_from_slice(edits);
        let root = apply_unit(&store, &tree_name(), None, 1, &unit).unwrap();
        let overflow: Vec<u64> = om_store::tree::check(&store, root)
            .unwrap()
            .pages
            .iter()
            .filter(|(n, h)| store.read(*n, h).unwrap().kind() == PageKind::Overflow)
            .map(|(n, _)| *n)
            .collect();
        assert!(!overflow.is_empty());
        (store, root, overflow)
    };
    // The children `a` and `b`, `b`'s value on overflow pages.
    let b_spills = [
        Edit::leaf(under(&[b"a"]), b"a".to_vec()),
        Edit::leaf(under(&[b"b"]), long_value(6000)),
    ];
    let failed = |read: &Option<ReadError>| matches!(read, Some(ReadError::Unreadable { .. }));
    let (store, root, overflow) = tree(&b_spills);
    for &page in &overflow {
        store.fail_reads(page, 1);
    }
    let view = PagedView::open(&store, root).unwrap();
    let (given, error) = children_in_pages(&view, &at, 1);
    assert_eq!(given, [b"a".to_vec()]);
    assert!(failed(&error), "{error:?}");
    let (store, root, overflow) = tree(&b_spills);
    for page in overflow {
        assert!(store.damage(page, 100, 0xff));
    }
    let view = PagedView::open(&store, root).unwrap();
    let Some(om_query::Children::Listed(listing)) =
        crate::connection::ViewListing::of(&view, &at).unwrap()
    else {
        panic!("a view's node is listed");
    };
    let mut keys = Vec::new();
    let looked_up = listing.keys(
        Bound::Included(b"a"),
        om_query::Direction::Forward,
        1,
        &mut keys,
    );
    assert_eq!(looked_up.ok(), Some(true));
    assert_eq!(keys, [&b"a"[..]]);
    let (given, error) = children_in_pages(&view, &at, 1);
    assert_eq!(given, [b"a".to_vec()]);
    assert!(failed(&error), "{error:?}");
    let (given, error) = subtrees_in_pages(&view, &at, 1);
    assert_eq!(given, [b"a".to_vec()]);
    assert!(failed(&error), "{error:?}");
    let mut both = Vec::new();
    let page = view.children_page(&at, Bound::Unbounded, Direction::Forward, 5, &mut both);
    assert!(
        matches!(page, Err(ReadError::Unreadable { .. })),
        "{page:?}"
    );
    let mut whole = Vec::new();
    let page = view.subtrees_page(
        &at,
        Bound::Unbounded,
        Direction::Forward,
        5,
        NO_LIMIT,
        &mut whole,
    );
    assert!(
        matches!(page, Err(ReadError::Unreadable { .. })),
        "{page:?}"
    );
    // Backward: `a` with its value on an overflow page and the node `x`
    // under it, then `b`; the overflow page's next read fails, once.
    let (store, root, overflow) = tree(&[
        Edit::leaf(under(&[b"a"]), long_value(6000)),
        Edit::leaf(under(&[b"a", b"x"]), b"x".to_vec()),
        Edit::leaf(under(&[b"b"]), b"b".to_vec()),
    ]);
    let [page] = overflow[..] else {
        panic!("`a`'s value on one overflow page: {overflow:?}");
    };
    store.fail_reads(page, 1);
    let view = PagedView::open(&store, root).unwrap();
    let backward = |from: Bound<&[u8]>| {
        let mut page = Vec::new();
        let more = view.children_page(&at, from, Direction::Backward, 1, &mut page);
        let given: Vec<Vec<u8>> = page.iter().map(|child| child.segment.to_vec()).collect();
        (given, more)
    };
    // The look past `b` finds `a` through `x` and reads neither `a`'s node
    // nor its value: the failing read is left.
    assert_eq!(backward(Bound::Unbounded), (vec![b"b".to_vec()], Ok(true)));
    // The next page seeks `a`'s own node from `x` and reads its value.
    let (given, more) = backward(Bound::Excluded(b"b"));
    assert!(
        matches!(&more, Err(ReadError::Unreadable { why, .. }) if why.contains("unreadable")),
        "{given:02x?}, {more:?}"
    );
    // The read failed once: the page after it gives `a`, and nothing
    // follows.
    assert_eq!(
        backward(Bound::Excluded(b"b")),
        (vec![b"a".to_vec()], Ok(false))
    );
}

/// A look past a full page that cannot read a page leaves the failure to
/// the next page (#1200; `met_again`), for each way a page fails to read:
/// damaged (its checksum), missing, unreadable (an I/O error that stays),
/// misplaced (another page's bytes at its number) and stale (another
/// version of it). The page is the leaf past the last child given, which
/// holds the next child, or the overflow page of the next child's key,
/// which spills and shares its first 1,024 bytes with the bound the look
/// seeks. Listed in pages, the children before it come whole, and the
/// next page fails before it gives anything: no page is empty with more
/// to follow, no child given twice or left out. An I/O error that fails
/// the look's read alone leaves the next page to give the child the look
/// went for, and the rest.
#[test]
fn a_look_ahead_that_cannot_read_a_page_leaves_it_to_the_next_page() {
    use om_store::tree::{built, key};
    #[derive(Clone, Copy, Debug)]
    enum Fails {
        Damaged,
        Missing,
        Io,
        Misplaced,
        Stale,
        IoOnce,
    }
    let item = |key: Vec<u8>, value: &[u8]| (built::KIND_VALUE, key, value.to_vec());
    let one = |segment: &[u8]| key::encode(&[segment]);
    let long = vec![b'p'; 5000];
    let under = |segment: &[u8]| key::encode(&[long.as_slice(), segment]);
    // The tree, with `value` for the children past the page given; its
    // root, the page the look reads, and another page of it.
    let build = |spilled: bool, store: &MemStore, value: &[u8]| {
        if spilled {
            // One leaf: `long`, and its children `a` and `b`, every key
            // spilled to one overflow page.
            let leaves = [vec![
                item(one(long.as_slice()), b"p"),
                item(under(b"a"), b"a"),
                item(under(b"b"), value),
            ]];
            let built = built::tree(store, &leaves, &[]);
            assert!(built.overflow.iter().all(|pages| pages.len() == 1));
            (built.root, built.overflow[2][0], built.overflow[1][0])
        } else {
            // The root's children `a`, `b` in one leaf, `c`, `d` in the
            // next.
            let leaves = [
                vec![item(one(b"a"), b"v"), item(one(b"b"), b"v")],
                vec![item(one(b"c"), value), item(one(b"d"), value)],
            ];
            let built = built::tree(store, &leaves, &[one(b"c").as_slice()]);
            let top = store.read(built.root.page, &built.root.hash).unwrap();
            (built.root, top.references()[1].0, built.root.page)
        }
    };
    let all = |names: &[&[u8]]| names.iter().map(|n| n.to_vec()).collect::<Vec<_>>();
    for (spilled, path, max, children, before) in [
        (false, vec![], 2, all(&[b"a", b"b", b"c", b"d"]), 2),
        (true, vec![long.clone()], 1, all(&[b"a", b"b"]), 1),
    ] {
        for fails in [
            Fails::Damaged,
            Fails::Missing,
            Fails::Io,
            Fails::Misplaced,
            Fails::Stale,
            Fails::IoOnce,
        ] {
            let case = format!("{fails:?}, spilled {spilled}");
            let store = MemStore::new(Numbering::Sequential);
            let (root, page, other) = build(spilled, &store, b"v");
            let why = match fails {
                Fails::Damaged => {
                    assert!(store.damage(page, 100, 0xff));
                    "damaged"
                }
                Fails::Missing => {
                    assert!(store.remove(page));
                    "missing"
                }
                Fails::Io => {
                    store.fail_reads(page, u32::MAX);
                    "unreadable"
                }
                Fails::IoOnce => {
                    store.fail_reads(page, 1);
                    "unreadable"
                }
                Fails::Misplaced => {
                    store.put_bytes(page, &store.bytes(other).unwrap());
                    "misplaced"
                }
                Fails::Stale => {
                    let again = MemStore::new(Numbering::Sequential);
                    let (_, same, _) = build(spilled, &again, b"w");
                    assert_eq!(same, page, "{case}");
                    store.put_bytes(page, &again.bytes(page).unwrap());
                    "stale"
                }
            };
            let view = PagedView::open(&store, root).unwrap();
            let (given, error) = children_in_pages(&view, &refs(&path), max);
            if let Fails::IoOnce = fails {
                assert_eq!(given, children, "{case}");
                assert!(error.is_none(), "{case}: {error:?}");
                continue;
            }
            assert_eq!(given, children[..before], "{case}");
            assert!(
                matches!(&error, Some(ReadError::Unreadable { why: w, .. }) if w.contains(why)),
                "{case}: {error:?}"
            );
        }
    }
}

/// On the page file, an import refused after it committed batches (and
/// checkpoints) leaves no page in use and none dying: once its pages have
/// settled and two checkpoints have passed, as the page file's rules keep
/// them until then, the pages in use are what they were before it. (`MemStore`, a store for tests, keeps every page it was
/// given by design.)
#[test]
fn a_refused_import_leaves_no_page_in_use_on_the_page_file() {
    let mut long = leaf(&[b'z'; 220], b"v");
    long.push(0x40);
    let (_, good) = fixture_files().swap_remove(1);
    let sim = Sim::new(3);
    let fs = sim.fs();
    let lock = fs.lock().unwrap();
    let pf = PageFile::create(&fs, &lock, small_config()).unwrap();
    let mut units = OnPageFile {
        pf: &pf,
        checkpoints: 0,
    };
    let kept = TreeName::new(b"file:2".to_vec()).unwrap();
    import(Cursor::new(&good), &mut units, &kept, 1, SMALL).unwrap();
    // Pages a map on disk recorded unsettled are reused two checkpoints
    // after they settle (docs/server-storage-format.md 4.5, "Holds"):
    // four rounds of a checkpoint and a reclaim settle everything here.
    // The pages in use: those neither free nor dying.
    let settle = |units: &mut OnPageFile<'_>| {
        for _ in 0..4 {
            units.checkpoint().unwrap();
            units.pf.reclaim(usize::MAX);
        }
        let space = units.pf.space();
        (space.end - 1 - space.free - space.dying, space.dying)
    };
    let before = settle(&mut units);
    // Two hundred batches of one leaf, then a key the writer cannot write.
    // Leaves of 1,000 bytes, three a page: the root list stays open across
    // the pages, and the tree passes the page file's budget of 8 pages
    // waiting for a checkpoint.
    let mut pages = vec![leaves_then(&[])];
    for key in 0..200u16 {
        if key % 3 == 0 {
            pages.push(Vec::new());
        }
        let page = pages.last_mut().expect("a page");
        page.extend(leaf(
            &[b'k', (key >> 8) as u8, key as u8],
            &long_value(1000),
        ));
    }
    pages.last_mut().expect("a page").extend(long);
    let bodies: Vec<&[u8]> = pages.iter().map(Vec::as_slice).collect();
    let refused_file = file_of(&bodies);
    let checkpoints = units.checkpoints;
    let first = units.pf.newest_entry() + 1;
    let one = Batch {
        nodes: 1,
        bytes: 1 << 20,
    };
    let refused = import(
        Cursor::new(&refused_file),
        &mut units,
        &tree_name(),
        first,
        one,
    );
    assert!(
        matches!(refused, Err(ImportError::Unwritable { .. })),
        "{refused:?}"
    );
    assert!(
        units.checkpoints > checkpoints,
        "the import checkpointed along the way"
    );
    assert_eq!(units.pf.tree_names(), vec![kept]);
    let after = settle(&mut units);
    assert_eq!(after, before, "pages in use and dying");
    println!(
        "pages in use and dying, before the refused import and after it settled: {before:?}, \
         {after:?}"
    );
}

/// A tree of random shape: lists empty and full, values empty, short and
/// long (which the writer writes in pieces), segments with zero and `ff`
/// bytes, as the writer writes them.
fn random_tree(rng: &mut Rng, depth: usize) -> Vec<OwnedNode> {
    let mut segments = std::collections::BTreeSet::new();
    let width = match rng.below(8) {
        0 => 0,
        1 if depth < 2 => 40 + rng.below(80),
        _ => 1 + rng.below(6),
    };
    for _ in 0..width {
        let len = 1 + rng.below(4);
        let segment: Vec<u8> = (0..len)
            .map(|_| match rng.below(4) {
                0 => 0,
                1 => 0xff,
                _ => rng.next() as u8,
            })
            .collect();
        segments.insert(segment);
    }
    segments
        .into_iter()
        .map(|segment| {
            if depth < 5 && rng.below(3) == 0 {
                OwnedNode::list(segment, random_tree(rng, depth + 1))
            } else {
                let len = match rng.below(6) {
                    0 => 0,
                    1 => 1001 + rng.below(3000),
                    _ => rng.below(60),
                };
                OwnedNode::leaf(
                    segment,
                    (0..len).map(|_| rng.next() as u8).collect::<Vec<_>>(),
                )
            }
        })
        .collect()
}

/// What import accepts, export gives back: for random trees as the writer
/// writes them, in random batches, the import's export imported again in
/// the same batches has the same root hash (as every fixture file and
/// FileMaker template does in the tests above).
#[test]
fn an_export_of_what_import_accepts_imports_to_the_same_root_hash() {
    let scratch = Scratch::new("round-trip");
    let mut nodes = 0;
    for seed in 1..=40u64 {
        let mut rng = Rng(seed * 0x9e37_79b9);
        let root = OwnedNode::list(Vec::new(), random_tree(&mut rng, 0));
        let bytes =
            om_hbam::writer::rebuild(&root, &om_hbam::new_superblock("Random").unwrap()).unwrap();
        let batch = Batch {
            nodes: 1 + rng.below(64),
            bytes: 1 + rng.below(8_000),
        };
        let (store, imported) = import_into_memory(&bytes, batch);
        let view = PagedView::open(&store, imported.root).unwrap();
        let exported = paged_export(&view, &imported.page_zero, &scratch, "export.fmp12");
        let (again_store, again) = import_into_memory(&exported, batch);
        assert_eq!(again.root.hash, imported.root.hash, "seed {seed}");
        assert_eq!(again.nodes, imported.nodes, "seed {seed}");
        assert_eq!(
            items(&again_store, again.root),
            items(&store, imported.root),
            "seed {seed}"
        );
        // At another first entry and in other batches the nodes are the
        // same, kinds and values included, though the root hash need not be.
        let mut other = MemStore::new(Numbering::Scattered(seed));
        let elsewhere = import(
            Cursor::new(&exported),
            &mut other,
            &tree_name(),
            1_000 + seed,
            Batch::default(),
        )
        .unwrap();
        assert_eq!(
            items(&other, elsewhere.root),
            items(&store, imported.root),
            "seed {seed}"
        );
        nodes += imported.nodes;
    }
    println!("{nodes} nodes in 40 trees");
}

/// Every node of the tree at `root`, with its path and kind.
fn items(store: &MemStore, root: Root) -> Vec<(Vec<Vec<u8>>, Node)> {
    Tree::open(store, root)
        .unwrap()
        .walk::<Vec<u8>>(&[])
        .map(|item| {
            let item = item.unwrap();
            (item.key.path(), item.node)
        })
        .collect()
}

/// Sol's construction (review round 2): `branches` top-level lists, each a
/// chain of 15 lists of 219-byte segments holding two 700-byte leaves at
/// depth 16, laid out as the file writer lays it out: a page opening the
/// chain and its first leaf, a page pushing the chain again for the second
/// leaf and closing it. Every node passes the writer's own checks, but the
/// first key of each second page costs 3,321 bytes in the index, so each
/// index page holds one or two entries.
fn deep_branches(branches: u8) -> Vec<u8> {
    let push = |segment: &[u8]| {
        let code = om_hbam::slot::minimal_code(true, segment.len(), None).unwrap();
        om_hbam::slot::encode_chunk(code, segment, &[]).unwrap()
    };
    let mut pages = Vec::new();
    for branch in 0..branches {
        let segments: Vec<Vec<u8>> = (0..15)
            .map(|depth| vec![if depth == 0 { 0x41 + branch } else { 0x61 }; 219])
            .collect();
        let mut first = Vec::new();
        if branch == 0 {
            first.push(0x80);
        }
        for segment in &segments {
            first.extend(push(segment));
            first.push(0x80);
        }
        first.extend(leaf(&[1], &long_value(700)));
        let mut second = Vec::new();
        for segment in &segments {
            second.extend(push(segment));
        }
        second.extend(leaf(&[2], &long_value(700)));
        second.extend([0x40; 15]);
        if branch + 1 == branches {
            second.push(0x40);
        }
        pages.push(first);
        pages.push(second);
    }
    let bodies: Vec<&[u8]> = pages.iter().map(Vec::as_slice).collect();
    file_of(&bodies)
}

/// A file the reader reads and whose every node the writer can write, but
/// whose index the writer cannot build within 16 levels (Sol's construction
/// in review round 2), is refused at import, at the first node whose index
/// entry could cost more than `om_hbam::writer::MAX_INDEX_ENTRY`, so that
/// nothing imported has an export that fails for its index.
#[test]
fn a_file_whose_export_index_could_pass_16_levels_is_refused() {
    let bytes = deep_branches(18);
    // The file engine's tree of it cannot be written: its index grows past
    // 16 levels.
    let file = HbamFile::from_bytes(bytes.clone()).unwrap();
    let tree = file.key_tree().unwrap().root.to_owned_node();
    assert!(om_hbam::writer::unwritable_in_tree(&tree, 1).is_empty());
    let rebuilt = om_hbam::writer::rebuild(&tree, file.superblock().raw);
    assert!(
        matches!(&rebuilt, Err(om_hbam::Error::Write(why)) if why.contains("16 levels")),
        "{:?}",
        rebuilt.as_ref().err()
    );
    let mut store = MemStore::new(Numbering::Sequential);
    let refused = import(
        Cursor::new(&bytes),
        &mut store,
        &tree_name(),
        1,
        Batch::default(),
    );
    // Four 219-byte lists cost 891 bytes; the fifth passes 1,019.
    let fifth: Vec<Vec<u8>> = std::iter::once(vec![0x41; 219])
        .chain(std::iter::repeat_n(vec![0x61; 219], 4))
        .collect();
    assert!(
        matches!(&refused, Err(ImportError::Unwritable { path, .. }) if *path == fifth),
        "{refused:?}"
    );
    assert!(store.trees().is_empty());
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
    let mut largest = (0, String::new());
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
        if compared.index_cost.0 > largest.0 {
            largest = (
                compared.index_cost.0,
                format!("{shown}, depth {}", compared.index_cost.1.len()),
            );
        }
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
        "{} files, {} refused, {nodes} nodes, {calls} calls compared: {refused:?}; largest \
         index entry {} bytes ({})",
        found.len(),
        refused.len(),
        largest.0,
        largest.1
    );
}

/// The reads of the `index-reads` step of [`paged_measurement`] over the
/// index at `index`, timed: its keys listed, its entries read by key, the
/// upkeep's check through a find's listing, and one walk of it.
#[cfg(target_os = "linux")]
fn index_reads(view: &dyn View, index: &[&[u8]]) -> String {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use om_hbam::RidList;
    let ms = |started: Instant| started.elapsed().as_secs_f64() * 1e3;
    let started = Instant::now();
    let keys: Vec<Vec<u8>> = view
        .children(index, Bound::Unbounded, Direction::Forward)
        .map(|child| child.unwrap().segment.into_owned())
        .collect();
    let listed = ms(started);
    let started = Instant::now();
    let mut built = BTreeMap::new();
    for key in &keys {
        let mut path = index.to_vec();
        path.push(key);
        let entry = view.subtree(&path, NO_LIMIT).unwrap().unwrap();
        built.insert(key.clone(), RidList::from_owned(&entry).unwrap());
    }
    let read = ms(started);
    let Some(om_query::Children::Listed(listing)) =
        crate::connection::ViewListing::of(view, index).unwrap()
    else {
        panic!("a view lists an index");
    };
    let started = Instant::now();
    let agrees =
        om_query::upkeep::holds_exactly(listing.as_ref(), &built, |n| RidList::from_owned(n).ok());
    assert!(agrees.unwrap());
    let checked = ms(started);
    let started = Instant::now();
    let walked = view.walk(index).map(Result::unwrap).count();
    let walk = ms(started);
    format!(
        "{} keys listed {listed:.0} ms, their entries read by key {read:.0} ms, the check \
         through the listing {checked:.0} ms, a walk of the index {walk:.0} ms ({walked} nodes)",
        keys.len()
    )
}

/// A measurement, not a test: the store engine's import, export and reads
/// of a generated file against the file engine's, one step per process so
/// that each peak is its own. The file is one `export_measurement`
/// generates (`engine::export`: one table of `OM_EXPORT_RECORDS` records,
/// eight 64-byte text cells each). Every step reports its time and the
/// peak resident memory above the process's own before it began (`VmHWM`,
/// reset through `clear_refs`). For a release build, one run at a time:
///
/// ```sh
/// OM_PAGED_FILE=target/m/f.fmp12 OM_PAGED_STORE=target/m/store OM_PAGED_STEP=import \
///   cargo test --release -p om-server --lib -- --ignored paged_measurement --nocapture
/// ```
///
/// The steps: `import`, the file into a new page file at `OM_PAGED_STORE`
/// on the disk (a cache of `OM_PAGED_CACHE_MIB`, 64 MiB unless set, at most
/// 4,096 pages waiting for a checkpoint), checkpointed, with the page
/// file's size; `export`, the page file opened, its checkpoint pinned and
/// the tree exported beside the file; `reads`, the same opened view's walk
/// of the table's records and 1,000 record subtrees read at seeded places,
/// twice (the first pass reads the pages, the second finds what the cache
/// kept), then the table's records listed; `mem-reads`, the file imported into a store in memory first,
/// then the same reads twice, through `MemStore::new` (which checks each
/// page's hash on its first read) or with `OM_PAGED_TRUSTING=1` through
/// `MemStore::trusting` (which checks none); `file-reads`, the file
/// engine's load and the same reads and listing through its view; `file-export`, the
/// file engine's load and its written export of the same file;
/// `index-reads`, the opened view's reads of Name's value index, twice:
/// every key listed (heads only, a page at a time), every entry read by
/// its key (as a find reads the entries it takes), the upkeep's check of
/// the index through a find's listing (`om_query::upkeep::holds_exactly`:
/// both), and one walk of the whole index (what a reader that kept its
/// place would read); `file-index-reads`, the same through the file
/// engine's view.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "a measurement for release builds; set OM_PAGED_FILE, OM_PAGED_STORE and OM_PAGED_STEP"]
fn paged_measurement() {
    use std::time::Instant;

    use om_store::os::OsFs;

    fn var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }
    /// The field `name` of this process's status, in MB.
    fn status(name: &str) -> f64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<f64>().ok())
            .unwrap()
            / 1024.0
    }
    let cache_mib: u64 = std::env::var("OM_PAGED_CACHE_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let config = Config {
        cache_bytes: cache_mib << 20,
        dirty_budget: 4096,
        reserve: 256,
        growth: 256,
    };
    let file = PathBuf::from(var("OM_PAGED_FILE"));
    let folder = PathBuf::from(var("OM_PAGED_STORE"));
    let step = var("OM_PAGED_STEP");
    let size = std::fs::metadata(&file).unwrap().len() as f64 / 1e6;
    let page_zero_at = folder.with_extension("page0");
    let table = [om_hbam::slot::uint_segment(fixture::TABLE), vec![5]];
    let table: Vec<&[u8]> = refs(&table);
    /// The whole-table walk and 1,000 record reads: their times in ms,
    /// and the nodes walked.
    fn reads(view: &dyn View, table: &[&[u8]]) -> (f64, f64, usize) {
        let started = Instant::now();
        let walked = view.walk(table).map(Result::unwrap).count();
        let walk = started.elapsed().as_secs_f64() * 1e3;
        let records = view
            .children(table, Bound::Unbounded, Direction::Backward)
            .next();
        let last = records.unwrap().unwrap().segment.into_owned();
        let count = om_hbam::serial::decode(&last).unwrap_or(1);
        let mut rng = Rng(7);
        let started = Instant::now();
        for _ in 0..1_000 {
            let id = 1 + rng.next() % count;
            let record = om_hbam::serial::encode(id);
            let mut path = table.to_vec();
            path.push(&record);
            assert!(view.subtree(&path, NO_LIMIT).unwrap().is_some());
        }
        let scattered = started.elapsed().as_secs_f64() * 1e3;
        (walk, scattered, walked)
    }
    /// The table's records listed, a page of children at a time: how many,
    /// and the milliseconds it took.
    fn listing(view: &dyn View, table: &[&[u8]]) -> (usize, f64) {
        let started = Instant::now();
        let listed = view
            .children(table, Bound::Unbounded, Direction::Forward)
            .map(Result::unwrap)
            .count();
        (listed, started.elapsed().as_secs_f64() * 1e3)
    }
    std::fs::write("/proc/self/clear_refs", "5").unwrap();
    let base = status("VmRSS:");
    let started = Instant::now();
    let report = match step.as_str() {
        "import" => {
            std::fs::create_dir_all(&folder).unwrap();
            let fs = OsFs::new(&folder);
            let lock = fs.lock().unwrap();
            let pf = PageFile::create(&fs, &lock, config).unwrap();
            let mut units = OnPageFile {
                pf: &pf,
                checkpoints: 0,
            };
            let source = std::io::BufReader::with_capacity(64 << 10, File::open(&file).unwrap());
            let imported = import(source, &mut units, &tree_name(), 1, Batch::default()).unwrap();
            units.checkpoint().unwrap();
            let took = started.elapsed().as_secs_f64() * 1e3;
            let checkpoints = units.checkpoints;
            // What the file holds: the tree's pages, and the rest.
            let tree = om_store::tree::check(&pf, imported.root).unwrap();
            let in_tree = tree.pages.len() as f64 * PAGE_SIZE as f64 / 1e6;
            let space = pf.space();
            drop(pf);
            std::fs::write(&page_zero_at, &imported.page_zero).unwrap();
            let pages = std::fs::metadata(folder.join(om_store::pagefile::FILE))
                .unwrap()
                .len() as f64
                / 1e6;
            format!(
                "imported in {took:.0} ms: {} nodes in {} batches, {checkpoints} checkpoints; \
                 page file {pages:.1} MB ({:.2} times the file), of it the tree's {} pages \
                 {in_tree:.1} MB ({:.2} times; {} leaves, {} index pages, height {}), {} pages \
                 free and {} dying",
                imported.nodes,
                imported.entries.1,
                pages / size,
                tree.pages.len(),
                in_tree / size,
                tree.leaves,
                tree.index_pages,
                tree.height,
                space.free,
                space.dying,
            )
        }
        "export" | "reads" => {
            let fs = OsFs::new(&folder);
            let lock = fs.lock().unwrap();
            let (pf, _) = PageFile::open(&fs, &lock, config).unwrap();
            let pin = pf.pin(pf.checkpoint_entry().1.index).unwrap();
            let view = PagedView::open(&pf, pin.roots()[0].1).unwrap();
            if step == "export" {
                let page_zero = std::fs::read(&page_zero_at).unwrap();
                let out = file.with_file_name("exported.fmp12");
                let mut into = File::options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&out)
                    .unwrap();
                let exported =
                    export(&view, &page_zero, Target::File(&mut into), Progress::none()).unwrap();
                std::fs::remove_file(&out).unwrap();
                format!("{:.1} MB written and checked", exported.size as f64 / 1e6)
            } else {
                let (walk, scattered, walked) = reads(&view, &table);
                // More passes when asked (`OM_PAGED_PASSES`, 2 unless set),
                // so that a profile of the step is mostly of warm reads.
                let passes: usize = std::env::var("OM_PAGED_PASSES")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2);
                let mut warm = Vec::new();
                for _ in 1..passes.max(2) {
                    warm.push(reads(&view, &table));
                }
                let (again, again_scattered) = (
                    warm.iter().map(|r| r.0).sum::<f64>() / warm.len() as f64,
                    warm.iter().map(|r| r.1).sum::<f64>() / warm.len() as f64,
                );
                let (listed, listed_ms) = listing(&view, &table);
                format!(
                    "cache {cache_mib} MiB: walk of the table {walk:.0} ms ({walked} nodes), 1,000 \
                     records {scattered:.1} ms; again ({} passes) {again:.0} ms and \
                     {again_scattered:.1} ms; {listed} records listed {listed_ms:.0} ms",
                    warm.len()
                )
            }
        }
        "index-reads" | "file-index-reads" => {
            let index = [
                om_hbam::slot::uint_segment(fixture::TABLE),
                vec![1],
                om_hbam::slot::uint_segment(fixture::NAME),
                vec![0],
            ];
            let index: Vec<&[u8]> = refs(&index);
            let passes = |view: &dyn View| {
                (0..2)
                    .map(|_| index_reads(view, &index))
                    .collect::<Vec<_>>()
                    .join("; again: ")
            };
            if step == "index-reads" {
                let fs = OsFs::new(&folder);
                let lock = fs.lock().unwrap();
                let (pf, _) = PageFile::open(&fs, &lock, config).unwrap();
                let pin = pf.pin(pf.checkpoint_entry().1.index).unwrap();
                let view = PagedView::open(&pf, pin.roots()[0].1).unwrap();
                format!("cache {cache_mib} MiB: {}", passes(&view))
            } else {
                let hbam = HbamFile::open(&file).unwrap();
                let store = Store::load(&hbam, "Measured.fmp12").unwrap();
                drop(hbam);
                let report = passes(&store.tree_view());
                report
            }
        }
        "decode" => {
            // What import does before it writes: the stream read and each
            // node's checks, with nothing applied.
            let source = std::io::BufReader::with_capacity(64 << 10, File::open(&file).unwrap());
            let mut stream = NodeStream::new(source).unwrap();
            let mut nodes = 0u64;
            while let Some(node) = stream.next_node().unwrap() {
                let path = node.path.to_vec();
                let kind = match (node.push, node.value) {
                    (false, Some(v)) => Node::Value(v.to_vec()),
                    (true, None) => Node::List,
                    (true, Some(v)) => Node::Both(v.to_vec()),
                    (false, None) => unreachable!(),
                };
                check(&path, &kind).unwrap();
                nodes += 1;
            }
            format!("{nodes} nodes read and checked")
        }
        "mem-reads" => {
            let trusting = std::env::var("OM_PAGED_TRUSTING").is_ok_and(|v| v == "1");
            let mut store = if trusting {
                MemStore::trusting(Numbering::Sequential)
            } else {
                MemStore::new(Numbering::Sequential)
            };
            let source = std::io::BufReader::with_capacity(64 << 10, File::open(&file).unwrap());
            let imported = import(source, &mut store, &tree_name(), 1, Batch::default()).unwrap();
            let view = PagedView::open(&store, imported.root).unwrap();
            let (walk, scattered, walked) = reads(&view, &table);
            let (again, again_scattered, _) = reads(&view, &table);
            format!(
                "{} store in memory: walk of the table {walk:.0} ms ({walked} nodes), 1,000 \
                 records {scattered:.1} ms; again {again:.0} ms and {again_scattered:.1} ms",
                if trusting { "trusting" } else { "checking" }
            )
        }
        "file-reads" | "file-export" => {
            let hbam = HbamFile::open(&file).unwrap();
            let store = Store::load(&hbam, "Measured.fmp12").unwrap();
            drop(hbam);
            let loaded = started.elapsed().as_secs_f64() * 1e3;
            if step == "file-reads" {
                let (walk, scattered, walked) = reads(&store.tree_view(), &table);
                let (listed, listed_ms) = listing(&store.tree_view(), &table);
                format!(
                    "load {loaded:.0} ms; walk of the table {walk:.0} ms ({walked} nodes), 1,000 \
                     records {scattered:.1} ms; {listed} records listed {listed_ms:.0} ms"
                )
            } else {
                let Source::Written { change_number } = store.compacted_source() else {
                    unreachable!()
                };
                let out = file.with_file_name("file-exported.fmp12");
                let mut into = File::options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&out)
                    .unwrap();
                let started = Instant::now();
                write_checked(
                    &store.tree_view(),
                    store.derived().page_zero(),
                    change_number,
                    &mut into,
                    Progress::none(),
                )
                .unwrap();
                let written = started.elapsed().as_secs_f64() * 1e3;
                std::fs::remove_file(&out).unwrap();
                format!("load {loaded:.0} ms; export written and checked in {written:.0} ms")
            }
        }
        other => panic!("unknown step {other}"),
    };
    println!(
        "{step} {size:.1} MB file: {:.0} ms in all, peak {:.1} MB above the process's {base:.0} \
         MB; {report}",
        started.elapsed().as_secs_f64() * 1e3,
        status("VmHWM:") - base,
    );
}

/// Review of #1166 (P1): a paged read that meets a fault fails the call,
/// whatever it gave before it, rather than answer those as a page after
/// which the next call starts past them (the module doc, "A read that
/// fails partway"). On the reviewer's tree (leaves `a, d` and `b, c`, the
/// separator `b`, written page by page), a walk of the root in one page
/// with room for every node, through the view's own walk, and in pages of
/// two, fails where the cursor steps from `d` into the leaf that begins
/// before it. Answered as a continuation, the next page, sought past `d`,
/// found nothing beyond it and the walk ended without `b` and `c`. (A walk
/// in pages of three, and a listing of the root's children, take no step
/// across the fault: a seek past `d` finds nothing beyond it, as from the
/// root, #1180; they are not asserted here.) And on #998's tree (the
/// separator `x`), a page of children that meets the child that comes back
/// fails, rather than answer the children before it and "more"; so does a
/// full page of two, `a` and `d`, whose look past `d` comes back to `b`,
/// though that look asks only whether more follow (#1200).
#[test]
fn a_paged_read_fails_at_a_fault_rather_than_end_on_the_next_page() {
    use om_store::tree::{built, key};
    let store = MemStore::new(Numbering::Sequential);
    let item = |s: &[u8]| (built::KIND_VALUE, key::encode(&[s]), b"v".to_vec());
    let tree_with = |separator: &[u8]| {
        let separator = key::encode(&[separator]);
        let leaves = [vec![item(b"a"), item(b"d")], vec![item(b"b"), item(b"c")]];
        built::tree(&store, &leaves, &[separator.as_slice()]).root
    };
    let root_path: [&[u8]; 0] = [];
    let failed = |read: &Result<Option<Vec<Vec<u8>>>, ReadError>| {
        matches!(read, Err(ReadError::Unreadable { .. }))
    };
    let view = PagedView::open(&store, tree_with(b"b")).unwrap();
    let mut whole = Vec::new();
    let one_page = view.walk_page(&root_path, None, 100, &mut whole);
    assert!(failed(&one_page), "{one_page:?}");
    let walked: Result<Vec<Step>, ReadError> = (&view as &dyn View).walk(&root_path).collect();
    assert!(walked.is_err(), "{walked:?}");
    let mut first = Vec::new();
    let resume = view.walk_page(&root_path, None, 2, &mut first).unwrap();
    assert_eq!(resume, Some(vec![b"a".to_vec()]));
    let mut second = Vec::new();
    let next = view.walk_page(&root_path, resume.as_deref(), 2, &mut second);
    assert!(failed(&next), "{next:?}");
    let view = PagedView::open(&store, tree_with(b"x")).unwrap();
    let mut children = Vec::new();
    let listed = view.children_page(
        &root_path,
        Bound::Unbounded,
        Direction::Forward,
        10,
        &mut children,
    );
    assert!(
        matches!(listed, Err(ReadError::Unreadable { .. })),
        "{listed:?}"
    );
    let mut two = Vec::new();
    let looked = view.children_page(
        &root_path,
        Bound::Unbounded,
        Direction::Forward,
        2,
        &mut two,
    );
    assert!(
        matches!(looked, Err(ReadError::Unreadable { .. })),
        "{looked:?}"
    );
}
