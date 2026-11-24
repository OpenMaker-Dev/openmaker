//! A page store in memory, for tests: it keeps every page it was given,
//! numbers pages in one of two ways, and lets a test damage, swap or drop a
//! stored page.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{Fault, Hash, Page, PageNo, PageStore, Release, StoreError};

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
    allocated: BTreeSet<PageNo>,
    counter: u64,
    released: Vec<(PageNo, Hash, Release)>,
    written: Vec<PageNo>,
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
                allocated: BTreeSet::new(),
                counter: 0,
                released: Vec::new(),
                written: Vec::new(),
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
        self.lock().pages.insert(number, stored);
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

    /// The releases received since the last call, in order.
    pub fn take_released(&self) -> Vec<(PageNo, Hash, Release)> {
        std::mem::take(&mut self.lock().released)
    }

    /// The numbers of the pages written since the last call, in order.
    pub fn take_written(&self) -> Vec<PageNo> {
        std::mem::take(&mut self.lock().written)
    }
}

fn mix(mut z: u64) -> u64 {
    // SplitMix64's finalizer: each step is invertible, so it is a bijection.
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl PageStore for MemStore {
    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        let mut inner = self.lock();
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

    fn allocate(&self) -> Result<PageNo, StoreError> {
        let mut inner = self.lock();
        inner.counter += 1;
        let number = match self.numbering {
            Numbering::Sequential => inner.counter,
            Numbering::Scattered(seed) => mix(inner.counter ^ seed),
        };
        if !inner.allocated.insert(number) {
            return Err(StoreError::Failed(format!("page {number} allocated twice")));
        }
        Ok(number)
    }

    fn write(&self, page: Arc<Page>) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let number = page.number();
        if !inner.allocated.contains(&number) || inner.pages.contains_key(&number) {
            return Err(StoreError::Failed(format!(
                "page {number} written without being allocated, or twice"
            )));
        }
        let stored = Stored {
            bytes: page.bytes().into(),
            page: self.trusting.then_some(page),
        };
        inner.pages.insert(number, stored);
        inner.written.push(number);
        Ok(())
    }

    fn release(&self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError> {
        self.lock().released.push((number, *hash, how));
        Ok(())
    }
}
