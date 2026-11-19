//! OpenMaker's hosted-file list: the files an OpenMaker Server publishes for
//! the Hosts dialog. This is OpenMaker's own extension to RCFMP12, not a
//! FileMaker operation (`docs/hosted-file-list.md`).
//!
//! FileMaker Pro lists a host's files with GetFileList (op77, spec
//! `docs/re/network-protocol.md` 5.3), whose reply is not decoded yet, so
//! OpenMaker's client cannot read FileMaker Server's list. OpenMaker's
//! client and server use this extension instead, and only with each other:
//! the client asks the server object `_is_a(`[`REPO_ID`]`)`, the type check
//! every CORBA object answers, and calls [`LIST_FILES`] only after `true`.
//! A host that does not implement the interface answers `false` (CORBA's
//! `_is_a` semantics; FileMaker Server 26.0.3 did, observed 2026-11-02 on a
//! lab server) and is never sent the call.
//!
//! - Request `OpenMakerListFiles(in unsigned long connectionid, in unsigned
//!   long version)`: the id `Connect` returned, and the newest list version
//!   the client reads ([`VERSION`]).
//! - Reply: `unsigned long version` (the one used, at most the client's),
//!   then `sequence<string>`: the published files' names as clients open
//!   them, UTF-8. Names only: no paths, accounts or other files.

use std::collections::HashSet;

use om_giop::{
    build_reply, build_request, Reader as CdrReader, Reply, ReplyStatus, Writer as CdrWriter,
};

use crate::OBJECT_KEY;

/// The interface a host claims with `_is_a` when it answers [`LIST_FILES`].
pub const REPO_ID: &[u8] = b"IDL:OpenMaker/HostedFiles:1.0";

/// The CORBA operation that lists the files.
pub const LIST_FILES: &[u8] = b"OpenMakerListFiles";

/// The list version this side reads and writes.
pub const VERSION: u32 = 1;

/// The most names a list may hold; a longer one is malformed.
pub const MAX_FILES: usize = 10_000;

/// The longest name, in UTF-8 bytes.
pub const MAX_NAME_LEN: usize = 1024;

/// An [`LIST_FILES`] request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListFilesRequest {
    pub connection_id: u32,
    /// The newest list version the client reads.
    pub version: u32,
}

/// Why a list reply cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListFilesError {
    /// Not a GIOP reply.
    Giop(String),
    /// The host raised an exception instead of listing.
    Refused,
    /// The reply ends early.
    Truncated,
    /// A name is not a NUL-terminated CDR string.
    UnterminatedName,
    /// A version the client did not ask for.
    Version(u32),
    /// More names than [`MAX_FILES`].
    TooMany(u32),
    /// A name no hosted file can have ([`is_listable_name`]), or one twice.
    BadName(String),
    /// Bytes after the list.
    Trailing(usize),
}

impl std::fmt::Display for ListFilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListFilesError::Giop(e) => write!(f, "not a reply: {e}"),
            ListFilesError::Refused => f.write_str("the host refused the request"),
            ListFilesError::Truncated => f.write_str("the list ends early"),
            ListFilesError::UnterminatedName => {
                f.write_str("a file name is not a NUL-terminated CDR string")
            }
            ListFilesError::Version(v) => write!(f, "unexpected list version {v}"),
            ListFilesError::TooMany(n) => write!(f, "{n} names, more than {MAX_FILES}"),
            ListFilesError::BadName(name) => write!(f, "{name:?} is not a hosted file's name"),
            ListFilesError::Trailing(n) => write!(f, "{n} bytes after the list"),
        }
    }
}

impl std::error::Error for ListFilesError {}

/// Whether `name` can be a hosted file's name in a list: not empty, at most
/// [`MAX_NAME_LEN`] bytes, with no path separator or control character. A
/// server publishes only such names; a client takes any other as malformed.
pub fn is_listable_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// Build the request (GIOP message) for the files of the connection that
/// `Connect` named `connection_id`.
pub fn build_list_files(request_id: u32, connection_id: u32) -> Vec<u8> {
    // Every field needs at most 4-byte alignment, and the body starts
    // 8-aligned, so authoring it at origin 0 pads as on the wire.
    let mut body = CdrWriter::with_origin(0);
    body.u32(connection_id);
    body.u32(VERSION);
    build_request(
        request_id,
        true,
        OBJECT_KEY,
        LIST_FILES,
        &[],
        &body.into_bytes(),
    )
}

/// Parse a request body ([`om_giop::Request::body`], the server side).
pub fn parse_list_files_request(body: &[u8]) -> Option<ListFilesRequest> {
    let mut r = CdrReader::new(body, 0);
    Some(ListFilesRequest {
        connection_id: r.u32().ok()?,
        version: r.u32().ok()?,
    })
}

/// Build the reply listing `names`, which must be listable
/// ([`is_listable_name`]), at most [`MAX_FILES`] of them.
pub fn build_list_files_reply(request_id: u32, names: &[&str]) -> Vec<u8> {
    debug_assert!(names.len() <= MAX_FILES);
    debug_assert!(names.iter().all(|name| is_listable_name(name)));
    let mut body = CdrWriter::with_origin(0);
    body.u32(VERSION);
    body.u32(u32::try_from(names.len()).unwrap_or(u32::MAX));
    for name in names {
        body.string(name.as_bytes());
    }
    build_reply(
        request_id,
        ReplyStatus::NoException,
        &[],
        &body.into_bytes(),
    )
}

/// The names a reply message lists, in its order. Anything but a complete,
/// well-formed list of distinct listable names is an error, never a
/// shorter list.
pub fn parse_list_files_reply(message: &[u8]) -> Result<Vec<String>, ListFilesError> {
    let reply = Reply::parse(message).map_err(|e| ListFilesError::Giop(e.to_string()))?;
    if reply.status != ReplyStatus::NoException {
        return Err(ListFilesError::Refused);
    }
    let mut r = CdrReader::new(reply.body, 0);
    let truncated = |_| ListFilesError::Truncated;
    let version = r.u32().map_err(truncated)?;
    if version != VERSION {
        return Err(ListFilesError::Version(version));
    }
    let count = r.u32().map_err(truncated)?;
    if count as usize > MAX_FILES {
        return Err(ListFilesError::TooMany(count));
    }
    let mut names = Vec::with_capacity(count as usize);
    let mut seen = HashSet::new();
    for _ in 0..count {
        // The legacy CDR reader accepts an absent terminator. This
        // extension requires the complete string representation, including
        // a nonzero length and its final NUL.
        let encoded = r.octet_sequence().map_err(truncated)?;
        let raw = encoded
            .strip_suffix(&[0])
            .ok_or(ListFilesError::UnterminatedName)?;
        let name = std::str::from_utf8(raw)
            .map_err(|_| ListFilesError::BadName(String::from_utf8_lossy(raw).into_owned()))?;
        // FileMaker matches file names case-insensitively, so two that
        // differ only in case would be the same file.
        if !is_listable_name(name) || !seen.insert(name.to_lowercase()) {
            return Err(ListFilesError::BadName(name.to_owned()));
        }
        names.push(name.to_owned());
    }
    match r.remaining() {
        0 => Ok(names),
        n => Err(ListFilesError::Trailing(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_giop::{build_system_exception_reply, Completion, Request};

    #[test]
    fn request_round_trips() {
        let message = build_list_files(6, 0x463b_59ad);
        let request = Request::parse(&message).unwrap();
        assert_eq!(request.operation, LIST_FILES);
        assert_eq!(request.object_key, OBJECT_KEY);
        assert_eq!(
            parse_list_files_request(request.body),
            Some(ListFilesRequest {
                connection_id: 0x463b_59ad,
                version: VERSION,
            })
        );
    }

    #[test]
    fn reply_round_trips_including_an_empty_list() {
        let names = ["Alpha.fmp12", "Beta Café.fmp12"];
        let reply = build_list_files_reply(6, &names);
        assert_eq!(parse_list_files_reply(&reply).unwrap(), names);
        let empty = build_list_files_reply(6, &[]);
        assert_eq!(
            parse_list_files_reply(&empty).unwrap(),
            Vec::<String>::new()
        );
    }

    /// A reply whose body is `body`.
    fn reply(body: &[u8]) -> Vec<u8> {
        build_reply(6, ReplyStatus::NoException, &[], body)
    }

    fn list(version: u32, count: u32, names: &[&[u8]]) -> Vec<u8> {
        let mut body = CdrWriter::with_origin(0);
        body.u32(version);
        body.u32(count);
        for name in names {
            body.string(name);
        }
        body.into_bytes()
    }

    #[test]
    fn malformed_and_truncated_replies_are_errors_not_shorter_lists() {
        let whole = list(VERSION, 2, &[b"Alpha.fmp12", b"Beta.fmp12"]);
        for cut in 0..whole.len() {
            assert!(
                parse_list_files_reply(&reply(&whole[..cut])).is_err(),
                "a list cut at {cut} bytes was accepted"
            );
        }
        let mut extra = whole.clone();
        extra.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            parse_list_files_reply(&reply(&extra)),
            Err(ListFilesError::Trailing(4))
        );
        assert_eq!(
            parse_list_files_reply(&reply(&list(2, 0, &[]))),
            Err(ListFilesError::Version(2))
        );
        assert_eq!(
            parse_list_files_reply(&reply(&list(VERSION, 10_001, &[]))),
            Err(ListFilesError::TooMany(10_001))
        );
        // A declared string that fits in the reply still needs its NUL;
        // an empty octet sequence isn't an empty CDR string either.
        for raw in [&b"Alpha.fmp12"[..], &b""[..]] {
            let mut body = CdrWriter::with_origin(0);
            body.u32(VERSION);
            body.u32(1);
            body.octet_sequence(raw);
            assert_eq!(
                parse_list_files_reply(&reply(&body.into_bytes())),
                Err(ListFilesError::UnterminatedName)
            );
        }
        // Paths, control characters, blanks, bad UTF-8 and duplicates
        // (FileMaker names are case-insensitive) are not file names.
        for bad in [
            &b"/srv/fm/Alpha.fmp12"[..],
            b"Data\\Alpha.fmp12",
            b"Al\npha.fmp12",
            b"  ",
            b"\xff.fmp12",
        ] {
            assert!(matches!(
                parse_list_files_reply(&reply(&list(VERSION, 1, &[bad]))),
                Err(ListFilesError::BadName(_))
            ));
        }
        assert!(matches!(
            parse_list_files_reply(&reply(&list(VERSION, 2, &[b"Alpha.fmp12", b"ALPHA.fmp12"]))),
            Err(ListFilesError::BadName(_))
        ));
    }

    #[test]
    fn an_exception_is_a_refusal() {
        let message = build_system_exception_reply(
            6,
            "IDL:omg.org/CORBA/BAD_OPERATION:1.0",
            0,
            Completion::No,
        );
        assert_eq!(
            parse_list_files_reply(&message),
            Err(ListFilesError::Refused)
        );
        assert!(matches!(
            parse_list_files_reply(b"GIOP"),
            Err(ListFilesError::Giop(_))
        ));
    }
}
