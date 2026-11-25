//! The store's incident journal (`docs/server-storage.md` sections 6.1 and
//! 6.12): what the store gave up, recorded before it gave it up, reported by
//! every open until an administrator acknowledges it. The byte format is
//! section 7 of `docs/server-storage-format.md`.
//!
//! `<store>/incidents` is the store's own file, apart from the log, the page
//! file and `state`, and read without them: a store whose log or page file
//! cannot be read still says what it gave up. It is a sequence of records,
//! each at an offset that is a multiple of [`ALIGN`], each with a kind, a
//! bounded payload, its number and the start count of the open that wrote
//! it, and a SHA-256 hash over all of it.
//!
//! - **Record, flush, then act.** [`Journal::record`] writes a record and
//!   flushes the file (and the store folder, when the call made the file)
//!   before it returns; a caller acts only on a record it returned. A write
//!   or flush that fails leaves the journal refusing every later call: what
//!   reached the device is unknown, and the store stops.
//! - **Nothing is written over.** A record goes after everything the file
//!   holds, at the next multiple of [`ALIGN`]: a record a crash cut short,
//!   or bytes damaged since, stay where they are and are reported.
//! - **A journal that cannot be read in full does not stop the store.** A
//!   missing file, a short one, or bytes that do not check are what the read
//!   found ([`Found`]); every record that checks is read, wherever it is.
//!   Everything read is bounded ([`MAX_FILE`], [`MAX_PAYLOAD`],
//!   [`MAX_RECORDS`]).
//! - **Acknowledging** ([`Journal::acknowledge`]) writes the records not
//!   acknowledged to [`NEW_FILE`] after a base record that carries the next
//!   number, flushes it, renames it over the journal and flushes the store
//!   folder. So the file holds only what no administrator has acknowledged,
//!   and its size is bounded by [`MAX_RECORDS`] such records; numbers never
//!   come again.

use std::fmt;

use ring::digest;

use crate::log::{
    self, ChainFault, Cut, CutWhy, DamagePlan, Earlier, Fate, FileFate, UnreadableEnd,
    SET_ASIDE_REASONS,
};
use crate::page::Hash;
use crate::pagefile::{CheckpointId, GiveUp, GivenUp, Lost};
use crate::state::{Marker, Segments};
use crate::sys::{self, ErrorKind, File, FileOptions, Fs};

/// The journal's name in the store folder.
pub const FILE: &str = "incidents";

/// The name an acknowledgement writes the journal under before it renames
/// it into place.
pub const NEW_FILE: &str = "incidents.new";

/// The format version this code writes, and the newest it reads.
pub const VERSION: u16 = 1;

/// Every record begins at a multiple of this, so that a record that does
/// not check is passed over to the next one that does.
pub const ALIGN: u64 = 512;

/// The largest payload a record holds.
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// The most records not acknowledged the journal holds: a give-up that would
/// pass it is refused, with nothing given up, until an administrator
/// acknowledges what is there.
pub const MAX_RECORDS: usize = 256;

/// The most bytes of the file that are read: the base record and
/// [`MAX_RECORDS`] records of the largest size, aligned. Bytes past it are
/// not read, and the read says so.
pub const MAX_FILE: u64 = (MAX_RECORDS as u64 + 1) * record_room(MAX_PAYLOAD);

const MAGIC: [u8; 8] = *b"OMINCID\0";
const HEADER: usize = 32;
const HASH: usize = 32;
/// The longest text a record keeps of an error.
const MAX_TEXT: usize = 2048;

/// An open's records are numbered from its start count times this: `state`
/// never issues a start count twice, and an open records far fewer, so a
/// number is never used twice, whatever happened to the journal (a base
/// record or the last records damaged or lost).
pub const PER_START: u64 = 1 << 20;

/// The room a record with `payload` bytes takes, aligned.
const fn record_room(payload: usize) -> u64 {
    let len = (HEADER + payload + HASH) as u64;
    len.div_ceil(ALIGN) * ALIGN
}

const fn align_up(at: u64) -> u64 {
    at.div_ceil(ALIGN) * ALIGN
}

/// The record kinds (format section 7.2).
const BASE: u16 = 0;
const CHECKPOINTS: u16 = 1;
const LOG_DAMAGE: u16 = 2;
const UNREADABLE_END: u16 = 3;
const LAST_BATCH: u16 = 4;
const JOURNAL_LOST: u16 = 5;

/// What the store gave up, or was about to: each is recorded, and flushed,
/// before the store acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// The page file's checkpoints and slots given up, exactly as the
    /// approval names them (with the restore points that go with each),
    /// before [`crate::pagefile::PageFile::open_giving_up`] is called with
    /// it: by the store itself for damage (`by_administrator` false), or by
    /// the administrator's explicit call for a read that keeps failing.
    /// `damaged` says which the open found; `why` is its text.
    Checkpoints {
        approval: GiveUp,
        damaged: bool,
        by_administrator: bool,
        why: String,
    },
    /// An acceptance of the log's damage, its plan exactly (what is kept,
    /// each file's fate with its entries and where it goes,
    /// [`DamagePlan::prefix`]), before the acceptance changes anything.
    LogDamage(Box<DamagePlan>),
    /// Whatever may follow the log's unreadable end `end`, after `last`,
    /// before it is given up.
    UnreadableEnd {
        last: log::Position,
        end: UnreadableEnd,
    },
    /// A copy of `state` was passed over and the log holds no whole batch
    /// after the marker: the last batch this server confirmed may be lost.
    /// `marker` is the marker read; `cut` and `unreadable_end` what the
    /// open then does to the log. Recorded before `state`'s mark is settled.
    LastBatch {
        marker: Marker,
        cut: Option<Cut>,
        unreadable_end: Option<UnreadableEnd>,
    },
    /// The journal was missing at an open: incidents recorded before and not
    /// acknowledged are unknown.
    JournalLost,
}

/// A record of the journal: its number (one more than the record before;
/// never used again), the start count of the open that wrote it, and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    pub number: u64,
    pub start_count: u64,
    pub what: What,
}

impl fmt::Display for Incident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incident {}: ", self.number)?;
        match &self.what {
            What::Checkpoints {
                approval,
                by_administrator,
                why,
                ..
            } => {
                let by = if *by_administrator {
                    "by the administrator's call"
                } else {
                    "by the store"
                };
                write!(f, "the page file: {approval}, {by} ({why})")
            }
            What::LogDamage(plan) => {
                write!(
                    f,
                    "the log's damage accepted: kept through entry {}, the marker was at {}",
                    plan.kept.index, plan.marker
                )?;
                for file in &plan.files {
                    match &file.fate {
                        Fate::SetAside { .. } => {
                            if let Some(prefix) = &plan.prefix {
                                write!(
                                    f,
                                    "; segment {} set aside as {}/{prefix}{:016x}",
                                    file.number,
                                    log::FOLDER,
                                    file.number
                                )?;
                            }
                        }
                        Fate::Missing => write!(f, "; segment {} missing", file.number)?,
                        Fate::MovedEarlier { to } => {
                            write!(f, "; segment {} set aside earlier as {to}", file.number)?
                        }
                        Fate::Kept | Fate::Lengthened { .. } => {}
                    }
                }
                Ok(())
            }
            What::UnreadableEnd { last, end } => write!(
                f,
                "whatever may follow entry {} given up, at a place of segment {} (offset {}) \
                 the device could not read",
                last.index, end.segment, end.offset
            ),
            What::LastBatch { marker, .. } => write!(
                f,
                "a copy of the state file was passed over and the log held no whole batch after \
                 entry {}: the last batch this server confirmed may be lost",
                marker.index
            ),
            What::JournalLost => f.write_str(
                "the incident journal was missing: incidents recorded before, not acknowledged, \
                 are unknown",
            ),
        }
    }
}

/// What a read of the journal found besides its records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// There was no journal.
    pub missing: bool,
    /// Offsets of places that are not zero and begin no record that checks
    /// (the first [`MAX_REPORTED`] of them), and how many in all.
    pub damaged: Vec<u64>,
    pub damaged_count: u64,
    /// The last of them comes after the last record that checks: a record a
    /// crash cut short (nothing was acted on it), or one damaged since (what
    /// it said is unknown).
    pub ends_damaged: bool,
    /// The device failed to read these places (offset, length).
    pub unreadable: Vec<(u64, u64)>,
    /// The file is longer than [`MAX_FILE`]: what follows was not read.
    pub too_long: bool,
}

impl Found {
    /// Whether the read found anything to say.
    pub fn is_clean(&self) -> bool {
        *self == Found::default()
    }
}

/// The most damaged places a read lists.
pub const MAX_REPORTED: usize = 16;

/// Why the journal could not be read or written.
#[derive(Debug)]
pub enum JournalError {
    /// A record of a format newer than this code reads.
    NewerFormat { version: u16 },
    /// [`MAX_RECORDS`] records are not acknowledged, or the file would pass
    /// [`MAX_FILE`]: nothing more is recorded, so nothing more is given up,
    /// until an administrator acknowledges them.
    Full,
    /// The record would be larger than [`MAX_PAYLOAD`].
    TooLarge { len: usize },
    /// The disk failed.
    Disk(sys::Error),
    /// An earlier write or flush of the journal failed.
    Failed,
    /// The call breaks a rule.
    Refused(&'static str),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::NewerFormat { version } => write!(
                f,
                "the incident journal holds a record of format version {version}, newer than \
                 this server reads ({VERSION})"
            ),
            JournalError::Full => f.write_str(
                "the incident journal is full of incidents no administrator has acknowledged: \
                 nothing more can be recorded, so nothing more is given up",
            ),
            JournalError::TooLarge { len } => write!(
                f,
                "an incident of {len} bytes is larger than a record holds ({MAX_PAYLOAD})"
            ),
            JournalError::Disk(e) => write!(f, "the incident journal: {e}"),
            JournalError::Failed => f.write_str(
                "an earlier write or flush of the incident journal failed; the store must stop \
                 and be opened again",
            ),
            JournalError::Refused(why) => write!(f, "the incident journal refused: {why}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<sys::Error> for JournalError {
    fn from(e: sys::Error) -> Self {
        JournalError::Disk(e)
    }
}

/// The open journal. The caller holds the store folder's lock for as long
/// as it lives.
pub struct Journal {
    file: Option<Box<dyn File>>,
    /// The records not acknowledged, in order.
    incidents: Vec<Incident>,
    /// What the read found.
    found: Found,
    /// The number the next record takes.
    next: u64,
    /// Where the next record goes.
    end: u64,
    /// The start count records are written with.
    start_count: u64,
    failed: bool,
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Journal")
            .field("incidents", &self.incidents.len())
            .field("found", &self.found)
            .field("next", &self.next)
            .field("end", &self.end)
            .field("failed", &self.failed)
            .finish()
    }
}

impl Journal {
    /// Read the journal of the store folder `fs` (its lock held by the
    /// caller), for an open whose start count is `start_count`. A missing
    /// file reads as no records, and the read says so; nothing is written.
    pub fn open(fs: &dyn Fs, start_count: u64) -> Result<Journal, JournalError> {
        let file = match fs.open(FILE, FileOptions::PLAIN) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Ok(Journal {
                    file: None,
                    incidents: Vec::new(),
                    found: Found {
                        missing: true,
                        ..Found::default()
                    },
                    next: start_count.saturating_mul(PER_START).max(1),
                    end: 0,
                    start_count,
                    failed: false,
                })
            }
            Err(e) => return Err(e.into()),
        };
        let size = file.size()?;
        let (bytes, unreadable) = read_bounded(&*file, size.min(MAX_FILE))?;
        let scan = scan(&bytes, &unreadable)?;
        let found = Found {
            missing: false,
            damaged: scan.damaged.iter().copied().take(MAX_REPORTED).collect(),
            damaged_count: scan.damaged.len() as u64,
            ends_damaged: scan
                .damaged
                .last()
                .is_some_and(|&at| scan.last_record.is_none_or(|last| at > last)),
            unreadable,
            too_long: size > MAX_FILE,
        };
        Ok(Journal {
            file: Some(file),
            incidents: scan.incidents,
            found,
            next: scan.next.max(start_count.saturating_mul(PER_START)),
            end: align_up(size),
            start_count,
            failed: false,
        })
    }

    /// The records not acknowledged, in order.
    pub fn incidents(&self) -> &[Incident] {
        &self.incidents
    }

    /// What the read found besides its records.
    pub fn found(&self) -> &Found {
        &self.found
    }

    /// The segment files acceptances not acknowledged set aside, as their
    /// records say: what a new plan names when it finds them missing.
    pub fn earlier_moves(&self) -> Vec<Earlier> {
        let mut out = Vec::new();
        for incident in &self.incidents {
            if let What::LogDamage(plan) = &incident.what {
                let Some(prefix) = &plan.prefix else { continue };
                for file in &plan.files {
                    if matches!(file.fate, Fate::SetAside { .. }) {
                        out.push(Earlier {
                            number: file.number,
                            first: file.first,
                            last: file.last,
                            moved_to: format!("{}/{prefix}{:016x}", log::FOLDER, file.number),
                        });
                    }
                }
            }
        }
        out
    }

    /// Append `what` and make it durable: the record, the file flushed, and
    /// the store folder flushed when this call made the file. Only then
    /// does it return the incident, and only then may the caller act on it.
    /// Refused, with nothing written, when the journal is full or the record
    /// too large. After a write or flush fails every later call fails.
    pub fn record(&mut self, fs: &dyn Fs, what: What) -> Result<Incident, JournalError> {
        if self.failed {
            return Err(JournalError::Failed);
        }
        if self.incidents.len() >= MAX_RECORDS {
            return Err(JournalError::Full);
        }
        let number = self.next;
        let incident = Incident {
            number,
            start_count: self.start_count,
            what,
        };
        let bytes = encode_record(&incident)?;
        if self.end + bytes.len() as u64 > MAX_FILE {
            return Err(JournalError::Full);
        }
        let made = self.file.is_none();
        let written = (|| -> Result<(), sys::Error> {
            if made {
                let file = match fs.create(FILE, FileOptions::PLAIN) {
                    Ok(file) => file,
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                        fs.open(FILE, FileOptions::PLAIN)?
                    }
                    Err(e) => return Err(e),
                };
                self.end = align_up(self.end.max(file.size()?));
                self.file = Some(file);
            }
            let file = self.file.as_ref().expect("made above");
            file.write_at(self.end, &bytes)?;
            file.flush()?;
            if made {
                fs.flush_folder("")?;
            }
            Ok(())
        })();
        if let Err(e) = written {
            self.failed = true;
            return Err(e.into());
        }
        self.end += bytes.len() as u64;
        self.next = number + 1;
        self.incidents.push(incident.clone());
        Ok(incident)
    }

    /// Acknowledge every record numbered up to `through`, with every place
    /// the read found that does not check (its incident, if any, cannot be
    /// known): the journal is written again holding only the records after
    /// `through`, after a base record that carries the next number, under
    /// [`NEW_FILE`], flushed, renamed over [`FILE`], and the store folder
    /// flushed. A crash leaves the journal as it was or as it is now.
    /// Returns how many records were acknowledged.
    pub fn acknowledge(&mut self, fs: &dyn Fs, through: u64) -> Result<usize, JournalError> {
        if self.failed {
            return Err(JournalError::Failed);
        }
        if through >= self.next {
            return Err(JournalError::Refused(
                "no record of that number has been written",
            ));
        }
        let keep: Vec<Incident> = self
            .incidents
            .iter()
            .filter(|i| i.number > through)
            .cloned()
            .collect();
        let acknowledged = self.incidents.len() - keep.len();
        let mut bytes = base_record(self.next, self.start_count);
        for incident in &keep {
            bytes.extend_from_slice(&encode_record(incident)?);
        }
        let written = (|| -> Result<Box<dyn File>, sys::Error> {
            match fs.remove(NEW_FILE) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let file = fs.create(NEW_FILE, FileOptions::PLAIN)?;
            file.write_at(0, &bytes)?;
            file.flush()?;
            drop(file);
            fs.rename(NEW_FILE, FILE)?;
            fs.flush_folder("")?;
            fs.open(FILE, FileOptions::PLAIN)
        })();
        match written {
            Ok(file) => {
                self.file = Some(file);
                self.incidents = keep;
                self.found = Found::default();
                self.end = bytes.len() as u64;
                Ok(acknowledged)
            }
            Err(e) => {
                self.failed = true;
                Err(e.into())
            }
        }
    }
}

/// Read the first `len` bytes of `file`, a block at a time; a block the
/// device fails to read is read a sector at a time, and a sector it still
/// fails is zeros, and said so. Any other error is the caller's.
/// The bytes read, and the places the device failed to read (offset,
/// length).
type ReadBack = (Vec<u8>, Vec<(u64, u64)>);

fn read_bounded(file: &dyn File, len: u64) -> Result<ReadBack, JournalError> {
    const BLOCK: u64 = 64 * 1024;
    let mut bytes = vec![0u8; len as usize];
    let mut unreadable = Vec::new();
    let mut at = 0u64;
    while at < len {
        let want = (len - at).min(BLOCK);
        let block = &mut bytes[at as usize..(at + want) as usize];
        match read_exact(file, at, block) {
            Ok(n) if (n as u64) < want => {
                // The file ends here.
                bytes.truncate((at + n as u64) as usize);
                break;
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::Read => {
                let mut sector = at;
                while sector < at + want {
                    let n = ALIGN.min(at + want - sector);
                    let place = &mut bytes[sector as usize..(sector + n) as usize];
                    match read_exact(file, sector, place) {
                        Ok(_) => {}
                        Err(e) if e.kind() == ErrorKind::Read => {
                            place.fill(0);
                            unreadable.push((sector, n));
                        }
                        Err(e) => return Err(e.into()),
                    }
                    sector += n;
                }
            }
            Err(e) => return Err(e.into()),
        }
        at += want;
    }
    Ok((bytes, unreadable))
}

/// Read up to `buf.len()` bytes at `offset`, as far as the file reaches.
fn read_exact(file: &dyn File, offset: u64, buf: &mut [u8]) -> sys::Result<usize> {
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

/// What a scan of the journal's bytes found.
struct Scan {
    incidents: Vec<Incident>,
    next: u64,
    damaged: Vec<u64>,
    last_record: Option<u64>,
}

/// Every record that checks, in order, wherever it begins; places that are
/// not zero and begin none are damaged. The next number is one past the
/// last record's and the base record's, and past one more for each damaged
/// place, and each place the device failed to read (`unreadable`, read as
/// zeros), after the last record (each could have begun one): a number is
/// never used twice, though some are skipped.
fn scan(bytes: &[u8], unreadable: &[(u64, u64)]) -> Result<Scan, JournalError> {
    let mut incidents: Vec<Incident> = Vec::new();
    let mut base = 1u64;
    let mut damaged = Vec::new();
    let mut last_record = None;
    let mut at = 0usize;
    while at < bytes.len() {
        let rest = &bytes[at..];
        match decode_record(rest) {
            Ok(Decoded::Record(record, len)) => {
                if incidents.last().is_none_or(|i| record.number > i.number) {
                    incidents.push(record);
                    last_record = Some(at as u64);
                } else {
                    damaged.push(at as u64);
                }
                at = align_up((at + len) as u64) as usize;
                continue;
            }
            Ok(Decoded::Base { next, len }) => {
                base = base.max(next);
                last_record = Some(at as u64);
                at = align_up((at + len) as u64) as usize;
                continue;
            }
            Ok(Decoded::Newer(version)) => return Err(JournalError::NewerFormat { version }),
            Err(()) => {
                let sector = &rest[..rest.len().min(ALIGN as usize)];
                if sector.iter().any(|&b| b != 0) {
                    damaged.push(at as u64);
                }
                at += ALIGN as usize;
            }
        }
    }
    let after = |at: u64| last_record.is_none_or(|last| at > last);
    let after_last = damaged.iter().filter(|&&at| after(at)).count() as u64
        + unreadable
            .iter()
            .filter(|(at, _)| after(*at))
            .map(|(_, len)| len.div_ceil(ALIGN))
            .sum::<u64>();
    let next = incidents
        .last()
        .map_or(1, |i| i.number.saturating_add(1))
        .max(base)
        .saturating_add(after_last);
    Ok(Scan {
        incidents,
        next,
        damaged,
        last_record,
    })
}

enum Decoded {
    Record(Incident, usize),
    Base { next: u64, len: usize },
    Newer(u16),
}

/// The record at the start of `bytes`, with the bytes its hash covers and
/// the hash; `Err` when there is none that checks there.
fn decode_record(bytes: &[u8]) -> Result<Decoded, ()> {
    if bytes.len() < HEADER + HASH || bytes[..8] != MAGIC {
        return Err(());
    }
    let len = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")) as usize;
    if len > MAX_PAYLOAD || bytes.len() < HEADER + len + HASH {
        return Err(());
    }
    let end = HEADER + len;
    let hash = digest::digest(&digest::SHA256, &bytes[..end]);
    if hash.as_ref() != &bytes[end..end + HASH] {
        return Err(());
    }
    let version = u16::from_le_bytes([bytes[8], bytes[9]]);
    if version > VERSION {
        return Ok(Decoded::Newer(version));
    }
    if version != VERSION {
        return Err(());
    }
    let kind = u16::from_le_bytes([bytes[10], bytes[11]]);
    let number = u64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes"));
    let start_count = u64::from_le_bytes(bytes[24..32].try_into().expect("8 bytes"));
    let mut payload = In::new(&bytes[HEADER..end]);
    let what = match kind {
        BASE => {
            let next = payload.u64().ok_or(())?;
            payload.done().ok_or(())?;
            return Ok(Decoded::Base {
                next,
                len: end + HASH,
            });
        }
        CHECKPOINTS => decode_checkpoints(&mut payload),
        LOG_DAMAGE => decode_plan(&mut payload).map(|p| What::LogDamage(Box::new(p))),
        UNREADABLE_END => (|| {
            Some(What::UnreadableEnd {
                last: payload.position()?,
                end: payload.unreadable_end()?,
            })
        })(),
        LAST_BATCH => (|| {
            Some(What::LastBatch {
                marker: Marker {
                    index: payload.u64()?,
                    hash: payload.hash()?,
                    committed: payload.u64()?,
                },
                cut: payload.opt(|p| p.cut())?,
                unreadable_end: payload.opt(|p| p.unreadable_end())?,
            })
        })(),
        JOURNAL_LOST => Some(What::JournalLost),
        _ => None,
    }
    .ok_or(())?;
    payload.done().ok_or(())?;
    if number == 0 {
        return Err(());
    }
    Ok(Decoded::Record(
        Incident {
            number,
            start_count,
            what,
        },
        end + HASH,
    ))
}

/// A record: header, payload, hash, then zeros to the next multiple of
/// [`ALIGN`].
fn record_bytes(kind: u16, number: u64, start_count: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(record_room(payload.len()) as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&number.to_le_bytes());
    out.extend_from_slice(&start_count.to_le_bytes());
    out.extend_from_slice(payload);
    let hash = digest::digest(&digest::SHA256, &out);
    out.extend_from_slice(hash.as_ref());
    out.resize(align_up(out.len() as u64) as usize, 0);
    out
}

fn base_record(next: u64, start_count: u64) -> Vec<u8> {
    record_bytes(BASE, 0, start_count, &next.to_le_bytes())
}

fn encode_record(incident: &Incident) -> Result<Vec<u8>, JournalError> {
    let mut out = Out::default();
    let kind = match &incident.what {
        What::Checkpoints {
            approval,
            damaged,
            by_administrator,
            why,
        } => {
            out.u8(u8::from(*damaged));
            out.u8(u8::from(*by_administrator));
            out.text(why);
            out.give_up(approval);
            CHECKPOINTS
        }
        What::LogDamage(plan) => {
            out.plan(plan)?;
            LOG_DAMAGE
        }
        What::UnreadableEnd { last, end } => {
            out.position(last);
            out.unreadable_end(end);
            UNREADABLE_END
        }
        What::LastBatch {
            marker,
            cut,
            unreadable_end,
        } => {
            out.u64(marker.index);
            out.bytes(&marker.hash);
            out.u64(marker.committed);
            out.opt(cut.as_ref(), Out::cut);
            out.opt(unreadable_end.as_ref(), Out::unreadable_end);
            LAST_BATCH
        }
        What::JournalLost => JOURNAL_LOST,
    };
    if out.0.len() > MAX_PAYLOAD {
        return Err(JournalError::TooLarge { len: out.0.len() });
    }
    Ok(record_bytes(
        kind,
        incident.number,
        incident.start_count,
        &out.0,
    ))
}

fn decode_checkpoints(p: &mut In) -> Option<What> {
    let damaged = p.flag()?;
    let by_administrator = p.flag()?;
    let why = p.text()?;
    let approval = p.give_up()?;
    Some(What::Checkpoints {
        approval,
        damaged,
        by_administrator,
        why,
    })
}

fn decode_plan(p: &mut In) -> Option<DamagePlan> {
    let start = p.position()?;
    let kept = p.position()?;
    let marker = p.u64()?;
    let cut_count = p.opt(In::u64)?;
    let listed_before = p.segments()?;
    let listed = p.segments()?;
    let opens_at = p.position()?;
    let count = p.u32()? as usize;
    // Each file takes at least its number and three flags.
    if count > p.left() / 11 {
        return None;
    }
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let number = p.u64()?;
        let first = p.opt(In::u64)?;
        let last = p.opt(In::u64)?;
        let fate = match p.u8()? {
            0 => Fate::Kept,
            1 => Fate::Lengthened { size: p.u64()? },
            2 => Fate::SetAside {
                reason: SET_ASIDE_REASONS.get(p.u8()? as usize)?,
            },
            3 => Fate::Missing,
            4 => Fate::MovedEarlier { to: p.text()? },
            _ => return None,
        };
        files.push(FileFate {
            number,
            first,
            last,
            fate,
        });
    }
    let tail_copied = p.opt(In::u64)?;
    let before_start = p.opt(|p| {
        Some(ChainFault {
            segment: p.u64()?,
            offset: p.u64()?,
            index: p.u64()?,
            unreadable: p.flag()?,
        })
    })?;
    let prefix = p.opt(In::text)?;
    Some(DamagePlan {
        start,
        kept,
        marker,
        cut_count,
        listed_before,
        listed,
        opens_at,
        files,
        tail_copied,
        before_start,
        prefix,
    })
}

/// A payload being written.
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }

    /// Text, cut to [`MAX_TEXT`] bytes at a character's boundary.
    fn text(&mut self, v: &str) {
        let mut end = v.len().min(MAX_TEXT);
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        self.0.extend_from_slice(&(end as u16).to_le_bytes());
        self.0.extend_from_slice(&v.as_bytes()[..end]);
    }

    fn opt<T>(&mut self, v: Option<&T>, write: impl FnOnce(&mut Out, &T)) {
        match v {
            Some(v) => {
                self.u8(1);
                write(self, v);
            }
            None => self.u8(0),
        }
    }

    fn opt_u64(&mut self, v: Option<u64>) {
        self.opt(v.as_ref(), |o, v| o.u64(*v));
    }

    fn position(&mut self, p: &log::Position) {
        self.u64(p.index);
        self.u64(p.term);
        self.bytes(&p.hash);
    }

    fn unreadable_end(&mut self, e: &UnreadableEnd) {
        self.u64(e.segment);
        self.u64(e.offset);
        self.u64(e.index);
    }

    fn cut(&mut self, c: &Cut) {
        self.u64(c.index);
        self.u64(c.segment);
        self.u64(c.offset);
        self.u64(c.dropped);
        self.u8(match c.why {
            CutWhy::NotWhole => 0,
            CutWhy::OlderCut => 1,
            CutWhy::Segments => 2,
        });
    }

    fn segments(&mut self, s: &Segments) {
        self.u64(s.first);
        self.u64(s.count);
    }

    fn checkpoint(&mut self, id: &CheckpointId) {
        self.u64(id.number);
        self.u64(id.meta.0);
        self.bytes(&id.meta.1 .0);
    }

    fn give_up(&mut self, g: &GiveUp) {
        self.u32(g.checkpoints.len() as u32);
        for c in &g.checkpoints {
            self.checkpoint(&c.checkpoint);
            match &c.lost {
                Lost::Points(points) => {
                    self.u8(0);
                    self.u32(points.len() as u32);
                    for (name, at) in points {
                        self.text(name);
                        self.u64(*at);
                    }
                }
                Lost::Unknown => self.u8(1),
            }
        }
        self.u32(g.slots.len() as u32);
        for &slot in &g.slots {
            self.u64(slot as u64);
        }
        self.checkpoint(&g.onto);
    }

    fn plan(&mut self, plan: &DamagePlan) -> Result<(), JournalError> {
        self.position(&plan.start);
        self.position(&plan.kept);
        self.u64(plan.marker);
        self.opt_u64(plan.cut_count);
        self.segments(&plan.listed_before);
        self.segments(&plan.listed);
        self.position(&plan.opens_at);
        self.u32(plan.files.len() as u32);
        for file in &plan.files {
            self.u64(file.number);
            self.opt_u64(file.first);
            self.opt_u64(file.last);
            match &file.fate {
                Fate::Kept => self.u8(0),
                Fate::Lengthened { size } => {
                    self.u8(1);
                    self.u64(*size);
                }
                Fate::SetAside { reason } => {
                    let at = SET_ASIDE_REASONS.iter().position(|r| r == reason).ok_or(
                        JournalError::Refused("a plan gives a reason the journal does not know"),
                    )?;
                    self.u8(2);
                    self.u8(at as u8);
                }
                Fate::Missing => self.u8(3),
                Fate::MovedEarlier { to } => {
                    self.u8(4);
                    self.text(to);
                }
            }
            if self.0.len() > MAX_PAYLOAD {
                return Err(JournalError::TooLarge { len: self.0.len() });
            }
        }
        self.opt_u64(plan.tail_copied);
        self.opt(plan.before_start.as_ref(), |o, f| {
            o.u64(f.segment);
            o.u64(f.offset);
            o.u64(f.index);
            o.u8(u8::from(f.unreadable));
        });
        self.opt(plan.prefix.as_ref(), |o, p| o.text(p));
        Ok(())
    }
}

/// A payload being read: every read checks that the bytes are there.
struct In<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> In<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        In { bytes, at: 0 }
    }

    fn left(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn done(&self) -> Option<()> {
        (self.at == self.bytes.len()).then_some(())
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn flag(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn hash(&mut self) -> Option<[u8; 32]> {
        self.take(32)?.try_into().ok()
    }

    fn text(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        if len > MAX_TEXT {
            return None;
        }
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }

    fn opt<T>(&mut self, read: impl FnOnce(&mut In<'a>) -> Option<T>) -> Option<Option<T>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(read(self)?)),
            _ => None,
        }
    }

    fn position(&mut self) -> Option<log::Position> {
        Some(log::Position {
            index: self.u64()?,
            term: self.u64()?,
            hash: self.hash()?,
        })
    }

    fn unreadable_end(&mut self) -> Option<UnreadableEnd> {
        Some(UnreadableEnd {
            segment: self.u64()?,
            offset: self.u64()?,
            index: self.u64()?,
        })
    }

    fn cut(&mut self) -> Option<Cut> {
        Some(Cut {
            index: self.u64()?,
            segment: self.u64()?,
            offset: self.u64()?,
            dropped: self.u64()?,
            why: match self.u8()? {
                0 => CutWhy::NotWhole,
                1 => CutWhy::OlderCut,
                2 => CutWhy::Segments,
                _ => return None,
            },
        })
    }

    fn segments(&mut self) -> Option<Segments> {
        Some(Segments {
            first: self.u64()?,
            count: self.u64()?,
        })
    }

    fn checkpoint(&mut self) -> Option<CheckpointId> {
        Some(CheckpointId {
            number: self.u64()?,
            meta: (self.u64()?, Hash(self.hash()?)),
        })
    }

    fn give_up(&mut self) -> Option<GiveUp> {
        let count = self.u32()? as usize;
        // Each takes at least an identity and a flag.
        if count > self.left() / 49 {
            return None;
        }
        let mut checkpoints = Vec::with_capacity(count);
        for _ in 0..count {
            let checkpoint = self.checkpoint()?;
            let lost = match self.u8()? {
                0 => {
                    let n = self.u32()? as usize;
                    if n > self.left() / 10 {
                        return None;
                    }
                    let mut points = Vec::with_capacity(n);
                    for _ in 0..n {
                        points.push((self.text()?, self.u64()?));
                    }
                    Lost::Points(points)
                }
                1 => Lost::Unknown,
                _ => return None,
            };
            checkpoints.push(GivenUp { checkpoint, lost });
        }
        let n = self.u32()? as usize;
        if n > self.left() / 8 {
            return None;
        }
        let mut slots = Vec::with_capacity(n);
        for _ in 0..n {
            slots.push(usize::try_from(self.u64()?).ok()?);
        }
        let onto = self.checkpoint()?;
        Some(GiveUp {
            checkpoints,
            slots,
            onto,
        })
    }
}

#[cfg(test)]
mod tests;
