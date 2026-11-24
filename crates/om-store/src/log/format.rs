//! The log's byte formats (`docs/server-storage-format.md` section 3): a
//! segment's header, a batch's header and an entry. Integers are
//! little-endian. Every structure here is checked by a SHA-256 hash before
//! any of its fields is used.

use ring::digest;

/// The format version this code writes, and the newest it reads.
pub const VERSION: u16 = 1;

/// The bytes a segment's header takes; a segment's batches follow it.
pub const SEGMENT_HEADER: u64 = 128;

/// The bytes a batch's header takes; its entries follow it.
pub const BATCH_HEADER: u64 = 136;

/// An entry's fixed fields, before its payload.
pub const ENTRY_FIXED: u64 = 32;

/// A SHA-256 hash.
pub const HASH: u64 = 32;

/// What an entry takes besides its payload.
pub const ENTRY_OVERHEAD: u64 = ENTRY_FIXED + HASH;

/// The smallest segment: room for its header and one batch of one empty
/// entry, rounded up to a sector.
pub const MIN_SEGMENT: u64 = 512;

/// The largest segment.
pub const MAX_SEGMENT: u64 = 1 << 30;

/// The largest payload the format holds, whatever the configuration: an
/// entry that declares more is not valid. It bounds what one read of an
/// entry can ask for. A configuration may allow less
/// (`Config::max_payload`); entries written under a larger one stay
/// readable up to this bound.
pub const MAX_PAYLOAD: u32 = 64 << 20;

/// The largest index an entry may have: one less than the largest number,
/// so that the index after every entry can be represented.
pub const MAX_INDEX: u64 = u64::MAX - 1;

const SEGMENT_MAGIC: [u8; 8] = *b"OMLOGSG\0";
const BATCH_MAGIC: [u8; 8] = *b"OMBATCH\0";

/// The hash of the entry before the first: what the chain starts from.
pub const ZERO_HASH: [u8; 32] = [0; 32];

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    for part in parts {
        ctx.update(part);
    }
    ctx.finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 is 32 bytes")
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("2 bytes"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

fn hash_at(bytes: &[u8], at: usize) -> [u8; 32] {
    bytes[at..at + 32].try_into().expect("32 bytes")
}

/// A segment's header (format section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentHeader {
    /// The store's id: its server's id in `state`.
    pub store: [u8; 16],
    /// The segment's number, which its file name also carries.
    pub number: u64,
    /// The segment's size in bytes, header included.
    pub size: u64,
    /// The index of the first entry it holds, or would hold.
    pub first: u64,
    /// The term of the entry before that one (0 before the first entry).
    pub prev_term: u64,
    /// The hash of the entry before that one ([`ZERO_HASH`] before the
    /// first entry).
    pub prev_hash: [u8; 32],
}

/// Why a segment's header is not valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFault {
    /// The file ends before the header does.
    Short,
    /// It does not begin as a segment's header does.
    Magic,
    /// Its hash does not match its content.
    Hash,
    /// The hash matches, and a field breaks the format.
    Malformed,
    /// The hash matches, and the version is newer than this code reads.
    Newer(u16),
}

impl SegmentHeader {
    pub fn encode(&self) -> [u8; SEGMENT_HEADER as usize] {
        let mut out = [0u8; SEGMENT_HEADER as usize];
        out[0..8].copy_from_slice(&SEGMENT_MAGIC);
        out[8..10].copy_from_slice(&VERSION.to_le_bytes());
        out[16..32].copy_from_slice(&self.store);
        out[32..40].copy_from_slice(&self.number.to_le_bytes());
        out[40..48].copy_from_slice(&self.size.to_le_bytes());
        out[48..56].copy_from_slice(&self.first.to_le_bytes());
        out[56..64].copy_from_slice(&self.prev_term.to_le_bytes());
        out[64..96].copy_from_slice(&self.prev_hash);
        let hash = sha256(&[&out[..96]]);
        out[96..128].copy_from_slice(&hash);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderFault> {
        if bytes.len() >= SEGMENT_MAGIC.len() && bytes[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC {
            return Err(HeaderFault::Magic);
        }
        if bytes.len() < SEGMENT_HEADER as usize {
            return Err(HeaderFault::Short);
        }
        if sha256(&[&bytes[..96]]) != hash_at(bytes, 96) {
            return Err(HeaderFault::Hash);
        }
        let version = u16_at(bytes, 8);
        if version > VERSION {
            return Err(HeaderFault::Newer(version));
        }
        let header = SegmentHeader {
            store: bytes[16..32].try_into().expect("16 bytes"),
            number: u64_at(bytes, 32),
            size: u64_at(bytes, 40),
            first: u64_at(bytes, 48),
            prev_term: u64_at(bytes, 56),
            prev_hash: hash_at(bytes, 64),
        };
        if version == 0
            || bytes[10..16] != [0; 6]
            || header.first == 0
            || header.first > MAX_INDEX
            || !(MIN_SEGMENT..=MAX_SEGMENT).contains(&header.size)
        {
            return Err(HeaderFault::Malformed);
        }
        Ok(header)
    }
}

/// A batch's header (format section 3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchHeader {
    /// How many entries the batch holds.
    pub count: u32,
    /// The index of its first entry.
    pub first: u64,
    /// The bytes its entries take, after the header.
    pub length: u64,
    /// The cut count `state` held when the batch was written.
    pub cut_count: u64,
    /// The hash of the entry before its first, which its first entry
    /// chains from.
    pub prev: [u8; 32],
    /// The hash of its last entry.
    pub last: [u8; 32],
}

impl BatchHeader {
    pub fn encode(&self) -> [u8; BATCH_HEADER as usize] {
        let mut out = [0u8; BATCH_HEADER as usize];
        out[0..8].copy_from_slice(&BATCH_MAGIC);
        out[8..10].copy_from_slice(&VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&self.count.to_le_bytes());
        out[16..24].copy_from_slice(&self.first.to_le_bytes());
        out[24..32].copy_from_slice(&self.length.to_le_bytes());
        out[32..40].copy_from_slice(&self.cut_count.to_le_bytes());
        out[40..72].copy_from_slice(&self.prev);
        out[72..104].copy_from_slice(&self.last);
        let hash = sha256(&[&out[..104]]);
        out[104..136].copy_from_slice(&hash);
        out
    }

    /// The header at the start of `bytes`, if one is there and valid. A
    /// batch is always of its segment's version, so any other version is
    /// not a valid header.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < BATCH_HEADER as usize || bytes[..BATCH_MAGIC.len()] != BATCH_MAGIC {
            return None;
        }
        if sha256(&[&bytes[..104]]) != hash_at(bytes, 104) {
            return None;
        }
        let header = BatchHeader {
            count: u32_at(bytes, 12),
            first: u64_at(bytes, 16),
            length: u64_at(bytes, 24),
            cut_count: u64_at(bytes, 32),
            prev: hash_at(bytes, 40),
            last: hash_at(bytes, 72),
        };
        let valid = u16_at(bytes, 8) == VERSION
            && bytes[10..12] == [0; 2]
            && header.count > 0
            && header.first > 0
            && header.first <= MAX_INDEX
            && header.length >= u64::from(header.count) * ENTRY_OVERHEAD;
        valid.then_some(header)
    }
}

/// An entry's fixed fields (format section 3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryHead {
    pub index: u64,
    pub term: u64,
    pub file: u64,
    pub kind: u16,
    /// The payload's length in bytes.
    pub len: u32,
}

impl EntryHead {
    /// The fixed fields at the start of `bytes` (at least
    /// [`ENTRY_FIXED`] long), or `None` when a reserved field is not zero,
    /// the index is past [`MAX_INDEX`] or the payload's length past
    /// [`MAX_PAYLOAD`]: those are checked before anything is read for the
    /// payload.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes[26..28] != [0; 2]
            || u64_at(bytes, 0) > MAX_INDEX
            || u32_at(bytes, 28) > MAX_PAYLOAD
        {
            return None;
        }
        Some(EntryHead {
            index: u64_at(bytes, 0),
            term: u64_at(bytes, 8),
            file: u64_at(bytes, 16),
            kind: u16_at(bytes, 24),
            len: u32_at(bytes, 28),
        })
    }

    /// The bytes the whole entry takes.
    pub fn size(&self) -> u64 {
        ENTRY_OVERHEAD + u64::from(self.len)
    }
}

/// An entry as written: its fixed fields, its payload and its hash, which
/// is SHA-256 over the previous entry's hash and the bytes before it.
pub fn encode_entry(
    prev: &[u8; 32],
    index: u64,
    term: u64,
    kind: u16,
    file: u64,
    payload: &[u8],
) -> (Vec<u8>, [u8; 32]) {
    let len = u32::try_from(payload.len()).expect("bounded by the log's configuration");
    let mut out = Vec::with_capacity(ENTRY_OVERHEAD as usize + payload.len());
    out.extend_from_slice(&index.to_le_bytes());
    out.extend_from_slice(&term.to_le_bytes());
    out.extend_from_slice(&file.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    let hash = sha256(&[prev, &out]);
    out.extend_from_slice(&hash);
    (out, hash)
}

/// The hash an entry whose bytes before the hash are `content` has after
/// `prev`.
pub fn entry_hash(prev: &[u8; 32], content: &[u8]) -> [u8; 32] {
    sha256(&[prev, content])
}

/// The file name of segment `number` in the store folder.
pub fn segment_path(number: u64) -> String {
    format!("{}/{number:016x}", super::FOLDER)
}

/// The number a segment's file name carries, if `name` is one.
pub fn segment_number(name: &str) -> Option<u64> {
    let digits = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if name.len() == 16 && name.bytes().all(digits) {
        u64::from_str_radix(name, 16).ok()
    } else {
        None
    }
}
