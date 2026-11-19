//! Operands shared by the Polish and formatted streams (calc-encoding.md 2):
//! key strings, text, field references, symbols and plug-in function ids.

/// The segments of a key string as stored: `[segment length][segment]...`
/// (calc-encoding.md 2.5). A formula's context (child 4) has this form; in a
/// stream the key is preceded by its total length.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub Vec<u8>);

impl Key {
    /// The one-segment key of a numeric id: its minimal big-endian bytes
    /// (`Key::from_id(0xd00002)` is `03 d0 00 02`).
    pub fn from_id(id: u64) -> Self {
        let bytes = id.to_be_bytes();
        let skip = bytes.iter().take(7).take_while(|&&b| b == 0).count();
        let mut key = vec![(8 - skip) as u8];
        key.extend_from_slice(&bytes[skip..]);
        Self(key)
    }

    /// The id of a one-segment numeric key.
    pub fn id(&self) -> Option<u64> {
        let (&len, segment) = self.0.split_first()?;
        if usize::from(len) != segment.len() || segment.is_empty() || segment.len() > 8 {
            return None;
        }
        Some(segment.iter().fold(0, |acc, &b| acc << 8 | u64::from(b)))
    }
}

/// Text with its stored style runs (calc-encoding.md 2.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub value: String,
    /// Style buffers; `None` for plain text, which is all FileMaker writes
    /// in formulas.
    pub style: Option<Style>,
}

impl Text {
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            style: None,
        }
    }
}

/// Nonzero style flags (bit 1 character styles, bit 2 paragraph styles) and
/// the style buffers, kept as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style {
    pub flags: u8,
    pub data: Vec<u8>,
}

/// A field reference: opcode `16`, or `17` where a field reference is
/// required (calc-encoding.md 2.5, 5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// [`Field::TABLE_PRESENT`] when written `Table::Field`; the bracket
    /// flags record `${...}` quoting.
    pub flags: u8,
    /// The table occurrence (the context's when written without `Table::`).
    pub occurrence: Key,
    pub field: Key,
    /// 1 in every stored reference; `field[n]` compiles to `GetRepetition`.
    pub repetition: u16,
}

impl Field {
    pub const FIELD_BRACKETED: u8 = 1;
    pub const TABLE_BRACKETED: u8 = 2;
    pub const TABLE_PRESENT: u8 = 4;

    /// A reference to `field` of `occurrence` as the parser writes it.
    pub fn new(occurrence: u64, field: u64, qualified: bool) -> Self {
        Self {
            flags: if qualified { Self::TABLE_PRESENT } else { 0 },
            occurrence: Key::from_id(occurrence),
            field: Key::from_id(field),
            repetition: 1,
        }
    }
}

/// An unresolved or local name (`0b`): Let and While variables, custom
/// function parameters, and names that resolve when compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: Text,
    /// Always 0 in stored formulas.
    pub flag: u8,
}

impl Symbol {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Text::plain(name),
            flag: 0,
        }
    }
}

/// A plug-in function: the plug-in's four-character id and its function
/// number. `FMDR` and `FMMB` are FileMaker's built-in Design and mobile
/// functions (calc-encoding.md 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PluginFunctionId {
    pub plugin: [u8; 4],
    pub function: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_hold_minimal_big_endian_ids() {
        assert_eq!(Key::from_id(0xd00002).0, [3, 0xd0, 0, 2]);
        assert_eq!(Key::from_id(10).0, [1, 10]);
        assert_eq!(Key::from_id(0).0, [1, 0]);
        assert_eq!(Key(vec![3, 0xd0, 0, 2]).id(), Some(0xd00002));
        assert_eq!(Key(vec![2, 1]).id(), None);
    }
}
