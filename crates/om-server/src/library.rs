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

use om_data::containers::{md5, stream_node};
use om_hbam::tree::read_owned;
use om_hbam::writer::value_on_path;
use om_hbam::{serial, OwnedNode, SlotReader};

use crate::modcount::{serial_leaf, serial_value, MOD_COUNT};

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

/// Store `data` in `library` (3.4): count + 1 on the member that already
/// holds data with its MD5, otherwise a new member with count 1. The header
/// is created on first use. `unique_id` makes the random ids (`d8`).
/// `None`, changing nothing, when a counter the store advances cannot
/// ([`store_counter_refusal`]).
pub fn store(
    library: &mut OwnedNode,
    data: BinaryData,
    mut unique_id: impl FnMut() -> [u8; 16],
) -> Option<Stored> {
    // Members are named by serial keys (containers.md), and op88's reply
    // names the one it reuses in a key of at most 255 bytes: a member
    // whose key in the file is not a serial key is not reused.
    if let Some(existing) =
        reused_member(library, &data.md5).filter(|m| member(library, m).is_some())
    {
        return adjust(library, &existing, 1).map(|_| Stored::Existing(existing));
    }
    // Both counters the new member advances, before anything changes.
    let header = library.child(&[keys::HEADER]);
    let id = serial_value(header.and_then(|h| h.child(&[keys::LAST_ID]))).checked_add(1)?;
    let header_count = serial_value(header.and_then(|h| h.child(&[MOD_COUNT]))).checked_add(2)?;
    let new_library = header.is_none();
    let header = library.ensure(&[&[keys::HEADER]]);
    if new_library {
        header.set_child(OwnedNode::leaf(
            vec![keys::MEMBER_COUNT],
            0u32.to_be_bytes(),
        ));
        for list in [keys::GLYPHS, keys::MEMBER_LIST, keys::MD5_INDEX] {
            header.set_child(OwnedNode::list(vec![list], Vec::new()));
        }
        header.set_child(OwnedNode::leaf(vec![keys::UNIQUE_ID], unique_id()));
    }
    let member = serial::encode(id);
    header.set_child(serial_leaf(keys::LAST_ID, id));
    header
        .ensure(&[&[keys::MEMBER_LIST]])
        .set_child(OwnedNode::leaf(member.clone(), Vec::new()));
    let index = OwnedNode::list(
        data.md5.to_vec(),
        vec![OwnedNode::leaf(member.clone(), Vec::new())],
    );
    header.ensure(&[&[keys::MD5_INDEX]]).set_child(index);
    if data.glyph {
        header
            .ensure(&[&[keys::GLYPHS]])
            .set_child(OwnedNode::leaf(member.clone(), Vec::new()));
    }
    recount(header);
    // FileMaker Server's catalog count goes up by two per new member
    // (live:c1, c3, c4) and by one per deleted member.
    header.set_child(serial_leaf(MOD_COUNT, header_count));
    let mut node = OwnedNode::list(member.clone(), data.keys);
    node.set_child(OwnedNode::leaf(vec![keys::REF_COUNT], 1u32.to_be_bytes()));
    node.set_child(OwnedNode::leaf(vec![keys::UNIQUE_ID], unique_id()));
    if data.glyph {
        node.set_child(OwnedNode::leaf(vec![keys::PRIVATE_FLAGS], GLYPH_FLAGS));
    }
    library.ensure(&[&[keys::MEMBERS]]).set_child(node);
    Some(Stored::New(member))
}

/// The member a [`store`] of data with `md5` reuses: the first one the
/// header's MD5 index lists for it, when its key is a serial key.
pub(crate) fn reused_member(library: &OwnedNode, md5: &[u8; 16]) -> Option<Vec<u8>> {
    library
        .at(&[&[keys::HEADER], &[keys::MD5_INDEX], md5])
        .and_then(|n| n.children.first())
        .map(|m| m.segment.clone())
        .filter(|member| serial::decode(member).is_some())
}

/// The first node, relative to `library`, that a [`store`] of data with
/// `md5` would write beneath and that holds a value
/// (`om_hbam::writer::value_on_path`): the header and its lists, the
/// members, the member it would reuse. Such a library is not one FileMaker
/// writes; the store is refused rather than make a list of a value.
pub fn store_refusal(library: &OwnedNode, md5: &[u8; 16]) -> Option<Vec<Vec<u8>>> {
    let header: &[u8] = &[keys::HEADER];
    let paths: [&[&[u8]]; 4] = [
        &[header, &[keys::MEMBER_LIST]],
        &[header, &[keys::MD5_INDEX]],
        &[header, &[keys::GLYPHS]],
        &[&[keys::MEMBERS]],
    ];
    paths
        .iter()
        .find_map(|path| value_on_path(library, path))
        .or_else(|| adjust_refusal(library, &reused_member(library, md5)?))
}

/// The key [`store`] gives a new member of `library`: one more than the
/// header's last issued id (3.4). `None` when that id is the largest a
/// serial key holds.
pub fn next_member(library: Option<&OwnedNode>) -> Option<Vec<u8>> {
    let last = library.and_then(|l| l.at(&[&[keys::HEADER], &[keys::LAST_ID]]));
    serial_value(last).checked_add(1).map(serial::encode)
}

/// Why a [`store`] of data with `md5` into `library` cannot advance the
/// counters it must: the reused member's ([`adjust_counter_refusal`]), or
/// for a new member the header's last issued id (by 1) and modification
/// count (by 2). These come from the file, which a Full Access catalog
/// upload can set; past the largest a serial key holds the store is
/// refused rather than wrapped around. For a log line; `None` when it can.
pub fn store_counter_refusal(library: &OwnedNode, md5: &[u8; 16]) -> Option<&'static str> {
    if let Some(existing) = reused_member(library, md5).filter(|m| member(library, m).is_some()) {
        return adjust_counter_refusal(library, &existing, 1);
    }
    let header = library.child(&[keys::HEADER]);
    if next_member(Some(library)).is_none() {
        return Some("the library's last member id is the largest a serial key holds");
    }
    serial_value(header.and_then(|h| h.child(&[MOD_COUNT])))
        .checked_add(2)
        .is_none()
        .then_some("the library header's modification count cannot advance")
}

/// Why an [`adjust`] of `member`'s count by `delta` cannot advance the
/// counters it must: the member's modification count (by 1) and, when the
/// member goes, the header's (by 1). For a log line; `None` when it can, or
/// when there is no such member or nothing to change.
pub fn adjust_counter_refusal(
    library: &OwnedNode,
    member: &[u8],
    delta: i32,
) -> Option<&'static str> {
    let node = library.at(&[&[keys::MEMBERS], member])?;
    if delta == 0 {
        return None;
    }
    if serial_value(node.child(&[MOD_COUNT]))
        .checked_add(1)
        .is_none()
    {
        return Some("the member's modification count cannot advance");
    }
    let header = library.child(&[keys::HEADER])?;
    let goes = adjusted(reference_count(node), delta) == 0;
    (goes
        && serial_value(header.child(&[MOD_COUNT]))
            .checked_add(1)
            .is_none())
    .then_some("the library header's modification count cannot advance")
}

/// The members node of a library, `05`.
pub const MEMBERS: u8 = keys::MEMBERS;

/// The first node, relative to `library`, that an [`adjust`] of `member`
/// would write beneath and that holds a value: the members or the member.
pub fn adjust_refusal(library: &OwnedNode, member: &[u8]) -> Option<Vec<Vec<u8>>> {
    value_on_path(library, &[&[keys::MEMBERS], member])
}

/// Set the header's member count `04` from its member list.
fn recount(header: &mut OwnedNode) {
    let count = header
        .child(&[keys::MEMBER_LIST])
        .map_or(0, |list| list.children.len()) as u32;
    header.set_child(OwnedNode::leaf(
        vec![keys::MEMBER_COUNT],
        count.to_be_bytes(),
    ));
}

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
/// its count stays above 0 (its count and its modification count), for a
/// caller that may have to put them back.
pub fn count_keys(member: &[u8]) -> [Vec<Vec<u8>>; 2] {
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

/// The keys under a library that a [`store`] of data with `md5` changes,
/// for a caller that may have to put them back: the count keys of the
/// member that already holds the data, else the header and the new member.
/// `None` when there is no library yet: all of it is new.
pub fn store_keys(library: Option<&OwnedNode>, md5: &[u8; 16]) -> Option<Vec<Vec<Vec<u8>>>> {
    let library = library?;
    let existing = library
        .at(&[&[keys::HEADER], &[keys::MD5_INDEX], md5])
        .and_then(|n| n.children.first())
        .map(|m| m.segment.clone())
        .filter(|m| member(library, m).is_some());
    Some(match existing {
        Some(existing) => count_keys(&existing).into_iter().collect(),
        None => {
            let Some(next) = next_member(Some(library)) else {
                // No member can be added ([`store_counter_refusal`]).
                return Some(vec![vec![vec![keys::HEADER]]]);
            };
            vec![vec![vec![keys::HEADER]], vec![vec![keys::MEMBERS], next]]
        }
    })
}

/// The keys under a library that an [`adjust`] deleting `member` changes:
/// the member and the header that lists it.
pub fn deletion_keys(member: &[u8]) -> [Vec<Vec<u8>>; 2] {
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
    let node = library.at(&[&[keys::MEMBERS], member])?;
    let count = reference_count(node);
    if delta == 0 {
        return Some(count);
    }
    let new = adjusted(count, delta);
    // The member's count and, when it goes, the header's, before anything
    // changes.
    let member_count = serial_value(node.child(&[MOD_COUNT])).checked_add(1)?;
    let header_count = match library.child(&[keys::HEADER]).filter(|_| new == 0) {
        Some(header) => Some(serial_value(header.child(&[MOD_COUNT])).checked_add(1)?),
        None => None,
    };
    let node = library.at_mut(&[&[keys::MEMBERS], member])?;
    node.set_child(OwnedNode::leaf(vec![keys::REF_COUNT], new.to_be_bytes()));
    node.set_child(serial_leaf(MOD_COUNT, member_count));
    if new == 0 {
        delete(library, member, header_count);
    }
    Some(new)
}

/// Remove `member` and its header entries; the last issued id stays. The
/// header's modification count becomes `header_count`, the caller's
/// checked advance of it.
fn delete(library: &mut OwnedNode, member: &[u8], header_count: Option<u64>) {
    let Some(node) = library.remove(&[&[keys::MEMBERS], member]) else {
        return;
    };
    let Some(header) = library.child_mut(&[keys::HEADER]) else {
        return;
    };
    for list in [keys::MEMBER_LIST, keys::GLYPHS] {
        header.remove(&[&[list], member]);
    }
    if let Some(md5) = node.child(&[keys::MD5]).and_then(|n| n.value.as_deref()) {
        if let Some(index) = header.at_mut(&[&[keys::MD5_INDEX], md5]) {
            index.remove(&[member]);
            if index.children.is_empty() {
                header.remove(&[&[keys::MD5_INDEX], md5]);
            }
        }
    }
    recount(header);
    if let Some(count) = header_count {
        header.set_child(serial_leaf(MOD_COUNT, count));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modcount::MOD_COUNT;

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

    /// The keys a store changes, which a commit keeps to put back.
    #[test]
    fn store_keys_name_what_a_store_changes() {
        let mut library = OwnedNode::default();
        let first = picture("a.png");
        assert_eq!(store_keys(None, &first.md5), None);
        store(&mut library, first.clone(), ids());
        let second = picture("b.png");
        assert_eq!(
            store_keys(Some(&library), &second.md5).unwrap(),
            vec![vec![vec![keys::HEADER]], vec![vec![keys::MEMBERS], vec![2]]]
        );
        assert_eq!(
            store_keys(Some(&library), &first.md5).unwrap(),
            count_keys(&[1]).to_vec()
        );
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
                    assert!(store_counter_refusal(&library, &data.md5).is_some());
                    assert_eq!(store(&mut library, data.clone(), || [7; 16]), None);
                }
                None => {
                    assert!(adjust_counter_refusal(&library, &[1], delta).is_some());
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
        assert_eq!(next_member(Some(&last_id)), None);
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
