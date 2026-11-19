//! Byte-level reading and writing shared by both streams: opcodes (2.1),
//! counts, key strings, text operands (2.3).

use crate::catalog::Opcode;
use crate::error::{DecodeError, DecodeErrorKind};
use crate::operand::{Field, Key, PluginFunctionId, Style, Symbol, Text};

pub(crate) type Result<T> = std::result::Result<T, DecodeError>;

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pub pos: usize,
    /// Deepest nesting a Polish decode accepts.
    pub max_depth: usize,
    /// Nodes a Polish decode may still build.
    pub nodes_left: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            max_depth: usize::MAX,
            nodes_left: usize::MAX,
        }
    }

    pub fn at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }

    pub fn error(&self, kind: DecodeErrorKind) -> DecodeError {
        DecodeError {
            offset: self.pos,
            kind,
        }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self
            .bytes
            .get(self.pos..self.pos + n)
            .ok_or_else(|| self.error(DecodeErrorKind::Truncated))?;
        self.pos += n;
        Ok(bytes)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// An opcode: one byte, or a prefix `f8`..`fe` and an index (2.1). `ff`
    /// is invalid everywhere.
    pub fn opcode(&mut self) -> Result<Opcode> {
        let start = self.pos;
        let b = self.u8()?;
        let base = match b {
            0xf8 => 2000,
            0xf9 => 2300,
            0xfa => 2900,
            0xfb => 3200,
            0xfc => 3500,
            0xfd => 3800,
            0xfe => 4100,
            0xff => {
                return Err(DecodeError {
                    offset: start,
                    kind: DecodeErrorKind::InvalidOpcode(0xff),
                })
            }
            _ => return Ok(Opcode(u16::from(b))),
        };
        Ok(Opcode(base + u16::from(self.u8()?)))
    }

    /// A key string `[total length][segment length][segment]...` (2.5).
    pub fn key(&mut self) -> Result<Key> {
        let len = self.u8()?;
        Ok(Key(self.take(usize::from(len))?.to_vec()))
    }

    /// A text operand: unit count, SCSU, and the style byte when `styled`.
    pub fn text(&mut self, styled: bool) -> Result<Text> {
        let first = self.u8()?;
        let count = match first {
            0..=0x7f => usize::from(first),
            0x80..=0xbf => usize::from(first & 0x3f) << 8 | usize::from(self.u8()?),
            0xc2 => usize::from(self.u16()?),
            0xc4 => self.u32()? as usize,
            _ => return Err(self.error(DecodeErrorKind::Malformed("text length"))),
        };
        let start = self.pos;
        let (value, used) = om_scsu::decode_scsu_units(&self.bytes[start..], count)
            .map_err(|_| self.error(DecodeErrorKind::Text))?;
        self.pos += used;
        let style = if styled { self.style()? } else { None };
        Ok(Text { value, style })
    }

    fn style(&mut self) -> Result<Option<Style>> {
        let flags = self.u8()?;
        if flags == 0 {
            return Ok(None);
        }
        let len = self.u32()? as usize;
        Ok(Some(Style {
            flags,
            data: self.take(len)?.to_vec(),
        }))
    }

    pub fn field(&mut self) -> Result<Field> {
        Ok(Field {
            flags: self.u8()?,
            occurrence: self.key()?,
            field: self.key()?,
            repetition: self.u16()?,
        })
    }

    pub fn symbol(&mut self) -> Result<Symbol> {
        Ok(Symbol {
            name: self.text(true)?,
            flag: self.u8()?,
        })
    }

    pub fn plugin_id(&mut self) -> Result<PluginFunctionId> {
        let plugin = self.take(4)?;
        Ok(PluginFunctionId {
            plugin: [plugin[0], plugin[1], plugin[2], plugin[3]],
            function: self.u16()?,
        })
    }
}

/// Appends the stored forms of opcodes and operands.
pub(crate) trait Write {
    fn opcode(&mut self, op: Opcode);
    fn u16(&mut self, n: u16);
    fn key(&mut self, key: &Key);
    fn text(&mut self, text: &Text, styled: bool);
    fn field(&mut self, field: &Field);
    fn symbol(&mut self, symbol: &Symbol);
    fn plugin_id(&mut self, id: PluginFunctionId);
}

impl Write for Vec<u8> {
    fn opcode(&mut self, op: Opcode) {
        let (prefix, base) = match op.0 {
            0..=999 => return self.push(op.0 as u8),
            2000..=2299 => (0xf8, 2000),
            2300..=2899 => (0xf9, 2300),
            2900..=3199 => (0xfa, 2900),
            3200..=3499 => (0xfb, 3200),
            3500..=3799 => (0xfc, 3500),
            3800..=4099 => (0xfd, 3800),
            _ => (0xfe, 4100),
        };
        self.extend_from_slice(&[prefix, (op.0 - base) as u8]);
    }

    fn u16(&mut self, n: u16) {
        self.extend_from_slice(&n.to_be_bytes());
    }

    fn key(&mut self, key: &Key) {
        self.push(key.0.len() as u8);
        self.extend_from_slice(&key.0);
    }

    /// The count in its smallest form, FileMaker's SCSU, then the style.
    fn text(&mut self, text: &Text, styled: bool) {
        let count = text.value.encode_utf16().count();
        match count {
            0..=0x7f => self.push(count as u8),
            0x80..=0x3fff => self.u16(0x8000 | count as u16),
            0x4000..=0xffff => {
                self.push(0xc2);
                self.u16(count as u16);
            }
            _ => {
                self.push(0xc4);
                self.extend_from_slice(&(count as u32).to_be_bytes());
            }
        }
        self.extend_from_slice(&om_scsu::encode_scsu(&text.value));
        if styled {
            match &text.style {
                None => self.push(0),
                Some(style) => {
                    self.push(style.flags);
                    self.extend_from_slice(&(style.data.len() as u32).to_be_bytes());
                    self.extend_from_slice(&style.data);
                }
            }
        }
    }

    fn field(&mut self, field: &Field) {
        self.push(field.flags);
        self.key(&field.occurrence);
        self.key(&field.field);
        self.u16(field.repetition);
    }

    fn symbol(&mut self, symbol: &Symbol) {
        self.text(&symbol.name, true);
        self.push(symbol.flag);
    }

    fn plugin_id(&mut self, id: PluginFunctionId) {
        self.extend_from_slice(&id.plugin);
        self.u16(id.function);
    }
}
