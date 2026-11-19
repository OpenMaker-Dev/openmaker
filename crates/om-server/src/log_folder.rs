//! The event log's folder (`crate::eventlog`), opened once and checked;
//! its files are made, opened, renamed and listed relative to that handle,
//! never through a symbolic link (`docs/admin-console.md`, "The event log
//! and alerts").
//!
//! On Unix the folder must belong to the server's user and nobody else
//! may write in it, or the server does not start: whoever can write in it
//! could swap a log for a link. A missing folder is made 0700. The folder's
//! path is taken without trailing separators and `.` components, so that
//! `linked/` and `linked/.` are the link `linked` and refused. Each log
//! must be a regular file of the server's user with no other link (an open
//! file a rotation has since unlinked has none, and is read); one that
//! other users may read or write is made 0600 (its lines name accounts and
//! client addresses), the `-old` files when the folder is opened, and the
//! process log says so. Elsewhere the folder's inherited permissions apply,
//! and links are refused by looking at each name before it is opened.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A log folder, checked.
#[derive(Debug)]
pub(crate) struct LogFolder {
    path: PathBuf,
    #[cfg(unix)]
    dir: std::os::fd::OwnedFd,
}

/// The size and modification time of a regular file.
pub(crate) type FileStat = (u64, Option<SystemTime>);

/// `path` without trailing separators and `.` components (a leading `.`
/// stays), so that its last component is the folder itself.
fn normalized(path: &Path) -> PathBuf {
    path.components().collect()
}

/// `path` refused for `why`.
fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{}: {why}", path.display()),
    )
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use log::warn;
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    use super::{normalized, refused, FileStat, LogFolder};

    /// Nothing opened is followed through a link, and opening a FIFO or a
    /// device that was put in a log's place does not wait.
    const FILE: OFlags = OFlags::NOFOLLOW
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOCTTY)
        .union(OFlags::CLOEXEC);

    impl LogFolder {
        /// The folder at `path`, made 0700 (with any missing parent) when
        /// missing. Links in the path above it are the administrator's; the
        /// folder itself may not be one.
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            let path = &normalized(path);
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let dir = match fs::open(path, flags, Mode::empty()) {
                Err(Errno::LOOP | Errno::NOTDIR) => {
                    return Err(refused(path, "not a folder (a link or a file)"))
                }
                result => result?,
            };
            let stat = fs::fstat(&dir)?;
            if stat.st_uid != rustix::process::geteuid().as_raw() {
                return Err(refused(path, "not the server's user's folder"));
            }
            if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
                return Err(refused(path, "other users may write in it"));
            }
            Ok(Self {
                path: path.to_path_buf(),
                dir,
            })
        }

        /// The log `name` for appending, made 0600 when missing.
        pub(crate) fn append(&self, name: &str) -> io::Result<File> {
            let flags = OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | FILE;
            let file = self.open_file(name, flags, Mode::RUSR | Mode::WUSR)?;
            self.tighten(name, &file)?;
            Ok(File::from(file))
        }

        /// Check the log `name` if it exists, as [`LogFolder::append`] does,
        /// making it 0600 when other users may read or write it.
        pub(crate) fn secure(&self, name: &str) -> io::Result<()> {
            match self.open_file(name, OFlags::RDONLY | FILE, Mode::empty()) {
                Ok(file) => self.tighten(name, &file),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }

        /// The log `name` for reading.
        pub(crate) fn read(&self, name: &str) -> io::Result<File> {
            let file = self.open_file(name, OFlags::RDONLY | FILE, Mode::empty())?;
            Ok(File::from(file))
        }

        /// Take other users' permissions off the log `name`, open as `file`.
        fn tighten(&self, name: &str, file: &OwnedFd) -> io::Result<()> {
            let mode = Mode::from_raw_mode(fs::fstat(file)?.st_mode);
            let others = Mode::RWXG | Mode::RWXO;
            if mode.intersects(others) {
                fs::fchmod(file, mode.difference(others))?;
                warn!(
                    "event log: other users could read or write {}: made it {:04o}",
                    self.path.join(name).display(),
                    mode.difference(others).bits() & 0o7777
                );
            }
            Ok(())
        }

        /// `name` in the folder opened with `flags`, checked
        /// ([`checked`]).
        fn open_file(&self, name: &str, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
            let path = self.path.join(name);
            let file = match fs::openat(&self.dir, name, flags, mode) {
                Err(Errno::LOOP) => return Err(refused(&path, "a link")),
                Err(Errno::NXIO) => return Err(refused(&path, "not a regular file")),
                result => result?,
            };
            checked(&path, &file)?;
            Ok(file)
        }

        /// Rename `from` to `to` in the folder, replacing `to`.
        pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
            Ok(fs::renameat(&self.dir, from, &self.dir, to)?)
        }
    }

    /// Refuse the file open as `file` (at `path`) unless it is a regular
    /// file of the server's user with no other link. An open file a
    /// rotation has unlinked since has no link left, and is the log a
    /// reader asked for.
    pub(super) fn checked(path: &Path, file: impl AsFd) -> io::Result<()> {
        let stat = fs::fstat(file)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(refused(path, "not a regular file"));
        }
        if stat.st_uid != rustix::process::geteuid().as_raw() {
            return Err(refused(path, "not the server's user's file"));
        }
        if stat.st_nlink > 1 {
            return Err(refused(path, "it has other links"));
        }
        Ok(())
    }

    impl LogFolder {
        /// The regular file `name`'s size and modification time; `None`
        /// when it is missing or something else.
        pub(crate) fn stat(&self, name: &str) -> Option<FileStat> {
            let stat = fs::statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                return None;
            }
            let modified = u64::try_from(stat.st_mtime).ok().map(|seconds| {
                let nanos = u32::try_from(stat.st_mtime_nsec).unwrap_or(0);
                SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos.min(999_999_999))
            });
            Some((u64::try_from(stat.st_size).unwrap_or(0), modified))
        }
    }
}

#[cfg(not(unix))]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::Path;

    use super::{normalized, refused, FileStat, LogFolder};

    impl LogFolder {
        /// The folder at `path`, made when missing; refused when it is a
        /// link.
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            let path = &normalized(path);
            std::fs::create_dir_all(path)?;
            if !std::fs::symlink_metadata(path)?.is_dir() {
                return Err(refused(path, "not a folder (a link or a file)"));
            }
            Ok(Self {
                path: path.to_path_buf(),
            })
        }

        /// The log `name` for appending, made when missing.
        pub(crate) fn append(&self, name: &str) -> io::Result<File> {
            let path = self.checked(name, true)?;
            OpenOptions::new().append(true).create(true).open(path)
        }

        /// The log `name` for reading.
        pub(crate) fn read(&self, name: &str) -> io::Result<File> {
            File::open(self.checked(name, false)?)
        }

        /// Check the log `name` if it exists: refused unless it is a
        /// regular file.
        pub(crate) fn secure(&self, name: &str) -> io::Result<()> {
            self.checked(name, true).map(drop)
        }

        /// The path of `name`, refused unless it is a regular file (or,
        /// with `missing`, nothing yet).
        fn checked(&self, name: &str, missing: bool) -> io::Result<std::path::PathBuf> {
            let path = self.path.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => Ok(path),
                Ok(_) => Err(refused(&path, "not a regular file (a link or a folder)")),
                Err(e) if missing && e.kind() == io::ErrorKind::NotFound => Ok(path),
                Err(e) => Err(e),
            }
        }

        /// Rename `from` to `to` in the folder, replacing `to`.
        pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
            std::fs::rename(self.path.join(from), self.path.join(to))
        }

        /// The regular file `name`'s size and modification time.
        pub(crate) fn stat(&self, name: &str) -> Option<FileStat> {
            let metadata = std::fs::symlink_metadata(self.path.join(name)).ok()?;
            metadata
                .is_file()
                .then(|| (metadata.len(), metadata.modified().ok()))
        }
    }
}

impl LogFolder {
    /// Where the folder is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    /// A planted link, a folder others may write and a log others may
    /// read: what each may not do.
    #[test]
    fn links_and_loose_permissions_are_refused_or_tightened() {
        let root = std::env::temp_dir().join(format!("om-log-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside.txt");
        std::fs::write(&outside, "kept\n").unwrap();

        // A folder that is a link to another is refused, also named with
        // a trailing separator or `.`.
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, root.join("linked")).unwrap();
        let mut slash = root.join("linked").into_os_string();
        slash.push("/");
        for linked in [
            root.join("linked"),
            slash.into(),
            root.join("linked").join("."),
        ] {
            let error = LogFolder::open(&linked).unwrap_err();
            assert!(error.to_string().contains("a link"), "{error}");
        }
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);

        // A log that is a link is neither written nor read, and the file it
        // names is left as it was.
        let folder = LogFolder::open(&real).unwrap();
        std::os::unix::fs::symlink(&outside, real.join("Event.log")).unwrap();
        std::os::unix::fs::symlink(&outside, real.join("Event-old.log")).unwrap();
        assert!(folder.append("Event.log").is_err());
        assert!(folder.read("Event-old.log").is_err());
        assert_eq!(folder.stat("Event-old.log"), None);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept\n");
        // Nor is a second name of another file.
        std::fs::hard_link(&outside, real.join("Access.log")).unwrap();
        assert!(folder.append("Access.log").is_err());

        // A new log is the server's user's only; one others may read is
        // made so, a renamed one too, and a missing one is no error.
        std::fs::remove_file(real.join("Access.log")).unwrap();
        folder.append("Access.log").unwrap();
        assert_eq!(mode(&real.join("Access.log")), 0o600);
        for name in ["Access.log", "Access-old.log"] {
            std::fs::write(real.join(name), "").unwrap();
            std::fs::set_permissions(real.join(name), std::fs::Permissions::from_mode(0o666))
                .unwrap();
        }
        folder.append("Access.log").unwrap();
        folder.secure("Access-old.log").unwrap();
        folder.secure("Missing.log").unwrap();
        assert_eq!(mode(&real.join("Access.log")), 0o600);
        assert_eq!(mode(&real.join("Access-old.log")), 0o600);

        // A reader's log that a rotation unlinked after it was opened is
        // still the log it opened.
        let mut open = folder.read("Access-old.log").unwrap();
        std::fs::remove_file(real.join("Access-old.log")).unwrap();
        sys::checked(&real.join("Access-old.log"), &open).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut open, &mut text).unwrap();

        // A folder others may write is refused; a missing one is made 0700.
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = LogFolder::open(&real).unwrap_err();
        assert!(
            error.to_string().contains("other users may write"),
            "{error}"
        );
        LogFolder::open(&root.join("made")).unwrap();
        assert_eq!(mode(&root.join("made")), 0o700);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
