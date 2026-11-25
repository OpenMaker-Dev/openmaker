//! A page store in memory, for tests: it keeps every page it was given,
//! numbers pages in one of two ways, lets a test damage, swap or drop a
//! stored page, and can fail any one of its calls.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    EntryPages, Fault, Hash, Page, PageKind, PageNo, PageStore, Release, Root, StoreError, TreeName,
};

/// How a [`MemStore`] numbers new pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    /// 1, 2, 3, ...
    Sequential,
    /// Distinct numbers spread over the whole range, from a seed: a bijective
    /// mix of a counter, so that no two pages share one.
    Scattered(u64),
}

struct Stored {
    bytes: Box<[u8]>,
    /// The checked page, once read (or as written, for a trusting store).
    page: Option<Arc<Page>>,
}

struct Inner {
    pages: BTreeMap<PageNo, Stored>,
    /// Numbers committed or held by an open entry.
    taken: BTreeSet<PageNo>,
    /// Numbers given back by entries that did not use them, handed out
    /// again first, the smallest first.
    free: BTreeSet<PageNo>,
    counter: u64,
    released: Vec<(PageNo, Hash, Release)>,
    written: Vec<PageNo>,
    /// The trees, by name, with their roots.
    trees: BTreeMap<TreeName, Root>,
    /// Whether a unit is open: one at a time, as in the page file, so that
    /// nothing changes the trees between a unit's begin and its commit.
    unit_open: bool,
    /// Calls made so far, and the one to fail.
    calls: u64,
    fail_at: Option<u64>,
    /// Pages whose next reads fail, and how many of them.
    failing: BTreeMap<PageNo, u32>,
}

/// A [`PageStore`] in memory. Pages are never freed, so every root it was
/// given stays readable.
pub struct MemStore {
    numbering: Numbering,
    /// Whether a page handed to `write` is kept as given, without checking
    /// its bytes on the first read, as a page file's cache would keep a page
    /// it wrote. Otherwise every page is checked from its bytes once.
    trusting: bool,
    inner: Mutex<Inner>,
}

fn injected() -> StoreError {
    StoreError::Failed("a failure the test asked for".into())
}

impl MemStore {
    /// A store that checks every page from its bytes on its first read.
    pub fn new(numbering: Numbering) -> MemStore {
        MemStore::with(numbering, false)
    }

    /// A store that keeps the pages it was given as they are, for
    /// measurements.
    pub fn trusting(numbering: Numbering) -> MemStore {
        MemStore::with(numbering, true)
    }

    fn with(numbering: Numbering, trusting: bool) -> MemStore {
        MemStore {
            numbering,
            trusting,
            inner: Mutex::new(Inner {
                pages: BTreeMap::new(),
                taken: BTreeSet::new(),
                free: BTreeSet::new(),
                counter: 0,
                released: Vec::new(),
                written: Vec::new(),
                trees: BTreeMap::new(),
                unit_open: false,
                calls: 0,
                fail_at: None,
                failing: BTreeMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock was held leaves nothing half-done here.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The numbers of every stored page, in order.
    pub fn numbers(&self) -> Vec<PageNo> {
        self.lock().pages.keys().copied().collect()
    }

    /// The numbers committed or held by an open entry, in order.
    pub fn taken(&self) -> Vec<PageNo> {
        self.lock().taken.iter().copied().collect()
    }

    /// How many pages are stored.
    pub fn len(&self) -> usize {
        self.lock().pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The stored bytes of a page.
    pub fn bytes(&self, number: PageNo) -> Option<Vec<u8>> {
        self.lock().pages.get(&number).map(|s| s.bytes.to_vec())
    }

    /// Stores `bytes` at `number`, as a disk that wrote them there would,
    /// whatever was there before.
    pub fn put_bytes(&self, number: PageNo, bytes: &[u8]) {
        let stored = Stored {
            bytes: bytes.into(),
            page: None,
        };
        let mut inner = self.lock();
        inner.pages.insert(number, stored);
        inner.taken.insert(number);
    }

    /// XORs one stored byte with `mask`.
    pub fn damage(&self, number: PageNo, at: usize, mask: u8) -> bool {
        let mut inner = self.lock();
        match inner.pages.get_mut(&number) {
            Some(stored) if at < stored.bytes.len() => {
                stored.bytes[at] ^= mask;
                stored.page = None;
                true
            }
            _ => false,
        }
    }

    /// Exchanges the stored bytes of two pages.
    pub fn swap(&self, a: PageNo, b: PageNo) -> bool {
        let (Some(x), Some(y)) = (self.bytes(a), self.bytes(b)) else {
            return false;
        };
        self.put_bytes(a, &y);
        self.put_bytes(b, &x);
        true
    }

    /// Forgets a stored page, as a disk that never wrote it would.
    pub fn remove(&self, number: PageNo) -> bool {
        self.lock().pages.remove(&number).is_some()
    }

    /// The releases committed so far, in order.
    pub fn released(&self) -> Vec<(PageNo, Hash, Release)> {
        self.lock().released.clone()
    }

    /// The numbers of the pages committed so far, in order.
    pub fn written(&self) -> Vec<PageNo> {
        self.lock().written.clone()
    }

    /// The releases committed since the last call, in order.
    pub fn take_released(&self) -> Vec<(PageNo, Hash, Release)> {
        std::mem::take(&mut self.lock().released)
    }

    /// The numbers of the pages committed since the last call, in order.
    pub fn take_written(&self) -> Vec<PageNo> {
        std::mem::take(&mut self.lock().written)
    }

    /// How many calls the store has had: reads, and `begin`, `allocate`,
    /// `write`, `release` and `commit`.
    pub fn calls(&self) -> u64 {
        self.lock().calls
    }

    /// Makes call number `n` from now (0 for the next) fail, once: a read
    /// with [`Fault::Io`], anything else with [`StoreError::Failed`]. `None`
    /// fails nothing.
    pub fn fail_call(&self, n: Option<u64>) {
        let mut inner = self.lock();
        inner.fail_at = n.map(|n| inner.calls + n);
    }

    /// Makes the next `reads` reads of page `number` fail with
    /// [`Fault::Io`]; 0 fails none.
    pub fn fail_reads(&self, number: PageNo, reads: u32) {
        self.lock().failing.insert(number, reads);
    }
}

impl Inner {
    /// Counts a call; true if it is the one to fail.
    fn fails(&mut self) -> bool {
        let n = self.calls;
        self.calls += 1;
        if self.fail_at == Some(n) {
            self.fail_at = None;
            return true;
        }
        false
    }
}

fn mix(mut z: u64) -> u64 {
    // SplitMix64's finalizer: each step is invertible, so it is a bijection.
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The changes of one entry to a [`MemStore`], the only one open. The
/// numbers it allocates and does not commit go back to the store's free
/// numbers, so an entry retried after a failure numbers its pages as
/// before.
pub struct MemEntry<'a> {
    store: &'a MemStore,
    allocated: Vec<PageNo>,
    pages: Vec<Arc<Page>>,
    released: Vec<(PageNo, Hash, Release)>,
    names: Vec<NameChange>,
    committed: bool,
}

/// A change to the set of trees, applied at commit.
enum NameChange {
    Add(TreeName, Root),
    Drop(TreeName),
    Rename(TreeName, TreeName),
    SetRoot(TreeName, Root),
}

impl PageStore for MemStore {
    type Entry<'a> = MemEntry<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let mut inner = self.lock();
        if inner.fails() {
            return Err(Fault::Io("a failure the test asked for".into()));
        }
        if let Some(left @ 1..) = inner.failing.get_mut(&number) {
            *left -= 1;
            return Err(Fault::Io("a failed read of a page the test named".into()));
        }
        let stored = inner.pages.get_mut(&number).ok_or(Fault::Missing)?;
        if let Some(page) = &stored.page {
            if page.hash() == *hash {
                return Ok(page.clone());
            }
        }
        let page = Arc::new(Page::verify(number, hash, stored.bytes.clone())?);
        stored.page = Some(page.clone());
        Ok(page)
    }

    fn begin(&self) -> Result<MemEntry<'_>, StoreError> {
        let mut inner = self.lock();
        if inner.fails() {
            return Err(injected());
        }
        if inner.unit_open {
            return Err(StoreError::Failed(
                "a unit while another is open: one at a time".into(),
            ));
        }
        inner.unit_open = true;
        Ok(MemEntry {
            store: self,
            allocated: Vec::new(),
            pages: Vec::new(),
            released: Vec::new(),
            names: Vec::new(),
            committed: false,
        })
    }

    fn trees(&self) -> Vec<(TreeName, Root)> {
        let inner = self.lock();
        inner.trees.iter().map(|(n, r)| (n.clone(), *r)).collect()
    }

    fn root(&self, name: &TreeName) -> Option<Root> {
        self.lock().trees.get(name).copied()
    }
}

impl EntryPages for MemEntry<'_> {
    fn allocate(&mut self) -> Result<PageNo, StoreError> {
        let mut inner = self.store.lock();
        if inner.fails() {
            return Err(injected());
        }
        let number = match inner.free.pop_first() {
            Some(number) => number,
            None => {
                inner.counter += 1;
                match self.store.numbering {
                    Numbering::Sequential => inner.counter,
                    Numbering::Scattered(seed) => mix(inner.counter ^ seed),
                }
            }
        };
        if !inner.taken.insert(number) {
            return Err(StoreError::Failed(format!("page {number} allocated twice")));
        }
        self.allocated.push(number);
        Ok(number)
    }

    fn write(&mut self, page: Arc<Page>) -> Result<(), StoreError> {
        if self.store.lock().fails() {
            return Err(injected());
        }
        let number = page.number();
        if !self.allocated.contains(&number) || self.pages.iter().any(|p| p.number() == number) {
            return Err(StoreError::Failed(format!(
                "page {number} written without being allocated, or twice"
            )));
        }
        self.pages.push(page);
        Ok(())
    }

    fn release(&mut self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError> {
        if self.store.lock().fails() {
            return Err(injected());
        }
        self.released.push((number, *hash, how));
        Ok(())
    }

    fn add_tree(&mut self, name: &TreeName, root: Root) {
        self.names.push(NameChange::Add(name.clone(), root));
    }

    fn drop_tree(&mut self, name: &TreeName) {
        self.names.push(NameChange::Drop(name.clone()));
    }

    fn rename_tree(&mut self, from: &TreeName, to: &TreeName) {
        self.names
            .push(NameChange::Rename(from.clone(), to.clone()));
    }

    fn set_root(&mut self, name: &TreeName, root: Root) {
        self.names.push(NameChange::SetRoot(name.clone(), root));
    }

    fn commit(mut self) -> Result<(), StoreError> {
        let mut inner = self.store.lock();
        if inner.fails() {
            return Err(injected());
        }
        // The changes to the trees and the releases, checked as the page
        // file checks them (`EntryPages`), before anything takes effect: a
        // root given is a tree's page this unit wrote, given to one tree at
        // most once; a tree given a new root had its old root released by
        // this unit, and a page released that is a tree's root is such an
        // old root; a tree dropped is one the unit gave no root, and its
        // root is released here; a page is released once, is stored, has
        // the hash given, and is not one this unit allocated.
        let refused = |why: String| Err(StoreError::Failed(why));
        let written: BTreeMap<PageNo, &Arc<Page>> =
            self.pages.iter().map(|p| (p.number(), p)).collect();
        let mut trees = inner.trees.clone();
        let mut named = BTreeSet::new();
        let mut replaced: BTreeMap<PageNo, Hash> = BTreeMap::new();
        let mut drops = Vec::new();
        for change in &self.names {
            let mut new_root = |root: &Root| {
                written
                    .get(&root.page)
                    .is_some_and(|p| p.hash() == root.hash && p.kind() != PageKind::Overflow)
                    && named.insert(root.page)
            };
            match change {
                NameChange::Add(name, root) => {
                    if trees.contains_key(name) {
                        return refused(format!("tree {name} exists"));
                    }
                    if !new_root(root) {
                        return refused(format!("the root of {name} is not a tree's page this unit wrote, or named twice"));
                    }
                    trees.insert(name.clone(), *root);
                }
                NameChange::Drop(name) => match trees.remove(name) {
                    Some(old) if !written.contains_key(&old.page) => {
                        drops.push((old.page, old.hash, Release::Dropped))
                    }
                    Some(_) => {
                        return refused(format!(
                            "tree {name} was made or given a root by this unit"
                        ))
                    }
                    None => return refused(format!("no tree {name}")),
                },
                NameChange::Rename(from, to) => {
                    let Some(root) = trees.remove(from) else {
                        return refused(format!("no tree {from}"));
                    };
                    if trees.insert(to.clone(), root).is_some() {
                        return refused(format!("tree {to} exists"));
                    }
                }
                NameChange::SetRoot(name, root) => {
                    let Some(old) = trees.get(name).copied() else {
                        return refused(format!("no tree {name}"));
                    };
                    if written.contains_key(&old.page) {
                        return refused(format!(
                            "tree {name} was made or given a root by this unit"
                        ));
                    }
                    if !new_root(root) {
                        return refused(format!("the root of {name} is not a tree's page this unit wrote, or named twice"));
                    }
                    replaced.insert(old.page, old.hash);
                    trees.insert(name.clone(), *root);
                }
            }
        }
        // No page this unit writes refers to a page that is a tree's root
        // once the unit takes effect (a root it gives, or one of a tree it
        // leaves alone): dropping either tree would free a page the other
        // still reaches. This store counts no shares, so none is allowed.
        let roots_after: BTreeSet<PageNo> = trees.values().map(|r| r.page).collect();
        let referring = self.pages.iter().find_map(|page| {
            page.references()
                .into_iter()
                .find(|(n, _)| roots_after.contains(n))
                .map(|(n, _)| (page.number(), n))
        });
        if let Some((from, to)) = referring {
            return refused(format!("page {from} refers to page {to}, a tree's root"));
        }
        let root_pages: BTreeSet<PageNo> = inner.trees.values().map(|r| r.page).collect();
        let mut seen = BTreeSet::new();
        for (number, hash, _) in self.released.iter().chain(&drops) {
            let stored = inner.pages.get(number).is_some_and(|s| match &s.page {
                Some(page) => page.hash() == *hash,
                None => Page::verify(*number, hash, s.bytes.clone()).is_ok(),
            });
            if !seen.insert(*number) || self.allocated.contains(number) || !stored {
                return refused(format!(
                    "page {number} released twice, unstored, its own, or with another hash"
                ));
            }
        }
        for (number, hash, _) in &self.released {
            if root_pages.contains(number) && replaced.get(number) != Some(hash) {
                return refused(format!(
                    "page {number}, a tree's root, released without a new root"
                ));
            }
        }
        if let Some(old) = replaced
            .keys()
            .find(|n| !self.released.iter().any(|(r, _, _)| r == *n))
        {
            return refused(format!(
                "page {old}, a tree's root, replaced but not released"
            ));
        }
        self.released.append(&mut drops);
        inner.trees = trees;
        for page in self.pages.drain(..) {
            let number = page.number();
            let stored = Stored {
                bytes: page.bytes().into(),
                page: self.store.trusting.then_some(page),
            };
            inner.pages.insert(number, stored);
            inner.written.push(number);
        }
        for number in &self.allocated {
            if !inner.pages.contains_key(number) {
                inner.taken.remove(number);
                inner.free.insert(*number);
            }
        }
        inner.released.append(&mut self.released);
        self.committed = true;
        Ok(())
    }
}

impl Drop for MemEntry<'_> {
    fn drop(&mut self) {
        let mut inner = self.store.lock();
        inner.unit_open = false;
        if self.committed {
            return;
        }
        for number in &self.allocated {
            inner.taken.remove(number);
            inner.free.insert(*number);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped entry gives its numbers back, so a retry numbers pages as
    /// an undisturbed store would.
    #[test]
    fn a_dropped_entry_gives_its_numbers_back() {
        let store = MemStore::new(Numbering::Sequential);
        let mut a = store.begin().unwrap();
        assert_eq!(a.allocate().unwrap(), 1);
        drop(a);
        let mut b = store.begin().unwrap();
        assert_eq!(b.allocate().unwrap(), 1);
    }
}
