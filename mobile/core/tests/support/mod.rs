//! A disposable, two-record file with an editable field, a Next button
//! and a script.
#[path = "../../../../crates/om-server/tests/fixtures/tasks.rs"]
mod tasks;

use om_hbam::{serial, slot::uint_segment, OwnedNode};
use om_layout::{encode, Rect};
use std::path::Path;

pub fn fixture(path: &Path) {
    // A script, so that the file has the catalog the Scripts menu reads.
    let mut tree = tasks::tree(&[tasks::script(1, "Start", Vec::new())], true);
    tasks::put_account(&mut tree, 2, "", None, 2);
    let alias = serial::decode(&uint_segment(tasks::OCCURRENCE)).unwrap();
    let box_at = |top| Rect {
        left: 24.,
        top,
        right: 264.,
        bottom: top + 44.,
        ..Rect::default()
    };
    let name = encode::field_object(1, &box_at(24.), alias, tasks::NAME);
    let next = encode::button_object(2, &box_at(92.), 16, 4, 0, &[0; 14]);
    let put = |tree: &mut OwnedNode, keys: &[&[u8]], bytes| {
        tree.put(keys, OwnedNode::leaf(Vec::new(), bytes));
    };
    put(
        &mut tree,
        &[&[4], &[5], &[1], &[2]],
        encode::new_layout_def(alias, 400.),
    );
    put(
        &mut tree,
        &[&[4], &[5], &[1], &[3], &[1], &[2]],
        encode::new_body_part(240.),
    );
    put(
        &mut tree,
        &[&[4], &[5], &[1], &[7]],
        encode::append_to_store(&[], &[1, 2], &[name, next]),
    );
    put(
        &mut tree,
        &[&[4], &[5], &[1], &[5], &[2], &[2], &[6]],
        tasks::calculated_calc("\"Next task\"").polish.unwrap(),
    );
    tasks::index_layouts(&mut tree);
    tasks::index_layout_definitions(&mut tree);
    tasks::write_tree(path, &tree);
}
