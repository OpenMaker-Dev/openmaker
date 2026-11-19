//! Container fields (spec `docs/re/containers.md`): op88 stores uploaded
//! binary data in a library, op87 adjusts a member's reference count, op96
//! serves a member's image scaled to a box. Record commits need nothing
//! container-specific: the field holds the member key as text (4.1).
//!
//! Locks are taken in the order of `writes`: the store, then the file's
//! thumbnail cache.

use std::sync::Arc;

use log::{debug, info, warn};
use om_protocol::container::{
    AdjustReferenceCount, DownloadThumbnail, ThumbnailReply, UploadBinaryReply, COUNT_FAILED,
    LIBRARY_OBJECT_EXISTS, THUMBNAIL_MISSING, THUMBNAIL_MORE,
};
use om_protocol::write::KeyPath;
use om_protocol::Resume;

use crate::hosting::Hosted;
use crate::library::{self, BinaryData, Stored, LIBRARY};
use crate::store::Store;
use crate::thumbnail::{self, Key, MAX_SIDE};

/// FileMaker's error for a missing table.
const TABLE_MISSING: u16 = 106;
/// The status for data that is not a valid binary-data tree (FileMaker
/// error 3, "command is unavailable"; OpenMaker's choice, with the reason
/// in the reply's error text).
const BAD_DATA: u16 = 3;

/// The library a table key names: `[T, 1f]` for a table of the file, the
/// file's own `[1f]` for an empty key.
fn library_path(store: &Store, table: Option<&[u8]>) -> Option<Vec<Vec<u8>>> {
    match table {
        None => Some(vec![vec![LIBRARY]]),
        Some(t) if store.has_table(t) => Some(vec![t.to_vec(), vec![LIBRARY]]),
        Some(_) => None,
    }
}

/// A table key as one optional segment; `None` when it has several.
fn table_segment(table: &KeyPath) -> Option<Option<&[u8]>> {
    match table.as_slice() {
        [] => Some(None),
        [t] => Some(Some(t)),
        _ => None,
    }
}

fn segments(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// A finished op88 reply.
pub fn upload_reply(status: u16, member: KeyPath, error: String) -> UploadBinaryReply {
    UploadBinaryReply {
        resume: Resume::no_stream(),
        error,
        member,
        status,
    }
}

/// op88: store an uploaded binary-data tree in `table`'s library,
/// deduplicated by MD5 (containers.md 3.4), the tree read within `limit`
/// bytes of memory (`BinaryData::parse_within`). `None` when the tree
/// would take more than `limit` and `limit` is less than the payload's own
/// limit (`library::tree_limit`): the host's limit cut it short, so the host
/// refuses the request; past the payload's own limit it is bad data.
pub fn upload_binary(
    hosted: &Hosted,
    table: &KeyPath,
    payload: &[u8],
    limit: usize,
    unique_id: impl FnMut() -> [u8; 16],
) -> Option<UploadBinaryReply> {
    let data = match BinaryData::parse_within(payload, limit) {
        Some(Ok(data)) => data,
        None if limit < library::tree_limit(payload.len()) => return None,
        failed => {
            let e = match failed {
                Some(Err(e)) => e,
                _ => BinaryData::TOO_LARGE.to_string(),
            };
            warn!("{:?}: refusing container data: {e}", hosted.name);
            return Some(upload_reply(BAD_DATA, Vec::new(), e));
        }
    };
    Some(store_upload(hosted, table, payload.len(), data, unique_id))
}

/// The rest of op88 for checked binary data of `bytes` bytes.
fn store_upload(
    hosted: &Hosted,
    table: &KeyPath,
    bytes: usize,
    data: BinaryData,
    unique_id: impl FnMut() -> [u8; 16],
) -> UploadBinaryReply {
    let mut store = hosted.write_store();
    let Some(path) = table_segment(table).and_then(|t| library_path(&store, t)) else {
        return upload_reply(TABLE_MISSING, Vec::new(), String::new());
    };
    match store.store_binary(&segments(&path), data, unique_id) {
        None => upload_reply(
            BAD_DATA,
            Vec::new(),
            "the file cannot hold it there".to_string(),
        ),
        Some(Stored::New(member)) => {
            info!(
                "{:?}: stored container data ({bytes} bytes) as member {member:02x?} of {path:02x?}",
                hosted.name
            );
            upload_reply(0, vec![member], String::new())
        }
        Some(Stored::Existing(member)) => {
            info!(
                "{:?}: container data already stored as member {member:02x?} of {path:02x?}",
                hosted.name
            );
            upload_reply(LIBRARY_OBJECT_EXISTS, vec![member], String::new())
        }
    }
}

/// op87: add a delta to a member's reference count, deleting the member at
/// 0 (containers.md 5.3). Returns the new count or [`COUNT_FAILED`], and
/// changes nothing unless the path names an existing member.
pub fn adjust_reference_count(hosted: &Hosted, req: &AdjustReferenceCount) -> u32 {
    let Some((table, member)) = library::member_path(&req.path) else {
        warn!(
            "{:?}: op87 for {:02x?}, which is not a library member",
            hosted.name, req.path
        );
        return COUNT_FAILED;
    };
    let mut store = hosted.write_store();
    let Some(path) = library_path(&store, table) else {
        return COUNT_FAILED;
    };
    match store.adjust_reference_count(&segments(&path), member, req.delta) {
        Some(count) => {
            if count == 0 {
                info!(
                    "{:?}: deleted container member {member:02x?} of {path:02x?}",
                    hosted.name
                );
            } else {
                debug!(
                    "{:?}: member {member:02x?} of {path:02x?}: count {count}",
                    hosted.name
                );
            }
            count
        }
        None => {
            warn!(
                "{:?}: op87 for missing member {member:02x?} of {path:02x?}",
                hosted.name
            );
            COUNT_FAILED
        }
    }
}

/// op96: one chunk of a member's image scaled to the requested box
/// (containers.md 6.3), at most `max_bytes` long. Thumbnails are returned
/// unencrypted (return flags 0).
pub fn thumbnail(hosted: &Hosted, req: &DownloadThumbnail, max_bytes: usize) -> ThumbnailReply {
    let missing = ThumbnailReply::Status(THUMBNAIL_MISSING);
    if !(1..=MAX_SIDE).contains(&req.width) || !(1..=MAX_SIDE).contains(&req.height) {
        return missing;
    }
    let [member] = req.member.as_slice() else {
        return missing;
    };
    let (key, cached, image) = {
        let store = hosted.read_store();
        let Some(path) = table_segment(&req.table).and_then(|t| library_path(&store, t)) else {
            return missing;
        };
        let Some(node) = store.library_member(&segments(&path), member) else {
            return missing;
        };
        let key = Key {
            library: path,
            member: member.clone(),
            width: req.width,
            height: req.height,
        };
        let cached = hosted.thumbnails.lock().expect("thumbnail lock").get(&key);
        let image = cached.is_none().then(|| thumbnail::master(node)).flatten();
        (key, cached, image)
    };
    // Rendering happens outside the store lock.
    let rendered = match (cached, image) {
        (Some(cached), _) => cached,
        (None, Some((format, bytes))) => {
            match thumbnail::render(format, &bytes, req.width, req.height) {
                Ok(rendered) => {
                    let rendered = Arc::new(rendered);
                    hosted
                        .thumbnails
                        .lock()
                        .expect("thumbnail lock")
                        .insert(key, rendered.clone());
                    rendered
                }
                Err(e) => {
                    warn!(
                        "{:?}: no thumbnail of member {member:02x?}: {e}",
                        hosted.name
                    );
                    return missing;
                }
            }
        }
        // Not an image the server can scale.
        (None, None) => return missing,
    };
    let total = rendered.bytes.len();
    let Some(start) = usize::try_from(req.offset).ok().filter(|&s| s <= total) else {
        return missing;
    };
    let chunk = (req.max_chunk.saturating_sub(4) as usize).clamp(1, max_bytes.max(1));
    let end = start.saturating_add(chunk).min(total);
    ThumbnailReply::Chunk {
        return_flags: 0,
        bytes: rendered.bytes[start..end].to_vec(),
        height: rendered.height,
        width: rendered.width,
        total: total as u64,
        status: if end < total { THUMBNAIL_MORE } else { 0 },
    }
}
