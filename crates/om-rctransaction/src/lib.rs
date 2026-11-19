//! The FileMaker RCTransaction envelope carried inside a `Perform` OctetSeq.
//!
//! A transaction is a 20-byte big-endian header (opcode plus addressing) and a
//! body that the client fills with a forward `Push` cursor and the server reads
//! with a `Pop` cursor from the end (a LIFO stack). Field types are not tagged
//! on the wire; the opcode fixes the layout, so both ends must agree on order.
//!
//! This crate provides the header and the push/pop primitives. The per-opcode
//! field order lives in `om-protocol`.
//!
//! Spec: `docs/re/network-protocol.md` section 4.

use om_scsu::{self, Framing};

/// Minimum valid transaction length (the header alone). The server rejects
/// `Perform` payloads shorter than this.
pub const HEADER_LEN: usize = 20;

/// The 20-byte RCTransaction header (DWARF `RCOpHeader`, spec 5.6.3).
///
/// Byte layout, all multi-byte fields big-endian:
/// `[0]=opcode [1]=task_id [2..4]=transid_hi [4..8]=transid_lo
/// [8..12]=session_id [12..16]=file_id [16..20]=idle_time`.
/// (`session_id` and `file_id` address the open file for a data op; both are 0
/// before the file is open, e.g. for Connect/Open.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub opcode: u8,
    pub task_id: u8,
    pub transid_hi: u16,
    pub transid_lo: u32,
    pub session_id: u32,
    pub file_id: u32,
    pub idle_time: u32,
}

impl Header {
    /// A header for `opcode` with everything else zero.
    pub fn new(opcode: u8) -> Self {
        Self {
            opcode,
            task_id: 0,
            transid_hi: 0,
            transid_lo: 0,
            session_id: 0,
            file_id: 0,
            idle_time: 0,
        }
    }

    /// Parse the 20-byte header from the start of `buf`.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let h = buf.get(..HEADER_LEN)?;
        Some(Self {
            opcode: h[0],
            task_id: h[1],
            transid_hi: u16::from_be_bytes([h[2], h[3]]),
            transid_lo: u32::from_be_bytes([h[4], h[5], h[6], h[7]]),
            session_id: u32::from_be_bytes([h[8], h[9], h[10], h[11]]),
            file_id: u32::from_be_bytes([h[12], h[13], h[14], h[15]]),
            idle_time: u32::from_be_bytes([h[16], h[17], h[18], h[19]]),
        })
    }

    /// Write the 20 header bytes.
    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(self.opcode);
        out.push(self.task_id);
        out.extend_from_slice(&self.transid_hi.to_be_bytes());
        out.extend_from_slice(&self.transid_lo.to_be_bytes());
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.file_id.to_be_bytes());
        out.extend_from_slice(&self.idle_time.to_be_bytes());
    }
}

/// Builds a transaction body with the forward `Push` cursor.
///
/// Push fields in the same order the official client does for the opcode; the
/// server pops them from the end. Methods consume and return the builder for
/// chaining; use [`Builder::header_mut`] on a binding when a header field (such
/// as the target file) must be set.
#[derive(Clone, Debug)]
pub struct Builder {
    header: Header,
    body: ResultBuilder,
}

impl Builder {
    /// Start a transaction for `opcode`.
    pub fn new(opcode: u8) -> Self {
        Self::with_header(Header::new(opcode))
    }

    /// Start from an explicit header.
    pub fn with_header(header: Header) -> Self {
        Self {
            header,
            body: ResultBuilder::new(),
        }
    }

    /// Mutable access to the header.
    pub fn header_mut(&mut self) -> &mut Header {
        &mut self.header
    }

    /// Push one octet.
    pub fn push_u8(mut self, v: u8) -> Self {
        self.body = self.body.push_u8(v);
        self
    }

    /// Push a boolean as one octet.
    pub fn push_bool(self, v: bool) -> Self {
        self.push_u8(u8::from(v))
    }

    /// Push a big-endian `u16`.
    pub fn push_u16(mut self, v: u16) -> Self {
        self.body = self.body.push_u16(v);
        self
    }

    /// Push a big-endian `u32`.
    pub fn push_u32(mut self, v: u32) -> Self {
        self.body = self.body.push_u32(v);
        self
    }

    /// Push a big-endian `u64`.
    pub fn push_u64(mut self, v: u64) -> Self {
        self.body = self.body.push_u64(v);
        self
    }

    /// Push a string as SCSU + XOR 0x5A with a trailing `u32` length
    /// (the stack `Push(unistring)` form).
    pub fn push_string(mut self, s: &str) -> Self {
        self.body = self.body.push_string(s);
        self
    }

    /// Push raw bytes verbatim (e.g. a packed slot buffer).
    pub fn push_raw(mut self, bytes: &[u8]) -> Self {
        self.body = self.body.push_raw(bytes);
        self
    }

    /// Finish, returning the full transaction bytes (header + body).
    pub fn finish(self) -> Vec<u8> {
        let body = self.body.finish();
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        self.header.write(&mut out);
        out.extend_from_slice(&body);
        out
    }
}

/// Builds a `Perform` reply result stack: the header-less LIFO body a server
/// pushes from offset 0 (`ResetPerformPush`). The last value pushed is the
/// first the client pops, so push the status `u16` last. The mirror of
/// [`ResultReader`].
///
/// Spec: `docs/re/network-protocol.md` section 5.6.3.
#[derive(Clone, Debug, Default)]
pub struct ResultBuilder {
    body: Vec<u8>,
}

impl ResultBuilder {
    /// An empty result stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one octet.
    pub fn push_u8(mut self, v: u8) -> Self {
        self.body.push(v);
        self
    }

    /// Push a boolean as one octet.
    pub fn push_bool(self, v: bool) -> Self {
        self.push_u8(u8::from(v))
    }

    /// Push a big-endian `u16`.
    pub fn push_u16(mut self, v: u16) -> Self {
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Push a big-endian `u32`.
    pub fn push_u32(mut self, v: u32) -> Self {
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Push a big-endian `u64`.
    pub fn push_u64(mut self, v: u64) -> Self {
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Push a string as SCSU + XOR 0x5A with a trailing `u32` length.
    pub fn push_string(mut self, s: &str) -> Self {
        let field = om_scsu::encode_field(s, Framing::TrailingU32)
            .expect("trailing-u32 length cannot overflow");
        self.body.extend_from_slice(&field);
        self
    }

    /// Push bytes in the `Push(uchar const*)` form: the bytes then a 1-byte
    /// length (spec 4.2), for bytes whose length the caller bounds itself.
    /// Bytes that come from a request or a file go through
    /// [`ResultBuilder::try_push_short_bytes`].
    ///
    /// # Panics
    /// If `bytes` is longer than 255, which this form cannot frame.
    pub fn push_short_bytes(self, bytes: &[u8]) -> Self {
        self.try_push_short_bytes(bytes)
            .expect("short bytes fit in a u8 length")
    }

    /// [`ResultBuilder::push_short_bytes`], or [`TooLong`] (pushing nothing)
    /// when `bytes` is longer than 255.
    pub fn try_push_short_bytes(mut self, bytes: &[u8]) -> Result<Self, TooLong> {
        let len = u8::try_from(bytes.len()).map_err(|_| TooLong { len: bytes.len() })?;
        self.body.extend_from_slice(bytes);
        self.body.push(len);
        Ok(self)
    }

    /// Push raw bytes verbatim (e.g. a packed slot buffer).
    pub fn push_raw(mut self, bytes: &[u8]) -> Self {
        self.body.extend_from_slice(bytes);
        self
    }

    /// Finish, returning the result-stack bytes.
    pub fn finish(self) -> Vec<u8> {
        self.body
    }
}

/// Bytes too long for the one-byte length of the `Push(uchar const*)` form
/// (at most 255).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong {
    /// The length that did not fit.
    pub len: usize,
}

impl core::fmt::Display for TooLong {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} bytes do not fit a one-byte length", self.len)
    }
}

impl std::error::Error for TooLong {}

/// Error popping past the start of a transaction body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Underflow;

/// LIFO pop cursor over a body, consuming from the end. Shared by [`Reader`]
/// (request transactions, with a header) and [`ResultReader`] (Perform reply
/// result stacks, header-less).
#[derive(Clone, Copy)]
struct Popper<'a> {
    body: &'a [u8],
    /// Bytes `[0, cursor)` are still unread.
    cursor: usize,
}

impl<'a> Popper<'a> {
    fn new(body: &'a [u8]) -> Self {
        Self {
            body,
            cursor: body.len(),
        }
    }

    fn take_tail(&mut self, n: usize) -> Result<&'a [u8], Underflow> {
        let start = self.cursor.checked_sub(n).ok_or(Underflow)?;
        let slice = &self.body[start..self.cursor];
        self.cursor = start;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Underflow> {
        Ok(self.take_tail(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Underflow> {
        Ok(u16::from_be_bytes(self.take_tail(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, Underflow> {
        Ok(u32::from_be_bytes(self.take_tail(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, Underflow> {
        Ok(u64::from_be_bytes(self.take_tail(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, Underflow> {
        let len = self.u32()? as usize;
        let body = self.take_tail(len)?;
        om_scsu::decode_scsu(body).map_err(|_| Underflow)
    }
}

/// Popping typed values from the end of a LIFO body, shared by [`Reader`]
/// (request transactions) and [`ResultReader`] (reply result stacks), so a
/// field group that appears in both directions is decoded once.
pub trait Pop<'a> {
    /// Pop one octet.
    fn pop_u8(&mut self) -> Result<u8, Underflow>;
    /// Pop a big-endian `u16`.
    fn pop_u16(&mut self) -> Result<u16, Underflow>;
    /// Pop a big-endian `u32`.
    fn pop_u32(&mut self) -> Result<u32, Underflow>;
    /// Pop a big-endian `u64`.
    fn pop_u64(&mut self) -> Result<u64, Underflow>;
    /// Pop exactly `n` raw bytes.
    fn pop_raw(&mut self, n: usize) -> Result<&'a [u8], Underflow>;
    /// Pop bytes in the `Push(uchar const*)` form: the bytes followed by a
    /// trailing 1-byte length (spec 4.2).
    fn pop_short_bytes(&mut self) -> Result<&'a [u8], Underflow> {
        let len = usize::from(self.pop_u8()?);
        self.pop_raw(len)
    }
}

impl<'a> Pop<'a> for Reader<'a> {
    fn pop_u8(&mut self) -> Result<u8, Underflow> {
        self.pop.u8()
    }
    fn pop_u16(&mut self) -> Result<u16, Underflow> {
        self.pop.u16()
    }
    fn pop_u32(&mut self) -> Result<u32, Underflow> {
        self.pop.u32()
    }
    fn pop_u64(&mut self) -> Result<u64, Underflow> {
        self.pop.u64()
    }
    fn pop_raw(&mut self, n: usize) -> Result<&'a [u8], Underflow> {
        self.pop.take_tail(n)
    }
}

impl<'a> Pop<'a> for ResultReader<'a> {
    fn pop_u8(&mut self) -> Result<u8, Underflow> {
        self.pop.u8()
    }
    fn pop_u16(&mut self) -> Result<u16, Underflow> {
        self.pop.u16()
    }
    fn pop_u32(&mut self) -> Result<u32, Underflow> {
        self.pop.u32()
    }
    fn pop_u64(&mut self) -> Result<u64, Underflow> {
        self.pop.u64()
    }
    fn pop_raw(&mut self, n: usize) -> Result<&'a [u8], Underflow> {
        self.pop.take_tail(n)
    }
}

/// Reads a received request transaction (header + LIFO body), popping from the end.
pub struct Reader<'a> {
    header: Header,
    pop: Popper<'a>,
}

impl<'a> Reader<'a> {
    /// Parse a full transaction (20-byte header + body).
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        let header = Header::parse(buf)?;
        Some(Self {
            header,
            pop: Popper::new(&buf[HEADER_LEN..]),
        })
    }

    /// The parsed header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Unread body bytes remaining before the pop cursor.
    pub fn remaining(&self) -> usize {
        self.pop.cursor
    }

    /// Pop one octet.
    pub fn pop_u8(&mut self) -> Result<u8, Underflow> {
        self.pop.u8()
    }
    /// Pop a boolean.
    pub fn pop_bool(&mut self) -> Result<bool, Underflow> {
        Ok(self.pop.u8()? != 0)
    }
    /// Pop a big-endian `u16`.
    pub fn pop_u16(&mut self) -> Result<u16, Underflow> {
        self.pop.u16()
    }
    /// Pop a big-endian `u32`.
    pub fn pop_u32(&mut self) -> Result<u32, Underflow> {
        self.pop.u32()
    }
    /// Pop a big-endian `u64`.
    pub fn pop_u64(&mut self) -> Result<u64, Underflow> {
        self.pop.u64()
    }
    /// Pop a string in the trailing-`u32` SCSU form.
    pub fn pop_string(&mut self) -> Result<String, Underflow> {
        self.pop.string()
    }
    /// Pop raw bytes in the `Push(uchar const*)` form: the bytes followed by a
    /// trailing 1-byte length (spec 4.2).
    pub fn pop_short_bytes(&mut self) -> Result<&'a [u8], Underflow> {
        let len = usize::from(self.pop.u8()?);
        self.pop.take_tail(len)
    }
    /// Pop exactly `n` raw bytes.
    pub fn pop_raw(&mut self, n: usize) -> Result<&'a [u8], Underflow> {
        self.pop.take_tail(n)
    }
    /// The body bytes not yet popped, from the start of the body.
    pub fn rest(&self) -> &'a [u8] {
        &self.pop.body[..self.pop.cursor]
    }
}

/// Reads a `Perform` reply result stack: a header-less LIFO body the server
/// builds with `ResetPerformPush` (no 20-byte header). Pop in the reverse of the
/// server's push order; the status/error `u16` is the tail, popped first.
///
/// Spec: `docs/re/network-protocol.md` section 5.6.
pub struct ResultReader<'a> {
    pop: Popper<'a>,
}

impl<'a> ResultReader<'a> {
    /// A reader over the whole result-stack buffer (no header).
    pub fn new(body: &'a [u8]) -> Self {
        Self {
            pop: Popper::new(body),
        }
    }

    /// Unread bytes remaining before the pop cursor.
    pub fn remaining(&self) -> usize {
        self.pop.cursor
    }

    /// The bytes not yet popped, from the start of the buffer (e.g. a trailing
    /// packed-slot payload after the status fields have been popped).
    pub fn rest(&self) -> &'a [u8] {
        &self.pop.body[..self.pop.cursor]
    }

    /// Pop one octet.
    pub fn pop_u8(&mut self) -> Result<u8, Underflow> {
        self.pop.u8()
    }
    /// Pop a boolean.
    pub fn pop_bool(&mut self) -> Result<bool, Underflow> {
        Ok(self.pop.u8()? != 0)
    }
    /// Pop a big-endian `u16`.
    pub fn pop_u16(&mut self) -> Result<u16, Underflow> {
        self.pop.u16()
    }
    /// Pop a big-endian `u32`.
    pub fn pop_u32(&mut self) -> Result<u32, Underflow> {
        self.pop.u32()
    }
    /// Pop a big-endian `u64`.
    pub fn pop_u64(&mut self) -> Result<u64, Underflow> {
        self.pop.u64()
    }
    /// Pop a string in the trailing-`u32` SCSU form.
    pub fn pop_string(&mut self) -> Result<String, Underflow> {
        self.pop.string()
    }
    /// Pop raw bytes in the Pascal-style `Push(uchar const*)` form: the bytes
    /// followed by a trailing 1-byte length (spec 4.2).
    pub fn pop_short_bytes(&mut self) -> Result<&'a [u8], Underflow> {
        let len = usize::from(self.pop.u8()?);
        self.pop.take_tail(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real op90 (Download With Lock) transaction from the s03 capture:
    // schema read, no credentials.
    const OP90: &[u8] = &[
        0x5a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x84, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x01, 0x02, 0x02,
    ];

    #[test]
    fn parses_captured_op90_header() {
        let header = Header::parse(OP90).unwrap();
        assert_eq!(header.opcode, 90);
        assert_eq!(header.transid_lo, 1);
        assert_eq!(header.session_id, 1);
        assert_eq!(header.file_id, 132); // 0x84: the open file, at offset 12
    }

    #[test]
    fn header_rebuilds_byte_exact() {
        let header = Header::parse(OP90).unwrap();
        let mut out = Vec::new();
        header.write(&mut out);
        assert_eq!(out, &OP90[..HEADER_LEN]);
    }

    #[test]
    fn typed_push_pop_round_trip() {
        // Push in order; pop in reverse.
        let bytes = Builder::new(80)
            .push_u32(0xDEAD_BEEF)
            .push_string("admin")
            .push_u16(8)
            .push_bool(true)
            .finish();

        let mut r = Reader::parse(&bytes).unwrap();
        assert_eq!(r.header().opcode, 80);
        assert!(r.pop_bool().unwrap());
        assert_eq!(r.pop_u16().unwrap(), 8);
        assert_eq!(r.pop_string().unwrap(), "admin");
        assert_eq!(r.pop_u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn short_bytes_pop_uses_trailing_u8_length() {
        // The captured Login reply stack: u16 version 9, key [01 03] + len 2, u16 error 0.
        let stack = [0x00, 0x09, 0x01, 0x03, 0x02, 0x00, 0x00];
        let mut r = ResultReader::new(&stack);
        assert_eq!(r.pop_u16().unwrap(), 0);
        assert_eq!(r.pop_short_bytes().unwrap(), &[0x01, 0x03]);
        assert_eq!(r.pop_u16().unwrap(), 9);
        assert_eq!(r.remaining(), 0);
        assert_eq!(ResultReader::new(&[5]).pop_short_bytes(), Err(Underflow));
    }

    #[test]
    fn pop_past_start_is_underflow() {
        let bytes = Builder::new(1).push_u8(7).finish();
        let mut r = Reader::parse(&bytes).unwrap();
        assert_eq!(r.pop_u8().unwrap(), 7);
        assert_eq!(r.pop_u8(), Err(Underflow));
    }

    #[test]
    fn result_builder_round_trips_through_result_reader() {
        let stack = ResultBuilder::new()
            .push_u16(9)
            .push_short_bytes(&[1, 3])
            .push_string("server")
            .push_u64(0x0102_0304_0506_0708)
            .push_u16(0)
            .finish();
        let mut r = ResultReader::new(&stack);
        assert_eq!(r.pop_u16(), Ok(0));
        assert_eq!(r.pop_u64(), Ok(0x0102_0304_0506_0708));
        assert_eq!(r.pop_string().as_deref(), Ok("server"));
        assert_eq!(r.pop_short_bytes(), Ok(&[1u8, 3][..]));
        assert_eq!(r.pop_u16(), Ok(9));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn reader_pops_short_bytes_and_exposes_rest() {
        let txn = Builder::new(90)
            .push_raw(&[7, 7])
            .push_raw(&[1, 2, 2])
            .finish();
        let mut r = Reader::parse(&txn).unwrap();
        assert_eq!(r.pop_short_bytes(), Ok(&[1u8, 2][..]));
        assert_eq!(r.rest(), &[7, 7]);
        assert_eq!(r.pop_raw(2), Ok(&[7u8, 7][..]));
    }
}
