//! A check of the writes an operation makes through its transaction, for
//! tests only (debug builds), as `super::copies` checks views
//! (docs/server-storage.md 5, "The operations' transaction"). The engine
//! switched to it (`Server::checking_reads`) gives each transaction a
//! [`Check`]: the tree the operation started on, and the writes it made, as
//! the store engine's entry holds them (`om_store::tree::Edit`). Every read
//! the operation makes through the transaction is answered again from the
//! starting tree with the writes laid over it (`super::Overlay`) and must
//! be the file engine's answer; at the end, the starting tree changed by
//! the writes must be the tree the file engine holds, node for node and
//! kind for kind, the derived state must be what the tree decodes to, and
//! the tree must count as changed since its last save when it was written.
//! A difference panics, failing the test that made it. That is the proof
//! that an operation's writes say everything it changed.
//!
//! An operation that changes the tree through the store's own helpers
//! (one not moved onto the transaction) is not checked
//! ([`Check::by_store`]).

use std::fmt::Debug;

use om_hbam::OwnedNode;
use om_store::tree::{Edit, Node};

use super::overlay::{Cover, Overlay};
use super::view::{TreeView, Version, View};
use crate::store::Store;

/// What a check keeps of one transaction.
pub(crate) struct Check {
    /// The tree the operation started on.
    start: OwnedNode,
    version: Version,
    /// The writes, in order.
    edits: Vec<Edit>,
    /// What the writes laid over the starting tree, as covers that do not
    /// overlap ([`Self::lay`]).
    covers: Vec<Cover>,
    /// The operation changed the tree through the store's own helpers, so
    /// its writes do not say all it changed: nothing more is checked.
    pub(crate) by_store: bool,
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
            start: store.tree().clone(),
            version: store.version(),
            edits: Vec::new(),
            covers: Vec::new(),
            by_store: false,
        }
    }

    /// Record a write: `node` (with its children) put at `path`, or the
    /// node there removed. A put is recorded as the store engine's entry
    /// holds it: the node, then each node under it, in key order.
    pub(crate) fn wrote(&mut self, path: &[Vec<u8>], node: Option<&OwnedNode>) {
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
        self.lay(path, node.cloned());
    }

    /// Lay a write over the starting tree: covers stay apart, a write at or
    /// below a cover changes what lies over it, and one above covers takes
    /// their place.
    fn lay(&mut self, path: &[Vec<u8>], node: Option<OwnedNode>) {
        let node = node.map(|node| OwnedNode {
            segment: path.last().cloned().unwrap_or_default(),
            ..node
        });
        if let Some((covered, laid)) = self
            .covers
            .iter_mut()
            .find(|(covered, _)| path.starts_with(covered))
        {
            let rest = segments(&path[covered.len()..]);
            match (rest.is_empty(), node) {
                (true, node) => *laid = node,
                (false, Some(node)) => {
                    let segment = covered.last().cloned().unwrap_or_default();
                    laid.get_or_insert_with(|| OwnedNode::list(segment, Vec::new()))
                        .put(&rest, node);
                }
                (false, None) => {
                    if let Some(laid) = laid {
                        laid.remove(&rest);
                    }
                }
            }
            return;
        }
        self.covers
            .retain(|(covered, _)| !covered.starts_with(path));
        self.covers.push((path.to_vec(), node));
    }

    /// `ours`, the file engine's answer to a read, against the answer the
    /// starting tree with the writes laid over it gives (`read`).
    pub(crate) fn read<T: PartialEq + Debug>(
        &self,
        what: &str,
        path: &[&[u8]],
        ours: &T,
        read: impl FnOnce(&dyn View) -> T,
    ) {
        if self.by_store {
            return;
        }
        let start = TreeView::new(&self.start, self.version);
        let theirs = read(&Overlay::new(&start, &self.covers));
        assert!(
            *ours == theirs,
            "an operation's {what} of {path:02x?} differs from its starting tree with its writes:\n\
             file engine: {ours:?}\nstarting tree with the writes: {theirs:?}"
        );
    }

    /// At the end of the operation: the starting tree changed by the
    /// writes is `store`'s tree, the derived state is what that tree
    /// decodes to, and a written tree counts as changed since its last
    /// save.
    pub(crate) fn finish(self, store: &Store) {
        if self.by_store {
            return;
        }
        let mut tree = self.start;
        for edit in &self.edits {
            apply(&mut tree, edit);
        }
        if let Some(path) = first_difference(&tree, store.tree()) {
            panic!(
                "an operation changed the tree beyond its writes, first at {path:02x?}: its \
                 writes were {:?}",
                self.edits
            );
        }
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

    /// Covers laid by writes at, below and above earlier ones answer as the
    /// tree the writes make, and an edit's node is a node of the file
    /// engine's tree again.
    #[test]
    fn laid_writes_answer_as_the_tree_they_make() {
        let mut start = OwnedNode::default();
        start.put(&[&[1], &[1]], leaf(b"a"));
        start.put(&[&[1], &[2]], leaf(b""));
        start.put(&[&[2]], leaf(b"two"));
        let mut check = Check {
            start: start.clone(),
            version: 1,
            edits: Vec::new(),
            covers: Vec::new(),
            by_store: false,
        };
        let path =
            |segments: &[&[u8]]| -> Vec<Vec<u8>> { segments.iter().map(|s| s.to_vec()).collect() };
        let mut subtree = OwnedNode::list(Vec::new(), vec![leaf(b"x"), leaf(b"y")]);
        subtree.children[0].segment = vec![1];
        subtree.children[1].segment = vec![2];
        let writes: Vec<(Vec<Vec<u8>>, Option<OwnedNode>)> = vec![
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
        let mut expected = start.clone();
        for (at, node) in &writes {
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
        let mut applied = start;
        for edit in &check.edits {
            apply(&mut applied, edit);
        }
        assert_eq!(first_difference(&applied, &expected), None, "the edits");
        let tree = TreeView::new(&expected, 1);
        let base = TreeView::new(&check.start, 1);
        let overlay = Overlay::new(&base, &check.covers);
        assert_eq!(
            crate::engine::view::wire(&overlay, &[]).unwrap(),
            crate::engine::view::wire(&tree, &[]).unwrap(),
            "the covers"
        );
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
}
