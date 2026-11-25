//! The design revision's sweep: every content path of an object is
//! changed in turn, and the projection of its class must change while the
//! Design projection (what the design revision hashes) changes only for a
//! Design path. Run over built members for every step that stores a
//! credential, as a script and as a button, and (ignored by default) over
//! every design object of the FileMaker templates; and the cases of #723.

use std::collections::BTreeMap;

use super::super::compare::{compare, Difference};
use super::super::revision::{is_stamp, parse_revision, split_value};
use super::super::tests::{descriptor, leaf, list, message, varint};
use super::super::{entry_keys, group_in, objects, Catalog, Kind, Object, Path, STEP_SECRETS};
use super::{classify, Class, Classed, Classified, Form};
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

/// Every path below `node` (relative to it, `node` itself as the empty
/// path) of a node with content: a value, or an empty list.
fn content_paths(node: &OwnedNode) -> Vec<Path> {
    let mut out = Vec::new();
    let mut stack: Vec<(Path, &OwnedNode)> = vec![(Vec::new(), node)];
    while let Some((path, n)) = stack.pop() {
        if n.value.is_some() || n.children.is_empty() {
            out.push(path.clone());
        }
        for child in &n.children {
            let mut p = path.clone();
            p.push(child.segment.clone());
            stack.push((p, child));
        }
    }
    out.sort();
    out
}

/// The class of the content at `rel` in a classified part, and whether it
/// is part of a split value written whole.
fn class_at(part: &Classed<'_>, rel: &[Vec<u8>]) -> (Class, bool) {
    let mut node = part;
    for (i, segment) in rel.iter().enumerate() {
        if node.children.is_empty() {
            if let Some((class, Some(_))) = &node.own {
                assert!(i > 0, "a split value below {rel:02x?}");
                return (*class, true);
            }
        }
        node = node
            .children
            .iter()
            .find(|c| c.segment == segment.as_slice())
            .unwrap_or_else(|| panic!("no classified node at {rel:02x?}"));
    }
    (node.own.as_ref().expect("content").0, false)
}

/// Whether a proper prefix of `rel` in `node` is a split value, whose
/// pieces have fixed lengths.
fn in_split_value(node: &OwnedNode, rel: &[Vec<u8>]) -> bool {
    (0..rel.len()).any(|k| {
        let segments: Vec<&[u8]> = rel[..k].iter().map(Vec::as_slice).collect();
        node.at(&segments).and_then(split_value).is_some()
    })
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
    let parent: Vec<&[u8]> = parent.iter().map(Vec::as_slice).collect();
    last == &[0] && node.at(&parent).and_then(split_value).is_some()
}

/// What the sweep covered.
#[derive(Debug, Default)]
struct Stats {
    objects: usize,
    with_revision: usize,
    /// Content paths changed, by class; stamps apart.
    paths: BTreeMap<&'static str, usize>,
    /// Credential, Data and Derived nodes removed.
    removed: usize,
    /// Planted secrets checked against the Design projection.
    planted: usize,
    /// Secrets planted by changes so far.
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

/// Change every content path of `object` in turn (module doc). `root` is
/// the tree the object is in, for [`group_in`]; `secrets` are secrets
/// planted in it, which the Design projection must not hold.
fn sweep(root: &OwnedNode, object: &Object<'_>, secrets: &[Vec<u8>], stats: &mut Stats) {
    let original = classify(object).expect("within the depth a file has");
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
    let skip = if object.catalog {
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
        for rel in content_paths(node) {
            if index == 0 && rel.first().is_some_and(|k| skip.contains(&k.as_slice())) {
                // A member's entry in a catalog's index: its member's.
                continue;
            }
            let stamp = match original.form {
                Form::General => rel.iter().any(|s| is_stamp(s)),
                Form::Script => rel.first().is_some_and(|s| is_stamp(s)),
            };
            let abs: Vec<&[u8]> = part_path.iter().chain(&rel).map(Vec::as_slice).collect();
            let class = if stamp {
                None
            } else {
                let (class, _) = class_at(classed, &rel);
                let group = group_in(root, &abs);
                assert!(
                    class.agrees_with(group),
                    "{class:?} against {group:?} at {abs:02x?}"
                );
                Some(class)
            };
            // Change the value: a Design path's or a stamp's, or a piece of
            // a split value, keeping its length; any other's to a planted
            // secret of another length.
            stats.next += 1;
            let secret = planted(1_000_000 + stats.next);
            let mut changed = (*node).clone();
            let segments: Vec<&[u8]> = rel.iter().map(Vec::as_slice).collect();
            let at = changed.at_mut(&segments).unwrap();
            let keep_length =
                matches!(class, None | Some(Class::Design)) || in_split_value(node, &rel);
            match (&mut at.value, keep_length) {
                (Some(value), true) if split_header(node, &rel) => value[3] ^= 0x01,
                (Some(value), true) if !value.is_empty() => {
                    *value.last_mut().unwrap() ^= 0x01;
                }
                (value, true) => *value = Some(vec![0x01]),
                (value, false) => *value = Some(secret.clone()),
            }
            let planted_here = !keep_length;
            let mut mutated = object.clone();
            match index {
                0 => mutated.node = &changed,
                i => mutated.entries[i - 1].1 = &changed,
            }
            let after = projections(&classify(&mutated).unwrap());
            if planted_here {
                assert!(!leaks(&after[0], &secret), "{abs:02x?}");
                stats.planted += 1;
            }
            let key = match class {
                None => {
                    assert!(after == before, "a stamp at {abs:02x?}");
                    "stamp"
                }
                Some(Class::Design) => {
                    assert!(after[0] != before[0], "Design at {abs:02x?}");
                    "Design"
                }
                Some(class) => {
                    for c in Class::ALL {
                        assert_eq!(
                            after[c as usize] != before[c as usize],
                            c == class,
                            "{c:?} after a change of {class:?} at {abs:02x?}"
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
                    // Its node removed: no count or framing of another
                    // class reaches the Design projection either. (An
                    // unidentified node's removal can let the rules
                    // identify the rest, which moves Design content.)
                    if class != Class::Unidentified && !rel.is_empty() {
                        let mut removed = (*node).clone();
                        removed.remove(&segments);
                        let mut without = object.clone();
                        match index {
                            0 => without.node = &removed,
                            i => without.entries[i - 1].1 = &removed,
                        }
                        let design = classify(&without).unwrap().projection(Class::Design);
                        assert!(design == before[0], "{class:?} removed at {abs:02x?}");
                        stats.removed += 1;
                    }
                    class_name(class)
                }
            };
            *stats.paths.entry(key).or_default() += 1;
        }
    }
}

/// A calculation slot `[00 n]`: its calculation's streams, its text and
/// its number.
fn calc_slot(n: u16, text: &[u8], number: u8) -> OwnedNode {
    list(
        &n.to_be_bytes(),
        vec![
            leaf(&[0x01], text),
            leaf(&[0x03], &[number]),
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

/// The step data of `step`: each credential slot holding a planted secret
/// (pushed to `secrets`), and a slot that holds none.
fn step_data(key: &[u8], step: u16, secrets: &mut Vec<Vec<u8>>) -> OwnedNode {
    let spec = STEP_SECRETS.iter().find(|s| s.step == step).unwrap();
    let mut slots: Vec<OwnedNode> = spec
        .slots
        .iter()
        .map(|(n, _)| {
            let secret = planted(u64::from(step) << 16 | u64::from(*n));
            secrets.push(secret.clone());
            calc_slot(*n, &secret, 1)
        })
        .collect();
    let plain = (0..40)
        .find(|n| !spec.slots.iter().any(|(s, _)| s == n))
        .unwrap();
    slots.push(calc_slot(plain, b"\"design\"", 0));
    slots.sort_by(|a, b| a.segment.cmp(&b.segment));
    list(key, slots)
}

/// A script member running `step` (data key 1), with a name, flags, UUID
/// and a stamp.
fn script_member(step: u16, secrets: &mut Vec<Vec<u8>>) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x04], &descriptor(1, step as i16)),
            list(&[0x05], vec![step_data(&[0x01], step, secrets)]),
            leaf(&[0x10], format!("Step {step}").as_bytes()),
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
fn button_member(ids: &[u64], step: u16, secrets: &mut Vec<Vec<u8>>) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x02], &[0x10, 0x82]),
            list(
                &[0x05],
                vec![list(
                    &serial::encode(5),
                    vec![leaf(&[0x01], &[1, 2, 3]), step_data(&[0x11], step, secrets)],
                )],
            ),
            leaf(&[0x07], &store(5, ids, &[])),
            leaf(&[0x10], format!("Button {step}").as_bytes()),
            leaf(&[0xfc, 0x03], &[9; 8]),
        ],
    )
}

/// A tree with a script and a layout per step in [`STEP_SECRETS`], each
/// with its index entry, and the two catalogs' indexes.
fn built(secrets: &mut Vec<Vec<u8>>) -> OwnedNode {
    let mut root = OwnedNode::default();
    for (i, spec) in STEP_SECRETS.iter().enumerate() {
        let id = serial::encode(i as u64 + 1);
        root.put(&[&[0x11], &[5], &id], script_member(spec.step, secrets));
        root.put(
            &[&[0x11], &[1], &[0x07], &id],
            list(&[], vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, 1])]),
        );
        root.put(
            &[&[0x04], &[5], &id],
            button_member(&[u64::from(spec.step)], spec.step, secrets),
        );
        root.put(
            &[&[0x04], &[1], &[0x07], &id],
            list(&[], vec![leaf(&[0x10], b"entry")]),
        );
    }
    for catalog in [0x11u8, 0x04] {
        root.put(&[&[catalog], &[1], &[0x00]], leaf(&[], &[0x0e]));
        root.put(&[&[catalog], &[1], &[0x03]], leaf(&[], &[1, 2, 3]));
        root.put(&[&[catalog], &[1], &[0x04]], leaf(&[], &[0x0d]));
        root.put(&[&[catalog], &[1], &[0xfc]], leaf(&[], &[4]));
    }
    root.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[8; 16]));
    root
}

#[test]
fn a_design_revision_changes_with_design_only_for_every_step_that_stores_a_credential() {
    let mut secrets = Vec::new();
    let root = built(&mut secrets);
    let mut stats = Stats::default();
    let found = objects(&root);
    for object in &found {
        sweep(&root, object, &secrets, &mut stats);
    }
    println!("{stats:?}");
    // Every step, as a script and a button, and the two catalogs: each has
    // a design revision, and the sweep reached every class but the
    // unidentified one.
    assert_eq!(stats.objects, 2 * STEP_SECRETS.len() + 2);
    assert_eq!(stats.with_revision, stats.objects);
    let credentials: usize = STEP_SECRETS.iter().map(|s| s.slots.len()).sum();
    // Each credential slot's text and three calculation streams, twice.
    assert_eq!(stats.paths["Credential"], 2 * 4 * credentials);
    assert!(stats.removed >= stats.paths["Credential"]);
    for class in ["Design", "Data", "Derived", "stamp"] {
        assert!(stats.paths.get(class).is_some_and(|&n| n > 0), "{class}");
    }

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
    list(
        &[],
        vec![
            leaf(&[0x04], &descriptor(1, 63)),
            list(
                &[0x05],
                vec![list(&[0x01], vec![calc_slot(3, b"\"Hi\"", 0), slot11])],
            ),
            leaf(&[0x10], &om_scsu::encode_scsu("Mail")),
        ],
    )
}

/// Import Records (35) with the profile `profile`.
fn import(profile: &[u8]) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x04], &descriptor(1, 35)),
            list(&[0x05], vec![list(&[0x01], vec![leaf(&[0x86], profile)])]),
            leaf(&[0x10], b"Import"),
        ],
    )
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
    let split_slot = |pw: &[u8]| {
        list(
            &[0, 11],
            vec![leaf(&[0], &(pw.len() as u32).to_be_bytes()), leaf(&[1], pw)],
        )
    };
    let with_list_value = |pw: &[u8], list_value: u8| {
        let mut slot = calc_slot(11, pw, 0);
        slot.value = Some(vec![0, 0, 0, list_value]);
        slot
    };
    let secrets_only = Difference::Secrets {
        data: false,
        derived: false,
    };
    let button = |ids: &[u64], extra: &[u8], pw: &[u8]| {
        let mut secrets = Vec::new();
        let mut member = button_member(&[], 138, &mut secrets);
        member.set_child(leaf(&[0x07], &store(5, ids, extra)));
        let data = member
            .at_mut(&[&[0x05], &serial::encode(5), &[0x11]])
            .unwrap();
        data.set_child(calc_slot(1, pw, 0));
        one_layout(member)
    };
    // A step list under `[00 04]` as well, which the decoders read by
    // number as `04`: the whole member could be read differently.
    let aliased = |pw: &[u8]| {
        let mut member = send_mail(calc_slot(11, pw, 0));
        member.set_child(leaf(&[0, 4], &descriptor(1, 1)));
        one_script(member, 1)
    };
    let script_with = |steps: Vec<u8>, pw: &[u8]| {
        let mut member = send_mail(calc_slot(11, pw, 0));
        member.set_child(leaf(&[0x04], &steps));
        one_script(member, 1)
    };
    let cases: Vec<(&str, OwnedNode, OwnedNode, Difference)> = vec![
        // Unexpected slot shapes: the password as the slot's own value, as
        // a split value at the slot. Beside a list value on the slot, the
        // password is an identified credential, and the list value is
        // unidentified content, which keeps the revision out.
        (
            "own value",
            one_script(send_mail(leaf(&[0, 11], &pw)), 1),
            one_script(send_mail(leaf(&[0, 11], &pw2)), 1),
            unidentified,
        ),
        (
            "split slot",
            one_script(send_mail(split_slot(&pw)), 1),
            one_script(send_mail(split_slot(&pw2)), 1),
            unidentified,
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
            "aliased step list",
            aliased(&pw),
            aliased(&pw2),
            unidentified,
        ),
        (
            "step id 0",
            script_with(descriptor(1, 0), &pw),
            script_with(descriptor(1, 0), &pw2),
            unidentified,
        ),
        // An ODBC profile mixes the query with the credentials: the
        // password alone, and the query alone, are both unidentified
        // content, so a deploy can never take a change in it for a stored
        // secret's (#723, "Keeping the target's credentials").
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

    // A name is reported only where it is Design content: not from a
    // member the decoders could read differently.
    let a = aliased(&pw);
    let names: Vec<Option<String>> = compare(&a, &a, Some(&a))
        .unwrap()
        .matched
        .iter()
        .filter(|m| !m.source.catalog)
        .map(|m| m.source.name.clone())
        .collect();
    assert_eq!(names, [None]);
    let plain = one_script(send_mail(calc_slot(11, &pw, 0)), 1);
    let named = compare(&plain, &plain, None).unwrap();
    assert_eq!(named.matched[1].source.name.as_deref(), Some("Mail"));

    // The same slots well formed: a design revision, equal across a
    // password change, which is "only stored secrets".
    let a = one_script(send_mail(calc_slot(11, &pw, 0)), 1);
    let b = one_script(send_mail(calc_slot(11, &pw2, 0)), 1);
    let (revision, difference) = compared(&a, &b);
    assert!(revision.is_some());
    assert_eq!(compared(&b, &a).0, revision);
    assert_eq!(difference, secrets_only);

    // A change only in a derived cache: the script's full-access flag in
    // its index entry `[11, 1, 07, S, f1]` is part of the script, so its
    // design revision moves and the change is Design; the catalog's digest
    // of the cache `[11, 1, 08]` is Derived, and moves no script.
    let flagged = one_script(send_mail(calc_slot(11, &pw, 0)), 3);
    assert_eq!(compared(&a, &flagged).1, Difference::Design);
    let flagged_revision = compared(&flagged, &a).0;
    assert!(flagged_revision.is_some());
    assert_ne!(flagged_revision, revision);
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
fn a_design_revision_changes_with_design_only_in_every_template_object() {
    let files = template_files();
    assert!(!files.is_empty(), "no template folder under re/extracted");
    let mut stats = Stats::default();
    for path in &files {
        let file = crate::HbamFile::open(path).unwrap();
        let root = file.key_tree().unwrap().root.to_owned_node();
        for object in objects(&root) {
            sweep(&root, &object, &[], &mut stats);
        }
    }
    println!("{} files: {stats:?}", files.len());
    assert!(stats.objects > 0);
    assert!(stats.paths.get("Design").is_some_and(|&n| n > 0));
}
