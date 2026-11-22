//! The RCFMP12 session protocol: object key, operations, opcode catalog, and
//! building the GIOP requests that carry FileMaker transactions.
//!
//! Both ends of the conversation live here: the client builds requests and
//! parses replies; a server (`om-server`) parses requests and builds replies.
//! The calls a server makes on its client's callback object (`Notify`,
//! [`notify`]) are built and read here the same way. Each pair is tested
//! against the other and against captured traffic.
//!
//! Spec: `docs/re/network-protocol.md` sections 2-6.

use om_giop::{
    build_reply, build_request, Reader as CdrReader, ReplyStatus, ServiceContext,
    Writer as CdrWriter, HEADER_LEN,
};

mod connect;
pub use connect::{
    build_connect, build_connect_reply, parse_connect_reply, parse_connect_request,
    validate_client_license, ConnectError, ConnectReply, ConnectRequest, MachineInfo,
};

mod download;
pub use download::{
    build_download, build_download_from_host, build_download_list, encode_key_list, key_list_keys,
    list_complete, page_cut, parse_download_from_host_reply, parse_download_list_reply,
    parse_download_list_request, parse_download_reply, parse_download_request, upload_position,
    DownloadListReply, DownloadListRequest, DownloadReply, DownloadRequest, KeyList, NodeState,
    ReceivedDownload, ReceivedDownloadList, Resume, UploadScan, DOWNLOAD_LIST_MAX_RETURN,
    DOWNLOAD_MAX_RETURN, PAGE_LIMIT,
};

mod file_list;
pub use file_list::{
    build_file_list, parse_file_list_reply, parse_file_list_request, FileListReply,
    FileListRequest, FILE_LIST_VERSION, SHARE_TYPE_FMAPP,
};

pub mod hosted_files;

mod keepalive;
pub use keepalive::{
    build_tickle, build_tickle_reply, parse_tickle_parms, parse_tickle_reply, parse_tickle_request,
    tickle_parms, TickleRequest, TickleSession, KEEPALIVE_INTERVAL,
};

mod login;
pub mod notify;
pub use login::{
    build_login, parse_login_request, LoginEnvironment, LoginRequest, ReceivedLogin,
    FILE_SESSION_ID, LOGIN_VERSION,
};

mod perform;
mod query;
mod security_catalog;
pub use perform::{
    build_open, build_request_notification, parse_login_reply, parse_open_reply,
    parse_open_request, parse_perform_reply, parse_request_notification, LoginReply, OpenReply,
    OpenRequest, PerformError, PerformReply, RequestNotification,
};
pub use query::{
    build_query, parse_query_reply, parse_query_request, QueryReply, QueryRequest, ReceivedQuery,
    QUERY_MAX_RETURN,
};

pub mod authoring;
pub mod container;
pub mod schema;
pub mod script;
pub mod write;

/// The server object key present in every RCFMP12 request.
pub const OBJECT_KEY: &[u8] = b"\xffomniINSPOA\xffFMPOA\x00FMRPO";

/// The RCFMP12 repository id, the argument to `_is_a`.
pub const REPO_ID: &[u8] = b"IDL:RCFMP12:1.0";

/// RCFMP12 operation names (CORBA operation strings).
pub mod op {
    pub const IS_A: &[u8] = b"_is_a";
    pub const CONNECT: &[u8] = b"Connect";
    pub const RECONNECT: &[u8] = b"Reconnect";
    pub const DISCONNECT: &[u8] = b"Disconnect";
    pub const PERFORM: &[u8] = b"Perform";
    pub const TICKLE: &[u8] = b"Tickle";
    pub const NOTIFY: &[u8] = b"Notify";
}

/// RCTransaction opcodes (byte 0 of a transaction), from the catalog in
/// `docs/re/network-protocol.md` section 5.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    UploadToHost = 13,
    UploadListToHostTempLocation = 14,
    DownloadFromHost = 15,
    PerformScriptOnServer = 30,
    Lock = 60,
    Upgrade = 61,
    Unlock = 62,
    RequestNotification = 63,
    KillAllLocks = 65,
    LockFinished = 66,
    HostCommit = 70,
    HostCreateRecord = 71,
    HostGetSerial = 72,
    HostUpdateTable = 73,
    ConnectToServer = 75,
    GetFileList = 77,
    OpenFile = 78,
    Login = 80,
    Logout = 81,
    HostTimeStamp = 82,
    GetFileSize = 83,
    GetGuestCount = 84,
    BuildIndex = 85,
    QueryProgram = 86,
    AdjustReferenceCount = 87,
    UploadBinaryData = 88,
    CompareModCounts = 89,
    DownloadWithLock = 90,
    UploadWithLock = 91,
    DownloadList = 92,
    DownloadFromTemp = 93,
    DownloadFile = 95,
    DownloadThumbnail = 96,
    ChangeSessionName = 102,
    TransactInfo = 103,
    SendPsosResultToClient = 105,
    /// OpenMaker's authoring extension ([`authoring`]); not a FileMaker
    /// opcode, sent only to OpenMaker Server.
    OpenMakerAuthoring = 240,
}

impl Opcode {
    /// Human name for an opcode byte, or `None` if unknown.
    pub fn name(code: u8) -> Option<&'static str> {
        Some(match code {
            13 => "UploadToHost",
            14 => "UploadListToHostTempLocation",
            15 => "DownloadFromHost",
            20 => "ODBCGetDSNList",
            21 => "ODBCConnect",
            22 => "ODBCCommand",
            23 => "ODBCQuery",
            30 => "PerformScriptOnServer",
            60 => "Lock",
            61 => "Upgrade",
            62 => "Unlock",
            63 => "RequestNotification",
            65 => "KillAllLocks",
            66 => "LockFinished",
            70 => "HostCommit",
            71 => "HostCreateRecord",
            72 => "HostGetSerial",
            73 => "HostUpdateTable",
            75 => "ConnectToServer",
            77 => "GetFileList",
            78 => "OpenFile",
            80 => "Login",
            81 => "Logout",
            82 => "HostTimeStamp",
            83 => "GetFileSize",
            84 => "GetGuestCount",
            85 => "BuildIndex",
            86 => "QueryProgram",
            87 => "AdjustReferenceCount",
            88 => "UploadBinaryData",
            89 => "CompareModCounts",
            90 => "DownloadWithLock",
            91 => "UploadWithLock",
            92 => "DownloadList",
            93 => "DownloadFromTemp",
            95 => "DownloadFile",
            96 => "DownloadThumbnail",
            97 => "VerifyContainer",
            98 => "GetContainerURL",
            99 => "TransferContainer",
            102 => "ChangeSessionName",
            103 => "TransactInfo",
            104 => "SendAppleIdMailOnServer",
            105 => "SendPsosResultToClient",
            240 => "OpenMakerAuthoring",
            _ => return None,
        })
    }
}

/// Build the `_is_a` request GIOP message.
///
/// `service_contexts` should carry the BiDirIIOP context (id 5) advertising the
/// client callback endpoints; see `docs/re/network-protocol.md` section 3.
pub fn build_is_a(request_id: u32, service_contexts: &[ServiceContext]) -> Vec<u8> {
    build_is_a_of(request_id, REPO_ID, service_contexts)
}

/// Build an `_is_a` request asking whether the server object is also of
/// type `repo_id`: how a client asks a host for an interface before it
/// uses it ([`hosted_files::REPO_ID`]).
pub fn build_is_a_of(
    request_id: u32,
    repo_id: &[u8],
    service_contexts: &[ServiceContext],
) -> Vec<u8> {
    // The _is_a body is a single CDR string (the repository id), written at the
    // 8-aligned body offset. The header-body length is not known until framing,
    // so compute the body offset the same way the framer does: header + the
    // request header fields padded to 8. Simplest correct approach: build with a
    // provisional origin, then let build_request reframe. build_request aligns
    // the body to 8 itself, so author the body with origin 0 and accept that CDR
    // in the body only uses 4-byte alignment here (the string length), which is
    // identical at any 8k offset.
    let mut body = CdrWriter::with_origin(0);
    body.string(repo_id);
    build_request(
        request_id,
        true,
        OBJECT_KEY,
        op::IS_A,
        service_contexts,
        &body.into_bytes(),
    )
}

/// The repository id argument of an `_is_a` request body.
pub fn parse_is_a_request(body: &[u8]) -> Option<&[u8]> {
    // The body is 8-aligned in the message, so reading at origin 0 aligns the
    // same way (see `build_is_a`).
    CdrReader::new(body, 0).string().ok()
}

/// Build the `_is_a` reply: one CDR boolean.
pub fn build_is_a_reply(request_id: u32, is_a: bool) -> Vec<u8> {
    build_reply(request_id, ReplyStatus::NoException, &[], &[u8::from(is_a)])
}

/// The connection id a `Disconnect(connectionid, string)` request names.
pub fn parse_disconnect_request(body: &[u8]) -> Option<u32> {
    CdrReader::new(body, 0).u32().ok()
}

/// Build a `Disconnect(connectionid, "")` request ending connection
/// `connection_id`'s session. A client sends it to the host; FileMaker
/// Server also calls it on a client's callback object, response expected,
/// to end the session of a client it asked to disconnect
/// (network-protocol.md 6.4, 6.5). Both objects have the key
/// [`OBJECT_KEY`].
pub fn build_disconnect(request_id: u32, connection_id: u32, response_expected: bool) -> Vec<u8> {
    let mut body = CdrWriter::with_origin(0);
    body.u32(connection_id);
    body.string(b"");
    build_request(
        request_id,
        response_expected,
        OBJECT_KEY,
        op::DISCONNECT,
        &[],
        &body.into_bytes(),
    )
}

/// Build the BiDirIIOP service context (id 5) that negotiates bidirectional
/// GIOP, so the server delivers its back-channel (`Tickle`/`Notify`) over the
/// existing connection instead of dialing the client's listen points.
///
/// `endpoints` are advertised `(host, port)` listen points. The context data is
/// a little-endian CDR encapsulation of `sequence<ListenPoint{string host,
/// u16 port}>`. Spec: `docs/re/network-protocol.md` section 3; the format is the
/// OMG GIOP 1.2 BI_DIR_IIOP context.
pub fn bidir_service_context(endpoints: &[(&str, u16)]) -> ServiceContext {
    let mut enc = CdrWriter::with_origin(0);
    enc.u8(1); // byte order: little-endian
    enc.u32(endpoints.len() as u32);
    for (host, port) in endpoints {
        enc.string(host.as_bytes());
        enc.u16(*port);
    }
    ServiceContext {
        id: 5,
        data: enc.into_bytes(),
    }
}

/// The default `maxreturn` for a `Perform` call: the maximum reply size the
/// client will accept. The server rejects values outside `0x14..=0x200000`
/// (spec 5.6); large replies arrive as GIOP fragments regardless.
pub const DEFAULT_MAX_RETURN: u32 = 0x0020_0000;

/// Build a `Perform` request GIOP message carrying `transaction` bytes, using
/// [`DEFAULT_MAX_RETURN`] as the reply-size limit.
///
/// `connection_id` is the value returned by `Connect`; `session` is the CORBA
/// string argument (empty in observed traffic).
pub fn build_perform(
    request_id: u32,
    connection_id: u32,
    session: &[u8],
    transaction: &[u8],
) -> Vec<u8> {
    build_perform_with_max(
        request_id,
        connection_id,
        session,
        transaction,
        DEFAULT_MAX_RETURN,
    )
}

/// Build a `Perform` request with an explicit `maxreturn` (the max reply size).
///
/// The third CORBA argument is `maxreturn`, NOT the transaction length (the
/// transaction's own length is the inout sequence's CDR prefix). Confirmed from
/// the capture: an 87-byte Open transaction was sent with `maxreturn` 2334.
pub fn build_perform_with_max(
    request_id: u32,
    connection_id: u32,
    session: &[u8],
    transaction: &[u8],
    max_return: u32,
) -> Vec<u8> {
    // Perform(in ulong conn, in string, in ulong maxreturn, inout sequence<octet>).
    // `build_request` places the body at an 8-aligned (hence 4-aligned) offset,
    // and every field here needs at most 4-byte alignment, so authoring the body
    // at origin 0 yields the same padding as at the real offset.
    let mut body = CdrWriter::with_origin(0);
    body.u32(connection_id);
    body.string(session);
    body.u32(max_return);
    body.octet_sequence(transaction);
    build_request(
        request_id,
        true,
        OBJECT_KEY,
        op::PERFORM,
        &[],
        &body.into_bytes(),
    )
}

/// The arguments of a `Perform(connectionid, sessKey, maxreturn, inout parms)`
/// request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerformRequest<'a> {
    pub connection_id: u32,
    pub session: &'a [u8],
    /// The largest reply the client accepts (spec 5.6.2).
    pub max_return: u32,
    /// The RCTransaction: 20-byte header + LIFO body.
    pub transaction: &'a [u8],
}

/// Parse a `Perform` request body ([`om_giop::Request::body`]).
pub fn parse_perform_request(body: &[u8]) -> Option<PerformRequest<'_>> {
    // The body starts 8-aligned in the message, so a reader at relative 0 is
    // aligned the same way for these 4-byte fields.
    let mut r = CdrReader::new(body, 0);
    Some(PerformRequest {
        connection_id: r.u32().ok()?,
        session: r.string().ok()?,
        max_return: r.u32().ok()?,
        transaction: r.octet_sequence().ok()?,
    })
}

/// The RCTransaction octet sequence inside a parsed `Perform` request body.
///
/// `body` is [`om_giop::Request::body`]; `body_offset` is its offset within the
/// whole message (needed because CDR alignment is message-relative).
pub fn perform_transaction(body: &[u8], body_offset: usize) -> Option<&[u8]> {
    debug_assert_eq!(body_offset % 8, 0);
    parse_perform_request(body).map(|p| p.transaction)
}

/// Build a `Perform` reply: the void return marshals nothing, then the inout
/// `parms` (the result stack) and the out `backchannel` (spec 5.6.1).
pub fn build_perform_reply(request_id: u32, result: &[u8], backchannel: &[u8]) -> Vec<u8> {
    let mut body = CdrWriter::with_origin(0);
    body.octet_sequence(result);
    // CDR aligns the second sequence's u32 length to 4 bytes.
    body.octet_sequence(backchannel);
    build_reply(
        request_id,
        ReplyStatus::NoException,
        &[],
        &body.into_bytes(),
    )
}

/// Offset of a GIOP request's body within its message, for [`perform_transaction`].
pub fn request_body_offset(message_len: usize, body_len: usize) -> usize {
    let _ = HEADER_LEN;
    message_len - body_len
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_giop::Request;

    #[test]
    fn is_a_round_trips() {
        let ctx = ServiceContext {
            id: 5,
            data: vec![1, 2, 3, 4],
        };
        let msg = build_is_a(2, std::slice::from_ref(&ctx));
        let req = Request::parse(&msg).unwrap();
        assert_eq!(req.request_id, 2);
        assert_eq!(req.object_key, OBJECT_KEY);
        assert_eq!(req.operation, op::IS_A);
        assert_eq!(req.service_contexts, vec![ctx]);
        assert!(req.body.ends_with(b"IDL:RCFMP12:1.0\0"));
    }

    #[test]
    fn perform_round_trips() {
        let txn = om_rctransaction::Builder::new(90).push_u32(1).finish();
        let msg = build_perform(8, 0x463b_59ad, b"", &txn);
        let req = Request::parse(&msg).unwrap();
        assert_eq!(req.operation, op::PERFORM);
        let offset = request_body_offset(msg.len(), req.body.len());
        let got = perform_transaction(req.body, offset).unwrap();
        assert_eq!(got, txn);
    }

    #[test]
    fn opcode_names() {
        assert_eq!(Opcode::name(90), Some("DownloadWithLock"));
        assert_eq!(Opcode::name(80), Some("Login"));
        assert_eq!(Opcode::name(200), None);
    }

    #[test]
    fn server_reads_perform_request() {
        let txn = om_rctransaction::Builder::new(90).push_u32(1).finish();
        let msg = build_perform_with_max(8, 0x463b_59ad, b"", &txn, 0x10_012c);
        let req = Request::parse(&msg).unwrap();
        let perform = parse_perform_request(req.body).unwrap();
        assert_eq!(perform.connection_id, 0x463b_59ad);
        assert_eq!(perform.session, b"");
        assert_eq!(perform.max_return, 0x10_012c);
        assert_eq!(perform.transaction, txn);
    }

    #[test]
    fn perform_reply_round_trips() {
        // The captured Login reply: 7-byte result stack, then 3 backchannel
        // bytes after a pad byte that aligns the second length.
        let result = [0x00, 0x09, 0x01, 0x03, 0x02, 0x00, 0x00];
        let msg = build_perform_reply(14, &result, &[0xfe, 0x09, 0x9e]);
        let reply = parse_perform_reply(&msg).unwrap();
        assert_eq!(reply.result, result);
        assert_eq!(reply.backchannel, [0xfe, 0x09, 0x9e]);
    }

    #[test]
    fn is_a_and_disconnect_round_trip() {
        let req_msg = build_is_a(2, &[]);
        let req = Request::parse(&req_msg).unwrap();
        assert_eq!(parse_is_a_request(req.body), Some(REPO_ID));
        let reply = om_giop::Reply::parse(&build_is_a_reply(2, true))
            .unwrap()
            .body
            .to_vec();
        assert_eq!(reply, [1]);

        let msg = build_disconnect(6, 0x463b_59ad, false);
        let req = Request::parse(&msg).unwrap();
        assert!(!req.response_expected);
        assert_eq!(parse_disconnect_request(req.body), Some(0x463b_59ad));
    }

    #[test]
    fn builds_the_servers_disconnect_call() {
        // FileMaker Server's call on FileMaker Pro's callback object, s07
        // conn-000 request 9 (network-protocol.md 6.5), and the client's
        // empty NO_EXCEPTION answer.
        let captured = [
            &b"GIOP\x01\x02\x01\x00\x45\x00\x00\x00\x09\x00\x00\x00\x03\x00\x00\x00\x00\x00\x00\x00\x17\x00\x00\x00"[..],
            OBJECT_KEY,
            b"\x00\x0b\x00\x00\x00Disconnect\x00\x00\x00\x00\x00\x00\x7e\x20\xad\xb6\x01\x00\x00\x00\x00",
        ]
        .concat();
        assert_eq!(build_disconnect(9, 0xb6ad_207e, true), captured);
        let req = Request::parse(&captured).unwrap();
        assert!(req.response_expected);
        assert_eq!(parse_disconnect_request(req.body), Some(0xb6ad_207e));
        assert_eq!(
            build_reply(9, ReplyStatus::NoException, &[], &[]),
            b"GIOP\x01\x02\x01\x01\x0c\x00\x00\x00\x09\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"
        );
    }
}
