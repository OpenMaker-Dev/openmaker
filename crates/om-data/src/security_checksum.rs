//! The checksums of the security catalog `[23]` that FileMaker recomputes
//! when it opens a file, refusing the file with 808 ("access privileges are
//! damaged") when one differs: an account's `04` (docs/re/local-files.md
//! 2.4), a privilege set's, an extended privilege's and a file access
//! entry's `04`, and the file access catalog's `09`
//! (docs/re/session-identity.md 4). Each is an MD5 over the member's
//! stored facts; all but an extended privilege's also hash the file's
//! `mInternalCreation` `[2, 254]`. A writer that stores one of these
//! members stores its checksum with it.
//!
//! Each function reads the member as stored and returns `None` for a form
//! the recipes do not cover (a password shorter than 31 bytes, a privilege
//! set with an action limited by a calculation, text that does not
//! decode), so a writer refuses rather than store a checksum FileMaker
//! might compute differently.

use md5::{Digest, Md5};
use om_hbam::OwnedNode;

/// What the checksums hash of the file itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    /// `mInternalCreation` `[2, 254]`, as stored.
    pub internal_creation: [u8; 4],
    /// The file access catalog's UUID `[23, 4, 1, 10]`, when it is 16 bytes.
    pub file_access_uuid: Option<[u8; 16]>,
    /// The file version `[2, 5]`; an account hashes the file's facts only
    /// above 10, as every file FileMaker 12 and later writes is.
    pub version: u16,
}

/// Member keys the recipes read.
const KIND: u8 = 0x02;
const RECORD_ACCESS: u8 = 0x02;
const DESCRIPTION: u8 = 0x03;
const PASSWORD: u8 = 0x06;
const STATUS: u8 = 0x0a;
const OTHER_PRIVILEGES: u8 = 0x0a;
const SET_KEY: u8 = 0x0b;
const PASSWORD_DAYS: u8 = 0x0c;
const PASSWORD_LENGTH: u8 = 0x0d;
const TABLES: u8 = 0x05;
const SETS: u8 = 0x05;
const ITEM_ACCESS: u8 = 0x09;
const NAME: u8 = 0x10;
/// A file access entry's type, UUID, date, account and hash or token.
const ACCESS_TYPE: u8 = 0x02;
const ACCESS_UUID: u8 = 0x0a;
const ACCESS_DATE: u8 = 0x0b;
const ACCESS_ACCOUNT: u8 = 0x0c;
const ACCESS_PROOF: u8 = 0x0d;
const ACCESS_FILES: u8 = 0x10;
/// The key of 1/1/1024 at midnight, an absent status or date as hashed.
const NO_DATE: [u8; 7] = [0x06, 0x05, 0x90, 0x00, 0x41, 0x80, 0x00];
/// The password value's hashed part: version, salt and hash (2.1).
const PASSWORD_HASHED: usize = 31;

/// A key as a one-segment path key: its length plus 1, its length, its bytes.
fn path_key(key: &[u8]) -> Option<Vec<u8>> {
    let len = u8::try_from(key.len()).ok()?;
    let mut out = vec![len.checked_add(1)?, len];
    out.extend(key);
    Some(out)
}

/// A stored value after a byte holding its length.
fn counted(value: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![u8::try_from(value.len()).ok()?];
    out.extend(value);
    Some(out)
}

/// Stored text (a value, or the node of a chunked or non-ASCII text) as
/// UTF-16 code units, big-endian; empty when absent.
fn utf16(member: &OwnedNode, key: u8) -> Option<Vec<u8>> {
    let Some(node) = member.child(&[key]) else {
        return Some(Vec::new());
    };
    let text = om_scsu::decode_scsu(&node.assembled()?).ok()?;
    Some(text.encode_utf16().flat_map(u16::to_be_bytes).collect())
}

fn value(member: &OwnedNode, key: u8) -> Option<&[u8]> {
    member.child(&[key])?.value.as_deref()
}

/// Whether each of `keys` is absent or a plain value. A key holding a list
/// instead is a form no recipe covers: it must not read as absent.
fn plain(member: &OwnedNode, keys: &[u8]) -> bool {
    keys.iter().all(|key| {
        member
            .child(&[*key])
            .is_none_or(|n| n.value.is_some() && n.children.is_empty())
    })
}

fn digest(parts: &[&[u8]]) -> [u8; 16] {
    let mut md5 = Md5::new();
    for part in parts {
        md5.update(part);
    }
    md5.finalize().into()
}

/// An account's `04` (local-files.md 2.4): `key` is its member key (`02`).
pub fn account(file: &FileStamp, key: &[u8], member: &OwnedNode) -> Option<[u8; 16]> {
    if !plain(member, &[KIND, PASSWORD, STATUS, SET_KEY]) {
        return None;
    }
    let mut facts = Vec::new();
    if file.version > 10 {
        if let Some(uuid) = file.file_access_uuid {
            facts.extend(uuid);
        }
        facts.extend(file.internal_creation);
    }
    facts.extend(path_key(key)?);
    facts.push(match value(member, KIND) {
        None => 0,
        Some([kind]) => *kind,
        Some(_) => return None,
    });
    facts.extend(utf16(member, NAME)?);
    facts.extend(value(member, PASSWORD)?.get(..PASSWORD_HASHED)?);
    facts.extend(utf16(member, DESCRIPTION)?);
    match value(member, STATUS) {
        Some(status) => facts.extend(counted(status)?),
        None => facts.extend(NO_DATE),
    }
    match value(member, SET_KEY) {
        Some(set) => facts.extend(counted(set)?),
        None => facts.extend([0x02, 0x01, 0x00]),
    }
    Some(digest(&[&facts]))
}

/// A record access value `02` as the five bytes a set's checksum hashes:
/// the create, view, edit and delete levels and the field default. `None`
/// for an action limited by a calculation, whose formula the recipe would
/// hash too.
fn record_access(access: Option<&[u8]>) -> Option<[u8; 5]> {
    let Some(access) = access else {
        return Some([0, 0, 0, 0, 1]);
    };
    let [actions, fields] = *access else {
        return None;
    };
    let levels = [
        actions >> 6,
        (actions >> 4) & 3,
        (actions >> 2) & 3,
        actions & 3,
    ];
    if levels.contains(&2) {
        return None;
    }
    Some([levels[0], levels[1], levels[2], levels[3], fields & 3])
}

/// A privilege set's `04` (session-identity.md 4.1).
pub fn privilege_set(file: &FileStamp, member: &OwnedNode) -> Option<[u8; 16]> {
    if !plain(
        member,
        &[
            RECORD_ACCESS,
            OTHER_PRIVILEGES,
            PASSWORD_DAYS,
            PASSWORD_LENGTH,
        ],
    ) {
        return None;
    }
    let mut facts = Vec::new();
    facts.extend(record_access(value(member, RECORD_ACCESS))?);
    let other = match value(member, OTHER_PRIVILEGES) {
        None => 0,
        Some([high, low]) => u32::from(u16::from_be_bytes([*high, *low])),
        Some(_) => return None,
    };
    let creation = u32::from_be_bytes(file.internal_creation);
    facts.extend(((other ^ 0x6000).wrapping_add(creation)).to_be_bytes());
    facts.extend(utf16(member, DESCRIPTION)?);
    for key in [PASSWORD_DAYS, PASSWORD_LENGTH] {
        match value(member, key) {
            None => facts.extend([0, 0]),
            Some(two @ [_, _]) => facts.extend(two),
            Some(_) => return None,
        }
    }
    for table in member.child(&[TABLES]).map_or(&[][..], |t| &t.children) {
        if !plain(table, &[RECORD_ACCESS]) {
            return None;
        }
        facts.extend(path_key(&table.segment)?);
        facts.extend(record_access(value(table, RECORD_ACCESS))?);
        for field in table.child(&[TABLES]).map_or(&[][..], |f| &f.children) {
            facts.extend(path_key(&field.segment)?);
            facts.push(*field.value.as_deref()?.first()?);
        }
    }
    // The entries under `09` in key order, defaults (`04`) and item entries
    // (`04 13`, one segment) together, each a value of one byte.
    for entry in member
        .child(&[ITEM_ACCESS])
        .map_or(&[][..], |a| &a.children)
    {
        let byte = match entry.value.as_deref() {
            Some([byte]) if entry.children.is_empty() => *byte,
            _ => return None,
        };
        facts.extend(path_key(&entry.segment)?);
        facts.push(byte);
    }
    Some(digest(&[&facts]))
}

/// An extended privilege's `04` (session-identity.md 4.2): nothing of the
/// file is hashed.
pub fn extended_privilege(member: &OwnedNode) -> Option<[u8; 16]> {
    let mut facts = utf16(member, NAME)?;
    facts.extend(utf16(member, DESCRIPTION)?);
    facts.push(if value(member, KIND) == Some(&[0x01, 0x01]) {
        1
    } else {
        2
    });
    for set in member.child(&[SETS]).map_or(&[][..], |s| &s.children) {
        facts.extend(path_key(&set.segment)?);
    }
    Some(digest(&[&facts]))
}

/// A file access entry's `04` (session-identity.md 4.3): `key` is its
/// member key.
pub fn file_access_entry(file: &FileStamp, key: &[u8], member: &OwnedNode) -> Option<[u8; 16]> {
    if !plain(
        member,
        &[ACCESS_TYPE, ACCESS_UUID, ACCESS_DATE, ACCESS_PROOF],
    ) {
        return None;
    }
    let mut facts = file.internal_creation.to_vec();
    facts.extend(path_key(key)?);
    let kind = match value(member, ACCESS_TYPE) {
        None => 0,
        Some([high, low]) => i16::from_be_bytes([*high, *low]),
        Some(_) => return None,
    };
    facts.extend(kind.to_be_bytes());
    facts.extend(value(member, ACCESS_UUID).filter(|u| u.len() == 16)?);
    facts.extend(utf16(member, ACCESS_FILES)?);
    match value(member, ACCESS_DATE) {
        Some(date) => facts.extend(counted(date)?),
        None => facts.extend(NO_DATE),
    }
    facts.extend(utf16(member, ACCESS_ACCOUNT)?);
    let proof = value(member, ACCESS_PROOF).unwrap_or_default();
    match kind {
        0 => facts.extend(proof.get(..32)?),
        1 => facts.extend(proof.get(..64)?),
        _ => {}
    }
    Some(digest(&[&facts]))
}

/// The file access catalog's `09` (session-identity.md 4.3): `requires`
/// is its `f0`, which authorization it requires.
pub fn file_access_catalog(file: &FileStamp, requires: u32) -> Option<[u8; 16]> {
    let uuid = file.file_access_uuid?;
    let flag: u32 = u32::from(requires & 3 != 0);
    Some(digest(&[
        &file.internal_creation,
        &uuid,
        &flag.to_be_bytes(),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn masked(text: &str) -> Vec<u8> {
        om_scsu::encode_scsu(text)
    }

    /// The English Tasks starter's facts (the specs' vectors).
    fn tasks() -> FileStamp {
        FileStamp {
            internal_creation: [0xda, 0xdd, 0x00, 0xd6],
            file_access_uuid: Some(hex("1a40e6a143b54e24be9884c5323d1a41").try_into().unwrap()),
            version: 1208,
        }
    }

    #[test]
    fn the_specs_vectors() {
        let admin = OwnedNode::list(
            [2],
            vec![
                OwnedNode::leaf(
                    [PASSWORD],
                    hex("01bf821c8e1e120aeda6413ac7b8a64b4ded8d15bbeeaa8bf89f02c6e7709b"),
                ),
                OwnedNode::leaf([STATUS], hex("089f91038005521730")),
                OwnedNode::leaf([SET_KEY], [1, 1]),
                OwnedNode::leaf([NAME], masked("Admin")),
            ],
        );
        assert_eq!(
            account(&tasks(), &[2], &admin).unwrap().to_vec(),
            hex("fd7ee9c1858689de75a95ebf99b187da")
        );
        let full_access = OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([RECORD_ACCESS], [0x55, 0x03]),
                OwnedNode::list(
                    [ITEM_ACCESS],
                    vec![
                        OwnedNode::leaf([0x04], [0xdf]),
                        OwnedNode::leaf([0x11], [0xd7]),
                        OwnedNode::leaf([0x21], [0xd7]),
                    ],
                ),
                OwnedNode::leaf([OTHER_PRIVILEGES], [0x9f, 0xff]),
                OwnedNode::leaf([NAME], masked("[Full Access]")),
            ],
        );
        assert_eq!(
            privilege_set(&tasks(), &full_access).unwrap().to_vec(),
            hex("5f6796ff3ed6536204fc13bd7324301c")
        );
        let reauthenticate = OwnedNode::list(
            [4],
            vec![
                OwnedNode::leaf([KIND], [1, 1]),
                OwnedNode::list([SETS], (1..=3).map(|s| OwnedNode::leaf([s], [])).collect()),
                OwnedNode::leaf([NAME], masked("fmreauthenticate10")),
            ],
        );
        assert_eq!(
            extended_privilege(&reauthenticate).unwrap().to_vec(),
            hex("92f8884732ae4993a3c8d24a1bead8e6")
        );
        assert_eq!(
            file_access_catalog(&tasks(), 1).unwrap().to_vec(),
            hex("54f80051fc0a43eb8c48c2db92100c51")
        );

        // A scalar stored as a list, or an access entry without its byte, is
        // no form the recipes cover: refused, not read as absent.
        let mut listed = full_access.clone();
        listed.set_child(OwnedNode::list(
            [OTHER_PRIVILEGES],
            vec![OwnedNode::leaf([1], [0xff])],
        ));
        assert_eq!(privilege_set(&tasks(), &listed), None);
        let mut empty = full_access.clone();
        empty.put(&[&[ITEM_ACCESS], &[0x04]], OwnedNode::leaf([0x04], []));
        assert_eq!(privilege_set(&tasks(), &empty), None);
    }
}
