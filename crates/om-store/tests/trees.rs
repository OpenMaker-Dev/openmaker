//! The page trees against a reference model (issue #686, "How to check").
//!
//! Every random test is seeded and replays exactly. `OM_STORE_SEEDS` (how
//! many seeds, default 2) and `OM_STORE_ENTRIES` (entries per seed, default
//! 120) raise them for long runs; `OM_STORE_SEED` runs one seed. A long run
//! is quicker with `--release`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use om_store::page::{
    Fault, Hash, MemStore, Numbering, PageFault, PageNo, PageStore, Release, MAX_KEY, MAX_VALUE,
};
use om_store::tree::{
    apply, check, create, key, Direction, Edit, Error, Node, Refusal, Root, Tree, MAX_DEPTH,
};

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
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn seeds() -> Vec<u64> {
    if let Ok(seed) = std::env::var("OM_STORE_SEED") {
        return vec![seed.parse().expect("OM_STORE_SEED is a number")];
    }
    (1..=env("OM_STORE_SEEDS", 2)).collect()
}

/// The reference: a map of paths, and the last entry that changed each path
/// (deleted paths included).
#[derive(Clone, Default)]
struct Model {
    nodes: BTreeMap<Path, Node>,
    changed: BTreeMap<Path, u64>,
}

impl Model {
    fn under<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = (&'a Path, &'a Node)> + 'a {
        self.nodes
            .range(path.clone()..)
            .take_while(move |(p, _)| p.starts_with(path))
    }

    fn apply(&mut self, entry: u64, edit: &Edit) {
        match edit {
            Edit::Put { path, node } => {
                for depth in 1..path.len() {
                    let ancestor = path[..depth].to_vec();
                    match self.nodes.get(&ancestor) {
                        None => {
                            self.nodes.insert(ancestor.clone(), Node::List);
                            self.changed.insert(ancestor, entry);
                        }
                        Some(Node::Value(v)) => {
                            let both = Node::Both(v.clone());
                            self.nodes.insert(ancestor.clone(), both);
                            self.changed.insert(ancestor, entry);
                        }
                        Some(_) => {}
                    }
                }
                let below: Vec<Path> = self
                    .under(path)
                    .map(|(p, _)| p.clone())
                    .filter(|p| p != path)
                    .collect();
                for p in below {
                    self.nodes.remove(&p);
                    self.changed.insert(p, entry);
                }
                if self.nodes.get(path) != Some(node) {
                    self.nodes.insert(path.clone(), node.clone());
                    self.changed.insert(path.clone(), entry);
                }
            }
            Edit::Delete { path } => {
                let gone: Vec<Path> = self.under(path).map(|(p, _)| p.clone()).collect();
                for p in gone {
                    self.nodes.remove(&p);
                    self.changed.insert(p, entry);
                }
            }
        }
    }

    fn changed_since(&self, path: &Path, entry: u64) -> bool {
        self.changed
            .range(path.clone()..)
            .take_while(|(p, _)| p.starts_with(path))
            .any(|(_, &e)| e > entry)
    }

    fn last_change(&self) -> u64 {
        self.changed.values().copied().max().unwrap_or(0)
    }

    /// Every node, in key order.
    fn listing(&self) -> Vec<(Path, Node)> {
        self.nodes
            .iter()
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect()
    }
}

/// Draws paths and values of every size class from a pool that makes
/// paths share prefixes.
struct Gen {
    rng: Rng,
    /// Paths used so far, to reuse.
    pool: Vec<Path>,
    /// Long segment bases: keys sharing them give long separators, some
    /// spilled.
    bases: Vec<Vec<u8>>,
    /// A path appended to in key order, for bulk entries.
    counter: u32,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        let mut rng = Rng(seed);
        let bases = vec![rng.bytes(1500), rng.bytes(4300), vec![0; 2400]];
        Gen {
            rng,
            pool: Vec::new(),
            bases,
            counter: 0,
        }
    }

    fn segment(&mut self) -> Vec<u8> {
        const ALPHABET: [u8; 7] = [0x00, 0x01, 0x02, 0x41, 0x42, 0xfe, 0xff];
        let r = self.rng.below(100);
        if r < 3 {
            Vec::new()
        } else if r < 70 {
            let n = self.rng.range(1, 3);
            (0..n)
                .map(|_| ALPHABET[self.rng.below(ALPHABET.len())])
                .collect()
        } else if r < 92 {
            let n = self.rng.range(4, 40);
            self.rng.bytes(n)
        } else if r < 98 {
            let base = self.bases[self.rng.below(self.bases.len())].clone();
            let mut s = base;
            let n = self.rng.range(1, 3);
            s.extend(self.rng.bytes(n));
            s
        } else {
            let n = self.rng.range(200, 3000);
            self.rng.bytes(n)
        }
    }

    fn path(&mut self, model: &Model) -> Path {
        let mut path: Path = if !self.pool.is_empty() && self.rng.percent(75) {
            let p = self.pool[self.rng.below(self.pool.len())].clone();
            let keep = self.rng.range(0, p.len());
            p[..keep].to_vec()
        } else if !model.nodes.is_empty() && self.rng.percent(50) {
            let n = self.rng.below(model.nodes.len());
            model.nodes.keys().nth(n).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        let depth = if self.rng.percent(2) {
            self.rng.range(10, MAX_DEPTH)
        } else {
            self.rng.range(1, 4)
        };
        while path.len() < depth.max(1) {
            path.push(self.segment());
        }
        path.truncate(MAX_DEPTH);
        while key::encoded_len(&path) > MAX_KEY {
            path.pop();
        }
        if path.is_empty() {
            path.push(vec![1]);
        }
        if self.pool.len() < 400 {
            self.pool.push(path.clone());
        } else {
            let i = self.rng.below(self.pool.len());
            self.pool[i] = path.clone();
        }
        path
    }

    fn value(&mut self) -> Vec<u8> {
        let r = self.rng.below(1000);
        let n = if r < 100 {
            0
        } else if r < 600 {
            self.rng.range(1, 8)
        } else if r < 880 {
            self.rng.range(9, 300)
        } else if r < 960 {
            self.rng.range(300, 1500)
        } else if r < 985 {
            // Around the spill boundary.
            self.rng.range(4000, 4100)
        } else if r < 997 {
            self.rng.range(4100, 20_000)
        } else {
            self.rng.range(60_000, MAX_VALUE)
        };
        self.rng.bytes(n)
    }

    fn node(&mut self) -> Node {
        match self.rng.below(100) {
            0..=69 => Node::Value(self.value()),
            70..=93 => Node::List,
            _ => Node::Both(self.value()),
        }
    }

    fn edit(&mut self, model: &Model) -> Edit {
        let r = self.rng.below(1000);
        if r < 2 {
            return Edit::delete(Vec::new());
        }
        if r < 260 {
            let path = if !model.nodes.is_empty() && self.rng.percent(85) {
                let n = self.rng.below(model.nodes.len());
                let p = model.nodes.keys().nth(n).cloned().unwrap_or_default();
                let keep = self.rng.range(1, p.len().max(1));
                p[..keep.min(p.len())].to_vec()
            } else {
                self.path(model)
            };
            return Edit::delete(path);
        }
        let path = self.path(model);
        let node = self.node();
        Edit::Put { path, node }
    }

    /// The edits of one entry: mostly a few, sometimes many, sometimes a
    /// bulk append in key order.
    fn entry(&mut self, model: &Model) -> Vec<Edit> {
        let r = self.rng.below(100);
        if r < 4 {
            let n = self.rng.range(100, 400);
            let parent = vec![b"bulk".to_vec(), vec![self.rng.below(3) as u8]];
            return (0..n)
                .map(|_| {
                    self.counter += 1;
                    let mut path = parent.clone();
                    path.push(self.counter.to_be_bytes().to_vec());
                    let size = self.rng.range(10, 120);
                    Edit::leaf(path, self.rng.bytes(size))
                })
                .collect();
        }
        let n = if r < 45 {
            1
        } else if r < 85 {
            self.rng.range(2, 6)
        } else {
            self.rng.range(7, 40)
        };
        let mut scratch = model.clone();
        (0..n)
            .map(|_| {
                let edit = self.edit(&scratch);
                scratch.apply(0, &edit);
                edit
            })
            .collect()
    }
}

fn walk_all<S: PageStore>(tree: &Tree<'_, S>) -> Result<Vec<(Path, Node)>, Error> {
    tree.walk::<Vec<u8>>(&[])
        .map(|item| item.map(|item| (item.key.path(), item.node)))
        .collect()
}

fn model_children(model: &Model, path: &Path) -> Vec<(Vec<u8>, Node)> {
    model
        .under(path)
        .filter(|(p, _)| p.len() == path.len() + 1)
        .map(|(p, n)| (p[path.len()].clone(), n.clone()))
        .collect()
}

/// Every page a version reaches.
fn reachable<S: PageStore>(store: &S, root: Root) -> BTreeSet<PageNo> {
    check(store, root)
        .unwrap()
        .pages
        .iter()
        .map(|(n, _)| *n)
        .collect()
}

/// Compares a version with the model in every way the tree reads.
fn compare<S: PageStore>(
    store: &S,
    root: Root,
    model: &Model,
    rng: &mut Rng,
    gen: &mut Gen,
    entry: u64,
) {
    let tree = Tree::open(store, root).unwrap();
    let all = walk_all(&tree).unwrap();
    let expected = model.listing();
    assert_eq!(all.len(), expected.len(), "node count");
    assert!(all == expected, "the walk differs from the model");
    let backward: Vec<(Path, Node)> = tree
        .range(Bound::Unbounded, Bound::Unbounded, Direction::Backward)
        .map(|item| item.map(|item| (item.key.path(), item.node)))
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        backward.iter().rev().eq(expected.iter()),
        "the backward walk differs"
    );

    let mut probes: Vec<Path> = (0..8).map(|_| gen.path(model)).collect();
    for _ in 0..8 {
        if model.nodes.is_empty() {
            break;
        }
        let n = rng.below(model.nodes.len());
        probes.push(model.nodes.keys().nth(n).cloned().unwrap());
    }
    probes.push(Vec::new());
    for path in &probes {
        let expected = if path.is_empty() {
            Some(Node::List)
        } else {
            model.nodes.get(path).cloned()
        };
        assert_eq!(tree.get(path).unwrap(), expected, "get {path:?}");

        let subtree: Vec<(Path, Node)> = tree
            .walk(path)
            .map(|item| item.map(|item| (item.key.path(), item.node)))
            .collect::<Result<_, _>>()
            .unwrap();
        let want: Vec<(Path, Node)> = model
            .under(path)
            .map(|(p, n)| (p.clone(), n.clone()))
            .collect();
        assert!(subtree == want, "walk {path:?}");

        let kids = model_children(model, path);
        let forward: Vec<_> = tree
            .children(path, Bound::Unbounded, Direction::Forward)
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(forward == kids, "children of {path:?}");
        let backward: Vec<_> = tree
            .children(path, Bound::Unbounded, Direction::Backward)
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            backward.iter().rev().eq(kids.iter()),
            "children of {path:?} backward"
        );
        if !kids.is_empty() {
            let from = if rng.percent(70) {
                kids[rng.below(kids.len())].0.clone()
            } else {
                gen.segment()
            };
            for (bound, direction) in [
                (Bound::Included(from.as_slice()), Direction::Forward),
                (Bound::Excluded(from.as_slice()), Direction::Forward),
                (Bound::Included(from.as_slice()), Direction::Backward),
                (Bound::Excluded(from.as_slice()), Direction::Backward),
            ] {
                let got: Vec<_> = tree
                    .children(path, bound, direction)
                    .collect::<Result<_, _>>()
                    .unwrap();
                let want: Vec<_> = match (direction, bound) {
                    (Direction::Forward, Bound::Included(f)) => kids
                        .iter()
                        .filter(|(s, _)| s.as_slice() >= f)
                        .cloned()
                        .collect(),
                    (Direction::Forward, _) => kids
                        .iter()
                        .filter(|(s, _)| s.as_slice() > from.as_slice())
                        .cloned()
                        .collect(),
                    (Direction::Backward, Bound::Included(f)) => kids
                        .iter()
                        .rev()
                        .filter(|(s, _)| s.as_slice() <= f)
                        .cloned()
                        .collect(),
                    (Direction::Backward, _) => kids
                        .iter()
                        .rev()
                        .filter(|(s, _)| s.as_slice() < from.as_slice())
                        .cloned()
                        .collect(),
                };
                assert!(
                    got == want,
                    "children of {path:?} from {from:?} {direction:?}"
                );
            }
        }
    }

    // Key ranges with random bounds, both ways.
    for _ in 0..4 {
        let a = key::encode(&gen.path(model));
        let b = key::encode(&gen.path(model));
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let lo_bound = if rng.percent(50) {
            Bound::Included(lo.as_slice())
        } else {
            Bound::Excluded(lo.as_slice())
        };
        let hi_bound = if rng.percent(50) {
            Bound::Included(hi.as_slice())
        } else {
            Bound::Excluded(hi.as_slice())
        };
        let want: Vec<Vec<u8>> = model
            .nodes
            .keys()
            .map(|p| key::encode(p))
            .filter(|k| std::ops::RangeBounds::contains(&(lo_bound, hi_bound), k.as_slice()))
            .collect();
        for direction in [Direction::Forward, Direction::Backward] {
            let got: Vec<Vec<u8>> = tree
                .range(lo_bound, hi_bound, direction)
                .map(|item| item.map(|item| item.key.into_bytes()))
                .collect::<Result<_, _>>()
                .unwrap();
            if direction == Direction::Forward {
                assert_eq!(got, want, "range forward");
            } else {
                assert!(got.iter().rev().eq(want.iter()), "range backward");
            }
        }
    }

    // "Changed since entry N": never false when the model saw a change; exact
    // for the whole tree.
    for _ in 0..10 {
        let n = rng.below(entry as usize + 1) as u64;
        let path = if rng.percent(50) || model.changed.is_empty() {
            probes[rng.below(probes.len())].clone()
        } else {
            let i = rng.below(model.changed.len());
            model.changed.keys().nth(i).cloned().unwrap()
        };
        let got = tree.changed_since(&path, n).unwrap();
        if model.changed_since(&path, n) {
            assert!(got, "a change under {path:?} since entry {n} not found");
        }
        // The tree was made by entry 1.
        let any = tree.changed_since::<Vec<u8>>(&[], n).unwrap();
        assert_eq!(
            any,
            model.last_change().max(1) > n,
            "the whole tree since entry {n}"
        );
    }
}

/// The issue's first, third and fifth checks: long random sequences against
/// the model, older roots reading what they read, "changed since" sound for
/// every path asked, and the pages each entry releases exactly those the new
/// version no longer reaches.
#[test]
fn random_edits_match_the_model() {
    let entries = env("OM_STORE_ENTRIES", 120);
    for seed in seeds() {
        let store = MemStore::new(Numbering::Sequential);
        let mut gen = Gen::new(seed);
        let mut rng = Rng(seed ^ 0xabcd);
        let mut model = Model::default();
        let mut root = create(&store, 1).unwrap();
        let mut kept: Vec<(Root, Model)> = Vec::new();
        let mut tallest = 0;
        let mut before = reachable(&store, root);
        for entry in 2..entries + 2 {
            let edits = gen.entry(&model);
            store.take_released();
            store.take_written();
            let new = apply(&store, root, entry, &edits)
                .unwrap_or_else(|e| panic!("seed {seed} entry {entry}: {e}"));
            for edit in &edits {
                model.apply(entry, edit);
            }
            let report =
                check(&store, new).unwrap_or_else(|e| panic!("seed {seed} entry {entry}: {e}"));
            tallest = tallest.max(report.height);
            let after: BTreeSet<PageNo> = report.pages.iter().map(|(n, _)| *n).collect();

            // Releases: each page once, exactly what the new version left.
            let released = store.take_released();
            let mut gone = BTreeSet::new();
            for (number, hash, how) in &released {
                assert!(
                    !after.contains(number),
                    "seed {seed} entry {entry}: page {number} released and still reached"
                );
                match how {
                    Release::Copied => {
                        assert!(gone.insert(*number), "page {number} released twice")
                    }
                    Release::Dropped => {
                        for (n, _) in subtree_pages(&store, *number, *hash) {
                            assert!(gone.insert(n), "page {n} released twice");
                        }
                    }
                }
            }
            let left: BTreeSet<PageNo> = before.difference(&after).copied().collect();
            assert_eq!(gone, left, "seed {seed} entry {entry}: released pages");
            let written: BTreeSet<PageNo> = store.take_written().into_iter().collect();
            let fresh: BTreeSet<PageNo> = after.difference(&before).copied().collect();
            assert_eq!(written, fresh, "seed {seed} entry {entry}: written pages");
            // An entry writes pages exactly when it changes something.
            assert_eq!(
                new == root,
                model.last_change() < entry,
                "seed {seed} entry {entry}"
            );

            root = new;
            before = after;
            compare(&store, root, &model, &mut rng, &mut gen, entry);
            if entry % 25 == 0 {
                kept.push((root, model.clone()));
            }
        }
        // Older roots still read exactly what they read.
        for (old, then) in &kept {
            let tree = Tree::open(&store, *old).unwrap();
            assert!(
                walk_all(&tree).unwrap() == then.listing(),
                "an older version changed"
            );
        }
        eprintln!(
            "seed {seed}: {} nodes, height {tallest}, {} pages stored",
            model.nodes.len(),
            store.len()
        );
    }
}

/// The pages of a subtree that a release dropped: an index page or leaf with
/// what it refers to, or a lone overflow page.
fn subtree_pages<S: PageStore>(store: &S, number: PageNo, hash: Hash) -> Vec<(PageNo, Hash)> {
    let page = store.read(number, &hash).unwrap();
    let mut out = vec![(number, hash)];
    for (n, h) in page.references() {
        out.extend(subtree_pages(store, n, h));
    }
    out
}

/// The issue's second check: the same sequence over stores that number
/// pages differently gives the same root hash after every entry.
#[test]
fn root_hashes_do_not_depend_on_page_numbers() {
    let entries = env("OM_STORE_ENTRIES", 120);
    for seed in seeds() {
        let a = MemStore::new(Numbering::Sequential);
        let b = MemStore::new(Numbering::Scattered(seed.wrapping_mul(7919)));
        let c = MemStore::trusting(Numbering::Scattered(seed));
        let mut gen = Gen::new(seed);
        let mut model = Model::default();
        let mut roots = [
            create(&a, 1).unwrap(),
            create(&b, 1).unwrap(),
            create(&c, 1).unwrap(),
        ];
        assert_eq!(roots[0].hash, roots[1].hash);
        for entry in 2..entries + 2 {
            let edits = gen.entry(&model);
            for edit in &edits {
                model.apply(entry, edit);
            }
            roots[0] = apply(&a, roots[0], entry, &edits).unwrap();
            roots[1] = apply(&b, roots[1], entry, &edits).unwrap();
            roots[2] = apply(&c, roots[2], entry, &edits).unwrap();
            assert_ne!(roots[0].page, roots[1].page);
            assert_eq!(roots[0].hash, roots[1].hash, "seed {seed} entry {entry}");
            assert_eq!(roots[0].hash, roots[2].hash, "seed {seed} entry {entry}");
        }
    }
}

/// Tree format version 1, pinned: a fixed sequence of edits gives this root
/// hash. A change to the page layout or the shape rules changes it, and
/// needs a new format version. The sequence is its own, not the random
/// tests' generator, so that only the format moves it.
#[test]
fn format_version_1_is_pinned() {
    let store = MemStore::new(Numbering::Sequential);
    let mut rng = Rng(1);
    let mut root = create(&store, 1).unwrap();
    let long = vec![0x42u8; 4200];
    for entry in 2..400u64 {
        let edits: Vec<Edit> = (0..1 + entry % 23)
            .map(|_| {
                let r = rng.next();
                let record = ((r >> 8) % 1500) as u32;
                let mut path = vec![
                    vec![0x80, (r % 3) as u8],
                    vec![5],
                    record.to_be_bytes().to_vec(),
                ];
                if r % 5 == 0 {
                    path.push(vec![(r >> 40) as u8 % 4]);
                }
                if r % 11 == 0 {
                    let mut segment = long.clone();
                    segment.extend(&record.to_be_bytes()[2..]);
                    path = vec![segment];
                }
                let size = match (r >> 24) % 50 {
                    0 => 4090 + (r >> 32) as usize % 10,
                    1 => 20_000,
                    _ => (r >> 32) as usize % 60,
                };
                let value = vec![(r >> 16) as u8; size];
                match (r >> 48) % 12 {
                    0 => Edit::list(path),
                    1 => Edit::Put {
                        path,
                        node: Node::Both(value),
                    },
                    2 => Edit::delete(path[..path.len().min(2)].to_vec()),
                    3 => Edit::delete(path),
                    _ => Edit::leaf(path, value),
                }
            })
            .collect();
        root = apply(&store, root, entry, &edits).unwrap();
    }
    let report = check(&store, root).unwrap();
    assert!(
        report.height >= 3 && report.spilled_items > 0 && report.spilled_separators > 0,
        "{report:?}"
    );
    assert_eq!(
        root.hash.to_hex(),
        "88205feb44aec9dc235b230588d1619f0d88bd5e528d0c02afc4f8d9b5c4c97f",
        "{} items",
        report.items
    );
}

/// The tree of `n` small records under one table, written in entries of
/// `per` puts.
fn records(store: &MemStore, n: u32, per: u32, value: usize) -> (Root, Model, u64) {
    let mut model = Model::default();
    let mut root = create(store, 1).unwrap();
    let mut entry = 1;
    let mut i = 0;
    while i < n {
        entry += 1;
        let edits: Vec<Edit> = (i..(i + per).min(n))
            .map(|r| {
                Edit::leaf(
                    vec![vec![0x80, 0x01], vec![5], r.to_be_bytes().to_vec(), vec![1]],
                    vec![(r % 251) as u8; value],
                )
            })
            .collect();
        for edit in &edits {
            model.apply(entry, edit);
        }
        root = apply(store, root, entry, &edits).unwrap();
        i += per;
    }
    (root, model, entry)
}

/// The issue's fourth check: a flipped byte in any page, two pages swapped,
/// or a page dropped makes every read that reaches it fail with an error
/// that names it, and never return wrong data.
#[test]
fn damaged_swapped_and_missing_pages_are_named() {
    let store = MemStore::new(Numbering::Sequential);
    let (mut root, mut model, mut entry) = records(&store, 600, 60, 40);
    // Spilled items and separators too.
    let long = vec![7u8; 4400];
    entry += 1;
    let edits: Vec<Edit> = (0..40u8)
        .map(|i| Edit::leaf(vec![long.clone(), vec![i]], vec![i; 9000]))
        .collect();
    for edit in &edits {
        model.apply(entry, edit);
    }
    root = apply(&store, root, entry, &edits).unwrap();
    let report = check(&store, root).unwrap();
    assert!(report.height >= 2 && report.spilled_items > 0 && report.overflow_pages > 0);
    let expected = model.listing();

    // Reads every node, checking that each read before a failure was right,
    // then every page (a walk does not need separators, so it does not read
    // the overflow pages of spilled ones; a lookup does). Returns the fault
    // the reads stopped at.
    let first_fault = || -> Option<PageFault> {
        let tree = match Tree::open(&store, root) {
            Ok(tree) => tree,
            Err(Error::Page(fault)) => return Some(fault),
            Err(e) => panic!("{e}"),
        };
        for (i, item) in tree.walk::<Vec<u8>>(&[]).enumerate() {
            match item {
                Ok(item) => assert!(
                    (item.key.path(), item.node) == expected[i],
                    "wrong data at {i}"
                ),
                Err(Error::Page(fault)) => return Some(fault),
                Err(e) => panic!("{e}"),
            }
        }
        match check(&store, root) {
            Ok(_) => None,
            Err(Error::Page(fault)) => Some(fault),
            Err(e) => panic!("{e}"),
        }
    };
    let read_all = || first_fault().map(|fault| fault.page);
    assert_eq!(read_all(), None);

    let pages: Vec<PageNo> = report.pages.iter().map(|(n, _)| *n).collect();
    let mut rng = Rng(99);
    for &page in &pages {
        let bytes = store.bytes(page).unwrap();
        // The checksum, the page number, the region header, a random byte,
        // and the last byte (zero fill, or data).
        let mut places = vec![0, 9, 17, 22, rng.below(bytes.len()), bytes.len() - 1];
        places.dedup();
        for at in places {
            let mask = (rng.below(255) + 1) as u8;
            store.damage(page, at, mask);
            assert_eq!(read_all(), Some(page), "byte {at} of page {page} damaged");
            store.damage(page, at, mask);
        }
    }
    assert_eq!(read_all(), None);
    for _ in 0..30 {
        let a = pages[rng.below(pages.len())];
        let b = pages[rng.below(pages.len())];
        if a == b {
            continue;
        }
        store.swap(a, b);
        let failed = read_all();
        assert!(
            failed == Some(a) || failed == Some(b),
            "swap of {a} and {b}: {failed:?}"
        );
        store.swap(a, b);
    }
    for &page in pages.iter().step_by(3) {
        let bytes = store.bytes(page).unwrap();
        store.remove(page);
        let fault = first_fault().expect("a dropped page is found");
        assert_eq!((fault.page, fault.fault), (page, Fault::Missing));
        store.put_bytes(page, &bytes);
    }
    assert_eq!(read_all(), None);
}

/// Bounds are enforced on the way in, before anything is applied.
#[test]
fn edits_beyond_the_bounds_are_refused_whole() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let longest = vec![vec![9u8; MAX_KEY - 2]];
    assert_eq!(key::encoded_len(&longest), MAX_KEY);
    let ok = apply(
        &store,
        root,
        2,
        &[Edit::leaf(longest.clone(), vec![5; MAX_VALUE])],
    )
    .unwrap();
    let tree = Tree::open(&store, ok).unwrap();
    assert_eq!(
        tree.get(&longest).unwrap(),
        Some(Node::Value(vec![5; MAX_VALUE]))
    );
    check(&store, ok).unwrap();

    let cases = [
        (
            Edit::leaf(vec![vec![9u8; MAX_KEY - 1]], vec![]),
            Refusal::KeyTooLong(MAX_KEY + 1),
        ),
        (
            Edit::leaf(vec![vec![1]], vec![0; MAX_VALUE + 1]),
            Refusal::ValueTooLong(MAX_VALUE + 1),
        ),
        (
            Edit::list(vec![vec![1]; MAX_DEPTH + 1]),
            Refusal::TooDeep(MAX_DEPTH + 1),
        ),
        (
            Edit::delete(vec![vec![1]; MAX_DEPTH + 1]),
            Refusal::TooDeep(MAX_DEPTH + 1),
        ),
        (Edit::list(Vec::new()), Refusal::RootPut),
    ];
    for (bad, why) in cases {
        store.take_written();
        let edits = [Edit::leaf(vec![b"first".to_vec()], vec![1]), bad];
        assert_eq!(
            apply(&store, ok, 3, &edits),
            Err(Error::Refused { edit: 1, why })
        );
        assert!(store.take_written().is_empty(), "nothing written");
    }
    let deepest = vec![vec![1]; MAX_DEPTH];
    let root = apply(&store, ok, 3, &[Edit::list(deepest.clone())]).unwrap();
    assert_eq!(
        Tree::open(&store, root).unwrap().get(&deepest).unwrap(),
        Some(Node::List)
    );
    assert_eq!(
        apply(&store, root, 3, &[Edit::list(vec![vec![2]])]),
        Err(Error::EntryOrder { entry: 3, last: 3 })
    );
}

/// Kinds of node, and the rules of a put: empty lists are kept, a list
/// becomes a leaf and back, a put replaces a whole subtree, missing
/// ancestors are made, and an ancestor that holds a value holds both.
#[test]
fn nodes_keep_their_kind() {
    let store = MemStore::new(Numbering::Sequential);
    let p = |s: &[&[u8]]| s.iter().map(|s| s.to_vec()).collect::<Path>();
    let mut root = create(&store, 1).unwrap();
    let mut entry = 1;
    let mut step = |edits: Vec<Edit>| {
        entry += 1;
        root = apply(&store, root, entry, &edits).unwrap();
        check(&store, root).unwrap();
        Tree::open(&store, root).unwrap()
    };
    let tree = step(vec![
        Edit::list(p(&[b"a"])),
        Edit::leaf(p(&[b"b", b"c", b"d"]), b"v".to_vec()),
    ]);
    assert_eq!(tree.get(&p(&[b"a"])).unwrap(), Some(Node::List));
    assert_eq!(tree.get(&p(&[b"b"])).unwrap(), Some(Node::List));
    assert_eq!(tree.get(&p(&[b"b", b"c"])).unwrap(), Some(Node::List));
    let tree = step(vec![Edit::leaf(p(&[b"b"]), b"x".to_vec())]);
    assert_eq!(
        walk_all(&tree).unwrap().len(),
        2,
        "the leaf replaced the subtree"
    );
    let tree = step(vec![Edit::list(p(&[b"b"]))]);
    assert_eq!(tree.get(&p(&[b"b"])).unwrap(), Some(Node::List));
    let tree = step(vec![
        Edit::leaf(p(&[b"v"]), b"1".to_vec()),
        Edit::leaf(p(&[b"v", b"w"]), b"2".to_vec()),
    ]);
    assert_eq!(
        tree.get(&p(&[b"v"])).unwrap(),
        Some(Node::Both(b"1".to_vec()))
    );
    let before = tree.root();
    let tree = step(vec![
        Edit::leaf(p(&[b"v", b"w"]), b"2".to_vec()),
        Edit::delete(p(&[b"nothing"])),
    ]);
    assert_eq!(
        tree.root(),
        before,
        "an entry that changes nothing writes nothing"
    );
    let tree = step(vec![Edit::delete(Vec::new())]);
    assert!(walk_all(&tree).unwrap().is_empty());
    assert_eq!(tree.height(), 1);
}

/// A subtree that spans many pages is deleted without reading its inside,
/// and its pages are released.
#[test]
fn deleting_a_subtree_of_many_pages() {
    let store = MemStore::new(Numbering::Sequential);
    let (root, mut model, entry) = records(&store, 4000, 500, 1500);
    let before = check(&store, root).unwrap();
    assert!(before.height >= 3, "height {}", before.height);
    // Something on either side of the subtree.
    let mut edits = vec![
        Edit::leaf(vec![vec![0x80, 0x00]], b"before".to_vec()),
        Edit::leaf(vec![vec![0x80, 0x02]], b"after".to_vec()),
    ];
    for edit in &edits {
        model.apply(entry + 1, edit);
    }
    let root = apply(&store, root, entry + 1, &edits).unwrap();
    store.take_released();
    edits = vec![Edit::delete(vec![vec![0x80, 0x01], vec![5]])];
    model.apply(entry + 2, &edits[0]);
    let after = apply(&store, root, entry + 2, &edits).unwrap();
    let report = check(&store, after).unwrap();
    let tree = Tree::open(&store, after).unwrap();
    assert!(walk_all(&tree).unwrap() == model.listing());
    let dropped = store
        .take_released()
        .iter()
        .filter(|r| r.2 == Release::Dropped)
        .count();
    assert!(dropped > 1, "whole subtrees dropped: {dropped}");
    assert!(report.pages.len() < 10, "{} pages left", report.pages.len());
    assert!(tree
        .changed_since(
            &[vec![0x80, 0x01], vec![5], 77u32.to_be_bytes().to_vec()],
            entry + 1
        )
        .unwrap());
    assert!(!tree.changed_since(&[vec![0x80, 0x01]], entry + 2).unwrap());
}

/// Keys that share a long prefix give separators that spill, and trees
/// several index levels tall still read and write right.
#[test]
fn tall_trees_with_spilled_separators() {
    let store = MemStore::new(Numbering::Scattered(5));
    let mut model = Model::default();
    let mut root = create(&store, 1).unwrap();
    let base = vec![0x42u8; 4200];
    let mut rng = Rng(17);
    let mut order: Vec<u16> = (0..700).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }
    for (e, chunk) in order.chunks(50).enumerate() {
        let entry = e as u64 + 2;
        let edits: Vec<Edit> = chunk
            .iter()
            .map(|i| {
                let mut segment = base.clone();
                segment.extend(i.to_be_bytes());
                Edit::leaf(vec![segment], vec![1; 30])
            })
            .collect();
        for edit in &edits {
            model.apply(entry, edit);
        }
        root = apply(&store, root, entry, &edits).unwrap();
    }
    let report = check(&store, root).unwrap();
    assert!(report.height >= 3, "height {}", report.height);
    assert!(
        report.spilled_separators > 10,
        "{} spilled separators",
        report.spilled_separators
    );
    let tree = Tree::open(&store, root).unwrap();
    assert_eq!(walk_all(&tree).unwrap().len(), 700);
    let mut gen = Gen::new(3);
    compare(&store, root, &model, &mut rng, &mut gen, 20);
    // Delete most of it again, from the middle out.
    let mut last = root;
    for (e, chunk) in order.chunks(80).take(8).enumerate() {
        let entry = e as u64 + 100;
        let edits: Vec<Edit> = chunk
            .iter()
            .map(|i| {
                let mut segment = base.clone();
                segment.extend(i.to_be_bytes());
                Edit::delete(vec![segment])
            })
            .collect();
        for edit in &edits {
            model.apply(entry, edit);
        }
        last = apply(&store, last, entry, &edits).unwrap();
        check(&store, last).unwrap();
    }
    compare(&store, last, &model, &mut rng, &mut gen, 120);
}

/// Appends in key order, one per entry, fill their pages.
#[test]
fn appends_fill_their_pages() {
    let store = MemStore::new(Numbering::Sequential);
    let (root, _, _) = records(&store, 3000, 1, 40);
    let report = check(&store, root).unwrap();
    // Each item takes 5 + 18 + 40 bytes; a full leaf holds about 259.
    let per_leaf = report.items as f64 / report.leaves as f64;
    assert!(per_leaf > 200.0, "{per_leaf:.0} items per leaf");
}

/// Review of #697: a root that collapses onto an index page the entry did
/// not copy, which itself has one child, keeps collapsing down to a leaf.
#[test]
fn a_root_collapses_through_pages_the_entry_did_not_copy() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let p = vec![0x42u8; 3600];
    let mut last = p.clone();
    last.push(0x43);
    let mut edits: Vec<Edit> = (1..=99u8)
        .map(|i| Edit::list(vec![p.clone(), vec![i]]))
        .collect();
    edits.push(Edit::list(vec![last.clone()]));
    let root = apply(&store, root, 2, &edits).unwrap();
    assert!(check(&store, root).unwrap().height >= 4);
    let root = apply(&store, root, 3, &[Edit::delete(vec![p])]).unwrap();
    let report = check(&store, root).unwrap();
    assert_eq!((report.height, report.items), (1, 1));
    assert_eq!(
        Tree::open(&store, root).unwrap().get(&[last]).unwrap(),
        Some(Node::List)
    );
    // The leaf with that one item, written by entry 3, as the reviewer
    // computed it on its own.
    assert!(root.hash.to_hex().starts_with("75309266"), "{}", root.hash);
}

/// Every overflow page a tree reaches.
fn overflow_pages(store: &MemStore, root: Root) -> Vec<PageNo> {
    check(store, root)
        .unwrap()
        .pages
        .iter()
        .filter(|(n, h)| store.read(*n, h).unwrap().kind() == om_store::page::PageKind::Overflow)
        .map(|(n, _)| *n)
        .collect()
}

/// Review of #697: listing a node's children reads no value but the
/// children's own, in either direction.
#[test]
fn listing_children_reads_only_their_own_values() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let edits = [Edit::leaf(
        vec![b"a".to_vec(), b"b".to_vec()],
        vec![7; 5000],
    )];
    let root = apply(&store, root, 2, &edits).unwrap();
    let overflow = overflow_pages(&store, root);
    assert_eq!(overflow.len(), 1);
    store.remove(overflow[0]);
    let tree = Tree::open(&store, root).unwrap();
    for direction in [Direction::Forward, Direction::Backward] {
        let kids: Vec<_> = tree
            .children::<Vec<u8>>(&[], Bound::Unbounded, direction)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kids, vec![(b"a".to_vec(), Node::List)], "{direction:?}");
    }
    // The child's own value is read, and its loss is named.
    let err = tree
        .children(&[b"a".to_vec()], Bound::Unbounded, Direction::Backward)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_err();
    assert!(matches!(err, Error::Page(f) if f.page == overflow[0] && f.fault == Fault::Missing));
}

/// Review of #697: "changed since" is answered from index pages; a leaf at
/// an end of the range is not read.
#[test]
fn changed_since_reads_no_leaf() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let edits: Vec<Edit> = (1..=5u8)
        .map(|i| Edit::leaf(vec![vec![i]], vec![i; 3992]))
        .collect();
    let root = apply(&store, root, 2, &edits).unwrap();
    let report = check(&store, root).unwrap();
    assert_eq!(report.height, 2);
    // The first leaf, after the root.
    store.remove(report.pages[1].0);
    let tree = Tree::open(&store, root).unwrap();
    assert!(tree.changed_since(&[vec![1u8]], 1).unwrap());
    assert!(!tree.changed_since(&[vec![1u8]], 2).unwrap());
    assert!(tree.changed_since(&[vec![5u8]], 1).unwrap());
}

/// Review of #697: a store call that fails during an entry, whichever call it
/// is, leaves the store as it was: the old root reads the same, no number
/// stays taken, no page is added and no release applied. Applying the entry
/// again then gives what an undisturbed store gives, down to page numbers
/// and the release log.
#[test]
fn a_failed_entry_leaves_the_store_as_it_was() {
    let p = |s: &[&[u8]]| s.iter().map(|s| s.to_vec()).collect::<Path>();
    let record = |r: u32| vec![b"r".to_vec(), r.to_be_bytes().to_vec()];
    let entries: Vec<Vec<Edit>> = vec![
        // New leaves, a spilled value, missing ancestors.
        vec![
            Edit::leaf(p(&[b"a"]), vec![1; 10]),
            Edit::leaf(p(&[b"b", b"c"]), vec![2; 9000]),
        ],
        // A replaced value; a value that comes to hold both.
        vec![
            Edit::leaf(p(&[b"a"]), vec![3; 20]),
            Edit::leaf(p(&[b"a", b"x"]), vec![4]),
        ],
        // Splits and a new root level.
        (0..40)
            .map(|r| Edit::leaf(record(r), vec![r as u8; 3000]))
            .collect(),
        // A list over a subtree with a spilled value; a delete.
        vec![Edit::list(p(&[b"b"])), Edit::delete(p(&[b"a", b"x"]))],
        // Nothing changes.
        vec![Edit::delete(p(&[b"none"]))],
        // A range over many pages: joins, and the root collapses.
        vec![Edit::delete(p(&[b"r"]))],
    ];
    let calm = MemStore::new(Numbering::Sequential);
    let rough = MemStore::new(Numbering::Sequential);
    let mut calm_root = create(&calm, 1).unwrap();
    let mut rough_root = create(&rough, 1).unwrap();
    let mut model = Model::default();
    let state = |s: &MemStore| (s.numbers(), s.taken(), s.released(), s.written());
    for (e, edits) in entries.iter().enumerate() {
        let entry = e as u64 + 2;
        let start = calm.calls();
        let want = apply(&calm, calm_root, entry, edits).unwrap();
        let calls = calm.calls() - start;
        let listing = model.listing();
        for n in 0..calls {
            let before = state(&rough);
            rough.fail_call(Some(n));
            let result = apply(&rough, rough_root, entry, edits);
            rough.fail_call(None);
            assert!(
                result.is_err(),
                "entry {entry}: call {n} of {calls} failed unseen"
            );
            assert!(
                state(&rough) == before,
                "entry {entry}: call {n} left a trace"
            );
            let tree = Tree::open(&rough, rough_root).unwrap();
            assert!(
                walk_all(&tree).unwrap() == listing,
                "entry {entry}: call {n}"
            );
        }
        let got = apply(&rough, rough_root, entry, edits).unwrap();
        assert_eq!(got, want, "entry {entry}");
        assert!(state(&rough) == state(&calm), "entry {entry}");
        for edit in edits {
            model.apply(entry, edit);
        }
        calm_root = want;
        rough_root = got;
    }
}

/// The children of an index page, as (page, hash), and their counts.
fn index_children(store: &MemStore, number: PageNo, hash: Hash) -> Vec<(PageNo, Hash, usize)> {
    let page = store.read(number, &hash).unwrap();
    page.references()[..page.count()]
        .iter()
        .map(|(n, h)| (*n, *h, store.read(*n, h).unwrap().count()))
        .collect()
}

/// Re-check of #697, format section 2.7's example: the hint in an index page
/// is the place of the second part of the split child. Twenty-one empty
/// lists whose keys share 3,600 bytes fill leaves four at a time; the 21st
/// makes the index of six leaves too big, and it splits before its sixth
/// child (5 and 1), not in the middle (3 and 3).
#[test]
fn an_index_page_splits_at_the_second_part_of_its_split_child() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let edits: Vec<Edit> = (1..=21u8)
        .map(|i| {
            let mut segment = vec![0x42u8; 3600];
            segment.push(i);
            Edit::list(vec![segment])
        })
        .collect();
    let root = apply(&store, root, 2, &edits).unwrap();
    let top = index_children(&store, root.page, root.hash);
    let counts: Vec<usize> = top.iter().map(|c| c.2).collect();
    assert_eq!(counts, vec![5, 1]);
    let leaves: Vec<usize> = top
        .iter()
        .flat_map(|(n, h, _)| index_children(&store, *n, *h))
        .map(|c| c.2)
        .collect();
    assert_eq!(leaves, vec![4, 4, 4, 4, 4, 1]);
    assert_eq!(
        root.hash.to_hex(),
        "a56ee5392a0d5484a3f4902a81d2d1edef060bb8b341c97f17f62a7f4d54667c",
    );
}

/// Re-check of #697: a reverse listing reads the overflow pages of a
/// descendant's key only as far as the child's own key goes.
#[test]
fn a_reverse_listing_reads_a_long_key_only_through_the_child() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let child = vec![0x63u8; 1100];
    let edits = [Edit::list(vec![child.clone(), vec![0x67u8; 20_000]])];
    let root = apply(&store, root, 2, &edits).unwrap();
    let pages = check(&store, root).unwrap().pages;
    // The leaf, then the grandchild's two overflow pages.
    assert_eq!(pages.len(), 3);
    store.remove(pages[2].0);
    let tree = Tree::open(&store, root).unwrap();
    for direction in [Direction::Forward, Direction::Backward] {
        let kids: Vec<_> = tree
            .children::<Vec<u8>>(&[], Bound::Unbounded, direction)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kids, vec![(child.clone(), Node::List)], "{direction:?}");
    }
}

/// Re-check of #697: "changed since" compares separators by the bytes
/// their page holds, and reads none whole.
#[test]
fn changed_since_reads_no_separator_it_need_not() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    let edits: Vec<Edit> = (1..=16u8)
        .map(|i| {
            let mut segment = vec![0x42u8; 4200];
            segment.push(i);
            Edit::list(vec![segment])
        })
        .collect();
    let root = apply(&store, root, 2, &edits).unwrap();
    let top = store.read(root.page, &root.hash).unwrap();
    assert_eq!(top.count(), 2);
    // The separator's overflow page follows the two children.
    let refs = top.references();
    assert_eq!(refs.len(), 3);
    store.remove(refs[2].0);
    let tree = Tree::open(&store, root).unwrap();
    assert!(tree.changed_since(&[vec![1u8]], 1).unwrap());
    assert!(!tree.changed_since(&[vec![1u8]], 2).unwrap());
}

/// Final re-check of #697: a key that ends exactly where the bytes a page
/// holds of a longer stored key end is decided without that key's overflow
/// pages, in a lookup and in a reverse listing.
#[test]
fn a_comparison_decided_at_a_page_boundary_reads_no_further() {
    let store = MemStore::new(Numbering::Sequential);
    let root = create(&store, 1).unwrap();
    // `[p]` encodes to 1,024 bytes, all of which its spilled child's item
    // keeps in its page.
    let p = vec![0x70u8; 1022];
    let edits = [
        Edit::list(vec![p.clone()]),
        Edit::list(vec![p.clone(), vec![0x67u8; 3500]]),
    ];
    let root = apply(&store, root, 2, &edits).unwrap();
    let pages = check(&store, root).unwrap().pages;
    assert_eq!(pages.len(), 2, "the leaf and the child's overflow page");
    store.remove(pages[1].0);
    let tree = Tree::open(&store, root).unwrap();
    assert_eq!(
        tree.get(std::slice::from_ref(&p)).unwrap(),
        Some(Node::List)
    );
    for direction in [Direction::Forward, Direction::Backward] {
        let kids: Vec<_> = tree
            .children::<Vec<u8>>(&[], Bound::Unbounded, direction)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kids, vec![(p.clone(), Node::List)], "{direction:?}");
    }
}
