//! Container data (spec `docs/re/containers.md`): the libraries of binary
//! data members that container fields name. A table's container values live
//! in its library `[T, 1f]`, layout graphics in the file's `[1f]`; a record's
//! container field holds only the member's key as text (1.4).
//!
//! These functions work on one library node `[L]`: op88 stores uploaded data
//! deduplicated by MD5 (3.4), op87 adjusts a member's reference count and
//! deletes it at zero (5.3). Reading a member as calculations see container
//! data, and the binary-data trees themselves, are every host's
//! (`om_data::containers`).

use std::borrow::Cow;
use std::ops::Bound;

use om_data::containers::{md5, stream_node};
use om_hbam::tree::read_owned;
use om_hbam::writer::{unwritable, Unwritable};
use om_hbam::{serial, OwnedNode, SlotReader};

use crate::derived;
use crate::engine::{Direction, ReadError, View};
use crate::modcount::{self, serial_leaf, MOD_COUNT};
use crate::store::describe;

pub use om_data::containers::LIBRARY;

/// Keys of a library (1.2) and of its members (1.3).
mod keys {
    pub const HEADER: u8 = 0x01;
    pub const MEMBERS: u8 = 0x05;
    /// Under the header.
    pub const LAST_ID: u8 = 0x00;
    pub const MEMBER_COUNT: u8 = 0x04;
    pub const GLYPHS: u8 = 0x06;
    pub const MEMBER_LIST: u8 = 0x07;
    pub const MD5_INDEX: u8 = 0x08;
    /// Under the header and under a member.
    pub const UNIQUE_ID: u8 = 0xd8;
    /// Under a member.
    pub const DIRECTORY: u8 = 0x02;
    pub const MD5: u8 = 0x04;
    pub const REF_COUNT: u8 = 0x12;
    pub const PRIVATE_FLAGS: u8 = 0xf1;
}

/// The most memory an op88 payload's tree may take, per byte of the
/// payload, plus [`TREE_SLACK`]. Binary data is mostly stream bytes in
/// pieces of 1000 (1.5), which take about 1.1 times their size as a tree;
/// a payload of short keys would take tens of times its size, and is
/// refused as it is read. OpenMaker's limit; FileMaker Server's is
/// unmeasured.
const TREE_PER_BYTE: usize = 2;
const TREE_SLACK: usize = 64 << 10;

/// The most memory an op88 payload of `len` bytes may take as a tree
/// ([`TREE_PER_BYTE`], [`TREE_SLACK`]): what a host reserves before it
/// reads one (`crate::uploads`).
pub(crate) fn tree_limit(len: usize) -> usize {
    len.saturating_mul(TREE_PER_BYTE).saturating_add(TREE_SLACK)
}

/// The glyph flag stream and the private member flags of a glyph (1.3).
const GLYPH: &[u8; 4] = b"GLPH";
const GLYPH_FLAGS: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

/// An uploaded binary-data tree, checked and ready to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryData {
    /// Its MD5 (`04`), as uploaded and recomputed.
    pub md5: [u8; 16],
    /// `02`, `04` and the streams the directory lists, each stored inline
    /// or chunked as the file stores it.
    pub keys: Vec<OwnedNode>,
    /// Whether it is a glyph (a monochrome layout icon).
    pub glyph: bool,
}

impl BinaryData {
    /// Read an op88 payload (spec 2, 3.1). The directory must list streams
    /// that are present with the declared sizes, and the MD5 must be the
    /// one of the streams (2.2); FileMaker Server checks neither.
    pub fn parse(payload: &[u8]) -> Result<Self, String> {
        Self::parse_within(payload, tree_limit(payload.len()))
            .unwrap_or_else(|| Err(Self::TOO_LARGE.to_string()))
    }

    /// Why [`BinaryData::parse`] refuses a payload whose tree would pass
    /// [`tree_limit`].
    pub(crate) const TOO_LARGE: &'static str = "binary data takes too much memory for its size";

    /// [`BinaryData::parse`], its tree read within `limit` bytes of memory:
    /// the memory a host reserved for it ([`tree_limit`] at most). `None`
    /// when the tree would take more, so that the host can tell its own
    /// limit from data that is not binary data.
    pub fn parse_within(payload: &[u8], limit: usize) -> Option<Result<Self, String>> {
        // A payload without its opening `80` reads the same: the slot
        // reader skips it.
        let tree = match read_owned(SlotReader::new(payload), limit) {
            Ok(tree) => tree,
            Err(om_hbam::Error::TooLarge { .. }) => return None,
            Err(e) => return Some(Err(format!("binary data is not a slot list: {e}"))),
        };
        Some(Self::checked(tree))
    }

    /// The binary data of `tree`, read from a payload, once its directory,
    /// streams and MD5 are checked.
    fn checked(tree: OwnedNode) -> Result<Self, String> {
        let directory = tree
            .child(&[keys::DIRECTORY])
            .and_then(|n| n.value.clone())
            .ok_or("binary data has no stream directory")?;
        let md5_value: [u8; 16] = tree
            .child(&[keys::MD5])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| v.try_into().ok())
            .ok_or("binary data has no 16-byte MD5")?;
        let (count, entries) = directory
            .split_first_chunk::<2>()
            .ok_or("stream directory too short")?;
        let count = usize::from(u16::from_be_bytes(*count));
        if entries.len() != count * 8 {
            return Err(format!(
                "stream directory lists {count} streams in {} bytes",
                entries.len()
            ));
        }
        let mut streams = Vec::with_capacity(count);
        let mut keys = vec![
            OwnedNode::leaf(vec![keys::DIRECTORY], directory.clone()),
            OwnedNode::leaf(vec![keys::MD5], md5_value.to_vec()),
        ];
        let mut glyph = false;
        for entry in entries.chunks_exact(8) {
            let (kind, size) = entry.split_at(4);
            let size = u32::from_be_bytes(size.try_into().expect("4 bytes")) as usize;
            let bytes = tree
                .child(kind)
                .and_then(OwnedNode::assembled)
                .ok_or_else(|| format!("stream {} is missing", String::from_utf8_lossy(kind)))?;
            if bytes.len() != size {
                return Err(format!(
                    "stream {} holds {} bytes, the directory says {size}",
                    String::from_utf8_lossy(kind),
                    bytes.len()
                ));
            }
            glyph |= kind == GLYPH && bytes.as_ref() == [1];
            keys.push(stream_node(kind, &bytes));
            streams.push((kind.to_vec(), bytes.into_owned()));
        }
        let computed = md5(streams.iter().map(|(k, b)| (k.as_slice(), b.as_slice())));
        if computed != md5_value {
            return Err("the MD5 does not match the streams".into());
        }
        keys.sort_by(|a, b| a.segment.cmp(&b.segment));
        if keys.windows(2).any(|w| w[0].segment == w[1].segment) {
            return Err("the stream directory lists a stream twice".into());
        }
        Ok(Self {
            md5: md5_value,
            keys,
            glyph,
        })
    }

    /// Data a server script set into a container field: the tree of
    /// `om_data::containers::tree`, which builds it to the rules above.
    pub fn from_tree(keys: Vec<OwnedNode>, md5: [u8; 16]) -> Self {
        let glyph = keys
            .iter()
            .any(|k| k.segment == GLYPH && k.value.as_deref() == Some(&[1]));
        Self { md5, keys, glyph }
    }
}

/// What op88 did with the data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    /// Stored as this new member.
    New(Vec<u8>),
    /// A member already held the same data; its count went up by one.
    Existing(Vec<u8>),
}

/// What the library rules read of a library `[L]`, by paths relative to
/// it: a library node in memory (the commit path changes it in place), or
/// the one a view reads at a path ([`At`], an operation's transaction).
pub(crate) trait Reads {
    /// The node at `path`: `None` when there is none, else its own value
    /// (`None` for a list without one).
    fn value(&self, path: &[&[u8]]) -> Result<Option<Option<Vec<u8>>>, ReadError>;
    /// The keys of the first `limit` children of the node at `path`.
    fn first(&self, path: &[&[u8]], limit: usize) -> Result<Vec<Vec<u8>>, ReadError>;
    /// How many children the node at `path` has (0 when there is none).
    fn count(&self, path: &[&[u8]]) -> Result<usize, ReadError>;
}

impl Reads for OwnedNode {
    fn value(&self, path: &[&[u8]]) -> Result<Option<Option<Vec<u8>>>, ReadError> {
        Ok(self.at(path).map(|node| node.value.clone()))
    }

    fn first(&self, path: &[&[u8]], limit: usize) -> Result<Vec<Vec<u8>>, ReadError> {
        Ok(self.at(path).map_or_else(Vec::new, |node| {
            let first = node.children.iter().take(limit);
            first.map(|child| child.segment.clone()).collect()
        }))
    }

    fn count(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        Ok(self.at(path).map_or(0, |node| node.children.len()))
    }
}

/// The library `view` reads at `path`, which may not exist yet.
pub(crate) struct At<'a> {
    pub view: &'a dyn View,
    pub path: &'a [&'a [u8]],
}

impl At<'_> {
    fn join<'p>(&'p self, rest: &[&'p [u8]]) -> Vec<&'p [u8]> {
        let mut path = self.path.to_vec();
        path.extend_from_slice(rest);
        path
    }
}

impl Reads for At<'_> {
    fn value(&self, path: &[&[u8]]) -> Result<Option<Option<Vec<u8>>>, ReadError> {
        let head = self.view.node(&self.join(path))?;
        Ok(head.map(|head| head.value.map(Cow::into_owned)))
    }

    fn first(&self, path: &[&[u8]], limit: usize) -> Result<Vec<Vec<u8>>, ReadError> {
        let path = self.join(path);
        let children = self
            .view
            .children(&path, Bound::Unbounded, Direction::Forward);
        children
            .take(limit)
            .map(|child| child.map(|child| child.segment.into_owned()))
            .collect()
    }

    fn count(&self, path: &[&[u8]]) -> Result<usize, ReadError> {
        let path = self.join(path);
        let mut count = 0;
        for child in self
            .view
            .children(&path, Bound::Unbounded, Direction::Forward)
        {
            child?;
            count += 1;
        }
        Ok(count)
    }
}

/// One change of a library, by a path relative to it: the node put there
/// (with its children, replacing whatever is there and creating missing
/// parents, as `OwnedNode::put`), or, with `None`, the node there removed.
pub(crate) type Change = (Vec<Vec<u8>>, Option<OwnedNode>);

/// `changes` made to `library` in place.
fn apply(library: &mut OwnedNode, changes: Vec<Change>) {
    for (path, node) in changes {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        match node {
            Some(node) => library.put(&path, node),
            None => {
                library.remove(&path);
            }
        }
    }
}

/// A library in memory never fails a read.
fn read<T>(read: Result<T, ReadError>) -> T {
    read.expect("a library in memory never fails a read")
}

/// The count a key of the library holds (`[u8 length][serial key]`), 0
/// when absent or malformed (`modcount::serial_value`).
fn serial_at(library: &dyn Reads, path: &[&[u8]]) -> Result<u64, ReadError> {
    let value = library.value(path)?.flatten();
    Ok(value.as_deref().and_then(modcount::parse).unwrap_or(0))
}

/// What a [`store`] of data does in a library: count one more reference
/// on the member that already holds it, or add a new member. One decision
/// ([`store_plan`]), which the store carries out ([`store_as`],
/// [`store_changes`]) and a caller that may have to put the change back
/// journals ([`Self::keys`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorePlan {
    /// The member that already holds the data.
    Existing(Vec<u8>),
    /// A new member.
    New,
}

/// What a [`store`] of data with `md5` into `library` (`None`: there is no
/// library yet) does. Members are named by serial keys (containers.md), and
/// op88's reply names the one it reuses in a key of at most 255 bytes: a
/// member whose key in the file is not a serial key is not reused.
pub fn store_plan(library: Option<&OwnedNode>, md5: &[u8; 16]) -> StorePlan {
    library.map_or(StorePlan::New, |library| read(plan(library, md5)))
}

/// [`store_plan`] for the library `library` reads.
pub(crate) fn plan(library: &dyn Reads, md5: &[u8; 16]) -> Result<StorePlan, ReadError> {
    Ok(match reused(library, md5)? {
        Some(existing) if library.value(&[&[keys::MEMBERS], &existing])?.is_some() => {
            StorePlan::Existing(existing)
        }
        _ => StorePlan::New,
    })
}

impl StorePlan {
    /// The keys under `library` the store changes, for a caller that may
    /// have to put them back: the counts of the member it reuses, else the
    /// header and the new member. `None` when there is no library yet: all
    /// of it is new.
    pub fn keys(&self, library: Option<&OwnedNode>) -> Option<Vec<Vec<Vec<u8>>>> {
        let library = library?;
        Some(match self {
            StorePlan::Existing(existing) => count_keys(existing).into_iter().collect(),
            StorePlan::New => match read(next_member(library)) {
                Some(next) => vec![vec![vec![keys::HEADER]], vec![vec![keys::MEMBERS], next]],
                // No member can be added ([`store_counter_refusal`]).
                None => vec![vec![vec![keys::HEADER]]],
            },
        })
    }
}

/// Store `data` in `library` (3.4): count + 1 on the member that already
/// holds data with its MD5, otherwise a new member with count 1. The header
/// is created on first use. `unique_id` makes the random ids (`d8`).
/// `None`, changing nothing, when a counter the store advances cannot
/// ([`store_counter_refusal`]).
#[cfg(test)]
pub fn store(
    library: &mut OwnedNode,
    data: BinaryData,
    unique_id: impl FnMut() -> [u8; 16],
) -> Option<Stored> {
    let plan = store_plan(Some(library), &data.md5);
    store_as(library, data, &plan, unique_id)
}

/// [`store`] as `plan` decided it ([`store_plan`], for this library and
/// this data).
pub fn store_as(
    library: &mut OwnedNode,
    data: BinaryData,
    plan: &StorePlan,
    unique_id: impl FnMut() -> [u8; 16],
) -> Option<Stored> {
    let (stored, changes) = read(store_changes(library, data, plan, unique_id))?;
    apply(library, changes);
    Some(stored)
}

/// The changes a [`store`] of `data` into the library `library` reads
/// makes, as `plan` decided it ([`plan`]), with what it stored. `None`
/// when a counter the store advances cannot ([`store_counter_refusal`]).
pub(crate) fn store_changes(
    library: &dyn Reads,
    data: BinaryData,
    plan: &StorePlan,
    mut unique_id: impl FnMut() -> [u8; 16],
) -> Result<Option<(Stored, Vec<Change>)>, ReadError> {
    if let StorePlan::Existing(existing) = plan {
        let changes = adjust_changes(library, existing, 1)?;
        return Ok(changes.map(|(_, changes)| (Stored::Existing(existing.clone()), changes)));
    }
    let header: &[u8] = &[keys::HEADER];
    let in_header = |key: u8| vec![vec![keys::HEADER], vec![key]];
    // Both counters the new member advances, before anything changes.
    let Some(id) = serial_at(library, &[header, &[keys::LAST_ID]])?.checked_add(1) else {
        return Ok(None);
    };
    let Some(header_count) = serial_at(library, &[header, &[MOD_COUNT]])?.checked_add(2) else {
        return Ok(None);
    };
    let mut changes: Vec<Change> = Vec::new();
    if library.value(&[header])?.is_none() {
        for list in [keys::GLYPHS, keys::MEMBER_LIST, keys::MD5_INDEX] {
            changes.push((
                in_header(list),
                Some(OwnedNode::list(vec![list], Vec::new())),
            ));
        }
        let made = OwnedNode::leaf(vec![keys::UNIQUE_ID], unique_id());
        changes.push((in_header(keys::UNIQUE_ID), Some(made)));
    }
    let member = serial::encode(id);
    changes.push((
        in_header(keys::LAST_ID),
        Some(serial_leaf(keys::LAST_ID, id)),
    ));
    let listed = library.value(&[header, &[keys::MEMBER_LIST], &member])?;
    let member_count =
        library.count(&[header, &[keys::MEMBER_LIST]])? + usize::from(listed.is_none());
    let mut under_header = |list: u8, key: &[u8], node: OwnedNode| {
        let path = vec![vec![keys::HEADER], vec![list], key.to_vec()];
        changes.push((path, Some(node)));
    };
    let entry = || OwnedNode::leaf(member.clone(), Vec::new());
    under_header(keys::MEMBER_LIST, &member, entry());
    let index = OwnedNode::list(data.md5.to_vec(), vec![entry()]);
    under_header(keys::MD5_INDEX, &data.md5, index);
    if data.glyph {
        under_header(keys::GLYPHS, &member, entry());
    }
    // The header's member count `04`, from its member list.
    let count = OwnedNode::leaf(
        vec![keys::MEMBER_COUNT],
        (member_count as u32).to_be_bytes(),
    );
    changes.push((in_header(keys::MEMBER_COUNT), Some(count)));
    // FileMaker Server's catalog count goes up by two per new member
    // (live:c1, c3, c4) and by one per deleted member.
    changes.push((
        in_header(MOD_COUNT),
        Some(serial_leaf(MOD_COUNT, header_count)),
    ));
    let mut node = OwnedNode::list(member.clone(), data.keys);
    node.set_child(OwnedNode::leaf(vec![keys::REF_COUNT], 1u32.to_be_bytes()));
    node.set_child(OwnedNode::leaf(vec![keys::UNIQUE_ID], unique_id()));
    if data.glyph {
        node.set_child(OwnedNode::leaf(vec![keys::PRIVATE_FLAGS], GLYPH_FLAGS));
    }
    changes.push((vec![vec![keys::MEMBERS], member.clone()], Some(node)));
    Ok(Some((Stored::New(member), changes)))
}

/// The member a [`store`] of data with `md5` reuses: the first one the
/// header's MD5 index lists for it, when its key is a serial key.
fn reused(library: &dyn Reads, md5: &[u8; 16]) -> Result<Option<Vec<u8>>, ReadError> {
    let first = library.first(&[&[keys::HEADER], &[keys::MD5_INDEX], md5], 1)?;
    Ok(first
        .into_iter()
        .next()
        .filter(|member| serial::decode(member).is_some()))
}

/// Why a store of `data` into the library at `path` in the tree `view`
/// reads (or, with `member`, an adjustment of that member's count by its
/// delta) would change a value it does not name, add what the file cannot
/// hold, or advance a counter past the largest a serial key holds: a node
/// on the library's path, its header, lists, members or the member that
/// holds a value (`om_hbam::writer::holds_value`), a last member id or
/// modification count the file holds at the largest
/// ([`store_counter_refusal`], [`adjust_counter_refusal`]), or an uploaded
/// key the new member could not hold (`om_hbam::writer::unwritable`). For
/// a log line; `None` when the write is safe.
pub(crate) fn refusal(
    view: &dyn View,
    path: &[&[u8]],
    data: Option<&BinaryData>,
    member: Option<(&[u8], i32)>,
) -> Result<Option<String>, ReadError> {
    let value = |found: Vec<Vec<u8>>| {
        Some(describe(&[Unwritable {
            path: found,
            reason: "a value cannot become a list",
        }]))
    };
    if let Some(found) = derived::value_on_path(view, path)? {
        return Ok(value(found));
    }
    let library = At { view, path };
    let exists = view.node(path)?.is_some();
    // The nodes the change writes beneath, below the library: the members
    // or the member, for a store also the header's lists and the member
    // it would reuse. Read from the root, which holds no value above the
    // library (checked above).
    let beneath = |rest: &[&[u8]]| derived::value_on_path(view, &library.join(rest));
    let inner = match (exists, member, data) {
        (false, _, _) | (true, None, None) => None,
        (true, Some((member, _)), _) => beneath(&[&[keys::MEMBERS], member])?,
        (true, None, Some(data)) => {
            let header: &[u8] = &[keys::HEADER];
            let lists: [&[&[u8]]; 4] = [
                &[header, &[keys::MEMBER_LIST]],
                &[header, &[keys::MD5_INDEX]],
                &[header, &[keys::GLYPHS]],
                &[&[keys::MEMBERS]],
            ];
            let mut found = None;
            for rest in lists {
                found = beneath(rest)?;
                if found.is_some() {
                    break;
                }
            }
            match (found, reused(&library, &data.md5)?) {
                (Some(found), _) => Some(found),
                (None, Some(reused)) => beneath(&[&[keys::MEMBERS], &reused])?,
                (None, None) => None,
            }
        }
    };
    if let Some(found) = inner {
        return Ok(value(found));
    }
    let counters = match (exists, member, data) {
        (false, _, _) | (true, None, None) => None,
        (true, Some((member, delta)), _) => adjust_counter_refusal(&library, member, delta)?,
        (true, None, Some(data)) => store_counter_refusal(&library, &data.md5)?,
    };
    if let Some(reason) = counters {
        return Ok(Some(format!("{path:02x?}: {reason}")));
    }
    let Some(data) = data else {
        return Ok(None);
    };
    if exists && matches!(plan(&library, &data.md5)?, StorePlan::Existing(_)) {
        return Ok(None);
    }
    let Some(new_member) = next_member(&library)? else {
        return Ok(Some(format!("{path:02x?}: no member id is left")));
    };
    let mut parents = path.to_vec();
    parents.push(&[keys::MEMBERS]);
    parents.push(&new_member);
    Ok(data
        .keys
        .iter()
        .map(|key| unwritable(&parents, key, 3))
        .find(|refused| !refused.is_empty())
        .map(|refused| describe(&refused)))
}

/// The key [`store`] gives a new member of `library`: one more than the
/// header's last issued id (3.4). `None` when that id is the largest a
/// serial key holds.
pub(crate) fn next_member(library: &dyn Reads) -> Result<Option<Vec<u8>>, ReadError> {
    let last = serial_at(library, &[&[keys::HEADER], &[keys::LAST_ID]])?;
    Ok(last.checked_add(1).map(serial::encode))
}

/// Why a [`store`] of data with `md5` into `library` cannot advance the
/// counters it must: the reused member's ([`adjust_counter_refusal`]), or
/// for a new member the header's last issued id (by 1) and modification
/// count (by 2). These come from the file, which a Full Access catalog
/// upload can set; past the largest a serial key holds the store is
/// refused rather than wrapped around. For a log line; `None` when it can.
pub(crate) fn store_counter_refusal(
    library: &dyn Reads,
    md5: &[u8; 16],
) -> Result<Option<&'static str>, ReadError> {
    if let StorePlan::Existing(existing) = plan(library, md5)? {
        return adjust_counter_refusal(library, &existing, 1);
    }
    if next_member(library)?.is_none() {
        return Ok(Some(
            "the library's last member id is the largest a serial key holds",
        ));
    }
    let count = serial_at(library, &[&[keys::HEADER], &[MOD_COUNT]])?;
    Ok(count
        .checked_add(2)
        .is_none()
        .then_some("the library header's modification count cannot advance"))
}

/// Why an [`adjust`] of `member`'s count by `delta` cannot advance the
/// counters it must: the member's modification count (by 1) and, when the
/// member goes, the header's (by 1). For a log line; `None` when it can, or
/// when there is no such member or nothing to change.
pub(crate) fn adjust_counter_refusal(
    library: &dyn Reads,
    member: &[u8],
    delta: i32,
) -> Result<Option<&'static str>, ReadError> {
    let at: [&[u8]; 2] = [&[keys::MEMBERS], member];
    if library.value(&at)?.is_none() || delta == 0 {
        return Ok(None);
    }
    if serial_at(library, &[at[0], at[1], &[MOD_COUNT]])?
        .checked_add(1)
        .is_none()
    {
        return Ok(Some("the member's modification count cannot advance"));
    }
    if library.value(&[&[keys::HEADER]])?.is_none() {
        return Ok(None);
    }
    let goes = adjusted(reference_count_at(library, member)?, delta) == 0;
    let header = serial_at(library, &[&[keys::HEADER], &[MOD_COUNT]])?;
    Ok((goes && header.checked_add(1).is_none())
        .then_some("the library header's modification count cannot advance"))
}

/// The members node of a library, `05`.
pub const MEMBERS: u8 = keys::MEMBERS;

/// The table and member an op87 path names: `[T, 1f, 05, M]` in table `T`'s
/// library or `[1f, 05, M]` in the file's (5.3). Anything else is refused:
/// FileMaker Server writes the count to whatever key it is given, even the
/// file's root.
pub fn member_path(path: &[Vec<u8>]) -> Option<(Option<&[u8]>, &[u8])> {
    let (table, rest) = match path {
        [table, rest @ ..] if rest.len() == 3 => (Some(table.as_slice()), rest),
        rest => (None, rest),
    };
    match rest {
        [library, members, member]
            if library == &[LIBRARY]
                && members == &[keys::MEMBERS]
                && serial::decode(member).is_some() =>
        {
            Some((table, member.as_slice()))
        }
        _ => None,
    }
}

/// The member `[L, 05, member]`, if it exists.
pub fn member<'a>(library: &'a OwnedNode, member: &[u8]) -> Option<&'a OwnedNode> {
    library.at(&[&[keys::MEMBERS], member])
}

/// The keys under a library that an [`adjust`] of `member` changes while
/// its count stays above 0 (its count and its modification count).
fn count_keys(member: &[u8]) -> [Vec<Vec<u8>>; 2] {
    [keys::REF_COUNT, MOD_COUNT].map(|key| vec![vec![keys::MEMBERS], member.to_vec(), vec![key]])
}

/// A member's binary-data tree as an upload carries it (3.1): the
/// directory, the MD5 and the streams, without what the server keeps
/// beside them (count, unique id, stamps, flags). For storing the same data
/// in another table's library.
pub fn member_tree(member: &OwnedNode) -> Option<BinaryData> {
    let md5: [u8; 16] = member
        .child(&[keys::MD5])
        .and_then(|n| n.value.as_deref())
        .and_then(|v| v.try_into().ok())?;
    member.child(&[keys::DIRECTORY])?;
    let keys = member
        .children
        .iter()
        .filter(|k| {
            k.segment == [keys::DIRECTORY] || k.segment == [keys::MD5] || k.segment.len() == 4
        })
        .cloned()
        .collect();
    Some(BinaryData::from_tree(keys, md5))
}

/// The keys under `library` that an [`adjust`] of `member` by `delta`
/// changes, by the decision it makes ([`deletes`]), for a caller that may
/// have to put them back: the member's counts, or, when it goes, the
/// member and the header that lists it; none for a delta of 0. `None` when
/// the library has no such member.
pub fn adjust_keys(library: &OwnedNode, member: &[u8], delta: i32) -> Option<Vec<Vec<Vec<u8>>>> {
    let count = reference_count(library.at(&[&[keys::MEMBERS], member])?);
    Some(match delta {
        0 => Vec::new(),
        _ if deletes(count, delta) => deletion_keys(member).into_iter().collect(),
        _ => count_keys(member).into_iter().collect(),
    })
}

/// Whether an [`adjust`] by `delta` of a member counted `count` deletes it.
fn deletes(count: u32, delta: i32) -> bool {
    delta != 0 && adjusted(count, delta) == 0
}

/// The keys under a library that an [`adjust`] deleting `member` changes:
/// the member and the header that lists it.
fn deletion_keys(member: &[u8]) -> [Vec<Vec<u8>>; 2] {
    [
        vec![vec![keys::MEMBERS], member.to_vec()],
        vec![vec![keys::HEADER]],
    ]
}

/// A member's reference count (`12`, missing = 0).
pub fn reference_count(member: &OwnedNode) -> u32 {
    member
        .child(&[keys::REF_COUNT])
        .and_then(|n| n.value.as_deref())
        .and_then(|v| v.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

/// [`reference_count`] of `member` in the library `library` reads.
fn reference_count_at(library: &dyn Reads, member: &[u8]) -> Result<u32, ReadError> {
    let value = library
        .value(&[&[keys::MEMBERS], member, &[keys::REF_COUNT]])?
        .flatten();
    Ok(value
        .as_deref()
        .and_then(|v| <[u8; 4]>::try_from(v).ok())
        .map_or(0, u32::from_be_bytes))
}

/// `count` adjusted by `delta`, clamped at 0; u32::MAX is op87's failure
/// reply, so a count stops short of it.
fn adjusted(count: u32, delta: i32) -> u32 {
    (i64::from(count) + i64::from(delta)).clamp(0, i64::from(u32::MAX - 1)) as u32
}

/// Add `delta` to `member`'s reference count, clamped at 0, and delete the
/// member when it reaches 0 (5.3). Returns the new count, or `None` when
/// the library has no such member, or, changing nothing, when a
/// modification count it advances cannot ([`adjust_counter_refusal`]). A
/// delta of 0 only reads the count.
pub fn adjust(library: &mut OwnedNode, member: &[u8], delta: i32) -> Option<u32> {
    let (count, changes) = read(adjust_changes(library, member, delta))?;
    apply(library, changes);
    Some(count)
}

/// The changes an [`adjust`] of `member` by `delta` makes in the library
/// `library` reads, with the new count. A member that goes takes its
/// entries in the header with it, and an MD5 index entry left without
/// members goes too; the last issued id stays.
pub(crate) fn adjust_changes(
    library: &dyn Reads,
    member: &[u8],
    delta: i32,
) -> Result<Option<(u32, Vec<Change>)>, ReadError> {
    let at: [&[u8]; 2] = [&[keys::MEMBERS], member];
    if library.value(&at)?.is_none() {
        return Ok(None);
    }
    let count = reference_count_at(library, member)?;
    if delta == 0 {
        return Ok(Some((count, Vec::new())));
    }
    let new = adjusted(count, delta);
    let deleted = deletes(count, delta);
    let header: &[u8] = &[keys::HEADER];
    let has_header = library.value(&[header])?.is_some();
    // The member's count and, when it goes, the header's, before anything
    // changes.
    let Some(member_count) = serial_at(library, &[at[0], at[1], &[MOD_COUNT]])?.checked_add(1)
    else {
        return Ok(None);
    };
    let header_count = match deleted && has_header {
        true => match serial_at(library, &[header, &[MOD_COUNT]])?.checked_add(1) {
            Some(count) => Some(count),
            None => return Ok(None),
        },
        false => None,
    };
    let below =
        |rest: &[&[u8]]| -> Vec<Vec<u8>> { at.iter().chain(rest).map(|s| s.to_vec()).collect() };
    if !deleted {
        let count = OwnedNode::leaf(vec![keys::REF_COUNT], new.to_be_bytes());
        return Ok(Some((
            new,
            vec![
                (below(&[&[keys::REF_COUNT]]), Some(count)),
                (
                    below(&[&[MOD_COUNT]]),
                    Some(serial_leaf(MOD_COUNT, member_count)),
                ),
            ],
        )));
    }
    let md5 = library.value(&[at[0], at[1], &[keys::MD5]])?.flatten();
    let mut changes: Vec<Change> = vec![(below(&[]), None)];
    if !has_header {
        return Ok(Some((new, changes)));
    }
    let in_header = |rest: &[&[u8]]| -> Vec<Vec<u8>> {
        std::iter::once(header)
            .chain(rest.iter().copied())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let mut listed = false;
    for list in [keys::MEMBER_LIST, keys::GLYPHS] {
        if library.value(&[header, &[list], member])?.is_some() {
            listed |= list == keys::MEMBER_LIST;
            changes.push((in_header(&[&[list], member]), None));
        }
    }
    if let Some(md5) = md5 {
        let index: [&[u8]; 3] = [header, &[keys::MD5_INDEX], &md5];
        if library.value(&index)?.is_some() {
            let first = library.first(&index, 2)?;
            if first.iter().all(|key| key.as_slice() == member) {
                // Left without members, the entry goes.
                changes.push((in_header(&index[1..]), None));
            } else if library
                .value(&[index[0], index[1], index[2], member])?
                .is_some()
            {
                changes.push((in_header(&[index[1], index[2], member]), None));
            }
        }
    }
    // The header's member count `04`, from its member list.
    let count = library.count(&[header, &[keys::MEMBER_LIST]])? - usize::from(listed);
    let count = OwnedNode::leaf(vec![keys::MEMBER_COUNT], (count as u32).to_be_bytes());
    changes.push((in_header(&[&[keys::MEMBER_COUNT]]), Some(count)));
    if let Some(count) = header_count {
        changes.push((
            in_header(&[&[MOD_COUNT]]),
            Some(serial_leaf(MOD_COUNT, count)),
        ));
    }
    Ok(Some((new, changes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modcount::{serial_value, MOD_COUNT};

    /// A binary-data tree as FileMaker Pro uploads it: directory, MD5 and
    /// streams, as a bracketed slot list.
    fn upload(streams: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut directory = (streams.len() as u16).to_be_bytes().to_vec();
        for (kind, bytes) in streams {
            directory.extend(kind.as_slice());
            directory.extend((bytes.len() as u32).to_be_bytes());
        }
        let hash = md5(streams.iter().map(|(k, b)| (k.as_slice(), *b)));
        let mut keys = vec![
            OwnedNode::leaf(vec![keys::DIRECTORY], directory),
            OwnedNode::leaf(vec![keys::MD5], hash.to_vec()),
        ];
        keys.extend(streams.iter().map(|(k, b)| stream_node(k.as_slice(), b)));
        OwnedNode::list(Vec::new(), keys).to_wire().unwrap()
    }

    fn ids() -> impl FnMut() -> [u8; 16] {
        let mut n = 0u8;
        move || {
            n += 1;
            [n; 16]
        }
    }

    fn picture(name: &str) -> BinaryData {
        let png = [0x89, b'P', b'N', b'G'].repeat(600);
        BinaryData::parse(&upload(&[
            (b"FNAM", name.as_bytes()),
            (b"PNGf", &png),
            (b"SIZE", &[0, 16, 0, 16]),
        ]))
        .unwrap()
    }

    fn value<'a>(library: &'a OwnedNode, path: &[&[u8]]) -> &'a [u8] {
        library.at(path).unwrap().value.as_deref().unwrap()
    }

    #[test]
    fn checks_the_directory_and_md5() {
        let data = picture("a.png");
        // Long streams are stored chunked, as the file stores them.
        let png = data.keys.iter().find(|k| k.segment == b"PNGf").unwrap();
        assert_eq!(png.children.len(), 4);
        assert_eq!(png.assembled().unwrap().len(), 2400);

        let mut tampered = upload(&[(b"FNAM", b"a.png")]);
        let at = tampered.windows(5).position(|w| w == b"a.png").unwrap();
        tampered[at] = b'b';
        assert!(BinaryData::parse(&tampered).unwrap_err().contains("MD5"));
        // ENCR is left out of the MD5.
        assert!(BinaryData::parse(&upload(&[(b"ENCR", b"key"), (b"FILE", b"x")])).is_ok());
        assert!(BinaryData::parse(&[0x80, 0x40]).is_err());
        // A stream in pieces of 1000 bytes stays within the tree limit; a
        // payload of short keys, tens of times its size as a tree, does not.
        assert!(BinaryData::parse(&upload(&[(b"FILE", &vec![7; 1 << 20])])).is_ok());
        let mut short = vec![0x80];
        for key in 0..20_000u16 {
            short.push(0x08);
            short.extend(key.to_be_bytes());
        }
        short.push(0x40);
        assert!(BinaryData::parse(&short).unwrap_err().contains("memory"));
    }

    /// FileMaker Server's library through live:c1-c5: create, deduplicate,
    /// release, replace, delete.
    #[test]
    fn follows_filemaker_servers_library_rules() {
        use keys::*;
        let mut library = OwnedNode::default();
        let mut ids = ids();
        let first = picture("a.png");
        assert_eq!(
            store(&mut library, first.clone(), &mut ids),
            Some(Stored::New(vec![1]))
        );
        // live:c1/a2_8002_1f.
        let header = |key: u8, library: &OwnedNode| -> Vec<u8> {
            value(library, &[&[HEADER], &[key]]).to_vec()
        };
        assert_eq!(header(LAST_ID, &library), [1, 1]);
        assert_eq!(header(MEMBER_COUNT, &library), [0, 0, 0, 1]);
        assert_eq!(header(MOD_COUNT, &library), [1, 2]);
        assert!(library
            .at(&[&[HEADER], &[GLYPHS]])
            .unwrap()
            .children
            .is_empty());
        assert!(library
            .at(&[&[HEADER], &[MD5_INDEX], &first.md5, &[1]])
            .is_some());
        let m1 = |key: &[u8], library: &OwnedNode| -> Option<Vec<u8>> {
            library
                .at(&[&[MEMBERS], &[1], key])
                .and_then(|n| n.value.clone())
        };
        assert_eq!(m1(&[REF_COUNT], &library).unwrap(), [0, 0, 0, 1]);
        assert_eq!(m1(&[MOD_COUNT], &library), None);
        assert_eq!(m1(b"FNAM", &library).unwrap(), b"a.png");

        // live:c2: the same data again counts the existing member.
        assert_eq!(
            store(&mut library, first.clone(), &mut ids),
            Some(Stored::Existing(vec![1]))
        );
        assert_eq!(m1(&[REF_COUNT], &library).unwrap(), [0, 0, 0, 2]);
        assert_eq!(m1(&[MOD_COUNT], &library).unwrap(), [1, 1]);
        assert_eq!(header(MOD_COUNT, &library), [1, 2]);

        // live:c3: release one, read, store new data, release the old.
        assert_eq!(adjust(&mut library, &[1], -1), Some(1));
        assert_eq!(adjust(&mut library, &[1], 0), Some(1));
        assert_eq!(
            store(&mut library, picture("b.png"), &mut ids),
            Some(Stored::New(vec![2]))
        );
        assert_eq!(adjust(&mut library, &[1], -1), Some(0));
        assert!(member(&library, &[1]).is_none());
        assert!(library.at(&[&[HEADER], &[MD5_INDEX], &first.md5]).is_none());
        assert_eq!(header(LAST_ID, &library), [1, 2]);
        assert_eq!(header(MEMBER_COUNT, &library), [0, 0, 0, 1]);
        assert_eq!(header(MOD_COUNT, &library), [1, 5]);

        // live:c5: deleting the last member leaves an empty header; a
        // delta below the count clamps at 0; ids are never reused.
        assert_eq!(adjust(&mut library, &[2], -5), Some(0));
        assert_eq!(header(MEMBER_COUNT, &library), [0, 0, 0, 0]);
        assert!(library
            .at(&[&[HEADER], &[MEMBER_LIST]])
            .unwrap()
            .children
            .is_empty());
        assert!(library
            .at(&[&[HEADER], &[MD5_INDEX]])
            .unwrap()
            .children
            .is_empty());
        assert_eq!(adjust(&mut library, &[2], 1), None);
        assert_eq!(
            store(&mut library, first, &mut ids),
            Some(Stored::New(vec![3]))
        );
        // The header's unique id was made once.
        assert_eq!(header(UNIQUE_ID, &library), [1; 16]);
    }

    #[test]
    fn op87_paths_name_a_library_member() {
        let path = |p: &[&[u8]]| p.iter().map(|s| s.to_vec()).collect::<Vec<_>>();
        let table: &[u8] = &[0x80, 0x02];
        assert_eq!(
            member_path(&path(&[table, &[0x1f], &[0x05], &[0x01]])),
            Some((Some(table), &[0x01][..]))
        );
        assert_eq!(
            member_path(&path(&[&[0x1f], &[0x05], &[0x80, 0x05]])),
            Some((None, &[0x80, 0x05][..]))
        );
        // The empty key FileMaker Server took for a member (section 10).
        assert_eq!(member_path(&[]), None);
        assert_eq!(member_path(&path(&[table, &[0x1f], &[0x05]])), None);
        assert_eq!(member_path(&path(&[table, &[0x05], &[0x01]])), None);
        assert_eq!(member_path(&path(&[&[0x1f], &[0x05], &[0x80]])), None);
    }

    #[test]
    fn members_read_as_container_data() {
        use om_data::containers::{fnam, read, reference, MemberInfo};
        let png = [0x89, b'P', b'N', b'G'].repeat(600);
        let mut library = OwnedNode::default();
        let data = BinaryData::parse(&upload(&[
            (b"DPI_", &[0, 72, 0, 72]),
            (b"FNAM", &fnam("image", "Ångström.png")),
            (b"PNGf", &png),
            (b"SIZE", &[0, 16, 0, 16]),
        ]))
        .unwrap();
        let md5 = data.md5;
        assert_eq!(store(&mut library, data, ids()), Some(Stored::New(vec![1])));
        let member = super::member(&library, &[1]).unwrap().clone();
        let member = &member;
        let info = MemberInfo::read(member).unwrap();
        assert_eq!((info.name.as_str(), info.md5), ("Ångström.png", md5));
        // The name is the text; the main stream comes first, the others in
        // directory order, chunked ones assembled; intact.
        let stored = read(member).unwrap();
        let container = &stored.data;
        assert_eq!(container.text.as_str(), "Ångström.png");
        let kinds: Vec<&[u8; 4]> = container.streams.iter().map(|(k, _)| &k.0).collect();
        assert_eq!(kinds, [b"PNGf", b"DPI_", b"SIZE"]);
        assert_eq!(container.data(), png.as_slice());
        assert!(stored.intact);

        // Data that no longer matches its MD5 reads, and is not intact.
        let mut damaged = member.clone();
        damaged.set_child(OwnedNode::leaf(b"SIZE".to_vec(), vec![0, 1, 0, 1]));
        assert!(!read(&damaged).unwrap().intact);

        // `MAIN` names the main stream.
        let named = BinaryData::parse(&upload(&[
            (b"JPEG", b"jpeg"),
            (b"MAIN", b"PNGf"),
            (b"PNGf", b"png"),
        ]))
        .unwrap();
        assert_eq!(
            store(&mut library, named, ids()),
            Some(Stored::New(vec![2]))
        );
        let named = read(super::member(&library, &[2]).unwrap()).unwrap().data;
        assert_eq!(named.data(), b"png");
        assert_eq!(named.text.as_str(), "");

        // References: the member key in hexadecimal, then U+E000.
        assert_eq!(reference("01\u{e000}"), Some(vec![0x01]));
        assert_eq!(reference("photo.png"), None);
    }

    /// The keys a store changes, which a commit keeps to put back, come
    /// from the decision the store carries out: a member the MD5 index
    /// names under a key that is not a serial key is not reused, so the
    /// store adds a new member and changes the header, and that is what is
    /// kept, not the named member's counts.
    #[test]
    fn a_store_plan_names_what_its_store_changes() {
        let mut library = OwnedNode::default();
        let first = picture("a.png");
        assert_eq!(store_plan(None, &first.md5).keys(None), None);
        store(&mut library, first.clone(), ids());
        let second = picture("b.png");
        let new = store_plan(Some(&library), &second.md5);
        assert_eq!(new, StorePlan::New);
        assert_eq!(
            new.keys(Some(&library)).unwrap(),
            vec![vec![vec![keys::HEADER]], vec![vec![keys::MEMBERS], vec![2]]]
        );
        let reused = store_plan(Some(&library), &first.md5);
        assert_eq!(reused, StorePlan::Existing(vec![1]));
        assert_eq!(
            reused.keys(Some(&library)).unwrap(),
            count_keys(&[1]).to_vec()
        );

        // The index names member `ff`, which is not a serial key.
        let third = picture("c.png");
        let index = [&[keys::HEADER][..], &[keys::MD5_INDEX], &third.md5];
        library.put(
            &index,
            OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![0xff], Vec::new())]),
        );
        library.put(
            &[&[keys::MEMBERS], &[0xff]],
            OwnedNode::list(Vec::new(), Vec::new()),
        );
        let plan = store_plan(Some(&library), &third.md5);
        assert_eq!(plan, StorePlan::New);
        let kept = plan.keys(Some(&library)).unwrap();
        let mut changed = library.clone();
        assert_eq!(
            store_as(&mut changed, third, &plan, ids()),
            Some(Stored::New(vec![2]))
        );
        // The header and the new member changed, as kept; the named
        // member did not.
        assert_eq!(
            kept,
            vec![vec![vec![keys::HEADER]], vec![vec![keys::MEMBERS], vec![2]]]
        );
        assert_ne!(
            changed.child(&[keys::HEADER]),
            library.child(&[keys::HEADER])
        );
        assert!(changed.at(&[&[keys::MEMBERS], &[2]]).is_some());
        let named: [&[u8]; 2] = [&[keys::MEMBERS], &[0xff]];
        assert_eq!(changed.at(&named), library.at(&named));
    }

    #[test]
    fn glyphs_are_flagged() {
        let mut library = OwnedNode::default();
        let data = BinaryData::parse(&upload(&[(b"GLPH", &[1]), (b"SVG ", b"<svg/>")])).unwrap();
        assert!(data.glyph);
        assert_eq!(store(&mut library, data, ids()), Some(Stored::New(vec![1])));
        assert!(library
            .at(&[&[keys::HEADER], &[keys::GLYPHS], &[1]])
            .is_some());
        assert_eq!(
            value(&library, &[&[keys::MEMBERS], &[1], &[keys::PRIVATE_FLAGS]]),
            GLYPH_FLAGS
        );
        adjust(&mut library, &[1], -1);
        assert!(library
            .at(&[&[keys::HEADER], &[keys::GLYPHS]])
            .unwrap()
            .children
            .is_empty());
    }

    /// Counters a library holds at the largest a serial key holds (a Full
    /// Access catalog upload can set them, #361): a store or an adjustment
    /// that would advance one past it is refused and changes nothing, rather
    /// than wrapping around to a used member id or count.
    #[test]
    fn counters_at_the_largest_refuse_and_change_nothing() {
        let mut ids = ids();
        let mut library = OwnedNode::default();
        assert_eq!(
            store(&mut library, picture("a.png"), &mut ids),
            Some(Stored::New(vec![1]))
        );
        let other = picture("b.png");
        let with = |path: &[&[u8]], key: u8, value: u64| {
            let mut changed = library.clone();
            changed.put(path, serial_leaf(key, value));
            changed
        };
        let refused = |mut library: OwnedNode, data: Option<&BinaryData>, delta: i32| {
            let before = library.clone();
            match data {
                Some(data) => {
                    assert!(store_counter_refusal(&library, &data.md5)
                        .unwrap()
                        .is_some());
                    assert_eq!(store(&mut library, data.clone(), || [7; 16]), None);
                }
                None => {
                    assert!(adjust_counter_refusal(&library, &[1], delta)
                        .unwrap()
                        .is_some());
                    assert_eq!(adjust(&mut library, &[1], delta), None);
                }
            }
            assert_eq!(library, before);
        };

        // A new member takes the next id and advances the header by 2.
        let last_id = with(
            &[&[keys::HEADER], &[keys::LAST_ID]],
            keys::LAST_ID,
            u64::MAX,
        );
        assert_eq!(next_member(&last_id).unwrap(), None);
        refused(last_id, Some(&other), 0);
        let header = |count| with(&[&[keys::HEADER], &[MOD_COUNT]], MOD_COUNT, count);
        refused(header(u64::MAX - 1), Some(&other), 0);
        let mut room = header(u64::MAX - 2);
        assert_eq!(
            store(&mut room, other.clone(), || [7; 16]),
            Some(Stored::New(vec![2]))
        );
        assert_eq!(
            serial_value(room.at(&[&[keys::HEADER], &[MOD_COUNT]])),
            u64::MAX
        );

        // Reusing or adjusting a member advances its count by 1.
        let member = with(&[&[keys::MEMBERS], &[1], &[MOD_COUNT]], MOD_COUNT, u64::MAX);
        refused(member.clone(), Some(&picture("a.png")), 0);
        refused(member, None, -1);
        // Releasing its last reference also advances the header's by 1.
        refused(header(u64::MAX), None, -1);
        let mut kept = header(u64::MAX);
        assert_eq!(adjust(&mut kept, &[1], 1), Some(2));
    }
}
