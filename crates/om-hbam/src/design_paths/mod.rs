//! Which paths of a file are Design, Data or Access, the file's design
//! objects, and their revisions (`docs/server-storage.md` sections 4 and
//! 9; `docs/authoring.md`, "Revisions").
//!
//! **Groups.** [`group_of`] puts a path in [`Group::Design`],
//! [`Group::Data`], [`Group::Access`] or [`Group::None`] by the rules of
//! [`rules()`], one table, each rule with the spec section that says what
//! its paths hold. The grouping is by path, not by catalog member: a serial
//! field's next value is a key inside its field definition and is Data
//! (write-path.md 3.2). A key no spec names is matched by no rule and is in
//! no group, as is the file as a whole (`[2]`); that is never an error.
//!
//! **Objects.** [`objects`] lists a tree's design objects: every member
//! `[cat, 5, id]` of a Design catalog, decoded by OpenMaker or not. An
//! object is its member and nothing else. Its catalog's index `[cat, 1]`
//! (the name list, the display order, member caches, digests, a field
//! catalog's lists by type and dependencies) belongs to the catalog as a
//! whole ([`indexes`]): it is derived from the members or orders them, and
//! counting it in a member's revision would make one object's change move
//! another's. A field is an object of its own; what its table's field
//! catalog holds for no single field is that catalog's index `[T, 3, 1]`.
//! A table is its member `[3, 10, 5, T]`; its fields and its data under
//! `[T]` are not part of it. An emptied member (a deletion, scripts.md) is
//! no object.
//!
//! **Revisions.** An object's [`Object::revision`] is the canonical form of
//! its member without its Data and Access paths ([`Object::design`]) and
//! without modification stamps ([`is_stamp`]). Paths in no group stay in:
//! they are content OpenMaker does not understand. Scripts keep the form
//! they always had ([`content_revision`]); every other kind uses the
//! general form ([`catalog_revision`]), which is the form layouts and value
//! lists always had, so no revision `om-author` reported before changes. A
//! table's `fields_revision` is the general form over its whole field
//! catalog `[T, 3]`, Data paths included, as before.

mod report;
mod revision;
mod rules;

pub use report::{report, Member, MemberIs, Report};
pub use revision::{
    catalog_revision, content_revision, is_stamp, parse_revision, revision_text, Revision,
    MOD_ACCOUNT, MOD_COUNT, MOD_TIME, MOD_USER,
};
pub use rules::{
    group_of, hex, markdown_table, pattern_text, rule_for, rules, Catalog, Group, Key, Rule, Seg,
    CATALOGS, COMMON_INDEX, COMMON_MEMBER,
};

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

/// A design object: a member of a Design catalog.
#[derive(Clone, Debug)]
pub struct Object<'t> {
    pub kind: Kind,
    /// The member's key segment: its id within its catalog (and, for a
    /// field, its table). Deploys match objects by kind and key
    /// (`docs/server-storage.md` section 9).
    pub key: Vec<u8>,
    /// A field's table's key.
    pub table: Option<Vec<u8>>,
    /// The member's name `10`, when it has one that decodes.
    pub name: Option<String>,
    /// The member's path.
    pub path: Path,
    pub member: &'t OwnedNode,
}

impl Object<'_> {
    /// The id FileMaker shows for it: the key read as a serial key
    /// (file-metadata.md 1). `om-author`'s references number tables,
    /// occurrences and fields by the key's big-endian bytes instead.
    pub fn id(&self) -> Option<u64> {
        crate::serial::decode(&self.key)
    }

    /// The member without its Data and Access paths: what its revision
    /// covers, and what a deploy takes from a source.
    pub fn design(&self) -> OwnedNode {
        design_of(&self.path, self.member).0
    }

    /// The Data and Access paths taken out of the member, relative to it,
    /// with their group: what a deploy keeps from its target.
    pub fn excluded(&self) -> Vec<(Path, Group)> {
        design_of(&self.path, self.member).1
    }

    /// The shapes ([`Report`]) of the member's paths that no rule covers:
    /// a member holding one is not deployed over an existing one
    /// (`docs/server-storage.md` section 9).
    pub fn unclassified(&self) -> Vec<String> {
        let mut report = Report::default();
        let segments: Vec<&[u8]> = self.path.iter().map(Vec::as_slice).collect();
        report::count_under(self.member, &segments, &mut report);
        report.unclassified.into_keys().collect()
    }

    /// The object's revision.
    pub fn revision(&self) -> Revision {
        form(self.kind, &self.design())
    }
}

/// The revision of a member of `kind` (with its table for a field), as
/// [`Object::revision`] computes it, for a caller that holds the member
/// alone.
pub fn revision_of(kind: Kind, table: Option<&[u8]>, member: &OwnedNode) -> Revision {
    let mut path = kind.catalog().at(table);
    path.push(vec![0x05]);
    path.push(member.segment.clone());
    form(kind, &design_of(&path, member).0)
}

fn form(kind: Kind, design: &OwnedNode) -> Revision {
    match kind {
        Kind::Script => content_revision(design),
        _ => catalog_revision(design),
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

/// A tree's design objects, catalog by catalog in [`CATALOGS`] order, each
/// catalog's in key order.
pub fn objects(root: &OwnedNode) -> Vec<Object<'_>> {
    let mut out = Vec::new();
    for catalog in CATALOGS {
        let Some(kind) = catalog.kind else { continue };
        for (table, path, node) in instances(root, catalog) {
            let Some(members) = node.child(&[0x05]) else {
                continue;
            };
            for member in members.children.iter().filter(|m| !m.children.is_empty()) {
                let mut at = path.clone();
                at.push(vec![0x05]);
                at.push(member.segment.clone());
                out.push(Object {
                    kind,
                    key: member.segment.clone(),
                    table: table.clone(),
                    name: name(member),
                    path: at,
                    member,
                });
            }
        }
    }
    out
}

/// A member's name `10` as text (XOR 0x5A masked SCSU, file-format.md 3.5).
fn name(member: &OwnedNode) -> Option<String> {
    let value = member.child(&[0x10])?.assembled()?;
    om_scsu::decode_scsu(&value).ok()
}

/// A Design catalog's index `[cat, 1]`: content that belongs to the
/// catalog as a whole.
#[derive(Clone, Debug)]
pub struct Index<'t> {
    pub catalog: &'static Catalog,
    pub table: Option<Vec<u8>>,
    pub path: Path,
    pub node: &'t OwnedNode,
}

impl Index<'_> {
    /// The general form over the whole index: what `om-author` reports as
    /// the layout catalog's `layouts_revision`.
    pub fn revision(&self) -> Revision {
        catalog_revision(self.node)
    }
}

/// Every Design catalog's index in a tree.
pub fn indexes(root: &OwnedNode) -> Vec<Index<'_>> {
    let mut out = Vec::new();
    for catalog in CATALOGS.iter().filter(|c| c.group == Group::Design) {
        for (table, mut path, node) in instances(root, catalog) {
            if let Some(index) = node.child(&[0x01]) {
                path.push(vec![0x01]);
                out.push(Index {
                    catalog,
                    table,
                    path,
                    node: index,
                });
            }
        }
    }
    out
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

/// `member` at `path` without its Data and Access paths, and those paths.
fn design_of(path: &[Vec<u8>], member: &OwnedNode) -> (OwnedNode, Vec<(Path, Group)>) {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let (cands, best) = candidates(&segments);
    let mut excluded = Vec::new();
    let mut at = Vec::new();
    let node = prune(member, path.len(), &cands, best, &mut at, &mut excluded);
    (node, excluded)
}

fn outside(rule: Option<&Rule>) -> Option<Group> {
    rule.map(|r| r.group)
        .filter(|g| matches!(g, Group::Data | Group::Access))
}

fn prune(
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
        match outside(best) {
            Some(group) if deeper.is_empty() => excluded.push((at.clone(), group)),
            _ if deeper.is_empty() => out.children.push(child.clone()),
            own => {
                // A rule goes deeper: decide the children one by one, and
                // drop the node's own value when it is outside.
                let mut kept = prune(child, depth + 1, &deeper, best, at, excluded);
                if let Some(group) = own {
                    if kept.value.take().is_some() {
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

#[cfg(test)]
mod tests;
