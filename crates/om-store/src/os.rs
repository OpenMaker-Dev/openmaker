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
//!   Unix, when the file's blocks do not cover the part below its old size
//!   (a sparse file), that part is written back, which fills its holes. On
//!   a file system that compresses zeros or copies on every write (ZFS,
//!   btrfs), none of this keeps later writes from running out of space.
//! - **No links** (`crate::sys`). Every name along a path is checked with
//!   `symlink_metadata` before the operation, and a file is opened with
//!   `O_NOFOLLOW` where its value is known (Linux, macOS) and then checked
//!   to be the entry at its path (Unix) or not a link (Windows, opened
//!   with `FILE_FLAG_OPEN_REPARSE_POINT`). What remains: a race, a
//!   process with the server's own rights that swaps a folder for a link
//!   between the check and the use (closing it needs operations relative
//!   to an open folder, which the standard library does not offer); and
//!   hard links, which are not detected.
//! - **The lock** is `File::try_lock` (`flock` on Unix, `LockFileEx` on
//!   Windows) on the file [`sys::LOCK_NAME`] in the store folder, created
//!   when missing and never removed: no operation takes that name. The
//!   system releases it when the process ends.
//! - **The store folder** ([`Fs::create_root`]): made if missing, in a
//!   folder above that must exist, which is flushed on every call (the
//!   folder that really holds the entry, after following links).
//! - **Permissions.** On Unix, new files are readable by the server's
//!   account only (0600) and new folders 0700 (`docs/server-storage.md`
//!   section 11).
//! - **Windows** compiles, but a folder flush there fails as unsupported:
//!   the standard library cannot open a folder, so a create or rename
//!   cannot be made durable, and nothing that needs one claims it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::sys::{
    self, Clock, Error, ErrorKind, File, FileOptions, FolderLock, Fs, LockRecord, Op, Random,
    Result,
};

/// The name of the lock file in the store folder.
pub const LOCK_FILE: &str = sys::LOCK_NAME;

/// How many bytes [`File::allocate`] writes at a time.
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

/// `O_NOFOLLOW`: the generic Linux value (asm-generic/fcntl.h), the value
/// of Linux on ARM and POWER (their asm/fcntl.h), and macOS's.
#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
))]
const O_NOFOLLOW: Option<i32> = Some(0o400000);

#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "powerpc",
        target_arch = "powerpc64"
    )
))]
const O_NOFOLLOW: Option<i32> = Some(0o100000);

#[cfg(target_vendor = "apple")]
const O_NOFOLLOW: Option<i32> = Some(0x100);

#[cfg(all(
    unix,
    not(any(
        target_vendor = "apple",
        all(
            target_os = "linux",
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "riscv32",
                target_arch = "riscv64",
                target_arch = "s390x",
                target_arch = "loongarch64",
                target_arch = "arm",
                target_arch = "aarch64",
                target_arch = "powerpc",
                target_arch = "powerpc64"
            )
        )
    ))
))]
const O_NOFOLLOW: Option<i32> = None;

/// Opens a link itself rather than what it leads to (Windows).
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// A store folder on the operating system's file system. Clones share the
/// record of the lock they gave out.
#[derive(Clone, Debug)]
pub struct OsFs {
    root: PathBuf,
    issued: Arc<Mutex<Option<LockRecord>>>,
}

impl OsFs {
    /// The store folder at `root`. Nothing is read or created yet. Links
    /// on the way to `root` are followed; none below it are.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            issued: Arc::default(),
        }
    }

    /// The store folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The full path of `path`, after checking that no name along it that
    /// exists is a symbolic link.
    fn path(&self, path: &str, op: Op) -> Result<PathBuf> {
        let mut full = self.root.clone();
        let mut checking = true;
        for name in sys::names(path, op)? {
            full.push(name);
            if !checking {
                continue;
            }
            match fs::symlink_metadata(&full) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::new(ErrorKind::Link, op, path))
                }
                Ok(_) => {}
                // Nothing below a missing name exists either.
                Err(e) if e.kind() == io::ErrorKind::NotFound => checking = false,
                Err(e) => return Err(Error::from_io(e, ErrorKind::Other, op, path)),
            }
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
        no_follow(&mut open, options.sync_writes);
        let file = open
            .open(&full)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, op, path))?;
        check_opened(&file, &full).map_err(|kind| Error::new(kind, op, path))?;
        Ok(Box::new(OsFile {
            file,
            path: path.to_owned(),
            sync_after_write: options.sync_writes && O_DSYNC.is_none(),
        }))
    }
}

/// Set what every open of a store file asks for: no link followed at the
/// leaf, owner-only permissions for a new file, and `O_DSYNC` for
/// synchronous writes where it is known.
fn no_follow(open: &mut fs::OpenOptions, sync_writes: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
        let mut flags = O_NOFOLLOW.unwrap_or(0);
        if sync_writes {
            flags |= O_DSYNC.unwrap_or(0);
        }
        open.custom_flags(flags);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = sync_writes;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (open, sync_writes);
}

/// Whether the opened `file` is the entry at `full` itself, not what a
/// link there leads to.
fn check_opened(file: &fs::File, full: &Path) -> std::result::Result<(), ErrorKind> {
    let opened = file.metadata().map_err(|_| ErrorKind::Other)?;
    if opened.file_type().is_symlink() {
        return Err(ErrorKind::Link);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let named = fs::symlink_metadata(full).map_err(|_| ErrorKind::Link)?;
        if named.file_type().is_symlink()
            || (named.dev(), named.ino()) != (opened.dev(), opened.ino())
        {
            return Err(ErrorKind::Link);
        }
    }
    #[cfg(not(unix))]
    let _ = full;
    Ok(())
}

impl Fs for OsFs {
    fn create_root(&self) -> Result<()> {
        match folder_builder().create(&self.root) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.root.is_dir() => {}
            Err(e) => return Err(Error::from_io(e, ErrorKind::Other, Op::CreateRoot, "")),
        }
        // The folder that holds the store folder's entry, links on the way
        // to it followed.
        let real = fs::canonicalize(&self.root)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::CreateRoot, ""))?;
        let parent = real
            .parent()
            .ok_or_else(|| Error::new(ErrorKind::InvalidPath, Op::CreateRoot, ""))?;
        sync_folder(parent).map_err(|e| Error::from_io(e, ErrorKind::Flush, Op::CreateRoot, ""))
    }

    fn lock(&self) -> Result<FolderLock> {
        let full = self.root.join(LOCK_FILE);
        if fs::symlink_metadata(&full).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::new(ErrorKind::Link, Op::Lock, LOCK_FILE));
        }
        let mut open = fs::OpenOptions::new();
        open.read(true).write(true).create(true).truncate(false);
        no_follow(&mut open, false);
        let file = open
            .open(&full)
            .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::Lock, LOCK_FILE))?;
        check_opened(&file, &full).map_err(|kind| Error::new(kind, Op::Lock, LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => {
                let (lock, record) = FolderLock::new(file);
                *self.issued.lock().unwrap_or_else(|e| e.into_inner()) = Some(record);
                Ok(lock)
            }
            Err(fs::TryLockError::WouldBlock) => {
                Err(Error::new(ErrorKind::Locked, Op::Lock, LOCK_FILE))
            }
            Err(fs::TryLockError::Error(e)) => {
                Err(Error::from_io(e, ErrorKind::Other, Op::Lock, LOCK_FILE))
            }
        }
    }

    fn owns(&self, lock: &FolderLock) -> bool {
        self.issued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|record| record.is(lock))
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

impl OsFile {
    /// Write `len` bytes from `at`: zeros, or with `rewrite` the bytes
    /// already there (which fills holes and changes nothing else).
    fn fill(&self, mut at: u64, end: u64, rewrite: bool) -> Result<()> {
        let mut chunk = vec![0u8; ZERO_CHUNK.min(end.saturating_sub(at)) as usize];
        while at < end {
            let n = (end - at).min(ZERO_CHUNK) as usize;
            if rewrite {
                let read = self.read_at(at, &mut chunk[..n])?;
                if read < n {
                    // The file is shorter than its size said: the rest
                    // reads as zeros anyway.
                    chunk[read..n].fill(0);
                }
            }
            self.write_at(at, &chunk[..n]).map_err(|e| {
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
        let old = self.size()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let blocks = self
                .file
                .metadata()
                .map_err(|e| Error::from_io(e, ErrorKind::Other, Op::Allocate, &self.path))?
                .blocks();
            let below = old.min(size);
            // Fewer 512-byte blocks than bytes: the file has holes.
            if blocks.saturating_mul(512) < below {
                self.fill(0, below, true)?;
            }
        }
        self.fill(old, size, false)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn kind<T>(result: Result<T>) -> Option<ErrorKind> {
        result.err().map(|e| e.kind())
    }

    #[test]
    fn the_store_folder_keeps_to_itself() {
        use std::os::unix::fs::symlink;
        let base = TempFolder::new("confined");
        let outside = TempFolder::new("outside");
        // A new store folder: made, or found, and its entry flushed.
        let fs = OsFs::new(base.path.join("store"));
        fs.create_root().unwrap();
        fs.create_root().unwrap();
        let root = fs.root().to_owned();

        // A link inside the store leads nowhere: every operation through
        // it is refused, and nothing appears outside.
        symlink(&outside.path, root.join("log")).unwrap();
        std::fs::write(outside.path.join("target"), b"x").unwrap();
        symlink(outside.path.join("target"), root.join("leaf")).unwrap();
        let plain = FileOptions::PLAIN;
        assert_eq!(kind(fs.create("log/escaped", plain)), Some(ErrorKind::Link));
        assert_eq!(kind(fs.open("log/target", plain)), Some(ErrorKind::Link));
        assert_eq!(kind(fs.open("leaf", plain)), Some(ErrorKind::Link));
        assert_eq!(kind(fs.create_folder("log/x")), Some(ErrorKind::Link));
        assert_eq!(kind(fs.list("log")), Some(ErrorKind::Link));
        assert_eq!(kind(fs.flush_folder("log")), Some(ErrorKind::Link));
        assert_eq!(kind(fs.remove("log/target")), Some(ErrorKind::Link));
        assert_eq!(
            kind(fs.rename("log/target", "log/moved")),
            Some(ErrorKind::Link)
        );
        assert!(!outside.path.join("escaped").exists());
        assert!(outside.path.join("target").exists());

        // The lock's name is the lock's alone.
        let lock = fs.lock().unwrap();
        drop(fs.create("x", plain).unwrap());
        for refused in [
            kind(fs.remove(LOCK_FILE)),
            kind(fs.rename(LOCK_FILE, "y")),
            kind(fs.rename("x", LOCK_FILE)),
            kind(fs.create(LOCK_FILE, plain)),
            kind(fs.open(LOCK_FILE, plain)),
            // The lock in another case, as a file system that ignores case
            // would read it, and names that break the rule.
            kind(fs.rename("LOCK", "old-lock")),
            kind(fs.remove("Lock")),
            kind(fs.create("State", plain)),
            kind(fs.create("a:b", plain)),
            kind(fs.create("trailing.", plain)),
            kind(fs.create("nul.txt", plain)),
            kind(fs.create("log/com1", plain)),
            kind(fs.create("caf\u{e9}", plain)),
        ] {
            assert_eq!(refused, Some(ErrorKind::InvalidPath));
        }
        drop(fs.create("com", plain).unwrap());
        assert_eq!(kind(fs.lock()), Some(ErrorKind::Locked));
        assert!(fs.owns(&lock));
        assert!(fs.clone().owns(&lock));
        // Another Fs of the same folder did not give it out.
        assert!(!OsFs::new(&root).owns(&lock));
        // Nor a lock made outside any Fs, nor one let go.
        let (forged, _) = FolderLock::new(());
        assert!(!fs.owns(&forged));
        drop(lock);
        let live = fs.lock().unwrap();
        assert!(fs.owns(&live));
    }

    /// `create_root` makes only the store folder, and fails, saying what to
    /// do, where it cannot make the folder's entry durable.
    #[test]
    fn create_root_needs_a_folder_above_it_can_flush() {
        use std::os::unix::fs::PermissionsExt;
        let base = TempFolder::new("root");
        let missing = OsFs::new(base.path.join("missing/store"));
        let error = missing.create_root().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(
            error.to_string().contains("create that folder first"),
            "{error}"
        );
        assert!(!base.path.join("missing").exists());

        // A folder above that this account may write and search but not
        // read: the store folder can be made, but its entry not flushed.
        let above = base.path.join("above");
        fs::create_dir(&above).unwrap();
        fs::set_permissions(&above, fs::Permissions::from_mode(0o300)).unwrap();
        let readable = fs::File::open(&above).is_ok(); // run as root
        let result = OsFs::new(above.join("store")).create_root();
        fs::set_permissions(&above, fs::Permissions::from_mode(0o700)).unwrap();
        if !readable {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Flush);
            assert!(error.to_string().contains("read access"), "{error}");
            // A retry finds the store folder and fails the same way rather
            // than pass over the flush.
            fs::set_permissions(&above, fs::Permissions::from_mode(0o300)).unwrap();
            let again = OsFs::new(above.join("store")).create_root();
            fs::set_permissions(&above, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(again.unwrap_err().kind(), ErrorKind::Flush);
        }
    }

    #[test]
    fn allocate_fills_the_holes_of_a_sparse_file() {
        use std::os::unix::fs::MetadataExt;
        let folder = TempFolder::new("sparse");
        let fs = folder.fs();
        drop(fs.create("s", FileOptions::PLAIN).unwrap());
        let path = folder.path.join("s");
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(65536)
            .unwrap();
        let file = fs.open("s", FileOptions::PLAIN).unwrap();
        file.write_at(100, b"kept").unwrap();
        file.allocate(65536).unwrap();
        file.flush().unwrap();
        assert!(fs::metadata(&path).unwrap().blocks() * 512 >= 65536);
        let mut bytes = [0; 4];
        file.read_at(100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"kept");
    }
}
