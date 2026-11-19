//! Local files for scripts: FileMaker's path lists and the data file steps
//! (Create, Open, Read from, Write to and Close Data File, Get and Set Data
//! File Position, Get File Exists, Get File Size, Delete File, Rename File)
//! with `Get ( OpenDataFileInfo )`, `Get ( DocumentsPath )`,
//! `Get ( DesktopPath )` and `Get ( TemporaryPath )` (Claris script steps
//! reference, "Creating file paths"), and the plug-in subset's desktop
//! folder, folder creation, file deletion, whole-file reads and writes and
//! path conversion (`Folders.UserDesktop`, `Files.CreateDirectory`,
//! `Files.Delete`, the DynaPDF documents, `Path.FileMakerPathToNativePath`;
//! docs/plugins.md), which take native paths.
//!
//! Paths. A path list has one path per line, tried in order. A path is
//! `file:`, `filemac:`, `filewin:` or `filelinux:` (or no prefix, which is
//! `file:`) and then a FileMaker path; a platform prefix of another
//! platform is skipped, as are other schemes (`fmnet:`, `image:`, URLs).
//! Full paths start with `/`: on macOS the first part is the volume
//! (`/Macintosh HD/Users/...`, the startup disk, or a disk mounted under
//! `/Volumes`), on Windows a drive (`/C:/...`) or a share
//! (`//computer/share/...`), on Linux the root. Relative paths are relative
//! to the user's Documents folder, where FileMaker puts them for a hosted
//! database.
//!
//! Policy: the steps can reach what the user's own OS permissions allow, as
//! in FileMaker Pro, and nothing a path does not name plainly:
//!
//! - `.` and `..` are resolved on the text before the path is used; a
//!   relative path whose `..` climbs out of the Documents folder, a part
//!   with a NUL or (on Windows) a `:` outside the drive, and an unknown
//!   volume are error 826 "File path specified is not a valid file path";
//! - Delete File and Rename File act on files, not folders (100), and not
//!   on a data file a script has open (300); Rename File's new name is a
//!   name only, without a folder, separator or `..` (826), and never
//!   replaces an existing file (800);
//! - Create Data File replaces a file, never a folder (800), and creates
//!   missing folders only with "Create folders";
//! - a folder is created only at an absolute native path, its `.` and `..`
//!   resolved as above (826), never over a file (800);
//! - a whole file is read or written only at such a path; a write goes to
//!   a folder that exists (100), replaces a file atomically, never a folder
//!   (800) nor a data file a script has open (300);
//! - a file is deleted at such a path only if it exists (100), is no folder
//!   (800) and is no data file a script has open (300).
//! - the open files, their ids, limits (25 open, 64 MB a read or write),
//!   encodings, positions and errors are every host's
//!   (`om_scripts::runtime::data_files`); this module finds the files.
//!
//! Data files stay open until Close Data File or until the database file
//! closes (FileMaker: until the application quits).

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use om_data::calc::Value;
use om_data::runtime::data_files::{self, code as shared};
use om_data::runtime::{Encoding, ErrorCode};

/// FileMaker errors of the file steps (catalogs/error-codes.csv), as
/// `om_scripts::runtime::data_files::code` defines them.
pub(crate) mod code {
    use super::shared;

    pub const FILE_MISSING: i32 = shared::FILE_MISSING.0;
    pub const IN_USE: i32 = shared::IN_USE.0;
    pub const CANNOT_CREATE: i32 = shared::CANNOT_CREATE.0;
    pub const CANNOT_OPEN: i32 = shared::CANNOT_OPEN.0;
    pub const INVALID_PATH: i32 = shared::INVALID_PATH.0;
}

fn error(code: i32) -> ErrorCode {
    ErrorCode(code)
}

/// The platform a path prefix names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Mac,
    Windows,
    Linux,
}

const THIS: Platform = if cfg!(target_os = "macos") {
    Platform::Mac
} else if cfg!(windows) {
    Platform::Windows
} else {
    Platform::Linux
};

/// The folders the Get path selectors name.
#[derive(Clone, Debug)]
pub(crate) struct Folders {
    pub documents: PathBuf,
    pub desktop: PathBuf,
    pub temporary: PathBuf,
}

impl Folders {
    /// This user's folders; the temporary folder is this process's own,
    /// as FileMaker keeps one per session.
    pub(crate) fn of_this_user() -> Folders {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Folders {
            documents: home.join("Documents"),
            desktop: home.join("Desktop"),
            temporary: std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id())),
        }
    }
}

/// The startup disk's name on macOS: the volume under `/Volumes` that links
/// to `/`.
fn boot_volume() -> String {
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            if std::fs::read_link(entry.path()).is_ok_and(|t| t == Path::new("/")) {
                return entry.file_name().to_string_lossy().into_owned();
            }
        }
    }
    "Macintosh HD".into()
}

/// A folder or file as a FileMaker path (`/Macintosh HD/Users/...`,
/// `/C:/Users/...`, `/home/...`); folders end with `/`.
pub(crate) fn filemaker_path(path: &Path, folder: bool) -> String {
    let mut text = match THIS {
        Platform::Windows => {
            let raw = path.to_string_lossy().replace('\\', "/");
            match raw.strip_prefix("//") {
                Some(unc) => format!("//{unc}"),
                None => format!("/{raw}"),
            }
        }
        Platform::Mac => {
            let raw = path.to_string_lossy().into_owned();
            match raw.strip_prefix("/Volumes/") {
                Some(rest) => format!("/{rest}"),
                None => format!("/{}{raw}", boot_volume()),
            }
        }
        Platform::Linux => path.to_string_lossy().into_owned(),
    };
    if folder && !text.ends_with('/') {
        text.push('/');
    }
    text
}

/// A native file path as a `file:` path list line; a relative one is taken
/// from the current directory. A native path is not one as it is: on macOS
/// FileMaker's absolute paths start with the volume's name (`/tmp/a` is
/// volume `tmp`, error 826), on Windows with the drive.
pub(crate) fn file_line(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    format!("file:{}", filemaker_path(&path, false))
}

/// What the user typed into an in-app file dialog, as a path list line: a
/// native absolute path in FileMaker's form, anything else as typed (a
/// path relative to the Documents folder, or a FileMaker path).
pub(crate) fn chosen_path(text: &str) -> String {
    let path = text.trim();
    if Path::new(path).is_absolute() {
        file_line(Path::new(path))
    } else {
        path.to_owned()
    }
}

/// A folder as a FileMaker path, empty when it does not exist (FileMaker
/// Pro's `DesktopPath`, `DocumentsPath` and `PreferencesPath` in a home
/// without them, calc-functions.md 6.3 O1).
pub(crate) fn existing_folder(path: &Path) -> String {
    if path.is_dir() {
        filemaker_path(path, true)
    } else {
        String::new()
    }
}

/// `Get ( FileMakerPath )`: the folder holding the application.
pub(crate) fn application_folder() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| filemaker_path(p, true)))
        .unwrap_or_default()
}

/// `Get ( PreferencesPath )`: the user's preferences folder (macOS
/// `Library/Preferences`, Windows `AppData/Local`, the XDG configuration
/// folder elsewhere).
pub(crate) fn preferences_folder() -> String {
    let home = |rest: &str| {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|h| PathBuf::from(h).join(rest))
    };
    let folder = match THIS {
        Platform::Mac => home("Library/Preferences"),
        Platform::Windows => std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        Platform::Linux => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home(".config")),
    };
    folder.map(|f| existing_folder(&f)).unwrap_or_default()
}

/// `Get ( SystemDrive )`: the volume the system runs from (`/Macintosh
/// HD/`, `/C:/`; `/` on Linux).
pub(crate) fn system_drive() -> String {
    match THIS {
        Platform::Mac => format!("/{}/", boot_volume()),
        Platform::Windows => {
            let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
            format!("/{drive}/")
        }
        Platform::Linux => "/".into(),
    }
}

/// The most entries `Get ( DocumentsPathListing )` gives (OpenMaker's
/// bound on a large folder).
const LISTING_LIMIT: usize = 10_000;

/// `Get ( DocumentsPathListing )`: every file and folder under `folder`,
/// as FileMaker paths (folders end with `/`), one per line, names in
/// order; empty when the folder does not exist.
pub(crate) fn listing(folder: &Path) -> String {
    fn walk(folder: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= LISTING_LIMIT {
                return;
            }
            let path = entry.path();
            // Symbolic links are listed, not followed.
            let folder = entry.file_type().is_ok_and(|t| t.is_dir());
            out.push(filemaker_path(&path, folder));
            if folder {
                walk(&path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(folder, &mut out);
    out.join("\r")
}

/// The parts of a path after `.` and `..` are resolved; `None` when `..`
/// climbs above the start or a part is not a name.
fn normalize(parts: &[&str], may_climb: bool) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for part in parts {
        match *part {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() && !may_climb {
                    return None;
                }
            }
            part => {
                if part.contains('\0') || (THIS == Platform::Windows && part.contains(':')) {
                    return None;
                }
                out.push(part.to_owned());
            }
        }
    }
    Some(out)
}

/// A path line's prefix, lowercase (`None` when it has none, which is
/// `file:`), and the path after it.
fn split_prefix(line: &str) -> (Option<String>, &str) {
    match line.split_once(':') {
        Some((prefix, rest))
            if prefix.chars().all(|c| c.is_ascii_alphabetic()) && prefix.len() > 1 =>
        {
            (Some(prefix.to_ascii_lowercase()), rest)
        }
        _ => (None, line),
    }
}

/// One path of a path list on this machine. `Ok(None)`: a path of another
/// platform or kind, skipped; `Err(826)`: not a valid path.
fn resolve(line: &str, documents: &Path) -> Result<Option<PathBuf>, ErrorCode> {
    let line = line.trim();
    let (prefix, rest) = split_prefix(line);
    let prefix = prefix.unwrap_or_else(|| "file".to_owned());
    let platform = match prefix.as_str() {
        "file" => None,
        "filemac" => Some(Platform::Mac),
        "filewin" => Some(Platform::Windows),
        "filelinux" => Some(Platform::Linux),
        _ => return Ok(None),
    };
    if platform.is_some_and(|p| p != THIS) {
        return Ok(None);
    }
    if rest.is_empty() {
        return Err(error(code::INVALID_PATH));
    }
    let invalid = || error(code::INVALID_PATH);
    if let Some(full) = rest.strip_prefix('/') {
        let unc = full.strip_prefix('/');
        let parts: Vec<&str> = unc.unwrap_or(full).split('/').collect();
        return Ok(Some(match (THIS, unc) {
            (Platform::Windows, Some(_)) => {
                // //computer/share/...
                let (host, share) = (
                    parts.first().ok_or_else(invalid)?,
                    parts.get(1).ok_or_else(invalid)?,
                );
                let tail = normalize(&parts[2..], false).ok_or_else(invalid)?;
                let mut path = PathBuf::from(format!(r"\\{host}\{share}\"));
                path.extend(tail);
                path
            }
            (Platform::Windows, None) => {
                let drive = parts.first().filter(|d| d.len() == 2 && d.ends_with(':'));
                let drive = drive.ok_or_else(invalid)?;
                let tail = normalize(&parts[1..], true).ok_or_else(invalid)?;
                let mut path = PathBuf::from(format!(r"{drive}\"));
                path.extend(tail);
                path
            }
            (Platform::Mac, _) => {
                let volume = parts
                    .first()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(invalid)?;
                let tail = normalize(&parts[1..], true).ok_or_else(invalid)?;
                let mut path = if *volume == boot_volume() {
                    PathBuf::from("/")
                } else {
                    let mounted = Path::new("/Volumes").join(volume);
                    if !mounted.exists() {
                        return Err(invalid());
                    }
                    mounted
                };
                path.extend(tail);
                path
            }
            (Platform::Linux, _) => {
                let tail = normalize(&parts, true).ok_or_else(invalid)?;
                let mut path = PathBuf::from("/");
                path.extend(tail);
                path
            }
        }));
    }
    // Relative: inside the Documents folder.
    let parts: Vec<&str> = rest.split(['/', '\\']).collect();
    let tail = normalize(&parts, false).ok_or_else(invalid)?;
    if tail.is_empty() {
        return Err(invalid());
    }
    let mut path = documents.to_path_buf();
    path.extend(tail);
    Ok(Some(path))
}

/// A native absolute path with `.` and `..` resolved on its text; `None`
/// for a relative path, a NUL, `..` above the root, or (on Windows) a `:`
/// in a name.
fn native_absolute(text: &str) -> Option<PathBuf> {
    let path = Path::new(text);
    if text.contains('\0') || !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => {
                if THIS == Platform::Windows && name.to_string_lossy().contains(':') {
                    return None;
                }
                out.push(name);
            }
        }
    }
    Some(out)
}

/// The native absolute path `text` names, `.` and `..` resolved (826
/// otherwise): where a whole-file read, write or delete acts.
pub(crate) fn native_target(text: &str) -> Result<PathBuf, ErrorCode> {
    native_absolute(text).ok_or(error(code::INVALID_PATH))
}

/// The candidates of a path list in order: the usable paths, or the first
/// error when none is.
pub(crate) fn candidates(paths: &[String], documents: &Path) -> Result<Vec<PathBuf>, ErrorCode> {
    let mut found = Vec::new();
    let mut first_error = None;
    for line in paths {
        match resolve(line, documents) {
            Ok(Some(path)) => found.push(path),
            Ok(None) => {}
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    if found.is_empty() {
        return Err(first_error.unwrap_or(error(code::FILE_MISSING)));
    }
    Ok(found)
}

/// The first path of the list that names an existing file (or folder,
/// with `folders`).
pub(crate) fn existing(
    paths: &[String],
    documents: &Path,
    folders: bool,
) -> Result<PathBuf, ErrorCode> {
    candidates(paths, documents)?
        .into_iter()
        .find(|p| p.is_file() || folders && p.exists())
        .ok_or(error(code::FILE_MISSING))
}

fn io_error(e: &std::io::Error, otherwise: i32) -> ErrorCode {
    data_files::io_error(e, error(otherwise))
}

/// The data files scripts have open, by path.
pub(crate) struct DataFiles {
    pub folders: Folders,
    handles: data_files::DataFiles<PathBuf>,
}

impl Default for DataFiles {
    fn default() -> Self {
        DataFiles {
            folders: Folders::of_this_user(),
            handles: data_files::DataFiles::default(),
        }
    }
}

impl DataFiles {
    #[cfg(test)]
    fn in_folder(documents: PathBuf) -> Self {
        DataFiles {
            folders: Folders {
                desktop: documents.clone(),
                temporary: documents.clone(),
                documents,
            },
            handles: data_files::DataFiles::default(),
        }
    }

    fn documents(&self) -> &Path {
        &self.folders.documents
    }

    fn is_open(&self, path: &Path) -> bool {
        self.handles.is_open(&path.to_path_buf())
    }

    /// `Get ( TemporaryPath )`'s folder, created when first asked for.
    pub(crate) fn temporary_folder(&self) -> &Path {
        let _ = std::fs::create_dir_all(&self.folders.temporary);
        &self.folders.temporary
    }

    /// `Get ( OpenDataFileInfo )`: one line per open file, its id, a tab
    /// and its path with `file:`.
    pub(crate) fn info(&self) -> String {
        self.handles.info()
    }

    pub(crate) fn exists(&self, paths: &[String]) -> Result<bool, ErrorCode> {
        match existing(paths, self.documents(), true) {
            Ok(_) => Ok(true),
            Err(e) if e.0 == code::FILE_MISSING => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn size(&self, paths: &[String]) -> Result<i64, ErrorCode> {
        let path = existing(paths, self.documents(), false)?;
        let metadata = std::fs::metadata(&path).map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        Ok(metadata.len() as i64)
    }

    pub(crate) fn create(&self, paths: &[String], create_folders: bool) -> Result<(), ErrorCode> {
        let path = candidates(paths, self.documents())?
            .into_iter()
            .next()
            .ok_or(error(code::FILE_MISSING))?;
        if path.is_dir() {
            return Err(error(code::CANNOT_CREATE));
        }
        if self.is_open(&path) {
            return Err(error(code::IN_USE));
        }
        if let Some(parent) = path.parent() {
            if !parent.is_dir() {
                if !create_folders {
                    return Err(error(code::FILE_MISSING));
                }
                std::fs::create_dir_all(parent).map_err(|e| io_error(&e, code::CANNOT_CREATE))?;
            }
        }
        // An existing file is replaced by an empty one.
        File::create(&path).map_err(|e| io_error(&e, code::CANNOT_CREATE))?;
        Ok(())
    }

    pub(crate) fn open(&mut self, paths: &[String]) -> Result<i64, ErrorCode> {
        let path = existing(paths, self.documents(), false)?;
        self.handles.admit(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .or_else(|_| File::open(&path))
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        let shown = format!("file:{}", filemaker_path(&path, false));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.handles.insert(path, shown, name, file)
    }

    pub(crate) fn close(&mut self, id: i64) -> Result<(), ErrorCode> {
        self.handles.close(id)
    }

    pub(crate) fn position(&mut self, id: i64) -> Result<i64, ErrorCode> {
        self.handles.position(id)
    }

    pub(crate) fn set_position(&mut self, id: i64, position: i64) -> Result<(), ErrorCode> {
        self.handles.set_position(id, position)
    }

    /// Write to Data File, text as `text` gives it.
    pub(crate) fn write(
        &mut self,
        id: i64,
        data: &Value,
        text: impl FnOnce(&Value) -> String,
        encoding: Encoding,
        line_feed: bool,
    ) -> Result<(), ErrorCode> {
        self.handles.write(id, data, text, encoding, line_feed)
    }

    pub(crate) fn read(
        &mut self,
        id: i64,
        amount: Option<i64>,
        encoding: Encoding,
    ) -> Result<Value, ErrorCode> {
        self.handles.read(id, amount, encoding)
    }

    pub(crate) fn delete(&self, paths: &[String]) -> Result<(), ErrorCode> {
        let path = existing(paths, self.documents(), false)?;
        if self.is_open(&path) {
            return Err(error(code::IN_USE));
        }
        std::fs::remove_file(&path).map_err(|e| io_error(&e, code::IN_USE))
    }

    /// The desktop folder as a native path, when it exists and its path is
    /// text (`Folders.UserDesktop`).
    pub(crate) fn desktop_folder(&self) -> Option<String> {
        let desktop = &self.folders.desktop;
        if !desktop.is_dir() {
            return None;
        }
        desktop.to_str().map(str::to_owned)
    }

    /// Create the folder at native path `path` and the folders above it
    /// (`Files.CreateDirectory`); one already there is no error.
    pub(crate) fn create_folder(&self, path: &str) -> Result<(), ErrorCode> {
        let path = native_absolute(path).ok_or(error(code::INVALID_PATH))?;
        if path.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(&path).map_err(|e| io_error(&e, code::CANNOT_CREATE))
    }

    /// The bytes of the file at native path `path`; one with more than
    /// `limit` is refused (2). The bound holds while reading, whatever size
    /// the file reports (a growing file, `/proc`'s), and only a regular file
    /// is opened, so no read waits on a pipe.
    pub(crate) fn read_native(&self, path: &str, limit: u64) -> Result<Vec<u8>, ErrorCode> {
        let path = native_absolute(path).ok_or(error(code::INVALID_PATH))?;
        if !path.is_file() {
            return Err(error(code::FILE_MISSING));
        }
        let file = File::open(&path).map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        let regular = file.metadata().is_ok_and(|m| m.is_file());
        if !regular {
            return Err(error(code::FILE_MISSING));
        }
        let mut data = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        if data.len() as u64 > limit {
            return Err(shared::MEMORY);
        }
        Ok(data)
    }

    /// The native path of a path list's first line for this platform
    /// (`Path.FileMakerPathToNativePath`): a full FileMaker path by this
    /// platform's rules, as a path list resolves it, its closing `/` kept.
    /// A line of another platform (`filemac:` off a Mac) is skipped, never
    /// read by this platform's rules; a list without a line for this
    /// platform is 100. A relative path is 826 [O, the plug-in's docs:
    /// "This function does not work for relative paths"]. With
    /// `pass_through`, a line without a prefix that is already a native
    /// path naming something that exists, or in a folder that exists, is
    /// returned as given [I: the docs' "pass through a native path directly
    /// ... if the path is valid already"].
    pub(crate) fn native_of(&self, list: &str, pass_through: bool) -> Result<String, ErrorCode> {
        let mut first_error = None;
        for line in list.split(['\r', '\n']).map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if line.contains('\0') {
                first_error.get_or_insert(error(code::INVALID_PATH));
                continue;
            }
            let (prefix, rest) = split_prefix(line);
            let native = Path::new(line);
            if pass_through
                && prefix.is_none()
                && native.is_absolute()
                && (native.exists() || native.parent().is_some_and(Path::is_dir))
            {
                return Ok(line.to_owned());
            }
            if !rest.starts_with('/') {
                first_error.get_or_insert(error(code::INVALID_PATH));
                continue;
            }
            match resolve(line, self.documents()) {
                Ok(Some(path)) => {
                    let Some(text) = path.to_str() else {
                        first_error.get_or_insert(error(code::INVALID_PATH));
                        continue;
                    };
                    let mut text = text.to_owned();
                    if rest.ends_with('/') && !text.ends_with(std::path::MAIN_SEPARATOR) {
                        text.push(std::path::MAIN_SEPARATOR);
                    }
                    return Ok(text);
                }
                Ok(None) => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or(error(code::FILE_MISSING)))
    }

    /// Delete the file at native path `path` (`Files.Delete`): one that
    /// exists (100), no folder (800), no data file a script has open (300).
    /// A link is deleted itself, not what it names.
    pub(crate) fn delete_native(&self, path: &str) -> Result<(), ErrorCode> {
        let path = native_absolute(path).ok_or(error(code::INVALID_PATH))?;
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|e| io_error(&e, code::FILE_MISSING))?;
        if metadata.is_dir() {
            return Err(error(code::CANNOT_CREATE));
        }
        if self.holds(&path) {
            return Err(error(code::IN_USE));
        }
        std::fs::remove_file(&path).map_err(|e| io_error(&e, code::IN_USE))
    }

    /// Whether the file at `path` is one a script has open as a data file:
    /// the same file, however the path names it (a link, another spelling).
    /// Off Unix, where a file's identity is not at hand, any open data file
    /// counts, so nothing replaces or deletes one renamed since it opened.
    pub(crate) fn holds(&self, path: &Path) -> bool {
        if self.is_open(path) {
            return true;
        }
        let Ok(target) = std::fs::metadata(path) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            self.handles.files().any(|(_, file)| {
                file.metadata()
                    .is_ok_and(|m| m.dev() == target.dev() && m.ino() == target.ino())
            })
        }
        #[cfg(not(unix))]
        {
            let _ = target;
            self.handles.files().next().is_some()
        }
    }

    /// Replace the file at native path `path` with `data`.
    pub(crate) fn write_native(&self, path: &str, data: &[u8]) -> Result<(), ErrorCode> {
        let path = native_absolute(path).ok_or(error(code::INVALID_PATH))?;
        if path.is_dir() {
            return Err(error(code::CANNOT_CREATE));
        }
        if !path.parent().is_some_and(Path::is_dir) {
            return Err(error(code::FILE_MISSING));
        }
        if self.holds(&path) {
            return Err(error(code::IN_USE));
        }
        crate::report::replace_file(&path, data).map_err(|e| io_error(&e, code::CANNOT_CREATE))
    }

    pub(crate) fn rename(&self, paths: &[String], new_name: &str) -> Result<(), ErrorCode> {
        let name = new_name.trim();
        let plain = Path::new(name).components().count() == 1
            && matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            )
            && !name.contains(['/', '\\', ':', '\0']);
        if !plain {
            return Err(error(code::INVALID_PATH));
        }
        let path = existing(paths, self.documents(), false)?;
        if self.is_open(&path) {
            return Err(error(code::IN_USE));
        }
        let target = path.with_file_name(name);
        if target.exists() {
            return Err(error(code::CANNOT_CREATE));
        }
        std::fs::rename(&path, &target).map_err(|e| io_error(&e, code::CANNOT_CREATE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("om-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn list(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn paths_resolve_by_prefix_platform_and_policy() {
        let docs = Path::new("/home/u/Documents");
        assert_eq!(
            resolve("file:data.txt", docs).unwrap(),
            Some(docs.join("data.txt"))
        );
        assert_eq!(
            resolve("data.txt", docs).unwrap(),
            Some(docs.join("data.txt"))
        );
        assert_eq!(
            resolve("file:a/./b/../c.txt", docs).unwrap(),
            Some(docs.join("a").join("c.txt"))
        );
        // Climbing out of the Documents folder is not a valid path.
        assert_eq!(
            resolve("file:../secret.txt", docs),
            Err(ErrorCode(code::INVALID_PATH))
        );
        assert_eq!(resolve("file:", docs), Err(ErrorCode(code::INVALID_PATH)));
        // Other kinds of paths are skipped.
        assert_eq!(resolve("fmnet:/host/file.fmp12", docs).unwrap(), None);
        assert_eq!(resolve("image:photo.jpg", docs).unwrap(), None);
        let other = if THIS == Platform::Windows {
            "filemac:/Macintosh HD/x"
        } else {
            "filewin:/C:/x.txt"
        };
        assert_eq!(resolve(other, docs).unwrap(), None);
        if THIS == Platform::Mac {
            let boot = boot_volume();
            assert_eq!(
                resolve(&format!("filemac:/{boot}/tmp/x.txt"), docs).unwrap(),
                Some(PathBuf::from("/tmp/x.txt"))
            );
            assert_eq!(
                resolve("file:/No Such Volume 1234/x", docs),
                Err(ErrorCode(code::INVALID_PATH))
            );
            assert_eq!(
                filemaker_path(Path::new("/tmp/x.txt"), false),
                format!("/{boot}/tmp/x.txt")
            );
        }
        if THIS == Platform::Linux {
            assert_eq!(
                resolve("filelinux:/tmp/../etc/x", docs).unwrap(),
                Some(PathBuf::from("/etc/x"))
            );
        }
        // The first usable path of a list.
        let first = candidates(&list(&["fmnet:/h/f", "file:b.txt", "file:c.txt"]), docs).unwrap();
        assert_eq!(first[0], docs.join("b.txt"));
    }

    /// FileMaker paths to native ones by this platform's rules, a
    /// `filemac:` line skipped rather than read as Linux's; and a file
    /// deleted, never a folder, a missing file or an open data file.
    #[cfg(target_os = "linux")]
    #[test]
    fn native_paths_and_deletes_follow_the_platforms_rules() {
        let dir = folder("native-delete");
        let mut files = DataFiles::in_folder(dir.clone());
        let d = dir.to_str().unwrap().to_owned();
        let native = |list: &str| files.native_of(list, true);
        assert_eq!(native(&format!("file:{d}/a.pdf")), Ok(format!("{d}/a.pdf")));
        assert_eq!(native(&format!("file:{d}/x/../")), Ok(format!("{d}/")));
        assert_eq!(native("file:a.pdf"), Err(ErrorCode(code::INVALID_PATH)));
        // A Mac's line is not this platform's, whatever its text.
        let mac = format!("filemac:/Macintosh HD{d}/a.pdf");
        assert_eq!(native(&mac), Err(ErrorCode(code::FILE_MISSING)));
        let both = format!("{mac}\rfilelinux:{d}/b.pdf");
        assert_eq!(native(&both), Ok(format!("{d}/b.pdf")));
        // A native path that exists is passed through; one with a NUL is
        // no path.
        assert_eq!(native(&d), Ok(d.clone()));
        assert_eq!(
            native(&format!("{d}/\0bad")),
            Err(ErrorCode(code::INVALID_PATH))
        );
        std::fs::write(dir.join("gone.pdf"), b"%PDF").unwrap();
        assert_eq!(files.delete_native(&format!("{d}/gone.pdf")), Ok(()));
        assert!(!dir.join("gone.pdf").exists());
        assert_eq!(
            files.delete_native(&format!("{d}/gone.pdf")),
            Err(ErrorCode(code::FILE_MISSING))
        );
        std::fs::create_dir(dir.join("kept")).unwrap();
        assert_eq!(
            files.delete_native(&format!("{d}/kept")),
            Err(ErrorCode(code::CANNOT_CREATE))
        );
        let path = list(&["file:open.txt"]);
        files.create(&path, false).unwrap();
        files.open(&path).unwrap();
        assert_eq!(
            files.delete_native(&format!("{d}/open.txt")),
            Err(ErrorCode(code::IN_USE))
        );
        assert!(dir.join("open.txt").exists());
    }

    /// A whole-file read stops at its bound whatever size the file reports,
    /// and a write never replaces a script's open data file, through a
    /// linked folder either.
    #[cfg(unix)]
    #[test]
    fn whole_file_reads_are_bounded_and_writes_spare_open_data_files() {
        let dir = folder("native");
        let mut files = DataFiles::in_folder(dir.clone());
        let native = |path: PathBuf| path.to_str().unwrap().to_owned();
        std::fs::write(dir.join("small.pdf"), b"12345").unwrap();
        let small = native(dir.join("small.pdf"));
        assert_eq!(files.read_native(&small, 5), Ok(b"12345".to_vec()));
        assert_eq!(files.read_native(&small, 4), Err(shared::MEMORY));
        // Its size is 0; its text is longer.
        #[cfg(target_os = "linux")]
        assert_eq!(
            files.read_native("/proc/self/status", 16),
            Err(shared::MEMORY)
        );
        std::fs::create_dir(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        let path = list(&["file:real/out.pdf"]);
        files.create(&path, false).unwrap();
        files.open(&path).unwrap();
        assert_eq!(
            files.write_native(&native(dir.join("alias").join("out.pdf")), b"%PDF"),
            Err(ErrorCode(code::IN_USE))
        );
        assert_eq!(
            std::fs::read(dir.join("real").join("out.pdf")).unwrap(),
            b""
        );
    }

    #[test]
    fn data_files_create_write_read_and_seek() {
        let dir = folder("rw");
        let mut files = DataFiles::in_folder(dir.clone());
        let path = list(&["file:log/change.log"]);
        assert_eq!(files.exists(&path), Ok(false));
        // The folder is missing and not created.
        assert_eq!(
            files.create(&path, false),
            Err(ErrorCode(code::FILE_MISSING))
        );
        files.create(&path, true).unwrap();
        assert_eq!(files.size(&path), Ok(0));
        let id = files.open(&path).unwrap();
        assert_eq!(files.open(&path), Err(ErrorCode(code::IN_USE)));
        let text = |v: &Value| match v {
            Value::Text(t) => t.as_str().to_owned(),
            _ => String::new(),
        };
        files
            .write(id, &Value::text("Completed"), text, Encoding::Utf8, false)
            .unwrap();
        assert_eq!(files.position(id), Ok(9));
        files
            .write(id, &Value::text("é"), text, Encoding::Utf16, true)
            .unwrap();
        assert_eq!(files.size(&path), Ok(9 + 2 + 2));
        files.set_position(id, 0).unwrap();
        let read = files.read(id, Some(4), Encoding::Utf8).unwrap();
        assert_eq!(text(&read), "Comp");
        files.set_position(id, 9).unwrap();
        let read = files.read(id, None, Encoding::Utf16).unwrap();
        assert_eq!(text(&read), "é\n");
        assert_eq!(
            files.read(id, None, Encoding::Utf8).unwrap_err(),
            shared::END_OF_FILE
        );
        files.set_position(id, 0).unwrap();
        let Value::Container(bytes) = files.read(id, Some(3), Encoding::Bytes).unwrap() else {
            panic!("bytes are container data");
        };
        assert_eq!(bytes.data(), b"Com");
        assert!(files.info().starts_with(&format!("{id}\tfile:")));
        assert!(files.info().ends_with("change.log"));
        // An open file is neither deleted nor renamed.
        assert_eq!(files.delete(&path), Err(ErrorCode(code::IN_USE)));
        files.close(id).unwrap();
        assert_eq!(files.close(id), Err(ErrorCode(code::FILE_MISSING)));
        assert_eq!(
            files.set_position(id, 0),
            Err(ErrorCode(code::FILE_MISSING))
        );
        // Rename takes a name only and never replaces a file.
        assert_eq!(
            files.rename(&path, "../x.log"),
            Err(ErrorCode(code::INVALID_PATH))
        );
        assert_eq!(
            files.rename(&path, "sub/x.log"),
            Err(ErrorCode(code::INVALID_PATH))
        );
        files.rename(&path, "change1.log").unwrap();
        let renamed = list(&["file:log/change1.log"]);
        assert_eq!(files.exists(&renamed), Ok(true));
        files.create(&path, false).unwrap();
        assert_eq!(
            files.rename(&renamed, "change.log"),
            Err(ErrorCode(code::CANNOT_CREATE))
        );
        // Delete removes files, not folders.
        assert_eq!(
            files.delete(&list(&["file:log"])),
            Err(ErrorCode(code::FILE_MISSING))
        );
        files.delete(&renamed).unwrap();
        assert_eq!(files.exists(&renamed), Ok(false));
        assert_eq!(files.delete(&renamed), Err(ErrorCode(code::FILE_MISSING)));
        // Create Data File empties an existing file.
        let id = files.open(&path).unwrap();
        files
            .write(id, &Value::text("abc"), text, Encoding::Utf8, false)
            .unwrap();
        files.close(id).unwrap();
        files.create(&path, false).unwrap();
        assert_eq!(files.size(&path), Ok(0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn at_most_25_data_files_are_open() {
        let dir = folder("limit");
        let mut files = DataFiles::in_folder(dir.clone());
        for i in 0..26 {
            files
                .create(&list(&[&format!("file:{i}.txt")]), false)
                .unwrap();
        }
        for i in 0..25 {
            files.open(&list(&[&format!("file:{i}.txt")])).unwrap();
        }
        assert_eq!(files.open(&list(&["file:25.txt"])), Err(shared::MEMORY));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
