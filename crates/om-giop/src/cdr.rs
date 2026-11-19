//! CDR (Common Data Representation) reader and writer, little-endian.
//!
//! GIOP uses CDR for its headers and bodies. FileMaker negotiates little-endian
//! (byte-order flag 1 in every captured message), so this reader/writer is
//! little-endian only. Primitive values are aligned to their size relative to
//! the start of the GIOP message; alignment padding is skipped on read and
//! written as zero (senders may leave it uninitialized, so readers must never
//! inspect padding bytes).
//!
//! Spec: `docs/re/network-protocol.md` section 2; CDR is the public CORBA
//! standard (OMG GIOP/IIOP).

/// Error reading past the end of a CDR buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Underflow;

/// A little-endian CDR reader over a borrowed buffer.
///
/// Alignment is measured from offset 0, which the caller sets to the start of
/// the enclosing GIOP message so inter-field padding matches the wire.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Create a reader positioned at `pos` within `buf`.
    pub fn new(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    /// Current absolute position (alignment origin is 0).
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes remaining.
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn align(&mut self, n: usize) {
        self.pos += (n - (self.pos % n)) % n;
    }

    /// Read `n` raw bytes with no alignment.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Underflow> {
        let end = self.pos.checked_add(n).ok_or(Underflow)?;
        let slice = self.buf.get(self.pos..end).ok_or(Underflow)?;
        self.pos = end;
        Ok(slice)
    }

    /// Read one octet (no alignment).
    pub fn u8(&mut self) -> Result<u8, Underflow> {
        Ok(self.bytes(1)?[0])
    }

    /// Read an aligned little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16, Underflow> {
        self.align(2);
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    /// Read an aligned little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32, Underflow> {
        self.align(4);
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    /// Read a CDR `sequence<octet>`: a `u32` length then that many bytes.
    pub fn octet_sequence(&mut self) -> Result<&'a [u8], Underflow> {
        let len = self.u32()? as usize;
        self.bytes(len)
    }

    /// Read a CDR `string`: a `u32` length (including the NUL) then the bytes,
    /// returning the content without the trailing NUL.
    pub fn string(&mut self) -> Result<&'a [u8], Underflow> {
        let seq = self.octet_sequence()?;
        Ok(seq.strip_suffix(&[0]).unwrap_or(seq))
    }

    /// Skip a CDR `sequence<octet>` without borrowing it.
    pub fn skip_octet_sequence(&mut self) -> Result<(), Underflow> {
        let len = self.u32()? as usize;
        self.bytes(len)?;
        Ok(())
    }
}

/// A little-endian CDR writer.
///
/// Alignment is measured from the offset given at construction, so a writer
/// started at the message body offset pads exactly as the wire expects.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
    origin: usize,
}

impl Writer {
    /// A writer whose first byte is at absolute offset `origin` in the message.
    pub fn with_origin(origin: usize) -> Self {
        Self {
            buf: Vec::new(),
            origin,
        }
    }

    /// Consume the writer, returning the written bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn align(&mut self, n: usize) {
        let abs = self.origin + self.buf.len();
        let pad = (n - (abs % n)) % n;
        self.buf.resize(self.buf.len() + pad, 0);
    }

    /// Append raw bytes with no alignment.
    pub fn bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Write one octet.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write an aligned little-endian `u16`.
    pub fn u16(&mut self, v: u16) {
        self.align(2);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an aligned little-endian `u32`.
    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a CDR `sequence<octet>`.
    pub fn octet_sequence(&mut self, data: &[u8]) {
        self.u32(data.len() as u32);
        self.bytes(data);
    }

    /// Write a CDR `string` (length includes the trailing NUL this adds).
    pub fn string(&mut self, s: &[u8]) {
        self.u32(s.len() as u32 + 1);
        self.bytes(s);
        self.u8(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u32_alignment_skips_padding() {
        // At pos 1, a u32 aligns to pos 4.
        let buf = [0xAA, 0xCC, 0xCC, 0xCC, 0x78, 0x56, 0x34, 0x12];
        let mut r = Reader::new(&buf, 1);
        assert_eq!(r.u32().unwrap(), 0x1234_5678);
        assert_eq!(r.pos(), 8);
    }

    #[test]
    fn writer_pads_with_zero_relative_to_origin() {
        let mut w = Writer::with_origin(1);
        w.u8(0xAA);
        w.u32(0x1234_5678); // origin+1 = pos 2, pad to 4 => 2 zero bytes
        assert_eq!(
            w.into_bytes(),
            vec![0xAA, 0x00, 0x00, 0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn string_round_trip() {
        let mut w = Writer::with_origin(0);
        w.string(b"_is_a");
        let bytes = w.into_bytes();
        assert_eq!(bytes, [6, 0, 0, 0, b'_', b'i', b's', b'_', b'a', 0]);
        let mut r = Reader::new(&bytes, 0);
        assert_eq!(r.string().unwrap(), b"_is_a");
    }
}
