//! The store engine (docs/server-storage.md 5, 6, 11): each file the server
//! hosts is a tree in one page file of `om-store`'s, in the store folder
//! (`--store-dir`), read through a [`PagedView`] of one version of it.
//!
//! A file named by the server's database folders that the store does not
//! hold, or holds as imported from other bytes, is imported when the server
//! starts ([`StoreFolder::hold`]): read from its `.fmp12`, which is never
//! written, into a tree of its own (`paged::import`), outside the log
//! (section 6.10). The import goes into a tree under a name of its own, and
//! one unit of work then drops the tree it replaces, gives the new one the
//! file's tree name and records what was imported, the source's length and
//! SHA-256 and its page 0; a checkpoint then makes it durable. The SHA-256
//! is of the bytes the import read, taken in the same pass
//! ([`Hashing`]), and a source whose length, modification or change time
//! moved while it was read is not recorded ([`StoreError::SourceChanged`]):
//! the import is dropped and the next start tries again. A stop at any
//! point leaves the store holding the file whole, as imported or as before,
//! and never part of an import: what an earlier start left under an
//! import's name is dropped when the folder is opened, and an import that
//! fails after its tree was made drops it at once.
//!
//! Nothing is written in a folder before it is known to be this server's
//! ([`MARKER`]): an empty folder is given the marker file first; a folder
//! with the marker goes on; any other folder is refused untouched, with
//! what it holds.
//!
//! The state readers need besides the tree ([`Derived`]) is computed from
//! the view when the file is hosted. At this step the engine takes no
//! changes: every file is hosted read-only ([`ReadOnlyCause::StoreEngine`]),
//! so a client's write requests are refused before they reach the engine,
//! as those to any file the server hosts read-only are, and each operation
//! that would change the file and still reaches it (a server script's
//! write, an authoring edit) answers a typed "unavailable", FileMaker's
//! error 3 ([`StoreEngine::submit`]; docs/server-storage.md 11 lists each
//! answer).
//!
//! The folder is not a format promise (section 6.10; groups are #778): the
//! tree [`META`] carries [`FORMAT_MARK`], and a server that does not find
//! exactly that mark in a folder whose page file holds trees refuses the
//! folder, so a later server can tell this one's and import again.

use std::io::{self, Read as IoRead, Seek, SeekFrom};
use std::sync::{Arc, Mutex, PoisonError};

use log::{info, warn};
use om_store::page::{EntryPages, Hash, PageStore, Root, TreeName};
use om_store::pagefile::{
    CheckpointError, Config, EntryId, OpenError, PageFile, Pin, PinError, Purpose, Reservation,
    SpaceError,
};
use om_store::sys::{FolderLock, Fs};
use om_store::tree::{self, Edit, Node, Tree};

use super::paged::{self, Batch, ImportError, PagedView, Units};
use super::reading::Waiting;
use super::{
    design, not_run, run, Credentials, Draft, Edited, Environment, FileNotSaved, Hold,
    HostEnvironment, Locks, Operation, Outcome, ReadError, Reply, StorageError, View, WRITER,
};
use crate::anchored::FileId;
use crate::derived::Derived;
use crate::hosting::Hosted;
use crate::store::ReadOnlyCause;

/// The tree where the store folder records what it holds.
pub(crate) const META: &[u8] = b"om-server:hosted";
/// Its rows: the folder's mark, the number the next file tree takes, and
/// each file by the name clients open it by: its tree's name, its source's
/// length and SHA-256, and its page 0.
const FORMAT: &[u8] = b"format";
const NEXT: &[u8] = b"next";
const FILES: &[u8] = b"files";
const TREE: &[u8] = b"tree";
const SOURCE: &[u8] = b"source";
const PAGE_ZERO: &[u8] = b"page0";
/// The mark of a folder this server writes: a later server that reads
/// another mark, or none, refuses the folder.
pub(crate) const FORMAT_MARK: &[u8] = b"om-server store folder 0: files imported outside the \
    log, hosted read-only; not a format promise (docs/server-storage.md 6.10)";
/// The marker file a store folder this server made holds, written before
/// anything else in the folder: its content is [`FORMAT_MARK`] and a line
/// end. A folder without it is never written to.
pub(crate) const MARKER: &str = "om-server-store";
/// The most of a file named [`MARKER`] that is read.
const MAX_MARKER: usize = 1024;

/// The names of the trees that hold files, and of a tree an import is
/// filling, each followed by a number.
const FILE_TREE: &str = "om-server:file:";
const IMPORTING: &str = "om-server:importing:";

#[cfg(test)]
thread_local! {
    /// Fail the next import's last unit, after its tree was made: for the
    /// test of what such a failure leaves.
    pub(crate) static FAIL_PUBLISHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Fail the next checkpoint, before it writes anything: for the test of
    /// an import published and not yet durable.
    pub(crate) static FAIL_CHECKPOINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Fail the drop of the next failed import's tree: for the test of
    /// what that leaves.
    pub(crate) static FAIL_IMPORT_DROP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The page file's settings for the store engine: the page cache's size
/// from `--store-cache-mib`, the rest `om-store`'s defaults.
pub fn config(cache_bytes: u64) -> Config {
    Config {
        cache_bytes,
        ..Config::default()
    }
}

/// Why the store folder could not be opened, or a file not held.
#[derive(Debug)]
pub enum StoreError {
    /// A source file could not be read.
    Io(io::Error),
    /// The store folder could not be made or locked (another server holds
    /// it, for one).
    Folder(om_store::sys::Error),
    /// The page file could not be opened.
    Open(OpenError),
    /// The folder holds trees this server did not write, or another mark:
    /// what it found.
    Foreign(String),
    /// A unit of work, a read of the store's own tree, or a checkpoint
    /// failed.
    Store(String),
    /// The import refused the file (`paged::ImportError`).
    Import(ImportError),
    /// The source changed while the import read it: nothing of it is
    /// recorded, and the next start imports it again.
    SourceChanged,
    /// The store folder is not one only the server's own user can reach,
    /// or it was replaced while it was opened: why, and what to do.
    NotPrivate(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "reading the file: {e}"),
            StoreError::Folder(e) => write!(f, "the store folder: {e}"),
            StoreError::Open(e) => write!(f, "the store's page file does not open: {e:?}"),
            StoreError::Foreign(what) => write!(
                f,
                "the store folder holds {what}, which this server did not make: nothing in it \
                 was changed; give --store-dir an empty or new folder (one this server made \
                 has the file {MARKER})"
            ),
            StoreError::Store(e) => write!(f, "the store: {e}"),
            StoreError::Import(e) => write!(f, "the import refused the file: {e}"),
            StoreError::SourceChanged => write!(
                f,
                "the file changed while it was imported: not hosted, and imported again at \
                 the next start"
            ),
            StoreError::NotPrivate(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<om_store::sys::Error> for StoreError {
    fn from(e: om_store::sys::Error) -> Self {
        StoreError::Folder(e)
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<tree::Error> for StoreError {
    fn from(e: tree::Error) -> Self {
        StoreError::Store(e.to_string())
    }
}

impl From<CheckpointError> for StoreError {
    fn from(e: CheckpointError) -> Self {
        StoreError::Store(format!("a checkpoint failed: {e:?}"))
    }
}

impl From<PinError> for StoreError {
    fn from(e: PinError) -> Self {
        StoreError::Store(format!("the version could not be pinned: {e:?}"))
    }
}

/// The store folder and its page file, as the server opened them at its
/// start: one for the whole server.
pub struct StoreFolder {
    /// Dropped before the folder's lock, which must outlive it.
    pages: PageFile,
    _lock: FolderLock,
    /// One unit of work at a time: units outside the log are numbered after
    /// the newest entry.
    writing: Mutex<()>,
}

impl std::fmt::Debug for StoreFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreFolder")
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

/// What the store holds of one file ([`StoreFolder::hold`]).
pub struct Held {
    /// The version the file is read from, kept from reuse.
    pub pin: Pin,
    /// The file's tree's root in that version.
    pub root: Root,
    /// The file's page 0 as imported.
    pub page_zero: Vec<u8>,
    /// Whether this call imported it (otherwise the store held it).
    pub imported: bool,
    /// The length and SHA-256 of the `.fmp12` it was imported from.
    pub source: Digest,
}

impl StoreFolder {
    /// Open the store folder `fs` gives, making it and its page file when
    /// missing, its lock held for as long as the value lives: a folder
    /// another process holds is refused. Nothing is written in the folder
    /// before it is known to be this server's ([`claim`]); one whose page
    /// file holds trees without this server's mark ([`FORMAT_MARK`]) is
    /// refused too. What an import left part way at an earlier start is
    /// dropped.
    /// The store folder at `dir` on this system's file system
    /// ([`Self::open`]), which only the server's own user may reach: the
    /// directory opened (never a link) must be owned by the server's user,
    /// with no access for its group or others, and the store must have
    /// opened that same directory, by device and inode, not another that
    /// took its name meanwhile (docs/server-storage.md 11). A folder that
    /// does not exist is made, for the server's user only. Checked on
    /// Unix; other systems take the folder as it is.
    pub fn open_dir(dir: &std::path::Path, cache_bytes: u64) -> Result<StoreFolder, StoreError> {
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| {
                    StoreError::NotPrivate(format!("--store-dir {}: {e}", dir.display()))
                })?;
            private_folder(dir)?
        };
        let folder = Self::open(&om_store::os::OsFs::new(dir.to_path_buf()), cache_bytes)?;
        #[cfg(unix)]
        {
            let now = rustix::fs::lstat(dir).map_err(|e| {
                StoreError::NotPrivate(format!("the store folder {}: {e}", dir.display()))
            })?;
            if (now.st_dev, now.st_ino) != (opened.st_dev, opened.st_ino) {
                return Err(StoreError::NotPrivate(format!(
                    "the store folder {} was replaced while the server opened it: start again",
                    dir.display()
                )));
            }
        }
        Ok(folder)
    }

    pub fn open(fs: &dyn Fs, cache_bytes: u64) -> Result<StoreFolder, StoreError> {
        Self::open_with(fs, config(cache_bytes))
    }

    /// [`StoreFolder::open`] with the page file's settings `config`.
    pub(crate) fn open_with(fs: &dyn Fs, config: Config) -> Result<StoreFolder, StoreError> {
        fs.create_root()?;
        claim(fs)?;
        let lock = fs.lock()?;
        let pages = match PageFile::open(fs, &lock, config) {
            Ok((pages, _)) => pages,
            Err(OpenError::Missing) => {
                PageFile::create(fs, &lock, config).map_err(StoreError::Open)?
            }
            Err(e) => return Err(StoreError::Open(e)),
        };
        let folder = StoreFolder {
            pages,
            _lock: lock,
            writing: Mutex::new(()),
        };
        folder.check_mark()?;
        folder.drop_imports_left()?;
        Ok(folder)
    }

    /// The page file, which readers read through.
    pub(crate) fn pages(&self) -> &PageFile {
        &self.pages
    }

    fn meta() -> TreeName {
        TreeName::new(META.to_vec()).expect("a tree name")
    }

    /// Refuse a folder this server did not write; mark a new one.
    fn check_mark(&self) -> Result<(), StoreError> {
        let names = self.pages.tree_names();
        match self.pages.root(&Self::meta()) {
            Some(root) => match Tree::open(&self.pages, root)?.get(&[FORMAT])? {
                Some(Node::Value(mark)) if mark == FORMAT_MARK => Ok(()),
                Some(other) => Err(StoreError::Foreign(format!(
                    "the mark {:?}",
                    String::from_utf8_lossy(other.value())
                ))),
                None => Err(StoreError::Foreign("no mark".into())),
            },
            None if names.is_empty() => {
                let _writing = self.lock_writing();
                let edits = [
                    Edit::Put {
                        path: vec![FORMAT.to_vec()],
                        node: Node::Value(FORMAT_MARK.to_vec()),
                    },
                    Edit::Put {
                        path: vec![NEXT.to_vec()],
                        node: Node::Value(0u64.to_be_bytes().to_vec()),
                    },
                    Edit::Put {
                        path: vec![FILES.to_vec()],
                        node: Node::List,
                    },
                ];
                let entry = self.next_entry()?;
                let res = self.reserve(entry, 16)?;
                paged::apply_unit(&res, &Self::meta(), None, entry, &edits)?;
                drop(res);
                self.checkpoint()?;
                Ok(())
            }
            None => Err(StoreError::Foreign(format!(
                "{} tree(s) and no mark",
                names.len()
            ))),
        }
    }

    /// Drop every tree an import left under its own name: a start that
    /// stopped before the unit that names it.
    fn drop_imports_left(&self) -> Result<(), StoreError> {
        let _writing = self.lock_writing();
        self.settle()
    }

    /// Make what the store holds durable before anything of it is served,
    /// under the writing lock: drop every tree an import left under its own
    /// name (a stop, or a failure whose own drop failed), and checkpoint
    /// the units committed since the last checkpoint on disk (an import
    /// published whose checkpoint failed). Nothing to do costs nothing.
    fn settle(&self) -> Result<(), StoreError> {
        let left: Vec<TreeName> = self
            .pages
            .tree_names()
            .into_iter()
            .filter(|name| name.as_bytes().starts_with(IMPORTING.as_bytes()))
            .collect();
        if left.is_empty() && self.pages.checkpoint_entry().1.index >= self.pages.newest_entry() {
            return Ok(());
        }
        for name in &left {
            warn!(
                "the store: dropping {:?}, an import a stop or a failure left part way",
                String::from_utf8_lossy(name.as_bytes())
            );
            let entry = self.next_entry()?;
            let res = self.reserve(entry, 8)?;
            paged::drop_unit(&res, name)?;
        }
        self.checkpoint()?;
        Ok(())
    }

    fn lock_writing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.writing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The entry index the next unit outside the log takes.
    fn next_entry(&self) -> Result<u64, StoreError> {
        self.pages
            .newest_entry()
            .checked_add(1)
            .ok_or_else(|| StoreError::Store("no entry index is left".into()))
    }

    /// Room for an entry of `pages` pages, a checkpoint first when the
    /// pages waiting for one reach the budget.
    fn reserve(&self, entry: u64, pages: u64) -> Result<Reservation<'_>, StoreError> {
        OnPages { pages: &self.pages }
            .reserve(entry, pages)
            .map_err(StoreError::from)
    }

    /// A checkpoint of what was committed. Units outside the log have no
    /// term and no hash.
    fn checkpoint(&self) -> Result<(), StoreError> {
        #[cfg(test)]
        if FAIL_CHECKPOINT.with(|fail| fail.replace(false)) {
            return Err(StoreError::Store(
                "a test's failure of the checkpoint".into(),
            ));
        }
        let entry = EntryId {
            index: self.pages.newest_entry(),
            term: 0,
            hash: Hash::default(),
        };
        self.pages.checkpoint(entry, None)?;
        Ok(())
    }

    /// The tree that holds the file clients open as `name`, read from
    /// `source`: the one the store holds when it was imported from bytes of
    /// the same length and SHA-256, else a new import of `source`, which
    /// replaces the one held before only once it is complete. The source is
    /// read whole once for its digest when the store holds the name, and
    /// once for an import, which takes the digest of what it read in the
    /// same pass; it is never written. A source whose [`Stamp`] moved while
    /// it was imported, or a page of which the import read again with other
    /// bytes ([`Hashing`]), is refused ([`StoreError::SourceChanged`]), and an
    /// import that fails after its tree was made drops that tree. Pinned,
    /// so that the version read is kept from reuse.
    pub fn hold<R: IoRead + Seek + Stamped>(
        &self,
        name: &str,
        mut source: R,
    ) -> Result<Held, StoreError> {
        let _writing = self.lock_writing();
        // Nothing is served, or imported, from a store whose last change is
        // not durable yet.
        self.settle()?;
        let meta = Self::meta();
        let meta_root = self
            .pages
            .root(&meta)
            .ok_or_else(|| StoreError::Foreign("no mark".into()))?;
        let recorded = Recorded::read(&self.pages, meta_root, name)?;
        if let Some(recorded) = recorded.as_ref() {
            if self.pages.root(&recorded.tree).is_some() && digest(&mut source)? == recorded.source
            {
                let pin = self.pages.pin_newest()?;
                let root = root_in(&pin, &recorded.tree)?;
                return Ok(Held {
                    pin,
                    root,
                    page_zero: recorded.page_zero.clone(),
                    imported: false,
                    source: recorded.source,
                });
            }
        }
        let before = source.stamp()?;
        let number = next_number(&self.pages, meta_root)?;
        let importing = tree_name(IMPORTING, number);
        let named = tree_name(FILE_TREE, number);
        let first = self.next_entry()?;
        let mut hashing = Hashing::new(source);
        let imported = paged::import(
            io::BufReader::with_capacity(64 << 10, &mut hashing),
            &mut OnPages { pages: &self.pages },
            &importing,
            first,
            Batch::default(),
        );
        // A source that changed while it was read is reported as such,
        // whatever the import made of the bytes it got.
        let imported = match imported {
            Ok(imported) => imported,
            Err(_) if hashing.changed() || hashing.inner().stamp()? != before => {
                warn!("the store: {name:?} changed while it was imported");
                return Err(StoreError::SourceChanged);
            }
            Err(e) => return Err(StoreError::Import(e)),
        };
        // The import's tree exists from here: whatever fails drops it, so
        // that the next import, which takes the same number, finds no tree
        // of its name.
        let published = (|| {
            let digest = hashing.finish()?;
            if hashing.changed() || hashing.inner().stamp()? != before {
                warn!("the store: {name:?} changed while it was imported");
                return Err(StoreError::SourceChanged);
            }
            #[cfg(test)]
            if FAIL_PUBLISHING.with(|fail| fail.replace(false)) {
                return Err(StoreError::Store(
                    "a test's failure of the last unit".into(),
                ));
            }
            let file = vec![FILES.to_vec(), name.as_bytes().to_vec()];
            let row = |key: &[u8], value: Vec<u8>| Edit::Put {
                path: [&file[..], &[key.to_vec()]].concat(),
                node: Node::Value(value),
            };
            let next = number
                .checked_add(1)
                .ok_or_else(|| StoreError::Store("no tree number is left".into()))?;
            let edits = [
                Edit::Put {
                    path: file.clone(),
                    node: Node::List,
                },
                row(TREE, named.as_bytes().to_vec()),
                row(SOURCE, digest.bytes()),
                row(PAGE_ZERO, imported.page_zero.clone()),
                Edit::Put {
                    path: vec![NEXT.to_vec()],
                    node: Node::Value(next.to_be_bytes().to_vec()),
                },
            ];
            let entry = self.next_entry()?;
            let res = self.reserve(entry, 32)?;
            let mut unit = res.begin().map_err(tree::Error::from)?;
            let new_meta = tree::apply(&res, &mut unit, Some(meta_root), entry, &edits)?;
            unit.set_root(&meta, new_meta);
            if let Some(old) = recorded.as_ref().filter(|r| res.root(&r.tree).is_some()) {
                unit.drop_tree(&old.tree);
            }
            unit.rename_tree(&importing, &named);
            unit.commit().map_err(tree::Error::from)?;
            drop(res);
            self.checkpoint()?;
            Ok(digest)
        })();
        let digest = match published {
            Ok(digest) => digest,
            Err(error) => {
                if self.pages.root(&importing).is_some() {
                    let dropped = self
                        .next_entry()
                        .and_then(|entry| self.reserve(entry, 8))
                        .and_then(|res| {
                            #[cfg(test)]
                            if FAIL_IMPORT_DROP.with(|fail| fail.replace(false)) {
                                return Err(StoreError::Store(
                                    "a test's failure of the drop".into(),
                                ));
                            }
                            Ok(paged::drop_unit(&res, &importing)?)
                        })
                        .and_then(|()| self.checkpoint());
                    if let Err(e) = dropped {
                        warn!(
                            "the store: the import of {name:?} is left under {:?} until the \
                             next start: {e}",
                            String::from_utf8_lossy(importing.as_bytes())
                        );
                    }
                }
                return Err(error);
            }
        };
        info!(
            "the store: {name:?} imported, {} nodes, {} bytes",
            imported.nodes, digest.len
        );
        let pin = self.pages.pin_newest()?;
        let root = root_in(&pin, &named)?;
        Ok(Held {
            pin,
            root,
            page_zero: imported.page_zero,
            imported: true,
            source: digest,
        })
    }

    /// The names the store records files by, and each file's tree:
    /// `(name, tree)`, for tests.
    #[cfg(test)]
    pub(crate) fn recorded(&self) -> Vec<(String, Vec<u8>)> {
        let Some(root) = self.pages.root(&Self::meta()) else {
            return Vec::new();
        };
        let tree = Tree::open(&self.pages, root).unwrap();
        tree.children(
            &[FILES],
            std::ops::Bound::Unbounded,
            tree::Direction::Forward,
        )
        .map(|child| {
            let (segment, _) = child.unwrap();
            let name = String::from_utf8(segment.clone()).unwrap();
            let tree = tree.get(&[FILES, &segment, TREE]).unwrap().unwrap();
            (name, tree.value().to_vec())
        })
        .collect()
    }
}

/// The root of tree `name` in the version `pin` keeps.
fn root_in(pin: &Pin, name: &TreeName) -> Result<Root, StoreError> {
    pin.roots()
        .iter()
        .find(|(tree, _)| tree == name)
        .map(|(_, root)| *root)
        .ok_or_else(|| StoreError::Store("the file's tree is not in the version pinned".into()))
}

fn tree_name(prefix: &str, number: u64) -> TreeName {
    TreeName::new(format!("{prefix}{number}").into_bytes()).expect("a short tree name")
}

/// The number the next file tree takes, from the store's own tree.
fn next_number(pages: &PageFile, meta: Root) -> Result<u64, StoreError> {
    match Tree::open(pages, meta)?.get(&[NEXT])? {
        Some(Node::Value(bytes)) => bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| StoreError::Store("the next tree number does not decode".into())),
        _ => Err(StoreError::Store("no next tree number".into())),
    }
}

/// A source file's length and SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digest {
    len: u64,
    sha256: [u8; 32],
}

impl Digest {
    fn bytes(&self) -> Vec<u8> {
        [&self.len.to_be_bytes()[..], &self.sha256].concat()
    }

    fn from_bytes(bytes: &[u8]) -> Option<Digest> {
        let (len, sha256) = bytes.split_first_chunk::<8>()?;
        Some(Digest {
            len: u64::from_be_bytes(*len),
            sha256: sha256.try_into().ok()?,
        })
    }
}

/// The length and SHA-256 of what `source` holds, read from its start.
fn digest<R: IoRead + Seek>(source: &mut R) -> io::Result<Digest> {
    source.seek(SeekFrom::Start(0))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 1 << 16];
    let mut len = 0u64;
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        context.update(&buffer[..n]);
        len += n as u64;
    }
    let mut sha256 = [0; 32];
    sha256.copy_from_slice(context.finish().as_ref());
    Ok(Digest { len, sha256 })
}

/// The pages [`Hashing`] keeps a SHA-256 of: the `.fmp12` block size, so
/// that each block the import reads is one page.
const HASHED_PAGE: usize = 4096;

/// `source`, read for an import, with the SHA-256 of the whole file taken
/// in the same pass, in file order, and the SHA-256 of each page as it was
/// first read. Every read is served from whole pages: a page read for the
/// first time is hashed into the file's SHA-256 (with the pages before it
/// the import skipped), and a page read again is compared with its first
/// hash, so a byte the import reads twice is the byte the file's SHA-256
/// holds, or the read fails ([`Hashing::changed`]). [`Hashing::finish`]
/// hashes what the import did not read. 32 bytes are kept per 4 KiB page
/// (8 MiB for each GiB of the file) while the file is imported.
struct Hashing<R> {
    inner: R,
    /// Where the next read reads, and where the inner reader is.
    pos: u64,
    inner_pos: Option<u64>,
    /// The SHA-256 of each page from the first, as first read.
    pages: Vec<[u8; 32]>,
    /// The bytes hashed so far: the pages above, the last of them short
    /// at the end of the file.
    hashed: u64,
    /// The file ended at a short or empty page.
    ended: bool,
    context: ring::digest::Context,
    /// The page read last, whole, and its index.
    page: Vec<u8>,
    page_at: Option<u64>,
    /// A page read again did not hold what it held at first.
    changed: bool,
}

impl<R: IoRead + Seek> Hashing<R> {
    fn new(inner: R) -> Self {
        Hashing {
            inner,
            pos: 0,
            inner_pos: None,
            pages: Vec::new(),
            hashed: 0,
            ended: false,
            context: ring::digest::Context::new(&ring::digest::SHA256),
            page: Vec::with_capacity(HASHED_PAGE),
            page_at: None,
            changed: false,
        }
    }

    fn inner(&self) -> &R {
        &self.inner
    }

    /// Whether a page read again held other bytes than at its first read:
    /// the source changed while it was imported.
    fn changed(&self) -> bool {
        self.changed
    }

    /// Read page `index` whole (short at the end of the file) from the
    /// inner reader into `self.page`.
    fn read_page(&mut self, index: u64) -> io::Result<()> {
        let at = index * HASHED_PAGE as u64;
        if self.inner_pos != Some(at) {
            self.inner.seek(SeekFrom::Start(at))?;
            self.inner_pos = Some(at);
        }
        self.page.resize(HASHED_PAGE, 0);
        let mut filled = 0;
        while filled < HASHED_PAGE {
            let n = match self.inner.read(&mut self.page[filled..]) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.inner_pos = None;
                    self.page_at = None;
                    return Err(e);
                }
            };
            if n == 0 {
                break;
            }
            filled += n;
        }
        self.page.truncate(filled);
        self.inner_pos = Some(at + filled as u64);
        self.page_at = Some(index);
        Ok(())
    }

    /// Page `index` in `self.page`: read and hashed for the first time
    /// (with each page before it not hashed yet), or read again and
    /// compared with its first hash. An index past the end holds nothing.
    fn load(&mut self, index: u64) -> io::Result<()> {
        if self.page_at == Some(index) {
            return Ok(());
        }
        let first = self.pages.len() as u64;
        if self.ended && index >= first {
            self.page.clear();
            self.page_at = Some(index);
            return Ok(());
        }
        if index < first {
            self.read_page(index)?;
            let hash = ring::digest::digest(&ring::digest::SHA256, &self.page);
            if hash.as_ref() != self.pages[index as usize] {
                self.changed = true;
                self.page_at = None;
                return Err(io::Error::other(
                    "a page read again does not hold what it held when first read",
                ));
            }
            return Ok(());
        }
        for next in first..=index {
            self.read_page(next)?;
            let mut hash = [0; 32];
            hash.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, &self.page).as_ref());
            self.context.update(&self.page);
            self.hashed += self.page.len() as u64;
            if self.page.len() < HASHED_PAGE {
                self.ended = true;
                if self.page.is_empty() {
                    break;
                }
            }
            self.pages.push(hash);
            if self.ended {
                break;
            }
        }
        if self.page_at != Some(index) {
            // The file ended before `index`.
            self.page.clear();
            self.page_at = Some(index);
        }
        Ok(())
    }

    /// The length and SHA-256 of the whole file, hashing what is left.
    fn finish(&mut self) -> io::Result<Digest> {
        while !self.ended {
            self.load(self.pages.len() as u64)?;
        }
        let context = std::mem::replace(
            &mut self.context,
            ring::digest::Context::new(&ring::digest::SHA256),
        );
        let mut sha256 = [0; 32];
        sha256.copy_from_slice(context.finish().as_ref());
        Ok(Digest {
            len: self.hashed,
            sha256,
        })
    }
}

impl<R: IoRead + Seek> IoRead for Hashing<R> {
    /// Page by page until `buf` is full or the file ends. A page that
    /// fails after others were copied fails the next read.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            if let Err(e) = self.load(self.pos / HASHED_PAGE as u64) {
                return if n == 0 { Err(e) } else { Ok(n) };
            }
            let at = (self.pos % HASHED_PAGE as u64) as usize;
            let Some(left) = self.page.get(at..).filter(|left| !left.is_empty()) else {
                break;
            };
            let take = (buf.len() - n).min(left.len());
            buf[n..n + take].copy_from_slice(&left[..take]);
            n += take;
            self.pos += take as u64;
        }
        Ok(n)
    }
}

impl<R: IoRead + Seek> Seek for Hashing<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::Current(by) => self.pos.checked_add_signed(by).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "a seek before the start")
            })?,
            SeekFrom::End(by) => {
                let at = self.inner.seek(SeekFrom::End(by))?;
                self.inner_pos = Some(at);
                at
            }
        };
        Ok(self.pos)
    }
}

/// What tells a source's contents changed without reading them: its
/// length, modification and change times and inode. The change time moves
/// with every write and cannot be set back by the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    changed: (i64, i64),
    inode: u64,
}

/// A source whose [`Stamp`] can be read: a file. One without (a test's
/// bytes in memory) answers none, which never changes.
pub trait Stamped {
    fn stamp(&self) -> io::Result<Option<Stamp>>;
}

impl Stamped for std::fs::File {
    fn stamp(&self) -> io::Result<Option<Stamp>> {
        let metadata = self.metadata()?;
        #[cfg(unix)]
        let (changed, inode) = {
            use std::os::unix::fs::MetadataExt;
            ((metadata.ctime(), metadata.ctime_nsec()), metadata.ino())
        };
        #[cfg(not(unix))]
        let (changed, inode) = ((0, 0), 0);
        Ok(Some(Stamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            changed,
            inode,
        }))
    }
}

impl<T> Stamped for io::Cursor<T> {
    fn stamp(&self) -> io::Result<Option<Stamp>> {
        Ok(None)
    }
}

/// The status of the directory at `dir`, opened without following a link
/// and checked to be one only the server's own user can reach
/// ([`StoreFolder::open_dir`]).
#[cfg(unix)]
fn private_folder(dir: &std::path::Path) -> Result<rustix::fs::Stat, StoreError> {
    use rustix::fs::{Mode, OFlags};
    let refused =
        |why: String| StoreError::NotPrivate(format!("--store-dir {}: {why}", dir.display()));
    let fd = rustix::fs::open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| match e {
        rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP => {
            refused("not a folder (a link, or a file): give it a new or empty folder".into())
        }
        e => refused(e.to_string()),
    })?;
    let stat = rustix::fs::fstat(&fd).map_err(|e| refused(e.to_string()))?;
    let mode = stat.st_mode & 0o777;
    if mode & 0o077 != 0 {
        return Err(StoreError::NotPrivate(format!(
            "--store-dir {} can be reached by its group or others (mode {mode:03o}); the store \
             holds whole databases: run `chmod 700 {}`, or give a new folder, which the server \
             makes for its own user only",
            dir.display(),
            dir.display()
        )));
    }
    let user = rustix::process::geteuid().as_raw();
    if stat.st_uid != user {
        return Err(StoreError::NotPrivate(format!(
            "--store-dir {} belongs to another user (uid {}): give a folder of the server's own \
             user (uid {user})",
            dir.display(),
            stat.st_uid
        )));
    }
    Ok(stat)
}

/// Make sure the store folder `fs` gives is this server's before anything
/// is written in it: an empty folder is given the [`MARKER`] file (and a
/// marker a stop cut short, alone in the folder, is written again); a
/// folder with the marker goes on; any other folder is refused, untouched,
/// with what it holds. A marker of another content (another format of this
/// server's) is refused too.
fn claim(fs: &dyn Fs) -> Result<(), StoreError> {
    let marker = [FORMAT_MARK, b"\n"].concat();
    let names = fs.list("")?;
    let found = match names.iter().any(|name| name == MARKER) {
        true => {
            let file = fs.open(MARKER, om_store::sys::FileOptions::PLAIN)?;
            let mut bytes = vec![0; MAX_MARKER];
            let n = file.read_at(0, &mut bytes)?;
            bytes.truncate(n);
            Some(bytes)
        }
        false => None,
    };
    match found {
        Some(bytes) if bytes == marker => Ok(()),
        Some(bytes) if names.len() == 1 && marker.starts_with(&bytes) => write_marker(fs, &marker),
        None if names.is_empty() => write_marker(fs, &marker),
        Some(bytes) => Err(StoreError::Foreign(format!(
            "a {MARKER} of another format ({:?})",
            String::from_utf8_lossy(bytes.split(|b| *b == b'\n').next().unwrap_or_default())
        ))),
        None => {
            let shown: Vec<&str> = names.iter().take(5).map(String::as_str).collect();
            let more = match names.len().saturating_sub(shown.len()) {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            Err(StoreError::Foreign(format!(
                "{}{more} and no {MARKER}",
                shown.join(", ")
            )))
        }
    }
}

/// Write the marker file, durable before anything else is written.
fn write_marker(fs: &dyn Fs, marker: &[u8]) -> Result<(), StoreError> {
    let file = match fs.create(MARKER, om_store::sys::FileOptions::SYNC) {
        Ok(file) => file,
        // Written meanwhile by another start, or cut short by a stop.
        Err(e) if e.kind() == om_store::sys::ErrorKind::AlreadyExists => {
            fs.open(MARKER, om_store::sys::FileOptions::SYNC)?
        }
        Err(e) => return Err(e.into()),
    };
    file.write_at(0, marker)?;
    file.flush()?;
    drop(file);
    fs.flush_folder("")?;
    info!("the store: a new store folder, marked with {MARKER}");
    Ok(())
}

/// What the store's own tree records of a file.
struct Recorded {
    tree: TreeName,
    source: Digest,
    page_zero: Vec<u8>,
}

impl Recorded {
    fn read(pages: &PageFile, meta: Root, name: &str) -> Result<Option<Recorded>, StoreError> {
        let tree = Tree::open(pages, meta)?;
        let row = |key: &[u8]| -> Result<Option<Vec<u8>>, StoreError> {
            Ok(match tree.get(&[FILES, name.as_bytes(), key])? {
                Some(Node::Value(value)) => Some(value),
                _ => None,
            })
        };
        let (Some(name), Some(source), Some(page_zero)) =
            (row(TREE)?, row(SOURCE)?, row(PAGE_ZERO)?)
        else {
            return Ok(None);
        };
        let (Some(tree), Some(source)) = (TreeName::new(name), Digest::from_bytes(&source)) else {
            return Ok(None);
        };
        Ok(Some(Recorded {
            tree,
            source,
            page_zero,
        }))
    }
}

/// The page file as the units of an import go into it: each reserved for,
/// with a checkpoint whenever the pages waiting for one reach the budget.
struct OnPages<'f> {
    pages: &'f PageFile,
}

impl<'f> OnPages<'f> {
    fn reserve(&self, entry: u64, pages: u64) -> Result<Reservation<'f>, tree::Error> {
        loop {
            match self.pages.reserve(entry, pages, Purpose::Entry) {
                Ok(reservation) => return Ok(reservation),
                Err(SpaceError::CheckpointNeeded { .. }) => {
                    let id = EntryId {
                        index: self.pages.newest_entry(),
                        term: 0,
                        hash: Hash::default(),
                    };
                    self.pages
                        .checkpoint(id, None)
                        .map_err(|e| failed(format!("a checkpoint failed: {e:?}")))?;
                }
                Err(e) => return Err(failed(format!("no room for an entry: {e:?}"))),
            }
        }
    }
}

fn failed(why: String) -> tree::Error {
    tree::Error::Store(om_store::page::StoreError::Failed(why))
}

impl Units for OnPages<'_> {
    fn has_tree(&self, tree: &TreeName) -> bool {
        self.pages.root(tree).is_some()
    }

    fn apply(
        &mut self,
        tree: &TreeName,
        root: Option<Root>,
        entry: u64,
        edits: &[Edit],
    ) -> Result<Root, tree::Error> {
        // A unit that runs out of its reservation changed nothing: it is
        // applied again with a larger one.
        let mut pages = 16 + edits.len() as u64 / 8;
        loop {
            let reservation = self.reserve(entry, pages)?;
            match paged::apply_unit(&reservation, tree, root, entry, edits) {
                Err(tree::Error::Store(om_store::page::StoreError::Full)) => {
                    pages = pages.saturating_mul(2).saturating_add(64)
                }
                done => return done,
            }
        }
    }

    fn drop_tree(&mut self, tree: &TreeName, entry: u64) -> Result<(), tree::Error> {
        let reservation = self.reserve(entry, 8)?;
        paged::drop_unit(&reservation, tree)
    }
}

/// What the store engine holds of one hosted file: the version read, its
/// tree's root there, the state derived from it, and what reads leave
/// waiting (nothing is ever stored, so no index is noted as waiting).
pub struct StoreFile {
    folder: Arc<StoreFolder>,
    _pin: Pin,
    root: Root,
    derived: Arc<Derived>,
    waiting: Waiting,
    /// The source file's device and inode when it was hosted.
    on_disk: Option<FileId>,
    /// The length and SHA-256 of what it was imported from: what a backup
    /// of it holds (`backups`).
    source: Digest,
}

impl std::fmt::Debug for StoreFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreFile")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl StoreFile {
    /// The file `held` in `folder`, its state `derived` computed from its
    /// view, read from the source with identity `on_disk`.
    pub(crate) fn new(
        folder: Arc<StoreFolder>,
        held: Held,
        derived: Derived,
        on_disk: Option<FileId>,
    ) -> StoreFile {
        StoreFile {
            folder,
            _pin: held.pin,
            root: held.root,
            derived: Arc::new(derived),
            waiting: Waiting::storing_nothing(),
            on_disk,
            source: held.source,
        }
    }

    /// The length and SHA-256 of the `.fmp12` the file was imported from.
    pub(crate) fn source(&self) -> (u64, [u8; 32]) {
        (self.source.len, self.source.sha256)
    }

    /// The view of the file's version.
    pub(crate) fn view(&self) -> Result<PagedView<'_, PageFile>, ReadError> {
        PagedView::open(self.folder.pages(), self.root)
    }

    pub(crate) fn derived(&self) -> &Arc<Derived> {
        &self.derived
    }

    pub(crate) fn waiting(&self) -> &Waiting {
        &self.waiting
    }
}

/// The view of a version in `folder` whose root is `root`, for the state a
/// file is hosted with: computed before the file is a [`StoreFile`].
pub(crate) fn view_of(
    folder: &StoreFolder,
    root: Root,
) -> Result<PagedView<'_, PageFile>, ReadError> {
    PagedView::open(folder.pages(), root)
}

/// The most hosting a file on the store engine reads whole of it, each
/// read on its own, counted as an owned tree's memory
/// (`om_hbam::tree::memory`): its schema catalog `[3]`, its layout catalog
/// `[4]`, each table's field catalog `[T, 03]`, its custom functions `[16]`
/// and value lists `[21]`, its privilege sets `[23, 2, 5]`, and each
/// account `[23, 1, 5, A]`. Past it the file is not hosted
/// ([`crate::hosting::HostingError::TooLarge`]); the page cache's bound
/// leaves these out, as it leaves out the state derived from them
/// (docs/server-storage.md 11). OpenMaker's choice.
pub const HOSTING_LIMIT: usize = 256 << 20;

/// A view that refuses, as too large, any subtree its reader asks for whole
/// past `limit`, even with no limit of its own ([`super::NO_LIMIT`]): what
/// hosting reads of a file on the store engine.
pub(crate) struct Capped<'v> {
    inner: &'v dyn View,
    limit: usize,
}

impl<'v> Capped<'v> {
    pub(crate) fn new(inner: &'v dyn View, limit: usize) -> Self {
        Capped { inner, limit }
    }
}

impl View for Capped<'_> {
    fn version(&self) -> super::Version {
        self.inner.version()
    }

    fn node(&self, path: &[&[u8]]) -> Result<Option<super::Head<'_>>, ReadError> {
        self.inner.node(path)
    }

    fn subtree(
        &self,
        path: &[&[u8]],
        limit: usize,
    ) -> Result<Option<std::borrow::Cow<'_, om_hbam::OwnedNode>>, ReadError> {
        self.inner.subtree(path, limit.min(self.limit))
    }

    fn children_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: std::ops::Bound<&[u8]>,
        direction: super::Direction,
        max: usize,
        out: &mut Vec<super::Child<'v>>,
    ) -> Result<bool, ReadError> {
        self.inner.children_page(path, from, direction, max, out)
    }

    fn subtrees_page<'v>(
        &'v self,
        path: &[&[u8]],
        from: std::ops::Bound<&[u8]>,
        direction: super::Direction,
        max: usize,
        limit: usize,
        out: &mut Vec<std::borrow::Cow<'v, om_hbam::OwnedNode>>,
    ) -> Result<bool, ReadError> {
        self.inner
            .subtrees_page(path, from, direction, max, limit.min(self.limit), out)
    }

    fn walk_page<'v>(
        &'v self,
        path: &[&[u8]],
        after: Option<&[Vec<u8>]>,
        max: usize,
        out: &mut Vec<super::view::Step<'v>>,
    ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
        self.inner.walk_page(path, after, max, out)
    }

    fn changed_since(&self, path: &[&[u8]], since: super::Version) -> Result<bool, ReadError> {
        self.inner.changed_since(path, since)
    }
}

/// The store engine: the store folder it hosts files from, and the
/// environment the operations it runs read the world from.
pub struct StoreEngine {
    folder: Arc<StoreFolder>,
    environment: Box<dyn Environment>,
    /// The files whose root a test made fail to open
    /// (`ServerHandle::failing_reads` with the empty path).
    #[cfg(debug_assertions)]
    broken_roots: Mutex<Vec<String>>,
}

impl std::fmt::Debug for StoreEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreEngine")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

/// Why each operation that would change a file is refused.
const UNAVAILABLE: &str = "the store engine takes no changes yet: the file is hosted read-only";

impl StoreEngine {
    pub fn new(folder: Arc<StoreFolder>) -> Self {
        StoreEngine {
            folder,
            environment: Box::new(HostEnvironment),
            #[cfg(debug_assertions)]
            broken_roots: Mutex::default(),
        }
    }

    /// The store folder files are hosted from.
    pub(crate) fn folder(&self) -> &Arc<StoreFolder> {
        &self.folder
    }

    /// The version `file` is hosted at, for reading.
    pub fn read<'h>(&self, file: &'h Hosted) -> super::reading::Reading<'h> {
        #[cfg(debug_assertions)]
        let broken = self
            .broken_roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&file.name);
        #[cfg(not(debug_assertions))]
        let broken = false;
        super::reading::Reading::of_store(file, file.store_file(), broken)
    }

    /// From now on the root of the file named `file` does not open: every
    /// reading of it is broken, as one whose root page no longer reads.
    /// Only in debug builds, for tests.
    #[cfg(debug_assertions)]
    pub(crate) fn break_root(&self, file: &str) {
        self.broken_roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(file.to_owned());
    }

    /// Run `operation` on `file` as the store engine can: the lock table's
    /// operations as the file engine runs them, op60 with the modification
    /// count read from the file's view, a save as a read-only file's
    /// (`FileNotSaved::ReadOnly`), and every other operation, each of which
    /// would change the file, refused without running ([`unavailable`]).
    pub fn submit_with(
        &self,
        file: &Hosted,
        operation: Operation,
        credentials: Credentials,
    ) -> Outcome {
        super::reading::assert_no_reading_held("an operation submitted");
        match operation {
            Operation::UpgradeLock(_)
            | Operation::ReleaseLock(_)
            | Operation::ReleaseLocks(_)
            | Operation::TakeScriptLock(_)
            | Operation::ReleaseScriptLock(_)
            | Operation::ReleaseSessionLocks(_) => {
                let mut locks = Locks::new(file.locks(&WRITER));
                let outcome = run(
                    operation,
                    credentials,
                    &file.name,
                    Hold::LockTable,
                    &mut locks,
                    &*self.environment,
                );
                locks.keep();
                outcome
            }
            Operation::TakeLock(op) => {
                // op60 reads the node's modification count through the
                // file's tree, as the file engine does through its view.
                let stored = file.store_file();
                let view = match stored.view() {
                    Ok(view) => view,
                    Err(error) => {
                        warn!(
                            "{:?}: op60: the file's tree cannot be read: {error}",
                            file.name
                        );
                        return unavailable(Operation::TakeLock(op));
                    }
                };
                let mut locks = Locks::new(file.locks(&WRITER));
                let reply = crate::writes::lock(op, &file.name, &view, &mut locks);
                locks.keep();
                Outcome::new(Reply::TakeLock(reply))
            }
            Operation::SaveFile => Outcome::new(Reply::SaveFile(Err(FileNotSaved::ReadOnly))),
            other => {
                info!("{:?}: not done: {UNAVAILABLE}", file.name);
                unavailable(other)
            }
        }
    }

    pub fn submit(&self, file: &Hosted, operation: Operation) -> Outcome {
        self.submit_with(file, operation, Credentials::None)
    }

    /// A draft is a write: what reads left waiting is never stored (none
    /// is noted on the store engine), and a program's own writes are
    /// refused as on a file hosted read-only.
    pub fn submit_draft(&self, file: &Hosted, draft: Draft) -> Outcome {
        let operation = match draft {
            Draft::ChangeSchema { writes } if !writes.is_empty() => {
                Operation::ChangeSchema(super::ChangeSchema {
                    writes,
                    member_keys: Vec::new(),
                    indexes: Vec::new(),
                })
            }
            Draft::ChangeSchema { .. } | Draft::StoreIndexes => {
                return Outcome::new(Reply::StoreIndexes(Ok(())))
            }
        };
        self.submit(file, operation)
    }

    /// Why `file` takes no changes: the store engine takes none yet.
    pub fn read_only(&self, _file: &Hosted) -> Option<ReadOnlyCause> {
        Some(ReadOnlyCause::StoreEngine)
    }

    /// The device and inode of the `.fmp12` the file was imported from or
    /// checked against when it was hosted.
    pub fn file_on_disk(&self, file: &Hosted) -> Option<FileId> {
        file.store_file().on_disk
    }
}

/// The answer to `operation`, which would change the file, refused
/// without running: a typed "unavailable", FileMaker's error 3 ("Command
/// is unavailable"; OpenMaker's choice, as a server script's account step
/// on a file hosted read-only answers). Each reply carries it where it has
/// a status (`not_run`): a server script's step fails with 3, an authoring
/// edit is refused with status 3 and says nothing was changed. Nothing is
/// stored and no client is told of a change.
fn unavailable(operation: Operation) -> Outcome {
    let error = StorageError {
        code: UNAVAILABLE_CODE,
        message: UNAVAILABLE.to_owned(),
        unknown: false,
    };
    match operation {
        Operation::EditDesign(_) => Outcome::new(Reply::EditDesign(Edited {
            reply: design::Reply::Refused {
                status: design::status::UNSUPPORTED,
                message: format!("Nothing was changed: {UNAVAILABLE}."),
                current: None,
            },
            not_stored: None,
        })),
        other => not_run(other, &error),
    }
}

/// FileMaker's error 3, "Command is unavailable".
const UNAVAILABLE_CODE: u16 = 3;

/// The state of `file` the store engine computes from `view` when it is
/// hosted, page 0 as imported.
pub(crate) fn derived_of(
    view: &dyn View,
    name: &str,
    page_zero: &[u8],
) -> Result<Derived, ReadError> {
    let catalog = crate::derived::read_catalog(view).map_err(|e| match e {
        // A catalog past what the reader reads whole (`Capped`).
        om_hbam::Error::TooLarge { limit } => ReadError::TooLarge {
            path: Vec::new(),
            limit,
        },
        e => ReadError::Unreadable {
            path: Vec::new(),
            why: format!("the schema does not read: {e}"),
        },
    })?;
    let mut derived = Derived::new(view, catalog, name)?;
    // Accounts that cannot be read whole (past the limit, or unreadable)
    // would leave the file listed and every login failing: refused, as a
    // catalog past the limit is, and the file is not hosted.
    if let Err(error) = derived.accounts() {
        return Err(error.clone());
    }
    derived.set_page_zero(page_zero);
    Ok(derived)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use om_store::sim::{Fault, Sim};

    use super::*;
    use crate::scripts::fixture;

    const CACHE: u64 = 1 << 20;
    const NAME: &str = "Tasks.fmp12";

    /// A page file small in memory, so that an import checkpoints along the
    /// way: a stop can come after part of it is on disk.
    fn small() -> Config {
        Config {
            cache_bytes: 256 << 10,
            dirty_budget: 8,
            reserve: 16,
            growth: 64,
        }
    }

    fn open(sim: &Sim) -> StoreFolder {
        StoreFolder::open_with(&sim.fs(), small()).unwrap()
    }

    /// The fixture's file with tasks 1 to `tasks`, as bytes.
    fn file(tag: &str, tasks: u64) -> Vec<u8> {
        let mut tree = fixture::tree(&[], true);
        fixture::add_tasks(&mut tree, 1..=tasks);
        let file = fixture::TempFile::from_tree(tag, &tree);
        std::fs::read(&file.0).unwrap()
    }

    /// The root hash of the file the store holds, none when it holds none;
    /// the page file holds no other tree but the store's own.
    fn held(folder: &StoreFolder) -> Option<Hash> {
        let recorded = folder.recorded();
        let mut expected = vec![META.to_vec()];
        expected.extend(recorded.iter().map(|(_, tree)| tree.clone()));
        let mut names: Vec<Vec<u8>> = folder
            .pages()
            .tree_names()
            .into_iter()
            .map(|name| name.as_bytes().to_vec())
            .collect();
        names.sort();
        expected.sort();
        assert_eq!(names, expected, "trees the store does not record");
        match &recorded[..] {
            [] => None,
            [(name, tree)] => {
                assert_eq!(name, NAME);
                let tree = TreeName::new(tree.clone()).unwrap();
                Some(folder.pages().root(&tree).expect("the tree it names").hash)
            }
            more => panic!("more than one file: {more:?}"),
        }
    }

    /// The import at start, stopped by a power cut at each of its steps in
    /// turn (what was not flushed kept or lost as the seed draws), leaves
    /// the store holding the file whole, as a run that was not stopped holds
    /// it, or not at all; a new import of a changed file, stopped so,
    /// leaves the file held as it was or as the new import makes it. The
    /// folder opened again drops what a stopped import left under its own
    /// name, and holds no tree an import filled.
    #[test]
    fn an_import_stopped_at_any_step_leaves_the_file_whole_or_as_it_was() {
        let first = file("store-stop-first", 3000);
        let second = file("store-stop-second", 3500);
        // Runs that are not stopped: their steps, and the trees they make.
        let sim = Sim::new(7);
        let folder = open(&sim);
        let opened = sim.steps();
        let whole = folder.hold(NAME, Cursor::new(&first)).unwrap().root.hash;
        let after_first = sim.steps();
        let replaced = folder.hold(NAME, Cursor::new(&second)).unwrap().root.hash;
        let after_second = sim.steps();
        drop(folder);
        assert_ne!(whole, replaced);
        // The same file again is held, not imported.
        let sim = Sim::new(7);
        let folder = open(&sim);
        assert!(folder.hold(NAME, Cursor::new(&first)).unwrap().imported);
        assert!(!folder.hold(NAME, Cursor::new(&first)).unwrap().imported);
        drop(folder);

        let mut seen = [0usize; 3];
        for (replacing, steps) in [
            (false, after_first - opened),
            (true, after_second - after_first),
        ] {
            for k in 1..=steps {
                let sim = Sim::new(k);
                let folder = open(&sim);
                if replacing {
                    folder.hold(NAME, Cursor::new(&first)).unwrap();
                }
                sim.place(sim.steps() + k, Fault::Crash);
                let source = if replacing { &second } else { &first };
                let _ = folder.hold(NAME, Cursor::new(source));
                drop(folder);
                sim.restart();
                let folder = open(&sim);
                let at = held(&folder);
                let allowed = if replacing {
                    vec![Some(whole), Some(replaced)]
                } else {
                    vec![None, Some(whole)]
                };
                assert!(
                    allowed.contains(&at),
                    "replacing {replacing}, stopped at step {k}: {at:?}"
                );
                seen[match at {
                    None => 0,
                    Some(hash) if hash == whole => 1,
                    Some(_) => 2,
                }] += 1;
            }
        }
        // Each outcome happened.
        assert!(seen.iter().all(|&n| n > 0), "{seen:?}");
        eprintln!(
            "stopped at each of {} and {} steps: {seen:?}",
            after_first - opened,
            after_second - after_first
        );
    }

    /// Nothing is written or removed in a store folder before it is known
    /// to be this server's ([`MARKER`]): a folder holding an unrelated
    /// `pages.new`, and one holding a page file another server made
    /// without the marker, are refused with what they hold and left byte
    /// for byte as they were. An empty folder is marked, opens, and opens
    /// again.
    #[test]
    fn a_folder_this_server_did_not_make_is_refused_untouched() {
        use om_store::os::OsFs;
        let base = std::env::temp_dir().join(format!(
            "om-store-claim-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let snapshot = |dir: &std::path::Path| {
            let mut files: Vec<(std::ffi::OsString, Vec<u8>)> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect();
            files.sort();
            files
        };
        let unrelated = base.join("unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(unrelated.join("pages.new"), b"someone else's").unwrap();
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        {
            let fs = OsFs::new(&other);
            let lock = fs.lock().unwrap();
            let pages = PageFile::create(&fs, &lock, config(CACHE)).unwrap();
            let res = pages.reserve(1, 16, Purpose::Entry).unwrap();
            let edits = [Edit::Put {
                path: vec![b"x".to_vec()],
                node: Node::Value(b"y".to_vec()),
            }];
            let tree = TreeName::new(b"another".to_vec()).unwrap();
            paged::apply_unit(&res, &tree, None, 1, &edits).unwrap();
            drop(res);
            let id = EntryId {
                index: pages.newest_entry(),
                term: 0,
                hash: Hash::default(),
            };
            pages.checkpoint(id, None).unwrap();
        }
        for dir in [&unrelated, &other] {
            let before = snapshot(dir);
            let refused = StoreFolder::open(&OsFs::new(dir), CACHE);
            assert!(
                matches!(refused, Err(StoreError::Foreign(_))),
                "{dir:?}: {refused:?}"
            );
            assert_eq!(snapshot(dir), before, "{dir:?}");
        }
        let new = base.join("new");
        std::fs::create_dir_all(&new).unwrap();
        drop(StoreFolder::open(&OsFs::new(&new), CACHE).unwrap());
        assert_eq!(
            std::fs::read(new.join(MARKER)).unwrap(),
            [FORMAT_MARK, b"\n"].concat()
        );
        drop(StoreFolder::open(&OsFs::new(&new), CACHE).unwrap());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The engine opens only a store folder no one but the server's user
    /// can reach, checked on the directory it opens: one its group or
    /// others can reach and a link to a private folder are refused, a
    /// private one opens, and a missing one is made private and opens.
    #[cfg(unix)]
    #[test]
    fn the_engine_opens_only_a_private_store_folder() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "om-store-private-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let shared = base.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        for mode in [0o755, 0o750, 0o701] {
            std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(mode)).unwrap();
            let refused = StoreFolder::open_dir(&shared, CACHE);
            assert!(
                matches!(refused, Err(StoreError::NotPrivate(_))),
                "{mode:o}: {:?}",
                refused.err()
            );
        }
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(StoreFolder::open_dir(&shared, CACHE).unwrap());
        let link = base.join("link");
        std::os::unix::fs::symlink(&shared, &link).unwrap();
        assert!(matches!(
            StoreFolder::open_dir(&link, CACHE),
            Err(StoreError::NotPrivate(_))
        ));
        let missing = base.join("missing").join("Store");
        drop(StoreFolder::open_dir(&missing, CACHE).unwrap());
        let mode = std::fs::metadata(&missing).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A file's bytes as one version until `switch` bytes were read in all,
    /// and as another after, its [`Stamp`] moving with them: a source
    /// replaced in place while it is read.
    struct Changing {
        versions: [Vec<u8>; 2],
        read: u64,
        switch: u64,
        pos: u64,
    }

    impl Changing {
        fn now(&self) -> &[u8] {
            &self.versions[usize::from(self.read >= self.switch)]
        }
    }

    impl IoRead for Changing {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let bytes = self.now();
            let at = (self.pos as usize).min(bytes.len());
            let n = buf.len().min(bytes.len() - at);
            buf[..n].copy_from_slice(&bytes[at..at + n]);
            self.pos += n as u64;
            self.read += n as u64;
            Ok(n)
        }
    }

    impl Seek for Changing {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            let len = self.now().len() as u64;
            self.pos = match to {
                SeekFrom::Start(at) => at,
                SeekFrom::Current(by) => self.pos.saturating_add_signed(by),
                SeekFrom::End(by) => len.saturating_add_signed(by),
            };
            Ok(self.pos)
        }
    }

    impl Stamped for Changing {
        fn stamp(&self) -> io::Result<Option<Stamp>> {
            let switched = self.read >= self.switch;
            Ok(Some(Stamp {
                len: self.now().len() as u64,
                modified: None,
                changed: (i64::from(switched), 0),
                inode: 1,
            }))
        }
    }

    /// A file's bytes, page 1 of which holds other bytes from its second
    /// read on, its length and times as they were (a [`Stamp`] that never
    /// moves): what a write in place of the same length between two reads
    /// looks like on a system whose times do not show it.
    struct ChangedOnRereading {
        versions: [Vec<u8>; 2],
        reads_of_page_1: u32,
        pos: u64,
    }

    impl IoRead for ChangedOnRereading {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let page_1 = self.pos / HASHED_PAGE as u64 == 1;
            let bytes = &self.versions[usize::from(page_1 && self.reads_of_page_1 > 0)];
            if page_1 {
                self.reads_of_page_1 += 1;
            }
            let at = (self.pos as usize).min(bytes.len());
            let n = buf.len().min(bytes.len() - at);
            buf[..n].copy_from_slice(&bytes[at..at + n]);
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for ChangedOnRereading {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.pos = match to {
                SeekFrom::Start(at) => at,
                SeekFrom::Current(by) => self.pos.saturating_add_signed(by),
                SeekFrom::End(by) => (self.versions[0].len() as u64).saturating_add_signed(by),
            };
            Ok(self.pos)
        }
    }

    /// Every byte the import reads twice is the byte the file's SHA-256
    /// holds ([`Hashing`]): read page by page as the import reads, a page
    /// read again after a seek back must hold what it held at its first
    /// read. Changed in between, with the length and times as they were,
    /// the read fails and the source is taken as changed, which refuses the
    /// import ([`StoreError::SourceChanged`]). Unchanged, it is read again
    /// and the digest is the whole file's. (The files the store tests
    /// import are read in file order, each page once, so the reader is
    /// driven here as the import drives it after a backward leaf link.)
    #[test]
    fn a_page_read_again_must_hold_what_it_held() {
        let source = file("store-reread", 400);
        assert!(source.len() >= 4 * HASHED_PAGE);
        let mut edited = source.clone();
        edited[HASHED_PAGE + 100] ^= 0xff;
        let read_back = |reader: &mut dyn IoReadSeek| {
            let mut first = vec![0; 3 * HASHED_PAGE];
            reader.read_exact(&mut first).unwrap();
            assert_eq!(first, source[..3 * HASHED_PAGE]);
            reader.seek(SeekFrom::Start(HASHED_PAGE as u64)).unwrap();
            let mut again = vec![0; HASHED_PAGE];
            reader.read_exact(&mut again).map(|()| again)
        };
        let mut hashing = Hashing::new(ChangedOnRereading {
            versions: [source.clone(), edited],
            reads_of_page_1: 0,
            pos: 0,
        });
        assert!(read_back(&mut hashing).is_err());
        assert!(hashing.changed());

        let mut hashing = Hashing::new(Cursor::new(source.clone()));
        let again = read_back(&mut hashing).unwrap();
        assert_eq!(again, source[HASHED_PAGE..2 * HASHED_PAGE]);
        assert!(!hashing.changed());
        let whole = hashing.finish().unwrap();
        assert_eq!(whole, digest(&mut Cursor::new(&source)).unwrap());
    }

    /// A reader the test above drives through `dyn`.
    trait IoReadSeek: IoRead + Seek {}
    impl<T: IoRead + Seek> IoReadSeek for T {}

    /// The SHA-256 the store records is of the bytes it imported: a source
    /// replaced in place while the import reads it is not recorded, and is
    /// imported again once it stands still; the bytes read first are then
    /// not taken for it either.
    #[test]
    fn a_source_that_changes_while_it_is_imported_is_not_recorded() {
        let first = file("store-changing-first", 50);
        let second = file("store-changing-second", 60);
        let sim = Sim::new(5);
        let folder = open(&sim);
        let changing = Changing {
            switch: first.len() as u64 / 2,
            versions: [first.clone(), second.clone()],
            read: 0,
            pos: 0,
        };
        let refused = folder.hold(NAME, changing);
        assert!(
            matches!(refused, Err(StoreError::SourceChanged)),
            "{:?}",
            refused.err()
        );
        assert_eq!(held(&folder), None);
        assert!(folder.hold(NAME, Cursor::new(&second)).unwrap().imported);
        assert!(!folder.hold(NAME, Cursor::new(&second)).unwrap().imported);
        assert!(folder.hold(NAME, Cursor::new(&first)).unwrap().imported);
    }

    /// An import that fails after its tree was made (here at its last
    /// unit) drops that tree at once: the next import, which takes the same
    /// number, goes through in the same process.
    #[test]
    fn an_import_that_fails_at_its_last_unit_leaves_nothing_in_the_way() {
        let source = file("store-fail-last", 40);
        let sim = Sim::new(9);
        let folder = open(&sim);
        FAIL_PUBLISHING.with(|fail| fail.set(true));
        let failed = folder.hold(NAME, Cursor::new(&source));
        assert!(
            matches!(failed, Err(StoreError::Store(_))),
            "{:?}",
            failed.err()
        );
        assert_eq!(held(&folder), None);
        assert!(folder.hold(NAME, Cursor::new(&source)).unwrap().imported);
        assert!(held(&folder).is_some());
    }

    /// A published import is served only once it is durable: when the
    /// checkpoint after its last unit fails, the import fails, and a second
    /// hold of the same bytes in the same process checkpoints before it
    /// serves the tree (not imported again); after a crash the store holds
    /// it.
    #[test]
    fn an_import_whose_checkpoint_failed_is_served_once_durable() {
        let source = file("store-fail-checkpoint", 30);
        let sim = Sim::new(11);
        let folder = open(&sim);
        FAIL_CHECKPOINT.with(|fail| fail.set(true));
        let failed = folder.hold(NAME, Cursor::new(&source));
        assert!(
            matches!(failed, Err(StoreError::Store(_))),
            "{:?}",
            failed.err()
        );
        let pages = folder.pages();
        assert!(pages.checkpoint_entry().1.index < pages.newest_entry());
        let held = folder.hold(NAME, Cursor::new(&source)).unwrap();
        assert!(!held.imported);
        let pages = folder.pages();
        assert_eq!(pages.checkpoint_entry().1.index, pages.newest_entry());
        drop((held, folder));
        sim.restart();
        let folder = open(&sim);
        assert!(!folder.hold(NAME, Cursor::new(&source)).unwrap().imported);
    }

    /// An import whose own drop of its tree failed leaves that tree under
    /// the import's name: the next import, which takes the same number,
    /// drops it first and goes through in the same process.
    #[test]
    fn an_import_tree_a_failed_drop_left_is_dropped_by_the_next() {
        let source = file("store-fail-drop", 30);
        let sim = Sim::new(13);
        let folder = open(&sim);
        // The last unit fails, and so does the drop of the import's tree.
        FAIL_PUBLISHING.with(|fail| fail.set(true));
        FAIL_IMPORT_DROP.with(|fail| fail.set(true));
        assert!(folder.hold(NAME, Cursor::new(&source)).is_err());
        let importing = |folder: &StoreFolder| {
            folder
                .pages()
                .tree_names()
                .iter()
                .any(|name| name.as_bytes().starts_with(IMPORTING.as_bytes()))
        };
        assert!(importing(&folder));
        assert!(folder.hold(NAME, Cursor::new(&source)).unwrap().imported);
        assert!(!importing(&folder));
        assert!(held(&folder).is_some());
    }

    /// What hosting reads whole of a file on the store engine is read
    /// within a limit ([`HOSTING_LIMIT`]): past it the state it derives is
    /// refused as too large, never read whole.
    #[test]
    fn hosting_reads_nothing_whole_past_its_limit() {
        let source = file("store-capped", 10);
        let sim = Sim::new(4);
        let folder = open(&sim);
        let held = folder.hold(NAME, Cursor::new(&source)).unwrap();
        let view = view_of(&folder, held.root).unwrap();
        assert!(derived_of(&Capped::new(&view, HOSTING_LIMIT), NAME, &held.page_zero).is_ok());
        let refused = derived_of(&Capped::new(&view, 256), NAME, &held.page_zero);
        assert!(
            matches!(refused, Err(ReadError::TooLarge { limit: 256, .. })),
            "{:?}",
            refused.err()
        );
    }

    /// A file whose accounts cannot be read whole within the limit is not
    /// hosted, as one whose catalog cannot: its derived state is refused
    /// as too large rather than kept with its accounts unreadable, which
    /// would leave it listed and every login failing. The same file without
    /// the large account derives within the same limit.
    #[test]
    fn a_file_whose_accounts_pass_the_limit_is_refused() {
        let limit = 128 << 10;
        let derive = |tag: &str, large_account: bool| {
            let mut tree = fixture::tree(&[], true);
            if large_account {
                // Admin's record (key 2) with a member of 256 KiB.
                tree.put(
                    &[&[23], &[1], &[5], &[2], &[0x7f]],
                    om_hbam::OwnedNode::leaf(Vec::new(), vec![0x5a; 256 << 10]),
                );
            }
            let source = std::fs::read(&fixture::TempFile::from_tree(tag, &tree).0).unwrap();
            let sim = Sim::new(6);
            let folder = open(&sim);
            let held = folder.hold(NAME, Cursor::new(&source)).unwrap();
            let view = view_of(&folder, held.root).unwrap();
            derived_of(&Capped::new(&view, limit), NAME, &held.page_zero).map(|_| ())
        };
        assert_eq!(derive("store-accounts-small", false), Ok(()));
        let refused = derive("store-accounts-large", true);
        assert!(
            matches!(refused, Err(ReadError::TooLarge { .. })),
            "{refused:?}"
        );
    }

    /// A measurement, not a test: the roots tables the pins of `n` files
    /// imported at one start keep (`OM_ROOTS_FILES`, 1, 100 and 1,000 by
    /// default). Each import is an entry of its own, and each file's pin
    /// keeps that entry's table of every tree then: about n²/2 entries in
    /// all. A start that imports nothing pins one entry for every file,
    /// which share one table. For a release build:
    ///
    /// ```sh
    /// cargo test --release -p om-server --lib -- --ignored roots_tables --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement for release builds"]
    fn roots_tables_kept_by_imported_files() {
        let source = file("store-roots", 0);
        let counts: Vec<usize> = std::env::var("OM_ROOTS_FILES")
            .map(|v| v.split(',').map(|n| n.parse().unwrap()).collect())
            .unwrap_or_else(|_| vec![1, 100, 1000]);
        let entry = std::mem::size_of::<(TreeName, Root)>();
        for n in counts {
            let sim = Sim::new(1);
            let folder = StoreFolder::open_with(&sim.fs(), config(64 << 20)).unwrap();
            let started = std::time::Instant::now();
            let held: Vec<Held> = (0..n)
                .map(|i| {
                    folder
                        .hold(&format!("File {i:04}.fmp12"), Cursor::new(&source))
                        .unwrap()
                })
                .collect();
            // Tables shared by pins of one entry count once.
            let mut tables: Vec<&[(TreeName, Root)]> = held.iter().map(|h| h.pin.roots()).collect();
            tables.sort_by_key(|t| t.as_ptr());
            tables.dedup_by_key(|t| t.as_ptr());
            let entries: usize = tables.iter().map(|t| t.len()).sum();
            let names: usize = tables
                .iter()
                .flat_map(|t| t.iter())
                .map(|(name, _)| name.as_bytes().len())
                .sum();
            println!(
                "{n} files imported in {:.1} s: {} tables, {entries} entries, {:.1} MB \
                 ({entry} bytes each and their names, {names} bytes)",
                started.elapsed().as_secs_f64(),
                tables.len(),
                (entries * entry + names) as f64 / 1e6,
            );
            drop(held);
            let again: Vec<Held> = (0..n)
                .map(|i| {
                    folder
                        .hold(&format!("File {i:04}.fmp12"), Cursor::new(&source))
                        .unwrap()
                })
                .collect();
            let mut tables: Vec<&[(TreeName, Root)]> =
                again.iter().map(|h| h.pin.roots()).collect();
            tables.sort_by_key(|t| t.as_ptr());
            tables.dedup_by_key(|t| t.as_ptr());
            println!(
                "  held again, none imported: {} table(s), {} entries",
                tables.len(),
                tables.iter().map(|t| t.len()).sum::<usize>()
            );
        }
    }
}
