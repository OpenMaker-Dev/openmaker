//! CORBA GIOP 1.2 framing for the FileMaker RCFMP12 transport.
//!
//! Only what OpenMaker needs: little-endian GIOP 1.2 Request and Reply messages
//! carrying the `RCFMP12` operations, with the KeyAddr target form and the
//! trailing service-context list. Incoming GIOP 1.2 fragments are joined by
//! [`MessageReader::read_logical`]; outgoing messages are never fragmented.
//!
//! Spec: `docs/re/network-protocol.md` section 2 and 3. GIOP itself is the
//! public OMG CORBA standard.

pub mod cdr;
pub mod io;

use cdr::Underflow;
pub use cdr::{Reader, Writer};
pub use io::{MessageReader, ReadError};

/// GIOP message header length (magic, version, flags, type, size).
pub const HEADER_LEN: usize = 12;

const MAGIC: &[u8; 4] = b"GIOP";

/// GIOP message types used here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgType {
    Request,
    Reply,
    CancelRequest,
    LocateRequest,
    LocateReply,
    CloseConnection,
    MessageError,
    Fragment,
    Other(u8),
}

impl MsgType {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Request,
            1 => Self::Reply,
            2 => Self::CancelRequest,
            3 => Self::LocateRequest,
            4 => Self::LocateReply,
            5 => Self::CloseConnection,
            6 => Self::MessageError,
            7 => Self::Fragment,
            other => Self::Other(other),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Request => 0,
            Self::Reply => 1,
            Self::CancelRequest => 2,
            Self::LocateRequest => 3,
            Self::LocateReply => 4,
            Self::CloseConnection => 5,
            Self::MessageError => 6,
            Self::Fragment => 7,
            Self::Other(v) => v,
        }
    }
}

/// Reply status values (GIOP 1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyStatus {
    NoException,
    UserException,
    SystemException,
    LocationForward,
    LocationForwardPerm,
    NeedsAddressingMode,
    Other(u32),
}

impl ReplyStatus {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::NoException,
            1 => Self::UserException,
            2 => Self::SystemException,
            3 => Self::LocationForward,
            4 => Self::LocationForwardPerm,
            5 => Self::NeedsAddressingMode,
            other => Self::Other(other),
        }
    }
}

/// One IOP service context (id plus opaque encapsulated bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceContext {
    pub id: u32,
    pub data: Vec<u8>,
}

/// Errors parsing a GIOP message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParseError {
    /// Fewer than one full message is available.
    Incomplete,
    /// Not a GIOP message (bad magic).
    BadMagic,
    /// Only GIOP 1.x little-endian is supported.
    Unsupported,
    /// A field ran past the message end.
    Underflow,
}

impl From<Underflow> for ParseError {
    fn from(_: Underflow) -> Self {
        ParseError::Underflow
    }
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            ParseError::Incomplete => "incomplete GIOP message",
            ParseError::BadMagic => "not a GIOP message",
            ParseError::Unsupported => "unsupported GIOP message or version",
            ParseError::Underflow => "GIOP field ran past the message end",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ParseError {}

/// A parsed GIOP message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version_minor: u8,
    pub little_endian: bool,
    pub fragment_follows: bool,
    pub msg_type: MsgType,
    pub size: usize,
}

/// Parse the 12-byte GIOP header; returns the header and total message length.
pub fn parse_header(buf: &[u8]) -> Result<(Header, usize), ParseError> {
    if buf.len() < HEADER_LEN {
        return Err(ParseError::Incomplete);
    }
    if &buf[0..4] != MAGIC {
        return Err(ParseError::BadMagic);
    }
    if buf[4] != 1 {
        return Err(ParseError::Unsupported);
    }
    let flags = buf[6];
    let little_endian = flags & 1 != 0;
    if !little_endian {
        return Err(ParseError::Unsupported);
    }
    let size = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
    let total = HEADER_LEN + size;
    if buf.len() < total {
        return Err(ParseError::Incomplete);
    }
    Ok((
        Header {
            version_minor: buf[5],
            little_endian,
            fragment_follows: flags & 2 != 0,
            msg_type: MsgType::from_u8(buf[7]),
            size,
        },
        total,
    ))
}

/// A parsed GIOP Request (1.2, KeyAddr form).
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub request_id: u32,
    pub response_expected: bool,
    pub object_key: &'a [u8],
    pub operation: &'a [u8],
    pub service_contexts: Vec<ServiceContext>,
    /// Body bytes after the header, starting at the 8-aligned body offset.
    pub body: &'a [u8],
}

/// A parsed GIOP Reply (1.2).
#[derive(Clone, Debug)]
pub struct Reply<'a> {
    pub request_id: u32,
    pub status: ReplyStatus,
    pub service_contexts: Vec<ServiceContext>,
    pub body: &'a [u8],
}

fn read_service_contexts(r: &mut Reader) -> Result<Vec<ServiceContext>, ParseError> {
    let count = r.u32()?;
    let mut out = Vec::with_capacity(count.min(64) as usize);
    for _ in 0..count {
        let id = r.u32()?;
        let data = r.octet_sequence()?.to_vec();
        out.push(ServiceContext { id, data });
    }
    Ok(out)
}

impl<'a> Request<'a> {
    /// Parse a complete Request message (header included).
    pub fn parse(msg: &'a [u8]) -> Result<Self, ParseError> {
        let (header, total) = parse_header(msg)?;
        if header.msg_type != MsgType::Request {
            return Err(ParseError::Unsupported);
        }
        let mut r = Reader::new(&msg[..total], HEADER_LEN);
        let request_id = r.u32()?;
        let response_flags = r.u8()?;
        r.bytes(3)?; // reserved
        let disposition = r.u16()?; // TargetAddress discriminant
        if disposition != 0 {
            return Err(ParseError::Unsupported); // only KeyAddr
        }
        let object_key = r.octet_sequence()?;
        let operation = r.string()?;
        let service_contexts = read_service_contexts(&mut r)?;
        // The request body is 8-aligned after the header.
        let mut body_at = r.pos();
        body_at += (8 - (body_at % 8)) % 8;
        let body = msg.get(body_at..total).unwrap_or(&[]);
        Ok(Request {
            request_id,
            response_expected: response_flags & 1 != 0,
            object_key,
            operation,
            service_contexts,
            body,
        })
    }
}

impl<'a> Reply<'a> {
    /// Parse a complete Reply message (header included).
    pub fn parse(msg: &'a [u8]) -> Result<Self, ParseError> {
        let (header, total) = parse_header(msg)?;
        if header.msg_type != MsgType::Reply {
            return Err(ParseError::Unsupported);
        }
        let mut r = Reader::new(&msg[..total], HEADER_LEN);
        let request_id = r.u32()?;
        let status = ReplyStatus::from_u32(r.u32()?);
        let service_contexts = read_service_contexts(&mut r)?;
        let mut body_at = r.pos();
        body_at += (8 - (body_at % 8)) % 8;
        let body = msg.get(body_at..total).unwrap_or(&[]);
        Ok(Reply {
            request_id,
            status,
            service_contexts,
            body,
        })
    }
}

/// Build a GIOP 1.2 little-endian Request message.
///
/// `body` is the already-marshalled request body (CDR built with an origin of
/// the 8-aligned body offset).
pub fn build_request(
    request_id: u32,
    response_expected: bool,
    object_key: &[u8],
    operation: &[u8],
    service_contexts: &[ServiceContext],
    body: &[u8],
) -> Vec<u8> {
    // Header body is written with origin = HEADER_LEN so alignment matches.
    let mut w = Writer::with_origin(HEADER_LEN);
    w.u32(request_id);
    w.u8(if response_expected { 3 } else { 0 });
    w.bytes(&[0, 0, 0]); // reserved
    w.u16(0); // TargetAddress: KeyAddr
    w.octet_sequence(object_key);
    w.string(operation);
    write_service_contexts(&mut w, service_contexts);
    frame(MsgType::Request, w.into_bytes(), body)
}

/// Build a GIOP 1.2 little-endian Reply message.
pub fn build_reply(
    request_id: u32,
    status: ReplyStatus,
    service_contexts: &[ServiceContext],
    body: &[u8],
) -> Vec<u8> {
    let mut w = Writer::with_origin(HEADER_LEN);
    w.u32(request_id);
    w.u32(match status {
        ReplyStatus::NoException => 0,
        ReplyStatus::UserException => 1,
        ReplyStatus::SystemException => 2,
        ReplyStatus::LocationForward => 3,
        ReplyStatus::LocationForwardPerm => 4,
        ReplyStatus::NeedsAddressingMode => 5,
        ReplyStatus::Other(v) => v,
    });
    write_service_contexts(&mut w, service_contexts);
    frame(MsgType::Reply, w.into_bytes(), body)
}

/// The request id of a GIOP 1.2 LocateRequest (a client asking whether an
/// object lives here before invoking it).
pub fn parse_locate_request(msg: &[u8]) -> Result<u32, ParseError> {
    let (header, total) = parse_header(msg)?;
    if header.msg_type != MsgType::LocateRequest {
        return Err(ParseError::Unsupported);
    }
    Ok(Reader::new(&msg[..total], HEADER_LEN).u32()?)
}

/// The `OBJECT_HERE` locate status: the object lives at the address asked.
pub const OBJECT_HERE: u32 = 1;

/// Build a GIOP 1.2 LocateReply saying the object is here (`OBJECT_HERE`).
pub fn build_locate_reply_here(request_id: u32) -> Vec<u8> {
    let mut w = Writer::with_origin(HEADER_LEN);
    w.u32(request_id);
    w.u32(OBJECT_HERE);
    frame(MsgType::LocateReply, w.into_bytes(), &[])
}

/// Build a GIOP 1.2 LocateRequest asking whether the object `object_key`
/// lives on the other side (KeyAddr form). FileMaker Server sends one
/// before its first call on a client's callback object
/// (network-protocol.md 6.5).
pub fn build_locate_request(request_id: u32, object_key: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_origin(HEADER_LEN);
    w.u32(request_id);
    w.u16(0); // TargetAddress: KeyAddr
    w.octet_sequence(object_key);
    frame(MsgType::LocateRequest, w.into_bytes(), &[])
}

/// The request id and locate status of a GIOP 1.2 LocateReply
/// ([`OBJECT_HERE`], or another status: `UNKNOWN_OBJECT` 0, a forward 2
/// to 5).
pub fn parse_locate_reply(msg: &[u8]) -> Result<(u32, u32), ParseError> {
    let (header, total) = parse_header(msg)?;
    if header.msg_type != MsgType::LocateReply {
        return Err(ParseError::Unsupported);
    }
    let mut r = Reader::new(&msg[..total], HEADER_LEN);
    Ok((r.u32()?, r.u32()?))
}

/// CORBA completion status of a system exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Yes,
    No,
    Maybe,
}

/// Build a GIOP 1.2 Reply carrying a CORBA system exception, e.g.
/// `IDL:omg.org/CORBA/BAD_OPERATION:1.0` for an unknown operation. The body is
/// the standard `string repository id, ulong minor, ulong completed`.
pub fn build_system_exception_reply(
    request_id: u32,
    repository_id: &str,
    minor: u32,
    completed: Completion,
) -> Vec<u8> {
    // The body starts 8-aligned, so origin 0 aligns the same way.
    let mut body = Writer::with_origin(0);
    body.string(repository_id.as_bytes());
    body.u32(minor);
    body.u32(match completed {
        Completion::Yes => 0,
        Completion::No => 1,
        Completion::Maybe => 2,
    });
    build_reply(
        request_id,
        ReplyStatus::SystemException,
        &[],
        &body.into_bytes(),
    )
}

/// Build a GIOP 1.2 CloseConnection: the header alone. FileMaker Server
/// sends it before closing a connection that has been silent too long
/// (network-protocol.md 6.2).
pub fn build_close_connection() -> Vec<u8> {
    frame(MsgType::CloseConnection, Vec::new(), &[])
}

fn write_service_contexts(w: &mut Writer, contexts: &[ServiceContext]) {
    w.u32(contexts.len() as u32);
    for ctx in contexts {
        w.u32(ctx.id);
        w.octet_sequence(&ctx.data);
    }
}

fn frame(msg_type: MsgType, mut header_body: Vec<u8>, body: &[u8]) -> Vec<u8> {
    if !body.is_empty() {
        // Pad the header body so the message body lands on an 8-byte boundary.
        let abs = HEADER_LEN + header_body.len();
        let pad = (8 - (abs % 8)) % 8;
        header_body.resize(header_body.len() + pad, 0);
        header_body.extend_from_slice(body);
    }
    let mut msg = Vec::with_capacity(HEADER_LEN + header_body.len());
    msg.extend_from_slice(MAGIC);
    msg.extend_from_slice(&[1, 2]); // version 1.2
    msg.push(1); // flags: little-endian
    msg.push(msg_type.to_u8());
    msg.extend_from_slice(&(header_body.len() as u32).to_le_bytes());
    msg.extend_from_slice(&header_body);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_connection_is_a_bare_header() {
        // FileMaker Server's, from capture s02 conn-001.
        let captured = [0x47, 0x49, 0x4f, 0x50, 1, 2, 1, 5, 0, 0, 0, 0];
        assert_eq!(build_close_connection(), captured);
        let (header, len) = parse_header(&captured).unwrap();
        assert_eq!((header.msg_type, len), (MsgType::CloseConnection, 12));
    }

    /// The first client message in the s03 capture: _is_a for IDL:RCFMP12:1.0.
    /// The client's three BiDirIIOP listen addresses are replaced by
    /// documentation addresses of the same lengths (RFC 5737, RFC 3849).
    const IS_A: &[u8] = &[
        0x47, 0x49, 0x4f, 0x50, 0x01, 0x02, 0x01, 0x00, 0xa8, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc7, 0xd5, 0x17, 0x00, 0x00, 0x00, 0xff, 0x6f,
        0x6d, 0x6e, 0x69, 0x49, 0x4e, 0x53, 0x50, 0x4f, 0x41, 0xff, 0x46, 0x4d, 0x50, 0x4f, 0x41,
        0x00, 0x46, 0x4d, 0x52, 0x50, 0x4f, 0xc0, 0x06, 0x00, 0x00, 0x00, 0x5f, 0x69, 0x73, 0x5f,
        0x61, 0x00, 0xfb, 0x04, 0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x54, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x31, 0x39,
        0x38, 0x2e, 0x35, 0x31, 0x2e, 0x31, 0x30, 0x30, 0x2e, 0x31, 0x00, 0x00, 0x8b, 0x13, 0x0f,
        0x00, 0x00, 0x00, 0x31, 0x39, 0x38, 0x2e, 0x35, 0x31, 0x2e, 0x31, 0x30, 0x30, 0x2e, 0x31,
        0x39, 0x36, 0x00, 0x00, 0x8b, 0x13, 0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0x32, 0x30, 0x30,
        0x31, 0x3a, 0x30, 0x64, 0x62, 0x38, 0x3a, 0x61, 0x31, 0x65, 0x30, 0x3a, 0x3a, 0x33, 0x39,
        0x32, 0x66, 0x3a, 0x65, 0x65, 0x36, 0x31, 0x00, 0x8b, 0x13, 0x10, 0x00, 0x00, 0x00, 0x49,
        0x44, 0x4c, 0x3a, 0x52, 0x43, 0x46, 0x4d, 0x50, 0x31, 0x32, 0x3a, 0x31, 0x2e, 0x30, 0x00,
    ];

    const FMRPO_KEY: &[u8] = b"\xffomniINSPOA\xffFMPOA\x00FMRPO";

    #[test]
    fn parses_captured_is_a() {
        let req = Request::parse(IS_A).unwrap();
        assert_eq!(req.request_id, 2);
        assert!(req.response_expected);
        assert_eq!(req.object_key, FMRPO_KEY);
        assert_eq!(req.operation, b"_is_a");
        // One BiDirIIOP service context, id 5.
        assert_eq!(req.service_contexts.len(), 1);
        assert_eq!(req.service_contexts[0].id, 5);
        // Body is the _is_a argument: the repository-id string.
        assert!(req.body.ends_with(b"IDL:RCFMP12:1.0\0"));
    }

    #[test]
    fn rebuilt_is_a_reparses_identically() {
        let req = Request::parse(IS_A).unwrap();
        // Rebuild the body at its true message offset so CDR alignment matches.
        let body_origin = IS_A.len() - req.body.len();
        let mut w = Writer::with_origin(body_origin);
        w.string(b"IDL:RCFMP12:1.0");
        let rebuilt = build_request(
            req.request_id,
            req.response_expected,
            req.object_key,
            req.operation,
            &req.service_contexts,
            &w.into_bytes(),
        );
        let reparsed = Request::parse(&rebuilt).unwrap();
        assert_eq!(reparsed.request_id, 2);
        assert_eq!(reparsed.object_key, FMRPO_KEY);
        assert_eq!(reparsed.operation, b"_is_a");
        assert_eq!(reparsed.service_contexts, req.service_contexts);
        assert!(reparsed.body.ends_with(b"IDL:RCFMP12:1.0\0"));
    }

    #[test]
    fn reply_round_trip() {
        let body = [0x01]; // boolean TRUE
        let msg = build_reply(2, ReplyStatus::NoException, &[], &body);
        let reply = Reply::parse(&msg).unwrap();
        assert_eq!(reply.request_id, 2);
        assert_eq!(reply.status, ReplyStatus::NoException);
        assert_eq!(reply.body, &body);
    }

    #[test]
    fn detects_incomplete_message() {
        assert_eq!(parse_header(&IS_A[..20]), Err(ParseError::Incomplete));
    }

    #[test]
    fn system_exception_reply() {
        let msg = build_system_exception_reply(
            6,
            "IDL:omg.org/CORBA/BAD_OPERATION:1.0",
            0,
            Completion::No,
        );
        let reply = Reply::parse(&msg).unwrap();
        assert_eq!(reply.status, ReplyStatus::SystemException);
        let mut r = Reader::new(&msg, msg.len() - reply.body.len());
        assert_eq!(r.string().unwrap(), b"IDL:omg.org/CORBA/BAD_OPERATION:1.0");
        assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (0, 1));
    }

    #[test]
    fn locate_request_and_reply() {
        // FileMaker Server's LocateRequest for FileMaker Pro's callback
        // object and the client's LocateReply, from capture s07 conn-000
        // (network-protocol.md 6.5).
        let request = [
            &b"GIOP\x01\x02\x01\x03\x23\x00\x00\x00\x03\x00\x00\x00\x00\x00\x00\x00\x17\x00\x00\x00"[..],
            FMRPO_KEY,
        ]
        .concat();
        assert_eq!(build_locate_request(3, FMRPO_KEY), request);
        assert_eq!(parse_locate_request(&request), Ok(3));
        let reply = b"GIOP\x01\x02\x01\x04\x08\x00\x00\x00\x03\x00\x00\x00\x01\x00\x00\x00";
        assert_eq!(build_locate_reply_here(3), reply);
        assert_eq!(parse_locate_reply(reply), Ok((3, OBJECT_HERE)));
        assert_eq!(
            parse_locate_reply(&request),
            Err(ParseError::Unsupported),
            "a LocateRequest is not a reply"
        );
    }
}
