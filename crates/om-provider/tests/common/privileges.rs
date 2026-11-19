//! Synthetic security catalog shared by provider and hosted desktop tests.
#![allow(dead_code)]

use om_hbam::OwnedNode;
use om_scripts::Script;

use crate::fixture;

pub const CUSTOM_KEY: &[u8] = &[0x80, 0x84];
pub const AUTH_SENTINEL: &[u8] = b"synthetic-authentication-material-never-retain";

/// The Tasks file with Admin in `set`: [Full Access] (`01`), [Data Entry
/// Only] (`02`) and [Read-Only Access] (`03`) as the starter solutions store
/// them (session-identity.md 2.7), or the custom set "Office" (CUSTOM_KEY:
/// create, view and edit records, not delete), each a set the host
/// enforces. Admin's checksum holds [`AUTH_SENTINEL`] (its password can
/// hold nothing else: FileMaker reads no password from a value with more
/// than its check bytes after the hash).
pub fn tree(scripts: &[Script], set: &[u8]) -> OwnedNode {
    let mut root = fixture::tree(scripts, true);
    let mut reference = vec![set.len() as u8];
    reference.extend(set);
    root.put(
        &[&[23], &[1], &[5], &[2], &[0x0b]],
        OwnedNode::leaf([], reference),
    );
    root.put(
        &[&[23], &[1], &[5], &[2], &[4]],
        OwnedNode::leaf([], AUTH_SENTINEL.to_vec()),
    );
    let password = om_crypto::account::StoredPassword::encode(fixture::PASSWORD, [2; 10]);
    root.put(
        &[&[23], &[1], &[5], &[2], &[6]],
        OwnedNode::leaf([], password),
    );
    // Another account's set must never be substituted for the login key.
    root.put(
        &[&[23], &[1], &[5], &[3], &[0x0b]],
        OwnedNode::leaf([], vec![1, 1]),
    );
    for (key, name, records, layouts, others, options) in [
        (&[1][..], "[Full Access]", 0x55, 0xdf, 0xd7, [0x9f, 0xff]),
        (
            &[2][..],
            "[Data Entry Only]",
            0x55,
            0xde,
            0xd6,
            [0x03, 0x04],
        ),
        (
            &[3][..],
            "[Read-Only Access]",
            0x10,
            0xde,
            0xd6,
            [0x03, 0x04],
        ),
        (CUSTOM_KEY, "Office", 0x54, 0xde, 0xd6, [0x03, 0x04]),
    ] {
        root.put(
            &[&[23], &[2], &[5], key, &[2]],
            OwnedNode::leaf([], vec![records, 3]),
        );
        root.put(&[&[23], &[2], &[5], key, &[0x10]], text(name));
        root.put(
            &[&[23], &[2], &[5], key, &[0x0a]],
            OwnedNode::leaf([], options.to_vec()),
        );
        for (catalog, quad) in [(0x04u8, layouts), (0x11, others), (0x21, others)] {
            root.put(
                &[&[23], &[2], &[5], key, &[9], &[catalog]],
                OwnedNode::leaf([], vec![quad]),
            );
        }
        root.put(
            &[&[23], &[2], &[5], key, &[0x0c]],
            OwnedNode::leaf([], vec![0, 0]),
        );
    }
    // Deliberately nonalphabetic member order; filter without reordering.
    for (id, name, sets) in [
        (1, "zeta", vec![CUSTOM_KEY]),
        (2, "alpha", vec![CUSTOM_KEY]),
        (3, "fmapp", vec![&[1][..], &[2], &[3]]),
        (4, "full-only", vec![&[1][..]]),
    ] {
        root.put(&[&[23], &[3], &[5], &[id], &[0x10]], text(name));
        for set in sets {
            root.put(
                &[&[23], &[3], &[5], &[id], &[5], set],
                OwnedNode::leaf([], vec![]),
            );
        }
    }
    root
}

fn text(value: &str) -> OwnedNode {
    om_scripts::text_node(&[], value)
}
