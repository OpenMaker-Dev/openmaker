//! The upload/download family of transactions: Download With Lock (op90)
//! fetches one key path of the open file's HBAM tree; Download List (op92)
//! fetches selected children of a key path. Both answer with packed slots
//! (spec `docs/re/network-protocol.md` 5.6.6, 5.6.6a, 5.6.6b, 5.6.6c).
//!
//! Requests and replies share one framing (`RCUploadDownload`), in push order:
//! `[payload N][u32 N][resume block][op addition]`. The payload is the op92 key
//! list (empty for op90); in a reply it is a page of slot bytes. The
//! [`Resume`] block pages a transfer too large for one reply: a nonzero cursor
//! in a reply means "call again", echoing the block, until it comes back zero.

use om_rctransaction::{Builder, Header, Pop, Reader, ResultReader};

use crate::{Opcode, PerformError, FILE_SESSION_ID};

/// `maxreturn` the official client sends with every op90 request.
pub const DOWNLOAD_MAX_RETURN: u32 = 0x0010_012c;

/// `maxreturn` the official client sends with every op92 request.
pub const DOWNLOAD_LIST_MAX_RETURN: u32 = 0x0010_012b;

/// The most slot bytes one reply carries, and the most payload bytes one
/// request uploads (spec 5.6.6c).
pub const PAGE_LIMIT: usize = 0xF_FF00;

/// A key path as the upload/download ops frame it: each segment as
/// `[u8 len][bytes]` (the length of the whole follows separately).
///
/// # Panics
/// If a segment or the whole path is longer than 255 bytes, which the one-byte
/// lengths cannot frame.
pub(crate) fn encode_path(key_path: &[impl AsRef<[u8]>]) -> Vec<u8> {
    let mut path = Vec::new();
    for segment in key_path {
        let segment = segment.as_ref();
        let len = u8::try_from(segment.len()).expect("key segment fits in a u8 length");
        path.push(len);
        path.extend_from_slice(segment);
    }
    assert!(path.len() <= 255, "key path fits in a u8 length");
    path
}

/// Split an encoded key path back into segments.
pub(crate) fn decode_path(mut encoded: &[u8]) -> Result<Vec<Vec<u8>>, PerformError> {
    let mut path = Vec::new();
    while let Some((&len, rest)) = encoded.split_first() {
        let len = usize::from(len);
        if rest.len() < len {
            return Err(PerformError::Truncated);
        }
        path.push(rest[..len].to_vec());
        encoded = &rest[len..];
    }
    Ok(path)
}

/// A file-scoped header for `opcode`.
pub(crate) fn file_header(opcode: Opcode, transaction_id: u32, file_id: u32) -> Header {
    let mut header = Header::new(opcode as u8);
    header.transid_lo = transaction_id;
    header.session_id = FILE_SESSION_ID;
    header.file_id = file_id;
    header
}

/// The resume block that pages a transfer (spec 5.6.6c).
///
/// The handles are server-private: a client echoes them unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// Handle of the server's source position; 0 when nothing is retained.
    pub perform: u64,
    /// Handle of the server's transfer cursor. Nonzero in a reply means more
    /// follows: call again with this block.
    pub cursor: u64,
    /// In a request: -1 to start a transfer or continue an upload, >= 0 to
    /// continue a download. In a reply: -1 = no slot stream (a missing or
    /// terminal key, or an upload continues), 0 = complete, > 0 = the number
    /// of lists open at the page cut.
    pub depth: i16,
    /// Empty, except while uploading: the key path (relative to the transfer
    /// root) where the server stopped unpacking.
    pub copy_key: Vec<Vec<u8>>,
}

impl Resume {
    /// The block of a first request.
    pub fn start() -> Self {
        Self {
            perform: 0,
            cursor: 0,
            depth: -1,
            copy_key: Vec::new(),
        }
    }

    /// The block of a reply that completes a stream.
    pub fn complete() -> Self {
        Self {
            depth: 0,
            ..Self::start()
        }
    }

    /// The block of a reply that carries no slot stream.
    pub fn no_stream() -> Self {
        Self::start()
    }

    /// Whether the server holds more of this transfer.
    pub fn has_more(&self) -> bool {
        self.cursor != 0
    }

    /// The fields in push order: perform, cursor, depth, copy key, its length.
    pub(crate) fn fields(&self) -> [Vec<u8>; 5] {
        let copy_key = encode_path(&self.copy_key);
        let len = copy_key.len() as u8;
        [
            self.perform.to_be_bytes().to_vec(),
            self.cursor.to_be_bytes().to_vec(),
            self.depth.to_be_bytes().to_vec(),
            copy_key,
            vec![len],
        ]
    }

    pub(crate) fn pop<'a>(r: &mut impl Pop<'a>) -> Result<Self, PerformError> {
        let copy_key = decode_path(r.pop_short_bytes()?)?;
        let depth = r.pop_u16()? as i16;
        let cursor = r.pop_u64()?;
        let perform = r.pop_u64()?;
        Ok(Self {
            perform,
            cursor,
            depth,
            copy_key,
        })
    }
}

/// Push `fields` onto `out` in order, each only if it still fits in
/// `max_return` bytes. This is how FileMaker Server writes a reply into a
/// buffer sized to the client's `maxreturn`: no room is reserved for the
/// trailer, so a field that does not fit is dropped (spec 5.6.6c).
pub(crate) fn push_within(out: &mut Vec<u8>, fields: &[Vec<u8>], max_return: usize) {
    for field in fields {
        if out.len() + field.len() <= max_return {
            out.extend_from_slice(field);
        }
    }
}

/// The shared request framing up to the resume block: payload, `N`, block.
pub(crate) fn push_request_base(header: Header, payload: &[u8], resume: &Resume) -> Builder {
    let payload_len = u32::try_from(payload.len()).expect("payload under 4 GiB");
    let mut builder = Builder::with_header(header)
        .push_raw(payload)
        .push_u32(payload_len);
    for field in resume.fields() {
        builder = builder.push_raw(&field);
    }
    builder
}

/// Everything needed to build one Download transaction.
#[derive(Clone, Debug)]
pub struct DownloadRequest<'a> {
    /// The connection's next file-transaction id (header transid-low).
    pub transaction_id: u32,
    /// The opened file's server id (`OpenReply::file_id`).
    pub file_id: u32,
    /// Key path to download, one byte string per segment (e.g. `[[2]]`).
    pub key_path: &'a [&'a [u8]],
    /// [`Resume::start`] for the first request; the previous reply's block,
    /// echoed, for each continuation.
    pub resume: Resume,
}

/// Build the Download With Lock (op90) transaction.
///
/// # Panics
/// If a key segment or the encoded path is longer than 255 bytes, which cannot
/// be framed with the one-byte lengths op90 uses.
pub fn build_download(req: &DownloadRequest<'_>) -> Vec<u8> {
    build_download_op(Opcode::DownloadWithLock, req)
}

/// Build a Download From Host (op15) transaction: op90's request, which the
/// server answers without taking a lock (schema-path.md 3.2). The server
/// parses it with [`parse_download_request`].
pub fn build_download_from_host(req: &DownloadRequest<'_>) -> Vec<u8> {
    build_download_op(Opcode::DownloadFromHost, req)
}

fn build_download_op(opcode: Opcode, req: &DownloadRequest<'_>) -> Vec<u8> {
    let header = file_header(opcode, req.transaction_id, req.file_id);
    let path = encode_path(req.key_path);
    push_request_base(header, &[], &req.resume)
        .push_raw(&path)
        .push_u8(path.len() as u8)
        .finish()
}

/// A parsed Download With Lock (op90) request (the server side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedDownload {
    pub key_path: Vec<Vec<u8>>,
    pub resume: Resume,
}

/// Parse a Download With Lock (op90) transaction (the server side of
/// [`build_download`]).
pub fn parse_download_request(transaction: &[u8]) -> Result<ReceivedDownload, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let key_path = decode_path(r.pop_short_bytes()?)?;
    let resume = Resume::pop(&mut r)?;
    let payload_len = r.pop_u32()? as usize;
    if payload_len != 0 || r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(ReceivedDownload { key_path, resume })
}

/// What an op90 target key is (`HBPosition::NodeState`, spec 5.6.6c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    /// No such key: no slot stream, no value.
    Missing,
    /// The key holds a value, returned inline instead of a slot stream.
    Terminal,
    /// The key is a node: its subtree comes back as the slot stream.
    NonTerminal,
    /// A state byte not seen in traffic.
    Other(u8),
}

impl NodeState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Missing,
            1 => Self::Terminal,
            2 => Self::NonTerminal,
            other => Self::Other(other),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Missing => 0,
            Self::Terminal => 1,
            Self::NonTerminal => 2,
            Self::Other(v) => v,
        }
    }
}

/// One Download (op90) reply: a page of the target's slot stream, or its
/// inline value, with the resume block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadReply {
    /// The server's lock status; 0 = downloaded and locked as requested.
    pub lock_error: u16,
    /// Packed HBAM slots relative to the requested path (decode with
    /// `om_hbam`). Concatenate the pages of a paged transfer.
    pub slots: Vec<u8>,
    pub resume: Resume,
    /// The key's value when it is [`NodeState::Terminal`].
    pub value: Vec<u8>,
    pub node_state: NodeState,
}

impl DownloadReply {
    /// A reply carrying a node's slot stream (or one page of it).
    pub fn node(slots: Vec<u8>, resume: Resume) -> Self {
        Self {
            lock_error: 0,
            slots,
            resume,
            value: Vec::new(),
            node_state: NodeState::NonTerminal,
        }
    }

    /// A reply for a key that holds a value rather than a list.
    pub fn terminal(value: Vec<u8>) -> Self {
        Self {
            lock_error: 0,
            slots: Vec::new(),
            resume: Resume::no_stream(),
            value,
            node_state: NodeState::Terminal,
        }
    }

    /// A reply for a key the file does not have.
    pub fn missing() -> Self {
        Self {
            lock_error: 0,
            slots: Vec::new(),
            resume: Resume::no_stream(),
            value: Vec::new(),
            node_state: NodeState::Missing,
        }
    }

    /// The same reply with `lock_error` set.
    pub fn with_lock_error(self, lock_error: u16) -> Self {
        Self { lock_error, ..self }
    }

    /// The result stack a server sends (spec 5.6.6c):
    /// `[slots N][u32 N][resume block][value V][i16 V][u8 node state][u16 lockError]`.
    pub fn to_result(&self) -> Vec<u8> {
        self.to_result_within(usize::MAX)
    }

    /// [`DownloadReply::to_result`] in a buffer of `max_return` bytes, dropping
    /// trailer fields that do not fit as FileMaker Server does. The slot bytes
    /// must already fit (the server cuts pages at whole chunks).
    ///
    /// # Panics
    /// If the slots or value are 4 GiB or longer, or the value 32 KiB or longer.
    pub fn to_result_within(&self, max_return: usize) -> Vec<u8> {
        self.result_within(max_return, true)
    }

    /// The op15 Download From Host result stack: op90's without the trailing
    /// `u16 lockError` (schema-path.md 3.2), in a buffer of `max_return`
    /// bytes as [`DownloadReply::to_result_within`].
    pub fn to_unlocked_result_within(&self, max_return: usize) -> Vec<u8> {
        self.result_within(max_return, false)
    }

    fn result_within(&self, max_return: usize, with_lock_error: bool) -> Vec<u8> {
        let n = u32::try_from(self.slots.len()).expect("slot data under 4 GiB");
        let v = i16::try_from(self.value.len()).expect("inline value under 32 KiB");
        let [perform, cursor, depth, copy_key, copy_key_len] = self.resume.fields();
        let mut out = self.slots.clone();
        let mut fields = vec![
            n.to_be_bytes().to_vec(),
            perform,
            cursor,
            depth,
            copy_key,
            copy_key_len,
            self.value.clone(),
            v.to_be_bytes().to_vec(),
            vec![self.node_state.to_u8()],
        ];
        if with_lock_error {
            fields.push(self.lock_error.to_be_bytes().to_vec());
        }
        push_within(&mut out, &fields, max_return);
        out
    }
}

/// Parse a Download (op90) result stack (see [`DownloadReply::to_result`]).
pub fn parse_download_reply(result: &[u8]) -> Result<DownloadReply, PerformError> {
    let mut r = ResultReader::new(result);
    let lock_error = r.pop_u16()?;
    pop_download_reply(r, lock_error)
}

/// Parse an op15 Download From Host result stack (the client side): op90's
/// without the lock error, which reads as 0.
pub fn parse_download_from_host_reply(result: &[u8]) -> Result<DownloadReply, PerformError> {
    pop_download_reply(ResultReader::new(result), 0)
}

fn pop_download_reply(
    mut r: ResultReader<'_>,
    lock_error: u16,
) -> Result<DownloadReply, PerformError> {
    let node_state = NodeState::from_u8(r.pop_u8()?);
    let value_len = usize::try_from(r.pop_u16()? as i16).map_err(|_| PerformError::Truncated)?;
    let value = Pop::pop_raw(&mut r, value_len)?.to_vec();
    let resume = Resume::pop(&mut r)?;
    let slots = pop_payload(&mut r)?;
    Ok(DownloadReply {
        lock_error,
        slots,
        resume,
        value,
        node_state,
    })
}

/// Pop `u32 N` and require the `N` bytes before it to be all that is left.
pub(crate) fn pop_payload<'a>(r: &mut impl PopRest<'a>) -> Result<Vec<u8>, PerformError> {
    let n = r.pop_u32()? as usize;
    let rest = r.rest();
    if rest.len() != n {
        return Err(PerformError::Truncated);
    }
    Ok(rest.to_vec())
}

/// A [`Pop`] that can also show the unread bytes.
pub(crate) trait PopRest<'a>: Pop<'a> {
    fn rest(&self) -> &'a [u8];
}

impl<'a> PopRest<'a> for ResultReader<'a> {
    fn rest(&self) -> &'a [u8] {
        ResultReader::rest(self)
    }
}

impl<'a> PopRest<'a> for Reader<'a> {
    fn rest(&self) -> &'a [u8] {
        Reader::rest(self)
    }
}

/// Everything needed to build one Download List (op92) transaction.
#[derive(Clone, Debug)]
pub struct DownloadListRequest<'a> {
    /// The connection's next file-transaction id (header transid-low).
    pub transaction_id: u32,
    /// The opened file's server id (`OpenReply::file_id`).
    pub file_id: u32,
    /// The parent key path, e.g. `[[0x80, 0x01], [5]]` for a table's records.
    pub key_path: &'a [&'a [u8]],
    /// The children of `key_path` to fetch, in key order. Sent with the first
    /// request only; continuations carry no payload.
    pub keys: &'a [&'a [u8]],
    /// The last byte of the request: 0 for record lists (a lock per child), 1
    /// for the `[0x1f, 5]` asset lists in capture s03 (one lock on the parent).
    pub flag: u8,
    /// [`Resume::start`] for the first request; the previous reply's block,
    /// echoed, for each continuation.
    pub resume: Resume,
}

/// A parsed Download List (op92) request (the server side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedDownloadList {
    pub key_path: Vec<Vec<u8>>,
    /// The uploaded key list, or the next part of it: a slot list with one
    /// empty leaf per requested child (see [`key_list_keys`]).
    pub payload: Vec<u8>,
    pub flag: u8,
    pub resume: Resume,
}

/// The op92 key list: a bracketed slot list with one empty leaf per key.
///
/// # Panics
/// If a key is longer than 255 bytes.
pub fn encode_key_list(keys: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![om_hbam::slot::BEGIN];
    for key in keys {
        let code =
            om_hbam::slot::minimal_code(false, key.len(), None).expect("key under 256 bytes");
        out.extend(om_hbam::slot::encode_chunk(code, key, &[]).expect("empty leaf encodes"));
    }
    out.push(om_hbam::slot::POP);
    out
}

/// The top-level keys of a complete op92 key list ([`KeyList`]), copied.
pub fn key_list_keys(payload: &[u8]) -> Result<Vec<Vec<u8>>, PerformError> {
    Ok(KeyList::parse(payload)?
        .keys()
        .map(<[u8]>::to_vec)
        .collect())
}

/// A complete op92 key list, checked as reading it into a key tree would
/// check it, whose keys are then read from its bytes again: a tree, or a
/// copy of each key, of a list of short keys takes tens of times the list's
/// size.
#[derive(Clone, Copy, Debug)]
pub struct KeyList<'a> {
    payload: &'a [u8],
}

impl<'a> KeyList<'a> {
    /// Check the list: it fails where `om_hbam::KeyTree::from_slots` fails
    /// (a malformed slot, keys out of order, a key given two values).
    pub fn parse(payload: &'a [u8]) -> Result<Self, PerformError> {
        let mut order = om_hbam::tree::SlotOrder::default();
        for slot in om_hbam::SlotReader::new(payload) {
            let slot = slot.map_err(|_| PerformError::Truncated)?;
            order
                .check(&slot.path, slot.value)
                .map_err(|_| PerformError::Truncated)?;
        }
        Ok(Self { payload })
    }

    /// The top-level keys, in ascending order, each once.
    pub fn keys(&self) -> impl Iterator<Item = &'a [u8]> + 'a {
        let mut last: Option<&[u8]> = None;
        // The list was checked, so every slot reads.
        om_hbam::SlotReader::new(self.payload)
            .map_while(Result::ok)
            .filter_map(move |slot| {
                let key = slot.path.first()?.0;
                (last != Some(key)).then(|| {
                    last = Some(key);
                    key
                })
            })
    }
}

/// Build the Download List (op92) transaction. A continuation (a resume block
/// with a nonzero cursor) carries no key list.
///
/// # Panics
/// If a key, segment or the encoded path is longer than 255 bytes, or the key
/// list is larger than [`PAGE_LIMIT`] (a client must then upload it in parts).
pub fn build_download_list(req: &DownloadListRequest<'_>) -> Vec<u8> {
    let header = file_header(Opcode::DownloadList, req.transaction_id, req.file_id);
    let payload = if req.resume.has_more() {
        Vec::new()
    } else {
        encode_key_list(req.keys)
    };
    assert!(payload.len() <= PAGE_LIMIT, "key list fits in one request");
    let path = encode_path(req.key_path);
    push_request_base(header, &payload, &req.resume)
        .push_raw(&path)
        .push_u8(path.len() as u8)
        .push_u8(req.flag)
        .finish()
}

/// Parse a Download List (op92) transaction (the server side of
/// [`build_download_list`]).
pub fn parse_download_list_request(
    transaction: &[u8],
) -> Result<ReceivedDownloadList, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let flag = r.pop_u8()?;
    let key_path = decode_path(r.pop_short_bytes()?)?;
    let resume = Resume::pop(&mut r)?;
    let payload = pop_payload(&mut r)?;
    Ok(ReceivedDownloadList {
        key_path,
        payload,
        flag,
        resume,
    })
}

/// A Download List (op92) reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadListReply {
    /// 0 = success (the lock status).
    pub status: u16,
    /// The parent node restricted to the requested children that exist, as
    /// packed slots relative to the parent path (or one page of them).
    pub slots: Vec<u8>,
    pub resume: Resume,
}

impl DownloadListReply {
    /// The result stack a server sends:
    /// `[slots N][u32 N][resume block][u16 status]` (spec 5.6.6b, 5.6.6c).
    pub fn to_result(&self) -> Vec<u8> {
        self.to_result_within(usize::MAX)
    }

    /// [`DownloadListReply::to_result`] in a buffer of `max_return` bytes,
    /// dropping trailer fields that do not fit (as for op90).
    ///
    /// # Panics
    /// If the slot data is 4 GiB or longer.
    pub fn to_result_within(&self, max_return: usize) -> Vec<u8> {
        let n = u32::try_from(self.slots.len()).expect("slot data under 4 GiB");
        let [perform, cursor, depth, copy_key, copy_key_len] = self.resume.fields();
        let mut out = self.slots.clone();
        push_within(
            &mut out,
            &[
                n.to_be_bytes().to_vec(),
                perform,
                cursor,
                depth,
                copy_key,
                copy_key_len,
                self.status.to_be_bytes().to_vec(),
            ],
            max_return,
        );
        out
    }
}

/// Parse a Download List (op92) result stack.
pub fn parse_download_list_reply(result: &[u8]) -> Result<DownloadListReply, PerformError> {
    let mut r = ResultReader::new(result);
    let status = r.pop_u16()?;
    let resume = Resume::pop(&mut r)?;
    let slots = pop_payload(&mut r)?;
    Ok(DownloadListReply {
        status,
        slots,
        resume,
    })
}

/// Where the next page of a slot stream ends: the longest run of whole chunks
/// from `start` that fits in `limit` bytes, but always at least one chunk so a
/// transfer cannot stall. Also returns the list depth after the cut (spec
/// 5.6.6c): 1 inside the requested node's list, plus pushes, minus pops;
/// 0 when the stream is complete.
///
/// `depth` is the depth at `start` (0 before the stream's opening `0x80`).
pub fn page_cut(stream: &[u8], start: usize, limit: usize, depth: i16) -> (usize, i16) {
    use om_hbam::slot::{decode_chunk, ChunkKind};
    let mut end = start;
    let mut depth = depth;
    while end < stream.len() {
        let Ok(chunk) = decode_chunk(stream, end) else {
            // Not a chunk stream: send the rest whole.
            return (stream.len(), 0);
        };
        if end > start && end - start + chunk.raw.len() > limit {
            break;
        }
        end += chunk.raw.len();
        match chunk.kind {
            // The stream's opening 0x80 enters the requested node's list;
            // a push's own 0x80 was already counted with the push.
            ChunkKind::Begin if depth == 0 && start == 0 && end == 1 => depth = 1,
            ChunkKind::Begin => {}
            ChunkKind::Push => depth += 1,
            ChunkKind::Pop => depth -= 1,
            ChunkKind::Leaf => {}
        }
    }
    (end, depth)
}

/// Whether an uploaded slot list is complete (spec 5.6.6c): unpacking
/// ignores `0x80`, each push opens a level and `0x40` closes one, and the
/// `0x40` that closes the outermost level completes the upload. That covers
/// both forms clients send: a bracketed list (`80 ... 40`, op92 key lists and
/// op86 programs) and the children of an existing node closed by its `40`
/// (`push R 80 ... 40 40`, op14 record uploads).
pub fn list_complete(bytes: &[u8]) -> bool {
    use om_hbam::slot::{decode_chunk, ChunkKind};
    let mut depth = 0i32;
    let mut pos = 0;
    while pos < bytes.len() {
        let Ok(chunk) = decode_chunk(bytes, pos) else {
            return false;
        };
        pos += chunk.raw.len();
        match chunk.kind {
            ChunkKind::Push => depth += 1,
            ChunkKind::Pop => {
                depth -= 1;
                if depth < 0 {
                    return pos == bytes.len();
                }
            }
            ChunkKind::Begin | ChunkKind::Leaf => {}
        }
    }
    false
}

/// The key path at the end of a partial upload, relative to its root: where
/// the server stopped unpacking (the copy key, spec 5.6.6c). It can be longer
/// than a reply's one-byte copy-key length frames; [`UploadScan::copy_key`]
/// is the form a reply may carry.
pub fn upload_position(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut reader = om_hbam::SlotReader::new(bytes);
    while let Some(Ok(_)) = reader.next() {}
    reader.path().iter().map(|s| s.0.to_vec()).collect()
}

/// [`list_complete`] and [`upload_position`] for a payload that arrives in
/// parts, reading each byte once: a server keeps one with the upload and
/// scans the payload again after appending each part. Rescanning the whole
/// payload for every part would cost time quadratic in its size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UploadScan {
    /// Where the next chunk starts. A chunk cut off by the end of the bytes
    /// so far is decoded again from here when more arrive.
    offset: usize,
    /// The payload length at the last scan.
    len: usize,
    /// Lists open, as [`list_complete`] counts them: pushes minus pops.
    depth: usize,
    /// The end of the `0x40` that closed the outermost list, once read.
    /// Nothing after it is read.
    closed_at: Option<usize>,
    /// A chunk with an unknown code was read: nothing after it ever is.
    broken: bool,
    /// The key path [`upload_position`] reports: a slot reader's stack.
    path: Vec<Vec<u8>>,
    /// The slot reader stopped at a path deeper than
    /// [`om_hbam::slot::MAX_DEPTH`]: `path` no longer changes.
    path_final: bool,
}

impl UploadScan {
    /// Read what `payload` adds to the bytes scanned before. `payload` is
    /// the whole upload so far and must begin with the bytes of the previous
    /// call.
    pub fn scan(&mut self, payload: &[u8]) {
        use om_hbam::slot::{decode_chunk, ChunkKind, MAX_DEPTH};
        debug_assert!(payload.len() >= self.len, "an upload only grows");
        self.len = payload.len();
        if self.closed_at.is_some() || self.broken {
            return;
        }
        while self.offset < payload.len() {
            let chunk = match decode_chunk(payload, self.offset) {
                Ok(chunk) => chunk,
                // The rest of this chunk is in a later part.
                Err(om_hbam::Error::Truncated { .. }) => return,
                Err(_) => {
                    self.broken = true;
                    return;
                }
            };
            self.offset += chunk.raw.len();
            // A key below the reader's depth limit stops its path.
            let too_deep = self.path.len() >= MAX_DEPTH;
            match chunk.kind {
                ChunkKind::Begin => {}
                ChunkKind::Pop if self.depth == 0 => {
                    self.closed_at = Some(self.offset);
                    return;
                }
                ChunkKind::Pop => {
                    self.depth -= 1;
                    if !self.path_final {
                        self.path.pop();
                    }
                }
                ChunkKind::Push => {
                    self.depth += 1;
                    if !self.path_final && too_deep {
                        self.path_final = true;
                    } else if !self.path_final {
                        self.path.push(chunk.key.to_vec());
                    }
                }
                ChunkKind::Leaf => self.path_final |= too_deep,
            }
        }
    }

    /// [`list_complete`] of the payload scanned.
    pub fn complete(&self) -> bool {
        self.closed_at == Some(self.len)
    }

    /// Whether no further bytes can complete the payload: it holds a chunk
    /// with an unknown code, or bytes after the `0x40` that closed it.
    /// [`list_complete`] is false for every continuation of such a payload.
    pub fn never_completes(&self) -> bool {
        self.broken || self.closed_at.is_some_and(|end| end != self.len)
    }

    /// [`upload_position`] of the payload scanned.
    pub fn position(&self) -> &[Vec<u8>] {
        &self.path
    }

    /// The position as the copy key of a reply, or `None` when it is longer
    /// than the copy key's one-byte length can frame (spec 5.6.6c).
    pub fn copy_key(&self) -> Option<Vec<Vec<u8>>> {
        let encoded: usize = self.path.iter().map(|segment| 1 + segment.len()).sum();
        (encoded <= usize::from(u8::MAX)).then(|| self.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first op90 the official client sent after Open in capture s03
    /// (request id 8): transaction id 1, file id 0x84, key path `[2]`.
    const CAPTURED_FIRST_OP90: &[u8] = &[
        0x5a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x84, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x01, 0x02,
        0x02,
    ];

    fn first(key_path: &'static [&'static [u8]]) -> DownloadRequest<'static> {
        DownloadRequest {
            transaction_id: 1,
            file_id: 0x84,
            key_path,
            resume: Resume::start(),
        }
    }

    #[test]
    fn rebuilds_captured_first_download() {
        assert_eq!(build_download(&first(&[&[2]])), CAPTURED_FIRST_OP90);
        assert_eq!(
            parse_download_request(CAPTURED_FIRST_OP90),
            Ok(ReceivedDownload {
                key_path: vec![vec![2]],
                resume: Resume::start(),
            })
        );
    }

    #[test]
    fn multi_segment_path_length_suffix() {
        let txn = build_download(&first(&[&[0x17], &[0x01], &[0x06]]));
        // Captured tail for this path: 00 01 17 01 01 01 06 06.
        assert_eq!(
            &txn[42..],
            &[0x00, 0x01, 0x17, 0x01, 0x01, 0x01, 0x06, 0x06]
        );
    }

    /// The live paged exchange of spec 5.6.6c, request 2: a continuation of
    /// `[3]` with handles from the server and depth 4.
    #[test]
    fn continuation_request_echoes_the_block() {
        let resume = Resume {
            perform: 0x0000_7e0f_1111_1111,
            cursor: 0x0000_7e0f_2222_2222,
            depth: 4,
            copy_key: Vec::new(),
        };
        let txn = build_download(&DownloadRequest {
            transaction_id: 0x0c,
            file_id: 0x166,
            key_path: &[&[3]],
            resume: resume.clone(),
        });
        let mut tail = vec![0, 0, 0, 0];
        tail.extend(resume.perform.to_be_bytes());
        tail.extend(resume.cursor.to_be_bytes());
        tail.extend([0x00, 0x04, 0x00, 0x01, 0x03, 0x02]);
        assert_eq!(&txn[20..], &tail[..]);
        assert_eq!(parse_download_request(&txn).unwrap().resume, resume);
    }

    #[test]
    fn reply_shapes_round_trip() {
        // The 356 data replies of s03: block 00 x20, 00 02.
        let node = DownloadReply::node(vec![0x80, 0x00, 0x01, 0x40], Resume::complete());
        let result = node.to_result();
        assert_eq!(
            &result[4..],
            &{
                let mut t = vec![0, 0, 0, 4];
                t.extend([0; 21]);
                t.extend([0x02, 0x00, 0x00]);
                t
            }[..]
        );
        assert_eq!(parse_download_reply(&result), Ok(node));

        // The two empty replies: 00 x16, ff ff, 00 x4.
        let missing = DownloadReply::missing();
        let result = missing.to_result();
        let mut expected = vec![0; 20];
        expected.extend([0xff, 0xff, 0, 0, 0, 0, 0, 0]);
        assert_eq!(result, expected);
        assert_eq!(parse_download_reply(&result), Ok(missing));

        // Request 568: a terminal value inline, length then state 1.
        let terminal = DownloadReply::terminal(b"<svg/>".to_vec());
        let result = terminal.to_result();
        assert!(result.ends_with(b"<svg/>\x00\x06\x01\x00\x00"));
        assert_eq!(parse_download_reply(&result), Ok(terminal));
    }

    /// FileMaker Server's replies for subtree `[3]` of test project (2690
    /// slot bytes) under small `maxreturn` values, black-box probed live: the
    /// bytes after the slots for each limit from 2690 to 2718.
    #[test]
    fn maxreturn_drops_trailer_fields_that_do_not_fit() {
        let slots = vec![0x40; 2690];
        let reply = DownloadReply::node(slots, Resume::complete());
        // N = 0x0a82, then `zeros` zero bytes, then `tail` (hex).
        let row = |zeros: usize, tail: &str| format!("00000a82{}{tail}", "00".repeat(zeros));
        let observed: Vec<(usize, String)> = vec![
            (2690, String::new()),
            (2691, "00".into()),
            (2692, "0000".into()),
            (2693, "000000".into()),
            (2694, row(0, "")),
            (2695, row(1, "")),
            (2696, row(2, "")),
            (2697, row(3, "")),
            (2698, row(3, "02")),
            (2699, row(5, "")),
            (2700, row(5, "02")),
            (2701, row(5, "02")),
            (2702, row(8, "")),
            (2706, row(11, "02")),
            (2708, row(13, "02")),
            (2709, row(13, "02")),
            (2710, row(16, "")),
            (2714, row(19, "02")),
            (2716, row(21, "02")),
            (2717, row(21, "02")),
            (2718, row(21, "020000")),
        ];
        for (max, after) in observed {
            let result = reply.to_result_within(max);
            let got: String = result[2690..].iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(got, after, "maxreturn {max}");
        }
    }

    #[test]
    fn download_list_round_trips() {
        let request = DownloadListRequest {
            transaction_id: 0xc9,
            file_id: 0x84,
            key_path: &[&[0x80, 0x01], &[5]],
            keys: &[&[1], &[2], &[0x81, 0x9a]],
            flag: 0,
            resume: Resume::start(),
        };
        let txn = build_download_list(&request);
        // Captured tail: key list, its length, the block, path, flag.
        assert!(txn.ends_with(
            &[
                0x80, 0x00, 0x01, 0x00, 0x02, 0x08, 0x81, 0x9a, 0x40, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0x00, 0x02, 0x80, 0x01, 0x01, 0x05, 0x05,
                0x00
            ][..]
        ));
        let received = parse_download_list_request(&txn).unwrap();
        assert_eq!(received.key_path, vec![vec![0x80, 0x01], vec![5]]);
        assert_eq!(
            key_list_keys(&received.payload),
            Ok(vec![vec![1], vec![2], vec![0x81, 0x9a]])
        );
        assert_eq!((received.flag, received.resume), (0, Resume::start()));

        let reply = DownloadListReply {
            status: 0,
            slots: vec![0x80, 0x20, 0x01, 0x80, 0x40, 0x40],
            resume: Resume::complete(),
        };
        let result = reply.to_result();
        assert_eq!(result.len(), 6 + 25);
        assert_eq!(parse_download_list_reply(&result), Ok(reply));
    }

    /// The live split upload of spec 5.6.6c: keys `10 | 11 {01} | fb`, cut
    /// inside child `11`; the server reports copy key `01 11`.
    #[test]
    fn partial_upload_position() {
        let part1 = [0x80, 0x00, 0x10, 0x20, 0x11, 0x80, 0x00, 0x01];
        assert!(!list_complete(&part1));
        assert_eq!(upload_position(&part1), vec![vec![0x11]]);
        let whole = [&part1[..], &[0x40, 0x00, 0xfb, 0x40]].concat();
        assert!(list_complete(&whole));
        // An op14 record upload (cap:920's form, its serial replaced): record
        // 83 4e's fields, then the 40 that closes the destination.
        let record = [
            0x28, 0x83, 0x4e, 0x80, 0x04, 0x06, 0x6c, 0x63, 0x62, 0x6a, 0x6e, 0x6d, 0x40, 0x40,
        ];
        assert!(list_complete(&record));
        assert!(!list_complete(&record[..record.len() - 1]));
        assert_eq!(
            key_list_keys(&whole),
            Ok(vec![vec![0x10], vec![0x11], vec![0xfb]])
        );
        let reply = DownloadListReply {
            status: 0,
            slots: Vec::new(),
            resume: Resume {
                perform: 1,
                cursor: 2,
                depth: -1,
                copy_key: upload_position(&part1),
            },
        };
        assert!(reply
            .to_result()
            .ends_with(&[0xff, 0xff, 0x01, 0x11, 0x02, 0x00, 0x00]));
    }

    /// Scanning an upload part by part answers what scanning it whole does,
    /// wherever the parts are cut: here after every byte, so inside chunks
    /// too.
    #[test]
    fn upload_scan_agrees_at_every_cut() {
        let part1 = vec![0x80, 0x00, 0x10, 0x20, 0x11, 0x80, 0x00, 0x01];
        let whole = [&part1[..], &[0x40, 0x00, 0xfb, 0x40]].concat();
        let record = vec![
            0x28, 0x83, 0x4e, 0x80, 0x04, 0x06, 0x6c, 0x63, 0x62, 0x6a, 0x6e, 0x6d, 0x40, 0x40,
        ];
        let nested = vec![
            0x80, 0x06, 0x01, 0x02, b'a', b'b', 0x20, 0x02, 0x80, 0x06, 0x03, 0x04, b'c', b'd',
            b'e', b'f', 0x40, 0x00, 0x04, 0x40,
        ];
        // A 300-byte value (u16 length) for cuts inside one large chunk.
        let mut long = vec![0x80, 0x07, 0x01, 0x01, 0x2c];
        long.extend([0x5a; 300]);
        long.extend([0x00, 0x02, 0x40]);
        // 18 nested pushes: the slot reader's path stops at 17 levels, the
        // list still closes.
        let mut deep = vec![0x80];
        for key in 1..=18u8 {
            deep.extend([0x20, key, 0x80]);
        }
        deep.extend([0x00, 0x99]);
        deep.extend([0x40; 19]);
        let vectors = [
            whole.clone(),
            record,
            nested,
            long,
            deep,
            // Bytes after the closing 0x40: never complete.
            [&whole[..], &[0x00, 0x01]].concat(),
            // A chunk code outside the grammar.
            vec![0x80, 0x00, 0x01, 0x41, 0x00, 0x02, 0x40],
        ];
        for bytes in &vectors {
            let mut scan = UploadScan::default();
            let mut never = false;
            for end in 0..=bytes.len() {
                let prefix = &bytes[..end];
                scan.scan(prefix);
                assert_eq!(scan.complete(), list_complete(prefix), "{prefix:02x?}");
                assert_eq!(scan.position(), upload_position(prefix), "{prefix:02x?}");
                never |= scan.never_completes();
                assert!(!(never && list_complete(prefix)), "{prefix:02x?}");
            }
        }
        // The live split of spec 5.6.6c reports copy key `01 11`.
        let mut scan = UploadScan::default();
        scan.scan(&part1);
        assert_eq!(scan.copy_key(), Some(vec![vec![0x11]]));
        // A path whose copy key a one-byte length cannot frame has none.
        let mut long_keys = vec![0x80];
        for key in 0..3u8 {
            long_keys.extend([0x38, 100]);
            long_keys.extend([key; 100]);
            long_keys.push(0x80);
        }
        let mut scan = UploadScan::default();
        scan.scan(&long_keys);
        assert_eq!(scan.position().len(), 3);
        assert_eq!(scan.copy_key(), None);
    }

    /// Pages cut at whole chunks, report the open-list depth, and concatenate
    /// back to the stream.
    #[test]
    fn page_cuts_at_chunk_boundaries() {
        // [ L01="ab"  P02 [ L03="cdef" ] L04 ] : a node with a nested list.
        let stream = [
            0x80, 0x06, 0x01, 0x02, b'a', b'b', 0x20, 0x02, 0x80, 0x06, 0x03, 0x04, b'c', b'd',
            b'e', b'f', 0x40, 0x00, 0x04, 0x40,
        ];
        let (end1, depth1) = page_cut(&stream, 0, 9, 0);
        assert_eq!((end1, depth1), (9, 2), "cut after the nested list's 0x80");
        let (end2, depth2) = page_cut(&stream, end1, 8, depth1);
        assert_eq!((end2, depth2), (17, 1), "cut after the nested list closes");
        let (end3, depth3) = page_cut(&stream, end2, 100, depth2);
        assert_eq!((end3, depth3), (20, 0), "complete");
        // A chunk larger than the limit still makes progress.
        assert_eq!(page_cut(&stream, 1, 2, 1), (6, 1));
    }
}
