//! The store's `state` file (`docs/server-storage.md` section 6.1): this
//! server's id, its start count, its term and vote, its log segments, the
//! marker (section 6.3) and the repair record (section 7.5). The byte
//! format is section 1 of `docs/server-storage-format.md`.
//!
//! The file holds two copies in fixed slots, each with a number and a
//! SHA-256 hash. An update writes the next number into the slot that does
//! not hold the current copy, in place, and flushes: one small write and
//! one flush, since the marker is updated once per commit batch. The valid
//! copy with the highest number counts, so a crash during an update leaves
//! either the copy before it or the new one. With no valid copy the open
//! fails ([`StateError::NoValidCopy`]) and the caller decides what follows.
//!
//! The file is made once, by [`State::create`], under another name and
//! renamed into place, so that `state` exists only complete.
//!
//! An open does not trust that what it reads is on the device: an earlier
//! process may have stopped between a write and its flush, or seen a flush
//! fail, and the system's cache then shows bytes the device may not hold
//! (section 6.12). So before it writes the next copy over the other slot,
//! an open flushes the store folder (for `state`'s own name) and writes the
//! copy it read back into its slot and flushes it.
//!
//! After a write or flush of `state` fails, the [`State`] refuses further
//! updates: what reached the device is unknown, and the store must stop
//! and be opened again (section 6.12).

use std::fmt;

use ring::digest;

use crate::sys::{self, ErrorKind, File, FileOptions, FolderLock, Fs, Random};

/// The file's name in the store folder.
pub const FILE: &str = "state";

/// The name a new `state` is written under before it is renamed into
/// place.
pub const NEW_FILE: &str = "state.new";

/// The format version this code writes, and the newest it reads.
pub const VERSION: u16 = 1;

/// The size of each copy's slot. A copy never exceeds it.
pub const SLOT: usize = 4096;

const MAGIC: [u8; 8] = *b"OMSTATE\0";
const HEADER: usize = 24;
const HASH: usize = 32;
/// The body of a version 1 copy.
const BODY_V1: usize = 122;

/// A server's id: 16 random bytes drawn when its store is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerId(pub [u8; 16]);

/// The log's segments: numbered files, used as a contiguous run that is
/// retired from the front and cut from the back (section 6.3), so the list
/// is held as its first number and its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segments {
    /// The number of the oldest segment in use; with none in use, the
    /// number the next one gets.
    pub first: u64,
    pub count: u64,
}

/// The marker (section 6.3): the last entry a flushed batch holds, and the
/// highest committed position this server has heard of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    pub index: u64,
    pub hash: [u8; 32],
    pub committed: u64,
}

/// The repair record (section 7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repair {
    /// The marker's index when the server entered repair. It does not
    /// change until the server leaves repair.
    pub required_end: u64,
}

/// What a copy of `state` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Values {
    pub server_id: ServerId,
    /// How many times the store has been opened, its creation included.
    pub start_count: u64,
    pub term: u64,
    /// The server voted for in `term`.
    pub vote: Option<ServerId>,
    pub segments: Segments,
    pub marker: Marker,
    /// Present while the server is in repair.
    pub repair: Option<Repair>,
}

/// Why a copy is not valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The file ends before the copy does.
    Short,
    /// It does not begin with the format's mark.
    Magic,
    /// Its stated length does not fit its slot.
    Length,
    /// Its hash does not match its content.
    Hash,
    /// The hash matches, but the content breaks the format.
    Malformed,
}

impl Invalid {
    fn words(self) -> &'static str {
        match self {
            Invalid::Short => "the file ends before it",
            Invalid::Magic => "it does not begin as a copy does",
            Invalid::Length => "its length does not fit",
            Invalid::Hash => "its hash does not match",
            Invalid::Malformed => "its content breaks the format",
        }
    }
}

/// What an open read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The number of the copy it read.
    pub number: u64,
    /// The slot that copy was in.
    pub slot: usize,
    /// The other slot: its copy's number, or why it is not valid.
    pub other: Result<u64, Invalid>,
}

/// Why `state` could not be opened, created or updated.
#[derive(Debug)]
pub enum StateError {
    /// The store folder has no `state`: it was never set up, or the file
    /// was lost.
    Missing,
    /// Neither copy is valid: the server does not know what it confirmed.
    NoValidCopy { first: Invalid, second: Invalid },
    /// A copy is of a format newer than this code reads.
    NewerFormat { version: u16 },
    /// The disk failed.
    Disk(sys::Error),
    /// An earlier write or flush of `state` failed.
    Failed,
    /// The update breaks a rule of the record (nothing was written).
    Refused(&'static str),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Missing => f.write_str(
                "the store folder has no state file: it was never set up, or the file was lost",
            ),
            StateError::NoValidCopy { first, second } => write!(
                f,
                "neither copy of the state file is valid (the first: {}; the second: {}), so \
                 this server does not know what it confirmed and does not start; restore the \
                 store from a backup, or add the server to its cluster again as a new server",
                first.words(),
                second.words()
            ),
            StateError::NewerFormat { version } => write!(
                f,
                "the state file has format version {version}, newer than this server reads \
                 ({VERSION})"
            ),
            StateError::Disk(e) => write!(f, "{e}"),
            StateError::Failed => f.write_str(
                "an earlier write or flush of the state file failed; the store must stop and be \
                 opened again",
            ),
            StateError::Refused(why) => write!(f, "state update refused: {why}"),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StateError::Disk(e) => Some(e),
            _ => None,
        }
    }
}

impl From<sys::Error> for StateError {
    fn from(e: sys::Error) -> Self {
        StateError::Disk(e)
    }
}

/// The open `state` file (module docs).
pub struct State {
    file: Box<dyn File>,
    values: Values,
    /// The current copy's number and slot.
    number: u64,
    slot: usize,
    opened: Option<Opened>,
    /// A write or flush failed: nothing more is written.
    failed: bool,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("values", &self.values)
            .field("number", &self.number)
            .field("slot", &self.slot)
            .field("opened", &self.opened)
            .field("failed", &self.failed)
            .finish()
    }
}

impl State {
    /// Set up `state` in a store folder that has none, for a new server
    /// with an id drawn from `random`. The creation counts as the first
    /// start. `_lock` shows that the caller holds the store folder.
    ///
    /// It writes both copies to [`NEW_FILE`], flushes, renames it to
    /// [`FILE`] and flushes the folder, so that `state` never exists
    /// incomplete. A [`NEW_FILE`] left by a creation that did not finish
    /// is replaced. An existing `state` fails with
    /// [`sys::ErrorKind::AlreadyExists`].
    pub fn create(
        fs: &dyn Fs,
        random: &dyn Random,
        _lock: &FolderLock,
    ) -> Result<Self, StateError> {
        match fs.open(FILE, FileOptions::PLAIN) {
            Ok(_) => {
                return Err(StateError::Disk(sys::Error::new(
                    ErrorKind::AlreadyExists,
                    sys::Op::Create,
                    FILE,
                )))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        match fs.remove(NEW_FILE) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut id = [0u8; 16];
        random.fill(&mut id)?;
        let values = Values {
            server_id: ServerId(id),
            start_count: 1,
            term: 0,
            vote: None,
            segments: Segments { first: 1, count: 0 },
            marker: Marker {
                index: 0,
                hash: [0; 32],
                committed: 0,
            },
            repair: None,
        };
        let mut bytes = encode(1, &values);
        bytes.extend_from_slice(&encode(2, &values));
        let file = fs.create(NEW_FILE, FileOptions::PLAIN)?;
        file.write_at(0, &bytes)?;
        file.flush()?;
        drop(file);
        fs.rename(NEW_FILE, FILE)?;
        fs.flush_folder("")?;
        let file = fs.open(FILE, FileOptions::PLAIN)?;
        Ok(Self {
            file,
            values,
            number: 2,
            slot: 1,
            opened: None,
            failed: false,
        })
    }

    /// Open `state`: read both copies, take the valid one with the highest
    /// number, make it durable (module docs), then raise the start count
    /// and flush that before returning. `_lock` shows that the caller
    /// holds the store folder.
    ///
    /// The start count rises by one, or by two when the other copy is not
    /// valid: that copy may have been a newer one, written by a start
    /// whose count would otherwise be given again.
    pub fn open(fs: &dyn Fs, _lock: &FolderLock) -> Result<Self, StateError> {
        let file = match fs.open(FILE, FileOptions::PLAIN) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(StateError::Missing),
            Err(e) => return Err(e.into()),
        };
        fs.flush_folder("")?;
        let mut bytes = vec![0u8; 2 * SLOT];
        let read = file.read_at(0, &mut bytes)?;
        let slot = |i: usize| &bytes[(i * SLOT).min(read)..((i + 1) * SLOT).min(read)];
        let copies = [decode(slot(0)), decode(slot(1))];
        for copy in &copies {
            if let Ok(Decoded::Newer(version)) = copy {
                return Err(StateError::NewerFormat { version: *version });
            }
        }
        let number = |i: usize| match &copies[i] {
            Ok(Decoded::Valid(number, _)) => Ok(*number),
            Ok(Decoded::Newer(_)) => unreachable!("refused above"),
            Err(invalid) => Err(*invalid),
        };
        // The highest number; between equal ones, the first slot.
        let chosen = match (number(0), number(1)) {
            (Ok(a), Ok(b)) if b > a => 1,
            (Ok(_), _) => 0,
            (Err(_), Ok(_)) => 1,
            (Err(first), Err(second)) => return Err(StateError::NoValidCopy { first, second }),
        };
        let Ok(Decoded::Valid(read_number, values)) = copies[chosen] else {
            unreachable!("chosen among the valid copies");
        };
        let other = number(1 - chosen);
        let start_count = values
            .start_count
            .checked_add(if other.is_ok() { 1 } else { 2 })
            .ok_or(StateError::Refused("the start count is at its limit"))?;
        // The copy read, back into its slot and onto the device.
        file.write_at((chosen * SLOT) as u64, &encode(read_number, &values))?;
        file.flush()?;
        let mut state = Self {
            file,
            values,
            number: read_number,
            slot: chosen,
            opened: Some(Opened {
                number: read_number,
                slot: chosen,
                other,
            }),
            failed: false,
        };
        state.write(Values {
            start_count,
            ..values
        })?;
        Ok(state)
    }

    /// The values of the current copy.
    pub fn values(&self) -> &Values {
        &self.values
    }

    /// The current copy's number and slot.
    pub fn copy(&self) -> (u64, usize) {
        (self.number, self.slot)
    }

    /// What the open read (`None` after [`State::create`]).
    pub fn opened(&self) -> Option<&Opened> {
        self.opened.as_ref()
    }

    /// Change the values with `change` and make the result durable: one
    /// write in place and one flush. Refused, with nothing written, when
    /// the result changes the server id or the start count, lowers the
    /// term, changes a vote already cast in the term, or changes the
    /// required end of a repair. After an error other than a refusal,
    /// every later update fails with [`StateError::Failed`].
    pub fn update(&mut self, change: impl FnOnce(&mut Values)) -> Result<(), StateError> {
        if self.failed {
            return Err(StateError::Failed);
        }
        let before = self.values;
        let mut after = before;
        change(&mut after);
        check(&before, &after)?;
        self.write(after)
    }

    /// Write `values` as the next copy, into the slot the current one is
    /// not in, and flush.
    fn write(&mut self, values: Values) -> Result<(), StateError> {
        let number = self
            .number
            .checked_add(1)
            .ok_or(StateError::Refused("the copy number is at its limit"))?;
        let slot = 1 - self.slot;
        let bytes = encode(number, &values);
        let written = self
            .file
            .write_at((slot * SLOT) as u64, &bytes)
            .and_then(|()| self.file.flush());
        if let Err(e) = written {
            self.failed = true;
            return Err(e.into());
        }
        self.number = number;
        self.slot = slot;
        self.values = values;
        Ok(())
    }
}

/// The rules an update keeps (see [`State::update`]).
fn check(before: &Values, after: &Values) -> Result<(), StateError> {
    if after.server_id != before.server_id {
        return Err(StateError::Refused("the server id never changes"));
    }
    if after.start_count != before.start_count {
        return Err(StateError::Refused(
            "the start count is raised only by an open",
        ));
    }
    if after.term < before.term {
        return Err(StateError::Refused("the term never goes back"));
    }
    if after.term == before.term && before.vote.is_some() && after.vote != before.vote {
        return Err(StateError::Refused("a vote cast in a term stays"));
    }
    if let (Some(was), Some(now)) = (before.repair, after.repair) {
        if was != now {
            return Err(StateError::Refused(
                "the required end of a repair does not change until the repair ends",
            ));
        }
    }
    if after
        .segments
        .first
        .checked_add(after.segments.count)
        .is_none()
    {
        return Err(StateError::Refused("the segment numbers overflow"));
    }
    Ok(())
}

/// A copy, or a copy of a newer format.
#[derive(Debug)]
enum Decoded {
    Valid(u64, Values),
    Newer(u16),
}

/// A copy with `number` and `values`, padded to its slot.
fn encode(number: u64, values: &Values) -> Vec<u8> {
    let mut out = Vec::with_capacity(SLOT);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(BODY_V1 as u32).to_le_bytes());
    out.extend_from_slice(&number.to_le_bytes());
    out.extend_from_slice(&values.server_id.0);
    out.extend_from_slice(&values.start_count.to_le_bytes());
    out.extend_from_slice(&values.term.to_le_bytes());
    out.push(u8::from(values.vote.is_some()));
    out.extend_from_slice(&values.vote.map_or([0; 16], |id| id.0));
    out.extend_from_slice(&values.segments.first.to_le_bytes());
    out.extend_from_slice(&values.segments.count.to_le_bytes());
    out.extend_from_slice(&values.marker.index.to_le_bytes());
    out.extend_from_slice(&values.marker.hash);
    out.extend_from_slice(&values.marker.committed.to_le_bytes());
    out.push(u8::from(values.repair.is_some()));
    out.extend_from_slice(&values.repair.map_or(0, |r| r.required_end).to_le_bytes());
    debug_assert_eq!(out.len(), HEADER + BODY_V1);
    let hash = digest::digest(&digest::SHA256, &out);
    out.extend_from_slice(hash.as_ref());
    out.resize(SLOT, 0);
    out
}

/// Read the copy at the start of `slot` (which may be shorter than a slot
/// when the file is).
fn decode(slot: &[u8]) -> Result<Decoded, Invalid> {
    if slot.len() < HEADER {
        return Err(
            if slot.len() >= MAGIC.len() && slot[..MAGIC.len()] != MAGIC {
                Invalid::Magic
            } else {
                Invalid::Short
            },
        );
    }
    if slot[..MAGIC.len()] != MAGIC {
        return Err(Invalid::Magic);
    }
    let length = u32::from_le_bytes(slot[12..16].try_into().expect("4 bytes")) as usize;
    if length > SLOT - HEADER - HASH {
        return Err(Invalid::Length);
    }
    let end = HEADER + length;
    if slot.len() < end + HASH {
        return Err(Invalid::Short);
    }
    let hash = digest::digest(&digest::SHA256, &slot[..end]);
    if hash.as_ref() != &slot[end..end + HASH] {
        return Err(Invalid::Hash);
    }
    let version = u16::from_le_bytes([slot[8], slot[9]]);
    if version > VERSION {
        return Ok(Decoded::Newer(version));
    }
    if version == 0 || slot[10..12] != [0, 0] || length != BODY_V1 {
        return Err(Invalid::Malformed);
    }
    let number = u64::from_le_bytes(slot[16..24].try_into().expect("8 bytes"));
    let body = &slot[HEADER..end];
    let u64_at = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().expect("8 bytes"));
    let id_at = |at: usize| ServerId(body[at..at + 16].try_into().expect("16 bytes"));
    let vote = match body[32] {
        0 if body[33..49] == [0; 16] => None,
        1 => Some(id_at(33)),
        _ => return Err(Invalid::Malformed),
    };
    let repair = match body[113] {
        0 if u64_at(114) == 0 => None,
        1 => Some(Repair {
            required_end: u64_at(114),
        }),
        _ => return Err(Invalid::Malformed),
    };
    let values = Values {
        server_id: id_at(0),
        start_count: u64_at(16),
        term: u64_at(24),
        vote,
        segments: Segments {
            first: u64_at(49),
            count: u64_at(57),
        },
        marker: Marker {
            index: u64_at(65),
            hash: body[73..105].try_into().expect("32 bytes"),
            committed: u64_at(105),
        },
        repair,
    };
    if values
        .segments
        .first
        .checked_add(values.segments.count)
        .is_none()
    {
        return Err(Invalid::Malformed);
    }
    Ok(Decoded::Valid(number, values))
}

#[cfg(test)]
mod tests;
