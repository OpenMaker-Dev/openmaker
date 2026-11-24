//! What the store engine touches outside the process: files and folders,
//! the clock and randomness (`docs/server-storage.md` section 12). The
//! engine reaches them only through these traits. [`crate::os`] implements
//! them over the operating system, [`crate::sim`] in memory with injected
//! crashes and storage faults. The network joins them with replication
//! (section 7), as one more trait here and one more part of the simulated
//! machine.
//!
//! Everything is synchronous: the server is thread-based, and the simulator
//! runs a whole cluster in one thread from a seed.
//!
//! **Paths** are relative to the store folder, with `/` between names:
//! `state`, `log/00000001`. `""` is the store folder itself. One rule for
//! names holds on every system, so that a store folder moves between them
//! and no two names can be one file:
//!
//! - a name is 1 to 255 bytes of lowercase ASCII letters, digits, `.`, `_`
//!   and `-`; it is not `.` or `..` and does not end in `.`. With no
//!   capital letters and nothing outside ASCII, two names that differ
//!   only by case, or by a Unicode form, cannot both exist: a file system
//!   that ignores case (APFS and NTFS by default, ext4 with casefolding)
//!   would take them for one file. Windows reads a trailing `.` and `:`
//!   (a drive or a stream) as other names, and neither is allowed;
//! - Windows device names (`con`, `prn`, `aux`, `nul`, `com0` to `com9`,
//!   `lpt0` to `lpt9`, alone or before a `.`) are refused anywhere;
//! - [`LOCK_NAME`] in the store folder is reserved, in any case: no
//!   operation takes it.
//!
//! The store's own names (`state`, `state.new`, `pages`, `log/` with
//! numbered segments, `blobs/` with hashes written in lowercase hex) keep
//! the rule.
//!
//! **No links.** A store folder holds no symbolic links: an operation
//! whose path runs through one fails with [`ErrorKind::Link`] rather than
//! reach outside the folder. A log on another disk is a mount, not a link.
//! The store folder itself may be reached through links. Hard links are
//! not detected: a file of the store folder that also has a name
//! elsewhere can be changed through that name.
//!
//! **Durability.** A write is durable once a [`File::flush`] of its file
//! has returned `Ok`, or once it has returned on a file opened with
//! [`FileOptions::sync_writes`]. A create, rename or remove is durable once
//! a [`Fs::flush_folder`] of its folder has returned `Ok`. Until then a
//! crash may keep it, lose it or keep a part of it. After a flush that
//! failed, what it covered is in an unknown state and no later flush
//! vouches for it (section 6.12): the engine stops and recovers, and never
//! retries the flush.
//!
//! **Errors** say what failed. That stored bytes are damaged is never an
//! error of this layer: damage is found above it, by hashes.

use std::fmt;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// The result of an operation of this layer.
pub type Result<T> = std::result::Result<T, Error>;

/// The longest path the store uses, in bytes.
pub const MAX_PATH: usize = 1024;

/// The most names in a path.
pub const MAX_DEPTH: usize = 16;

/// The longest name, in bytes.
pub const MAX_NAME: usize = 255;

/// The name in the store folder kept for its lock (the real file system
/// keeps the lock in a file of this name). No path may be it.
pub const LOCK_NAME: &str = "lock";

/// The files and folders of one store folder.
pub trait Fs: Send + Sync {
    /// Make the store folder if it is missing, inside the folder above it,
    /// which must exist, and flush that folder above, on every call: the
    /// store folder's own entry is then durable whether this call made it
    /// or an earlier one that stopped short did. Call it at every start,
    /// before [`Fs::lock`]; it costs one folder flush. A folder above that
    /// is missing fails with [`ErrorKind::NotFound`] (create it first), one
    /// that cannot be opened and flushed with [`ErrorKind::Flush`] (give
    /// the server's account read access to it). Folders further up were
    /// there before the store, and are neither made nor flushed.
    fn create_root(&self) -> Result<()>;

    /// Take the store folder's lock for this process (section 6.1). It is
    /// held until the returned value is dropped or the process ends. A
    /// lock another process holds, or this process holds already, fails
    /// with [`ErrorKind::Locked`] without waiting.
    fn lock(&self) -> Result<FolderLock>;

    /// Whether `lock` is the lock this `Fs` (or a clone of it) gave out
    /// and that is still held: what writes the store's files takes the
    /// lock by value and checks it here, so that one lock makes one
    /// writer. A lock another `Fs` gave out, for this folder or another,
    /// is not this one's.
    fn owns(&self, lock: &FolderLock) -> bool;

    /// Create the file `path`, empty. A name that exists fails with
    /// [`ErrorKind::AlreadyExists`]. The new name is durable after a flush
    /// of its folder.
    fn create(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>>;

    /// Open the existing file `path` for reading and writing.
    fn open(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>>;

    /// Create the folder `path`, empty. Durable after a flush of the folder
    /// that holds it.
    fn create_folder(&self, path: &str) -> Result<()>;

    /// Make every create, rename and remove in the folder `path` durable.
    fn flush_folder(&self, path: &str) -> Result<()>;

    /// Give the file `from` the name `to` in the same folder, replacing a
    /// file of that name. Paths in two folders fail with
    /// [`ErrorKind::InvalidPath`].
    fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// Remove the file `path`, or the folder `path` when it is empty. An
    /// open handle of a removed file keeps working until it is dropped.
    fn remove(&self, path: &str) -> Result<()>;

    /// The names in the folder `path`, sorted by their bytes.
    fn list(&self, path: &str) -> Result<Vec<String>>;
}

/// An open file. Reads and writes name their place; there is no cursor.
pub trait File: Send + Sync {
    /// Read from `offset` into `buf`, as far as the file reaches. Returns
    /// how many bytes were read: fewer than `buf.len()` only at the end of
    /// the file.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// Write all of `data` at `offset`, extending the file when it reaches
    /// past the end (a gap reads as zeros). On an error some of `data` may
    /// have been written.
    fn write_at(&self, offset: u64, data: &[u8]) -> Result<()>;

    /// Make everything written to the file so far durable, its size
    /// included. On an error, what the flush covered is in an unknown
    /// state, and a later flush that succeeds does not vouch for it.
    fn flush(&self) -> Result<()>;

    /// Make the file at least `size` bytes long, taking the space now, so
    /// that writes below `size` never fail for lack of space on a file
    /// system that writes in place. The new bytes read as zeros. Durable
    /// after a flush, like a write. On Unix a part below the old size that
    /// the file system holds as a hole is filled too, by writing back the
    /// bytes there, so no other write to the file may run meanwhile;
    /// elsewhere only the part past the old size is taken.
    fn allocate(&self, size: u64) -> Result<()>;

    /// The file's size in bytes.
    fn size(&self) -> Result<u64>;
}

/// How a file is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileOptions {
    /// Every write is durable when it returns, as if a flush followed it,
    /// and a write the device could not store fails itself rather than a
    /// later flush (section 6.3, segments).
    pub sync_writes: bool,
}

impl FileOptions {
    /// Writes durable after a flush.
    pub const PLAIN: FileOptions = FileOptions { sync_writes: false };
    /// Writes durable when they return.
    pub const SYNC: FileOptions = FileOptions { sync_writes: true };
}

/// The store folder's lock (section 6.1), released when dropped.
///
/// Each lock carries a token of its own, which nothing can copy: an `Fs`
/// keeps a [`LockRecord`] of the lock it gave out and recognizes that lock,
/// and only that one, for as long as it lives.
pub struct FolderLock {
    token: Arc<Token>,
    _held: Box<dyn Send + Sync>,
}

/// What makes a lock itself: its allocation, which outlives no lock.
struct Token;

impl FolderLock {
    /// A new lock, which `held` keeps until it is dropped, and the record
    /// that recognizes it (for implementations).
    pub fn new(held: impl Send + Sync + 'static) -> (Self, LockRecord) {
        let token = Arc::new(Token);
        let record = LockRecord(Arc::downgrade(&token));
        let lock = Self {
            token,
            _held: Box::new(held),
        };
        (lock, record)
    }
}

impl fmt::Debug for FolderLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FolderLock")
    }
}

/// What an `Fs` keeps of the lock it gave out.
#[derive(Debug, Clone)]
pub struct LockRecord(Weak<Token>);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token")
    }
}

impl LockRecord {
    /// Whether `lock` is the recorded lock, and still held. The record
    /// keeps the token's allocation, so no other lock can share its
    /// address while the record exists.
    pub fn is(&self, lock: &FolderLock) -> bool {
        self.0.strong_count() > 0 && Weak::ptr_eq(&self.0, &Arc::downgrade(&lock.token))
    }
}

/// The time.
pub trait Clock: Send + Sync {
    /// The time since the Unix epoch by the system's wall clock. It can
    /// jump in either direction; nothing the store keeps depends on it
    /// agreeing with another server's (section 14).
    fn wall(&self) -> Duration;

    /// The time since a start fixed for the process. It never goes back.
    fn monotonic(&self) -> Duration;
}

/// Randomness, for ids and nonces.
pub trait Random: Send + Sync {
    /// Fill `buf` with random bytes.
    fn fill(&self, buf: &mut [u8]) -> Result<()>;
}

/// What failed, and how.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    op: Op,
    path: String,
    source: Option<std::io::Error>,
}

/// How an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The file or folder does not exist.
    NotFound,
    /// A file or folder of that name exists.
    AlreadyExists,
    /// The device is full, or the account's quota is spent.
    NoSpace,
    /// A read failed.
    Read,
    /// A write failed. Some of it may have been written.
    Write,
    /// A flush failed. What it covered is in an unknown state.
    Flush,
    /// Another process, or this one, holds the store folder's lock.
    Locked,
    /// The process has stopped: in the simulator, it crashed or was
    /// stopped and nothing it does reaches the disk any more.
    Halted,
    /// Not a path inside the store folder, a name that breaks the rule for
    /// names (module docs), or a rename between folders.
    InvalidPath,
    /// A name along the path is a symbolic link (module docs).
    Link,
    /// The system cannot do it.
    Unsupported,
    /// Another error the system reported.
    Other,
}

/// The operation that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    CreateRoot,
    Lock,
    Create,
    Open,
    CreateFolder,
    FlushFolder,
    Rename,
    Remove,
    List,
    Read,
    Write,
    Flush,
    Allocate,
    Size,
    Random,
}

impl Op {
    /// The operation in words, for messages.
    pub fn verb(self) -> &'static str {
        match self {
            Op::CreateRoot => "set up",
            Op::Lock => "lock",
            Op::Create => "create",
            Op::Open => "open",
            Op::CreateFolder => "create the folder",
            Op::FlushFolder => "flush the folder",
            Op::Rename => "rename",
            Op::Remove => "remove",
            Op::List => "list",
            Op::Read => "read",
            Op::Write => "write",
            Op::Flush => "flush",
            Op::Allocate => "allocate space for",
            Op::Size => "read the size of",
            Op::Random => "draw random bytes for",
        }
    }
}

impl Error {
    /// An error of `kind` in operation `op` on `path`.
    pub fn new(kind: ErrorKind, op: Op, path: &str) -> Self {
        Self {
            kind,
            op,
            path: path.to_owned(),
            source: None,
        }
    }

    /// The system's `error` in operation `op` on `path`. Errors that say
    /// what is wrong keep their kind (not found, exists, no space,
    /// unsupported); any other is `failed`, the operation's own failure.
    pub fn from_io(error: std::io::Error, failed: ErrorKind, op: Op, path: &str) -> Self {
        use std::io::ErrorKind as Io;
        let kind = match error.kind() {
            Io::NotFound => ErrorKind::NotFound,
            Io::AlreadyExists => ErrorKind::AlreadyExists,
            Io::StorageFull | Io::QuotaExceeded => ErrorKind::NoSpace,
            Io::Unsupported => ErrorKind::Unsupported,
            _ => failed,
        };
        Self {
            kind,
            op,
            path: path.to_owned(),
            source: Some(error),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn op(&self) -> Op {
        self.op
    }

    /// The path, relative to the store folder.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ErrorKind::NotFound => "it does not exist",
            ErrorKind::AlreadyExists => "it already exists",
            ErrorKind::NoSpace => "the disk is full",
            ErrorKind::Read => "the read failed",
            ErrorKind::Write => "the write failed, and what it wrote is unknown",
            ErrorKind::Flush => "the flush failed, and what it covered is in an unknown state",
            ErrorKind::Locked => "another process holds the store folder's lock",
            ErrorKind::Halted => "the process has stopped",
            ErrorKind::InvalidPath => "not a path inside the store folder",
            ErrorKind::Link => {
                "a name on its path is a symbolic link, which a store folder may not hold"
            }
            ErrorKind::Unsupported => "this system cannot do it",
            ErrorKind::Other => "it failed",
        };
        let what = match (self.op, self.kind) {
            (Op::CreateRoot, ErrorKind::NotFound) => {
                "the folder above it does not exist; create that folder first"
            }
            (Op::CreateRoot, ErrorKind::Flush) => {
                "the folder above it could not be flushed, so the store folder might not \
                 survive a power cut; give the server's account read access to that folder"
            }
            _ => what,
        };
        let path = if self.path.is_empty() {
            "the store folder"
        } else {
            &self.path
        };
        write!(f, "cannot {} {path}: {what}", self.op.verb())?;
        if let Some(source) = &self.source {
            write!(f, " ({source})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// The names of `path`, checked (module docs). `""` is the store folder
/// and has none.
pub fn names(path: &str, op: Op) -> Result<Vec<&str>> {
    let invalid = || Error::new(ErrorKind::InvalidPath, op, path);
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > MAX_PATH {
        return Err(invalid());
    }
    let names: Vec<&str> = path.split('/').collect();
    if names.len() > MAX_DEPTH {
        return Err(invalid());
    }
    if !names.iter().all(|name| allowed(name)) || path.eq_ignore_ascii_case(LOCK_NAME) {
        return Err(invalid());
    }
    Ok(names)
}

/// Whether `name` keeps the rule for names (module docs).
fn allowed(name: &str) -> bool {
    let bytes = name.as_bytes();
    let charset = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b);
    if bytes.is_empty()
        || bytes.len() > MAX_NAME
        || !bytes.iter().all(charset)
        || name == "."
        || name == ".."
        || name.ends_with('.')
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name);
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    !device
}

/// The folder `path` is in (`""` for the store folder) and its name.
pub fn split(path: &str, op: Op) -> Result<(&str, &str)> {
    let names = names(path, op)?;
    match path.rsplit_once('/') {
        Some((folder, name)) => Ok((folder, name)),
        None if !names.is_empty() => Ok(("", path)),
        None => Err(Error::new(ErrorKind::InvalidPath, op, path)),
    }
}
