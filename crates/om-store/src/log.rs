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
//!   records the marker (the last entry's index and hash, and the highest
//!   committed position heard) in `state`. An entry is durable when the
//!   marker covers it.
//! - **Opening** ([`Log::open`]) checks the log against the marker:
//!   everything through it must be there, whole and chained, else the log
//!   is damaged ([`LogError::Damaged`]) and nothing is changed. Whole
//!   batches after the marker are kept and the marker moved over them; what
//!   is incomplete after it is cut.
//! - **Cutting** on a leader's instruction ([`Log::cut`]) lowers the marker
//!   first when it covers what is cut.
//!
//! The caller drives everything; nothing here runs on its own. After a
//! write, flush or `state` update fails, the log refuses every later call
//! ([`LogError::Poisoned`]): what reached the device is unknown, and the
//! store stops and opens again (section 6.12). A full disk refuses an
//! append that would need a new segment before anything is written.
//!
//! The log reaches the disk only through [`crate::sys`], and `state` only
//! through [`State`], whose lock it checks on every call that writes.

mod format;
mod scan;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use crate::state::{Marker, Segments, State, StateError};
use crate::sys::{self, ErrorKind, File, FileOptions, Fs};

use format::{encode_entry, segment_number, segment_path, BatchHeader, SegmentHeader, CUT_MARK};
pub use format::{
    HeaderFault, BATCH_HEADER, ENTRY_OVERHEAD, MAX_SEGMENT, MIN_SEGMENT, SEGMENT_HEADER, VERSION,
    ZERO_HASH,
};
use scan::{Event, Found, ReadFault, Scanner, Seg};

/// The folder of the log's segments in the store folder.
pub const FOLDER: &str = "log";

/// The log's sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The size of a new segment in bytes, its header included. Segments
    /// made earlier keep the size their header records.
    pub segment_size: u64,
    /// The largest payload an entry may have. An entry never spans
    /// segments, so an entry with the largest payload must fit in an empty
    /// segment; larger work is a group (section 6.9).
    pub max_payload: u32,
}

impl Config {
    /// 64 MiB segments, payloads of up to 16 MiB.
    pub const DEFAULT: Config = Config {
        segment_size: 64 << 20,
        max_payload: 16 << 20,
    };

    fn check(&self) -> Result<(), LogError> {
        let room = self
            .segment_size
            .saturating_sub(SEGMENT_HEADER + BATCH_HEADER + ENTRY_OVERHEAD);
        if !(MIN_SEGMENT..=MAX_SEGMENT).contains(&self.segment_size) {
            return Err(LogError::Refused("the segment size is out of bounds"));
        }
        if u64::from(self.max_payload) > room {
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
    /// The highest committed position heard, as the marker recorded it.
    pub committed: u64,
    /// How many entries after the marker were found whole and kept.
    pub kept: u64,
    /// What followed the durable entry and was cut, if anything did.
    pub cut: Option<Cut>,
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
    /// `state` failed or refused; the log is poisoned.
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
    /// The call breaks a rule; nothing was written.
    Refused(&'static str),
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
            LogError::Refused(why) => write!(f, "log refused: {why}"),
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
    config: Config,
    store: [u8; 16],
    /// The segments and the marker as last recorded in `state`.
    run: Segments,
    marker: Marker,
    /// The listed segments, in order.
    segs: Vec<Seg>,
    /// Where the next batch goes in the last segment (none without one).
    write: Option<u64>,
    durable: Position,
    appended: Position,
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
            .field("run", &self.run)
            .field("marker", &self.marker)
            .field("write", &self.write)
            .field("durable", &self.durable)
            .field("appended", &self.appended)
            .field("batch", &self.batch.len())
            .field("spare", &self.spare.is_some())
            .field("poisoned", &self.poisoned)
            .finish()
    }
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

/// Read up to `buf.len()` bytes at `offset`, as far as the file reaches.
fn read_full(file: &dyn File, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = file.read_at(offset + done as u64, &mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}

/// Whether the bytes of `file` from `offset`, `len` of them or to its end,
/// are all zero.
fn zero_at(file: &dyn File, offset: u64, len: u64) -> sys::Result<bool> {
    let mut buf = vec![0u8; len as usize];
    let n = read_full(file, offset, &mut buf)?;
    Ok(buf[..n].iter().all(|&b| b == 0))
}

/// A batch as written: its header, then its entries.
fn batch_bytes(entries: &[Pending]) -> Vec<u8> {
    let length: u64 = entries.iter().map(|e| e.bytes.len() as u64).sum();
    let first = entries.first().expect("a batch has entries").position.index;
    let last = entries.last().expect("a batch has entries").position.hash;
    let header = BatchHeader {
        count: u32::try_from(entries.len()).expect("a batch fits in a segment"),
        first,
        length,
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
fn segment_for(segs: &[Seg], index: u64) -> Option<usize> {
    segs.iter().rposition(|s| s.header.first <= index)
}

impl Log {
    /// Open the log of the store whose `state` is given (section 6.7 step
    /// 3), reading it from `start`: the entry a checkpoint covers, or
    /// [`Position::START`].
    ///
    /// Every listed segment must be there with a valid header. Every entry
    /// from `start` through the marker must be there, whole and chained,
    /// with the marker's hash at its index, and the entry at `start` with
    /// its hash; else the log is damaged and nothing is changed. When
    /// `start` covers the marker's entry it answers for it. After the marker
    /// (or `start`, whichever is later), whole batches are kept; what
    /// follows the first that is not whole is cut. Then, in this order: the
    /// segments holding entries after the marker are flushed; the place the
    /// log now ends is zeroed (format section 3.6), so that the cut holds;
    /// the marker is moved to the last entry and listed segments past it
    /// are dropped, in one `state` update; and segment files `state` does
    /// not list are removed. A crash at any point leaves a log the next open
    /// reads to the same end.
    pub fn open(
        fs: Arc<dyn Fs>,
        state: &mut State,
        config: Config,
        start: Position,
    ) -> Result<(Log, Opened), LogError> {
        config.check()?;
        if !state.holds_lock_of(&*fs) {
            return Err(LogError::Refused(
                "the state's lock is not this store folder's, or is no longer held",
            ));
        }
        let values = *state.values();
        let run = values.segments;
        let marker = values.marker;
        let store = values.server_id.0;
        let Some(after_start) = start.index.checked_add(1) else {
            return Err(LogError::Refused("the start is at the last index"));
        };

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
            let n = match read_full(&*file, 0, &mut bytes) {
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
        let mut tail_segs = BTreeSet::new();
        let mut kept = 0u64;
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
            let mut scanner = Scanner::new(&segs, from, false);
            // Entries after `firm` wait for their batch to prove whole.
            let mut tail: Vec<Found> = Vec::new();
            let stop = loop {
                match scanner.next_event().map_err(|f| unreadable(&segs, f))? {
                    Event::Batch => {}
                    Event::Entry(found) if found.index <= firm => {
                        check(&position(&found), found.seg, found.offset)?;
                        if found.index > marker.index {
                            tail_segs.insert(found.seg);
                            kept += 1;
                        }
                        end = Some((found.seg, found.offset + found.size));
                        last = position(&found);
                    }
                    Event::Entry(found) => tail.push(found),
                    Event::BatchEnd { whole: true } => {
                        for found in tail.drain(..) {
                            tail_segs.insert(found.seg);
                            kept += 1;
                            end = Some((found.seg, found.offset + found.size));
                            last = position(&found);
                        }
                    }
                    Event::BatchEnd { whole: false } if !tail.is_empty() => {
                        let (seg, offset) = end.expect("set before the walk");
                        break (seg, offset);
                    }
                    Event::BatchEnd { whole: false } => {}
                    Event::Segment { seg } => end = Some((seg, SEGMENT_HEADER)),
                    Event::End { seg, offset } => break (seg, offset),
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
            return Err(damage(DamageKind::EndsEarly, run.first, 0, start.index + 1));
        }

        // The log holds what it must. Make what it keeps durable, then the
        // cut, then record both.
        if no_folder {
            fs.create_folder(FOLDER).map_err(LogError::Disk)?;
            fs.flush_folder("").map_err(LogError::Disk)?;
        }
        for &seg in &tail_segs {
            segs[seg].file.flush().map_err(LogError::Disk)?;
        }
        let mut cut = None;
        if let Some((seg, offset)) = end {
            let size = segs[seg].header.size;
            let len = CUT_MARK.min(size - offset);
            let clean = zero_at(&*segs[seg].file, offset, len).map_err(|error| {
                if error.kind() == ErrorKind::Read {
                    LogError::Unreadable {
                        segment: segs[seg].number,
                        offset,
                        error,
                    }
                } else {
                    LogError::Disk(error)
                }
            })?;
            if !clean {
                segs[seg]
                    .file
                    .write_at(offset, &vec![0u8; len as usize])
                    .map_err(LogError::Disk)?;
            }
            let dropped = (segs.len() - seg - 1) as u64;
            if !clean || dropped > 0 {
                cut = Some(Cut {
                    index: last.index + 1,
                    segment: segs[seg].number,
                    offset,
                    dropped,
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
        if !removed.is_empty() {
            fs.flush_folder(FOLDER).map_err(LogError::Disk)?;
        }

        let opened = Opened {
            durable: last,
            committed: marker.committed,
            kept,
            cut,
            removed,
        };
        let log = Log {
            fs,
            config,
            store,
            run: listed,
            marker: new_marker,
            write: end.map(|(_, offset)| offset),
            segs,
            durable: last,
            appended: last,
            batch: Vec::new(),
            here: 0,
            there: 0,
            spare: None,
            poisoned: false,
        };
        Ok((log, opened))
    }

    /// The last durable entry: the one the marker covers.
    pub fn durable(&self) -> Position {
        self.durable
    }

    /// The last entry appended, durable or in the open batch.
    pub fn appended(&self) -> Position {
        self.appended
    }

    /// The highest committed position heard, as the marker records it.
    pub fn committed(&self) -> u64 {
        self.marker.committed
    }

    /// The first index the log holds (the entries before it were retired or
    /// are covered by the checkpoint it was opened from).
    pub fn first_index(&self) -> u64 {
        self.segs
            .first()
            .map_or(self.durable.index + 1, |s| s.header.first)
    }

    /// The segments in use, as `state` lists them.
    pub fn segments(&self) -> Segments {
        self.run
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

    /// The disk failed in a call that writes: the log stops.
    fn failed(&mut self, error: sys::Error) -> LogError {
        self.poisoned = true;
        LogError::Disk(error)
    }

    fn state_failed(&mut self, error: StateError) -> LogError {
        self.poisoned = true;
        LogError::State(error)
    }

    /// `state` is the one the log was opened with, as the log last wrote it.
    fn check_state(&self, state: &State) -> Result<(), LogError> {
        let values = state.values();
        if !state.holds_lock_of(&*self.fs) || values.server_id.0 != self.store {
            return Err(LogError::Refused(
                "the state given is not this store's, or its lock is no longer held",
            ));
        }
        if values.segments != self.run || values.marker != self.marker {
            return Err(LogError::Refused(
                "the state's segments or marker changed outside the log",
            ));
        }
        Ok(())
    }

    /// Make the next segment ahead of need (format section 3.7): create its
    /// file, take its space, flush it and flush the folder, so that a batch
    /// that reaches it never waits for an allocation and never fails half
    /// way for lack of space. Nothing to do when it is ready. A disk too full
    /// for it fails with [`LogError::NoSpace`], and an allocation that fails
    /// part way with [`LogError::Disk`]; neither stops the log, since
    /// nothing listed was touched, and a later call finishes the file. A
    /// failed flush stops the log.
    pub fn prepare(&mut self) -> Result<(), LogError> {
        self.usable()?;
        if self.spare.is_some() {
            return Ok(());
        }
        let number = self
            .run
            .first
            .checked_add(self.run.count)
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
    /// added, when the payload is too large, the term is lower than the
    /// last entry's, the batch would span more than two segments
    /// ([`LogError::BatchFull`]), or the entry needs the next segment and
    /// the disk has no room for it ([`LogError::NoSpace`]).
    pub fn append(
        &mut self,
        term: u64,
        kind: u16,
        file: u64,
        payload: &[u8],
    ) -> Result<Position, LogError> {
        self.usable()?;
        if payload.len() > self.config.max_payload as usize {
            return Err(LogError::TooLarge {
                len: payload.len(),
                max: self.config.max_payload,
            });
        }
        if term < self.appended.term {
            return Err(LogError::Refused(
                "an entry's term is lower than the term of the entry before it",
            ));
        }
        let index = self
            .appended
            .index
            .checked_add(1)
            .ok_or(LogError::Refused("the log is at its last index"))?;
        let size = ENTRY_OVERHEAD + payload.len() as u64;
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

    /// Write the open batch and record the marker over it (format section
    /// 3.8), with `committed`, when given, as the highest committed position
    /// heard (it never goes down). Returns the durable position.
    ///
    /// The entries that fit in the current segment are written there as one
    /// batch, with one synchronous write; the rest go to the next segment as
    /// a second batch, written with its header in one synchronous write. A
    /// synchronous write is on the device when it returns (`crate::sys`,
    /// durability), so the batch is durable before `state` is updated: the
    /// marker alone when the batch stayed in one segment (one write and one
    /// flush of `state`), the marker and the new segment together otherwise
    /// (both slots). Any failure stops the log; an entry the failed call
    /// wrote may be found whole at the next open, and its outcome was not
    /// reported.
    pub fn flush_batch(
        &mut self,
        state: &mut State,
        committed: Option<u64>,
    ) -> Result<Position, LogError> {
        self.usable()?;
        self.check_state(state)?;
        let committed = committed.map_or(self.marker.committed, |c| c.max(self.marker.committed));
        if self.batch.is_empty() {
            if committed != self.marker.committed {
                self.set_marker(state, self.durable, committed, None)?;
            }
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
            let bytes = batch_bytes(here);
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
            let number = self.run.first + self.run.count;
            let before = here.last().map_or(self.durable, |e| e.position);
            let header = SegmentHeader {
                store: self.store,
                number,
                size: self.config.segment_size,
                first: before.index + 1,
                prev_term: before.term,
                prev_hash: before.hash,
            };
            let mut bytes = header.encode().to_vec();
            bytes.extend_from_slice(&batch_bytes(there));
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
        self.set_marker(state, last, committed, new_seg)?;
        self.write = write;
        self.durable = last;
        self.here = 0;
        self.there = 0;
        Ok(last)
    }

    /// Record the highest committed position heard, `committed`, in the
    /// marker on its own (it never goes down): one write and one flush of
    /// `state`.
    pub fn set_committed(&mut self, state: &mut State, committed: u64) -> Result<(), LogError> {
        self.usable()?;
        self.check_state(state)?;
        if committed > self.marker.committed {
            self.set_marker(state, self.durable, committed, None)?;
        }
        Ok(())
    }

    /// Record the marker at `at` with `committed`, and list `new_seg` with
    /// it when given.
    fn set_marker(
        &mut self,
        state: &mut State,
        at: Position,
        committed: u64,
        new_seg: Option<Seg>,
    ) -> Result<(), LogError> {
        let marker = Marker {
            index: at.index,
            hash: at.hash,
            committed,
        };
        let mut run = self.run;
        if new_seg.is_some() {
            run.count += 1;
        }
        if let Err(e) = state.update(|v| {
            v.marker = marker;
            v.segments = run;
        }) {
            return Err(self.state_failed(e));
        }
        self.marker = marker;
        self.run = run;
        if let Some(seg) = new_seg {
            self.segs.push(seg);
        }
        Ok(())
    }

    /// Remove the entries from `from` on (section 7.1, a follower whose log
    /// differs from the leader's). Returns the last entry left.
    ///
    /// Refused at or before the highest committed position heard, and
    /// before the first entry the log holds. Entries of the open batch are
    /// dropped in memory. When the marker covers entries that are cut, the
    /// lowered marker is recorded first, in both slots of `state` and with
    /// the listed segments after the one that holds `from` dropped; then
    /// the first cut entry (or its batch's header) is zeroed with a
    /// synchronous write, so that it cannot be read again; then the dropped
    /// segments' files are removed and the folder flushed (format section
    /// 3.9). Appends continue from the zeroed place.
    pub fn cut(&mut self, state: &mut State, from: u64) -> Result<Position, LogError> {
        self.usable()?;
        self.check_state(state)?;
        if from == 0 || from <= self.marker.committed {
            return Err(LogError::Refused(
                "entries at or before the committed position are never cut",
            ));
        }
        if from > self.appended.index {
            return Ok(self.appended);
        }
        if from > self.durable.index {
            let keep = (from - self.durable.index - 1) as usize;
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
            return Ok(self.appended);
        }
        if from < self.first_index() {
            return Err(LogError::Refused("the entries before it are retired"));
        }
        let at = segment_for(&self.segs, from).expect("from is at or after the first index");
        let (seg, offset, before) = {
            let header = self.segs[at].header;
            let mut before = Position {
                index: header.first - 1,
                term: header.prev_term,
                hash: header.prev_hash,
            };
            let mut scanner = Scanner::new(&self.segs, at, false);
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
                    Ok(Event::End { seg, offset, .. }) => {
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
            committed: self.marker.committed,
        };
        let run = Segments {
            first: self.run.first,
            count: seg as u64 + 1,
        };
        if let Err(e) = state.update(|v| {
            v.marker = marker;
            v.segments = run;
        }) {
            return Err(self.state_failed(e));
        }
        self.marker = marker;
        let dropped = self.run.count != run.count;
        // The spare was made for the number after the old run.
        let spare = self.run.first + self.run.count;
        self.run = run;
        let size = self.segs[seg].header.size;
        let zeros = vec![0u8; CUT_MARK.min(size - offset) as usize];
        if let Err(e) = self.segs[seg].file.write_at(offset, &zeros) {
            return Err(self.failed(e));
        }
        let mut remove: Vec<u64> = self.segs.drain(seg + 1..).map(|s| s.number).collect();
        if dropped && self.spare.take().is_some() {
            remove.push(spare);
        }
        for number in &remove {
            match self.fs.remove(&segment_path(*number)) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(self.failed(e)),
            }
        }
        if !remove.is_empty() {
            if let Err(e) = self.fs.flush_folder(FOLDER) {
                return Err(self.failed(e));
            }
        }
        self.write = Some(offset);
        self.batch.clear();
        self.here = 0;
        self.there = 0;
        self.durable = before;
        self.appended = before;
        Ok(before)
    }

    /// Retire segments from the front once nothing needs their entries:
    /// every segment whose entries all come before `oldest_needed`, except
    /// the last. `state` stops listing them first (both slots), then their
    /// files are removed and the folder flushed, so that `state` never lists
    /// a segment that is gone (format section 3.10). Returns how many were
    /// retired. Refused when `oldest_needed` is past the entry after the
    /// durable one.
    pub fn retire(&mut self, state: &mut State, oldest_needed: u64) -> Result<u64, LogError> {
        self.usable()?;
        self.check_state(state)?;
        if oldest_needed > self.durable.index.saturating_add(1) {
            return Err(LogError::Refused("entries that are not durable are needed"));
        }
        let mut count = 0usize;
        while count + 1 < self.segs.len() && self.segs[count + 1].header.first <= oldest_needed {
            count += 1;
        }
        if count == 0 {
            return Ok(0);
        }
        let run = Segments {
            first: self.run.first + count as u64,
            count: self.run.count - count as u64,
        };
        if let Err(e) = state.update(|v| v.segments = run) {
            return Err(self.state_failed(e));
        }
        self.run = run;
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
            Some(Scanner::new(&self.segs, at, true))
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
                Event::End { seg, offset, .. } => {
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
