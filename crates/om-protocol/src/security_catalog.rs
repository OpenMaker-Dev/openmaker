//! The file's security catalog as OpenMaker Server's authoring extension
//! describes it (operation 5, Describe Security, `docs/authoring.md`): its
//! accounts, privilege sets and extended privileges, decoded by the host
//! from `[23]` (docs/re/local-files.md 2, docs/re/session-identity.md).
//!
//! It holds only decoded facts. No password verifier (`[23, 1, 5, A, 06]`),
//! account record member `04` or other raw security bytes is part of it:
//! the host never reads them for it, so they cannot reach a reply. A fact
//! the host cannot read is `None`, never an empty or default value.

use om_rctransaction::{ResultBuilder, ResultReader};

use crate::perform::PerformError;

/// The accounts, privilege sets and extended privileges of a file, each in
/// the catalog's key order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityCatalog {
    pub accounts: Vec<CatalogAccount>,
    pub privilege_sets: Vec<CatalogPrivilegeSet>,
    pub extended_privileges: Vec<CatalogExtendedPrivilege>,
}

/// An account record `[23, 1, 5, A]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogAccount {
    /// The record key `A`. Record `01` is [Guest] (local-files.md 2.2).
    pub key: Vec<u8>,
    /// Its name `16`; `None` when it is not text, as [Guest]'s localized
    /// name reference is not (local-files.md 2.1).
    pub name: Option<String>,
    /// Its kind `02`: 0 an account of the file itself, any other value one
    /// another service authenticates. Absent reads as 0; `None` when the
    /// value is not one byte.
    pub kind: Option<u8>,
    /// Whether it is active (status `0a`, bit 19 of its microseconds
    /// clear); `None` when the status cannot be read.
    pub active: Option<bool>,
    /// Whether its password must be changed at its next login (status
    /// `0a`, bit 18); `None` when the status cannot be read.
    pub must_change_password: Option<bool>,
    /// The record key of its privilege set under `[23, 2, 5]` (`0b`, a
    /// framed key, session-identity.md 1); `None` when absent or not one
    /// framed segment.
    pub privilege_set: Option<Vec<u8>>,
}

/// A privilege set record `[23, 2, 5, P]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPrivilegeSet {
    /// The record key `P`. Keys 1, 2 and 3 name the built-in sets in every
    /// observed file.
    pub key: Vec<u8>,
    /// Its name `10`; `None` when it is not text.
    pub name: Option<String>,
    /// Its record privilege flags `02` as stored (at most
    /// [`MAX_RECORD_FLAGS`] bytes); `None` when absent or longer. Only the
    /// first byte's documented values are read (session-identity.md 2).
    pub record_flags: Option<Vec<u8>>,
    /// How many days a password lasts under it (`0c`, a big-endian u16;
    /// absent is 0, no limit); `None` when the value is not two bytes.
    pub password_days: Option<u16>,
}

/// An extended privilege record `[23, 3, 5, E]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogExtendedPrivilege {
    /// The record key `E`.
    pub key: Vec<u8>,
    /// Its keyword `10` (such as `fmapp`); `None` when it is not text.
    pub keyword: Option<String>,
    /// The record keys of the privilege sets that have it (the children of
    /// `05`), in key order.
    pub sets: Vec<Vec<u8>>,
}

/// The most record privilege flag bytes a catalog carries for a set.
pub const MAX_RECORD_FLAGS: usize = 16;

/// The longest record key a catalog carries (a key's one-byte length).
pub const MAX_KEY: usize = u8::MAX as usize;

/// Push the catalog's reply fields (the host side). Fields are popped in
/// the order [`pop_catalog`] reads them, so they are pushed in reverse.
/// `None` when a key is longer than [`MAX_KEY`] or flags longer than
/// [`MAX_RECORD_FLAGS`]: the host leaves such records out before it builds
/// the reply.
pub(crate) fn push_catalog(b: ResultBuilder, catalog: &SecurityCatalog) -> Option<ResultBuilder> {
    let mut items = Vec::new();
    items.push(Item::U32(u32::try_from(catalog.accounts.len()).ok()?));
    for account in &catalog.accounts {
        items.push(Item::Key(&account.key));
        opt_string(&mut items, account.name.as_deref());
        match account.kind {
            Some(kind) => items.extend([Item::Bool(true), Item::U8(kind)]),
            None => items.push(Item::Bool(false)),
        }
        opt_bool(&mut items, account.active);
        opt_bool(&mut items, account.must_change_password);
        match &account.privilege_set {
            Some(set) => items.extend([Item::Bool(true), Item::Key(set)]),
            None => items.push(Item::Bool(false)),
        }
    }
    items.push(Item::U32(u32::try_from(catalog.privilege_sets.len()).ok()?));
    for set in &catalog.privilege_sets {
        items.push(Item::Key(&set.key));
        opt_string(&mut items, set.name.as_deref());
        match &set.record_flags {
            Some(flags) if flags.len() <= MAX_RECORD_FLAGS => {
                items.extend([Item::Bool(true), Item::Key(flags)])
            }
            Some(_) => return None,
            None => items.push(Item::Bool(false)),
        }
        match set.password_days {
            Some(days) => items.extend([Item::Bool(true), Item::U16(days)]),
            None => items.push(Item::Bool(false)),
        }
    }
    items.push(Item::U32(
        u32::try_from(catalog.extended_privileges.len()).ok()?,
    ));
    for privilege in &catalog.extended_privileges {
        items.push(Item::Key(&privilege.key));
        opt_string(&mut items, privilege.keyword.as_deref());
        items.push(Item::U32(u32::try_from(privilege.sets.len()).ok()?));
        items.extend(privilege.sets.iter().map(|set| Item::Key(set)));
    }
    items.into_iter().rev().try_fold(b, |b, item| match item {
        Item::U8(v) => Some(b.push_u8(v)),
        Item::U16(v) => Some(b.push_u16(v)),
        Item::U32(v) => Some(b.push_u32(v)),
        Item::Bool(v) => Some(b.push_bool(v)),
        Item::Key(bytes) => b.try_push_short_bytes(bytes).ok(),
        Item::Text(text) => Some(b.push_string(text)),
    })
}

/// A reply field, in pop order.
enum Item<'a> {
    U8(u8),
    U16(u16),
    U32(u32),
    Bool(bool),
    /// Bytes with a one-byte length.
    Key(&'a [u8]),
    Text(&'a str),
}

fn opt_string<'a>(items: &mut Vec<Item<'a>>, text: Option<&'a str>) {
    match text {
        Some(text) => items.extend([Item::Bool(true), Item::Text(text)]),
        None => items.push(Item::Bool(false)),
    }
}

fn opt_bool(items: &mut Vec<Item<'_>>, value: Option<bool>) {
    match value {
        Some(value) => items.extend([Item::Bool(true), Item::Bool(value)]),
        None => items.push(Item::Bool(false)),
    }
}

/// Pop the catalog's reply fields (the client side). Every count is checked
/// against the bytes left before anything is read for it, and nothing is
/// allocated from a count: each entry takes at least one byte, so a count
/// past what is left is malformed.
pub(crate) fn pop_catalog(r: &mut ResultReader<'_>) -> Result<SecurityCatalog, PerformError> {
    let mut catalog = SecurityCatalog::default();
    for _ in 0..count(r)? {
        let key = r.pop_short_bytes()?.to_vec();
        let name = pop_opt_string(r)?;
        let kind = if r.pop_bool()? {
            Some(r.pop_u8()?)
        } else {
            None
        };
        let active = pop_opt_bool(r)?;
        let must_change_password = pop_opt_bool(r)?;
        let privilege_set = if r.pop_bool()? {
            Some(r.pop_short_bytes()?.to_vec())
        } else {
            None
        };
        catalog.accounts.push(CatalogAccount {
            key,
            name,
            kind,
            active,
            must_change_password,
            privilege_set,
        });
    }
    for _ in 0..count(r)? {
        let key = r.pop_short_bytes()?.to_vec();
        let name = pop_opt_string(r)?;
        let record_flags = if r.pop_bool()? {
            let flags = r.pop_short_bytes()?;
            if flags.len() > MAX_RECORD_FLAGS {
                return Err(PerformError::Truncated);
            }
            Some(flags.to_vec())
        } else {
            None
        };
        let password_days = if r.pop_bool()? {
            Some(r.pop_u16()?)
        } else {
            None
        };
        catalog.privilege_sets.push(CatalogPrivilegeSet {
            key,
            name,
            record_flags,
            password_days,
        });
    }
    for _ in 0..count(r)? {
        let key = r.pop_short_bytes()?.to_vec();
        let keyword = pop_opt_string(r)?;
        let mut sets = Vec::new();
        for _ in 0..count(r)? {
            sets.push(r.pop_short_bytes()?.to_vec());
        }
        catalog
            .extended_privileges
            .push(CatalogExtendedPrivilege { key, keyword, sets });
    }
    Ok(catalog)
}

/// A list's count, refused when more entries than bytes are left.
fn count(r: &mut ResultReader<'_>) -> Result<usize, PerformError> {
    let count = r.pop_u32()? as usize;
    if count > r.remaining() {
        return Err(PerformError::Truncated);
    }
    Ok(count)
}

fn pop_opt_string(r: &mut ResultReader<'_>) -> Result<Option<String>, PerformError> {
    Ok(if r.pop_bool()? {
        Some(r.pop_string()?)
    } else {
        None
    })
}

fn pop_opt_bool(r: &mut ResultReader<'_>) -> Result<Option<bool>, PerformError> {
    Ok(if r.pop_bool()? {
        Some(r.pop_bool()?)
    } else {
        None
    })
}
