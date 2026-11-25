//! The superblock (`docs/server-storage-format.md` section 4.2): four
//! slots at the start of the page file, two copies of each of the two
//! newest checkpoints. A copy names its checkpoint's number, the entry it
//! covers, the tree format version and the first page of its meta stream
//! by number and hash; everything else (the roots, the restore points, the
//! map) is in the meta stream, so a copy has a fixed size.

use ring::digest;

use super::EntryId;
use crate::page::{Hash, PageNo};

/// The size of each slot. A copy never exceeds it.
pub const SLOT: usize = 4096;

/// The number of slots.
pub const SLOTS: usize = 4;

/// The superblock format version this code writes, and the newest it
/// reads.
pub const VERSION: u16 = 1;

const MAGIC: [u8; 8] = *b"OMPAGES\0";
const HEADER: usize = 24;
const BODY_V1: usize = 96;
const HASH: usize = 32;

/// The bytes of a version 1 copy, its hash included. An update writes just
/// these at the start of a slot.
pub const COPY: usize = HEADER + BODY_V1 + HASH;

/// The slots of pair `pair` (0 or 1): a checkpoint's two copies are 8 KiB
/// apart, so that a run of damaged bytes shorter than 4 KiB reaches at
/// most one copy of each checkpoint.
pub fn pair_slots(pair: usize) -> [usize; 2] {
    [pair, pair + 2]
}

/// The pair slot `slot` belongs to.
pub fn pair_of(slot: usize) -> usize {
    slot % 2
}

/// What a copy holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superblock {
    /// The checkpoint's number: one more than the highest number on disk
    /// when it was written.
    pub number: u64,
    /// The last entry the checkpoint's state includes.
    pub entry: EntryId,
    /// The tree format version of its trees.
    pub tree_format: u8,
    /// The first page of its meta stream.
    pub meta: (PageNo, Hash),
}

/// Why a copy is not valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The file ends before the copy does.
    Short,
    /// It does not begin with the format's mark (a slot never written, or
    /// cleared).
    Magic,
    /// Its stated length is not the length of its version's body.
    Length,
    /// Its hash does not match its content.
    Hash,
    /// The hash matches, but the content breaks the format.
    Malformed,
}

/// A slot's copy, or one of a newer format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    Valid(Superblock),
    Newer(u16),
}

/// The copy's bytes.
pub fn encode(sb: &Superblock) -> Vec<u8> {
    let mut out = Vec::with_capacity(COPY);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(BODY_V1 as u32).to_le_bytes());
    out.extend_from_slice(&sb.number.to_le_bytes());
    out.extend_from_slice(&sb.entry.index.to_le_bytes());
    out.extend_from_slice(&sb.entry.term.to_le_bytes());
    out.extend_from_slice(&sb.entry.hash.0);
    out.push(sb.tree_format);
    out.extend_from_slice(&[0; 7]);
    out.extend_from_slice(&sb.meta.0.to_le_bytes());
    out.extend_from_slice(&sb.meta.1 .0);
    debug_assert_eq!(out.len(), HEADER + BODY_V1);
    let hash = digest::digest(&digest::SHA256, &out);
    out.extend_from_slice(hash.as_ref());
    out
}

/// Reads the copy at the start of `slot` (shorter than a slot when the file
/// is).
pub fn decode(slot: &[u8]) -> Result<Decoded, Invalid> {
    if slot.len() < MAGIC.len() {
        return Err(Invalid::Short);
    }
    if slot[..MAGIC.len()] != MAGIC {
        return Err(Invalid::Magic);
    }
    if slot.len() < HEADER {
        return Err(Invalid::Short);
    }
    let u64_at = |at: usize| u64::from_le_bytes(slot[at..at + 8].try_into().expect("8 bytes"));
    let length = u32::from_le_bytes(slot[12..16].try_into().expect("4 bytes")) as usize;
    if length > SLOT - HEADER - HASH {
        return Err(Invalid::Length);
    }
    let end = HEADER + length;
    if slot.len() < end + HASH {
        return Err(Invalid::Short);
    }
    if digest::digest(&digest::SHA256, &slot[..end]).as_ref() != &slot[end..end + HASH] {
        return Err(Invalid::Hash);
    }
    let version = u16::from_le_bytes([slot[8], slot[9]]);
    if version > VERSION {
        return Ok(Decoded::Newer(version));
    }
    if version == 0 || slot[10..12] != [0, 0] {
        return Err(Invalid::Malformed);
    }
    if length != BODY_V1 {
        return Err(Invalid::Length);
    }
    let b = HEADER;
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&slot[b + 16..b + 48]);
    let mut meta_hash = [0u8; 32];
    meta_hash.copy_from_slice(&slot[b + 64..b + 96]);
    let sb = Superblock {
        number: u64_at(16),
        entry: EntryId {
            index: u64_at(b),
            term: u64_at(b + 8),
            hash: Hash(hash),
        },
        tree_format: slot[b + 48],
        meta: (u64_at(b + 56), Hash(meta_hash)),
    };
    if sb.number == 0 || sb.meta.0 == 0 || slot[b + 49..b + 56].iter().any(|&x| x != 0) {
        return Err(Invalid::Malformed);
    }
    Ok(Decoded::Valid(sb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Superblock {
        Superblock {
            number: 7,
            entry: EntryId {
                index: 1234,
                term: 3,
                hash: Hash([5; 32]),
            },
            tree_format: 1,
            meta: (99, Hash([6; 32])),
        }
    }

    #[test]
    fn a_copy_round_trips_and_every_damaged_byte_is_found() {
        let bytes = encode(&sample());
        assert_eq!(bytes.len(), COPY);
        assert_eq!(decode(&bytes), Ok(Decoded::Valid(sample())));
        for at in 0..COPY {
            let mut damaged = bytes.clone();
            damaged[at] ^= 0x41;
            assert!(
                !matches!(decode(&damaged), Ok(Decoded::Valid(_))),
                "byte {at}"
            );
        }
        for cut in 0..COPY {
            assert!(decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        assert_eq!(decode(&[0; COPY]), Err(Invalid::Magic));
    }

    #[test]
    fn a_newer_format_is_named() {
        let mut bytes = encode(&sample());
        bytes[8] = 2;
        let end = COPY - HASH;
        let hash = digest::digest(&digest::SHA256, &bytes[..end]);
        bytes[end..].copy_from_slice(hash.as_ref());
        assert_eq!(decode(&bytes), Ok(Decoded::Newer(2)));
    }
}
