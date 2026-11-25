//! Exports (docs/server-storage.md 2, rule 2; 5, "The engine"; 6.10): one
//! version of a hosted file written as an `.fmp12`.
//!
//! **What an export holds.** Exactly the version's key tree, every node
//! with its kind and value, empty lists included, written from a walk of
//! its view in key order into `om_hbam`'s streaming writer
//! ([`om_hbam::writer::NodeWriter`]), and the file's page 0 as it was
//! loaded ([`crate::derived::Derived::page_zero`]) with the fields a writer
//! maintains set as a save sets them: the clean-close flag, and the change
//! number counted on from the one the engine names for the version
//! (hbam-writer.md 5.1, 5.2). The engine says how a version is exported
//! ([`Source`]): written so, or, for a version that is the file exactly as
//! it was loaded, as the bytes it was loaded from (below).
//!
//! **The file engine's versions** (`Store::export_source`). A version a
//! save wrote, with nothing changed since, is written from the change
//! number that save started from, so its export is the saved file byte for
//! byte. A version with changes the disk lacks (a failed save's) is written
//! from the change number the next save starts from: what that save would
//! write. A version no save wrote, the file as it was loaded, is the bytes
//! it was loaded from, read from the file on disk where the store still
//! has it (the descriptor it holds locked, or the entry at the file's path
//! while it is the file the server read there, #468): FileMaker's own
//! pages, which no writer of the tree reproduces. So is a file hosted
//! read-only because it holds content the writer cannot write (section 2,
//! rule 2), also when changes its refused saves left wait in memory: those
//! are then not in the export, and [`Exported::current`] says so.
//!
//! **Checked.** Before it gets its final name, the file written is read
//! back with the streaming reader ([`om_hbam::stream::NodeStream`]) and
//! compared with a second walk of the view, node by node: path, kind (list
//! or value) and value, with a value over 1,000 bytes as the writer
//! splits it (hbam-writer.md 2.3), and page 0 as written. A difference, or
//! a file that does not read back, is an error. An image that is not the
//! version ([`Exported::current`] false) is read back to its end and its
//! page 0 compared, but not its nodes.
//!
//! **Memory** is what the writer holds (a few pages and the open key path,
//! hbam-writer.md 7.2), what the reader holds (a page and the open path),
//! one page of the view's walk at a time and the path of the node being
//! written, and a copy buffer of [`COPY_BUFFER`] bytes for an image. A
//! value is borrowed from the view while it is written and compared.
//! Nothing grows with the file.
//!
//! **Where it goes.** [`prepare`] writes and checks the export under
//! `<name>.omexport` in the destination folder ([`Folder`]), created only
//! where nothing is (an entry there is an error, never removed); the
//! caller may then let go of the view. [`Prepared::publish`] flushes it to
//! the device, renames it to the final name, never over an entry there,
//! and flushes the folder. A failure at any step, or a [`Prepared`]
//! dropped unpublished, removes what this export created, and only while
//! it is still that file: the temporary entry, or the final one when the
//! folder flush after the rename fails. [`FileEngine::export`] does both
//! steps with the file engine's reading, which it holds only for the
//! first.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, Permissions};
use std::io::{self, Seek, SeekFrom, Write};

use om_hbam::stream::NodeStream;
use om_hbam::writer::{self as hbam_writer, NodeWriter, MAX_INLINE_VALUE};

use super::reading::Read;
use super::view::{ReadError, View};
use super::FileEngine;
use crate::anchored::{file_identity, FileId};
use crate::hosting::Hosted;

/// What a temporary export's name adds to the final name.
const TEMPORARY: &str = ".omexport";

/// How many bytes an image copy reads at once.
pub const COPY_BUFFER: usize = 256 << 10;

/// How often, in bytes written, the caller hears how far an export got.
const REPORT_EVERY: u64 = 8 << 20;

/// How the engine exports a version (the module doc).
#[derive(Debug)]
pub enum Source {
    /// Written from the view, page 0 the loaded one with the change number
    /// a writer starts from.
    Written { change_number: u64 },
    /// The bytes the file was loaded from, read from `file`'s start;
    /// `current` when they are this version.
    Image { file: File, current: bool },
}

/// A folder an export is written into: new entries are made, renamed and
/// removed in it by name, never through a link at the name.
pub trait Folder {
    /// A new entry `name`, for reading and writing, only where nothing is,
    /// owner-only until the export gives it its permissions.
    fn create(&self, name: &OsStr) -> io::Result<File>;
    /// Whether the open `file` is entry `name`, a link there not followed.
    fn is_entry(&self, file: &File, name: &OsStr) -> io::Result<bool>;
    /// Rename entry `from` to `to`, never over an entry at `to`
    /// ([`io::ErrorKind::AlreadyExists`]).
    fn rename_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()>;
    /// Remove entry `name`.
    fn remove(&self, name: &OsStr) -> io::Result<()>;
    /// Flush the folder's entries to the device.
    fn sync(&self) -> io::Result<()>;
}

/// A hosted file's kind of folder ([`crate::anchored::Folder`]), opened by
/// its path and worked through by name.
impl Folder for crate::anchored::Folder {
    fn create(&self, name: &OsStr) -> io::Result<File> {
        self.create_new(name, true)
    }

    fn is_entry(&self, file: &File, name: &OsStr) -> io::Result<bool> {
        crate::anchored::Folder::is_entry(self, file, name)
    }

    fn rename_new(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        crate::anchored::Folder::rename_new(self, from, to)
    }

    fn remove(&self, name: &OsStr) -> io::Result<()> {
        crate::anchored::Folder::remove(self, name)
    }

    fn sync(&self) -> io::Result<()> {
        crate::anchored::Folder::sync(self)
    }
}

/// Where an export goes: `name` in `folder`, with `permissions` once it is
/// complete (owner-only without).
pub struct Destination<'d> {
    pub folder: &'d dyn Folder,
    pub name: &'d OsStr,
    pub permissions: Option<Permissions>,
}

/// What the caller hears while an export writes, and how it stops one:
/// `stop` is asked before each write, `written` told the bytes written
/// every few megabytes and at the end.
#[derive(Clone, Copy)]
pub struct Progress<'p> {
    pub stop: &'p dyn Fn() -> bool,
    pub written: &'p dyn Fn(u64),
}

impl Progress<'_> {
    /// No stopping and nothing to tell.
    pub fn none() -> Progress<'static> {
        fn never() -> bool {
            false
        }
        fn ignore(_: u64) {}
        Progress {
            stop: &never,
            written: &ignore,
        }
    }
}

/// A finished export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exported {
    /// Its length in bytes.
    pub size: u64,
    /// Its device and inode, where the system has them.
    pub id: Option<FileId>,
    /// Whether it holds the version read. Only an image of a file hosted
    /// read-only for content the writer cannot write, with changes its
    /// refused saves left in memory, does not (the module doc).
    pub current: bool,
    /// On the file engine, how many saves the file had had since it was
    /// loaded when the version exported is the file on disk, with no
    /// changes the disk lacks; `None` when it has such changes.
    pub saves: Option<u64>,
}

/// Why an export failed. Nothing is left under the final name, nor under
/// the temporary one.
#[derive(Debug)]
pub enum ExportError {
    /// A read of the view failed.
    Read(ReadError),
    /// The view holds what the writer cannot write, or the file's page 0 is
    /// not one it writes.
    Unwritable(om_hbam::Error),
    /// The file written does not read back.
    Unreadable(om_hbam::Error),
    /// The file written reads back other than the view: `what` at `path`.
    Differs {
        path: Vec<Vec<u8>>,
        what: &'static str,
    },
    /// The bytes the file was loaded from could not be read.
    Image(io::Error),
    /// Creating, writing, flushing or renaming the export failed.
    Io(io::Error),
    /// An entry is in the way at the temporary name.
    TemporaryTaken(OsString),
    /// An entry is at the final name.
    Exists(OsString),
    /// The caller stopped it.
    Stopped,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Read(e) => write!(f, "the file could not be read: {e}"),
            ExportError::Unwritable(e) => write!(f, "the file cannot be written: {e}"),
            ExportError::Unreadable(e) => write!(f, "the export does not read back: {e}"),
            ExportError::Differs { path, what } => {
                write!(f, "the export differs from the file at {path:02x?}: {what}")
            }
            ExportError::Image(e) => {
                write!(f, "the file as it was loaded could not be read: {e}")
            }
            ExportError::Io(e) => write!(f, "{e}"),
            ExportError::TemporaryTaken(name) => write!(
                f,
                "{} is in the way of the export (left by an earlier one?)",
                name.to_string_lossy()
            ),
            ExportError::Exists(name) => write!(f, "{} already exists", name.to_string_lossy()),
            ExportError::Stopped => write!(f, "the export was stopped"),
        }
    }
}

impl std::error::Error for ExportError {}

/// The marker of an export its caller stopped, carried through the writer
/// as an I/O error.
#[derive(Debug)]
struct Stop;

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for Stop {}

fn stopped(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Stop>())
}

/// An I/O error of the destination, or the caller's stop.
fn destination(e: io::Error) -> ExportError {
    if stopped(&e) {
        ExportError::Stopped
    } else {
        ExportError::Io(e)
    }
}

/// An error of the writer: the destination's I/O, the caller's stop, or
/// what the writer refuses.
fn writer_error(e: om_hbam::Error) -> ExportError {
    match e {
        om_hbam::Error::Io(e) => destination(e),
        e => ExportError::Unwritable(e),
    }
}

/// The export's file, counting what is written and asking whether to
/// stop before each write.
struct Counted<'a, 'p> {
    file: &'a mut File,
    progress: Progress<'p>,
    unreported: u64,
}

impl Counted<'_, '_> {
    fn report(&mut self) {
        if self.unreported > 0 {
            (self.progress.written)(std::mem::take(&mut self.unreported));
        }
    }
}

impl Write for Counted<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if (self.progress.stop)() {
            return Err(io::Error::other(Stop));
        }
        let n = self.file.write(buf)?;
        self.unreported += n as u64;
        if self.unreported >= REPORT_EVERY {
            self.report();
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl io::Read for Counted<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for Counted<'_, '_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

/// Write the version `read` reads, as `source` says, into `target`, an
/// empty file open for reading and writing, and check it (the module doc).
/// What the export holds; on an error `target` holds part of a file, for
/// the caller to discard.
pub fn write_checked(
    read: Read<'_>,
    source: Source,
    target: &mut File,
    progress: Progress<'_>,
) -> Result<Checked, ExportError> {
    let mut counted = Counted {
        file: target,
        progress,
        unreported: 0,
    };
    let checked = match source {
        Source::Written { change_number } => {
            let page_zero =
                hbam_writer::with_change_number(read.derived.page_zero(), change_number);
            write_view(read.view, &page_zero, &mut counted)?;
            counted.report();
            let file = &*counted.file;
            check(read.view, file, Expect::Written(&page_zero, change_number))?;
            Checked { current: true }
        }
        Source::Image { file, current } => {
            copy_image(&file, &mut counted)?;
            counted.report();
            let target = &*counted.file;
            check(
                read.view,
                target,
                Expect::Image(read.derived.page_zero(), current),
            )?;
            Checked { current }
        }
    };
    Ok(checked)
}

/// What [`write_checked`] wrote.
#[derive(Clone, Copy, Debug)]
pub struct Checked {
    /// [`Exported::current`].
    pub current: bool,
}

/// Every node of `view` but the root, in the order of a depth-first walk,
/// into a writer of a file with page 0 `page_zero`.
fn write_view(
    view: &dyn View,
    page_zero: &[u8],
    target: &mut Counted<'_, '_>,
) -> Result<(), ExportError> {
    let mut writer = NodeWriter::new(target, page_zero).map_err(writer_error)?;
    let mut path: Vec<Vec<u8>> = Vec::new();
    for step in view.walk(&[]) {
        let step = step.map_err(ExportError::Read)?;
        if step.depth == 0 {
            // The root is the file's list itself: it has no key to carry a
            // value.
            if step.head.value.as_deref().is_some_and(|v| !v.is_empty()) {
                return Err(ExportError::Unwritable(om_hbam::Error::Write(
                    "the root holds a value",
                )));
            }
            continue;
        }
        path.truncate(step.depth - 1);
        path.push(step.segment.into_owned());
        writer
            .node(&path, step.head.list, step.head.value.as_deref())
            .map_err(writer_error)?;
    }
    writer.finish().map_err(writer_error)?;
    Ok(())
}

/// Copy `image` from its start to `target`.
fn copy_image(image: &File, target: &mut Counted<'_, '_>) -> Result<(), ExportError> {
    use std::io::Read as _;
    let mut source = image;
    source
        .seek(SeekFrom::Start(0))
        .map_err(ExportError::Image)?;
    let mut buffer = vec![0; COPY_BUFFER];
    loop {
        let n = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ExportError::Image(e)),
        };
        target.write_all(&buffer[..n]).map_err(destination)?;
    }
    target.flush().map_err(destination)
}

/// What the file read back must hold besides the view's nodes.
enum Expect<'a> {
    /// Written from the view with this page 0 and starting change number:
    /// page 0 closed by the writer, and long values split as it splits
    /// them.
    Written(&'a [u8], u64),
    /// The image of a file loaded with this page 0; its nodes compared
    /// when it is the version.
    Image(&'a [u8], bool),
}

/// Read `file` back from its start and compare it with `view` (the module
/// doc, "Checked").
fn check(view: &dyn View, file: &File, expect: Expect<'_>) -> Result<(), ExportError> {
    let mut stream = NodeStream::new(file).map_err(ExportError::Unreadable)?;
    let page_zero = stream.superblock().raw;
    let differs = |what| ExportError::Differs {
        path: Vec::new(),
        what,
    };
    let (split, compare) = match expect {
        Expect::Written(given, start) => {
            // Each page but page 0 took the next change number.
            let pages = u64::from(stream.page_count()).saturating_sub(1);
            let expected = start
                .checked_add(pages)
                .and_then(|last| hbam_writer::closed_page_zero(given, last));
            if expected.as_deref() != Some(page_zero) {
                return Err(differs("page 0 is not the one written"));
            }
            (true, true)
        }
        Expect::Image(loaded, current) => {
            if page_zero != loaded {
                return Err(differs("page 0 is not the one the file was loaded with"));
            }
            (false, current)
        }
    };
    if !compare {
        while stream
            .next_node()
            .map_err(ExportError::Unreadable)?
            .is_some()
        {}
        return Ok(());
    }
    let mut path: Vec<Vec<u8>> = Vec::new();
    for step in view.walk(&[]) {
        let step = step.map_err(ExportError::Read)?;
        if step.depth == 0 {
            continue;
        }
        path.truncate(step.depth - 1);
        path.push(step.segment.into_owned());
        let value = step.head.value.as_deref().unwrap_or_default();
        if split && !step.head.list && value.len() > MAX_INLINE_VALUE {
            // A list of the length and the pieces (hbam-writer.md 2.3).
            next_is(&mut stream, &path, true, &[])?;
            let length = u32::try_from(value.len()).map_err(|_| differs("a value over 4 GiB"))?;
            path.push(vec![0]);
            next_is(&mut stream, &path, false, &length.to_be_bytes())?;
            for (i, piece) in value.chunks(MAX_INLINE_VALUE).enumerate() {
                *path.last_mut().expect("the piece's key") = om_hbam::serial::encode(i as u64 + 1);
                next_is(&mut stream, &path, false, piece)?;
            }
            path.pop();
        } else {
            next_is(&mut stream, &path, step.head.list, value)?;
        }
    }
    match stream.next_node().map_err(ExportError::Unreadable)? {
        None => Ok(()),
        Some(node) => Err(ExportError::Differs {
            path: node.path.to_vec(),
            what: "a node the view does not hold",
        }),
    }
}

/// The next node `stream` reads is the one at `path`, a list or not, with
/// `value` (empty for none: a file holds no value as no bytes).
fn next_is(
    stream: &mut NodeStream<&File>,
    path: &[Vec<u8>],
    list: bool,
    value: &[u8],
) -> Result<(), ExportError> {
    let differs = |what| ExportError::Differs {
        path: path.to_vec(),
        what,
    };
    let node = stream
        .next_node()
        .map_err(ExportError::Unreadable)?
        .ok_or_else(|| differs("the node is missing from the export"))?;
    if node.path != path {
        return Err(differs("the export holds another node here"));
    }
    if node.push != list {
        return Err(differs("the node's kind differs"));
    }
    if node.value.unwrap_or_default() != value {
        return Err(differs("the node's value differs"));
    }
    Ok(())
}

/// An export written and checked under its temporary name ([`prepare`]),
/// not yet published. Dropped unpublished, it removes its file.
pub struct Prepared<'d> {
    folder: &'d dyn Folder,
    name: OsString,
    temporary: OsString,
    /// The temporary file, while it is this export's to remove.
    file: Option<File>,
    current: bool,
}

/// The temporary name of an export named `name`.
pub fn temporary_name(name: &OsStr) -> OsString {
    let mut temporary = name.to_owned();
    temporary.push(TEMPORARY);
    temporary
}

/// Write and check the version `read` reads, as `source` says, under the
/// temporary name beside `to`'s (the module doc).
pub fn prepare<'d>(
    read: Read<'_>,
    source: Source,
    to: &Destination<'d>,
    progress: Progress<'_>,
) -> Result<Prepared<'d>, ExportError> {
    let temporary = temporary_name(to.name);
    let file = match to.folder.create(&temporary) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExportError::TemporaryTaken(temporary))
        }
        Err(e) => return Err(ExportError::Io(e)),
    };
    let mut prepared = Prepared {
        folder: to.folder,
        name: to.name.to_owned(),
        temporary,
        file: Some(file),
        current: true,
    };
    let file = prepared.file.as_mut().expect("just created");
    prepared.current = write_checked(read, source, file, progress)?.current;
    if let Some(permissions) = &to.permissions {
        file.set_permissions(permissions.clone())
            .map_err(ExportError::Io)?;
    }
    Ok(prepared)
}

impl Prepared<'_> {
    /// Flush the export, give it its final name and flush the folder. The
    /// file engine's saves on `exported.saves`, from the reading the export
    /// was written from.
    pub fn publish(mut self, saves: Option<u64>) -> Result<Exported, ExportError> {
        let file = self.file.as_ref().expect("until published");
        file.sync_all().map_err(ExportError::Io)?;
        let size = file.metadata().map_err(ExportError::Io)?.len();
        let id = file_identity(file).map_err(ExportError::Io)?;
        if !self.owned(&self.temporary) {
            // Replaced since it was made: whatever is there is not this
            // export's to rename or remove.
            self.file = None;
            return Err(ExportError::Io(io::Error::other(
                "the export's temporary file was replaced",
            )));
        }
        match self.folder.rename_new(&self.temporary, &self.name) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(ExportError::Exists(self.name.clone()))
            }
            Err(e) => return Err(ExportError::Io(e)),
        }
        let file = self.file.take().expect("until published");
        if let Err(e) = self.folder.sync() {
            // Nothing is left under the final name: it is removed while it
            // is still this export.
            if self.folder.is_entry(&file, &self.name).unwrap_or(false) {
                let _ = self.folder.remove(&self.name);
            }
            return Err(ExportError::Io(e));
        }
        Ok(Exported {
            size,
            id,
            current: self.current,
            saves,
        })
    }

    /// Whether entry `name` is still this export's file.
    fn owned(&self, name: &OsStr) -> bool {
        self.file
            .as_ref()
            .is_some_and(|file| self.folder.is_entry(file, name).unwrap_or(false))
    }
}

impl Drop for Prepared<'_> {
    fn drop(&mut self) {
        if self.owned(&self.temporary) {
            let _ = self.folder.remove(&self.temporary);
        }
    }
}

impl FileEngine {
    /// Export the version of `file` its store holds now to `to` (the module
    /// doc): written and checked under the store's read lock, which a
    /// reading holds (`FileEngine::read`), then flushed and renamed after
    /// the lock is let go. A server switched to the test view
    /// (`Server::checking_reads`) exports through it alone.
    pub fn export(
        &self,
        file: &Hosted,
        to: &Destination<'_>,
        progress: Progress<'_>,
    ) -> Result<Exported, ExportError> {
        let (prepared, saves) = {
            let reading = self.read(file);
            let source = reading.export_source().map_err(ExportError::Image)?;
            let saves = reading.saved_version();
            let prepared = reading.serve_once(|read| prepare(read, source, to, progress))?;
            (prepared, saves)
        };
        prepared.publish(saves)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, HbamFile, OwnedNode};
    use om_protocol::write::{GetSerial, SERIAL_INCREMENT};
    use om_scripts::text_node;

    use super::super::copies::Copies;
    use super::super::{AdvanceSerial, Operation, Writer};
    use super::*;
    use crate::anchored::FileAt;
    use crate::config::HostedFile;
    use crate::eventlog::EventLog;
    use crate::hosting::Hosting;
    use crate::scripts::fixture;

    /// A folder for one test's exports in the build's target folder,
    /// removed with what is in it when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/om-export-tests")
                .join(format!("{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        /// The folder, opened as a hosted file's is.
        fn folder(&self) -> crate::anchored::Folder {
            FileAt::open(&self.0.join("any.fmp12"))
                .unwrap()
                .folder_now()
                .unwrap()
        }

        fn read(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.0.join(name)).unwrap()
        }

        /// The names in the folder, sorted.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `name` in `folder`.
    fn to<'d>(folder: &'d crate::anchored::Folder, name: &'d str) -> Destination<'d> {
        Destination {
            folder,
            name: OsStr::new(name),
            permissions: None,
        }
    }

    /// The fixture's file with 40 tasks, a value of 2,500 bytes (held in
    /// pieces once the file is loaded) and an empty list, hosted from a
    /// file of its own.
    fn hosted(tag: &str) -> (fixture::TempFile, Arc<Hosted>) {
        let mut tree = fixture::tree(&[], true);
        fixture::add_tasks(&mut tree, 1..=40);
        tree.put(
            &[&[2], &[0x70]],
            OwnedNode::leaf(Vec::new(), long_value(2500)),
        );
        tree.put(&[&[2], &[0x71]], OwnedNode::list(Vec::new(), Vec::new()));
        let file = fixture::TempFile::from_tree(tag, &tree);
        let hosting = Hosting::load(
            &[HostedFile {
                name: fixture::FILE.into(),
                path: file.0.clone(),
            }],
            false,
            &EventLog::default(),
            &FileEngine::default(),
        )
        .unwrap();
        let hosted = hosting.by_name(fixture::FILE).unwrap();
        (file, hosted)
    }

    fn long_value(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Task `id`'s record path.
    fn record(id: u64) -> Vec<Vec<u8>> {
        vec![uint_segment(fixture::TABLE), vec![0x05], serial::encode(id)]
    }

    /// Change the tree without saving it: a task renamed, and a value of
    /// 3,100 bytes held whole, as a write leaves one until the file is
    /// loaded again.
    fn change(file: &Hosted, name: &str) {
        let mut store = file.write_store(&Writer::for_tests());
        let task = OwnedNode::list(
            Vec::new(),
            vec![text_node(&uint_segment(fixture::NAME), name)],
        );
        assert!(store.put_subtree(&record(7), task));
        assert!(store.put_subtree(
            &[vec![2], vec![0x72]],
            OwnedNode::leaf(Vec::new(), long_value(3100))
        ));
    }

    /// Save what [`change`] left.
    fn save(file: &Hosted) {
        let mut store = file.write_store(&Writer::for_tests());
        assert_eq!(store.save(&file.at).unwrap(), true);
    }

    /// Export `file`'s current version to `name` through the test view,
    /// which copies every node and walks three at a time, failing the reads
    /// at or below `failing`.
    fn export_through_copies(
        file: &Hosted,
        failing: &[Vec<Vec<u8>>],
        folder: &crate::anchored::Folder,
        name: &str,
    ) -> Result<Exported, ExportError> {
        let engine = FileEngine::default();
        let reading = engine.read(file);
        let source = reading.export_source().map_err(ExportError::Image)?;
        let saves = reading.saved_version();
        let read = reading.read();
        let copies = Copies::new(read.view, failing);
        let read = Read {
            view: &copies,
            ..read
        };
        let destination = to(folder, name);
        let prepared = prepare(read, source, &destination, Progress::none())?;
        drop(reading);
        prepared.publish(saves)
    }

    /// An export of a hosted file is the file on disk for its version: as
    /// it was loaded, the bytes it was loaded from; once saved, what the
    /// save wrote; with changes the disk lacks, what the next save writes.
    /// Each reads back as the view (the check, which every export makes),
    /// and the test view, which copies and pages, gives the same bytes.
    #[test]
    fn an_export_is_the_file_a_save_writes_for_its_version() {
        let (disk, file) = hosted("export-versions");
        let scratch = Scratch::new("versions");
        let folder = scratch.folder();
        let engine = FileEngine::default();
        let export = |name: &str| engine.export(&file, &to(&folder, name), Progress::none());

        let loaded = export("loaded.fmp12").unwrap();
        assert_eq!(
            scratch.read("loaded.fmp12"),
            std::fs::read(&disk.0).unwrap()
        );
        assert_eq!((loaded.current, loaded.saves), (true, Some(0)));
        export_through_copies(&file, &[], &folder, "loaded-copies.fmp12").unwrap();
        assert_eq!(
            scratch.read("loaded-copies.fmp12"),
            scratch.read("loaded.fmp12")
        );

        change(&file, "Renamed");
        let unsaved = export("unsaved.fmp12").unwrap();
        assert_eq!((unsaved.current, unsaved.saves), (true, None));
        export_through_copies(&file, &[], &folder, "unsaved-copies.fmp12").unwrap();
        save(&file);
        let saved = std::fs::read(&disk.0).unwrap();
        assert_eq!(scratch.read("unsaved.fmp12"), saved);
        assert_eq!(scratch.read("unsaved-copies.fmp12"), saved);
        assert_ne!(saved, scratch.read("loaded.fmp12"));

        let after = export("saved.fmp12").unwrap();
        assert_eq!((after.saves, after.size), (Some(1), saved.len() as u64));
        assert_eq!(scratch.read("saved.fmp12"), saved);
        export_through_copies(&file, &[], &folder, "saved-copies.fmp12").unwrap();
        assert_eq!(scratch.read("saved-copies.fmp12"), saved);

        // The export reads as the file holds it: the renamed task, and the
        // long value in its pieces.
        let read = HbamFile::open(scratch.0.join("saved.fmp12")).unwrap();
        let seven = record(7);
        let path: Vec<&[u8]> = seven.iter().map(Vec::as_slice).collect();
        let task = read.subtree(&path).unwrap().unwrap().to_owned_node();
        let name = task.child(&uint_segment(fixture::NAME)).unwrap();
        assert_eq!(name, &text_node(&uint_segment(fixture::NAME), "Renamed"));
        let long = read.subtree(&[&[2], &[0x72]]).unwrap().unwrap();
        assert_eq!(long.assembled().unwrap().as_ref(), long_value(3100));
        assert!(read.subtree(&[&[2], &[0x71]]).unwrap().unwrap().push);
        assert_eq!(
            scratch.names(),
            [
                "loaded-copies.fmp12",
                "loaded.fmp12",
                "saved-copies.fmp12",
                "saved.fmp12",
                "unsaved-copies.fmp12",
                "unsaved.fmp12"
            ]
        );
    }

    /// A read that fails fails the export with its error, whether it comes
    /// while the export is written or while it is checked (an image is
    /// copied without the view, then checked against it), and leaves
    /// nothing in the folder.
    #[test]
    fn a_failed_read_fails_the_export_and_leaves_nothing() {
        let (_disk, file) = hosted("export-failing");
        let scratch = Scratch::new("failing");
        let folder = scratch.folder();
        let failing = vec![record(20)];
        for saved in [false, true] {
            if saved {
                change(&file, "Saved");
                save(&file);
            }
            match export_through_copies(&file, &failing, &folder, "failed.fmp12") {
                Err(ExportError::Read(ReadError::Unreadable { path, .. })) => {
                    assert_eq!(path, failing[0])
                }
                other => panic!("saved {saved}: {other:?}"),
            }
            assert!(scratch.names().is_empty(), "{:?}", scratch.names());
        }
    }

    /// An export never removes or replaces an entry it did not make: one
    /// at the temporary name fails it, one at the final name fails its
    /// publication; both are left as they were, and so is nothing else.
    #[test]
    fn an_export_leaves_other_entries_alone() {
        let (_disk, file) = hosted("export-entries");
        let scratch = Scratch::new("entries");
        let folder = scratch.folder();
        let engine = FileEngine::default();
        std::fs::write(scratch.0.join("taken.fmp12.omexport"), b"mine").unwrap();
        assert!(matches!(
            engine.export(&file, &to(&folder, "taken.fmp12"), Progress::none()),
            Err(ExportError::TemporaryTaken(_))
        ));
        std::fs::write(scratch.0.join("there.fmp12"), b"there").unwrap();
        assert!(matches!(
            engine.export(&file, &to(&folder, "there.fmp12"), Progress::none()),
            Err(ExportError::Exists(_))
        ));
        assert_eq!(scratch.names(), ["taken.fmp12.omexport", "there.fmp12"]);
        assert_eq!(scratch.read("taken.fmp12.omexport"), b"mine");
        assert_eq!(scratch.read("there.fmp12"), b"there");
        // A stop asked for while it writes.
        let stop = || true;
        let progress = Progress {
            stop: &stop,
            written: &|_| {},
        };
        assert!(matches!(
            engine.export(&file, &to(&folder, "stopped.fmp12"), progress),
            Err(ExportError::Stopped)
        ));
        assert_eq!(scratch.names(), ["taken.fmp12.omexport", "there.fmp12"]);
    }

    /// A write committed while an export runs is in it whole or not at
    /// all: the export reads one version. On the file engine the commit
    /// waits for the export's view, so the export holds the version before
    /// it, and the next export the one after.
    #[test]
    fn a_change_committed_during_an_export_is_wholly_in_it_or_not_at_all() {
        let (disk, file) = hosted("export-overlap");
        let scratch = Scratch::new("overlap");
        let folder = scratch.folder();
        let engine = FileEngine::default();
        engine
            .export(&file, &to(&folder, "before.fmp12"), Progress::none())
            .unwrap();
        let serial = Operation::AdvanceSerial(AdvanceSerial {
            request: GetSerial {
                table: vec![uint_segment(fixture::TABLE)],
                field: vec![uint_segment(fixture::COUNT)],
                value: String::new(),
                increment: 1,
                flags: SERIAL_INCREMENT,
            },
        });
        let (started, writing) = mpsc::channel();
        let started = Mutex::new(Some(started));
        let committed = AtomicBool::new(false);
        let (engine, file, committed) = (&engine, &file, &committed);
        std::thread::scope(|scope| {
            let committer = scope.spawn(move || {
                writing.recv().unwrap();
                engine.submit(&file, serial);
                committed.store(true, Ordering::SeqCst);
            });
            // Once the export writes, the commit is submitted, and given
            // time to run if it could.
            let stop = || {
                if let Some(started) = started.lock().unwrap().take() {
                    started.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    assert!(!committed.load(Ordering::SeqCst));
                }
                false
            };
            let progress = Progress {
                stop: &stop,
                written: &|_| {},
            };
            engine
                .export(&file, &to(&folder, "during.fmp12"), progress)
                .unwrap();
            committer.join().unwrap();
        });
        assert!(committed.load(Ordering::SeqCst));
        assert_eq!(scratch.read("during.fmp12"), scratch.read("before.fmp12"));
        engine
            .export(&file, &to(&folder, "after.fmp12"), Progress::none())
            .unwrap();
        assert_ne!(scratch.read("after.fmp12"), scratch.read("before.fmp12"));
        assert_eq!(scratch.read("after.fmp12"), std::fs::read(&disk.0).unwrap());
    }

    /// The check finds a file that differs from the view: in a value, in a
    /// kind, by a node missing or added, or in page 0. Each variant is the
    /// view of a tree that differs from the one written by that alone.
    #[test]
    fn the_check_finds_every_difference() {
        let mut tree = fixture::tree(&[], true);
        fixture::add_tasks(&mut tree, 1..=5);
        tree.put(&[&[2], &[0x71]], OwnedNode::list(Vec::new(), Vec::new()));
        tree.put(
            &[&[2], &[0x72]],
            OwnedNode::leaf(Vec::new(), long_value(1500)),
        );
        let page_zero = om_hbam::new_superblock("Check").unwrap();
        let scratch = Scratch::new("check");
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scratch.0.join("written.fmp12"))
            .unwrap();
        let view = super::super::TreeView::new(&tree, 0);
        let mut counted = Counted {
            file: &mut file,
            progress: Progress::none(),
            unreported: 0,
        };
        write_view(&view, &page_zero, &mut counted).unwrap();
        let expect = || Expect::Written(&page_zero, 0);
        check(&view, &file, expect()).unwrap();
        let task = record(3);
        let task: Vec<&[u8]> = task.iter().map(Vec::as_slice).collect();
        let variants: Vec<(&str, Box<dyn Fn(&mut OwnedNode)>)> = vec![
            (
                "a value",
                Box::new(|t: &mut OwnedNode| {
                    t.put(
                        &[&[2], &[0x72]],
                        OwnedNode::leaf(Vec::new(), long_value(1499)),
                    )
                }),
            ),
            (
                "a kind",
                Box::new(|t: &mut OwnedNode| {
                    t.put(&[&[2], &[0x71]], OwnedNode::leaf(Vec::new(), Vec::new()))
                }),
            ),
            (
                "an empty list missing",
                Box::new(|t: &mut OwnedNode| {
                    t.remove(&[&[2], &[0x71]]);
                }),
            ),
            (
                "a node added",
                Box::new(|t: &mut OwnedNode| {
                    t.put(&[&[2], &[0x73]], OwnedNode::leaf(Vec::new(), vec![1]))
                }),
            ),
            (
                "a record missing",
                Box::new(move |t: &mut OwnedNode| {
                    t.remove(&task);
                }),
            ),
        ];
        for (what, vary) in variants {
            let mut other = tree.clone();
            vary(&mut other);
            let other_view = super::super::TreeView::new(&other, 0);
            assert!(
                matches!(
                    check(&other_view, &file, expect()),
                    Err(ExportError::Differs { .. })
                ),
                "{what}"
            );
        }
        let mut other_page = page_zero.clone();
        other_page[0x100] ^= 1;
        assert!(matches!(
            check(&view, &file, Expect::Written(&other_page, 0)),
            Err(ExportError::Differs { .. })
        ));
        assert!(matches!(
            check(&view, &file, Expect::Written(&page_zero, 1)),
            Err(ExportError::Differs { .. })
        ));
    }
}

/// The same against FileMaker's own files: the 66 Mac templates
/// (git-ignored extractions, `re/extracted`). Each is exported as it was
/// loaded (its own bytes), then with a change the disk lacks, which must
/// be what the save then writes, then as saved.
///
/// ```sh
/// cargo test -p om-server --lib -- --ignored exports_of_filemakers_files
/// ```
#[cfg(test)]
mod templates {
    use std::path::{Path, PathBuf};

    use om_hbam::{HbamFile, OwnedNode};

    use super::*;
    use crate::anchored::FileAt;
    use crate::store::Store;

    const MAC_TEMPLATES: &str =
        "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates";

    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the templates are present") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "fmp12") {
                out.push(path);
            }
        }
    }

    /// Export `store`'s version to `name` in `folder`.
    fn export(store: &Store, at: &FileAt, folder: &crate::anchored::Folder, name: &str) -> Vec<u8> {
        let view = store.tree_view();
        let read = Read {
            view: &view,
            derived: store.derived(),
            waiting: store.waiting(),
            read_only: store.is_read_only(),
        };
        let source = store.export_source(at).unwrap();
        let to = Destination {
            folder,
            name: OsStr::new(name),
            permissions: None,
        };
        prepare(read, source, &to, Progress::none())
            .unwrap()
            .publish(store.saved_version())
            .unwrap();
        let path = at.place().with_file_name(name);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        bytes
    }

    #[test]
    #[ignore = "requires the git-ignored extracted templates"]
    fn exports_of_filemakers_files() {
        let mut found = Vec::new();
        files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(MAC_TEMPLATES),
            &mut found,
        );
        found.sort();
        assert!(!found.is_empty());
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/om-export-tests")
            .join(format!("templates-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let (mut written, mut read_only) = (0, 0);
        for template in &found {
            let path = scratch.join("Template.fmp12");
            std::fs::copy(template, &path).unwrap();
            let at = FileAt::open(&path).unwrap();
            let folder = at.folder_now().unwrap();
            let original = std::fs::read(&path).unwrap();
            let file = HbamFile::from_bytes(original.clone()).unwrap();
            let mut store = Store::load(&file, "Template.fmp12").unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            {
                use std::os::unix::fs::MetadataExt;
                store.set_file(Some((metadata.dev(), metadata.ino())));
            }
            let shown = template.display();
            assert_eq!(
                export(&store, &at, &folder, "loaded.fmp12"),
                original,
                "{shown}"
            );
            if store.is_read_only() {
                read_only += 1;
            } else {
                assert!(store.put_subtree(
                    &[vec![2], vec![0x7e]],
                    OwnedNode::leaf(Vec::new(), b"exported".to_vec())
                ));
                let unsaved = export(&store, &at, &folder, "unsaved.fmp12");
                assert!(store.save(&at).unwrap());
                let saved = std::fs::read(&path).unwrap();
                assert_eq!(unsaved, saved, "{shown}");
                assert_eq!(
                    export(&store, &at, &folder, "saved.fmp12"),
                    saved,
                    "{shown}"
                );
                written += 1;
            }
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::remove_dir_all(&scratch).unwrap();
        println!(
            "{} files: {written} exported as loaded, unsaved and saved, {read_only} read-only as loaded",
            found.len()
        );
    }
}
