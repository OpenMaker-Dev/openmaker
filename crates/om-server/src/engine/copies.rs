//! A second view of a hosted file, for tests only (debug builds): it shows
//! that readers do not lean on the file engine's (docs/server-storage.md
//! 5, "Views"). [`Copies`] serves the same tree, but hands out every node
//! as an owned copy, gives children and walks three at a time, and fails
//! every read that reaches a path it was told to fail, as an engine fails
//! the reads that reach a damaged page (section 6.12): the node at that
//! path and everything below it.
//!
//! A server switched to it (`Server::checking_reads`) answers each read
//! that goes through [`super::Reading::serve`] (downloads and finds)
//! through the file engine's view first and then through this one, from
//! the same version, and panics when the two answers differ: the test
//! fixtures of `tests/` serve every request so, and a difference fails the
//! test that made it. Told to fail a path (`ServerHandle::failing_reads`),
//! it answers through this view alone, so that the client gets what a
//! failed read gives. Its derived state is computed through it, as another
//! engine would compute its own, and checked against the file engine's.
//!
//! For a find (`Reading::serve_find`: op86's program, a server script's
//! find) it also refuses to give an index whole: a subtree at or above a
//! field's index `[T, 01, F, kind]` is a failed read, so a find that still
//! read an index whole would answer otherwise than the file engine, and the
//! test that made it fails. A find reads an index through listings, an
//! entry at a time (docs/server-storage.md 5, "Views").
//!
//! The store engine's view is checked the same way ([`StoreCopy`]): at a
//! file's first read the switch writes the file engine's tree as an
//! `.fmp12` and imports it into a store in memory, as the store engine
//! imports a file it hosts (`paged::import`), with its derived state
//! computed from that view as the store engine computes it
//! (`store::derived_of`). Every read at that version is then answered a
//! third time, through a [`PagedView`] of the copy, and must give the same
//! answer. The third read takes the file engine's state of the file that
//! is not a version's (read-only, waiting work) with the copy's view and
//! derived state. Reads that are not compared so: one at a later version
//! (the file changed; the store engine takes no changes), one a test told
//! to fail (answered through the test view alone), one that may run only
//! once (`Reading::serve_once`); a file whose copy cannot be made (its
//! tree cannot be written as an `.fmp12`, or the import refuses it) fails
//! its test. The switch counts each kind ([`Compared`],
//! `ServerHandle::store_comparisons`), and with `OM_STORE_CHECK_LOG` set
//! to a file each read notes its kind there with the test's name.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::{Arc, Mutex, PoisonError};

use log::warn;
use om_hbam::OwnedNode;

use super::paged::{Batch, PagedView};
use super::reading::{Read, Reading};
use super::view::{owned_path, Child, Direction, Head, ReadError, Step, Version, View, NO_LIMIT};
use crate::derived::{self, Derived};

/// The most children or nodes of a walk one call gives.
const PAGE: usize = 3;

/// A view over another that copies everything it gives, in small pages,
/// and fails the reads that reach `failing`.
pub(crate) struct Copies<'v> {
    inner: &'v dyn View,
    failing: &'v [Vec<Vec<u8>>],
    /// Whether a subtree at or above a field's index is refused: the view
    /// serves a find (module doc).
    whole_indexes_refused: bool,
}

impl<'v> Copies<'v> {
    pub(crate) fn new(inner: &'v dyn View, failing: &'v [Vec<Vec<u8>>]) -> Self {
        Copies {
            inner,
            failing,
            whole_indexes_refused: false,
        }
    }

    /// The view for a find: it also refuses to give an index whole.
    pub(crate) fn for_finds(self) -> Self {
        Copies {
            whole_indexes_refused: true,
            ..self
        }
    }

    /// Refuse a subtree that holds a field's index whole, for a find: at a
    /// table's value or word index `[T, 01, F, 00]` or `[T, 01, F, 01]`, or
    /// above it (`[T, 01, F]`, `[T, 01]`, `[T]`, the root). An index's
    /// counts (`10`, `11`) are no index.
    fn check_whole_index(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        // The value and word indexes `00` and `01`, not their counts.
        let holds_one = match path {
            [] => true,
            [t] | [t, [1]] | [t, [1], _] | [t, [1], _, [0 | 1]] => is_table(t),
            _ => false,
        };
        if self.whole_indexes_refused && holds_one {
            return Err(ReadError::Unreadable {
                path: owned_path(path),
                why: "the test view refuses a find an index read whole".into(),
            });
        }
        Ok(())
    }

    /// Refuse a page of a field index's entries each with its records, for
    /// a find: a find lists an index's keys ([`View::children_page`]) and
    /// reads an entry's records by its key only for an entry it takes.
    fn check_listed_index(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        if self.whole_indexes_refused && matches!(path, [t, [1], _, [0 | 1]] if is_table(t)) {
            return Err(ReadError::Unreadable {
                path: owned_path(path),
                why: "the test view refuses a find an index's entries listed with their records"
                    .into(),
            });
        }
        Ok(())
    }

    /// The failing path at or above `path`, if any.
    fn failing_at(&self, path: &[&[u8]]) -> Option<&Vec<Vec<u8>>> {
        self.failing.iter().find(|failing| {
            failing.len() <= path.len() && failing.iter().zip(path).all(|(f, p)| f == p)
        })
    }

    /// Fail a read of the node at `path`.
    fn check(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        match self.failing_at(path) {
            Some(failing) => Err(unreadable(failing)),
            None => Ok(()),
        }
    }

    /// Fail a read of the subtree at `path`: at or below a failing path, or
    /// holding one that exists.
    fn check_subtree(&self, path: &[&[u8]]) -> Result<(), ReadError> {
        self.check(path)?;
        for failing in self.failing {
            let below = failing.len() > path.len() && failing.iter().zip(path).all(|(f, p)| f == p);
            let segments: Vec<&[u8]> = failing.iter().map(Vec::as_slice).collect();
            if below && self.inner.node(&segments)?.is_some() {
                return Err(unreadable(failing));
            }
        }
        Ok(())
    }
}

/// Whether `key` is a table's key: a serial key from `80 01` (129) on,
/// below the table occurrences' ids (schema-path.md 5.2, 5.3).
fn is_table(key: &[u8]) -> bool {
    om_hbam::serial::decode(key).is_some_and(|id| (0x80..0x10_4080).contains(&id))
}

fn unreadable(path: &[Vec<u8>]) -> ReadError {
    ReadError::Unreadable {
        path: path.to_vec(),
        why: "the test view was told to fail it".into(),
    }
}

fn child_path<'p>(path: &[&'p [u8]], segment: &'p [u8]) -> Vec<&'p [u8]> {
    let mut path = path.to_vec();
    path.push(segment);
    path
}

impl View for Copies<'_> {
    fn version(&self) -> Version {
        self.inner.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
        self.check(path)?;
        Ok(self.inner.node(path)?.map(Head::into_owned))
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
        self.check_whole_index(path)?;
        self.check_subtree(path)?;
        Ok(self
            .inner
            .subtree(path, limit)?
            .map(|node| Cow::Owned(node.into_owned())))
    }

    fn children_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        out: &mut Vec<Child<'a>>,
    ) -> Result<bool, ReadError> {
        // Listing a node's children reads where the node is, whether it is
        // there or not.
        self.check(path)?;
        let mut page = Vec::new();
        let more = self
            .inner
            .children_page(path, from, direction, max.min(PAGE), &mut page)?;
        for (given, child) in page.into_iter().enumerate() {
            if let Err(error) = self.check(&child_path(path, &child.segment)) {
                // The children before it are given; the next page reaches
                // it first.
                return if given > 0 { Ok(true) } else { Err(error) };
            }
            out.push(Child {
                segment: Cow::Owned(child.segment.into_owned()),
                head: child.head.into_owned(),
            });
        }
        Ok(more)
    }

    fn subtrees_page<'a>(
        &'a self,
        path: &[&[u8]],
        from: Bound<&[u8]>,
        direction: Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<Cow<'a, OwnedNode>>,
    ) -> Result<bool, ReadError> {
        self.check(path)?;
        self.check_listed_index(path)?;
        let mut page = Vec::new();
        let more =
            self.inner
                .subtrees_page(path, from, direction, max.min(PAGE), limit, &mut page)?;
        for (given, child) in page.into_iter().enumerate() {
            let at = child_path(path, &child.segment);
            if let Err(error) = self
                .check_whole_index(&at)
                .and_then(|()| self.check_subtree(&at))
            {
                return if given > 0 { Ok(true) } else { Err(error) };
            }
            out.push(Cow::Owned(child.into_owned()));
        }
        Ok(more)
    }

    fn walk_page<'a>(
        &'a self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<Step<'a>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        // A walk reads where its node is, whether it is there or not, and
        // a page resumed after a node reads where that node is.
        self.check(path)?;
        if let Some(after) = after {
            let mut at = path.to_vec();
            at.extend(after.iter().map(Vec::as_slice));
            self.check(&at)?;
        }
        let mut page = Vec::new();
        let resume = self
            .inner
            .walk_page(path, after, max.min(PAGE), &mut page)?;
        // Each step's path below `path`, followed from where the page
        // starts, and that of the last step given.
        let mut below: Vec<Vec<u8>> = after.map(<[Vec<u8>]>::to_vec).unwrap_or_default();
        let mut last = None;
        for step in page {
            below.truncate(step.depth.saturating_sub(1));
            if step.depth > 0 {
                below.push(step.segment.to_vec());
            }
            let mut at = path.to_vec();
            at.extend(below.iter().map(Vec::as_slice));
            if let Err(error) = self.check(&at) {
                // The steps before it are given; the next page, resumed
                // after the last of them, reaches it first.
                return match last {
                    Some(last) => Ok(Some(last)),
                    None => Err(error),
                };
            }
            last = Some(below.clone());
            out.push(Step {
                depth: step.depth,
                segment: Cow::Owned(step.segment.into_owned()),
                head: step.head.into_owned(),
            });
        }
        Ok(resume)
    }

    fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
        self.inner.changed_since(path, since)
    }
}

impl super::FileEngine {
    /// The test view reads go through, if a test asked for it.
    pub(super) fn copies(&self) -> Option<Arc<Switch>> {
        self.copies
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn switch(&self) -> Arc<Switch> {
        self.copies
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Default::default)
            .clone()
    }

    /// Answer reads through the test view, checked against the file
    /// engine's (`Server::checking_reads`).
    pub(crate) fn check_reads(&self) {
        self.switch();
    }

    /// The reads compared on a store copy so far, and those that were not
    /// (`ServerHandle::store_comparisons`); none without the test view.
    pub(crate) fn store_comparisons(&self) -> Option<Compared> {
        self.copies().map(|switch| switch.compared())
    }

    /// Fail every read of `path` (and below it) in the file named `file`,
    /// answering reads through the test view alone
    /// (`ServerHandle::failing_reads`).
    pub(crate) fn fail_reads(&self, file: &str, path: Vec<Vec<u8>>) {
        self.switch().fail(file, path);
    }
}

/// A server's switch to the test view (`Server::checking_reads`,
/// `ServerHandle::failing_reads`).
pub(crate) struct Switch {
    /// The paths of each file, by name, whose reads fail; when any is set,
    /// reads are answered through the test view alone.
    failing: Mutex<HashMap<String, Vec<Vec<Vec<u8>>>>>,
    /// Each file's derived state as the test view computed it, by the
    /// file engine's it was checked against (the same `Arc` while the
    /// file engine's has not changed).
    derived: Mutex<HashMap<u32, Computed>>,
    /// Each file's copy in a store, by its id and load
    /// (`Derived::instance`): `None` for a file whose tree the import
    /// refuses (it holds what the store engine does not host).
    stores: Mutex<HashMap<(u32, u64), Copied>>,
    /// The test that switched the server (its thread's name), for
    /// `OM_STORE_CHECK_LOG`.
    test: Option<String>,
    /// The reads compared on a store copy, and those that were not, by why.
    counts: Mutex<Compared>,
}

/// How many reads a switched server answered a third time through a store
/// copy and compared, and how many it did not, by why
/// (`ServerHandle::store_comparisons`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compared {
    /// Compared with the store copy's answer.
    pub compared: u64,
    /// At a version after the copy's: the file changed.
    pub changed: u64,
    /// Of a file whose copy could not be made.
    pub no_copy: u64,
    /// Answered through the test view alone, as a test told it to fail.
    pub failing: u64,
    /// A read that may run only once (`Reading::serve_once`).
    pub single_run: u64,
}

impl Default for Switch {
    fn default() -> Self {
        Switch {
            failing: Default::default(),
            derived: Default::default(),
            stores: Default::default(),
            test: std::thread::current().name().map(str::to_owned),
            counts: Default::default(),
        }
    }
}

/// A file's store copy ([`StoreCopy`]), or none where its tree cannot be
/// imported.
type Copied = Option<Arc<StoreCopy>>;

/// A file's tree at one version, imported into a store in memory as the
/// store engine imports a file it hosts, and the state derived from that
/// view as the store engine derives it.
pub(crate) struct StoreCopy {
    store: om_store::page::MemStore,
    root: om_store::page::Root,
    derived: Derived,
    /// The file engine's version the copy holds.
    version: Version,
}

impl StoreCopy {
    /// The copy of the version `reading` holds: `None` where the tree
    /// cannot be written as an `.fmp12` or the import refuses it. Its
    /// derived state must be the file engine's.
    fn of(reading: &Reading<'_>) -> Option<StoreCopy> {
        let view = reading.engine_view();
        let current = reading.derived();
        let root = view.subtree(&[], NO_LIMIT).ok()??.into_owned();
        let bytes = om_hbam::writer::rebuild(&root, current.page_zero()).ok()?;
        let mut store = om_store::page::MemStore::new(om_store::page::Numbering::Sequential);
        let name = om_store::page::TreeName::new(b"copy".to_vec())?;
        let imported = super::paged::import(
            std::io::Cursor::new(bytes),
            &mut store,
            &name,
            1,
            Batch::default(),
        )
        .ok()?;
        let paged = PagedView::open(&store, imported.root).ok()?;
        let mut derived =
            super::store::derived_of(&paged, current.name(), current.page_zero()).ok()?;
        if current.desktop() {
            derived.serve_desktop();
        }
        assert!(
            derived.same_as(current, true),
            "{:?}: the store engine's derived state differs from the file engine's:\n{derived:?}\n{current:?}",
            reading.file().name
        );
        Some(StoreCopy {
            store,
            root: imported.root,
            derived,
            version: view.version(),
        })
    }
}

/// The file engine's derived state of a file, and the test view's computed
/// when that was current.
type Computed = (Arc<Derived>, Arc<Derived>);

impl Switch {
    /// Fail every read of `path` in the file named `file` (and of what is
    /// below it).
    pub(crate) fn fail(&self, file: &str, path: Vec<Vec<u8>>) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(file.to_lowercase())
            .or_default()
            .push(path);
    }

    fn failing(&self) -> Option<HashMap<String, Vec<Vec<Vec<u8>>>>> {
        let failing = self.failing.lock().unwrap_or_else(PoisonError::into_inner);
        (!failing.is_empty()).then(|| failing.clone())
    }

    /// Whether reads are answered twice and compared.
    pub(crate) fn checks(&self) -> bool {
        self.failing().is_none()
    }

    /// `read`, answered through the test view (module doc); its derived
    /// state holds the accounts read through the test view only when
    /// `accounts` (a reader of them, `Reading::serve_with_accounts`).
    pub(crate) fn serve<T: PartialEq + std::fmt::Debug>(
        &self,
        reading: &Reading<'_>,
        (accounts, finds): (bool, bool),
        mut read: impl FnMut(Read<'_>) -> T,
    ) -> T {
        let failing = self.failing();
        let paths = failing
            .as_ref()
            .and_then(|failing| failing.get(&reading.file().name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
        let file_read = reading.read();
        let copies = Copies::new(reading.engine_view(), &paths);
        let derived = self.derived_of(reading, &copies, failing.is_none(), accounts);
        let copies = match finds {
            true => copies.for_finds(),
            false => copies,
        };
        let copied = Read {
            view: &copies,
            derived: &derived,
            ..file_read
        };
        if failing.is_some() {
            self.count(|c| &mut c.failing, "failing");
            return read(copied);
        }
        let expected = read(file_read);
        let answer = read(copied);
        assert_eq!(
            answer,
            expected,
            "{:?}: the test view's answer differs from the file engine's",
            reading.file().name
        );
        if let Some(copy) = self.store_copy(reading) {
            let paged = PagedView::open(&copy.store, copy.root)
                .expect("a copy in memory opens as it opened when it was made");
            let stored = read(Read {
                view: &paged,
                derived: &copy.derived,
                ..file_read
            });
            assert_eq!(
                stored,
                expected,
                "{:?}: the store engine's answer differs from the file engine's",
                reading.file().name
            );
            self.count(|c| &mut c.compared, "compared");
        }
        answer
    }

    /// The reads compared on a store copy so far, and those that were not.
    pub(crate) fn compared(&self) -> Compared {
        *self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The store copy of the file `reading` reads, made at its first read,
    /// when the file is still at the version the copy holds; a later
    /// version is noted as a change.
    fn store_copy(&self, reading: &Reading<'_>) -> Option<Arc<StoreCopy>> {
        let key = (reading.file().id, reading.derived().instance());
        let mut stores = self.stores.lock().unwrap_or_else(PoisonError::into_inner);
        let copy = stores
            .entry(key)
            .or_insert_with(|| StoreCopy::of(reading).map(Arc::new))
            .clone();
        drop(stores);
        // A server checking reads claims the store comparison for its
        // files: one that cannot be copied fails its test (none of the
        // suite's files is such a file).
        let Some(copy) = copy else {
            self.count(|c| &mut c.no_copy, "no-copy");
            panic!(
                "{:?}: no store copy could be made of it, so its reads are not compared on the \
                 store engine's view",
                reading.file().name
            );
        };
        if reading.engine_view().version() == copy.version {
            return Some(copy);
        }
        self.count(|c| &mut c.changed, "changed");
        None
    }

    /// Count a read of the kind `kind` names, and note it in
    /// `OM_STORE_CHECK_LOG` with the test's name.
    fn count(&self, kind: impl FnOnce(&mut Compared) -> &mut u64, what: &str) {
        use std::io::Write;
        *kind(&mut self.counts.lock().unwrap_or_else(PoisonError::into_inner)) += 1;
        let Some(log) = std::env::var_os("OM_STORE_CHECK_LOG") else {
            return;
        };
        let test = self.test.as_deref().unwrap_or("?");
        let binary = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.file_stem()?.to_string_lossy().into_owned()))
            .unwrap_or_default();
        // One write, so that the lines of tests running at once do not mix.
        let line = format!("{binary}\t{test}\t{what}\n");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// `read`, answered through the test view alone, unchecked: a read
    /// that may run only once, of no accounts.
    pub(crate) fn serve_once<T>(
        &self,
        reading: &Reading<'_>,
        finds: bool,
        read: impl FnOnce(Read<'_>) -> T,
    ) -> T {
        let failing = self.failing();
        let paths = failing
            .as_ref()
            .and_then(|failing| failing.get(&reading.file().name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
        let copies = Copies::new(reading.engine_view(), &paths);
        let derived = self.derived_of(reading, &copies, failing.is_none(), false);
        let copies = match finds {
            true => copies.for_finds(),
            false => copies,
        };
        self.count(|c| &mut c.single_run, "single-run");
        read(Read {
            view: &copies,
            derived: &derived,
            ..reading.read()
        })
    }

    /// The derived state of the file `reading` reads, computed through
    /// `copies`: the catalog, the locale and the date order once for each
    /// load of the file, and the catalog again each time the file engine
    /// reads it again (after a schema change); the accounts, read through
    /// `copies` for this read alone, when `accounts` (a reader of them), and
    /// otherwise not read at all, held as unreadable ([`not_read`]), so that
    /// a reader that did not ask for them gets none and the test view copies
    /// no account record for it (a password's verifier included). With
    /// `check` it must equal the file engine's, and failing to compute it
    /// fails the check.
    ///
    /// Without (a test told the view to fail paths), each part of the state
    /// answers for its own readers:
    ///
    /// - The catalog, the locale and the date order are those computed when
    ///   the file was loaded or its schema changed, or the file engine's
    ///   when this view cannot compute them. Their readers also read the
    ///   tree, through this view, and those reads fail where it fails; so
    ///   a damaged part of the catalog fails the reads that reach it and
    ///   leaves the state readers were given.
    /// - The accounts are a reader's whole read: a login reads nothing else
    ///   of the file. They are read for each reader of them, so while a
    ///   failing path reaches what they are computed from
    ///   ([`derived::read_accounts`]: `[23, 1, 5]` and `[23, 2, 5]`, at,
    ///   above or below either) they are unavailable to it, whatever state
    ///   is cached and whichever part failed to compute first, and a login
    ///   gets a failed read's reply.
    fn derived_of(
        &self,
        reading: &Reading<'_>,
        copies: &Copies<'_>,
        check: bool,
        accounts: bool,
    ) -> Arc<Derived> {
        let current = reading.derived();
        let id = reading.file().id;
        let mut cache = self.derived.lock().unwrap_or_else(PoisonError::into_inner);
        let computed = match cache.get(&id) {
            Some((of, mine)) if Arc::ptr_eq(of, current) => Ok(mine.clone()),
            // The same load of the file, its state computed again.
            Some((of, mine)) if of.instance() == current.instance() => {
                // The file engine computed its state again: the tree changed
                // since the version the last one was computed from.
                assert!(
                    copies.changed_since(&[], of.version()).unwrap_or(true),
                    "derived state changed without a change of the version"
                );
                if of.shares_catalog(current) {
                    // Its accounts alone, which this state does not hold.
                    Ok(mine.clone())
                } else {
                    catalog_of(reading, copies)
                        .and_then(|catalog| Ok(Arc::new(mine.with_catalog(copies, catalog)?)))
                }
            }
            // The first read of this load of the file (a file closed and
            // opened again is a new store, whose versions start over).
            _ => catalog_of(reading, copies).and_then(|catalog| {
                let mut mine = Derived::with_parts(copies, catalog, not_read(), current.name())?;
                if current.desktop() {
                    mine.serve_desktop();
                }
                // Not in the tree: what the engine loaded, as the name is.
                mine.set_page_zero(current.page_zero());
                Ok(Arc::new(mine))
            }),
        };
        let derived = match computed {
            Ok(mine) => {
                if check {
                    assert!(
                        mine.same_as(current, false),
                        "{:?}: the test view's derived state differs from the file engine's:\n{mine:?}\n{current:?}",
                        reading.file().name
                    );
                }
                cache.insert(id, (current.clone(), mine.clone()));
                mine
            }
            Err(error) if check => panic!(
                "{:?}: the test view could not compute derived state: {error}",
                reading.file().name
            ),
            Err(error) => {
                warn!(
                    "{:?}: the test view could not compute derived state ({error}); \
                     using the file engine's",
                    reading.file().name
                );
                Arc::new(current.with_accounts(current.version(), not_read()))
            }
        };
        drop(cache);
        if !accounts {
            return derived;
        }
        // Read for this reader, through `copies`: what it copies of each
        // account record is wiped once decoded (`hosting::read_accounts`).
        let with =
            Arc::new(derived.with_accounts(derived.version(), derived::read_accounts(copies)));
        if check {
            assert!(
                with.same_as(current, true),
                "{:?}: the test view's accounts differ from the file engine's:\n{with:?}\n{current:?}",
                reading.file().name
            );
        }
        with
    }
}

/// What the test view's derived state holds for the accounts when its
/// reader did not ask for them (`Switch::derived_of`): an error, so that a
/// reader that reads them without asking fails its test.
fn not_read() -> derived::Accounts {
    Err(ReadError::Unreadable {
        path: vec![vec![23], vec![1], vec![5]],
        why: "the test view reads the accounts only for a reader of them \
              (`Reading::serve_with_accounts`)"
            .into(),
    })
}

/// The catalog the test view's derived state holds: the one it decodes
/// through `copies`, or, when the file's catalog does not decode through
/// either view (the same error from both), the one the file engine kept.
/// The store keeps the catalog it last decoded when a schema change leaves
/// one it cannot decode (`Store::reread_catalog`); no view of this version
/// can give that one, which an earlier version held, so that is no
/// difference between the views. A catalog only the test view fails to
/// decode is its own failure.
fn catalog_of(reading: &Reading<'_>, copies: &Copies<'_>) -> om_hbam::Result<om_hbam::Catalog> {
    match derived::read_catalog(copies) {
        Ok(catalog) => Ok(catalog),
        Err(error) => match derived::read_catalog(reading.engine_view()) {
            Err(theirs) if theirs.to_string() == error.to_string() => {
                Ok(reading.derived().catalog().clone())
            }
            _ => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::view::{wire, TreeView, NO_LIMIT};

    /// The test view reads what the tree view reads, copied and in pages of
    /// three, and fails what is at or below a failing path, and a subtree
    /// or walk that holds one.
    #[test]
    fn the_test_view_refuses_a_find_an_index_read_whole() {
        let mut root = OwnedNode::default();
        let index: [&[u8]; 4] = [&[0x80, 0x01], &[1], &[1], &[0]];
        for key in [[1u8], [2], [3]] {
            let entry: Vec<&[u8]> = index.iter().copied().chain([&key[..]]).collect();
            root.put(&entry, OwnedNode::leaf(vec![0], vec![0, 0, 0, 2]));
        }
        let counts: [&[u8]; 4] = [&[0x80, 0x01], &[1], &[1], &[0x10]];
        root.put(&counts, OwnedNode::leaf(Vec::new(), vec![0; 16]));
        let tree = TreeView::new(&root, 1);
        let plain = Copies::new(&tree, &[]);
        let finds = Copies::new(&tree, &[]).for_finds();
        // An index's counts are no index.
        assert!(finds.subtree(&counts, NO_LIMIT).unwrap().is_some());
        // A find reads an entry, and lists the index's keys a page at a
        // time, but not its entries with their records.
        let entry: Vec<&[u8]> = index.iter().copied().chain([&[2u8][..]]).collect();
        assert!(finds.subtree(&entry, NO_LIMIT).unwrap().is_some());
        let mut keys = Vec::new();
        finds
            .children_page(&index, Bound::Unbounded, Direction::Forward, 10, &mut keys)
            .unwrap();
        assert_eq!(keys.len(), 3);
        let mut page = Vec::new();
        let listed = finds.subtrees_page(
            &index,
            Bound::Unbounded,
            Direction::Forward,
            10,
            NO_LIMIT,
            &mut page,
        );
        assert!(listed.is_err());
        assert!(plain
            .subtrees_page(
                &index,
                Bound::Unbounded,
                Direction::Forward,
                10,
                NO_LIMIT,
                &mut page
            )
            .is_ok());
        // The index whole, or anything above it, is refused; other readers
        // read it as before.
        for depth in 0..=4 {
            let path = &index[..depth];
            assert!(finds.subtree(path, NO_LIMIT).is_err(), "{path:02x?}");
            assert!(
                plain.subtree(path, NO_LIMIT).unwrap().is_some(),
                "{path:02x?}"
            );
        }
        let mut page = Vec::new();
        let above = &index[..3];
        assert!(finds
            .subtrees_page(
                above,
                Bound::Unbounded,
                Direction::Forward,
                10,
                NO_LIMIT,
                &mut page
            )
            .is_err());
    }

    #[test]
    fn the_test_view_copies_pages_and_fails_as_told() {
        let mut root = OwnedNode::default();
        for id in 0..10u8 {
            root.put(&[&[1], &[id], &[0]], OwnedNode::leaf(Vec::new(), vec![id]));
        }
        root.put(&[&[2]], OwnedNode::leaf(Vec::new(), b"two".to_vec()));
        let tree = TreeView::new(&root, 7);
        let failing = vec![vec![vec![1], vec![5]]];
        let copies = Copies::new(&tree, &failing);
        let view: &dyn View = &copies;
        assert_eq!(view.version(), 7);
        // Untouched paths read the same, owned.
        assert!(matches!(
            view.subtree(&[&[2]], NO_LIMIT).unwrap(),
            Some(Cow::Owned(node)) if &node == root.at(&[&[2]]).unwrap()
        ));
        assert_eq!(wire(view, &[&[2]]).unwrap(), wire(&tree, &[&[2]]).unwrap());
        let mut page = Vec::new();
        assert!(view
            .children_page(
                &[&[1]],
                Bound::Unbounded,
                Direction::Forward,
                100,
                &mut page
            )
            .unwrap());
        assert_eq!(page.len(), PAGE);
        // Children before the failing one list; reaching it fails.
        let listed: Vec<Result<Child, ReadError>> = view
            .children(&[&[1]], Bound::Unbounded, Direction::Forward)
            .collect();
        assert_eq!(listed.len(), 6);
        assert!(listed[..5].iter().all(Result::is_ok));
        assert_eq!(listed[5], Err(unreadable(&failing[0])));
        // Backward from below it does not reach it.
        assert_eq!(
            view.children(&[&[1]], Bound::Excluded(&[5]), Direction::Backward)
                .count(),
            5
        );
        // Listing the children of it, or of a node below it, fails, also
        // when there are none to give: the node itself, a missing one, a
        // bound past its last child.
        let failing_list = vec![vec![vec![1], vec![4]], vec![vec![9]]];
        let listing = Copies::new(&tree, &failing_list);
        let listing: &dyn View = &listing;
        for (path, from) in [
            (&[&[1][..], &[4][..]][..], Bound::Unbounded),
            (&[&[1][..], &[4][..], &[0][..]][..], Bound::Unbounded),
            (&[&[9][..]][..], Bound::Unbounded),
            (&[&[1][..], &[4][..]][..], Bound::Excluded(&[7][..])),
        ] {
            let listed: Vec<_> = listing.children(path, from, Direction::Forward).collect();
            assert!(matches!(listed[..], [Err(_)]), "{path:02x?}");
            let whole: Vec<_> = listing
                .subtrees(path, from, Direction::Forward, NO_LIMIT)
                .collect();
            assert!(matches!(whole[..], [Err(_)]), "{path:02x?}");
        }
        assert!(wire(listing, &[&[9]]).is_err());
        // A page resumed after a node below the failing one fails, though
        // what follows that node is not below it.
        let mut out = Vec::new();
        let after = [vec![5], vec![0]];
        assert!(view.walk_page(&[&[1]], Some(&after), 10, &mut out).is_err());
        assert!(out.is_empty());
        // A read at or below it, a subtree or walk holding it, fail.
        assert!(view.node(&[&[1], &[5], &[0]]).is_err());
        assert!(view.node(&[&[1], &[4], &[0]]).is_ok());
        assert!(view.subtree(&[&[1]], NO_LIMIT).is_err());
        assert!(view.subtree(&[], NO_LIMIT).is_err());
        assert!(wire(view, &[&[1]]).is_err());
        assert!(wire(view, &[&[1], &[4]]).is_ok());
        // Without failing paths, every subtree's wire form is the tree's.
        let copies = Copies::new(&tree, &[]);
        for path in [&[][..], &[&[1][..]], &[&[1][..], &[3][..]]] {
            assert_eq!(wire(&copies, path), wire(&tree, path), "{path:02x?}");
        }
    }
}
