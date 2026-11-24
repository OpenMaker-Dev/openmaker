//! Which paths of a file are Design, Data, Access, Secret or Derived, the
//! file's design objects, and their two hashes (`docs/server-storage.md`
//! sections 4 and 9; `docs/authoring.md`, "Revisions").
//!
//! **Groups.** [`group_in`] puts a path of a tree in a [`Group`] by the
//! path rules of [`rules()`], one table, each rule with the spec section
//! that says what its paths hold, and by the step rules
//! ([`STEP_SECRETS`]), which find the credentials a script step or a
//! button's step stores. The grouping is by path, not by catalog member: a
//! serial field's next value is a key inside its field definition and is
//! Data (write-path.md 3.2). A key no spec names is matched by no rule and
//! is in no group, as is the file as a whole (`[2]`); that is never an
//! error. There is no grouping by path alone in the API: which slot of a
//! step's data holds a password depends on the step, which only the tree
//! shows, and a path-only answer of Design for a stored password could be
//! taken as final. The rules reach the first-level keys of members and
//! catalog indexes; below a rule everything is that rule's, so what a
//! value or a step's data holds beyond the rules is not seen (a password
//! typed into a calculation is formula text, Design).
//!
//! **Objects.** [`objects`] lists a tree's design objects: every member
//! `[cat, 5, id]` of a Design catalog, decoded by OpenMaker or not, and
//! each Design catalog as a whole. A member object is its member and its
//! entries in the catalog's index, where an index keeps one per member of
//! what a tool shows of it (a script's cached name and full-access flag,
//! scripts.md). A catalog object is the rest of its index: the display
//! order and the catalog's settings are Design; name lists, digests,
//! counts and dependency lists are Derived (they follow from the members,
//! and no tool shows them); the last issued id is Data. A field is an
//! object of its own; its table's field catalog index `[T, 3, 1]` is the
//! field catalog's object. A table is its member `[3, 10, 5, T]`; its
//! fields and its data under `[T]` are not part of it. An emptied member
//! (a deletion, scripts.md) is no object.
//!
//! **Hashes** ([`revision`]). An object's [`Object::edit_token`] covers
//! its whole member, stored passwords included, without modification
//! stamps: what authoring exchanges to guard an edit. Its
//! [`Object::design_revision`] covers its Design content only (Data,
//! Access and Derived left out, a Secret value marked), for what is shown
//! or kept; it never decides an edit. Whether an object changed is decided
//! on the full content and reported with the design revisions.

mod report;
mod revision;
mod rules;
mod secrets;

pub use report::{report, Member, MemberIs, Report};
pub use revision::{
    catalog_revision, is_stamp, legacy_catalog_revision, legacy_script_form_revision,
    parse_revision, revision_text, script_form_revision, split_value, DesignRevision, Revision,
    MOD_ACCOUNT, MOD_COUNT, MOD_TIME, MOD_USER,
};
pub use rules::{
    hex, is_odbc_profile, markdown_secrets, markdown_table, pattern_text, rules, Catalog, Group,
    Key, Rule, Seg, StepSecret, CATALOGS, COMMON_INDEX, COMMON_MEMBER, OCCURRENCE_BASE,
    STEP_SECRETS, TABLE_BASE,
};

use revision::{design_digest, Form, Mark, Marks};
use rules::group_of;

use crate::tree::OwnedNode;

/// A path as owned segments.
pub type Path = Vec<Vec<u8>>;

/// A catalog in a tree: its table (for a catalog under a table), its path
/// and its node.
type Instance<'t> = (Option<Vec<u8>>, Path, &'t OwnedNode);

/// The kinds of design object, one per Design catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Table,
    Occurrence,
    Relationship,
    Field,
    Layout,
    Theme,
    CustomFunction,
    Script,
    PrivilegeSet,
    ExtendedPrivilege,
    Font,
    LibraryItem,
    DataSource,
    ValueList,
    BaseDirectory,
    CustomMenuSet,
    CustomMenu,
}

impl Kind {
    /// The kind's name, as `om-author` names references where it has one.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::Occurrence => "occurrence",
            Kind::Relationship => "relationship",
            Kind::Field => "field",
            Kind::Layout => "layout",
            Kind::Theme => "theme",
            Kind::CustomFunction => "custom_function",
            Kind::Script => "script",
            Kind::PrivilegeSet => "privilege_set",
            Kind::ExtendedPrivilege => "extended_privilege",
            Kind::Font => "font",
            Kind::LibraryItem => "library_item",
            Kind::DataSource => "data_source",
            Kind::ValueList => "value_list",
            Kind::BaseDirectory => "base_directory",
            Kind::CustomMenuSet => "custom_menu_set",
            Kind::CustomMenu => "custom_menu",
        }
    }

    /// The catalog of this kind's members.
    pub fn catalog(self) -> &'static Catalog {
        Catalog::of(self)
    }
}

/// A design object: a member of a Design catalog, or the catalog as a
/// whole (its index).
#[derive(Clone, Debug)]
pub struct Object<'t> {
    /// The kind of the catalog's members.
    pub kind: Kind,
    /// The catalog as a whole, not one member.
    pub catalog: bool,
    /// The member's key segment: its id within its catalog (and, for a
    /// field, its table); empty for a catalog. Deploys match objects by
    /// kind and key (`docs/server-storage.md` section 9).
    pub key: Vec<u8>,
    /// A field's (or the field catalog's) table's key.
    pub table: Option<Vec<u8>>,
    /// The member's name when it has one that decodes: `10`, a theme's
    /// display name `0a` (layout-model.md 7.5); a catalog's name.
    pub name: Option<String>,
    /// The member's or the index's path.
    pub path: Path,
    pub node: &'t OwnedNode,
    /// A member's entries in its catalog's index (a script's
    /// `[11, 1, 07, S]`), with their paths: part of the object.
    pub entries: Vec<(Path, &'t OwnedNode)>,
}

/// One part of an object's design content: its path, the node without
/// its Data, Access and Derived paths, each Secret path emptied (no value,
/// no children) or, for Secret parts of a value, with those parts removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub path: Path,
    pub node: OwnedNode,
}

/// A part's projection, with what the design revision needs.
struct Projection {
    node: OwnedNode,
    marks: Marks,
    excluded: Vec<(Path, Group)>,
}

impl Object<'_> {
    /// The id FileMaker shows for it: the key read as a serial key
    /// (file-metadata.md 1), except a base directory's four-byte key
    /// (containers.md 8.1) and a font's two-byte one (layout-model.md 8),
    /// which are big-endian numbers. `om-author`'s references number
    /// tables, occurrences and fields by the key's big-endian bytes.
    pub fn id(&self) -> Option<u64> {
        match self.kind {
            _ if self.catalog => None,
            Kind::BaseDirectory | Kind::Font => crate::slot::be_uint(&self.key),
            _ => crate::serial::decode(&self.key),
        }
    }

    /// The edit token: the whole member (or index), stored passwords
    /// included, without modification stamps; the script form for a
    /// script. What authoring exchanges as `revision` and
    /// `expected_revision`, and what decides whether the object changed.
    pub fn edit_token(&self) -> Revision {
        edit_token(if self.catalog { None } else { Some(self.kind) }, self.node)
    }

    fn form(&self) -> Form {
        match self.kind {
            Kind::Script if !self.catalog => Form::Script,
            _ => Form::General,
        }
    }

    fn projections(&self) -> Vec<(Path, Projection)> {
        let secrets = if self.catalog {
            Vec::new()
        } else {
            secrets::of(self.kind, self.node)
        };
        let marks: Marks = secrets.into_iter().map(|s| (s.path, s.mark)).collect();
        let mut out = vec![(
            self.path.clone(),
            project(&self.path, self.node, marks, self.catalog),
        )];
        for (path, node) in &self.entries {
            out.push((path.clone(), project(path, node, Vec::new(), false)));
        }
        out
    }

    /// The object's design content, part by part, the member (or index)
    /// first: what its design revision covers, and what a deploy takes
    /// from a source.
    pub fn design(&self) -> Vec<Part> {
        self.projections()
            .into_iter()
            .map(|(path, p)| Part { path, node: p.node })
            .collect()
    }

    /// The paths of the object's parts that its design content leaves out
    /// or empties, with their group: Data, Access, Derived and Secret,
    /// which a deploy keeps from its target or recomputes. Paths are full.
    pub fn excluded(&self) -> Vec<(Path, Group)> {
        let mut out = Vec::new();
        for (path, p) in self.projections() {
            for (below, group) in p.excluded {
                let mut full = path.clone();
                full.extend(below);
                out.push((full, group));
            }
        }
        out
    }

    /// The member's Secret paths, relative to it, with what each holds.
    pub fn secrets(&self) -> Vec<(Path, &'static str)> {
        if self.catalog {
            return Vec::new();
        }
        secrets::of(self.kind, self.node)
            .into_iter()
            .map(|s| (s.path, s.holds))
            .collect()
    }

    /// The shapes ([`Report`]) of the node's paths that no rule covers: a
    /// member holding one is not deployed over an existing one
    /// (`docs/server-storage.md` section 9). Content below a rule (a
    /// value, a step's data) is that rule's, so an empty list does not
    /// mean the rules understand every byte.
    pub fn unclassified(&self) -> Vec<String> {
        let mut report = Report::default();
        let segments: Vec<&[u8]> = self.path.iter().map(Vec::as_slice).collect();
        report::count_under(self.node, &segments, &mut report);
        report.unclassified.into_keys().collect()
    }

    /// The design revision: never an edit token.
    pub fn design_revision(&self) -> DesignRevision {
        let form = self.form();
        let parts: Vec<(Path, OwnedNode, Marks, Form)> = self
            .projections()
            .into_iter()
            .enumerate()
            .map(|(i, (path, p))| {
                let form = if i == 0 { form } else { Form::General };
                (path, p.node, p.marks, form)
            })
            .collect();
        design_digest(&parts)
    }
}

/// The edit token of a member of `kind` (`None`: a catalog's index or
/// any other subtree): [`script_form_revision`] for a script,
/// [`catalog_revision`] otherwise.
pub fn edit_token(kind: Option<Kind>, node: &OwnedNode) -> Revision {
    match kind {
        Some(Kind::Script) => script_form_revision(node),
        _ => catalog_revision(node),
    }
}

/// The tables' data keys: the top-level keys of the table form.
fn tables(root: &OwnedNode) -> impl Iterator<Item = &OwnedNode> {
    root.children
        .iter()
        .filter(|c| Seg::Table.matches(&c.segment))
}

/// Every catalog `catalog` stands for in `root`: one, or one per table.
fn instances<'t>(root: &'t OwnedNode, catalog: &'static Catalog) -> Vec<Instance<'t>> {
    let tables: Vec<Option<Vec<u8>>> = if catalog.per_table() {
        tables(root).map(|t| Some(t.segment.clone())).collect()
    } else {
        vec![None]
    };
    tables
        .into_iter()
        .filter_map(|table| {
            let path = catalog.at(table.as_deref());
            let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            let node = root.at(&segments)?;
            Some((table, path, node))
        })
        .collect()
}

/// The index keys of `catalog` whose entries belong to its members.
fn entry_keys(catalog: &Catalog) -> Vec<&'static [u8]> {
    catalog
        .index
        .iter()
        .filter(|k| k.entries)
        .filter_map(|k| match k.key {
            Seg::Key(key) => Some(key),
            _ => None,
        })
        .collect()
}

/// A tree's design objects, catalog by catalog in [`CATALOGS`] order: the
/// catalog as a whole when it has an index, then its members in key order.
pub fn objects(root: &OwnedNode) -> Vec<Object<'_>> {
    let mut out = Vec::new();
    for catalog in CATALOGS {
        let Some(kind) = catalog.kind else { continue };
        let entry_keys = entry_keys(catalog);
        for (table, path, node) in instances(root, catalog) {
            let index = node.child(&[0x01]);
            if let Some(index) = index {
                let mut at = path.clone();
                at.push(vec![0x01]);
                out.push(Object {
                    kind,
                    catalog: true,
                    key: Vec::new(),
                    table: table.clone(),
                    name: Some(catalog.name.into()),
                    path: at,
                    node: index,
                    entries: Vec::new(),
                });
            }
            let Some(members) = node.child(&[0x05]) else {
                continue;
            };
            for member in members.children.iter().filter(|m| !m.children.is_empty()) {
                let mut at = path.clone();
                at.push(vec![0x05]);
                at.push(member.segment.clone());
                let entries = entry_keys
                    .iter()
                    .filter_map(|key| {
                        let entry = index?.child(key)?.child(&member.segment)?;
                        let mut entry_path = path.clone();
                        entry_path.extend([vec![0x01], key.to_vec(), member.segment.clone()]);
                        Some((entry_path, entry))
                    })
                    .collect();
                out.push(Object {
                    kind,
                    catalog: false,
                    key: member.segment.clone(),
                    table: table.clone(),
                    name: name(kind, member),
                    path: at,
                    node: member,
                    entries,
                });
            }
        }
    }
    out
}

/// A member's name as text (XOR 0x5A masked SCSU, file-format.md 3.5):
/// `10`, or a theme's display name `0a`.
fn name(kind: Kind, member: &OwnedNode) -> Option<String> {
    let key = match kind {
        Kind::Theme => 0x0a,
        _ => 0x10,
    };
    let value = member.child(&[key])?.assembled()?;
    om_scsu::decode_scsu(&value).ok()
}

/// The member of a kind with step data that `path` lies in: its kind and
/// how many segments its path has.
fn member_of(path: &[&[u8]]) -> Option<(Kind, usize)> {
    match path {
        [[0x11], [0x05], _, ..] => Some((Kind::Script, 3)),
        [[0x04], [0x05], _, ..] => Some((Kind::Layout, 3)),
        _ => None,
    }
}

/// The group of `path` in the tree `root`. Secret where a step rule finds
/// a credential at or above it, and for any slot of step data whose step
/// cannot be identified, or a path in step data under a member the tree
/// does not hold; otherwise its path rule's.
pub fn group_in(root: &OwnedNode, path: &[&[u8]]) -> Group {
    if let Some((kind, depth)) = member_of(path) {
        let below = &path[depth..];
        match root.at(&path[..depth]) {
            Some(member) => {
                if secrets::is_secret(kind, member, below) {
                    return Group::Secret;
                }
            }
            None => {
                let empty = OwnedNode::default();
                if secrets::is_secret(kind, &empty, below) {
                    return Group::Secret;
                }
            }
        }
    }
    group_of(path)
}

/// The rules that may still match below `path`, and the longest that
/// matches it.
fn candidates(path: &[&[u8]]) -> (Vec<&'static Rule>, Option<&'static Rule>) {
    let mut cands: Vec<&'static Rule> = rules().iter().collect();
    let mut best = None;
    for (depth, segment) in path.iter().enumerate() {
        (cands, best) = step(&cands, depth, segment, best);
    }
    (cands, best)
}

/// One segment further: of `cands` (rules longer than `depth` that match
/// the path so far), those that match `segment` too and are longer, and
/// the longest rule now matching.
fn step(
    cands: &[&'static Rule],
    depth: usize,
    segment: &[u8],
    best: Option<&'static Rule>,
) -> (Vec<&'static Rule>, Option<&'static Rule>) {
    let mut deeper = Vec::new();
    let mut best = best;
    let mut here = None;
    for rule in cands {
        if !rule.pattern[depth].matches(segment) {
            continue;
        }
        if rule.pattern.len() == depth + 1 {
            here.get_or_insert(*rule);
        } else {
            deeper.push(*rule);
        }
    }
    if here.is_some() {
        best = here;
    }
    (deeper, best)
}

/// Whether a path of `marks` lies strictly below `at`.
fn marked_below(marks: &Marks, at: &[Vec<u8>]) -> bool {
    marks
        .iter()
        .any(|(s, _)| s.len() > at.len() && s.iter().zip(at).all(|(a, b)| a == b))
}

/// `node` at `path` without its Data, Access and Derived paths (and, for
/// a catalog's index, the entries that belong to its members), its
/// `marks` (relative to it) applied.
fn project(path: &[Vec<u8>], node: &OwnedNode, marks: Marks, catalog: bool) -> Projection {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let (cands, best) = candidates(&segments);
    let mut excluded = Vec::new();
    let mut at = Vec::new();
    let walk = Walk {
        marks: &marks,
        catalog,
    };
    let node = walk.prune(node, path.len(), &cands, best, &mut at, &mut excluded);
    // The design content holds a value with Secret parts without them; its
    // marks name where they were, in the value it holds.
    let marks = marks
        .into_iter()
        .map(|(path, mark)| match mark {
            Mark::Parts(mut ranges) => {
                ranges.sort_by_key(|r| r.start);
                let mut removed = 0;
                let at = ranges
                    .into_iter()
                    .map(|r| {
                        let position = r.start - removed;
                        removed += r.len();
                        position..position
                    })
                    .collect();
                (path, Mark::Parts(at))
            }
            whole => (path, whole),
        })
        .collect();
    Projection {
        node,
        marks,
        excluded,
    }
}

struct Walk<'m> {
    marks: &'m Marks,
    catalog: bool,
}

impl Walk<'_> {
    /// Whether the rule's paths are left out of this node's content.
    fn outside(&self, rule: Option<&Rule>) -> Option<Group> {
        let rule = rule?;
        if rule.group.left_out() {
            Some(rule.group)
        } else if self.catalog && rule.entries {
            // Part of the members' objects.
            Some(Group::Design)
        } else {
            None
        }
    }

    fn prune(
        &self,
        node: &OwnedNode,
        depth: usize,
        cands: &[&'static Rule],
        best: Option<&'static Rule>,
        at: &mut Path,
        excluded: &mut Vec<(Path, Group)>,
    ) -> OwnedNode {
        let mut out = OwnedNode {
            segment: node.segment.clone(),
            value: node.value.clone(),
            push: node.push,
            children: Vec::with_capacity(node.children.len()),
        };
        for child in &node.children {
            let (deeper, best) = step(cands, depth, &child.segment, best);
            at.push(child.segment.clone());
            if let Some((_, mark)) = self.marks.iter().find(|(p, _)| p == at) {
                excluded.push((at.clone(), Group::Secret));
                out.children.push(emptied(child, mark));
                at.pop();
                continue;
            }
            let whole = deeper.is_empty() && !marked_below(self.marks, at);
            match self.outside(best) {
                Some(Group::Design) if whole => {}
                Some(group) if whole => excluded.push((at.clone(), group)),
                _ if whole => out.children.push(child.clone()),
                own => {
                    // A rule or a Secret path goes deeper: decide the
                    // children one by one, and drop the node's own value
                    // when it is outside.
                    let mut kept = self.prune(child, depth + 1, &deeper, best, at, excluded);
                    if let Some(group) = own {
                        if kept.value.take().is_some() && group != Group::Design {
                            excluded.push((at.clone(), group));
                        }
                    }
                    out.children.push(kept);
                }
            }
            at.pop();
        }
        out
    }
}

/// A Secret node as design content shows it: emptied, or for Secret parts
/// of a value, the value without them.
fn emptied(node: &OwnedNode, mark: &Mark) -> OwnedNode {
    match mark {
        Mark::Whole => OwnedNode {
            segment: node.segment.clone(),
            ..OwnedNode::default()
        },
        Mark::Parts(ranges) => {
            let value = node.assembled().map(|v| v.into_owned()).unwrap_or_default();
            let mut kept = Vec::new();
            let mut from = 0;
            for range in ranges {
                kept.extend_from_slice(&value[from.min(value.len())..range.start.min(value.len())]);
                from = from.max(range.end);
            }
            kept.extend_from_slice(&value[from.min(value.len())..]);
            OwnedNode::leaf(node.segment.clone(), kept)
        }
    }
}

#[cfg(test)]
mod tests;
