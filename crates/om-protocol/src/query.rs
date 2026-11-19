//! The QueryProgram (op86) transaction: run a small program of find, join,
//! sort and set operations against the open file (spec
//! `docs/re/query-program.md`; framing `docs/re/network-protocol.md` 5.6.6c).
//!
//! The request uploads the program, a slot tree, with the shared
//! upload/download framing; the reply carries the program's result spots
//! `[3]` re-rooted as a slot stream. op86 has no per-op request or reply
//! fields, so the reply ends with the resume block.

use om_rctransaction::{Reader, ResultReader};

use crate::download::{file_header, pop_payload, push_request_base, push_within};
use crate::{Opcode, PerformError, Resume};

/// `maxreturn` the official client sends with every op86 request.
pub const QUERY_MAX_RETURN: u32 = 0x0010_002a;

/// Everything needed to build one QueryProgram transaction.
#[derive(Clone, Debug)]
pub struct QueryRequest<'a> {
    /// The connection's next file-transaction id (header transid-low).
    pub transaction_id: u32,
    /// The opened file's server id (`OpenReply::file_id`).
    pub file_id: u32,
    /// The program: a bracketed slot list (empty for a continuation).
    pub program: &'a [u8],
    /// [`Resume::start`] for the first request; the previous reply's block,
    /// echoed, for each continuation.
    pub resume: Resume,
}

/// Build the QueryProgram (op86) transaction.
pub fn build_query(req: &QueryRequest<'_>) -> Vec<u8> {
    let header = file_header(Opcode::QueryProgram, req.transaction_id, req.file_id);
    push_request_base(header, req.program, &req.resume).finish()
}

/// A parsed QueryProgram request (the server side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedQuery {
    /// The program, or the next part of an upload.
    pub program: Vec<u8>,
    pub resume: Resume,
}

/// Parse a QueryProgram (op86) transaction.
pub fn parse_query_request(transaction: &[u8]) -> Result<ReceivedQuery, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let resume = Resume::pop(&mut r)?;
    let program = pop_payload(&mut r)?;
    Ok(ReceivedQuery { program, resume })
}

/// A QueryProgram reply: the result spots (or one page of them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryReply {
    /// The program's `[3]` subtree as a slot stream: `80`, one pushed list
    /// per result spot, `40`.
    pub slots: Vec<u8>,
    pub resume: Resume,
}

impl QueryReply {
    /// The result stack a server sends: `[slots N][u32 N][resume block]`.
    pub fn to_result(&self) -> Vec<u8> {
        self.to_result_within(usize::MAX)
    }

    /// [`QueryReply::to_result`] in a buffer of `max_return` bytes, dropping
    /// trailer fields that do not fit (as for op90).
    ///
    /// # Panics
    /// If the slot data is 4 GiB or longer.
    pub fn to_result_within(&self, max_return: usize) -> Vec<u8> {
        let n = u32::try_from(self.slots.len()).expect("slot data under 4 GiB");
        let mut out = self.slots.clone();
        let mut fields = vec![n.to_be_bytes().to_vec()];
        fields.extend(self.resume.fields());
        push_within(&mut out, &fields, max_return);
        out
    }
}

/// Parse a QueryProgram (op86) result stack.
pub fn parse_query_reply(result: &[u8]) -> Result<QueryReply, PerformError> {
    let mut r = ResultReader::new(result);
    let resume = Resume::pop(&mut r)?;
    let slots = pop_payload(&mut r)?;
    Ok(QueryReply { slots, resume })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_reply_round_trip() {
        let program = [0x80, 0x20, 0x01, 0x80, 0x40, 0x40];
        let txn = build_query(&QueryRequest {
            transaction_id: 0xca,
            file_id: 0x84,
            program: &program,
            resume: Resume::start(),
        });
        // Captured tail of every op86 request: N, 16 zero bytes, ff ff, 00.
        let mut tail = vec![0, 0, 0, 6];
        tail.extend([0; 16]);
        tail.extend([0xff, 0xff, 0x00]);
        assert!(txn.ends_with(&tail));
        assert_eq!(
            parse_query_request(&txn),
            Ok(ReceivedQuery {
                program: program.to_vec(),
                resume: Resume::start(),
            })
        );

        // cap:456's reply: an empty result spot [2], then the 23-byte trailer.
        let reply = QueryReply {
            slots: vec![0x80, 0x20, 0x02, 0x80, 0x40, 0x40],
            resume: Resume::complete(),
        };
        let result = reply.to_result();
        assert_eq!(result.len(), 6 + 23);
        assert!(result.ends_with(&[0; 19]));
        assert_eq!(parse_query_reply(&result), Ok(reply));
    }
}
