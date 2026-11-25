//! What a reader gets of a hosted file (docs/server-storage.md 5, "Views"):
//! [`FileEngine::read`] gives a [`Reading`], one version of the file held
//! for as long as the reader keeps it, and through it a [`Read`]: the
//! view of that version ([`View`]), the state derived from it
//! ([`Derived`]), what reads leave waiting for a write ([`Waiting`]), and
//! whether the file is hosted read-only.
//!
//! The file engine's view holds the store's read lock, as a reader holding
//! `Hosted::read_store` did: a reading costs what taking that lock costs,
//! and the derived state is the store's own, shared ([`Arc`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLockReadGuard};

use om_hbam::OwnedNode;

use super::view::{Child, Direction, Head, ReadError, Step, Version, View};
use super::FileEngine;
use crate::derived::Derived;
use crate::hosting::Hosted;
use crate::store::{IndexId, MemberKey, Store};

/// Per catalog path: the last member id and modification count issued.
type IssuedKeys = HashMap<Vec<Vec<u8>>, (u64, u64)>;

/// What reads of a file leave waiting for a write to store
/// (`engine`, "Drafts"): the member ids and modification counts
/// GenerateMemberKey issued per catalog, and the indexes (table, field,
/// kind) finds built on demand. A read adds to them while it holds its
/// view; the file engine takes them under the store's write lock, so no
/// read adds to them between taking and storing. State of the running
/// server, not of the file's tree: the store engine will make issuing a
/// member key and building an index operations of their own.
#[derive(Debug, Default)]
pub struct Waiting {
    issued: Mutex<IssuedKeys>,
    on_demand: Mutex<BTreeSet<IndexId>>,
}

impl Waiting {
    /// The member keys issued, also after a request panicked holding them.
    /// Every section that holds them reads, inserts or drains whole entries
    /// (an entry is inserted only once both its checked increments
    /// succeeded), so the map is whole even then. None of them can panic
    /// today; this keeps one that someday does from failing every later
    /// query on the file, each of which asks whether keys are waiting.
    fn issued(&self) -> MutexGuard<'_, IssuedKeys> {
        self.issued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Issue the next member id and modification count of the catalog at
    /// `catalog`: one more than the larger of what is `stored` in it and
    /// what was issued since. `None`, noting nothing, past the largest a
    /// serial key holds.
    pub fn issue_member_key(&self, catalog: &[Vec<u8>], stored: (u64, u64)) -> Option<(u64, u64)> {
        let mut issued = self.issued();
        let last = issued.get(catalog).copied().unwrap_or_default();
        // The stored values come from the file: past the largest a serial
        // key holds there is no next one to issue, and nothing is noted as
        // issued.
        let next = (
            last.0.max(stored.0).checked_add(1)?,
            last.1.max(stored.1).checked_add(1)?,
        );
        issued.insert(catalog.to_vec(), next);
        Some(next)
    }

    /// Whether GenerateMemberKey issued ids not yet written.
    pub fn has_member_keys(&self) -> bool {
        !self.issued().is_empty()
    }

    /// Take the member ids and modification counts issued and not yet
    /// written, by catalog: they are no longer waiting.
    pub fn take_member_keys(&mut self) -> Vec<MemberKey> {
        let mut keys: Vec<MemberKey> = self
            .issued
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .collect();
        keys.sort();
        keys
    }

    /// Put `keys`, taken by [`Self::take_member_keys`] and not written,
    /// back among those waiting.
    pub fn put_back_member_keys(&mut self, keys: Vec<MemberKey>) {
        let issued = self
            .issued
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for (catalog, (id, count)) in keys {
            let waiting = issued.entry(catalog).or_default();
            *waiting = (waiting.0.max(id), waiting.1.max(count));
        }
    }

    /// Note an index a find built on demand, due to be stored.
    pub fn note_index(&self, index: IndexId) {
        self.on_demand.lock().expect("index lock").insert(index);
    }

    /// Take `built` back from the indexes due to be stored.
    pub fn forget_indexes(&self, built: &[IndexId]) {
        let mut due = self.on_demand.lock().expect("index lock");
        for id in built {
            due.remove(id);
        }
    }

    /// The indexes noted as due to be stored, in order.
    pub fn indexes(&self) -> Vec<IndexId> {
        self.on_demand
            .lock()
            .expect("index lock")
            .iter()
            .cloned()
            .collect()
    }

    /// Whether finds built indexes that are not yet stored.
    pub fn has_indexes(&self) -> bool {
        !self.on_demand.lock().expect("index lock").is_empty()
    }

    /// The last member id and modification count issued for `catalog`
    /// that wait to be written (`ServerHandle::member_keys`).
    #[cfg(debug_assertions)]
    pub(crate) fn issued_for(&self, catalog: &[Vec<u8>]) -> Option<(u64, u64)> {
        self.issued().get(catalog).copied()
    }

    /// The indexes built on demand and the member keys issued that wait,
    /// each in order (`Store::waiting_for_tests`).
    #[cfg(all(test, debug_assertions))]
    pub(crate) fn snapshot(&self) -> (Vec<IndexId>, Vec<MemberKey>) {
        let indexes = self
            .on_demand
            .lock()
            .expect("index lock")
            .iter()
            .cloned()
            .collect();
        let mut keys: Vec<MemberKey> = self.issued().iter().map(|(c, k)| (c.clone(), *k)).collect();
        keys.sort();
        (indexes, keys)
    }

    /// Put `indexes`, taken by [`Self::take_indexes`] and not stored, back
    /// among those waiting.
    pub fn put_back_indexes(&mut self, indexes: Vec<IndexId>) {
        self.on_demand
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(indexes);
    }

    /// Take the indexes due to be stored: they are no longer waiting.
    pub fn take_indexes(&mut self) -> Vec<IndexId> {
        std::mem::take(self.on_demand.get_mut().expect("index lock"))
            .into_iter()
            .collect()
    }
}

/// The file engine's view of a hosted file: its store's tree, under the
/// store's read lock, which the view holds for as long as it lives.
pub struct FileView<'h> {
    store: RwLockReadGuard<'h, Store>,
}

impl View for FileView<'_> {
    fn version(&self) -> Version {
        self.store.tree_view().version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        Ok(self.store.tree_view().head(path))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<std::borrow::Cow<'_, OwnedNode>>, ReadError> {
        Ok(self
            .store
            .tree_view()
            .lend(path, limit)?
            .map(std::borrow::Cow::Borrowed))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: std::ops::Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'v>>,
    ) -> Result<bool, ReadError> {
        self.store
            .tree_view()
            .children_into(path, from, direction, max, out)
    }

    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: std::ops::Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<std::borrow::Cow<'v, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        self.store
            .tree_view()
            .subtrees_into(path, from, direction, max, limit, out)
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        Ok(self.store.tree_view().walk_into(path, after, max, out))
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.store.tree_view().changed_since(path, since)
    }

    fn count_children(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        self.store.tree_view().count_children(path)
    }
}

#[cfg(debug_assertions)]
thread_local! {
    /// How many readings this thread holds ([`FileEngine::read`]).
    static HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Debug builds: panic when this thread holds a reading. A reading holds
/// the store's read lock: a second one on the same thread, or a write,
/// waits behind a writer queued meanwhile, which waits for the first
/// (#421). Every reading and every submitted operation checks it, so a
/// test that takes the store's lock twice on one thread fails at once
/// rather than hang when a writer happens to queue.
pub(crate) fn assert_no_reading_held(what: &str) {
    #[cfg(debug_assertions)]
    HELD.with(|held| {
        assert!(
            held.get() == 0,
            "{what} while this thread holds a reading of the file"
        );
    });
    #[cfg(not(debug_assertions))]
    let _ = what;
}

/// A reader's hold on one version of a hosted file ([`FileEngine::read`]).
pub struct Reading<'h> {
    file: &'h Hosted,
    view: FileView<'h>,
    derived: Arc<Derived>,
    /// The test view the reads go through instead ([`super::copies`]).
    #[cfg(debug_assertions)]
    copies: Option<Arc<super::copies::Switch>>,
}

/// What a read gets of one version of a file: its view, the state derived
/// from it, what reads leave waiting for a write, and whether the file is
/// hosted read-only. What it borrows lives as long as the [`Reading`].
#[derive(Clone, Copy)]
pub struct Read<'r> {
    pub view: &'r dyn View,
    pub derived: &'r Derived,
    pub waiting: &'r Waiting,
    pub read_only: bool,
    /// Why the file takes no more changes, if it takes none (a save's
    /// outcome was unknown, `Store::writes_refused`): the file's state.
    pub writes_refused: Option<&'r str>,
}

impl FileEngine {
    /// The version of `file` its store holds now, for reading: the store's
    /// read lock, held until the [`Reading`] is dropped.
    pub fn read<'h>(&self, file: &'h Hosted) -> Reading<'h> {
        assert_no_reading_held("a reading taken");
        let store = file.read_store();
        #[cfg(debug_assertions)]
        HELD.with(|held| held.set(held.get() + 1));
        let derived = store.derived().clone();
        let view = FileView { store };
        // The derived state was computed from this version or an earlier
        // one the tree changed since.
        debug_assert!(
            derived.version() == view.version()
                || view.changed_since(&[], derived.version()) == Ok(true)
        );
        Reading {
            file,
            view,
            derived,
            #[cfg(debug_assertions)]
            copies: self.copies(),
        }
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        HELD.with(|held| held.set(held.get().saturating_sub(1)));
    }
}

impl<'h> Reading<'h> {
    /// The file engine's own read of this version.
    pub fn read(&self) -> Read<'_> {
        Read {
            view: &self.view,
            derived: &self.derived,
            waiting: self.view.store.waiting(),
            read_only: self.view.store.is_read_only(),
            writes_refused: self.view.store.writes_refused(),
        }
    }

    /// Answer `read` from this version. The running server reads through
    /// the file engine's view once. A test that asked for the test view
    /// (`Server::checking_reads`) answers through that one, and first
    /// through the file engine's to check that both answer the same
    /// ([`super::copies`]): `read` then runs twice, and must not count on
    /// running once. A read of the accounts goes through
    /// [`Self::serve_with_accounts`]: the test view reads them only for it.
    pub fn serve<T: PartialEq + std::fmt::Debug>(&self, mut read: impl FnMut(Read<'_>) -> T) -> T {
        #[cfg(debug_assertions)]
        if let Some(copies) = &self.copies {
            return copies.serve(self, false, read);
        }
        read(self.read())
    }

    /// [`Self::serve`] for a read of the file's accounts
    /// (`derived::Derived::accounts`): a login and the other readers of
    /// them. The running server reads as [`Self::serve`] does; the test
    /// view reads the accounts through itself for this read alone, and for
    /// no other.
    pub fn serve_with_accounts<T: PartialEq + std::fmt::Debug>(
        &self,
        mut read: impl FnMut(Read<'_>) -> T,
    ) -> T {
        #[cfg(debug_assertions)]
        if let Some(copies) = &self.copies {
            return copies.serve(self, true, read);
        }
        read(self.read())
    }

    /// [`Self::serve`] for a read that may run only once (one that issues
    /// member keys): through the test view alone, unchecked, when a test
    /// asked for it.
    pub fn serve_once<T>(&self, read: impl FnOnce(Read<'_>) -> T) -> T {
        #[cfg(debug_assertions)]
        if let Some(copies) = &self.copies {
            return copies.serve_once(self, read);
        }
        read(self.read())
    }

    /// How an export writes this version (`super::export`): the file
    /// engine's choice, `Store::export_source`. A file on disk is opened
    /// here, under the store's read lock.
    pub fn export_source(&self) -> std::io::Result<super::export::Source> {
        self.view.store.export_source(&self.file.at)
    }

    /// How a compacted copy of this version is written
    /// (`Store::compacted_source`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn compacted_source(&self) -> super::export::Source {
        self.view.store.compacted_source()
    }

    /// The file on disk the store read or last saved, opened to read while
    /// its path still leads to it, with what a copy needs
    /// (`Store::on_disk`).
    pub fn on_disk(&self) -> std::io::Result<super::export::OnDisk> {
        self.view.store.on_disk(&self.file.at)
    }

    /// Whether [`Self::serve`] runs its read twice.
    pub fn runs_twice(&self) -> bool {
        #[cfg(debug_assertions)]
        if let Some(copies) = &self.copies {
            return copies.checks();
        }
        false
    }

    /// The hosted file read.
    #[cfg(debug_assertions)]
    pub(super) fn file(&self) -> &'h Hosted {
        self.file
    }

    /// The file engine's view.
    #[cfg(debug_assertions)]
    pub(super) fn file_view(&self) -> &FileView<'h> {
        &self.view
    }

    /// The file engine's derived state.
    #[cfg(debug_assertions)]
    pub(super) fn derived(&self) -> &Arc<Derived> {
        &self.derived
    }
}
