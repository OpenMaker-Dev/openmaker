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
fn value_list_members_hold_only_design() {
    // `om-server` and `om-provider` hash a value list member whole
    // (`catalog_revision`) as its edit token; no step rule reaches it, so
    // its design revision leaves nothing out either.
    for rule in rules() {
        if rule
            .pattern
            .starts_with(&[Seg::Key(&[0x21]), Seg::Key(&[0x05])])
        {
            assert_eq!(rule.group, Group::Design, "{}", rule.pattern_text());
        }
    }
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
fn data_inside_a_member_is_left_out_of_its_design_revision_not_its_token() {
    let field = |next: &[u8], name: &[u8]| {
        list(
            &[0x07],
            vec![
                leaf(&[0x02], &[1, 2]),
                leaf(&[0x0c], next),
                leaf(&[0x10], name),
                leaf(&[0x77], &[9]),
                leaf(&[0xfb], next),
                leaf(MOD_COUNT, next),
            ],
        )
    };
    let tree = |member: OwnedNode| {
        let mut root = OwnedNode::default();
        root.put(&[&[0x80, 0x01], &[3], &[5], &[7]], member);
        root
    };
    let hashes = |root: &OwnedNode| {
        let object = member(root, Kind::Field);
        (object.design_revision(), object.edit_token())
    };
    let (design, token) = hashes(&tree(field(b"1", b"Name")));
    let (served, served_token) = hashes(&tree(field(b"2", b"Name")));
    assert_eq!(design, served);
    assert_ne!(token, served_token);
    assert_ne!(design, hashes(&tree(field(b"1", b"Other"))).0);
    // A key no rule names stays in: OpenMaker does not know what it is.
    let mut unknown = field(b"1", b"Name");
    unknown.children[3].value = Some(vec![8]);
    assert_ne!(design, hashes(&tree(unknown)).0);

    let root = tree(field(b"1", b"Name"));
    let object = member(&root, Kind::Field);
    assert_eq!(object.id(), Some(7));
    let base = [vec![0x80, 0x01], vec![3], vec![5], vec![7]];
    let full = |key: u8| {
        let mut path = base.to_vec();
        path.push(vec![key]);
        path
    };
    assert_eq!(
        object.excluded(),
        vec![(full(0x0c), Group::Data), (full(0xfb), Group::Data)]
    );
    assert_eq!(
        object.unclassified(),
        vec!["[T, 03, 05, *, 77]".to_string()]
    );
    assert_eq!(object.design()[0].node.child(&[0x0c]), None);
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

/// A string as step streams hold it (script-steps.md 11.0).
fn string(text: &[u8]) -> Vec<u8> {
    let mut out = vec![text.len() as u8];
    out.extend(text);
    out
}

/// An ODBC profile (script-steps.md 11.5).
fn odbc(user: &[u8], password: &[u8], query: &[u8]) -> Vec<u8> {
    let mut out = b"ODBC".to_vec();
    out.extend([0, 1]);
    out.extend(string(b"dsn"));
    out.extend(string(user));
    out.extend(string(password));
    out.extend([0, 0]);
    out.extend(string(query));
    out
}

/// A script: step `step` (data key 1) with slots 7 and 11, an Import
/// Records (35, data key 2) with `profile`.
fn script(slot7: (&[u8], u8), slot11: &[u8], profile: &[u8], step: i16) -> OwnedNode {
    let mut steps = descriptor(1, step);
    steps.extend(descriptor(2, 35));
    list(
        &[3],
        vec![
            leaf(&[4], &steps),
            list(
                &[5],
                vec![
                    list(&[1], vec![slot(7, slot7.0, slot7.1), slot(11, slot11, 0)]),
                    list(&[2], vec![leaf(&[0x86], profile)]),
                ],
            ),
            leaf(&[0x10], b"Mail"),
        ],
    )
}

fn script_tree(member: OwnedNode) -> OwnedNode {
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[3]], member);
    root
}

fn hashes(member: OwnedNode) -> (DesignRevision, Revision) {
    let root = script_tree(member);
    let object = self::member(&root, Kind::Script);
    (object.design_revision(), object.edit_token())
}

#[test]
fn a_stored_credential_never_enters_the_design_revision_but_moves_the_token() {
    let profile = odbc(b"user", b"pw", b"SELECT 1");
    let base = hashes(script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63));
    // Another password, or another ODBC user or password: the same design
    // revision, another edit token.
    for other in [
        script((b"\"Hi\"", 0), b"\"other\"", &profile, 63),
        script(
            (b"\"Hi\"", 0),
            b"\"pw\"",
            &odbc(b"admin", b"secret", b"SELECT 1"),
            63,
        ),
        script(
            (b"\"Hi\"", 0),
            b"\"pw\"",
            &odbc(b"user", b"a much longer one", b"SELECT 1"),
            63,
        ),
    ] {
        let (design, token) = hashes(other);
        assert_eq!(design, base.0);
        assert_ne!(token, base.1);
    }
    // What lies beside a credential is design: the ODBC query, another
    // slot, and in Create PDF (243) the number beside the password slot 7
    // (allow screen reader, script-steps.md 12.2).
    assert_ne!(
        hashes(script(
            (b"\"Hi\"", 0),
            b"\"pw\"",
            &odbc(b"user", b"pw", b"SELECT 2"),
            63
        ))
        .0,
        base.0
    );
    assert_ne!(
        hashes(script((b"\"Bye\"", 0), b"\"pw\"", &profile, 63)).0,
        base.0
    );
    let pdf = |password: &[u8], screen_reader: u8| {
        hashes(script((password, screen_reader), b"x", &profile, 243)).0
    };
    assert_eq!(pdf(b"\"a\"", 1), pdf(b"\"b\"", 1));
    assert_ne!(pdf(b"\"a\"", 1), pdf(b"\"a\"", 0));
    // A disabled step stores its id negative: still Send Mail.
    assert_eq!(
        hashes(script((b"\"Hi\"", 0), b"\"x\"", &profile, -63)).0,
        hashes(script((b"\"Hi\"", 0), b"\"y\"", &profile, -63)).0
    );
    // Whether a credential is stored counts.
    let mut without = script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63);
    without.at_mut(&[&[5], &[1]]).unwrap().remove(&[&[0, 11]]);
    assert_ne!(hashes(without).0, base.0);
    // Slot 11 of a step that keeps no credential there is design.
    assert_ne!(
        hashes(script((b"\"Hi\"", 0), b"\"x\"", &profile, 1)).0,
        hashes(script((b"\"Hi\"", 0), b"\"y\"", &profile, 1)).0
    );

    let root = script_tree(script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63));
    let in_script = |rest: &[&[u8]]| {
        let mut path: Vec<&[u8]> = vec![&[0x11], &[5], &[3]];
        path.extend_from_slice(rest);
        group_in(&root, &path)
    };
    assert_eq!(in_script(&[&[5], &[1], &[0, 11], &[5]]), Group::Secret);
    assert_eq!(
        in_script(&[&[5], &[1], &[0, 11], &[5], &[6]]),
        Group::Secret
    );
    assert_eq!(in_script(&[&[5], &[1], &[0, 11], &[3]]), Group::Design);
    assert_eq!(in_script(&[&[5], &[1], &[0, 7], &[5]]), Group::Design);
    assert_eq!(in_script(&[&[5], &[2], &[0x86]]), Group::Secret);
    let object = member(&root, Kind::Script);
    let full = |rest: &[&[u8]]| {
        let mut path: Path = vec![vec![0x11], vec![5], vec![3]];
        path.extend(rest.iter().map(|s| s.to_vec()));
        path
    };
    assert_eq!(
        object.excluded(),
        vec![
            (full(&[&[5], &[1], &[0, 11], &[5]]), Group::Secret),
            (full(&[&[5], &[2], &[0x86]]), Group::Secret)
        ]
    );
    // Design content keeps the query and leaves the user and password out.
    let design = &object.design()[0].node;
    let shown = design
        .at(&[&[5], &[2], &[0x86]])
        .unwrap()
        .value
        .clone()
        .unwrap();
    assert!(shown.windows(8).any(|w| w == b"SELECT 1"));
    assert!(!shown.windows(4).any(|w| w == b"user"));
    let counted = report(&root);
    assert_eq!(counted.groups.get(&Group::Secret), Some(&3), "{counted}");
}

#[test]
fn step_data_whose_step_is_not_known_is_secret() {
    let profile = odbc(b"user", b"pw", b"SELECT 1");
    let slot11: [&[u8]; 6] = [&[0x11], &[5], &[3], &[5], &[1], &[0, 11]];
    let slot7: [&[u8]; 7] = [&[0x11], &[5], &[3], &[5], &[1], &[0, 7], &[3]];
    // A step id the catalog does not have, no step list, a list that does
    // not read, a data key no step names: every slot is Secret whole.
    let unknown = script_tree(script((b"\"Hi\"", 0), b"\"pw\"", &profile, 248));
    let mut no_list = script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63);
    no_list.remove(&[&[4]]);
    let mut torn = script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63);
    torn.set_child(leaf(&[4], &[0; 30]));
    let mut orphan = script((b"\"Hi\"", 0), b"\"pw\"", &profile, 63);
    orphan.set_child(leaf(&[4], &descriptor(9, 63)));
    for root in [
        unknown,
        script_tree(no_list),
        script_tree(torn),
        script_tree(orphan),
    ] {
        assert_eq!(group_in(&root, &slot11), Group::Secret);
        assert_eq!(group_in(&root, &slot7), Group::Secret);
        assert_eq!(
            group_in(&root, &[&[0x11], &[5], &[3], &[0x10]]),
            Group::Design
        );
    }
    // A path under a member the tree does not hold.
    assert_eq!(group_in(&OwnedNode::default(), &slot11), Group::Secret);
    // A known step that keeps no credential: design.
    let known = script_tree(script((b"\"Hi\"", 0), b"\"pw\"", &profile, 1));
    assert_eq!(group_in(&known, &slot11), Group::Design);
}

#[test]
fn a_button_keeps_its_steps_credentials_secret() {
    // A layout whose object store holds a button (object 5) running
    // Re-Login (138), and its attribute record's step data `11`.
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
    let mut step = Vec::new();
    varint(1 << 3, &mut step);
    varint(138, &mut step);
    let mut button = Vec::new();
    message(3, &step, &mut button);
    let mut object = Vec::new();
    varint(2 << 3, &mut object);
    varint(5, &mut object);
    message(16, &button, &mut object);
    let mut store = Vec::new();
    message(2, &object, &mut store);
    let layout = |password: &[u8]| {
        let mut root = OwnedNode::default();
        root.put(&[&[4], &[5], &[1], &[7]], leaf(&[], &store));
        root.put(
            &[&[4], &[5], &[1], &[5], &[5], &[0x11]],
            list(&[], vec![slot(0, b"\"me\"", 0), slot(1, password, 0)]),
        );
        root
    };
    let design = |root: &OwnedNode| member(root, Kind::Layout).design_revision();
    assert_eq!(design(&layout(b"\"a\"")), design(&layout(b"\"b\"")));
    let root = layout(b"\"a\"");
    let password: [&[u8]; 9] = [&[4], &[5], &[1], &[5], &[5], &[0x11], &[0, 1], &[5], &[5]];
    assert_eq!(group_in(&root, &password), Group::Secret);
}

#[test]
fn a_value_has_one_hash_inline_or_split_by_the_formats_rule() {
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
    let design = |m: OwnedNode| member_design(script_tree(m));
    fn member_design(root: OwnedNode) -> DesignRevision {
        member(&root, Kind::Script).design_revision()
    }
    assert_eq!(design(inline.clone()), design(split.clone()));
    // Near misses of the rule hash as the lists they are: pieces of 999
    // bytes, a total that does not add up, a header of another length, a
    // piece out of order, a node with a value of its own.
    let mut out_of_order = split_from(&bytes, total.clone(), 1000);
    out_of_order.children.swap(1, 2);
    let mut valued = split_from(&bytes, total.clone(), 1000);
    valued.value = Some(vec![1]);
    for near in [
        split_from(&bytes, total.clone(), 999),
        split_from(&bytes, 2499u32.to_be_bytes().to_vec(), 1000),
        split_from(&bytes, [total.clone(), vec![0]].concat(), 1000),
        out_of_order,
        valued,
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
fn a_catalog_is_an_object_over_its_order_and_settings() {
    let index = |order: [u8; 2], last: u8, digest: u8, full_access: u8| {
        list(
            &[1],
            vec![
                leaf(&[0], &[1, last]),
                list(
                    &[1],
                    vec![leaf(b"name a", &[1, 1]), leaf(b"name b", &[1, 2])],
                ),
                list(
                    &[3],
                    vec![
                        leaf(&[0, 0, 0, 1], &[1, order[0]]),
                        leaf(&[0, 0, 0, 2], &[1, order[1]]),
                    ],
                ),
                leaf(&[4], &[0, 0, 0, 2]),
                list(
                    &[7],
                    vec![
                        list(
                            &[1],
                            vec![leaf(&[0xf1], &[0, 0, 0, 0, 0, 0, 0, full_access])],
                        ),
                        list(&[2], vec![leaf(&[0xf1], &[0; 8])]),
                    ],
                ),
                leaf(&[8], &[digest; 16]),
            ],
        )
    };
    let revisions = |index: OwnedNode| {
        let mut root = OwnedNode::default();
        root.put(&[&[0x11], &[1]], index);
        for id in [1u8, 2] {
            root.put(
                &[&[0x11], &[5], &[id]],
                list(&[], vec![leaf(&[0x10], &[id])]),
            );
        }
        objects(&root)
            .iter()
            .map(|o| o.design_revision())
            .collect::<Vec<_>>()
    };
    let a = revisions(index([1, 2], 2, 7, 0));
    // Moving a script changes the catalog only; a counter or a digest
    // changes nothing; a script's cached full-access flag, which Script
    // Workspace shows (scripts.md), is that script's.
    let moved = revisions(index([2, 1], 2, 7, 0));
    assert_eq!(
        (moved[0] != a[0], moved[1] == a[1], moved[2] == a[2]),
        (true, true, true)
    );
    assert_eq!(a, revisions(index([1, 2], 9, 8, 0)));
    let granted = revisions(index([1, 2], 2, 7, 1));
    assert_eq!(
        (granted[0] == a[0], granted[1] != a[1], granted[2] == a[2]),
        (true, true, true)
    );

    // A privilege set's checksum, which covers the file's creation stamp,
    // is no part of the set's design revision; neither is pending field
    // work part of the field catalog's.
    let set = |checksum: u8| {
        let mut root = OwnedNode::default();
        root.put(
            &[&[0x17], &[2], &[5], &[1]],
            list(
                &[],
                vec![leaf(&[2], &[0x55, 3]), leaf(&[4], &[checksum; 16])],
            ),
        );
        member(&root, Kind::PrivilegeSet).design_revision()
    };
    assert_eq!(set(1), set(2));
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x80, 0x01], &[3], &[1], &[0xfd], &[1]],
        leaf(&[], &[0, 0, 0, 1]),
    );
    let before = objects(&root)[0].design_revision();
    root.put(
        &[&[0x80, 0x01], &[3], &[1], &[0xfd], &[1]],
        leaf(&[], &[0, 0, 0, 0x10]),
    );
    assert_eq!(objects(&root)[0].design_revision(), before);
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
    let design = member(&root, Kind::Font).design_revision();
    assert!(design.text().starts_with("design-sha256:"));
    assert_eq!(parse_revision(&design.text()), None);
}
