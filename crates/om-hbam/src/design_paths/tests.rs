use super::*;
use crate::serial;

/// A path the rule's pattern matches, one segment past it when `deeper`.
fn example(rule: &Rule, deeper: bool) -> Path {
    let mut path: Path = rule.pattern.iter().map(|s| s.example()).collect();
    if deeper {
        path.push(vec![0x7e]);
    }
    path
}

fn at(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

fn leaf(key: &[u8], value: &[u8]) -> OwnedNode {
    OwnedNode::leaf(key.to_vec(), value.to_vec())
}

fn list(key: &[u8], children: Vec<OwnedNode>) -> OwnedNode {
    OwnedNode::list(key.to_vec(), children)
}

/// The one member object of `kind` in `root`.
fn member<'t>(root: &'t OwnedNode, kind: Kind) -> Object<'t> {
    let mut found: Vec<Object<'t>> = objects(root)
        .into_iter()
        .filter(|o| o.kind == kind && !o.catalog)
        .collect();
    assert_eq!(found.len(), 1);
    found.remove(0)
}

#[test]
fn every_rule_has_a_path_that_hits_it_and_none_overlaps_another() {
    let all = rules();
    let mut root = OwnedNode::list(Vec::new(), Vec::new());
    for rule in all {
        for deeper in [false, true] {
            let path = example(rule, deeper);
            assert!(
                std::ptr::eq(rules::rule_for(&at(&path)).unwrap(), rule),
                "{} is shadowed at {path:02x?}",
                rule.pattern_text()
            );
        }
        let path = example(rule, true);
        root.put(&at(&path), OwnedNode::leaf(Vec::new(), vec![1]));
    }
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert!(
                !rules::overlap(&a.pattern, &b.pattern),
                "{} and {} can match one path",
                a.pattern_text(),
                b.pattern_text()
            );
        }
    }
    // The tree walk agrees: each rule covers its one path, nothing is left.
    let report = report(&root);
    assert_eq!(report.unclassified_count(), 0, "{report}");
    assert_eq!(report.hits.len(), all.len());
    assert!(report.hits.values().all(|&n| n == 1));
}

#[test]
fn table_and_occurrence_keys_of_every_length_are_classified() {
    // Ids at both ends of each serial key length (query-program.md 2.6)
    // that tables and occurrences can have.
    let tables = [
        TABLE_BASE,
        TABLE_BASE + 1,
        0x407f,
        0x4080,
        OCCURRENCE_BASE - 1,
    ];
    let occurrences = [
        OCCURRENCE_BASE,
        OCCURRENCE_BASE + 1,
        0x20_407f,
        0x20_4080,
        0x1020_407f,
        0x1020_4080,
        u64::MAX,
    ];
    let mut root = OwnedNode::default();
    for &id in &tables {
        let t = serial::encode(id);
        root.put(
            &[&[3], &[0x10], &[5], &t],
            list(&[], vec![leaf(&[0x10], b"T")]),
        );
        root.put(
            &[&t, &[3], &[5], &[1]],
            list(&[], vec![leaf(&[0x10], b"F")]),
        );
        root.put(&[&t, &[5], &[1], &[1]], leaf(&[], b"value"));
        root.put(&[&t, &[8], &[1]], leaf(&[], b"global"));
    }
    for &id in &occurrences {
        let o = serial::encode(id);
        root.put(
            &[&[3], &[0x11], &[5], &o],
            list(&[], vec![leaf(&[0x10], b"O")]),
        );
        root.put(&[&o, &[7], &[1], &[3]], leaf(&[], &[1, 0]));
    }
    for &id in &tables {
        let t = serial::encode(id);
        assert_eq!(group_in(&root, &[&t, &[8], &[1]]), Group::Data);
        assert_eq!(group_in(&root, &[&t, &[5], &[1], &[1]]), Group::Data);
        assert_eq!(
            group_in(&root, &[&t, &[3], &[5], &[1], &[0x10]]),
            Group::Design
        );
    }
    for &id in &occurrences {
        let o = serial::encode(id);
        assert_eq!(group_in(&root, &[&o, &[7], &[1], &[3]]), Group::Data);
    }
    let report = report(&root);
    assert_eq!(report.unclassified_count(), 0, "{report}");
    let objects = objects(&root);
    let count = |kind| {
        objects
            .iter()
            .filter(|o| o.kind == kind && !o.catalog)
            .count()
    };
    assert_eq!(count(Kind::Field), tables.len());
    assert_eq!(
        (count(Kind::Table), count(Kind::Occurrence)),
        (tables.len(), occurrences.len())
    );
}

#[test]
fn every_kind_has_one_catalog() {
    let kinds: Vec<Kind> = CATALOGS.iter().filter_map(|c| c.kind).collect();
    let mut unique = kinds.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), kinds.len());
    for catalog in CATALOGS {
        assert_eq!(catalog.kind.is_some(), catalog.group == Group::Design);
    }
}

#[test]
fn keys_inside_a_member_take_their_own_group_and_the_token_covers_them_all() {
    let field = |next: &[u8]| {
        list(
            &[0x07],
            vec![
                leaf(&[0x02], &[1, 2]),
                leaf(&[0x0c], next),
                leaf(&[0x10], b"Name"),
                leaf(&[0x77], &[9]),
                leaf(&[0xfb], &[1, 1]),
                leaf(MOD_COUNT, next),
            ],
        )
    };
    let tree = |member: OwnedNode| {
        let mut root = OwnedNode::default();
        root.put(&[&[0x80, 0x01], &[3], &[5], &[7]], member);
        root
    };
    let root = tree(field(b"1"));
    let path = |key: u8| [vec![0x80, 0x01], vec![3], vec![5], vec![7], vec![key]];
    assert_eq!(group_in(&root, &at(&path(0x0c))), Group::Data);
    assert_eq!(group_in(&root, &at(&path(0xfb))), Group::Data);
    assert_eq!(group_in(&root, &at(&path(0x10))), Group::Design);
    assert_eq!(group_in(&root, &at(&path(0x77))), Group::None);
    let object = member(&root, Kind::Field);
    assert_eq!(object.id(), Some(7));
    assert_eq!(
        object.unclassified(),
        vec!["[T, 03, 05, *, 77]".to_string()]
    );
    // The edit token covers the whole member, the serial's next value
    // included; the stamp only is left out.
    let mut restamped = field(b"1");
    restamped.children[5].value = Some(vec![9]);
    assert_eq!(
        member(&tree(restamped), Kind::Field).edit_token(),
        object.edit_token()
    );
    assert_ne!(
        member(&tree(field(b"2")), Kind::Field).edit_token(),
        object.edit_token()
    );
}

/// A script step descriptor (script-steps.md 2): data key `01 <key>`, step
/// id, no value or flags.
fn descriptor(key: u8, step: i16) -> Vec<u8> {
    let mut d = vec![2, 1, key, 0, 0, 0];
    d.extend([0; 14]);
    d.extend(step.to_be_bytes());
    d.extend([0; 6]);
    d
}

/// A calculation slot `[00 n]` holding `text` as its formula, and the
/// number `number` beside it.
fn slot(n: u8, text: &[u8], number: u8) -> OwnedNode {
    list(
        &[0, n],
        vec![
            leaf(&[3], &[number]),
            list(&[5], vec![leaf(&[5], text), leaf(&[6], text)]),
        ],
    )
}

/// A script: step `step` (data key 1) with slots 3, 7 and 11, an Import
/// Records (35, data key 2) with `profile`.
fn script(steps: Vec<u8>, slot11: OwnedNode, profile: &[u8]) -> OwnedNode {
    list(
        &[3],
        vec![
            leaf(&[4], &steps),
            list(
                &[5],
                vec![
                    list(
                        &[1],
                        vec![slot(3, b"\"Hi\"", 0), slot(7, b"\"pw\"", 1), slot11],
                    ),
                    list(&[2], vec![leaf(&[0x86], profile)]),
                ],
            ),
            leaf(&[0x10], b"Mail"),
        ],
    )
}

fn steps(step: i16) -> Vec<u8> {
    [descriptor(1, step), descriptor(2, 35)].concat()
}

fn script_tree(member: OwnedNode) -> OwnedNode {
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[3]], member);
    root
}

/// The group of `rest` below script 3's member.
fn in_script(root: &OwnedNode, rest: &[&[u8]]) -> Group {
    let mut path: Vec<&[u8]> = vec![&[0x11], &[5], &[3]];
    path.extend_from_slice(rest);
    group_in(root, &path)
}

#[test]
fn a_stored_credential_is_secret_and_what_lies_beside_it_is_design() {
    let root = script_tree(script(steps(63), slot(11, b"\"pw\"", 0), b"TABS\x00\x00"));
    // Send Mail's SMTP password: its calculation, and so the slot and the
    // member holding it.
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 11], &[5]]),
        Group::Secret
    );
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 11], &[5], &[6]]),
        Group::Secret
    );
    assert_eq!(in_script(&root, &[&[5], &[1], &[0, 11]]), Group::Secret);
    assert_eq!(in_script(&root, &[]), Group::Secret);
    // Beside it: the slot's number, another slot, a file profile.
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 11], &[3]]),
        Group::Design
    );
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 3], &[5]]),
        Group::Design
    );
    assert_eq!(in_script(&root, &[&[5], &[2], &[0x86]]), Group::Design);
    // A slot the step does not hold yet, where a credential would be.
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 21], &[5]]),
        Group::Secret
    );
    // Create PDF (243): the password calculation of slot 7 is Secret, the
    // number beside it (allow screen reader) is not.
    let pdf = script_tree(script(steps(243), slot(11, b"x", 0), b"TABS"));
    assert_eq!(in_script(&pdf, &[&[5], &[1], &[0, 7], &[5]]), Group::Secret);
    assert_eq!(in_script(&pdf, &[&[5], &[1], &[0, 7], &[3]]), Group::Design);
    // A disabled step stores its id negative: still Send Mail.
    let disabled = script_tree(script(steps(-63), slot(11, b"\"pw\"", 0), b"TABS"));
    assert_eq!(
        in_script(&disabled, &[&[5], &[1], &[0, 11], &[5]]),
        Group::Secret
    );
    // An ODBC profile, and one that does not read.
    let odbc = script_tree(script(steps(63), slot(11, b"x", 0), b"ODBC\x00\x01\x03dsn"));
    assert_eq!(in_script(&odbc, &[&[5], &[2], &[0x86]]), Group::Secret);
    let mut torn = script(steps(63), slot(11, b"x", 0), b"");
    torn.put(
        &[&[5], &[2], &[0x86]],
        list(&[], vec![leaf(&[0], &[0, 0, 0, 9])]),
    );
    assert_eq!(
        in_script(&script_tree(torn), &[&[5], &[2], &[0x86]]),
        Group::Secret
    );
    // A step that keeps no credential: design.
    let plain = script_tree(script(steps(1), slot(11, b"\"pw\"", 0), b"TABS"));
    assert_eq!(
        in_script(&plain, &[&[5], &[1], &[0, 11], &[5]]),
        Group::Design
    );
    assert_eq!(in_script(&plain, &[]), Group::None);
    let object = member(&root, Kind::Script);
    assert_eq!(
        object.secrets(),
        vec![(
            vec![vec![5], vec![1], vec![0, 11], vec![5]],
            "the SMTP password"
        )]
    );
    let counted = report(&root);
    assert_eq!(counted.groups.get(&Group::Secret), Some(&2), "{counted}");
}

#[test]
fn step_data_whose_step_is_not_identified_with_certainty_is_secret() {
    let password: [&[u8]; 4] = [&[5], &[1], &[0, 11], &[5]];
    let subject: [&[u8]; 4] = [&[5], &[1], &[0, 3], &[5]];
    let profile = b"TABS".as_slice();
    let cases: Vec<(&str, OwnedNode)> = vec![
        // A step id the catalog does not have, and 0 (a button's no
        // action) in a script.
        (
            "unknown id",
            script(steps(248), slot(11, b"\"pw\"", 0), profile),
        ),
        ("id 0", script(steps(0), slot(11, b"\"pw\"", 0), profile)),
        // No step list, one that does not divide into descriptors, a
        // descriptor whose data key does not decode, two descriptors
        // naming one data key with different steps, a data key no
        // descriptor names.
        ("no list", {
            let mut s = script(steps(63), slot(11, b"\"pw\"", 0), profile);
            s.remove(&[&[4]]);
            s
        }),
        (
            "torn list",
            script(vec![0; 30], slot(11, b"\"pw\"", 0), profile),
        ),
        ("bad key", {
            let mut d = steps(63);
            d[0] = 9;
            script(d, slot(11, b"\"pw\"", 0), profile)
        }),
        (
            "conflicting",
            script(
                [steps(63), descriptor(1, 1)].concat(),
                slot(11, b"\"pw\"", 0),
                profile,
            ),
        ),
        (
            "orphan",
            script(descriptor(9, 63), slot(11, b"\"pw\"", 0), profile),
        ),
        // A credential slot not shaped as script-steps.md 4.2 gives: the
        // password as the slot's own value, the slot a split value, an
        // unknown part.
        (
            "value on the slot",
            script(steps(63), leaf(&[0, 11], b"secret"), profile),
        ),
        (
            "split slot",
            script(
                steps(63),
                list(
                    &[0, 11],
                    vec![leaf(&[0], &[0, 0, 0, 6]), leaf(&[1], b"secret")],
                ),
                profile,
            ),
        ),
        (
            "unknown part",
            script(
                steps(63),
                list(&[0, 11], vec![leaf(&[0x77], b"secret")]),
                profile,
            ),
        ),
        // A credential slot whose field spec `10` is not 14 bytes, and a
        // descriptor whose data key is no serial key.
        (
            "short field spec",
            script(
                steps(63),
                list(&[0, 11], vec![leaf(&[0x10], b"secret")]),
                profile,
            ),
        ),
        ("data key no serial key", {
            let mut d = steps(63);
            d[2] = 0x80;
            script(d, slot(11, b"\"pw\"", 0), profile)
        }),
    ];
    for (case, member) in cases {
        let root = script_tree(member);
        assert_eq!(in_script(&root, &password), Group::Secret, "{case}");
        assert_eq!(in_script(&root, &subject), Group::Secret, "{case}");
        assert_eq!(in_script(&root, &[&[0x10]]), Group::Design, "{case}");
    }
    // A path under a member the tree does not hold.
    assert_eq!(in_script(&OwnedNode::default(), &password), Group::Secret);
}

/// A varint and a length-delimited protobuf field.
fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn message(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(field) << 3 | 2, out);
    varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

/// A layout whose object store holds button 5 with a `ScriptStep` holding
/// the ids `ids`, and the button's step data `11` with Re-Login's account
/// and password.
fn layout(ids: &[u64]) -> OwnedNode {
    layout_with(ids, &[])
}

/// [`layout`] with `extra` bytes at the end of the button's step message.
fn layout_with(ids: &[u64], extra: &[u8]) -> OwnedNode {
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
    varint(5, &mut object);
    message(16, &button, &mut object);
    let mut store = Vec::new();
    message(2, &object, &mut store);
    let mut root = OwnedNode::default();
    root.put(&[&[4], &[5], &[1], &[7]], leaf(&[], &store));
    root.put(
        &[&[4], &[5], &[1], &[5], &[5], &[0x11]],
        list(
            &[],
            vec![
                slot(0, b"\"me\"", 0),
                slot(1, b"\"pw\"", 0),
                slot(2, b"1", 0),
            ],
        ),
    );
    root
}

#[test]
fn a_buttons_credentials_are_secret_and_a_disputed_button_step_is_no_step() {
    let in_button = |root: &OwnedNode, slot: u8| {
        group_in(
            root,
            &[&[4], &[5], &[1], &[5], &[5], &[0x11], &[0, slot], &[5]],
        )
    };
    // Re-Login: account and password Secret, slot 2 design.
    let root = layout(&[138]);
    assert_eq!(
        (
            in_button(&root, 0),
            in_button(&root, 1),
            in_button(&root, 2)
        ),
        (Group::Secret, Group::Secret, Group::Design)
    );
    // Perform Script, then Re-Login in a repeated field: the layout decoder
    // takes the last, a first-wins reading the first. Neither is trusted.
    let disputed = layout(&[1, 138]);
    assert_eq!(in_button(&disputed, 2), Group::Secret);
    // Perform Script: no credential.
    let plain = layout(&[1]);
    assert_eq!(in_button(&plain, 1), Group::Design);
    // A field number 0 in the step message, which om-layout's reader
    // refuses: the button is not identified.
    let refused = layout_with(&[1], &[0x00, 0x00]);
    assert_eq!(in_button(&refused, 2), Group::Secret);
}

#[test]
fn a_member_whose_keys_can_read_as_others_by_number_is_secret_whole() {
    // The decoders find the step list by number: `[00 04]` reads as 4 and
    // sorts before `[04]`, so they take Re-Login (138) for data key 1 where
    // `[04]` says Perform Script (1).
    let mut member = script(steps(1), slot(11, b"\"pw\"", 0), b"TABS");
    member.set_child(leaf(&[0, 4], &steps(138)));
    let root = script_tree(member);
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 7], &[5]]),
        Group::Secret
    );
    assert_eq!(
        in_script(&root, &[&[5], &[1], &[0, 3], &[5]]),
        Group::Secret
    );
    assert_eq!(in_script(&root, &[&[0x10]]), Group::Secret);
    // A layout whose object store or a record's step data has an alias.
    let mut aliased_store = layout(&[1]);
    aliased_store.put(&[&[4], &[5], &[1], &[0, 7]], leaf(&[], &[]));
    let mut aliased_record = layout(&[1]);
    aliased_record.put(&[&[4], &[5], &[1], &[5], &[5], &[0, 0x11]], leaf(&[], &[]));
    let path: [&[u8]; 8] = [&[4], &[5], &[1], &[5], &[5], &[0x11], &[0, 1], &[5]];
    for root in [aliased_store, aliased_record] {
        assert_eq!(group_in(&root, &path), Group::Secret);
    }
    // A web viewer's address `00 00` reads as 0, which no decoder looks
    // up: the layout is read as before.
    let mut web_viewer = layout(&[1]);
    web_viewer.put(&[&[4], &[5], &[1], &[5], &[5], &[0, 0]], leaf(&[], &[]));
    assert_eq!(group_in(&web_viewer, &path), Group::Design);
}

#[test]
fn a_value_has_one_token_inline_or_split_by_the_formats_rule() {
    let bytes: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
    let split_from = |bytes: &[u8], header: Vec<u8>, size: usize| {
        let mut node = list(&[0x04], vec![leaf(&[0], &header)]);
        for (i, piece) in bytes.chunks(size).enumerate() {
            node.children
                .push(leaf(&serial::encode(i as u64 + 1), piece));
        }
        node
    };
    let total = (bytes.len() as u32).to_be_bytes().to_vec();
    let with_list = |step_list: OwnedNode| list(&[1], vec![step_list, leaf(&[0x10], b"S")]);
    let inline = with_list(leaf(&[0x04], &bytes));
    let split = with_list(split_from(&bytes, total.clone(), 1000));
    assert_eq!(script_form_revision(&inline), script_form_revision(&split));
    assert_eq!(catalog_revision(&inline), catalog_revision(&split));
    assert_ne!(
        legacy_catalog_revision(&inline),
        legacy_catalog_revision(&split)
    );
    // Near misses of the rule hash as the lists they are: pieces of 999
    // bytes, a total that does not add up, a header of another length or
    // under another key, a piece out of order or with a child, an extra
    // node, an empty or an oversized last piece, a node with a value of
    // its own.
    let mut out_of_order = split_from(&bytes, total.clone(), 1000);
    out_of_order.children.swap(1, 2);
    let mut valued = split_from(&bytes, total.clone(), 1000);
    valued.value = Some(vec![1]);
    let mut header_key = split_from(&bytes, total.clone(), 1000);
    header_key.children[0].segment = vec![0x7f];
    let mut child = split_from(&bytes, total.clone(), 1000);
    child.children[1].children.push(leaf(&[1], &[1]));
    let mut extra = split_from(&bytes, total.clone(), 1000);
    extra.children.push(leaf(&[0x7e], &[]));
    let mut empty_last = split_from(&bytes, total.clone(), 1000);
    empty_last.children.push(leaf(&serial::encode(4), &[]));
    let oversized = {
        let mut node = list(&[0x04], vec![leaf(&[0], &1001u32.to_be_bytes())]);
        node.children.push(leaf(&[1], &[7; 1001]));
        node
    };
    for near in [
        split_from(&bytes, total.clone(), 999),
        split_from(&bytes, 2499u32.to_be_bytes().to_vec(), 1000),
        split_from(&bytes, [total.clone(), vec![0]].concat(), 1000),
        out_of_order,
        valued,
        header_key,
        child,
        extra,
        empty_last,
        oversized,
    ] {
        assert_eq!(split_value(&near), None);
        assert_ne!(
            catalog_revision(&with_list(near)),
            catalog_revision(&inline)
        );
    }
    // A list meeting the rule is a split value as far as the format can
    // tell (file-format.md 3.6), and hashes as one.
    let tiny = list(
        &[0x16],
        vec![leaf(&[0], &[0, 0, 0, 1]), leaf(&[1], &[0x41])],
    );
    assert_eq!(split_value(&tiny), Some(vec![0x41]));
    assert_eq!(
        catalog_revision(&tiny),
        catalog_revision(&leaf(&[0x16], &[0x41]))
    );
}

#[test]
fn a_catalog_and_its_members_are_objects_with_their_index_entries() {
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x11], &[1]],
        list(
            &[1],
            vec![
                leaf(&[0], &[1, 2]),
                list(
                    &[3],
                    vec![leaf(&[0, 0, 0, 1], &[1, 1]), leaf(&[0, 0, 0, 2], &[1, 2])],
                ),
                list(
                    &[7],
                    vec![list(&[1], vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, 1])])],
                ),
                leaf(&[8], &[7; 16]),
            ],
        ),
    );
    for id in [1u8, 2] {
        root.put(
            &[&[0x11], &[5], &[id]],
            list(&[], vec![leaf(&[0x10], &[id])]),
        );
    }
    let found = objects(&root);
    assert_eq!(found.len(), 3);
    assert!(found[0].catalog && found[0].entries.is_empty());
    let entries: Vec<Path> = found[1].entries.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(entries, vec![vec![vec![0x11], vec![1], vec![7], vec![1]]]);
    assert!(found[2].entries.is_empty());
    let index = |key: u8| [vec![0x11], vec![1], vec![key]];
    assert_eq!(group_in(&root, &at(&index(3))), Group::Design);
    assert_eq!(group_in(&root, &at(&index(7))), Group::Design);
    assert_eq!(group_in(&root, &at(&index(8))), Group::Derived);
    assert_eq!(group_in(&root, &at(&index(0))), Group::Data);
    // A privilege set's checksum covers the file's creation stamp.
    assert_eq!(
        group_in(&root, &[&[0x17], &[2], &[5], &[1], &[4]]),
        Group::Derived
    );
}

#[test]
fn base_directories_fonts_and_themes_have_their_own_ids_and_names() {
    let text = |key: u8, text: &str| leaf(&[key], &om_scsu::encode_scsu(text));
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x26], &[5], &[0, 0, 0, 0]],
        list(&[], vec![text(0x10, "Tasks/")]),
    );
    root.put(
        &[&[0x19], &[5], &[0, 1]],
        list(&[], vec![text(0x10, "Helvetica")]),
    );
    root.put(
        &[&[0x06], &[5], &[1]],
        list(
            &[],
            vec![text(0x0a, "Universal"), text(0x10, "com.example.theme")],
        ),
    );
    let found: Vec<(Kind, Option<u64>, Option<String>)> = objects(&root)
        .into_iter()
        .map(|o| (o.kind, o.id(), o.name))
        .collect();
    assert_eq!(
        found,
        vec![
            (Kind::Theme, Some(1), Some("Universal".into())),
            (Kind::Font, Some(1), Some("Helvetica".into())),
            (Kind::BaseDirectory, Some(0), Some("Tasks/".into())),
        ]
    );
}
