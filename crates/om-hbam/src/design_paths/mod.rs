//! Which paths of a file are Design, Data, Access, Secret or Derived, the
//! file's design objects, their edit tokens and design revisions, and the
//! comparison of two files' designs (`docs/server-storage.md` sections 4
//! and 9; `docs/authoring.md`, "Revisions" and "Comparing designs").
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
//! taken as final. "Not Secret" means identified and known not to hold a
//! credential: what cannot be identified with certainty is Secret. The
//! rules reach the first-level keys of members and
//! catalog indexes; below a rule everything is that rule's, so what a
//! value or a step's data holds beyond the rules is not seen (a password
//! typed into a calculation is formula text, Design).
//!
//! **Objects.** [`objects`] lists a tree's design objects: every member
//! `[cat, 5, id]` of a Design catalog, decoded by OpenMaker or not, and
//! each Design catalog as a whole. A member object is its member and its
//! entries in the catalog's index ([`Object::entries`]), where an index
//! keeps one per member of what a tool shows of it (a script's cached name
//! and full-access flag, scripts.md). A catalog object is its index: the
//! display order and the catalog's settings are Design; name lists,
//! digests, counts and dependency lists are Derived (they follow from the
//! members, and no tool shows them); the last issued id is Data. A field is an
//! object of its own; its table's field catalog index `[T, 3, 1]` is the
//! field catalog's object. A table is its member `[3, 10, 5, T]`; its
//! fields and its data under `[T]` are not part of it. An emptied member
//! (a deletion, scripts.md) is no object. That is the object of an edit,
//! and of its edit token; for design revisions and comparisons a member
//! object is its member alone and a catalog object all of its catalog
//! that is in no member object ([`design`]), and every catalog has one.
//!
//! **Edit tokens** ([`revision`]). An object's [`Object::edit_token`]
//! covers its whole member (or index), stored passwords included, without
//! modification stamps: what `om-author`, `om-provider` and `om-server`
//! exchange as a revision to guard an edit.
//!
//! **Design revisions** ([`design`]). An object's
//! [`Object::design_revision`] is the hash of its Design pieces alone: for
//! a member object its member, for a catalog object all of its catalog
//! that is in no member object (its index with every entry included). A
//! script's or a button's step data is one opaque piece, never Design, so
//! an object holding step data has none, as has one holding a piece no
//! rule names; a member the decoders could read differently is not
//! classified at all.
//!
//! **Comparison** ([`compare`], [`compare_files`]). Two files' objects,
//! their identities checked first, matched with or without a base, and
//! each matched object compared by its pieces group by group, reported as
//! kinds, ids, names, the parts that differ and design revisions, never
//! content.

mod compare;
mod design;
mod inventory;
mod report;
mod revision;
mod rules;
mod secrets;

pub use compare::{
    compare, compare_files, design_tree, Agreement, Alone, ChangedBy, CompareError, Comparison,
    Difference, FromBase, Ident, Matched, PossibleMatch, Presence, ReadFailure, Which,
    COMPARE_DESIGNS, NAME_CANDIDATES,
};
pub use design::{DesignRevision, DESIGN_REVISION_VERSION};
pub use inventory::{inventory, Checked, IdentityError, IdentityProblem};
pub use report::{report, Member, MemberIs, Report};
pub use revision::{
    catalog_revision, is_stamp, legacy_catalog_revision, legacy_script_form_revision,
    parse_revision, revision_text, script_form_revision, split_value, Revision, MOD_ACCOUNT,
    MOD_COUNT, MOD_TIME, MOD_USER,
};
pub use rules::{
    hex, is_odbc_profile, markdown_secrets, markdown_table, pattern_text, rules, Catalog, Group,
    Key, Rule, Seg, StepSecret, CATALOGS, COMMON_INDEX, COMMON_MEMBER, OCCURRENCE_BASE,
    STEP_SECRETS, TABLE_BASE,
};

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
/// whole (its index). Only [`objects`] makes one, from a tree, so what it
/// holds is that tree's member at its own path.
#[derive(Clone, Debug)]
pub struct Object<'t> {
    kind: Kind,
    catalog: bool,
    key: Vec<u8>,
    table: Option<Vec<u8>>,
    path: Path,
    node: &'t OwnedNode,
    entries: Vec<(Path, &'t OwnedNode)>,
    /// A catalog object's whole catalog node and its path: what the design
    /// revision and the comparison cover of it, all of the catalog but its
    /// member objects.
    whole: Option<(Path, &'t OwnedNode)>,
}

/// The index of a catalog that has none: a catalog object exists for every
/// catalog.
static NO_INDEX: OwnedNode = OwnedNode {
    segment: Vec::new(),
    value: None,
    push: false,
    children: Vec::new(),
};

impl<'t> Object<'t> {
    /// The kind of the catalog's members.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Whether it is the catalog as a whole, not one member.
    pub fn is_catalog(&self) -> bool {
        self.catalog
    }

    /// The member's key segment: its id within its catalog (and, for a
    /// field, its table); empty for a catalog. Deploys match objects by
    /// kind and key (`docs/server-storage.md` section 9).
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// A field's (or the field catalog's) table's key.
    pub fn table(&self) -> Option<&[u8]> {
        self.table.as_deref()
    }

    /// The member's name when it has one that decodes: `10`, a theme's
    /// display name `0a` (layout-model.md 7.5); a catalog's name. Decoded
    /// when asked. For a report, [`Object::design_name`].
    pub fn name(&self) -> Option<String> {
        if self.catalog {
            return Some(Catalog::of(self.kind).name.into());
        }
        name(self.kind, self.node)
    }

    /// The member's or the index's path (a catalog's index path even when
    /// it has no index).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn node(&self) -> &'t OwnedNode {
        self.node
    }

    /// A member's entries in its catalog's index (a script's
    /// `[11, 1, 07, S]`), with their paths: part of the object.
    pub fn entries(&self) -> &[(Path, &'t OwnedNode)] {
        &self.entries
    }

    /// The name, only where it is Design throughout (a catalog's always),
    /// and only of a member read as the decoders read it: what a report may
    /// show. Given out through a checked inventory ([`inventory`]).
    pub(crate) fn design_name(&self) -> Option<String> {
        if self.catalog {
            return self.name();
        }
        let classified = design::classify(self).ok()?;
        classified.design_name().then(|| self.name()).flatten()
    }

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

    /// The edit token of the member (or index), stored passwords
    /// included, without modification stamps; the script form for a
    /// script. What authoring exchanges as `revision` and
    /// `expected_revision`. It covers the member alone, not its
    /// [`Object::entries`].
    pub fn edit_token(&self) -> Revision {
        edit_token(if self.catalog { None } else { Some(self.kind) }, self.node)
    }

    /// The object's design revision: the hash of its Design pieces, in its
    /// member, or for a catalog in all of the catalog but its member
    /// objects, with no step data and no piece of any other group in it
    /// ([`design`]'s module doc). `None` when the object holds step data or
    /// an unidentified piece. Given out only for an object of a checked
    /// inventory ([`inventory`]).
    pub(crate) fn design_revision(&self) -> Option<DesignRevision> {
        design::classify(self).ok()?.revision()
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
            // A catalog object for every catalog: all of the catalog but its
            // member objects (`design`'s module doc).
            let mut at = path.clone();
            at.push(vec![0x01]);
            out.push(Object {
                kind,
                catalog: true,
                key: Vec::new(),
                table: table.clone(),
                path: at,
                node: index.unwrap_or(&NO_INDEX),
                entries: Vec::new(),
                whole: Some((path.clone(), node)),
            });
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
                    path: at,
                    node: member,
                    entries,
                    whole: None,
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
/// a credential at, below or above it (a member or slot holding one is
/// Secret), for every profile, for every slot of step data whose step is not
/// identified with certainty, and for a path in step data under a member
/// the tree does not hold; otherwise its path rule's. "Not Secret" means
/// identified and known not to hold a credential.
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

#[cfg(test)]
mod tests;
