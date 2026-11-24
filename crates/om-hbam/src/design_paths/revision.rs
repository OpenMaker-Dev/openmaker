//! Edit tokens: the SHA-256 of a member's content in a canonical form,
//! what an authoring client and the host exchange as `expected_revision`
//! and `revision` to guard an edit ([`Revision`], `sha256:` text). A token
//! covers the whole member, stored passwords included, leaving out only
//! modification stamps (schema-path.md 8): any other change changes it, so
//! no edit is lost. Only a client that has the member already sees it.
//! [`script_form_revision`] for a script, [`catalog_revision`] for
//! everything else.
//!
//! The canonical form walks a subtree in key order, its top node first.
//! Each node is written as its segment (a `u64` big-endian length, then
//! the bytes), whether it holds a value (`1` and the value framed the same
//! way, or `0`), and its number of children (`u64` big-endian), then each
//! child the same way. Every part is framed by a length or a count, so two
//! different subtrees never give the same bytes. The top node's segment is
//! written too: a member's token includes its key, its id.
//!
//! A value is written as its bytes, however the tree holds it. A node is a
//! **split value** by the format's rule (file-format.md 3.6): no value of
//! its own, a first child `00` holding a header of 4, 8 or 12 bytes whose
//! first `u32` is the total length, then children keyed `1` to `n` as
//! serial keys in order, each a leaf, every one but the last 1,000 bytes
//! and the last 1 to 1,000, adding up to the total, and nothing else. Such
//! a node is written as the value its pieces join to, as a leaf. So a value
//! kept inline in memory and the same value split by the writer hash
//! alike. A list that meets the rule is a split value as far as the format
//! can tell, and is hashed as one.
//!
//! There are two forms: the general form leaves stamps out wherever they
//! occur and does not write whether a node is a list; the script form
//! leaves out only the top node's stamp and writes each node's list flag
//! (`1` a list, `0` a leaf) after its segment. Scripts use the script form,
//! every other object the general one.
//!
//! [`legacy_script_form_revision`] and [`legacy_catalog_revision`] are the
//! tokens as clients computed them before split values were hashed whole:
//! a host accepts them from an older client and answers in them, for
//! comparison only.

use crate::tree::OwnedNode;

/// An edit token.
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

/// A script's edit token: the script form over its whole member.
pub fn script_form_revision(node: &OwnedNode) -> Revision {
    digest(node, Stamps::Own, true)
}

/// The edit token of everything but a script: the general form over a
/// whole subtree, every path in it (a layout or value list member, a
/// table's field catalog `[T, 3]` for `fields_revision`, the layout
/// catalog's index `[4, 1]` for `layouts_revision`).
pub fn catalog_revision(node: &OwnedNode) -> Revision {
    digest(node, Stamps::Everywhere, true)
}

/// [`script_form_revision`] as clients computed it before split values
/// were hashed whole, for an older client only.
pub fn legacy_script_form_revision(node: &OwnedNode) -> Revision {
    digest(node, Stamps::Own, false)
}

/// [`catalog_revision`] as clients computed it before split values were
/// hashed whole, for an older client only.
pub fn legacy_catalog_revision(node: &OwnedNode) -> Revision {
    digest(node, Stamps::Everywhere, false)
}

fn digest(node: &OwnedNode, stamps: Stamps, whole_values: bool) -> Revision {
    let mut bytes = Vec::new();
    canonical(node, stamps, whole_values, &mut bytes);
    om_crypto::sha256(&bytes)
}

/// An edit token as text: `sha256:` and 64 lowercase hex digits.
pub fn revision_text(revision: &Revision) -> String {
    format!("sha256:{}", super::rules::hex(revision))
}

/// An edit token from its text, `None` when it is not one.
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

/// The largest piece of a split value (file-format.md 3.6).
const PIECE: usize = 1000;

/// The value a split value joins to, by the format's rule (module doc);
/// `None` for any other node.
pub fn split_value(node: &OwnedNode) -> Option<Vec<u8>> {
    if node.value.is_some() {
        return None;
    }
    let (header, pieces) = node.children.split_first()?;
    let header_value = header.value.as_deref()?;
    if header.segment != [0]
        || !header.children.is_empty()
        || ![4, 8, 12].contains(&header_value.len())
    {
        return None;
    }
    let total = u32::from_be_bytes(header_value[..4].try_into().ok()?) as usize;
    let mut out = Vec::new();
    for (n, piece) in pieces.iter().enumerate() {
        let value = piece.value.as_deref()?;
        let last = n + 1 == pieces.len();
        let fits = if last {
            (1..=PIECE).contains(&value.len())
        } else {
            value.len() == PIECE
        };
        if crate::serial::decode(&piece.segment) != Some(n as u64 + 1)
            || !piece.children.is_empty()
            || !fits
            || out.len() + value.len() > total
        {
            return None;
        }
        out.extend_from_slice(value);
    }
    (!pieces.is_empty() && out.len() == total).then_some(out)
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

fn framed(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(b);
}

fn canonical(node: &OwnedNode, stamps: Stamps, whole_values: bool, out: &mut Vec<u8>) {
    framed(out, &node.segment);
    let list_flag = stamps != Stamps::Everywhere;
    // A split value is written as the value it joins to, as a leaf.
    if let Some(value) = whole_values.then(|| split_value(node)).flatten() {
        if list_flag {
            out.push(0);
        }
        out.push(1);
        framed(out, &value);
        out.extend_from_slice(&0u64.to_be_bytes());
        return;
    }
    if list_flag {
        out.push(u8::from(node.push));
    }
    match &node.value {
        Some(value) => {
            out.push(1);
            framed(out, value);
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
        canonical(child, below, whole_values, out);
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
