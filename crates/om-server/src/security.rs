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
//! It is read through a reading of the file (`engine::Reading::serve`): its
//! records listed one key at a time and each member it decodes read whole,
//! all from one version. A read that fails is the caller's error.

use std::ops::Bound;

use om_hbam::OwnedNode;
use om_protocol::authoring::{
    CatalogAccount, CatalogExtendedPrivilege, CatalogPrivilegeSet, SecurityCatalog,
    MAX_RECORD_FLAGS,
};

use crate::accounts::{AccountKind, AccountStatus};
use crate::engine::{Direction, FileEngine, ReadError, View, NO_LIMIT};
use crate::hosting::Hosted;

const ACCOUNTS: [&[u8]; 3] = [&[23], &[1], &[5]];
const PRIVILEGE_SETS: [&[u8]; 3] = [&[23], &[2], &[5]];
const EXTENDED_PRIVILEGES: [&[u8]; 3] = [&[23], &[3], &[5]];

/// The members of an account record decoded ([`account`]), in key order.
const ACCOUNT_MEMBERS: [u8; 4] = [2, 0x0a, 0x0b, 16];
/// The members of a privilege set record decoded ([`privilege_set`]), in
/// key order.
const SET_MEMBERS: [u8; 3] = [2, 0x0c, 0x10];
/// The members of an extended privilege record decoded
/// ([`extended_privilege`]), in key order.
const EXTENDED_MEMBERS: [u8; 2] = [5, 0x10];

/// `hosted`'s security catalog in the version `engine` holds now.
pub(crate) fn catalog(engine: &FileEngine, hosted: &Hosted) -> Result<SecurityCatalog, ReadError> {
    engine.read(hosted).serve(|read| {
        Ok(SecurityCatalog {
            accounts: records(read.view, &ACCOUNTS, &ACCOUNT_MEMBERS)?
                .iter()
                .map(account)
                .collect(),
            privilege_sets: records(read.view, &PRIVILEGE_SETS, &SET_MEMBERS)?
                .iter()
                .map(privilege_set)
                .collect(),
            extended_privileges: records(read.view, &EXTENDED_PRIVILEGES, &EXTENDED_MEMBERS)?
                .iter()
                .map(extended_privilege)
                .collect(),
        })
    })
}

/// The records of the catalog at `path` in key order, each with only its
/// `members` (those present, each whole, in key order, as a record holds
/// them): what the decoders below read.
fn records(view: &dyn View, path: &[&[u8]], members: &[u8]) -> Result<Vec<OwnedNode>, ReadError> {
    let mut records = Vec::new();
    for child in view.children(path, Bound::Unbounded, Direction::Forward) {
        let key = child?.segment;
        let mut record = OwnedNode::list(key.to_vec(), Vec::new());
        for member in members {
            let member = [*member];
            let at: Vec<&[u8]> = path.iter().copied().chain([&key[..], &member]).collect();
            if let Some(node) = view.subtree(&at, NO_LIMIT)? {
                record.children.push(node.into_owned());
            }
        }
        records.push(record);
    }
    Ok(records)
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
