//! Exports (docs/server-storage.md 2, rule 2; 5, "Exports"; 6.10): one
//! version of a hosted file as an `.fmp12`.
//!
//! **Two ways.** The engine says how a version is exported ([`Source`]):
//!
//! - **The file on disk** ([`OnDisk`]), when the disk holds the version: a
//!   file engine's version a save wrote, or the file as it was loaded,
//!   with nothing changed since. The file is opened under the store's read
//!   lock, which is let go at once, and copied after it. No view is read:
//!   the file is what a checked save wrote, or what the server loaded.
//! - **Written from the view** ([`Source::Written`]), when the disk lacks
//!   the version (changes in memory that no save wrote), and for a caller
//!   that asks for a file rewritten (the desktop's compacted copy). A
//!   depth-first walk of the view in key order goes into `om_hbam`'s
//!   streaming writer ([`om_hbam::writer::NodeWriter`]): every node with
//!   its kind and value, empty lists included, and the file's page 0 as it
//!   was loaded ([`crate::derived::Derived::page_zero`]) with the fields a
//!   writer maintains set as a save sets them, the clean-close flag and the
//!   change number counted on from the one the engine names (hbam-writer.md
//!   5.1, 5.2). The file engine's view holds the store's read lock for the
//!   whole walk: writes to the file wait for it (docs/performance/export.md).
//!
//! **The file engine's versions** (`Store::export_source`). With nothing
//! changed since the last save or the load, the version is the file on
//! disk, opened where the store can tell it is that file (the descriptor
//! it holds locked, or the entry at the file's path while it is the file
//! the server read or last saved there,
//! [#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)). With
//! changes the disk lacks, it is written from the change number the next
//! save starts from: what that save would write. A file hosted read-only
//! because it holds content the writer cannot write is always the file on
//! disk, the bytes it was loaded from (section 2, rule 2); while changes
//! its refused saves left wait in memory, an export of the version held
//! is refused before anything is written ([`ExportError::Unsaved`]). A
//! backup asks for the file on disk whatever memory holds
//! ([`FileEngine::stored`]).
//!
//! **Checked.** A copy is checked to hold as many bytes as the file had
//! when it was opened, and the file not to have grown past them (one byte
//! more is read to see). It is not read back: that is what FileMaker
//! Server's Verify option of a backup schedule adds, which OpenMaker does
//! not have ([#158](https://github.com/OpenMaker-Dev/openmaker/issues/158)).
//! A written file is read back with the streaming reader
//! ([`om_hbam::stream::NodeStream`]) and
//! compared with a second walk of the view, node by node: path, kind (list
//! or value) and value, with a value over 1,000 bytes as the writer splits
//! it (hbam-writer.md 2.3), and page 0 as written. A difference, or a file
//! that does not read back, is an error.
//!
//! **Memory** for a copy is what `io::copy` takes, nothing where the kernel
//! copies; for a written file, what the writer holds (a few pages and
//! the open key path, hbam-writer.md 7.2), what the reader holds (a page
//! and the open path), one page of the view's walk at a time and the path
//! of the node being written. A value is borrowed from the view while it is
//! written and compared. Nothing grows with the file.
//!
//! **Where it goes.** An export to a folder is made there without its name
//! where the system can, else under its name, as copies were made before
//! exports, and checked there ([`Prepared`], `crate::staged`).
//! [`Prepared::publish`] flushes it and gives an unnamed file its name by a
//! link that fails when the name is taken, then flushes the folder; when
//! that flush fails the export stays complete under its name
//! ([`ExportError::NotFlushed`]). Nothing removes a name. An export can also
//! fill a file its caller made ([`Target::File`], whose contract is there),
//! from its first byte.

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

/// How many bytes a copy copies between two looks at its progress: the
/// part a backup's copy always took.
pub const COPY_PART: u64 = 8 << 20;

/// How often, in bytes written, the caller hears how far an export got.
const REPORT_EVERY: u64 = 8 << 20;

/// How the engine exports a version (the module doc).
#[derive(Debug)]
pub enum Source {
    /// Copied from the file on disk, which holds the version.
    OnDisk(OnDisk),
    /// Written from the view, page 0 the loaded one with the change number
    /// a writer starts from.
    Written { change_number: u64 },
}

/// A version of a file as the file on disk holds it, opened under the
/// store's read lock and read after it: the descriptor keeps naming that
/// file whatever later saves rename over its path.
#[derive(Debug)]
pub struct OnDisk {
    /// The file, opened to read.
    pub file: File,
    /// Its length when it was opened; a copy must be as long.
    pub len: u64,
    /// Its permissions, which a backup's copy gets.
    pub permissions: Permissions,
    /// On the file engine, how many saves wrote the file since it was
    /// loaded (`Store::saves`): an unchanged count means the same file.
    pub saves: u64,
    /// Whether the store held changes this file lacks when it was opened:
    /// changes a failed save, or a read-only file's refused saves, left in
    /// memory.
    pub unsaved: bool,
}

/// A folder an export is written into, by its descriptor
/// (`crate::staged`): a backup's folder, or a hosted file's kind of folder.
pub trait Folder {
    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::BorrowedFd<'_>;

    /// Whether the folder's file system makes hard links. False only for a
    /// test's folder that refuses them as such a file system would
    /// (`backups`); an export otherwise finds out when it links.
    fn links(&self) -> bool {
        true
    }
}

/// A hosted file's kind of folder ([`crate::anchored::Folder`]), opened by
/// its path.
impl Folder for crate::anchored::Folder {
    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::BorrowedFd<'_> {
        crate::anchored::Folder::descriptor(self)
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
    /// The file on disk could not be opened or read.
    Source(io::Error),
    /// The file on disk was `expected` bytes long when it was opened and
    /// `copied` were read from it: it changed while it was copied.
    SourceChanged { expected: u64, copied: u64 },
    /// The file on disk lacks changes memory holds that its saves were
    /// refused (a file hosted read-only for content the writer cannot
    /// write): an export of the version held would lack them.
    Unsaved,
    /// Creating, writing, flushing or naming the export failed.
    Io(io::Error),
    /// The export is complete under its name, and flushing its folder
    /// afterwards failed: whether the name survives a crash is not
    /// established. The file stays.
    NotFlushed {
        exported: Exported,
        error: io::Error,
    },
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
            ExportError::Source(e) => write!(f, "{e}"),
            ExportError::SourceChanged { expected, copied } => write!(
                f,
                "{copied} bytes copied of {expected}: the file changed while it was copied"
            ),
            ExportError::Io(e) => write!(f, "{e}"),
            ExportError::Unsaved => {
                write!(f, "the file on disk lacks changes that could not be saved")
            }
            ExportError::NotFlushed { error, .. } => write!(
                f,
                "the export is complete under its name, but flushing its folder failed: {error}"
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

/// Copy `source`, `len` bytes long when it was opened, from its start into
/// `target`, an empty file, and check that it was still that long (the
/// module doc, "Checked"). The copy goes from descriptor to descriptor in
/// parts of [`COPY_PART`] bytes, which the kernel copies where the system
/// can (`copy_file_range` on Linux), as backups always copied; `progress`
/// is asked whether to stop, and told, at each part. On an error `target`
/// holds part of a file, for the caller to discard.
pub fn copy_checked(
    source: &File,
    len: u64,
    target: &mut File,
    progress: Progress<'_>,
) -> Result<(), ExportError> {
    let mut from = source;
    from.seek(SeekFrom::Start(0)).map_err(ExportError::Source)?;
    // From the target's first byte, whatever its position: a write past it
    // would leave a hole of zeros before the copy.
    if target.metadata().map_err(ExportError::Io)?.len() != 0 {
        return Err(ExportError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the export's file is not empty",
        )));
    }
    target.seek(SeekFrom::Start(0)).map_err(ExportError::Io)?;
    // No more than the file held when it was opened, and one byte to see
    // whether it holds more now: a file another program writes to cannot
    // make the copy run on.
    let limit = len.saturating_add(1);
    let mut copied = 0u64;
    while copied < limit {
        if (progress.stop)() {
            return Err(ExportError::Stopped);
        }
        let part = (limit - copied).min(COPY_PART);
        let n = io::copy(&mut io::Read::take(from, part), target).map_err(ExportError::Io)?;
        if n == 0 {
            break;
        }
        copied += n;
        (progress.written)(n);
    }
    if copied != len {
        return Err(ExportError::SourceChanged {
            expected: len,
            copied,
        });
    }
    if target.metadata().map_err(ExportError::Io)?.len() != len {
        return Err(ExportError::Io(io::Error::other(
            "the copy is not as long as the bytes copied into it",
        )));
    }
    Ok(())
}

/// Write the version `read` reads from its view into `target`, an empty
/// file open for reading and writing, page 0 the loaded one with
/// `change_number`, and check it against the view (the module doc). On an
/// error `target` holds part of a file, for the caller to discard.
pub fn write_walked(
    read: Read<'_>,
    change_number: u64,
    target: &mut File,
    progress: Progress<'_>,
) -> Result<(), ExportError> {
    let mut counted = Counted {
        file: target,
        progress,
        unreported: 0,
    };
    let page_zero = hbam_writer::with_change_number(read.derived.page_zero(), change_number);
    write_view(read.view, &page_zero, &mut counted)?;
    counted.report();
    check(read.view, counted.file, &page_zero, change_number)
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

/// Read `file` back from its start and compare it with `view` (the module
/// doc, "Checked"): page 0 is `given` closed by the writer, its pages
/// numbered from change number `start`, and a long value is split as the
/// writer splits it.
fn check(view: &dyn View, file: &File, given: &[u8], start: u64) -> Result<(), ExportError> {
    let mut stream = NodeStream::new(file).map_err(ExportError::Unreadable)?;
    let differs = |what| ExportError::Differs {
        path: Vec::new(),
        what,
    };
    // Each page but page 0 took the next change number.
    let pages = u64::from(stream.page_count()).saturating_sub(1);
    let expected = start
        .checked_add(pages)
        .and_then(|last| hbam_writer::closed_page_zero(given, last));
    if expected.as_deref() != Some(stream.superblock().raw) {
        return Err(differs("page 0 is not the one written"));
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
        if !step.head.list && value.len() > MAX_INLINE_VALUE {
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

/// An export made for its name in its folder ([`prepare`]), not yet
/// published (`crate::staged`).
pub struct Prepared {
    staged: crate::staged::Staged,
    name: OsString,
}

/// A prepared export and how filling it went. On an error the caller drops
/// it, after any lock it holds: an unnamed file then vanishes, and a file
/// made under its name stays as written, as copies always did.
pub type Filled = (Prepared, Result<(), ExportError>);

/// Make the file for `to`'s name in its folder: unnamed where the system
/// can, else under the name, only where nothing is.
fn prepare(to: &Destination<'_>) -> Result<Prepared, ExportError> {
    #[cfg(unix)]
    let mut staged = crate::staged::Staged::new(to.folder.descriptor(), to.name).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            ExportError::Exists(to.name.to_owned())
        } else {
            ExportError::Io(e)
        }
    })?;
    #[cfg(not(unix))]
    let mut staged: crate::staged::Staged =
        return Err(ExportError::Io(crate::staged::Staged::unsupported()));
    if !to.folder.links() {
        staged.refuse_links();
    }
    Ok(Prepared {
        staged,
        name: to.name.to_owned(),
    })
}

impl Prepared {
    /// Fill the file with `fill` and give it `to`'s permissions.
    fn fill(
        mut self,
        to: &Destination<'_>,
        fill: impl FnOnce(&mut File) -> Result<(), ExportError>,
    ) -> Filled {
        let filled = fill(self.staged.file_mut()).and_then(|()| match &to.permissions {
            Some(permissions) => self
                .staged
                .file()
                .set_permissions(permissions.clone())
                .map_err(ExportError::Io),
            None => Ok(()),
        });
        (self, filled)
    }

    /// Flush the export and give it its name, never over an entry there,
    /// then flush the folder ([`ExportError::NotFlushed`] when that fails:
    /// the file stays).
    pub fn publish(self) -> Result<Exported, ExportError> {
        let Prepared { staged, name } = self;
        let (file, named) = staged.publish().map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExportError::Exists(name.clone())
            } else {
                ExportError::Io(e)
            }
        })?;
        let exported = exported(&file)?;
        match named {
            crate::staged::Named::Flushed => Ok(exported),
            crate::staged::Named::NotFlushed(error) => {
                Err(ExportError::NotFlushed { exported, error })
            }
        }
    }
}

/// Write and check the version `read` reads, from `change_number`, for
/// `to` (the module doc).
pub fn prepare_walked(
    read: Read<'_>,
    change_number: u64,
    to: &Destination<'_>,
    progress: Progress<'_>,
) -> Result<Filled, ExportError> {
    Ok(prepare(to)?.fill(to, |file| write_walked(read, change_number, file, progress)))
}

/// Copy and check `on_disk` for `to`.
pub fn prepare_copy(
    on_disk: &OnDisk,
    to: &Destination<'_>,
    progress: Progress<'_>,
) -> Result<Filled, ExportError> {
    Ok(prepare(to)?.fill(to, |file| {
        copy_checked(&on_disk.file, on_disk.len, file, progress)
    }))
}

/// Copy `on_disk` to `to`, checked, and publish it: what a backup does
/// with each file it does not link.
pub fn copy(
    on_disk: &OnDisk,
    to: &Destination<'_>,
    progress: Progress<'_>,
) -> Result<Exported, ExportError> {
    let (prepared, filled) = prepare_copy(on_disk, to, progress)?;
    filled?;
    prepared.publish()
}

/// Which file an export of the version a store holds writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The version as it is: the file on disk when it holds it, else the
    /// tree written ([`FileEngine::export`]).
    Current,
    /// The tree written whatever the disk holds, from the change number the
    /// next save starts from: a compacted copy (hbam-writer.md 9.3).
    Compacted,
}

/// Where an export goes: a new entry of a folder ([`Destination`],
/// `crate::staged`), or a file the caller made, which it then publishes
/// itself (the desktop's unnamed copy).
///
/// A caller's file must be empty, open for reading and writing at any
/// position, not for appending (refused before anything is written: a walk
/// writes its pages at offsets), and the caller's alone while the export
/// fills it: what another holder of the same file writes meanwhile cannot
/// be seen, and can be overwritten or make the export wrong. It is written
/// from its first byte and checked to be as long as the export.
pub enum Target<'t, 'd> {
    #[allow(
        dead_code,
        reason = "part of the engine's export (docs/server-storage.md 5); backups copy the \
                  stored version (`export::copy`), and an export of the current version to a \
                  folder is not offered yet (section 6.10)"
    )]
    Folder(&'t Destination<'d>),
    File(&'t mut File),
}

impl FileEngine {
    /// Export the version of `file` its store holds now, as `kind` says,
    /// to `target` (the module doc). A version on disk is opened under the
    /// store's read lock and copied after it is let go; a version written
    /// is written and checked under the lock, for as long as the walk takes.
    /// Flushing and renaming come after the lock. A server switched to the
    /// test view (`Server::checking_reads`) walks through it alone.
    pub fn export(
        &self,
        file: &Hosted,
        kind: Kind,
        target: Target<'_, '_>,
        progress: Progress<'_>,
    ) -> Result<Exported, ExportError> {
        let reading = self.read(file);
        let source = match kind {
            Kind::Current => reading.export_source().map_err(ExportError::Source)?,
            Kind::Compacted => reading.compacted_source(),
        };
        match (source, target) {
            // Refused before anything is written: the copy would lack what
            // memory holds.
            (Source::OnDisk(on_disk), _) if on_disk.unsaved => Err(ExportError::Unsaved),
            (_, Target::File(into)) if appends(into)? => Err(ExportError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the export's file is open for appending",
            ))),
            (Source::OnDisk(on_disk), Target::Folder(to)) => {
                drop(reading);
                copy(&on_disk, to, progress)
            }
            (Source::OnDisk(on_disk), Target::File(into)) => {
                drop(reading);
                copy_checked(&on_disk.file, on_disk.len, into, progress)?;
                exported(into)
            }
            (Source::Written { change_number }, Target::Folder(to)) => {
                let (prepared, filled) =
                    reading.serve_once(|read| prepare_walked(read, change_number, to, progress))?;
                // A file that failed is dropped after the lock.
                drop(reading);
                filled?;
                prepared.publish()
            }
            (Source::Written { change_number }, Target::File(into)) => {
                reading.serve_once(|read| write_walked(read, change_number, into, progress))?;
                drop(reading);
                exported(into)
            }
        }
    }

    /// The file on disk of `file`, the version its last save wrote or that
    /// it was loaded from, whatever memory holds: opened under the store's
    /// read lock for as long as an open takes, for a backup to copy after
    /// ([`copy`]).
    pub fn stored(&self, file: &Hosted) -> io::Result<OnDisk> {
        self.read(file).on_disk()
    }
}

/// Whether `file` is open for appending.
fn appends(file: &File) -> Result<bool, ExportError> {
    #[cfg(unix)]
    {
        let flags = rustix::fs::fcntl_getfl(file).map_err(|e| ExportError::Io(e.into()))?;
        Ok(flags.contains(rustix::fs::OFlags::APPEND))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(false)
    }
}

/// What a file holds once an export filled it.
fn exported(file: &File) -> Result<Exported, ExportError> {
    Ok(Exported {
        size: file.metadata().map_err(ExportError::Io)?.len(),
        id: file_identity(file).map_err(ExportError::Io)?,
    })
}

/// Through the test view too, which only debug builds have.
#[cfg(all(test, debug_assertions))]
mod tests {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, HbamFile, OwnedNode};
    use om_protocol::write::{GetSerial, SERIAL_INCREMENT};
    use om_scripts::text_node;

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

    /// Export the version `file` holds now to `name` in `folder`.
    fn export(
        engine: &FileEngine,
        file: &Hosted,
        kind: Kind,
        folder: &crate::anchored::Folder,
        name: &str,
        progress: Progress<'_>,
    ) -> Result<Exported, ExportError> {
        engine.export(file, kind, Target::Folder(&to(folder, name)), progress)
    }

    /// An engine whose walks go through the test view, which copies every
    /// node and gives three at a time, and fails the reads at or below
    /// `failing` in the fixture's file.
    fn engine(failing: Option<Vec<Vec<u8>>>) -> FileEngine {
        let engine = FileEngine::default();
        match failing {
            Some(path) => engine.fail_reads(fixture::FILE, path),
            None => engine.check_reads(),
        }
        engine
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

    /// op72 on the fixture's serial field: a write that changes the file
    /// and saves it.
    fn serial() -> Operation {
        Operation::AdvanceSerial(AdvanceSerial {
            request: GetSerial {
                table: vec![uint_segment(fixture::TABLE)],
                field: vec![uint_segment(fixture::COUNT)],
                value: String::new(),
                increment: 1,
                flags: SERIAL_INCREMENT,
            },
        })
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
        assert!(store.save(&file.at).unwrap());
    }

    /// Whether the version `file` holds is exported from the file on disk.
    fn on_disk(engine: &FileEngine, file: &Hosted) -> bool {
        matches!(
            engine.read(file).export_source().unwrap(),
            Source::OnDisk(_)
        )
    }

    /// An export of a hosted file is the file on disk for its version: as
    /// it was loaded, the bytes it was loaded from; once saved, what the
    /// save wrote; with changes the disk lacks, what the next save writes,
    /// written from the view (through the test view, which copies and
    /// pages) and checked against it. A caller's file gets the same bytes.
    #[test]
    fn an_export_is_the_file_on_disk_or_what_the_next_save_writes() {
        let (disk, file) = hosted("export-versions");
        let scratch = Scratch::new("versions");
        let folder = scratch.folder();
        let engine = engine(None);
        let export =
            |kind, name: &str| export(&engine, &file, kind, &folder, name, Progress::none());

        export(Kind::Current, "loaded.fmp12").unwrap();
        assert_eq!(
            scratch.read("loaded.fmp12"),
            std::fs::read(&disk.0).unwrap()
        );

        change(&file, "Renamed");
        export(Kind::Current, "unsaved.fmp12").unwrap();
        let mut into = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scratch.0.join("into.fmp12"))
            .unwrap();
        engine
            .export(
                &file,
                Kind::Current,
                Target::File(&mut into),
                Progress::none(),
            )
            .unwrap();
        save(&file);
        let saved = std::fs::read(&disk.0).unwrap();
        assert_eq!(scratch.read("unsaved.fmp12"), saved);
        assert_eq!(scratch.read("into.fmp12"), saved);
        assert_ne!(saved, scratch.read("loaded.fmp12"));

        let after = export(Kind::Current, "saved.fmp12").unwrap();
        assert_eq!(after.size, saved.len() as u64);
        assert_eq!(scratch.read("saved.fmp12"), saved);
        // A compacted copy is the tree written from the change number the
        // next save starts from.
        export(Kind::Compacted, "compacted.fmp12").unwrap();
        let compacted = HbamFile::open(scratch.0.join("compacted.fmp12")).unwrap();
        let read = HbamFile::open(&disk.0).unwrap();
        assert_eq!(
            compacted.key_tree().unwrap().root.to_owned_node(),
            read.key_tree().unwrap().root.to_owned_node()
        );

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
                "compacted.fmp12",
                "into.fmp12",
                "loaded.fmp12",
                "saved.fmp12",
                "unsaved.fmp12"
            ]
        );
    }

    /// The view is walked exactly when memory holds what the disk lacks,
    /// and for a compacted copy: with a read of the view failing, a version
    /// on disk is exported (copied, never read through the view), while a
    /// changed version and a compacted copy fail with the read's error and
    /// leave nothing in the folder.
    #[test]
    fn the_view_is_walked_exactly_when_memory_holds_what_the_disk_lacks() {
        let (_disk, file) = hosted("export-walked");
        let scratch = Scratch::new("walked");
        let folder = scratch.folder();
        let engine = engine(Some(record(20)));
        let export =
            |kind, name: &str| export(&engine, &file, kind, &folder, name, Progress::none());
        let failed = |result: Result<Exported, ExportError>| match result {
            Err(ExportError::Read(ReadError::Unreadable { path, .. })) => path == record(20),
            _ => false,
        };
        for saved in [false, true] {
            assert!(on_disk(&engine, &file), "saved {saved}");
            export(Kind::Current, "copied.fmp12").unwrap();
            assert!(failed(export(Kind::Compacted, "compacted.fmp12")));
            assert_eq!(scratch.names(), ["copied.fmp12"]);
            std::fs::remove_file(scratch.0.join("copied.fmp12")).unwrap();
            change(&file, "Changed");
            assert!(!on_disk(&engine, &file));
            assert!(failed(export(Kind::Current, "changed.fmp12")));
            assert!(scratch.names().is_empty(), "{:?}", scratch.names());
            save(&file);
        }
    }

    /// A write is not held up by an export of a version on disk: the store
    /// is let go once the file is opened, so a write taken only when no one
    /// holds the store (`FileEngine::try_submit`) runs, and saves, while
    /// the copy is under way, and the export holds the version before it,
    /// whole. An export that walks the view holds the store: the same write
    /// does not run, and the export holds the version without it.
    #[test]
    fn a_write_runs_while_a_version_on_disk_is_copied() {
        let (disk, file) = hosted("export-overlap");
        let scratch = Scratch::new("overlap");
        let folder = scratch.folder();
        let engine = engine(None);
        export(
            &engine,
            &file,
            Kind::Current,
            &folder,
            "before.fmp12",
            Progress::none(),
        )
        .unwrap();
        // Once the export writes, the write is tried, once.
        let tried: Cell<Option<bool>> = Cell::new(None);
        let stop = || {
            if tried.get().is_none() {
                tried.set(Some(engine.try_submit(&file, serial()).is_some()));
            }
            false
        };
        let progress = Progress {
            stop: &stop,
            written: &|_| {},
        };
        export(
            &engine,
            &file,
            Kind::Current,
            &folder,
            "during.fmp12",
            progress,
        )
        .unwrap();
        assert_eq!(tried.get(), Some(true), "the write waited for the copy");
        assert_eq!(scratch.read("during.fmp12"), scratch.read("before.fmp12"));
        export(
            &engine,
            &file,
            Kind::Current,
            &folder,
            "after.fmp12",
            Progress::none(),
        )
        .unwrap();
        let after = std::fs::read(&disk.0).unwrap();
        assert_ne!(after, scratch.read("before.fmp12"));
        assert_eq!(scratch.read("after.fmp12"), after);

        change(&file, "Changed");
        tried.set(None);
        let progress = Progress {
            stop: &stop,
            written: &|_| {},
        };
        export(
            &engine,
            &file,
            Kind::Current,
            &folder,
            "walked.fmp12",
            progress,
        )
        .unwrap();
        assert_eq!(tried.get(), Some(false), "the write ran during the walk");
        save(&file);
        assert_eq!(
            scratch.read("walked.fmp12"),
            std::fs::read(&disk.0).unwrap()
        );
    }

    /// A copy is checked against the file on disk: one that grew or shrank
    /// while it was copied fails the export and leaves nothing. A file that
    /// keeps growing cannot make the copy run on.
    #[test]
    fn a_copy_is_checked_against_the_file_on_disk() {
        let (disk, file) = hosted("export-copy-check");
        let scratch = Scratch::new("copy-check");
        let folder = scratch.folder();
        let engine = engine(None);
        for (name, grow) in [("grown.fmp12", true), ("short.fmp12", false)] {
            let len = std::fs::metadata(&disk.0).unwrap().len();
            // Before each part is copied: grown by a page every time, or
            // cut short by one once.
            let stop = || {
                let source = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&disk.0)
                    .unwrap();
                let now = source.metadata().unwrap().len();
                if grow {
                    source.set_len(now + 4096).unwrap();
                } else if now == len {
                    source.set_len(len - 4096).unwrap();
                }
                false
            };
            let progress = Progress {
                stop: &stop,
                written: &|_| {},
            };
            let changed = export(&engine, &file, Kind::Current, &folder, name, progress);
            assert!(
                matches!(changed, Err(ExportError::SourceChanged { .. })),
                "{name}: {changed:?}"
            );
            assert!(scratch.names().is_empty());
            // The file on disk as the tree holds it again.
            save_fresh(&file);
        }
    }

    /// Make the file on disk the tree's again: a change, then a save.
    fn save_fresh(file: &Hosted) {
        change(file, "Fresh");
        save(file);
    }

    /// An export never removes or replaces an entry it did not make: one at
    /// the final name fails it and is left as it was, and nothing else is
    /// left; so does a stop. (Each way of naming a file: `crate::staged`'s
    /// tests.)
    #[test]
    fn an_export_leaves_other_entries_alone() {
        let (_disk, file) = hosted("export-entries");
        let scratch = Scratch::new("entries");
        let folder = scratch.folder();
        let engine = engine(None);
        std::fs::write(scratch.0.join("there.fmp12"), b"there").unwrap();
        for kind in [Kind::Current, Kind::Compacted] {
            assert!(matches!(
                export(
                    &engine,
                    &file,
                    kind,
                    &folder,
                    "there.fmp12",
                    Progress::none()
                ),
                Err(ExportError::Exists(_))
            ));
            assert_eq!(scratch.names(), ["there.fmp12"]);
            assert_eq!(scratch.read("there.fmp12"), b"there");
        }
        // A stop asked for while it writes, copying and walking.
        let progress = Progress {
            stop: &|| true,
            written: &|_| {},
        };
        for kind in [Kind::Current, Kind::Compacted] {
            assert!(matches!(
                export(&engine, &file, kind, &folder, "stopped.fmp12", progress),
                Err(ExportError::Stopped)
            ));
        }
        assert_eq!(scratch.names(), ["there.fmp12"]);
    }

    /// A caller's empty file is written from its first byte whatever its
    /// position, copied and walked: no hole of zeros before the export, and
    /// as long as the file.
    #[test]
    fn a_callers_file_is_written_from_its_first_byte() {
        use std::io::Seek as _;
        let (disk, file) = hosted("export-position");
        let scratch = Scratch::new("position");
        let engine = engine(None);
        for (kind, name) in [(Kind::Current, "copied"), (Kind::Compacted, "walked")] {
            let mut into = std::fs::File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(scratch.0.join(name))
                .unwrap();
            into.seek(SeekFrom::Start(4096)).unwrap();
            let exported = engine
                .export(&file, kind, Target::File(&mut into), Progress::none())
                .unwrap();
            let written = scratch.read(name);
            assert_eq!(exported.size, written.len() as u64, "{name}");
            if kind == Kind::Current {
                assert_eq!(written, std::fs::read(&disk.0).unwrap());
            } else {
                assert!(HbamFile::from_bytes(written).is_ok(), "{name}");
            }
        }
    }

    /// A caller's file open for appending is refused before anything is
    /// written, copied or walked: a walk writes its pages at offsets.
    #[test]
    fn a_callers_file_open_for_appending_is_refused() {
        let (_disk, file) = hosted("export-append");
        let scratch = Scratch::new("append");
        let engine = engine(None);
        for kind in [Kind::Current, Kind::Compacted] {
            let mut into = std::fs::File::options()
                .read(true)
                .append(true)
                .create(true)
                .truncate(false)
                .open(scratch.0.join("appended"))
                .unwrap();
            let refused = engine.export(&file, kind, Target::File(&mut into), Progress::none());
            assert!(
                matches!(&refused, Err(ExportError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput),
                "{kind:?}: {refused:?}"
            );
            assert_eq!(into.metadata().unwrap().len(), 0);
        }
    }

    /// An export of a version the file on disk lacks changes of, which its
    /// refused saves left in memory, is refused before anything is
    /// written: the desktop's copy of such a file gets the refusal, and its
    /// file stays empty.
    #[test]
    fn a_copy_that_would_lack_changes_is_refused_before_it_writes() {
        let (_disk, file) = hosted("export-refused");
        let scratch = Scratch::new("refused");
        let folder = scratch.folder();
        let engine = engine(None);
        file.write_store(&Writer::for_tests())
            .set_unwritable_for_tests();
        change(&file, "Unsaved");
        let mut into = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scratch.0.join("into.fmp12"))
            .unwrap();
        let progress = Progress {
            stop: &|| panic!("written"),
            written: &|_| panic!("written"),
        };
        let refused = engine.export(&file, Kind::Current, Target::File(&mut into), progress);
        assert!(matches!(refused, Err(ExportError::Unsaved)), "{refused:?}");
        assert_eq!(into.metadata().unwrap().len(), 0);
        let refused = export(&engine, &file, Kind::Current, &folder, "f.fmp12", progress);
        assert!(matches!(refused, Err(ExportError::Unsaved)), "{refused:?}");
        assert_eq!(scratch.names(), ["into.fmp12"]);
        // A backup still copies the file on disk, saying what it lacks.
        let stored = engine.stored(&file).unwrap();
        assert!(stored.unsaved);
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
        check(&view, &file, &page_zero, 0).unwrap();
        let task = record(3);
        let task: Vec<&[u8]> = task.iter().map(Vec::as_slice).collect();
        type Vary<'a> = Box<dyn Fn(&mut OwnedNode) + 'a>;
        let variants: Vec<(&str, Vary)> = vec![
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
                    check(&other_view, &file, &page_zero, 0),
                    Err(ExportError::Differs { .. })
                ),
                "{what}"
            );
        }
        let mut other_page = page_zero.clone();
        other_page[0x100] ^= 1;
        assert!(matches!(
            check(&view, &file, &other_page, 0),
            Err(ExportError::Differs { .. })
        ));
        assert!(matches!(
            check(&view, &file, &page_zero, 1),
            Err(ExportError::Differs { .. })
        ));
    }
}

/// The same against FileMaker's own files: the 66 Mac templates
/// (git-ignored extractions, `re/extracted`). Each is exported as it was
/// loaded (its own bytes, copied), then with a change the disk lacks
/// (written from the view, which must be what the save then writes), then
/// as saved (the saved file, copied).
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

    /// Export `store`'s version to `name` in `folder`, as the store says;
    /// its bytes, and whether it was written from the view.
    fn export(
        store: &Store,
        at: &FileAt,
        folder: &crate::anchored::Folder,
        name: &str,
    ) -> (Vec<u8>, bool) {
        let to = Destination {
            folder,
            name: OsStr::new(name),
            permissions: None,
        };
        let walked = match store.export_source(at).unwrap() {
            Source::OnDisk(on_disk) => {
                copy(&on_disk, &to, Progress::none()).unwrap();
                false
            }
            Source::Written { change_number } => {
                let view = store.tree_view();
                let read = Read {
                    view: &view,
                    derived: store.derived(),
                    waiting: store.waiting(),
                    read_only: store.is_read_only(),
                };
                let (prepared, filled) =
                    prepare_walked(read, change_number, &to, Progress::none()).unwrap();
                filled.unwrap();
                prepared.publish().unwrap();
                true
            }
        };
        let path = at.place().with_file_name(name);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        (bytes, walked)
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
            let loaded = export(&store, &at, &folder, "loaded.fmp12");
            assert_eq!(loaded, (original, false), "{shown}");
            if store.is_read_only() {
                read_only += 1;
            } else {
                assert!(store.put_subtree(
                    &[vec![2], vec![0x7e]],
                    OwnedNode::leaf(Vec::new(), b"exported".to_vec())
                ));
                let (unsaved, walked) = export(&store, &at, &folder, "unsaved.fmp12");
                assert!(walked, "{shown}");
                assert!(store.save(&at).unwrap());
                let saved = std::fs::read(&path).unwrap();
                assert_eq!(unsaved, saved, "{shown}");
                let after = export(&store, &at, &folder, "saved.fmp12");
                assert_eq!(after, (saved, false), "{shown}");
                written += 1;
            }
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::remove_dir_all(&scratch).unwrap();
        println!(
            "{} files: {written} copied as loaded, written unsaved and copied saved, {read_only} \
             read-only copied as loaded",
            found.len()
        );
    }
}

/// A measurement, not a test: how long each whole-file copy holds the
/// store's read lock, its time and its peak memory, for a file a save
/// wrote. One step per process, so that each peak is its own: `generate`
/// writes a file of `OM_EXPORT_RECORDS` records (one table, eight 64-byte
/// text cells each, as #673 measured) to `OM_EXPORT_FILE`; every other
/// step loads it, changes one node and saves it, so that the disk holds the
/// version, then makes one copy of it beside it, reporting the time under
/// the lock, the total and the peak resident memory above the loaded store
/// (`VmHWM`, reset through `clear_refs` once the store is saved). For a
/// release build:
///
/// ```sh
/// OM_EXPORT_RECORDS=200000 OM_EXPORT_FILE=target/m/f.fmp12 OM_EXPORT_STEP=generate \
///   cargo test --release -p om-server --lib -- --ignored export_measurement --nocapture
/// ```
///
/// The steps: `walk`, the tree written and checked under the lock, then
/// flushed and renamed (at 0b36912 a backup and a desktop Copy of a saved
/// version; a compacted copy then and now); `copy`, the file opened under
/// the lock and copied and checked after it (a backup and a desktop Copy
/// now); and the code before exports, re-enacted with the calls it made:
/// `backup-before` (the file opened under the lock, then copied with
/// `io::copy` in 8 MB parts, its permissions set and flushed),
/// `copy-before` (the locked descriptor cloned under the lock, then read by
/// position into the copy and flushed) and `compacted-before` (the whole
/// file rebuilt in memory under the lock, then written and flushed).
#[cfg(all(test, target_os = "linux"))]
mod measurement {
    use std::io::Read as _;
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, HbamFile, OwnedNode};

    use super::*;
    use crate::anchored::FileAt;
    use crate::scripts::fixture;
    use crate::store::Store;

    fn var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }

    /// The field `name` of this process's status, in MB.
    fn status(name: &str) -> f64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<f64>().ok())
            .unwrap()
            / 1024.0
    }

    fn generate(path: &std::path::Path) {
        let records: u64 = var("OM_EXPORT_RECORDS").parse().unwrap();
        let mut tree = fixture::tree(&[], true);
        let cell = |field: u64, record: u64| {
            let text: String = format!("{record:012}-{field:02}-")
                .chars()
                .cycle()
                .take(64)
                .collect();
            om_scripts::text_node(&uint_segment(field), &text)
        };
        let list = (1..=records)
            .map(|id| OwnedNode::list(serial::encode(id), (1..=8).map(|f| cell(f, id)).collect()))
            .collect();
        tree.put(
            &[&uint_segment(fixture::TABLE), &[0x05]],
            OwnedNode::list(vec![0x05], list),
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fixture::write_tree(path, &tree);
        println!(
            "generated {records} records: {:.1} MB",
            std::fs::metadata(path).unwrap().len() as f64 / 1e6
        );
    }

    /// A new file `path`, for reading and writing.
    fn create(path: &std::path::Path) -> File {
        File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap()
    }

    #[test]
    #[ignore = "a measurement for release builds; set OM_EXPORT_STEP"]
    fn export_measurement() {
        let path = PathBuf::from(var("OM_EXPORT_FILE"));
        let step = var("OM_EXPORT_STEP");
        if step == "generate" {
            return generate(&path);
        }
        let file = HbamFile::open(&path).unwrap();
        let mut store = Store::load(&file, "Measured.fmp12").unwrap();
        drop(file);
        {
            use std::os::unix::fs::MetadataExt;
            let m = std::fs::metadata(&path).unwrap();
            store.set_file(Some((m.dev(), m.ino())));
        }
        assert!(store.put_subtree(
            &[vec![2], vec![0x7e]],
            OwnedNode::leaf(Vec::new(), b"measured".to_vec())
        ));
        let at = FileAt::open(&path).unwrap();
        assert!(store.save(&at).unwrap());
        let folder = at.folder_now().unwrap();
        let out = path.with_file_name("Measured-copy.fmp12");
        let name = out.file_name().unwrap();
        let to = Destination {
            folder: &folder,
            name,
            permissions: None,
        };
        std::fs::write("/proc/self/clear_refs", "5").unwrap();
        let base = status("VmRSS:");
        let size = std::fs::metadata(&path).unwrap().len() as f64 / 1e6;
        let mut started = Instant::now();
        let held: Duration;
        match step.as_str() {
            "walk" => {
                let Source::Written { change_number } = store.compacted_source() else {
                    unreachable!()
                };
                let view = store.tree_view();
                let read = Read {
                    view: &view,
                    derived: store.derived(),
                    waiting: store.waiting(),
                    read_only: store.is_read_only(),
                };
                let (prepared, filled) =
                    prepare_walked(read, change_number, &to, Progress::none()).unwrap();
                held = started.elapsed();
                filled.unwrap();
                prepared.publish().unwrap();
            }
            "copy" => {
                let Source::OnDisk(on_disk) = store.export_source(&at).unwrap() else {
                    panic!("the saved version is not on disk")
                };
                held = started.elapsed();
                copy(&on_disk, &to, Progress::none()).unwrap();
            }
            "backup-before" => {
                let source = store.open_saved(&at).unwrap();
                let metadata = source.metadata().unwrap();
                held = started.elapsed();
                let mut copy = create(&out);
                loop {
                    let n = io::copy(&mut (&source).take(8 << 20), &mut copy).unwrap();
                    if n == 0 {
                        break;
                    }
                }
                copy.set_permissions(metadata.permissions()).unwrap();
                copy.sync_all().unwrap();
            }
            "copy-before" => {
                // The store's locked descriptor, as `Store::saved_file`
                // cloned it under the lock.
                let locked = store.open_saved(&at).unwrap();
                started = Instant::now();
                let clone = locked.try_clone().unwrap();
                held = started.elapsed();
                struct FromStart<'a>(&'a File, u64);
                impl io::Read for FromStart<'_> {
                    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                        let n = self.0.read_at(buf, self.1)?;
                        self.1 += n as u64;
                        Ok(n)
                    }
                }
                let mut copy = create(&out);
                io::copy(&mut FromStart(&clone, 0), &mut copy).unwrap();
                copy.sync_all().unwrap();
            }
            "compacted-before" => {
                let root = store.node(&[]).unwrap();
                let page_zero = store.derived().page_zero().to_vec();
                let bytes = om_hbam::writer::rebuild(root, &page_zero).unwrap();
                held = started.elapsed();
                let mut copy = create(&out);
                copy.write_all(&bytes).unwrap();
                drop(bytes);
                copy.sync_all().unwrap();
            }
            other => panic!("unknown step {other}"),
        }
        let total = started.elapsed();
        println!(
            "{step} {size:.1} MB file: {:.3} ms under the read lock, {:.0} ms in all, peak {:.1} \
             MB above the loaded store ({base:.0} MB)",
            held.as_secs_f64() * 1e3,
            total.as_secs_f64() * 1e3,
            status("VmHWM:") - base,
        );
        std::fs::remove_file(&out).unwrap();
    }
}
