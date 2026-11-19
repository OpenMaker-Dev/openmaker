//! Parsing `Perform` replies.
//!
//! `Perform`'s wire return is `void`; the reply body is two CDR `sequence<octet>`
//! in IDL order: the inout `parms` (the response) then the out `backchannel`
//! (a server-CPU/idle piggyback starting `0xFE`, ignorable by app logic). The
//! response is a header-less LIFO result stack: pop from the tail, status first.
//!
//! Spec: `docs/re/network-protocol.md` section 5.6.

use om_giop::{Reader as CdrReader, Reply, ReplyStatus};
use om_rctransaction::{Builder, Reader, ResultBuilder, ResultReader};

use crate::{build_perform, Opcode};

/// Build an Open (op78) `Perform` request for a hosted file.
///
/// The op78 transaction body (server pops from the tail: version, host,
/// filename, filepath, file-handle) is: `push u64 file-handle(0)`, empty
/// filepath string, filename string, host string, `u16 version(8)`. Target file
/// and key in the header are 0 (the file is not open yet). Spec 5.5.
pub fn build_open(request_id: u32, connection_id: u32, host: &str, filename: &str) -> Vec<u8> {
    let mut builder = Builder::new(Opcode::OpenFile as u8);
    // The captured Open used transaction id 1 (id 0 is never seen on the wire).
    builder.header_mut().transid_lo = 1;
    let txn = builder
        .push_u64(0) // file handle (assigned by the reply)
        .push_string("") // file path (empty in observed traffic)
        .push_string(filename) // e.g. "test project.fmp12"
        .push_string(host) // dialed host
        .push_u16(8) // protocol/version constant
        .finish();
    build_perform(request_id, connection_id, b"", &txn)
}

/// A parsed Open (op78) request (the server side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    /// The client's file handle (0 before the file is open).
    pub file_handle: u64,
    pub file_path: String,
    /// The hosted file name, e.g. `test project.fmp12`.
    pub filename: String,
    /// The host name the client dialed.
    pub host: String,
    pub version: u16,
}

/// Parse an Open (op78) transaction, popping the fields [`build_open`] pushes.
pub fn parse_open_request(transaction: &[u8]) -> Result<OpenRequest, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let version = r.pop_u16()?;
    let host = r.pop_string()?;
    let filename = r.pop_string()?;
    let file_path = r.pop_string()?;
    let file_handle = r.pop_u64()?;
    Ok(OpenRequest {
        file_handle,
        file_path,
        filename,
        host,
        version,
    })
}

/// A Request Notification (op63) transaction (spec 5.6.9; write-path.md
/// 6.1): the client asks to be told about changes to an opened file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestNotification {
    /// The notification codes wanted; `ffffffff` (all) from FileMaker Pro.
    pub attention_mask: u32,
    /// The client's session id to address notifications to; `1`.
    pub message_address: u32,
    /// The watched path; empty = the whole file.
    pub path: Vec<Vec<u8>>,
    /// The opened file (equals the header's `file_id`).
    pub file_id: u32,
    /// The client's own handle for the file, `-4` for the first file and 2
    /// lower for each file opened after it; notifications name it.
    pub client_file_id: i32,
}

/// Build a Request Notification (op63) transaction.
pub fn build_request_notification(transaction_id: u32, req: &RequestNotification) -> Vec<u8> {
    let header = crate::download::file_header(
        crate::Opcode::RequestNotification,
        transaction_id,
        req.file_id,
    );
    let path = crate::download::encode_path(&req.path);
    om_rctransaction::Builder::with_header(header)
        .push_u32(req.attention_mask)
        .push_u32(req.message_address)
        .push_raw(&path)
        .push_u8(path.len() as u8)
        .push_u32(req.file_id)
        .push_u32(req.client_file_id as u32)
        .finish()
}

/// Parse a Request Notification (op63) transaction.
pub fn parse_request_notification(transaction: &[u8]) -> Result<RequestNotification, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let client_file_id = r.pop_u32()? as i32;
    let file_id = r.pop_u32()?;
    let path = crate::download::decode_path(r.pop_short_bytes()?)?;
    let message_address = r.pop_u32()?;
    let attention_mask = r.pop_u32()?;
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(RequestNotification {
        attention_mask,
        message_address,
        path,
        file_id,
        client_file_id,
    })
}

/// A parsed `Perform` reply: the response result stack and the backchannel.
#[derive(Clone, Debug)]
pub struct PerformReply {
    /// The inout `parms` sequence: a header-less LIFO result stack.
    pub result: Vec<u8>,
    /// The out `backchannel` sequence (server-CPU/idle piggyback), kept for completeness.
    pub backchannel: Vec<u8>,
}

/// Error parsing a `Perform` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PerformError {
    Giop(String),
    Status(String),
    Truncated,
    /// A result-stack field could not be popped (short/corrupt response).
    Underflow,
    /// A FileMaker-level error code popped from the result stack tail.
    Server(u16),
    /// An Open refusal that carried a message string.
    OpenError(u16, String),
    /// A reply could not be built: a value of this many bytes is longer
    /// than the one-byte length of its field.
    TooLong(usize),
}

impl core::fmt::Display for PerformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PerformError::Giop(e) => write!(f, "perform reply not valid GIOP: {e}"),
            PerformError::Status(s) => write!(f, "perform reply status {s}"),
            PerformError::Truncated => f.write_str("perform reply truncated"),
            PerformError::Underflow => f.write_str("perform result stack underflow"),
            PerformError::Server(code) => write!(f, "server error code 0x{code:04x}"),
            PerformError::OpenError(code, msg) => {
                write!(f, "open error 0x{code:04x}: {msg}")
            }
            PerformError::TooLong(len) => {
                write!(f, "a {len}-byte value is too long for its field")
            }
        }
    }
}
impl std::error::Error for PerformError {}

impl From<om_rctransaction::Underflow> for PerformError {
    fn from(_: om_rctransaction::Underflow) -> Self {
        PerformError::Underflow
    }
}

impl From<om_rctransaction::TooLong> for PerformError {
    fn from(e: om_rctransaction::TooLong) -> Self {
        PerformError::TooLong(e.len)
    }
}

/// Split a `Perform` reply message into its two octet sequences.
pub fn parse_perform_reply(message: &[u8]) -> Result<PerformReply, PerformError> {
    let reply = Reply::parse(message).map_err(|e| PerformError::Giop(format!("{e}")))?;
    if reply.status != ReplyStatus::NoException {
        return Err(PerformError::Status(format!("{:?}", reply.status)));
    }
    let body_offset = message.len() - reply.body.len();
    let mut r = CdrReader::new(message, body_offset);
    let result = r
        .octet_sequence()
        .map_err(|_| PerformError::Truncated)?
        .to_vec();
    let backchannel = r
        .octet_sequence()
        .map_err(|_| PerformError::Truncated)?
        .to_vec();
    Ok(PerformReply {
        result,
        backchannel,
    })
}

/// A successful Open (op78) reply (spec 5.6.4), fields in pop order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReply {
    pub status: u16,
    pub server_message: String,
    /// `file[0x3ac]`: half of the login key-derivation nonce (spec 4.3 / 5.6.4).
    pub file_nonce: u32,
    /// `file[0xc]` flag.
    pub flag: bool,
    /// `file[0xaa0]`: the server's id for the opened file. Later transactions on
    /// the file carry it as the header `file_id`, and Login pushes it last.
    pub file_id: u32,
    /// `file[0xaa4]`.
    pub file_aa4: u16,
    /// The status-0 marker byte.
    pub marker: u8,
    /// `file[0x170]` string (empty in observed traffic).
    pub file_text: String,
    /// A flags word (`0x00a10000` in observed traffic).
    pub flags: u32,
    /// `file[0xc08]`: the session/incarnation value.
    pub session_value: u64,
}

impl OpenReply {
    /// The result stack a server sends: every field of spec 5.6.4, pushed in
    /// the reverse of the client's pop order so `status` is the tail.
    pub fn to_result(&self) -> Vec<u8> {
        ResultBuilder::new()
            .push_u64(self.session_value)
            .push_u32(self.flags)
            .push_string(&self.file_text)
            .push_u8(self.marker)
            .push_u16(self.file_aa4)
            .push_u32(self.file_id)
            .push_bool(self.flag)
            .push_u32(self.file_nonce)
            .push_string(&self.server_message)
            .push_u16(self.status)
            .finish()
    }

    /// The Open FileMaker Server refuses with every field of the reply, as
    /// under Block New Users (`status` 1638, spec 5.6.4): the server's name
    /// in `serverMsg`, the bool at `0x17` set, `file[0xaa4]` 9 and the
    /// rest zero.
    pub fn refused(status: u16, server_name: &str) -> Self {
        Self {
            status,
            server_message: server_name.to_owned(),
            file_nonce: 0,
            flag: true,
            file_id: 0,
            file_aa4: 9,
            marker: 0,
            file_text: String::new(),
            flags: 0,
            session_value: 0,
        }
    }

    /// The result stack for a failed Open: an optional message under the
    /// nonzero `status`. A captured 0x507c reply had no message.
    pub fn error_result(status: u16, message: Option<&str>) -> Vec<u8> {
        let stack = ResultBuilder::new();
        let stack = match message {
            Some(text) => stack.push_string(text),
            None => stack,
        };
        stack.push_u16(status).finish()
    }
}

/// Parse an Open (op78) result stack, popping every field of spec 5.6.4.
/// A refused Open is an error: [`PerformError::Server`] with its status
/// when the reply holds every field (1638 under Block New Users, whose
/// string is the server's name), [`PerformError::OpenError`] when only a
/// message follows the status (the status code does not imply a message).
pub fn parse_open_reply(result: &[u8]) -> Result<OpenReply, PerformError> {
    let mut s = ResultReader::new(result);
    let status = s.pop_u16()?;
    if status != 0 {
        let mut whole = ResultReader::new(s.rest());
        if open_fields(&mut whole, status).is_ok() && whole.remaining() == 0 {
            return Err(PerformError::Server(status));
        }
        // Some refusals carry only a message string beneath the status.
        if let Ok(msg) = s.pop_string() {
            return Err(PerformError::OpenError(status, msg));
        }
        return Err(PerformError::Server(status));
    }
    open_fields(&mut s, status)
}

/// The fields of an Open reply under its `status`, in pop order.
fn open_fields(s: &mut ResultReader<'_>, status: u16) -> Result<OpenReply, PerformError> {
    Ok(OpenReply {
        status,
        server_message: s.pop_string()?,
        file_nonce: s.pop_u32()?,
        flag: s.pop_bool()?,
        file_id: s.pop_u32()?,
        file_aa4: s.pop_u16()?,
        marker: s.pop_u8()?,
        file_text: s.pop_string()?,
        flags: s.pop_u32()?,
        session_value: s.pop_u64()?,
    })
}

/// The outcome of a Login (op80) reply (spec 5.6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginReply {
    /// 0 = success; 0xd0 = password change required; 0x20d0 = external-auth continuation.
    pub error_code: u16,
    /// The short opaque result key (`01 03` in observed traffic), when present.
    pub result_key: Vec<u8>,
    /// The reply version (9 in observed traffic), when present.
    pub version: Option<u16>,
}

impl LoginReply {
    /// Whether the server accepted the credentials.
    pub fn is_success(&self) -> bool {
        self.error_code == 0
    }

    /// The result stack a server sends: `version` and `result_key` (when
    /// `version` is set) under the `error_code` tail (spec 5.6.5).
    pub fn to_result(&self) -> Vec<u8> {
        let stack = match self.version {
            Some(version) => ResultBuilder::new()
                .push_u16(version)
                .push_short_bytes(&self.result_key),
            None => ResultBuilder::new(),
        };
        stack.push_u16(self.error_code).finish()
    }
}

/// Parse a Login (op80) result stack: pops `errorCode` (the tail), then, when
/// the stack holds them, `resultKey` (bytes + u8 length) and `u16 version`.
///
/// Only the error code is guaranteed: non-zero codes may carry a different
/// tail (e.g. a minimum password length for 0xd0), so the remaining fields are
/// read only when they form a well-shaped key and version.
pub fn parse_login_reply(result: &[u8]) -> Result<LoginReply, PerformError> {
    let mut s = ResultReader::new(result);
    let error_code = s.pop_u16()?;
    let mut rest = ResultReader::new(s.rest());
    let (result_key, version) = match (rest.pop_short_bytes(), rest.pop_u16()) {
        (Ok(key), Ok(version)) => (key.to_vec(), Some(version)),
        _ => (Vec::new(), None),
    };
    Ok(LoginReply {
        error_code,
        result_key,
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_reply_pops_status_first() {
        // Synthetic result stack: push status(0) then serverMsg then nonce;
        // the server pushes status first so it is at the HEAD, but the status is
        // popped FIRST from the tail, so build tail-first here: nonce, msg, status.
        use om_rctransaction::Builder;
        // Builder pushes forward; to get [.. serverMsg .. status] with status at
        // the tail, push nonce, then string, then status last.
        let body = Builder::new(78)
            .push_u64(0x0102_0304_0506_0708) // session value (popped last)
            .push_u32(0x00a1_0000) // flags
            .push_string("") // file text
            .push_u8(0) // marker
            .push_u16(9) // file[0xaa4]
            .push_u32(132) // file id
            .push_bool(false) // flag
            .push_u32(0xA1B2_C3D4) // file nonce (popped third)
            .push_string("server") // serverMsg (popped second)
            .push_u16(0) // status (tail, popped first)
            .finish();
        // Strip the 20-byte header to get a bare result stack.
        let result = &body[om_rctransaction::HEADER_LEN..];
        assert_eq!(result.len(), 40, "matches the captured Open reply size");
        let open = parse_open_reply(result).unwrap();
        assert_eq!(open.status, 0);
        assert_eq!(open.server_message, "server");
        assert_eq!(open.file_nonce, 0xA1B2_C3D4);
        assert_eq!(open.file_id, 132);
        assert_eq!(open.file_aa4, 9);
        assert_eq!(open.flags, 0x00a1_0000);
        assert_eq!(open.session_value, 0x0102_0304_0506_0708);
    }

    #[test]
    fn open_reply_nonzero_status_is_error() {
        use om_rctransaction::Builder;
        let body = Builder::new(78).push_u16(0x4fb3).finish();
        let result = &body[om_rctransaction::HEADER_LEN..];
        assert_eq!(parse_open_reply(result), Err(PerformError::Server(0x4fb3)));
    }

    #[test]
    fn server_parses_client_open_request() {
        let msg = build_open(6, 0x463b_59ad, "fms.example.com", "test project.fmp12");
        let req = om_giop::Request::parse(&msg).unwrap();
        let perform = crate::parse_perform_request(req.body).unwrap();
        let open = parse_open_request(perform.transaction).unwrap();
        assert_eq!(open.host, "fms.example.com");
        assert_eq!(open.filename, "test project.fmp12");
        assert_eq!(open.file_path, "");
        assert_eq!(open.version, 8);
    }

    #[test]
    fn open_reply_to_result_round_trips() {
        let reply = OpenReply {
            status: 0,
            server_message: "server".into(),
            file_nonce: 0xA1B2_C3D4,
            flag: false,
            file_id: 132,
            file_aa4: 9,
            marker: 0,
            file_text: String::new(),
            flags: 0x00a1_0000,
            session_value: 0x0102_0304_0506_0708,
        };
        let result = reply.to_result();
        assert_eq!(result.len(), 40, "matches the captured Open reply size");
        assert_eq!(parse_open_reply(&result), Ok(reply));
        assert_eq!(
            parse_open_reply(&OpenReply::error_result(0x507c, Some("server"))),
            Err(PerformError::OpenError(0x507c, "server".into()))
        );
    }

    #[test]
    fn open_refused_under_block_new_users_matches_capture() {
        // s07 conn-001 request 6 (spec 5.6.4), the server's name as
        // `server` (SCSU, XOR 0x5A).
        let captured = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x29, 0x3f, 0x28, 0x2c, 0x3f, 0x28, 0x00, 0x00, 0x00, 0x06, 0x06, 0x66,
        ];
        assert_eq!(OpenReply::refused(1638, "server").to_result(), captured);
        // Its string is the server's name, not a message.
        assert_eq!(parse_open_reply(&captured), Err(PerformError::Server(1638)));
    }

    #[test]
    fn login_reply_to_result_matches_capture() {
        let reply = LoginReply {
            error_code: 0,
            result_key: vec![1, 3],
            version: Some(9),
        };
        // The captured 7-byte Login result stack (spec 5.6.5).
        assert_eq!(
            reply.to_result(),
            [0x00, 0x09, 0x01, 0x03, 0x02, 0x00, 0x00]
        );
        let rejected = LoginReply {
            error_code: 0x0d4,
            result_key: Vec::new(),
            version: None,
        };
        assert_eq!(parse_login_reply(&rejected.to_result()), Ok(rejected));
    }
}
