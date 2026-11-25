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
//! what a helper does while it runs goes unchecked: when it starts, the
//! tree must be what the operation's writes made of it
//! ([`Check::before_store`]), and when it returns, the check takes the
//! tree as the helper left it ([`Check::after_store`]).

use om_hbam::OwnedNode;
use om_store::tree::{Edit, Node};

use std::sync::Arc;

use super::view::{TreeView, Version, View};
use crate::derived::Derived;
use crate::recalc::TableBasis;
use crate::store::{Bases, Journal, Store};

/// What a check keeps of one transaction.
pub(crate) struct Check {
    /// Whether the operation's tree writes are checked: an operation not
    /// moved onto the transaction's writes at all has none, and only its
    /// state of its own and its verified-index marks are
    /// (`engine::file::on_the_store`).
    tree: bool,
    /// The tree the operation started on with the writes applied in order,
    /// taken again as each store helper left it ([`Self::after_store`]).
    current: OwnedNode,
    version: Version,
    /// The writes, in order.
    edits: Vec<Edit>,
    /// The derived state the operation started with, its verified-index
    /// marks in it.
    start: Arc<Derived>,
    /// The bases of stored calculations the operation started with.
    start_bases: Arc<Bases>,
    /// Those bases with the basis writes recorded applied, taken again as
    /// each store helper left them.
    bases: Bases,
}

/// What a check keeps once the operation ended, to check a roll-back of
/// it ([`Settled::after_roll_back`]).
pub(crate) struct Settled {
    start: Arc<Derived>,
    start_bases: Arc<Bases>,
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
    /// A check of an operation on `store`; with `tree` false, of its state
    /// of its own and its marks alone.
    pub(crate) fn new(store: &Store, tree: bool) -> Self {
        Check {
            tree,
            current: if tree {
                store.tree().clone()
            } else {
                OwnedNode::default()
            },
            version: store.version(),
            edits: Vec::new(),
            start: store.derived().clone(),
            start_bases: store.bases().clone(),
            bases: (**store.bases()).clone(),
        }
    }

    /// Record a write of the version's own state: `table`'s basis set to
    /// `basis` (`None`: removed).
    pub(crate) fn wrote_basis(&mut self, table: &[u8], basis: Option<TableBasis>) {
        match basis {
            Some(basis) => self.bases.insert(table.to_vec(), basis),
            None => self.bases.remove(table),
        };
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
        for edit in &self.edits[first..] {
            apply(&mut self.current, edit);
        }
    }

    /// A store helper is about to change the tree: the tree must be what
    /// the operation's writes made of it, since the start or since the
    /// last helper returned.
    pub(crate) fn before_store(&self, store: &Store) {
        if self.tree {
            self.agrees(store, "before a store helper");
            self.bases_agree(store, "before a store helper");
        }
    }

    /// A store helper returned: what it changed is taken as it is, and the
    /// writes after it are checked from there.
    pub(crate) fn after_store(&mut self, store: &Store) {
        if self.tree {
            self.current = store.tree().clone();
            self.bases = (**store.bases()).clone();
        }
    }

    /// `ours`, the file engine's answer to a read, against the answer the
    /// starting tree with the writes applied gives (`read`).
    pub(crate) fn read<T: PartialEq>(
        &self,
        what: &str,
        path: &[&[u8]],
        ours: &T,
        read: impl FnOnce(&dyn View) -> T,
    ) {
        if !self.tree {
            return;
        }
        let theirs = read(&TreeView::new(&self.current, self.version));
        // Paths only: an account's node holds its password's verifier.
        assert!(
            *ours == theirs,
            "an operation's {what} of {path:02x?} differs from its starting tree with its writes"
        );
    }

    /// The tree the writes made is `store`'s tree.
    fn agrees(&self, store: &Store, when: &str) {
        if let Some(path) = first_difference(&self.current, store.tree()) {
            let written: Vec<&[Vec<u8>]> = self.edits.iter().map(edit_path).collect();
            panic!(
                "an operation changed the tree beyond its writes {when}, first at {path:02x?}; \
                 it wrote at {written:02x?}"
            );
        }
    }

    /// The bases the basis writes made are `store`'s bases.
    fn bases_agree(&self, store: &Store, when: &str) {
        if let Some(table) = first_different_basis(&self.bases, store.bases()) {
            panic!(
                "an operation changed the basis of table {table:02x?}'s stored calculations \
                 beyond its writes {when}"
            );
        }
    }

    /// At the end of the operation, `journal` its change: the starting tree
    /// changed by the writes is `store`'s tree, the derived state is what
    /// that tree decodes to, and a written tree counts as changed since its
    /// last save; the bases are those the basis writes made; whatever of
    /// the bases and the verified-index marks changed, the journal keeps
    /// as it was, to put back; and every mark that an index agrees holds
    /// for the tree.
    pub(crate) fn finish(self, store: &Store, journal: &Journal) -> Settled {
        if self.tree {
            self.agrees(store, "by its end");
            self.bases_agree(store, "by its end");
            if !self.edits.is_empty() {
                assert!(
                    store.has_unsaved_changes(),
                    "an operation wrote the tree, and it does not count as changed since its save"
                );
            }
        }
        if **store.bases() != *self.start_bases {
            assert!(
                journal
                    .kept_bases()
                    .is_some_and(|kept| **kept == *self.start_bases),
                "an operation changed the bases of stored calculations, and its journal does \
                 not keep them as they were"
            );
        }
        if store.derived().marks() != self.start.marks() {
            assert!(
                journal
                    .kept_schema()
                    .is_some_and(|kept| kept.marks() == self.start.marks()),
                "an operation changed the verified-index marks, and its journal does not keep \
                 them as they were"
            );
        }
        marks_hold(store, "by its end");
        if self.tree {
            derived_agrees(store);
        }
        Settled {
            start: self.start,
            start_bases: self.start_bases,
        }
    }
}

impl Settled {
    /// The operation's change was taken back: the bases are those it
    /// started with, and the verified-index marks those it started with,
    /// but for any whose index the save that failed took out
    /// (`Store::drop_unwritable_indexes`), every one that an index agrees
    /// holding.
    pub(crate) fn after_roll_back(&self, store: &Store) {
        if let Some(table) = first_different_basis(&self.start_bases, store.bases()) {
            panic!(
                "a roll-back left the basis of table {table:02x?}'s stored calculations other \
                 than the operation found it"
            );
        }
        let start = self.start.marks();
        for (id, agrees) in store.derived().marks() {
            assert!(
                start.get(id) == Some(agrees),
                "a roll-back left a verified-index mark of table {:02x?} field {:02x?} index \
                 {} the operation did not find",
                id.0,
                id.1,
                id.2
            );
        }
        marks_hold(store, "after a roll-back");
    }
}

/// The table of the first basis where `a` and `b` differ.
fn first_different_basis(a: &Bases, b: &Bases) -> Option<Vec<u8>> {
    a.keys()
        .chain(b.keys())
        .find(|table| a.get(*table) != b.get(*table))
        .cloned()
}

/// Every verified-index mark of `store` that an index agrees with its
/// records holds for its tree: a mark may be missing, never wrong.
fn marks_hold(store: &Store, when: &str) {
    for (id, agrees) in store.derived().marks() {
        if *agrees {
            assert!(
                store.index_holds(id),
                "a verified-index mark says index {} of table {:02x?} field {:02x?} agrees \
                 with its records {when}, and it does not",
                id.2,
                id.0,
                id.1
            );
        }
    }
}

/// The derived state of `store` is what its tree decodes to.
fn derived_agrees(store: &Store) {
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
        let store = crate::store::tests::store_of(start.clone());
        let mut check = Check::new(&store, true);
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
        assert_eq!(first_difference(&check.current, &expected), None);
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
    /// changed, the parent a put created or turned from an empty-valued
    /// leaf into a list included, so for that parent's other children too,
    /// and false elsewhere; and a write that changes nothing changes
    /// nothing (review of #785, rounds 1 and 2).
    #[test]
    fn a_transaction_answers_for_its_own_writes() {
        let (a, b, c, d): (&[u8], &[u8], &[u8], &[u8]) = (&[0x0a], &[0x0b], &[0x0c], &[0x0d]);
        let mut start = OwnedNode::default();
        start.put(&[c], leaf(b"same"));
        start.put(&[d], leaf(b""));
        let mut store = crate::store::tests::store_of(start);
        let answers = crate::engine::written(&mut store, |tx| {
            let since = tx.version();
            tx.put(path(&[c]), leaf(b"same")).unwrap();
            let unchanged = View::changed_since(tx, &[c], since).unwrap();
            // [A] is created, [D] turned into a list.
            tx.put(path(&[a, b]), leaf(b"x")).unwrap();
            tx.put(path(&[d, b]), leaf(b"y")).unwrap();
            [&[a][..], &[a, b], &[a, b, c], &[a, c], &[d], &[d, c], &[c]]
                .map(|at| View::changed_since(tx, at, since).unwrap())
                .into_iter()
                .chain([unchanged])
                .collect::<Vec<_>>()
        });
        assert_eq!(answers, [true, true, true, true, true, true, false, false]);
    }

    /// A change no write records, made after a store helper returned: the
    /// check takes the tree as the helper left it when the helper returns,
    /// so the change is caught at the next helper's start or the
    /// operation's end, whatever the operation writes in between (review
    /// of #785, round 2).
    #[test]
    fn a_change_after_a_store_helper_returned_is_caught() {
        let table: &[u8] = &[0x80, 0x01];
        for second_helper in [false, true] {
            let mut store = crate::store::tests::store_of(OwnedNode::default());
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::engine::written(&mut store, |tx| {
                    tx.drop_emptied_library(table);
                    // Not through the transaction: unrecorded.
                    assert!(tx.store.put_subtree(&path(&[&[0x0e]]), leaf(b"unrecorded")));
                    if second_helper {
                        tx.drop_emptied_library(table);
                    }
                    tx.put(path(&[&[0x0f]]), leaf(b"recorded")).unwrap();
                })
            }));
            let message = caught.expect_err("the change is caught");
            let message = message
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            let when = if second_helper {
                "before a store helper"
            } else {
                "by its end"
            };
            assert!(message.contains(when), "{message}");
        }
    }
}
