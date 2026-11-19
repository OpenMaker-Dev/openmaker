//! Keys stored inside step descriptors (script-steps.md 2.1, 4.1).

use std::fmt;

/// Filler FileMaker writes after a fresh key (script-steps.md 6).
const FRESH_FILLER: [u8; 5] = [0, 0xbe, 0xbe, 0xbe, 0xbe];

/// An `HBKey<5>` as stored in 6 bytes: `[length][key bytes]`, where the key is
/// a sequence of `[seglen][segment]`. Bytes past `1 + length` are not part of
/// the key and are not normalized (`00 be`, `be` and stale bytes all occur);
/// they are kept verbatim and ignored by every comparison of the key itself.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct StoredKey(pub [u8; 6]);

impl StoredKey {
    /// The empty key (length 0), as stored by steps that have no data subtree.
    pub const EMPTY: Self = Self([0; 6]);

    /// The first 6 bytes of `bytes`.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.get(..6)?.try_into().ok()?))
    }

    /// A fresh key holding the raw key bytes `key` (segments with their
    /// length bytes), padded the way FileMaker pads a new key.
    pub fn new(key: &[u8]) -> Option<Self> {
        if key.len() > 5 {
            return None;
        }
        let mut bytes = [0; 6];
        bytes[0] = key.len() as u8;
        bytes[1..=key.len()].copy_from_slice(key);
        bytes[1 + key.len()..].copy_from_slice(&FRESH_FILLER[..5 - key.len()]);
        Some(Self(bytes))
    }

    /// A fresh key of one segment.
    pub fn from_segment(segment: &[u8]) -> Option<Self> {
        let mut key = vec![u8::try_from(segment.len()).ok()?];
        key.extend_from_slice(segment);
        Self::new(&key)
    }

    /// A fresh key for a catalog id (one big-endian segment, as catalog
    /// member ids are stored).
    pub fn from_id(id: u64) -> Option<Self> {
        Self::from_segment(&om_hbam::slot::uint_segment(id))
    }

    /// The key bytes after the length byte; `None` when the length byte is
    /// out of range.
    pub fn key(&self) -> Option<&[u8]> {
        self.0.get(1..1 + usize::from(self.0[0]))
    }

    pub fn is_empty(&self) -> bool {
        self.0[0] == 0
    }

    /// The key's segments; `None` unless they exactly fill the key.
    pub fn segments(&self) -> Option<Vec<&[u8]>> {
        let mut rest = self.key()?;
        let mut out = Vec::new();
        while let Some((&len, tail)) = rest.split_first() {
            let len = usize::from(len);
            out.push(tail.get(..len)?);
            rest = &tail[len..];
        }
        Some(out)
    }

    /// The segment of a one-segment key.
    pub fn segment(&self) -> Option<&[u8]> {
        match self.segments()?.as_slice() {
            [segment] if !segment.is_empty() => Some(segment),
            _ => None,
        }
    }

    /// A one-segment key as the numeric id catalogs use (big-endian segment).
    pub fn id(&self) -> Option<u64> {
        om_hbam::slot::be_uint(self.segment()?)
    }

    /// A one-segment key as the id FileMaker shows: the segment's serial
    /// decode (file-metadata.md 1; `80 0d` is script 141). Script
    /// references are read this way, matching [`crate::Script::id`].
    pub fn serial_id(&self) -> Option<u64> {
        om_hbam::serial::decode(self.segment()?)
    }

    /// A fresh key for the id FileMaker shows (its serial key segment), the
    /// inverse of [`StoredKey::serial_id`].
    pub fn from_serial_id(id: u64) -> Option<Self> {
        Self::from_segment(&om_hbam::serial::encode(id))
    }

    /// Whether both keys are the same key, whatever their filler.
    pub fn same_key(&self, other: &Self) -> bool {
        self.key().is_some() && self.key() == other.key()
    }
}

impl fmt::Debug for StoredKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StoredKey(")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))?;
        write!(f, ")")
    }
}

/// A `DBFieldSpec` (14 bytes): table occurrence key, field key and a
/// big-endian repetition (script-steps.md 4.1). Stored in `mReserved` for
/// slot 0 and at `[00 n, 10]` for other slots.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FieldSpec(pub [u8; 14]);

impl FieldSpec {
    pub fn new(table: StoredKey, field: StoredKey, repetition: i16) -> Self {
        let mut bytes = [0; 14];
        bytes[..6].copy_from_slice(&table.0);
        bytes[6..12].copy_from_slice(&field.0);
        bytes[12..].copy_from_slice(&repetition.to_be_bytes());
        Self(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.get(..14)?.try_into().ok()?))
    }

    /// The target spec of a variable: table `HBAbstractKey(0)`, field
    /// `HBAbstractKey(0x20)` (`VariableStore::kTargetVariable*Key`); its name
    /// is text data (script-steps.md 4.1).
    pub fn variable(repetition: i16) -> Self {
        let table = StoredKey::from_segment(&[0]).expect("one byte");
        let field = StoredKey::from_segment(&[0x20]).expect("one byte");
        Self::new(table, field, repetition)
    }

    pub fn table(&self) -> StoredKey {
        StoredKey::from_bytes(&self.0[..6]).expect("six bytes")
    }

    pub fn field(&self) -> StoredKey {
        StoredKey::from_bytes(&self.0[6..12]).expect("six bytes")
    }

    pub fn repetition(&self) -> i16 {
        i16::from_be_bytes([self.0[12], self.0[13]])
    }

    /// Whether this spec names a variable rather than a field.
    pub fn is_variable(&self) -> bool {
        self.table().key() == Some(&[1, 0][..]) && self.field().key() == Some(&[1, 0x20][..])
    }

    /// Whether this spec names a field: a cleared one has occurrence 0 or
    /// field 0, as FileMaker Pro stores each unused input of Show Custom
    /// Dialog (`02 01 00 00 f0 4a 02 01 00 00 79 00 00 01`, #940).
    pub fn names_field(&self) -> bool {
        let nonzero = |key: StoredKey| key.id().is_some_and(|id| id != 0);
        nonzero(self.table()) && nonzero(self.field())
    }
}

impl fmt::Debug for FieldSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldSpec")
            .field("table", &self.table())
            .field("field", &self.field())
            .field("repetition", &self.repetition())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_is_not_part_of_the_key() {
        // FMServer_Sample's stale filler and a template's `00 be be`.
        let stale = StoredKey([2, 1, 6, 0, 0x6f, 0]);
        let fresh = StoredKey([2, 1, 6, 0, 0xbe, 0xbe]);
        assert_eq!(stale.segment(), Some(&[6][..]));
        assert_eq!(stale.id(), Some(6));
        assert!(stale.same_key(&fresh));
        assert_ne!(stale, fresh);
        assert_eq!(StoredKey::from_segment(&[6]), Some(fresh));
        assert_eq!(
            StoredKey::from_segment(&[0x80, 0xc8]).unwrap().0,
            [3, 2, 0x80, 0xc8, 0, 0xbe]
        );
        assert!(StoredKey::EMPTY.is_empty());
        assert_eq!(StoredKey::EMPTY.segment(), None);
        // A segment longer than the key is malformed.
        assert_eq!(StoredKey([2, 3, 1, 2, 0, 0]).segments(), None);
    }

    #[test]
    fn script_references_are_serial_ids() {
        let key = StoredKey::from_serial_id(141).unwrap();
        assert_eq!(key.0, [3, 2, 0x80, 0x0d, 0, 0xbe]);
        assert_eq!(key.serial_id(), Some(141));
        assert_eq!(key.id(), Some(0x800d));
        assert_eq!(StoredKey::from_serial_id(3), StoredKey::from_id(3));
        assert_eq!(StoredKey::EMPTY.serial_id(), None);
    }

    #[test]
    fn variable_targets_use_the_abstract_keys() {
        let spec =
            FieldSpec::from_bytes(&[2, 1, 0, 0, 1, 0, 2, 1, 0x20, 0, 0xaf, 0x0f, 0, 1]).unwrap();
        assert!(spec.is_variable());
        assert_eq!(spec.repetition(), 1);
        assert!(FieldSpec::variable(1).is_variable());
        let field = FieldSpec::new(
            StoredKey::new(&[3, 0xd0, 0, 5]).unwrap(),
            StoredKey::from_id(5).unwrap(),
            1,
        );
        assert!(!field.is_variable());
        assert_eq!(field.field().id(), Some(5));
    }
}
