//! The path groups and revisions of `om_hbam::design_paths`, which
//! `om-author`'s revisions are computed through, on the synthetic Tasks
//! file the authoring tests host (`common::form_tree`): which changes move
//! which object's revision, and that a file written and read back keeps
//! them all.

mod common;

use std::collections::BTreeMap;

use common::fixture;
use om_hbam::design_paths::{self, group_of, Group, Kind, Revision};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, HbamFile, OwnedNode};

type Id = (Kind, Option<Vec<u8>>, Vec<u8>);

fn revisions(root: &OwnedNode) -> BTreeMap<Id, Revision> {
    design_paths::objects(root)
        .iter()
        .map(|o| ((o.kind, o.table.clone(), o.key.clone()), o.revision()))
        .collect()
}

/// The objects whose revision differs between `a` and `b`, or that only
/// one has.
fn changed(a: &BTreeMap<Id, Revision>, b: &BTreeMap<Id, Revision>) -> Vec<Id> {
    let mut out: Vec<Id> = a
        .iter()
        .filter(|(id, revision)| b.get(*id) != Some(revision))
        .map(|(id, _)| id.clone())
        .collect();
    out.extend(b.keys().filter(|id| !a.contains_key(*id)).cloned());
    out
}

fn field_path(field: u64) -> [Vec<u8>; 4] {
    [
        uint_segment(fixture::TABLE),
        vec![3],
        vec![5],
        uint_segment(field),
    ]
}

fn at(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// The Tasks file with its Form layout's objects, Count made an
/// auto-enter serial field with its next value `0c` and data version `fb`
/// (schema-path.md 5.5, write-path.md 3.2).
fn tree() -> OwnedNode {
    let mut root = common::form_tree();
    let count = field_path(fixture::COUNT);
    let mut put = |key: u8, node: OwnedNode| {
        let mut path = count.to_vec();
        path.push(vec![key]);
        root.put(&at(&path), node);
    };
    put(0x0c, om_scripts::text_node(&[], "1"));
    put(0xfb, OwnedNode::leaf(Vec::new(), vec![1, 1]));
    root
}

/// Give every modification stamp in `node` another value.
fn restamp(node: &mut OwnedNode) {
    for child in &mut node.children {
        if design_paths::is_stamp(&child.segment) {
            child.value = Some(vec![9; 3]);
            child.children.clear();
        } else {
            restamp(child);
        }
    }
}

#[test]
fn stamps_serial_numbers_and_accounts_change_no_revision() {
    let root = tree();
    let before = revisions(&root);
    assert!(before.keys().any(|(kind, ..)| *kind == Kind::Script));
    assert!(before.keys().any(|(kind, ..)| *kind == Kind::Layout));
    assert!(before.keys().any(|(kind, ..)| *kind == Kind::Field));

    let mut restamped = root.clone();
    restamp(&mut restamped);
    assert_eq!(changed(&before, &revisions(&restamped)), Vec::<Id>::new());

    // op72 advances the serial's next value.
    let mut served = root.clone();
    let mut next = field_path(fixture::COUNT).to_vec();
    next.push(vec![0x0c]);
    assert_eq!(group_of(&at(&next)), Group::Data);
    served.put(&at(&next), om_scripts::text_node(&[], "2"));
    assert_eq!(changed(&before, &revisions(&served)), Vec::<Id>::new());

    // Clerk's account renamed, and its password changed.
    let mut accounts = root.clone();
    let clerk: [&[u8]; 4] = [&[0x17], &[1], &[5], &[3]];
    assert_eq!(group_of(&clerk), Group::Access);
    let account = accounts.at_mut(&clerk).expect("Clerk's account");
    account.set_child(om_scripts::text_node(&[0x10], "Clerk 2"));
    account.set_child(OwnedNode::leaf(vec![0x06], vec![7; 32]));
    assert_eq!(changed(&before, &revisions(&accounts)), Vec::<Id>::new());
}

#[test]
fn a_change_moves_its_own_objects_revision_and_no_other() {
    let root = tree();
    let before = revisions(&root);

    let mut renamed = root.clone();
    let name = field_path(fixture::NAME);
    renamed
        .at_mut(&at(&name))
        .expect("the Name field")
        .set_child(om_scripts::text_node(&[0x10], "Title"));
    assert_eq!(
        changed(&before, &revisions(&renamed)),
        vec![(
            Kind::Field,
            Some(uint_segment(fixture::TABLE)),
            uint_segment(fixture::NAME)
        )]
    );

    // A new object on the Form layout: its attribute record and the
    // object store grown by its message.
    let mut placed = root.clone();
    let layout = uint_segment(fixture::LAYOUT);
    let member = placed
        .at_mut(&[&[4], &[5], &layout])
        .expect("the Form layout");
    let key = serial::encode(4);
    member.put(
        &[&[5], &key, &[0xd8]],
        OwnedNode::leaf(Vec::new(), vec![0x44; 16]),
    );
    let store = member.child_mut(&[7]).expect("the object store");
    store
        .value
        .as_mut()
        .expect("a stored value")
        .extend([0x12, 0x02, 0x08, 0x04]);
    assert_eq!(
        changed(&before, &revisions(&placed)),
        vec![(Kind::Layout, None, layout)]
    );
}

#[test]
fn a_file_written_and_read_back_keeps_every_revision() {
    let root = tree();
    let before = revisions(&root);
    let superblock = om_hbam::new_superblock("Tasks").expect("a superblock");
    let bytes = om_hbam::writer::rebuild(&root, &superblock).expect("a file");
    let file = HbamFile::from_bytes(bytes).expect("the file read back");
    let reread = file.key_tree().expect("its keys").root.to_owned_node();
    assert_eq!(revisions(&reread), before);
    // The report sees the keys the fixture adds that FileMaker does not
    // define, and nothing else outside the rules.
    let report = design_paths::report(&reread);
    let shapes: Vec<&str> = report.unclassified.keys().map(String::as_str).collect();
    assert_eq!(
        shapes,
        ["[04, 05, *, 77]", "[11, 05, *, 77]", "[T, 03, 01, 77]"],
        "{report}"
    );
}
