//! What the trees need from below: the page store.

use std::fmt;
use std::sync::Arc;

use super::{Fault, Hash, Page, PageNo};

/// How a version being built stops reaching a page the version it was built
/// from reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// The page was replaced by a copy: copy-on-write, a split, a join, or an
    /// index root replaced by its only child. What the page refers to is
    /// still reached by the new version, except what is released on its own.
    Copied,
    /// The page and everything it refers to are no longer reached by the new
    /// version: a deleted range, or the overflow pages of an item deleted or
    /// replaced.
    Dropped,
}

/// The page store could not allocate or take a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// No space for another page.
    Full,
    /// Anything else, with the store's description.
    Failed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Full => f.write_str("the page store is full"),
            StoreError::Failed(why) => write!(f, "the page store failed: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Where the trees keep their pages (`docs/server-storage.md` sections 6.4
/// to 6.8). The page file implements it; [`super::MemStore`] does in memory
/// for tests.
///
/// What an implementation must guarantee:
///
/// - **Reads are checked.** `read(number, hash)` returns the page built by
///   [`Page::verify`] from the bytes stored at `number`, with `hash` as the
///   expected hash, or a page this store was handed by `write` under that
///   number and hash. The tree checks the number and hash of what it gets,
///   and a [`Page`] cannot be made any other way, so a store cannot hand the
///   tree unchecked bytes. A failure is reported as the [`Fault`] found.
/// - **Caching by number and hash.** A cache of pages must be keyed by both,
///   so that a number that was freed and reused never answers for the page
///   that was there before (section 6.4). Published pages never change, so a
///   cache never invalidates anything.
/// - **Pages stay while reached.** A page handed to `write` stays readable
///   under its number and hash for as long as any version, view,
///   checkpoint, restore point, clone or state copy in progress reaches it.
///   Until a checkpoint writes it to disk it is kept in memory (section
///   6.5), so `write` does no I/O on the commit path.
/// - **Numbers are the store's own.** `allocate` returns a number that no
///   reachable page uses. Nothing the tree computes depends on numbers:
///   page hashes leave them out, so two stores that number pages
///   differently hold trees with the same root hashes.
/// - **Releases.** After an entry's new pages are written, the tree calls
///   `release` for every page the version it started from reached and the
///   new version does not, each once, a page before the pages it refers to.
///   [`Release`] says whether what the page refers to is still reached.
///   The store frees a page only when nothing reaches it any more. A store
///   that shares pages between trees (clones, section 6.8) counts the trees
///   that reach each page; on [`Release::Copied`] of a page that another
///   tree still reaches, the copy shares the page's references, so the
///   store raises their counts (Rodeh's method), learning them from
///   [`Page::references`].
/// - **Failures.** An error from `allocate`, `write` or `release` ends the
///   entry's application with that error. Pages written before it are
///   reached by no root, and the store reclaims them.
///
/// Every method takes `&self`: a store shared between the writer and
/// readers synchronizes itself.
pub trait PageStore {
    /// The page at `number`, checked against `hash`, the hash its parent (or
    /// the superblock) holds for it.
    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault>;

    /// A number for a new page.
    fn allocate(&self) -> Result<PageNo, StoreError>;

    /// A new page, under the number `allocate` gave for it.
    fn write(&self, page: Arc<Page>) -> Result<(), StoreError>;

    /// The version being built no longer reaches page `number`.
    fn release(&self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError>;
}

impl<S: PageStore + ?Sized> PageStore for &S {
    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault> {
        (**self).read(number, hash)
    }

    fn allocate(&self) -> Result<PageNo, StoreError> {
        (**self).allocate()
    }

    fn write(&self, page: Arc<Page>) -> Result<(), StoreError> {
        (**self).write(page)
    }

    fn release(&self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError> {
        (**self).release(number, hash, how)
    }
}
