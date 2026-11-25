//! A check of the writes an operation makes through its transaction, for
//! tests only (debug builds), as `super::copies` checks views
//! (docs/server-storage.md 5, "The operations' transaction"). The engine
//! switched to it (`Server::checking_reads`) gives each transaction a
//! [`Check`]: the tree the operation started on, to which the writes it
//! makes are applied in order, as the store engine's entry holds them
//! (`om_store::tree::Edit`). Every read the operation makes through the
//! transaction is answered again from that tree and must be the file
//! engine's answer; at the end, that tree must be the tree the file engine
//! holds, node for node and kind for kind, the derived state must be what
//! the tree decodes to, and the tree must count as changed since its last
//! save when it was written. A difference panics, failing the test that
//! made it. That is the proof that an operation's writes say everything it
//! changed.
//!
//! An operation not moved onto the transaction changes the tree through
//! the store's own helpers, which say nothing of what they change. Only
//! those changes go unchecked ([`Check::before_store`]): what the
//! operation wrote before a helper must be in the tree when the helper
//! starts, and the check then takes the tree as the helper left it before
//! the next read or write it checks ([`Check::fresh`]).

use std::cell::{Cell, RefCell};

use om_hbam::OwnedNode;
use om_store::tree::{Edit, Node};

use super::view::{TreeView, Version, View};
use crate::store::Store;

/// What a check keeps of one transaction.
pub(crate) struct Check {
    /// The tree the operation started on with the writes applied in order,
    /// or the tree as a store helper left it ([`Self::fresh`]).
    current: RefCell<OwnedNode>,
    version: Version,
    /// The writes, in order.
    edits: Vec<Edit>,
    /// A store helper changed the tree since `current` was last taken from
    /// it ([`Self::before_store`]).
    stale: Cell<bool>,
}

/// `node` as a node of the store engine's edits, without its children.
pub(crate) fn edit_node(node: &OwnedNode) -> Node {
    match (node.push || !node.children.is_empty(), &node.value) {
        (false, Some(value)) => Node::Value(value.clone()),
        (true, Some(value)) => Node::Both(value.clone()),
        (_, None) => Node::List,
    }
}

/// An edit's node as a node of the file engine's tree, named `segment`.
fn owned(segment: Vec<u8>, node: &Node) -> OwnedNode {
    match node {
        Node::List => OwnedNode::list(segment, Vec::new()),
        Node::Value(value) => OwnedNode::leaf(segment, value.clone()),
        Node::Both(value) => OwnedNode {
            segment,
            value: Some(value.clone()),
            push: true,
            children: Vec::new(),
        },
    }
}

fn segments(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// `edit` applied to `root` as the file engine's tree applies it
/// (`OwnedNode::put`, `OwnedNode::remove`).
fn apply(root: &mut OwnedNode, edit: &Edit) {
    match edit {
        Edit::Put { path, node } => {
            let segment = path.last().cloned().unwrap_or_default();
            root.put(&segments(path), owned(segment, node));
        }
        Edit::Delete { path } => {
            root.remove(&segments(path));
        }
    }
}

impl Check {
    pub(crate) fn new(store: &Store) -> Self {
        Check {
            current: RefCell::new(store.tree().clone()),
            version: store.version(),
            edits: Vec::new(),
            stale: Cell::new(false),
        }
    }

    /// Record a write the file engine made: `node` (with its children) put
    /// at `path`, or the node there removed. A put is recorded as the store
    /// engine's entry holds it, the node, then each node under it in key
    /// order, and each edit is applied to the tree in that order. A write
    /// that changed nothing is not one (`Store::write_in`).
    pub(crate) fn wrote(&mut self, path: &[Vec<u8>], node: Option<&OwnedNode>) {
        let first = self.edits.len();
        match node {
            None => self.edits.push(Edit::Delete {
                path: path.to_vec(),
            }),
            Some(node) => {
                let mut stack = vec![(path.to_vec(), node)];
                while let Some((at, node)) = stack.pop() {
                    self.edits.push(Edit::Put {
                        path: at.clone(),
                        node: edit_node(node),
                    });
                    for child in node.children.iter().rev() {
                        let mut below = at.clone();
                        below.push(child.segment.clone());
                        stack.push((below, child));
                    }
                }
            }
        }
        let current = self.current.get_mut();
        for edit in &self.edits[first..] {
            apply(current, edit);
        }
    }

    /// A store helper is about to change the tree: what the operation wrote
    /// so far must be in it, and the check takes the tree as the helper
    /// leaves it before the next read or write it checks.
    pub(crate) fn before_store(&mut self, store: &Store) {
        if self.stale.get() {
            return;
        }
        self.agrees(store, "before a store helper");
        self.stale.set(true);
    }

    /// The tree as a store helper left it, when one changed it since the
    /// check last took the tree: called before a checked read or write.
    pub(crate) fn fresh(&self, store: &Store) {
        if self.stale.replace(false) {
            *self.current.borrow_mut() = store.tree().clone();
        }
    }

    /// `ours`, the file engine's answer to a read, against the answer the
    /// starting tree with the writes applied gives (`read`).
    pub(crate) fn read<T: PartialEq>(
        &self,
        store: &Store,
        what: &str,
        path: &[&[u8]],
        ours: &T,
        read: impl FnOnce(&dyn View) -> T,
    ) {
        self.fresh(store);
        let current = self.current.borrow();
        let theirs = read(&TreeView::new(&current, self.version));
        // Paths only: an account's node holds its password's verifier.
        assert!(
            *ours == theirs,
            "an operation's {what} of {path:02x?} differs from its starting tree with its writes"
        );
    }

    /// The tree the writes made is `store`'s tree.
    fn agrees(&self, store: &Store, when: &str) {
        if let Some(path) = first_difference(&self.current.borrow(), store.tree()) {
            let written: Vec<&[Vec<u8>]> = self.edits.iter().map(edit_path).collect();
            panic!(
                "an operation changed the tree beyond its writes {when}, first at {path:02x?}; \
                 it wrote at {written:02x?}"
            );
        }
    }

    /// At the end of the operation: the starting tree changed by the
    /// writes is `store`'s tree, the derived state is what that tree
    /// decodes to, and a written tree counts as changed since its last
    /// save.
    pub(crate) fn finish(self, store: &Store) {
        self.fresh(store);
        self.agrees(store, "by its end");
        if !self.edits.is_empty() {
            assert!(
                store.has_unsaved_changes(),
                "an operation wrote the tree, and it does not count as changed since its save"
            );
        }
        let view = store.tree_view();
        let Ok(catalog) = crate::derived::read_catalog(&view) else {
            // The store keeps the schema it last read (`Store::reread_catalog`).
            return;
        };
        let derived = store.derived();
        let decoded = derived.with_catalog(&view, catalog).map(|decoded| {
            decoded.with_accounts(store.version(), crate::derived::read_accounts(&view))
        });
        if let Ok(decoded) = decoded {
            assert!(
                decoded.same_as(derived, true),
                "an operation's writes left the derived state behind its tree:\n{derived:?}\n\
                 decoded again: {decoded:?}"
            );
        }
    }
}

fn edit_path(edit: &Edit) -> &[Vec<u8>] {
    match edit {
        Edit::Put { path, .. } | Edit::Delete { path } => path,
    }
}

/// The path of the first node where `a` and `b` differ, in a walk of both,
/// or `None` when they are equal node for node and kind for kind.
fn first_difference(a: &OwnedNode, b: &OwnedNode) -> Option<Vec<Vec<u8>>> {
    let mut stack: Vec<(Vec<Vec<u8>>, &OwnedNode, &OwnedNode)> = vec![(Vec::new(), a, b)];
    while let Some((path, a, b)) = stack.pop() {
        if a.segment != b.segment
            || a.value != b.value
            || a.push != b.push
            || a.children.len() != b.children.len()
        {
            return Some(path);
        }
        for (a, b) in a.children.iter().zip(&b.children).rev() {
            if a.segment != b.segment {
                return Some(path);
            }
            let mut below = path.clone();
            below.push(a.segment.clone());
            stack.push((below, a, b));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(value: &[u8]) -> OwnedNode {
        OwnedNode::leaf(Vec::new(), value.to_vec())
    }

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|s| s.to_vec()).collect()
    }

    /// A check of `writes` from `start`: the tree they make, as the file
    /// engine's tree applies them, and the check that recorded them.
    fn checked(
        start: &OwnedNode,
        writes: &[(Vec<Vec<u8>>, Option<OwnedNode>)],
    ) -> (OwnedNode, Check) {
        let mut check = Check {
            current: RefCell::new(start.clone()),
            version: 1,
            edits: Vec::new(),
            stale: Cell::new(false),
        };
        let mut expected = start.clone();
        for (at, node) in writes {
            check.wrote(at, node.as_ref());
            match node {
                Some(node) => {
                    let mut node = node.clone();
                    node.segment = at.last().unwrap().clone();
                    expected.put(&segments(at), node);
                }
                None => {
                    expected.remove(&segments(at));
                }
            }
        }
        (expected, check)
    }

    /// The writes replayed in order answer as the tree they make, read by
    /// read and whole, and an edit's node is a node of the file engine's
    /// tree again.
    #[test]
    fn replayed_writes_answer_as_the_tree_they_make() {
        let mut start = OwnedNode::default();
        start.put(&[&[1], &[1]], leaf(b"a"));
        start.put(&[&[1], &[2]], leaf(b""));
        start.put(&[&[2]], leaf(b"two"));
        let mut subtree = OwnedNode::list(Vec::new(), vec![leaf(b"x"), leaf(b"y")]);
        subtree.children[0].segment = vec![1];
        subtree.children[1].segment = vec![2];
        let writes = [
            (
                path(&[&[1], &[2], &[9]]),
                Some(leaf(b"under an empty value")),
            ),
            (path(&[&[1]]), Some(subtree)),
            (path(&[&[1], &[1]]), None),
            (path(&[&[3], &[1], &[1]]), Some(leaf(b"deep"))),
            (path(&[&[3]]), None),
            (path(&[&[3], &[2]]), Some(leaf(b"after a removal"))),
            (path(&[&[2]]), Some(OwnedNode::list(Vec::new(), Vec::new()))),
        ];
        let (expected, check) = checked(&start, &writes);
        assert_eq!(first_difference(&check.current.borrow(), &expected), None);
        let mut applied = start;
        for edit in &check.edits {
            apply(&mut applied, edit);
        }
        assert_eq!(first_difference(&applied, &expected), None, "the edits");
        for node in [
            leaf(b"v"),
            leaf(b""),
            OwnedNode::list(Vec::new(), Vec::new()),
        ] {
            let mut both = node.clone();
            both.push = true;
            for node in [node, both] {
                assert_eq!(owned(Vec::new(), &edit_node(&node)), node);
            }
        }
    }

    /// A put that creates its parent, or turns a parent that holds an
    /// empty value into a list, then the removal of what it put: the parent
    /// stays, an empty list, in the file engine's tree, and the check reads
    /// it there too, by node and in a listing (review of #785, round 1).
    #[test]
    fn a_parent_a_put_made_stays_after_its_child_is_removed() {
        let a: &[u8] = &[0x0a];
        let b: &[u8] = &[0x0b];
        let mut with_leaf = OwnedNode::default();
        with_leaf.put(&[a], leaf(b""));
        for start in [OwnedNode::default(), with_leaf] {
            let mut store = crate::store::tests::store_of(start);
            let (parent, listed) = crate::engine::written(&mut store, |tx| {
                tx.put(path(&[a, b]), leaf(b"x")).unwrap();
                tx.remove(path(&[a, b])).unwrap();
                let parent = View::node(tx, &[a]).unwrap().map(|head| head.into_owned());
                (parent, crate::engine::view::wire(tx, &[]).unwrap())
            });
            let parent = parent.expect("the parent stays");
            assert!(parent.list && parent.value.is_none());
            assert_eq!(
                listed,
                crate::engine::view::wire(&store.tree_view(), &[]).unwrap()
            );
        }
    }

    /// What a transaction answers about its own writes: `changed_since`
    /// at the version it started on is true at, above and below a path it
    /// changed, false elsewhere, and a write that changes nothing changes
    /// nothing (review of #785, round 1).
    #[test]
    fn a_transaction_answers_for_its_own_writes() {
        let (a, b, c): (&[u8], &[u8], &[u8]) = (&[0x0a], &[0x0b], &[0x0c]);
        let mut start = OwnedNode::default();
        start.put(&[c], leaf(b"same"));
        let mut store = crate::store::tests::store_of(start);
        let answers = crate::engine::written(&mut store, |tx| {
            let since = tx.version();
            tx.put(path(&[c]), leaf(b"same")).unwrap();
            let unchanged = View::changed_since(tx, &[c], since).unwrap();
            tx.put(path(&[a, b]), leaf(b"x")).unwrap();
            [&[a][..], &[a, b], &[a, b, c], &[c]]
                .map(|at| View::changed_since(tx, at, since).unwrap())
                .into_iter()
                .chain([unchanged])
                .collect::<Vec<_>>()
        });
        assert_eq!(answers, [true, true, true, false, false]);
    }
}
