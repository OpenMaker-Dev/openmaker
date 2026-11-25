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
//! it is read, with only the members it decodes, each read whole within
//! what one reply carries ([`MEMBER_LIMIT`]), and an extended privilege's
//! sets `05` read as the keys of its children, all the reply takes of
//! them. A read that fails, or a member past the limit, is the caller's
//! error.

use std::ops::Bound;

use om_hbam::OwnedNode;
use om_protocol::authoring::{
    CatalogAccount, CatalogExtendedPrivilege, CatalogPrivilegeSet, SecurityCatalog,
    MAX_DEFINITION_BYTES, MAX_RECORD_FLAGS,
};

use crate::accounts::{AccountKind, AccountStatus};
use crate::engine::{Direction, FileEngine, ReadError, View};
use crate::hosting::Hosted;

const ACCOUNTS: [&[u8]; 3] = [&[23], &[1], &[5]];
const PRIVILEGE_SETS: [&[u8]; 3] = [&[23], &[2], &[5]];
const EXTENDED_PRIVILEGES: [&[u8]; 3] = [&[23], &[3], &[5]];

/// The members of a record a decoder reads, in key order: each read whole
/// ([`Member::Whole`]), or as the keys of its children alone
/// ([`Member::Keys`]).
#[derive(Clone, Copy)]
enum Member {
    Whole(u8),
    Keys(u8),
}

/// The members of an account record decoded ([`account`]).
const ACCOUNT_MEMBERS: [Member; 4] = [
    Member::Whole(2),
    Member::Whole(0x0a),
    Member::Whole(0x0b),
    Member::Whole(16),
];
/// The members of a privilege set record decoded ([`privilege_set`]).
const SET_MEMBERS: [Member; 3] = [Member::Whole(2), Member::Whole(0x0c), Member::Whole(0x10)];
/// The members of an extended privilege record decoded
/// ([`extended_privilege`]): its sets `05` are the keys of its children.
const EXTENDED_MEMBERS: [Member; 2] = [Member::Keys(5), Member::Whole(0x10)];

/// The most one member read whole may take, and the most the keys of one
/// member read as keys may (counted as `om_hbam::tree::memory` counts an
/// owned tree): what one reply carries. A member past it refuses the
/// request ([`ReadError::TooLarge`]), as a reply past it is refused.
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
/// present, in key order, as a record holds them: one read whole, or one
/// read as keys with each key an empty leaf. One record is held at a time.
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
            let (Member::Whole(segment) | Member::Keys(segment)) = *member;
            let segment = [segment];
            let at: Vec<&[u8]> = path.iter().copied().chain([&key[..], &segment]).collect();
            match member {
                Member::Whole(_) => {
                    if let Some(node) = view.subtree(&at, MEMBER_LIMIT)? {
                        record.children.push(node.into_owned());
                    }
                }
                Member::Keys(_) => {
                    let mut keys = Vec::new();
                    let mut taken = 0usize;
                    for child in view.children(&at, Bound::Unbounded, Direction::Forward) {
                        let key = child?.segment.into_owned();
                        taken = taken.saturating_add(key.len());
                        if taken > MEMBER_LIMIT {
                            return Err(ReadError::TooLarge {
                                path: at.iter().map(|s| s.to_vec()).collect(),
                                limit: MEMBER_LIMIT,
                            });
                        }
                        keys.push(OwnedNode::leaf(key, Vec::new()));
                    }
                    if !keys.is_empty() {
                        record
                            .children
                            .push(OwnedNode::list(segment.to_vec(), keys));
                    }
                }
            }
        }
        decoded.push(decode(&record));
    }
    Ok(decoded)
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
    use crate::engine::{Child, Head, Step, TreeView, Version};

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
}
