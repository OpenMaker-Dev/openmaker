//! The security catalog a hosted file's own [Full Access] account may read
//! with Describe Security (authoring operation 5, `docs/authoring.md`): the
//! file's accounts, privilege sets and extended privileges, decoded here on
//! the host (docs/re/local-files.md 2, docs/re/session-identity.md).
//!
//! Only the members it decodes are read: an account's name (key 16, raw
//! `10`), kind `02`,
//! status `0a` and privilege set `0b`; a set's name `10`, record flags `02`
//! and password lifetime `0c`; an extended privilege's keyword `10` and its
//! sets `05`. An account's password `06` and its undecoded `04` are never
//! read, so no credential can reach the reply. What cannot be decoded is
//! `None`, never a default.
//!
//! It is read through a reading of the file (`engine::Reading::serve`), all
//! from one version: its records listed one key at a time, each decoded as
//! it is read, with only the members it decodes, each read as its decoder
//! reads it ([`Member`]): heads and keys first, a member's pieces only where
//! a decoder uses them, an extended privilege's sets `05` as the keys of
//! its children, all the reply takes of them. A text member or a member's
//! keys past what one reply carries, as stored, refuse the request
//! ([`MEMBER_LIMIT`]). A read that fails, or a member past the limit, is
//! the caller's error.

use std::ops::Bound;

use om_hbam::OwnedNode;
use om_protocol::authoring::{
    CatalogAccount, CatalogExtendedPrivilege, CatalogPrivilegeSet, SecurityCatalog,
    MAX_DEFINITION_BYTES, MAX_RECORD_FLAGS,
};

use crate::accounts::{AccountKind, AccountStatus};
use crate::engine::{Child, Direction, FileEngine, Head, ReadError, View};
use crate::hosting::Hosted;

const ACCOUNTS: [&[u8]; 3] = [&[23], &[1], &[5]];
const PRIVILEGE_SETS: [&[u8]; 3] = [&[23], &[2], &[5]];
const EXTENDED_PRIVILEGES: [&[u8]; 3] = [&[23], &[3], &[5]];

/// The members of a record a decoder reads, in key order, each read as
/// its decoder reads it.
#[derive(Clone, Copy)]
enum Member {
    /// A value a decoder reads with [`member`]: a leaf's own value, and
    /// empty for a node with children, whose children are never read.
    Scalar(u8),
    /// Text a decoder reads with [`text`]: the node's own value, or else
    /// its pieces (`om_hbam::OwnedNode::assembled`), listed.
    Text(u8),
    /// The keys of the node's children, all the reply takes of them.
    Keys(u8),
}

/// The members of an account record decoded ([`account`]).
const ACCOUNT_MEMBERS: [Member; 4] = [
    Member::Scalar(2),
    Member::Scalar(0x0a),
    Member::Scalar(0x0b),
    Member::Text(16),
];
/// The members of a privilege set record decoded ([`privilege_set`]).
const SET_MEMBERS: [Member; 3] = [Member::Scalar(2), Member::Scalar(0x0c), Member::Text(0x10)];
/// The members of an extended privilege record decoded
/// ([`extended_privilege`]): its sets `05` are the keys of its children.
const EXTENDED_MEMBERS: [Member; 2] = [Member::Keys(5), Member::Text(0x10)];

/// The most a text member or the keys of a member may take, as stored: the
/// bytes of the keys and values of what the decoder is given of it ([`Budget`]).
/// That is what one reply carries; a member past it refuses the request
/// ([`ReadError::TooLarge`]). It bounds the stored bytes read for a member
/// the reply uses, input from a file, not the reply: a name whose stored
/// pieces are past it refuses the whole catalog, though its decoded text
/// may be short (SCSU) or not decode at all (a bad chunk header), where the
/// decoder would leave that one name out. FileMaker's own security editor
/// is not known to write a member of that size. A scalar member is read as
/// its own value alone, as large as one node's value is, and its children
/// (a value in pieces, which no decoder takes) never.
const MEMBER_LIMIT: usize = MAX_DEFINITION_BYTES;

/// `hosted`'s security catalog in the version `engine` holds now.
pub(crate) fn catalog(engine: &FileEngine, hosted: &Hosted) -> Result<SecurityCatalog, ReadError> {
    engine.read(hosted).serve(|read| read_catalog(read.view))
}

/// The security catalog of the version `view` reads.
fn read_catalog(view: &dyn View) -> Result<SecurityCatalog, ReadError> {
    Ok(SecurityCatalog {
        accounts: decoded(view, &ACCOUNTS, &ACCOUNT_MEMBERS, account)?,
        privilege_sets: decoded(view, &PRIVILEGE_SETS, &SET_MEMBERS, privilege_set)?,
        extended_privileges: decoded(
            view,
            &EXTENDED_PRIVILEGES,
            &EXTENDED_MEMBERS,
            extended_privilege,
        )?,
    })
}

/// The records of the catalog at `path` in key order, each decoded by
/// `decode` as it is read, from a node that holds only `members`, those
/// present, in key order, each as its decoder reads it. One record is held
/// at a time.
fn decoded<T>(
    view: &dyn View,
    path: &[&[u8]],
    members: &[Member],
    decode: fn(&OwnedNode) -> T,
) -> Result<Vec<T>, ReadError> {
    let mut decoded = Vec::new();
    for child in view.children(path, Bound::Unbounded, Direction::Forward) {
        let key = child?.segment;
        let mut record = OwnedNode::list(key.to_vec(), Vec::new());
        for member in members {
            let (Member::Scalar(segment) | Member::Text(segment) | Member::Keys(segment)) = *member;
            let member_key = [segment];
            let at: Vec<&[u8]> = path
                .iter()
                .copied()
                .chain([&key[..], &member_key])
                .collect();
            let mut budget = Budget::new(&at);
            let node = match member {
                Member::Scalar(_) => read_scalar(view, &at, segment)?,
                Member::Text(_) => read_text(view, &at, segment, &mut budget)?,
                Member::Keys(_) => read_keys(view, &at, segment, &mut budget)?,
            };
            record.children.extend(node);
        }
        decoded.push(decode(&record));
    }
    Ok(decoded)
}

/// What the decoder is given of one member, against [`MEMBER_LIMIT`]: a
/// text member's own value, or the key and own value of each of its pieces;
/// the key and own value of each child of a member read as keys. Not
/// charged: a scalar member's own value; the probe for a first child
/// ([`has_children`]), whose key and value the view hands out and the
/// decoder never sees (for a scalar member that opens a list, and for each
/// piece of a text member that does); and the rest of
/// the page of up to 256 children a listing fetches before the entry that
/// passes the limit is charged.
struct Budget {
    path: Vec<Vec<u8>>,
    taken: usize,
}

impl Budget {
    fn new(path: &[&[u8]]) -> Self {
        Budget {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            taken: 0,
        }
    }

    /// Count `child`, its key and its own value, handed out.
    fn take(&mut self, child: &Child<'_>) -> Result<(), ReadError> {
        let bytes = child.segment.len() + child.head.value.as_ref().map_or(0, |v| v.len());
        self.taken = self.taken.saturating_add(bytes);
        if self.taken > MEMBER_LIMIT {
            return Err(ReadError::TooLarge {
                path: self.path.clone(),
                limit: MEMBER_LIMIT,
            });
        }
        Ok(())
    }
}

/// Whether the node at `at`, whose head is `head`, has a child: one child
/// listed, when its head says it opens a list.
fn has_children(view: &dyn View, at: &[&[u8]], head: &Head<'_>) -> Result<bool, ReadError> {
    if !head.list {
        return Ok(false);
    }
    let mut first = Vec::new();
    view.children_page(at, Bound::Unbounded, Direction::Forward, 1, &mut first)?;
    Ok(!first.is_empty())
}

/// The member at `at` as [`member`] reads it: a leaf holding its own value,
/// or an empty one for a node without a value or with children.
fn read_scalar(view: &dyn View, at: &[&[u8]], segment: u8) -> Result<Option<OwnedNode>, ReadError> {
    let Some(head) = view.node(at)? else {
        return Ok(None);
    };
    let value = match &head.value {
        Some(value) if !has_children(view, at, &head)? => value.to_vec(),
        _ => Vec::new(),
    };
    Ok(Some(OwnedNode::leaf(vec![segment], value)))
}

/// The member at `at` as [`text`] reads it: its own value, or else its
/// children as listed, each with its own value and, when it has children,
/// one placeholder child, so that `OwnedNode::assembled` joins exactly the
/// pieces it joined from the node itself.
fn read_text(
    view: &dyn View,
    at: &[&[u8]],
    segment: u8,
    budget: &mut Budget,
) -> Result<Option<OwnedNode>, ReadError> {
    let Some(head) = view.node(at)? else {
        return Ok(None);
    };
    if let Some(value) = head.value {
        budget.take(&Child {
            segment: std::borrow::Cow::Borrowed(&[]),
            head: Head {
                list: head.list,
                value: Some(value.clone()),
            },
        })?;
        return Ok(Some(OwnedNode::leaf(vec![segment], value.into_owned())));
    }
    let mut pieces = Vec::new();
    for child in view.children(at, Bound::Unbounded, Direction::Forward) {
        let child = child?;
        budget.take(&child)?;
        let below: Vec<&[u8]> = at.iter().copied().chain([&child.segment[..]]).collect();
        let children = if has_children(view, &below, &child.head)? {
            vec![OwnedNode::default()]
        } else {
            Vec::new()
        };
        pieces.push(OwnedNode {
            segment: child.segment.into_owned(),
            value: child.head.value.map(std::borrow::Cow::into_owned),
            push: child.head.list,
            children,
        });
    }
    Ok(Some(OwnedNode::list(vec![segment], pieces)))
}

/// The keys of the children of the member at `at`, each an empty leaf: all
/// an extended privilege's sets give the reply. `None` without children.
fn read_keys(
    view: &dyn View,
    at: &[&[u8]],
    segment: u8,
    budget: &mut Budget,
) -> Result<Option<OwnedNode>, ReadError> {
    let mut keys = Vec::new();
    for child in view.children(at, Bound::Unbounded, Direction::Forward) {
        let child = child?;
        budget.take(&child)?;
        keys.push(OwnedNode::leaf(child.segment.into_owned(), Vec::new()));
    }
    Ok((!keys.is_empty()).then(|| OwnedNode::list(vec![segment], keys)))
}

fn account(record: &OwnedNode) -> CatalogAccount {
    let status = AccountStatus::decode(member(record, &[0x0a]));
    CatalogAccount {
        key: record.segment.clone(),
        name: text(record, &[16]),
        kind: AccountKind::decode(member(record, &[2])).map(|kind| match kind {
            AccountKind::FileMakerFile => 0,
            AccountKind::External(kind) => kind,
        }),
        active: status.map(|status| status.is_active()),
        must_change_password: status.map(|status| status.must_change_password()),
        privilege_set: member(record, &[0x0b])
            .and_then(om_data::host::security::account_key)
            .map(<[u8]>::to_vec),
    }
}

fn privilege_set(record: &OwnedNode) -> CatalogPrivilegeSet {
    CatalogPrivilegeSet {
        key: record.segment.clone(),
        name: text(record, &[0x10]),
        record_flags: member(record, &[2])
            .filter(|flags| !flags.is_empty() && flags.len() <= MAX_RECORD_FLAGS)
            .map(<[u8]>::to_vec),
        password_days: match member(record, &[0x0c]) {
            None => Some(0),
            Some(&[high, low]) => Some(u16::from_be_bytes([high, low])),
            Some(_) => None,
        },
    }
}

fn extended_privilege(record: &OwnedNode) -> CatalogExtendedPrivilege {
    CatalogExtendedPrivilege {
        key: record.segment.clone(),
        keyword: text(record, &[0x10]),
        sets: record
            .child(&[5])
            .map(|sets| {
                sets.children
                    .iter()
                    .map(|set| set.segment.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The value of `record`'s member `key`, as `hosting` reads account
/// members: absent is `None`, and a member that is not a leaf reads as
/// empty, which no decoder accepts.
fn member<'a>(record: &'a OwnedNode, key: &[u8]) -> Option<&'a [u8]> {
    record.child(key).map(|node| match &node.value {
        Some(value) if node.children.is_empty() => value.as_slice(),
        _ => &[],
    })
}

/// The member `key` as FileMaker text, or `None` when it is absent or not
/// text (such as [Guest]'s localized name reference).
fn text(record: &OwnedNode, key: &[u8]) -> Option<String> {
    let value = record.child(key)?.assembled()?;
    om_scsu::decode_scsu(&value).ok()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::cell::Cell;
    use std::ops::Bound;

    use om_hbam::OwnedNode;

    use super::*;
    use crate::engine::{Step, TreeView, Version};

    /// A view over another that counts the bytes of what it hands out: the
    /// values of heads, the segments of children and steps, and each
    /// subtree as `om_hbam::tree::memory` counts it.
    struct Counting<'v> {
        inner: &'v dyn View,
        bytes: Cell<usize>,
    }

    impl Counting<'_> {
        fn add(&self, bytes: usize) {
            self.bytes.set(self.bytes.get() + bytes);
        }

        fn head(&self, head: &Head<'_>) {
            self.add(head.value.as_ref().map_or(0, |value| value.len()));
        }
    }

    impl View for Counting<'_> {
        fn version(&self) -> Version {
            self.inner.version()
        }

        fn node(&self, path: &[&[u8]]) -> Result<Option<Head<'_>>, ReadError> {
            let head = self.inner.node(path)?;
            if let Some(head) = &head {
                self.head(head);
            }
            Ok(head)
        }

        fn subtree(
            &self,
            path: &[&[u8]],
            limit: usize,
        ) -> Result<Option<Cow<'_, OwnedNode>>, ReadError> {
            let node = self.inner.subtree(path, limit)?;
            if let Some(node) = &node {
                self.add(om_hbam::tree::memory(node));
            }
            Ok(node)
        }

        fn children_page<'v>(
            &'v self,
            path: &[&[u8]],
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            out: &mut Vec<Child<'v>>,
        ) -> Result<bool, ReadError> {
            let start = out.len();
            let more = self.inner.children_page(path, from, direction, max, out)?;
            for child in &out[start..] {
                self.add(child.segment.len());
                self.head(&child.head);
            }
            Ok(more)
        }

        fn subtrees_page<'v>(
            &'v self,
            path: &[&[u8]],
            from: Bound<&[u8]>,
            direction: Direction,
            max: usize,
            limit: usize,
            out: &mut Vec<Cow<'v, OwnedNode>>,
        ) -> Result<bool, ReadError> {
            let start = out.len();
            let more = self
                .inner
                .subtrees_page(path, from, direction, max, limit, out)?;
            for node in &out[start..] {
                self.add(om_hbam::tree::memory(node));
            }
            Ok(more)
        }

        fn walk_page<'v>(
            &'v self,
            path: &[&[u8]],
            after: Option<&[Vec<u8>]>,
            max: usize,
            out: &mut Vec<Step<'v>>,
        ) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
            let start = out.len();
            let resume = self.inner.walk_page(path, after, max, out)?;
            for step in &out[start..] {
                self.add(step.segment.len());
                self.head(&step.head);
            }
            Ok(resume)
        }

        fn changed_since(&self, path: &[&[u8]], since: Version) -> Result<bool, ReadError> {
            self.inner.changed_since(path, since)
        }
    }

    /// Describe Security reads what its reply holds, not what lies below
    /// it: an extended privilege's sets `05` are read as their keys, so a
    /// megabyte under one of them is never handed out, and the catalog is
    /// decoded as before. A member whole past what one reply carries
    /// refuses the request.
    #[test]
    fn the_security_catalog_reads_only_what_its_reply_holds() {
        let mut root = OwnedNode::default();
        let text = |text: &str| om_scripts::text_node(&[], text);
        root.put(&[&[23], &[1], &[5], &[2], &[16]], text("Admin"));
        root.put(&[&[23], &[2], &[5], &[1], &[0x10]], text("[Full Access]"));
        root.put(&[&[23], &[3], &[5], &[1], &[0x10]], text("fmapp"));
        root.put(
            &[&[23], &[3], &[5], &[1], &[5], &[1], &[9]],
            OwnedNode::leaf(Vec::new(), vec![0x5a; 1 << 20]),
        );
        root.put(
            &[&[23], &[3], &[5], &[1], &[5], &[2]],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
        let tree = TreeView::new(&root, 0);
        let view = Counting {
            inner: &tree,
            bytes: Cell::new(0),
        };
        let catalog = read_catalog(&view).unwrap();
        assert_eq!(catalog.accounts[0].name.as_deref(), Some("Admin"));
        assert_eq!(
            catalog.privilege_sets[0].name.as_deref(),
            Some("[Full Access]")
        );
        let extended = &catalog.extended_privileges[0];
        assert_eq!(extended.keyword.as_deref(), Some("fmapp"));
        assert_eq!(extended.sets, [vec![1u8], vec![2]]);
        assert!(
            view.bytes.get() < 4096,
            "{} bytes handed out",
            view.bytes.get()
        );

        root.put(
            &[&[23], &[1], &[5], &[2], &[16]],
            OwnedNode::leaf(Vec::new(), vec![0x41; MEMBER_LIMIT + 1]),
        );
        let tree = TreeView::new(&root, 0);
        assert!(matches!(
            read_catalog(&tree),
            Err(ReadError::TooLarge { .. })
        ));
    }

    /// The review of #751, round 2 (P1, P2): a member no decoder takes is
    /// not read, however large. An account's status `0a` and a privilege
    /// set's record flags `02`, each a value in pieces past what one reply
    /// carries, read as their heads and one key: the catalog answers as it
    /// answered when those were read whole (a member with children decodes
    /// as no value), and little is handed out. What the view hands out of a
    /// member it lists is counted, values included: an extended
    /// privilege's set key carrying a value past the limit refuses the
    /// request.
    #[test]
    fn a_large_member_the_reply_discards_is_not_read() {
        let mut root = OwnedNode::default();
        let text = |text: &str| om_scripts::text_node(&[], text);
        // A value in pieces (hbam-writer.md 2.3): its length, then pieces.
        let in_pieces = |bytes: usize| {
            let total = OwnedNode::leaf(vec![0], (bytes as u32).to_be_bytes().to_vec());
            let pieces = (1..=bytes.div_ceil(1000) as u64)
                .map(|n| OwnedNode::leaf(om_hbam::serial::encode(n), vec![0x5a; 1000]));
            OwnedNode::list(Vec::new(), std::iter::once(total).chain(pieces).collect())
        };
        root.put(&[&[23], &[1], &[5], &[2], &[16]], text("Admin"));
        root.put(
            &[&[23], &[1], &[5], &[2], &[0x0a]],
            in_pieces(MEMBER_LIMIT + 1000),
        );
        root.put(&[&[23], &[2], &[5], &[1], &[0x10]], text("[Full Access]"));
        root.put(
            &[&[23], &[2], &[5], &[1], &[2]],
            in_pieces(MEMBER_LIMIT + 1000),
        );
        root.put(&[&[23], &[3], &[5], &[1], &[0x10]], text("fmapp"));
        let tree = TreeView::new(&root, 0);
        let view = Counting {
            inner: &tree,
            bytes: Cell::new(0),
        };
        let catalog = read_catalog(&view).unwrap();
        let account = &catalog.accounts[0];
        assert_eq!(account.name.as_deref(), Some("Admin"));
        assert_eq!((account.active, account.must_change_password), (None, None));
        let set = &catalog.privilege_sets[0];
        assert_eq!(set.name.as_deref(), Some("[Full Access]"));
        assert_eq!((&set.record_flags, set.password_days), (&None, Some(0)));
        assert!(
            view.bytes.get() < 4096,
            "{} bytes handed out",
            view.bytes.get()
        );

        root.put(
            &[&[23], &[3], &[5], &[1], &[5], &[1]],
            OwnedNode::leaf(Vec::new(), vec![0x5a; MEMBER_LIMIT]),
        );
        let tree = TreeView::new(&root, 0);
        assert!(matches!(
            read_catalog(&tree),
            Err(ReadError::TooLarge { .. })
        ));
    }
}
