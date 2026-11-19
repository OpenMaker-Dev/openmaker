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
//! read, so no credential can reach the reply. What cannot be read is
//! `None`, never a default.

use om_hbam::OwnedNode;
use om_protocol::authoring::{
    CatalogAccount, CatalogExtendedPrivilege, CatalogPrivilegeSet, SecurityCatalog,
    MAX_RECORD_FLAGS,
};

use crate::accounts::{AccountKind, AccountStatus};
use crate::hosting::Hosted;

const ACCOUNTS: [&[u8]; 3] = [&[23], &[1], &[5]];
const PRIVILEGE_SETS: [&[u8]; 3] = [&[23], &[2], &[5]];
const EXTENDED_PRIVILEGES: [&[u8]; 3] = [&[23], &[3], &[5]];

/// `hosted`'s security catalog as its store holds it now.
pub(crate) fn catalog(hosted: &Hosted) -> SecurityCatalog {
    let store = hosted.read_store();
    let records = |path: &[&[u8]]| {
        store
            .node(path)
            .map(|node| node.children.as_slice())
            .unwrap_or_default()
    };
    SecurityCatalog {
        accounts: records(&ACCOUNTS).iter().map(account).collect(),
        privilege_sets: records(&PRIVILEGE_SETS).iter().map(privilege_set).collect(),
        extended_privileges: records(&EXTENDED_PRIVILEGES)
            .iter()
            .map(extended_privilege)
            .collect(),
    }
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
