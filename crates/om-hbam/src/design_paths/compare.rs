//! The comparison of two files' designs, a source and a target, with or
//! without a base (`docs/server-storage.md` section 9; `docs/authoring.md`,
//! "Comparing designs"): the operation [`COMPARE_DESIGNS`].
//!
//! **Matching.** With a base, an object of the source is the target's
//! object of the same kind and id when the base has that object too; one
//! the base lacks is new, even when the other file has an object with its
//! id (two files can give one id to unrelated new objects). Without a
//! base, two objects are matched only when kind, id and name all agree,
//! the name decoded on both sides and equal byte for byte; one agreeing in
//! id alone, or in name alone, is listed as a possible match and never
//! matched. A field (and a table's field catalog) is matched only when its
//! table is.
//!
//! **Differences.** Every matched object is compared by each class of its
//! content ([`super::design`]), whether it has a design revision or not:
//! the classes' projections of the two objects are hashed and compared in
//! the process, and only the [`Difference`] is reported. With a base, each
//! side is also compared with the base, and a side changed an object when
//! its Design content, its stored secrets or its unidentified content
//! differs from the base's.
//!
//! **What a report carries.** Kinds, ids, names, classes of difference and
//! design revisions, and nothing else: no edit token, no hash of content
//! outside Design, no content. A name is reported only where it is Design
//! content.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use super::design::{classify, Class, Classified, DesignRevision};
use super::rules::Seg;
use super::{objects, Kind, Object};
use crate::container::HbamFile;
use crate::slot::KeySegment;
use crate::tree::{KeyTree, OwnedNode};

/// The operation's stable id: the `om-author` command and its MCP tool
/// are named after it.
pub const COMPARE_DESIGNS: &str = "compare_designs";

/// How many possible matches by name one object lists at most; the rest
/// are counted in [`Comparison::possible_matches_left_out`].
pub const NAME_CANDIDATES: usize = 8;

/// How two versions of an object differ, decided from their bytes class
/// by class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difference {
    /// No content differs; modification stamps may.
    Same,
    /// Design content differs (other content may too).
    Design,
    /// Stored secrets differ, every differing Secret path an identified
    /// credential, and no Design or unidentified content; Data or Derived
    /// content as said.
    Secrets { data: bool, derived: bool },
    /// Only Data or Derived content differs, as said (at least one).
    Outside { data: bool, derived: bool },
    /// Unidentified content differs: a path in no group, or Secret because
    /// it could not be identified; Design content too when `design`.
    Unidentified { design: bool },
}

impl Difference {
    /// The difference between two objects' class digests.
    pub(crate) fn of(a: &[[u8; 32]; 5], b: &[[u8; 32]; 5]) -> Self {
        let differs = |class: Class| a[class as usize] != b[class as usize];
        let (data, derived) = (differs(Class::Data), differs(Class::Derived));
        if differs(Class::Unidentified) {
            Difference::Unidentified {
                design: differs(Class::Design),
            }
        } else if differs(Class::Design) {
            Difference::Design
        } else if differs(Class::Credential) {
            Difference::Secrets { data, derived }
        } else if data || derived {
            Difference::Outside { data, derived }
        } else {
            Difference::Same
        }
    }

    /// Its name in a report.
    pub fn name(self) -> &'static str {
        match self {
            Difference::Same => "same",
            Difference::Design => "design",
            Difference::Secrets {
                data: false,
                derived: false,
            } => "secrets",
            Difference::Secrets {
                data: true,
                derived: false,
            } => "secrets_and_data",
            Difference::Secrets {
                data: false,
                derived: true,
            } => "secrets_and_derived",
            Difference::Secrets {
                data: true,
                derived: true,
            } => "secrets_data_and_derived",
            Difference::Outside {
                data: true,
                derived: false,
            } => "data",
            Difference::Outside {
                data: false,
                derived: true,
            } => "derived",
            Difference::Outside { .. } => "data_and_derived",
            Difference::Unidentified { design: false } => "unidentified",
            Difference::Unidentified { design: true } => "unidentified_and_design",
        }
    }

    /// Every name [`Difference::name`] gives, in that order.
    pub const NAMES: [&'static str; 11] = [
        "same",
        "design",
        "secrets",
        "secrets_and_data",
        "secrets_and_derived",
        "secrets_data_and_derived",
        "data",
        "derived",
        "data_and_derived",
        "unidentified",
        "unidentified_and_design",
    ];

    /// Whether a side that differs so from the base changed the object:
    /// its Design content, its stored secrets or its unidentified content.
    /// A difference only in Data or Derived content is no change: a serial
    /// field's next value moves with new records, a checksum with the file.
    pub fn is_change(self) -> bool {
        matches!(
            self,
            Difference::Design | Difference::Secrets { .. } | Difference::Unidentified { .. }
        )
    }
}

/// Which side changed a matched object since the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangedBy {
    Source,
    Target,
    Both,
}

/// An object as a report names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub kind: Kind,
    /// The catalog as a whole, not one member.
    pub catalog: bool,
    /// A field's (or a field catalog's) table's id.
    pub table: Option<u64>,
    /// The id FileMaker shows ([`Object::id`]); `None` for a catalog, and
    /// for a key that does not read as one.
    pub id: Option<u64>,
    /// The name, where it decodes and is Design content.
    pub name: Option<String>,
}

/// A matched object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matched {
    pub source: Ident,
    /// The target's object: its name may differ (with a base).
    pub target_name: Option<String>,
    pub difference: Difference,
    pub source_revision: Option<DesignRevision>,
    pub target_revision: Option<DesignRevision>,
    /// With a base: each side against it.
    pub base: Option<FromBase>,
}

impl Matched {
    /// Changed by both sides, and the two differ: the administrator
    /// settles it.
    pub fn conflict(&self) -> bool {
        self.base.as_ref().and_then(FromBase::changed_by) == Some(ChangedBy::Both)
            && self.difference.is_change()
    }
}

/// A matched object against the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FromBase {
    pub revision: Option<DesignRevision>,
    /// How the source differs from the base.
    pub source: Difference,
    /// How the target differs from the base.
    pub target: Difference,
}

impl FromBase {
    pub fn changed_by(&self) -> Option<ChangedBy> {
        match (self.source.is_change(), self.target.is_change()) {
            (true, true) => Some(ChangedBy::Both),
            (true, false) => Some(ChangedBy::Source),
            (false, true) => Some(ChangedBy::Target),
            (false, false) => None,
        }
    }
}

/// Why an object of one file is matched with none of the other's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// Without a base: no object of the other file is it.
    Only,
    /// With a base: the base does not have it, so it is new in this file.
    /// `id_taken`: the other file has another new object with its id.
    New { id_taken: bool },
    /// With a base: the base has it and the other file does not, which
    /// deleted it; `changed` whether this file changed it since the base.
    DeletedInOther { changed: bool },
}

/// An object of one file matched with none of the other's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alone {
    pub object: Ident,
    pub revision: Option<DesignRevision>,
    pub presence: Presence,
}

/// What two objects agree in without being matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agreement {
    /// The same kind and id (and table): their names differ, one has
    /// none, their table is not matched, or (with a base) the base lacks
    /// both.
    Id,
    /// The same kind (and table) and name, another id.
    Name,
}

/// Two objects that may be one: an administrator says which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PossibleMatch {
    pub source: Ident,
    pub target: Ident,
    pub agrees: Agreement,
    pub source_revision: Option<DesignRevision>,
    pub target_revision: Option<DesignRevision>,
}

/// The comparison of a source's and a target's designs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub with_base: bool,
    /// In the source's object order.
    pub matched: Vec<Matched>,
    pub only_in_source: Vec<Alone>,
    pub only_in_target: Vec<Alone>,
    pub possible_matches: Vec<PossibleMatch>,
    /// Possible matches by name past [`NAME_CANDIDATES`] for one object.
    pub possible_matches_left_out: usize,
}

/// Which file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Source,
    Target,
    Base,
}

impl Which {
    pub fn name(self) -> &'static str {
        match self {
            Which::Source => "source",
            Which::Target => "target",
            Which::Base => "base",
        }
    }
}

/// Why a comparison could not be made.
#[derive(Debug)]
pub enum CompareError {
    /// A file could not be read as a FileMaker file.
    Read { which: Which, error: crate::Error },
    /// A tree deeper than a file's keys can be (file-format.md 2.2).
    TooDeep { which: Which },
}

impl std::fmt::Display for CompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompareError::Read { which, error } => {
                write!(f, "The {} file cannot be read: {error}", which.name())
            }
            CompareError::TooDeep { which } => write!(
                f,
                "The {} file's keys are nested deeper than a FileMaker file's can be.",
                which.name()
            ),
        }
    }
}

impl std::error::Error for CompareError {}

/// An object's identity within its file: kind, whether a catalog, table
/// key and member key.
type Key = (Kind, bool, Option<Vec<u8>>, Vec<u8>);

/// Objects that can be possible matches by name: kind, whether a
/// catalog, table key and name.
type NameKey<'a> = (Kind, bool, Option<&'a [u8]>, &'a str);

/// An object of a file, classified and digested.
struct Entry {
    key: Key,
    ident: Ident,
    revision: Option<DesignRevision>,
    digests: [[u8; 32]; 5],
}

/// A file's objects, by key.
struct Side {
    entries: Vec<Entry>,
    by_key: BTreeMap<Key, usize>,
}

impl Side {
    fn of(root: &OwnedNode, which: Which) -> Result<Side, CompareError> {
        let mut entries = Vec::new();
        for object in objects(root) {
            let classified = classify(&object).ok_or(CompareError::TooDeep { which })?;
            entries.push(Entry {
                key: (
                    object.kind,
                    object.catalog,
                    object.table.clone(),
                    object.key.clone(),
                ),
                ident: ident(&object, &classified),
                revision: classified.revision(),
                digests: classified.digests(),
            });
        }
        let by_key = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.key.clone(), i))
            .collect();
        Ok(Side { entries, by_key })
    }

    fn get(&self, key: &Key) -> Option<&Entry> {
        self.by_key.get(key).map(|&i| &self.entries[i])
    }

    /// Its table objects' keys.
    fn tables(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.key.0 == Kind::Table && !e.key.1)
    }
}

/// How a report names `object`: its name only where the node it is read
/// from is Design content.
fn ident(object: &Object<'_>, classified: &Classified<'_>) -> Ident {
    let name_key: &[u8] = match object.kind {
        Kind::Theme => &[0x0a],
        _ => &[0x10],
    };
    let design_name = object.catalog
        || classified.top.children.iter().any(|c| {
            c.segment == name_key
                && matches!(c.own, Some((Class::Design, _)))
                && c.children.is_empty()
        });
    Ident {
        kind: object.kind,
        catalog: object.catalog,
        table: object.table.as_deref().and_then(crate::serial::decode),
        id: object.id(),
        name: object.name.clone().filter(|_| design_name),
    }
}

/// Whether a field's (or field catalog's) table is matched; true for any
/// other object.
fn table_matched(key: &Key, tables: &BTreeSet<Vec<u8>>) -> bool {
    match (&key.0, &key.2) {
        (Kind::Field, Some(table)) => tables.contains(table),
        _ => true,
    }
}

/// Compare the designs of `source` and `target`, with `base` when given:
/// trees as [`design_tree`] reads them from files, or whole key trees.
pub fn compare(
    source: &OwnedNode,
    target: &OwnedNode,
    base: Option<&OwnedNode>,
) -> Result<Comparison, CompareError> {
    let source = Side::of(source, Which::Source)?;
    let target = Side::of(target, Which::Target)?;
    let base = base.map(|b| Side::of(b, Which::Base)).transpose()?;
    Ok(compare_sides(&source, &target, base.as_ref()))
}

fn compare_sides(source: &Side, target: &Side, base: Option<&Side>) -> Comparison {
    // Which tables are matched: with a base, those the base has too;
    // without, those whose names agree.
    let tables: BTreeSet<Vec<u8>> = source
        .tables()
        .filter(|s| {
            let Some(t) = target.get(&s.key) else {
                return false;
            };
            match base {
                Some(base) => base.get(&s.key).is_some(),
                None => s.ident.name.is_some() && s.ident.name == t.ident.name,
            }
        })
        .map(|s| s.key.3.clone())
        .collect();
    let mut out = Comparison {
        with_base: base.is_some(),
        ..Comparison::default()
    };
    let mut paired_in_target = vec![false; target.entries.len()];
    let mut alone_source = Vec::new();
    for s in &source.entries {
        let Some(&ti) = target.by_key.get(&s.key) else {
            alone_source.push(s);
            continue;
        };
        let t = &target.entries[ti];
        let in_base = base.and_then(|b| b.get(&s.key));
        let matchable = table_matched(&s.key, &tables)
            && match base {
                Some(_) => in_base.is_some(),
                None => s.ident.name.is_some() && s.ident.name == t.ident.name,
            };
        if matchable {
            paired_in_target[ti] = true;
            out.matched.push(Matched {
                source: s.ident.clone(),
                target_name: t.ident.name.clone(),
                difference: Difference::of(&s.digests, &t.digests),
                source_revision: s.revision,
                target_revision: t.revision,
                base: in_base.map(|b| FromBase {
                    revision: b.revision,
                    source: Difference::of(&b.digests, &s.digests),
                    target: Difference::of(&b.digests, &t.digests),
                }),
            });
        } else if base.is_some() && in_base.is_none() {
            // New on both sides with one id: unrelated objects.
            alone_source.push(s);
        } else {
            paired_in_target[ti] = true;
            out.possible_matches.push(PossibleMatch {
                source: s.ident.clone(),
                target: t.ident.clone(),
                agrees: Agreement::Id,
                source_revision: s.revision,
                target_revision: t.revision,
            });
        }
    }
    let alone_target: Vec<&Entry> = target
        .entries
        .iter()
        .zip(&paired_in_target)
        .filter(|(_, paired)| !**paired)
        .map(|(t, _)| t)
        .collect();
    out.only_in_source = alone(&alone_source, target, base);
    out.only_in_target = alone(&alone_target, source, base);
    if base.is_none() {
        // Possible matches by name, among the objects matched with none.
        let mut by_name: BTreeMap<NameKey<'_>, Vec<&Entry>> = BTreeMap::new();
        for t in &alone_target {
            if let Some(name) = &t.ident.name {
                by_name
                    .entry((t.key.0, t.key.1, t.key.2.as_deref(), name))
                    .or_default()
                    .push(t);
            }
        }
        for s in &alone_source {
            let Some(name) = &s.ident.name else { continue };
            let Some(candidates) = by_name.get(&(s.key.0, s.key.1, s.key.2.as_deref(), name))
            else {
                continue;
            };
            for t in candidates.iter().take(NAME_CANDIDATES) {
                out.possible_matches.push(PossibleMatch {
                    source: s.ident.clone(),
                    target: t.ident.clone(),
                    agrees: Agreement::Name,
                    source_revision: s.revision,
                    target_revision: t.revision,
                });
            }
            out.possible_matches_left_out += candidates.len().saturating_sub(NAME_CANDIDATES);
        }
    }
    out
}

/// The objects of one side matched with none of `other`'s.
fn alone(entries: &[&Entry], other: &Side, base: Option<&Side>) -> Vec<Alone> {
    entries
        .iter()
        .map(|e| {
            let presence = match base {
                None => Presence::Only,
                Some(base) => match base.get(&e.key) {
                    None => Presence::New {
                        id_taken: other.get(&e.key).is_some(),
                    },
                    Some(b) => Presence::DeletedInOther {
                        changed: Difference::of(&b.digests, &e.digests).is_change(),
                    },
                },
            };
            Alone {
                object: e.ident.clone(),
                revision: e.revision,
                presence,
            }
        })
        .collect()
}

/// Whether a slot of a file holds what design objects are made of: all
/// but each table's records, indexes, record list, global values and
/// container library, and the occurrences' saved found sets.
fn design_slot(path: &[KeySegment<'_>]) -> bool {
    match path {
        [table, second, ..] if Seg::Table.matches(table.0) => second.0 == [0x03],
        [occurrence, _, ..] if Seg::Occurrence.matches(occurrence.0) => false,
        _ => true,
    }
}

/// The part of a file's key tree its design objects are read from.
pub fn design_tree(file: &HbamFile) -> crate::Result<OwnedNode> {
    Ok(KeyTree::from_slots_where(file.slots(), design_slot)?
        .root
        .to_owned_node())
}

/// [`compare`] for files on this computer.
pub fn compare_files(
    source: &FsPath,
    target: &FsPath,
    base: Option<&FsPath>,
) -> Result<Comparison, CompareError> {
    let read = |path: &FsPath, which: Which| {
        HbamFile::open(path)
            .and_then(|file| design_tree(&file))
            .map_err(|error| CompareError::Read { which, error })
    };
    let source = read(source, Which::Source)?;
    let target = read(target, Which::Target)?;
    let base = base.map(|path| read(path, Which::Base)).transpose()?;
    compare(&source, &target, base.as_ref())
}

#[cfg(test)]
mod tests;
