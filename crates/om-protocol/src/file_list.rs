//! GetFileList (op77): the files a host lists in FileMaker Pro's Hosts dialog
//! (spec `docs/re/network-protocol.md` 5.3).
//!
//! op77 is an `RCUploadDownload` transaction (5.6.6c): the request is the
//! shared block with no payload, then the credentials, the share type and
//! two `u16`s; the reply is the list as a download page, then the count,
//! the host's name and an error. A list longer than one reply continues on
//! later pages, each asked for by echoing the reply's resume block (5.6.6c).
//! It needs no open file, so its header names none. Only the anonymous
//! request is built, as FileMaker Pro's Hosts window sends it before anyone
//! signs in: its credentials are all empty, so nothing is obscured.

use om_rctransaction::{Header, Pop, Reader, ResultReader};

use crate::download::{pop_payload, push_request_base, Resume};
use crate::{Opcode, PerformError};

/// The request's last `u16`, `mClientVersion` (spec 5.3).
pub const FILE_LIST_VERSION: u16 = 8;

/// The share type FileMaker Pro's Hosts window lists files for: the
/// "Access via FileMaker Network" extended privilege (spec 5.3, observed).
pub const SHARE_TYPE_FMAPP: &str = "fmapp";

/// The reply error with which external authentication continues: only
/// then does the reply carry an authentication blob (spec 5.3).
const EXTERNAL_AUTHENTICATION: u16 = 0x20d0;

/// The credentials of an anonymous request: an empty external-authentication
/// blob (`u16` length), privilege extension, password and account name
/// (`u32` lengths each), with no bytes to obscure (spec 5.3: 14 zero bytes).
const ANONYMOUS_CREDENTIALS: [u8; 14] = [0; 14];

/// An op77 request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileListRequest {
    /// The extended privilege the caller lists files for (`fmapp` from
    /// FileMaker Pro's Hosts window; empty lists every shared file).
    pub share_type: String,
    /// `RCGetFileList` bits: 1 ignore the hide bits, 4 a Data API listing.
    pub flags: u16,
    pub version: u16,
    /// Whether the request names an account (its credentials are not empty).
    /// Only read on the server side; [`build_file_list`] sends none.
    pub signed_in: bool,
    /// [`Resume::start`] for the first page; the previous reply's block,
    /// echoed, for each later one.
    pub resume: Resume,
}

impl FileListRequest {
    /// The anonymous request FileMaker Pro's Hosts window sends.
    pub fn anonymous() -> Self {
        Self {
            share_type: SHARE_TYPE_FMAPP.to_owned(),
            flags: 0,
            version: FILE_LIST_VERSION,
            signed_in: false,
            resume: Resume::start(),
        }
    }
}

/// Build an anonymous op77 transaction. [`FileListRequest::signed_in`] is
/// ignored: no credentials are sent.
pub fn build_file_list(transaction_id: u32, request: &FileListRequest) -> Vec<u8> {
    let mut header = Header::new(Opcode::GetFileList as u8);
    header.transid_lo = transaction_id;
    push_request_base(header, &[], &request.resume)
        .push_raw(&ANONYMOUS_CREDENTIALS)
        .push_string(&request.share_type)
        .push_u16(request.flags)
        .push_u16(request.version)
        .finish()
}

/// Parse an op77 transaction (the server side). The credentials are read
/// past without being revealed: only whether an account is named is kept.
pub fn parse_file_list_request(transaction: &[u8]) -> Result<FileListRequest, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let version = r.pop_u16()?;
    let flags = r.pop_u16()?;
    let share_type = r.pop_string()?;
    let mut signed_in = false;
    // Account name, password, privilege extension: obscured bytes under a
    // u32 length each.
    for field in 0..3 {
        let len = usize::try_from(r.pop_u32()?).map_err(|_| PerformError::Truncated)?;
        r.pop_raw(len)?;
        signed_in |= field == 0 && len > 0;
    }
    let blob = usize::from(r.pop_u16()?);
    r.pop_raw(blob)?;
    let resume = Resume::pop(&mut r)?;
    pop_payload(&mut r)?;
    Ok(FileListRequest {
        share_type,
        flags,
        version,
        signed_in,
        resume,
    })
}

/// An op77 reply: the list, or one page of it, whose nodes are keyed
/// `01`, `02`, ... in listing order (each a file: `01` its id, `07` its
/// folder, `10` its name, `1818` its preferences), and the count of files,
/// the host's name and an error (spec 5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileListReply {
    /// The list tree's slot bytes.
    pub slots: Vec<u8>,
    pub resume: Resume,
    pub count: u16,
    pub host_name: String,
    /// 0, or why there is no list (18 sign-in required, 1638 or 812 the
    /// host takes no new users, ...).
    pub error: u16,
}

impl FileListReply {
    /// A complete list of `count` files, whose tree is `slots`.
    pub fn listed(slots: Vec<u8>, count: u16, host_name: &str) -> Self {
        Self {
            slots,
            resume: Resume::complete(),
            count,
            host_name: host_name.to_owned(),
            error: 0,
        }
    }

    /// The result stack a server sends:
    /// `[slots N][u32 N][resume block][u16 count][host name][u16 error]`.
    /// No external-authentication blob is sent.
    ///
    /// # Panics
    /// If the slots are 4 GiB or longer.
    pub fn to_result(&self) -> Vec<u8> {
        let n = u32::try_from(self.slots.len()).expect("slot data under 4 GiB");
        let mut out = self.slots.clone();
        out.extend(n.to_be_bytes());
        for field in self.resume.fields() {
            out.extend(field);
        }
        out.extend(self.count.to_be_bytes());
        out.extend(
            om_scsu::encode_field(&self.host_name, om_scsu::Framing::TrailingU32)
                .expect("trailing-u32 length cannot overflow"),
        );
        out.extend(self.error.to_be_bytes());
        out
    }
}

/// Parse an op77 result stack (the client side). An external-authentication
/// blob (error 0x20d0) is read past.
pub fn parse_file_list_reply(result: &[u8]) -> Result<FileListReply, PerformError> {
    let mut r = ResultReader::new(result);
    let error = r.pop_u16()?;
    let host_name = r.pop_string()?;
    let count = r.pop_u16()?;
    if error == EXTERNAL_AUTHENTICATION {
        let blob = usize::from(r.pop_u16()?);
        r.pop_raw(blob)?;
    }
    let resume = Resume::pop(&mut r)?;
    let slots = pop_payload(&mut r)?;
    Ok(FileListReply {
        slots,
        resume,
        count,
        host_name,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let digits: String = text.chars().filter(char::is_ascii_hexdigit).collect();
        (0..digits.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).unwrap())
            .collect()
    }

    /// FileMaker Pro 26's Hosts window: 70 bytes, the 50 after the header
    /// as spec 5.3's `fmapp` vector (observed through a relay capture).
    #[test]
    fn the_anonymous_request_is_the_hosts_window_s() {
        let transaction = build_file_list(1, &FileListRequest::anonymous());
        assert_eq!(transaction[0], Opcode::GetFileList as u8);
        assert_eq!(transaction.len(), 70);
        assert_eq!(
            transaction[20..],
            hex("00000000 0000000000000000 0000000000000000 ffff 00
                 0000 00000000 00000000 00000000
                 3c373b2a2a 00000005
                 0000 0008")[..]
        );
        assert_eq!(
            parse_file_list_request(&transaction),
            Ok(FileListRequest::anonymous())
        );
        // A later page's request echoes the reply's resume block.
        let next = FileListRequest {
            resume: Resume {
                perform: 7,
                cursor: 9,
                depth: 2,
                copy_key: Vec::new(),
            },
            ..FileListRequest::anonymous()
        };
        assert_eq!(
            parse_file_list_request(&build_file_list(2, &next)),
            Ok(next)
        );
    }

    #[test]
    fn a_reply_round_trips_and_its_errors_parse() {
        let reply = FileListReply::listed(vec![0x80, 0x20, 0x01], 1, "server");
        assert_eq!(parse_file_list_reply(&reply.to_result()), Ok(reply));
        let refused = FileListReply {
            error: 18,
            ..FileListReply::listed(Vec::new(), 0, "server")
        };
        assert_eq!(parse_file_list_reply(&refused.to_result()), Ok(refused));
    }

    #[test]
    fn truncated_transactions_are_errors() {
        let transaction = om_rctransaction::Builder::new(Opcode::GetFileList as u8).finish();
        assert!(parse_file_list_request(&transaction).is_err());
        let result = FileListReply::listed(vec![1, 2, 3], 1, "server").to_result();
        assert!(parse_file_list_reply(&result[1..]).is_err());
        assert!(parse_file_list_reply(&result[..result.len() - 1]).is_err());
    }
}
