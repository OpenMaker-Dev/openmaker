//! Container fields (spec `docs/re/containers.md`): op88 stores uploaded
//! binary data in a library, op87 adjusts a member's reference count, op96
//! serves a member's image scaled to a box. Record commits need nothing
//! container-specific: the field holds the member key as text (4.1).
//!
//! op88 and op87 change the file as engine operations
//! (`engine::StoreContainerData`, `engine::AdjustContainerCount`): what
//! they do to the tree is [`store_upload`] and [`adjust_reference_count`],
//! run against the engine's transaction. Locks are taken in the order of
//! `writes`: the store, then the file's thumbnail cache.

use std::sync::Arc;

use log::{debug, warn};
use om_protocol::container::{
    AdjustReferenceCount, DownloadThumbnail, ThumbnailReply, UploadBinaryReply, COUNT_FAILED,
    LIBRARY_OBJECT_EXISTS, THUMBNAIL_MISSING, THUMBNAIL_MORE,
};
use om_protocol::write::KeyPath;
use om_protocol::Resume;

use image::ImageFormat;
use om_data::containers::keys::MEMBERS;

use crate::connection::ACCESS_DENIED;
use crate::derived::Derived;
use crate::engine::{Environment, Read, ReadError, StoreContainerData, Transaction, View};
use crate::hosting::Hosted;
use crate::library::{self, BinaryData, Stored, LIBRARY};
use crate::thumbnail::{self, Key, Thumbnail, MAX_SIDE};

/// FileMaker's error for a missing table.
const TABLE_MISSING: u16 = 106;
/// The status for data that is not a valid binary-data tree (FileMaker
/// error 3, "command is unavailable"; OpenMaker's choice, with the reason
/// in the reply's error text).
const BAD_DATA: u16 = 3;

/// The library a table key names: `[T, 1f]` for a table of the file, the
/// file's own `[1f]` for an empty key.
fn library_path(derived: &Derived, table: Option<&[u8]>) -> Option<Vec<Vec<u8>>> {
    match table {
        None => Some(vec![vec![LIBRARY]]),
        Some(t) if derived.has_table(t) => Some(vec![t.to_vec(), vec![LIBRARY]]),
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

/// op88's upload read as a binary-data tree, deduplicated later by MD5
/// (containers.md 3.4), the tree read within `limit` bytes of memory
/// (`BinaryData::parse_within`): the data, or why it is not valid binary
/// data, which the operation answers ([`store_upload`]). `None` when the
/// tree would take more than `limit` and `limit` is less than the
/// payload's own limit (`library::tree_limit`): the host's limit cut it
/// short, so the host refuses the request; past the payload's own limit it
/// is bad data. `file` names the file for the log.
pub fn read_upload(file: &str, payload: &[u8], limit: usize) -> Option<Result<BinaryData, String>> {
    match BinaryData::parse_within(payload, limit) {
        Some(Ok(data)) => Some(Ok(data)),
        None if limit < library::tree_limit(payload.len()) => None,
        failed => {
            let e = match failed {
                Some(Err(e)) => e,
                _ => BinaryData::TOO_LARGE.to_string(),
            };
            warn!("{file:?}: refusing container data: {e}");
            Some(Err(e))
        }
    }
}

/// op88 for data read from an upload ([`read_upload`]), on the file named
/// `file` (for the log): bad data is refused without touching the tree;
/// otherwise the data is stored in the library the table key names, a new
/// member's unique id taken from `environment`.
pub fn store_upload(
    op: StoreContainerData,
    file: &str,
    tx: &mut Transaction<'_>,
    environment: &dyn Environment,
) -> UploadBinaryReply {
    let StoreContainerData { table, data, bytes } = op;
    let data = match data {
        Ok(data) => data,
        Err(e) => return upload_reply(BAD_DATA, Vec::new(), e),
    };
    if tx.is_read_only() {
        return upload_reply(ACCESS_DENIED, Vec::new(), String::new());
    }
    let Some(path) = table_segment(&table).and_then(|t| library_path(&tx.derived(), t)) else {
        return upload_reply(TABLE_MISSING, Vec::new(), String::new());
    };
    let unique_id = || {
        let mut id = [0u8; 16];
        environment.fill_random(&mut id);
        id
    };
    match store_binary(tx, file, &segments(&path), data, unique_id) {
        None => upload_reply(
            BAD_DATA,
            Vec::new(),
            "the file cannot hold it there".to_string(),
        ),
        Some(Stored::New(member)) => {
            tx.once_stored(format!(
                "{file:?}: stored container data ({bytes} bytes) as member {member:02x?} of {path:02x?}"
            ));
            upload_reply(0, vec![member], String::new())
        }
        Some(Stored::Existing(member)) => {
            tx.once_stored(format!(
                "{file:?}: container data already stored as member {member:02x?} of {path:02x?}"
            ));
            upload_reply(LIBRARY_OBJECT_EXISTS, vec![member], String::new())
        }
    }
}

/// op87: add a delta to a member's reference count, deleting the member at
/// 0 (containers.md 5.3), in the file named `file` (for the log). Returns
/// the new count or [`COUNT_FAILED`], and changes nothing unless the path
/// names an existing member.
pub fn adjust_reference_count(
    req: &AdjustReferenceCount,
    file: &str,
    tx: &mut Transaction<'_>,
) -> u32 {
    let Some((table, member)) = library::member_path(&req.path) else {
        warn!(
            "{file:?}: op87 for {:02x?}, which is not a library member",
            req.path
        );
        return COUNT_FAILED;
    };
    if tx.is_read_only() {
        return COUNT_FAILED;
    }
    let Some(path) = library_path(&tx.derived(), table) else {
        return COUNT_FAILED;
    };
    match adjust_count(tx, file, &segments(&path), member, req.delta) {
        Some(count) => {
            if count == 0 {
                tx.once_stored(format!(
                    "{file:?}: deleted container member {member:02x?} of {path:02x?}"
                ));
            } else {
                debug!("{file:?}: member {member:02x?} of {path:02x?}: count {count}");
            }
            count
        }
        None => {
            warn!("{file:?}: op87 for missing member {member:02x?} of {path:02x?}");
            COUNT_FAILED
        }
    }
}

/// op88's store of `data` in the library at `path` (`[T, 1f]` or
/// `[1f]`), creating the library on first use (containers.md 3.4), as the
/// library's rules decide it from what the transaction reads
/// ([`library::store_changes`]) and written through it. A store they
/// refuse ([`library::refusal`]) is logged and changes nothing.
pub(crate) fn store_binary(
    tx: &mut Transaction<'_>,
    file: &str,
    path: &[&[u8]],
    data: BinaryData,
    unique_id: impl FnMut() -> [u8; 16],
) -> Option<Stored> {
    let decided = (|| {
        let view: &dyn View = tx;
        if let Some(reason) = library::refusal(view, path, Some(&data), None)? {
            return Ok(Err(reason));
        }
        let library = library::At { view, path };
        let plan = library::plan(&library, &data.md5)?;
        library::store_changes(&library, data, &plan, unique_id).map(Ok)
    })();
    match decided {
        Ok(Ok(Some((stored, changes)))) => {
            write(tx, path, changes);
            Some(stored)
        }
        // Not reached: the refusal checked the same counters.
        Ok(Ok(None)) => None,
        Ok(Err(reason)) => {
            warn!("{file}: container data not stored: {reason}");
            None
        }
        Err(error) => {
            warn!("{file}: container data not stored: {error}");
            None
        }
    }
}

/// op87's adjustment of `member`'s reference count by `delta` in the
/// library at `path`, deleting it at 0 (containers.md 5.3), as the
/// library's rules decide it ([`library::adjust_changes`]) and written
/// through the transaction: the new count, or `None` when there is no such
/// member. An adjustment they refuse is logged and changes nothing.
pub(crate) fn adjust_count(
    tx: &mut Transaction<'_>,
    file: &str,
    path: &[&[u8]],
    member: &[u8],
    delta: i32,
) -> Option<u32> {
    let decided = (|| {
        let view: &dyn View = tx;
        if let Some(reason) = library::refusal(view, path, None, Some((member, delta)))? {
            return Ok(Err(reason));
        }
        library::adjust_changes(&library::At { view, path }, member, delta).map(Ok)
    })();
    match decided {
        Ok(Ok(Some((count, changes)))) => {
            write(tx, path, changes);
            Some(count)
        }
        Ok(Ok(None)) => None,
        Ok(Err(reason)) => {
            warn!("{file}: reference count not changed: {reason}");
            None
        }
        Err(error) => {
            warn!("{file}: reference count not changed: {error}");
            None
        }
    }
}

/// A library's `changes` written through the transaction, below the
/// library at `path`. The rules' refusal checked what each put leaves; a
/// write refused anyway fails the operation's save, so nothing of it is
/// kept.
fn write(tx: &mut Transaction<'_>, path: &[&[u8]], changes: Vec<library::Change>) {
    for (rest, node) in changes {
        let at: Vec<Vec<u8>> = path.iter().map(|s| s.to_vec()).chain(rest).collect();
        let written = match node {
            Some(node) => tx.replace(at, node),
            None => tx.remove(at),
        };
        if let Err(reason) = written {
            tx.refuse(reason);
        }
    }
}

/// op96's thumbnail `req` asks for, read from `read`: which one it is (the
/// member's library and key, and the box), `None` for a box no thumbnail
/// is made at or a member the library does not hold (containers.md 6.3).
pub fn thumbnail_key(read: Read<'_>, req: &DownloadThumbnail) -> Result<Option<Key>, ReadError> {
    if !(1..=MAX_SIDE).contains(&req.width) || !(1..=MAX_SIDE).contains(&req.height) {
        return Ok(None);
    }
    let [member] = req.member.as_slice() else {
        return Ok(None);
    };
    let Some(library) = table_segment(&req.table).and_then(|t| library_path(read.derived, t))
    else {
        return Ok(None);
    };
    if read.view.node(&member_path(&library, member))?.is_none() {
        return Ok(None);
    }
    Ok(Some(Key {
        library,
        member: member.clone(),
        width: req.width,
        height: req.height,
    }))
}

/// The image of the member `key` names, read from `read`
/// ([`thumbnail::master`]): `None` for a member that holds no image the
/// server can scale.
pub fn thumbnail_image(
    read: Read<'_>,
    key: &Key,
) -> Result<Option<(ImageFormat, Vec<u8>)>, ReadError> {
    thumbnail::master(read.view, &member_path(&key.library, &key.member))
}

/// The path of `member` in the library at `library`.
fn member_path<'a>(library: &'a [Vec<u8>], member: &'a [u8]) -> Vec<&'a [u8]> {
    let mut path = segments(library);
    path.extend([&[MEMBERS][..], member]);
    path
}

/// op96: one chunk, at most `max_bytes` long, of the thumbnail `key` of
/// `hosted` (containers.md 6.3): `cached`, or else rendered from `image`
/// and cached; both were read under the store's lock, the rendering
/// happens outside it. Thumbnails are returned unencrypted (return flags
/// 0).
pub fn thumbnail_reply(
    hosted: &Hosted,
    req: &DownloadThumbnail,
    key: Key,
    cached: Option<Arc<Thumbnail>>,
    image: Option<(ImageFormat, Vec<u8>)>,
    max_bytes: usize,
) -> ThumbnailReply {
    let missing = ThumbnailReply::Status(THUMBNAIL_MISSING);
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
                        "{:?}: no thumbnail of member {:02x?}: {e}",
                        hosted.name, key.member
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
