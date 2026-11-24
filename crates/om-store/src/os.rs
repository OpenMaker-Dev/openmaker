//! The traits of [`crate::sys`] over the operating system. This is the only
//! module of the crate that calls `std::fs`, the system clock or the
//! system's random source.
//!
//! - **Flushes reach the device.** A file flush is `sync_data`: `fdatasync`
//!   on Linux, which also writes the size when it changed; `F_FULLFSYNC` on
//!   macOS, which empties the drive's cache where plain `fsync` does not.
//!   A folder flush opens the folder and syncs it.
//! - **Synchronous writes** ([`FileOptions::sync_writes`]) open the file
//!   with `O_DSYNC` on Linux, so that the write itself reports what the
//!   device did not store. Where that flag is not known (other systems,
//!   and Linux on processors whose value differs), each write is followed
//!   by `sync_data`, which gives the same guarantee at the same point; on
//!   macOS that is the better choice, since `O_DSYNC` there stops at the
//!   drive's cache.
//! - **Allocation writes zeros** and leaves the flush to the caller: the
//!   standard library has no `fallocate` and the crate has no `unsafe`. On
//!   a file system that compresses zeros or copies on every write (ZFS,
//!   btrfs), that does not keep later writes from running out of space.
//! - **The lock** is `File::try_lock` (`flock` on Unix, `LockFileEx` on
//!   Windows) on the file `lock` in the store folder, which is created when
//!   missing and never removed; the store's own files never take that
//!   name. The system releases it when the process ends.
//! - **Permissions.** On Unix, new files are readable by the server's
//!   account only (0600) and new folders 0700 (`docs/server-storage.md`
//!   section 11).
//! - **Windows** compiles, but a folder flush there fails as unsupported:
//!   the standard library cannot open a folder, so a create or rename
//!   cannot be made durable, and nothing that needs one claims it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::sys::{
    self, Clock, Error, ErrorKind, File, FileOptions, FolderLock, Fs, Op, Random, Result,
};

/// The name of the lock file in the store folder.
pub const LOCK_FILE: &str = "lock";

/// How many zeros [`File::allocate`] writes at a time.
const ZERO_CHUNK: u64 = 1 << 20;

/// `O_DSYNC` on Linux where it has the generic value (asm-generic/fcntl.h).
#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
))]
const O_DSYNC: Option<i32> = Some(0o10000);

#[cfg(not(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
)))]
const O_DSYNC: Option<i32> = None;

/// A store folder on the operating system's file system.
#[derive(Clone, Debug)]
pub struct OsFs {
    root: PathBuf,
}

impl OsFs {
    /// The store folder at `root`. Nothing is read or created yet.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the store folder and the folders above it that are missing
    /// (0700 on Unix). A folder that exists is left as it is.
    pub fn create_root(&self) -> Result<()> {
        let mut builder = folder_builder();
        builder
            .recursive(true)
            .create(&self.root)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::CreateFolder, ""))
    }

    fn path(&self, path: &str, op: Op) -> Result<PathBuf> {
        let mut full = self.root.clone();
        for name in sys::names(path, op)? {
            full.push(name);
        }
        Ok(full)
    }

    fn open_file(&self, path: &str, options: FileOptions, create: bool) -> Result<Box<dyn File>> {
        let op = if create { Op::Create } else { Op::Open };
        if path.is_empty() {
            return Err(Error::new(ErrorKind::InvalidPath, op, path));
        }
        let full = self.path(path, op)?;
        let mut open = fs::OpenOptions::new();
        open.read(true).write(true);
        if create {
            open.create_new(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
            if options.sync_writes {
                if let Some(flag) = O_DSYNC {
                    open.custom_flags(flag);
                }
            }
        }
        let file = open
            .open(&full)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, op, path))?;
        Ok(Box::new(OsFile {
            file,
            path: path.to_owned(),
            sync_after_write: options.sync_writes && O_DSYNC.is_none(),
        }))
    }
}

impl Fs for OsFs {
    fn lock(&self) -> Result<FolderLock> {
        let mut open = fs::OpenOptions::new();
        open.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let file = open
            .open(self.root.join(LOCK_FILE))
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::Lock, LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => Ok(FolderLock::new(file)),
            Err(fs::TryLockError::WouldBlock) => {
                Err(Error::new(ErrorKind::Locked, Op::Lock, LOCK_FILE))
            }
            Err(fs::TryLockError::Error(e)) => {
                Err(Error::from_io(e, ErrorKind::Other, Op::Lock, LOCK_FILE))
            }
        }
    }

    fn create(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>> {
        self.open_file(path, options, true)
    }

    fn open(&self, path: &str, options: FileOptions) -> Result<Box<dyn File>> {
        self.open_file(path, options, false)
    }

    fn create_folder(&self, path: &str) -> Result<()> {
        let full = self.path(path, Op::CreateFolder)?;
        folder_builder()
            .create(full)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::CreateFolder, path))
    }

    fn flush_folder(&self, path: &str) -> Result<()> {
        let full = self.path(path, Op::FlushFolder)?;
        sync_folder(&full).map_err(|e| Error::from_io(e, ErrorKind::Flush, Op::FlushFolder, path))
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from_folder, _) = sys::split(from, Op::Rename)?;
        let (to_folder, _) = sys::split(to, Op::Rename)?;
        if from_folder != to_folder {
            return Err(Error::new(ErrorKind::InvalidPath, Op::Rename, to));
        }
        let source = self.path(from, Op::Rename)?;
        let target = self.path(to, Op::Rename)?;
        if fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir()) {
            return Err(Error::new(ErrorKind::Other, Op::Rename, from));
        }
        fs::rename(source, target)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::Rename, from))
    }

    fn remove(&self, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(Error::new(ErrorKind::InvalidPath, Op::Remove, path));
        }
        let full = self.path(path, Op::Remove)?;
        let error = |e| Error::from_io(e, ErrorKind::Other, Op::Remove, path);
        let metadata = fs::symlink_metadata(&full).map_err(error)?;
        if metadata.is_dir() {
            fs::remove_dir(&full).map_err(error)
        } else {
            fs::remove_file(&full).map_err(error)
        }
    }

    fn list(&self, path: &str) -> Result<Vec<String>> {
        let full = self.path(path, Op::List)?;
        let error = |e| Error::from_io(e, ErrorKind::Other, Op::List, path);
        let mut names = Vec::new();
        for entry in fs::read_dir(full).map_err(error)? {
            let name = entry.map_err(error)?.file_name();
            // The store names its own files; a name that is not UTF-8 was
            // put there by something else.
            let name = name
                .into_string()
                .map_err(|_| Error::new(ErrorKind::Other, Op::List, path))?;
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
}

/// An open file of an [`OsFs`].
#[derive(Debug)]
struct OsFile {
    file: fs::File,
    path: String,
    /// Follow each write with `sync_data`: synchronous writes where the
    /// file was not opened with `O_DSYNC`.
    sync_after_write: bool,
}

impl File for OsFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            let at = offset
                .checked_add(done as u64)
                .ok_or_else(|| Error::new(ErrorKind::Read, Op::Read, &self.path))?;
            match pread(&self.file, &mut buf[done..], at) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::from_io(e, ErrorKind::Read, Op::Read, &self.path)),
            }
        }
        Ok(done)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        let mut done = 0;
        while done < data.len() {
            let at = offset
                .checked_add(done as u64)
                .ok_or_else(|| Error::new(ErrorKind::Write, Op::Write, &self.path))?;
            match pwrite(&self.file, &data[done..], at) {
                Ok(0) => {
                    return Err(Error::from_io(
                        io::ErrorKind::WriteZero.into(),
                        ErrorKind::Write,
                        Op::Write,
                        &self.path,
                    ))
                }
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::from_io(e, ErrorKind::Write, Op::Write, &self.path)),
            }
        }
        if self.sync_after_write {
            self.file
                .sync_data()
                .map_err(|e| Error::from_io(e, ErrorKind::Write, Op::Write, &self.path))?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.file
            .sync_data()
            .map_err(|e| Error::from_io(e, ErrorKind::Flush, Op::Flush, &self.path))
    }

    fn allocate(&self, size: u64) -> Result<()> {
        let mut at = self.size()?;
        let zeros = vec![0u8; ZERO_CHUNK.min(size.saturating_sub(at)) as usize];
        while at < size {
            let n = (size - at).min(ZERO_CHUNK) as usize;
            self.write_at(at, &zeros[..n]).map_err(|e| {
                let kind = if e.kind() == ErrorKind::NoSpace {
                    ErrorKind::NoSpace
                } else {
                    ErrorKind::Write
                };
                Error::new(kind, Op::Allocate, &self.path)
            })?;
            at += n as u64;
        }
        Ok(())
    }

    fn size(&self) -> Result<u64> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::Size, &self.path))
    }
}

#[cfg(unix)]
fn pread(file: &fs::File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(unix)]
fn pwrite(file: &fs::File, data: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, data, offset)
}

#[cfg(windows)]
fn pread(file: &fs::File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(windows)]
fn pwrite(file: &fs::File, data: &[u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, data, offset)
}

#[cfg(not(any(unix, windows)))]
fn pread(_file: &fs::File, _buf: &mut [u8], _offset: u64) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(unix, windows)))]
fn pwrite(_file: &fs::File, _data: &[u8], _offset: u64) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

/// How new folders are made: the account's only on Unix.
#[cfg(unix)]
fn folder_builder() -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

#[cfg(not(unix))]
fn folder_builder() -> fs::DirBuilder {
    fs::DirBuilder::new()
}

/// Flush a folder's entries to the device.
#[cfg(unix)]
fn sync_folder(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_folder(_path: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The system's clocks.
#[derive(Clone, Copy, Debug)]
pub struct OsClock {
    start: Instant,
}

impl OsClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for OsClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for OsClock {
    /// A clock set before 1970 reads as the epoch.
    fn wall(&self) -> Duration {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }

    fn monotonic(&self) -> Duration {
        self.start.elapsed()
    }
}

/// The system's random source (`ring`'s, from the operating system).
#[derive(Debug)]
pub struct OsRandom {
    source: ring::rand::SystemRandom,
}

impl OsRandom {
    pub fn new() -> Self {
        Self {
            source: ring::rand::SystemRandom::new(),
        }
    }
}

impl Default for OsRandom {
    fn default() -> Self {
        Self::new()
    }
}

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<()> {
        ring::rand::SecureRandom::fill(&self.source, buf)
            .map_err(|_| Error::new(ErrorKind::Other, Op::Random, ""))
    }
}

/// A store folder in the system's temporary folder, removed with what it
/// holds when dropped (tests).
#[cfg(test)]
pub(crate) struct TempFolder {
    path: PathBuf,
}

#[cfg(test)]
impl TempFolder {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("om-store-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary folder");
        Self { path }
    }

    pub fn fs(&self) -> OsFs {
        OsFs::new(&self.path)
    }
}

#[cfg(test)]
impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
