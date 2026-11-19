//! Serial keys (`HBSerialKey`): the prefix-coded unsigned integers FileMaker
//! uses for record ids, instruction numbers and other numeric key segments
//! (spec `docs/re/query-program.md` 2.6).
//!
//! A value takes 1 to 9 bytes. The count of leading 1 bits in the first byte
//! gives the length (0 for one byte, 1 for two, ...), the remaining bits hold
//! the value minus the length's offset, big-endian. Encoded keys sort in
//! numeric order, so the key tree keeps records in id order. Use these values,
//! not the raw big-endian reading of the bytes, for arithmetic such as RID-list
//! bitmap runs.

/// The smallest value each length encodes (index = length - 1).
const OFFSETS: [u64; 9] = [
    0,
    0x80,
    0x4080,
    0x20_4080,
    0x1020_4080,
    0x8_1020_4080,
    0x408_1020_4080,
    0x2_0408_1020_4080,
    0x102_0408_1020_4080,
];

/// The encoded length of a serial key from its first byte.
fn length_of(first: u8) -> usize {
    first.leading_ones() as usize + 1
}

/// Decode a serial key segment, or `None` if its length does not match its
/// first byte.
pub fn decode(bytes: &[u8]) -> Option<u64> {
    let (&first, rest) = bytes.split_first()?;
    let len = length_of(first);
    if bytes.len() != len {
        return None;
    }
    // The first byte keeps the bits after the length prefix (none for 8 and
    // 9 bytes, whose prefix fills it).
    let mask = if len >= 8 { 0 } else { 0xffu8 >> len };
    let raw = rest
        .iter()
        .fold(u64::from(first & mask), |acc, &b| acc << 8 | u64::from(b));
    raw.checked_add(OFFSETS[len - 1])
}

/// Encode `value` as a serial key segment.
pub fn encode(value: u64) -> Vec<u8> {
    let len = OFFSETS
        .iter()
        .rposition(|&offset| value >= offset)
        .expect("offset 0 admits every value")
        + 1;
    let raw = value - OFFSETS[len - 1];
    let mut out = raw.to_be_bytes()[8 - len.min(8)..].to_vec();
    if len == 9 {
        out.insert(0, 0);
    }
    // Set the length prefix: `len - 1` leading ones then a zero (no zero for
    // nine bytes, whose prefix fills the first byte).
    let prefix = (0xff00u16 >> (len - 1)) as u8;
    out[0] |= prefix;
    out
}

/// The id FileMaker shows for a catalog member whose key segment reads
/// `raw` big-endian (the numbering [`crate::Node::id`] and the catalogs use):
/// the segment's serial decode (file-metadata.md 1). Table `80 02` is 130,
/// occurrence `d0 00 02` is 1065090; one-byte keys are the same either way.
/// `None` when the segment is not a serial key.
pub fn id_of_raw(raw: u64) -> Option<u64> {
    decode(&crate::slot::uint_segment(raw))
}

/// The big-endian reading of catalog id `id`'s key segment, the inverse of
/// [`id_of_raw`]; `None` for ids whose key is longer than 8 bytes.
pub fn raw_of_id(id: u64) -> Option<u64> {
    crate::slot::be_uint(&encode(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        assert_eq!(decode(&[0x01]), Some(1));
        assert_eq!(decode(&[0x7f]), Some(127));
        assert_eq!(decode(&[0x81, 0x9a]), Some(538));
        assert_eq!(decode(&[0x81, 0x80]), Some(512));
        assert_eq!(decode(&[0xc4, 0xf1, 0x6c]), Some(340_460));
        assert_eq!(decode(&[0xc7, 0x7f, 0x6d]), Some(0x07_7f6d + 0x4080));
        assert_eq!(encode(538), [0x81, 0x9a]);
        assert_eq!(encode(340_460), [0xc4, 0xf1, 0x6c]);
    }

    #[test]
    fn round_trips_and_sorts() {
        let samples = [
            0u64,
            1,
            0x7f,
            0x80,
            0x407f,
            0x4080,
            0x20_407f,
            0x20_4080,
            0x1020_4080,
            0x8_1020_4080,
            0x408_1020_4080,
            0x2_0408_1020_4080,
            0x102_0408_1020_4080,
            u64::MAX,
        ];
        let mut previous: Option<Vec<u8>> = None;
        for value in samples {
            let key = encode(value);
            assert_eq!(decode(&key), Some(value), "{value:#x} -> {key:02x?}");
            if let Some(previous) = previous {
                assert!(previous < key, "keys sort in numeric order at {value:#x}");
            }
            previous = Some(key);
        }
        assert_eq!(encode(0x80).len(), 2);
        assert_eq!(encode(0x4080).len(), 3);
        assert_eq!(encode(u64::MAX).len(), 9);
    }

    #[test]
    fn catalog_ids_are_the_serial_decode_of_the_raw_key() {
        assert_eq!(id_of_raw(0x800d), Some(141));
        assert_eq!(id_of_raw(0x8002), Some(130));
        assert_eq!(id_of_raw(0xd0_0002), Some(1_065_090));
        assert_eq!(id_of_raw(0x8080), Some(256));
        assert_eq!(id_of_raw(0x7f), Some(127));
        assert_eq!(id_of_raw(0), Some(0));
        assert_eq!(raw_of_id(141), Some(0x800d));
        assert_eq!(raw_of_id(5), Some(5));
        // `81` alone is not a serial key.
        assert_eq!(id_of_raw(0x81), None);
    }

    #[test]
    fn rejects_wrong_lengths() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0x81]), None);
        assert_eq!(decode(&[0x01, 0x02]), None);
    }
}
