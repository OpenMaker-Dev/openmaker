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

const SAME: Difference = Difference {
    design: false,
    step_data: false,
    unidentified: false,
    data: false,
    derived: false,
};
const DESIGN: Difference = Difference {
    design: true,
    ..SAME
};
/// A password is in step data: a change of it is a change of step data.
const STEP_DATA: Difference = Difference {
    step_data: true,
    ..SAME
};
const DATA: Difference = Difference { data: true, ..SAME };
const UNIDENTIFIED: Difference = Difference {
    unidentified: true,
    ..SAME
};

#[test]
fn objects_are_matched_by_id_with_a_base_and_by_id_and_name_without() {
    let base = base();
    // One row per case: what the source and the target change, and for
    // the object it is about (kind, id): the difference between source
    // and target, with the base each side's difference from it, who
    // changed it and whether that is a conflict.
    type Change = fn(&mut OwnedNode);
    type Row = (
        &'static str,
        Change,
        Change,
        Kind,
        u64,
        [Difference; 3],
        (Option<ChangedBy>, bool),
    );
    let rows: &[Row] = &[
        (
            "untouched",
            |_| {},
            |_| {},
            Kind::Script,
            2,
            [SAME, SAME, SAME],
            (None, false),
        ),
        (
            "stamps only",
            |r| set(r, &under(BETA, &[&[0xfc]]), &[9]),
            |_| {},
            Kind::Script,
            2,
            [SAME, SAME, SAME],
            (None, false),
        ),
        (
            "renamed in the source",
            |r| rename(r, BETA, "Beta 2"),
            |_| {},
            Kind::Script,
            2,
            [DESIGN, DESIGN, SAME],
            (Some(ChangedBy::Source), false),
        ),
        (
            "a password changed in the target",
            |_| {},
            |r| password(r, ALPHA, b"new"),
            Kind::Script,
            1,
            [STEP_DATA, SAME, STEP_DATA],
            (Some(ChangedBy::Target), false),
        ),
        (
            "a serial number taken in the target",
            |_| {},
            |r| set(r, &[TABLE, &[3], &[5], &[1], &[0x0c]], b"8"),
            Kind::Field,
            1,
            [DATA, SAME, DATA],
            (None, false),
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
                Difference {
                    data: true,
                    derived: true,
                    ..SAME
                },
                SAME,
                Difference {
                    data: true,
                    derived: true,
                    ..SAME
                },
            ],
            (None, false),
        ),
        (
            "an unexplained key changed",
            |r| set(r, &[&[0x11], &[5], &[3], &[0x13]], &[0, 0, 0, 1]),
            |_| {},
            Kind::Script,
            3,
            [UNIDENTIFIED, UNIDENTIFIED, SAME],
            (Some(ChangedBy::Source), false),
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
                Difference {
                    design: true,
                    unidentified: true,
                    ..SAME
                },
                Difference {
                    design: true,
                    unidentified: true,
                    ..SAME
                },
                SAME,
            ],
            (Some(ChangedBy::Source), false),
        ),
        (
            "changed on both sides, differently: a conflict",
            |r| rename(r, BETA, "Beta S"),
            |r| rename(r, BETA, "Beta T"),
            Kind::Script,
            2,
            [DESIGN, DESIGN, DESIGN],
            (Some(ChangedBy::Both), true),
        ),
        (
            "changed on both sides, the same way",
            |r| rename(r, BETA, "Beta 2"),
            |r| rename(r, BETA, "Beta 2"),
            Kind::Script,
            2,
            [SAME, DESIGN, DESIGN],
            (Some(ChangedBy::Both), false),
        ),
        (
            "the same definition changed on both sides, a serial number taken on one",
            |r| set(r, &[TABLE, &[3], &[5], &[1], &[0x02]], &[1, 1]),
            |r| {
                set(r, &[TABLE, &[3], &[5], &[1], &[0x02]], &[1, 1]);
                set(r, &[TABLE, &[3], &[5], &[1], &[0x0c]], b"8");
            },
            Kind::Field,
            1,
            [
                DATA,
                DESIGN,
                Difference {
                    design: true,
                    data: true,
                    ..SAME
                },
            ],
            (Some(ChangedBy::Both), false),
        ),
    ];
    for &(
        case,
        change_source,
        change_target,
        kind,
        id,
        [difference, from_source, from_target],
        (changed_by, conflict),
    ) in rows
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
        assert_eq!(from.changed_by(), changed_by, "{case}");
        assert_eq!(m.conflict(), conflict, "{case}: conflict");
        // Without a base the same objects are matched by kind, id and
        // name, where the name agrees, with the same difference.
        let without = compare(&source, &target, None).unwrap();
        if m.source.name == m.target_name {
            assert_eq!(matched(&without, kind, id).difference, difference, "{case}");
        }
        assert!(without.matched.iter().all(|m| m.base.is_none()));
    }

    // Matched objects carry design revisions, and one only where the
    // object holds no step data and no unidentified piece: the library
    // item and the field, not a script.
    let same = compare(&base, &base, Some(&base)).unwrap();
    let item = matched(&same, Kind::LibraryItem, 1);
    assert!(item.source_revision.is_some());
    assert_eq!(item.source_revision, item.target_revision);
    assert!(matched(&same, Kind::Field, 1).source_revision.is_some());
    assert_eq!(matched(&same, Kind::Script, 1).source_revision, None);
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
            // The table's field catalog, and its field: their table is
            // not matched.
            (Kind::Field, None, None, Agreement::Id),
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
    // Every object matched with none is listed once, a possible match
    // or not.
    assert_eq!(
        only(&without.only_in_source),
        [
            (Kind::Table, Some(129), Presence::Only),
            (Kind::Script, Some(1), Presence::Only),
            (Kind::Script, Some(2), Presence::Only),
            (Kind::Field, None, Presence::Only),
            (Kind::Field, Some(1), Presence::Only),
        ]
    );
    assert_eq!(
        only(&without.only_in_target),
        [
            (Kind::Table, Some(129), Presence::Only),
            (Kind::Script, Some(2), Presence::Only),
            (Kind::Script, Some(4), Presence::Only),
            (Kind::Field, None, Presence::Only),
            (Kind::Field, Some(1), Presence::Only),
        ]
    );
    assert_eq!(matched(&without, Kind::Script, 3).difference, SAME);
    // No table, field or field catalog is matched; the tables' catalog,
    // which names no table, is.
    assert!(without.matched.iter().all(
        |m| m.source.kind != Kind::Field && (m.source.kind != Kind::Table || m.source.catalog)
    ));
    assert_eq!(serial::decode(TABLE), Some(129));
}

/// The scripts `names` as scripts 1, 2, ... of a file.
fn scripts(names: &[&str]) -> OwnedNode {
    let mut root = OwnedNode::default();
    for (i, name) in names.iter().enumerate() {
        root.put(&[&[0x11], &[5], &[i as u8 + 1]], script(name, b"pw"));
    }
    root
}

fn possible(c: &Comparison) -> Vec<(Option<u64>, Option<u64>, Agreement)> {
    c.possible_matches
        .iter()
        .map(|p| (p.source.id, p.target.id, p.agrees))
        .collect()
}

#[test]
fn candidates_by_id_and_by_name_are_both_listed_and_ambiguous_ids_refused() {
    // Agreement in id and in name, crossed: neither hides the other.
    let crossed = compare(
        &scripts(&["Alpha", "Beta"]),
        &scripts(&["Beta", "Alpha"]),
        None,
    );
    assert_eq!(
        possible(&crossed.unwrap()),
        [
            (Some(1), Some(1), Agreement::Id),
            (Some(2), Some(2), Agreement::Id),
            (Some(1), Some(2), Agreement::Name),
            (Some(2), Some(1), Agreement::Name),
        ]
    );
    // At most eight by name for one object; the rest counted.
    let mut many = vec!["Other"];
    many.extend(["Same"; 10]);
    let capped = compare(&scripts(&["Same"]), &scripts(&many), None).unwrap();
    let by_name = capped
        .possible_matches
        .iter()
        .filter(|p| p.agrees == Agreement::Name)
        .count();
    assert_eq!((by_name, capped.possible_matches_left_out), (8, 2));
    // With the object of the same key in the bucket: fields under a table
    // that is not matched, field 1 named alike on both sides. It is an id
    // candidate, not a name one, and is not counted among those left out.
    let fields = |table: &str, count: u8| {
        let mut root = OwnedNode::default();
        root.put(
            &[&[0x03], &[0x10], &[5], TABLE],
            list(&[], vec![leaf(&[0x10], &om_scsu::encode_scsu(table))]),
        );
        for id in 1..=count {
            root.put(
                &[TABLE, &[3], &[5], &[id]],
                list(&[], vec![leaf(&[0x10], &om_scsu::encode_scsu("Same"))]),
            );
        }
        root
    };
    let bucket = compare(&fields("Tasks", 1), &fields("Jobs", 11), None).unwrap();
    let names: Vec<(Option<u64>, Option<u64>)> = bucket
        .possible_matches
        .iter()
        .filter(|p| p.agrees == Agreement::Name)
        .map(|p| (p.source.id, p.target.id))
        .collect();
    assert_eq!(names.len(), 8);
    assert!(names.iter().all(|&(s, t)| s == Some(1) && t != Some(1)));
    assert_eq!(bucket.possible_matches_left_out, 2);
    assert!(bucket
        .possible_matches
        .iter()
        .any(|p| p.agrees == Agreement::Id && p.source.id == Some(1) && !p.source.catalog));
    // Objects with no name: one when kind, id and every class of content
    // agree, a possible match otherwise; and a file matches itself whole.
    let relationship = |definition: &[u8]| {
        let mut root = OwnedNode::default();
        root.put(
            &[&[0x03], &[0xfb], &[5], &[1]],
            list(&[], vec![leaf(&[0x02], definition)]),
        );
        root
    };
    let same = compare(&relationship(&[1]), &relationship(&[1]), None).unwrap();
    let members: Vec<&Matched> = same.matched.iter().filter(|m| !m.source.catalog).collect();
    assert_eq!(
        (
            members.len(),
            members[0].difference,
            same.possible_matches.len()
        ),
        (1, SAME, 0)
    );
    // A table with no name, matched by its content, makes its fields
    // eligible by the same test (finding 10 of #768's second review).
    let mut unnamed_table = OwnedNode::default();
    unnamed_table.put(
        &[&[0x03], &[0x10], &[5], TABLE],
        list(&[], vec![leaf(&[0x03], b"a comment")]),
    );
    unnamed_table.put(
        &[TABLE, &[3], &[5], &[1]],
        list(&[], vec![leaf(&[0x10], &om_scsu::encode_scsu("Count"))]),
    );
    let fields = compare(&unnamed_table, &unnamed_table, None).unwrap();
    assert!(fields.possible_matches.is_empty() && fields.only_in_source.is_empty());
    assert_eq!(matched(&fields, Kind::Field, 1).difference, SAME);
    let other = compare(&relationship(&[1]), &relationship(&[2]), None).unwrap();
    assert_eq!(possible(&other), [(Some(1), Some(1), Agreement::Id)]);
    let base = base();
    let itself = compare(&base, &base, None).unwrap();
    assert!(itself.possible_matches.is_empty() && itself.only_in_source.is_empty());
    assert!(itself.matched.iter().all(|m| m.difference.is_same()));
    // A name is reported where it is Design throughout: a name kept in
    // pieces (non-ASCII text, file-format.md 3.6), and one whose node
    // holds a node of its own under the name's rule. (Within a member the
    // inventory accepts, a name holds nothing of another class.)
    let mut pieces = scripts(&["Alpha", "Beta"]);
    pieces.put(
        &[&[0x11], &[5], &[1], &[0x10]],
        list(&[], vec![leaf(&[0], &[0, 0, 0, 2]), leaf(&[1], b"Ab")]),
    );
    pieces.put(&[&[0x11], &[5], &[2], &[0x10], &[0x7e]], leaf(&[], &[1]));
    let compared = compare(&pieces, &pieces, None).unwrap();
    for id in [1, 2] {
        let reported = &matched(&compared, Kind::Script, id).source.name;
        assert!(reported.is_some(), "script {id}");
    }
    // Identities are checked before anything is read: a font key of
    // another width than two bytes, a base directory's of other than four,
    // a key that is no serial key (finding 8 of #768's second review: a
    // token as a key), each refused with its kind and no id.
    let font = |key: &[u8]| {
        let mut root = OwnedNode::default();
        root.put(
            &[&[0x19], &[5], key],
            list(&[], vec![leaf(&[0x10], &om_scsu::encode_scsu("Helvetica"))]),
        );
        root
    };
    let refused = |kind, id, problem| CompareError::Identity {
        which: Which::Target,
        error: IdentityError {
            kind: Some(kind),
            table: None,
            id,
            problem,
        },
    };
    let shape = |kind| refused(kind, None, IdentityProblem::KeyShape);
    assert_eq!(
        compare(&base, &font(&[0x01]), None).unwrap_err(),
        shape(Kind::Font)
    );
    assert_eq!(
        compare(&base, &font(b"TOKEN99"), None).unwrap_err(),
        shape(Kind::Font)
    );
    let mut directory = OwnedNode::default();
    directory.put(
        &[&[0x26], &[5], &[0, 1]],
        list(&[], vec![leaf(&[0xf0], &[1])]),
    );
    assert_eq!(
        compare(&base, &directory, None).unwrap_err(),
        shape(Kind::BaseDirectory)
    );
    let mut token = base.clone();
    token.put(&[&[0x11], &[5], b"\0TOKEN99"], script("Token", b"pw"));
    let error = compare(&base, &token, None).unwrap_err();
    assert_eq!(error, shape(Kind::Script));
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("TOKEN"), "{text}");
    }
    // A member with no children is part of its catalog's object, whose
    // projection holds its key: every key under a member list is checked,
    // whoever's it is (finding 1 of #768's fourth review).
    let mut childless = base.clone();
    childless.put(
        &[&[0x11], &[5], b"TOKEN99-not-a-serial-key"],
        OwnedNode::default(),
    );
    let error = compare(&base, &childless, None).unwrap_err();
    assert_eq!(error, shape(Kind::Script));
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("TOKEN"), "{text}");
    }
    let mut one_byte = font(&[0x00, 0x01]);
    one_byte.put(&[&[0x19], &[5], &[0x07]], OwnedNode::default());
    assert_eq!(
        compare(&base, &one_byte, None).unwrap_err(),
        shape(Kind::Font)
    );
    // Keys out of order or repeated, which a file never holds: a member
    // twice, or two member lists, one of which would be enumerated and the
    // other skipped (finding 4 of #768's third review).
    let mut twice = font(&[0x00, 0x01]);
    let members = twice.at_mut(&[&[0x19], &[5]]).unwrap();
    let copy = members.children[0].clone();
    members.children.push(copy);
    assert_eq!(
        compare(&base, &twice, None).unwrap_err(),
        refused(Kind::Font, None, IdentityProblem::Unsorted)
    );
    let mut two_lists = base.clone();
    let scripts = two_lists.at_mut(&[&[0x11]]).unwrap();
    let list_again = scripts.child(&[5]).unwrap().clone();
    scripts.children.push(list_again);
    assert_eq!(
        compare(&base, &two_lists, None).unwrap_err(),
        refused(Kind::Script, None, IdentityProblem::Unsorted)
    );
    // Every input is checked before any is classified (finding 5): a
    // source deeper than a file can be is not reached when the target is
    // refused.
    let mut deep = base.clone();
    let mut path: Vec<&[u8]> = vec![&[0x11], &[5], &[2], &[0x10]];
    path.extend(std::iter::repeat_n(&[0x01][..], 70));
    deep.put(&path, leaf(&[], &[1]));
    assert_eq!(
        compare(&deep, &font(&[0x01]), None).unwrap_err(),
        shape(Kind::Font)
    );
    assert_eq!(
        compare(&deep, &base, None).unwrap_err(),
        CompareError::TooDeep {
            which: Which::Source
        }
    );
}

#[test]
fn an_error_names_no_key_and_a_missing_file_is_missing() {
    // A key path in a read error can hold any content: the comparison's
    // error keeps what failed, not where.
    let error = crate::Error::KeyOrder {
        path: "the-stored-password".into(),
    };
    let failed = CompareError::Read {
        which: Which::Base,
        failure: ReadFailure::of(&error),
    };
    for text in [failed.to_string(), format!("{failed:?}")] {
        assert!(!text.contains("password"), "{text}");
    }
    assert_eq!(
        failed,
        CompareError::Read {
            which: Which::Base,
            failure: ReadFailure::Damaged
        }
    );
    let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("no such file.fmp12");
    assert_eq!(
        compare_files(&missing, &missing, None).unwrap_err(),
        CompareError::Read {
            which: Which::Source,
            failure: ReadFailure::Missing
        }
    );
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
    // Accounts and the file's status: not kept either.
    root.put(&[&[0x17], &[1], &[5], &[1], &[0x10]], leaf(&[], b"Admin"));
    root.put(&[&[0x17], &[2], &[5], &[1], &[0x10]], leaf(&[], b"Full"));
    root.put(&[&[0x02], &[0x01]], leaf(&[], &[1]));
    let bytes = crate::writer::rebuild(&root, &crate::new_superblock("Test").unwrap()).unwrap();
    let file = HbamFile::from_bytes(bytes).unwrap();
    let design = design_tree(&file).unwrap();
    assert!(design.at(&[TABLE, &[5]]).is_none());
    assert!(design.at(&[TABLE, &[1]]).is_none());
    assert!(design.at(&[TABLE, &[0x0d]]).is_none());
    assert!(design.at(&[&[0xd0, 0x00, 0x01]]).is_none());
    assert!(design.at(&[&[0x17], &[1]]).is_none());
    assert!(design.at(&[&[0x02]]).is_none());
    assert_eq!(design.at(&[&[0x17], &[2]]), root.at(&[&[0x17], &[2]]));
    assert_eq!(design.at(&[TABLE, &[3]]), root.at(&[TABLE, &[3]]));
    assert_eq!(design.at(&[&[0x11]]), root.at(&[&[0x11]]));
    let comparison = compare(&design, &root, None).unwrap();
    assert!(!comparison.matched.is_empty());
    assert!(comparison.matched.iter().all(|m| m.difference.is_same()));
}
