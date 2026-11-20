//! A hosted file's folder and file, checked each time it is read or saved.
//!
//! A hosted file is found by its path. Each read, save, backup copy and
//! reopen opens the file's folder by that path again and does its work
//! through that one handle, by the file's name (`openat`, `renameat` and
//! their kin), after a check of what the path leads to now:
//! - a save or a backup copy checks that the name still leads to the file
//!   the server last read or wrote there (its device and inode; the store
//!   keeps them, `Store::save`);
//! - a reopen, which may find a file the administrator replaced, checks
//!   that the folder is the one the file was first hosted from.
//!
//! Someone who may write in a folder above the file, and swaps a folder of
//! the path for a link to other content or to an empty folder, cannot send
//! the file's saves, backups or reopens there
//! ([#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)): they
//! fail, and a save's changes wait in memory. A folder the administrator
//! moved, with the path made to lead to it again, still holds the same file:
//! saves and backups follow it, a reopen does not. Nothing is held open
//! between uses, and nothing about the file's own name changes: a link
//! there is followed to read the file and replaced by a save, as before.
//!
//! On Linux the folder is opened for searching only (`O_PATH`), so a
//! folder the server may search but not list still works. Elsewhere it is
//! opened for reading; where that is not allowed, and on Windows, which has
//! no folder handles here, the work goes by path again after the check.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// A hosted file: the path it was given and, on Unix, the identity of the
/// folder that path ended in when the file was hosted.
#[derive(Clone, Debug)]
pub struct FileAt {
    path: PathBuf,
    #[cfg(unix)]
    folder: FileId,
}

impl FileAt {
    /// Note the folder `path` ends in, as the path resolves now.
    pub fn open(path: &Path) -> io::Result<FileAt> {
        if path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no file", path.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let folder = std::fs::metadata(folder_of(path))?;
            Ok(FileAt {
                path: path.to_path_buf(),
                folder: (folder.dev(), folder.ino()),
            })
        }
        #[cfg(not(unix))]
        Ok(FileAt {
            path: path.to_path_buf(),
        })
    }

    /// The path the file was given by.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's name in its folder.
    pub fn name(&self) -> &OsStr {
        self.path.file_name().expect("checked by FileAt::open")
    }

    /// The file's folder, opened by the path again, for a save or a backup
    /// copy: the caller checks the file in it ([`Folder::identity`]).
    pub fn folder_now(&self) -> io::Result<Folder> {
        self.open_folder(false)
    }

    /// The file's folder, opened by the path again, for a reopen: refused
    /// unless it is the folder the file was first hosted from.
    pub fn folder(&self) -> io::Result<Folder> {
        self.open_folder(true)
    }

    fn open_folder(&self, checked: bool) -> io::Result<Folder> {
        let dir = folder_of(&self.path).to_path_buf();
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            #[cfg(target_os = "linux")]
            let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
            #[cfg(not(target_os = "linux"))]
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
            let (fd, found) = match rustix::fs::open(&dir, flags, Mode::empty()) {
                Ok(fd) => {
                    let stat = rustix::fs::fstat(&fd)?;
                    (Some(fd), identity(&stat))
                }
                // Not to be listed (outside Linux): checked by path, used
                // by path.
                Err(rustix::io::Errno::ACCESS) => {
                    use std::os::unix::fs::MetadataExt;
                    let folder = std::fs::metadata(&dir)?;
                    (None, (folder.dev(), folder.ino()))
                }
                Err(e) => return Err(e.into()),
            };
            if checked && found != self.folder {
                return Err(io::Error::other(format!(
                    "{} is no longer the folder {} was hosted from (it was replaced or \
                     moved); the file is not read or written there",
                    dir.display(),
                    self.name().to_string_lossy()
                )));
            }
            Ok(Folder { dir, fd })
        }
        #[cfg(not(unix))]
        {
            let _ = checked;
            Ok(Folder { dir })
        }
    }
}

/// A hosted file's folder, opened and checked by [`FileAt::folder`].
pub struct Folder {
    dir: PathBuf,
    #[cfg(unix)]
    fd: Option<rustix::fd::OwnedFd>,
}

impl Folder {
    /// Open entry `name` for reading (a link at the name followed), refused
    /// unless it is a regular file, and without blocking (a FIFO named as a
    /// database would hold the caller): so that a file swapped in after a
    /// check cannot be read instead.
    pub fn open_regular(&self, name: &OsStr) -> io::Result<File> {
        let file = match self.handle() {
            #[cfg(unix)]
            Some(fd) => {
                use rustix::fs::{Mode, OFlags};
                File::from(rustix::fs::openat(
                    fd,
                    name,
                    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )?)
            }
            _ => {
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
                }
                options.open(self.dir.join(name))?
            }
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
    pub fn create_new(&self, name: &OsStr, owner_only: bool) -> io::Result<File> {
        #[cfg(unix)]
        let mode = {
            use rustix::fs::Mode;
            let owner = Mode::RUSR | Mode::WUSR;
            if owner_only {
                owner
            } else {
                owner | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH
            }
        };
        match self.handle() {
            #[cfg(unix)]
            Some(fd) => {
                use rustix::fs::OFlags;
                Ok(File::from(rustix::fs::openat(
                    fd,
                    name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    mode,
                )?))
            }
            _ => {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(mode_bits(mode));
                }
                #[cfg(not(unix))]
                let _ = owner_only;
                options.open(self.dir.join(name))
            }
        }
    }

    /// The permissions of the file at entry `name` (a link there followed);
    /// `None` when there is no such file.
    pub fn permissions(&self, name: &OsStr) -> io::Result<Option<std::fs::Permissions>> {
        let found = match self.handle() {
            #[cfg(unix)]
            Some(fd) => {
                use std::os::unix::fs::PermissionsExt;
                rustix::fs::statat(fd, name, rustix::fs::AtFlags::empty())
                    .map(|stat| {
                        let mode = rustix::fs::Mode::from_raw_mode(stat.st_mode);
                        std::fs::Permissions::from_mode(mode_bits(mode))
                    })
                    .map_err(io::Error::from)
            }
            _ => std::fs::metadata(self.dir.join(name)).map(|m| m.permissions()),
        };
        match found {
            Ok(permissions) => Ok(Some(permissions)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether the open `file` is entry `name`, the entry not followed when
    /// it is a link. Unsupported without inode identities (Windows).
    pub fn is_entry(&self, file: &File, name: &OsStr) -> io::Result<bool> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let open = file.metadata()?;
            let named = match self.handle() {
                Some(fd) => identity(&rustix::fs::statat(
                    fd,
                    name,
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )?),
                None => {
                    let named = std::fs::symlink_metadata(self.dir.join(name))?;
                    (named.dev(), named.ino())
                }
            };
            Ok((open.dev(), open.ino()) == named)
        }
        #[cfg(not(unix))]
        {
            let _ = (file, name);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// The device and inode of the file entry `name` leads to (a link at
    /// the name followed). Unsupported without inode identities (Windows).
    pub fn identity(&self, name: &OsStr) -> io::Result<FileId> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            match self.handle() {
                Some(fd) => Ok(identity(&rustix::fs::statat(
                    fd,
                    name,
                    rustix::fs::AtFlags::empty(),
                )?)),
                None => {
                    let found = std::fs::metadata(self.dir.join(name))?;
                    Ok((found.dev(), found.ino()))
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// Whether entry `name` is a regular file, not followed when it is a
    /// link.
    pub fn is_regular(&self, name: &OsStr) -> bool {
        match self.handle() {
            #[cfg(unix)]
            Some(fd) => rustix::fs::statat(fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| {
                    rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        == rustix::fs::FileType::RegularFile
                }),
            _ => std::fs::symlink_metadata(self.dir.join(name)).is_ok_and(|m| m.is_file()),
        }
    }

    /// Rename entry `from` over entry `to`.
    pub fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        match self.handle() {
            #[cfg(unix)]
            Some(fd) => Ok(rustix::fs::renameat(fd, from, fd, to)?),
            _ => std::fs::rename(self.dir.join(from), self.dir.join(to)),
        }
    }

    /// Remove entry `name`.
    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        match self.handle() {
            #[cfg(unix)]
            Some(fd) => Ok(rustix::fs::unlinkat(
                fd,
                name,
                rustix::fs::AtFlags::empty(),
            )?),
            _ => std::fs::remove_file(self.dir.join(name)),
        }
    }

    /// Flush the folder, so that a rename in it is on disk. A folder the
    /// server may not read cannot be flushed.
    pub fn sync(&self) -> io::Result<()> {
        match self.handle() {
            #[cfg(unix)]
            Some(fd) => {
                use rustix::fs::{Mode, OFlags};
                // A search-only handle cannot be flushed: the folder itself,
                // opened for reading through it.
                let dir = rustix::fs::openat(
                    fd,
                    ".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                Ok(rustix::fs::fsync(&dir)?)
            }
            _ => File::open(&self.dir).and_then(|d| d.sync_all()),
        }
    }

    #[cfg(unix)]
    fn handle(&self) -> Option<&rustix::fd::OwnedFd> {
        self.fd.as_ref()
    }

    #[cfg(not(unix))]
    fn handle(&self) -> Option<&()> {
        None
    }
}

/// A file's device and inode.
pub type FileId = (u64, u64);

/// The device and inode of the open `file`; `None` without inode
/// identities (Windows).
pub fn file_identity(file: &File) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        file.metadata().ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        None
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
