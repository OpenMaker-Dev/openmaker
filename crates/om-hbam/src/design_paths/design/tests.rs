//! The design revision's sweep, over built members of every shape the
//! rules distinguish and (ignored by default) over every design object of
//! the FileMaker templates: every piece of an object is changed, its node
//! removed and a node added below it, and an operation on a piece that is
//! not Design must leave the Design projection (what the design revision
//! hashes) byte for byte as it was. Then the accounting of pieces against
//! the tree, and the cases of #723.

use std::collections::{BTreeMap, BTreeSet};

use super::super::compare::{compare, CompareError, Difference, Which};
use super::super::revision::{is_stamp, parse_revision, split_value};
use super::super::rules::Group;
use super::super::tests::{descriptor, leaf, list, message, varint};
use super::super::{
    entry_keys, group_in, instances, objects, Catalog, Kind, Object, Path, CATALOGS, STEP_SECRETS,
};
use super::{classify, Class, Classed, Classified, Refusal};
use crate::serial;
use crate::tree::OwnedNode;

/// A planted secret: 24 bytes no file holds and no framing writes (a hash
/// of `n`), so that any run of eight of them found elsewhere was copied.
fn planted(n: u64) -> Vec<u8> {
    let mut seed = b"planted secret ".to_vec();
    seed.extend(n.to_be_bytes());
    om_crypto::sha256(&seed)[..24].to_vec()
}

/// Whether any run of eight bytes of `secret` is in `bytes`.
fn leaks(bytes: &[u8], secret: &[u8]) -> bool {
    let runs: Vec<u64> = secret
        .windows(8)
        .map(|w| u64::from_le_bytes(w.try_into().unwrap()))
        .collect();
    bytes
        .windows(8)
        .any(|w| runs.contains(&u64::from_le_bytes(w.try_into().unwrap())))
}

fn at(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// The classified node at `rel` in a part, and whether `rel` lies inside a
/// split value written whole (then the node is that value's).
fn classed_at<'c, 't>(part: &'c Classed<'t>, rel: &[Vec<u8>]) -> (&'c Classed<'t>, bool) {
    let mut node = part;
    for segment in rel {
        // Below a node classified without children, only a split value
        // written whole has pieces (the sweep never descends into what an
        // object leaves out).
        if node.children.is_empty() {
            return (node, true);
        }
        node = node
            .children
            .iter()
            .find(|c| c.segment == segment.as_slice())
            .unwrap_or_else(|| panic!("no classified node at {rel:02x?}"));
    }
    (node, false)
}

/// Whether a proper prefix of `rel` in `node` is a split value, whose
/// pieces have fixed lengths.
fn in_split_value(node: &OwnedNode, rel: &[Vec<u8>]) -> bool {
    (0..rel.len()).any(|k| node.at(&at(&rel[..k])).and_then(split_value).is_some())
}

/// Whether `rel` in `node` is the header `00` of a split value: its total
/// length, then a text's character count (file-format.md 3.6). A split
/// value is hashed as the value it joins to, so the header's other words
/// are framing that neither an edit token nor a design revision covers;
/// the sweep changes its total.
fn split_header(node: &OwnedNode, rel: &[Vec<u8>]) -> bool {
    let Some((last, parent)) = rel.split_last() else {
        return false;
    };
    last == &[0] && node.at(&at(parent)).and_then(split_value).is_some()
}

/// Every node below `node` (relative paths, `node` itself as the empty
/// path), but the top node's stamps and what `skip` says is not the
/// object's, in order.
fn nodes(node: &OwnedNode, skip: &dyn Fn(&[Vec<u8>]) -> bool) -> Vec<Path> {
    let mut out = Vec::new();
    let mut stack: Vec<(Path, &OwnedNode)> = vec![(Vec::new(), node)];
    while let Some((path, n)) = stack.pop() {
        out.push(path.clone());
        for child in &n.children {
            if path.is_empty() && is_stamp(&child.segment) {
                continue;
            }
            let mut p = path.clone();
            p.push(child.segment.clone());
            if !skip(&p) {
                stack.push((p, child));
            }
        }
    }
    out.sort();
    out
}

/// What the sweep covered.
#[derive(Debug, Default)]
struct Stats {
    objects: usize,
    with_revision: usize,
    /// Value changes, by the class of the value.
    values: BTreeMap<&'static str, usize>,
    /// Nodes removed whose subtree holds no Design piece.
    removed: usize,
    /// Nodes added below a node whose being there is not Design.
    added: usize,
    /// Planted secrets checked against the Design projection.
    planted: usize,
    /// Presence and value classes checked against `group_in`.
    oracle: usize,
    next: u64,
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Design => "Design",
        Class::Credential => "Credential",
        Class::Data => "Data",
        Class::Derived => "Derived",
        Class::Unidentified => "Unidentified",
    }
}

fn projections(classified: &Classified<'_>) -> [Vec<u8>; 5] {
    Class::ALL.map(|c| classified.projection(c))
}

fn digests(projections: &[Vec<u8>; 5]) -> [[u8; 32]; 5] {
    std::array::from_fn(|i| om_crypto::sha256(&projections[i]))
}

/// The tree holding only `object`'s parts, `replaced` (part index and
/// node) in place of one: what `group_in` reads for its paths.
fn mini_root(object: &Object<'_>, replaced: Option<(usize, &OwnedNode)>) -> OwnedNode {
    let mut root = OwnedNode::default();
    let mut parts: Vec<(&Path, &OwnedNode)> = vec![(&object.path, object.node)];
    parts.extend(object.entries.iter().map(|(p, n)| (p, *n)));
    for (i, (path, node)) in parts.into_iter().enumerate() {
        let node = match replaced {
            Some((j, other)) if j == i => other,
            _ => node,
        };
        root.put(&at(path), node.clone());
    }
    root
}

/// `object` with part `index` replaced by `node`.
fn with_part<'a>(object: &Object<'a>, index: usize, node: &'a OwnedNode) -> Object<'a> {
    let mut changed = object.clone();
    match index {
        0 => changed.node = node,
        i => changed.entries[i - 1].1 = node,
    }
    changed
}

/// Sweep `object` (module doc). `secrets` are secrets planted in it, which
/// the Design projection must not hold.
fn sweep(object: &Object<'_>, secrets: &[Vec<u8>], stats: &mut Stats) {
    let original = classify(object).expect("a member read as the decoders read it");
    let before = projections(&original);
    let before_digests = digests(&before);
    let revision = original.revision();
    assert_eq!(revision.is_some(), !original.holds(Class::Unidentified));
    if let Some(revision) = revision {
        assert_eq!(revision.0, om_crypto::sha256(&before[0]));
        stats.with_revision += 1;
    }
    assert_eq!(before[0][..8], [0xff; 8]);
    for secret in secrets {
        assert!(
            !leaks(&before[0], secret),
            "{:?} {:02x?}",
            object.kind,
            object.key
        );
        stats.planted += 1;
    }
    stats.objects += 1;
    let keys = if object.catalog {
        entry_keys(Catalog::of(object.kind))
    } else {
        Vec::new()
    };
    let mut parts: Vec<(Path, &OwnedNode)> = vec![(object.path.clone(), object.node)];
    parts.extend(object.entries.iter().cloned());
    for (index, (part_path, node)) in parts.iter().enumerate() {
        let classed = match index {
            0 => &original.top,
            i => &original.entries[i - 1].1,
        };
        // A catalog's index leaves out its members' entries.
        let theirs = |p: &[Vec<u8>]| {
            index == 0
                && p.len() == 2
                && keys.contains(&p[0].as_slice())
                && object.members.contains(&p[1])
        };
        for rel in nodes(node, &theirs) {
            let abs: Vec<Vec<u8>> = part_path.iter().chain(&rel).cloned().collect();
            let target = node.at(&at(&rel)).unwrap();
            let (here_node, in_whole) = classed_at(classed, &rel);
            let here = if in_whole {
                here_node.value.as_ref().map(|(c, _)| *c)
            } else {
                here_node.here
            };
            let value = target.value.as_ref().map(|_| {
                if in_whole {
                    here_node.value.as_ref().unwrap().0
                } else {
                    here_node.value.as_ref().expect("a classified value").0
                }
            });

            // The oracle: `group_in` for the node without its children,
            // for its value; and as an empty list, for its being there,
            // where that differs from its value's class (a node whose own
            // value is marked) or it holds no value and is no split value
            // (whose pieces are checked one by one).
            if let Some(class) = value {
                let mut leafed = (*node).clone();
                leafed.at_mut(&at(&rel)).unwrap().children.clear();
                let root = mini_root(object, Some((index, &leafed)));
                let group = group_in(&root, &at(&abs));
                assert!(
                    class.agrees_with(group),
                    "value {class:?} / {group:?} at {abs:02x?}"
                );
                stats.oracle += 1;
            }
            let presence = match (here, value) {
                (Some(h), Some(v)) if h != v => Some(h),
                (Some(h), None) if split_value(target).is_none() => Some(h),
                _ => None,
            };
            if let Some(class) = presence.filter(|_| !in_whole) {
                let mut emptied = (*node).clone();
                let n = emptied.at_mut(&at(&rel)).unwrap();
                n.children.clear();
                n.value = None;
                let root = mini_root(object, Some((index, &emptied)));
                let group = group_in(&root, &at(&abs));
                assert!(
                    class.agrees_with(group),
                    "being {class:?} / {group:?} at {abs:02x?}"
                );
                stats.oracle += 1;
            }

            let after = |changed: &OwnedNode| -> [Vec<u8>; 5] {
                projections(&classify(&with_part(object, index, changed)).unwrap())
            };

            // Change the value: a Design value's, or a piece of a split
            // value, keeping its length; any other's to a planted secret of
            // another length.
            if let Some(class) = value {
                stats.next += 1;
                let secret = planted(1_000_000 + stats.next);
                let mut changed = (*node).clone();
                let n = changed.at_mut(&at(&rel)).unwrap();
                let v = n.value.as_mut().unwrap();
                let planting = class != Class::Design && !in_split_value(node, &rel);
                if planting {
                    *v = secret.clone();
                } else if split_header(node, &rel) {
                    v[3] ^= 0x01;
                } else if v.is_empty() {
                    *v = vec![0x01];
                } else {
                    *v.last_mut().unwrap() ^= 0x01;
                }
                let after = after(&changed);
                if planting {
                    assert!(!leaks(&after[0], &secret), "{abs:02x?}");
                    stats.planted += 1;
                }
                if class == Class::Design {
                    assert!(after[0] != before[0], "Design value at {abs:02x?}");
                } else {
                    for c in Class::ALL {
                        assert_eq!(
                            after[c as usize] != before[c as usize],
                            c == class,
                            "{c:?} after a change of a {class:?} value at {abs:02x?}"
                        );
                    }
                    let expected = match class {
                        Class::Credential => Difference::Secrets {
                            data: false,
                            derived: false,
                        },
                        Class::Data => Difference::Outside {
                            data: true,
                            derived: false,
                        },
                        Class::Derived => Difference::Outside {
                            data: false,
                            derived: true,
                        },
                        _ => Difference::Unidentified { design: false },
                    };
                    assert_eq!(Difference::of(&before_digests, &digests(&after)), expected);
                }
                *stats.values.entry(class_name(class)).or_default() += 1;
            }

            if rel.is_empty() || in_whole {
                continue;
            }
            // Remove the node: when no Design piece is at or below it, the
            // Design projection does not move.
            if !here_node.holds(Class::Design) {
                let mut removed = (*node).clone();
                removed.remove(&at(&rel));
                let after = after(&removed);
                assert!(after[0] == before[0], "removed at {abs:02x?}");
                stats.removed += 1;
            }
            // Add a node below it: below a node whose being there is not
            // Design, it is not Design either, and the Design projection
            // does not move.
            if here.is_some_and(|c| c != Class::Design) {
                let mut added = (*node).clone();
                added
                    .at_mut(&at(&rel))
                    .unwrap()
                    .set_child(leaf(&[0x7e], &[1]));
                let classified = classify(&with_part(object, index, &added)).unwrap();
                let mut new_rel = rel.clone();
                new_rel.push(vec![0x7e]);
                let part = match index {
                    0 => &classified.top,
                    i => &classified.entries[i - 1].1,
                };
                let (new_node, _) = classed_at(part, &new_rel);
                assert_ne!(new_node.here, Some(Class::Design), "added at {abs:02x?}");
                assert!(
                    classified.projection(Class::Design) == before[0],
                    "added at {abs:02x?}"
                );
                stats.added += 1;
            }
        }
    }
}

/// A calculation slot `[00 n]` with its parts as given.
fn slot(
    n: u16,
    own: Option<&[u8]>,
    text: Option<&[u8]>,
    calc: Option<&[u8]>,
    number: bool,
) -> OwnedNode {
    let mut children = Vec::new();
    if let Some(text) = text {
        children.push(leaf(&[0x01], text));
    }
    if number {
        children.push(leaf(&[0x03], &[1]));
    }
    if let Some(calc) = calc {
        children.push(list(
            &[0x05],
            vec![
                leaf(&[0x04], &[0, 0, 0, 1]),
                leaf(&[0x05], calc),
                leaf(&[0x06], calc),
            ],
        ));
    }
    let mut node = list(&n.to_be_bytes(), children);
    node.value = own.map(<[u8]>::to_vec);
    node
}

/// The step data of `step`: each credential slot holding a planted secret
/// (pushed to `secrets`) in its text and calculation, with a number, and
/// a slot that holds none.
fn step_data(key: &[u8], step: u16, secrets: &mut Vec<Vec<u8>>) -> OwnedNode {
    let spec = STEP_SECRETS.iter().find(|s| s.step == step).unwrap();
    let mut slots: Vec<OwnedNode> = spec
        .slots
        .iter()
        .map(|(n, _)| {
            let secret = planted(u64::from(step) << 16 | u64::from(*n));
            secrets.push(secret.clone());
            slot(*n, None, Some(&secret), Some(&secret), true)
        })
        .collect();
    let plain = (0..40)
        .find(|n| !spec.slots.iter().any(|(s, _)| s == n))
        .unwrap();
    slots.push(slot(
        plain,
        Some(&[0, 0, 0, 1]),
        None,
        Some(b"\"design\""),
        true,
    ));
    slots.sort_by(|a, b| a.segment.cmp(&b.segment));
    list(key, slots)
}

/// A script member whose steps are `steps` (data keys 1, 2, ...), its step
/// data `data`, a name, flags, UUID and a stamp.
fn script_member(steps: &[u16], data: Vec<OwnedNode>) -> OwnedNode {
    let list_bytes: Vec<u8> = steps
        .iter()
        .enumerate()
        .flat_map(|(i, s)| descriptor(i as u8 + 1, *s as i16))
        .collect();
    let mut data_list = data;
    data_list.push(leaf(&[0x00], &[1, steps.len() as u8]));
    data_list.sort_by(|a, b| a.segment.cmp(&b.segment));
    list(
        &[],
        vec![
            leaf(&[0x04], &list_bytes),
            list(&[0x05], data_list),
            leaf(&[0x10], &om_scsu::encode_scsu("Script")),
            leaf(&[0xd8], &[7; 16]),
            leaf(&[0xf0], &[0; 8]),
            leaf(&[0xfc], &[1, 1]),
        ],
    )
}

/// A `LayoutObjectStore` holding one button, key `key`, whose
/// `ScriptStep` holds `ids` in its field 1, followed by `extra`.
fn store(key: u64, ids: &[u64], extra: &[u8]) -> Vec<u8> {
    let mut step = Vec::new();
    for &id in ids {
        varint(1 << 3, &mut step);
        varint(id, &mut step);
    }
    step.extend_from_slice(extra);
    let mut button = Vec::new();
    message(3, &step, &mut button);
    let mut object = Vec::new();
    varint(2 << 3, &mut object);
    varint(key, &mut object);
    message(16, &button, &mut object);
    let mut store = Vec::new();
    message(2, &object, &mut store);
    store
}

/// A layout member whose button 5 runs `step`, with its step data.
fn button_member(ids: &[u64], data: OwnedNode) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x02], &[0x10, 0x82]),
            list(
                &[0x05],
                vec![list(
                    &serial::encode(5),
                    vec![leaf(&[0x01], &[1, 2, 3]), data],
                )],
            ),
            leaf(&[0x07], &store(5, ids, &[])),
            leaf(&[0x10], &om_scsu::encode_scsu("Layout")),
            leaf(&[0xfc, 0x03], &[9; 8]),
        ],
    )
}

/// The profile type words of script-steps.md 11.5, and one no spec names.
const PROFILE_TYPES: [&[u8; 4]; 14] = [
    b"TABS", b"COMS", b"MRGE", b"DBF ", b"HTML", b"XLXE", b"FMPR", b"XLS ", b"XLSX", b"BTCH",
    b"XML ", b"XMLE", b"ODBC", b"ZZZZ",
];

/// Every shape the rules distinguish, as script, layout, field and catalog
/// members of one tree (module doc); the secrets planted in it.
fn built() -> (OwnedNode, Vec<Vec<u8>>) {
    let mut secrets = Vec::new();
    let mut root = OwnedNode::default();
    let mut id = 0u64;
    let mut next = || {
        id += 1;
        serial::encode(id)
    };
    // Every step that stores a credential, as a script and as a button.
    for spec in STEP_SECRETS {
        let key = next();
        let data = step_data(&[0x01], spec.step, &mut secrets);
        root.put(
            &[&[0x11], &[5], &key],
            script_member(&[spec.step], vec![data]),
        );
        let data = step_data(&[0x11], spec.step, &mut secrets);
        root.put(
            &[&[0x04], &[5], &key],
            button_member(&[u64::from(spec.step)], data),
        );
        root.put(
            &[&[0x04], &[1], &[0x07], &key],
            list(&[], vec![leaf(&[0x10], b"entry")]),
        );
    }
    // A credential slot of every shape: own value none, of 4 bytes or of
    // another length, by text, calculation and number there or not,
    // nothing at all included; in Send Mail's six credential slots.
    let owns: [Option<&[u8]>; 3] = [None, Some(&[0, 0, 0, 2]), Some(b"7 bytes")];
    let mut shapes = Vec::new();
    for own in owns {
        for text in [false, true] {
            for calc in [false, true] {
                for number in [false, true] {
                    shapes.push((own, text, calc, number));
                }
            }
        }
    }
    let send_mail = STEP_SECRETS.iter().find(|s| s.step == 63).unwrap();
    for chunk in shapes.chunks(send_mail.slots.len()) {
        let slots: Vec<OwnedNode> = chunk
            .iter()
            .zip(send_mail.slots)
            .map(|(&(own, text, calc, number), (n, _))| {
                let secret = planted(0x5000 + u64::from(*n) + 100 * secrets.len() as u64);
                secrets.push(secret.clone());
                slot(
                    *n,
                    own,
                    text.then_some(secret.as_slice()),
                    calc.then_some(secret.as_slice()),
                    number,
                )
            })
            .collect();
        let key = next();
        root.put(
            &[&[0x11], &[5], &key],
            script_member(&[63], vec![list(&[0x01], slots)]),
        );
    }
    // Profiles of every type word (Import Records, 35), Print Setup's page
    // format and an import's profile extension `87`, a key and a slot part
    // no spec names, a data node's own value, a slot whose only child is
    // not identified, the step data's own value, a step not identified.
    let mut data: Vec<OwnedNode> = PROFILE_TYPES
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let mut profile = word.to_vec();
            profile.extend([0, 0]);
            let secret = planted(0x7000 + i as u64);
            profile.extend(&secret);
            secrets.push(secret);
            list(&[i as u8 + 1], vec![leaf(&[0x86], &profile)])
        })
        .collect();
    let mut steps: Vec<u16> = vec![35; PROFILE_TYPES.len()];
    let extra = |n: u8, children: Vec<OwnedNode>| list(&[n], children);
    steps.extend([42, 35, 1, 1, 1, 248]);
    data.push(extra(15, vec![leaf(&[0x87], b"page format")]));
    data.push(extra(16, vec![leaf(&[0x87], b"extension")]));
    data.push(extra(17, vec![leaf(&[0x7e], b"no spec names this key")]));
    let mut owned = extra(18, vec![slot(2, None, None, Some(b"x"), true)]);
    owned.value = Some(b"a data node's own value".to_vec());
    data.push(owned);
    data.push(extra(
        19,
        vec![list(&[0, 3], vec![leaf(&[0x7e], b"an unnamed part")])],
    ));
    data.push(extra(
        20,
        vec![slot(1, Some(&[0, 0, 0, 1]), Some(b"x"), None, true)],
    ));
    let key = next();
    let mut member = script_member(&steps, data);
    member.child_mut(&[0x05]).unwrap().value = Some(b"the list's own value".to_vec());
    root.put(&[&[0x11], &[5], &key], member);
    // An unexplained key holding a stamp-shaped key, in a script (script
    // form) and a layout (general form).
    let key = next();
    let mut member = script_member(&[1], vec![]);
    member.set_child(list(&[0x13], vec![leaf(&[0xfc], &[1, 1])]));
    root.put(&[&[0x11], &[5], &key], member);
    let data = list(&[0x11], vec![slot(2, None, None, Some(b"x"), true)]);
    let mut layout = button_member(&[1], data);
    layout.set_child(list(&[0x16], vec![leaf(&[0xfc, 0x02], b"Admin")]));
    root.put(&[&[0x04], &[5], &key], layout);
    // The scripts' index: an entry per script, a leftover one for a
    // script that is gone, and a value on the entry list itself; the
    // catalogs' order, digest, count and last id.
    let scripts: Vec<Vec<u8>> = root
        .at(&[&[0x11], &[5]])
        .unwrap()
        .children
        .iter()
        .map(|c| c.segment.clone())
        .collect();
    for script in &scripts {
        root.put(
            &[&[0x11], &[1], &[0x07], script],
            list(&[], vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, 1])]),
        );
    }
    root.put(
        &[&[0x11], &[1], &[0x07], &serial::encode(99)],
        list(&[], vec![leaf(&[0xf1], &[1; 8])]),
    );
    root.at_mut(&[&[0x11], &[1], &[0x07]]).unwrap().value = Some(b"list".to_vec());
    for catalog in [0x11u8, 0x04] {
        root.put(&[&[catalog], &[1], &[0x00]], leaf(&[], &[0x0e]));
        root.put(&[&[catalog], &[1], &[0x03]], leaf(&[], &[1, 2, 3]));
        root.put(&[&[catalog], &[1], &[0x04]], leaf(&[], &[0x0d]));
        root.put(&[&[catalog], &[1], &[0xfc]], leaf(&[], &[4]));
    }
    root.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[8; 16]));
    // A field with a next serial value held as a list (Data alone below a
    // Design member), and its catalog's name list.
    root.put(
        &[&[0x80, 0x01], &[3], &[5], &[1]],
        list(
            &[],
            vec![
                leaf(&[0x02], &[1, 0]),
                list(
                    &[0x0c],
                    vec![leaf(&[0x00], &[0, 0, 0, 9]), leaf(&[0x01], b"7")],
                ),
                leaf(&[0x10], &om_scsu::encode_scsu("Count")),
            ],
        ),
    );
    root.put(
        &[&[0x80, 0x01], &[3], &[1], &[0x01], b"count"],
        leaf(&[], &[1]),
    );
    (root, secrets)
}

/// The pieces (nodes that are a terminal or hold a value) under the
/// tree's design catalogs: each index without its own stamps, each
/// member that is not emptied without its own stamps, and anything else in
/// a catalog; and the emptied members apart.
fn tree_pieces(root: &OwnedNode) -> (usize, usize) {
    fn count(node: &OwnedNode, top: bool) -> usize {
        let own = usize::from(node.value.is_some() || node.children.is_empty());
        own + node
            .children
            .iter()
            .filter(|c| !(top && is_stamp(&c.segment)))
            .map(|c| count(c, false))
            .sum::<usize>()
    }
    let (mut pieces, mut emptied) = (0, 0);
    for catalog in CATALOGS.iter().filter(|c| c.group == Group::Design) {
        for (_, _, node) in instances(root, catalog) {
            pieces += usize::from(node.value.is_some());
            for child in &node.children {
                match child.segment.as_slice() {
                    [0x01] => pieces += count(child, true),
                    [0x05] => {
                        pieces += usize::from(child.value.is_some());
                        for member in &child.children {
                            if member.children.is_empty() {
                                emptied += 1;
                            } else {
                                pieces += count(member, true);
                            }
                        }
                    }
                    _ => pieces += count(child, false),
                }
            }
        }
    }
    (pieces, emptied)
}

/// Every piece under the design catalogs is in exactly one object's
/// classification: the sums agree.
fn account(root: &OwnedNode) -> usize {
    let found = objects(root);
    let classified: usize = found
        .iter()
        .filter_map(|o| classify(o).ok())
        .map(|c| c.pieces())
        .sum();
    let (pieces, _) = tree_pieces(root);
    assert_eq!(classified, pieces);
    pieces
}

#[test]
fn a_design_revision_moves_with_design_pieces_only_for_every_shape() {
    let (root, secrets) = built();
    let mut stats = Stats::default();
    let found = objects(&root);
    for object in &found {
        sweep(object, &secrets, &mut stats);
    }
    println!("{stats:?}");
    // Every object was swept: 13 steps as a script and as a button, four
    // scripts of credential slot shapes, one of other step data, one
    // script and one layout with a stamp-shaped key, a field, and the
    // scripts', layouts' and field catalogs.
    assert_eq!(stats.objects, 13 * 2 + 4 + 1 + 2 + 1 + 3);
    // Every credential slot of every step of STEP_SECRETS, 22 in 13 steps,
    // as a script's and a button's, holds Credential pieces in what the
    // sweep swept: a rule removed takes its coverage with it, and the
    // literals fail.
    let mut covered = BTreeSet::new();
    for (i, spec) in STEP_SECRETS.iter().enumerate() {
        let key = serial::encode(i as u64 + 1);
        for (kind, data) in [
            (Kind::Script, vec![vec![0x05], vec![0x01]]),
            (
                Kind::Layout,
                vec![vec![0x05], serial::encode(5), vec![0x11]],
            ),
        ] {
            let object = found
                .iter()
                .find(|o| o.kind == kind && !o.catalog && o.key == key)
                .unwrap();
            let classified = classify(object).unwrap();
            for (n, _) in spec.slots {
                let mut rel = data.clone();
                rel.push(n.to_be_bytes().to_vec());
                let (slot, _) = classed_at(&classified.top, &rel);
                for part in [[0x01u8], [0x05]] {
                    let piece = slot.children.iter().find(|c| c.segment == part).unwrap();
                    assert_eq!(piece.here, Some(Class::Credential), "{} {n}", spec.name);
                }
                covered.insert((kind, spec.step, *n));
            }
        }
    }
    let steps: BTreeSet<u16> = covered.iter().map(|(_, s, _)| *s).collect();
    assert_eq!((steps.len(), covered.len()), (13, 2 * 22));
    for class in ["Design", "Credential", "Data", "Derived", "Unidentified"] {
        assert!(stats.values.get(class).is_some_and(|&n| n > 0), "{class}");
    }
    assert!(stats.removed > 0 && stats.added > 0 && stats.oracle > 0);
    // The pieces of all objects are the pieces under the catalogs.
    assert!(account(&root) > 0);

    // A design revision is never an edit token: another prefix, another
    // text that an edit token's parser refuses.
    let script = found
        .iter()
        .find(|o| o.kind == Kind::Script && !o.catalog)
        .unwrap();
    let revision = script.design_revision().unwrap();
    assert_ne!(revision.0, script.edit_token());
    assert!(revision.text().starts_with("design-sha256:"));
    assert_eq!(revision.text().len(), "design-sha256:".len() + 64);
    assert_eq!(parse_revision(&revision.text()), None);
}

/// A tree holding `script` as script 1, with its index entry.
fn one_script(script: OwnedNode, entry_flag: u8) -> OwnedNode {
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[1]], script);
    root.put(
        &[&[0x11], &[1], &[0x07], &[1]],
        list(&[], vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, entry_flag])]),
    );
    root
}

/// A tree holding `layout` as layout 1.
fn one_layout(layout: OwnedNode) -> OwnedNode {
    let mut root = OwnedNode::default();
    root.put(&[&[0x04], &[5], &[1]], layout);
    root
}

/// Send Mail (63) with slot 11, the SMTP password, as given.
fn send_mail(slot11: OwnedNode) -> OwnedNode {
    script_member(
        &[63],
        vec![list(
            &[0x01],
            vec![slot(3, None, None, Some(b"\"Hi\""), false), slot11],
        )],
    )
}

/// Import Records (35) with the profile `profile`.
fn import(profile: &[u8]) -> OwnedNode {
    script_member(&[35], vec![list(&[0x01], vec![leaf(&[0x86], profile)])])
}

/// An ODBC profile (script-steps.md 11.5) holding `password` and `query`.
fn odbc(password: &[u8], query: &[u8]) -> Vec<u8> {
    let mut v = b"ODBC".to_vec();
    for part in [&b"dsn"[..], b"user", password, query] {
        v.extend((part.len() as u32).to_be_bytes());
        v.extend_from_slice(part);
    }
    v
}

/// The one member object of `root`, and its matched comparison with
/// `other`'s.
fn compared(root: &OwnedNode, other: &OwnedNode) -> (Option<super::DesignRevision>, Difference) {
    let object = objects(root).into_iter().find(|o| !o.catalog).unwrap();
    let comparison = compare(root, other, Some(root)).unwrap();
    let matched = comparison
        .matched
        .iter()
        .find(|m| !m.source.catalog)
        .unwrap();
    (object.design_revision(), matched.difference)
}

fn calc_slot(n: u16, text: &[u8]) -> OwnedNode {
    slot(n, None, None, Some(text), false)
}

/// The cases the reviews of #718 found (#723): each object holding a
/// secret where the rules cannot say exactly what it is has no design
/// revision and no byte of the secret in its Design projection, and a
/// change of the secret alone is "only stored secrets" only where the
/// secret is an identified credential; otherwise it is unidentified
/// content.
#[test]
fn the_cases_of_723_have_no_design_revision_and_leak_nothing() {
    let pw = planted(1);
    let pw2 = planted(2);
    let unidentified = Difference::Unidentified { design: false };
    let secrets_only = Difference::Secrets {
        data: false,
        derived: false,
    };
    let split_slot = |pw: &[u8]| {
        list(
            &[0, 11],
            vec![leaf(&[0], &(pw.len() as u32).to_be_bytes()), leaf(&[1], pw)],
        )
    };
    let with_list_value = |pw: &[u8], list_value: u8| {
        let mut slot = calc_slot(11, pw);
        slot.value = Some(vec![0, 0, 0, list_value]);
        slot
    };
    let button = |ids: &[u64], extra: &[u8], pw: &[u8]| {
        let data = list(&[0x11], vec![calc_slot(0, b"\"me\""), calc_slot(1, pw)]);
        let mut member = button_member(&[], data);
        member.set_child(leaf(&[0x07], &store(5, ids, extra)));
        one_layout(member)
    };
    let script_with = |steps: Vec<u8>, pw: &[u8]| {
        let mut member = send_mail(calc_slot(11, pw));
        member.set_child(leaf(&[0x04], &steps));
        one_script(member, 1)
    };
    let cases: Vec<(&str, OwnedNode, OwnedNode, Difference)> = vec![
        // Unexpected slot shapes: the password as the slot's own value,
        // four bytes of it or more, and as a split value at the slot; a
        // password beside a list value is an identified credential, and
        // the list value is not identified.
        (
            "own value",
            one_script(send_mail(leaf(&[0, 11], &pw)), 1),
            one_script(send_mail(leaf(&[0, 11], &pw2)), 1),
            unidentified,
        ),
        (
            "four-byte own value",
            one_script(send_mail(leaf(&[0, 11], &pw[..4])), 1),
            one_script(send_mail(leaf(&[0, 11], &pw2[..4])), 1),
            unidentified,
        ),
        (
            "split slot",
            one_script(send_mail(split_slot(&pw)), 1),
            one_script(send_mail(split_slot(&pw2)), 1),
            secrets_only,
        ),
        (
            "password beside a list value",
            one_script(send_mail(with_list_value(&pw, 2)), 1),
            one_script(send_mail(with_list_value(&pw2, 2)), 1),
            secrets_only,
        ),
        (
            "list value beside a password",
            one_script(send_mail(with_list_value(&pw, 2)), 1),
            one_script(send_mail(with_list_value(&pw, 3)), 1),
            unidentified,
        ),
        // Scanner and decoder disagreeing: Perform Script, then Re-Login
        // in one repeated field; an id before a malformed suffix; two
        // descriptors naming one data key with different steps; step id 0
        // in a script.
        (
            "repeated step field",
            button(&[1, 138], &[], &pw),
            button(&[1, 138], &[], &pw2),
            unidentified,
        ),
        (
            "malformed suffix",
            button(&[138], &[0x0a], &pw),
            button(&[138], &[0x0a], &pw2),
            unidentified,
        ),
        (
            "disputed data key",
            script_with([descriptor(1, 63), descriptor(1, 1)].concat(), &pw),
            script_with([descriptor(1, 63), descriptor(1, 1)].concat(), &pw2),
            unidentified,
        ),
        (
            "step id 0",
            script_with(descriptor(1, 0), &pw),
            script_with(descriptor(1, 0), &pw2),
            unidentified,
        ),
        // A profile mixes a query with the credentials, and its type word
        // decides nothing: the password and the query of an ODBC profile,
        // and a password in a profile of a type no spec names, are all
        // unidentified, so a deploy can never take a change in it for a
        // stored secret's (#723, "Keeping the target's credentials").
        (
            "ODBC password",
            one_script(import(&odbc(&pw, b"SELECT 1")), 1),
            one_script(import(&odbc(&pw2, b"SELECT 1")), 1),
            unidentified,
        ),
        (
            "ODBC query",
            one_script(import(&odbc(&pw, b"SELECT 1")), 1),
            one_script(import(&odbc(&pw, b"SELECT 2")), 1),
            unidentified,
        ),
        (
            "unknown profile type",
            one_script(import(&[b"ZZZZ".as_slice(), &pw].concat()), 1),
            one_script(import(&[b"ZZZZ".as_slice(), &pw2].concat()), 1),
            unidentified,
        ),
    ];
    for (case, a, b, expected) in &cases {
        let (revision, difference) = compared(a, b);
        assert_eq!(revision, None, "{case}");
        assert_eq!(difference, *expected, "{case}");
        for root in [a, b] {
            let object = objects(root).into_iter().find(|o| !o.catalog).unwrap();
            let design = classify(&object).unwrap().projection(Class::Design);
            for secret in [&pw, &pw2] {
                assert!(!leaks(&design, secret), "{case}");
            }
        }
    }

    // A member the decoders could read differently, a step list under
    // `[00 04]` as well: not classified, no design revision, no name, and
    // a comparison refused.
    let mut aliased = send_mail(calc_slot(11, &pw));
    aliased.set_child(leaf(&[0, 4], &descriptor(1, 1)));
    let aliased = one_script(aliased, 1);
    let object = objects(&aliased).into_iter().find(|o| !o.catalog).unwrap();
    assert_eq!(classify(&object).err(), Some(Refusal::Misread));
    assert_eq!(object.design_revision(), None);
    assert_eq!(object.design_name(), None);
    assert_eq!(
        compare(&aliased, &aliased, None).unwrap_err(),
        CompareError::Misread {
            which: Which::Source,
            kind: Kind::Script,
            id: Some(1)
        }
    );
    let plain = one_script(send_mail(calc_slot(11, &pw)), 1);
    let named = compare(&plain, &plain, None).unwrap();
    assert_eq!(named.matched[1].source.name.as_deref(), Some("Script"));

    // Removing the last credential child of a slot moves the Design
    // projection no more than changing it does: the slot's being there is
    // Design, with or without children.
    let a = one_script(send_mail(slot(11, None, Some(&pw), None, false)), 1);
    let b = one_script(send_mail(slot(11, None, None, None, false)), 1);
    let (revision, difference) = compared(&a, &b);
    assert!(revision.is_some());
    assert_eq!(compared(&b, &a).0, revision);
    assert_eq!(difference, secrets_only);

    // A change only in a derived cache: the script's full-access flag in
    // its index entry `[11, 1, 07, S, f1]` is part of the script, so its
    // design revision moves and the change is Design; the catalog's digest
    // of the cache `[11, 1, 08]` is Derived, and moves no script.
    let a = one_script(send_mail(calc_slot(11, &pw)), 1);
    let flagged = one_script(send_mail(calc_slot(11, &pw)), 3);
    assert_eq!(compared(&a, &flagged).1, Difference::Design);
    assert_ne!(compared(&flagged, &a).0, compared(&a, &flagged).0);
    let mut digested = a.clone();
    digested.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[1; 16]));
    let mut digested_again = a.clone();
    digested_again.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[2; 16]));
    let comparison = compare(&digested, &digested_again, None).unwrap();
    let by_catalog: Vec<(bool, Difference)> = comparison
        .matched
        .iter()
        .map(|m| (m.source.catalog, m.difference))
        .collect();
    assert_eq!(
        by_catalog,
        [
            (
                true,
                Difference::Outside {
                    data: false,
                    derived: true
                }
            ),
            (false, Difference::Same),
        ]
    );
    // An entry left behind by a script that is gone is the catalog's.
    let mut leftover = a.clone();
    leftover.put(
        &[&[0x11], &[1], &[0x07], &[9]],
        list(&[], vec![leaf(&[0xf1], &[1; 8])]),
    );
    let mut leftover_again = leftover.clone();
    leftover_again.put(&[&[0x11], &[1], &[0x07], &[9], &[0xf1]], leaf(&[], &[2; 8]));
    let comparison = compare(&leftover, &leftover_again, None).unwrap();
    assert_eq!(comparison.matched[0].difference, Difference::Design);
    assert_eq!(comparison.matched[1].difference, Difference::Same);
}

/// The template folders `crates/om-hbam/tests/design_paths.rs` reads.
const DIRS: [&str; 3] = [
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates",
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "../../re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample",
];

fn template_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "fmp12")
                || path.to_string_lossy().ends_with("FMServer_Sample")
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for dir in DIRS.iter().map(std::path::Path::new).filter(|d| d.is_dir()) {
        walk(dir, &mut out);
    }
    out.sort();
    out
}

#[test]
#[ignore = "requires the git-ignored extracted templates in re/extracted"]
fn a_design_revision_moves_with_design_pieces_only_in_every_template_object() {
    let files = template_files();
    assert!(!files.is_empty(), "no template folder under re/extracted");
    let mut stats = Stats::default();
    let mut pieces = 0;
    for path in &files {
        let file = crate::HbamFile::open(path).unwrap();
        let root = file.key_tree().unwrap().root.to_owned_node();
        for object in objects(&root) {
            sweep(&object, &[], &mut stats);
        }
        pieces += account(&root);
    }
    println!("{} files, {pieces} pieces: {stats:?}", files.len());
    assert!(stats.objects > 0 && pieces > 0);
    assert!(stats.values.get("Design").is_some_and(|&n| n > 0));
}
