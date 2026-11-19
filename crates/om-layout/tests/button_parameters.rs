//! A button's descriptor and parameter data are separate catalog members.
use om_hbam::{KeySegment, KeyTree, Node, OwnedNode};
use om_layout::Layout;

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = n as u8 & 0x7f;
        n >>= 7;
        out.push(byte | if n != 0 { 0x80 } else { 0 });
        if n == 0 {
            return out;
        }
    }
}
fn message(field: u64, bytes: &[u8]) -> Vec<u8> {
    [
        varint(field * 8 + 2),
        varint(bytes.len() as u64),
        bytes.to_vec(),
    ]
    .concat()
}
fn borrowed(node: &OwnedNode) -> Node<'_> {
    Node {
        segment: KeySegment(&node.segment),
        value: node.value.as_deref(),
        push: node.push,
        children: node.children.iter().map(borrowed).collect(),
    }
}

#[test]
fn buttons_and_groups_hydrate_their_own_complete_parameter_subtrees() {
    let mut root = OwnedNode::default();
    let mut store = Vec::new();
    // Include a serial object id above 127 and an object with no parameters.
    for (id, group, parameters) in [(300, false, true), (42, true, true), (3, false, false)] {
        let descriptor = [8, 6, 16, 4, 24, 2]; // Go to Layout, by number, with calculations.
        let payload = message(if group { 1 } else { 3 }, &descriptor);
        let object = [
            vec![8, if group { 8 } else { 10 }, 16],
            varint(id),
            message(if group { 12 } else { 16 }, &payload),
        ]
        .concat();
        store.extend(message(2, &object));
        if parameters {
            root.put(
                &[&[4], &[5], &[1], &[5], &om_hbam::serial::encode(id), &[17]],
                OwnedNode::list(
                    [],
                    vec![
                        OwnedNode::list(
                            [0, 5],
                            vec![OwnedNode::list(
                                [5],
                                vec![OwnedNode::leaf([6], [id as u8, 1, 2])],
                            )],
                        ),
                        OwnedNode::leaf([0, 6], 2u32.to_be_bytes()),
                        OwnedNode::leaf([0x8c], [9, 8, 7]),
                    ],
                ),
            );
        }
    }
    root.put(&[&[4], &[5], &[1], &[7]], OwnedNode::leaf([], store));
    let tree = KeyTree {
        root: borrowed(&root),
    };
    let layout = Layout::decode_from_tree(&tree, 1).unwrap().unwrap();
    for id in [300, 42] {
        let step = layout.objects.get(id).unwrap().script_step().unwrap();
        assert_eq!((step.id, step.value, step.flags), (6, 4, 2));
        assert_eq!(
            step.data.as_ref(),
            root.at(&[&[4], &[5], &[1], &[5], &om_hbam::serial::encode(id), &[17]])
        );
    }
    assert!(layout
        .objects
        .get(3)
        .unwrap()
        .script_step()
        .unwrap()
        .data
        .is_none());
}
