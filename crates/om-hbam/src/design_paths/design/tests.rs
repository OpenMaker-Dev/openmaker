//! The partition of a file's design catalogs into objects' pieces, and the
//! sweep of every piece, over built trees of every shape the rules
//! distinguish and (ignored by default) over the FileMaker templates; and
//! the cases of #723.
//!
//! Both are checked against an oracle of their own: a piece's class read
//! from its path alone, by the rule table and the two places step data
//! lives, without the walk under test.

use std::collections::{BTreeMap, BTreeSet};

use super::super::compare::{compare, CompareError, Difference, IdentityProblem, Which};
use super::super::revision::{is_stamp, parse_revision, split_value};
use super::super::rules::{rule_for, rules, Group};
use super::super::tests::{descriptor, leaf, list, message, varint};
use super::super::{instances, objects, Kind, Object, Path, CATALOGS};
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

/// A piece: a node's being there, or its own value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Piece {
    Here,
    Value,
}

/// The oracle: a piece's class from its path, by the rule table and the
/// places step data lives (`[11, 5, S, 05]`, `[4, 5, L, 05, O, 11]`).
fn oracle(path: &[Vec<u8>], piece: Piece) -> Class {
    let p = at(path);
    let step_data = matches!(&p[..], [[0x11], [0x05], _, [0x05], ..])
        || matches!(&p[..], [[0x04], [0x05], _, [0x05], _, [0x11], ..]);
    if step_data {
        return Class::StepData;
    }
    if let Some(rule) = rule_for(&p) {
        return Class::of(rule.group);
    }
    let above = rules().iter().any(|r| {
        r.pattern.len() > p.len() && r.pattern.iter().zip(&p).all(|(s, seg)| s.matches(seg))
    });
    match (above, piece) {
        (true, Piece::Here) => Class::Design,
        _ => Class::Unidentified,
    }
}

/// Every piece under the tree's design catalogs, by path, but the stamps
/// at the two places they are left out (a member object's own node, a
/// catalog's index) with what is below them.
fn tree_pieces(root: &OwnedNode) -> BTreeSet<(Path, Piece)> {
    fn walk(
        node: &OwnedNode,
        path: &mut Path,
        out: &mut BTreeSet<(Path, Piece)>,
        skip: &dyn Fn(&Path, &OwnedNode) -> bool,
    ) {
        out.insert((path.clone(), Piece::Here));
        if node.value.is_some() {
            out.insert((path.clone(), Piece::Value));
        }
        for child in &node.children {
            path.push(child.segment.clone());
            if !skip(path, child) {
                walk(child, path, out, skip);
            }
            path.pop();
        }
    }
    let mut out = BTreeSet::new();
    for catalog in CATALOGS.iter().filter(|c| c.group == Group::Design) {
        for (_, path, node) in instances(root, catalog) {
            let base = path.len();
            let skip = |p: &Path, n: &OwnedNode| {
                let rel = &p[base..];
                is_stamp(&n.segment)
                    && match rel {
                        // The index's own stamps.
                        [index, _] => index == &[0x01],
                        // A member object's own stamps.
                        [list, member, _] => {
                            list == &[0x05]
                                && node
                                    .at(&[&[0x05], member])
                                    .is_some_and(|m| !m.children.is_empty())
                        }
                        _ => false,
                    }
            };
            walk(node, &mut path.clone(), &mut out, &skip);
        }
    }
    out
}

/// Each piece an object's classification holds, by path, with its class;
/// a split value written whole stands for the pieces it was joined from.
fn classified_pieces(
    top_path: &[Vec<u8>],
    top: &OwnedNode,
    classified: &Classified<'_>,
) -> Vec<(Path, Piece, Class)> {
    fn walk(
        node: &OwnedNode,
        classed: &Classed<'_>,
        path: &mut Path,
        out: &mut Vec<(Path, Piece, Class)>,
    ) {
        out.push((path.clone(), Piece::Here, classed.here));
        // A split value written whole: a value the node does not hold.
        let folded = node.value.is_none() && classed.value.is_some();
        if folded {
            // Every node joined into the value, in the value's class.
            let class = classed.value.as_ref().unwrap().0;
            let mut stack: Vec<(Path, &OwnedNode)> = node
                .children
                .iter()
                .map(|c| {
                    let mut p = path.clone();
                    p.push(c.segment.clone());
                    (p, c)
                })
                .collect();
            while let Some((p, n)) = stack.pop() {
                out.push((p.clone(), Piece::Here, class));
                if n.value.is_some() {
                    out.push((p.clone(), Piece::Value, class));
                }
                for c in &n.children {
                    let mut q = p.clone();
                    q.push(c.segment.clone());
                    stack.push((q, c));
                }
            }
            return;
        }
        if let Some((class, _)) = &classed.value {
            out.push((path.clone(), Piece::Value, *class));
        }
        for child in &classed.children {
            let original = node.child(child.segment).unwrap();
            path.push(child.segment.to_vec());
            walk(original, child, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(top, &classified.top, &mut top_path.to_vec(), &mut out);
    out
}

/// The path and node an object's classification starts from.
fn part<'a>(object: &Object<'a>) -> (Path, &'a OwnedNode) {
    match &object.whole {
        Some((path, node)) => (path.clone(), *node),
        None => (object.path.clone(), object.node),
    }
}

/// Every piece under the design catalogs is in exactly one object, in the
/// class the oracle gives it: checked by path, so a miss and a duplicate
/// cannot cancel. The number of pieces.
fn partition(root: &OwnedNode) -> usize {
    let mut seen: BTreeMap<(Path, Piece), usize> = BTreeMap::new();
    for (i, object) in objects(root).iter().enumerate() {
        let (path, node) = part(object);
        let classified = classify(object).expect("a member read as the decoders read it");
        for (p, piece, class) in classified_pieces(&path, node, &classified) {
            assert_eq!(class, oracle(&p, piece), "{piece:?} at {p:02x?}");
            if let Some(other) = seen.insert((p.clone(), piece), i) {
                panic!("{piece:?} at {p:02x?} is in objects {other} and {i}");
            }
        }
    }
    let classified: BTreeSet<(Path, Piece)> = seen.into_keys().collect();
    let expected = tree_pieces(root);
    let missing: Vec<_> = expected.difference(&classified).collect();
    let extra: Vec<_> = classified.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "missing {missing:02x?}, extra {extra:02x?}"
    );
    expected.len()
}

/// What the sweep covered.
#[derive(Debug, Default)]
struct Stats {
    objects: usize,
    with_revision: usize,
    /// Operations, by kind and by the class of the piece.
    ops: BTreeMap<(&'static str, &'static str), usize>,
    planted: usize,
    next: u64,
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Design => "Design",
        Class::Data => "Data",
        Class::Derived => "Derived",
        Class::StepData => "StepData",
        Class::Unidentified => "Unidentified",
    }
}

fn projections(classified: &Classified<'_>) -> [Vec<u8>; 5] {
    Class::ALL.map(|c| classified.projection(c))
}

/// `object` with its part (member, or catalog node) replaced by `node`.
fn with_part<'a>(object: &Object<'a>, node: &'a OwnedNode) -> Object<'a> {
    let mut changed = object.clone();
    match &mut changed.whole {
        Some((_, whole)) => *whole = node,
        None => changed.node = node,
    }
    changed
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

/// Whether a proper prefix of `rel` in `node` is a split value, whose
/// pieces have fixed lengths.
fn in_split_value(node: &OwnedNode, rel: &[Vec<u8>]) -> bool {
    (0..rel.len()).any(|k| node.at(&at(&rel[..k])).and_then(split_value).is_some())
}

/// Assert that after an operation exactly the projections of `expected`
/// changed.
fn changed_exactly(
    before: &[Vec<u8>; 5],
    after: &[Vec<u8>; 5],
    expected: &BTreeSet<Class>,
    what: &str,
) {
    for class in Class::ALL {
        assert_eq!(
            before[class as usize] != after[class as usize],
            expected.contains(&class),
            "{class:?} after {what}"
        );
    }
}

/// Sweep `object`: for each of its pieces, change its value and remove it
/// keeping its node, remove its node, and add a node below it; after each,
/// the projections of exactly the classes of the pieces touched change.
/// `secrets` are secrets planted in it, which the Design projection must
/// not hold.
fn sweep(object: &Object<'_>, secrets: &[Vec<u8>], stats: &mut Stats) {
    let original = classify(object).expect("a member read as the decoders read it");
    let before = projections(&original);
    let revision = original.revision();
    assert_eq!(
        revision.is_some(),
        !original.holds(Class::StepData) && !original.holds(Class::Unidentified)
    );
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
    let (top_path, top) = part(object);
    let pieces = classified_pieces(&top_path, top, &original);
    let owned: BTreeSet<Path> = pieces.iter().map(|(p, _, _)| p.clone()).collect();
    let classes_below = |rel: &[Vec<u8>]| -> BTreeSet<Class> {
        pieces
            .iter()
            .filter(|(p, _, _)| p[top_path.len()..].starts_with(rel))
            .map(|(_, _, c)| *c)
            .collect()
    };
    let after = |changed: &OwnedNode| projections(&classify(&with_part(object, changed)).unwrap());
    let member_list = object.whole.is_some();
    for (path, piece, class) in &pieces {
        let rel = path[top_path.len()..].to_vec();
        let target = top.at(&at(&rel)).unwrap();
        let what = |op: &str| format!("{op} of {piece:?} at {path:02x?}");
        match piece {
            Piece::Value => {
                // Change it: keeping its length in a split value, else to a
                // planted secret of another length.
                stats.next += 1;
                let secret = planted(1_000_000 + stats.next);
                let mut changed = top.clone();
                let v = changed.at_mut(&at(&rel)).unwrap().value.as_mut().unwrap();
                let planting = !in_split_value(top, &rel);
                if planting {
                    *v = secret.clone();
                } else if split_header(top, &rel) {
                    v[3] ^= 0x01;
                } else {
                    *v.last_mut().unwrap() ^= 0x01;
                }
                let after_change = after(&changed);
                changed_exactly(&before, &after_change, &[*class].into(), &what("a change"));
                if planting && *class != Class::Design {
                    assert!(!leaks(&after_change[0], &secret), "{}", what("a change"));
                    stats.planted += 1;
                }
                // Remove it, keeping the node.
                let mut removed = top.clone();
                removed.at_mut(&at(&rel)).unwrap().value = None;
                changed_exactly(
                    &before,
                    &after(&removed),
                    &[*class].into(),
                    &what("a removal"),
                );
                *stats.ops.entry(("value", class_name(*class))).or_default() += 2;
            }
            Piece::Here => {
                if rel.is_empty() {
                    continue;
                }
                // Remove the node, unless that empties a member, which then
                // is an object no more.
                let empties = !member_list && rel.len() == 1 && top.children.len() == 1;
                if !empties {
                    let mut removed = top.clone();
                    removed.remove(&at(&rel));
                    let expected = classes_below(&rel);
                    changed_exactly(
                        &before,
                        &after(&removed),
                        &expected,
                        &what("removing the node"),
                    );
                    *stats.ops.entry(("remove", class_name(*class))).or_default() += 1;
                }
                // Add a node below it, a leaf with a value, under a key it
                // does not have; not below a catalog's member with no
                // children, which would make it an object.
                let emptied_member =
                    member_list && rel.len() == 2 && rel[0] == [0x05] && target.children.is_empty();
                if emptied_member {
                    continue;
                }
                let key = (0x70u8..0x7f)
                    .rev()
                    .find(|k| target.child(&[*k]).is_none())
                    .unwrap();
                let mut added = top.clone();
                added
                    .at_mut(&at(&rel))
                    .unwrap()
                    .set_child(leaf(&[key], &[1]));
                let mut new_path = path.clone();
                new_path.push(vec![key]);
                assert!(!owned.contains(&new_path));
                let expected: BTreeSet<Class> = [
                    oracle(&new_path, Piece::Here),
                    oracle(&new_path, Piece::Value),
                ]
                .into();
                // A split value the node unfolds stays its class.
                changed_exactly(&before, &after(&added), &expected, &what("adding below"));
                *stats.ops.entry(("add", class_name(*class))).or_default() += 1;
            }
        }
    }
}

/// A calculation slot `[00 n]` holding `text` in its text and calculation.
fn slot(n: u16, text: &[u8]) -> OwnedNode {
    list(
        &n.to_be_bytes(),
        vec![
            leaf(&[0x01], text),
            leaf(&[0x03], &[1]),
            list(
                &[0x05],
                vec![
                    leaf(&[0x04], &[0, 0, 0, 1]),
                    leaf(&[0x05], text),
                    leaf(&[0x06], text),
                ],
            ),
        ],
    )
}

/// A script member running `step` (data key 1), whose step data holds
/// `data`, with a name, flags, UUID and a stamp.
fn script_member(step: i16, data: Vec<OwnedNode>) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x04], &descriptor(1, step)),
            list(&[0x05], vec![leaf(&[0x00], &[1, 1]), list(&[0x01], data)]),
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

/// A layout member whose button 5 runs step `id`, its step data holding
/// `data`.
fn button_member(id: u64, data: Vec<OwnedNode>) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x02], &[0x10, 0x82]),
            list(
                &[0x05],
                vec![list(
                    &serial::encode(5),
                    vec![leaf(&[0x01], &[1, 2, 3]), list(&[0x11], data)],
                )],
            ),
            leaf(&[0x07], &store(5, &[id], &[])),
            leaf(&[0x10], &om_scsu::encode_scsu("Layout")),
            leaf(&[0xfc, 0x03], &[9; 8]),
        ],
    )
}

/// One tree with every shape the partition and the sweep tell apart: step
/// data of a step storing a password (Re-Login, 138) and of a profile, in a
/// script and a button; a script with an unexplained key holding a
/// stamp-shaped key; a field whose next serial value is a list of Data;
/// and catalogs with a value of their own, a branch beside the index and
/// the members, a value on the member list, a member with a value and no
/// children, an index list shaped like a split value, a stamp-shaped key
/// inside an index entry, an entry of a member that is gone. The secrets
/// planted in it.
fn built() -> (OwnedNode, Vec<Vec<u8>>) {
    let (pw, odbc) = (planted(1), planted(2));
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x11], &[5], &[1]],
        script_member(138, vec![slot(0, b"\"me\""), slot(1, &pw)]),
    );
    root.put(
        &[&[0x11], &[5], &[2]],
        script_member(
            35,
            vec![leaf(&[0x86], &[b"ODBC".as_slice(), &odbc].concat())],
        ),
    );
    let mut odd = script_member(1, vec![slot(2, b"x")]);
    odd.set_child(list(&[0x13], vec![leaf(&[0xfc], &[1, 1])]));
    root.put(&[&[0x11], &[5], &[3]], odd);
    root.put(
        &[&[0x04], &[5], &[1]],
        button_member(138, vec![slot(0, b"\"me\""), slot(1, &pw)]),
    );
    // The scripts' catalog: its own value, a branch beside the index and
    // the members, a value on the member list, an emptied member with a
    // value, entries (one with a stamp-shaped key, one of a script that is
    // gone), an order, a digest, a count, a last id and the index's stamp.
    root.at_mut(&[&[0x11]]).unwrap().value = Some(b"own".to_vec());
    root.put(&[&[0x11], &[0x7e]], leaf(&[], b"beside"));
    root.at_mut(&[&[0x11], &[5]]).unwrap().value = Some(b"list".to_vec());
    root.put(&[&[0x11], &[5], &[9]], leaf(&[], b"emptied"));
    for script in [1u8, 2, 3, 99] {
        root.put(
            &[&[0x11], &[1], &[0x07], &[script]],
            list(&[], vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, 1])]),
        );
    }
    root.put(&[&[0x11], &[1], &[0x07], &[1], &[0xfc]], leaf(&[], &[2, 2]));
    root.put(&[&[0x11], &[1], &[0x00]], leaf(&[], &[0x03]));
    root.put(&[&[0x11], &[1], &[0x03]], leaf(&[], &[1, 2, 3]));
    root.put(&[&[0x11], &[1], &[0x04]], leaf(&[], &[0x03]));
    root.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[8; 16]));
    root.put(&[&[0x11], &[1], &[0xfc]], leaf(&[], &[4]));
    // The layouts' index: an entry list shaped like a split value, its
    // header and one piece, with layout 1 there too.
    root.put(
        &[&[0x04], &[1], &[0x07]],
        list(
            &[],
            vec![leaf(&[0x00], &[0, 0, 0, 3]), leaf(&[0x01], b"abc")],
        ),
    );
    // A field with a next serial value held as a list of Data, and its
    // catalog's name list.
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
    (root, vec![pw, odbc])
}

#[test]
fn every_piece_is_in_one_object_and_moves_only_its_own_class() {
    let (root, secrets) = built();
    // The partition, by path.
    assert!(partition(&root) > 0);
    let mut stats = Stats::default();
    let found = objects(&root);
    for object in &found {
        sweep(object, &secrets, &mut stats);
    }
    println!("{stats:?}");
    // Three scripts, a layout, a field, and the catalogs of scripts,
    // layouts and fields; a design revision for those without step data
    // or an unidentified piece: the field and its catalog, and the
    // layouts' catalog.
    assert_eq!(stats.objects, 8);
    assert_eq!(stats.with_revision, 3);
    for (op, class) in [
        ("value", "Design"),
        ("value", "Data"),
        ("value", "Derived"),
        ("value", "StepData"),
        ("value", "Unidentified"),
        ("remove", "Design"),
        ("remove", "StepData"),
        ("add", "Design"),
        ("add", "StepData"),
        ("add", "Unidentified"),
    ] {
        assert!(
            stats.ops.get(&(op, class)).is_some_and(|&n| n > 0),
            "{op} {class}"
        );
    }

    // A design revision is never an edit token: another prefix, another
    // text that an edit token's parser refuses.
    let field = found
        .iter()
        .find(|o| o.kind == Kind::Field && !o.catalog)
        .unwrap();
    let revision = field.design_revision().unwrap();
    assert_ne!(revision.0, field.edit_token());
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

/// The one member object of `root`: its design revision, and how it
/// differs from `other`'s.
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

/// The cases the reviews of #718 found (#723), and those of #768's
/// reviews: a stored password is in step data, which is one piece no
/// projection but its own looks into. Whatever shape it has, a change of it
/// is "step data differs", the object has no design revision, and no byte
/// of it is in the Design projection.
#[test]
fn the_cases_of_723_change_step_data_only_and_leak_nothing() {
    let pw = planted(1);
    let pw2 = planted(2);
    let step_data = Difference {
        step_data: true,
        ..Difference::default()
    };
    let send_mail = |slot11: OwnedNode| script_member(63, vec![slot(3, b"\"Hi\""), slot11]);
    let split_slot = |pw: &[u8]| {
        list(
            &[0, 11],
            vec![leaf(&[0], &(pw.len() as u32).to_be_bytes()), leaf(&[1], pw)],
        )
    };
    let with_value = |pw: &[u8]| {
        let mut s = slot(11, pw);
        s.value = Some(vec![0, 0, 0, 2]);
        s
    };
    let only_text = |pw: Option<&[u8]>| {
        list(
            &[0, 11],
            pw.map(|pw| vec![leaf(&[0x01], pw)]).unwrap_or_default(),
        )
    };
    let button = |ids: &[u64], extra: &[u8], pw: &[u8]| {
        let mut member = button_member(1, vec![slot(0, b"\"me\""), slot(1, pw)]);
        member.set_child(leaf(&[0x07], &store(5, ids, extra)));
        let mut root = OwnedNode::default();
        root.put(&[&[0x04], &[5], &[1]], member);
        root
    };
    let with_steps = |steps: Vec<u8>, pw: &[u8]| {
        let mut member = send_mail(slot(11, pw));
        member.set_child(leaf(&[0x04], &steps));
        one_script(member, 1)
    };
    let profile = |word: &[u8], pw: &[u8]| {
        one_script(
            script_member(35, vec![leaf(&[0x86], &[word, pw].concat())]),
            1,
        )
    };
    let cases: Vec<(&str, OwnedNode, OwnedNode)> = vec![
        (
            "a password",
            one_script(send_mail(slot(11, &pw)), 1),
            one_script(send_mail(slot(11, &pw2)), 1),
        ),
        (
            "as the slot's own value",
            one_script(send_mail(leaf(&[0, 11], &pw)), 1),
            one_script(send_mail(leaf(&[0, 11], &pw2)), 1),
        ),
        (
            "four bytes of it",
            one_script(send_mail(leaf(&[0, 11], &pw[..4])), 1),
            one_script(send_mail(leaf(&[0, 11], &pw2[..4])), 1),
        ),
        (
            "split at the slot",
            one_script(send_mail(split_slot(&pw)), 1),
            one_script(send_mail(split_slot(&pw2)), 1),
        ),
        (
            "beside a list value",
            one_script(send_mail(with_value(&pw)), 1),
            one_script(send_mail(with_value(&pw2)), 1),
        ),
        (
            "the last credential child removed",
            one_script(send_mail(only_text(Some(&pw))), 1),
            one_script(send_mail(only_text(None)), 1),
        ),
        (
            "a repeated step field",
            button(&[1, 138], &[], &pw),
            button(&[1, 138], &[], &pw2),
        ),
        (
            "a malformed suffix",
            button(&[138], &[0x0a], &pw),
            button(&[138], &[0x0a], &pw2),
        ),
        (
            "a disputed data key",
            with_steps([descriptor(1, 63), descriptor(1, 1)].concat(), &pw),
            with_steps([descriptor(1, 63), descriptor(1, 1)].concat(), &pw2),
        ),
        (
            "step id 0",
            with_steps(descriptor(1, 0), &pw),
            with_steps(descriptor(1, 0), &pw2),
        ),
        (
            "a data key 00",
            with_steps(descriptor(0, 63), &pw),
            with_steps(descriptor(0, 63), &pw2),
        ),
        (
            "an ODBC profile",
            profile(b"ODBC", &pw),
            profile(b"ODBC", &pw2),
        ),
        (
            "a profile of an unknown type",
            profile(b"ZZZZ", &pw),
            profile(b"ZZZZ", &pw2),
        ),
        (
            "a step with no parameters",
            one_script(script_member(4, vec![leaf(&[0, 9], &pw)]), 1),
            one_script(script_member(4, vec![leaf(&[0, 9], &pw2)]), 1),
        ),
    ];
    for (case, a, b) in &cases {
        let (revision, difference) = compared(a, b);
        assert_eq!(revision, None, "{case}");
        assert_eq!(difference, step_data, "{case}");
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
    // a comparison refused before anything is read from it.
    let mut aliased = send_mail(slot(11, &pw));
    aliased.set_child(leaf(&[0, 4], &descriptor(1, 1)));
    let aliased = one_script(aliased, 1);
    let object = objects(&aliased).into_iter().find(|o| !o.catalog).unwrap();
    assert_eq!(classify(&object).err(), Some(Refusal::Misread));
    assert_eq!(object.design_revision(), None);
    assert_eq!(object.design_name(), None);
    assert_eq!(
        compare(&aliased, &aliased, None).unwrap_err(),
        CompareError::Identity {
            which: Which::Source,
            kind: Kind::Script,
            table: None,
            id: Some(1),
            problem: IdentityProblem::Misread
        }
    );

    // The refusal comes from the identity check, before anything is
    // classified: with a font of a malformed key after it, the misread
    // script, first in catalog order, is the one refused.
    let mut both = aliased.clone();
    both.put(
        &[&[0x19], &[5], &[0x01]],
        list(&[], vec![leaf(&[0x10], &om_scsu::encode_scsu("Helvetica"))]),
    );
    assert_eq!(
        compare(&both, &both, None).unwrap_err(),
        CompareError::Identity {
            which: Which::Source,
            kind: Kind::Script,
            table: None,
            id: Some(1),
            problem: IdentityProblem::Misread
        }
    );

    // The index entries are the catalog's: a change only in a script's
    // cached flag `[11, 1, 07, S, f1]` changes the catalog, not the script;
    // so does the catalog's digest `[11, 1, 08]`, as Derived.
    let a = one_script(send_mail(slot(11, &pw)), 1);
    let flagged = one_script(send_mail(slot(11, &pw)), 3);
    let comparison = compare(&a, &flagged, None).unwrap();
    let by_catalog: Vec<(bool, Difference)> = comparison
        .matched
        .iter()
        .map(|m| (m.source.catalog, m.difference))
        .collect();
    let design = Difference {
        design: true,
        ..Difference::default()
    };
    assert_eq!(by_catalog, [(true, design), (false, Difference::default())]);
    let mut digested = a.clone();
    digested.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[1; 16]));
    let comparison = compare(&a, &digested, None).unwrap();
    let derived = Difference {
        derived: true,
        ..Difference::default()
    };
    assert_eq!(comparison.matched[0].difference, derived);
    assert!(comparison.matched[1].difference.is_same());
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
fn every_template_piece_is_in_one_object_and_moves_only_its_own_class() {
    let files = template_files();
    assert_eq!(files.len(), 66, "the 66 FileMaker Pro 26 starter files");
    let mut stats = Stats::default();
    let mut pieces = 0;
    for path in &files {
        let file = crate::HbamFile::open(path).unwrap();
        let root = file.key_tree().unwrap().root.to_owned_node();
        pieces += partition(&root);
        for object in objects(&root) {
            sweep(&object, &[], &mut stats);
        }
    }
    println!("{} files, {pieces} pieces: {stats:?}", files.len());
    assert!(stats.objects > 0 && pieces > 0);
}
