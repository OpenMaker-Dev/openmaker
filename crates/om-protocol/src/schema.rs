//! Schema editing transactions (spec `docs/re/schema-path.md`): the host
//! clock (op82), lock upgrades (op61) and the request to process a table's
//! deferred field work (op73). The rest of Manage Database reuses other
//! codecs: op15 is op90's request answered without a lock
//! ([`crate::build_download_from_host`]), op13 is op14's layout with the
//! temporary-area flag clear ([`crate::write::build_upload_to_host`]), and the
//! schema op86 instructions are programs like any other.

use om_rctransaction::{Builder, Reader, ResultBuilder, ResultReader};

use crate::download::file_header;
use crate::write::{finished, pop_key, push_key, result_key, KeyPath};
use crate::{Opcode, PerformError};

// ---------------------------------------------------------------------------
// op82 HostTimeStamp (spec 3.1)

/// Build an op82 transaction: the header alone.
pub fn build_host_timestamp(transaction_id: u32, file_id: u32) -> Vec<u8> {
    Builder::with_header(file_header(Opcode::HostTimeStamp, transaction_id, file_id)).finish()
}

/// The op82 reply: the host's date-time key (the 8-byte timestamp index
/// key of query-program.md 7.3) as a one-segment key, with no status.
pub fn host_timestamp_reply(date_time_key: &[u8]) -> Vec<u8> {
    let key: KeyPath = vec![date_time_key.to_vec()];
    result_key(ResultBuilder::new(), &key).finish()
}

/// Parse an op82 result stack (the client side): the date-time key.
pub fn parse_host_timestamp_reply(result: &[u8]) -> Result<Vec<u8>, PerformError> {
    let mut r = ResultReader::new(result);
    let mut key = pop_key(&mut r)?;
    if r.remaining() != 0 || key.len() != 1 {
        return Err(PerformError::Truncated);
    }
    Ok(key.remove(0))
}

// ---------------------------------------------------------------------------
// op61 Upgrade (spec 4.2)

/// An op61 request: change the type of a lock the transaction holds, e.g. a
/// catalog's write lock (9) to exclusive (10) before replacing it. The reply
/// is op60's ([`crate::write::LockReply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upgrade {
    /// The client's wait block address; opaque.
    pub waiter: u64,
    pub timeout: u32,
    pub path: KeyPath,
    pub host_file: i32,
    pub txn: u32,
    pub new_type: u16,
    pub current_type: u16,
}

/// Build an op61 transaction.
pub fn build_upgrade(transaction_id: u32, file_id: u32, req: &Upgrade) -> Vec<u8> {
    let header = file_header(Opcode::Upgrade, transaction_id, file_id);
    let b = Builder::with_header(header)
        .push_u64(req.waiter)
        .push_u32(req.timeout);
    push_key(b, &req.path)
        .push_u32(req.host_file as u32)
        .push_u32(req.txn)
        .push_u16(req.new_type)
        .push_u16(req.current_type)
        .finish()
}

/// Parse an op61 transaction (the server side).
pub fn parse_upgrade_request(transaction: &[u8]) -> Result<Upgrade, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let current_type = r.pop_u16()?;
    let new_type = r.pop_u16()?;
    let txn = r.pop_u32()?;
    let host_file = r.pop_u32()? as i32;
    let path = pop_key(&mut r)?;
    let timeout = r.pop_u32()?;
    let waiter = r.pop_u64()?;
    finished(&r)?;
    Ok(Upgrade {
        waiter,
        timeout,
        path,
        host_file,
        txn,
        new_type,
        current_type,
    })
}

// ---------------------------------------------------------------------------
// op73 HostUpdateTable (spec 3.6, 7.2)

/// Build an op73 transaction: process the deferred field work of `table`.
pub fn build_update_table(transaction_id: u32, file_id: u32, table: &KeyPath) -> Vec<u8> {
    let header = file_header(Opcode::HostUpdateTable, transaction_id, file_id);
    push_key(Builder::with_header(header), table).finish()
}

/// Parse an op73 transaction (the server side): the table key.
pub fn parse_update_table_request(transaction: &[u8]) -> Result<KeyPath, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let table = pop_key(&mut r)?;
    finished(&r)?;
    Ok(table)
}

/// The op73 reply: no fields at all.
pub fn update_table_reply() -> Vec<u8> {
    ResultBuilder::new().finish()
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

    #[test]
    fn host_timestamp_reply_is_one_key() {
        // live:s0/r274_op82: 2026-10-28 15:12:42.
        let reply = hex("08 9faa9c 8005658730 09");
        let key = parse_host_timestamp_reply(&reply).unwrap();
        assert_eq!(key, hex("9faa9c 8005658730"));
        assert_eq!(host_timestamp_reply(&key), reply);
        assert_eq!(
            build_host_timestamp(0x1010, 0x165),
            hex("52000000 00001010 00000001 00000165 00000000")
        );
    }

    #[test]
    fn upgrade_and_update_table_round_trip() {
        // live:s0/r302_op61_up16.
        let req = hex("3d000000 00001016 00000001 00000165 00000000
             000000016dab3000 ffffffff 0103011004 00000165 0000002c 000a 0009");
        let parsed = parse_upgrade_request(&req).unwrap();
        assert_eq!(parsed.path, vec![vec![3], vec![0x10]]);
        assert_eq!(
            (parsed.txn, parsed.new_type, parsed.current_type),
            (0x2c, 10, 9)
        );
        assert_eq!(build_upgrade(0x1016, 0x165, &parsed), req);

        // live:s2b/b_op73.
        let req = hex("49000000 0000101e 00000001 00000165 00000000 02800703");
        let table = parse_update_table_request(&req).unwrap();
        assert_eq!(table, vec![vec![0x80, 0x07]]);
        assert_eq!(build_update_table(0x101e, 0x165, &table), req);
        assert!(update_table_reply().is_empty());
    }
}
