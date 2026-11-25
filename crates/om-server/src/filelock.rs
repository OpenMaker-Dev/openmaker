//! Advisory locks that keep two OpenMaker processes (or two opens in one
//! process, through another path to the same file) from writing one file
//! at the same time: the desktop's local host and OpenMaker Server each
//! hold the lock of every file they host, from opening it ([`open_locked`],
//! the one implementation for both) until they stop hosting it.
//!
//! The lock is `flock` on an open descriptor: exclusive for a file opened
//! for writing, shared for one opened read-only, never waiting. Saves
//! replace the file with a new one ([`crate::store::Store::save`]), so the
//! store locks the new file before it is renamed into place and only then
//! lets go of the old one: whoever opens the file by its name finds it
//! locked, before the rename and after. A descriptor opened just before
//! such a rename still names the old file, whose lock is free once the
//! save is done: a lock only counts when the descriptor is still the file
//! at its name, which [`open_locked`] checks after locking. The OS releases
//! the lock with the descriptor, also when the process ends. Platforms
//! without `flock` (Windows) and file systems that do not support it (some
//! network shares) report [`io::ErrorKind::Unsupported`]; the desktop
//! opens a file there read-only, and OpenMaker Server hosts it unlocked, as
//! it always did.

use std::fs::File;
use std::io;
use std::path::Path;

/// How a file is locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockKind {
    /// Opened for writing: no one else may open it.
    Exclusive,
    /// Opened read-only: others may read it, no one may write it.
    Shared,
}

/// A locked open file. Dropping it releases the lock.
#[derive(Debug)]
pub(crate) struct FileLock {
    file: File,
    kind: LockKind,
}

impl FileLock {
    /// Lock `file` without waiting. A lock held by another open of the
    /// file fails with [`io::ErrorKind::WouldBlock`], a file system or
    /// platform without locks with [`io::ErrorKind::Unsupported`]; the
    /// file comes back with the error.
    pub fn acquire(file: File, kind: LockKind) -> Result<Self, (io::Error, File)> {
        match lock(&file, kind) {
            Ok(()) => Ok(Self { file, kind }),
            Err(e) => Err((e, file)),
        }
    }

    pub fn kind(&self) -> LockKind {
        self.kind
    }

    /// The locked file.
    pub fn file(&self) -> &File {
        &self.file
    }
}

/// A host's file: locked, or open without a lock where it cannot be locked
/// here.
#[derive(Debug)]
pub(crate) enum Locked {
    Lock(FileLock),
    Unlocked(File),
}

impl Locked {
    /// The open file.
    pub fn file(&self) -> &File {
        match self {
            Locked::Lock(lock) => lock.file(),
            Locked::Unlocked(file) => file,
        }
    }

    /// The lock, where there is one.
    pub fn into_lock(self) -> Option<FileLock> {
        match self {
            Locked::Lock(lock) => Some(lock),
            Locked::Unlocked(_) => None,
        }
    }
}

/// What an attempt of [`open_locked`] does with the file it locked.
pub(crate) enum Settled<T> {
    /// Keep it.
    Done(T),
    /// Open it again: what was learned changes how (a name was added
    /// between the open and the lock, a leftover copy stays).
    Again,
}

/// Why [`open_locked`] did not open a file.
#[derive(Debug)]
pub(crate) enum NotOpened<E> {
    /// Another open holds its lock, or it kept being replaced.
    InUse,
    Failed(E),
}

/// How many times [`open_locked`] tries again when the file at the name
/// was replaced between its open and its lock (another OpenMaker saving
/// it).
const OPEN_ATTEMPTS: u32 = 8;

/// Open a host's file and lock it without waiting, so that the lock is on
/// the file its name leads to now: the one implementation for the
/// desktop's local host and OpenMaker Server. `open` opens the file (given
/// the attempt's number) and says how to lock it, with what it learned on
/// the way (`C`). The lock counts only while the descriptor is still the
/// file at its name (`current`): one opened just before another
/// OpenMaker's save renamed a new file there holds the old file, whose lock
/// is free once that save is done, so the open is tried again; a file whose
/// lock another open holds, or that keeps changing, is in use. Where the
/// file cannot be locked (no `flock` here) or checked (no file identity),
/// it comes back unlocked. `settle` then decides with the file locked and
/// current: keep it, or open again.
pub(crate) fn open_locked<C, T, E: From<io::Error>>(
    open: &mut dyn FnMut(u32) -> Result<(File, LockKind, C), E>,
    current: &mut dyn FnMut(&File, &C) -> io::Result<bool>,
    settle: &mut dyn FnMut(Locked, C) -> Result<Settled<T>, E>,
) -> Result<T, NotOpened<E>> {
    for attempt in 0..OPEN_ATTEMPTS {
        let (file, kind, context) = open(attempt).map_err(NotOpened::Failed)?;
        let mut locked = match FileLock::acquire(file, kind) {
            Ok(lock) => Locked::Lock(lock),
            Err((e, _)) if e.kind() == io::ErrorKind::WouldBlock => return Err(NotOpened::InUse),
            Err((e, file)) if e.kind() == io::ErrorKind::Unsupported => Locked::Unlocked(file),
            Err((e, _)) => return Err(NotOpened::Failed(e.into())),
        };
        match current(locked.file(), &context) {
            Ok(true) => {}
            // Replaced (or removed) since the open: the lock guards nothing.
            Ok(false) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // No file identity here (Windows): the lock cannot be checked,
            // and guards nothing.
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                if let Locked::Lock(lock) = locked {
                    locked = Locked::Unlocked(lock.file);
                }
            }
            Err(e) => return Err(NotOpened::Failed(e.into())),
        }
        match settle(locked, context).map_err(NotOpened::Failed)? {
            Settled::Done(done) => return Ok(done),
            Settled::Again => continue,
        }
    }
    Err(NotOpened::InUse)
}

/// Whether `file` is the file at `path` now: the same device and inode,
/// `path` not followed when it is a symbolic link. A descriptor of a file
/// that was renamed over, or a path replaced by a link, is not.
#[cfg(unix)]
pub(crate) fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let open = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    Ok(open.dev() == named.dev() && open.ino() == named.ino())
}

/// How many names (hard links) the open file has.
#[cfg(unix)]
pub(crate) fn link_count(file: &File) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink())
}

#[cfg(not(unix))]
pub(crate) fn same_file(_file: &File, _path: &Path) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(unix))]
pub(crate) fn link_count(_file: &File) -> io::Result<u64> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn lock(file: &File, kind: LockKind) -> io::Result<()> {
    use rustix::fs::{flock, FlockOperation};
    let operation = match kind {
        LockKind::Exclusive => FlockOperation::NonBlockingLockExclusive,
        LockKind::Shared => FlockOperation::NonBlockingLockShared,
    };
    flock(file, operation).map_err(|errno| {
        use rustix::io::Errno;
        // A file system without locks (or a lock manager that is down).
        if [Errno::OPNOTSUPP, Errno::NOTSUP, Errno::NOSYS, Errno::NOLCK].contains(&errno) {
            io::ErrorKind::Unsupported.into()
        } else {
            io::Error::from(errno)
        }
    })
}

#[cfg(not(unix))]
fn lock(_file: &File, _kind: LockKind) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn folder(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("om-filelock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_open_of_a_locked_file_is_refused_until_the_first_ends() {
        let dir = folder("second");
        let path = dir.join("locked.fmp12");
        std::fs::write(&path, b"x").unwrap();
        let open = || File::options().read(true).write(true).open(&path).unwrap();
        let first = FileLock::acquire(open(), LockKind::Exclusive).unwrap();
        for kind in [LockKind::Exclusive, LockKind::Shared] {
            let (refused, _) = FileLock::acquire(open(), kind).unwrap_err();
            assert_eq!(refused.kind(), io::ErrorKind::WouldBlock, "{kind:?}");
        }
        drop(first);
        let shared = FileLock::acquire(open(), LockKind::Shared).unwrap();
        // Readers share; a writer waits for them.
        let reader = FileLock::acquire(open(), LockKind::Shared).unwrap();
        assert!(FileLock::acquire(open(), LockKind::Exclusive).is_err());
        drop((shared, reader));
        assert!(FileLock::acquire(open(), LockKind::Exclusive).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_descriptor_renamed_over_is_no_longer_the_file() {
        let dir = folder("identity");
        let path = dir.join("file.fmp12");
        std::fs::write(&path, b"old").unwrap();
        let old = File::open(&path).unwrap();
        assert!(same_file(&old, &path).unwrap());
        assert_eq!(link_count(&old).unwrap(), 1);
        let new = dir.join("new");
        std::fs::write(&new, b"new").unwrap();
        std::fs::rename(&new, &path).unwrap();
        assert!(!same_file(&old, &path).unwrap());
        // A link where the file was is not the file either.
        let current = File::open(&path).unwrap();
        let moved = dir.join("moved.fmp12");
        std::fs::rename(&path, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &path).unwrap();
        assert!(!same_file(&current, &path).unwrap());
        std::fs::hard_link(&moved, dir.join("second name")).unwrap();
        assert_eq!(link_count(&current).unwrap(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
