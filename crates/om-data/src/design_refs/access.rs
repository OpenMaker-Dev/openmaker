//! The one way the design graph reaches a file's catalogs.
//!
//! The builder holds the key tree only inside a [`Layer`], and readers reach
//! nodes only through the [`At`] handles it gives: the node in a handle is
//! private to this module, and no reader function takes a `KeyTree` or a
//! `Node`. Every accessor marks what it gives, on the node itself:
//!
//! - a value taken ([`At::bytes`], [`At::text`], [`At::uint`]) or a place
//!   declared to name nothing ([`At::declare`], with its reason from one
//!   table, [`Declared`]): the node and everything under it is accounted
//!   for;
//! - a list whose keys were looked at ([`At::get`], [`At::children`]): each
//!   key under it must be accounted for in turn;
//! - a value present but not readable in the form asked for: failed. The
//!   accessor answers [`Read::Unreadable`], which has no conversion to an
//!   absent value, and the failure stays on the node.
//!
//! At the end [`Layer::sweep`] walks every catalog the build was given and
//! reports, by its path, each stored node nothing accounted for, each
//! failed one, and each that holds a value beside keys. A reader that stops
//! early, returns early or does not know a key leaves nodes unmarked, and
//! they are reported without the reader having to remember to.
//!
//! Decoders that take a node ([`At::decode`]) get it unmarked: whatever the
//! reader relies on from them it marks through the accessors, so trusting a
//! decoder can only leave a node reported, never hide one.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use om_hbam::slot::be_uint;
use om_hbam::{serial, KeySegment, KeyTree, Node};

use super::read::Builder;
use super::{Kind, ObjectId, Place, UnreadReason};

/// What an accessor found at a place: absent, read, or present and not
/// readable. The third state is recorded on the node when it is met; there
/// is deliberately no way to turn it into an absent value.
#[must_use]
#[derive(Debug)]
pub(super) enum Read<T> {
    Absent,
    Ok(T),
    Unreadable,
}

/// Places a reader accounts for without reading them, because the spec
/// says what they hold and that it names no design object. One table: each
/// declaration's reason is [`Declared::spec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Declared {
    /// A catalog's index `01`: names, display order and counters derived
    /// from its members.
    CatalogIndex,
    /// A member's owner, UUIDs and tags `d7`-`da` and its modification
    /// stamps `fc`, `fc01`-`fc03`.
    Stamp,
    /// A comment, a description or a value list's custom values: text.
    Text,
    /// `fb`: a table's or field's data version, or an occurrence's list of
    /// the occurrences relationships join it to.
    DataVersion,
    /// A field's next serial value, auto-entered data, custom message and
    /// range bounds.
    FieldSetting,
    /// A custom function's parameter count, parameter names and flag.
    FunctionSignature,
    /// A privilege set's access settings and checksum, and a table's or
    /// field's access within it: settings on objects, not uses of them.
    AccessSetting,
    /// A layout's indexes of its objects' names `08`, positions `0a` and
    /// tab order `15`.
    LayoutIndex,
    /// A layout's theme `0b`, member and view flags `f0` `f1`, a part's
    /// list of its objects `03, P, 07`, and an object's tab order `07`
    /// and data format `0d`.
    LayoutSetting,
    /// A formula's formatted stream `05`: the text it shows; the compiled
    /// stream `06` holds what runs.
    FormattedFormula,
    /// `[2]` besides the document preferences `18`: version, creation,
    /// history, locale, icons and page setup.
    FileStatus,
    /// A data source's file paths and options.
    DataSourcePath,
    /// A trigger cache's count of triggers `04`.
    TriggerCount,
    /// A step's data, read through the step's parameter schema: what the
    /// schema does not explain is reported by the step's reader.
    StepData,
    /// A formula stored for an option that is off (an auto-enter
    /// calculation of a field that does not auto-enter one, a tooltip
    /// without its flag): kept by FileMaker, never evaluated.
    Inactive,
    /// A deleted member: FileMaker empties it, keeping only its key.
    Deleted,
    /// A layout folder, folder end or separator (member flags `01`, `02`,
    /// `10`): not a layout and never shown, so what its definition and
    /// settings name is not used.
    LayoutFolder,
    /// A place its reader reported unread itself, with what it could name.
    Reported,
}

impl Declared {
    /// Where the specs say what the place holds.
    pub(super) fn spec(self) -> &'static str {
        match self {
            Declared::CatalogIndex => "schema-path.md 5.1",
            Declared::Stamp => "script-steps.md 4.3; schema-path.md 8.1",
            Declared::Text => "schema-path.md 5, 5.5; calc-functions.md 6.0; file-metadata.md 6",
            Declared::DataVersion => "schema-path.md 5.3, 5.8, 5.10; file-metadata.md 4.2",
            Declared::FieldSetting => "schema-path.md 5.5, 5.13 (om_data::field_options)",
            Declared::FunctionSignature => "calc-encoding.md 8; calc-functions.md 6.0",
            Declared::AccessSetting => "session-identity.md 2, 4.1",
            Declared::LayoutIndex => "layout-model.md 10.5",
            Declared::LayoutSetting => "layout-model.md 4.1, 5.5, 7.5, 10.5, 10.6",
            Declared::FormattedFormula => "calc-encoding.md 1, 2",
            Declared::FileStatus => "file-format.md (catalog 2); file-metadata.md 0, 2",
            Declared::DataSourcePath => "file-metadata.md 1.1",
            Declared::TriggerCount => "file-metadata.md 7.1",
            Declared::StepData => "script-steps.md 4, 5 (om_scripts::schema)",
            Declared::Inactive => "schema-path.md 5.5; layout-model.md 5.1, 5.3.4",
            Declared::Deleted => "scripts.md (a deleted member is emptied)",
            Declared::LayoutFolder => "file-metadata.md 1.1; layout-model.md 10.5, 10.6",
            Declared::Reported => "reported unread by its reader",
        }
    }
}

/// What a node's marks say: its keys were looked at, it was accounted for
/// whole, or it was present and not readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Listed,
    Taken,
    Failed(&'static str),
}

/// The catalogs a build reads, with what has been read of them.
pub(super) struct Layer<'t, 'a> {
    tree: &'t KeyTree<'a>,
    marks: RefCell<HashMap<usize, Mark>>,
}

/// A stored node, reached through a [`Layer`].
#[derive(Clone, Copy)]
pub(super) struct At<'t, 'a> {
    node: &'t Node<'a>,
    layer: &'t Layer<'t, 'a>,
}

fn address(node: &Node<'_>) -> usize {
    node as *const Node<'_> as usize
}

impl<'t, 'a> Layer<'t, 'a> {
    pub(super) fn new(tree: &'t KeyTree<'a>) -> Layer<'t, 'a> {
        Layer {
            tree,
            marks: RefCell::new(HashMap::new()),
        }
    }

    /// The node at `path` from the root, each node on the way looked at.
    pub(super) fn at(&'t self, path: &[u64]) -> Option<At<'t, 'a>> {
        let mut at = At {
            node: &self.tree.root,
            layer: self,
        };
        for key in path {
            at = at.get(*key)?;
        }
        Some(at)
    }

    fn mark(&self, node: &Node<'_>, mark: Mark) {
        let mut marks = self.marks.borrow_mut();
        let slot = marks.entry(address(node)).or_insert(mark);
        // A failure stays; being taken whole outranks being looked at.
        *slot = match (*slot, mark) {
            (Mark::Failed(why), _) => Mark::Failed(why),
            (_, Mark::Failed(why)) => Mark::Failed(why),
            (Mark::Taken, _) | (_, Mark::Taken) => Mark::Taken,
            _ => Mark::Listed,
        };
    }

    fn mark_of(&self, node: &Node<'_>) -> Option<Mark> {
        self.marks.borrow().get(&address(node)).copied()
    }

    /// The places the sweep covers: the design catalogs given and each
    /// table's field catalog `[T, 3]`.
    fn roots(&self) -> Vec<(Vec<Vec<u8>>, &'t Node<'a>)> {
        let mut roots = Vec::new();
        for (path, _) in super::read::DESIGN_CATALOGS {
            let mut node = &self.tree.root;
            let mut segments = Vec::new();
            let mut found = true;
            for key in path {
                match node.get(*key) {
                    Some(child) => {
                        segments.push(child.segment.0.to_vec());
                        node = child;
                    }
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            if found {
                roots.push((segments, node));
            }
        }
        for catalog in &self.tree.root.children {
            // Table catalogs are keyed by table ids from 128 up, two bytes
            // or more; only their field catalogs are design.
            if catalog.segment.0.len() > 1 {
                if let Some(fields) = catalog.get(3) {
                    roots.push((
                        vec![catalog.segment.0.to_vec(), fields.segment.0.to_vec()],
                        fields,
                    ));
                }
            }
        }
        roots
    }

    /// Report every stored node of the catalogs given that no accessor
    /// accounted for, each failed one and each holding a value beside keys,
    /// at its path. One walk, charged to the work budget; a budget spent
    /// here stops the build like any other.
    pub(super) fn sweep(&self, b: &mut Builder) {
        for (path, root) in self.roots() {
            let mut stack: Vec<(&Node<'a>, usize, bool)> = vec![(root, path.len() - 1, false)];
            let mut at: Vec<Vec<u8>> = path[..path.len() - 1].to_vec();
            while let Some((node, depth, inside_taken)) = stack.pop() {
                at.truncate(depth);
                at.push(node.segment.0.to_vec());
                if !b.spend(
                    1,
                    ObjectId::File,
                    &Place::new("the check of every stored key"),
                ) {
                    return;
                }
                // Even a key with no value is reported when nothing took
                // it: a key's segment can itself name an object (a
                // layout's field index holds only keys).
                let holds_value = node.value.is_some_and(|v| !v.is_empty());
                if holds_value && !node.children.is_empty() {
                    report(
                        b,
                        &at,
                        "a value stored beside keys",
                        UnreadReason::Malformed,
                    );
                    continue;
                }
                let mark = self.mark_of(node);
                let taken = inside_taken || mark == Some(Mark::Taken);
                match mark {
                    Some(Mark::Failed(why)) => {
                        report(b, &at, why, UnreadReason::Malformed);
                        continue;
                    }
                    None if !inside_taken => {
                        report(
                            b,
                            &at,
                            "a stored key no reader took, which may hold anything",
                            UnreadReason::NotDecoded,
                        );
                        continue;
                    }
                    _ => {}
                }
                for child in node.children.iter().rev() {
                    stack.push((child, depth + 1, taken));
                }
            }
        }
    }
}

/// One finding of the sweep, at `path`.
fn report(b: &mut Builder, path: &[Vec<u8>], why: &str, reason: UnreadReason) {
    let shown: Vec<String> = path
        .iter()
        .map(|s| KeySegment(s.as_slice()).to_string())
        .collect();
    b.unread(
        holder_of(path),
        Place::new(format!("stored {}", shown.join("/"))),
        reason,
        why,
        &Kind::ANY,
    );
}

/// The object a stored node belongs to, from its path.
pub(super) fn holder_of(path: &[Vec<u8>]) -> ObjectId {
    let seg = |i: usize| path.get(i).map(Vec::as_slice);
    let uint = |i: usize| seg(i).and_then(be_uint);
    let serial_at = |i: usize| seg(i).and_then(serial::decode);
    let one = |i: usize, value: u8| seg(i) == Some(&[value][..]);
    let member = |catalog: &[u8]| -> bool {
        catalog.iter().enumerate().all(|(i, value)| one(i, *value))
            && one(catalog.len(), 5)
            && path.len() > catalog.len() + 1
    };
    let id = |n: Option<u64>, f: fn(u64) -> ObjectId| n.map_or(ObjectId::File, f);
    if member(&[3, 0x10]) {
        return id(uint(3), ObjectId::Table);
    }
    if member(&[3, 0x11]) {
        return id(uint(3), ObjectId::Occurrence);
    }
    if member(&[3, 0xfb]) {
        return id(uint(3), ObjectId::Relationship);
    }
    if member(&[4]) {
        if let (Some(layout), true, Some(object)) = (uint(2), one(3, 5), serial_at(4)) {
            if path.len() > 4 && object != 0 {
                return ObjectId::LayoutObject { layout, object };
            }
        }
        return id(uint(2), ObjectId::Layout);
    }
    if member(&[0x10]) {
        return id(uint(2), ObjectId::CustomFunction);
    }
    if member(&[0x11]) {
        return id(serial_at(2), ObjectId::Script);
    }
    if member(&[0x17, 2]) {
        return id(uint(3), ObjectId::PrivilegeSet);
    }
    if member(&[0x20]) {
        return id(serial_at(2), ObjectId::DataSource);
    }
    if member(&[0x21]) {
        return id(uint(2), ObjectId::ValueList);
    }
    if member(&[0x41]) {
        return id(uint(2), ObjectId::CustomMenu);
    }
    // A table's field catalog `[T, 3]`.
    if seg(0).is_some_and(|s| s.len() > 1) && one(1, 3) {
        if let (Some(table), true, Some(field)) = (uint(0), one(2, 5), uint(3)) {
            return ObjectId::Field { table, field };
        }
        return id(uint(0), ObjectId::Table);
    }
    ObjectId::File
}

impl<'t, 'a> At<'t, 'a> {
    fn wrap(self, node: &'t Node<'a>) -> At<'t, 'a> {
        At {
            node,
            layer: self.layer,
        }
    }

    fn fail(self, why: &'static str) {
        self.layer.mark(self.node, Mark::Failed(why));
    }

    pub(super) fn segment(self) -> KeySegment<'a> {
        self.node.segment
    }

    pub(super) fn id(self) -> Option<u64> {
        self.node.id()
    }

    /// Open this node as a list: looked at, or failed when it holds a value
    /// (where keys were expected, or beside them).
    fn open(self) -> bool {
        if self.node.value.is_some_and(|v| !v.is_empty()) {
            self.fail(if self.node.children.is_empty() {
                "a value stored where keys were expected"
            } else {
                "a value stored beside keys"
            });
            return false;
        }
        self.layer.mark(self.node, Mark::Listed);
        true
    }

    /// The child `key`. A node holding a value has no keys to give: it is
    /// failed, and every child absent.
    pub(super) fn get(self, key: u64) -> Option<At<'t, 'a>> {
        if !self.open() {
            return None;
        }
        self.node.get(key).map(|n| self.wrap(n))
    }

    /// The child with exactly these segment bytes.
    pub(super) fn child(self, segment: &[u8]) -> Option<At<'t, 'a>> {
        if !self.open() {
            return None;
        }
        self.node.child(segment).map(|n| self.wrap(n))
    }

    /// Every child, in key order.
    pub(super) fn children(self) -> Read<Vec<At<'t, 'a>>> {
        if !self.open() {
            return Read::Unreadable;
        }
        Read::Ok(self.node.children.iter().map(|n| self.wrap(n)).collect())
    }

    /// Whether the node holds nothing: no value (or an empty one) and no
    /// keys. Marks nothing.
    pub(super) fn is_empty(self) -> bool {
        self.node.value.is_none_or(<[u8]>::is_empty) && self.node.children.is_empty()
    }

    /// The node's value, assembled from its pieces when stored in pieces.
    /// An empty list reads as an empty value.
    pub(super) fn bytes(self) -> Read<Cow<'a, [u8]>> {
        let node = self.node;
        match (node.value, node.children.is_empty()) {
            (Some(value), true) => {
                self.layer.mark(node, Mark::Taken);
                Read::Ok(Cow::Borrowed(value))
            }
            (Some([]), false) => match node.assembled() {
                Some(bytes) if !bytes.is_empty() => {
                    self.layer.mark(node, Mark::Taken);
                    Read::Ok(bytes)
                }
                _ => {
                    self.fail("a value stored in pieces that do not assemble");
                    Read::Unreadable
                }
            },
            (Some(_), false) => {
                self.fail("a value stored beside keys");
                Read::Unreadable
            }
            (None, true) => {
                self.layer.mark(node, Mark::Taken);
                Read::Ok(Cow::Borrowed(&[]))
            }
            (None, false) => match node.assembled() {
                Some(bytes) => {
                    self.layer.mark(node, Mark::Taken);
                    Read::Ok(bytes)
                }
                None => {
                    self.fail("a value stored in pieces that do not assemble");
                    Read::Unreadable
                }
            },
        }
    }

    /// [`Self::bytes`] of the child `key`: absent when there is none.
    pub(super) fn bytes_at(self, key: u64) -> Read<Cow<'a, [u8]>> {
        self.get(key).map_or(Read::Absent, At::bytes)
    }

    /// [`Self::text`] of the child `key`: absent when there is none.
    pub(super) fn text_at(self, key: u64) -> Read<String> {
        self.get(key).map_or(Read::Absent, At::text)
    }

    /// [`Self::uint`] of the child `key`: absent when there is none.
    pub(super) fn uint_at(self, key: u64) -> Read<u64> {
        self.get(key).map_or(Read::Absent, At::uint)
    }

    /// The value as FileMaker text (file-format.md 3.5).
    pub(super) fn text(self) -> Read<String> {
        match self.bytes() {
            Read::Ok(bytes) => match om_scsu::decode_scsu(&bytes) {
                Ok(text) => Read::Ok(text),
                Err(_) => {
                    self.fail("text that does not decode");
                    Read::Unreadable
                }
            },
            Read::Absent => Read::Absent,
            Read::Unreadable => Read::Unreadable,
        }
    }

    /// The value as a big-endian unsigned integer of 1 to 8 bytes.
    pub(super) fn uint(self) -> Read<u64> {
        match self.bytes() {
            Read::Ok(bytes) => match be_uint(&bytes) {
                Some(value) => Read::Ok(value),
                None => {
                    self.fail("a number of the wrong length");
                    Read::Unreadable
                }
            },
            Read::Absent => Read::Absent,
            Read::Unreadable => Read::Unreadable,
        }
    }

    /// Whether the node is exactly the leaf `expected`, the one form every
    /// observation shows. Accounted for when it is; failed when not.
    pub(super) fn observed(self, expected: &[u8]) -> bool {
        let exact = self.node.children.is_empty() && self.node.value == Some(expected);
        if exact {
            self.layer.mark(self.node, Mark::Taken);
        } else {
            self.fail("a key whose meaning is not specified, not in its observed form");
        }
        exact
    }

    /// [`Self::observed`] for a key observed in any one of `forms`.
    pub(super) fn observed_any(self, forms: &[&[u8]]) -> bool {
        let exact = self.node.children.is_empty()
            && forms.iter().any(|form| self.node.value == Some(*form));
        if exact {
            self.layer.mark(self.node, Mark::Taken);
        } else {
            self.fail("a key whose meaning is not specified, not in its observed form");
        }
        exact
    }

    /// Whether the node is a counter in its observed form, the key string
    /// of the last key issued under its list: a length byte, then a serial
    /// key of that length. Accounted for when it is; failed when not.
    pub(super) fn counter(self) -> bool {
        let key = self.node.children.is_empty()
            && self.node.value.is_some_and(|value| {
                value.split_first().is_some_and(|(len, key)| {
                    usize::from(*len) == key.len() && serial::decode(key).is_some()
                })
            });
        if key {
            self.layer.mark(self.node, Mark::Taken);
        } else {
            self.fail("a counter not in its observed form (a key string)");
        }
        key
    }

    /// Account for the node and everything under it, for the reason the
    /// spec gives (`why`). Its shape is still checked by the sweep.
    pub(super) fn declare(self, why: Declared) {
        let _ = why.spec();
        self.layer.mark(self.node, Mark::Taken);
    }

    /// Hand the node to a decoder that reads it by itself. Nothing is
    /// marked: the reader marks what it relies on through the accessors.
    pub(super) fn decode<T>(self, decoder: impl FnOnce(&Node<'a>) -> T) -> T {
        decoder(self.node)
    }

    /// Hand the node to a decoder that reads a whole tree, as the only
    /// member of the catalog at `catalog` (`[4]` and `5` for a layout), so
    /// that the decoder reads nothing outside the node (no themes, fonts
    /// or images elsewhere in the file). Nothing is marked.
    pub(super) fn decode_alone<T>(
        self,
        catalog: &'static [u8],
        decoder: impl FnOnce(&KeyTree<'a>) -> T,
    ) -> T {
        let list = |segment: &'static [u8], children: Vec<Node<'a>>| Node {
            segment: KeySegment(segment),
            value: None,
            push: true,
            children,
        };
        let tree = KeyTree {
            root: list(
                &[],
                vec![list(catalog, vec![list(&[5], vec![self.node.clone()])])],
            ),
        };
        decoder(&tree)
    }

    /// The bytes and nodes stored under the node, counted only up to `cap`:
    /// `None` past it, so that sizing costs no more than the budget left.
    pub(super) fn size_within(self, cap: usize) -> Option<usize> {
        let mut total = 0usize;
        let mut stack = vec![self.node];
        while let Some(node) = stack.pop() {
            total = total.saturating_add(1 + node.value.map_or(0, <[u8]>::len));
            if total > cap {
                return None;
            }
            stack.extend(node.children.iter());
        }
        Some(total)
    }
}
