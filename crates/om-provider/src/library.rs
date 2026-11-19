//! Container data of the hosted file for calculations (containers.md 6.1):
//! a container field's record value names a member of its table's library
//! `[T, 1f, 05, M]`. A calculation that reads the field gets the member's
//! file name and MD5 first (two small op90 downloads, `04` and `FNAM`), and
//! its data (an op90 of the member) only when a function reads the streams.
//!
//! Members do not change while stored, but dropping a library reuses ids,
//! so request/page boundaries revalidate headers. [`LibraryCache`] keys
//! reads by the owning connection and native table, retaining stream bodies
//! within [`DATA_BUDGET`] only while their fresh header hash still matches.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use om_data::calc::eval::StoredContainer;
use om_data::containers::{self, MemberInfo, FILE_NAME, LIBRARY};
type MemberKey = ([u8; 16], u64, Vec<u8>);
use om_data::DataError;
use om_hbam::{KeySegment, KeyTree, SlotReader};
use om_protocol::NodeState;
use om_session::Session;

use crate::{failure, key_bytes};

/// How many bytes of member data a connection keeps.
pub const DATA_BUDGET: usize = 128 << 20;

/// What a connection read of its file's libraries.
#[derive(Debug, Default)]
pub(crate) struct LibraryCache {
    members: HashMap<MemberKey, Arc<MemberInfo>>,
    data: HashMap<MemberKey, Arc<StoredContainer>>,
    /// Data keys, oldest first, and the bytes they hold.
    order: VecDeque<MemberKey>,
    bytes: usize,
}

fn size(stored: &StoredContainer) -> usize {
    stored.data.retained_data_size()
}

/// What tells the file `session` has open apart, for container ownership
/// and this cache: its connection's key, with the host's id of the file
/// mixed into its last four bytes. A connection carries several files
/// (#740), and their native table ids may coincide.
pub(crate) fn file_key(session: &Session) -> [u8; 16] {
    let mut key = *session.connection_key();
    if let Some(opened) = session.opened_file() {
        for (byte, id) in key[12..].iter_mut().zip(opened.reply.file_id.to_be_bytes()) {
            *byte ^= id;
        }
    }
    key
}

impl LibraryCache {
    /// Revalidate headers at a new page/request boundary. Stream bodies
    /// survive only while their content hash still matches the fresh header.
    pub fn refresh(&mut self) {
        self.members.clear();
    }

    /// The header of member `member` of `table`'s library, downloaded when
    /// not read yet; `None` when the library has no such member.
    pub fn member(
        &mut self,
        session: &mut Session,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, DataError> {
        let key: MemberKey = (file_key(session), table, member.to_vec());
        if let Some(info) = self.members.get(&key) {
            return Ok(Some(Arc::clone(info)));
        }
        let path = member_path(table, member);
        let Some(md5) = download_value(session, &path, &[containers::keys::MD5])? else {
            return Ok(None);
        };
        let Ok(md5) = <[u8; 16]>::try_from(md5.as_slice()) else {
            return Ok(None);
        };
        let (path_type, name) = download_value(session, &path, FILE_NAME)?
            .and_then(|fnam| containers::first_path(&fnam))
            .unwrap_or_default();
        let info = Arc::new(MemberInfo {
            name,
            md5,
            owner: Some(file_key(session)),
            path_type,
        });
        self.members.insert(key, Arc::clone(&info));
        Ok(Some(info))
    }

    /// That member's data, downloaded when not held; `None` when the
    /// library has no such member.
    pub fn data(
        &mut self,
        session: &mut Session,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, DataError> {
        self.data_as(session, table, member, table)
    }

    /// Read on native `table`, retaining the caller's schema identity in
    /// the returned value without copying stream bodies on each fetch.
    pub fn data_as(
        &mut self,
        session: &mut Session,
        table: u64,
        member: &[u8],
        client_table: u64,
    ) -> Result<Option<Arc<StoredContainer>>, DataError> {
        let key: MemberKey = (file_key(session), table, member.to_vec());
        let Some(info) = self.member(session, table, member)? else {
            return Ok(None);
        };
        if let Some(stored) = self.data.get(&key) {
            if stored
                .data
                .library
                .as_ref()
                .is_some_and(|r| r.md5 == info.md5)
            {
                return Ok(Some(Arc::clone(stored)));
            }
        }
        if let Some(old) = self.data.remove(&key) {
            self.bytes -= size(&old);
            self.order.retain(|k| k != &key);
        }
        let path = member_path(table, member);
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let reply = session
            .download(&refs)
            .map_err(|e| failure("download container data", e))?;
        if reply.node_state != NodeState::NonTerminal {
            return Ok(None);
        }
        let tree = KeyTree::from_slots(SlotReader::with_path(
            &reply.slots,
            path.iter().map(|k| KeySegment(k)).collect(),
        ))
        .map_err(|e| failure("decode container data", e))?;
        let Some(node) = tree.node(&refs) else {
            return Ok(None);
        };
        let Some(mut stored) = containers::read(&node.to_owned_node()) else {
            return Ok(None);
        };
        // A drop may reuse the numeric id. Never label replacement bytes
        // with a header from before the drop (or an intervening write).
        if MemberInfo::read(&node.to_owned_node()).map_or(true, |current| current.md5 != info.md5) {
            self.members.remove(&key);
            return Err(DataError::new(
                "The container changed while it was being read. Refresh and try again.",
            ));
        }
        stored.data.library = info.value(client_table, member).library;
        let stored = Arc::new(stored);
        self.bytes += size(&stored);
        self.order.push_back(key.clone());
        self.data.insert(key, Arc::clone(&stored));
        while self.bytes > DATA_BUDGET && self.order.len() > 1 {
            let old = self.order.pop_front().expect("nonempty");
            if let Some(dropped) = self.data.remove(&old) {
                self.bytes -= size(&dropped);
            }
        }
        Ok(Some(stored))
    }
}

/// `[T, 1f, 05, M]`.
fn member_path(table: u64, member: &[u8]) -> Vec<Vec<u8>> {
    vec![
        key_bytes(table),
        vec![LIBRARY],
        vec![containers::keys::MEMBERS],
        member.to_vec(),
    ]
}

/// The value of the key `key` under `path` (op90): a leaf's value, or a
/// chunked value assembled (1.5); `None` when there is no such key.
fn download_value(
    session: &mut Session,
    path: &[Vec<u8>],
    key: &[u8],
) -> Result<Option<Vec<u8>>, DataError> {
    let mut full = path.to_vec();
    full.push(key.to_vec());
    let refs: Vec<&[u8]> = full.iter().map(Vec::as_slice).collect();
    let reply = session
        .download(&refs)
        .map_err(|e| failure("download a container header", e))?;
    match reply.node_state {
        NodeState::Terminal => Ok(Some(reply.value)),
        NodeState::NonTerminal => {
            let tree = KeyTree::from_slots(SlotReader::with_path(
                &reply.slots,
                full.iter().map(|k| KeySegment(k)).collect(),
            ))
            .map_err(|e| failure("decode a container header", e))?;
            Ok(tree
                .node(&refs)
                .and_then(|n| n.assembled())
                .map(|v| v.into_owned()))
        }
        _ => Ok(None),
    }
}
