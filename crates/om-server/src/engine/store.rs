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
//! SHA-256 and its page 0; a checkpoint then makes it durable. A stop at any
//! point leaves the store holding the file whole, as imported or as before,
//! and never part of an import: what an earlier start left under an
//! import's name is dropped when the folder is opened.
//!
//! The state readers need besides the tree ([`Derived`]) is computed from
//! the view when the file is hosted. At this step the engine takes no
//! changes: every file is hosted read-only ([`ReadOnlyCause::StoreEngine`]),
//! each write operation is refused as a file the server holds read-only
//! refuses it, and what the engine cannot do answers a typed "unavailable"
//! ([`StoreEngine::submit`], docs/server-storage.md 11 lists each answer).
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
    not_run, run, Credentials, Draft, Environment, FileNotSaved, Hold, HostEnvironment, Locks,
    Operation, Outcome, ReadError, Reply, StorageError, View, WRITER,
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
/// The names of the trees that hold files, and of a tree an import is
/// filling, each followed by a number.
const FILE_TREE: &str = "om-server:file:";
const IMPORTING: &str = "om-server:importing:";

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
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "reading the file: {e}"),
            StoreError::Folder(e) => write!(f, "the store folder: {e}"),
            StoreError::Open(e) => write!(f, "the store's page file does not open: {e:?}"),
            StoreError::Foreign(what) => write!(
                f,
                "the store folder holds {what}, which this server did not write: move it \
                 away, and its files are imported again"
            ),
            StoreError::Store(e) => write!(f, "the store: {e}"),
            StoreError::Import(e) => write!(f, "the import refused the file: {e}"),
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
}

impl StoreFolder {
    /// Open the store folder `fs` gives, making it and its page file when
    /// missing, its lock held for as long as the value lives: a folder
    /// another process holds is refused. One whose page file holds trees
    /// without this server's mark ([`FORMAT_MARK`]) is refused; what an
    /// import left part way at an earlier start is dropped.
    pub fn open(fs: &dyn Fs, cache_bytes: u64) -> Result<StoreFolder, StoreError> {
        Self::open_with(fs, config(cache_bytes))
    }

    /// [`StoreFolder::open`] with the page file's settings `config`.
    pub(crate) fn open_with(fs: &dyn Fs, config: Config) -> Result<StoreFolder, StoreError> {
        fs.create_root()?;
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
        let left: Vec<TreeName> = self
            .pages
            .tree_names()
            .into_iter()
            .filter(|name| name.as_bytes().starts_with(IMPORTING.as_bytes()))
            .collect();
        if left.is_empty() {
            return Ok(());
        }
        let _writing = self.lock_writing();
        for name in &left {
            warn!(
                "the store: dropping {:?}, an import a stop left part way",
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
    /// read whole once for its digest, and again for an import; it is
    /// never written. Pinned, so that the version read is kept from reuse.
    pub fn hold<R: IoRead + Seek>(&self, name: &str, mut source: R) -> Result<Held, StoreError> {
        let digest = digest(&mut source)?;
        let _writing = self.lock_writing();
        let meta = Self::meta();
        let meta_root = self
            .pages
            .root(&meta)
            .ok_or_else(|| StoreError::Foreign("no mark".into()))?;
        let recorded = Recorded::read(&self.pages, meta_root, name)?;
        if let Some(recorded) = recorded.as_ref().filter(|r| r.source == digest) {
            if self.pages.root(&recorded.tree).is_some() {
                let pin = self.pages.pin_newest()?;
                let root = root_in(&pin, &recorded.tree)?;
                return Ok(Held {
                    pin,
                    root,
                    page_zero: recorded.page_zero.clone(),
                    imported: false,
                });
            }
        }
        source.seek(SeekFrom::Start(0))?;
        let number = next_number(&self.pages, meta_root)?;
        let importing = tree_name(IMPORTING, number);
        let named = tree_name(FILE_TREE, number);
        let first = self.next_entry()?;
        let imported = paged::import(
            io::BufReader::with_capacity(64 << 10, source),
            &mut OnPages { pages: &self.pages },
            &importing,
            first,
            Batch::default(),
        )
        .map_err(StoreError::Import)?;
        let file = vec![FILES.to_vec(), name.as_bytes().to_vec()];
        let row = |key: &[u8], value: Vec<u8>| Edit::Put {
            path: [&file[..], &[key.to_vec()]].concat(),
            node: Node::Value(value),
        };
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
                node: Node::Value((number + 1).to_be_bytes().to_vec()),
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
struct Digest {
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
        }
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

/// The store engine: the store folder it hosts files from, and the
/// environment the operations it runs read the world from.
pub struct StoreEngine {
    folder: Arc<StoreFolder>,
    environment: Box<dyn Environment>,
}

impl std::fmt::Debug for StoreEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreEngine")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

/// Why each write is refused: what the file engine says of a file it holds
/// read-only, and why.
const READ_ONLY: &str = "the file is open read-only: the store engine takes no changes yet";

impl StoreEngine {
    pub fn new(folder: Arc<StoreFolder>) -> Self {
        StoreEngine {
            folder,
            environment: Box::new(HostEnvironment),
        }
    }

    /// The store folder files are hosted from.
    pub(crate) fn folder(&self) -> &Arc<StoreFolder> {
        &self.folder
    }

    /// The version `file` is hosted at, for reading.
    pub fn read<'h>(&self, file: &'h Hosted) -> super::reading::Reading<'h> {
        super::reading::Reading::of_store(file, file.store_file())
    }

    /// Run `operation` on `file` as the store engine can: the lock table's
    /// operations as the file engine runs them, op60 with the modification
    /// count read from the file's view, and every operation that would
    /// change the file refused as a file the server holds read-only refuses
    /// it: nothing of it is stored, and the reply says so where it can
    /// (`not_run`: an op240 Save is told the file is read-only, the rest
    /// that it was not stored, with the save error a read-only file gives).
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
                let mod_count = match op.request.want_mod_count {
                    true => match self.mod_count(file, &op.request.path) {
                        Ok(count) => Some(count),
                        Err(error) => {
                            warn!("{:?}: op60: {error}", file.name);
                            return Outcome::new(
                                Reply::TakeLock(crate::writes::lock_unavailable()),
                            );
                        }
                    },
                    false => None,
                };
                let mut locks = Locks::new(file.locks(&WRITER));
                let reply = crate::writes::lock_counted(op, &file.name, mod_count, &mut locks);
                locks.keep();
                Outcome::new(Reply::TakeLock(reply))
            }
            Operation::SaveFile => Outcome::new(Reply::SaveFile(Err(FileNotSaved::ReadOnly))),
            other => {
                info!("{:?}: not done: {READ_ONLY}", file.name);
                not_run(other, &read_only_error())
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

    /// The modification count of the node at `path`, as the file engine
    /// gives it for op60 (`Store::mod_count`).
    fn mod_count(&self, file: &Hosted, path: &[Vec<u8>]) -> Result<Vec<u8>, ReadError> {
        let stored = file.store_file();
        let view = stored.view()?;
        crate::derived::mod_count(&view, path, &file.name)
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

/// The error a write refused on a file hosted read-only says: the save
/// error a read-only file's save gives on the file engine (800).
fn read_only_error() -> StorageError {
    StorageError {
        code: crate::hosting::save_error(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            READ_ONLY,
        )),
        message: READ_ONLY.to_owned(),
        unknown: false,
    }
}

/// The state of `file` the store engine computes from `view` when it is
/// hosted, page 0 as imported.
pub(crate) fn derived_of(
    view: &dyn View,
    name: &str,
    page_zero: &[u8],
) -> Result<Derived, ReadError> {
    let catalog = crate::derived::read_catalog(view).map_err(|e| ReadError::Unreadable {
        path: Vec::new(),
        why: format!("the schema does not read: {e}"),
    })?;
    let mut derived = Derived::new(view, catalog, name)?;
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

    /// A store folder whose page file holds trees but not this server's
    /// mark is refused, and so is one with another mark.
    #[test]
    fn a_folder_this_server_did_not_write_is_refused() {
        let sim = Sim::new(3);
        {
            let fs = sim.fs();
            let lock = fs.lock().unwrap();
            let pages = PageFile::create(&fs, &lock, config(CACHE)).unwrap();
            let res = pages.reserve(1, 16, Purpose::Entry).unwrap();
            let edits = [Edit::Put {
                path: vec![b"x".to_vec()],
                node: Node::Value(b"y".to_vec()),
            }];
            let other = TreeName::new(b"another".to_vec()).unwrap();
            paged::apply_unit(&res, &other, None, 1, &edits).unwrap();
            drop(res);
            let id = EntryId {
                index: pages.newest_entry(),
                term: 0,
                hash: Hash::default(),
            };
            pages.checkpoint(id, None).unwrap();
        }
        sim.restart();
        assert!(matches!(
            StoreFolder::open(&sim.fs(), CACHE),
            Err(StoreError::Foreign(_))
        ));
    }
}
