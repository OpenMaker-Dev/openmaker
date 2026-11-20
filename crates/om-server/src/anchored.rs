//! A hosted file's place on disk, held through its folder.
//!
//! A hosted file is found by its path once, when it is first hosted:
//! [`FileAt::open`] resolves the path then, links included, and holds the
//! folder it ends in open. Every later read, save and reopen of the file
//! goes through that folder handle, by the file's name, without following a
//! link at the name (`openat`, `renameat` and their kin with
//! `O_NOFOLLOW`). Someone who may write in a database folder, or in a
//! folder above one, and who swaps a folder of the path for a link
//! afterwards, cannot move the file's saves or reopens somewhere else
//! ([#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)). A
//! folder that is moved away keeps the file: the handle follows the folder,
//! not its path.
//!
//! Without folder handles (Windows), each call resolves the path again, as
//! before.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

/// Where a hosted file is: its path as it was given (for messages), and on
/// Unix its folder, held open, and its name in it. Clones share the folder
/// handle.
#[derive(Clone)]
pub struct FileAt {
    path: PathBuf,
    #[cfg(unix)]
    folder: Arc<rustix::fd::OwnedFd>,
    #[cfg(unix)]
    name: OsString,
}

impl std::fmt::Debug for FileAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FileAt").field(&self.path).finish()
    }
}

impl FileAt {
    /// Resolve `path` now and hold the folder it ends in. Links in the path
    /// are followed now, the file's own included, so a file hosted through
    /// a link is saved where the link pointed when it was hosted. A path
    /// whose file does not exist yet names a file in its folder as that
    /// resolves now.
    pub fn open(path: &Path) -> io::Result<FileAt> {
        let real = match std::fs::canonicalize(path) {
            Ok(real) => real,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let name = path.file_name().ok_or_else(|| not_a_file(path))?;
                let parent = match path.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent,
                    _ => Path::new("."),
                };
                std::fs::canonicalize(parent)?.join(name)
            }
            Err(e) => return Err(e),
        };
        let name = real.file_name().ok_or_else(|| not_a_file(path))?;
        let folder = real.parent().ok_or_else(|| not_a_file(path))?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let folder = rustix::fs::open(
                folder,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(FileAt {
                path: path.to_path_buf(),
                folder: Arc::new(folder),
                name: name.to_os_string(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(FileAt {
                path: folder.join(name),
            })
        }
    }

    /// The path the file was given by, for messages.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's name in its folder.
    pub fn name(&self) -> &OsStr {
        #[cfg(unix)]
        {
            &self.name
        }
        #[cfg(not(unix))]
        {
            self.path.file_name().unwrap_or_default()
        }
    }

    /// Open entry `name` of the folder for reading, refused unless it is a
    /// regular file, and without blocking (a FIFO swapped in would hold the
    /// caller): so that a file swapped in after a check cannot be read
    /// instead.
    pub fn open_regular(&self, name: &OsStr) -> io::Result<File> {
        #[cfg(unix)]
        let file = {
            use rustix::fs::{Mode, OFlags};
            File::from(rustix::fs::openat(
                &*self.folder,
                name,
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        };
        #[cfg(not(unix))]
        let file = File::open(self.entry(name))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(file)
    }

    /// Create entry `name` of the folder for writing, only where nothing
    /// is (`O_EXCL`), with permissions `mode` less the umask on Unix.
    pub fn create_new(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            Ok(File::from(rustix::fs::openat(
                &*self.folder,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(mode as rustix::fs::RawMode),
            )?))
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.entry(name))
        }
    }

    /// The permissions of entry `name`, a link's own when it is one; `None`
    /// when there is no such entry.
    pub fn permissions(&self, name: &OsStr) -> io::Result<Option<std::fs::Permissions>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match rustix::fs::statat(&*self.folder, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => Ok(Some(std::fs::Permissions::from_mode(
                    (stat.st_mode as u32) & 0o7777,
                ))),
                Err(rustix::io::Errno::NOENT) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        #[cfg(not(unix))]
        match std::fs::metadata(self.entry(name)) {
            Ok(metadata) => Ok(Some(metadata.permissions())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether the open `file` is entry `name` of the folder, the entry not
    /// followed when it is a link. Unsupported without inode identities.
    pub fn is_entry(&self, file: &File, name: &OsStr) -> io::Result<bool> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let open = file.metadata()?;
            let named =
                rustix::fs::statat(&*self.folder, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
            Ok(open.dev() == named.st_dev as u64 && open.ino() == named.st_ino as u64)
        }
        #[cfg(not(unix))]
        {
            let _ = (file, name);
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    /// Whether entry `name` of the folder is a regular file, not followed
    /// when it is a link.
    pub fn is_regular(&self, name: &OsStr) -> bool {
        #[cfg(unix)]
        {
            rustix::fs::statat(&*self.folder, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| {
                    rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        == rustix::fs::FileType::RegularFile
                })
        }
        #[cfg(not(unix))]
        std::fs::symlink_metadata(self.entry(name)).is_ok_and(|m| m.is_file())
    }

    /// Rename entry `from` of the folder over entry `to`.
    pub fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::renameat(
                &*self.folder,
                from,
                &*self.folder,
                to,
            )?)
        }
        #[cfg(not(unix))]
        std::fs::rename(self.entry(from), self.entry(to))
    }

    /// Remove entry `name` of the folder.
    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::unlinkat(
                &*self.folder,
                name,
                rustix::fs::AtFlags::empty(),
            )?)
        }
        #[cfg(not(unix))]
        std::fs::remove_file(self.entry(name))
    }

    /// Flush the folder, so that a rename in it is on disk.
    pub fn sync_folder(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::fsync(&*self.folder)?)
        }
        #[cfg(not(unix))]
        match self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            Some(dir) => File::open(dir).and_then(|d| d.sync_all()),
            None => Ok(()),
        }
    }

    #[cfg(not(unix))]
    fn entry(&self, name: &OsStr) -> PathBuf {
        match self.path.parent() {
            Some(dir) => dir.join(name),
            None => PathBuf::from(name),
        }
    }
}

fn not_a_file(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} names no file", path.display()),
    )
}
