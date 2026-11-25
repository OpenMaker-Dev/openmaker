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

use super::design::{classify, Class, Classified, DesignRevision, Refusal};
use super::rules::{Group, CATALOGS};
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

/// Why a file could not be read: what failed, never where in it, since a
/// key's path can hold any content, a stored password's included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// No file at the path.
    Missing,
    /// The file may not be read.
    PermissionDenied,
    /// Reading the file failed otherwise.
    Io,
    /// Not a FileMaker file, or one this reader does not open (encrypted).
    NotFileMaker,
    /// A FileMaker file whose pages or keys do not decode.
    Damaged,
}

impl ReadFailure {
    fn of(error: &crate::Error) -> Self {
        match error {
            crate::Error::Io(io) => match io.kind() {
                std::io::ErrorKind::NotFound => ReadFailure::Missing,
                std::io::ErrorKind::PermissionDenied => ReadFailure::PermissionDenied,
                _ => ReadFailure::Io,
            },
            crate::Error::NotHbam => ReadFailure::NotFileMaker,
            _ => ReadFailure::Damaged,
        }
    }

    fn text(self) -> &'static str {
        match self {
            ReadFailure::Missing => "there is no file at its path",
            ReadFailure::PermissionDenied => "it may not be read",
            ReadFailure::Io => "reading it failed",
            ReadFailure::NotFileMaker => "it is not a FileMaker file this reader opens",
            ReadFailure::Damaged => "its pages or keys do not decode",
        }
    }
}

/// Why a comparison could not be made. It names a file, a kind and an id
/// at most, never a key's bytes or any content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareError {
    /// A file could not be read as a FileMaker file.
    Read { which: Which, failure: ReadFailure },
    /// A tree deeper than a file's keys can be (file-format.md 2.2).
    TooDeep { which: Which },
    /// A member with a key the decoders could read as another
    /// (`[00 04]` as the step list `04`): which of its nodes are which is in
    /// doubt, so it is not compared.
    Misread {
        which: Which,
        kind: Kind,
        id: Option<u64>,
    },
    /// Two objects of one kind (and table) that report one id: matching by
    /// id would have to guess which is which.
    Ambiguous {
        which: Which,
        kind: Kind,
        table: Option<u64>,
        id: Option<u64>,
    },
}

fn id_text(id: Option<u64>) -> String {
    id.map_or_else(|| "no id".to_string(), |id| format!("id {id}"))
}

impl std::fmt::Display for CompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            CompareError::Read { which, failure } => {
                write!(
                    f,
                    "The {} file cannot be read: {}.",
                    which.name(),
                    failure.text()
                )
            }
            CompareError::TooDeep { which } => write!(
                f,
                "The {} file's keys are nested deeper than a FileMaker file's can be.",
                which.name()
            ),
            CompareError::Misread { which, kind, id } => write!(
                f,
                "In the {} file, the {} with {} holds a key the decoders could read as another, \
                 so what it holds cannot be told; it is not compared.",
                which.name(),
                kind.name(),
                id_text(id)
            ),
            CompareError::Ambiguous {
                which,
                kind,
                table,
                id,
            } => {
                let table = table.map_or_else(String::new, |t| format!(" of table {t}"));
                write!(
                    f,
                    "In the {} file, two {} objects{table} have {}: they cannot be told apart.",
                    which.name(),
                    kind.name(),
                    id_text(id)
                )
            }
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
            let classified = classify(&object).map_err(|refusal| match refusal {
                Refusal::TooDeep => CompareError::TooDeep { which },
                Refusal::Misread => CompareError::Misread {
                    which,
                    kind: object.kind(),
                    id: object.id(),
                },
            })?;
            entries.push(Entry {
                key: (
                    object.kind(),
                    object.is_catalog(),
                    object.table().map(<[u8]>::to_vec),
                    object.key().to_vec(),
                ),
                ident: ident(&object, &classified),
                revision: classified.revision(),
                digests: classified.digests(),
            });
        }
        // One report identity per object: ids that read alike from keys
        // that differ (a font's `01` and `00 01`), or a key twice in a tree
        // built in memory, are refused.
        let mut seen = BTreeSet::new();
        for e in &entries {
            let i = &e.ident;
            if !seen.insert((i.kind, i.catalog, i.table, i.id)) {
                return Err(CompareError::Ambiguous {
                    which,
                    kind: i.kind,
                    table: i.table,
                    id: i.id,
                });
            }
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

    /// Its table objects.
    fn tables(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.key.0 == Kind::Table && !e.key.1)
    }
}

/// How a report names `object`: its name only where it is a Design piece.
fn ident(object: &Object<'_>, classified: &Classified<'_>) -> Ident {
    let name = (object.is_catalog() || classified.design_name())
        .then(|| object.name().map(str::to_string))
        .flatten();
    Ident {
        kind: object.kind(),
        catalog: object.is_catalog(),
        table: object.table().and_then(crate::serial::decode),
        id: object.id(),
        name,
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

/// Whether two objects with one key, the base lacking or not consulted,
/// are one: their names agree, or neither has one and every class of their
/// content is equal.
fn same_object(s: &Entry, t: &Entry) -> bool {
    match (&s.ident.name, &t.ident.name) {
        (Some(a), Some(b)) => a == b,
        (None, None) => s.digests == t.digests,
        _ => false,
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
    let mut source_matched = vec![false; source.entries.len()];
    let mut target_matched = vec![false; target.entries.len()];
    for (si, s) in source.entries.iter().enumerate() {
        let Some(&ti) = target.by_key.get(&s.key) else {
            continue;
        };
        let t = &target.entries[ti];
        let in_base = base.and_then(|b| b.get(&s.key));
        let one = table_matched(&s.key, &tables)
            && match base {
                Some(_) => in_base.is_some(),
                None => same_object(s, t),
            };
        if one {
            source_matched[si] = true;
            target_matched[ti] = true;
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
        } else if base.is_none() || in_base.is_some() {
            // One id, not one object: listed for a person to settle. With
            // a base that lacks both, they are two new objects instead.
            out.possible_matches.push(possible(s, t, Agreement::Id));
        }
    }
    let unmatched = |side: &'_ Side, matched: &[bool]| -> Vec<usize> {
        (0..side.entries.len()).filter(|&i| !matched[i]).collect()
    };
    let (source_alone, target_alone) = (
        unmatched(source, &source_matched),
        unmatched(target, &target_matched),
    );
    if base.is_none() {
        // Possible matches by name, among the objects matched with none,
        // whether or not they agree with another in id.
        let mut by_name: BTreeMap<NameKey<'_>, Vec<&Entry>> = BTreeMap::new();
        for &ti in &target_alone {
            let t = &target.entries[ti];
            if let Some(name) = &t.ident.name {
                by_name
                    .entry((t.key.0, t.key.1, t.key.2.as_deref(), name))
                    .or_default()
                    .push(t);
            }
        }
        for &si in &source_alone {
            let s = &source.entries[si];
            let Some(name) = &s.ident.name else { continue };
            let Some(candidates) = by_name.get(&(s.key.0, s.key.1, s.key.2.as_deref(), name))
            else {
                continue;
            };
            let mut listed = 0usize;
            for t in candidates.iter().filter(|t| t.key != s.key) {
                if listed == NAME_CANDIDATES {
                    out.possible_matches_left_out = out.possible_matches_left_out.saturating_add(1);
                    continue;
                }
                listed += 1;
                out.possible_matches.push(possible(s, t, Agreement::Name));
            }
        }
    }
    out.only_in_source = alone(source, &source_alone, target, base);
    out.only_in_target = alone(target, &target_alone, source, base);
    out
}

fn possible(s: &Entry, t: &Entry, agrees: Agreement) -> PossibleMatch {
    PossibleMatch {
        source: s.ident.clone(),
        target: t.ident.clone(),
        agrees,
        source_revision: s.revision,
        target_revision: t.revision,
    }
}

/// The objects `indexes` of `side` matched with none of `other`'s.
fn alone(side: &Side, indexes: &[usize], other: &Side, base: Option<&Side>) -> Vec<Alone> {
    indexes
        .iter()
        .map(|&i| {
            let e = &side.entries[i];
            let presence = match base {
                None => Presence::Only,
                Some(base) => match (base.get(&e.key), other.get(&e.key)) {
                    (None, other) => Presence::New {
                        id_taken: other.is_some(),
                    },
                    (Some(b), None) => Presence::DeletedInOther {
                        changed: Difference::of(&b.digests, &e.digests).is_change(),
                    },
                    // In all three, and not matched: a field whose table
                    // is not.
                    (Some(_), Some(_)) => Presence::Only,
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

/// Whether a slot of a file belongs to a design catalog (or leads to
/// one): [`design_tree`] keeps only these.
fn design_slot(path: &[KeySegment<'_>]) -> bool {
    CATALOGS
        .iter()
        .filter(|c| c.group == Group::Design)
        .any(|c| c.path.iter().zip(path).all(|(seg, s)| seg.matches(s.0)))
}

/// The design catalogs of a file's key tree, and nothing else: the file is
/// read whole and every slot decoded, and only the slots of design
/// catalogs (and the keys leading to them) are kept in the tree.
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
            .map_err(|error| CompareError::Read {
                which,
                failure: ReadFailure::of(&error),
            })
    };
    let source = read(source, Which::Source)?;
    let target = read(target, Which::Target)?;
    let base = base.map(|path| read(path, Which::Base)).transpose()?;
    compare(&source, &target, base.as_ref())
}

#[cfg(test)]
mod tests;
