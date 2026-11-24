use super::*;

/// A path the rule's pattern matches, one segment past it when `deeper`.
fn example(rule: &Rule, deeper: bool) -> Vec<Vec<u8>> {
    let mut path: Vec<Vec<u8>> = rule.pattern.iter().map(|s| s.example()).collect();
    if deeper {
        path.push(vec![0x7e]);
    }
    path
}

#[test]
fn every_rule_has_a_path_that_hits_it_and_none_overlaps_another() {
    let all = rules();
    let mut root = OwnedNode::list(Vec::new(), Vec::new());
    for rule in all {
        for deeper in [false, true] {
            let path = example(rule, deeper);
            let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            assert!(
                std::ptr::eq(rule_for(&segments).unwrap(), rule),
                "{} is shadowed at {segments:02x?}",
                rule.pattern_text()
            );
        }
        let path = example(rule, true);
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        root.put(&segments, OwnedNode::leaf(Vec::new(), vec![1]));
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
fn hosts_and_clients_compute_the_same_script_layout_and_value_list_revisions() {
    // `om-server` and `om-provider` hash a script, layout or value list
    // member whole (`content_revision`, `catalog_revision`); that equals
    // its object revision only while no path of those catalogs is Data or
    // Access.
    for kind in [Kind::Script, Kind::Layout, Kind::ValueList] {
        let catalog = kind.catalog();
        for rule in rules() {
            if catalog.path.iter().zip(&rule.pattern).all(|(a, b)| a == b) {
                assert!(
                    matches!(rule.group, Group::Design | Group::None),
                    "{} is {:?}",
                    rule.pattern_text(),
                    rule.group
                );
            }
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
                OwnedNode::leaf(vec![0x02], vec![1, 2]),
                OwnedNode::leaf(vec![0x0c], next.to_vec()),
                OwnedNode::leaf(vec![0x10], name.to_vec()),
                OwnedNode::leaf(vec![0x77], vec![9]),
                OwnedNode::leaf(vec![0xfb], next.to_vec()),
                OwnedNode::leaf(MOD_COUNT.to_vec(), next.to_vec()),
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
