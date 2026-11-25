//! What the trees need from below: the page store, and the changes one entry
//! makes to it.

use std::fmt;
use std::sync::Arc;

use super::{Fault, Hash, Page, PageNo};

/// A version of a tree: its root page and the hash of that page, which
/// covers everything in the tree except page numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Root {
    pub page: PageNo,
    pub hash: Hash,
}

/// The name of one of the state's trees: 1 to 255 bytes. The store gives
/// names no meaning; the engine names one tree per hosted file and one for
/// the rest of the state (section 6.2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeName(Vec<u8>);

impl TreeName {
    /// The longest name, in bytes.
    pub const MAX: usize = 255;

    pub fn new(bytes: impl Into<Vec<u8>>) -> Option<TreeName> {
        let bytes = bytes.into();
        (!bytes.is_empty() && bytes.len() <= Self::MAX).then_some(TreeName(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for TreeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TreeName({})", self.0.escape_ascii())
    }
}

impl fmt::Display for TreeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.escape_ascii())
    }
}

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

/// The page store could not allocate, take or commit a page.
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
///   expected hash, or a page committed under that number and hash (below).
///   The tree checks the number and hash of what it gets, and a [`Page`]
///   cannot be made any other way, so a store cannot hand the tree
///   unchecked bytes. A failure is reported as the [`Fault`] found.
/// - **Caching by number and hash.** A cache of pages must be keyed by both,
///   so that a number that was freed and reused never answers for the page
///   that was there before (section 6.4). Committed pages never change, so a
///   cache never invalidates anything.
/// - **The store holds the trees.** It keeps each tree's name and its root
///   in the newest version ([`PageStore::trees`]), and changes them only
///   when a unit of work that names them commits.
/// - **One unit of work an entry.** Everything one entry changes in the
///   store, in every tree it changes, goes through one [`EntryPages`] that
///   `begin` returns: the pages written and released, the trees made,
///   dropped and renamed, and each changed tree's new root. It takes
///   effect at once when it is committed, or not at all ([`EntryPages`]
///   says how). One unit is open at a time: `begin` is refused while
///   another is open, so nothing changes the trees between a unit's begin
///   and its commit.
/// - **Pages stay while reached.** A committed page stays readable under its
///   number and hash for as long as any version, view, checkpoint, restore
///   point, clone or state copy in progress reaches it. Until a checkpoint
///   writes it to disk it is kept in memory (section 6.5), so nothing on
///   the commit path does I/O.
/// - **Numbers are the store's own.** Nothing the tree computes depends on
///   page numbers: page hashes leave them out, so two stores that number
///   pages differently hold trees with the same root hashes.
///
/// `read` and `begin` take `&self`: a store shared between a file's writer
/// and its readers synchronizes itself, and readers see only committed
/// pages.
pub trait PageStore {
    /// The changes of one entry, open until committed or dropped.
    type Entry<'a>: EntryPages
    where
        Self: 'a;

    /// The page at `number`, checked against `hash`, the hash its parent (or
    /// the superblock) holds for it.
    fn read(&self, number: PageNo, hash: &Hash) -> Result<Arc<Page>, Fault>;

    /// The trees of the newest version, by name, with their roots.
    fn trees(&self) -> Vec<(TreeName, Root)>;

    /// The root of tree `name` in the newest version.
    fn root(&self, name: &TreeName) -> Option<Root>;

    /// Opens the unit of work of one entry: everything the entry changes.
    /// Refused while another unit is open.
    fn begin(&self) -> Result<Self::Entry<'_>, StoreError>;
}

/// The changes one entry makes to the page store: the pages it writes, the
/// pages the new version no longer reaches, and the trees it makes, drops,
/// renames and gives a new root. Nothing of them is visible to readers or
/// counted in the store's space until [`EntryPages::commit`].
///
/// - **Allocating.** `allocate` returns a number that no committed page uses
///   and nothing else holds, and holds it for this entry.
/// - **Writing.** `write` takes a new page under a number this entry
///   allocated. The page is kept as given: no copy and no hash is needed
///   (the tree has hashed it).
/// - **Releasing.** `release` records that the version being built no longer
///   reaches a page the version it started from reached. The tree releases
///   every such page once, a page before the pages it refers to, and
///   [`Release`] says whether what the page refers to is still reached. A
///   store that shares pages between trees (clones, section 6.8) counts the
///   trees that reach each page; on [`Release::Copied`] of a page that
///   another tree still reaches, the copy shares the page's references, so
///   the store raises their counts (Rodeh's method), learning them from
///   [`Page::references`]. A page is freed only when nothing reaches it
///   (section 6.7).
/// - **Naming trees.** `add_tree` makes a tree whose root is a page this
///   entry wrote; `drop_tree` drops a tree, and the store releases the root
///   it holds for it (as [`Release::Dropped`]); `rename_tree` gives a tree
///   another name; `set_root` gives a tree a root this entry wrote, the
///   old one having been released by this entry (as either kind). They
///   take effect in the order given, at commit. Within one entry a tree's
///   root is given at most once (`add_tree` or `set_root`), `drop_tree`
///   names a tree that existed when the entry began and that the entry gave
///   no root, and a page released that is a tree's root is the old root of
///   a tree the entry gives a new one. No page the entry writes refers to a
///   page that is a tree's root once the entry takes effect (a root it
///   gives, or the root of a tree it leaves alone): a page two trees reach
///   would be freed with the one dropped while the other still reached it.
///   Clones will share pages with a share the store records; until one is
///   recorded, such a reference is refused. A unit that breaks any of this
///   is refused whole. A tree is named by the store's own record of it, so a caller
///   gives the store no root of a tree it already holds.
/// - **Committing.** `commit` makes all of it take effect together: the
///   written pages become readable and reached, the releases are applied
///   in the order given, and numbers allocated and not written are given
///   back. If `commit` returns an error, none of it took effect, as if the
///   entry had been dropped.
/// - **Dropping.** Dropped without a commit, the entry has had no effect:
///   every number it allocated is free again, its pages are gone and its
///   releases forgotten. An `apply` that fails drops its entry, so the
///   same entry can be applied again to the same root, or abandoned.
///
/// The space an entry may need is reserved before the entry is appended
/// (section 6.5), so a store can make `allocate` and `commit` succeed for
/// an entry it has reserved for.
pub trait EntryPages {
    /// A number for a new page.
    fn allocate(&mut self) -> Result<PageNo, StoreError>;

    /// A new page, under a number `allocate` gave this entry.
    fn write(&mut self, page: Arc<Page>) -> Result<(), StoreError>;

    /// The version being built no longer reaches page `number`.
    fn release(&mut self, number: PageNo, hash: &Hash, how: Release) -> Result<(), StoreError>;

    /// Tree `name` is made, with root `root`, a page this entry wrote.
    fn add_tree(&mut self, name: &TreeName, root: Root);

    /// Tree `name` is dropped, with everything its root reaches.
    fn drop_tree(&mut self, name: &TreeName);

    /// Tree `from` takes the name `to`.
    fn rename_tree(&mut self, from: &TreeName, to: &TreeName);

    /// Tree `name`'s root becomes `root`, a page this entry wrote.
    fn set_root(&mut self, name: &TreeName, root: Root);

    /// Makes the entry's changes take effect, all together.
    fn commit(self) -> Result<(), StoreError>;
}
