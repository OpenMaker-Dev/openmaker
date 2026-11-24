//! A hosted file's folder and file, checked each time it is read or saved.
//!
//! A hosted file is found by its path. Each read, save, backup copy and
//! reopen opens the file's folder by that path again and does its work
//! through that one handle, by the file's name, never following a link at
//! the name (`openat` with `O_NOFOLLOW`, `renameat` and their kin), after a
//! check of what the path leads to now:
//! - a save or a backup copy checks that the name is the file the server
//!   read or last saved there (its device and inode; the store keeps them,
//!   `Store::save`), and takes the file's permissions from that same look;
//! - a reopen, which may find a file the administrator replaced, checks
//!   that the folder is the one the file was first hosted from.
//!
//! Someone who may write in a folder above the file, and swaps a folder of
//! the path for a link to other content, to an empty folder, or to a folder
//! whose entry links back to the file, cannot send the file's saves,
//! backups or reopens there
//! ([#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)): they
//! fail, and a save's changes wait in memory. A folder the administrator
//! moved, with the path made to lead to it again, still holds the same
//! file: saves and backups follow it, a reopen does not. Nothing is held
//! open between uses.
//!
//! A path whose last part is a link (a file named on the command line may
//! be one) is resolved once, when the file is hosted on Unix: the file is
//! then worked on where the link led, and the link is left as it is. A
//! file the desktop opened keeps its already resolved path. On Windows a
//! hosted file named by a link is refused before it can be saved.
//!
//! On Linux the folder is opened for searching only (`O_PATH`), so a
//! folder the server may search but not list works. Elsewhere on Unix it
//! is opened for reading, and a folder the server may not read cannot hold
//! a hosted file. Windows, which has no folder handles or inode identities
//! here, goes by the path.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// A file's device and inode.
pub type FileId = (u64, u64);

/// A hosted file: the path it was given, the path it is worked on by (the
/// same, unless its last part was a link when it was hosted), and on Unix
/// the identity of the folder that path ended in then.
#[derive(Clone, Debug)]
pub struct FileAt {
    path: PathBuf,
    place: PathBuf,
    #[cfg(unix)]
    folder: FileId,
}

impl FileAt {
    /// Note where `path` leads now: on Unix a link as its last part is
    /// resolved once, here, and the folder the file is in is noted.
    pub fn open(path: &Path) -> io::Result<FileAt> {
        FileAt::new(path, cfg!(unix))
    }

    /// [`FileAt::open`] without resolving a link as `path`'s last part: for
    /// a file the desktop opened and locked by that path, whose lock check
    /// keeps to the file it opened.
    pub fn open_as_named(path: &Path) -> io::Result<FileAt> {
        FileAt::new(path, false)
    }

    fn new(path: &Path, resolve: bool) -> io::Result<FileAt> {
        let named = |p: &Path| {
            p.file_name().is_some().then_some(()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} names no file", path.display()),
                )
            })
        };
        named(path)?;
        let place = match std::fs::symlink_metadata(path) {
            Ok(m) if resolve && m.file_type().is_symlink() => std::fs::canonicalize(path)?,
            _ => path.to_path_buf(),
        };
        named(&place)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let folder = std::fs::metadata(folder_of(&place))?;
            Ok(FileAt {
                path: path.to_path_buf(),
                place,
                folder: (folder.dev(), folder.ino()),
            })
        }
        #[cfg(not(unix))]
        Ok(FileAt {
            path: path.to_path_buf(),
            place,
        })
    }

    /// The path the file was given by, for messages.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path the file is read and saved by: [`FileAt::path`], or where
    /// a link as its last part led when the file was hosted.
    pub fn place(&self) -> &Path {
        &self.place
    }

    /// The file's name in its folder.
    pub fn name(&self) -> &OsStr {
        self.place.file_name().expect("checked by FileAt::open")
    }

    /// The file's folder, opened by the path again, for a save or a backup
    /// copy: the caller checks the file in it ([`Folder::entry`]).
    pub fn folder_now(&self) -> io::Result<Folder> {
        self.open_folder(false)
    }

    /// The file's folder, opened by the path again, for a reopen: refused
    /// unless it is the folder the file was first hosted from.
    pub fn folder(&self) -> io::Result<Folder> {
        self.open_folder(true)
    }

    fn open_folder(&self, checked: bool) -> io::Result<Folder> {
        let dir = folder_of(&self.place).to_path_buf();
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            #[cfg(target_os = "linux")]
            let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
            #[cfg(not(target_os = "linux"))]
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
            let fd = rustix::fs::open(&dir, flags, Mode::empty())?;
            if checked && identity(&rustix::fs::fstat(&fd)?) != self.folder {
                return Err(io::Error::other(format!(
                    "{} is no longer the folder {} was hosted from (it was replaced or \
                     moved); the file is not read there",
                    dir.display(),
                    self.name().to_string_lossy()
                )));
            }
            Ok(Folder { fd })
        }
        #[cfg(not(unix))]
        {
            let _ = checked;
            Ok(Folder { dir })
        }
    }
}

/// What a folder's entry is, from one look that does not follow a link.
pub struct Entry {
    /// Its device and inode (`None` on Windows).
    pub id: Option<FileId>,
    /// Whether it is a regular file (not a link, folder or other).
    pub regular: bool,
    /// How many names (hard links) it has; 1 on Windows, which this does
    /// not read.
    pub links: u64,
    pub permissions: std::fs::Permissions,
}

/// A hosted file's folder, opened by [`FileAt::folder`] or
/// [`FileAt::folder_now`].
pub struct Folder {
    #[cfg(not(unix))]
    dir: PathBuf,
    #[cfg(unix)]
    fd: rustix::fd::OwnedFd,
}

impl Folder {
    /// Open entry `name` for reading, never through a link, refused unless
    /// it is a regular file, and without blocking (a FIFO named as a
    /// database would hold the caller): so that a file swapped in after a
    /// check cannot be read instead.
    pub fn open_regular(&self, name: &OsStr) -> io::Result<File> {
        #[cfg(unix)]
        let file = {
            use rustix::fs::{Mode, OFlags};
            File::from(rustix::fs::openat(
                &self.fd,
                name,
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        };
        #[cfg(not(unix))]
        let file = {
            let path = self.dir.join(name);
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "{} is a symbolic link; name the regular database file directly",
                        path.display()
                    ),
                ));
            }
            File::open(path)?
        };
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(file)
    }

    /// Create entry `name` for writing, only where nothing is: owner-only
    /// (0600) when `owner_only`, else a new file's permissions (0666), less
    /// the umask on Unix.
    // A local file lock can move to this descriptor. Copy reads need
    // the same saved file the lock names.
    pub fn create_new(&self, name: &OsStr, owner_only: bool) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let owner = Mode::RUSR | Mode::WUSR;
            let mode = if owner_only {
                owner
            } else {
                owner | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH
            };
            Ok(File::from(rustix::fs::openat(
                &self.fd,
                name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                mode,
            )?))
        }
        #[cfg(not(unix))]
        {
            let _ = owner_only;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(self.dir.join(name))
        }
    }

    /// Entry `name` as one look sees it, a link not followed; `None` when
    /// there is no such entry.
    pub fn entry(&self, name: &OsStr) -> io::Result<Option<Entry>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    let mode = rustix::fs::Mode::from_raw_mode(stat.st_mode);
                    Ok(Some(Entry {
                        id: Some(identity(&stat)),
                        regular: rustix::fs::FileType::from_raw_mode(stat.st_mode)
                            == rustix::fs::FileType::RegularFile,
                        #[allow(clippy::useless_conversion)]
                        links: u64::from(stat.st_nlink),
                        permissions: std::fs::Permissions::from_mode(mode_bits(mode)),
                    }))
                }
                Err(rustix::io::Errno::NOENT) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        #[cfg(not(unix))]
        match std::fs::symlink_metadata(self.dir.join(name)) {
            Ok(m) => Ok(Some(Entry {
                id: None,
                regular: m.is_file(),
                links: 1,
                permissions: m.permissions(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether the open `file` is entry `name`, the entry not followed when
    /// it is a link. Unsupported without inode identities (Windows).
    pub fn is_entry(&self, file: &File, name: &OsStr) -> io::Result<bool> {
        match (file_identity(file)?, self.entry(name)?) {
            (Some(open), Some(entry)) => Ok(entry.id == Some(open)),
            (Some(_), None) => Ok(false),
            (None, _) => Err(io::ErrorKind::Unsupported.into()),
        }
    }

    /// Whether entry `name` is a regular file, not followed when it is a
    /// link.
    #[cfg(not(unix))]
    pub fn is_regular(&self, name: &OsStr) -> bool {
        self.entry(name).is_ok_and(|e| e.is_some_and(|e| e.regular))
    }

    /// Rename entry `from` over entry `to`.
    pub fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::renameat(&self.fd, from, &self.fd, to)?)
        }
        #[cfg(not(unix))]
        std::fs::rename(self.dir.join(from), self.dir.join(to))
    }

    /// Remove entry `name`.
    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::unlinkat(
                &self.fd,
                name,
                rustix::fs::AtFlags::empty(),
            )?)
        }
        #[cfg(not(unix))]
        std::fs::remove_file(self.dir.join(name))
    }

    /// Flush the folder, so that a rename in it is on disk. A folder the
    /// server may not read cannot be flushed.
    pub fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            // A search-only handle cannot be flushed: the folder itself,
            // opened for reading through it.
            let dir = rustix::fs::openat(
                &self.fd,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(rustix::fs::fsync(&dir)?)
        }
        #[cfg(not(unix))]
        File::open(&self.dir).and_then(|d| d.sync_all())
    }
}

/// The device and inode of the open `file`: `None` without inode
/// identities (Windows), an error when they cannot be read (checks fail
/// closed).
pub fn file_identity(file: &File) -> io::Result<Option<FileId>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata()?;
        Ok(Some((m.dev(), m.ino())))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(None)
    }
}

/// The folder `path` names its file in: `.` for a bare name.
fn folder_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// A mode's permission bits as `std` takes them.
#[cfg(unix)]
#[allow(clippy::useless_conversion)]
fn mode_bits(mode: rustix::fs::Mode) -> u32 {
    // `RawMode` is narrower than 32 bits on some systems (macOS).
    mode.bits().into()
}

/// A file's device and inode, as `std`'s `MetadataExt` gives them.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
fn identity(stat: &rustix::fs::Stat) -> FileId {
    // `st_dev` is narrower than 64 bits on some systems (macOS).
    (stat.st_dev as u64, stat.st_ino as u64)
}
