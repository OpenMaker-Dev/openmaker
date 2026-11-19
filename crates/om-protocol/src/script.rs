//! Perform Script on Server (spec `docs/re/script-runtime.md` 11):
//! RCPerformScriptOnHost (op30), the client's request to run a script on
//! the host, and RCSendPSOSRetToClient (op105), the result of a Perform
//! Script on Server with Callback that the host sends back.
//!
//! No capture of either exchange exists. The fields and their push order
//! are the spec's tables (11.2, 11.4, read from FileMaker's `Call` and
//! `Perform` methods). Details the spec leaves open are marked *inferred*
//! here and listed in script-runtime.md "Implementation notes (PSOS)":
//! the parameter's Text push taken as a plain unistring, the script key as
//! the catalog key segment, the order and zero values of the callback ids,
//! op105's transaction header and the client's answer to it.

use om_rctransaction::{Builder, Reader, ResultBuilder, ResultReader};

use crate::download::file_header;
use crate::write::finished;
use crate::{Opcode, PerformError};

/// op30 flags bit 0, `kAsyncFlag`: reply at once, do not wait for the
/// script (Perform Script on Server with "Wait for completion" off).
pub const ASYNC: u16 = 1;

/// From this protocol version on the request carries the two callback ids
/// (`Call` arguments 7 and 8). The version is the one the Login reply
/// names (9 from FileMaker Server 26 and OpenMaker Server).
pub const CALLBACK_IDS_VERSION: u16 = 9;

/// The longest script parameter and result, in characters (11.2, 11.4).
pub const TEXT_LIMIT: usize = 1_000_000;

/// The largest op30 reply body a client accepts; a body of 0 or 1 bytes or
/// over this is error 3 (11.2).
pub const MAX_REPLY: usize = 1 << 20;

/// FileMaker's error for a parameter over [`TEXT_LIMIT`], raised before
/// sending (11.2).
pub const PARAMETER_TOO_LONG: u16 = 513;

/// FileMaker's error for a callback result over [`TEXT_LIMIT`] (11.4).
pub const RESULT_TOO_LONG: u16 = 736;

/// The longest script key: `HBSerialKey<7>`.
pub const SCRIPT_KEY_MAX: usize = 7;

/// The callback ids of a Perform Script on Server with Callback (11.4).
/// Plain Perform Script on Server sends both as 0 (inferred).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackIds {
    /// `Call` argument 7, pushed second; inferred to be the callback file
    /// user (the server's `rCallBackFileUserID`).
    pub file_user: u32,
    /// `Call` argument 8, pushed first: the client's callback id, the key
    /// of its callback map, which op105 names (`rClientCallBackID`).
    pub client: u32,
}

impl CallbackIds {
    /// Whether these ask for an op105 when the script finishes.
    pub fn wants_callback(&self) -> bool {
        self.client != 0
    }
}

/// An op30 request: run a script of the opened file on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerformScript {
    /// [`ASYNC`] or 0.
    pub flags: u16,
    /// The script's key in the script catalog `[17, 5, S]`, the content
    /// bytes of an `HBSerialKey<7>`: the stored key segment, whose serial
    /// decode is the script's id ([`script_key`], file-metadata.md 1).
    pub script: Vec<u8>,
    /// `Get ( ScriptParameter )` as text; empty when there is none (the
    /// client pushes an empty unistring then).
    pub parameter: String,
    /// Present when the connection's protocol version is at least
    /// [`CALLBACK_IDS_VERSION`].
    pub callback: Option<CallbackIds>,
}

impl PerformScript {
    /// Whether the client waits for the script's result.
    pub fn waits(&self) -> bool {
        self.flags & ASYNC == 0
    }
}

/// The op30 script key of script `id` (`Script::id`, the id FileMaker
/// shows): the key segment the script catalog stores it under, its serial
/// key (`80 0d` for script 141, file-metadata.md 1). `None` when it does
/// not fit an `HBSerialKey<7>`.
pub fn script_key(id: u64) -> Option<Vec<u8>> {
    let key = om_hbam::serial::encode(id);
    (key.len() <= SCRIPT_KEY_MAX && id != 0).then_some(key)
}

/// The script id an op30 script key names, the inverse of [`script_key`].
pub fn script_id(key: &[u8]) -> Option<u64> {
    om_hbam::serial::decode(key).filter(|id| *id != 0)
}

/// Build an op30 transaction (the client side). Push order (11.2): the
/// callback ids (argument 8, then 7), the parameter, the script key (bytes,
/// then a 1-byte length), the flags.
///
/// # Panics
/// If the script key is longer than [`SCRIPT_KEY_MAX`] bytes.
pub fn build_perform_script(transaction_id: u32, file_id: u32, req: &PerformScript) -> Vec<u8> {
    assert!(req.script.len() <= SCRIPT_KEY_MAX, "an HBSerialKey<7>");
    let header = file_header(Opcode::PerformScriptOnServer, transaction_id, file_id);
    let mut b = Builder::with_header(header);
    if let Some(ids) = req.callback {
        b = b.push_u32(ids.client).push_u32(ids.file_user);
    }
    // Inferred: `Push(Text, max)` of a plain parameter is the unistring
    // form, the same bytes the empty-parameter case pushes.
    b.push_string(&req.parameter)
        .push_raw(&req.script)
        .push_u8(req.script.len() as u8)
        .push_u16(req.flags)
        .finish()
}

/// Parse an op30 transaction (the server side). `protocol_version` is the
/// connection's (the Login reply's version): the callback ids are read
/// from [`CALLBACK_IDS_VERSION`] on.
pub fn parse_perform_script_request(
    transaction: &[u8],
    protocol_version: u16,
) -> Result<PerformScript, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let flags = r.pop_u16()?;
    let script = r.pop_short_bytes()?.to_vec();
    let parameter = r.pop_string()?;
    let callback = if protocol_version >= CALLBACK_IDS_VERSION {
        let file_user = r.pop_u32()?;
        let client = r.pop_u32()?;
        Some(CallbackIds { file_user, client })
    } else {
        None
    };
    finished(&r)?;
    Ok(PerformScript {
        flags,
        script,
        parameter,
        callback,
    })
}

/// The op30 reply: the script's final `Get ( LastError )`, its Exit Script
/// result and the extended error text (11.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerformScriptReply {
    pub error: u16,
    /// Empty when the client did not wait.
    pub result: String,
    /// Detail for an error that carries one; empty otherwise (inferred,
    /// script-runtime.md U8).
    pub extended_error: String,
}

impl PerformScriptReply {
    /// Push order: extended error text, result, error.
    pub fn to_result(&self) -> Vec<u8> {
        ResultBuilder::new()
            .push_string(&self.extended_error)
            .push_string(truncated(&self.result))
            .push_u16(self.error)
            .finish()
    }
}

/// `text` cut to [`TEXT_LIMIT`] characters, the result's maximum (inferred:
/// FileMaker's push stops at its maximum length).
fn truncated(text: &str) -> &str {
    match text.char_indices().nth(TEXT_LIMIT) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// Parse an op30 result stack (the client side). A body of 0 or 1 bytes or
/// over [`MAX_REPLY`] is FileMaker's error 3 (11.2).
pub fn parse_perform_script_reply(result: &[u8]) -> Result<PerformScriptReply, PerformError> {
    if result.len() <= 1 || result.len() > MAX_REPLY {
        return Ok(PerformScriptReply {
            error: 3,
            ..PerformScriptReply::default()
        });
    }
    let mut r = ResultReader::new(result);
    let error = r.pop_u16()?;
    let result = r.pop_string()?;
    let extended_error = if r.remaining() == 0 {
        String::new()
    } else {
        r.pop_string()?
    };
    Ok(PerformScriptReply {
        error,
        result,
        extended_error,
    })
}

/// An op105 RCSendPSOSRetToClient: the host tells the client that a script
/// started by Perform Script on Server with Callback finished (11.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PsosResult {
    /// The client's callback id from the op30 request.
    pub callback: u32,
    /// The server script's final `Get ( LastError )`; [`RESULT_TOO_LONG`]
    /// when its result did not fit.
    pub error: u16,
    pub result: String,
    pub extended_error: String,
}

/// Build an op105 transaction (the server side). Push order (11.4):
/// extended error text, result, error, client callback id.
///
/// The header is inferred: op105's own 20 bytes are not documented, so
/// it is the header of a file-scoped transaction on the file the script
/// ran in.
pub fn build_psos_result(transaction_id: u32, file_id: u32, req: &PsosResult) -> Vec<u8> {
    let header = file_header(Opcode::SendPsosResultToClient, transaction_id, file_id);
    Builder::with_header(header)
        .push_string(&req.extended_error)
        .push_string(&req.result)
        .push_u16(req.error)
        .push_u32(req.callback)
        .finish()
}

/// Parse an op105 transaction (the client side).
pub fn parse_psos_result(transaction: &[u8]) -> Result<PsosResult, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let callback = r.pop_u32()?;
    let error = r.pop_u16()?;
    let result = r.pop_string()?;
    let extended_error = r.pop_string()?;
    finished(&r)?;
    Ok(PsosResult {
        callback,
        error,
        result,
        extended_error,
    })
}

/// The client's answer to an op105, the result stack of its `Perform`
/// reply: status 0 (inferred; the spec does not give the client's reply).
pub fn psos_result_reply() -> Vec<u8> {
    ResultBuilder::new().push_u16(0).finish()
}

/// Parse a client's answer to an op105 (the server side): its status.
pub fn parse_psos_result_reply(result: &[u8]) -> Result<u16, PerformError> {
    Ok(ResultReader::new(result).pop_u16()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// An ASCII unistring: each byte XOR 0x5A, then the u32 BE byte count
    /// (network-protocol.md 4.2).
    fn unistring(text: &str) -> String {
        let bytes: String = text.bytes().map(|b| format!("{:02x}", b ^ 0x5a)).collect();
        format!("{bytes} {:08x}", text.len())
    }

    #[test]
    fn request_follows_the_field_table() {
        // 11.2: callback ids (argument 8 first), parameter, key bytes then
        // their length, flags last (the server pops it first).
        let req = PerformScript {
            flags: ASYNC,
            script: vec![0x05],
            parameter: "Go".into(),
            callback: Some(CallbackIds {
                file_user: 0x1234,
                client: 7,
            }),
        };
        let txn = build_perform_script(0x21, 0x165, &req);
        let expected = hex(&format!(
            "1e 00 0000 00000021 00000001 00000165 00000000 \
             00000007 00001234 {} 05 01 0001",
            unistring("Go")
        ));
        assert_eq!(txn, expected);
        assert_eq!(parse_perform_script_request(&txn, 9).unwrap(), req);
        assert!(!req.waits());

        // Before protocol version 9 there are no ids; plain PSOS waits.
        let old = PerformScript {
            flags: 0,
            script: vec![0x81, 0x9a],
            parameter: String::new(),
            callback: None,
        };
        let txn = build_perform_script(1, 0x165, &old);
        assert!(txn.ends_with(&hex("00000000 819a 02 0000")));
        assert_eq!(parse_perform_script_request(&txn, 8).unwrap(), old);
        assert!(old.waits());
        // Read with the ids a version-9 server expects, it is short.
        assert!(parse_perform_script_request(&txn, 9).is_err());
        // Trailing bytes are not ignored.
        let mut long = build_perform_script(1, 0x165, &req);
        long.insert(20, 0);
        assert_eq!(
            parse_perform_script_request(&long, 9),
            Err(PerformError::Truncated)
        );
    }

    #[test]
    fn script_keys_are_catalog_segments() {
        assert_eq!(script_key(5), Some(vec![5]));
        assert_eq!(script_key(141), Some(vec![0x80, 0x0d]));
        assert_eq!(script_key(538), Some(vec![0x81, 0x9a]));
        assert_eq!(script_id(&[0x80, 0x0d]), Some(141));
        assert_eq!(script_id(&[0x81]), None);
        assert_eq!(script_key(0), None);
        assert_eq!(script_key(u64::MAX), None);
        assert_eq!(script_key(0x2_0408_1020_407f).unwrap().len(), 7);
        assert_eq!(script_key(0x2_0408_1020_4080), None);
    }

    #[test]
    fn reply_follows_the_field_table() {
        // 11.2: extended error text, result, error u16 at the tail.
        let reply = PerformScriptReply {
            error: 0,
            result: "ok".into(),
            extended_error: String::new(),
        };
        let result = reply.to_result();
        assert_eq!(
            result,
            hex(&format!("{} {} 0000", unistring(""), unistring("ok")))
        );
        assert_eq!(parse_perform_script_reply(&result).unwrap(), reply);

        let failed = PerformScriptReply {
            error: 104,
            result: String::new(),
            extended_error: "Missing".into(),
        };
        assert_eq!(
            parse_perform_script_reply(&failed.to_result()).unwrap(),
            failed
        );
        // An empty, one-byte or oversized body is error 3.
        for body in [&[][..], &[0]] {
            assert_eq!(parse_perform_script_reply(body).unwrap().error, 3);
        }
        assert_eq!(
            parse_perform_script_reply(&vec![0; MAX_REPLY + 1])
                .unwrap()
                .error,
            3
        );
        // A result over the limit is cut to it.
        let long = PerformScriptReply {
            result: "x".repeat(TEXT_LIMIT + 5),
            ..PerformScriptReply::default()
        };
        let parsed = parse_perform_script_reply(&long.to_result()).unwrap();
        assert_eq!(parsed.result.len(), TEXT_LIMIT);
    }

    #[test]
    fn psos_result_follows_the_field_table() {
        // 11.4: extended error text, result, error, client callback id.
        let msg = PsosResult {
            callback: 3,
            error: 401,
            result: "none".into(),
            extended_error: String::new(),
        };
        let txn = build_psos_result(9, 0x165, &msg);
        let expected = hex(&format!(
            "69 00 0000 00000009 00000001 00000165 00000000 {} {} 0191 00000003",
            unistring(""),
            unistring("none")
        ));
        assert_eq!(txn, expected);
        assert_eq!(parse_psos_result(&txn).unwrap(), msg);
        assert_eq!(parse_psos_result_reply(&psos_result_reply()).unwrap(), 0);
    }
}
