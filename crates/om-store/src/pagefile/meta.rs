//! A checkpoint's meta stream (`docs/server-storage-format.md` section
//! 4.3 and 4.4): the roots of the state's trees, the restore points, and
//! the map of free pages, as one byte stream kept in pages of the overflow
//! kind behind a chain of directory pages. The superblock names the first
//! directory page by number and hash, each directory page names the next
//! and its data pages the same way, so every byte of the stream is checked
//! by a hash before it is used.

use std::sync::Arc;

use super::map::{Image, Pending, Share, Version};
use super::{EntryId, RestorePoint, TreeName};
use crate::page::{Builder, Hash, Page, PageFault, PageKind, PageNo, OVERFLOW_DATA};
use crate::tree::Root;

const MAGIC: [u8; 8] = *b"OMMETA\0\0";

/// The meta stream format version this code writes, and the newest it
/// reads.
pub const VERSION: u16 = 1;

/// A reference to a page: its number and hash.
const REF: usize = 40;

/// The references one directory page holds after the reference to the
/// next one.
pub(crate) const DIR_ENTRIES: usize = (OVERFLOW_DATA - REF) / REF;

/// The most trees a checkpoint names. Opening reads and checks every
/// root, a page at a time: at the limit, 65,536 reads of 16 KiB, and a
/// table of at most 19 MiB in memory.
pub const MAX_TREES: usize = 1 << 16;

/// The most restore points a checkpoint lists (and as many retiring).
pub const MAX_RESTORE_POINTS: usize = 1 << 16;

/// The longest restore point name, in bytes.
pub const MAX_RESTORE_NAME: usize = 64;

/// What a checkpoint's meta stream holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    pub number: u64,
    pub entry: EntryId,
    pub tree_format: u8,
    /// The checkpoint before it, kept on disk while this one is the newest:
    /// its number and version, and the first page of its meta stream, which
    /// identifies it (a number whose checkpoint never reached the disk can
    /// be given again).
    pub predecessor: Option<(u64, Version, (PageNo, Hash))>,
    /// The state's trees, by name, in name order.
    pub trees: Vec<(TreeName, Root)>,
    /// The restore points this checkpoint lists.
    pub restore_points: Vec<RestorePoint>,
    /// The restore points the predecessor lists and this one does not:
    /// kept while the predecessor is on disk.
    pub retiring: Vec<RestorePoint>,
    pub map: Image,
}

const HEADER: usize = 16;
const FIXED: usize = HEADER + 8 + 48 + 8 + 8 + 16 + REF + 8;
const PENDING: usize = 8 + 32 + 16 + 1 + 16 + 8;
const SHARE: usize = 8 + 4 + 16;

fn restore_len(rp: &RestorePoint) -> usize {
    1 + rp.name.len() + 8 + 48 + 16 + REF
}

/// The length of the stream of a checkpoint that names trees whose names
/// take `tree_names` bytes in all, the restore points given, and a map of
/// `end` pages with `pending` dying pages and `shares` shared ones.
pub(crate) fn stream_len(
    trees: usize,
    tree_names: usize,
    restore_points: &[RestorePoint],
    retiring: &[RestorePoint],
    end: PageNo,
    pending: usize,
    shares: usize,
) -> usize {
    FIXED
        + 4
        + trees * (1 + REF)
        + tree_names
        + 4
        + restore_points.iter().map(restore_len).sum::<usize>()
        + 4
        + retiring.iter().map(restore_len).sum::<usize>()
        + 8
        + end.div_ceil(8) as usize
        + 8
        + pending * PENDING
        + 8
        + shares * SHARE
}

/// The pages a stream of `len` bytes takes, directory pages included.
pub(crate) fn pages_for(len: usize) -> usize {
    let data = len.div_ceil(OVERFLOW_DATA).max(1);
    data + data.div_ceil(DIR_ENTRIES)
}

fn put_entry(out: &mut Vec<u8>, entry: &EntryId) {
    out.extend_from_slice(&entry.index.to_le_bytes());
    out.extend_from_slice(&entry.term.to_le_bytes());
    out.extend_from_slice(&entry.hash.0);
}

fn put_version(out: &mut Vec<u8>, v: Version) {
    out.extend_from_slice(&v.entry.to_le_bytes());
    out.extend_from_slice(&v.step.to_le_bytes());
}

fn put_restore(out: &mut Vec<u8>, rp: &RestorePoint) {
    out.push(rp.name.len() as u8);
    out.extend_from_slice(rp.name.as_bytes());
    out.extend_from_slice(&rp.checkpoint.to_le_bytes());
    put_entry(out, &rp.entry);
    put_version(out, rp.version);
    out.extend_from_slice(&rp.meta.0.to_le_bytes());
    out.extend_from_slice(&rp.meta.1 .0);
}

impl Meta {
    pub fn encode(&self) -> Vec<u8> {
        let names: usize = self.trees.iter().map(|(n, _)| n.as_bytes().len()).sum();
        let len = stream_len(
            self.trees.len(),
            names,
            &self.restore_points,
            &self.retiring,
            self.map.end,
            self.map.pending.len(),
            self.map.shares.len(),
        );
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&self.number.to_le_bytes());
        put_entry(&mut out, &self.entry);
        out.push(self.tree_format);
        out.extend_from_slice(&[0; 7]);
        let (pn, pv, pm) =
            self.predecessor
                .unwrap_or((0, Version::default(), (0, Hash::default())));
        out.extend_from_slice(&pn.to_le_bytes());
        put_version(&mut out, pv);
        out.extend_from_slice(&pm.0.to_le_bytes());
        out.extend_from_slice(&pm.1 .0);
        out.extend_from_slice(&self.map.end.to_le_bytes());
        out.extend_from_slice(&(self.trees.len() as u32).to_le_bytes());
        for (name, root) in &self.trees {
            out.push(name.as_bytes().len() as u8);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&root.page.to_le_bytes());
            out.extend_from_slice(&root.hash.0);
        }
        for list in [&self.restore_points, &self.retiring] {
            out.extend_from_slice(&(list.len() as u32).to_le_bytes());
            for rp in list {
                put_restore(&mut out, rp);
            }
        }
        out.extend_from_slice(&(self.map.free.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.map.free);
        out.extend_from_slice(&(self.map.pending.len() as u64).to_le_bytes());
        for (n, p) in &self.map.pending {
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&p.hash.0);
            put_version(&mut out, p.death);
            out.push(u8::from(p.birth.is_some()) | (u8::from(p.expand) << 1));
            put_version(&mut out, p.birth.unwrap_or_default());
            out.extend_from_slice(&p.hold.to_le_bytes());
        }
        out.extend_from_slice(&(self.map.shares.len() as u64).to_le_bytes());
        for (n, s) in &self.map.shares {
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&s.count.to_le_bytes());
            put_version(&mut out, s.last);
        }
        debug_assert_eq!(out.len(), len);
        out
    }

    /// The meta a stream holds, checked against the format. Every count is
    /// checked against the bytes left before anything is allocated for it.
    pub fn decode(stream: &[u8]) -> Result<Meta, &'static str> {
        let mut r = Reader { b: stream, at: 0 };
        if r.take(8)? != MAGIC {
            return Err("not a meta stream");
        }
        let version = r.u16()?;
        if version != VERSION {
            return Err("a meta stream of another format version");
        }
        if r.take(6)?.iter().any(|&b| b != 0) {
            return Err("reserved bytes that are not zero");
        }
        let number = r.u64()?;
        let entry = r.entry()?;
        let tree_format = r.u8()?;
        if r.take(7)?.iter().any(|&b| b != 0) {
            return Err("reserved bytes that are not zero");
        }
        let pn = r.u64()?;
        let pv = r.version()?;
        let pm = (r.u64()?, r.hash()?);
        let predecessor = (pn != 0).then_some((pn, pv, pm));
        if pn == 0 && (pv != Version::default() || pm != (0, Hash::default())) {
            return Err("a version without a predecessor");
        }
        if pn != 0 && pm.0 == 0 {
            return Err("a predecessor without its meta stream");
        }
        let end = r.u64()?;
        let count = r.count(4, 1 + REF + 1)?;
        if count > MAX_TREES {
            return Err("too many trees");
        }
        let mut trees: Vec<(TreeName, Root)> = Vec::with_capacity(count);
        for _ in 0..count {
            let len = usize::from(r.u8()?);
            let name = TreeName::new(r.take(len)?.to_vec()).ok_or("a tree name")?;
            let page = r.u64()?;
            let hash = r.hash()?;
            if page == 0 || page >= end {
                return Err("a root outside the pages");
            }
            if trees.last().is_some_and(|(last, _)| *last >= name) {
                return Err("trees out of order");
            }
            trees.push((name, Root { page, hash }));
        }
        let mut lists = Vec::new();
        for _ in 0..2 {
            let count = r.count(4, 1 + 1 + 8 + 48 + 16 + REF)?;
            if count > MAX_RESTORE_POINTS {
                return Err("too many restore points");
            }
            let mut list: Vec<RestorePoint> = Vec::with_capacity(count);
            for _ in 0..count {
                let len = usize::from(r.u8()?);
                if len == 0 || len > MAX_RESTORE_NAME {
                    return Err("a restore point name");
                }
                let name = std::str::from_utf8(r.take(len)?)
                    .map_err(|_| "a restore point name")?
                    .to_owned();
                let checkpoint = r.u64()?;
                let entry = r.entry()?;
                let version = r.version()?;
                let meta = (r.u64()?, r.hash()?);
                // A checkpoint kept as a restore point lists itself with no
                // page: its own meta stream is the one being read.
                let own = checkpoint == number && meta == (0, Hash::default());
                if (!own && (meta.0 == 0 || meta.0 >= end)) || checkpoint == 0 {
                    return Err("a restore point outside the pages");
                }
                if list.iter().any(|rp| rp.name == name) {
                    return Err("a restore point listed twice");
                }
                list.push(RestorePoint {
                    name,
                    checkpoint,
                    entry,
                    version,
                    meta,
                });
            }
            lists.push(list);
        }
        let retiring = lists.pop().unwrap_or_default();
        let restore_points = lists.pop().unwrap_or_default();
        let free_len = r.count(8, 1)?;
        let free = r.take(free_len)?.to_vec();
        let count = r.count(8, PENDING)?;
        let mut pending = Vec::with_capacity(count);
        for _ in 0..count {
            let n = r.u64()?;
            let hash = r.hash()?;
            let death = r.version()?;
            let flags = r.u8()?;
            let birth = r.version()?;
            let hold = r.u64()?;
            if flags > 3 || (flags & 1 == 0 && birth != Version::default()) {
                return Err("a dying page's flags");
            }
            let birth = (flags & 1 != 0).then_some(birth);
            let expand = flags & 2 != 0;
            pending.push((
                n,
                Pending {
                    hash,
                    birth,
                    death,
                    expand,
                    hold,
                    carried: birth.is_none() || expand,
                },
            ));
        }
        let count = r.count(8, SHARE)?;
        let mut shares = Vec::with_capacity(count);
        for _ in 0..count {
            let n = r.u64()?;
            let count = r.u32()?;
            let last = r.version()?;
            shares.push((n, Share { count, last }));
        }
        if r.at != stream.len() {
            return Err("bytes after the end of the meta stream");
        }
        Ok(Meta {
            number,
            entry,
            tree_format,
            predecessor,
            trees,
            restore_points,
            retiring,
            map: Image {
                end,
                free,
                pending,
                shares,
            },
        })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).ok_or("a length past the stream")?;
        let out = self
            .b
            .get(self.at..end)
            .ok_or("the meta stream ends early")?;
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("2")))
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }

    fn hash(&mut self) -> Result<Hash, &'static str> {
        Ok(Hash(self.take(32)?.try_into().expect("32")))
    }

    fn version(&mut self) -> Result<Version, &'static str> {
        Ok(Version {
            entry: self.u64()?,
            step: self.u64()?,
        })
    }

    fn entry(&mut self) -> Result<EntryId, &'static str> {
        Ok(EntryId {
            index: self.u64()?,
            term: self.u64()?,
            hash: self.hash()?,
        })
    }

    /// A count of `width` bytes, of records of at least `min` bytes each,
    /// which the bytes left must be able to hold.
    fn count(&mut self, width: usize, min: usize) -> Result<usize, &'static str> {
        let n = match width {
            4 => u64::from(self.u32()?),
            _ => self.u64()?,
        };
        let left = (self.b.len() - self.at) as u64;
        if n.saturating_mul(min as u64) > left {
            return Err("a count larger than the stream");
        }
        Ok(n as usize)
    }
}

/// Builds the pages of `stream` under `numbers` (as many as
/// [`pages_for`] says: the directory pages first, then the data pages),
/// each stamped with `stamp`. Returns the pages and the first directory
/// page's reference.
pub(crate) fn build(
    stream: &[u8],
    numbers: &[PageNo],
    stamp: u64,
) -> (Vec<Arc<Page>>, (PageNo, Hash)) {
    let chunks: Vec<&[u8]> = if stream.is_empty() {
        vec![&[0u8][..0]]
    } else {
        stream.chunks(OVERFLOW_DATA).collect()
    };
    let dirs = chunks.len().div_ceil(DIR_ENTRIES);
    assert_eq!(
        numbers.len(),
        dirs + chunks.len(),
        "the stream's page count"
    );
    let mut pages = Vec::with_capacity(numbers.len());
    let mut refs = Vec::with_capacity(chunks.len());
    for (chunk, &number) in chunks.iter().zip(&numbers[dirs..]) {
        let mut b = Builder::new(PageKind::Overflow, 0, stamp);
        b.data(chunk);
        let page = Arc::new(b.finish(number).expect("a chunk fits a page"));
        refs.push((number, page.hash()));
        pages.push(page);
    }
    let mut next: (PageNo, Hash) = (0, Hash::default());
    for d in (0..dirs).rev() {
        let mut data = Vec::with_capacity(OVERFLOW_DATA);
        data.extend_from_slice(&next.0.to_le_bytes());
        data.extend_from_slice(&next.1 .0);
        let end = ((d + 1) * DIR_ENTRIES).min(refs.len());
        for (n, h) in &refs[d * DIR_ENTRIES..end] {
            data.extend_from_slice(&n.to_le_bytes());
            data.extend_from_slice(&h.0);
        }
        let mut b = Builder::new(PageKind::Overflow, 0, stamp);
        b.data(&data);
        let page = Arc::new(b.finish(numbers[d]).expect("a directory fits a page"));
        next = (numbers[d], page.hash());
        pages.push(page);
    }
    (pages, next)
}

/// Why a meta stream could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaFault {
    /// A page of it cannot be used.
    Page(PageFault),
    /// Its pages are sound and it breaks the format.
    Malformed(&'static str),
}

impl std::fmt::Display for MetaFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetaFault::Page(fault) => write!(f, "{fault}"),
            MetaFault::Malformed(why) => write!(f, "malformed: {why}"),
        }
    }
}

/// A stream's bytes, and every page that holds it.
pub(crate) type Stream = (Vec<u8>, Vec<(PageNo, Hash)>);

/// Reads the stream whose first directory page is `root`, with `load`
/// reading and checking one page. At most `max_pages` pages are read. Returns
/// the stream and every page of it.
pub(crate) fn read_stream(
    root: (PageNo, Hash),
    max_pages: u64,
    mut load: impl FnMut(PageNo, &Hash) -> Result<Arc<Page>, PageFault>,
) -> Result<Stream, MetaFault> {
    let mut pages = Vec::new();
    let mut data_refs = Vec::new();
    // A stream never names a page twice: refusing a repeat keeps a crafted
    // directory from making a stream larger than the pages it occupies.
    let mut seen = std::collections::BTreeSet::new();
    let mut next = root;
    while next.0 != 0 {
        if pages.len() as u64 >= max_pages || !seen.insert(next.0) {
            return Err(MetaFault::Malformed(
                "a directory longer than the file, or naming a page twice",
            ));
        }
        let page = load(next.0, &next.1).map_err(MetaFault::Page)?;
        pages.push(next);
        if page.kind() != PageKind::Overflow {
            return Err(MetaFault::Malformed("a directory page of another kind"));
        }
        let data = page.data();
        if data.len() < 2 * REF || (data.len() - REF) % REF != 0 {
            return Err(MetaFault::Malformed("a directory page's length"));
        }
        let at = |i: usize| -> (PageNo, Hash) {
            let b = &data[i..i + REF];
            (
                u64::from_le_bytes(b[..8].try_into().expect("8")),
                Hash(b[8..].try_into().expect("32")),
            )
        };
        let following = at(0);
        for i in (REF..data.len()).step_by(REF) {
            let r = at(i);
            if !seen.insert(r.0) {
                return Err(MetaFault::Malformed("a stream naming a page twice"));
            }
            data_refs.push(r);
        }
        if following.0 == 0 && following.1 != Hash::default() {
            return Err(MetaFault::Malformed("a directory's end"));
        }
        next = following;
    }
    if (pages.len() + data_refs.len()) as u64 > max_pages {
        return Err(MetaFault::Malformed("a stream longer than the file"));
    }
    let mut stream = Vec::with_capacity(data_refs.len() * OVERFLOW_DATA);
    for (number, hash) in data_refs {
        let page = load(number, &hash).map_err(MetaFault::Page)?;
        if page.kind() != PageKind::Overflow {
            return Err(MetaFault::Malformed("a stream page of another kind"));
        }
        stream.extend_from_slice(page.data());
        pages.push((number, hash));
    }
    Ok((stream, pages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(end: PageNo, pending: usize) -> Meta {
        let rp = RestorePoint {
            name: "nightly".into(),
            checkpoint: 4,
            entry: EntryId {
                index: 40,
                term: 1,
                hash: Hash([4; 32]),
            },
            version: Version { entry: 40, step: 4 },
            meta: (12, Hash([7; 32])),
        };
        Meta {
            number: 9,
            entry: EntryId {
                index: 77,
                term: 2,
                hash: Hash([1; 32]),
            },
            tree_format: 1,
            predecessor: Some((8, Version { entry: 70, step: 8 }, (11, Hash([8; 32])))),
            trees: vec![
                (
                    TreeName::new(b"file/1".to_vec()).unwrap(),
                    Root {
                        page: 5,
                        hash: Hash([2; 32]),
                    },
                ),
                (
                    TreeName::new(b"rest".to_vec()).unwrap(),
                    Root {
                        page: 6,
                        hash: Hash([3; 32]),
                    },
                ),
            ],
            restore_points: vec![rp.clone()],
            retiring: vec![RestorePoint {
                name: "old".into(),
                ..rp
            }],
            map: Image {
                end,
                free: vec![0b10; end.div_ceil(8) as usize],
                pending: (0..pending as u64)
                    .map(|i| {
                        (
                            100 + i,
                            Pending {
                                hash: Hash([i as u8; 32]),
                                birth: (i % 2 == 0).then_some(Version::at(i)),
                                death: Version::at(i + 1),
                                expand: i % 3 == 0,
                                hold: i % 4,
                                carried: i % 2 != 0 || i % 3 == 0,
                            },
                        )
                    })
                    .collect(),
                shares: vec![(
                    3,
                    Share {
                        count: 2,
                        last: Version::at(5),
                    },
                )],
            },
        }
    }

    #[test]
    fn a_meta_stream_round_trips_through_its_pages() {
        // A stream long enough for two directory pages.
        let meta = sample(4_000, 120_000);
        let stream = meta.encode();
        let n = pages_for(stream.len());
        assert!(n > DIR_ENTRIES, "two directory pages: {n}");
        let numbers: Vec<PageNo> = (1000..1000 + n as u64).collect();
        let (pages, root) = build(&stream, &numbers, 9);
        let by_number: std::collections::BTreeMap<_, _> =
            pages.iter().map(|p| (p.number(), p.clone())).collect();
        let (back, read) = read_stream(root, 1 << 20, |n, h| {
            let page = &by_number[&n];
            Page::verify(n, h, page.bytes().into())
                .map(Arc::new)
                .map_err(|fault| PageFault {
                    page: n,
                    hash: *h,
                    fault,
                })
        })
        .unwrap();
        assert_eq!(read.len(), n);
        assert_eq!(Meta::decode(&back).unwrap(), meta);
    }

    #[test]
    fn a_damaged_stream_is_refused() {
        let meta = sample(64, 3);
        let stream = meta.encode();
        for cut in [0, 10, stream.len() - 1] {
            assert!(Meta::decode(&stream[..cut]).is_err());
        }
        let mut longer = stream.clone();
        longer.push(0);
        assert!(Meta::decode(&longer).is_err());
        // A count that claims more than the stream holds is refused before
        // anything is allocated for it.
        let at = FIXED;
        let mut big = stream;
        big[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Meta::decode(&big), Err("a count larger than the stream"));
    }
}
