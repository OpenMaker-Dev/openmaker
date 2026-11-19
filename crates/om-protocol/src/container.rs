//! Container data (spec `docs/re/containers.md`): UploadBinaryData (op88)
//! stores a binary-data tree in a table's library, AdjustReferenceCount
//! (op87) counts its holders, and DownloadThumbnail (op96) returns its image
//! scaled to a box.

use om_rctransaction::{Builder, Pop, Reader, ResultBuilder, ResultReader};

use crate::download::{file_header, pop_payload, push_request_base};
use crate::write::{pop_key, push_key, result_key, KeyPath};
use crate::{Opcode, PerformError, Resume};

/// op88 status: a member with the same MD5 already existed; its count was
/// incremented (`kWarn_LibraryObjectExists`, spec 3.2).
pub const LIBRARY_OBJECT_EXISTS: u16 = 0x1006;
/// op88 status of an intermediate part of a multi-part upload (spec 3.3).
pub const UPLOAD_CONTINUES: u16 = 0xffff;
/// op87 reply meaning failure (spec 5.3).
pub const COUNT_FAILED: u32 = u32::MAX;
/// op96 status: a chunk of the thumbnail, more follow (`kRetryError`).
pub const THUMBNAIL_MORE: u16 = 0x10;
/// op96 status: no such member, or no image in it (`kDataMissing`).
pub const THUMBNAIL_MISSING: u16 = 0x0a;

// ---------------------------------------------------------------------------
// op88 UploadBinaryData (spec 3)

/// An op88 request: one part of a binary-data tree for a table's library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadBinary {
    /// The binary-data tree (spec 2) as a bracketed slot list, or this part
    /// of it.
    pub payload: Vec<u8>,
    pub resume: Resume,
    /// The table whose library receives the data, e.g. `[[0x80, 0x02]]`.
    pub table: KeyPath,
    /// The container field, which selects its storage options.
    pub field: KeyPath,
    /// The evaluated external-storage path, empty for embedded storage.
    pub solved_path: String,
    /// Reserved; FileMaker Pro sends zeros.
    pub reserved: [u8; 16],
}

/// Build an op88 transaction.
pub fn build_upload_binary(transaction_id: u32, file_id: u32, req: &UploadBinary) -> Vec<u8> {
    let header = file_header(Opcode::UploadBinaryData, transaction_id, file_id);
    let b = push_request_base(header, &req.payload, &req.resume);
    let b = push_key(b, &req.table);
    push_key(b, &req.field)
        .push_string(&req.solved_path)
        .push_raw(&req.reserved)
        .finish()
}

/// Parse an op88 transaction (the server side).
pub fn parse_upload_binary_request(transaction: &[u8]) -> Result<UploadBinary, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let reserved = r.pop_raw(16)?.try_into().expect("16 bytes");
    let solved_path = r.pop_string()?;
    let field = pop_key(&mut r)?;
    let table = pop_key(&mut r)?;
    let resume = Resume::pop(&mut r)?;
    let payload = pop_payload(&mut r)?;
    Ok(UploadBinary {
        payload,
        resume,
        table,
        field,
        solved_path,
        reserved,
    })
}

/// An op88 reply: the empty page trailer, then an error text, the member key
/// and the status (spec 3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadBinaryReply {
    pub resume: Resume,
    pub error: String,
    /// The new or existing member, e.g. `[[0x01]]`; empty on failure and for
    /// intermediate parts.
    pub member: KeyPath,
    /// 0 stored, [`LIBRARY_OBJECT_EXISTS`], [`UPLOAD_CONTINUES`], or an
    /// error.
    pub status: u16,
}

impl UploadBinaryReply {
    pub fn to_result(&self) -> Vec<u8> {
        let mut b = ResultBuilder::new().push_u32(0);
        for field in self.resume.fields() {
            b = b.push_raw(&field);
        }
        result_key(b.push_string(&self.error), &self.member)
            .push_u16(self.status)
            .finish()
    }
}

/// Parse an op88 result stack (the client side).
pub fn parse_upload_binary_reply(result: &[u8]) -> Result<UploadBinaryReply, PerformError> {
    let mut r = ResultReader::new(result);
    let status = r.pop_u16()?;
    let member = pop_key(&mut r)?;
    let error = r.pop_string()?;
    let resume = Resume::pop(&mut r)?;
    if r.pop_u32()? != 0 || r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(UploadBinaryReply {
        resume,
        error,
        member,
        status,
    })
}

// ---------------------------------------------------------------------------
// op87 AdjustReferenceCount (spec 5.3)

/// An op87 request: add `delta` to a library member's reference count (0
/// reads it). The reply is the count after the change, a u32.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdjustReferenceCount {
    /// `[T, 1f, 05, M]` for a table library, `[1f, 05, M]` for the file's.
    pub path: KeyPath,
    pub delta: i32,
}

/// Build an op87 transaction.
pub fn build_adjust_reference_count(
    transaction_id: u32,
    file_id: u32,
    req: &AdjustReferenceCount,
) -> Vec<u8> {
    let header = file_header(Opcode::AdjustReferenceCount, transaction_id, file_id);
    push_key(Builder::with_header(header), &req.path)
        .push_u32(req.delta as u32)
        .finish()
}

/// Parse an op87 transaction (the server side).
pub fn parse_adjust_reference_count_request(
    transaction: &[u8],
) -> Result<AdjustReferenceCount, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let delta = r.pop_u32()? as i32;
    let path = pop_key(&mut r)?;
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(AdjustReferenceCount { path, delta })
}

/// The op87 reply: the member's count after the change, or
/// [`COUNT_FAILED`].
pub fn adjust_reference_count_reply(count: u32) -> Vec<u8> {
    ResultBuilder::new().push_u32(count).finish()
}

/// Parse an op87 result stack (the client side).
pub fn parse_adjust_reference_count_reply(result: &[u8]) -> Result<u32, PerformError> {
    let mut r = ResultReader::new(result);
    let count = r.pop_u32()?;
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// op96 DownloadThumbnail (spec 6.3)

/// op96 flag: return the thumbnail decrypted.
pub const THUMBNAIL_DECRYPTED: u8 = 0x08;

/// An op96 request: one chunk of a member's image scaled to a box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadThumbnail {
    /// Reserved; zeros from FileMaker Pro.
    pub reserved: [u32; 4],
    pub height: u32,
    pub width: u32,
    /// The member relative to the table library, e.g. `[[0x01]]`.
    pub member: KeyPath,
    pub table: KeyPath,
    pub flags: u8,
    /// Byte offset of the chunk wanted.
    pub offset: u64,
    /// The largest chunk the client accepts plus 4.
    pub max_chunk: u32,
}

/// Build an op96 transaction.
pub fn build_download_thumbnail(
    transaction_id: u32,
    file_id: u32,
    req: &DownloadThumbnail,
) -> Vec<u8> {
    let header = file_header(Opcode::DownloadThumbnail, transaction_id, file_id);
    let mut b = Builder::with_header(header);
    for reserved in req.reserved {
        b = b.push_u32(reserved);
    }
    let b = push_key(b.push_u32(req.height).push_u32(req.width), &req.member);
    push_key(b, &req.table)
        .push_u8(req.flags)
        .push_u64(req.offset)
        .push_u32(req.max_chunk)
        .finish()
}

/// Parse an op96 transaction (the server side).
pub fn parse_download_thumbnail_request(
    transaction: &[u8],
) -> Result<DownloadThumbnail, PerformError> {
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let max_chunk = r.pop_u32()?;
    let offset = r.pop_u64()?;
    let flags = r.pop_u8()?;
    let table = pop_key(&mut r)?;
    let member = pop_key(&mut r)?;
    let width = r.pop_u32()?;
    let height = r.pop_u32()?;
    let mut reserved = [0u32; 4];
    for slot in reserved.iter_mut().rev() {
        *slot = r.pop_u32()?;
    }
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(DownloadThumbnail {
        reserved,
        height,
        width,
        member,
        table,
        flags,
        offset,
        max_chunk,
    })
}

/// An op96 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailReply {
    /// A chunk of the thumbnail: status 0 for the last, [`THUMBNAIL_MORE`]
    /// when more follow.
    Chunk {
        /// Bit 0: the bytes are encrypted.
        return_flags: u32,
        bytes: Vec<u8>,
        height: u32,
        width: u32,
        /// The whole thumbnail's length.
        total: u64,
        status: u16,
    },
    /// Only a status, e.g. [`THUMBNAIL_MISSING`].
    Status(u16),
}

impl ThumbnailReply {
    pub fn to_result(&self) -> Vec<u8> {
        match self {
            ThumbnailReply::Chunk {
                return_flags,
                bytes,
                height,
                width,
                total,
                status,
            } => ResultBuilder::new()
                .push_u32(*return_flags)
                .push_raw(bytes)
                .push_u32(bytes.len() as u32)
                .push_u32(*height)
                .push_u32(*width)
                .push_u64(*total)
                .push_u16(*status)
                .finish(),
            ThumbnailReply::Status(status) => ResultBuilder::new().push_u16(*status).finish(),
        }
    }
}

/// Parse an op96 result stack (the client side).
pub fn parse_download_thumbnail_reply(result: &[u8]) -> Result<ThumbnailReply, PerformError> {
    let mut r = ResultReader::new(result);
    let status = r.pop_u16()?;
    if r.remaining() == 0 {
        return Ok(ThumbnailReply::Status(status));
    }
    let total = r.pop_u64()?;
    let width = r.pop_u32()?;
    let height = r.pop_u32()?;
    let n = r.pop_u32()? as usize;
    let bytes = r.pop_raw(n)?.to_vec();
    let return_flags = r.pop_u32()?;
    if r.remaining() != 0 {
        return Err(PerformError::Truncated);
    }
    Ok(ThumbnailReply::Chunk {
        return_flags,
        bytes,
        height,
        width,
        total,
        status,
    })
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

    // Live runs against test project (re/work/containers/live, spec 11).

    #[test]
    fn adjust_reference_count() {
        let req = hex("57000000 00001001 00000001 00000165 00000000 \
             028002011f0105010109 ffffffff");
        let parsed = parse_adjust_reference_count_request(&req).unwrap();
        assert_eq!(
            parsed,
            AdjustReferenceCount {
                path: vec![vec![0x80, 0x02], vec![0x1f], vec![0x05], vec![0x01]],
                delta: -1,
            }
        );
        assert_eq!(build_adjust_reference_count(0x1001, 0x165, &parsed), req);
    }

    #[test]
    fn download_thumbnail() {
        // c7/g_thumb_ab: member 05 scaled into 64 x 32, decrypted.
        let req = hex("60000000 00001002 00000001 00000165 00000000 \
             00000000 00000000 00000000 00000000 00000020 00000040 \
             0105 02 028002 03 08 0000000000000000 000fffe6");
        let parsed = parse_download_thumbnail_request(&req).unwrap();
        assert_eq!((parsed.width, parsed.height), (64, 32));
        assert_eq!(parsed.member, vec![vec![0x05]]);
        assert_eq!(parsed.flags, THUMBNAIL_DECRYPTED);
        assert_eq!(parsed.max_chunk, 0x000f_ffe6);
        assert_eq!(build_download_thumbnail(0x1002, 0x165, &parsed), req);

        let chunk = ThumbnailReply::Chunk {
            return_flags: 0,
            bytes: vec![0x89, 0x50, 0x4e, 0x47],
            height: 32,
            width: 64,
            total: 4,
            status: 0,
        };
        assert_eq!(
            chunk.to_result(),
            hex("00000000 89504e47 00000004 00000020 00000040 0000000000000004 0000")
        );
        assert_eq!(
            parse_download_thumbnail_reply(&chunk.to_result()).unwrap(),
            chunk
        );
        let missing = ThumbnailReply::Status(THUMBNAIL_MISSING);
        assert_eq!(missing.to_result(), hex("000a"));
        assert_eq!(
            parse_download_thumbnail_reply(&hex("000a")).unwrap(),
            missing
        );
    }

    #[test]
    fn upload_binary_reply() {
        // live:c1/a_up1-p0 and c2/a_dup-p0 (spec 3.2).
        let stored = UploadBinaryReply {
            resume: Resume::no_stream(),
            error: String::new(),
            member: vec![vec![0x01]],
            status: 0,
        };
        assert_eq!(
            stored.to_result(),
            hex("00000000 0000000000000000 0000000000000000 ffff 00 00000000 0101 02 0000")
        );
        assert_eq!(
            parse_upload_binary_reply(&stored.to_result()).unwrap(),
            stored
        );
        let existing = UploadBinaryReply {
            status: LIBRARY_OBJECT_EXISTS,
            ..stored
        };
        assert!(existing.to_result().ends_with(&hex("0101 02 1006")));
    }
}
