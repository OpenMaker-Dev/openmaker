//! Matching and classes of difference, as one table of cases over a small
//! base file and its changed copies.

use super::super::tests::{descriptor, leaf, list};
use super::*;
use crate::serial;

/// The table's key, `80 01` (table 129).
const TABLE: &[u8] = &[0x80, 0x01];

/// A script member: Re-Login (138, data key 1) whose password calculation
/// is `password`, named `name`.
fn script(name: &str, password: &[u8]) -> OwnedNode {
    list(
        &[],
        vec![
            leaf(&[0x04], &descriptor(1, 138)),
            list(
                &[0x05],
                vec![list(
                    &[0x01],
                    vec![list(
                        &[0, 1],
                        vec![list(&[0x05], vec![leaf(&[0x05], password)])],
                    )],
                )],
            ),
            leaf(&[0x10], name.as_bytes()),
            leaf(&[0xfc], &[1]),
        ],
    )
}

/// The base: scripts 1 "Alpha" and 2 "Beta", script 3 "Gamma" holding the
/// unexplained key `13`, table 129 "Tasks" with field 1 "Count" (a next
/// serial value `0c`), a file library item (an MD5 `04` and a reference
/// count `12`), and the scripts' catalog index.
fn base() -> OwnedNode {
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[1]], script("Alpha", b"pw"));
    root.put(&[&[0x11], &[5], &[2]], script("Beta", b"pw"));
    let mut gamma = script("Gamma", b"pw");
    gamma.set_child(leaf(&[0x13], &[0, 0, 0, 0]));
    root.put(&[&[0x11], &[5], &[3]], gamma);
    root.put(&[&[0x11], &[1], &[0x03]], leaf(&[], &[1, 2, 3]));
    root.put(&[&[0x11], &[1], &[0x08]], leaf(&[], &[8; 16]));
    root.put(
        &[&[0x03], &[0x10], &[5], TABLE],
        list(&[], vec![leaf(&[0x10], b"Tasks")]),
    );
    root.put(
        &[TABLE, &[3], &[5], &[1]],
        list(
            &[],
            vec![
                leaf(&[0x02], &[1, 0]),
                leaf(&[0x0c], b"7"),
                leaf(&[0x10], b"Count"),
            ],
        ),
    );
    root.put(
        &[&[0x1f], &[5], &[1]],
        list(
            &[],
            vec![
                leaf(&[0x02], &[2]),
                leaf(&[0x04], &[4; 16]),
                leaf(&[0x10], b"logo"),
                leaf(&[0x12], &[1]),
            ],
        ),
    );
    root
}

fn set(root: &mut OwnedNode, path: &[&[u8]], value: &[u8]) {
    root.put(path, leaf(&[], value));
}

const ALPHA: &[&[u8]] = &[&[0x11], &[5], &[1]];
const BETA: &[&[u8]] = &[&[0x11], &[5], &[2]];

fn under<'a>(member: &[&'a [u8]], rest: &[&'a [u8]]) -> Vec<&'a [u8]> {
    member.iter().chain(rest).copied().collect()
}

fn rename(root: &mut OwnedNode, member: &[&[u8]], name: &str) {
    set(root, &under(member, &[&[0x10]]), name.as_bytes());
}

fn password(root: &mut OwnedNode, member: &[&[u8]], password: &[u8]) {
    set(
        root,
        &under(member, &[&[0x05], &[0x01], &[0, 1], &[0x05], &[0x05]]),
        password,
    );
}

/// The matched object of `kind` and `id` (a member).
fn matched(comparison: &Comparison, kind: Kind, id: u64) -> &Matched {
    comparison
        .matched
        .iter()
        .find(|m| m.source.kind == kind && !m.source.catalog && m.source.id == Some(id))
        .unwrap_or_else(|| panic!("{kind:?} {id} is not matched: {comparison:#?}"))
}

const SAME: Difference = Difference::Same;
const SECRETS: Difference = Difference::Secrets {
    data: false,
    derived: false,
};

#[test]
fn objects_are_matched_by_id_with_a_base_and_by_id_and_name_without() {
    let base = base();
    // One row per case: what the source and the target change, and for
    // the object it is about (kind, id): the difference between source
    // and target, and with the base each side's difference from it.
    type Change = fn(&mut OwnedNode);
    type Row = (&'static str, Change, Change, Kind, u64, [Difference; 3]);
    let rows: &[Row] = &[
        (
            "untouched",
            |_| {},
            |_| {},
            Kind::Script,
            2,
            [SAME, SAME, SAME],
        ),
        (
            "stamps only",
            |r| set(r, &under(BETA, &[&[0xfc]]), &[9]),
            |_| {},
            Kind::Script,
            2,
            [SAME, SAME, SAME],
        ),
        (
            "renamed in the source",
            |r| rename(r, BETA, "Beta 2"),
            |_| {},
            Kind::Script,
            2,
            [Difference::Design, Difference::Design, SAME],
        ),
        (
            "a password changed in the target",
            |_| {},
            |r| password(r, ALPHA, b"new"),
            Kind::Script,
            1,
            [SECRETS, SAME, SECRETS],
        ),
        (
            "a serial number taken in the target",
            |_| {},
            |r| set(r, &[TABLE, &[3], &[5], &[1], &[0x0c]], b"8"),
            Kind::Field,
            1,
            [
                Difference::Outside {
                    data: true,
                    derived: false,
                },
                SAME,
                Difference::Outside {
                    data: true,
                    derived: false,
                },
            ],
        ),
        (
            "a library item's count and digest",
            |_| {},
            |r| {
                set(r, &[&[0x1f], &[5], &[1], &[0x12]], &[2]);
                set(r, &[&[0x1f], &[5], &[1], &[0x04]], &[5; 16]);
            },
            Kind::LibraryItem,
            1,
            [
                Difference::Outside {
                    data: true,
                    derived: true,
                },
                SAME,
                Difference::Outside {
                    data: true,
                    derived: true,
                },
            ],
        ),
        (
            "an unexplained key changed",
            |r| set(r, &[&[0x11], &[5], &[3], &[0x13]], &[0, 0, 0, 1]),
            |_| {},
            Kind::Script,
            3,
            [
                Difference::Unidentified { design: false },
                Difference::Unidentified { design: false },
                SAME,
            ],
        ),
        (
            "an unexplained key and the name changed",
            |r| {
                set(r, &[&[0x11], &[5], &[3], &[0x13]], &[0, 0, 0, 1]);
                rename(r, &[&[0x11], &[5], &[3]], "Gamma 2");
            },
            |_| {},
            Kind::Script,
            3,
            [
                Difference::Unidentified { design: true },
                Difference::Unidentified { design: true },
                SAME,
            ],
        ),
        (
            "changed on both sides, differently: a conflict",
            |r| rename(r, BETA, "Beta S"),
            |r| rename(r, BETA, "Beta T"),
            Kind::Script,
            2,
            [Difference::Design, Difference::Design, Difference::Design],
        ),
        (
            "changed on both sides, the same way",
            |r| rename(r, BETA, "Beta 2"),
            |r| rename(r, BETA, "Beta 2"),
            Kind::Script,
            2,
            [SAME, Difference::Design, Difference::Design],
        ),
    ];
    for &(case, change_source, change_target, kind, id, [difference, from_source, from_target]) in
        rows
    {
        let (mut source, mut target) = (base.clone(), base.clone());
        change_source(&mut source);
        change_target(&mut target);
        let with = compare(&source, &target, Some(&base)).unwrap();
        let m = matched(&with, kind, id);
        assert_eq!(m.difference, difference, "{case}");
        let from = m.base.as_ref().unwrap();
        assert_eq!(
            (from.source, from.target),
            (from_source, from_target),
            "{case}"
        );
        let expected_by = match (from_source.is_change(), from_target.is_change()) {
            (true, true) => Some(ChangedBy::Both),
            (true, false) => Some(ChangedBy::Source),
            (false, true) => Some(ChangedBy::Target),
            (false, false) => None,
        };
        assert_eq!(from.changed_by(), expected_by, "{case}");
        assert_eq!(
            m.conflict(),
            case.ends_with("a conflict"),
            "{case}: conflict"
        );
        // Without a base the same objects are matched by kind, id and
        // name, where the name agrees, with the same difference.
        let without = compare(&source, &target, None).unwrap();
        if m.source.name == m.target_name {
            assert_eq!(matched(&without, kind, id).difference, difference, "{case}");
        }
        assert!(without.matched.iter().all(|m| m.base.is_none()));
    }

    // Matched objects carry design revisions, and one only where the
    // object holds no unidentified content (Gamma's key `13`).
    let same = compare(&base, &base, Some(&base)).unwrap();
    let alpha = matched(&same, Kind::Script, 1);
    assert!(alpha.source_revision.is_some());
    assert_eq!(alpha.source_revision, alpha.target_revision);
    assert_eq!(matched(&same, Kind::Script, 3).source_revision, None);
    assert!(same.only_in_source.is_empty() && same.only_in_target.is_empty());
    assert!(same.possible_matches.is_empty());

    // With a base: a new object; an id both sides gave to unrelated new
    // objects; one the target deleted after the source changed it.
    let (mut source, mut target) = (base.clone(), base.clone());
    source.put(&[&[0x11], &[5], &[9]], script("Delta", b"pw"));
    source.put(&[&[0x11], &[5], &[7]], script("X", b"pw"));
    target.put(&[&[0x11], &[5], &[7]], script("Y", b"pw"));
    rename(&mut source, BETA, "Beta 2");
    target.remove(BETA);
    let with = compare(&source, &target, Some(&base)).unwrap();
    let presence = |alone: &[Alone]| -> Vec<(Option<u64>, Presence)> {
        alone.iter().map(|a| (a.object.id, a.presence)).collect()
    };
    assert_eq!(
        presence(&with.only_in_source),
        [
            (Some(2), Presence::DeletedInOther { changed: true }),
            (Some(7), Presence::New { id_taken: true }),
            (Some(9), Presence::New { id_taken: false }),
        ]
    );
    assert_eq!(
        presence(&with.only_in_target),
        [(Some(7), Presence::New { id_taken: true })]
    );
    assert!(with.possible_matches.is_empty());

    // Without a base: an id alone (renamed), a name alone (another id),
    // and a field whose table's name differs, which is not matched even
    // though the field's own id and name agree.
    let mut target = base.clone();
    rename(&mut target, BETA, "Beta 2");
    target.remove(ALPHA);
    target.put(&[&[0x11], &[5], &[4]], script("Alpha", b"pw"));
    rename(&mut target, &[&[0x03], &[0x10], &[5], TABLE], "Jobs");
    let without = compare(&base, &target, None).unwrap();
    let possible: Vec<(Kind, Option<u64>, Option<u64>, Agreement)> = without
        .possible_matches
        .iter()
        .map(|p| (p.source.kind, p.source.id, p.target.id, p.agrees))
        .collect();
    assert_eq!(
        possible,
        [
            (Kind::Table, Some(129), Some(129), Agreement::Id),
            (Kind::Script, Some(2), Some(2), Agreement::Id),
            (Kind::Field, Some(1), Some(1), Agreement::Id),
            (Kind::Script, Some(1), Some(4), Agreement::Name),
        ]
    );
    let only = |alone: &[Alone]| -> Vec<(Kind, Option<u64>, Presence)> {
        alone
            .iter()
            .map(|a| (a.object.kind, a.object.id, a.presence))
            .collect()
    };
    assert_eq!(
        only(&without.only_in_source),
        [(Kind::Script, Some(1), Presence::Only)]
    );
    assert_eq!(
        only(&without.only_in_target),
        [(Kind::Script, Some(4), Presence::Only)]
    );
    assert_eq!(matched(&without, Kind::Script, 3).difference, SAME);
    assert!(without
        .matched
        .iter()
        .all(|m| m.source.kind != Kind::Field && m.source.kind != Kind::Table));
    assert_eq!(serial::decode(TABLE), Some(129));
}

#[test]
fn a_file_is_read_for_its_design_alone() {
    let mut root = base();
    // Records, an index, a record list and an occurrence's found set:
    // not read.
    root.put(&[TABLE, &[5], &[1], &[1]], leaf(&[], b"a record"));
    root.put(&[TABLE, &[1], &[1]], leaf(&[], b"an index"));
    root.put(&[TABLE, &[0x0d]], leaf(&[], &[1]));
    root.put(&[&[0xd0, 0x00, 0x01], &[7], &[1]], leaf(&[], &[1]));
    let bytes = crate::writer::rebuild(&root, &crate::new_superblock("Test").unwrap()).unwrap();
    let file = HbamFile::from_bytes(bytes).unwrap();
    let design = design_tree(&file).unwrap();
    assert!(design.at(&[TABLE, &[5]]).is_none());
    assert!(design.at(&[TABLE, &[1]]).is_none());
    assert!(design.at(&[TABLE, &[0x0d]]).is_none());
    assert!(design.at(&[&[0xd0, 0x00, 0x01], &[7]]).is_none());
    assert_eq!(design.at(&[TABLE, &[3]]), root.at(&[TABLE, &[3]]));
    assert_eq!(design.at(&[&[0x11]]), root.at(&[&[0x11]]));
    let comparison = compare(&design, &root, None).unwrap();
    assert!(comparison
        .matched
        .iter()
        .all(|m| m.difference == Difference::Same));
}
