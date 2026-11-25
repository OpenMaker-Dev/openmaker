//! The comparison of two files' designs, a source and a target, with or
//! without a base (`docs/server-storage.md` section 9; `docs/authoring.md`,
//! "Comparing designs"): the operation [`COMPARE_DESIGNS`].
//!
//! **Inventories first.** Every input's [`inventory`] is checked before a
//! name of any input is decoded or anything is classified or hashed:
//! sibling keys in order and unique, every key under a member list of the
//! shape its catalog's spec gives, no two objects of one kind (and table)
//! with one id, and no member the decoders could read differently. Any
//! failure refuses the comparison ([`CompareError::Identity`]).
//!
//! **Matching.** With a base, an object of the source is the target's
//! object of the same kind and id when the base has that object too; one
//! the base lacks is new, even when the other file has an object with its
//! id (two files can give one id to unrelated new objects). Without a
//! base, two objects of one kind and id are matched when their names,
//! decoded on both sides, are equal byte for byte, or when neither has a
//! name and every class of their content is equal: that is no proof that
//! they share a history, and a deploy must not take it for one. A field
//! (and a table's field catalog) is matched only when its table is, by the
//! same test; one whose id and name agree under a table that is not
//! matched is a possible match by id. Agreeing in id alone is listed as a
//! possible match, and without a base so is agreeing in name alone;
//! neither is matched. Every object matched with none is listed once, in
//! the file it is in.
//!
//! **Differences.** Every matched object is compared by each class of its
//! pieces ([`super::design`]), whether it has a design revision or not:
//! the classes' projections of the two objects are hashed and compared in
//! the process, and only the [`Difference`] is reported. With a base, each
//! side is also compared with the base, and a side changed an object when
//! its Design, its step data or its unidentified pieces differ from the
//! base's.
//!
//! **What a report carries.** Kinds, ids, names, the parts that differ and
//! design revisions, and nothing else: no edit token, no hash of content
//! outside Design, no content. A name is reported only where it is one
//! Design piece.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use super::design::{classify, Class, Classified, DesignRevision, Refusal};
use super::inventory::{inventory, on_the_way, Checked, IdentityError, IdentityProblem};
use super::rules::{Group, CATALOGS};
use super::{Kind, Object};
use crate::container::HbamFile;
use crate::slot::KeySegment;
use crate::tree::{KeyTree, OwnedNode};

/// The operation's stable id: the `om-author` command and its MCP tool
/// are named after it.
pub const COMPARE_DESIGNS: &str = "compare_designs";

/// How many possible matches by name one object lists at most; the rest
/// are counted in [`Comparison::possible_matches_left_out`].
pub const NAME_CANDIDATES: usize = 8;

/// How two versions of an object differ: which kinds of piece differ,
/// decided from their bytes. All false: the same (modification stamps may
/// differ).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Difference {
    pub design: bool,
    /// The step data of a script or a button: compared as bytes, not read.
    pub step_data: bool,
    /// A piece no rule names.
    pub unidentified: bool,
    pub data: bool,
    pub derived: bool,
}

impl Difference {
    /// The parts' names in a report, in this order.
    pub const PARTS: [&'static str; 5] = ["design", "step_data", "unidentified", "data", "derived"];

    /// The difference between two objects' class digests.
    pub(crate) fn of(a: &[[u8; 32]; 5], b: &[[u8; 32]; 5]) -> Self {
        let differs = |class: Class| a[class as usize] != b[class as usize];
        Difference {
            design: differs(Class::Design),
            step_data: differs(Class::StepData),
            unidentified: differs(Class::Unidentified),
            data: differs(Class::Data),
            derived: differs(Class::Derived),
        }
    }

    /// Whether nothing differs.
    pub fn is_same(self) -> bool {
        self == Difference::default()
    }

    /// The names of the parts that differ, in [`Difference::PARTS`] order;
    /// none for the same.
    pub fn parts(self) -> Vec<&'static str> {
        let flags = [
            self.design,
            self.step_data,
            self.unidentified,
            self.data,
            self.derived,
        ];
        Self::PARTS
            .into_iter()
            .zip(flags)
            .filter_map(|(name, set)| set.then_some(name))
            .collect()
    }

    /// Whether a side that differs so from the base changed the object:
    /// its Design, its step data or its unidentified pieces. A difference
    /// only in Data or Derived content is no change: a serial field's next
    /// value moves with new records, a checksum with the file.
    pub fn is_change(self) -> bool {
        self.design || self.step_data || self.unidentified
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

/// Why a comparison could not be made. It names a file, a kind, a table
/// and an id at most, the id only when the key has its specified shape;
/// never a key's bytes or any content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareError {
    /// A file could not be read as a FileMaker file.
    Read { which: Which, failure: ReadFailure },
    /// A tree deeper than a file's keys can be (file-format.md 2.2).
    TooDeep { which: Which },
    /// A file whose design objects are not trusted ([`inventory`]).
    Identity { which: Which, error: IdentityError },
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
            CompareError::Identity { which, error } => {
                write!(
                    f,
                    "In the {} file, {error}; nothing is compared.",
                    which.name()
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
    /// A file's objects of a checked inventory, classified and digested.
    fn of(found: &[Checked<'_>], which: Which) -> Result<Side, CompareError> {
        let mut entries = Vec::with_capacity(found.len());
        for object in found {
            let classified = classify(object).map_err(|refusal| match refusal {
                Refusal::TooDeep => CompareError::TooDeep { which },
                // The inventory refused these already.
                Refusal::Misread => CompareError::Identity {
                    which,
                    error: IdentityError {
                        kind: Some(object.kind()),
                        table: object.table().and_then(crate::serial::decode),
                        id: object.id(),
                        problem: IdentityProblem::Misread,
                    },
                },
            })?;
            entries.push(Entry {
                key: (
                    object.kind(),
                    object.is_catalog(),
                    object.table().map(<[u8]>::to_vec),
                    object.key().to_vec(),
                ),
                ident: ident(object, &classified),
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
}

/// How a report names `object`: its name only where it is one Design
/// piece.
fn ident(object: &Object<'_>, classified: &Classified<'_>) -> Ident {
    let name = (object.is_catalog() || classified.design_name())
        .then(|| object.name())
        .flatten();
    Ident {
        kind: object.kind(),
        catalog: object.is_catalog(),
        table: object.table().and_then(crate::serial::decode),
        id: object.id(),
        name,
    }
}

/// Whether two objects with one key are one: with a base, when the base
/// has it too; without, when their names agree, or neither has one and
/// every class of their content is equal.
fn one_object(s: &Entry, t: &Entry, base: Option<&Side>) -> bool {
    match base {
        Some(base) => base.get(&s.key).is_some(),
        None => match (&s.ident.name, &t.ident.name) {
            (Some(a), Some(b)) => a == b,
            (None, None) => s.digests == t.digests,
            _ => false,
        },
    }
}

/// Compare the designs of `source` and `target`, with `base` when given:
/// trees as [`design_tree`] reads them from files, or whole key trees.
pub fn compare(
    source: &OwnedNode,
    target: &OwnedNode,
    base: Option<&OwnedNode>,
) -> Result<Comparison, CompareError> {
    // Every input's identities first: nothing is classified, named or
    // hashed until all have passed.
    let check =
        |root, which| inventory(root).map_err(|error| CompareError::Identity { which, error });
    let source_objects = check(source, Which::Source)?;
    let target_objects = check(target, Which::Target)?;
    let base_objects = base.map(|b| check(b, Which::Base)).transpose()?;
    let source = Side::of(&source_objects, Which::Source)?;
    let target = Side::of(&target_objects, Which::Target)?;
    let base = base_objects
        .as_deref()
        .map(|b| Side::of(b, Which::Base))
        .transpose()?;
    Ok(compare_sides(&source, &target, base.as_ref()))
}

fn compare_sides(source: &Side, target: &Side, base: Option<&Side>) -> Comparison {
    // Which tables are matched, by the test every object is matched by.
    let tables: BTreeSet<Vec<u8>> = source
        .entries
        .iter()
        .filter(|s| s.key.0 == Kind::Table && !s.key.1)
        .filter(|s| target.get(&s.key).is_some_and(|t| one_object(s, t, base)))
        .map(|s| s.key.3.clone())
        .collect();
    let table_matched = |key: &Key| match (&key.0, &key.2) {
        (Kind::Field, Some(table)) => tables.contains(table),
        _ => true,
    };
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
        if table_matched(&s.key) && one_object(s, t, base) {
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
    let unmatched = |side: &Side, matched: &[bool]| -> Vec<usize> {
        (0..side.entries.len()).filter(|&i| !matched[i]).collect()
    };
    let source_alone = unmatched(source, &source_matched);
    let target_alone = unmatched(target, &target_matched);
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
            let Some(bucket) = by_name.get(&(s.key.0, s.key.1, s.key.2.as_deref(), name)) else {
                continue;
            };
            // The bucket holds the target with this object's key when that
            // one is unmatched and of the same name: an id candidate, not a
            // name one.
            let same_key = target.by_key.get(&s.key).is_some_and(|&ti| {
                !target_matched[ti] && target.entries[ti].ident.name == s.ident.name
            });
            let eligible = bucket.len() - usize::from(same_key);
            let listed = eligible.min(NAME_CANDIDATES);
            for t in bucket.iter().filter(|t| t.key != s.key).take(listed) {
                out.possible_matches.push(possible(s, t, Agreement::Name));
            }
            out.possible_matches_left_out = out
                .possible_matches_left_out
                .saturating_add(eligible - listed);
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
/// one), its keys on the way read as the decoders read them, by number, so
/// that the inventory sees, and refuses, a key they could take for a
/// catalog's ([`on_the_way`]): [`design_tree`] keeps only these.
fn design_slot(path: &[KeySegment<'_>]) -> bool {
    CATALOGS
        .iter()
        .filter(|c| c.group == Group::Design)
        .any(|c| c.path.iter().zip(path).all(|(seg, s)| on_the_way(seg, s.0)))
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
