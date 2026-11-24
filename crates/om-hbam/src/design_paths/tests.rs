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

#[test]
fn every_rule_has_a_path_that_hits_it_and_none_overlaps_another() {
    let all = rules();
    let mut root = OwnedNode::list(Vec::new(), Vec::new());
    for rule in all {
        for deeper in [false, true] {
            let path = example(rule, deeper);
            assert!(
                std::ptr::eq(rule_for(&at(&path)).unwrap(), rule),
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
            OwnedNode::list(vec![], vec![leaf(&[0x10], b"T")]),
        );
        root.put(
            &[&t, &[3], &[5], &[1]],
            OwnedNode::list(vec![], vec![leaf(&[0x10], b"F")]),
        );
        root.put(&[&t, &[5], &[1], &[1]], leaf(&[], b"value"));
        root.put(&[&t, &[8], &[1]], leaf(&[], b"global"));
        assert_eq!(group_of(&[&t, &[8], &[1]]), Group::Data);
        assert_eq!(group_of(&[&t, &[5], &[1], &[1]]), Group::Data);
        assert_eq!(group_of(&[&t, &[3], &[5], &[1], &[0x10]]), Group::Design);
    }
    for &id in &occurrences {
        let o = serial::encode(id);
        root.put(
            &[&[3], &[0x11], &[5], &o],
            OwnedNode::list(vec![], vec![leaf(&[0x10], b"O")]),
        );
        root.put(&[&o, &[7], &[1], &[3]], leaf(&[], &[1, 0]));
        assert_eq!(group_of(&[&o, &[7], &[1], &[3]]), Group::Data);
    }
    let report = report(&root);
    assert_eq!(report.unclassified_count(), 0, "{report}");
    let objects = objects(&root);
    let fields = objects
        .iter()
        .filter(|o| o.kind == Kind::Field && !o.catalog)
        .count();
    assert_eq!(fields, tables.len());
    let named = |kind| {
        objects
            .iter()
            .filter(|o| o.kind == kind && !o.catalog)
            .count()
    };
    assert_eq!(
        (named(Kind::Table), named(Kind::Occurrence)),
        (tables.len(), occurrences.len())
    );
}

#[test]
fn value_list_members_hold_only_design() {
    // `om-server` and `om-provider` hash a value list member whole
    // (`catalog_revision`); that equals its object revision only while no
    // path of the catalog's members is in another group and no step rule
    // reaches it.
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
fn data_inside_a_member_is_left_out_of_its_revision() {
    let field = |next: &[u8], name: &[u8]| {
        OwnedNode::list(
            vec![0x07],
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
    let table = Some(&[0x80, 0x01][..]);
    let a = revision_of(Kind::Field, table, &field(b"1", b"Name"));
    assert_eq!(a, revision_of(Kind::Field, table, &field(b"2", b"Name")));
    assert_ne!(a, revision_of(Kind::Field, table, &field(b"1", b"Other")));
    // A key no rule names stays in: OpenMaker does not know what it is.
    let mut unknown = field(b"1", b"Name");
    unknown.children[3].value = Some(vec![8]);
    assert_ne!(a, revision_of(Kind::Field, table, &unknown));

    let mut root = OwnedNode::default();
    root.put(&[&[0x80, 0x01], &[3], &[5], &[7]], field(b"1", b"Name"));
    let objects = objects(&root);
    assert_eq!(objects.len(), 1);
    let object = &objects[0];
    assert_eq!((object.kind, object.id()), (Kind::Field, Some(7)));
    assert_eq!(object.revision(), a);
    assert_eq!(
        object.excluded(),
        vec![
            (vec![vec![0x0c]], Group::Data),
            (vec![vec![0xfb]], Group::Data)
        ]
    );
    assert_eq!(
        object.unclassified(),
        vec!["[T, 03, 05, *, 77]".to_string()]
    );
    assert_eq!(object.design().child(&[0x0c]), None);
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

/// A calculation slot `[00 n]` holding `text` as its formula.
fn slot(n: u8, text: &[u8]) -> OwnedNode {
    OwnedNode::list(
        vec![0, n],
        vec![OwnedNode::list(
            vec![5],
            vec![leaf(&[5], text), leaf(&[6], text)],
        )],
    )
}

/// A script with Send Mail (step 63, data key 1) and an Import Records
/// (35, data key 2) with `profile`.
fn script(password: &[u8], subject: &[u8], profile: &[u8], step: i16) -> OwnedNode {
    let mut steps = descriptor(1, step);
    steps.extend(descriptor(2, 35));
    OwnedNode::list(
        vec![3],
        vec![
            leaf(&[4], &steps),
            OwnedNode::list(
                vec![5],
                vec![
                    OwnedNode::list(vec![1], vec![slot(3, subject), slot(11, password)]),
                    OwnedNode::list(vec![2], vec![leaf(&[0x86], profile)]),
                ],
            ),
            leaf(&[0x10], b"Mail"),
        ],
    )
}

#[test]
fn a_stored_credential_is_secret_and_its_value_never_enters_a_revision() {
    let odbc = b"ODBC\x00\x00 dsn user password";
    let base = script(b"\"pw\"", b"\"Hi\"", odbc, 63);
    let revision = |s: &OwnedNode| revision_of(Kind::Script, None, s);
    let a = revision(&base);
    // Another password, or another ODBC password, keeps the revision.
    assert_eq!(a, revision(&script(b"\"other\"", b"\"Hi\"", odbc, 63)));
    assert_eq!(
        a,
        revision(&script(b"\"pw\"", b"\"Hi\"", b"ODBC other", 63))
    );
    // A disabled step stores its id negative: still a credential.
    assert_eq!(
        revision(&script(b"\"x\"", b"\"Hi\"", odbc, -63)),
        revision(&script(b"\"y\"", b"\"Hi\"", odbc, -63))
    );
    // Other content counts, and so does whether a credential is stored.
    assert_ne!(a, revision(&script(b"\"pw\"", b"\"Bye\"", odbc, 63)));
    let mut without = base.clone();
    without.at_mut(&[&[5], &[1]]).unwrap().remove(&[&[0, 11]]);
    assert_ne!(a, revision(&without));
    // A file profile is design; so is slot 11 of another step.
    assert_ne!(
        revision(&script(b"\"pw\"", b"\"Hi\"", b"TABS\x00\x00", 63)),
        revision(&script(b"\"pw\"", b"\"Hi\"", b"TABS\x00\x01", 63))
    );
    assert_ne!(
        revision(&script(b"\"x\"", b"\"Hi\"", odbc, 1)),
        revision(&script(b"\"y\"", b"\"Hi\"", odbc, 1))
    );

    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[3]], base);
    let password: [&[u8]; 6] = [&[0x11], &[5], &[3], &[5], &[1], &[0, 11]];
    assert_eq!(group_in(&root, &password), Group::Secret);
    assert_eq!(
        group_in(
            &root,
            &[&[0x11], &[5], &[3], &[5], &[1], &[0, 11], &[5], &[5]]
        ),
        Group::Secret
    );
    assert_eq!(group_of(&password), Group::Design);
    assert_eq!(
        group_in(&root, &[&[0x11], &[5], &[3], &[5], &[1], &[0, 3]]),
        Group::Design
    );
    let object = objects(&root).into_iter().find(|o| !o.catalog).unwrap();
    assert_eq!(
        object.excluded(),
        vec![
            (vec![vec![5], vec![1], vec![0, 11]], Group::Secret),
            (vec![vec![5], vec![2], vec![0x86]], Group::Secret)
        ]
    );
    let design = object.design();
    let emptied = design.at(&[&[5], &[1], &[0, 11]]).unwrap();
    assert!(emptied.value.is_none() && emptied.children.is_empty());
    let counted = report(&root);
    assert_eq!(counted.groups.get(&Group::Secret), Some(&3), "{counted}");
}

#[test]
fn a_value_has_one_revision_inline_or_split() {
    let bytes: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
    let mut split = OwnedNode::list(
        vec![0x04],
        vec![leaf(&[0], &(bytes.len() as u32).to_be_bytes())],
    );
    for (i, piece) in bytes.chunks(1000).enumerate() {
        split
            .children
            .push(leaf(&serial::encode(i as u64 + 1), piece));
    }
    let member =
        |step_list: OwnedNode| OwnedNode::list(vec![1], vec![step_list, leaf(&[0x10], b"S")]);
    let inline = member(leaf(&[0x04], &bytes));
    let chunked = member(split);
    assert_eq!(
        revision_of(Kind::Script, None, &inline),
        revision_of(Kind::Script, None, &chunked)
    );
    assert_eq!(catalog_revision(&inline), catalog_revision(&chunked));
    assert_eq!(
        script_form_revision(&inline),
        script_form_revision(&chunked)
    );
}

#[test]
fn a_catalog_is_an_object_over_its_order_and_settings() {
    let index = |order: [u8; 2], last: u8, digest: u8| {
        OwnedNode::list(
            vec![1],
            vec![
                leaf(&[0], &[1, last]),
                OwnedNode::list(
                    vec![1],
                    vec![leaf(b"name a", &[1, 1]), leaf(b"name b", &[1, 2])],
                ),
                OwnedNode::list(
                    vec![3],
                    vec![
                        leaf(&[0, 0, 0, 1], &[1, order[0]]),
                        leaf(&[0, 0, 0, 2], &[1, order[1]]),
                    ],
                ),
                leaf(&[4], &[0, 0, 0, 2]),
                leaf(&[8], &[digest; 16]),
            ],
        )
    };
    let catalog = |index: OwnedNode| {
        let mut root = OwnedNode::default();
        root.put(&[&[0x11], &[1]], index);
        objects(&root)
            .into_iter()
            .map(|o| o.revision())
            .next()
            .unwrap()
    };
    let a = catalog(index([1, 2], 2, 7));
    // Moving a script changes the catalog; a counter or a digest does not.
    assert_ne!(a, catalog(index([2, 1], 2, 7)));
    assert_eq!(a, catalog(index([1, 2], 9, 8)));

    // A privilege set's checksum, which covers the file's creation stamp,
    // is no part of the set's revision; neither is pending field work.
    let set = |checksum: u8| {
        OwnedNode::list(
            vec![1],
            vec![leaf(&[2], &[0x55, 3]), leaf(&[4], &[checksum; 16])],
        )
    };
    assert_eq!(
        revision_of(Kind::PrivilegeSet, None, &set(1)),
        revision_of(Kind::PrivilegeSet, None, &set(2))
    );
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x80, 0x01], &[3], &[1], &[0xfd], &[1]],
        leaf(&[], &[0, 0, 0, 1]),
    );
    let before = objects(&root)[0].revision();
    root.put(
        &[&[0x80, 0x01], &[3], &[1], &[0xfd], &[1]],
        leaf(&[], &[0, 0, 0, 0x10]),
    );
    assert_eq!(objects(&root)[0].revision(), before);
}

#[test]
fn base_directories_fonts_and_themes_have_their_own_ids_and_names() {
    let text = |key: u8, text: &str| leaf(&[key], &om_scsu::encode_scsu(text));
    let mut root = OwnedNode::default();
    root.put(
        &[&[0x26], &[5], &[0, 0, 0, 0]],
        OwnedNode::list(vec![], vec![text(0x10, "Tasks/")]),
    );
    root.put(
        &[&[0x19], &[5], &[0, 1]],
        OwnedNode::list(vec![], vec![text(0x10, "Helvetica")]),
    );
    root.put(
        &[&[0x06], &[5], &[1]],
        OwnedNode::list(
            vec![],
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
