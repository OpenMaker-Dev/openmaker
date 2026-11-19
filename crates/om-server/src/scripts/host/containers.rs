//! Container fields in a server script session (containers.md,
//! "Implementation notes (OpenMaker Server scripts)").
//!
//! A calculation reads a container field as the file name of the library
//! member its stored reference names, and loads the member's streams only
//! when a function reads them ([`ServerHost::load_container`]).
//!
//! Set Field stores, as FileMaker Server's engine does (script oracle
//! s34-containers-setfield): container data as a library member, typed as
//! FileMaker types it (`om_data::containers::tree`); another record's value
//! of the same table as a reference to the same member; anything else
//! (text, a number, a date, a reference text without data) as its text. The
//! data waits in the session until the record is committed: the record's
//! cell holds a token of the session's (never a member reference, never
//! written to the file), and the commit stores the data in the library in
//! the same saved commit as the record (`writes::commit_records`), so a
//! failed save stores nothing. FileMaker's engine stores the data when Set
//! Field runs and releases it when the record is reverted (s34: a member
//! set twice in one edit counts two changes); the counts are the same once
//! committed.

use std::collections::HashMap;
use std::sync::Arc;

use om_calc::eval::StoredContainer;
use om_calc::{CalcError, Container, Value};
use om_data::containers::{self, MemberInfo};
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::OwnedNode;

use super::{values, Change, ServerHost};
use crate::library::{self, BinaryData};

/// Pending tokens: U+E001, the token's number, U+E000. A member reference
/// is hexadecimal before U+E000, so the two never meet.
const TOKEN_START: char = '\u{e001}';

/// Container data steps set that no commit stored yet, by token number.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    values: Vec<Option<Arc<PendingData>>>,
}

#[derive(Debug)]
struct PendingData {
    /// The value as calculations read it before the commit: the data as
    /// the member will hold it.
    value: Container,
    data: BinaryData,
}

/// The token number a cell's text holds, if it is one.
fn token(text: &str) -> Option<usize> {
    text.strip_prefix(TOKEN_START)?
        .strip_suffix('\u{e000}')?
        .parse()
        .ok()
}

impl Pending {
    fn get(&self, text: &str) -> Option<&Arc<PendingData>> {
        self.values.get(token(text)?)?.as_ref()
    }

    /// Keep the data whose tokens `used` names, drop the rest.
    fn retain(&mut self, used: &[usize]) {
        for (i, slot) in self.values.iter_mut().enumerate() {
            if !used.contains(&i) {
                *slot = None;
            }
        }
    }
}

/// The externally stored files of the hosted file at `hosted`
/// (containers.md 8): a remote stream's file beneath the hosted file's
/// container folder, opened as script paths are
/// ([`super::files::open_container_file`]).
pub(super) struct ContainerFolder<'a> {
    pub hosted: &'a std::path::Path,
}

/// The most bytes an externally stored file may hold here, as for one
/// inserted on the desktop.
const MAX_EXTERNAL: u64 = 1 << 30;

impl containers::ExternalFiles for ContainerFolder<'_> {
    fn read(&self, _member: &OwnedNode, _kind: [u8; 4], reference: &[u8]) -> Option<Vec<u8>> {
        use std::io::Read;
        let parts = reference_path(reference)?;
        let file = super::files::open_container_file(self.hosted, &parts)?;
        let mut out = Vec::new();
        file.take(MAX_EXTERNAL + 1).read_to_end(&mut out).ok()?;
        (out.len() as u64 <= MAX_EXTERNAL).then_some(out)
    }
}

/// The folders and file name, below the container folder, a remote
/// stream's reference names. Its layout (name, size, CRC;
/// `GetRemoteStreamFileName`) is not decoded (containers.md 8.2, 12.4):
/// until a FileMaker Pro probe gives one, no reference names a file and
/// external data reads as missing (`VerifyContainer` 0).
fn reference_path(_reference: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    None
}

/// What a session's record rules read of container fields: data steps set
/// for a write not yet stored, by its token, and members of the hosted
/// file's libraries (read when a calculation asks; the rules run without
/// the store's lock held).
struct SessionContainers {
    hosted: Arc<crate::hosting::Hosted>,
    pending: HashMap<String, Container>,
}

impl om_data::rules::ContainerReader for SessionContainers {
    fn value(&self, table: u64, text: &str) -> Option<Container> {
        if let Some(pending) = self.pending.get(text) {
            return Some(pending.clone());
        }
        let stored = self
            .hosted
            .read_store()
            .container(&uint_segment(table), text);
        stored.library.is_some().then_some(stored)
    }

    fn data(&self, table: u64, member: &[u8]) -> Option<StoredContainer> {
        let store = self.hosted.read_store();
        let node = store.library_member(&[&uint_segment(table), &[containers::LIBRARY]], member)?;
        containers::read_with(
            node,
            &ContainerFolder {
                hosted: &self.hosted.path,
            },
        )
    }
}

impl ServerHost {
    /// The container reader of the session's record rules.
    pub(super) fn container_reader(
        &self,
    ) -> Arc<dyn om_data::rules::ContainerReader + Send + Sync> {
        let pending = self
            .pending
            .values
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                Some((
                    format!("{TOKEN_START}{i}\u{e000}"),
                    p.as_ref()?.value.clone(),
                ))
            })
            .collect();
        Arc::new(SessionContainers {
            hosted: Arc::clone(&self.hosted),
            pending,
        })
    }

    /// The text Set Field stores in a container field of `table` for
    /// `value`; `None` for an empty value (the cell goes).
    pub(super) fn container_text(&mut self, table: u64, value: &Value) -> Option<String> {
        let Value::Container(c) = value else {
            return values::to_stored(om_calc::ValueType::Text, value, &self.cx());
        };
        if c.is_empty() {
            return None;
        }
        if let Some(r) = &c.library {
            let store = self.hosted.read_store();
            let t = uint_segment(r.table);
            let member = store.library_member(&[&t, &[containers::LIBRARY]], &r.member);
            if let Some(member) = member {
                if MemberInfo::read(member).is_some_and(|i| i.md5 == r.md5) {
                    if r.table == table {
                        // The same table's data: one more reference to its
                        // member (containers.md 5.1).
                        return Some(containers::reference_text(&r.member));
                    }
                    // Another table's: its tree as stored there.
                    if let Some(data) = library::member_tree(member) {
                        let stored = containers::read(member)?;
                        drop(store);
                        return Some(self.pend(stored.data, data));
                    }
                }
            }
        }
        if c.streams.is_empty() {
            // A reference text without data stays text.
            return Some(c.text.as_str().to_owned()).filter(|t| !t.is_empty());
        }
        let (keys, md5) = containers::tree(c)?;
        let value = containers::read(&OwnedNode::list(Vec::new(), keys.clone()))?.data;
        Some(self.pend(value, BinaryData::from_tree(keys, md5)))
    }

    /// Hold `data` for the commit and return the token its cell holds.
    fn pend(&mut self, value: Container, data: BinaryData) -> String {
        let used = self.pending_tokens();
        self.pending.retain(&used);
        let number = self.pending.values.len();
        self.pending
            .values
            .push(Some(Arc::new(PendingData { value, data })));
        format!("{TOKEN_START}{number}\u{e000}")
    }

    /// The tokens the open record and the open transaction's records hold.
    fn pending_tokens(&self) -> Vec<usize> {
        let held = self.transaction.iter().flat_map(|t| {
            t.changes.values().filter_map(|c| match c {
                Change::Write(record) => Some(&record.node),
                Change::Delete => None,
            })
        });
        self.record
            .iter()
            .map(|r| &r.node)
            .chain(held)
            .flat_map(|node| &node.children)
            .filter_map(|cell| token(&values::cell_text(cell)?))
            .collect()
    }

    /// The value of a container field of `table` whose cell holds `text`:
    /// data a step set and no commit stored yet, else the stored reference
    /// ([`crate::store::Store::container`]).
    pub(super) fn container_value(&self, table: u64, text: &str) -> Value {
        if let Some(pending) = self.pending.get(text) {
            return Value::Container(pending.value.clone());
        }
        // Text a script put into the field is text (s34).
        if containers::reference(text).is_none() {
            return Value::text(text);
        }
        Value::Container(
            self.hosted
                .read_store()
                .container(&uint_segment(table), text),
        )
    }

    /// The container data the cells of a record of `table` hold for the
    /// commit to store, by cell.
    pub(super) fn uploads(&self, table: u64, node: &OwnedNode) -> Vec<(u64, BinaryData)> {
        let store = self.hosted.read_store();
        let t = uint_segment(table);
        node.children
            .iter()
            .filter(|cell| store.container_cell(&t, &cell.segment))
            .filter_map(|cell| {
                let pending = self.pending.get(&values::cell_text(cell)?)?;
                Some((be_uint(&cell.segment)?, pending.data.clone()))
            })
            .collect()
    }

    /// Export Field Contents (132): the field's container data (its main
    /// stream), or its text as UTF-16, written as a new file at the first
    /// path that can take it, only beneath the session's Documents and
    /// temporary folders and through their handles ([`super::files`]).
    /// An empty field writes nothing. 800 when no path can be written,
    /// as for Export Records.
    pub(super) fn export_contents(
        &mut self,
        field: Option<om_scripts::runtime::FieldRef>,
        paths: &[String],
        create_folders: bool,
    ) -> om_scripts::runtime::HostResult {
        use om_scripts::runtime::{ErrorCode, HostError};
        self.require_view()?;
        // A server session has no active field.
        let field = field.ok_or(HostError::Unsupported)?;
        let value = self.value_of(field)?;
        let bytes = match &value {
            v if v.is_empty() => return Ok(()),
            Value::Container(c) if c.is_unloaded() => {
                let stored = self
                    .load_container(c)
                    .map_err(|e| HostError::Error(ErrorCode(e.code())))?;
                if stored.external && !stored.intact {
                    return Err(ErrorCode(1225).into());
                }
                stored.data.data().to_vec()
            }
            Value::Container(c) if !c.streams.is_empty() => c.data().to_vec(),
            // Text as "a UTF-16 format text file" (Claris; little-endian with
            // a byte order mark, OpenMaker's choice).
            other => {
                let mut out = vec![0xff, 0xfe];
                let text = other.to_text(&self.cx()).into_string();
                out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
                out
            }
        };
        let destination = self
            .destination(paths, create_folders)
            .ok_or(ErrorCode(800))?;
        destination.write(&bytes).map_err(|_| ErrorCode(800).into())
    }

    /// The data of a container a calculation holds by its library member
    /// (`CalcHost::container_data`): every stream and whether they match
    /// the stored MD5. A member the library no longer has is error 1225.
    pub(super) fn load_container(&self, c: &Container) -> Result<StoredContainer, CalcError> {
        let r = c.library.as_ref().ok_or(CalcError::NOT_SUPPORTED)?;
        let store = self.hosted.read_store();
        let member = store
            .library_member(&[&uint_segment(r.table), &[containers::LIBRARY]], &r.member)
            .filter(|m| MemberInfo::read(m).is_some_and(|i| i.md5 == r.md5))
            .ok_or(CalcError::NOT_SUPPORTED)?;
        let folder = ContainerFolder {
            hosted: &self.hosted.path,
        };
        containers::read_with(member, &folder).ok_or(CalcError::NOT_SUPPORTED)
    }
}
