//! The map of free pages (`docs/server-storage.md` section 6.7): which pages
//! of the page file are free, which are dying and when they may be reused,
//! and how many trees share each shared page (section 6.8). Pure
//! bookkeeping: no I/O, no clock, no randomness, and no iteration over a
//! hash map, so that the same calls give the same map.
//!
//! **Lifespans.** Every page that leaves the newest version is recorded
//! with its *birth*, the version that wrote it, and its *death*, the first
//! version that no longer reaches it. Versions are ordered pairs
//! ([`Version`]): an entry's changes are version `(entry, 0)`, and the
//! pages a checkpoint writes for itself (its meta stream) are version
//! `(entry, checkpoint number)`. A dying page is still reached by every
//! version from its birth up to, not including, its death, and is reused
//! only when none of those versions is kept: no checkpoint or restore point
//! on disk, no checkpoint being written and no pin lies in `[birth,
//! death)`.
//!
//! **Shares.** Without clones every page is reached by one tree, and no
//! record is kept for it. A page shared by several trees has a [`Share`]
//! with its count. Counts follow Rodeh's method: a clone raises the count
//! of its root only; when a shared page is copied, the copy shares its
//! references, whose counts are raised then (lazily, page by page); when a
//! page's count reaches zero it dies, and if it was dropped (not copied)
//! the counts of what it refers to go down in turn. That last step is
//! lazy: a dropped page is recorded with `expand`, and its references are
//! taken down when its bytes are next at hand (in memory, or read by
//! [`super::PageFile::reclaim`]), never before it is freed.

use std::collections::{BTreeMap, BTreeSet};

use crate::page::{Hash, PageNo};

/// A version of the page file's content: an entry, and a step within it
/// (0 for the entry's own changes, a checkpoint's number for the pages the
/// checkpoint writes for itself). Ordered by entry, then step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub entry: u64,
    pub step: u64,
}

impl Version {
    /// The version an entry's changes make.
    pub const fn at(entry: u64) -> Version {
        Version { entry, step: 0 }
    }
}

/// A page that left the newest version and is not free yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub hash: Hash,
    /// The version that wrote it; `None` until its bytes are read.
    pub birth: Option<Version>,
    /// The first version that does not reach it.
    pub death: Version,
    /// It was dropped with what it refers to, and its references have not
    /// been taken down yet.
    pub expand: bool,
    /// The first checkpoint whose map records it settled, when a map on
    /// disk records it unsettled: its bytes stay until every checkpoint on
    /// disk is at least that one, since opening an older one needs them.
    /// 0 when no map on disk needs them.
    pub hold: u64,
    /// A map on disk (or the checkpoint being written) records it
    /// unsettled, or records in use a page above it that it records
    /// unsettled: settling it sets `hold`. Not written; a map read back
    /// sets it for every record it holds unsettled.
    pub carried: bool,
}

impl Pending {
    /// Its birth is known and what it refers to taken down: it needs its
    /// bytes no more.
    pub fn settled(&self) -> bool {
        self.birth.is_some() && !self.expand
    }
}

/// How many trees reach a shared page, and the latest version at which a
/// reference to it went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Share {
    pub count: u32,
    pub last: Version,
}

/// A page a dropped page refers to, as the expansion of its parent finds
/// it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Child {
    pub number: PageNo,
    pub hash: Hash,
    /// Its birth when the parent records it (an index page's children).
    pub birth: Option<Version>,
    /// Whether it can refer to pages itself (leaves and index pages; not
    /// overflow pages).
    pub expand: bool,
}

/// Why a change to the map was refused. Nothing was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The page is not in use: it is free, dying, beyond the file, or
    /// page 0.
    NotInUse(PageNo),
}

/// What a release did to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Released {
    /// Another tree still reaches it.
    Shared,
    /// It is dying.
    Died,
}

/// The serialized form of the map, as a checkpoint writes it
/// (`docs/server-storage-format.md` section 4.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Image {
    pub end: PageNo,
    /// One bit per page, page `n` at bit `n % 8` of byte `n / 8`; set when
    /// free.
    pub free: Vec<u8>,
    pub pending: Vec<(PageNo, Pending)>,
    pub shares: Vec<(PageNo, Share)>,
}

/// The map (module docs).
#[derive(Clone, Debug, Default)]
pub(crate) struct Map {
    /// Pages `1..end` exist; page 0 is the superblock slots.
    end: PageNo,
    /// One bit per page below `end`, set when free.
    free: Vec<u64>,
    free_count: u64,
    /// The first word that may hold a free bit.
    hint: usize,
    pending: BTreeMap<PageNo, Pending>,
    shares: BTreeMap<PageNo, Share>,
}

impl Map {
    /// A map of `end` pages, none of them free.
    pub fn new(end: PageNo) -> Map {
        let mut map = Map::default();
        map.end = end.max(1);
        map.free = vec![0; words(map.end)];
        map
    }

    pub fn end(&self) -> PageNo {
        self.end
    }

    pub fn free_count(&self) -> u64 {
        self.free_count
    }

    pub fn pending(&self) -> &BTreeMap<PageNo, Pending> {
        &self.pending
    }

    pub fn pending_mut(&mut self) -> &mut BTreeMap<PageNo, Pending> {
        &mut self.pending
    }

    pub fn shares(&self) -> &BTreeMap<PageNo, Share> {
        &self.shares
    }

    pub fn is_free(&self, n: PageNo) -> bool {
        n < self.end && self.free[(n / 64) as usize] & (1 << (n % 64)) != 0
    }

    /// Whether `n` is a page of the newest version or of a unit being
    /// built: below `end`, not page 0, not free and not dying.
    pub fn in_use(&self, n: PageNo) -> bool {
        n != 0 && n < self.end && !self.is_free(n) && !self.pending.contains_key(&n)
    }

    /// The file grows to `end` pages; the new ones are free.
    pub fn grow(&mut self, end: PageNo) {
        if end <= self.end {
            return;
        }
        let old = self.end;
        self.end = end;
        self.free.resize(words(end), 0);
        for n in old..end {
            self.set_free(n);
        }
        self.hint = self.hint.min((old / 64) as usize);
    }

    fn set_free(&mut self, n: PageNo) {
        let (w, b) = ((n / 64) as usize, n % 64);
        debug_assert_eq!(self.free[w] & (1 << b), 0, "page {n} freed twice");
        self.free[w] |= 1 << b;
        self.free_count += 1;
        self.hint = self.hint.min(w);
    }

    /// Takes the lowest free page.
    pub fn take_lowest(&mut self) -> Option<PageNo> {
        while self.hint < self.free.len() {
            let word = self.free[self.hint];
            if word != 0 {
                let b = u64::from(word.trailing_zeros());
                self.free[self.hint] &= !(1 << b);
                self.free_count -= 1;
                return Some(self.hint as u64 * 64 + b);
            }
            self.hint += 1;
        }
        None
    }

    /// Gives back page `n`, taken and not used.
    pub fn give(&mut self, n: PageNo) {
        self.set_free(n);
    }

    /// Frees dying page `n`.
    pub fn free_pending(&mut self, n: PageNo) {
        if self.pending.remove(&n).is_some() {
            self.set_free(n);
        }
    }

    /// Records page `n` as dying from `death`. `n` must be in use.
    pub fn die(
        &mut self,
        n: PageNo,
        hash: Hash,
        birth: Option<Version>,
        death: Version,
        expand: bool,
    ) -> Result<(), Refused> {
        self.die_carried(n, hash, birth, death, expand, false)
    }

    fn die_carried(
        &mut self,
        n: PageNo,
        hash: Hash,
        birth: Option<Version>,
        death: Version,
        expand: bool,
        carried: bool,
    ) -> Result<(), Refused> {
        if !self.in_use(n) {
            return Err(Refused::NotInUse(n));
        }
        self.shares.remove(&n);
        self.pending.insert(
            n,
            Pending {
                hash,
                birth,
                death,
                expand,
                hold: 0,
                carried,
            },
        );
        Ok(())
    }

    /// Marks every unsettled record as carried by a map on disk (a
    /// checkpoint is writing them so).
    pub fn carry_unsettled(&mut self) {
        for record in self.pending.values_mut() {
            if !record.settled() {
                record.carried = true;
            }
        }
    }

    /// The count of trees that reach page `n`.
    pub fn count(&self, n: PageNo) -> u32 {
        self.shares.get(&n).map_or(1, |s| s.count)
    }

    /// Another tree reaches page `n` now (a clone of it, or a copy of a
    /// shared page that refers to it).
    pub fn share(&mut self, n: PageNo) {
        let share = self.shares.entry(n).or_insert(Share {
            count: 1,
            last: Version::default(),
        });
        share.count = share.count.saturating_add(1);
    }

    /// A tree stops reaching page `n` at version `at`, the way `how` says.
    /// `references` are the page's references, needed (and given) only
    /// when the page is shared and copied: the copy shares them. Returns
    /// whether the page died; a page that dies after being dropped is
    /// recorded to expand.
    pub fn release(
        &mut self,
        n: PageNo,
        hash: Hash,
        birth: Option<Version>,
        at: Version,
        dropped: bool,
        references: Option<&[(PageNo, Hash)]>,
    ) -> Result<Released, Refused> {
        if !self.in_use(n) {
            return Err(Refused::NotInUse(n));
        }
        match self.shares.get_mut(&n) {
            Some(share) if share.count > 1 => {
                share.count -= 1;
                share.last = share.last.max(at);
                if !dropped {
                    for &(r, _) in references.unwrap_or(&[]) {
                        self.share(r);
                    }
                }
                Ok(Released::Shared)
            }
            Some(share) => {
                let death = share.last.max(at);
                self.die(n, hash, birth, death, dropped)?;
                Ok(Released::Died)
            }
            None => {
                self.die(n, hash, birth, at, dropped)?;
                Ok(Released::Died)
            }
        }
    }

    /// Takes down the references of dying page `n`, which was dropped:
    /// each child loses one count, and a child whose count reaches zero
    /// dies at the later of `n`'s death and the last release of the child.
    /// Returns the children that died. A child that is not in use is
    /// returned in `twice`: something released it on its own as well.
    pub fn expand(&mut self, n: PageNo, children: &[Child]) -> (Vec<PageNo>, Vec<PageNo>) {
        let Some(record) = self.pending.get_mut(&n).filter(|r| r.expand) else {
            return (Vec::new(), Vec::new());
        };
        record.expand = false;
        let death = record.death;
        let carried = record.carried;
        let mut died = Vec::new();
        let mut twice = Vec::new();
        for child in children {
            if !self.in_use(child.number) {
                twice.push(child.number);
                continue;
            }
            let child_death = match self.shares.get_mut(&child.number) {
                Some(share) if share.count > 1 => {
                    share.count -= 1;
                    share.last = share.last.max(death);
                    continue;
                }
                Some(share) => share.last.max(death),
                None => death,
            };
            if self
                .die_carried(
                    child.number,
                    child.hash,
                    child.birth,
                    child_death,
                    child.expand,
                    carried,
                )
                .is_ok()
            {
                died.push(child.number);
            }
        }
        (died, twice)
    }

    /// The map as a checkpoint writes it, with the pages in `also_free`
    /// (numbers taken by units that have not committed) counted as free.
    pub fn image(&self, also_free: &BTreeSet<PageNo>) -> Image {
        let mut free = vec![0u8; bytes(self.end)];
        for (w, word) in self.free.iter().enumerate() {
            for b in 0..8 {
                let at = w * 8 + b;
                if at < free.len() {
                    free[at] = (word >> (b * 8)) as u8;
                }
            }
        }
        for &n in also_free {
            if n < self.end {
                free[(n / 8) as usize] |= 1 << (n % 8);
            }
        }
        Image {
            end: self.end,
            free,
            // What a reader of the image derives, not the running state.
            pending: self
                .pending
                .iter()
                .map(|(n, p)| {
                    let carried = !p.settled();
                    (*n, Pending { carried, ..*p })
                })
                .collect(),
            shares: self.shares.iter().map(|(n, s)| (*n, *s)).collect(),
        }
    }

    /// The map an image describes, checked: it must be one a checkpoint
    /// could have written.
    pub fn from_image(image: &Image) -> Result<Map, &'static str> {
        if image.end == 0 {
            return Err("a map of no pages");
        }
        if image.free.len() != bytes(image.end) {
            return Err("a free bitmap of the wrong length");
        }
        let mut map = Map::new(image.end);
        for (i, byte) in image.free.iter().enumerate() {
            for b in 0..8 {
                if byte & (1 << b) == 0 {
                    continue;
                }
                let n = i as u64 * 8 + b;
                if n == 0 || n >= image.end {
                    return Err("a free bit outside the pages");
                }
                map.set_free(n);
            }
        }
        let mut last = 0;
        for &(n, pending) in &image.pending {
            if n <= last || n >= image.end || map.is_free(n) {
                return Err("a dying page out of order, outside the pages, or free");
            }
            if pending.birth.is_some_and(|b| b >= pending.death) {
                return Err("a dying page born at or after its death");
            }
            let carried = !pending.settled();
            map.pending.insert(n, Pending { carried, ..pending });
            last = n;
        }
        let mut last = 0;
        for &(n, share) in &image.shares {
            if n <= last || n >= image.end || !map.in_use(n) || share.count == 0 {
                return Err("a share count out of order, outside the pages, or of no tree");
            }
            map.shares.insert(n, share);
            last = n;
        }
        map.hint = 0;
        Ok(map)
    }
}

fn words(end: PageNo) -> usize {
    end.div_ceil(64) as usize
}

fn bytes(end: PageNo) -> usize {
    end.div_ceil(8) as usize
}

/// The versions kept from reuse: checkpoints and restore points (on disk,
/// or being written), and pins.
#[derive(Clone, Debug, Default)]
pub(crate) struct Kept {
    pub checkpoints: BTreeSet<Version>,
    pub pins: BTreeSet<Version>,
    /// The lowest number of a checkpoint on disk (one an open could use).
    pub on_disk: u64,
}

impl Kept {
    /// Whether a page born at `birth` and dying at `death` is reached by a
    /// kept version: one `v` with `birth <= v < death`. A birth not known
    /// yet counts as the earliest. A checkpoint's own pages (born at a
    /// step above 0) are reached by that checkpoint's version alone, so
    /// only checkpoints and restore points keep them: a pin is of the
    /// trees at an entry.
    pub fn protects(&self, birth: Option<Version>, death: Version) -> bool {
        let from = birth.unwrap_or_default();
        let hit = |set: &BTreeSet<Version>| set.range(from..death).next().is_some();
        hit(&self.checkpoints) || (from.step == 0 && hit(&self.pins))
    }

    /// Whether a dying page can be freed: settled, reached by no kept
    /// version, and not held for a map on disk.
    pub fn frees(&self, record: &Pending) -> bool {
        record.settled()
            && record.hold <= self.on_disk
            && !self.protects(record.birth, record.death)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        Hash([n; 32])
    }

    fn v(entry: u64) -> Version {
        Version::at(entry)
    }

    /// A map of `end` pages with `used` taken, in order, from the lowest.
    fn map_with(end: PageNo, used: u64) -> Map {
        let mut map = Map::new(1);
        map.grow(end);
        for _ in 0..used {
            map.take_lowest().unwrap();
        }
        map
    }

    #[test]
    fn pages_are_taken_lowest_first_and_given_back() {
        let mut map = map_with(10, 0);
        assert_eq!(map.free_count(), 9);
        assert_eq!(map.take_lowest(), Some(1));
        assert_eq!(map.take_lowest(), Some(2));
        map.give(1);
        assert_eq!(map.take_lowest(), Some(1));
        for n in 3..10 {
            assert_eq!(map.take_lowest(), Some(n));
        }
        assert_eq!(map.take_lowest(), None);
        map.grow(130);
        assert_eq!(map.take_lowest(), Some(10));
        assert_eq!(map.free_count(), 119);
    }

    #[test]
    fn a_page_is_protected_by_a_kept_version_in_its_lifespan_only() {
        let own = |entry, step| Version { entry, step };
        let kept = Kept {
            checkpoints: [own(9, 3)].into(),
            pins: [v(5)].into(),
            on_disk: 3,
        };
        assert!(kept.protects(Some(v(3)), v(6)));
        assert!(kept.protects(Some(v(5)), v(6)));
        assert!(!kept.protects(Some(v(6)), v(9)));
        assert!(kept.protects(Some(v(6)), v(10)));
        // A page a checkpoint at 9 wrote for itself, replaced at (12, 4):
        // kept by that checkpoint.
        assert!(kept.protects(Some(own(9, 3)), own(12, 4)));
        // The meta pages of checkpoint (4, 2), replaced at (9, 3), are not
        // kept by the pin at 5 (a pin is of trees) nor by checkpoint 3.
        assert!(!kept.protects(Some(own(4, 2)), own(9, 3)));
        // Unknown birth counts from the start.
        assert!(kept.protects(None, v(6)));
        assert!(!kept.protects(None, v(5)));
    }

    /// Rodeh's counts at the level of the map: a clone shares the root; a
    /// copy of a shared page shares its references; a dropped page that
    /// dies takes down its references, which die only when no tree reaches
    /// them, at the latest of their releases.
    #[test]
    fn share_counts_follow_copies_and_drops() {
        // Pages 1 (root) -> 2, 3 (children) -> 4 (overflow of 3).
        let mut map = map_with(10, 4);
        let child = |n: PageNo, expand| Child {
            number: n,
            hash: h(n as u8),
            birth: Some(v(1)),
            expand,
        };
        // A clone at entry 2: both trees reach page 1.
        map.share(1);
        assert_eq!(map.count(1), 2);
        // Tree A copies its root at entry 3: page 1 stays (tree B), and
        // the copy shares 2 and 3.
        let refs = [(2, h(2)), (3, h(3))];
        assert_eq!(
            map.release(1, h(1), Some(v(1)), v(3), false, Some(&refs)),
            Ok(Released::Shared)
        );
        assert_eq!((map.count(1), map.count(2), map.count(3)), (1, 2, 2));
        // Tree A drops its child 3 at entry 4: tree B still reaches it.
        assert_eq!(
            map.release(3, h(3), Some(v(1)), v(4), true, None),
            Ok(Released::Shared)
        );
        assert_eq!(map.count(3), 1);
        // Tree B drops its root at entry 6: page 1 dies, to expand.
        assert_eq!(
            map.release(1, h(1), Some(v(1)), v(6), true, None),
            Ok(Released::Died)
        );
        assert!(map.pending()[&1].expand);
        // Expanding page 1: 2 is still reached by tree A's copy; 3 dies
        // at 6, the later of its two releases.
        let (died, twice) = map.expand(1, &[child(2, true), child(3, true)]);
        assert_eq!((died, twice), (vec![3], vec![]));
        assert_eq!(map.pending()[&3].death, v(6));
        assert_eq!(map.count(2), 1);
        // Expanding page 3 takes its overflow page down with it.
        let (died, _) = map.expand(3, &[child(4, false)]);
        assert_eq!(died, vec![4]);
        assert!(!map.pending()[&4].expand);
        // Releasing a dying page again is refused, and changes nothing.
        let before = map.image(&BTreeSet::new());
        assert_eq!(
            map.release(3, h(3), None, v(7), true, None),
            Err(Refused::NotInUse(3))
        );
        assert_eq!(map.image(&BTreeSet::new()), before);
        // An expansion that finds a child already dying reports it.
        let (_, twice) = map.expand(1, &[child(3, true)]);
        assert!(twice.is_empty(), "page 1 was expanded already");
    }

    #[test]
    fn an_image_round_trips_and_a_bad_one_is_refused() {
        let mut map = map_with(100, 50);
        map.die(7, h(7), Some(v(2)), v(9), true).unwrap();
        map.die(9, h(9), None, Version { entry: 9, step: 4 }, false)
            .unwrap();
        map.share(11);
        map.free_pending(7);
        let held: BTreeSet<PageNo> = [12, 13].into();
        let image = map.image(&held);
        let again = Map::from_image(&image).unwrap();
        assert_eq!(again.image(&BTreeSet::new()), image);
        assert!(again.is_free(7) && again.is_free(12) && !again.is_free(9));
        assert_eq!(again.free_count(), map.free_count() + 2);

        let mut bad = image.clone();
        bad.free[0] |= 1;
        assert!(Map::from_image(&bad).is_err(), "page 0 free");
        let mut bad = image.clone();
        bad.pending.push((9, bad.pending[0].1));
        assert!(Map::from_image(&bad).is_err(), "a page dying twice");
        let mut bad = image.clone();
        bad.free.pop();
        assert!(Map::from_image(&bad).is_err(), "a short bitmap");
        let mut bad = image;
        bad.shares[0].1.count = 0;
        assert!(Map::from_image(&bad).is_err(), "a share of no tree");
    }
}
