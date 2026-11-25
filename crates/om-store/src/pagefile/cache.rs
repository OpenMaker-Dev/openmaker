//! The page cache (`docs/server-storage.md` section 6.6): pages that are on
//! disk, keyed by page number and hash, up to a fixed number of pages,
//! least recently used first out. It never invalidates: a page on disk
//! never changes, and a number reused for another page has another hash,
//! so a lookup with the old hash misses. Pages not written yet are not
//! here: the page file keeps them apart, and they cannot be evicted.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::page::{Hash, Page, PageNo};

pub(crate) struct Cache {
    capacity: usize,
    tick: u64,
    /// Lookups only; eviction follows `order`, so nothing depends on the
    /// map's iteration order.
    pages: HashMap<PageNo, (Arc<Page>, u64)>,
    order: BTreeMap<u64, PageNo>,
    hits: u64,
    misses: u64,
}

impl Cache {
    /// A cache of at most `capacity` pages.
    pub fn new(capacity: usize) -> Cache {
        Cache {
            capacity,
            tick: 0,
            pages: HashMap::new(),
            order: BTreeMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Hits and misses so far.
    pub fn counts(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Page `number` if the cache holds it with `hash`, as most recently
    /// used.
    pub fn get(&mut self, number: PageNo, hash: &Hash) -> Option<Arc<Page>> {
        self.tick += 1;
        let tick = self.tick;
        match self.pages.get_mut(&number) {
            Some((page, used)) if page.hash() == *hash => {
                self.order.remove(used);
                *used = tick;
                self.order.insert(tick, number);
                self.hits += 1;
                Some(page.clone())
            }
            _ => {
                self.misses += 1;
                None
            }
        }
    }

    /// Keeps `page`, in place of any page cached under its number, and
    /// evicts the least recently used past the capacity.
    pub fn insert(&mut self, page: Arc<Page>) {
        if self.capacity == 0 {
            return;
        }
        self.tick += 1;
        let number = page.number();
        if let Some((_, used)) = self.pages.insert(number, (page, self.tick)) {
            self.order.remove(&used);
        }
        self.order.insert(self.tick, number);
        while self.pages.len() > self.capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.pages.remove(&oldest);
        }
    }

    /// Forgets page `number` (it was freed).
    pub fn remove(&mut self, number: PageNo) {
        if let Some((_, used)) = self.pages.remove(&number) {
            self.order.remove(&used);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{Builder, PageKind};

    fn page(number: PageNo, byte: u8) -> Arc<Page> {
        let mut b = Builder::new(PageKind::Overflow, 0, 1);
        b.data(&[byte; 10]);
        Arc::new(b.finish(number).unwrap())
    }

    #[test]
    fn keyed_by_number_and_hash_and_bounded() {
        let mut cache = Cache::new(2);
        let (a, b, c) = (page(1, 1), page(2, 2), page(3, 3));
        cache.insert(a.clone());
        cache.insert(b.clone());
        assert!(cache.get(1, &a.hash()).is_some());
        cache.insert(c.clone());
        // Page 2 was the least recently used.
        assert!(cache.get(2, &b.hash()).is_none());
        assert!(cache.get(1, &a.hash()).is_some());
        assert_eq!(cache.len(), 2);
        // Number 1 reused for another page: the old hash misses.
        let a2 = page(1, 9);
        cache.insert(a2.clone());
        assert!(cache.get(1, &a.hash()).is_none());
        assert!(cache.get(1, &a2.hash()).is_some());
        cache.remove(1);
        assert!(cache.get(1, &a2.hash()).is_none());
    }
}
