//! A page store in memory, for tests: it keeps every page it was given,
//! numbers pages in one of two ways, lets a test damage, swap or drop a
//! stored page, and can fail any one of its calls.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{EntryPages, Fault, Hash, Page, PageNo, PageStore, Release, StoreError};

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
    counter: u64,
    released: Vec<(PageNo, Hash, Release)>,
    written: Vec<PageNo>,
    /// Calls made so far, and the one to fail.
    calls: u64,
    fail_at: Option<u64>,
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
                counter: 0,
                released: Vec::new(),
                written: Vec::new(),
                calls: 0,
                fail_at: None,
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

/// The changes of one entry to a [`MemStore`].
pub struct MemEntry<'a> {
    store: &'a MemStore,
    /// The store's counter when the entry began, and after its last
    /// allocation, so that a dropped entry can give its numbers back to the
    /// counter when nothing else allocated since.
    counter_at_begin: u64,
    counter_after: u64,
    allocated: Vec<PageNo>,
    pages: Vec<Arc<Page>>,
    released: Vec<(PageNo, Hash, Release)>,
    committed: bool,
}

impl PageStore for MemStore {
    type Entry<'a> = MemEntry<'a>;

    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let mut inner = self.lock();
        if inner.fails() {
            return Err(Fault::Io("a failure the test asked for".into()));
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
        Ok(MemEntry {
            store: self,
            counter_at_begin: inner.counter,
            counter_after: inner.counter,
            allocated: Vec::new(),
            pages: Vec::new(),
            released: Vec::new(),
            committed: false,
        })
    }
}

impl EntryPages for MemEntry<'_> {
    fn allocate(&mut self) -> Result<PageNo, StoreError> {
        let mut inner = self.store.lock();
        if inner.fails() {
            return Err(injected());
        }
        inner.counter += 1;
        let number = match self.store.numbering {
            Numbering::Sequential => inner.counter,
            Numbering::Scattered(seed) => mix(inner.counter ^ seed),
        };
        if !inner.taken.insert(number) {
            return Err(StoreError::Failed(format!("page {number} allocated twice")));
        }
        self.counter_after = inner.counter;
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

    fn commit(mut self) -> Result<(), StoreError> {
        let mut inner = self.store.lock();
        if inner.fails() {
            return Err(injected());
        }
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
            }
        }
        inner.released.append(&mut self.released);
        self.committed = true;
        Ok(())
    }
}

impl Drop for MemEntry<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut inner = self.store.lock();
        for number in &self.allocated {
            inner.taken.remove(number);
        }
        if inner.counter == self.counter_after {
            inner.counter = self.counter_at_begin;
        }
    }
}
