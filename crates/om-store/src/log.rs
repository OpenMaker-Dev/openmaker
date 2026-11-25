//! The change log (`docs/server-storage.md` section 6.3): entries in
//! segment files under `log/`, written in batches, each batch made durable
//! by a synchronous write and then recorded by the marker in `state`. The
//! byte formats and the order of every write are section 3 of
//! `docs/server-storage-format.md`.
//!
//! - **Entries** have an index counted from 1, a term, a kind and the file
//!   they belong to (two numbers this layer stores and does not interpret),
//!   an opaque payload of at most [`Config::max_payload`] bytes, and a
//!   SHA-256 hash over the previous entry's hash and their own bytes.
//! - **Segments** are files of [`Config::segment_size`] bytes, their space
//!   taken when they are made. The next one is made ahead of need
//!   ([`Log::prepare`]); a segment is listed in `state`, and so used, only
//!   once its header is on the device, and its name was made durable when
//!   it was made. Entries never span segments; a batch that does not fit in
//!   what is left of one is written as two batches, the second at the start
//!   of the next segment.
//! - **Batches**: [`Log::append`] adds an entry to the open batch in memory;
//!   [`Log::flush_batch`] writes the batch with synchronous writes, then
//!   records the marker (the last entry's index and hash, and the committed
//!   position the caller gives, which never passes the log's own entries)
//!   in `state`. An entry is durable when the marker covers it.
//! - **Opening** ([`Log::open`]) checks the log against the marker:
//!   everything through it must be there, whole and chained, else the log
//!   is damaged ([`LogError::Damaged`]) and nothing is changed. Whole
//!   batches after the marker that carry the current cut count are kept and
//!   the marker moved over them; the rest is cut.
//! - **Cutting** on a leader's instruction ([`Log::cut`]) lowers the marker
//!   and raises the cut count in one `state` update, written to both slots:
//!   the cut is done when that update is. Every batch carries the cut count
//!   it was written under, so nothing written before a cut is read after
//!   it. A cut needs a committed position the caller supplied since the log
//!   was opened.
//! - **An unreadable end**: an open that cannot read a place after the
//!   marker ends the log there for that open and takes no appends. A cut
//!   from the entry after the durable one, or giving it up
//!   ([`Log::give_up_unreadable_end`], the administrator's "accept the loss"
//!   on a single server), discards whatever may follow it for good and lists
//!   a fresh segment that continues the log from its last entry, so that
//!   walks move to it there. The place itself is read again only if a later
//!   cut takes the log back into its segment and the log grows over it.
//!
//! The caller drives everything; nothing here runs on its own. After a
//! write, flush or `state` update fails, the log refuses every later call
//! ([`LogError::Poisoned`]): what reached the device is unknown, and the
//! store stops and opens again (section 6.12). So does a `state` update
//! that `state` refuses after the call wrote to a segment; one refused
//! before anything was written stops nothing. A full disk refuses an
//! append that would need a new segment before anything is written, and so
//! does a segment number `state` could not list.
//!
//! The log owns the store's [`State`], and with it the store folder's lock,
//! for as long as it lives: one log per store, by construction. It reaches
//! the disk only through [`crate::sys`]. What else `state` holds (the term
//! and vote, the repair record) is read and changed through the log
//! ([`Log::state`], [`Log::update_state`]).

mod format;
mod scan;
#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;

use crate::state::{Marker, Segments, State, StateError, Values};
use crate::sys::{self, ErrorKind, File, FileOptions, Fs};

use format::{encode_entry, segment_number, segment_path, BatchHeader, SegmentHeader};
pub use format::{
    HeaderFault, BATCH_HEADER, ENTRY_OVERHEAD, MAX_INDEX, MAX_PAYLOAD, MAX_SEGMENT, MIN_SEGMENT,
    SEGMENT_HEADER, VERSION, ZERO_HASH,
};
use scan::{read_exact, Event, Found, Judge, Keep, ReadFault, Scanner, Seg};

/// The folder of the log's segments in the store folder.
pub const FOLDER: &str = "log";

/// The log's sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The size of a new segment in bytes, its header included. Segments
    /// made earlier keep the size their header records.
    pub segment_size: u64,
    /// The largest payload an append takes, at most [`MAX_PAYLOAD`]. An
    /// entry never spans segments, so an entry with the largest payload must
    /// fit in an empty segment; larger work is a group (section 6.9).
    /// Entries written under a larger setting stay readable.
    pub max_payload: u32,
}

impl Config {
    /// 64 MiB segments, payloads of up to 16 MiB.
    pub const DEFAULT: Config = Config {
        segment_size: 64 << 20,
        max_payload: 16 << 20,
    };

    /// Whether the configuration is within the format's bounds: a segment
    /// size within them, and an entry with the largest payload fitting in
    /// a segment.
    pub fn check(&self) -> Result<(), LogError> {
        let room = self
            .segment_size
            .saturating_sub(SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD);
        if !(MIN_SEGMENT..=MAX_SEGMENT).contains(&self.segment_size) {
            return Err(LogError::Refused("the segment size is out of bounds"));
        }
        if self.max_payload > MAX_PAYLOAD || u64::from(self.max_payload) > room {
            return Err(LogError::Refused(
                "an entry with the largest payload does not fit in a segment",
            ));
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// An entry's place in the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub index: u64,
    pub term: u64,
    pub hash: [u8; 32],
}

impl Position {
    /// Before the first entry.
    pub const START: Position = Position {
        index: 0,
        term: 0,
        hash: ZERO_HASH,
    };
}

/// An entry read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: u64,
    pub term: u64,
    pub kind: u16,
    pub file: u64,
    pub payload: Vec<u8>,
    pub hash: [u8; 32],
}

impl Entry {
    pub fn position(&self) -> Position {
        Position {
            index: self.index,
            term: self.term,
            hash: self.hash,
        }
    }
}

/// What an open found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The last durable entry: the marker, moved over whatever whole
    /// batches followed it.
    pub durable: Position,
    /// The committed position the marker recorded (see
    /// [`Log::set_committed`] for what a caller gives). It is not
    /// confirmed: a lost or unreadable copy of `state` can have taken it
    /// back one advance (format section 1.5), so [`Log::cut`] refuses until
    /// the caller supplies a committed position again ([`Log::flush_batch`],
    /// [`Log::set_committed`]).
    pub recorded_committed: u64,
    /// The cut count `state` holds: the batches after the marker that carry
    /// it are the only ones kept.
    pub cut_count: u64,
    /// How many entries after the marker were found whole and kept.
    pub kept: u64,
    /// What followed the durable entry and was cut, if anything did.
    pub cut: Option<Cut>,
    /// The log ends, for this open, at a place after the durable entry the
    /// device failed to read. It may continue past it: nothing there was
    /// cut, dropped or written over, and the log refuses appends
    /// ([`LogError::EndUnreadable`]) until it is opened again, cut, or
    /// what may follow is given up ([`Log::give_up_unreadable_end`]).
    pub unreadable_end: Option<UnreadableEnd>,
    /// The numbers of segment files no longer listed that the open
    /// removed: segments retired or cut by a process that stopped before
    /// removing them, and spares.
    pub removed: Vec<u64>,
}

/// Where an open cut the log (section 6.12: "cut from the first bad entry,
/// and log the cut").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    /// The first index no longer in the log.
    pub index: u64,
    /// The segment and offset the log now ends at.
    pub segment: u64,
    pub offset: u64,
    /// Listed segments after it that were dropped.
    pub dropped: u64,
    pub why: CutWhy,
}

/// Why an open cut the log after its durable entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutWhy {
    /// A batch was not whole: a write cut short, or damage.
    NotWhole,
    /// A batch was written before the last cut.
    OlderCut,
    /// Only listed segments past the end were dropped.
    Segments,
}

/// What accepting the loss after damage through the marker gave up
/// ([`Log::accept_damage`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DamageAccepted {
    /// The last entry kept: chained from the start, through every segment
    /// that still reads, up to the later of the marker and the start. When
    /// it is that entry, no confirmed entry was given up.
    pub kept: Position,
    /// The marker before: the entries after `kept` through it had been
    /// confirmed, and are given up.
    pub marker: u64,
    /// The cut count `state` recorded, raised by one, when anything after
    /// `kept` was given up (the marker lowered, or listed segments after
    /// the kept ones dropped): what was written after `kept` before the
    /// call, confirmed or not, is never part of the log again. `None` when
    /// nothing after `kept` was given up: what follows the marker is judged
    /// by the open as any.
    pub cut_count: Option<u64>,
    /// The segments `state` listed before the call, and lists now.
    pub listed_before: Segments,
    pub listed: Segments,
    /// Every listed segment no longer listed, with the entries it held as
    /// far as the headers say, and where its file went. Those numbered
    /// before `listed.first` held only entries before the start's
    /// successor: what a checkpoint older than the start loses.
    pub given_up: Vec<SegmentGivenUp>,
    /// The kept segment whose bytes after `kept` are given up (when the cut
    /// count was raised): copied, since it stays in use, to
    /// `<prefix><number>.tail`; the places of it the device failed to
    /// read are zeros there, listed in `tail_unreadable` (offset, length).
    pub tail_copied: Option<u64>,
    pub tail_unreadable: Vec<(u64, u64)>,
    /// Kept segments whose files were shorter than their headers say,
    /// lengthened to that size with zeros (format section 3.2).
    pub lengthened: Vec<u64>,
    /// The names this acceptance gave what it gave up, in `log/`: a file
    /// given up is renamed `<prefix><number>`, a name no open removes and
    /// no other acceptance uses; `None` when nothing was moved or copied.
    pub prefix: Option<String>,
}

/// A listed segment an acceptance gave up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentGivenUp {
    pub number: u64,
    /// The first entry it held, from its own header, and its last, from the
    /// next listed segment's header: `None` where that header is missing
    /// or not valid (unknown).
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// The path its file was renamed to; `None` when there was no file.
    pub moved_to: Option<String>,
}

/// What [`Log::accept_damage`] decided, before anything is written.
struct AcceptPlan {
    accepted: DamageAccepted,
    values: Values,
    /// The segments whose files are renamed first.
    moves: Vec<u64>,
    /// Kept segments to lengthen, and to what size.
    lengthen: Vec<(u64, u64)>,
}

/// The most segments an acceptance looks at: far more than a log keeps.
const MAX_RUN: u64 = 1 << 20;

/// The start of the names an acceptance gives what it gives up, in
/// `log/`: `given-up-<k>.` (the next `k` free), followed by the segment's
/// number. No open removes them: it removes only files named as segments.
pub const GIVEN_UP_PREFIX: &str = "given-up-";

/// Whether `state` holds a decision a call made before it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// `state` holds it: it stands, whatever failed after it.
    Done,
    /// The update of `state` that records it failed part way: it may stand
    /// or not. The next open shows which: it ends the log where the
    /// decision does, or finds what the call was given again.
    Unknown,
}

/// [`Log::accept_damage`] failed: why, the `state` (as [`OpenFailed`]),
/// and what the call gave up, when it got as far as recording it.
#[derive(Debug)]
pub struct AcceptFailed {
    pub failed: Box<OpenFailed>,
    pub decided: Option<(DamageAccepted, Recorded)>,
}

/// A place after the durable entry the device failed to read, where an
/// open ended the log: the segment, the offset of the read that failed, and
/// the index the log expected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreadableEnd {
    pub segment: u64,
    pub offset: u64,
    pub index: u64,
}

/// What [`Log::cut`] or [`Log::give_up_unreadable_end`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutDone {
    /// The last entry left: appends continue after it.
    pub last: Position,
    /// Entries of the open batch dropped, in memory.
    pub unflushed: u64,
    /// Durable entries cut: the marker was lowered over them.
    pub durable: u64,
    /// The cut count `state` recorded, when the cut was recorded there:
    /// durable entries were cut, or what may follow an unreadable end was
    /// given up. `None` when nothing was written.
    pub cut_count: Option<u64>,
    /// The log had ended, since it was opened, at this place the device
    /// failed to read: whatever may follow it is no longer part of the log,
    /// and appends are taken again.
    pub unreadable_end: Option<UnreadableEnd>,
    /// The segment `state` now lists after the one the log ended in, whose
    /// header continues the log from `last`: walks move to it from `last`
    /// and read nothing further of the segment the log ended in, until a
    /// later cut of durable entries drops it (format section 3.9).
    pub fence: Option<u64>,
    /// The numbers of the segment files removed.
    pub removed: Vec<u64>,
}

impl CutDone {
    /// A cut that found nothing at or after its index: nothing changed.
    pub fn nothing(last: Position) -> Self {
        CutDone {
            last,
            unflushed: 0,
            durable: 0,
            cut_count: None,
            unreadable_end: None,
            fence: None,
            removed: Vec::new(),
        }
    }
}

/// What is wrong with a damaged log (section 6.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    /// A listed segment's file is missing.
    Missing,
    /// A listed segment's header is not valid.
    Header(HeaderFault),
    /// A listed segment's header belongs to another store.
    OtherStore,
    /// A listed segment's header carries another number than its name.
    Renamed,
    /// A listed segment's file is shorter than its header says.
    Short,
    /// The listed segments' first indexes go back.
    Order,
    /// The log does not reach back to the entry it is read from.
    Gap,
    /// The log ends before an entry it must hold: the marker's, or one
    /// before it, or one a read asked for.
    EndsEarly,
    /// The entry at the marker's index has another hash.
    Marker,
    /// The entry the start names has another hash.
    Start,
}

/// Damage, and where it is: the segment's number, the offset in it and the
/// index of the entry expected there (0 when it is not about an entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    pub kind: DamageKind,
    pub segment: u64,
    pub offset: u64,
    pub index: u64,
}

/// Why the log refused a call or failed.
#[derive(Debug)]
pub enum LogError {
    /// The log cannot produce what it confirmed (section 6.12). Nothing was
    /// changed; what to do is the caller's decision: in repair on a cluster,
    /// stop on a single server.
    Damaged(Damage),
    /// The device failed to read the log at a place.
    Unreadable {
        segment: u64,
        offset: u64,
        error: sys::Error,
    },
    /// A segment is of a format newer than this code reads.
    NewerFormat { segment: u64, version: u16 },
    /// The disk failed. After a failed write or flush the log is poisoned.
    Disk(sys::Error),
    /// `state` failed or refused. A failure stops the log, and so does a
    /// refusal after the call wrote to a segment (a batch flush, giving up);
    /// a refusal of a call that wrote nothing first does not.
    State(StateError),
    /// The disk is too full for the next segment: the append was refused
    /// and nothing was written.
    NoSpace(sys::Error),
    /// The open batch cannot take this entry without spanning more than
    /// two segments: flush the batch first.
    BatchFull,
    /// The payload is larger than [`Config::max_payload`].
    TooLarge { len: usize, max: u32 },
    /// An earlier failure stopped the log: open it again.
    Poisoned,
    /// The log ends, for this open, at a place the device failed to read
    /// ([`Opened::unreadable_end`]): an append would write over what may
    /// continue the log. Open it again.
    EndUnreadable(UnreadableEnd),
    /// A cut was asked for before the caller supplied a committed position
    /// since the log was opened; nothing was changed.
    CommittedUnconfirmed,
    /// The call breaks a rule; nothing was written.
    Refused(&'static str),
    /// An acceptance of a loss was refused, with nothing changed: the
    /// framing before the start's entry broke at `offset` of `segment`,
    /// the segment that holds the start's successor, so that where that
    /// begins cannot be established.
    FramingBroken { segment: u64, offset: u64 },
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogError::Damaged(d) => write!(
                f,
                "the log is damaged ({:?} in segment {} at offset {}, entry {}): it cannot \
                 produce everything it confirmed",
                d.kind, d.segment, d.offset, d.index
            ),
            LogError::Unreadable {
                segment,
                offset,
                error,
            } => write!(
                f,
                "the log could not be read in segment {segment} at offset {offset}: {error}"
            ),
            LogError::NewerFormat { segment, version } => write!(
                f,
                "log segment {segment} has format version {version}, newer than this server \
                 reads ({VERSION})"
            ),
            LogError::Disk(e) => write!(f, "{e}"),
            LogError::State(e) => write!(f, "{e}"),
            LogError::NoSpace(e) => write!(f, "the log has no room for its next segment: {e}"),
            LogError::BatchFull => f.write_str("the open batch is full; flush it first"),
            LogError::TooLarge { len, max } => {
                write!(
                    f,
                    "an entry of {len} bytes is larger than the log takes ({max})"
                )
            }
            LogError::Poisoned => f.write_str(
                "an earlier write or flush of the log failed; the store must stop and be opened \
                 again",
            ),
            LogError::EndUnreadable(end) => write!(
                f,
                "the log refuses appends: it ends at a place the device failed to read \
                 (segment {}, offset {}, entry {}), past which it may continue; open it again, \
                 or give up what may follow it",
                end.segment, end.offset, end.index
            ),
            LogError::CommittedUnconfirmed => f.write_str(
                "log refused a cut: no committed position has been supplied since the log was \
                 opened, and the one recorded may be behind",
            ),
            LogError::Refused(why) => write!(f, "log refused: {why}"),
            LogError::FramingBroken { segment, offset } => write!(
                f,
                "the log's framing before the checkpoint's entry is broken in segment \
                 {segment} at offset {offset}: where the entry after it begins cannot be \
                 established, and nothing was given up"
            ),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LogError::Unreadable { error, .. }
            | LogError::Disk(error)
            | LogError::NoSpace(error) => Some(error),
            LogError::State(e) => Some(e),
            _ => None,
        }
    }
}

/// [`Log::open`] failed: why, and the `state` it was given, so that the
/// caller can still record what it decides (in repair, say), or try the
/// open again in the same process. A `state` whose own write failed
/// refuses every update until it is opened again.
#[derive(Debug)]
pub struct OpenFailed {
    pub error: LogError,
    pub state: State,
}

impl fmt::Display for OpenFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for OpenFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// An entry of the open batch.
struct Pending {
    bytes: Vec<u8>,
    position: Position,
    /// It goes to the next segment.
    next_segment: bool,
}

/// The open log (module docs).
pub struct Log {
    fs: Arc<dyn Fs>,
    /// The store's `state`, which holds the store folder's lock. Its
    /// segment run, marker and cut count change only through the log.
    state: State,
    config: Config,
    /// The listed segments, in order.
    segs: Vec<Seg>,
    /// Where the next batch goes in the last segment (none without one).
    write: Option<u64>,
    durable: Position,
    appended: Position,
    /// The entry the log was opened from (a checkpoint's): walks that begin
    /// before it in its segment reach it by framing alone.
    anchor: Position,
    /// No cut reaches this entry or one before it: the entry the log was
    /// opened from, or the one before the oldest needed when it last
    /// retired. A checkpoint covers only committed entries, and the
    /// committed position the marker records can go back one advance with a
    /// lost copy of `state` (format section 1.5).
    floor: u64,
    /// The caller supplied a committed position since the log was opened.
    committed_confirmed: bool,
    /// The open ended the log at a place it could not read: no append
    /// until the log is opened again, cut, or what may follow is given up.
    unreadable: Option<Unreadable>,
    batch: Vec<Pending>,
    /// The bytes of the open batch's entries in the current segment and in
    /// the next.
    here: u64,
    there: u64,
    /// The next segment, made, its space taken and flushed, its name
    /// durable, not listed yet.
    spare: Option<Box<dyn File>>,
    poisoned: bool,
}

impl fmt::Debug for Log {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Log")
            .field("state", &self.state)
            .field("write", &self.write)
            .field("durable", &self.durable)
            .field("appended", &self.appended)
            .field("batch", &self.batch.len())
            .field("spare", &self.spare.is_some())
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

/// Where an open ended the log at a place it could not read.
#[derive(Clone, Copy, Debug)]
struct Unreadable {
    end: UnreadableEnd,
    /// The number of the segment the log ends in: a number, not a place in
    /// the run, which a retirement shortens from the front.
    segment: u64,
}

/// A failed read: [`LogError::Unreadable`] when the device failed to read,
/// else the disk's error (the process stopped, say).
fn unreadable(segs: &[Seg], fault: ReadFault) -> LogError {
    if fault.error.kind() != ErrorKind::Read {
        return LogError::Disk(fault.error);
    }
    LogError::Unreadable {
        segment: segs[fault.seg].number,
        offset: fault.offset,
        error: fault.error,
    }
}

fn damage(kind: DamageKind, segment: u64, offset: u64, index: u64) -> LogError {
    LogError::Damaged(Damage {
        kind,
        segment,
        offset,
        index,
    })
}

fn position(found: &Found) -> Position {
    Position {
        index: found.index,
        term: found.term,
        hash: found.hash,
    }
}

/// A batch as written under `cut_count`, after the entry whose hash is
/// `prev`: its header, then its entries.
fn batch_bytes(entries: &[Pending], prev: [u8; 32], cut_count: u64) -> Vec<u8> {
    let length: u64 = entries.iter().map(|e| e.bytes.len() as u64).sum();
    let first = entries.first().expect("a batch has entries").position.index;
    let last = entries.last().expect("a batch has entries").position.hash;
    let header = BatchHeader {
        count: u32::try_from(entries.len()).expect("a batch fits in a segment"),
        first,
        length,
        cut_count,
        prev,
        last,
    };
    let mut out = Vec::with_capacity((BATCH_HEADER + length) as usize);
    out.extend_from_slice(&header.encode());
    for entry in entries {
        out.extend_from_slice(&entry.bytes);
    }
    out
}

/// The last of `segs` whose first index is at or before `index`.
/// Moves the files of segments `moves` aside in `log/`, renamed
/// `given-up-<k>.<number>` (`k` the next number no name uses), and copies
/// segment `tail` to `given-up-<k>.<number>.tail`: a rename reads nothing,
/// so a file the device cannot read is kept whole; a copy writes zeros for
/// a block it cannot read, and says where (offset, length). Everything is
/// flushed, and the folder with it. Returns `given-up-<k>.` and those
/// places.
fn park(
    fs: &dyn Fs,
    moves: &[u64],
    tail: Option<u64>,
) -> Result<(String, Vec<(u64, u64)>), LogError> {
    let disk = |e: sys::Error| match e.kind() {
        ErrorKind::NoSpace => LogError::NoSpace(e),
        _ => LogError::Disk(e),
    };
    let taken = fs
        .list(FOLDER)
        .map_err(disk)?
        .iter()
        .filter_map(|name| {
            let rest = name.strip_prefix(GIVEN_UP_PREFIX)?;
            rest.split('.').next()?.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0);
    let prefix = format!("{GIVEN_UP_PREFIX}{}.", taken + 1);
    let named = |number: u64| format!("{FOLDER}/{prefix}{number:016x}");
    for &number in moves {
        fs.rename(&segment_path(number), &named(number))
            .map_err(disk)?;
    }
    let mut unreadable = Vec::new();
    if let Some(number) = tail {
        let source = fs
            .open(&segment_path(number), FileOptions::PLAIN)
            .map_err(disk)?;
        let size = source.size().map_err(disk)?;
        let copy = fs
            .create(&format!("{}.tail", named(number)), FileOptions::PLAIN)
            .map_err(disk)?;
        let mut buf = vec![0u8; 64 << 10];
        let mut at = 0u64;
        while at < size {
            let want = ((size - at) as usize).min(buf.len());
            let n = match read_exact(&*source, at, &mut buf[..want]) {
                Ok(n) => n,
                Err(error) if error.kind() == ErrorKind::Read => {
                    // A block the device cannot read: zeros, and said so.
                    buf[..want].fill(0);
                    unreadable.push((at, want as u64));
                    want
                }
                Err(e) => return Err(disk(e)),
            };
            if n == 0 {
                break;
            }
            copy.write_at(at, &buf[..n]).map_err(disk)?;
            at += n as u64;
        }
        copy.flush().map_err(disk)?;
    }
    fs.flush_folder(FOLDER).map_err(disk)?;
    Ok((prefix, unreadable))
}

fn segment_for(segs: &[Seg], index: u64) -> Option<usize> {
    segs.iter().rposition(|s| s.header.first <= index)
}

/// What an open found, before the log is built around its `state`.
struct Recovered {
    segs: Vec<Seg>,
    write: Option<u64>,
    /// The number of the segment the log ends in.
    end_segment: Option<u64>,
    durable: Position,
    opened: Opened,
}

/// The most an open holds of a batch's verified entries before it writes
/// them back.
const WRITE_BACK_CHUNK: u64 = 1 << 20;

/// What an open writes again after its firm position (format section 3.6):
/// the entries of the batch being read, as the walk verified them, in runs
/// of up to [`WRITE_BACK_CHUNK`] bytes, and the batch's header once the
/// batch is kept. Nothing is read for it: the bytes written are the bytes
/// the walk checked, from the buffers it checked them in (section 3.11).
#[derive(Default)]
struct WriteBack {
    /// The segment and offset of the run's first byte.
    at: Option<(usize, u64)>,
    run: Vec<u8>,
}

impl WriteBack {
    /// Hold the verified bytes of the entry at `offset` of `segs[seg]`,
    /// writing the run once it is long enough.
    fn add(
        &mut self,
        segs: &[Seg],
        seg: usize,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<(), LogError> {
        match self.at {
            Some((s, o)) if s == seg && o + self.run.len() as u64 == offset => {
                self.run.extend_from_slice(&bytes);
            }
            at => {
                if at.is_some() {
                    self.write(segs)?;
                }
                self.at = Some((seg, offset));
                self.run = bytes;
            }
        }
        if self.run.len() as u64 >= WRITE_BACK_CHUNK {
            self.write(segs)?;
        }
        Ok(())
    }

    /// Write the run held, with a synchronous write.
    fn write(&mut self, segs: &[Seg]) -> Result<(), LogError> {
        if let Some((seg, offset)) = self.at.take() {
            segs[seg]
                .file
                .write_at(offset, &self.run)
                .map_err(LogError::Disk)?;
        }
        self.run.clear();
        Ok(())
    }

    /// The batch whose header's verified bytes are `header`, at `offset` of
    /// `segs[seg]`, is kept: write what is held of its entries, then its
    /// header; both in one write when nothing of the batch was written yet.
    fn finish(
        &mut self,
        segs: &[Seg],
        seg: usize,
        offset: u64,
        header: &[u8],
    ) -> Result<(), LogError> {
        if self.at == Some((seg, offset + BATCH_HEADER)) {
            let mut all = Vec::with_capacity(header.len() + self.run.len());
            all.extend_from_slice(header);
            all.extend_from_slice(&self.run);
            self.clear();
            return segs[seg]
                .file
                .write_at(offset, &all)
                .map_err(LogError::Disk);
        }
        self.write(segs)?;
        segs[seg]
            .file
            .write_at(offset, header)
            .map_err(LogError::Disk)
    }

    /// The batch is not kept: what is held of it is not written.
    fn clear(&mut self) {
        self.at = None;
        self.run.clear();
    }
}

impl Log {
    /// Open the log of the store whose `state` is given (section 6.7 step
    /// 3), reading it from `start`: the entry a checkpoint covers, or
    /// [`Position::START`]. The log owns `state` from then on; on an error
    /// it hands it back ([`OpenFailed`], boxed: a `State` is large).
    ///
    /// Every listed segment must be there with a valid header. Every entry
    /// from `start` through the marker must be there, whole and chained,
    /// with the marker's hash at its index, and the entry at `start` with
    /// its hash; else the log is damaged and nothing is changed. When
    /// `start` covers the marker's entry it answers for it. After the marker
    /// (or `start`, whichever is later), a batch is kept only when it is
    /// whole and carries the cut count `state` holds; the first that is not,
    /// and everything after it, is cut. Then, in this order: the segments
    /// holding entries after the marker are flushed; the marker is moved to
    /// the last entry and listed segments past it are dropped, in one `state`
    /// update; and segment files `state` does not list are removed (format
    /// section 3.6). A crash at any point leaves a log the next open reads
    /// to the same end.
    pub fn open(
        fs: Arc<dyn Fs>,
        mut state: State,
        config: Config,
        start: Position,
    ) -> Result<(Log, Opened), Box<OpenFailed>> {
        match Self::recover(&*fs, &mut state, config, start) {
            Ok(recovered) => {
                let opened = recovered.opened.clone();
                let log = Log {
                    fs,
                    state,
                    config,
                    segs: recovered.segs,
                    write: recovered.write,
                    durable: recovered.durable,
                    appended: recovered.durable,
                    anchor: start,
                    floor: start.index,
                    committed_confirmed: false,
                    unreadable: recovered.opened.unreadable_end.map(|end| Unreadable {
                        end,
                        segment: recovered
                            .end_segment
                            .expect("an open that ended unreadable ended in a segment"),
                    }),
                    batch: Vec::new(),
                    here: 0,
                    there: 0,
                    spare: None,
                    poisoned: false,
                };
                Ok((log, opened))
            }
            Err(error) => Err(Box::new(OpenFailed { error, state })),
        }
    }

    fn recover(
        fs: &dyn Fs,
        state: &mut State,
        config: Config,
        start: Position,
    ) -> Result<Recovered, LogError> {
        config.check()?;
        if !state.holds_lock_of(fs) {
            return Err(LogError::Refused(
                "the state's lock is not this store folder's, or is no longer held",
            ));
        }
        let values = *state.values();
        let run = values.segments;
        let marker = values.marker;
        let store = values.server_id.0;
        let cut_count = values.cut_count;
        if start.index > MAX_INDEX {
            return Err(LogError::Refused("the start is past the last index"));
        }
        if marker.index > MAX_INDEX {
            return Err(LogError::Refused("the marker is past the last index"));
        }
        let after_start = start.index + 1;

        // The folder is made, when missing, only once the checks pass: an
        // open that finds damage changes nothing.
        let (names, no_folder) = match fs.list(FOLDER) {
            Ok(names) => (names, false),
            Err(e) if e.kind() == ErrorKind::NotFound => (Vec::new(), true),
            Err(e) => return Err(LogError::Disk(e)),
        };

        let mut segs: Vec<Seg> = Vec::new();
        for number in run.first..run.first.saturating_add(run.count) {
            let path = segment_path(number);
            let file = match fs.open(&path, FileOptions::SYNC) {
                Ok(file) => file,
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    return Err(damage(DamageKind::Missing, number, 0, 0))
                }
                Err(e) => return Err(LogError::Disk(e)),
            };
            let mut bytes = [0u8; SEGMENT_HEADER as usize];
            let n = match read_exact(&*file, 0, &mut bytes) {
                Ok(n) => n,
                Err(error) if error.kind() == ErrorKind::Read => {
                    return Err(LogError::Unreadable {
                        segment: number,
                        offset: 0,
                        error,
                    })
                }
                Err(e) => return Err(LogError::Disk(e)),
            };
            let header = match SegmentHeader::decode(&bytes[..n]) {
                Ok(header) => header,
                Err(HeaderFault::Newer(version)) => {
                    return Err(LogError::NewerFormat {
                        segment: number,
                        version,
                    })
                }
                Err(fault) => return Err(damage(DamageKind::Header(fault), number, 0, 0)),
            };
            if header.store != store {
                return Err(damage(DamageKind::OtherStore, number, 0, 0));
            }
            if header.number != number {
                return Err(damage(DamageKind::Renamed, number, 0, 0));
            }
            if file.size().map_err(LogError::Disk)? < header.size {
                return Err(damage(DamageKind::Short, number, 0, 0));
            }
            if segs
                .last()
                .is_some_and(|prev| header.first < prev.header.first)
            {
                return Err(damage(DamageKind::Order, number, 0, header.first));
            }
            segs.push(Seg {
                number,
                header,
                file,
            });
        }

        // Walk from the segment that holds the entry after `start`.
        let firm = marker.index.max(start.index);
        let mut last = start;
        let mut end: Option<(usize, u64)> = None;
        let mut kept = 0u64;
        let mut why: Option<CutWhy> = None;
        let mut unreadable_end: Option<UnreadableEnd> = None;
        if let Some(first) = segs.first() {
            if first.header.first > after_start {
                return Err(damage(
                    DamageKind::Gap,
                    first.number,
                    SEGMENT_HEADER,
                    after_start,
                ));
            }
            let from = segment_for(&segs, after_start).expect("the first segment starts in time");
            let header = segs[from].header;
            let before = Position {
                index: header.first - 1,
                term: header.prev_term,
                hash: header.prev_hash,
            };
            let check = |at: &Position, seg: usize, offset: u64| -> Result<(), LogError> {
                if at.index == start.index && at.hash != start.hash {
                    return Err(damage(
                        DamageKind::Start,
                        segs[seg].number,
                        offset,
                        at.index,
                    ));
                }
                if at.index == marker.index && at.hash != marker.hash {
                    return Err(damage(
                        DamageKind::Marker,
                        segs[seg].number,
                        offset,
                        at.index,
                    ));
                }
                Ok(())
            };
            check(&before, from, 0)?;
            last = before;
            end = Some((from, SEGMENT_HEADER));
            // Anchored at the start: what comes before it in its segment
            // is taken by its framing alone (format section 3.5).
            let mut scanner = Scanner::new(&segs, from, Keep::BytesAfter(firm))
                .judging(Judge { firm, cut_count })
                .anchored_at(start.index, start.hash, start.term);
            // Entries after `firm` wait for their batch to prove whole and
            // current: how many, and the last (a batch is in one segment).
            // Their bytes, as verified, wait in `back`, and the batch's
            // header in `header`, to be written again once the batch is
            // kept: bytes read back are never made durable by a flush alone
            // (format section 3.11). Nothing at or before `firm` is written
            // again: what the marker covers was written by a synchronous
            // write that returned, and what the start covers is in a
            // checkpoint, which covers only entries the marker covered once
            // (server-storage.md 6.5 and 6.7); the marker can be behind the
            // start only by a lost copy of `state` (format section 1.5).
            let mut tail: Option<(u64, Found)> = None;
            let mut back = WriteBack::default();
            let mut header: Option<(usize, u64, [u8; BATCH_HEADER as usize])> = None;
            let mut batch_cut_count = cut_count;
            let mut incomplete = false;
            let stop = loop {
                let event = match scanner.next_event() {
                    Ok(event) => event,
                    // After the firm position a read that fails ends the log
                    // for this open only (format section 3.6): nothing past
                    // it is cut, dropped or written over, and the log takes
                    // no appends until it is opened again. At or before the
                    // firm position, the caller tells it apart from damage.
                    Err(fault)
                        if fault.error.kind() == ErrorKind::Read && scanner.position().0 > firm =>
                    {
                        unreadable_end = Some(UnreadableEnd {
                            segment: segs[fault.seg].number,
                            offset: fault.offset,
                            index: scanner.position().0,
                        });
                        break end.expect("set before the walk");
                    }
                    Err(fault) => return Err(unreadable(&segs, fault)),
                };
                let was_incomplete = std::mem::replace(&mut incomplete, false);
                let stale = |batch: u64| {
                    if batch == cut_count {
                        CutWhy::NotWhole
                    } else {
                        CutWhy::OlderCut
                    }
                };
                match event {
                    // A batch written before the last cut counts only up to
                    // the firm position: the walk reads nothing of it past
                    // that, so what follows the firm position is judged by
                    // the batch's header alone (format section 3.6).
                    Event::Batch {
                        seg,
                        offset,
                        cut_count: written,
                        header: bytes,
                    } => {
                        batch_cut_count = written;
                        header = bytes.map(|bytes| (seg, offset, bytes));
                    }
                    Event::Entry(found) if found.index <= firm => {
                        // An entry taken by its framing is the start's or
                        // before it: the checkpoint covers it.
                        if found.verified {
                            check(&position(&found), found.seg, found.offset)?;
                        }
                        if found.index > marker.index {
                            kept += 1;
                        }
                        end = Some((found.seg, found.offset + found.size));
                        last = position(&found);
                    }
                    Event::Entry(mut found) => {
                        let bytes = std::mem::take(&mut found.bytes);
                        back.add(&segs, found.seg, found.offset, bytes)?;
                        let count = tail.as_ref().map_or(0, |(n, _)| *n);
                        tail = Some((count + 1, found));
                    }
                    Event::BatchEnd { whole } => match tail.take() {
                        // A batch written before the last cut is never part
                        // of the log after it, however it chains.
                        Some((count, found)) if whole && batch_cut_count == cut_count => {
                            let (seg, offset, bytes) = header
                                .take()
                                .expect("a batch of the current count had its header read");
                            back.finish(&segs, seg, offset, &bytes)?;
                            kept += count;
                            end = Some((found.seg, found.offset + found.size));
                            last = position(&found);
                        }
                        Some(_) => {
                            back.clear();
                            why = Some(stale(batch_cut_count));
                            break end.expect("set before the walk");
                        }
                        None => incomplete = !whole,
                    },
                    Event::Segment { seg } => end = Some((seg, SEGMENT_HEADER)),
                    Event::End { seg, offset } => {
                        // An entry that broke a batch off ends the walk.
                        if was_incomplete {
                            why = Some(stale(batch_cut_count));
                        }
                        break (seg, offset);
                    }
                }
            };
            if last.index < marker.index {
                return Err(damage(
                    DamageKind::EndsEarly,
                    segs[stop.0].number,
                    stop.1,
                    last.index + 1,
                ));
            }
            if last.index < start.index {
                return Err(damage(
                    DamageKind::Gap,
                    segs[stop.0].number,
                    stop.1,
                    last.index + 1,
                ));
            }
        } else if marker.index > start.index {
            return Err(damage(DamageKind::EndsEarly, run.first, 0, after_start));
        }

        // The log holds what it must, and what it keeps after the firm
        // position was written again as the walk went. Make its folder's
        // name durable, then record it. The store folder is flushed on every
        // open, whether this open made `log/` or found it: an earlier open
        // may have made it and failed to flush (format section 3.6).
        if no_folder {
            fs.create_folder(FOLDER).map_err(LogError::Disk)?;
        }
        fs.flush_folder("").map_err(LogError::Disk)?;
        let mut cut = None;
        if let (Some((seg, offset)), None) = (end, &unreadable_end) {
            let dropped = (segs.len() - seg - 1) as u64;
            if why.is_some() || dropped > 0 {
                cut = Some(Cut {
                    index: last.index + 1,
                    segment: segs[seg].number,
                    offset,
                    dropped,
                    why: why.unwrap_or(CutWhy::Segments),
                });
            }
            segs.truncate(seg + 1);
        }
        let listed = Segments {
            first: run.first,
            count: segs.len() as u64,
        };
        let new_marker = Marker {
            index: last.index,
            hash: last.hash,
            committed: marker.committed,
        };
        state
            .update(|v| {
                v.marker = new_marker;
                v.segments = listed;
            })
            .map_err(LogError::State)?;

        // Segment files no longer listed: dropped by this open, or left by a
        // process that stopped before removing them.
        let mut removed = Vec::new();
        for name in &names {
            let Some(number) = segment_number(name) else {
                continue;
            };
            if (listed.first..listed.first + listed.count).contains(&number) {
                continue;
            }
            match fs.remove(&segment_path(number)) {
                Ok(()) => removed.push(number),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(LogError::Disk(e)),
            }
        }
        // Flushed on every open too: an earlier one may have made or
        // removed a file in it and failed to flush.
        fs.flush_folder(FOLDER).map_err(LogError::Disk)?;

        Ok(Recovered {
            write: end.map(|(_, offset)| offset),
            end_segment: end.map(|(seg, _)| segs[seg].number),
            segs,
            durable: last,
            opened: Opened {
                durable: last,
                recorded_committed: marker.committed,
                cut_count,
                kept,
                cut,
                unreadable_end,
                removed,
            },
        })
    }

    /// Accept the loss of everything the log can no longer produce, and
    /// open it: the storage half of the administrator's "accept the loss"
    /// on a single server, for a log that does not hold, whole, every entry
    /// through its marker ([`LogError::Damaged`]; server-storage.md 6.12).
    /// `start` is the entry of the checkpoint the store opens on. The rule
    /// (format section 6.2):
    ///
    /// - **kept**: the chain from `start`: in the listed segment that holds
    ///   the entry after it, reached by framing alone (nothing before the
    ///   start is needed), then checked forward by the chain from the
    ///   start's hash through the segments that follow without a gap, up
    ///   to the later of the marker and the start; and the listed segments
    ///   before it that follow one another without a gap. A kept file
    ///   shorter than its header says is read as far as it goes, and
    ///   lengthened with zeros;
    /// - **given up**: every listed segment not kept, and what follows the
    ///   last entry so reached (`kept`); the marker is lowered to it. Every
    ///   file given up is renamed, in `log/`, to a name of this
    ///   acceptance's own that no open removes, and the kept one whose tail
    ///   is given up is copied there: durable before `state` changes. The
    ///   file system renames within one folder only, and a rename reads
    ///   nothing, so a file given up because the device cannot read it is
    ///   kept whole;
    /// - **refused**, with nothing changed: when the framing before the
    ///   start breaks in the segment that holds the start's successor, so
    ///   that where it begins cannot be established; when nothing would
    ///   change; on a segment of a newer format; on a read the device fails
    ///   (`LogError::Unreadable`).
    ///
    /// One `state` update, written to both slots, lists the kept segments,
    /// lowers the marker (its committed position with it, when that was
    /// past) and raises the cut count when anything after `kept` is given
    /// up. Then the log is opened as [`Log::open`] opens it, which removes
    /// the files no longer listed. A failure after the decision says what
    /// it was and whether `state` holds it ([`AcceptFailed::decided`]). It
    /// needs no committed position: it is the administrator's decision,
    /// which no committed position could make on a single server.
    pub fn accept_damage(
        fs: Arc<dyn Fs>,
        mut state: State,
        config: Config,
        start: Position,
    ) -> Result<(Log, Opened, DamageAccepted), Box<AcceptFailed>> {
        let fail = |error, state, decided| {
            Box::new(AcceptFailed {
                failed: Box::new(OpenFailed { error, state }),
                decided,
            })
        };
        let plan = match Self::decide_damaged(&*fs, &state, config, start) {
            Ok(plan) => plan,
            Err(error) => return Err(fail(error, state, None)),
        };
        let AcceptPlan {
            mut accepted,
            values,
            moves,
            lengthen,
        } = plan;
        // What is given up is moved aside first, under names no open
        // removes, and the kept segment whose tail is given up copied: all
        // durable before `state` changes.
        if !moves.is_empty() || accepted.tail_copied.is_some() {
            match park(&*fs, &moves, accepted.tail_copied) {
                Ok((prefix, unreadable)) => {
                    for given in &mut accepted.given_up {
                        if moves.contains(&given.number) {
                            given.moved_to =
                                Some(format!("{FOLDER}/{prefix}{:016x}", given.number));
                        }
                    }
                    accepted.prefix = Some(format!("{FOLDER}/{prefix}"));
                    accepted.tail_unreadable = unreadable;
                }
                Err(error) => return Err(fail(error, state, None)),
            }
        }
        // Kept files that are short, lengthened with zeros: past what is
        // kept, which the walk read, nothing in them is part of the log.
        for &(number, size) in &lengthen {
            let lengthened = fs
                .open(&segment_path(number), FileOptions::PLAIN)
                .and_then(|file| file.allocate(size).and_then(|()| file.flush()));
            match lengthened {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NoSpace => {
                    return Err(fail(LogError::NoSpace(e), state, None))
                }
                Err(e) => return Err(fail(LogError::Disk(e), state, None)),
            }
        }
        if let Err(e) = state.update(|v| *v = values) {
            // A refusal writes nothing; any other failure may have written
            // the first slot.
            let decided = match e {
                StateError::Refused(_) => None,
                _ => Some((accepted, Recorded::Unknown)),
            };
            return Err(fail(LogError::State(e), state, decided));
        }
        match Log::open(fs, state, config, start) {
            Ok((log, opened)) => Ok((log, opened, accepted)),
            Err(failed) => Err(Box::new(AcceptFailed {
                failed,
                decided: Some((accepted, Recorded::Done)),
            })),
        }
    }

    /// The decision of [`Log::accept_damage`], and what it writes. Nothing
    /// is written here.
    fn decide_damaged(
        fs: &dyn Fs,
        state: &State,
        config: Config,
        start: Position,
    ) -> Result<AcceptPlan, LogError> {
        config.check()?;
        if !state.holds_lock_of(fs) {
            return Err(LogError::Refused(
                "the state's lock is not this store folder's, or is no longer held",
            ));
        }
        let values = *state.values();
        let (run, marker, store) = (values.segments, values.marker, values.server_id.0);
        if start.index > MAX_INDEX || marker.index > MAX_INDEX {
            return Err(LogError::Refused(
                "the start or the marker is past the last index",
            ));
        }
        if run.count > MAX_RUN {
            return Err(LogError::Refused(
                "state lists more segments than an acceptance looks at",
            ));
        }
        let run_end = run
            .first
            .checked_add(run.count)
            .ok_or(LogError::Refused("the segment numbers are spent"))?;
        let firm = marker.index.max(start.index);
        // The listed segments the folder holds, in order.
        let names = match fs.list(FOLDER) {
            Ok(names) => names,
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(LogError::Disk(e)),
        };
        let mut numbers: Vec<u64> = names
            .iter()
            .filter_map(|name| segment_number(name))
            .filter(|n| (run.first..run_end).contains(n))
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        let mut present = std::collections::BTreeSet::new();
        let mut usable: Vec<Seg> = Vec::new();
        let mut short = std::collections::BTreeSet::new();
        for number in numbers {
            let file = match fs.open(&segment_path(number), FileOptions::SYNC) {
                Ok(file) => file,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(LogError::Disk(e)),
            };
            present.insert(number);
            let mut bytes = [0u8; SEGMENT_HEADER as usize];
            let n = match read_exact(&*file, 0, &mut bytes) {
                Ok(n) => n,
                Err(error) if error.kind() == ErrorKind::Read => {
                    return Err(LogError::Unreadable {
                        segment: number,
                        offset: 0,
                        error,
                    })
                }
                Err(e) => return Err(LogError::Disk(e)),
            };
            let header = match SegmentHeader::decode(&bytes[..n]) {
                Ok(header) => header,
                Err(HeaderFault::Newer(version)) => {
                    return Err(LogError::NewerFormat {
                        segment: number,
                        version,
                    })
                }
                Err(_) => continue,
            };
            if header.store != store || header.number != number {
                continue;
            }
            // A short file is read as far as it goes: its length is no
            // reason to pass it over.
            if file.size().map_err(LogError::Disk)? < header.size {
                short.insert(number);
            }
            usable.push(Seg {
                number,
                header,
                file,
            });
        }
        let first_of: std::collections::BTreeMap<u64, u64> =
            usable.iter().map(|s| (s.number, s.header.first)).collect();
        // Two segments follow one another when their numbers do and the
        // second begins no earlier than the first.
        let follows =
            |a: &Seg, b: &Seg| b.number == a.number + 1 && b.header.first >= a.header.first;
        let fits = |p: &Position| {
            !(p.index == start.index && p.hash != start.hash)
                && !(p.index == marker.index && p.hash != marker.hash)
        };
        // An empty log that needs to hold nothing has nothing to give up.
        let mut reached = usable.is_empty() && run.count == 0 && marker.index <= start.index;
        let mut kept = start;
        let mut keep: Option<(u64, u64)> = None;
        let after = start.index + 1;
        if let Some(from) = usable.iter().rposition(|s| s.header.first <= after) {
            let mut to = from;
            while to + 1 < usable.len() && follows(&usable[to], &usable[to + 1]) {
                to += 1;
            }
            let mut front = from;
            while front > 0 && follows(&usable[front - 1], &usable[front]) {
                front -= 1;
            }
            let chain = &usable[from..=to];
            let header = chain[0].header;
            let before = Position {
                index: header.first - 1,
                term: header.prev_term,
                hash: header.prev_hash,
            };
            if fits(&before) {
                let mut seen = (before, 0usize);
                if before.index < firm {
                    let mut scanner = Scanner::new(chain, 0, Keep::Nothing).anchored_at(
                        start.index,
                        start.hash,
                        start.term,
                    );
                    loop {
                        match scanner.next_event() {
                            Err(fault) => return Err(unreadable(chain, fault)),
                            Ok(Event::Entry(found)) => {
                                let at = position(&found);
                                if found.verified && !fits(&at) {
                                    break;
                                }
                                seen = (at, found.seg);
                                if at.index >= firm {
                                    break;
                                }
                            }
                            Ok(Event::End { seg, offset }) => {
                                // Before the start, a walk that ends where
                                // its framing broke cannot say where the
                                // start's successor begins.
                                if scanner.before_anchor() && scanner.broke() {
                                    return Err(LogError::FramingBroken {
                                        segment: chain[seg].number,
                                        offset,
                                    });
                                }
                                break;
                            }
                            Ok(_) => {}
                        }
                    }
                }
                if seen.0.index >= start.index {
                    kept = seen.0;
                    reached = seen.0.index >= firm;
                    let last = if reached { to } else { from + seen.1 };
                    keep = Some((usable[front].number, usable[last].number));
                }
            }
        }
        let listed = match keep {
            Some((first, last)) => Segments {
                first,
                count: last - first + 1,
            },
            None => Segments {
                first: run_end,
                count: 0,
            },
        };
        let listed_end = listed.first + listed.count;
        let kept_numbers = listed.first..listed_end;
        let lengthen: Vec<(u64, u64)> = usable
            .iter()
            .filter(|s| kept_numbers.contains(&s.number) && short.contains(&s.number))
            .map(|s| (s.number, s.header.size))
            .collect();
        if reached && listed == run && lengthen.is_empty() {
            return Err(LogError::Refused(
                "the log holds every entry through its marker: there is no loss to accept",
            ));
        }
        // Anything after `kept` given up: the marker lowered, or listed
        // segments after the kept ones dropped.
        let raise = !reached || listed_end < run_end;
        let cut_count = if raise {
            Some(
                values
                    .cut_count
                    .checked_add(1)
                    .ok_or(LogError::Refused("the cut count is at its limit"))?,
            )
        } else {
            None
        };
        let mut given_up = Vec::new();
        let mut moves = Vec::new();
        for number in (run.first..run_end).filter(|n| !kept_numbers.contains(n)) {
            given_up.push(SegmentGivenUp {
                number,
                first: first_of.get(&number).copied(),
                last: (number + 1 < run_end)
                    .then(|| first_of.get(&(number + 1)).map(|f| f - 1))
                    .flatten(),
                moved_to: None,
            });
            if present.contains(&number) {
                moves.push(number);
            }
        }
        let tail_copied = (raise && listed.count > 0).then_some(listed_end - 1);
        let lowered = if reached {
            marker
        } else {
            Marker {
                index: kept.index,
                hash: kept.hash,
                committed: marker.committed.min(kept.index),
            }
        };
        let new = Values {
            marker: lowered,
            segments: listed,
            cut_count: cut_count.unwrap_or(values.cut_count),
            ..values
        };
        Ok(AcceptPlan {
            accepted: DamageAccepted {
                kept,
                marker: marker.index,
                cut_count,
                listed_before: run,
                listed,
                given_up,
                tail_copied,
                tail_unreadable: Vec::new(),
                lengthened: lengthen.iter().map(|(n, _)| *n).collect(),
                prefix: None,
            },
            values: new,
            moves,
            lengthen,
        })
    }

    /// What `state` holds.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Change what `state` holds besides the log's own fields (the term and
    /// vote, the repair record), as [`State::update`] does. Refused, with
    /// nothing written, when the change touches the segment run, the marker
    /// or the cut count, which only the log changes. A failed write stops
    /// the log.
    pub fn update_state(&mut self, change: impl FnOnce(&mut Values)) -> Result<(), LogError> {
        self.usable()?;
        self.check_lock()?;
        let before = *self.state.values();
        let mut after = before;
        change(&mut after);
        if after.segments != before.segments
            || after.marker != before.marker
            || after.cut_count != before.cut_count
        {
            return Err(LogError::Refused(
                "the segment run, the marker and the cut count change only through the log",
            ));
        }
        self.update(|v| *v = after)
    }

    /// The log's `state`, closing the log.
    pub fn into_state(self) -> State {
        self.state
    }

    /// The last durable entry: the one the marker covers.
    pub fn durable(&self) -> Position {
        self.durable
    }

    /// The last entry appended, durable or in the open batch.
    pub fn appended(&self) -> Position {
        self.appended
    }

    /// The committed position the marker records (see
    /// [`Log::set_committed`]).
    pub fn committed(&self) -> u64 {
        self.state.values().marker.committed
    }

    /// Whether the caller supplied a committed position since the log was
    /// opened, which a cut needs (see [`Opened::recorded_committed`]).
    pub fn committed_confirmed(&self) -> bool {
        self.committed_confirmed
    }

    /// The first index the log holds (the entries before it were retired or
    /// are covered by the checkpoint it was opened from).
    pub fn first_index(&self) -> u64 {
        self.segs
            .first()
            .map_or_else(|| self.durable.index.saturating_add(1), |s| s.header.first)
    }

    /// The segments in use, as `state` lists them.
    pub fn segments(&self) -> Segments {
        self.state.values().segments
    }

    /// How many entries the open batch holds.
    pub fn batch_len(&self) -> usize {
        self.batch.len()
    }

    /// Whether the next segment is made and ready.
    pub fn spare_ready(&self) -> bool {
        self.spare.is_some()
    }

    /// Whether an earlier failure stopped the log.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn usable(&self) -> Result<(), LogError> {
        if self.poisoned {
            Err(LogError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// The store folder's lock is still this log's: every call that writes
    /// checks it, so that a log of a process that has stopped writes
    /// nothing.
    fn check_lock(&self) -> Result<(), LogError> {
        if self.state.holds_lock_of(&*self.fs) {
            Ok(())
        } else {
            Err(LogError::Refused(
                "the store folder's lock is no longer held",
            ))
        }
    }

    /// The disk failed in a call that writes: the log stops.
    fn failed(&mut self, error: sys::Error) -> LogError {
        self.poisoned = true;
        LogError::Disk(error)
    }

    /// Update `state`. A refusal writes nothing; any other failure stops the
    /// log.
    fn update(&mut self, change: impl FnOnce(&mut Values)) -> Result<(), LogError> {
        self.update_with(false, change)
    }

    /// Update `state`; when the call already `wrote` to a segment, a refusal
    /// stops the log too: what reached the segment is then not recorded, and
    /// what the log holds in memory no longer matches it.
    fn update_with(
        &mut self,
        wrote: bool,
        change: impl FnOnce(&mut Values),
    ) -> Result<(), LogError> {
        match self.state.update(change) {
            Ok(()) => Ok(()),
            Err(e @ StateError::Refused(_)) if !wrote => Err(LogError::State(e)),
            Err(e) => {
                self.poisoned = true;
                Err(LogError::State(e))
            }
        }
    }

    /// Make the next segment ahead of need (format section 3.7): create its
    /// file, take its space, flush it and flush the folder, so that a batch
    /// that reaches it never waits for an allocation and never fails half
    /// way for lack of space. Nothing to do when it is ready. A disk too full
    /// for it fails with [`LogError::NoSpace`], and an allocation that fails
    /// part way with [`LogError::Disk`]; neither stops the log, since
    /// nothing listed was touched, and a later call finishes the file. A
    /// failed flush stops the log. A segment number whose run `state` could
    /// not list (its end past the largest number) is refused before anything
    /// is made.
    pub fn prepare(&mut self) -> Result<(), LogError> {
        self.usable()?;
        self.check_lock()?;
        if self.spare.is_some() {
            return Ok(());
        }
        // The run that lists it must end within the numbers, as `state`
        // requires: refused here, before anything is written for it.
        let run = self.segments();
        let number = run
            .first
            .checked_add(run.count)
            .filter(|n| n.checked_add(1).is_some())
            .ok_or(LogError::Refused("the segment numbers are spent"))?;
        let path = segment_path(number);
        let file = match self.fs.open(&path, FileOptions::PLAIN) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                match self.fs.create(&path, FileOptions::PLAIN) {
                    Ok(file) => file,
                    Err(e) if e.kind() == ErrorKind::NoSpace => return Err(LogError::NoSpace(e)),
                    Err(e) => return Err(self.failed(e)),
                }
            }
            Err(e) => return Err(self.failed(e)),
        };
        match file.allocate(self.config.segment_size) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NoSpace => return Err(LogError::NoSpace(e)),
            Err(e) if e.kind() == ErrorKind::Write => return Err(LogError::Disk(e)),
            Err(e) => return Err(self.failed(e)),
        }
        if let Err(e) = file.flush() {
            return Err(self.failed(e));
        }
        if let Err(e) = self.fs.flush_folder(FOLDER) {
            return Err(self.failed(e));
        }
        drop(file);
        match self.fs.open(&path, FileOptions::SYNC) {
            Ok(file) => {
                self.spare = Some(file);
                Ok(())
            }
            Err(e) => Err(self.failed(e)),
        }
    }

    /// Add an entry to the open batch, in memory, and return its place.
    /// Nothing is written until [`Log::flush_batch`]. Refused, with nothing
    /// added, when the log ends at a place it could not read
    /// ([`LogError::EndUnreadable`]), when the payload is too large, the
    /// term is lower than the last entry's, the index would pass
    /// [`MAX_INDEX`], the batch would span more than two segments
    /// ([`LogError::BatchFull`]), or the entry needs the next segment and
    /// the disk has no room for it ([`LogError::NoSpace`]).
    pub fn append(
        &mut self,
        term: u64,
        kind: u16,
        file: u64,
        payload: &[u8],
    ) -> Result<Position, LogError> {
        let next_segment = self.admit(term, payload.len())?;
        let index = self.appended.index + 1;
        let size = ENTRY_OVERHEAD + payload.len() as u64;
        let (bytes, hash) = encode_entry(&self.appended.hash, index, term, kind, file, payload);
        let position = Position { index, term, hash };
        if next_segment {
            self.there += size;
        } else {
            self.here += size;
        }
        self.batch.push(Pending {
            bytes,
            position,
            next_segment,
        });
        self.appended = position;
        Ok(position)
    }

    /// Whether [`Log::append`] would take an entry of `len` payload bytes
    /// at `term` now, with every refusal of `append`, and the next segment
    /// made when the entry would need it; nothing is added. A caller that
    /// must not be refused once it has done other work for the entry (the
    /// store applies an entry before it appends it) asks first: nothing
    /// changes the answer before the `append` that follows, but a failure.
    pub fn ready_for(&mut self, term: u64, len: usize) -> Result<(), LogError> {
        self.admit(term, len).map(|_| ())
    }

    /// The checks of an append, and the next segment made when it is
    /// needed: whether the entry goes to the next segment.
    fn admit(&mut self, term: u64, len: usize) -> Result<bool, LogError> {
        self.usable()?;
        if let Some(unreadable) = self.unreadable {
            return Err(LogError::EndUnreadable(unreadable.end));
        }
        if len > self.config.max_payload as usize {
            return Err(LogError::TooLarge {
                len,
                max: self.config.max_payload,
            });
        }
        if term < self.appended.term {
            return Err(LogError::Refused(
                "an entry's term is lower than the term of the entry before it",
            ));
        }
        if self.appended.index >= MAX_INDEX {
            return Err(LogError::Refused("the log is at its last index"));
        }
        let size = ENTRY_OVERHEAD + len as u64;
        let room = match (self.write, self.segs.last()) {
            (Some(offset), Some(seg)) => seg.header.size - offset,
            _ => 0,
        };
        let next_segment = self.there > 0 || BATCH_HEADER + self.here + size > room;
        if next_segment {
            if SEGMENT_HEADER + BATCH_HEADER + self.there + size > self.config.segment_size {
                return Err(LogError::BatchFull);
            }
            self.prepare()?;
        }
        Ok(next_segment)
    }

    /// Write the open batch and record the marker over it (format section
    /// 3.8), with `committed`, when given, as the committed position (it
    /// never goes down; giving one confirms it, see [`Log::cut`]). Returns
    /// the durable position. Refused, with nothing written and the log
    /// going on, when `committed` is past the last entry this flush makes
    /// durable (see [`Log::set_committed`] for what a caller gives).
    ///
    /// The entries that fit in the current segment are written there as one
    /// batch, with one synchronous write; the rest go to the next segment as
    /// a second batch, written with its header in one synchronous write.
    /// Every batch carries the cut count. A synchronous write is on the
    /// device when it returns (`crate::sys`, durability), so the batch is
    /// durable before `state` is updated: the marker alone when the batch
    /// stayed in one segment (one write and one flush of `state`), the
    /// marker and the new segment together otherwise (both slots). Any
    /// failure stops the log, a refusal of the `state` update after the
    /// batch was written included; an entry the failed call wrote may be
    /// found whole at the next open, and its outcome was not reported.
    pub fn flush_batch(&mut self, committed: Option<u64>) -> Result<Position, LogError> {
        self.usable()?;
        self.check_lock()?;
        if committed.is_some_and(|c| c > self.appended.index) {
            return Err(LogError::Refused(
                "a committed position past the log's last entry",
            ));
        }
        let values = *self.state.values();
        let recorded = values.marker.committed;
        let new_committed = committed.map_or(recorded, |c| c.max(recorded));
        if self.batch.is_empty() {
            if new_committed != recorded {
                self.set_marker(self.durable, new_committed, None, false)?;
            }
            self.committed_confirmed |= committed.is_some();
            return Ok(self.durable);
        }
        let batch = std::mem::take(&mut self.batch);
        let split = batch
            .iter()
            .position(|e| e.next_segment)
            .unwrap_or(batch.len());
        let (here, there) = batch.split_at(split);
        let mut write = self.write;
        if !here.is_empty() {
            let offset = write.expect("entries are placed in a segment only when there is one");
            let bytes = batch_bytes(here, self.durable.hash, values.cut_count);
            let seg = self.segs.last().expect("a segment to write in");
            if let Err(e) = seg.file.write_at(offset, &bytes) {
                return Err(self.failed(e));
            }
            write = Some(offset + bytes.len() as u64);
        }
        let mut new_seg = None;
        if !there.is_empty() {
            let file = self
                .spare
                .take()
                .expect("an entry is placed in the next segment only once it is ready");
            let number = values.segments.first + values.segments.count;
            let before = here.last().map_or(self.durable, |e| e.position);
            let header = SegmentHeader {
                store: values.server_id.0,
                number,
                size: self.config.segment_size,
                first: before.index + 1,
                prev_term: before.term,
                prev_hash: before.hash,
            };
            let mut bytes = header.encode().to_vec();
            bytes.extend_from_slice(&batch_bytes(there, before.hash, values.cut_count));
            if let Err(e) = file.write_at(0, &bytes) {
                return Err(self.failed(e));
            }
            write = Some(bytes.len() as u64);
            new_seg = Some(Seg {
                number,
                header,
                file,
            });
        }
        let last = self.appended;
        self.set_marker(last, new_committed, new_seg, true)?;
        self.committed_confirmed |= committed.is_some();
        self.write = write;
        self.durable = last;
        self.here = 0;
        self.there = 0;
        Ok(last)
    }

    /// Record the committed position `committed` in the marker on its own
    /// (it never goes down): one write and one flush of `state` when it
    /// rises. It confirms the committed position for [`Log::cut`] either
    /// way. Refused, with nothing written and the log going on, when it is
    /// past the durable entry.
    ///
    /// What a caller gives is the last entry of this log known to be
    /// committed, never past the log's own entries: on a single server the
    /// last entry it made durable; on a cluster the lower of the leader's
    /// committed position and the last entry of this log known to match the
    /// leader's (Raft's follower rule). The bound matters: a follower that
    /// recorded its leader's committed position while its own log held
    /// stale entries from before it would refuse, for good, the cut that
    /// removes them (format section 3.8).
    pub fn set_committed(&mut self, committed: u64) -> Result<(), LogError> {
        self.usable()?;
        self.check_lock()?;
        if committed > self.durable.index {
            return Err(LogError::Refused(
                "a committed position past the durable entry",
            ));
        }
        if committed > self.committed() {
            self.set_marker(self.durable, committed, None, false)?;
        }
        self.committed_confirmed = true;
        Ok(())
    }

    /// Record the marker at `at` with `committed`, and list `new_seg` with
    /// it when given; `wrote` when the call wrote a batch first.
    fn set_marker(
        &mut self,
        at: Position,
        committed: u64,
        new_seg: Option<Seg>,
        wrote: bool,
    ) -> Result<(), LogError> {
        let marker = Marker {
            index: at.index,
            hash: at.hash,
            committed,
        };
        let mut run = self.segments();
        if new_seg.is_some() {
            run.count += 1;
        }
        self.update_with(wrote, |v| {
            v.marker = marker;
            v.segments = run;
        })?;
        if let Some(seg) = new_seg {
            self.segs.push(seg);
        }
        Ok(())
    }

    /// Remove the entries from `from` on (section 7.1, a follower whose log
    /// differs from the leader's), and say what was done.
    ///
    /// Refused ([`LogError::CommittedUnconfirmed`]) until the caller has
    /// supplied a committed position since the log was opened, because the
    /// one recorded can be behind after a restart; then refused at or
    /// before that position, at or before the entry the log was opened from
    /// or the one before the oldest entry a retirement still needed (all
    /// committed), and before the first entry the log holds.
    ///
    /// - When the log ends at a place it could not read
    ///   ([`Opened::unreadable_end`]) and `from` is the entry after the
    ///   durable one, what may follow that place is given up, as by
    ///   [`Log::give_up_unreadable_end`]. A `from` further on removes
    ///   nothing, as on any log that holds nothing there, and the log still
    ///   ends unreadable: a cut never removes anything before `from`.
    /// - Entries of the open batch are dropped in memory.
    /// - When the marker covers entries that are cut, one `state` update,
    ///   written to both slots, lowers the marker, raises the cut count and
    ///   drops the listed segments after the one that holds `from`: the cut
    ///   is done when it is, since no batch written before it is read after
    ///   it. Then the dropped segments' files are removed and the folder
    ///   flushed (format section 3.9). An unreadable end the log had is
    ///   then past the cut, and appends are taken again.
    ///
    /// Appends continue from where the first cut entry was.
    pub fn cut(&mut self, from: u64) -> Result<CutDone, LogError> {
        self.usable()?;
        self.check_lock()?;
        if !self.committed_confirmed {
            return Err(LogError::CommittedUnconfirmed);
        }
        if from == 0 || from <= self.committed().max(self.floor) {
            return Err(LogError::Refused(
                "entries at or before the committed position are never cut",
            ));
        }
        if self.unreadable.is_some() && from == self.durable.index + 1 {
            return self.give_up();
        }
        if from > self.appended.index {
            return Ok(CutDone::nothing(self.appended));
        }
        if from > self.durable.index {
            let keep = (from - self.durable.index - 1) as usize;
            let dropped = (self.batch.len() - keep) as u64;
            self.batch.truncate(keep);
            self.here = 0;
            self.there = 0;
            for entry in &self.batch {
                let size = entry.bytes.len() as u64;
                if entry.next_segment {
                    self.there += size;
                } else {
                    self.here += size;
                }
            }
            self.appended = self.batch.last().map_or(self.durable, |e| e.position);
            return Ok(CutDone {
                unflushed: dropped,
                ..CutDone::nothing(self.appended)
            });
        }
        if from < self.first_index() {
            return Err(LogError::Refused("the entries before it are retired"));
        }
        let values = *self.state.values();
        let Some(cut_count) = values.cut_count.checked_add(1) else {
            return Err(LogError::Refused("the cut count is at its limit"));
        };
        let at = segment_for(&self.segs, from).expect("from is at or after the first index");
        let (seg, offset, before) = {
            let header = self.segs[at].header;
            let mut before = Position {
                index: header.first - 1,
                term: header.prev_term,
                hash: header.prev_hash,
            };
            let anchor = self.anchor;
            let mut scanner = Scanner::new(&self.segs, at, Keep::Nothing).anchored_at(
                anchor.index,
                anchor.hash,
                anchor.term,
            );
            loop {
                match scanner.next_event() {
                    Err(fault) => return Err(unreadable(&self.segs, fault)),
                    Ok(Event::Entry(found)) if found.index == from => {
                        let offset = if found.first_in_batch {
                            found.batch_offset
                        } else {
                            found.offset
                        };
                        break (found.seg, offset, before);
                    }
                    Ok(Event::Entry(found)) => before = position(&found),
                    Ok(Event::End { seg, offset }) => {
                        return Err(damage(
                            DamageKind::EndsEarly,
                            self.segs[seg].number,
                            offset,
                            before.index + 1,
                        ))
                    }
                    Ok(_) => {}
                }
            }
        };
        let marker = Marker {
            index: before.index,
            hash: before.hash,
            committed: values.marker.committed,
        };
        let run = Segments {
            first: values.segments.first,
            count: seg as u64 + 1,
        };
        self.update(|v| {
            v.marker = marker;
            v.segments = run;
            v.cut_count = cut_count;
        })?;
        // The spare was made for the number after the old run.
        let dropped = values.segments.count != run.count;
        let spare = values.segments.first + values.segments.count;
        let done = CutDone {
            last: before,
            unflushed: self.batch.len() as u64,
            durable: self.durable.index - before.index,
            cut_count: Some(cut_count),
            // What lay past an unreadable end is now after the cut, and
            // carries an older cut count: appends may go on.
            unreadable_end: self.unreadable.take().map(|u| u.end),
            fence: None,
            removed: Vec::new(),
        };
        self.write = Some(offset);
        self.batch.clear();
        self.here = 0;
        self.there = 0;
        self.durable = before;
        self.appended = before;
        let mut remove: Vec<u64> = self.segs.drain(seg + 1..).map(|s| s.number).collect();
        if dropped && self.spare.take().is_some() {
            remove.push(spare);
        }
        let mut removed = Vec::new();
        for number in remove {
            match self.fs.remove(&segment_path(number)) {
                Ok(()) => removed.push(number),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(self.failed(e)),
            }
        }
        // On every cut, whatever it removed: the folder's flush does not
        // depend on who changed it or when.
        if let Err(e) = self.fs.flush_folder(FOLDER) {
            return Err(self.failed(e));
        }
        Ok(CutDone { removed, ..done })
    }

    /// Give up whatever may follow the place where the log ends unreadable
    /// ([`Opened::unreadable_end`]): the storage half of the
    /// administrator's "accept the loss" on a single server
    /// (server-storage.md 6.12 and 11). The caller names the last entry the
    /// open verified, `index` and `hash` ([`Opened::durable`]).
    ///
    /// The next segment is made (as by [`Log::prepare`]) and its header,
    /// naming the entry after that one as its first and continuing the
    /// chain from it, is written synchronously; then one `state` update,
    /// written to both slots, lists it and raises the cut count. From then
    /// nothing written before is part of the log, whatever an open can read
    /// later: it carries an older cut count. Every walk moves to the fresh
    /// segment from the last verified entry and reads nothing further of
    /// the segment the log ended in (format section 3.5, rule 1), until a
    /// later cut of durable entries drops the fresh segment and the log
    /// grows in that segment again: over the place, or into the next
    /// segment when too little room is left before it. Appends are taken
    /// again.
    ///
    /// It needs no committed position: it removes nothing at or before the
    /// durable entry, which the marker covers (server-storage.md 6.3).
    /// Refused, with nothing written, when the log does not end at a place
    /// it could not read, when the entry named is not the last one the open
    /// verified, or when `state` lists segments after the one the log ends
    /// in, which no write of this log leaves (format section 3.9). A disk
    /// too full for the segment refuses it ([`LogError::NoSpace`]) as an
    /// append is refused.
    pub fn give_up_unreadable_end(
        &mut self,
        index: u64,
        hash: [u8; 32],
    ) -> Result<CutDone, LogError> {
        self.usable()?;
        self.check_lock()?;
        if self.unreadable.is_none() {
            return Err(LogError::Refused(
                "the log does not end at a place it could not read",
            ));
        }
        if (index, hash) != (self.durable.index, self.durable.hash) {
            return Err(LogError::Refused(
                "the entry named is not the last one the open verified",
            ));
        }
        self.give_up()
    }

    /// What may follow the unreadable end is given up: the fence of
    /// [`Log::give_up_unreadable_end`].
    fn give_up(&mut self) -> Result<CutDone, LogError> {
        let unreadable = self.unreadable.expect("the log ends unreadable");
        // No append is taken while the log ends unreadable.
        let last = self.durable;
        if self.segs.last().map(|s| s.number) != Some(unreadable.segment) {
            return Err(LogError::Refused(
                "state lists segments after the one the log ends in",
            ));
        }
        if last.index >= MAX_INDEX {
            return Err(LogError::Refused("the log is at its last index"));
        }
        let values = *self.state.values();
        let Some(cut_count) = values.cut_count.checked_add(1) else {
            return Err(LogError::Refused("the cut count is at its limit"));
        };
        self.prepare()?;
        let file = self.spare.take().expect("prepared");
        let run = values.segments;
        let number = run.first + run.count;
        let header = SegmentHeader {
            store: values.server_id.0,
            number,
            size: self.config.segment_size,
            first: last.index + 1,
            prev_term: last.term,
            prev_hash: last.hash,
        };
        if let Err(e) = file.write_at(0, &header.encode()) {
            return Err(self.failed(e));
        }
        let listed = Segments {
            first: run.first,
            count: run.count + 1,
        };
        self.update_with(true, |v| {
            v.segments = listed;
            v.cut_count = cut_count;
        })?;
        self.segs.push(Seg {
            number,
            header,
            file,
        });
        self.write = Some(SEGMENT_HEADER);
        self.unreadable = None;
        Ok(CutDone {
            cut_count: Some(cut_count),
            unreadable_end: Some(unreadable.end),
            fence: Some(number),
            ..CutDone::nothing(last)
        })
    }

    /// Retire segments from the front once nothing needs their entries:
    /// every segment whose entries all come before `oldest_needed`, except
    /// the last. `state` stops listing them first (both slots), then their
    /// files are removed and the folder flushed, so that `state` never lists
    /// a segment that is gone (format section 3.10). Returns how many were
    /// retired. Refused when `oldest_needed` is past the entry after the
    /// durable one. No cut reaches back before `oldest_needed` afterwards.
    pub fn retire(&mut self, oldest_needed: u64) -> Result<u64, LogError> {
        self.usable()?;
        self.check_lock()?;
        if oldest_needed > self.durable.index.saturating_add(1) {
            return Err(LogError::Refused("entries that are not durable are needed"));
        }
        self.floor = self.floor.max(oldest_needed.saturating_sub(1));
        let mut count = 0usize;
        while count + 1 < self.segs.len() && self.segs[count + 1].header.first <= oldest_needed {
            count += 1;
        }
        if count == 0 {
            return Ok(0);
        }
        let old = self.segments();
        let run = Segments {
            first: old.first + count as u64,
            count: old.count - count as u64,
        };
        self.update(|v| v.segments = run)?;
        let gone: Vec<u64> = self.segs.drain(..count).map(|s| s.number).collect();
        for number in gone {
            match self.fs.remove(&segment_path(number)) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(self.failed(e)),
            }
        }
        if let Err(e) = self.fs.flush_folder(FOLDER) {
            return Err(self.failed(e));
        }
        Ok(count as u64)
    }

    /// The durable entries from index `from` on, in order, each checked by
    /// the hash chain as it is read (from the header of the segment that
    /// holds it). A break in the chain, or a log that ends before the
    /// durable entry, is an error of the read ([`LogError::Damaged`]).
    pub fn read(&self, from: u64) -> Result<Reader<'_>, LogError> {
        self.usable()?;
        if from == 0 {
            return Err(LogError::Refused("entries are counted from 1"));
        }
        let until = self.durable.index;
        let scanner = if from > until {
            None
        } else if from < self.first_index() {
            return Err(LogError::Refused("the entries before it are retired"));
        } else {
            let at = segment_for(&self.segs, from).expect("from is at or after the first index");
            // Past the entry the log was opened from, a walk anchors at it.
            let scanner = Scanner::new(&self.segs, at, Keep::Payloads);
            Some(if from > self.anchor.index {
                scanner.anchored_at(self.anchor.index, self.anchor.hash, self.anchor.term)
            } else {
                scanner
            })
        };
        Ok(Reader {
            segs: &self.segs,
            scanner,
            from,
            until,
        })
    }
}

/// The entries a [`Log::read`] returns.
pub struct Reader<'a> {
    segs: &'a [Seg],
    scanner: Option<Scanner<'a>>,
    from: u64,
    until: u64,
}

impl Iterator for Reader<'_> {
    type Item = Result<Entry, LogError>;

    fn next(&mut self) -> Option<Self::Item> {
        let scanner = self.scanner.as_mut()?;
        loop {
            let event = match scanner.next_event() {
                Ok(event) => event,
                Err(fault) => {
                    self.scanner = None;
                    return Some(Err(unreadable(self.segs, fault)));
                }
            };
            match event {
                Event::Entry(found) if found.index < self.from => {}
                Event::Entry(found) => {
                    if found.index >= self.until {
                        self.scanner = None;
                    }
                    return Some(Ok(Entry {
                        index: found.index,
                        term: found.term,
                        kind: found.kind,
                        file: found.file,
                        payload: found.payload,
                        hash: found.hash,
                    }));
                }
                Event::End { seg, offset } => {
                    let (next, _, _) = scanner.position();
                    self.scanner = None;
                    return Some(Err(damage(
                        DamageKind::EndsEarly,
                        self.segs[seg].number,
                        offset,
                        next,
                    )));
                }
                _ => {}
            }
        }
    }
}
