//! The files of script steps on OpenMaker Server: Import Records, Export
//! Records and the data file steps. As on FileMaker Server
//! (script-steps.md, "Implementation notes (import/export)", FileMaker
//! Server's folders and Paths on OpenMaker Server), a step's paths name
//! files only beneath two folders: the hosted file's Documents folder and
//! the session's temporary folder.
//!
//! - The Documents folder is `Documents` beside the `Databases` folder
//!   holding the hosted file (FileMaker Server's `Data/Documents`), or
//!   beside the file in another folder; the temporary folder is the
//!   session's own and goes when the session ends.
//!   `Get ( DocumentsPath )` and `Get ( TemporaryPath )` name them.
//! - A path list line is `file:` or `filelinux:` and a full path, or a
//!   relative one, which is in the temporary folder; lines of other
//!   platforms and schemes are skipped. A full path starts with one of the
//!   folders as those functions show it (or as the system resolves it).
//! - The rest of the path is walked beneath the folder's open handle, a
//!   name at a time and without following symbolic links (`O_NOFOLLOW`):
//!   a link anywhere below the folder is refused, dangling or not, and
//!   `..` goes back to a folder already walked, never above the folder.
//!   The file opened is the one checked; no path text is looked up again.
//! - A file read is a regular file with a single link. Another link could
//!   be a name for a file outside the folders, made by whoever can write
//!   in them, so a hard-linked file is refused like one outside. Pipes and
//!   devices are refused without being waited on.
//! - A file written is created new beside its destination (exclusively,
//!   never through a name that exists), written and then renamed over the
//!   destination's name, which must be free or a regular file. A hard
//!   link's other names keep their contents. It is not synced to disk (an
//!   export is a copy of what the hosted file keeps; a sync costs a
//!   script that exports in a loop up to a second per file).
//! - The temporary folder is made private (0700) in the system's temporary
//!   directory and used only when it is a directory of the server's own
//!   user, not a symbolic link: another local user cannot plant it. What a
//!   step writes in it is private too (files 0600, folders 0700), whatever
//!   the server's umask; it goes when the session ends, also when the
//!   session's thread panics.
//!
//! The data file steps (188-199) keep the open files of the session in
//! `om_scripts::runtime::data_files`, which every host shares (ids, limits,
//! encodings, positions and their errors); here they find their files the
//! same way, a line at a time, the first usable one winning:
//!
//! - Get File Exists is 1 for a file or folder beneath a folder (a link is
//!   not counted); Get File Size, Open Data File, Delete File and Rename
//!   File take a regular file with a single link, as an import does, and
//!   give 100 when no line names one (a folder, a link, a file outside).
//! - Open Data File opens the file for reading and writing, or for reading
//!   when it may not be written; a file is open once, by its device and
//!   inode. Writes go to the file itself, at its position.
//! - Create Data File makes the file empty as an export writes (a new file
//!   renamed over the name, so a hard link's other names keep their
//!   contents); a folder in its place is 800, a missing folder 100 unless
//!   "Create folders" is on, and a path outside the folders 800, as an
//!   export's.
//! - Rename File's new name is a name only (826 otherwise) and never
//!   replaces anything (800, `RENAME_NOREPLACE`).
//! - Delete File, Rename File and Create Data File refuse a file the
//!   session has open (300).
//! - The session's data files close when it ends: at the script's end, at
//!   an abort, and when the session is dropped after a panic.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use om_calc::Value;
use om_scripts::runtime::data_files::{self, code};
use om_scripts::runtime::{Encoding, ErrorCode};

use super::ServerHost;

/// What names an open data file: its device and inode.
pub(super) type FileKey = (u64, u64);

/// The session's open data files.
pub(super) type DataFiles = data_files::DataFiles<FileKey>;

/// The folders script paths may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Folder {
    Documents,
    Temporary,
}

/// One name of a path below its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Name(OsString),
    Parent,
}

/// A path list line: its folder, and the names below it ending with the
/// file's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Located {
    folder: Folder,
    parts: Vec<Part>,
}

/// How scripts may spell each folder's full path.
struct Spellings {
    documents: Vec<PathBuf>,
    temporary: Vec<PathBuf>,
}

/// `.` and `..` resolved on the text; `None` when `..` climbs above the
/// root. Only for showing a folder: paths are resolved by [`walk`].
fn lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

/// A folder as `Get ( DocumentsPath )` shows it: absolute, without `.`
/// and `..`.
fn shown(folder: &Path) -> PathBuf {
    let absolute = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    lexical(&absolute).unwrap_or(absolute)
}

/// Where a path list line points (module docs); `None` for a line that is
/// skipped or names no file beneath a folder.
fn locate(line: &str, spellings: &Spellings) -> Option<Located> {
    locate_path(line, spellings, false)
}

/// Exists may name a folder, including a permitted root. File-only steps
/// retain their final-name and trailing-slash restrictions.
fn locate_path(line: &str, spellings: &Spellings, folders: bool) -> Option<Located> {
    let line = line.trim();
    let (prefix, rest) = match line.split_once(':') {
        Some((prefix, rest))
            if prefix.len() > 1 && prefix.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            (prefix.to_ascii_lowercase(), rest)
        }
        _ => ("file".to_owned(), line),
    };
    if !matches!(prefix.as_str(), "file" | "filelinux")
        || rest.is_empty()
        || (!folders && rest.ends_with('/'))
    {
        return None;
    }
    let path = Path::new(rest);
    let (folder, below) = if rest.starts_with('/') {
        // The most specific folder spelling the path starts with.
        let candidates = [Folder::Documents, Folder::Temporary]
            .into_iter()
            .flat_map(|folder| {
                let list = match folder {
                    Folder::Documents => &spellings.documents,
                    Folder::Temporary => &spellings.temporary,
                };
                list.iter().map(move |spelling| (folder, spelling))
            });
        let (folder, spelling) = candidates
            .filter(|(_, spelling)| path.starts_with(spelling))
            .max_by_key(|(_, spelling)| spelling.components().count())?;
        (folder, path.strip_prefix(spelling).ok()?)
    } else {
        (Folder::Temporary, path)
    };
    let parts: Vec<Part> = below
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(Some(Part::Name(name.to_owned()))),
            Component::ParentDir => Some(Some(Part::Parent)),
            Component::CurDir => None,
            Component::RootDir | Component::Prefix(_) => Some(None),
        })
        .collect::<Option<_>>()?;
    (folders || matches!(parts.last(), Some(Part::Name(_)))).then_some(Located { folder, parts })
}

/// A path that leads outside the folders, or to what a step may not use
/// there (a link, a file with another link, a device).
#[derive(Debug)]
struct Outside;

impl std::fmt::Display for Outside {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("outside the folders")
    }
}

impl std::error::Error for Outside {}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, Outside)
}

/// Whether `e` is [`refused`]'s.
fn is_refused(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Outside>())
}

/// Walk `parts` beneath `root` (module docs): the folder holding the file
/// and the file's name. With `create`, missing folders are made.
fn walk(
    root: sys::Dir,
    parts: &[Part],
    create: bool,
    private: bool,
) -> io::Result<(sys::Dir, &OsStr)> {
    let Some((Part::Name(name), folders)) = parts.split_last() else {
        return Err(refused());
    };
    Ok((walk_folder(root, folders, create, private)?, name))
}

/// Open a folder using the same bounded, no-link walk as file parents.
/// Empty parts name the already-open permitted root.
fn walk_folder(
    root: sys::Dir,
    parts: &[Part],
    create: bool,
    private: bool,
) -> io::Result<sys::Dir> {
    let mut current = root;
    let mut above = Vec::new();
    for part in parts {
        match part {
            Part::Parent => current = above.pop().ok_or_else(refused)?,
            Part::Name(folder) => {
                let next = sys::open_folder(&current, folder, create, private)?;
                above.push(std::mem::replace(&mut current, next));
            }
        }
    }
    Ok(current)
}

/// A file a data file step names: its folder's handle, its name, and the
/// path `Get ( OpenDataFileInfo )` shows.
struct Named {
    folder: sys::Dir,
    name: OsString,
    shown: String,
    /// In the session's temporary folder: written private.
    private: bool,
}

/// A path as `Get ( OpenDataFileInfo )` shows it: `file:`, the folder as
/// `Get ( DocumentsPath )` or `Get ( TemporaryPath )` shows it, then the
/// names walked (`..` already resolved by the walk's rules).
fn shown_path(folder: &Path, parts: &[Part]) -> String {
    let mut names: Vec<&OsStr> = Vec::new();
    for part in parts {
        match part {
            Part::Name(name) => names.push(name),
            Part::Parent => {
                names.pop();
            }
        }
    }
    let mut path = shown(folder);
    path.extend(names);
    format!("file:{}", path.to_string_lossy())
}

/// Rename File's new name: one plain name, without a folder, separator or
/// `..` (826 otherwise).
fn plain_name(name: &str) -> Option<&OsStr> {
    let name = name.trim();
    let mut components = Path::new(name).components();
    let plain = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\', ':', '\0']);
    plain.then_some(OsStr::new(name))
}

/// Where an export writes: a folder's handle and a name in it.
pub(super) struct Destination {
    folder: sys::Dir,
    name: OsString,
    /// In the session's temporary folder: written private.
    private: bool,
}

impl Destination {
    /// Write `bytes` as a new file under the destination's name (module
    /// docs).
    pub(super) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        sys::replace(&self.folder, &self.name, bytes, self.private)
    }
}

impl ServerHost {
    /// FileMaker Server's Documents folder (module docs): `Documents`
    /// beside a `Databases` folder holding the file, else beside the file.
    fn documents_folder(&self) -> PathBuf {
        let dir = self.hosted.path.parent().unwrap_or(Path::new("."));
        match (dir.file_name(), dir.parent()) {
            (Some(name), Some(parent)) if name == "Databases" => parent.join("Documents"),
            _ => dir.join("Documents"),
        }
    }

    /// The session's temporary folder's name in the system's temporary
    /// directory.
    fn temporary_name(&self) -> String {
        format!("OpenMaker-S{}", self.user.connection)
    }

    /// The session's temporary folder.
    fn temporary_folder(&self) -> PathBuf {
        std::env::temp_dir().join(self.temporary_name())
    }

    /// `Get ( DocumentsPath )` (73) and `Get ( TemporaryPath )` (89): the
    /// folder as a full path with a final slash, as FileMaker Server shows
    /// them (`/opt/FileMaker/FileMaker Server/Data/Documents/`).
    pub(super) fn folder_path(&self, selector: u8) -> Value {
        let folder = if selector == 73 {
            self.documents_folder()
        } else {
            self.temporary_folder()
        };
        let mut text = shown(&folder).to_string_lossy().into_owned();
        if !text.ends_with('/') {
            text.push('/');
        }
        Value::text(text)
    }

    /// Remove the session's temporary folder (the session ends), when it is
    /// the session's own.
    pub(super) fn remove_temporary_folder(&self) {
        let _ = sys::remove_private(&std::env::temp_dir(), OsStr::new(&self.temporary_name()));
    }

    fn spellings(&self) -> Spellings {
        let documents = self.documents_folder();
        // Made first, so that the system can resolve it.
        let _ = std::fs::create_dir_all(&documents);
        let temp = std::env::temp_dir();
        let name = self.temporary_name();
        let mut spellings = Spellings {
            documents: vec![shown(&documents)],
            temporary: vec![shown(&temp.join(&name))],
        };
        spellings
            .documents
            .extend(std::fs::canonicalize(&documents).ok());
        spellings
            .temporary
            .extend(std::fs::canonicalize(&temp).ok().map(|t| t.join(&name)));
        spellings
    }

    /// A folder's handle, made when missing.
    fn open(&self, folder: Folder) -> io::Result<sys::Dir> {
        match folder {
            Folder::Documents => {
                let path = self.documents_folder();
                std::fs::create_dir_all(&path)?;
                sys::open_trusted(&path)
            }
            Folder::Temporary => {
                sys::open_private(&std::env::temp_dir(), OsStr::new(&self.temporary_name()))
            }
        }
    }

    /// The file an import reads: the first line naming one (module docs).
    pub(super) fn open_for_reading(&self, lines: &[String]) -> Option<File> {
        let spellings = self.spellings();
        lines.iter().find_map(|line| {
            let located = locate(line, &spellings)?;
            let root = self.open(located.folder).ok()?;
            let (folder, name) = walk(root, &located.parts, false, false).ok()?;
            sys::open_file(&folder, name).ok()
        })
    }

    /// Where an export writes: the first line whose folder exists (or is
    /// made, with `create_folders`) and whose name is free or a file.
    pub(super) fn destination(
        &self,
        lines: &[String],
        create_folders: bool,
    ) -> Option<Destination> {
        let spellings = self.spellings();
        lines.iter().find_map(|line| {
            let located = locate(line, &spellings)?;
            let root = self.open(located.folder).ok()?;
            let private = located.folder == Folder::Temporary;
            let (folder, name) = walk(root, &located.parts, create_folders, private).ok()?;
            sys::writable(&folder, name).ok()?;
            Some(Destination {
                name: name.to_owned(),
                folder,
                private,
            })
        })
    }
}

/// The data file steps (module docs).
impl ServerHost {
    /// The lines' files in order, walked beneath their folders as they are
    /// asked for: each line's folder handle and name, or the error that
    /// stopped it. A line that names nothing beneath a folder is left out.
    /// With `create_folders` a line's missing folders are made when it is
    /// walked.
    fn named<'a>(
        &'a self,
        lines: &'a [String],
        create_folders: bool,
    ) -> impl Iterator<Item = io::Result<Named>> + 'a {
        let spellings = self.spellings();
        lines
            .iter()
            .filter_map(move |line| locate(line, &spellings))
            .map(move |located| {
                let root = self.open(located.folder)?;
                let private = located.folder == Folder::Temporary;
                let base = match located.folder {
                    Folder::Documents => self.documents_folder(),
                    Folder::Temporary => self.temporary_folder(),
                };
                let shown = shown_path(&base, &located.parts);
                let (folder, name) = walk(root, &located.parts, create_folders, private)?;
                Ok(Named {
                    name: name.to_owned(),
                    folder,
                    shown,
                    private,
                })
            })
    }

    /// The first line naming a regular file with a single link, with its
    /// key; 100 when none does.
    fn named_file(&self, lines: &[String]) -> Result<(Named, sys::Stat), ErrorCode> {
        self.named(lines, false)
            .flatten()
            .find_map(|named| {
                let stat = sys::stat(&named.folder, &named.name).ok()?;
                (stat.kind == sys::Kind::File && stat.links == 1).then_some((named, stat))
            })
            .ok_or(code::FILE_MISSING)
    }

    /// Get File Exists (188): a file or folder beneath a folder.
    pub(super) fn data_file_exists(&self, lines: &[String]) -> bool {
        let spellings = self.spellings();
        lines.iter().any(|line| {
            let Some(located) = locate_path(line, &spellings, true) else {
                return false;
            };
            let Ok(root) = self.open(located.folder) else {
                return false;
            };
            let private = located.folder == Folder::Temporary;
            if line.trim().ends_with('/') || !matches!(located.parts.last(), Some(Part::Name(_))) {
                return walk_folder(root, &located.parts, false, private).is_ok();
            }
            let Ok((folder, name)) = walk(root, &located.parts, false, private) else {
                return false;
            };
            sys::stat(&folder, name)
                .is_ok_and(|stat| matches!(stat.kind, sys::Kind::File | sys::Kind::Folder))
        })
    }

    /// Get File Size (189).
    pub(super) fn data_file_size(&self, lines: &[String]) -> Result<i64, ErrorCode> {
        let (_, stat) = self.named_file(lines)?;
        Ok(i64::try_from(stat.size).unwrap_or(i64::MAX))
    }

    /// Create Data File (190): the first line beneath a folder, made an
    /// empty file.
    pub(super) fn create_data_file_at(
        &self,
        lines: &[String],
        create_folders: bool,
    ) -> Result<(), ErrorCode> {
        let named = match self.named(lines, create_folders).next() {
            // Outside the folders, as an export's (FileMaker Server: 800).
            None => return Err(code::CANNOT_CREATE),
            Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => return Err(code::FILE_MISSING),
            Some(Err(_)) => return Err(code::CANNOT_CREATE),
            Some(Ok(named)) => named,
        };
        if let Ok(stat) = sys::stat(&named.folder, &named.name) {
            if stat.kind != sys::Kind::File {
                return Err(code::CANNOT_CREATE);
            }
            if self.data_files.is_open(&stat.key) {
                return Err(code::IN_USE);
            }
        }
        sys::replace(&named.folder, &named.name, b"", named.private)
            .map_err(|e| data_files::io_error(&e, code::CANNOT_CREATE))
    }

    /// Open Data File (191): the first line naming a regular file with a
    /// single link; 802 when one does but cannot be opened.
    pub(super) fn open_data_file_at(&mut self, lines: &[String]) -> Result<i64, ErrorCode> {
        let mut failed = false;
        let found: Vec<Named> = self.named(lines, false).flatten().collect();
        for named in found {
            match sys::open_data_file(&named.folder, &named.name) {
                Ok((file, key)) => {
                    let name = named.name.to_string_lossy().into_owned();
                    return self.data_files.insert(key, named.shown, name, file);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound || is_refused(&e) => {}
                Err(_) => failed = true,
            }
        }
        Err(if failed {
            code::CANNOT_OPEN
        } else {
            code::FILE_MISSING
        })
    }

    /// Write to Data File (192): text in the file's formats.
    pub(super) fn write_data_file_at(
        &mut self,
        id: i64,
        data: &Value,
        encoding: Encoding,
        line_feed: bool,
    ) -> Result<(), ErrorCode> {
        let cx = self.cx();
        let text = |value: &Value| value.to_text(&cx).into_string();
        let text = text(data);
        self.data_files
            .write(id, data, |_| text, encoding, line_feed)
    }

    /// Delete File (197): a regular file with a single link that the
    /// session does not have open.
    pub(super) fn delete_data_file(&self, lines: &[String]) -> Result<(), ErrorCode> {
        let (named, stat) = self.named_file(lines)?;
        if self.data_files.is_open(&stat.key) {
            return Err(code::IN_USE);
        }
        sys::remove(&named.folder, &named.name).map_err(|e| data_files::io_error(&e, code::IN_USE))
    }

    /// Rename File (199): within its folder, never over another name.
    pub(super) fn rename_data_file(
        &self,
        lines: &[String],
        new_name: &str,
    ) -> Result<(), ErrorCode> {
        let new_name = plain_name(new_name).ok_or(code::INVALID_PATH)?;
        let (named, stat) = self.named_file(lines)?;
        if self.data_files.is_open(&stat.key) {
            return Err(code::IN_USE);
        }
        sys::rename(&named.folder, &named.name, new_name)
            .map_err(|e| data_files::io_error(&e, code::CANNOT_CREATE))
    }

    /// `Get ( OpenDataFileInfo )` (123).
    pub(super) fn data_file_info(&self) -> Value {
        Value::text(self.data_files.info())
    }
}

/// FileMaker Server's default base directory of a hosted file's externally
/// stored container data (containers.md 8.1): `RC_Data_FMS/<file name>`
/// (without the extension) beside the file.
const CONTAINER_FOLDER: &str = "RC_Data_FMS";

/// A file of externally stored container data: `parts` (folders, then the
/// file's name) beneath the container folder of the hosted file at
/// `hosted` (containers.md 8.1), as script paths are walked (module docs):
/// the hosted file's folder is the configuration's, everything below it is
/// walked a name at a time without following a symbolic link, and the file
/// must be a regular file with one link. A part that is empty, `.`, `..`
/// or holds a separator is refused: nothing a member stores may climb out
/// of the folder. `None` when there is no such file.
pub(super) fn open_container_file(hosted: &Path, parts: &[OsString]) -> Option<File> {
    let folder = hosted.parent()?;
    let stem = hosted.file_stem()?;
    let plain = |p: &OsString| {
        let text = p.to_string_lossy();
        !(text.is_empty() || text == "." || text == ".." || text.contains(['/', '\\', '\0']))
    };
    if parts.is_empty() || !parts.iter().all(plain) {
        return None;
    }
    let mut walked = vec![
        Part::Name(OsString::from(CONTAINER_FOLDER)),
        Part::Name(stem.to_owned()),
    ];
    walked.extend(parts.iter().cloned().map(Part::Name));
    let root = sys::open_trusted(folder).ok()?;
    let (dir, name) = walk(root, &walked, false, false).ok()?;
    sys::open_file(&dir, name).ok()
}

/// Folder handles and the calls on them (`openat` and its kin with
/// `O_NOFOLLOW`; Linux and macOS).
#[cfg(unix)]
mod sys {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::OwnedFd;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    pub(super) type Dir = OwnedFd;

    /// A folder below another, never through a symbolic link.
    const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// A folder the server's configuration names (symbolic links in its
    /// path are the administrator's).
    pub(super) fn open_trusted(path: &Path) -> io::Result<Dir> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        Ok(fs::open(path, flags, Mode::empty())?)
    }

    /// The session's temporary folder `name` in `parent`: made private,
    /// used when it is a folder (not a link) of this user.
    pub(super) fn open_private(parent: &Path, name: &OsStr) -> io::Result<Dir> {
        let parent = open_trusted(parent)?;
        match fs::mkdirat(&parent, name, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let folder = fs::openat(&parent, name, FOLDER, Mode::empty())?;
        if fs::fstat(&folder)?.st_uid != rustix::process::geteuid().as_raw() {
            return Err(super::refused());
        }
        fs::fchmod(&folder, Mode::RWXU)?;
        Ok(folder)
    }

    /// Remove the temporary folder `name` in `parent` when it is a folder
    /// of this user. Nobody else can replace it in between: the system's
    /// temporary directory is sticky.
    pub(super) fn remove_private(parent: &Path, name: &OsStr) -> io::Result<()> {
        let dir = open_trusted(parent)?;
        let stat = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
            || stat.st_uid != rustix::process::geteuid().as_raw()
        {
            return Err(super::refused());
        }
        std::fs::remove_dir_all(parent.join(name))
    }

    /// The folder `name` in `dir`, made first with `create` (0700 when
    /// `private`).
    pub(super) fn open_folder(
        dir: &Dir,
        name: &OsStr,
        create: bool,
        private: bool,
    ) -> io::Result<Dir> {
        match fs::openat(dir, name, FOLDER, Mode::empty()) {
            Err(Errno::NOENT) if create => {
                let mode = if private { 0o700 } else { 0o777 };
                match fs::mkdirat(dir, name, Mode::from_bits_truncate(mode)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
                Ok(fs::openat(dir, name, FOLDER, Mode::empty())?)
            }
            result => Ok(result?),
        }
    }

    /// The regular file `name` in `dir`, with one link.
    pub(super) fn open_file(dir: &Dir, name: &OsStr) -> io::Result<File> {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let file = fs::openat(dir, name, flags, Mode::empty())?;
        let stat = fs::fstat(&file)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(super::refused());
        }
        Ok(File::from(file))
    }

    /// Whether `name` in `dir` may be written: free, or a regular file.
    pub(super) fn writable(dir: &Dir, name: &OsStr) -> io::Result<()> {
        match fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => Ok(()),
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => Ok(()),
            Ok(_) => Err(super::refused()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write `bytes` as a new file named `name` in `dir`: created under a
    /// name of its own (0600 when `private`, whatever the umask), renamed
    /// over `name`.
    pub(super) fn replace(dir: &Dir, name: &OsStr, bytes: &[u8], private: bool) -> io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        for _ in 0..16 {
            let temporary = format!(
                ".openmaker-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let mode = if private { 0o600 } else { 0o666 };
            let file = match fs::openat(dir, &temporary, flags, Mode::from_bits_truncate(mode)) {
                Ok(file) => file,
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(e.into()),
            };
            let written = File::from(file)
                .write_all(bytes)
                .and_then(|()| Ok(fs::renameat(dir, &temporary, dir, name)?));
            if written.is_err() {
                let _ = fs::unlinkat(dir, &temporary, AtFlags::empty());
            }
            return written;
        }
        Err(io::ErrorKind::AlreadyExists.into())
    }

    /// What a name in a folder is, as the data file steps need it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Kind {
        File,
        Folder,
        Other,
    }

    /// A name's kind, links, size and key ([`super::FileKey`]), without
    /// following a symbolic link.
    pub(super) struct Stat {
        pub kind: Kind,
        pub links: u64,
        pub size: u64,
        pub key: super::FileKey,
    }

    /// An integer of the system's `stat` as a `u64`, whatever its width
    /// on this platform.
    fn widen<T: TryInto<u64>>(value: T) -> u64 {
        value.try_into().unwrap_or(u64::MAX)
    }

    fn of(stat: &fs::Stat) -> Stat {
        Stat {
            kind: match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => Kind::File,
                FileType::Directory => Kind::Folder,
                _ => Kind::Other,
            },
            links: widen(stat.st_nlink),
            size: widen(stat.st_size),
            key: (widen(stat.st_dev), widen(stat.st_ino)),
        }
    }

    /// `name` in `dir`, a symbolic link as itself.
    pub(super) fn stat(dir: &Dir, name: &OsStr) -> io::Result<Stat> {
        Ok(of(&fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?))
    }

    /// Open Data File: the regular file `name` in `dir` with one link, for
    /// reading and writing, or for reading when it may not be written;
    /// with its key.
    pub(super) fn open_data_file(dir: &Dir, name: &OsStr) -> io::Result<(File, super::FileKey)> {
        let flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let file = match fs::openat(dir, name, flags | OFlags::RDWR, Mode::empty()) {
            Err(Errno::ACCESS | Errno::ROFS | Errno::PERM) => {
                fs::openat(dir, name, flags | OFlags::RDONLY, Mode::empty())
            }
            other => other,
        };
        let file = match file {
            Err(Errno::LOOP) => return Err(super::refused()),
            other => other?,
        };
        let stat = of(&fs::fstat(&file)?);
        if stat.kind != Kind::File || stat.links != 1 {
            return Err(super::refused());
        }
        // Reads and writes wait for the disk as any file's do.
        let status = fs::fcntl_getfl(&file)?;
        fs::fcntl_setfl(&file, status - OFlags::NONBLOCK)?;
        Ok((File::from(file), stat.key))
    }

    /// Delete File: the name `name` in `dir`.
    pub(super) fn remove(dir: &Dir, name: &OsStr) -> io::Result<()> {
        Ok(fs::unlinkat(dir, name, AtFlags::empty())?)
    }

    /// Rename File: `from` in `dir` renamed `to` there, never over a name
    /// that exists.
    pub(super) fn rename(dir: &Dir, from: &OsStr, to: &OsStr) -> io::Result<()> {
        Ok(fs::renameat_with(
            dir,
            from,
            dir,
            to,
            fs::RenameFlags::NOREPLACE,
        )?)
    }
}

/// No folder handles on this platform: every script path is refused.
#[cfg(not(unix))]
mod sys {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub(super) enum Dir {}

    fn unsupported<T>() -> io::Result<T> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub(super) fn open_trusted(_: &Path) -> io::Result<Dir> {
        unsupported()
    }

    pub(super) fn open_private(_: &Path, _: &OsStr) -> io::Result<Dir> {
        unsupported()
    }

    pub(super) fn remove_private(_: &Path, _: &OsStr) -> io::Result<()> {
        unsupported()
    }

    pub(super) fn open_folder(dir: &Dir, _: &OsStr, _: bool, _: bool) -> io::Result<Dir> {
        match *dir {}
    }

    pub(super) fn open_file(dir: &Dir, _: &OsStr) -> io::Result<File> {
        match *dir {}
    }

    pub(super) fn writable(dir: &Dir, _: &OsStr) -> io::Result<()> {
        match *dir {}
    }

    pub(super) fn replace(dir: &Dir, _: &OsStr, _: &[u8], _: bool) -> io::Result<()> {
        match *dir {}
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Kind {
        File,
        Folder,
        Other,
    }

    pub(super) struct Stat {
        pub kind: Kind,
        pub links: u64,
        pub size: u64,
        pub key: super::FileKey,
    }

    pub(super) fn stat(dir: &Dir, _: &OsStr) -> io::Result<Stat> {
        match *dir {}
    }

    pub(super) fn open_data_file(dir: &Dir, _: &OsStr) -> io::Result<(File, super::FileKey)> {
        match *dir {}
    }

    pub(super) fn remove(dir: &Dir, _: &OsStr) -> io::Result<()> {
        match *dir {}
    }

    pub(super) fn rename(dir: &Dir, _: &OsStr, _: &OsStr) -> io::Result<()> {
        match *dir {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spellings() -> Spellings {
        Spellings {
            documents: vec![
                PathBuf::from("/srv/om/Documents"),
                PathBuf::from("/data/om/Documents"),
            ],
            temporary: vec![PathBuf::from("/tmp/OpenMaker-S1")],
        }
    }

    fn at(folder: Folder, parts: &[&str]) -> Option<Located> {
        Some(Located {
            folder,
            parts: parts
                .iter()
                .map(|p| match *p {
                    ".." => Part::Parent,
                    name => Part::Name(name.into()),
                })
                .collect(),
        })
    }

    #[test]
    fn path_lines() {
        let s = spellings();
        let docs = Folder::Documents;
        let tmp = Folder::Temporary;
        assert_eq!(locate("file:a.tab", &s), at(tmp, &["a.tab"]));
        assert_eq!(locate("a.tab", &s), at(tmp, &["a.tab"]));
        assert_eq!(
            locate("./sub/../a.tab", &s),
            at(tmp, &["sub", "..", "a.tab"])
        );
        assert_eq!(locate("../S2/a.tab", &s), at(tmp, &["..", "S2", "a.tab"]));
        assert_eq!(
            locate("filelinux:/srv/om/Documents/a.tab", &s),
            at(docs, &["a.tab"])
        );
        assert_eq!(
            locate("FILE:/data/om/Documents//x/./a.tab", &s),
            at(docs, &["x", "a.tab"])
        );
        assert_eq!(
            locate("file:/tmp/OpenMaker-S1/a.tab", &s),
            at(tmp, &["a.tab"])
        );
        assert_eq!(
            locate("/srv/om/Documents/../x.tab", &s),
            at(docs, &["..", "x.tab"])
        );
        // A drive letter is no scheme: a relative name.
        assert_eq!(locate("C:\\x.tab", &s), at(tmp, &["C:\\x.tab"]));
        for skipped in [
            "filemac:/Macintosh HD/a.tab",
            "filewin:/C:/a.tab",
            "fmnet:/host/file",
            "file:",
            "",
            "/srv/om/Documents",
            "/srv/om/Documents/",
            "/srv/om/Documents/a.tab/",
            "/srv/om/Documents/sub/..",
            "/srv/om/Documents2/a.tab",
            "/srv/om/./Documents/a.tab/..",
            "/srv/om/x/../Documents/a.tab",
            "/tmp/OpenMaker-S2/a.tab",
            "/etc/passwd",
        ] {
            assert_eq!(locate(skipped, &s), None, "{skipped}");
        }
        assert_eq!(lexical(Path::new("/a/b/../../..")), None);
        assert_eq!(
            lexical(Path::new("/a/./b/../c")),
            Some(PathBuf::from("/a/c"))
        );
    }

    /// A temporary folder planted by someone else is never used or
    /// removed through: a symbolic link (to a folder or dangling) is
    /// refused; a real folder of this user is made private.
    #[cfg(unix)]
    #[test]
    fn temporary_folders_are_private() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let parent = std::env::temp_dir().join(format!(
            "om-private-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(parent.join("target")).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // Made private when missing; tightened when this user's.
        assert!(sys::open_private(&parent, OsStr::new("S1")).is_ok());
        assert_eq!(mode(&parent.join("S1")), 0o700);
        std::fs::create_dir(parent.join("S2")).unwrap();
        std::fs::set_permissions(parent.join("S2"), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(sys::open_private(&parent, OsStr::new("S2")).is_ok());
        assert_eq!(mode(&parent.join("S2")), 0o700);

        // Planted links.
        symlink(parent.join("target"), parent.join("S3")).unwrap();
        symlink(parent.join("missing"), parent.join("S4")).unwrap();
        for name in ["S3", "S4"] {
            assert!(
                sys::open_private(&parent, OsStr::new(name)).is_err(),
                "{name}"
            );
            assert!(
                sys::remove_private(&parent, OsStr::new(name)).is_err(),
                "{name}"
            );
            assert!(parent.join(name).is_symlink());
        }
        assert!(!parent.join("missing").exists());
        std::fs::write(parent.join("target/keep"), "").unwrap();
        assert!(parent.join("target/keep").exists());

        assert!(sys::remove_private(&parent, OsStr::new("S1")).is_ok());
        assert!(!parent.join("S1").exists());
        std::fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn the_most_specific_folder_wins() {
        let s = Spellings {
            documents: vec![PathBuf::from("/srv")],
            temporary: vec![PathBuf::from("/srv/tmp/OpenMaker-S1")],
        };
        assert_eq!(
            locate("/srv/tmp/OpenMaker-S1/a", &s),
            at(Folder::Temporary, &["a"])
        );
        assert_eq!(
            locate("/srv/tmp/a", &s),
            at(Folder::Documents, &["tmp", "a"])
        );
    }
}

/// External container files beneath a hand-built folder layout
/// (containers.md 8.1): found where they are, never through a link or out
/// of the folder.
#[cfg(all(test, unix))]
mod container_folder_tests {
    use std::ffi::OsString;
    use std::io::Read;

    use super::open_container_file;

    fn parts(text: &[&str]) -> Vec<OsString> {
        text.iter().map(OsString::from).collect()
    }

    #[test]
    fn external_files_open_only_beneath_the_container_folder() {
        let root = std::env::temp_dir().join(format!("om-rc-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let hosted = root.join("Databases").join("Tasks.fmp12");
        let folder = root.join("Databases/RC_Data_FMS/Tasks");
        std::fs::create_dir_all(folder.join("Photo")).unwrap();
        std::fs::write(&hosted, b"file").unwrap();
        std::fs::write(folder.join("Photo/a.png"), b"png").unwrap();
        std::fs::write(root.join("Databases/secret"), b"secret").unwrap();
        let read = |p: &[&str]| {
            let mut out = Vec::new();
            open_container_file(&hosted, &parts(p))?
                .read_to_end(&mut out)
                .ok()?;
            Some(out)
        };
        assert_eq!(read(&["Photo", "a.png"]).as_deref(), Some(&b"png"[..]));
        // Nothing climbs out or walks a link.
        assert_eq!(read(&["..", "..", "secret"]), None);
        assert_eq!(read(&["Photo", ""]), None);
        assert_eq!(read(&["Photo/a.png"]), None);
        std::os::unix::fs::symlink(root.join("Databases"), folder.join("link")).unwrap();
        assert_eq!(read(&["link", "secret"]), None);
        std::os::unix::fs::symlink(root.join("Databases/secret"), folder.join("file")).unwrap();
        assert_eq!(read(&["file"]), None);
        // A file with a second name may be another folder's file.
        std::fs::hard_link(root.join("Databases/secret"), folder.join("Photo/hard")).unwrap();
        assert_eq!(read(&["Photo", "hard"]), None);
        // A container folder that is a link is refused too.
        std::fs::remove_dir_all(root.join("Databases/RC_Data_FMS")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere/Tasks/Photo")).unwrap();
        std::fs::write(root.join("elsewhere/Tasks/Photo/a.png"), b"png").unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("Databases/RC_Data_FMS"))
            .unwrap();
        assert_eq!(read(&["Photo", "a.png"]), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The review's P6: what an export writes in the session's temporary
/// folder is private whatever the server's umask, and the folder goes when
/// the session is dropped without closing (its thread panicked).
#[cfg(all(test, unix))]
mod temporary_tests {
    use std::os::unix::fs::PermissionsExt;

    use crate::config::Privileges;
    use crate::scripts::fixture::{self, TempFile};
    use crate::scripts::tests::{hosted, shared};

    use super::super::ServerHost;

    #[test]
    fn exports_into_the_temporary_folder_are_private_and_go_with_the_session() {
        let file = TempFile::new("temporary-modes", &[], true);
        let shared = shared(&file);
        // An id no other test's session has.
        let host = ServerHost::new(
            shared.clone(),
            hosted(&shared),
            0x7e3a_0006,
            fixture::ACCOUNT,
            Privileges::FullAccess,
        );
        let folder = std::env::temp_dir().join(host.temporary_name());
        // (A run that failed may have left it.)
        let _ = std::fs::remove_dir_all(&folder);
        let written = crate::with_umask(0o022, || {
            host.destination(&["sub/out.tab".into()], true)
                .expect("a destination")
                .write(b"x")
        });
        written.unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&folder.join("sub")), 0o700);
        assert_eq!(mode(&folder.join("sub").join("out.tab")), 0o600);
        drop(host);
        assert!(!folder.exists());
    }
}
