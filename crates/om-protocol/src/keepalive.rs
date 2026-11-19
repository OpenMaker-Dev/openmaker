//! The RCFMP12 `Tickle`: the keepalive a client sends on a quiet connection.
//!
//! `Tickle(in ulong connectionid, in string sessKey, inout sequence<octet>
//! parms)`. `parms` holds one 10-byte record per session open on the
//! connection: the session id, the user's idle seconds and a `u16` 0, all
//! big-endian. FileMaker Pro sends one 60 s after its last request and every
//! 60 s after that; the server answers with the sequence back (empty from
//! OpenMaker Server, which FileMaker Pro accepts).
//!
//! Spec: `docs/re/network-protocol.md` section 6.1.

use std::time::Duration;

use om_giop::{build_reply, build_request, Reader as CdrReader, Reply, ReplyStatus};

use crate::{op, PerformError, OBJECT_KEY};

/// How long FileMaker Pro lets a connection with an open file stay silent
/// before it tickles, and then the time between Tickles (spec 6.1).
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// Bytes of one record of a Tickle's `parms`.
const RECORD_LEN: usize = 10;

/// One record of a Tickle's `parms`: a session on the connection and how
/// long its user has been idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickleSession {
    /// The file session id (1 after Login, `crate::FILE_SESSION_ID`).
    pub session_id: u32,
    /// Seconds since the user last did something.
    pub idle_seconds: u32,
}

/// Marshal `parms`: each session as `{u32 BE id, u32 BE idle, u16 BE 0}`.
pub fn tickle_parms(sessions: &[TickleSession]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sessions.len() * RECORD_LEN);
    for session in sessions {
        out.extend_from_slice(&session.session_id.to_be_bytes());
        out.extend_from_slice(&session.idle_seconds.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    out
}

/// Read `parms` back into its records, or `None` if it is not a whole
/// number of records. The trailing `u16` (0 in every recording) is not kept.
pub fn parse_tickle_parms(parms: &[u8]) -> Option<Vec<TickleSession>> {
    if parms.len() % RECORD_LEN != 0 {
        return None;
    }
    Some(
        parms
            .chunks_exact(RECORD_LEN)
            .map(|record| TickleSession {
                session_id: u32::from_be_bytes(record[0..4].try_into().unwrap()),
                idle_seconds: u32::from_be_bytes(record[4..8].try_into().unwrap()),
            })
            .collect(),
    )
}

/// Build a `Tickle` request reporting `sessions`. `connection_id` is the
/// value `Connect` returned; `session` is the CORBA string (empty in every
/// recording).
pub fn build_tickle(
    request_id: u32,
    connection_id: u32,
    session: &[u8],
    sessions: &[TickleSession],
) -> Vec<u8> {
    // The body starts 8-aligned, and every field needs at most 4-byte
    // alignment, so authoring it at origin 0 pads as at the real offset.
    let mut body = om_giop::Writer::with_origin(0);
    body.u32(connection_id);
    body.string(session);
    body.octet_sequence(&tickle_parms(sessions));
    build_request(
        request_id,
        true,
        OBJECT_KEY,
        op::TICKLE,
        &[],
        &body.into_bytes(),
    )
}

/// The arguments of a `Tickle` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickleRequest<'a> {
    pub connection_id: u32,
    pub session: &'a [u8],
    /// The raw `parms` (see [`parse_tickle_parms`]).
    pub parms: &'a [u8],
}

/// Parse a `Tickle` request body ([`om_giop::Request::body`]).
pub fn parse_tickle_request(body: &[u8]) -> Option<TickleRequest<'_>> {
    let mut r = CdrReader::new(body, 0);
    Some(TickleRequest {
        connection_id: r.u32().ok()?,
        session: r.string().ok()?,
        parms: r.octet_sequence().ok()?,
    })
}

/// Build a `Tickle` reply: the void return marshals nothing, then the inout
/// `parms`.
pub fn build_tickle_reply(request_id: u32, parms: &[u8]) -> Vec<u8> {
    let mut body = om_giop::Writer::with_origin(0);
    body.octet_sequence(parms);
    build_reply(
        request_id,
        ReplyStatus::NoException,
        &[],
        &body.into_bytes(),
    )
}

/// The inout `parms` of a `Tickle` reply message.
pub fn parse_tickle_reply(message: &[u8]) -> Result<Vec<u8>, PerformError> {
    let reply = Reply::parse(message).map_err(|e| PerformError::Giop(format!("{e}")))?;
    if reply.status != ReplyStatus::NoException {
        return Err(PerformError::Status(format!("{:?}", reply.status)));
    }
    let body_offset = message.len() - reply.body.len();
    CdrReader::new(message, body_offset)
        .octet_sequence()
        .map(<[u8]>::to_vec)
        .map_err(|_| PerformError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_giop::Request;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// FileMaker Pro 26.0.3's Tickle, request id 252 of recording
    /// s06-omserver-o conn-001 (spec 6.1): connection 0x59689e53, one
    /// record for session 1 idle 153 s. Its padding happens to be zero, so
    /// it is the exact message OpenMaker builds.
    const CAPTURED: &str = "
        47494f50 01020100 56000000
        fc000000 03000000 00000000
        17000000 ff6f6d6e 69494e53 504f41ff 464d504f 4100464d 52504f00
        07000000 5469636b 6c650000
        00000000 00000000
        539e6859 01000000 00000000 0a000000
        00000001 00000099 0000";

    /// Request id 46 of s06-omserver-d conn-001: connection 0x4c19256c,
    /// idle 65 s, with FileMaker Pro's uninitialized padding (`3cf3` after
    /// the target address, `05000000` before the body, `43464d` after the
    /// string).
    const CAPTURED_PADDED: &str = "
        47494f50 01020100 56000000
        2e000000 03000000 00003cf3
        17000000 ff6f6d6e 69494e53 504f41ff 464d504f 4100464d 52504f00
        07000000 5469636b 6c650000
        00000000 05000000
        6c25194c 01000000 0043464d 0a000000
        00000001 00000041 0000";

    /// OpenMaker Server's reply to it: NO_EXCEPTION, `parms` empty.
    const REPLY: &str = "47494f50 01020101 10000000 2e000000 00000000 00000000 00000000";

    #[test]
    fn builds_the_captured_tickle() {
        let built = build_tickle(
            252,
            0x5968_9e53,
            b"",
            &[TickleSession {
                session_id: 1,
                idle_seconds: 153,
            }],
        );
        assert_eq!(built, hex(CAPTURED));
    }

    #[test]
    fn reads_tickles_whatever_their_padding() {
        for (captured, connection, idle) in [
            (CAPTURED, 0x5968_9e53, 153),
            (CAPTURED_PADDED, 0x4c19_256c, 65),
        ] {
            let msg = hex(captured);
            let request = Request::parse(&msg).unwrap();
            assert_eq!(request.operation, op::TICKLE);
            assert!(request.response_expected);
            assert_eq!(request.object_key, OBJECT_KEY);
            let tickle = parse_tickle_request(request.body).unwrap();
            assert_eq!(tickle.connection_id, connection);
            assert_eq!(tickle.session, b"");
            assert_eq!(
                parse_tickle_parms(tickle.parms),
                Some(vec![TickleSession {
                    session_id: 1,
                    idle_seconds: idle,
                }])
            );
        }
    }

    #[test]
    fn padding_is_the_only_difference_from_the_padded_capture() {
        let built = build_tickle(
            46,
            0x4c19_256c,
            b"",
            &[TickleSession {
                session_id: 1,
                idle_seconds: 65,
            }],
        );
        let mut captured = hex(CAPTURED_PADDED);
        // The three padding runs FileMaker Pro leaves uninitialized.
        for range in [22..24, 68..72, 81..84] {
            captured[range].fill(0);
        }
        assert_eq!(built, captured);
    }

    #[test]
    fn replies_round_trip() {
        let reply = build_tickle_reply(46, &[]);
        assert_eq!(reply, hex(REPLY));
        assert_eq!(parse_tickle_reply(&reply).unwrap(), Vec::<u8>::new());
        let notification = [0xff, 0x00, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00];
        let reply = build_tickle_reply(48, &notification);
        assert_eq!(parse_tickle_reply(&reply).unwrap(), notification);
    }

    #[test]
    fn parms_hold_whole_records() {
        let sessions = [
            TickleSession {
                session_id: 1,
                idle_seconds: 0x130,
            },
            TickleSession {
                session_id: 7,
                idle_seconds: 0,
            },
        ];
        let parms = tickle_parms(&sessions);
        assert_eq!(parms.len(), 20);
        assert_eq!(&parms[..10], hex("00000001 00000130 0000"));
        assert_eq!(parse_tickle_parms(&parms).unwrap(), sessions);
        assert_eq!(parse_tickle_parms(&[]), Some(Vec::new()));
        assert_eq!(parse_tickle_parms(&parms[..9]), None);
    }
}
