//! Revisions: the SHA-256 of content in a canonical form, without
//! modification stamps (schema-path.md 8).
//!
//! The canonical form walks a subtree in key order, its top node first.
//! Each node is written as its segment (a `u64` big-endian length, then
//! the bytes), a tag and what it holds, and its number of children (`u64`
//! big-endian), then each child the same way. The tag is `0` for no value,
//! `1` for a value (framed as the segment is) and `2` for a Secret path,
//! whose value and children are not written. Every part is framed by a
//! length, a tag or a count, so two different subtrees never give the same
//! bytes. The top node's segment is written too: a member's revision
//! includes its key, its id.
//!
//! A value is written as its bytes, however the tree holds it: a value too
//! large for one chunk, or non-ASCII text, is stored as a node with a
//! header `0` and pieces `1..n` (file-format.md 3.6), and such a node is
//! written as the value it assembles to ([`OwnedNode::assembled`]), with no
//! children and as a leaf. So a value kept inline in memory and the same
//! value split by the writer have one revision.
//!
//! There are two forms:
//!
//! - the general form ([`catalog_revision`]): stamps are left out wherever
//!   they occur, and whether a node is stored as a list is not written.
//!   Every object but a script uses it, as do a table's `fields_revision`
//!   and the layout catalog's `layouts_revision`.
//! - the script form ([`script_form_revision`]): only the top node's stamp
//!   is left out, and each node's list flag (`1` a list, `0` a leaf) is
//!   written after its segment. Scripts used it first; it stays theirs.

use crate::tree::OwnedNode;

/// A content revision.
pub type Revision = [u8; 32];

/// The keys of a modification stamp (schema-path.md 8.1): `fc` the
/// modification count, `fc 01` the user name, `fc 02` the account, `fc 03`
/// the host time.
pub const MOD_COUNT: &[u8] = &[0xfc];
pub const MOD_USER: &[u8] = &[0xfc, 0x01];
pub const MOD_ACCOUNT: &[u8] = &[0xfc, 0x02];
pub const MOD_TIME: &[u8] = &[0xfc, 0x03];

/// Whether `segment` is a key of a modification stamp.
pub fn is_stamp(segment: &[u8]) -> bool {
    [MOD_COUNT, MOD_USER, MOD_ACCOUNT, MOD_TIME].contains(&segment)
}

/// The script form over a whole subtree, nothing left out but its own
/// stamp and nothing hidden. A script's revision is
/// [`super::revision_of`], which hides its Secret paths; this is the raw
/// form, for hashing a node that is no object.
pub fn script_form_revision(node: &OwnedNode) -> Revision {
    digest(node, Form::Script, &[])
}

/// The general form over a whole subtree, every path in it, Data
/// included: a table's `fields_revision` over its field catalog `[T, 3]`
/// and `layouts_revision` over the layout catalog's index `[4, 1]`. An
/// object's revision is [`super::revision_of`], which leaves its Data
/// paths out.
pub fn catalog_revision(node: &OwnedNode) -> Revision {
    digest(node, Form::General, &[])
}

/// The two canonical forms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Form {
    General,
    Script,
}

/// The revision of `node` in `form`, each path of `secret` (relative to
/// `node`) written as a Secret.
pub(crate) fn digest(node: &OwnedNode, form: Form, secret: &[super::Path]) -> Revision {
    let mut bytes = Vec::new();
    let stamps = match form {
        Form::General => Stamps::Everywhere,
        Form::Script => Stamps::Own,
    };
    let mut at = Vec::new();
    canonical(node, stamps, secret, &mut at, &mut bytes);
    om_crypto::sha256(&bytes)
}

/// A revision as text: `sha256:` and 64 lowercase hex digits.
pub fn revision_text(revision: &Revision) -> String {
    format!("sha256:{}", super::rules::hex(revision))
}

/// A revision from its text, `None` when it is not one.
pub fn parse_revision(text: &str) -> Option<Revision> {
    let hex = text.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut revision = [0u8; 32];
    for (i, byte) in revision.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(revision)
}

/// Which modification stamps a canonical form leaves out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stamps {
    /// The node's own (and list flags are written).
    Own,
    /// None below the node's own.
    Kept,
    /// Every one, at any depth (and no list flags).
    Everywhere,
}

fn canonical(
    node: &OwnedNode,
    stamps: Stamps,
    secret: &[super::Path],
    at: &mut Vec<Vec<u8>>,
    out: &mut Vec<u8>,
) {
    fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u64).to_be_bytes());
        out.extend_from_slice(b);
    }
    bytes(out, &node.segment);
    if !at.is_empty() && secret.iter().any(|s| s == at) {
        if stamps != Stamps::Everywhere {
            out.push(0);
        }
        out.push(2);
        out.extend_from_slice(&0u64.to_be_bytes());
        return;
    }
    // A chunked value is written as the value it assembles to.
    let assembled = match &node.value {
        None if !node.children.is_empty() => node.assembled(),
        _ => None,
    };
    if let Some(value) = assembled {
        if stamps != Stamps::Everywhere {
            out.push(0);
        }
        out.push(1);
        bytes(out, &value);
        out.extend_from_slice(&0u64.to_be_bytes());
        return;
    }
    if stamps != Stamps::Everywhere {
        out.push(u8::from(node.push));
    }
    match &node.value {
        Some(value) => {
            out.push(1);
            bytes(out, value);
        }
        None => out.push(0),
    }
    let children: Vec<&OwnedNode> = node
        .children
        .iter()
        .filter(|c| stamps == Stamps::Kept || !is_stamp(&c.segment))
        .collect();
    out.extend_from_slice(&(children.len() as u64).to_be_bytes());
    let below = match stamps {
        Stamps::Own | Stamps::Kept => Stamps::Kept,
        Stamps::Everywhere => Stamps::Everywhere,
    };
    for child in children {
        at.push(child.segment.clone());
        canonical(child, below, secret, at, out);
        at.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(steps: &[u8]) -> OwnedNode {
        OwnedNode::list(
            vec![0x05],
            vec![
                OwnedNode::leaf(vec![0x04], steps.to_vec()),
                OwnedNode::leaf(vec![0x10], b"Greeting".to_vec()),
                OwnedNode::leaf(MOD_COUNT.to_vec(), vec![1, 1]),
            ],
        )
    }

    #[test]
    fn the_script_form_leaves_out_the_stamp_only() {
        let a = definition(&[1; 28]);
        let mut stamped = a.clone();
        stamped.children[2].value = Some(vec![1, 2]);
        stamped.set_child(OwnedNode::leaf(MOD_USER.to_vec(), b"x".to_vec()));
        assert_eq!(script_form_revision(&a), script_form_revision(&stamped));
        assert_ne!(
            script_form_revision(&a),
            script_form_revision(&definition(&[2; 28]))
        );
        let mut renamed = a.clone();
        renamed.children[1].value = Some(b"other".to_vec());
        assert_ne!(script_form_revision(&a), script_form_revision(&renamed));
        // A stamp below the member's own is content in the script form.
        let mut nested = a.clone();
        nested.children[0] = OwnedNode::list(
            vec![0x04],
            vec![OwnedNode::leaf(MOD_COUNT.to_vec(), vec![1, 1])],
        );
        let mut renested = nested.clone();
        renested.children[0].children[0].value = Some(vec![1, 2]);
        assert_ne!(
            script_form_revision(&nested),
            script_form_revision(&renested)
        );
    }

    #[test]
    fn the_general_form_leaves_out_every_stamp_and_list_flag() {
        let field = |name: &[u8], stamp: &[u8]| {
            OwnedNode::list(
                vec![1],
                vec![
                    OwnedNode::leaf(vec![0x10], name.to_vec()),
                    OwnedNode::leaf(MOD_TIME.to_vec(), stamp.to_vec()),
                ],
            )
        };
        let catalog = |name: &[u8], stamp: &[u8]| {
            OwnedNode::list(
                vec![3],
                vec![
                    OwnedNode::list(
                        vec![1],
                        vec![OwnedNode::leaf(MOD_COUNT.to_vec(), stamp.to_vec())],
                    ),
                    OwnedNode::list(vec![5], vec![field(name, stamp)]),
                ],
            )
        };
        let a = catalog(b"Name", &[1]);
        let mut restamped = catalog(b"Name", &[2]);
        restamped.children[1].push = false;
        assert_eq!(catalog_revision(&a), catalog_revision(&restamped));
        assert_ne!(
            catalog_revision(&a),
            catalog_revision(&catalog(b"Other", &[1]))
        );
        // The script form keeps the stamps below the member's own.
        assert_ne!(script_form_revision(&a), script_form_revision(&restamped));
    }

    #[test]
    fn framing_keeps_a_value_apart_from_a_key() {
        // One leaf `01` = `02`, against two leaves `01` and `02`; and a
        // value against an empty list of the same key.
        let one = OwnedNode::list(vec![], vec![OwnedNode::leaf(vec![1], vec![2])]);
        let two = OwnedNode::list(
            vec![],
            vec![
                OwnedNode::leaf(vec![1], vec![]),
                OwnedNode::leaf(vec![2], vec![]),
            ],
        );
        let list = OwnedNode::list(vec![], vec![OwnedNode::list(vec![1], vec![])]);
        let empty = OwnedNode::list(vec![], vec![OwnedNode::leaf(vec![1], vec![])]);
        let revisions = [&one, &two, &list, &empty].map(catalog_revision);
        for (i, a) in revisions.iter().enumerate() {
            for b in &revisions[i + 1..] {
                assert_ne!(a, b);
            }
        }
        let text = revision_text(&revisions[0]);
        assert_eq!(parse_revision(&text), Some(revisions[0]));
        assert_eq!(parse_revision("sha256:00"), None);
    }
}
