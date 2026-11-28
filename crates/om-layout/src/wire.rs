//! A minimal protobuf wire-format reader: just enough to walk the fields of a
//! proto2 message and keep the ones a typed decoder does not claim.
//!
//! Follows the libprotobuf parsing rules the typed decoders rely on: a field
//! whose wire type does not match the schema is treated as unknown, repeated
//! scalars are accepted packed or unpacked, and repeated occurrences of a
//! singular embedded message merge (equivalent to decoding their
//! concatenation, see [`Merged`]).

use std::borrow::Cow;
use std::fmt;

/// Highest legal field number (2^29 - 1).
const MAX_FIELD_NUMBER: u64 = (1 << 29) - 1;
/// Group nesting limit, so hostile input cannot exhaust the stack.
const MAX_GROUP_DEPTH: usize = 64;

/// Malformed wire data. Unknown or unexpected fields are never an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The message type being decoded, for example `"LayoutObject"`.
    pub message: &'static str,
    /// Byte offset within that message's own encoding.
    pub offset: usize,
    pub reason: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} at offset {}",
            self.message, self.reason, self.offset
        )
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A field value as encoded, borrowing the message buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    /// Wire type 0.
    Varint(u64),
    /// Wire type 1 (double, fixed64, sfixed64).
    Fixed64(u64),
    /// Wire type 2: bytes, string, embedded message, or packed scalars.
    Bytes(&'a [u8]),
    /// Wire types 3/4 (deprecated groups): the encoding between the tags.
    Group(&'a [u8]),
    /// Wire type 5 (float, fixed32, sfixed32).
    Fixed32(u32),
}

/// One field of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field<'a> {
    pub number: u32,
    pub value: Value<'a>,
}

/// An owned field kept verbatim because no typed member claimed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawField {
    pub number: u32,
    pub value: RawValue,
}

/// Owned form of [`Value`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawValue {
    Varint(u64),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Group(Vec<u8>),
    Fixed32(u32),
}

impl From<Field<'_>> for RawField {
    fn from(field: Field<'_>) -> Self {
        let value = match field.value {
            Value::Varint(v) => RawValue::Varint(v),
            Value::Fixed64(v) => RawValue::Fixed64(v),
            Value::Bytes(b) => RawValue::Bytes(b.to_vec()),
            Value::Group(b) => RawValue::Group(b.to_vec()),
            Value::Fixed32(v) => RawValue::Fixed32(v),
        };
        RawField {
            number: field.number,
            value,
        }
    }
}

/// Byte cursor over one message's encoding.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    message: &'static str,
}

impl<'a> Cursor<'a> {
    fn error(&self, offset: usize, reason: &'static str) -> Error {
        Error {
            message: self.message,
            offset,
            reason,
        }
    }

    fn varint(&mut self) -> Result<u64> {
        let start = self.pos;
        let mut value = 0u64;
        for i in 0..10 {
            let Some(&byte) = self.buf.get(self.pos) else {
                return Err(self.error(start, "truncated varint"));
            };
            self.pos += 1;
            if i == 9 && (2..0x80).contains(&byte) {
                return Err(self.error(start, "varint overflows u64"));
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(self.error(start, "varint longer than 10 bytes"))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let start = self.pos;
        if self.buf.len() - self.pos < len {
            return Err(self.error(start, "truncated value"));
        }
        self.pos += len;
        Ok(&self.buf[start..self.pos])
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.take(N)?;
        Ok(bytes.try_into().expect("take returns N bytes"))
    }

    /// One field, or the number of an end-group tag.
    fn item(&mut self, depth: usize) -> Result<Item<'a>> {
        let start = self.pos;
        let tag = self.varint()?;
        let number = tag >> 3;
        if number == 0 || number > MAX_FIELD_NUMBER {
            return Err(self.error(start, "invalid field number"));
        }
        let number = number as u32;
        let value = match tag & 7 {
            0 => Value::Varint(self.varint()?),
            1 => Value::Fixed64(u64::from_le_bytes(self.fixed()?)),
            2 => {
                let len_at = self.pos;
                let len = usize::try_from(self.varint()?)
                    .map_err(|_| self.error(len_at, "length overflows usize"))?;
                Value::Bytes(self.take(len)?)
            }
            3 => {
                if depth >= MAX_GROUP_DEPTH {
                    return Err(self.error(start, "groups nested too deeply"));
                }
                let body = self.pos;
                loop {
                    let end = self.pos;
                    match self.item(depth + 1)? {
                        Item::Field(_) => {}
                        Item::EndGroup(n) if n == number => {
                            break Value::Group(&self.buf[body..end]);
                        }
                        Item::EndGroup(_) => {
                            return Err(self.error(end, "end-group tag does not match"))
                        }
                    }
                }
            }
            4 => return Ok(Item::EndGroup(number)),
            5 => Value::Fixed32(u32::from_le_bytes(self.fixed()?)),
            _ => return Err(self.error(start, "invalid wire type")),
        };
        Ok(Item::Field(Field { number, value }))
    }
}

enum Item<'a> {
    Field(Field<'a>),
    EndGroup(u32),
}

/// Iterator over the top-level fields of one message, in wire order. Stops
/// after the first error.
pub struct Fields<'a> {
    cursor: Cursor<'a>,
    failed: bool,
}

impl<'a> Fields<'a> {
    /// `message` names the message type in errors.
    pub fn new(buf: &'a [u8], message: &'static str) -> Self {
        Fields {
            cursor: Cursor {
                buf,
                pos: 0,
                message,
            },
            failed: false,
        }
    }

    /// The offset in the message of the next field's tag: after a field is
    /// read, where its encoding ends.
    pub fn offset(&self) -> usize {
        self.cursor.pos
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<Field<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.cursor.pos == self.cursor.buf.len() {
            return None;
        }
        let start = self.cursor.pos;
        let item = match self.cursor.item(0) {
            Ok(Item::Field(field)) => Ok(field),
            Ok(Item::EndGroup(_)) => Err(self.cursor.error(start, "end-group tag without a start")),
            Err(e) => Err(e),
        };
        self.failed = item.is_err();
        Some(item)
    }
}

/// Append the elements of a repeated varint field, given one occurrence of it
/// on the wire (unpacked varint or packed bytes). Returns `false`, appending
/// nothing, for any other wire type so the caller can keep the field.
pub fn push_varints(out: &mut Vec<u64>, value: Value<'_>, message: &'static str) -> Result<bool> {
    match value {
        Value::Varint(v) => out.push(v),
        Value::Bytes(packed) => {
            let mut cursor = Cursor {
                buf: packed,
                pos: 0,
                message,
            };
            while cursor.pos < packed.len() {
                out.push(cursor.varint()?);
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The occurrences of a singular embedded-message field. Protobuf merges
/// repeated occurrences, which equals decoding their concatenation; the
/// common single occurrence stays borrowed.
#[derive(Default)]
pub struct Merged<'a>(Option<Cow<'a, [u8]>>);

impl<'a> Merged<'a> {
    pub fn push(&mut self, bytes: &'a [u8]) {
        self.0 = Some(match self.0.take() {
            None => Cow::Borrowed(bytes),
            Some(prev) => {
                let mut all = prev.into_owned();
                all.extend_from_slice(bytes);
                Cow::Owned(all)
            }
        });
    }

    /// Decode the merged message, or `None` when the field never occurred.
    pub fn decode<T>(&self, decode: impl FnOnce(&[u8]) -> Result<T>) -> Result<Option<T>> {
        self.0.as_deref().map(decode).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(buf: &[u8]) -> Result<Vec<Field<'_>>> {
        Fields::new(buf, "Test").collect()
    }

    #[test]
    fn every_wire_type() {
        let buf = [
            0x08, 0x96, 0x01, // 1: varint 150
            0x11, 1, 2, 3, 4, 5, 6, 7, 8, // 2: fixed64
            0x1a, 0x02, 0xaa, 0xbb, // 3: bytes
            0x23, 0x08, 0x01, 0x24, // 4: group holding field 1 = 1
            0x2d, 1, 0, 0, 0, // 5: fixed32 1
        ];
        assert_eq!(
            fields(&buf).unwrap(),
            [
                Field {
                    number: 1,
                    value: Value::Varint(150)
                },
                Field {
                    number: 2,
                    value: Value::Fixed64(0x0807_0605_0403_0201)
                },
                Field {
                    number: 3,
                    value: Value::Bytes(&[0xaa, 0xbb])
                },
                Field {
                    number: 4,
                    value: Value::Group(&[0x08, 0x01])
                },
                Field {
                    number: 5,
                    value: Value::Fixed32(1)
                },
            ]
        );
    }

    #[test]
    fn ten_byte_varint() {
        // int32 -1 is sign-extended to 10 bytes.
        let buf = [
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ];
        let f = fields(&buf).unwrap();
        assert_eq!(f[0].value, Value::Varint(u64::MAX));
    }

    #[test]
    fn malformed_input_errors() {
        let err = |buf: &[u8]| fields(buf).unwrap_err().reason;
        assert_eq!(err(&[0x08]), "truncated varint");
        assert_eq!(err(&[0x08, 0x80]), "truncated varint");
        assert_eq!(err(&[0x0a, 0x05, 0x00]), "truncated value");
        assert_eq!(err(&[0x09, 0x00]), "truncated value");
        assert_eq!(err(&[0x00, 0x00]), "invalid field number");
        assert_eq!(err(&[0x0e]), "invalid wire type");
        assert_eq!(err(&[0x0c]), "end-group tag without a start");
        assert_eq!(err(&[0x0b, 0x14]), "end-group tag does not match");
        assert_eq!(err(&[0x0b]), "truncated varint");
        assert_eq!(
            err(&[[0x08].as_slice(), &[0xff; 11]].concat()),
            "varint longer than 10 bytes"
        );
    }

    #[test]
    fn packed_and_unpacked_varints() {
        let mut out = Vec::new();
        assert!(push_varints(&mut out, Value::Varint(7), "Test").unwrap());
        assert!(push_varints(&mut out, Value::Bytes(&[0x01, 0x96, 0x01]), "Test").unwrap());
        assert!(!push_varints(&mut out, Value::Fixed32(3), "Test").unwrap());
        assert_eq!(out, [7, 1, 150]);
        assert!(push_varints(&mut out, Value::Bytes(&[0x80]), "Test").is_err());
    }

    #[test]
    fn merged_concatenates() {
        let mut m = Merged::default();
        assert_eq!(m.decode(|b| Ok(b.to_vec())).unwrap(), None);
        m.push(&[1]);
        m.push(&[2, 3]);
        assert_eq!(m.decode(|b| Ok(b.to_vec())).unwrap(), Some(vec![1, 2, 3]));
    }
}
