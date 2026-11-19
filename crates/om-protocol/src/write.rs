//! The record write path (spec `docs/re/write-path.md`): HostCreateRecord
//! (op71), HostGetSerial (op72), Upload List To Host Temp Location (op14),
//! HostCommit (op70), TransactInfo (op103), the locks Lock (op60), Unlock
//! (op62) and KillAllLocks (op65), and the change notifications a server
//! sends back.
//!
//! A **key** in these bodies is written with `Push(uchar const*)`: its
//! segments as `[u8 len][bytes]` and then the `u8` total length, e.g. table
//! `80 02` = `02 80 02 03`. Fields are listed in push order; the receiver pops
//! them from the end.

use om_hbam::OwnedNode;
use om_rctransaction::{Builder, Header, Pop, Reader, ResultBuilder, ResultReader, TooLong};

use crate::download::{
    decode_path, encode_path, file_header, pop_payload, push_request_base, push_within,
};
use crate::{Opcode, PerformError, Resume};

/// A key path: one byte string per segment.
pub type KeyPath = Vec<Vec<u8>>;

pub(crate) fn push_key(builder: Builder, key: &[impl AsRef<[u8]>]) -> Builder {
    let path = encode_path(key);
    let len = path.len() as u8;
    builder.push_raw(&path).push_u8(len)
}

pub(crate) fn result_key(builder: ResultBuilder, key: &[impl AsRef<[u8]>]) -> ResultBuilder {
    builder.push_short_bytes(&encode_path(key))
}

pub(crate) fn pop_key<'a>(r: &mut impl Pop<'a>) -> Result<KeyPath, PerformError> {
    decode_path(r.pop_short_bytes()?)
}

/// The users holding a lock that refused a request (spec 4.2): "session name
/// (account)" for each holder, and whether the holder is this connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub self_conflict: bool,
    pub users: String,
}

impl Conflict {
    fn push(&self, builder: ResultBuilder) -> ResultBuilder {
        builder
            .push_bool(self.self_conflict)
            .push_string(&self.users)
    }
}

fn pop_conflict(r: &mut ResultReader<'_>) -> Result<Conflict, PerformError> {
    let users = r.pop_string()?;
    let self_conflict = r.pop_u8()? != 0;
    Ok(Conflict {
        self_conflict,
        users,
    })
}

// ---------------------------------------------------------------------------
// op71 HostCreateRecord (spec 3.1)

/// Status of op71 when the table lock is held by another user.
pub const TABLE_LOCKED: u32 = 0x12e;

/// An op71 request: reserve `count` record ids in `table` for `txn`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRecord {
    /// The table id as a one-segment key, e.g. `[[0x80, 0x02]]`.
    pub table: KeyPath,
    pub count: u32,
    /// The transaction that will own the new records' locks.
    pub txn: u32,
}

/// Build an op71 transaction.
pub fn build_create_record(transaction_id: u32, file_id: u32, req: &CreateRecord) -> Vec<u8> {
    let header = file_header(Opcode::HostCreateRecord, transaction_id, file_id);
    push_key(Builder::with_header(header), &req.table)
        .push_u32(req.count)
        .push_u32(req.txn)
        .finish()
}

/// Parse an op71 transaction (the server side).
pub fn parse_create_record_request(transaction: &[u8]) -> Result<CreateRecord, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let txn = r.pop_u32()?;
    let count = r.pop_u32()?;
    let table = pop_key(&mut r)?;
    finished(&r)?;
    Ok(CreateRecord { table, count, txn })
}

/// An op71 reply: status 0 and the first new record's id as a one-segment
/// key, or an error (106 table missing, with the key `[00]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRecordReply {
    pub conflict: Option<Conflict>,
    /// A u32, unlike most ops.
    pub status: u32,
    pub record: KeyPath,
}

impl CreateRecordReply {
    /// Push order: (conflict), status, record key.
    pub fn to_result(&self) -> Vec<u8> {
        let mut b = ResultBuilder::new();
        if let Some(conflict) = &self.conflict {
            b = conflict.push(b);
        }
        result_key(b.push_u32(self.status), &self.record).finish()
    }
}

/// Parse an op71 result stack (the client side).
pub fn parse_create_record_reply(result: &[u8]) -> Result<CreateRecordReply, PerformError> {
    let mut r = ResultReader::new(result);
    let record = pop_key(&mut r)?;
    let status = r.pop_u32()?;
    let conflict = (status == TABLE_LOCKED)
        .then(|| pop_conflict(&mut r))
        .transpose()?;
    Ok(CreateRecordReply {
        conflict,
        status,
        record,
    })
}

// ---------------------------------------------------------------------------
// op72 HostGetSerial (spec 3.2)

/// op72 flag bit 0: store `value` as the next serial value.
pub const SERIAL_REPLACE: u32 = 1;
/// op72 flag bit 1: advance the next serial value by `increment`.
pub const SERIAL_INCREMENT: u32 = 2;
/// Status of op72 when the field definition is locked by another user.
pub const SCHEMA_LOCKED: u32 = 0x12f;

/// An op72 request: read (and advance or replace) a field's next serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetSerial {
    pub table: KeyPath,
    pub field: KeyPath,
    /// The value to store with [`SERIAL_REPLACE`]; empty otherwise.
    pub value: String,
    pub increment: i32,
    pub flags: u32,
}

/// Build an op72 transaction.
pub fn build_get_serial(transaction_id: u32, file_id: u32, req: &GetSerial) -> Vec<u8> {
    let header = file_header(Opcode::HostGetSerial, transaction_id, file_id);
    let b = push_key(Builder::with_header(header), &req.table);
    push_key(b, &req.field)
        .push_string(&req.value)
        .push_u32(req.increment as u32)
        .push_u32(req.flags)
        .finish()
}

/// Parse an op72 transaction (the server side).
pub fn parse_get_serial_request(transaction: &[u8]) -> Result<GetSerial, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let flags = r.pop_u32()?;
    let increment = r.pop_u32()? as i32;
    let value = r.pop_string()?;
    let field = pop_key(&mut r)?;
    let table = pop_key(&mut r)?;
    finished(&r)?;
    Ok(GetSerial {
        table,
        field,
        value,
        increment,
        flags,
    })
}

/// An op72 reply: the serial value before the update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetSerialReply {
    pub conflict: Option<Conflict>,
    /// 0, 102 field missing, or [`SCHEMA_LOCKED`].
    pub status: u32,
    pub value: String,
}

impl GetSerialReply {
    /// Push order: (conflict), status, value.
    pub fn to_result(&self) -> Vec<u8> {
        let mut b = ResultBuilder::new();
        if let Some(conflict) = &self.conflict {
            b = conflict.push(b);
        }
        b.push_u32(self.status).push_string(&self.value).finish()
    }
}

/// Parse an op72 result stack (the client side).
pub fn parse_get_serial_reply(result: &[u8]) -> Result<GetSerialReply, PerformError> {
    let mut r = ResultReader::new(result);
    let value = r.pop_string()?;
    let status = r.pop_u32()?;
    let conflict = (status == SCHEMA_LOCKED)
        .then(|| pop_conflict(&mut r))
        .transpose()?;
    Ok(GetSerialReply {
        conflict,
        status,
        value,
    })
}

// ---------------------------------------------------------------------------
// op14 Upload List To Host Temp Location (spec 3.3) and op70 HostCommit (3.4)

/// op14 flag bit 7: upload into the connection's temporary area, not the
/// file (clear in op13). The low bits are the destination's node state
/// ([`UPLOAD_NODE`] or [`UPLOAD_VALUE`]).
pub const UPLOAD_TO_TEMP: u8 = 0x80;
/// Upload flags: the destination is a node.
pub const UPLOAD_NODE: u8 = 2;
/// Upload flags: the destination is a terminal value.
pub const UPLOAD_VALUE: u8 = 1;
/// Upload flags: node state `00` with no payload, as FileMaker Pro empties
/// a deleted script's member (scripts.md, "Saving scripts to a host").
pub const UPLOAD_EMPTY: u8 = 0;
/// The flags of every observed op14 request.
pub const UPLOAD_FLAGS: u8 = UPLOAD_TO_TEMP | 2;

/// An op14 request: one part of a record upload; or op13, one part of a
/// subtree uploaded straight into the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// The subtree relative to `dest`. For op14 one node per record, keyed by
    /// record id, holding one leaf per stored field, then the `40` that
    /// closes `dest` (no leading `80`); for op13 a bracketed slot list. In a
    /// multi-part upload, this part's bytes.
    pub payload: Vec<u8>,
    pub resume: Resume,
    /// The value to store when the destination is a terminal key (op13 with
    /// node state 1, schema-path.md 3.3); empty for a node.
    pub terminal: Vec<u8>,
    pub flags: u8,
    /// The destination: `[T, 05]` for op14, a catalog index such as
    /// `[3, 16, 1]` for op13.
    pub dest: KeyPath,
}

/// The op14 payload for `records` (spec 3.3), the client side: one node per
/// record keyed by its id, holding one leaf per stored field, then the `40`
/// that closes the destination `[T, 05]`, with no leading `80`. `None` when
/// a key or value is too large for any slot.
pub fn upload_payload(records: Vec<OwnedNode>) -> Option<Vec<u8>> {
    let wire = OwnedNode::list(Vec::new(), records).to_wire()?;
    // `to_wire` opens the list with the `80` that op14 leaves out.
    Some(wire[1..].to_vec())
}

/// Build an op14 transaction.
pub fn build_upload(transaction_id: u32, file_id: u32, req: &Upload) -> Vec<u8> {
    build_upload_op(
        Opcode::UploadListToHostTempLocation,
        transaction_id,
        file_id,
        req,
    )
}

/// Build an op13 Upload To Host transaction: op14's layout, with the
/// temporary-area flag clear so the payload goes straight into the file
/// (schema-path.md 3.3). The server parses it with [`parse_upload_request`].
pub fn build_upload_to_host(transaction_id: u32, file_id: u32, req: &Upload) -> Vec<u8> {
    build_upload_op(Opcode::UploadToHost, transaction_id, file_id, req)
}

fn build_upload_op(opcode: Opcode, transaction_id: u32, file_id: u32, req: &Upload) -> Vec<u8> {
    let header = file_header(opcode, transaction_id, file_id);
    let b = push_request_base(header, &req.payload, &req.resume)
        .push_raw(&req.terminal)
        .push_u16(req.terminal.len() as u16)
        .push_u8(req.flags);
    push_key(b, &req.dest).finish()
}

/// Parse an op14 transaction (the server side).
pub fn parse_upload_request(transaction: &[u8]) -> Result<Upload, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let dest = pop_key(&mut r)?;
    let flags = r.pop_u8()?;
    let terminal_len = usize::from(r.pop_u16()?);
    let terminal = r.pop_raw(terminal_len)?.to_vec();
    let resume = Resume::pop(&mut r)?;
    let payload = pop_payload(&mut r)?;
    Ok(Upload {
        payload,
        resume,
        terminal,
        flags,
        dest,
    })
}

/// op70 flag `kCompleteBeforeReturning`, set in every observed request.
pub const COMMIT_COMPLETE_BEFORE_RETURNING: u8 = 1;

/// The value of a commit-list leaf `[T, 05, R]` for a new record.
pub const COMMIT_NEW: u8 = 1;
/// The value of a commit-list leaf `[T, 05, R]` for a deleted record. A
/// modified record is a node listing the changed field ids instead.
pub const COMMIT_DELETED: u8 = 3;

/// An op70 request: commit records from the temporary area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The commit list, a bracketed slot list keyed `[T, 05, R]`. In a
    /// multi-part upload, this part's bytes.
    pub list: Vec<u8>,
    pub resume: Resume,
    pub flags: u8,
    pub txn: u32,
}

/// The op70 commit list for `entries` of base table `table` (spec 3.4), the
/// client side: entries keyed by record id, each a leaf [`COMMIT_NEW`] or
/// [`COMMIT_DELETED`], or a node of value-less leaves naming the changed
/// fields. `None` when a key or value is too large for any slot.
pub fn commit_list(table: &[u8], entries: Vec<OwnedNode>) -> Option<Vec<u8>> {
    commit_list_tables(vec![(table.to_vec(), entries)])
}

/// A commit list over several tables, `[T, 05, R]` for each table in
/// order: one op70 commits them together (spec 3.4, "by the grammar,
/// several tables"), as a script transaction does.
pub fn commit_list_tables(tables: Vec<(Vec<u8>, Vec<OwnedNode>)>) -> Option<Vec<u8>> {
    let tables = tables
        .into_iter()
        .map(|(table, entries)| OwnedNode::list(table, vec![OwnedNode::list(vec![0x05], entries)]))
        .collect();
    OwnedNode::list(Vec::new(), tables).to_wire()
}

/// Build an op70 transaction.
pub fn build_commit(transaction_id: u32, file_id: u32, req: &Commit) -> Vec<u8> {
    let header = file_header(Opcode::HostCommit, transaction_id, file_id);
    push_request_base(header, &req.list, &req.resume)
        .push_u8(req.flags)
        .push_u32(req.txn)
        .finish()
}

/// Parse an op70 transaction (the server side).
pub fn parse_commit_request(transaction: &[u8]) -> Result<Commit, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let txn = r.pop_u32()?;
    let flags = r.pop_u8()?;
    let resume = Resume::pop(&mut r)?;
    let list = pop_payload(&mut r)?;
    Ok(Commit {
        list,
        resume,
        flags,
        txn,
    })
}

/// The reply to op14 and op70: an empty page `[u32 0][resume block]` with no
/// addition. A complete upload answers [`Resume::no_stream`]; a part of a
/// multi-part upload answers the block to continue with.
pub fn upload_reply(resume: &Resume, max_return: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut fields = vec![0u32.to_be_bytes().to_vec()];
    fields.extend(resume.fields());
    push_within(&mut out, &fields, max_return);
    out
}

/// Parse an op14 or op70 result stack (the client side).
pub fn parse_upload_reply(result: &[u8]) -> Result<Resume, PerformError> {
    let mut r = ResultReader::new(result);
    let resume = Resume::pop(&mut r)?;
    let n = r.pop_u32()?;
    if n != 0 || r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(resume)
}

// ---------------------------------------------------------------------------
// op103 TransactInfo (spec 3.5)

/// What a server answers to every op103.
pub const TRANSACT_INFO_REPLY: u32 = 0x15;

/// An op103 request: the files one transaction committed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactInfo {
    pub files: Vec<i32>,
    pub txn: u32,
}

/// Build an op103 transaction.
pub fn build_transact_info(transaction_id: u32, file_id: u32, req: &TransactInfo) -> Vec<u8> {
    let header = file_header(Opcode::TransactInfo, transaction_id, file_id);
    let mut b = Builder::with_header(header);
    for file in &req.files {
        b = b.push_u32(*file as u32);
    }
    b.push_u32(req.files.len() as u32)
        .push_u32(req.txn)
        .finish()
}

/// Parse an op103 transaction (the server side).
pub fn parse_transact_info_request(transaction: &[u8]) -> Result<TransactInfo, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let txn = r.pop_u32()?;
    let count = r.pop_u32()? as usize;
    if r.remaining() != count.saturating_mul(4) {
        return Err(PerformError::Truncated);
    }
    let mut files = (0..count)
        .map(|_| r.pop_u32().map(|f| f as i32))
        .collect::<Result<Vec<_>, _>>()?;
    files.reverse();
    Ok(TransactInfo { files, txn })
}

// ---------------------------------------------------------------------------
// Locks: op60 Lock (spec 4.2), op62 Unlock (4.3), op65 KillAllLocks (4.4)

/// Lock type for editing a record (`kDBWriteLock`).
pub const LOCK_WRITE: u16 = 9;
/// Lock type of a notification registration (`kDBNotifyLock`).
pub const LOCK_NOTIFY: u16 = 5;
/// op60 status: the server would have to block (`kLockMustWait`, spec 4.6).
pub const LOCK_MUST_WAIT: u16 = 0x1f41;
/// op60 status: the lock is held by another lock user.
pub const LOCK_CONFLICT: u16 = 0x1f43;

/// An op60 request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    /// The client's wait block address; opaque.
    pub waiter: u64,
    /// Seconds to wait; `u32::MAX` waits, 0 does not.
    pub timeout: u32,
    /// Whether the reply should carry the record's modification count.
    pub want_mod_count: bool,
    /// The locked path, `[T, 05, R]` for a record.
    pub path: KeyPath,
    /// With `want_mod_count`, the file id; otherwise 0.
    pub argument: i32,
    pub host_file: i32,
    pub txn: u32,
    pub lock_type: u16,
}

/// Build an op60 transaction.
pub fn build_lock(transaction_id: u32, file_id: u32, req: &Lock) -> Vec<u8> {
    let header = file_header(Opcode::Lock, transaction_id, file_id);
    let b = Builder::with_header(header)
        .push_u64(req.waiter)
        .push_u32(req.timeout)
        .push_bool(req.want_mod_count);
    push_key(b, &req.path)
        .push_u32(req.argument as u32)
        .push_u32(req.host_file as u32)
        .push_u32(req.txn)
        .push_u16(req.lock_type)
        .finish()
}

/// Parse an op60 transaction (the server side).
pub fn parse_lock_request(transaction: &[u8]) -> Result<Lock, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let lock_type = r.pop_u16()?;
    let txn = r.pop_u32()?;
    let host_file = r.pop_u32()? as i32;
    let argument = r.pop_u32()? as i32;
    let path = pop_key(&mut r)?;
    let want_mod_count = r.pop_u8()? != 0;
    let timeout = r.pop_u32()?;
    let waiter = r.pop_u64()?;
    finished(&r)?;
    Ok(Lock {
        waiter,
        timeout,
        want_mod_count,
        path,
        argument,
        host_file,
        txn,
        lock_type,
    })
}

/// An op60 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockReply {
    /// Granted. `mod_count` is present when the request asked for it: the
    /// record's `[T, 05, R, fc]` value as key bytes (`01 00` for a record
    /// never modified, empty for a record that does not exist).
    Granted { mod_count: Option<Vec<u8>> },
    /// [`LOCK_MUST_WAIT`]: the client waits for an op66 LockFinished. A bare
    /// status with no conflict (spec 4.2); FileMaker Server never sends it.
    MustWait,
    /// Refused, e.g. [`LOCK_CONFLICT`].
    Refused { status: u16, conflict: Conflict },
}

impl LockReply {
    /// The result stack a server sends (spec 4.2).
    ///
    /// # Errors
    /// [`TooLong`] for a grant whose modification count is longer than the
    /// 255 bytes its one-byte length frames, a value read from a file. A
    /// server that cannot send a grant must not keep the lock (OpenMaker
    /// Server frames the grant before it takes the lock). The other replies
    /// always frame.
    pub fn to_result(&self) -> Result<Vec<u8>, TooLong> {
        Ok(match self {
            LockReply::Granted { mod_count } => {
                let mut b = ResultBuilder::new();
                if let Some(mod_count) = mod_count {
                    b = b.try_push_short_bytes(mod_count)?;
                }
                b.push_u16(0).finish()
            }
            LockReply::MustWait => ResultBuilder::new().push_u16(LOCK_MUST_WAIT).finish(),
            LockReply::Refused { status, conflict } => conflict
                .push(ResultBuilder::new())
                .push_u16(*status)
                .finish(),
        })
    }
}

/// Parse an op60 result stack (the client side); `want_mod_count` as sent.
pub fn parse_lock_reply(result: &[u8], want_mod_count: bool) -> Result<LockReply, PerformError> {
    let mut r = ResultReader::new(result);
    let status = r.pop_u16()?;
    if status == 0 {
        let mod_count = want_mod_count
            .then(|| r.pop_short_bytes().map(<[u8]>::to_vec))
            .transpose()?;
        return Ok(LockReply::Granted { mod_count });
    }
    if status == LOCK_MUST_WAIT {
        return Ok(LockReply::MustWait);
    }
    Ok(LockReply::Refused {
        status,
        conflict: pop_conflict(&mut r)?,
    })
}

/// An op62 request: release one lock. The reply is the request's header
/// echoed with `idle_time` 0 ([`echo_header`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unlock {
    /// The notification address (a session id); 0 from FileMaker Pro.
    pub message_address: u32,
    pub attention_mask: u32,
    pub path: KeyPath,
    pub host_file: i32,
    pub txn: u32,
    pub lock_type: u16,
}

impl Unlock {
    /// The op62 that closes the file `host_file`: FileMaker Pro releases
    /// the notify lock on the file's root, attention `0x2000`
    /// (`kDBAttnChainingLogout`), transaction 0, when its user closes the
    /// file or the host asks it to (spec 4.3; network-protocol.md 6.5).
    pub fn close_file(host_file: i32) -> Self {
        Self {
            message_address: 0,
            attention_mask: attention::CHAINING_LOGOUT,
            path: Vec::new(),
            host_file,
            txn: 0,
            lock_type: LOCK_NOTIFY,
        }
    }

    /// Whether this op62 closes its file: the notify lock on the file's
    /// root, transaction 0, attention `0x2000`, the only form recorded
    /// (spec 4.3). A root unlock in another form does not close the file.
    pub fn closes_file(&self) -> bool {
        self.lock_type == LOCK_NOTIFY
            && self.path.is_empty()
            && self.txn == 0
            && self.attention_mask == attention::CHAINING_LOGOUT
    }
}

/// Build an op62 transaction.
pub fn build_unlock(transaction_id: u32, file_id: u32, req: &Unlock) -> Vec<u8> {
    let header = file_header(Opcode::Unlock, transaction_id, file_id);
    let b = Builder::with_header(header)
        .push_u32(req.message_address)
        .push_u32(req.attention_mask);
    push_key(b, &req.path)
        .push_u32(req.host_file as u32)
        .push_u32(req.txn)
        .push_u16(req.lock_type)
        .finish()
}

/// Parse an op62 transaction (the server side).
pub fn parse_unlock_request(transaction: &[u8]) -> Result<Unlock, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let lock_type = r.pop_u16()?;
    let txn = r.pop_u32()?;
    let host_file = r.pop_u32()? as i32;
    let path = pop_key(&mut r)?;
    let attention_mask = r.pop_u32()?;
    let message_address = r.pop_u32()?;
    finished(&r)?;
    Ok(Unlock {
        message_address,
        attention_mask,
        path,
        host_file,
        txn,
        lock_type,
    })
}

/// An op65 request: release every lock of the transaction (`all` false) or
/// of the connection on that file (`all` true). The reply is the request's
/// header echoed ([`echo_header`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillAllLocks {
    pub host_file: i32,
    pub txn: u32,
    pub all: bool,
}

/// Build an op65 transaction.
pub fn build_kill_all_locks(transaction_id: u32, file_id: u32, req: &KillAllLocks) -> Vec<u8> {
    let header = file_header(Opcode::KillAllLocks, transaction_id, file_id);
    Builder::with_header(header)
        .push_u32(req.host_file as u32)
        .push_u32(req.txn)
        .push_bool(req.all)
        .finish()
}

/// Parse an op65 transaction (the server side).
pub fn parse_kill_all_locks_request(transaction: &[u8]) -> Result<KillAllLocks, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let all = r.pop_u8()? != 0;
    let txn = r.pop_u32()?;
    let host_file = r.pop_u32()? as i32;
    finished(&r)?;
    Ok(KillAllLocks {
        host_file,
        txn,
        all,
    })
}

/// The reply to op62 and op65: the request's 20-byte header with
/// `idle_time` 0 (spec 4.3), so the u16 a client pops is 0.
pub fn echo_header(header: &Header) -> Vec<u8> {
    let mut echo = header.clone();
    echo.idle_time = 0;
    let mut out = Vec::new();
    echo.write(&mut out);
    out
}

// ---------------------------------------------------------------------------
// Change notifications (spec section 6)

/// `DBAttnCode` change kinds and scopes.
pub mod attention {
    pub const RECORD: u32 = 0x001;
    pub const ADD: u32 = 0x100;
    pub const MODIFY: u32 = 0x200;
    pub const DELETE: u32 = 0x400;
    /// `kDBAttnChainingLogout`: the attention of the op62 that closes a
    /// file.
    pub const CHAINING_LOGOUT: u32 = 0x2000;
}

/// One queued notification: the marshalled message and its code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The attention code, e.g. `ADD | RECORD`.
    pub code: u32,
    /// A marshalled `DBMesg` ([`record_message`]).
    pub message: Vec<u8>,
}

/// The marshalled `DBMesg` telling a client that the record at `path`
/// changed (spec 6.3): a 16-byte header (send-to address, reply-to 0, code,
/// item count 2, offset of the last item's length word), then the client's
/// file id and the path, each followed by its u16 length. The path is in
/// the in-memory key form: total length first, then `[u8 len][segment]`s.
pub fn record_message(address: u32, code: u32, client_file: i32, path: &[Vec<u8>]) -> Vec<u8> {
    let segments = encode_path(path);
    let mut key = vec![segments.len() as u8];
    key.extend(segments);
    let items: [&[u8]; 2] = [&client_file.to_be_bytes(), &key];
    let mut body = Vec::new();
    let mut last_length_word = 0;
    for item in items {
        body.extend_from_slice(item);
        last_length_word = 16 + body.len();
        body.extend((item.len() as u16).to_be_bytes());
    }
    let mut out = Vec::with_capacity(16 + body.len());
    out.extend(address.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out.extend(code.to_be_bytes());
    out.extend((items.len() as u16).to_be_bytes());
    out.extend((last_length_word as u16).to_be_bytes());
    out.extend(body);
    out
}

/// The `backchannel` of a Perform reply (or the `parms` of a `Notify` call)
/// carrying notifications: `ff 00`, then per message `[u32 code][u16 len]
/// [message]` (spec 6.4).
pub fn notification_backchannel(notifications: &[Notification]) -> Vec<u8> {
    let mut out = vec![0xff, 0x00];
    for n in notifications {
        out.extend(n.code.to_be_bytes());
        out.extend((n.message.len() as u16).to_be_bytes());
        out.extend(&n.message);
    }
    out
}

/// Parse a notification backchannel (the client side); `None` when it is not
/// one (e.g. the `fe` CPU report).
pub fn parse_notification_backchannel(backchannel: &[u8]) -> Option<Vec<Notification>> {
    let mut rest = backchannel.strip_prefix(&[0xff, 0x00])?;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let code = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
        let len = usize::from(u16::from_be_bytes(rest.get(4..6)?.try_into().ok()?));
        let message = rest.get(6..6 + len)?.to_vec();
        out.push(Notification { code, message });
        rest = &rest[6 + len..];
    }
    Some(out)
}

pub(crate) fn finished(r: &Reader<'_>) -> Result<(), PerformError> {
    if r.remaining() == 0 {
        Ok(())
    } else {
        Err(PerformError::Truncated)
    }
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

    fn table() -> KeyPath {
        vec![vec![0x80, 0x02]]
    }

    // Vectors from the live write runs against test project
    // (`re/work/write/vectors/live-*`, write-path.md 10).

    #[test]
    fn create_record() {
        let req = hex("47000000 00001001 00000001 00000165 00000000 028002 03 00000001 000101a1");
        let parsed = parse_create_record_request(&req).unwrap();
        assert_eq!(
            parsed,
            CreateRecord {
                table: table(),
                count: 1,
                txn: 0x101a1,
            }
        );
        assert_eq!(build_create_record(0x1001, 0x165, &parsed), req);

        let reply = CreateRecordReply {
            conflict: None,
            status: 0,
            record: vec![vec![0x04]],
        };
        assert_eq!(reply.to_result(), hex("00000000 0104 02"));
        assert_eq!(
            parse_create_record_reply(&reply.to_result()).unwrap(),
            reply
        );
        let missing = CreateRecordReply {
            conflict: None,
            status: 106,
            record: vec![vec![0x00]],
        };
        assert_eq!(missing.to_result(), hex("0000006a 0100 02"));
    }

    #[test]
    fn get_serial() {
        let req = hex(
            "48000000 00001005 00000001 00000165 00000000 028002 03 0101 02 \
             00000000 00000000 00000000",
        );
        let parsed = parse_get_serial_request(&req).unwrap();
        assert_eq!(
            parsed,
            GetSerial {
                table: table(),
                field: vec![vec![0x01]],
                value: String::new(),
                increment: 0,
                flags: 0,
            }
        );
        assert_eq!(build_get_serial(0x1005, 0x165, &parsed), req);
        let reply = GetSerialReply {
            conflict: None,
            status: 0,
            value: "1".into(),
        };
        assert_eq!(reply.to_result(), hex("00000000 6b 00000001"));
        assert_eq!(parse_get_serial_reply(&reply.to_result()).unwrap(), reply);
    }

    #[test]
    fn upload_and_commit() {
        // cap-op14-upload-new's form: record 83 4e, field 06 = a six-digit
        // serial ("698047" here); no leading 80, the last 40 closes the
        // destination.
        let upload = Upload {
            payload: hex("28 834e 80 04 06 6c63626a6e6d 40 40"),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: UPLOAD_FLAGS,
            dest: vec![vec![0x80, 0x02], vec![0x05]],
        };
        let txn = build_upload(7, 0x165, &upload);
        assert!(txn.ends_with(&hex("ffff 00 0000 82 028002 0105 05")));
        assert_eq!(parse_upload_request(&txn).unwrap(), upload);

        // live-op70-commit-delete: [80 02, 05, 04] = 03.
        let commit = Commit {
            list: hex("80 28 8002 80 20 05 80 01 04 03 40 40 40"),
            resume: Resume::start(),
            flags: COMMIT_COMPLETE_BEFORE_RETURNING,
            txn: 0x10203,
        };
        let txn = build_commit(8, 0x165, &commit);
        assert!(txn.ends_with(&hex("ffff 00 01 00010203")));
        assert_eq!(parse_commit_request(&txn).unwrap(), commit);

        // The client builds both lists from records and entries.
        let record = OwnedNode::list(
            vec![0x83, 0x4e],
            vec![OwnedNode::leaf(vec![0x06], hex("6c63626a6e6d"))],
        );
        assert_eq!(upload_payload(vec![record]).unwrap(), upload.payload);
        let deleted = OwnedNode::leaf(vec![0x04], vec![COMMIT_DELETED]);
        assert_eq!(
            commit_list(&[0x80, 0x02], vec![deleted]).unwrap(),
            commit.list
        );
        // cap-op70-commit-new and cap-op70-commit-modify (six changed fields).
        let new = OwnedNode::leaf(vec![0x83, 0x4e], vec![COMMIT_NEW]);
        assert_eq!(
            commit_list(&[0x80, 0x02], vec![new]).unwrap(),
            hex("80 28 8002 80 20 05 80 09 834e 01 40 40 40")
        );
        let changed = [
            &[0x04][..],
            &[0x81, 0x1c],
            &[0x81, 0x45],
            &[0x81, 0xed],
            &[0x82, 0x55],
            &[0x82, 0x56],
        ]
        .iter()
        .map(|field| OwnedNode::leaf(field.to_vec(), Vec::new()))
        .collect();
        let modified = OwnedNode::list(vec![0xc4, 0x19, 0x51], changed);
        assert_eq!(
            commit_list(&[0x80, 0x01], vec![modified]).unwrap(),
            hex(
                "80 28 8001 80 20 05 80 30 c41951 80 00 04 08 811c 08 8145 08 81ed 08 8255 \
                 08 8256 40 40 40 40"
            )
        );

        // The empty page every op14 and op70 is answered with.
        let reply = upload_reply(&Resume::no_stream(), usize::MAX);
        assert_eq!(
            reply,
            hex("00000000 0000000000000000 0000000000000000 ffff 00")
        );
        assert_eq!(parse_upload_reply(&reply).unwrap(), Resume::no_stream());
    }

    #[test]
    fn transact_info() {
        let req = hex("67000000 0000100a 00000001 00000165 00000000 00000165 00000001 000101a1");
        let parsed = parse_transact_info_request(&req).unwrap();
        assert_eq!(
            parsed,
            TransactInfo {
                files: vec![0x165],
                txn: 0x101a1,
            }
        );
        assert_eq!(build_transact_info(0x100a, 0x165, &parsed), req);
        let two = TransactInfo {
            files: vec![0x108, 0x84],
            txn: 0x3c,
        };
        assert_eq!(
            parse_transact_info_request(&build_transact_info(1, 0x84, &two)).unwrap(),
            two
        );
    }

    #[test]
    fn lock_unlock_and_kill_all() {
        // live-op60-conflict.
        let req = hex(
            "3c000000 00001003 00000001 00000165 00000000 00000002 0000b101 00000000 01 \
             02800201050106 07 00000165 00000165 00020251 0009",
        );
        let parsed = parse_lock_request(&req).unwrap();
        assert_eq!(
            parsed,
            Lock {
                waiter: 0x2_0000_b101,
                timeout: 0,
                want_mod_count: true,
                path: vec![vec![0x80, 0x02], vec![0x05], vec![0x06]],
                argument: 0x165,
                host_file: 0x165,
                txn: 0x20251,
                lock_type: LOCK_WRITE,
            }
        );
        assert_eq!(build_lock(0x1003, 0x165, &parsed), req);
        let refused = LockReply::Refused {
            status: LOCK_CONFLICT,
            conflict: Conflict {
                self_conflict: false,
                users: "openmaker (admin)".into(),
            },
        };
        assert_eq!(
            refused.to_result().unwrap(),
            hex("00 352a3f34373b313f287a723b3e37333473 00000011 1f43")
        );
        assert_eq!(
            parse_lock_reply(&refused.to_result().unwrap(), true).unwrap(),
            refused
        );
        let granted = LockReply::Granted {
            mod_count: Some(vec![0x01, 0x00]),
        };
        assert_eq!(granted.to_result().unwrap(), hex("0100 02 0000"));
        assert_eq!(
            parse_lock_reply(&granted.to_result().unwrap(), true).unwrap(),
            granted
        );
        let missing = LockReply::Granted {
            mod_count: Some(Vec::new()),
        };
        assert_eq!(missing.to_result().unwrap(), hex("00 0000"));
        assert_eq!(LockReply::MustWait.to_result().unwrap(), hex("1f41"));
        // A count the one-byte length cannot frame (one read from a file)
        // is an error, not a panic; 255 bytes still fit.
        let long = |len| LockReply::Granted {
            mod_count: Some(vec![0x55; len]),
        };
        assert_eq!(long(256).to_result(), Err(TooLong { len: 256 }));
        assert_eq!(long(255).to_result().unwrap().len(), 255 + 1 + 2);
        assert_eq!(
            parse_lock_reply(&hex("1f41"), true).unwrap(),
            LockReply::MustWait
        );

        // live-op62-unlock-record and its header echo.
        let req = hex(
            "3e000000 00001004 00000001 00000165 00000000 00000000 00000000 \
             02800201050101 07 00000165 00010a01 0009",
        );
        let parsed = parse_unlock_request(&req).unwrap();
        assert_eq!(parsed.path, vec![vec![0x80, 0x02], vec![0x05], vec![0x01]]);
        assert_eq!((parsed.txn, parsed.lock_type), (0x10a01, LOCK_WRITE));
        assert_eq!(build_unlock(0x1004, 0x165, &parsed), req);
        let header = Header::parse(&req).unwrap();
        assert_eq!(echo_header(&header), req[..20].to_vec());

        // cap:882, FileMaker Pro closing host file 0x96.
        let close = hex(
            "3e000000 00000025 00000001 00000096 00000000 00000000 00002000 00 00000096 \
             00000000 0005",
        );
        let parsed = parse_unlock_request(&close).unwrap();
        assert_eq!(parsed, Unlock::close_file(0x96));
        assert!(parsed.closes_file());
        assert_eq!(build_unlock(0x25, 0x96, &parsed), close);
        assert!(!parse_unlock_request(&req).unwrap().closes_file());
        // The same root unlock with another attention does not close.
        let other = Unlock {
            attention_mask: 0,
            ..parsed
        };
        assert!(!other.closes_file());

        // cap-op65-kill-all-locks shape.
        let kill = KillAllLocks {
            host_file: 0x84,
            txn: 0x3a,
            all: false,
        };
        let req = build_kill_all_locks(0x26, 0x84, &kill);
        assert!(req.ends_with(&hex("00000084 0000003a 00")));
        assert_eq!(parse_kill_all_locks_request(&req).unwrap(), kill);
    }

    #[test]
    fn notification_matches_the_live_piggyback() {
        // live-notify-piggyback-modify: record [80 02, 05, 04] modified.
        let message = record_message(
            1,
            attention::MODIFY | attention::RECORD,
            -4,
            &[vec![0x80, 0x02], vec![0x05], vec![0x04]],
        );
        let backchannel = notification_backchannel(&[Notification {
            code: 0x201,
            message,
        }]);
        assert_eq!(
            backchannel,
            hex(
                "ff00 00000201 0020 00000001 00000000 00000201 0002 001e fffffffc 0004 \
                 0702800201050104 0008"
            )
        );
        let parsed = parse_notification_backchannel(&backchannel).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].code, 0x201);
        assert_eq!(parse_notification_backchannel(&[0xfe, 0x22]), None);
    }
}
