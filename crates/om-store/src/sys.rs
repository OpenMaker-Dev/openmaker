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
//! `state`, `log/00000001`. A name is not empty, not `.` or `..`, and
//! holds no `/`, `\` or NUL. `""` is the store folder itself.
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
use std::time::Duration;

/// The result of an operation of this layer.
pub type Result<T> = std::result::Result<T, Error>;

/// The longest path the store uses, in bytes.
pub const MAX_PATH: usize = 1024;

/// The most names in a path.
pub const MAX_DEPTH: usize = 16;

/// The files and folders of one store folder.
pub trait Fs: Send + Sync {
    /// Take the store folder's lock for this process (section 6.1). It is
    /// held until the returned value is dropped or the process ends. A
    /// lock another process holds, or this process holds already, fails
    /// with [`ErrorKind::Locked`] without waiting.
    fn lock(&self) -> Result<FolderLock>;

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
    /// after a flush, like a write.
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
pub struct FolderLock {
    _held: Box<dyn Send + Sync>,
}

impl FolderLock {
    /// A lock that `held` keeps until it is dropped (for implementations).
    pub fn new(held: impl Send + Sync + 'static) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

impl fmt::Debug for FolderLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FolderLock")
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
    /// Not a path inside the store folder, or a rename between folders.
    InvalidPath,
    /// The system cannot do it.
    Unsupported,
    /// Another error the system reported.
    Other,
}

/// The operation that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
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
            ErrorKind::Unsupported => "this system cannot do it",
            ErrorKind::Other => "it failed",
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
    for name in &names {
        if name.is_empty() || *name == "." || *name == ".." || name.contains(['\\', '\0']) {
            return Err(invalid());
        }
    }
    Ok(names)
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
