//! Which paths of a file are Design, Data, Access, Secret or Derived, the
//! file's design objects, and their revisions (`docs/server-storage.md`
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
//! error. The rules reach the first-level keys of members and catalog
//! indexes; below a rule everything is that rule's, so what a value or a
//! step's data holds beyond the rules is not seen (a password typed into a
//! calculation is formula text, Design).
//!
//! **Objects.** [`objects`] lists a tree's design objects: every member
//! `[cat, 5, id]` of a Design catalog, decoded by OpenMaker or not, and
//! each Design catalog as a whole. A member object is its member alone. A
//! catalog object is its index `[cat, 1]`: the display order and the
//! catalog's other settings are Design; the name list, the member caches,
//! digests and dependency lists are Derived (they follow from the members,
//! and counting them in a member's revision would make one object's change
//! move another's); the last issued id is Data. A field is an object of its
//! own; its table's field catalog index `[T, 3, 1]` is the field catalog's
//! object. A table is its member `[3, 10, 5, T]`; its fields and its data
//! under `[T]` are not part of it. An emptied member (a deletion,
//! scripts.md) is no object.
//!
//! **Revisions.** An object's [`Object::revision`] is the canonical form
//! ([`revision`]) of its member or index without its Data, Access and
//! Derived paths and without modification stamps, each Secret path written
//! as a marker without its value. Paths in no group stay in: they are
//! content OpenMaker does not understand. Scripts keep the script form;
//! every other kind uses the general form. A table's `fields_revision` is
//! the general form over its whole field catalog `[T, 3]`
//! ([`catalog_revision`]), Data paths included, as before.

mod report;
mod revision;
mod rules;
mod secrets;

pub use report::{report, Member, MemberIs, Report};
pub use revision::{
    catalog_revision, is_stamp, parse_revision, revision_text, script_form_revision, Revision,
    MOD_ACCOUNT, MOD_COUNT, MOD_TIME, MOD_USER,
};
pub use rules::{
    group_of, hex, is_odbc_profile, markdown_secrets, markdown_table, pattern_text, rule_for,
    rules, Catalog, Group, Key, Rule, Seg, StepSecret, CATALOGS, COMMON_INDEX, COMMON_MEMBER,
    OCCURRENCE_BASE, STEP_SECRETS, TABLE_BASE,
};

use revision::{digest, Form};

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

    fn form(self) -> Form {
        match self {
            Kind::Script => Form::Script,
            _ => Form::General,
        }
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
}

/// What an object's content is: its node without its Data, Access and
/// Derived paths, each Secret path emptied, and those paths.
struct Projection {
    node: OwnedNode,
    secret: Vec<Path>,
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

    fn projection(&self) -> Projection {
        let secrets = if self.catalog {
            Vec::new()
        } else {
            secrets::of(self.kind, self.node)
                .into_iter()
                .map(|(path, _)| path)
                .collect()
        };
        project(&self.path, self.node, secrets)
    }

    /// The node without its Data, Access and Derived paths, each Secret
    /// path emptied (no value, no children): what its revision covers, and
    /// what a deploy takes from a source.
    pub fn design(&self) -> OwnedNode {
        self.projection().node
    }

    /// The paths taken out of the node or emptied, relative to it, with
    /// their group: Data, Access, Derived and Secret, which a deploy keeps
    /// from its target or recomputes.
    pub fn excluded(&self) -> Vec<(Path, Group)> {
        self.projection().excluded
    }

    /// The node's Secret paths, relative to it, with what each holds.
    pub fn secrets(&self) -> Vec<(Path, &'static str)> {
        if self.catalog {
            Vec::new()
        } else {
            secrets::of(self.kind, self.node)
        }
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

    /// The object's revision.
    pub fn revision(&self) -> Revision {
        let projection = self.projection();
        let form = if self.catalog {
            Form::General
        } else {
            self.kind.form()
        };
        digest(&projection.node, form, &projection.secret)
    }
}

/// The revision of a member of `kind` (with its table for a field), as
/// [`Object::revision`] computes it, for a caller that holds the member
/// alone.
pub fn revision_of(kind: Kind, table: Option<&[u8]>, member: &OwnedNode) -> Revision {
    let mut path = kind.catalog().at(table);
    path.push(vec![0x05]);
    path.push(member.segment.clone());
    Object {
        kind,
        catalog: false,
        key: member.segment.clone(),
        table: table.map(<[u8]>::to_vec),
        name: None,
        path,
        node: member,
    }
    .revision()
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

/// A tree's design objects, catalog by catalog in [`CATALOGS`] order: the
/// catalog as a whole when it has an index, then its members in key order.
pub fn objects(root: &OwnedNode) -> Vec<Object<'_>> {
    let mut out = Vec::new();
    for catalog in CATALOGS {
        let Some(kind) = catalog.kind else { continue };
        for (table, path, node) in instances(root, catalog) {
            if let Some(index) = node.child(&[0x01]) {
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
                });
            }
            let Some(members) = node.child(&[0x05]) else {
                continue;
            };
            for member in members.children.iter().filter(|m| !m.children.is_empty()) {
                let mut at = path.clone();
                at.push(vec![0x05]);
                at.push(member.segment.clone());
                out.push(Object {
                    kind,
                    catalog: false,
                    key: member.segment.clone(),
                    table: table.clone(),
                    name: name(kind, member),
                    path: at,
                    node: member,
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

/// The group of `path` in the tree `root`: Secret where a step rule finds
/// a credential at or above it, otherwise its path rule's ([`group_of`]).
pub fn group_in(root: &OwnedNode, path: &[&[u8]]) -> Group {
    if let Some((kind, depth)) = member_of(path) {
        if let Some(member) = root.at(&path[..depth]) {
            let below = &path[depth..];
            let secret = secrets::of(kind, member)
                .into_iter()
                .any(|(s, _)| s.len() <= below.len() && s.iter().zip(below).all(|(a, b)| a == b));
            if secret {
                return Group::Secret;
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

/// Whether a path of `secret` lies strictly below `at`.
fn secret_below(secret: &[Path], at: &[Vec<u8>]) -> bool {
    secret
        .iter()
        .any(|s| s.len() > at.len() && s.iter().zip(at).all(|(a, b)| a == b))
}

/// `node` at `path` without its Data, Access and Derived paths, its
/// `secret` paths (relative to it) emptied.
fn project(path: &[Vec<u8>], node: &OwnedNode, secret: Vec<Path>) -> Projection {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let (cands, best) = candidates(&segments);
    let mut excluded = Vec::new();
    let mut at = Vec::new();
    let node = prune(
        node,
        path.len(),
        &cands,
        best,
        &secret,
        &mut at,
        &mut excluded,
    );
    Projection {
        node,
        secret,
        excluded,
    }
}

fn outside(rule: Option<&Rule>) -> Option<Group> {
    rule.map(|r| r.group).filter(|g| g.left_out())
}

fn prune(
    node: &OwnedNode,
    depth: usize,
    cands: &[&'static Rule],
    best: Option<&'static Rule>,
    secret: &[Path],
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
        if secret.contains(at) {
            excluded.push((at.clone(), Group::Secret));
            out.children.push(OwnedNode {
                segment: child.segment.clone(),
                ..OwnedNode::default()
            });
            at.pop();
            continue;
        }
        let whole = deeper.is_empty() && !secret_below(secret, at);
        match outside(best) {
            Some(group) if whole => excluded.push((at.clone(), group)),
            _ if whole => out.children.push(child.clone()),
            own => {
                // A rule or a Secret path goes deeper: decide the children
                // one by one, and drop the node's own value when it is
                // outside.
                let mut kept = prune(child, depth + 1, &deeper, best, secret, at, excluded);
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
