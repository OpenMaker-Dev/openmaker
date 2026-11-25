//! Times the page trees in memory: SHA-256 and page checks, a bulk load of
//! about a million small items, single-put entries into it, batched random
//! puts, and lookups. Run with
//! `cargo run --release -p om-store --example tree_timing [items]`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use om_store::page::{
    EntryPages, Fault, Hash, MemStore, Numbering, Page, PageNo, PageStore, Release, StoreError,
    TreeName, PAGE_SIZE,
};
use om_store::tree::{self, check, Edit, Error, Root, Tree};

/// Applies the edits of entry `entry` to the tree `root` names (`None`: a
/// tree made) in a unit of work of its own, committed. These trees are not
/// named in the store: the store only keeps their pages.
fn apply_in<S: PageStore + ?Sized>(
    store: &S,
    root: Option<Root>,
    entry: u64,
    edits: &[Edit],
) -> Result<Root, Error> {
    let mut unit = store.begin()?;
    let new = tree::apply(store, &mut unit, root, entry, edits)?;
    unit.commit()?;
    Ok(new)
}

fn apply<S: PageStore + ?Sized>(
    store: &S,
    root: Root,
    entry: u64,
    edits: &[Edit],
) -> Result<Root, Error> {
    apply_in(store, Some(root), entry, edits)
}

/// A tree with no nodes: one empty leaf, written by entry `entry`.
fn create<S: PageStore + ?Sized>(store: &S, entry: u64) -> Result<Root, Error> {
    apply_in(store, None, entry, &[])
}

/// Pages in memory, freed as soon as the newest version no longer reaches
/// them, as a page file with no older version in use would. `MemStore` keeps
/// every version, which is not what is being timed.
#[derive(Default)]
struct Newest {
    pages: Mutex<(HashMap<PageNo, Arc<Page>>, PageNo)>,
}

impl Newest {
    fn len(&self) -> usize {
        self.pages.lock().unwrap().0.len()
    }

    fn drop_page(map: &mut HashMap<PageNo, Arc<Page>>, number: PageNo, how: Release) {
        if let Some(page) = map.remove(&number) {
            if how == Release::Dropped {
                for (n, _) in page.references() {
                    Newest::drop_page(map, n, how);
                }
            }
        }
    }
}

impl PageStore for Newest {
    type Entry<'a> = NewestEntry<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let pages = self.pages.lock().unwrap();
        let page = pages.0.get(&number).ok_or(Fault::Missing)?;
        if page.hash() != *hash {
            return Err(Fault::Stale { found: page.hash() });
        }
        Ok(page.clone())
    }

    /// The trees are not named here: the measurement keeps their roots.
    fn trees(&self) -> Vec<(TreeName, Root)> {
        Vec::new()
    }

    fn root(&self, _: &TreeName) -> Option<Root> {
        None
    }

    fn begin(&self) -> Result<NewestEntry<'_>, StoreError> {
        Ok(NewestEntry {
            store: self,
            pages: Vec::new(),
            released: Vec::new(),
        })
    }
}

/// An entry's pages and releases, kept until it commits. (Numbers it took
/// are not given back if it is dropped: nothing here fails.)
struct NewestEntry<'a> {
    store: &'a Newest,
    pages: Vec<Arc<Page>>,
    released: Vec<(PageNo, Release)>,
}

impl EntryPages for NewestEntry<'_> {
    fn allocate(&mut self) -> Result<PageNo, StoreError> {
        let mut pages = self.store.pages.lock().unwrap();
        pages.1 += 1;
        Ok(pages.1)
    }

    fn write(&mut self, page: Arc<Page>) -> Result<(), StoreError> {
        self.pages.push(page);
        Ok(())
    }

    fn release(&mut self, number: PageNo, _: &Hash, how: Release) -> Result<(), StoreError> {
        self.released.push((number, how));
        Ok(())
    }

    fn add_tree(&mut self, _: &TreeName, _: Root) {}

    fn drop_tree(&mut self, _: &TreeName) {}

    fn rename_tree(&mut self, _: &TreeName, _: &TreeName) {}

    fn set_root(&mut self, _: &TreeName, _: Root) {}

    fn commit(self) -> Result<(), StoreError> {
        let mut pages = self.store.pages.lock().unwrap();
        for page in self.pages {
            pages.0.insert(page.number(), page);
        }
        for (number, how) in self.released {
            Newest::drop_page(&mut pages.0, number, how);
        }
        Ok(())
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

fn path(record: u32) -> Vec<Vec<u8>> {
    vec![vec![0x80, 0x01], vec![5], record.to_be_bytes().to_vec()]
}

fn per(what: &str, n: usize, start: Instant) {
    let total = start.elapsed();
    println!(
        "{what}: {n} in {:.2} s, {:.2} us each",
        total.as_secs_f64(),
        total.as_secs_f64() * 1e6 / n as f64
    );
}

fn main() {
    let items: u32 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(1_000_000);
    let mut rng = Rng(1);

    // Hashing.
    let block = vec![0x5au8; PAGE_SIZE];
    let n = 20_000;
    let start = Instant::now();
    let mut h = Hash::default();
    for _ in 0..n {
        h = Hash::of(&block);
    }
    per("SHA-256 of 16 KiB", n, start);
    std::hint::black_box(h);

    // A bulk load in key order, 1,000 puts per entry.
    let store = Newest::default();
    let mut root = create(&store, 1).unwrap();
    let mut entry = 1;
    let start = Instant::now();
    let mut r = 0;
    while r < items {
        entry += 1;
        let edits: Vec<Edit> = (r..(r + 1000).min(items))
            .map(|r| Edit::leaf(path(r), (r as u64).to_le_bytes().repeat(2)))
            .collect();
        root = apply(&store, root, entry, &edits).unwrap();
        r += 1000;
    }
    per(
        "bulk load, puts in key order, 1,000 per entry",
        items as usize,
        start,
    );
    let report = check(&store, root).unwrap();
    println!(
        "  {} items, height {}, {} leaves ({:.0} items each), {} index pages, {} pages stored",
        report.items,
        report.height,
        report.leaves,
        report.items as f64 / report.leaves as f64,
        report.index_pages,
        store.len()
    );

    // Checking a page as a read from disk would.
    let page = report
        .pages
        .iter()
        .map(|(n, h)| store.read(*n, h).unwrap())
        .max_by_key(|page| page.count())
        .unwrap();
    let leaf = (page.number(), page.hash());
    let bytes = page.bytes().to_vec();
    let n = 20_000;
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(
            Page::verify(leaf.0, &leaf.1, bytes.clone().into_boxed_slice()).unwrap(),
        );
    }
    per(
        &format!("Page::verify of a full leaf ({} items)", page.count()),
        n,
        start,
    );

    // One put per entry, at random records, existing or new.
    let start = Instant::now();
    let n = 20_000;
    for _ in 0..n {
        entry += 1;
        let record = (rng.next() % (items as u64 * 11 / 10)) as u32;
        let edit = Edit::leaf(path(record), rng.next().to_le_bytes().to_vec());
        root = apply(&store, root, entry, &[edit]).unwrap();
    }
    per("entries of one random put", n, start);

    // Random puts, 100 per entry.
    let start = Instant::now();
    let n = 100_000;
    for _ in 0..n / 100 {
        entry += 1;
        let edits: Vec<Edit> = (0..100)
            .map(|_| {
                let record = (rng.next() % (items as u64 * 11 / 10)) as u32;
                Edit::leaf(path(record), rng.next().to_le_bytes().to_vec())
            })
            .collect();
        root = apply(&store, root, entry, &edits).unwrap();
    }
    per("random puts, 100 per entry", n, start);

    gets(&store, root, &mut rng, items, "random gets, pages cached");

    // The same tree read from bytes: every page checked on its first read.
    println!("  {} pages held", store.len());
    let cold = MemStore::new(Numbering::Sequential);
    for page in store.pages.lock().unwrap().0.values() {
        cold.put_bytes(page.number(), page.bytes());
    }
    gets(
        &cold,
        root,
        &mut rng,
        items,
        "random gets, each page checked on first read",
    );

    let tree = Tree::open(&store, root).unwrap();
    let start = Instant::now();
    let n = tree.walk::<Vec<u8>>(&[]).map(Result::unwrap).count();
    per("walk of every node", n, start);
}

fn gets<S: PageStore>(store: &S, root: Root, rng: &mut Rng, items: u32, what: &str) {
    let tree = Tree::open(store, root).unwrap();
    let n = 1_000_000;
    let start = Instant::now();
    let mut found = 0;
    for _ in 0..n {
        let record = (rng.next() % items as u64) as u32;
        found += usize::from(tree.get(&path(record)).unwrap().is_some());
    }
    per(what, n, start);
    assert_eq!(found, n);
}
