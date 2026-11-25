//! The file system work of backups, through open folder handles.
//!
//! **Folders.** A [`Folder`] is an open folder. Every name below it is
//! opened relative to it, never through a link: folders with `O_DIRECTORY
//! | O_NOFOLLOW`, files with `O_NOFOLLOW`. The same holds for listing,
//! renaming, linking and removing. A target folder is resolved once per
//! operation (the administrator's links followed) and then opened a name
//! at a time from the root without following any
//! ([`Folder::open_configured`]). A later change to the configured path,
//! or to a folder above it, cannot redirect what the operation reads,
//! renames or deletes.
//!
//! **Protection.** [`Protected`] holds the folders that hold hosted files
//! and their container data, found where the server reads them (links
//! followed). Each comes with the device and inode numbers of it and every
//! folder above it, read by walking `..` from the open folder. A folder
//! *holds* one of them when its own numbers are in that chain; a target
//! *lies in* container data when the chain of the target holds a container
//! folder's numbers. Numbers do not change when a path is relinked.
//!
//! **Other platforms.** On platforms without these calls, every operation
//! fails as unsupported, so backups are refused there.

use std::ffi::OsStr;
use std::fs::{File, Permissions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use log::warn;

/// How many bytes are copied between two checks for a cancel.
const CHUNK: u64 = 8 << 20;

/// The most entries a walk of one folder tree visits.
pub(super) const MAX_TREE_ENTRIES: usize = 100_000;

/// The deepest a copy of a folder tree goes.
pub(super) const MAX_TREE_DEPTH: usize = 32;

/// The deepest a removal goes: a backup's own folders, the database
/// folders it mirrors and container trees.
#[cfg(unix)]
const MAX_REMOVE_DEPTH: usize = 128;

/// How many folders up a chain of ancestors goes.
#[cfg(unix)]
const MAX_ANCESTORS: usize = 4096;

/// A file's or folder's device and inode numbers.
pub(super) type Id = (u64, u64);

/// A copy as it was made: its numbers and size, so that a later backup
/// links that copy only while it is still that file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    id: Id,
    size: u64,
}

/// How a copy did not complete.
#[derive(Debug)]
pub(super) enum CopyError {
    /// `stop` said to stop.
    Stopped,
    Io(io::Error),
}

impl From<io::Error> for CopyError {
    fn from(e: io::Error) -> Self {
        CopyError::Io(e)
    }
}

/// What a name in a folder is, a link as itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Folder,
    Other,
}

/// An open folder (module docs).
#[derive(Debug)]
pub(super) struct Folder {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

/// The folders hosted files and their container data are in, with their
/// chains of ancestors (module docs).
#[derive(Clone, Debug, Default)]
pub(super) struct Protected {
    /// For each folder that holds a hosted file, a hosted file's
    /// `RC_Data_FMS` and its `RC_Data_FMS/<file>`: its numbers, then those
    /// of each folder above it.
    chains: Vec<Vec<Id>>,
    /// The numbers of each folder that holds a hosted file.
    homes: Vec<Id>,
    /// The numbers of each `RC_Data_FMS` and `RC_Data_FMS/<file>`.
    containers: Vec<Id>,
    /// Configured files that could not be found, so that where their data
    /// lies is unknown: nothing is deleted while there are any
    /// ([`Protected::removal_refused`]).
    unresolved: Vec<PathBuf>,
}

impl Protected {
    /// Record `folder`, which holds a hosted file.
    pub fn add_home(&mut self, folder: &Folder) -> io::Result<()> {
        let chain = folder.chain()?;
        self.homes.push(chain[0]);
        self.chains.push(chain);
        Ok(())
    }

    /// Record `folder`, which is container data: no backup folder may lie
    /// in it.
    pub fn add_container(&mut self, folder: &Folder) -> io::Result<()> {
        let chain = folder.chain()?;
        self.containers.push(chain[0]);
        self.chains.push(chain);
        Ok(())
    }

    /// Record that the configured file `path` cannot be found: what of it
    /// could be opened is recorded, the rest is unknown.
    pub fn add_unresolved(&mut self, path: &Path) {
        self.unresolved.push(path.to_path_buf());
    }

    /// Why no backup may be deleted now, if anything forbids it: a
    /// configured file that cannot be found (fail closed).
    pub fn removal_refused(&self) -> Option<String> {
        self.unresolved.first().map(|path| {
            format!(
                "the configured file {} cannot be found, so where its data lies is unknown",
                path.display()
            )
        })
    }

    /// Whether `folder` holds a hosted file.
    pub fn is_home(&self, folder: &Folder) -> io::Result<bool> {
        Ok(self.homes.contains(&folder.id()?))
    }

    /// Whether hosted data lies in `folder` (or `folder` is a folder of
    /// it): `folder` may not be deleted.
    pub fn lies_in(&self, folder: &Folder) -> io::Result<bool> {
        let id = folder.id()?;
        Ok(self.chains.iter().any(|chain| chain.contains(&id)))
    }

    /// Whether `folder` lies in (or is) container data: no backup goes
    /// there.
    pub fn in_container_data(&self, folder: &Folder) -> io::Result<bool> {
        let chain = folder.chain()?;
        Ok(chain.iter().any(|id| self.containers.contains(id)))
    }
}

impl Identity {
    /// The copy an export made ([`crate::engine::export::Exported`]): its numbers
    /// and size.
    pub(super) fn exported(id: Id, size: u64) -> Identity {
        Identity { id, size }
    }
}

/// A backup exports each file into a folder of it
/// (`crate::engine::export`), by its descriptor.
impl crate::engine::export::Folder for Folder {
    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.fd)
    }
}

/// Copy `source`, `size` bytes from its start, to the new file `name` in
/// `into` with `permissions`, flushed to disk. Until it has them it is
/// owner-only, so that no one the source keeps out reads it while it is
/// written. `stop` is asked before each [`CHUNK`]; `copied` is told each
/// chunk's bytes.
pub(super) fn copy_file(
    source: &File,
    size: u64,
    into: &Folder,
    name: &OsStr,
    permissions: Permissions,
    stop: &dyn Fn() -> bool,
    copied: &mut dyn FnMut(u64),
) -> Result<Identity, CopyError> {
    let mut out = into.create_file(name, 0o600)?;
    let mut total = 0u64;
    loop {
        if stop() {
            return Err(CopyError::Stopped);
        }
        // A `Take` of a file to a file: the kernel copies it
        // (copy_file_range on Linux).
        let n = io::copy(&mut source.take(CHUNK), &mut out)?;
        if n == 0 {
            break;
        }
        total += n;
        copied(n);
    }
    if total != size {
        return Err(CopyError::Io(io::Error::other(format!(
            "{total} bytes copied of {size}: the file changed while it was copied"
        ))));
    }
    out.set_permissions(permissions)?;
    out.sync_all()?;
    Ok(Identity {
        id: file_id(&out)?,
        size: total,
    })
}

/// Make `name` in `into` a second name of the file `from_name` in `from`,
/// when that is still the copy `identity` describes. Returns whether it
/// did; when it did not (the copy is gone or was replaced, or the file
/// system has no hard links), nothing is left at `name`.
pub(super) fn link(
    from: &Folder,
    from_name: &OsStr,
    into: &Folder,
    name: &OsStr,
    identity: Identity,
) -> io::Result<bool> {
    if let Err(e) = into.link_from(from, from_name, name) {
        log::debug!("cannot link {}: {e}", name.to_string_lossy());
        return Ok(false);
    }
    // Linking does not follow a link at `from_name`: checked on the result.
    match into.stat(name)? {
        Some((Kind::File, id, size)) if (Identity { id, size }) == identity => Ok(true),
        _ => {
            into.remove_file(name)?;
            Ok(false)
        }
    }
}

/// The device and inode numbers of the open file `file`.
#[cfg(unix)]
fn file_id(file: &File) -> io::Result<Id> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id(_file: &File) -> io::Result<Id> {
    Err(unsupported())
}

/// The error of every folder operation where there are no
/// descriptor-relative calls.
#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "backups need Linux or macOS: this system cannot open folders without following links",
    )
}

/// What a tree copy shares while it walks: its bounds and its progress.
struct Walk<'a> {
    stop: &'a dyn Fn() -> bool,
    copied: &'a mut dyn FnMut(u64),
    entries: usize,
    files: usize,
}

impl Walk<'_> {
    /// Count one more entry, failing past [`MAX_TREE_ENTRIES`].
    fn count(&mut self) -> io::Result<()> {
        self.entries += 1;
        if self.entries > MAX_TREE_ENTRIES {
            return Err(io::Error::other(format!(
                "more than {MAX_TREE_ENTRIES} entries"
            )));
        }
        Ok(())
    }
}

/// Copy the regular files of the folder `parts` (plain names, opened one
/// by one without following links) below the folder `base` into the
/// folder of the same names below `dest`, with their permissions. A
/// folder that ends up without files is not kept. `None` when the folder
/// is absent; a part that is a link or not a folder, or that cannot be
/// read, is an error. Below it, links and other special entries are left
/// out and logged. At most [`MAX_TREE_DEPTH`] folders deep and
/// [`MAX_TREE_ENTRIES`] entries, or it fails. Returns how many files it
/// copied. Every folder it keeps is flushed. `base` is the
/// administrator's: links in its path are followed.
pub(super) fn copy_tree_beneath(
    base: &Path,
    parts: &[&OsStr],
    dest: &Folder,
    stop: &dyn Fn() -> bool,
    copied: &mut dyn FnMut(u64),
) -> Result<Option<usize>, CopyError> {
    let mut folder = Folder::open_following(base)?;
    for part in parts {
        folder = match folder.open(part) {
            Ok(next) => next,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(CopyError::Io(io::Error::new(
                    e.kind(),
                    format!("{}: {e}", part.to_string_lossy()),
                )))
            }
        };
    }
    let mut walk = Walk {
        stop,
        copied,
        entries: 0,
        files: 0,
    };
    copy_into(dest, parts, &folder, 0, &mut walk)?;
    Ok(Some(walk.files))
}

/// Copy the open folder `from` into the folder `names` below `into`
/// (made here, and removed again when no file is copied into it), depth
/// first, so that at most a few descriptors per level are open.
fn copy_into(
    into: &Folder,
    names: &[&OsStr],
    from: &Folder,
    depth: usize,
    walk: &mut Walk,
) -> Result<(), CopyError> {
    let Some((name, below)) = names.split_first() else {
        return copy_entries(into, from, depth, walk);
    };
    let made = into.kind(name)?.is_none();
    let folder = into.make(name)?;
    let before = walk.files;
    copy_into(&folder, below, from, depth, walk)?;
    if walk.files == before && made {
        drop(folder);
        into.remove_empty(name)?;
    } else {
        folder.sync()?;
    }
    Ok(())
}

/// Copy the entries of `from` into `into`.
fn copy_entries(
    into: &Folder,
    from: &Folder,
    depth: usize,
    walk: &mut Walk,
) -> Result<(), CopyError> {
    for (name, kind) in from.entries()? {
        walk.count()?;
        match kind {
            Kind::Folder => {
                if depth + 1 > MAX_TREE_DEPTH {
                    return Err(CopyError::Io(io::Error::other(format!(
                        "more than {MAX_TREE_DEPTH} folders deep"
                    ))));
                }
                // A folder swapped for a link since it was listed fails here.
                let below = from.open(&name)?;
                copy_into(into, &[&name], &below, depth + 1, walk)?;
            }
            Kind::File => {
                let file = from.read_file(&name)?;
                let metadata = file.metadata()?;
                copy_file(
                    &file,
                    metadata.len(),
                    into,
                    &name,
                    metadata.permissions(),
                    walk.stop,
                    walk.copied,
                )?;
                walk.files += 1;
            }
            Kind::Other => warn!(
                "{}: not backed up: a link or special file",
                name.to_string_lossy()
            ),
        }
    }
    Ok(())
}

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RawMode, RenameFlags};
    use rustix::io::Errno;

    use super::{Folder, Id, Kind, MAX_ANCESTORS, MAX_REMOVE_DEPTH, MAX_TREE_ENTRIES};

    /// A folder below another, never through a link.
    const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// A folder at a path or below another, links followed.
    const FOLLOWING: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC);

    /// A file below a folder, never through a link, not waiting on a FIFO.
    const READ: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOCTTY)
        .union(OFlags::CLOEXEC);

    /// A new file below a folder, where nothing is (a link not followed).
    const CREATE: OFlags = OFlags::WRONLY
        .union(OFlags::CREATE)
        .union(OFlags::EXCL)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    fn kind(stat: &fs::Stat) -> Kind {
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Folder,
            _ => Kind::Other,
        }
    }

    /// An integer of `stat` as a `u64`, whatever its width here.
    fn widen<T: TryInto<u64>>(value: T) -> u64 {
        value.try_into().unwrap_or(u64::MAX)
    }

    fn id(stat: &fs::Stat) -> Id {
        (widen(stat.st_dev), widen(stat.st_ino))
    }

    /// `name` with the error it had.
    fn named(name: &OsStr) -> impl Fn(Errno) -> io::Error + '_ {
        move |e| {
            let e = io::Error::from(e);
            io::Error::new(e.kind(), format!("{}: {e}", name.to_string_lossy()))
        }
    }

    impl Folder {
        /// The folder the administrator configured at `path`: resolved now,
        /// its links followed, then opened a name at a time from the root
        /// without following any, so that the folder opened is the one
        /// resolved (a link put in its way since fails the open).
        pub fn open_configured(path: &Path) -> io::Result<Folder> {
            let real = std::fs::canonicalize(path)?;
            let mut folder = Folder {
                fd: fs::open("/", FOLLOWING, Mode::empty())?,
            };
            for part in real.components() {
                match part {
                    Component::RootDir => {}
                    Component::Normal(name) => folder = folder.open(name)?,
                    _ => return Err(io::Error::other("not a plain path")),
                }
            }
            Ok(folder)
        }

        /// The folder at `path`, links followed: where the server reads a
        /// hosted file's folder.
        pub fn open_following(path: &Path) -> io::Result<Folder> {
            Ok(Folder {
                fd: fs::open(path, FOLLOWING, Mode::empty())?,
            })
        }

        /// The folder `name` below this one, not through a link.
        pub fn open(&self, name: &OsStr) -> io::Result<Folder> {
            Ok(Folder {
                fd: fs::openat(&self.fd, name, FOLDER, Mode::empty()).map_err(named(name))?,
            })
        }

        /// The folder `name` below this one, a link followed: where the
        /// server reads container data from when it follows links.
        pub fn open_following_at(&self, name: &OsStr) -> io::Result<Folder> {
            Ok(Folder {
                fd: fs::openat(&self.fd, name, FOLLOWING, Mode::empty()).map_err(named(name))?,
            })
        }

        /// The folder `name` below this one, made if it is missing.
        pub fn make(&self, name: &OsStr) -> io::Result<Folder> {
            match fs::mkdirat(&self.fd, name, Mode::from_bits_truncate(0o777)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(named(name)(e)),
            }
            self.open(name)
        }

        /// The new folder `name` below this one, where nothing may be.
        pub fn make_new(&self, name: &OsStr) -> io::Result<Folder> {
            fs::mkdirat(&self.fd, name, Mode::from_bits_truncate(0o777)).map_err(named(name))?;
            self.open(name)
        }

        /// What `name` is, a link as itself; `None` when nothing is there.
        pub fn kind(&self, name: &OsStr) -> io::Result<Option<Kind>> {
            Ok(self.stat(name)?.map(|(kind, _, _)| kind))
        }

        /// What `name` is, its numbers and size, a link as itself.
        pub fn stat(&self, name: &OsStr) -> io::Result<Option<(Kind, Id, u64)>> {
            match fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => Ok(Some((kind(&stat), id(&stat), widen(stat.st_size)))),
                Err(Errno::NOENT) => Ok(None),
                Err(e) => Err(named(name)(e)),
            }
        }

        /// The entries of this folder and what each is, at most
        /// [`MAX_TREE_ENTRIES`] (more is an error).
        pub fn entries(&self) -> io::Result<Vec<(OsString, Kind)>> {
            let mut entries = Vec::new();
            for entry in Dir::read_from(&self.fd)? {
                let entry = entry?;
                let name = OsStr::from_bytes(entry.file_name().to_bytes());
                if name == "." || name == ".." {
                    continue;
                }
                if entries.len() == MAX_TREE_ENTRIES {
                    return Err(io::Error::other(format!(
                        "more than {MAX_TREE_ENTRIES} entries"
                    )));
                }
                // Gone since it was listed: left out.
                if let Some(kind) = self.kind(name)? {
                    entries.push((name.to_os_string(), kind));
                }
            }
            Ok(entries)
        }

        /// The regular file `name` here, for reading, not through a link.
        pub fn read_file(&self, name: &OsStr) -> io::Result<File> {
            let fd = fs::openat(&self.fd, name, READ, Mode::empty()).map_err(named(name))?;
            if FileType::from_raw_mode(fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
                return Err(io::Error::other(format!(
                    "{}: not a regular file",
                    name.to_string_lossy()
                )));
            }
            Ok(File::from(fd))
        }

        /// The new file `name` here, with `mode` (less the umask). `RawMode`
        /// is `mode_t`: 32 bits on Linux, 16 on macOS.
        pub fn create_file(&self, name: &OsStr, mode: RawMode) -> io::Result<File> {
            let fd = fs::openat(&self.fd, name, CREATE, Mode::from_bits_truncate(mode))
                .map_err(named(name))?;
            Ok(File::from(fd))
        }

        /// Remove the file (or link) `name` here.
        pub fn remove_file(&self, name: &OsStr) -> io::Result<()> {
            fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(named(name))
        }

        /// Remove the empty folder `name` here.
        pub fn remove_empty(&self, name: &OsStr) -> io::Result<()> {
            fs::unlinkat(&self.fd, name, AtFlags::REMOVEDIR).map_err(named(name))
        }

        /// Rename `from` here to `to` here, never over a name that is
        /// there. Where the file system cannot refuse that itself, the
        /// name is checked first (only this server makes these names).
        pub fn rename_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
            match fs::renameat_with(&self.fd, from, &self.fd, to, RenameFlags::NOREPLACE) {
                Ok(()) => Ok(()),
                Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
                    if self.stat(to)?.is_some() {
                        return Err(io::ErrorKind::AlreadyExists.into());
                    }
                    fs::renameat(&self.fd, from, &self.fd, to).map_err(named(to))
                }
                Err(e) => Err(named(to)(e)),
            }
        }

        /// Rename `from` here over `to` here.
        pub fn replace(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
            fs::renameat(&self.fd, from, &self.fd, to).map_err(named(to))
        }

        /// Make `name` here a second name of `from_name` in `from`, a link
        /// there linked as itself.
        pub fn link_from(&self, from: &Folder, from_name: &OsStr, name: &OsStr) -> io::Result<()> {
            fs::linkat(&from.fd, from_name, &self.fd, name, AtFlags::empty()).map_err(named(name))
        }

        /// Flush this folder's entries to disk.
        pub fn sync(&self) -> io::Result<()> {
            Ok(fs::fsync(&self.fd)?)
        }

        /// This folder's device and inode numbers.
        pub fn id(&self) -> io::Result<Id> {
            Ok(id(&fs::fstat(&self.fd)?))
        }

        /// This folder's numbers, then those of each folder above it, up
        /// to the root, read by walking `..`.
        pub fn chain(&self) -> io::Result<Vec<Id>> {
            let mut chain = vec![self.id()?];
            let mut current = Folder {
                fd: fs::openat(&self.fd, "..", FOLLOWING, Mode::empty())?,
            };
            while chain.len() < MAX_ANCESTORS {
                let id = current.id()?;
                if chain.last() == Some(&id) {
                    return Ok(chain);
                }
                chain.push(id);
                current = Folder {
                    fd: fs::openat(&current.fd, "..", FOLLOWING, Mode::empty())?,
                };
            }
            Err(io::Error::other("too many folders above"))
        }

        /// Remove the folder `name` here, which `folder` is open on: its
        /// contents through `folder` (never through a link), then the
        /// folder itself, which fails unless it is empty (a folder put in
        /// its place since is not removed).
        pub fn remove(&self, name: &OsStr, folder: Folder) -> io::Result<()> {
            self.remove_at(name, folder, 0)
        }

        fn remove_at(&self, name: &OsStr, folder: Folder, depth: usize) -> io::Result<()> {
            if depth > MAX_REMOVE_DEPTH {
                return Err(io::Error::other(format!(
                    "more than {MAX_REMOVE_DEPTH} folders deep"
                )));
            }
            for (entry, kind) in folder.entries()? {
                match kind {
                    // A folder swapped for a link since it was listed
                    // fails to open: nothing is followed.
                    Kind::Folder => folder.remove_at(&entry, folder.open(&entry)?, depth + 1)?,
                    Kind::File | Kind::Other => folder.remove_file(&entry)?,
                }
            }
            drop(folder);
            self.remove_empty(name)
        }

        /// The bytes of the regular files in this folder tree, links not
        /// followed, within the walk's bounds.
        pub fn size(&self) -> u64 {
            fn walk(folder: &Folder, depth: usize, left: &mut usize) -> u64 {
                let Ok(entries) = folder.entries() else {
                    return 0;
                };
                let mut size = 0u64;
                for (name, kind) in entries {
                    if *left == 0 {
                        break;
                    }
                    *left -= 1;
                    match kind {
                        Kind::File => {
                            if let Ok(Some((_, _, bytes))) = folder.stat(&name) {
                                size = size.saturating_add(bytes);
                            }
                        }
                        Kind::Folder if depth < MAX_REMOVE_DEPTH => {
                            if let Ok(below) = folder.open(&name) {
                                size = size.saturating_add(walk(&below, depth + 1, left));
                            }
                        }
                        _ => {}
                    }
                }
                size
            }
            let mut left = MAX_TREE_ENTRIES;
            walk(self, 0, &mut left)
        }

        /// A second handle on this folder.
        pub fn duplicate(&self) -> io::Result<Folder> {
            Ok(Folder {
                fd: self.fd.try_clone()?,
            })
        }
    }
}

#[cfg(not(unix))]
mod other {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use super::{unsupported, Folder, Id, Kind};

    impl Folder {
        pub fn open_configured(_path: &Path) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn open_following(_path: &Path) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn open(&self, _name: &OsStr) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn open_following_at(&self, _name: &OsStr) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn make(&self, _name: &OsStr) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn make_new(&self, _name: &OsStr) -> io::Result<Folder> {
            Err(unsupported())
        }
        pub fn kind(&self, _name: &OsStr) -> io::Result<Option<Kind>> {
            Err(unsupported())
        }
        pub fn stat(&self, _name: &OsStr) -> io::Result<Option<(Kind, Id, u64)>> {
            Err(unsupported())
        }
        pub fn entries(&self) -> io::Result<Vec<(OsString, Kind)>> {
            Err(unsupported())
        }
        pub fn read_file(&self, _name: &OsStr) -> io::Result<File> {
            Err(unsupported())
        }
        pub fn create_file(&self, _name: &OsStr, _mode: u32) -> io::Result<File> {
            Err(unsupported())
        }
        pub fn remove_file(&self, _name: &OsStr) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn remove_empty(&self, _name: &OsStr) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn rename_new(&self, _from: &OsStr, _to: &OsStr) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn replace(&self, _from: &OsStr, _to: &OsStr) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn link_from(
            &self,
            _from: &Folder,
            _from_name: &OsStr,
            _name: &OsStr,
        ) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn sync(&self) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn id(&self) -> io::Result<Id> {
            Err(unsupported())
        }
        pub fn chain(&self) -> io::Result<Vec<Id>> {
            Err(unsupported())
        }
        pub fn remove(&self, _name: &OsStr, _folder: Folder) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn size(&self) -> u64 {
            0
        }
        pub fn duplicate(&self) -> io::Result<Folder> {
            Err(unsupported())
        }
    }
}
