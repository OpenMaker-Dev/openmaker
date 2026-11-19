//! Reading whole GIOP messages from a byte stream.
//!
//! Shared by the client and server transports: both read the same framing off
//! a TLS stream, and both must join GIOP 1.2 fragments into one logical message.

use std::io::Read;

use crate::{parse_header, MsgType, ParseError, HEADER_LEN};

/// Errors reading a GIOP message from a stream.
#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    /// The peer closed the stream before a full message arrived.
    Closed,
    /// The peer advertised a message larger than the reader's limit.
    TooLarge(usize),
    /// The bytes are not a GIOP message this crate understands.
    Giop(ParseError),
    /// A fragmented message continued with something other than a Fragment.
    ExpectedFragment,
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        ReadError::Io(e)
    }
}

impl core::fmt::Display for ReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "io: {e}"),
            ReadError::Closed => f.write_str("connection closed mid-message"),
            ReadError::TooLarge(size) => {
                write!(
                    f,
                    "peer advertised an oversized GIOP message ({size} bytes)"
                )
            }
            ReadError::Giop(e) => write!(f, "giop: {e}"),
            ReadError::ExpectedFragment => f.write_str("expected a GIOP fragment"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Buffers a byte stream and yields one GIOP message at a time.
///
/// A read error (such as a socket read timeout) loses nothing: bytes of an
/// incomplete message and the fragments joined so far are kept, and the next
/// call goes on from there.
#[derive(Debug)]
pub struct MessageReader {
    buf: Vec<u8>,
    max_len: usize,
    /// A logical message whose later fragments have not all arrived.
    joined: Option<Vec<u8>>,
}

impl MessageReader {
    /// A reader that refuses messages (and joined fragment chains) longer than
    /// `max_len` bytes, header included.
    pub fn new(max_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_len,
            joined: None,
        }
    }

    /// Read exactly one GIOP message (a single fragment, if the sender
    /// fragmented), buffering any extra bytes for the next call.
    ///
    /// The declared size is checked against the limit before the body is
    /// buffered, so a peer advertising a huge size cannot exhaust memory.
    pub fn read_message(&mut self, stream: &mut impl Read) -> Result<Vec<u8>, ReadError> {
        loop {
            if self.buf.len() >= HEADER_LEN {
                // The size lives at offset 8 (little-endian; parse_header
                // rejects big-endian messages).
                let size = u32::from_le_bytes(self.buf[8..12].try_into().unwrap()) as usize;
                if HEADER_LEN + size > self.max_len {
                    return Err(ReadError::TooLarge(size));
                }
            }
            match parse_header(&self.buf) {
                Ok((_, total)) => return Ok(self.buf.drain(..total).collect()),
                Err(ParseError::Incomplete) => {}
                Err(e) => return Err(ReadError::Giop(e)),
            }
            let mut chunk = [0u8; 16 * 1024];
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Err(ReadError::Closed);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Read one logical message, joining GIOP 1.2 fragments: the result carries
    /// the first message's header with its size covering the whole body and the
    /// "fragment follows" flag cleared.
    pub fn read_logical(&mut self, stream: &mut impl Read) -> Result<Vec<u8>, ReadError> {
        let mut msg = match self.joined.take() {
            Some(msg) => msg,
            None => {
                let msg = self.read_message(stream)?;
                let (header, _) = parse_header(&msg).map_err(ReadError::Giop)?;
                if !header.fragment_follows {
                    return Ok(msg);
                }
                msg
            }
        };
        loop {
            let frag = match self.read_message(stream) {
                Ok(frag) => frag,
                Err(e) => {
                    self.joined = Some(msg);
                    return Err(e);
                }
            };
            let (fh, _) = parse_header(&frag).map_err(ReadError::Giop)?;
            if fh.msg_type != MsgType::Fragment {
                return Err(ReadError::ExpectedFragment);
            }
            // A 1.2 Fragment body is the u32 request id, then the continued data.
            let data = frag
                .get(HEADER_LEN + 4..)
                .ok_or(ReadError::Giop(ParseError::Underflow))?;
            if msg.len() + data.len() > self.max_len {
                return Err(ReadError::TooLarge(msg.len() + data.len() - HEADER_LEN));
            }
            msg.extend_from_slice(data);
            if !fh.fragment_follows {
                break;
            }
        }
        let size = (msg.len() - HEADER_LEN) as u32;
        msg[8..12].copy_from_slice(&size.to_le_bytes());
        msg[6] &= !2;
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_reply, Reply, ReplyStatus};

    /// Split a Reply into a first message plus Fragments carrying `chunk` bytes each.
    fn fragment(msg: &[u8], request_id: u32, first_body: usize, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let (first, mut rest) = msg.split_at(HEADER_LEN + first_body);
        let mut head = first.to_vec();
        head[6] |= 2;
        head[8..12].copy_from_slice(&(first_body as u32).to_le_bytes());
        out.extend_from_slice(&head);
        while !rest.is_empty() {
            let take = chunk.min(rest.len());
            let (piece, tail) = rest.split_at(take);
            rest = tail;
            let flags = if rest.is_empty() { 1 } else { 3 };
            out.extend_from_slice(b"GIOP");
            out.extend_from_slice(&[1, 2, flags, 7]);
            out.extend_from_slice(&((4 + piece.len()) as u32).to_le_bytes());
            out.extend_from_slice(&request_id.to_le_bytes());
            out.extend_from_slice(piece);
        }
        out
    }

    #[test]
    fn reads_back_to_back_messages() {
        let a = build_reply(2, ReplyStatus::NoException, &[], &[1]);
        let b = build_reply(4, ReplyStatus::NoException, &[], &[0]);
        let stream = [a.clone(), b.clone()].concat();
        let mut reader = MessageReader::new(1 << 20);
        let mut src = &stream[..];
        assert_eq!(reader.read_message(&mut src).unwrap(), a);
        assert_eq!(reader.read_message(&mut src).unwrap(), b);
        assert!(matches!(
            reader.read_message(&mut src),
            Err(ReadError::Closed)
        ));
    }

    #[test]
    fn joins_fragments() {
        let body: Vec<u8> = (0..100u8).collect();
        let whole = build_reply(8, ReplyStatus::NoException, &[], &body);
        let stream = fragment(&whole, 8, 16, 24);
        let mut reader = MessageReader::new(1 << 20);
        let joined = reader.read_logical(&mut &stream[..]).unwrap();
        assert_eq!(joined, whole);
        assert_eq!(Reply::parse(&joined).unwrap().body, &body[..]);
    }

    /// A stream that hands out its bytes in the given pieces, with a
    /// timeout between them.
    struct Stalling {
        pieces: Vec<Vec<u8>>,
        stall: bool,
    }

    impl Read for Stalling {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.stall {
                self.stall = false;
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            self.stall = true;
            if self.pieces.is_empty() {
                return Ok(0);
            }
            let piece = self.pieces.remove(0);
            out[..piece.len()].copy_from_slice(&piece);
            Ok(piece.len())
        }
    }

    #[test]
    fn a_timeout_between_fragments_loses_nothing() {
        let body: Vec<u8> = (0..100u8).collect();
        let whole = build_reply(8, ReplyStatus::NoException, &[], &body);
        let stream = fragment(&whole, 8, 16, 24);
        // Cut the stream every 13 bytes, mid-header and mid-fragment.
        let mut source = Stalling {
            pieces: stream.chunks(13).map(<[u8]>::to_vec).collect(),
            stall: false,
        };
        let mut reader = MessageReader::new(1 << 20);
        let joined = loop {
            match reader.read_logical(&mut source) {
                Ok(msg) => break msg,
                Err(ReadError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        };
        assert_eq!(joined, whole);
    }

    #[test]
    fn refuses_oversized_message_before_reading_body() {
        let msg = build_reply(2, ReplyStatus::NoException, &[], &[0; 64]);
        let mut reader = MessageReader::new(32);
        assert!(matches!(
            reader.read_message(&mut &msg[..]),
            Err(ReadError::TooLarge(_))
        ));
    }
}
