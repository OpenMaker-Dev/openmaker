//! The Script Workspace's changes to the script catalog `[17]`, made the
//! way FileMaker Pro makes them before it uploads them (scripts.md,
//! "Saving scripts to a host" and "The catalog index"): every new or
//! changed member `[17, 5, S]` whole, every deleted one emptied, then the
//! whole index `[17, 1]`.
//!
//! [`plan`] compares the catalog the workspace asks for with the catalog as
//! the host stores it now and builds what to upload:
//! - **A new script** (New Script, or Duplicate's copy) gets the id after
//!   the last one the index issued (`00`; ids are never reused), its steps,
//!   its name, `13` = 0 (or the copy's), a fresh UUID `d8`, member flags
//!   `f0` (0, listed in the Scripts menu, or the copy's) and the
//!   modification stamp without a count. A copy's steps get fresh UUIDs, as
//!   FileMaker gives a duplicated step, and it keeps no source UUID `d9`.
//!   Its index entries are a name list entry, a place in the display order,
//!   one more in the count and an entry in the member cache `07` (its name
//!   and flags).
//! - **A changed script or folder** (renamed, Include in Menu, Run script
//!   with full access privileges) keeps every key as stored but the name
//!   and the flag word changed (`f0` bit 3, `f1` bit 0, as 8-byte words),
//!   and gets the stamp with its count raised; the name list and the member
//!   cache follow it, `f1` included (#914).
//! - **A deleted member** is emptied, and its name list entry, its `07`
//!   entry and its place in the display order go; the count goes down. The
//!   last issued id stays. Steps, buttons and privilege set entries that
//!   name it are left as they are (they name an id no member will have).
//! - **Moved members** take their places in the display order, which is
//!   written whole, positions 1, 2, ... (folders are spans of it).
//! - The index gets the catalog's digest `08` for its member cache as
//!   written ([`catalog_digest`]: FileMaker refuses to open a file whose
//!   digest does not match, error 808), and the stamp with its count
//!   raised. `20` and everything else stay byte for byte.
//!
//! A new member's keys are those of every script in the 66 starter files
//! less `d9` and the count `fc`, which FileMaker Pro 26 leaves out of the
//! tables and fields it creates (schema-path.md 5.8): for scripts this is
//! inferred, not compared with FileMaker Pro's own save.
//!
//! Refused (FileMaker error 3, nothing changed), because OpenMaker does not
//! save them yet: new folders and separators (and duplicates of folders);
//! a name another member has, a non-ASCII name, or any name in a file whose
//! name list does not show the ASCII key form; deleting or renaming one of
//! two members of one name (how the name list holds them is not
//! established); an index missing a part every file FileMaker made has; and
//! an index whose digest is not the one OpenMaker computes for its cache
//! (one it cannot reproduce, or a file FileMaker already refuses).

use std::collections::{BTreeMap, BTreeSet};

use om_hbam::slot::be_uint;
use om_hbam::{serial, OwnedNode};
use om_scripts::StepData;

use crate::field_catalog::{listed_key_or_why, name_key, MAX_NAME_CHARS, MAX_NAME_KEY};
use crate::schema_draft::Stamp;
use crate::{DataError, Script, ScriptCatalog, ScriptKind};

/// The script catalog `[17]`, its index `01` and its members `05`.
pub const SCRIPTS: u8 = 0x11;
pub const INDEX: u8 = 0x01;
pub const MEMBERS: u8 = 0x05;

/// Index keys (schema-path.md 5.1; the member cache `07` scripts.md):
/// the last issued id, the name list, the display order, the member count
/// and each member's cached name and flags.
const LAST_ID: u8 = 0x00;
const NAMES: u8 = 0x01;
const ORDER: u8 = 0x03;
const COUNT: u8 = 0x04;
const CACHE: u8 = 0x07;
/// `KGlobal`: the catalog's digest, which FileMaker checks when a file
/// opens (scripts.md, "The script catalog's digest").
const DIGEST: u8 = 0x08;

/// Member keys (script-steps.md 1).
const NAME: u8 = 0x10;
const COMPATIBILITY: u8 = 0x13;
const UNIQUE_ID: u8 = 0xd8;
const SOURCE_UUID: u8 = 0xd9;
/// `KMemberFlags`, bit 3 left out of the Scripts menu; `KPrivateMemberFlags`,
/// bit 0 "Run script with full access privileges" (script-steps.md 1).
const MEMBER_FLAGS: u8 = 0xf0;
const HIDDEN: u64 = 0x8;
const PRIVATE_FLAGS: u8 = 0xf1;
const RUN_WITH_FULL_ACCESS: u64 = 0x1;
const MOD_COUNT: &[u8] = &[0xfc];
const MOD_USER: &[u8] = &[0xfc, 0x01];
const MOD_ACCOUNT: &[u8] = &[0xfc, 0x02];
const MOD_TIME: &[u8] = &[0xfc, 0x03];

/// FileMaker's "Command is unavailable".
const UNSUPPORTED: i32 = 3;

/// What a catalog change uploads, in FileMaker Pro's order: each member,
/// then the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSave {
    /// The new and changed members `[17, 5, S]`, whole, in display order.
    pub members: Vec<OwnedNode>,
    /// The key segments of the deleted members, each emptied as FileMaker
    /// Pro deletes a script (op13 with node state `00`).
    pub deleted: Vec<Vec<u8>>,
    /// The index `[17, 1]`, whole.
    pub index: OwnedNode,
    /// The id the host stores each new member under, by the id the
    /// requested catalog gave it.
    pub assigned: BTreeMap<u64, u64>,
}

fn unsupported(message: impl Into<String>) -> DataError {
    DataError::with_code(UNSUPPORTED, message)
}

/// The index lacks a part every file FileMaker made has.
fn missing(what: &str) -> DataError {
    unsupported(format!(
        "The script catalog's index has no readable {what} (every file FileMaker made has \
         one); OpenMaker does not rebuild it, so nothing was saved."
    ))
}

/// `[u8 length][key]`: how the index stores a member id in a value.
fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The serial id a key string holds.
fn key_string_id(value: &[u8]) -> Option<u64> {
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len())
        .then(|| serial::decode(key))
        .flatten()
}

/// The name list key of an ASCII name in `[17, 1, 01]`: its name key and
/// three zero bytes (schema-path.md 5.6), as [`listed_key`] builds it.
#[cfg(test)]
fn listed(key: &[u8]) -> Vec<u8> {
    let mut listed = key.to_vec();
    listed.extend([0, 0, 0]);
    listed
}

/// Check a script or folder name and return its name key and the key the
/// name list holds it under ([`listed_key`]: FileMaker's, the same in every
/// file whatever its language, scripts.md "The catalog index"). FileMaker's
/// rules for script names are not established; these are OpenMaker's: a
/// name, at most 100 characters (FileMaker's limit for field names), no
/// control character or line break, and a name list key it can hold. Only
/// ASCII names are written for now.
fn check_name(name: &str) -> Result<(Vec<u8>, Vec<u8>), DataError> {
    let refuse = |why: &str| unsupported(format!("“{name}” cannot be a script name: {why}."));
    if name.is_empty() {
        return Err(unsupported("A script needs a name."));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(refuse(&format!(
            "names hold at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(refuse("it holds a control character or line break"));
    }
    if !name.is_ascii() {
        return Err(refuse(
            "OpenMaker saves script names of ASCII characters only for now",
        ));
    }
    let key = name_key(name);
    if key.is_empty() {
        return Err(refuse("it has no character FileMaker tells names apart by"));
    }
    let entry = listed_key_or_why(name).map_err(|why| refuse(&why))?;
    if entry.len() > MAX_NAME_KEY {
        return Err(refuse("its name key is longer than FileMaker stores"));
    }
    Ok((key, entry))
}

/// Set a member's or the index's modification stamp (schema-path.md 8.1):
/// the count raised unless the node is new, then the user, the account and
/// the host's time.
fn stamp(node: &mut OwnedNode, stamp: &Stamp, new: bool) {
    if !new {
        let next = node
            .child(MOD_COUNT)
            .and_then(|n| n.value.as_deref())
            .and_then(key_string_id)
            .unwrap_or(0)
            .saturating_add(1);
        node.set_child(OwnedNode::leaf(
            MOD_COUNT.to_vec(),
            key_string(&serial::encode(next)),
        ));
    }
    node.set_child(om_scripts::text_node(MOD_USER, &stamp.user));
    node.set_child(om_scripts::text_node(MOD_ACCOUNT, &stamp.account));
    // A key string: the time's key is shorter than 8 bytes before 00:16:40
    // (schema-path.md 3.1).
    node.set_child(OwnedNode::leaf(MOD_TIME.to_vec(), key_string(&stamp.time)));
}

/// The digest FileMaker keeps in the index at `08` and checks when a file
/// opens, refusing the file with error 808 when it does not match
/// (scripts.md, "The script catalog's digest"): MD5 over the member cache
/// `07, S` in key order, each entry's private flags `f1` as 8 bytes (zeros
/// when the entry has none) and then its name in UTF-16BE. `None` when an
/// entry has no readable name, or flags of another length.
pub fn catalog_digest(index: &OwnedNode) -> Option<[u8; 16]> {
    use md5::{Digest, Md5};
    let mut entries: Vec<&OwnedNode> = index
        .child(&[CACHE])
        .map(|cache| cache.children.iter().collect())
        .unwrap_or_default();
    entries.sort_by(|a, b| a.segment.cmp(&b.segment));
    let mut md5 = Md5::new();
    for entry in entries {
        let flags = match entry.child(&[PRIVATE_FLAGS]) {
            None => [0; 8],
            Some(node) => <[u8; 8]>::try_from(node.value.as_deref()?).ok()?,
        };
        let name = om_scripts::node_text(entry.child(&[NAME])?)?;
        md5.update(flags);
        for unit in name.encode_utf16() {
            md5.update(unit.to_be_bytes());
        }
    }
    Some(md5.finalize().into())
}

/// The ids of `catalog`'s members, in order.
fn ids(catalog: &ScriptCatalog) -> Vec<u64> {
    catalog.scripts.iter().map(|s| s.id).collect()
}

/// A member flag word (`f0`, `f1`) as a number, 0 when not stored.
fn flags(metadata: &[OwnedNode], key: u8) -> u64 {
    metadata
        .iter()
        .find(|n| n.segment == [key])
        .and_then(|n| n.value.as_deref())
        .and_then(be_uint)
        .unwrap_or(0)
}

/// A flag word as FileMaker stores one: 8 bytes, big-endian.
fn flag_word(key: u8, value: u64) -> OwnedNode {
    OwnedNode::leaf([key], value.to_be_bytes())
}

/// Whether a member's metadata, other than what a catalog change may set
/// (the name, the two flag words and the stamp), is the same.
fn same_rest(a: &Script, b: &Script) -> bool {
    let rest = |s: &Script| -> Vec<OwnedNode> {
        s.metadata
            .iter()
            .filter(|n| {
                !matches!(
                    n.segment.as_slice(),
                    [NAME] | [MEMBER_FLAGS] | [PRIVATE_FLAGS]
                )
            })
            .filter(|n| {
                ![MOD_COUNT, MOD_USER, MOD_ACCOUNT, MOD_TIME].contains(&n.segment.as_slice())
            })
            .cloned()
            .collect()
    };
    rest(a) == rest(b)
}

/// Plan saving `requested` over the host's catalog: `stored` (decoded, in
/// display order), its members' definitions `[17, 5, S]` by id and its
/// index `[17, 1]`, all as read now. `None` when nothing changes. Refused
/// whole (error 3) for a change OpenMaker does not save; nothing is planned
/// then.
pub fn plan(
    index: Option<&OwnedNode>,
    stored: &ScriptCatalog,
    definitions: &BTreeMap<u64, OwnedNode>,
    requested: &ScriptCatalog,
    by: &Stamp,
    uuid: &mut dyn FnMut() -> [u8; 16],
) -> Result<Option<CatalogSave>, DataError> {
    let before: BTreeSet<u64> = ids(stored).into_iter().collect();
    let after: BTreeSet<u64> = ids(requested).into_iter().collect();
    if after.len() != requested.scripts.len() {
        return Err(unsupported(
            "The scripts asked for name one member twice; nothing was saved.",
        ));
    }
    let deleted: Vec<&Script> = stored
        .scripts
        .iter()
        .filter(|s| !after.contains(&s.id))
        .collect();
    let mut new = Vec::new();
    // Changed members: (as asked, as stored).
    let mut changed: Vec<(&Script, &Script)> = Vec::new();
    for script in &requested.scripts {
        let Some(old) = stored.scripts.iter().find(|s| s.id == script.id) else {
            if script.kind != ScriptKind::Script {
                return Err(unsupported(
                    "OpenMaker does not save new folders or separators yet; nothing was saved.",
                ));
            }
            new.push(script);
            continue;
        };
        if script.kind != old.kind {
            return Err(unsupported(format!(
                "“{}” changed kind; OpenMaker does not save that, so nothing was saved.",
                old.name
            )));
        }
        let (menu, access) = (
            flags(&script.metadata, MEMBER_FLAGS) ^ flags(&old.metadata, MEMBER_FLAGS),
            flags(&script.metadata, PRIVATE_FLAGS) ^ flags(&old.metadata, PRIVATE_FLAGS),
        );
        if menu & !HIDDEN != 0
            || access & !RUN_WITH_FULL_ACCESS != 0
            || (access != 0 && script.kind != ScriptKind::Script)
            || !same_rest(script, old)
        {
            return Err(unsupported(format!(
                "“{}” was changed in a way OpenMaker does not save; nothing was saved.",
                old.name
            )));
        }
        if script.name != old.name
            && !matches!(script.kind, ScriptKind::Script | ScriptKind::Folder)
        {
            return Err(unsupported(format!(
                "A folder's end and a separator keep their names (“{}”).",
                old.name
            )));
        }
        if script.name != old.name || menu != 0 || access != 0 {
            changed.push((script, old));
        }
    }
    let kept: Vec<u64> = ids(requested)
        .into_iter()
        .filter(|id| before.contains(id))
        .collect();
    let moved = kept
        != ids(stored)
            .into_iter()
            .filter(|id| after.contains(id))
            .collect::<Vec<_>>();
    if new.is_empty() && changed.is_empty() && deleted.is_empty() && !moved {
        return Ok(None);
    }
    let Some(index) = index else {
        return Err(missing("index"));
    };
    let mut index = index.clone();
    let last = match index.child(&[LAST_ID]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(key_string_id)
            .ok_or_else(|| missing("last issued id"))?,
        None if before.is_empty() => 0,
        None => return Err(missing("last issued id")),
    };
    let count = match index.child(&[COUNT]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| missing("member count"))?,
        None if before.is_empty() => 0,
        None => return Err(missing("member count")),
    };
    if !before.is_empty() {
        for (key, what) in [
            (NAMES, "name list"),
            (ORDER, "display order"),
            (CACHE, "member cache (07)"),
        ] {
            if index.child(&[key]).is_none() {
                return Err(missing(what));
            }
        }
        // The save rewrites the digest, so it must be the digest FileMaker
        // left: one OpenMaker cannot reproduce (flags or names it reads
        // otherwise), or a file FileMaker would already refuse, is not
        // written over.
        let stored = index.child(&[DIGEST]).and_then(|n| n.value.as_deref());
        if stored.is_none() || catalog_digest(&index).as_ref().map(|d| &d[..]) != stored {
            return Err(unsupported(
                "The script catalog's digest (08 in its index) is not the one OpenMaker \
                 computes for its scripts, so OpenMaker does not save a change to them; nothing \
                 was saved.",
            ));
        }
    }
    let renamed: Vec<(&Script, &Script)> = changed
        .iter()
        .copied()
        .filter(|(s, old)| s.name != old.name)
        .collect();

    // A member's name list entry is found by the id it holds, so a deleted
    // or renamed member's entry goes whatever its language's key. One that
    // another member of the same name keeps would leave that member
    // unlisted: how FileMaker lists two of one name is not established.
    let going: BTreeSet<u64> = deleted
        .iter()
        .map(|s| s.id)
        .chain(renamed.iter().map(|(s, _)| s.id))
        .collect();
    for script in stored.scripts.iter().filter(|s| going.contains(&s.id)) {
        let shared = stored.scripts.iter().any(|other| {
            other.id != script.id
                && after.contains(&other.id)
                && !going.contains(&other.id)
                && name_key(&other.name) == name_key(&script.name)
        });
        if shared {
            return Err(unsupported(format!(
                "Another script or folder is named “{}”, and OpenMaker does not save a change \
                 to one of two members of one name yet; nothing was saved.",
                script.name
            )));
        }
    }
    let unlist = |index: &mut OwnedNode, segment: &[u8]| {
        let value = key_string(segment);
        index
            .ensure(&[&[NAMES]])
            .children
            .retain(|e| e.value.as_deref() != Some(value.as_slice()));
    };

    // The names every member keeps, by name key, to refuse a second one:
    // the keys OpenMaker computes and the ones the name list holds.
    let mut names: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
    for script in &stored.scripts {
        if !going.contains(&script.id) {
            names.insert(name_key(&script.name), script.id);
        }
    }
    let listed_names: BTreeMap<Vec<u8>, u64> = index
        .child(&[NAMES])
        .map(|n| n.children.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| Some((e.segment.clone(), key_string_id(e.value.as_deref()?)?)))
        .filter(|(_, id)| !going.contains(id))
        .collect();
    let mut take = |name: &str, id: u64| -> Result<Vec<u8>, DataError> {
        let (key, entry) = check_name(name)?;
        if names.insert(key, id).is_some() || listed_names.contains_key(&entry) {
            return Err(unsupported(format!(
                "Another script or folder is named “{name}”, and OpenMaker does not save two \
                 of one name yet; nothing was saved."
            )));
        }
        Ok(entry)
    };

    let mut members = Vec::new();
    let mut assigned = BTreeMap::new();
    let mut next = last.max(before.iter().copied().max().unwrap_or(0));
    for script in &new {
        next = next
            .checked_add(1)
            .ok_or_else(|| unsupported("The script catalog has no ids left."))?;
        assigned.insert(script.id, next);
    }
    // Every name is checked before anything is written.
    let mut keys = BTreeMap::new();
    for (script, _) in &renamed {
        keys.insert(script.id, take(&script.name, script.id)?);
    }
    for script in &new {
        keys.insert(script.id, take(&script.name, assigned[&script.id])?);
    }

    for script in &deleted {
        let segment = serial::encode(script.id);
        unlist(&mut index, &segment);
        index
            .ensure(&[&[CACHE]])
            .children
            .retain(|e| e.segment != segment);
    }
    for script in &requested.scripts {
        if let Some(&id) = assigned.get(&script.id) {
            let segment = serial::encode(id);
            let member = new_member(script, id, by, uuid);
            let flags = flags(&script.metadata, MEMBER_FLAGS);
            index.ensure(&[&[NAMES]]).set_child(OwnedNode::leaf(
                keys[&script.id].clone(),
                key_string(&segment),
            ));
            let mut cached = vec![
                om_scripts::text_node(&[NAME], &script.name),
                flag_word(MEMBER_FLAGS, flags),
            ];
            // A copy of a script with private flags caches them as its
            // member keeps them (#914).
            if let Some(private) = member.child(&[PRIVATE_FLAGS]) {
                cached.push(private.clone());
            }
            index
                .ensure(&[&[CACHE]])
                .set_child(OwnedNode::list(segment.clone(), cached));
            members.push(member);
        } else if let Some((_, old)) = changed.iter().find(|(s, _)| s.id == script.id) {
            let segment = serial::encode(script.id);
            let Some(stored_member) = definitions.get(&script.id) else {
                return Err(unsupported(format!(
                    "The script “{}” has no definition on the host.",
                    old.name
                )));
            };
            let mut member = stored_member.clone();
            let Some(cached) = index.at(&[&[CACHE], &segment]) else {
                return Err(missing(&format!("member cache entry for “{}”", old.name)));
            };
            let mut cached = cached.clone();
            if script.name != old.name {
                member.set_child(om_scripts::text_node(&[NAME], &script.name));
                cached.set_child(om_scripts::text_node(&[NAME], &script.name));
                unlist(&mut index, &segment);
                index.ensure(&[&[NAMES]]).set_child(OwnedNode::leaf(
                    keys[&script.id].clone(),
                    key_string(&segment),
                ));
            }
            let menu = flags(&script.metadata, MEMBER_FLAGS);
            if menu != flags(&old.metadata, MEMBER_FLAGS) {
                member.set_child(flag_word(MEMBER_FLAGS, menu));
                cached.set_child(flag_word(MEMBER_FLAGS, menu));
            }
            // The cache keeps the member's private flags too, as FileMaker
            // Pro writes them, and the digest is the cache's: with `f1` only
            // in the member, Pro does not see the script's full access
            // (#914).
            let access = flags(&script.metadata, PRIVATE_FLAGS);
            if access != flags(&old.metadata, PRIVATE_FLAGS) {
                member.set_child(flag_word(PRIVATE_FLAGS, access));
                cached.set_child(flag_word(PRIVATE_FLAGS, access));
            }
            index.put(&[&[CACHE], &segment], cached);
            stamp(&mut member, by, false);
            members.push(member);
        }
    }
    if !new.is_empty() || !deleted.is_empty() || moved {
        let order = requested
            .scripts
            .iter()
            .enumerate()
            .map(|(i, script)| {
                let id = assigned.get(&script.id).copied().unwrap_or(script.id);
                OwnedNode::leaf(
                    (i as u32 + 1).to_be_bytes().to_vec(),
                    key_string(&serial::encode(id)),
                )
            })
            .collect();
        index.set_child(OwnedNode::list(vec![ORDER], order));
    }
    if !new.is_empty() {
        index.set_child(OwnedNode::leaf(
            vec![LAST_ID],
            key_string(&serial::encode(next)),
        ));
    }
    if !new.is_empty() || !deleted.is_empty() {
        let count = u32::try_from(new.len())
            .ok()
            .and_then(|n| count.checked_add(n))
            .and_then(|n| n.checked_sub(u32::try_from(deleted.len()).ok()?))
            .ok_or_else(|| missing("member count that covers its members"))?;
        index.set_child(OwnedNode::leaf(vec![COUNT], count.to_be_bytes().to_vec()));
    }
    // FileMaker refuses a file whose digest is not its member cache's
    // (error 808), so the digest follows the cache as written.
    let digest = catalog_digest(&index)
        .ok_or_else(|| missing("member cache (07) entry with a name and flags"))?;
    index.set_child(OwnedNode::leaf(vec![DIGEST], digest.to_vec()));
    stamp(&mut index, by, false);
    Ok(Some(CatalogSave {
        members,
        deleted: deleted.iter().map(|s| serial::encode(s.id)).collect(),
        index,
        assigned,
    }))
}

/// The member `[17, 5, id]` of a new script: New Script's (no steps, no
/// metadata) or Duplicate's (a copy of another's steps and keys). Its
/// steps that have a UUID get fresh ones, as FileMaker gives a duplicated
/// step; it gets `10`, `13` (kept from a copy, else 0), a fresh UUID `d8`,
/// its member flags as an 8-byte word (`f0`, and `f1` when stored) and a
/// new member's stamp, without the copy's source UUID `d9` and count `fc`.
fn new_member(
    script: &Script,
    id: u64,
    by: &Stamp,
    uuid: &mut dyn FnMut() -> [u8; 16],
) -> OwnedNode {
    let mut copy = Script {
        id,
        ..script.clone()
    };
    for step in &mut copy.steps {
        if step.unique_id().is_some() {
            step.set_data_leaf(
                StepData::UNIQUE_ID,
                Some(OwnedNode::leaf([StepData::UNIQUE_ID], uuid())),
            );
        }
    }
    let mut member = copy.encode();
    member.children.retain(|c| {
        !matches!(c.segment.as_slice(), [SOURCE_UUID])
            && ![MOD_COUNT, MOD_USER, MOD_ACCOUNT, MOD_TIME].contains(&c.segment.as_slice())
    });
    member.set_child(om_scripts::text_node(&[NAME], &script.name));
    if member.child(&[COMPATIBILITY]).is_none() {
        member.set_child(OwnedNode::leaf([COMPATIBILITY], [0u8; 4]));
    }
    member.set_child(OwnedNode::leaf([UNIQUE_ID], uuid()));
    member.set_child(flag_word(
        MEMBER_FLAGS,
        flags(&script.metadata, MEMBER_FLAGS),
    ));
    if member.child(&[PRIVATE_FLAGS]).is_some() {
        member.set_child(flag_word(
            PRIVATE_FLAGS,
            flags(&script.metadata, PRIVATE_FLAGS),
        ));
    }
    stamp(&mut member, by, true);
    member
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::{KeySegment, Node};

    fn view(node: &OwnedNode) -> Node<'_> {
        Node {
            segment: KeySegment(&node.segment),
            value: node.value.as_deref(),
            push: node.push,
            children: node.children.iter().map(view).collect(),
        }
    }

    fn by() -> Stamp {
        Stamp {
            user: "Tester".into(),
            account: "Admin".into(),
            time: vec![0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x80],
        }
    }

    /// The English Tasks starter's first two members and their index
    /// entries, as stored: the folder "Tasks" (1, flags 9) and the script
    /// "Go to Entry Field" (2, flags 8), last id 2.
    fn starter() -> (OwnedNode, BTreeMap<u64, OwnedNode>, ScriptCatalog) {
        let leaf = |k: &[u8], v: &[u8]| OwnedNode::leaf(k.to_vec(), v.to_vec());
        let hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        let folder = OwnedNode::list(
            vec![1],
            vec![
                om_scripts::text_node(&[NAME], "Tasks"),
                leaf(&[0x13], &[0; 4]),
                leaf(&[0xd8], &[1; 16]),
                leaf(&[0xf0], &[0, 0, 0, 0, 0, 0, 0, 9]),
                leaf(&[0xfc], &[1, 1]),
            ],
        );
        let script = OwnedNode::list(
            vec![2],
            vec![
                leaf(&[0x04], &[]),
                om_scripts::text_node(&[NAME], "Go to Entry Field"),
                leaf(&[0x13], &[0; 4]),
                leaf(&[0xd8], &[2; 16]),
                leaf(&[0xd9], &[3; 16]),
                leaf(&[0xf0], &[0, 0, 0, 0, 0, 0, 0, 8]),
                leaf(&[0xfc], &[1, 2]),
            ],
        );
        let mut index = OwnedNode::list(
            vec![1],
            vec![
                leaf(&[0x00], &[1, 2]),
                OwnedNode::list(
                    vec![0x01],
                    vec![
                        // Stored name keys from the Tasks starter file.
                        leaf(
                            &hex(
                                "12b0138e020a1433138e020a126b136d143313da149c020a12a312ec126b13301250\
                                 000000",
                            ),
                            &[1, 2],
                        ),
                        leaf(&hex("1433120f1410131e1410000000"), &[1, 1]),
                    ],
                ),
                OwnedNode::list(
                    vec![0x03],
                    vec![leaf(&[0, 0, 0, 1], &[1, 1]), leaf(&[0, 0, 0, 2], &[1, 2])],
                ),
                leaf(&[0x04], &[0, 0, 0, 2]),
                OwnedNode::list(
                    vec![0x07],
                    vec![
                        OwnedNode::list(
                            vec![1],
                            vec![
                                om_scripts::text_node(&[NAME], "Tasks"),
                                leaf(&[0xf0], &[0, 0, 0, 0, 0, 0, 0, 9]),
                            ],
                        ),
                        OwnedNode::list(
                            vec![2],
                            vec![
                                om_scripts::text_node(&[NAME], "Go to Entry Field"),
                                leaf(&[0xf0], &[0, 0, 0, 0, 0, 0, 0, 8]),
                            ],
                        ),
                    ],
                ),
                leaf(&[0x20], &[4]),
                leaf(&[0xd8], &[8; 16]),
                leaf(&[0xfc], &[1, 3]),
            ],
        );
        let digest = catalog_digest(&index).unwrap();
        index.set_child(leaf(&[0x08], &digest));
        let definitions = BTreeMap::from([(1, folder.clone()), (2, script.clone())]);
        let catalog = ScriptCatalog {
            scripts: vec![
                Script::decode(&view(&folder)).unwrap(),
                Script::decode(&view(&script)).unwrap(),
            ],
            ..ScriptCatalog::default()
        };
        (index, definitions, catalog)
    }

    #[test]
    fn name_keys_are_the_starter_files() {
        // "Tasks" and "Go to Entry Field" as the Tasks starter lists them.
        assert_eq!(
            listed(&name_key("Tasks")),
            [0x14, 0x33, 0x12, 0x0f, 0x14, 0x10, 0x13, 0x1e, 0x14, 0x10, 0, 0, 0]
        );
    }

    #[test]
    fn the_digest_is_the_tasks_starters() {
        // The English Tasks starter's member cache and its stored `08`
        // (scripts.md, "The script catalog's digest").
        let names = [
            "Tasks",
            "Go to Entry Field",
            "Add Related Record",
            "Choose Assignee",
            "Create Assignee Entry",
            "Cancel Add Assignee",
            "Sort by Name",
            "Sort by Status",
            "--",
        ];
        let cache = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                OwnedNode::list(
                    serial::encode(i as u64 + 1),
                    vec![
                        om_scripts::text_node(&[NAME], name),
                        OwnedNode::leaf([MEMBER_FLAGS], [0, 0, 0, 0, 0, 0, 0, 8]),
                    ],
                )
            })
            .collect();
        let index = OwnedNode::list(vec![1], vec![OwnedNode::list(vec![CACHE], cache)]);
        assert_eq!(
            catalog_digest(&index).unwrap(),
            [
                0x27, 0x15, 0xc7, 0x2b, 0xbe, 0xdd, 0xe7, 0x53, 0x30, 0xff, 0x77, 0x46, 0x98, 0xfd,
                0x66, 0xaa
            ]
        );
    }

    #[test]
    fn a_new_script_gets_the_next_id_its_keys_and_index_entries() {
        let (index, definitions, stored) = starter();
        let mut requested = stored.clone();
        // The workspace numbered it 9; the host's next id is 3.
        requested.scripts.insert(
            1,
            Script {
                id: 9,
                name: "Find Late Tasks".into(),
                kind: ScriptKind::Script,
                ..Script::default()
            },
        );
        let save = plan(
            Some(&index),
            &stored,
            &definitions,
            &requested,
            &by(),
            &mut || [5; 16],
        )
        .unwrap()
        .unwrap();
        assert_eq!(save.assigned, BTreeMap::from([(9, 3)]));
        let [member] = save.members.as_slice() else {
            panic!("one member: {:?}", save.members)
        };
        assert_eq!(member.segment, [3]);
        let keys: Vec<&[u8]> = member
            .children
            .iter()
            .map(|c| c.segment.as_slice())
            .collect();
        assert_eq!(
            keys,
            [
                &[0x04][..],
                &[0x10],
                &[0x13],
                &[0xd8],
                &[0xf0],
                &[0xfc, 1],
                &[0xfc, 2],
                &[0xfc, 3]
            ]
        );
        let script = Script::decode(&view(member)).unwrap();
        assert_eq!(
            (script.id, script.name.as_str(), script.kind),
            (3, "Find Late Tasks", ScriptKind::Script)
        );
        assert!(script.steps.is_empty() && script.in_scripts_menu());
        let at = |path: &[&[u8]]| save.index.at(path).and_then(|n| n.value.clone());
        assert_eq!(at(&[&[0x00]]), Some(vec![1, 3]));
        assert_eq!(at(&[&[0x04]]), Some(vec![0, 0, 0, 3]));
        assert_eq!(at(&[&[0xfc]]), Some(vec![1, 4]));
        // After the folder, before Go to Entry Field: renumbered.
        let order: Vec<Vec<u8>> = save
            .index
            .child(&[0x03])
            .unwrap()
            .children
            .iter()
            .map(|e| e.value.clone().unwrap())
            .collect();
        assert_eq!(order, [vec![1, 1], vec![1, 3], vec![1, 2]]);
        assert_eq!(
            at(&[&[0x01], &listed(&name_key("Find Late Tasks"))]),
            Some(vec![1, 3])
        );
        assert_eq!(
            save.index
                .at(&[&[0x07], &[3], &[NAME]])
                .and_then(om_scripts::node_text),
            Some("Find Late Tasks".into())
        );
        assert_eq!(at(&[&[0x07], &[3], &[0xf0]]), Some(vec![0; 8]));
        // The digest follows the cache with the new entry; `20` stays.
        let digest = catalog_digest(&save.index).unwrap();
        assert_eq!(at(&[&[0x08]]), Some(digest.to_vec()));
        assert_ne!(Some(digest.to_vec()), index.child(&[0x08]).unwrap().value);
        assert_eq!(at(&[&[0x20]]), Some(vec![4]));
    }

    #[test]
    fn a_rename_keeps_every_key_but_the_name_and_moves_its_name_entry() {
        let (index, definitions, stored) = starter();
        let mut requested = stored.clone();
        requested.scripts[1].name = "Go to First Field".into();
        let save = plan(
            Some(&index),
            &stored,
            &definitions,
            &requested,
            &by(),
            &mut || unreachable!("a rename makes no UUID"),
        )
        .unwrap()
        .unwrap();
        let [member] = save.members.as_slice() else {
            panic!("one member")
        };
        let original = &definitions[&2];
        for child in &original.children {
            match child.segment.as_slice() {
                [NAME] | [0xfc] => {}
                _ => assert_eq!(member.child(&child.segment), Some(child)),
            }
        }
        assert_eq!(member.child(&[0xfc]).unwrap().value, Some(vec![1, 3]));
        let names = save.index.child(&[0x01]).unwrap();
        assert!(names
            .child(&listed(&name_key("Go to Entry Field")))
            .is_none());
        assert_eq!(
            names
                .child(&listed(&name_key("Go to First Field")))
                .unwrap()
                .value,
            Some(vec![1, 2])
        );
        // No new member: the order, last id and count stay.
        for key in [0x00, 0x03, 0x04] {
            assert_eq!(save.index.child(&[key]), index.child(&[key]));
        }
        // An old name listed under a key OpenMaker does not compute (an
        // accented name's second level) still goes: by the id it holds.
        let mut accented = index.clone();
        let names = accented.at_mut(&[&[0x01]]).unwrap();
        names.children.retain(|e| e.value != Some(vec![1, 2]));
        names.set_child(OwnedNode::leaf(
            vec![0x12, 0x0f, 0, 0, 1, 0x2f, 0],
            vec![1, 2],
        ));
        let mut stored_accented = stored.clone();
        stored_accented.scripts[1].name = "Aller à l'entrée".into();
        let save = plan(
            Some(&accented),
            &stored_accented,
            &definitions,
            &requested,
            &by(),
            &mut || unreachable!(),
        )
        .unwrap()
        .unwrap();
        let listed_two: Vec<&OwnedNode> = save
            .index
            .child(&[0x01])
            .unwrap()
            .children
            .iter()
            .filter(|e| e.value == Some(vec![1, 2]))
            .collect();
        assert_eq!(listed_two.len(), 1);
        assert_eq!(
            listed_two[0].segment,
            listed(&name_key("Go to First Field"))
        );
        // A copy read before another connection saved the script's steps
        // (an older stamp) renames it too, over the stored member.
        let mut stale = requested.clone();
        for node in &mut stale.scripts[1].metadata {
            if node.segment == [0xfc] {
                node.value = Some(vec![1, 1]);
            }
        }
        let save = plan(
            Some(&index),
            &stored,
            &definitions,
            &stale,
            &by(),
            &mut || unreachable!(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            save.members[0].child(&[0xfc]).unwrap().value,
            Some(vec![1, 3])
        );
    }

    fn planned(stored: &ScriptCatalog, requested: &ScriptCatalog) -> CatalogSave {
        let (index, definitions, _) = starter();
        let mut n = 0u8;
        plan(
            Some(&index),
            stored,
            &definitions,
            requested,
            &by(),
            &mut || {
                n += 1;
                [0x40 + n; 16]
            },
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn a_delete_empties_the_member_and_takes_its_index_entries() {
        let (index, _, stored) = starter();
        let mut requested = stored.clone();
        requested.scripts.remove(1);
        let save = planned(&stored, &requested);
        assert_eq!(save.deleted, [vec![2]]);
        assert!(save.members.is_empty());
        let names = save.index.child(&[0x01]).unwrap();
        assert!(names.children.iter().all(|e| e.value != Some(vec![1, 2])));
        assert_eq!(names.children.len(), 1);
        assert!(save.index.at(&[&[0x07], &[2]]).is_none());
        let order: Vec<(Vec<u8>, Vec<u8>)> = save
            .index
            .child(&[0x03])
            .unwrap()
            .children
            .iter()
            .map(|e| (e.segment.clone(), e.value.clone().unwrap()))
            .collect();
        assert_eq!(order, [(vec![0, 0, 0, 1], vec![1, 1])]);
        assert_eq!(
            save.index.child(&[0x04]).unwrap().value,
            Some(vec![0, 0, 0, 1])
        );
        // The last issued id stays: a later script never takes 2.
        assert_eq!(save.index.child(&[0x00]), index.child(&[0x00]));
    }

    #[test]
    fn a_duplicate_copies_steps_and_keys_with_fresh_uuids_and_no_source_uuid() {
        let (_, _, stored) = starter();
        let mut requested = stored.clone();
        // What the scripts pane's Duplicate makes: a copy of the stored
        // script, numbered and named by the pane.
        let mut copy = stored.scripts[1].clone();
        copy.id = 3;
        copy.name = "Go to Entry Field Copy".into();
        let mut step = om_scripts::ScriptStep::new(89);
        step.set_unique_ids([9; 16], Some([8; 16]));
        copy.insert_steps(0, [step]).unwrap();
        requested.scripts.push(copy);
        let save = planned(&stored, &requested);
        let [member] = save.members.as_slice() else {
            panic!("one member")
        };
        assert_eq!(member.segment, [3]);
        assert!(member.child(&[0xd9]).is_none() && member.child(&[0xfc]).is_none());
        assert_eq!(member.child(&[0xd8]).unwrap().value, Some(vec![0x42; 16]));
        // The copy's member flags (left out of the Scripts menu) and 13.
        assert_eq!(
            member.child(&[0xf0]).unwrap().value,
            Some(vec![0, 0, 0, 0, 0, 0, 0, 8])
        );
        assert_eq!(member.child(&[0x13]).unwrap().value, Some(vec![0; 4]));
        let script = Script::decode(&view(member)).unwrap();
        assert_eq!(script.steps.len(), 1);
        assert_eq!(script.steps[0].unique_id(), Some([0x41; 16]));
        assert_eq!(
            save.index.at(&[&[0x07], &[3], &[0xf0]]).unwrap().value,
            Some(vec![0, 0, 0, 0, 0, 0, 0, 8])
        );
    }

    #[test]
    fn moves_and_options_rewrite_the_order_and_the_flag_words() {
        let (_, definitions, stored) = starter();
        let mut requested = stored.clone();
        requested.scripts.reverse();
        requested.scripts[0].set_in_scripts_menu(true);
        requested.scripts[0].set_runs_with_full_access(true);
        let save = planned(&stored, &requested);
        let order: Vec<Vec<u8>> = save
            .index
            .child(&[0x03])
            .unwrap()
            .children
            .iter()
            .map(|e| e.value.clone().unwrap())
            .collect();
        assert_eq!(order, [vec![1, 2], vec![1, 1]]);
        let [member] = save.members.as_slice() else {
            panic!("one member")
        };
        assert_eq!(member.child(&[0xf0]).unwrap().value, Some(vec![0; 8]));
        assert_eq!(
            member.child(&[0xf1]).unwrap().value,
            Some(vec![0, 0, 0, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            save.index.at(&[&[0x07], &[2], &[0xf0]]).unwrap().value,
            Some(vec![0; 8])
        );
        // The member cache keeps the private flags as the member does, as
        // FileMaker Pro writes them, and the digest is over them: with `f1`
        // only in the member, Pro did not see the full access (#914).
        assert_eq!(
            save.index.at(&[&[0x07], &[2], &[0xf1]]).unwrap().value,
            Some(vec![0, 0, 0, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            save.index.child(&[0x08]).unwrap().value.as_deref(),
            Some(&catalog_digest(&save.index).unwrap()[..])
        );
        // Everything else as stored.
        for child in &definitions[&2].children {
            if !matches!(child.segment.as_slice(), [0xf0] | [0xfc]) {
                assert_eq!(member.child(&child.segment), Some(child));
            }
        }
        assert!(save.deleted.is_empty());
    }

    #[test]
    fn new_folders_a_taken_name_and_a_missing_index_are_refused() {
        let (index, definitions, stored) = starter();
        let refused = |requested: &ScriptCatalog| {
            plan(
                Some(&index),
                &stored,
                &definitions,
                requested,
                &by(),
                &mut || [0; 16],
            )
            .unwrap_err()
        };
        let mut folder = stored.clone();
        folder.scripts.push(Script {
            id: 3,
            name: "New Folder".into(),
            kind: ScriptKind::Folder,
            ..Script::default()
        });
        assert!(refused(&folder).message.contains("new folders"));
        let mut taken = stored.clone();
        taken.scripts[1].name = "TASKS".into();
        assert!(refused(&taken).message.contains("two of one name"));
        let mut accented = stored.clone();
        accented.scripts[1].name = "Créer".into();
        assert!(refused(&accented).message.contains("ASCII"));
        let mut renamed = stored.clone();
        renamed.scripts[1].name = "Other".into();
        let error = plan(None, &stored, &definitions, &renamed, &by(), &mut || {
            [0; 16]
        })
        .unwrap_err();
        assert_eq!(error.code, Some(3));
        // A digest that is not the cache's: FileMaker refuses the file, and
        // OpenMaker does not write over it.
        let mut tampered = index.clone();
        tampered.set_child(OwnedNode::leaf([0x08], [7; 16]));
        let error = plan(
            Some(&tampered),
            &stored,
            &definitions,
            &renamed,
            &by(),
            &mut || [0; 16],
        )
        .unwrap_err();
        assert!(error.message.contains("digest"), "{}", error.message);
        assert!(plan(
            Some(&index),
            &stored,
            &definitions,
            &stored,
            &by(),
            &mut || [0; 16]
        )
        .unwrap()
        .is_none());
    }
}
