//! Chunk (packed slot) codec and hierarchical key reader.
//!
//! This is the byte format of a page body and of the packed-slot buffer carried
//! by record transactions on the wire (file-format.md section 4,
//! network-protocol.md 5.5): the two are the same stream.
//!
//! A chunk is `[code][key][value]`. Code bits 3-4 give the key-size class and
//! bits 0-2 the value-size class (4.1); the top bits mark the hierarchy (4.2):
//! `0x80` begin, `0x40` pop, `0x20` or `0xC0` push, `0x00..=0x1F` leaf.
//!
//! List brackets (observed in every shipped file, file-format.md 3.6): `0x80`
//! follows exactly the push that creates a node (and opens the root list at
//! the start of the leaf chain); a page that continues an open node re-pushes
//! its path without `0x80`. `0x40` closes the current list, so the chain ends
//! with one extra `0x40` that closes the root.

use std::fmt;

use crate::error::{Error, Result};

/// Begin-hierarchy marker chunk (one byte, no key or value).
pub const BEGIN: u8 = 0x80;
/// End-hierarchy marker chunk: pops the last key segment.
pub const POP: u8 = 0x40;
/// Maximum key nesting depth (`FromDiskFmt` tracks 17 levels, file-format.md 2.2).
pub const MAX_DEPTH: usize = 17;

/// What a chunk does to the key stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    /// `0x80`: opens the child list of the node just pushed; no effect on the key path.
    Begin,
    /// `0x40`: closes the current list, removing the last key segment (or,
    /// with an empty stack, closing the root list).
    Pop,
    /// `0x20..=0x3F` or `0xC0..=0xFF`: append this chunk's key segment.
    Push,
    /// `0x00..=0x1F`: a value at `stack + key`.
    Leaf,
}

/// One decoded chunk, borrowing its bytes from the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub code: u8,
    pub kind: ChunkKind,
    /// The key segment (empty for begin/pop).
    pub key: &'a [u8],
    /// The value bytes (empty when the value class is 0).
    pub value: &'a [u8],
    /// The complete chunk bytes, code byte included.
    pub raw: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// Whether the code's value-size class carries a value (class != 0).
    pub fn has_value(&self) -> bool {
        matches!(self.kind, ChunkKind::Push | ChunkKind::Leaf) && self.code & 7 != 0
    }

    /// The value its slot gives its key: always one for a leaf (possibly
    /// empty), for a push only when its code has a value class.
    pub(crate) fn slot_value(&self) -> Option<&'a [u8]> {
        (self.kind != ChunkKind::Push || self.has_value()).then_some(self.value)
    }
}

/// What a chunk does to the key path of a slot stream (`S_SlotUnpack`,
/// file-format.md 3.3 and 4.2): the one rule every reader of the stream
/// applies ([`SlotReader`], [`crate::stream::NodeStream`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `0x80`: nothing changes.
    Begin,
    /// `0x40` with a nonempty path: its last segment is removed.
    Pop,
    /// `0x40` with an empty path: the root list closes, and nothing may
    /// follow it in the buffer.
    CloseRoot,
    /// A push: a slot at the path plus the chunk's key, which then stays
    /// on the path.
    Push,
    /// A leaf: a slot at the path plus the chunk's key; the path is
    /// unchanged.
    Leaf,
}

/// The [`Step`] of the chunk of `kind` at `offset`, read with the key path
/// `depth` segments deep, after a [`Step::CloseRoot`] when `root_closed`.
/// A chunk after the root list closed and a slot deeper than [`MAX_DEPTH`]
/// are errors.
pub(crate) fn step(
    kind: ChunkKind,
    depth: usize,
    root_closed: bool,
    offset: usize,
) -> Result<Step> {
    if root_closed {
        return Err(Error::PopUnderflow { offset });
    }
    match kind {
        ChunkKind::Begin => Ok(Step::Begin),
        ChunkKind::Pop if depth == 0 => Ok(Step::CloseRoot),
        ChunkKind::Pop => Ok(Step::Pop),
        ChunkKind::Push | ChunkKind::Leaf if depth + 1 > MAX_DEPTH => {
            Err(Error::TooDeep { offset })
        }
        ChunkKind::Push => Ok(Step::Push),
        ChunkKind::Leaf => Ok(Step::Leaf),
    }
}

fn kind_of(code: u8, offset: usize) -> Result<ChunkKind> {
    match code {
        BEGIN => Ok(ChunkKind::Begin),
        POP => Ok(ChunkKind::Pop),
        0xC0..=0xFF | 0x20..=0x3F => Ok(ChunkKind::Push),
        0x00..=0x1F => Ok(ChunkKind::Leaf),
        _ => Err(Error::UnsupportedCode { offset, code }),
    }
}

/// Decode the chunk starting at `buf[offset]` (`HBAMSlot::SlotUnpackLengths`).
pub fn decode_chunk(buf: &[u8], offset: usize) -> Result<Chunk<'_>> {
    let truncated = Error::Truncated { offset };
    let code = *buf.get(offset).ok_or(Error::Truncated { offset })?;
    let kind = kind_of(code, offset)?;
    if matches!(kind, ChunkKind::Begin | ChunkKind::Pop) {
        let raw = &buf[offset..offset + 1];
        return Ok(Chunk {
            code,
            kind,
            key: &[],
            value: &[],
            raw,
        });
    }
    let byte = |rel: usize| {
        buf.get(offset + rel)
            .copied()
            .ok_or(Error::Truncated { offset })
    };
    let (key_off, key_len) = match (code >> 3) & 3 {
        0 => (1, 1),
        1 => (1, 2),
        2 => (1, 3),
        _ => (2, usize::from(byte(1)?)),
    };
    let mut data_off = key_off + key_len;
    let data_len = match code & 7 {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 6,
        5 => 8,
        6 => {
            let n = usize::from(byte(data_off)?);
            data_off += 1;
            n
        }
        _ => {
            let n = usize::from(u16::from_be_bytes([byte(data_off)?, byte(data_off + 1)?]));
            data_off += 2;
            n
        }
    };
    let end = offset + data_off + data_len;
    if end > buf.len() {
        return Err(truncated);
    }
    Ok(Chunk {
        code,
        kind,
        key: &buf[offset + key_off..offset + key_off + key_len],
        value: &buf[offset + data_off..end],
        raw: &buf[offset..end],
    })
}

/// Iterate the raw chunks of a buffer in order.
pub struct Chunks<'a> {
    buf: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> Chunks<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Chunks {
            buf,
            pos: 0,
            failed: false,
        }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        match decode_chunk(self.buf, self.pos) {
            Ok(chunk) => {
                self.pos += chunk.raw.len();
                Some(Ok(chunk))
            }
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}

/// The smallest code FileMaker uses for a key/value of these sizes, or `None`
/// if the key is longer than 255 bytes or the value longer than 65535.
///
/// `push` selects a push slot (`0x20` bit); `has_value == false` emits value
/// class 0. Validated against every chunk of the shipped templates (tests).
pub fn minimal_code(push: bool, key_len: usize, value: Option<usize>) -> Option<u8> {
    let key_class = match key_len {
        1 => 0,
        2 => 1,
        3 => 2,
        0..=255 => 3,
        _ => return None,
    };
    let value_class = match value {
        None => 0,
        Some(1) => 1,
        Some(2) => 2,
        Some(4) => 3,
        Some(6) => 4,
        Some(8) => 5,
        Some(0..=255) => 6,
        Some(0..=65535) => 7,
        Some(_) => return None,
    };
    Some(if push { 0x20 } else { 0 } | key_class << 3 | value_class)
}

/// Encode a push or leaf chunk with an explicit code. Returns `None` when the
/// code's size classes cannot carry this key and value.
pub fn encode_chunk(code: u8, key: &[u8], value: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(4 + key.len() + value.len());
    encode_chunk_into(code, key, value, &mut out)?;
    Some(out)
}

/// [`encode_chunk`] appended to `out`, which is left as it was when the
/// code's size classes cannot carry this key and value.
pub(crate) fn encode_chunk_into(
    code: u8,
    key: &[u8],
    value: &[u8],
    out: &mut Vec<u8>,
) -> Option<()> {
    if !matches!(kind_of(code, 0).ok()?, ChunkKind::Push | ChunkKind::Leaf) {
        return None;
    }
    let key_len = match ((code >> 3) & 3, key.len()) {
        (0, 1) | (1, 2) | (2, 3) => None,
        (3, n) if n <= 255 => Some(n as u8),
        _ => return None,
    };
    match (code & 7, value.len()) {
        (0, 0) | (1, 1) | (2, 2) | (3, 4) | (4, 6) | (5, 8) => {}
        (6, n) if n <= 255 => {}
        (7, n) if n <= 65535 => {}
        _ => return None,
    }
    out.push(code);
    out.extend(key_len);
    out.extend_from_slice(key);
    match code & 7 {
        6 => out.push(value.len() as u8),
        7 => out.extend_from_slice(&(value.len() as u16).to_be_bytes()),
        _ => {}
    }
    out.extend_from_slice(value);
    Some(())
}

/// One segment of a hierarchical key, borrowed from the slot buffer.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeySegment<'a>(pub &'a [u8]);

impl<'a> KeySegment<'a> {
    pub fn bytes(self) -> &'a [u8] {
        self.0
    }

    /// The segment as a big-endian unsigned integer (1 to 8 bytes). Catalog
    /// ids, field ids, and record ids are stored this way.
    pub fn as_uint(self) -> Option<u64> {
        be_uint(self.0)
    }
}

/// A path of segments, as a [`crate::SlotReader`] or [`crate::Node::walk`]
/// gives it, can be passed where raw segment bytes are taken
/// ([`crate::writer::NodeWriter::node`]).
impl AsRef<[u8]> for KeySegment<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

impl fmt::Display for KeySegment<'_> {
    /// Short segments as decimal (catalog/entry ids), others as `x` + hex.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.len() {
            1..=4 => write!(f, "{}", be_uint(self.0).unwrap_or_default()),
            _ => {
                write!(f, "x")?;
                self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }
    }
}

impl fmt::Debug for KeySegment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Render a key path as `a/b/c` using [`KeySegment`]'s display form.
pub fn path_string(path: &[KeySegment<'_>]) -> String {
    path.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Big-endian unsigned integer of 1 to 8 bytes.
pub fn be_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |acc, &b| acc << 8 | u64::from(b)))
}

/// The key segment for a numeric id: its minimal big-endian bytes (at least
/// one). The inverse of [`KeySegment::as_uint`] for FileMaker's numeric keys,
/// whose multi-byte forms never start with a zero byte (`0x01`, `0x81 0x9a`,
/// `0xc4 0xf1 0x6c`).
pub fn uint_segment(id: u64) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take(7).take_while(|&&b| b == 0).count();
    bytes[skip..].to_vec()
}

/// A push or leaf slot with its full key path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot<'a> {
    /// Full key path, including this slot's own segment.
    pub path: Vec<KeySegment<'a>>,
    /// `Push` opens a level; `Leaf` is a terminal value.
    pub kind: ChunkKind,
    /// The value: always `Some` for a leaf (possibly empty); `Some` for a push
    /// only when its code carries a value class.
    pub value: Option<&'a [u8]>,
    pub code: u8,
    /// Offset of the chunk in its buffer.
    pub offset: usize,
}

/// Streams the slots of a packed buffer, rebuilding each key path as a
/// push/pop segment stack (`S_SlotUnpack`, file-format.md 3.3 and 4.2).
pub struct SlotReader<'a> {
    chunks: Chunks<'a>,
    stack: Vec<KeySegment<'a>>,
    root_closed: bool,
}

impl<'a> SlotReader<'a> {
    /// Read a buffer whose key context starts at the root (every on-disk page).
    pub fn new(buf: &'a [u8]) -> Self {
        Self::with_path(buf, Vec::new())
    }

    /// Read a buffer whose key context starts at `base` (a wire buffer that
    /// continues an established key path).
    pub fn with_path(buf: &'a [u8], base: Vec<KeySegment<'a>>) -> Self {
        SlotReader {
            chunks: Chunks::new(buf),
            stack: base,
            root_closed: false,
        }
    }

    /// The current key stack.
    pub fn path(&self) -> &[KeySegment<'a>] {
        &self.stack
    }

    /// Whether a pop with an empty stack closed the root list (the end of the
    /// leaf chain).
    pub fn root_closed(&self) -> bool {
        self.root_closed
    }

    /// Give `f` each remaining slot as [`Iterator::next`] would, in order,
    /// its path in the reader's own stack rather than a copy made for each
    /// slot. Stops at the first error, the reader's or `f`'s, as a caller of
    /// `next` with `?` does.
    pub fn try_for_each_slot(
        mut self,
        mut f: impl FnMut(&[KeySegment<'a>], ChunkKind, Option<&'a [u8]>) -> Result<()>,
    ) -> Result<()> {
        loop {
            let offset = self.chunks.pos;
            let Some(chunk) = self.chunks.next() else {
                return Ok(());
            };
            let chunk = chunk?;
            match step(chunk.kind, self.stack.len(), self.root_closed, offset)? {
                Step::Begin => {}
                Step::Pop => {
                    self.stack.pop();
                }
                Step::CloseRoot => self.root_closed = true,
                Step::Push | Step::Leaf => {
                    self.stack.push(KeySegment(chunk.key));
                    f(&self.stack, chunk.kind, chunk.slot_value())?;
                    // A leaf opens no level.
                    if chunk.kind == ChunkKind::Leaf {
                        self.stack.pop();
                    }
                }
            }
        }
    }
}

impl<'a> Iterator for SlotReader<'a> {
    type Item = Result<Slot<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let offset = self.chunks.pos;
            let chunk = match self.chunks.next()? {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            match step(chunk.kind, self.stack.len(), self.root_closed, offset) {
                Err(e) => {
                    self.chunks.failed = true;
                    return Some(Err(e));
                }
                Ok(Step::Begin) => {}
                Ok(Step::Pop) => {
                    self.stack.pop();
                }
                Ok(Step::CloseRoot) => self.root_closed = true,
                Ok(Step::Push | Step::Leaf) => {
                    let mut path = self.stack.clone();
                    path.push(KeySegment(chunk.key));
                    if chunk.kind == ChunkKind::Push {
                        self.stack.push(KeySegment(chunk.key));
                    }
                    return Some(Ok(Slot {
                        path,
                        kind: chunk.kind,
                        value: chunk.slot_value(),
                        code: chunk.code,
                        offset,
                    }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uint_segment_inverts_as_uint() {
        for bytes in [&[0x00][..], &[0x05], &[0x80, 0x01], &[0xc4, 0xf1, 0x6c]] {
            let id = KeySegment(bytes).as_uint().unwrap();
            assert_eq!(uint_segment(id), bytes);
        }
    }

    #[test]
    fn size_classes() {
        // 0x05: 1-byte key, 8-byte value ("Contact/" XOR 0x5A, Contacts/en).
        let buf = [0x05, 0x10, 0x19, 0x35, 0x34, 0x2e, 0x3b, 0x39, 0x2e, 0x75];
        let c = decode_chunk(&buf, 0).unwrap();
        assert_eq!(
            (c.kind, c.key, c.value.len(), c.raw.len()),
            (ChunkKind::Leaf, &[0x10][..], 8, 10)
        );
        // 0x1e: length-prefixed key, u8-length value.
        let buf = [0x1e, 0x02, 0xaa, 0xbb, 0x03, 1, 2, 3];
        let c = decode_chunk(&buf, 0).unwrap();
        assert_eq!((c.key, c.value), (&[0xaa, 0xbb][..], &[1, 2, 3][..]));
        // 0x07: u16 big-endian value length.
        let mut buf = vec![0x07, 0x01, 0x01, 0x2c];
        buf.extend(std::iter::repeat(0).take(300));
        assert_eq!(decode_chunk(&buf, 0).unwrap().value.len(), 300);
        assert!(matches!(
            decode_chunk(&buf[..100], 0),
            Err(Error::Truncated { .. })
        ));
        assert!(matches!(
            decode_chunk(&[0x41], 0),
            Err(Error::UnsupportedCode { .. })
        ));
    }

    #[test]
    fn push_pop_paths() {
        // begin, push 2, push 3, leaf 16 = "A", pop, leaf 4 = (no value).
        let buf = [
            0x80, 0x20, 0x02, 0x20, 0x03, 0x01, 0x10, 0x1b, 0x40, 0x00, 0x04,
        ];
        let slots: Vec<_> = SlotReader::new(&buf).map(|s| s.unwrap()).collect();
        let paths: Vec<_> = slots
            .iter()
            .map(|s| (path_string(&s.path), s.kind, s.value))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("2".to_string(), ChunkKind::Push, None),
                ("2/3".to_string(), ChunkKind::Push, None),
                ("2/3/16".to_string(), ChunkKind::Leaf, Some(&[0x1b][..])),
                ("2/4".to_string(), ChunkKind::Leaf, Some(&[][..])),
            ]
        );
        // A pop with an empty stack closes the root; nothing may follow it.
        let mut closed = SlotReader::new(&[0x40]);
        assert!(closed.next().is_none() && closed.root_closed());
        assert!(matches!(
            SlotReader::new(&[0x40, 0x40]).next(),
            Some(Err(Error::PopUnderflow { .. }))
        ));
    }

    #[test]
    fn encode_round_trip() {
        for (push, key, value) in [
            (false, vec![0x10], Some(vec![0x1b; 8])),
            (true, vec![0x80, 0x02], None),
            (false, vec![0; 21], Some(vec![2, 0x80, 3])),
            (false, vec![7], Some(vec![9; 1000])),
            (true, vec![], None),
        ] {
            let code = minimal_code(push, key.len(), value.as_ref().map(Vec::len)).unwrap();
            let bytes = encode_chunk(code, &key, value.as_deref().unwrap_or(&[])).unwrap();
            let c = decode_chunk(&bytes, 0).unwrap();
            assert_eq!(
                (c.key, c.value, c.raw.len()),
                (&key[..], value.as_deref().unwrap_or(&[]), bytes.len())
            );
        }
        assert_eq!(encode_chunk(0x03, &[1], &[1, 2]), None);
    }
}
