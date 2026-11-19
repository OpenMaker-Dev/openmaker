//! Manage Database's edits to the schema catalogs, made the way FileMaker
//! Pro makes them on its own copy of the catalogs before it uploads them
//! (docs/re/schema-path.md 1, 5, 7.2, 8). A [`SchemaDraft`] holds the base
//! tables `[3, 16]`, the occurrences `[3, 17]`, the relationships
//! `[3, fb]` and the field catalog `[T, 3]` of each table as the host
//! stores them; each [`SchemaChange`] is applied to the draft whole or not
//! at all, and [`SchemaDraft::finish`] adds the UUIDs, modification stamps
//! and deferred record work the save needs and lists the catalogs to
//! upload, in FileMaker Pro's commit order.
//!
//! What each change writes was read from FileMaker Pro 26.0.3's own saves
//! of the same change in copies of the Contacts starter solution, compared
//! key by key (schema-path.md 5.8): a new table gets an occurrence of the
//! same name, an empty field catalog and its entry in the dependency index;
//! a renamed table renames the occurrences that had its name; a deleted
//! table keeps its occurrences unless they are removed too; a deleted
//! member leaves the display order renumbered without gaps; a deleted
//! occurrence takes its relationships, its neighbours' relationship lists,
//! its routes and its key-field reach with it. Every other key, known or
//! not, is kept byte for byte.
//!
//! Refused (FileMaker error 3, nothing changed), because what FileMaker Pro
//! writes for them is not established: deleting a field that a
//! calculation, an auto-enter option, a summary or a relationship uses or
//! that has such options itself; changing the type of a field with
//! auto-enter options; changing calculation and summary fields. Names must
//! be ones a calculation can use as they are (`field_catalog::check_name`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use om_hbam::slot::be_uint;
use om_hbam::{serial, OwnedNode};

use crate::calc_field::{self, type_code, CalculationField, CompiledFormula, FormulaSettings};
use crate::calc_triggers::{self, Triggers};
use crate::field_catalog::{self, Refusal};
use crate::field_options::{
    self, dependencies, AutoEnterValue, FieldOptions, Indexing, Lookup, Message, NoMatch,
    OptionFormula, OptionMembers, AUTO_SOURCES,
};
use crate::schema::DUPLICATE_OFFSET;
use crate::{DataError, FieldKind, RelationshipMatch, SchemaChange};

/// The schema catalog `[3]` and its three catalogs: base tables, table
/// occurrences and relationships; a table's field catalog `[T, 3]`.
pub const SCHEMA: u8 = 0x03;
pub const TABLES: u8 = 0x10;
pub const OCCURRENCES: u8 = 0x11;
pub const RELATIONSHIPS: u8 = 0xfb;
pub const FIELDS: u8 = 0x03;

/// A catalog's index and members, and in the index the last issued id, the
/// name list, the display order, the member count, the field catalogs'
/// name list by type, the occurrences' routes, the dependency index and
/// the deferred field work (schema-path.md 5.1, 5.3, 5.5, 7.2).
const INDEX: u8 = 0x01;
const MEMBERS: u8 = 0x05;
const LAST_ID: u8 = 0x00;
const NAMES: u8 = 0x01;
const ORDER: u8 = 0x03;
const COUNT: u8 = 0x04;
const TYPED_NAMES: u8 = 0x06;
const ROUTES: u8 = 0x08;
const DEPENDENCIES: u8 = 0x0e;
const DEFERRED: u8 = 0xfd;
/// A member's definition, comment and name; a base table's data version
/// and an occurrence's list of relationships (`fb`); UUIDs.
const DEFINITION: u8 = 0x02;
/// A field's next serial value and its auto-entered data, as text.
const NEXT_SERIAL: u8 = 0x0c;
/// A calculation's formula `05`: its context `04`, formatted stream `05`
/// and Polish stream `06` (calc-encoding.md 1).
const FORMULA: u8 = 0x05;
const FORMULA_CONTEXT: u8 = 0x04;
const FORMULA_TEXT: u8 = 0x05;
const FORMULA_POLISH: u8 = 0x06;
/// A summary field's definition `09` (file-metadata.md 5.1).
const SUMMARY: u8 = 0x09;
/// `fffb, F` in the dependency index: the table's summary fields
/// (schema-path.md 5.11).
const SUMMARIES: &[u8] = &[0xff, 0xfb];
const AUTO_DATA: u8 = 0x0d;
/// Validation members: the value list `07`, the custom message `18`, the
/// validation calculation `19` and the range `21`, `22` (5.13).
const VALUE_LIST: u8 = 0x07;
/// A lookup's definition `08` and constant `0b`; in the field catalog's
/// dependency index, the related occurrences each lookup or related
/// auto-entered formula reads, `fb, F, <context>, <occurrence>` (5.13).
const LOOKUP: u8 = 0x08;
const LOOKUP_CONSTANT: u8 = 0x0b;
const RELATED_READS: u8 = 0xfb;
const MESSAGE: u8 = 0x18;
const VALIDATION_FORMULA: u8 = 0x19;
const MESSAGE_FORMULA: u8 = 0x20;
/// What FileMaker Pro 26 writes in `18` beside a calculated custom message,
/// for the versions that cannot show it (schema-path.md 5.9, E34).
const OLDER_VERSIONS_MESSAGE: &str = "The custom validation message for a field on this layout \
     is not available for this version of FileMaker software. To view the message, upgrade to \
     the latest version.";
const RANGE_FROM: u8 = 0x21;
const RANGE_TO: u8 = 0x22;
const COMMENT: u8 = 0x03;
const PREDICATES: u8 = 0x03;
const NAME: u8 = 0x10;
const RELATED: u8 = 0xfb;
const UNIQUE_ID: u8 = 0xd8;
/// A member's tags, "#tag " one after another (5.5).
const TAGS: u8 = 0xda;
/// Modification count, user, account and time (schema-path.md 8.1).
const MOD_COUNT: &[u8] = &[0xfc];
const MOD_USER: &[u8] = &[0xfc, 0x01];
const MOD_ACCOUNT: &[u8] = &[0xfc, 0x02];
const MOD_TIME: &[u8] = &[0xfc, 0x03];

/// In a table's entry of the occurrences' dependency index
/// `[3, 17, 1, 0e, T]`: the key fields' reach `00`, a field's own entry
/// `F` with its key-field marker `00`, and the field-derived lists that
/// begin `ff` (evaluation order `ff`, creation `ff00`, modification
/// `ff02`, `ff03`, containers `fffa`, auto-entered `fffc`, ...).
const REACH: u8 = 0x00;
const KEY_MARKER: u8 = 0x00;
/// A key field's marker is `03 21` and the field as a nested key string,
/// whatever its relationships' operators (E20, E21; meaning of `03 21`
/// not established).
const KEY_MARKER_PREFIX: [u8; 2] = [0x03, 0x21];
const ORDER_LIST: &[u8] = &[0xff];
const TABLE_MARKER: &[u8] = &[0xff, 0x03];
const CONTAINERS: &[u8] = &[0xff, 0xfa];
const VALIDATED: &[u8] = dependencies::VALIDATED;
/// A field catalog's dependency index `[T, 3, 1, 0e, fff0]`, and the
/// validation calculations' inputs `fff1` (schema-path.md 5.13).
const FIELD_DEPENDENCIES: &[u8] = &[0xff, 0xf0];
const VALIDATION_DEPENDENCIES: &[u8] = &[0xff, 0xf1];

/// `[3, 17, 1, 0e, T, ff03]` of every table FileMaker Pro makes.
const NEW_TABLE_MARKER: [u8; 3] = [0x02, 0x00, 0x0a];
/// A new occurrence's box colour (schema-path.md 5.3: `77 77 77 ff` in
/// FileMaker Pro 26's own new occurrences).
const NEW_OCCURRENCE_COLOR: [u8; 4] = [0x77, 0x77, 0x77, 0xff];

/// Field definition: kind, data type, option word (schema-path.md 5.5).
const KIND_NORMAL: u8 = 1;
const KIND_SUMMARY: u8 = 3;
const OPTIONS: std::ops::Range<usize> = 8..12;
/// The repetitions, `mMaxReps`.
const REPETITIONS: std::ops::Range<usize> = 24..26;
const VALIDATION: std::ops::Range<usize> = 12..16;
/// Auto-enter options: creation/modification value, serial, data,
/// calculation, last visited record, lookup.
const AUTO_ENTER: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x400;
const GLOBAL: u32 = 0x1_0000;
/// "Use Furigana Field" (schema-path.md 5.5, 5.9).
const FURIGANA_OPTION: u32 = 0x40;
const INDEXED: u32 = 0x8000_0000 | 0x2000_0000;

/// Deferred field work (schema-path.md 7.2).
const WORK_DELETE: u32 = 0x1;
/// Delete the repetitions above a reduced maximum.
const WORK_REPETITIONS: u32 = 0x4;
/// Set with `0x1` on every deleted field (`0x1e1` for a normal text
/// field; live:s2c).
const WORK_DELETED: u32 = 0x1e0;
const WORK_GLOBAL: u32 = 0x2;
const WORK_CONTAINER: u32 = 0x8;
const WORK_INDEX: u32 = 0x10;
const WORK_DROP_INDEX: u32 = 0x20;
const WORK_CONVERT: u32 = 0x40;
const WORK_MEMBER: u32 = 0x200;
const WORK_TO_DATE: u32 = 0x400;
const WORK_TO_TIMESTAMP: u32 = 0x800;
/// Kind, data type, auto-enter, validation or formula changed.
const WORK_DEFINITION: u32 = 0x80;
/// The repetition count was reduced.
const WORK_FEWER_REPETITIONS: u32 = 0x1000;
/// The work a storage change asks for, replaced as the storage is changed
/// again in the same edit.
const STORAGE_WORK: u32 = WORK_DELETE | WORK_GLOBAL | WORK_CONTAINER | WORK_CONVERT;

/// FileMaker's errors: 3 "Command is unavailable", 12 a name already in
/// use (as OpenMaker's authoring extension answers it), 102 "Field is
/// missing", 106 "Table is missing", 1300 "The specified name can't be
/// used".
const UNSUPPORTED: i32 = 3;
const NAME_EXISTS: i32 = 12;
const FIELD_MISSING: i32 = 102;
const TABLE_MISSING: i32 = 106;
const NAME_UNUSABLE: i32 = 1300;

/// The catalogs a draft edits, as the host stores them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaCatalogs {
    /// `[3, 16]`, `[3, 17]` and `[3, fb]`, each with its segment; `None`
    /// for a catalog the file does not have yet.
    pub tables: Option<OwnedNode>,
    pub occurrences: Option<OwnedNode>,
    pub relationships: Option<OwnedNode>,
    /// Each table's field catalog `[T, 3]` (segment `03`), by the table's
    /// key segment (`80 02`).
    pub fields: BTreeMap<Vec<u8>, OwnedNode>,
}

impl SchemaCatalogs {
    /// The catalogs as a key tree rooted at the file's root: `[3, ...]`
    /// and `[T, 3]`, for decoding the draft as the host would store it.
    pub fn to_root(&self) -> OwnedNode {
        let mut root = OwnedNode::list(Vec::new(), Vec::new());
        for catalog in [&self.tables, &self.occurrences, &self.relationships]
            .into_iter()
            .flatten()
        {
            root.put(&[&[SCHEMA], &catalog.segment], catalog.clone());
        }
        for (table, catalog) in &self.fields {
            root.put(&[table, &[FIELDS]], catalog.clone());
        }
        root
    }
}

/// Which catalog: the three of `[3]`, or a table's fields.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogPath {
    Tables,
    Occurrences,
    Relationships,
    Fields(Vec<u8>),
}

impl CatalogPath {
    /// The catalog's key path in the file.
    pub fn path(&self) -> Vec<Vec<u8>> {
        match self {
            CatalogPath::Tables => vec![vec![SCHEMA], vec![TABLES]],
            CatalogPath::Occurrences => vec![vec![SCHEMA], vec![OCCURRENCES]],
            CatalogPath::Relationships => vec![vec![SCHEMA], vec![RELATIONSHIPS]],
            CatalogPath::Fields(table) => vec![table.clone(), vec![FIELDS]],
        }
    }
}

/// Who saves and when: the modification stamp of every member and catalog
/// the save changes (schema-path.md 8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The user name (`Get ( UserName )`).
    pub user: String,
    pub account: String,
    /// The host's date-time key from op82 (8 bytes, schema-path.md 3.1).
    pub time: Vec<u8>,
}

/// What a save uploads and asks of the host, in FileMaker Pro's order
/// (schema-path.md 2.3, 2.3a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitPlan {
    /// Tables to remove with DeleteTable, before any catalog goes up.
    pub deleted_tables: Vec<Vec<u8>>,
    /// Catalogs to upload whole (op13), base tables first, then
    /// relationships, occurrences and the field catalogs.
    pub catalogs: Vec<(CatalogPath, OwnedNode)>,
    /// Tables whose field catalog goes up: each is locked (`[T, 5]`) for
    /// the save and gets op73 after its catalog.
    pub field_tables: Vec<Vec<u8>>,
}

/// A member a draft added or changed, to stamp.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MemberRef {
    catalog: CatalogPath,
    key: Vec<u8>,
}

/// Manage Database's edits on its copy of the catalogs.
#[derive(Clone, Debug)]
pub struct SchemaDraft {
    base: SchemaCatalogs,
    current: SchemaCatalogs,
    created: BTreeSet<MemberRef>,
    modified: BTreeSet<MemberRef>,
    /// Catalogs whose contents changed.
    changed: BTreeSet<CatalogPath>,
    /// Field catalogs this draft made, for tables it made.
    new_catalogs: BTreeSet<Vec<u8>>,
    /// Tables the host has that the draft deleted.
    deleted_tables: BTreeSet<Vec<u8>>,
    /// Deferred field work by table and field (schema-path.md 7.2).
    work: BTreeMap<(Vec<u8>, Vec<u8>), u32>,
    /// Tables whose stored data a change affects: their data version
    /// `[3, 16, 5, T, fb]` goes up at the save.
    data_changed: BTreeSet<Vec<u8>>,
    /// Fields whose data mod count `[T, 3, 5, F, fb]` goes up.
    field_data_changed: BTreeSet<(Vec<u8>, Vec<u8>)>,
    /// The file's decimal separator and functions, for formulas.
    formulas: FormulaSettings,
    /// Tables the provider found holding no record when the edit began
    /// ([`SchemaDraft::with_empty_tables`]).
    empty_tables: BTreeSet<Vec<u8>>,
    /// The host converts stored values for a type change as FileMaker
    /// Server does ([`SchemaDraft::with_host_conversion`]).
    host_converts: bool,
}

/// The error for a change the draft refuses.
fn refuse(message: impl Into<String>) -> DataError {
    DataError::new(message.into())
}

fn unsupported(message: impl Into<String>) -> DataError {
    DataError::with_code(UNSUPPORTED, message.into())
}

fn refusal(error: Refusal) -> DataError {
    match error {
        Refusal::Unsupported(reason) => unsupported(reason),
        Refusal::Exists { .. } => DataError::with_code(NAME_EXISTS, error.to_string()),
        other => DataError::with_code(NAME_UNUSABLE, other.to_string()),
    }
}

/// A name another member of the catalog has.
fn taken(message: String) -> DataError {
    DataError::with_code(NAME_EXISTS, message)
}

/// A key segment's catalog id as references give it: the big-endian
/// reading of the segment (`80 02` = 0x8002).
pub fn id_of(key: &[u8]) -> Option<u64> {
    be_uint(key)
}

/// The key segment of id `id` (the shortest big-endian bytes).
pub fn key_of(id: u64) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(7);
    bytes[skip..].to_vec()
}

/// `[u8 length][key]`: how catalogs store a member id in a value.
fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The key a key string holds.
fn key_string_key(value: &[u8]) -> Option<&[u8]> {
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len()).then_some(key)
}

/// `[u8 total][key string]`: how definitions store a member id.
fn nested_key_string(key: &[u8]) -> Vec<u8> {
    let inner = key_string(key);
    let mut out = vec![inner.len() as u8];
    out.extend(inner);
    out
}

/// One nested key string from the front of `input`.
fn read_nested_key<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (&total, rest) = input.split_first()?;
    let inner = rest.get(..usize::from(total))?;
    *input = rest.get(usize::from(total)..)?;
    key_string_key(inner)
}

/// The key `name` is listed under in `[cat, 1, 01]` (schema-path.md 5.6).
/// A name [`check_name`] passed always has one; a name already in the file
/// may not.
fn listed(name: &str) -> Result<Vec<u8>, DataError> {
    field_catalog::listed_key(name).ok_or_else(|| unkeyed(name))
}

/// A field's key in the name list by type `[T, 3, 1, 06]`
/// ([`field_catalog::typed_key`]).
fn typed(kind: u8, data_type: u8, global: bool, name: &str) -> Result<Vec<u8>, DataError> {
    field_catalog::typed_key(kind, data_type, global, name).ok_or_else(|| unkeyed(name))
}

/// The refusal for a name whose name list key OpenMaker cannot build.
fn unkeyed(name: &str) -> DataError {
    unsupported(format!(
        "OpenMaker cannot yet build the key FileMaker lists “{name}” under: FileMaker's weights \
         for one of its characters are not established."
    ))
}

/// Remove the entries of name list `list` that name member `key`, under
/// whatever key they are listed (one FileMaker or an earlier OpenMaker
/// wrote, or one OpenMaker cannot build).
fn unlist_member(list: &mut OwnedNode, key: &[u8]) {
    let value = key_string(key);
    list.children
        .retain(|entry| entry.value.as_deref() != Some(value.as_slice()));
}

fn text_of(node: &OwnedNode) -> Option<String> {
    om_scripts::node_text(node)
}

/// What the host does to a field's stored values when its type changes
/// from `old` to `new` (type codes; schema-path.md 7.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueWork {
    /// Every stored text stays and is read as the new type: among text,
    /// number and time, from a date or a timestamp to any of those, and
    /// between date and timestamp (which set neither `0x400` nor `0x800`).
    Kept,
    /// Rewritten as dates or timestamps (`0x400`, `0x800`) from text,
    /// number or time, or deleted with their container data for a change
    /// from a container (`0x8`): work the host does, which OpenMaker's own
    /// host for a file on this computer does and a hosted file's may not
    /// ([`SchemaDraft::with_host_conversion`], #1267).
    Converted,
    /// Kept as text in a container field, which reads it as a container
    /// only when it ends in U+E000: what FileMaker Pro shows for other text
    /// there is not established (#1268). So too for a type code OpenMaker
    /// does not know.
    NotEstablished,
}

fn value_work(old: u8, new: u8) -> ValueWork {
    let known = |code: u8| (1..=6).contains(&code);
    if old == new {
        ValueWork::Kept
    } else if !known(old) || !known(new) || new == 6 {
        ValueWork::NotEstablished
    } else if old == 6 || (matches!(old, 1 | 2 | 4) && matches!(new, 3 | 5)) {
        ValueWork::Converted
    } else {
        ValueWork::Kept
    }
}

/// The members of catalog `catalog`.
fn members(catalog: &OwnedNode) -> &[OwnedNode] {
    catalog
        .child(&[MEMBERS])
        .map_or(&[], |m| m.children.as_slice())
}

fn member_name(member: &OwnedNode) -> Option<String> {
    member.child(&[NAME]).and_then(text_of)
}

/// The member of `catalog` whose name OpenMaker takes for `name`
/// ([`field_catalog::same_names`]: neither case nor accents tell them
/// apart), other than `except`.
fn named<'a>(catalog: &'a OwnedNode, name: &str, except: Option<&[u8]>) -> Option<&'a OwnedNode> {
    members(catalog).iter().find(|member| {
        Some(member.segment.as_slice()) != except
            && member_name(member).is_some_and(|n| field_catalog::same_names(&n, name))
    })
}

/// Refuse writing `entry` into list `list` of `catalog`'s index for member
/// `key` when the list holds it for another member: [`named`] keeps names
/// that share a key out, and this keeps a name OpenMaker could not key, or
/// a list another writer left, from losing its entry.
fn check_free(catalog: &OwnedNode, list: u8, entry: &[u8], key: &[u8]) -> Result<(), DataError> {
    match catalog
        .at(&[&[INDEX], &[list], entry])
        .and_then(|e| e.value.as_deref())
    {
        Some(held) if held != key_string(key) => Err(taken(
            "Another member is listed under the key of this name; OpenMaker does not write \
             over its entry."
                .into(),
        )),
        _ => Ok(()),
    }
}

/// What `copy`, a duplicate made through the Create and options paths,
/// holds differently from `expected`, FileMaker Pro's copy of the original
/// member (schema-path.md 5.14), in words; `None` when it holds the same.
/// The copy's own name, UUID and bookkeeping (`10`, `d8`, `d9`, `fc`...)
/// and data version `fb` are not compared, nor the container library flags
/// `0x4000` and `0x8000` FileMaker clears in the definition.
/// The bytes of the two sort orders a relationship's definition stores, in
/// the order Edit Relationship gives them: the left occurrence's records',
/// then the right's (the definition holds the right's first,
/// file-metadata.md 4.1). `None` when they do not read.
fn stored_orders(definition: &[u8]) -> Option<[&[u8]; 2]> {
    let mut rest = definition;
    read_nested_key(&mut rest)?;
    read_nested_key(&mut rest)?;
    let sorts = rest.get(..rest.len().checked_sub(4)?)?;
    let (_, first) = om_scripts::specs::SortSpec::read(sorts)?;
    let (_, second) = om_scripts::specs::SortSpec::read(sorts.get(first..)?)?;
    (first + second == sorts.len()).then_some([&sorts[first..], &sorts[..first]])
}

fn duplicate_difference(expected: &OwnedNode, copy: &OwnedNode) -> Option<String> {
    const LIBRARY_FLAGS: u32 = 0x4000 | 0x8000;
    const OWN: [&[u8]; 8] = [
        &[NAME],
        &[UNIQUE_ID],
        &[0xd9],
        &[RELATED],
        MOD_COUNT,
        MOD_USER,
        MOD_ACCOUNT,
        MOD_TIME,
    ];
    let keys: BTreeSet<&[u8]> = expected
        .children
        .iter()
        .chain(&copy.children)
        .map(|c| c.segment.as_slice())
        .filter(|key| !OWN.contains(key))
        .collect();
    if expected.value != copy.value {
        return Some("its member's own value".into());
    }
    for key in keys {
        let (a, b) = (expected.child(key), copy.child(key));
        if key == [DEFINITION] {
            // Only the 26 bytes are compared below: anything else the
            // definition node holds would be lost.
            let shape = |n: Option<&OwnedNode>| n.map(|n| (n.push, sorted(n).children));
            if shape(a) != shape(b) {
                return Some("its definition".into());
            }
            let word = |d: &[u8], at: std::ops::Range<usize>| {
                d.get(at)
                    .map(|w| u32::from_be_bytes(w.try_into().expect("4 bytes")))
            };
            let (a, b) = (
                a.and_then(|n| n.value.as_deref()).unwrap_or_default(),
                b.and_then(|n| n.value.as_deref()).unwrap_or_default(),
            );
            if a.len() != b.len() {
                return Some("its definition".into());
            }
            if word(a, 4..8) != word(b, 4..8) {
                return Some("its index language".into());
            }
            let options = |d| word(d, OPTIONS).map(|o| o & !LIBRARY_FLAGS);
            if options(a) != options(b) {
                let differ = options(a).unwrap_or(0) ^ options(b).unwrap_or(0);
                return Some(format!("its options (0x{differ:08x})"));
            }
            let rest = |d: &[u8]| [&d[..4], &d[12..]].concat();
            if a.len() >= 12 && rest(a) != rest(b) {
                return Some("its definition".into());
            }
            continue;
        }
        if a.map(sorted) != b.map(sorted) {
            let what = match key {
                [COMMENT] => "its comment",
                [FORMULA] => "its formula",
                [VALUE_LIST] => "its value list",
                [LOOKUP] | [LOOKUP_CONSTANT] => "its lookup",
                [SUMMARY] => "its summary definition",
                [NEXT_SERIAL] => "its next serial value",
                [AUTO_DATA] => "its auto-entered data",
                [MESSAGE] | [MESSAGE_FORMULA] => "its validation message",
                [VALIDATION_FORMULA] => "its validation calculation",
                [RANGE_FROM] | [RANGE_TO] => "its range",
                _ => {
                    let key: String = key.iter().map(|b| format!("{b:02x}")).collect();
                    return Some(format!("its member {key}"));
                }
            };
            return Some(what.into());
        }
    }
    None
}

/// A field's tags `da` as FileMaker's Duplicate gives them to the copy
/// (schema-path.md 5.14): without FileMaker's default-object tag `_FMI_0`,
/// and with its primary-key tag `^FMI*` turned into `^FMI_`. Tags are
/// written "#tag " one after another; `None` when none is left, as no `da`
/// [I].
fn duplicate_tags(tags: &str) -> Option<String> {
    let mut kept: Vec<&str> = tags.split(' ').filter(|t| !t.is_empty()).collect();
    if kept.contains(&"#^FMI*") {
        kept.retain(|t| *t != "#^FMI*");
        if !kept.contains(&"#^FMI_") {
            kept.push("#^FMI_");
        }
    }
    kept.retain(|t| *t != "#_FMI_0");
    (!kept.is_empty()).then(|| kept.iter().map(|t| format!("{t} ")).collect())
}

/// `node` with its children, at every level, in key order, so two nodes
/// written in different orders compare equal.
fn sorted(node: &OwnedNode) -> OwnedNode {
    let mut node = node.clone();
    node.children = node.children.iter().map(sorted).collect();
    node.children.sort_by(|a, b| a.segment.cmp(&b.segment));
    node.push = node.push || !node.children.is_empty();
    node
}

/// Check a table, field or occurrence name: the rules
/// `field_catalog::check_name` applies to field names, which FileMaker's
/// calculations need to name the object as it is.
fn check_name(name: &str, what: &str) -> Result<(), DataError> {
    check_name_as(name, what, false)
}

/// [`check_name`], letting a name pass that calculations would write as
/// `${…}` when `quoted`.
fn check_name_as(name: &str, what: &str, quoted: bool) -> Result<(), DataError> {
    field_catalog::check_name_as(name, quoted)
        .map(|_| ())
        .map_err(|e| match e {
            Refusal::Name(reason) => DataError::with_code(
                NAME_UNUSABLE,
                reason.replace("a field name", &format!("a {what} name")),
            ),
            other => refusal(other),
        })
}

/// The key after `catalog`'s last issued id and every member's, so an id
/// is never issued twice.
fn next_key(catalog: &OwnedNode) -> Result<Vec<u8>, DataError> {
    let last = match catalog.at(&[&[INDEX], &[LAST_ID]]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(key_string_key)
            .and_then(serial::decode)
            .ok_or_else(|| Catalog::missing("last issued id"))?,
        None => 0,
    };
    let highest = members(catalog)
        .iter()
        .filter_map(|m| serial::decode(&m.segment))
        .max()
        .unwrap_or(0);
    let next = last
        .max(highest)
        .checked_add(1)
        .ok_or_else(|| unsupported("The catalog has no ids left."))?;
    Ok(serial::encode(next))
}

/// Index operations every catalog shares (schema-path.md 5.1).
struct Catalog<'a> {
    node: &'a mut OwnedNode,
}

impl Catalog<'_> {
    fn index(&mut self) -> &mut OwnedNode {
        self.node.ensure(&[&[INDEX]])
    }

    fn missing(what: &str) -> DataError {
        unsupported(format!(
            "The catalog's index has no readable {what} (every file FileMaker made has one); \
             OpenMaker does not rebuild it."
        ))
    }

    fn next_key(&self) -> Result<Vec<u8>, DataError> {
        next_key(self.node)
    }

    fn count(&self) -> Result<u32, DataError> {
        match self.node.at(&[&[INDEX], &[COUNT]]) {
            Some(node) => node
                .value
                .as_deref()
                .and_then(|v| <[u8; 4]>::try_from(v).ok())
                .map(u32::from_be_bytes)
                .ok_or_else(|| Self::missing("member count")),
            None if members(self.node).is_empty() => Ok(0),
            None => Err(Self::missing("member count")),
        }
    }

    fn set_count(&mut self, count: u32) {
        self.index()
            .set_child(OwnedNode::leaf(vec![COUNT], count.to_be_bytes().to_vec()));
    }

    /// The display order `[cat, 1, 03]` as member keys, in order.
    fn order(&self) -> Result<Vec<Vec<u8>>, DataError> {
        let Some(order) = self.node.at(&[&[INDEX], &[ORDER]]) else {
            return Ok(Vec::new());
        };
        order
            .children
            .iter()
            .map(|entry| {
                <[u8; 4]>::try_from(entry.segment.as_slice())
                    .ok()
                    .and(entry.value.as_deref())
                    .and_then(key_string_key)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| Self::missing("display order"))
            })
            .collect()
    }

    /// Write the display order as positions 1, 2, ... without gaps, as
    /// FileMaker Pro leaves it after a deletion.
    fn set_order(&mut self, keys: &[Vec<u8>]) {
        let entries = keys
            .iter()
            .enumerate()
            .map(|(i, key)| OwnedNode::leaf((i as u32 + 1).to_be_bytes().to_vec(), key_string(key)))
            .collect();
        self.index()
            .set_child(OwnedNode::list(vec![ORDER], entries));
    }

    /// Add a member under `key` named `name`: the member, the last issued
    /// id, the name list, the end of the display order and the count.
    fn add(&mut self, key: &[u8], member: OwnedNode, name: Option<&str>) -> Result<(), DataError> {
        let count = self
            .count()?
            .checked_add(1)
            .ok_or_else(|| Self::missing("member count below its limit"))?;
        if name.is_some() && !members(self.node).is_empty() {
            for (list, what) in [(NAMES, "name list"), (ORDER, "display order")] {
                if self.node.at(&[&[INDEX], &[list]]).is_none() {
                    return Err(Self::missing(what));
                }
            }
        }
        let entry = name.map(listed).transpose()?;
        if let Some(entry) = &entry {
            check_free(self.node, NAMES, entry, key)?;
        }
        self.node.put(&[&[MEMBERS], key], member);
        let value = key_string(key);
        self.index()
            .set_child(OwnedNode::leaf(vec![LAST_ID], value.clone()));
        if let Some(entry) = entry {
            self.index()
                .ensure(&[&[NAMES]])
                .set_child(OwnedNode::leaf(entry, value));
            // A new member goes after the last position, as FileMaker Pro
            // adds it, without renumbering the others.
            let next = match self
                .node
                .at(&[&[INDEX], &[ORDER]])
                .and_then(|o| o.children.last())
                .and_then(|e| <[u8; 4]>::try_from(e.segment.as_slice()).ok())
            {
                Some(last) => u32::from_be_bytes(last)
                    .checked_add(1)
                    .ok_or_else(|| Self::missing("display order below its limit"))?,
                None => 1,
            };
            self.index().ensure(&[&[ORDER]]).set_child(OwnedNode::leaf(
                next.to_be_bytes().to_vec(),
                key_string(key),
            ));
        }
        self.set_count(count);
        Ok(())
    }

    /// Remove member `key`: the member, its name list entry, its place in
    /// the display order (renumbered) and one from the count. The last
    /// issued id stays.
    fn remove(&mut self, key: &[u8]) -> Result<OwnedNode, DataError> {
        let count = self.count()?;
        let order = self.order()?;
        let member = self
            .node
            .remove(&[&[MEMBERS], key])
            .ok_or_else(|| refuse("The definition no longer exists."))?;
        if let Some(names) = self.node.at_mut(&[&[INDEX], &[NAMES]]) {
            unlist_member(names, key);
        }
        if self.node.at(&[&[INDEX], &[ORDER]]).is_some() {
            let kept: Vec<Vec<u8>> = order.into_iter().filter(|k| k != key).collect();
            self.set_order(&kept);
        }
        self.set_count(count.saturating_sub(1));
        Ok(member)
    }

    /// Rename member `key` to `new` in the member and the name list, which
    /// a catalog with members must have.
    fn rename(&mut self, key: &[u8], new: &str) -> Result<(), DataError> {
        if self.node.at(&[&[INDEX], &[NAMES]]).is_none() {
            return Err(Self::missing("name list"));
        }
        let entry = listed(new)?;
        check_free(self.node, NAMES, &entry, key)?;
        let names = self.index().ensure(&[&[NAMES]]);
        unlist_member(names, key);
        names.set_child(OwnedNode::leaf(entry, key_string(key)));
        if let Some(member) = self.node.at_mut(&[&[MEMBERS], key]) {
            member.set_child(om_scripts::text_node(&[NAME], new));
        }
        Ok(())
    }
}

/// The comment child of a member: set, or removed when empty.
fn set_comment(member: &mut OwnedNode, comment: &str) {
    if comment.is_empty() {
        member.children.retain(|c| c.segment != [COMMENT]);
    } else {
        member.set_child(om_scripts::text_node(&[COMMENT], comment));
    }
}

fn comment_of(member: &OwnedNode) -> String {
    member
        .child(&[COMMENT])
        .and_then(text_of)
        .unwrap_or_default()
}

/// A relationship as the graph needs it: its key, its two occurrences and
/// the fields each side matches on.
#[derive(Clone, Debug)]
struct Edge {
    key: Vec<u8>,
    left: Vec<u8>,
    right: Vec<u8>,
}

/// A field's read through a related occurrence (an `fb` node, 5.13).
struct FbRead {
    table: Vec<u8>,
    field: Vec<u8>,
    context: Vec<u8>,
    /// The relationships from the context to the occurrence, in order.
    relationships: Vec<Vec<u8>>,
    /// The occurrences after the context, the occurrence read last.
    through: Vec<Vec<u8>>,
}

/// Keys along a path of the graph: its relationships, and the occurrences
/// after its start.
type GraphPath = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// The relationships and the occurrences after `from` on the way from
/// `from` to `to` in `graph` (FileMaker's graph has one way between two
/// occurrences); `None` when they are not connected.
fn graph_path(graph: &[Edge], from: &[u8], to: &[u8]) -> Option<GraphPath> {
    let mut previous: BTreeMap<Vec<u8>, (Vec<u8>, Vec<u8>)> = BTreeMap::new();
    let mut queue = VecDeque::from([from.to_vec()]);
    let mut seen = BTreeSet::from([from.to_vec()]);
    while let Some(at) = queue.pop_front() {
        if at == to {
            let mut relationships = Vec::new();
            let mut through = Vec::new();
            let mut node = at;
            while let Some((before, relationship)) = previous.get(&node) {
                relationships.push(relationship.clone());
                through.push(node.clone());
                node = before.clone();
            }
            relationships.reverse();
            through.reverse();
            return Some((relationships, through));
        }
        for edge in graph {
            let next = if edge.left == at {
                &edge.right
            } else if edge.right == at {
                &edge.left
            } else {
                continue;
            };
            if seen.insert(next.clone()) {
                previous.insert(next.clone(), (at.clone(), edge.key.clone()));
                queue.push_back(next.clone());
            }
        }
    }
    None
}

/// The relationships of `catalog` whose definition reads.
fn edges(catalog: Option<&OwnedNode>) -> Vec<Edge> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    members(catalog).iter().filter_map(edge_of).collect()
}

/// Relationship `member`'s two occurrences, if its definition reads.
fn edge_of(member: &OwnedNode) -> Option<Edge> {
    let definition = member.child(&[DEFINITION])?.assembled()?;
    let mut input = definition.as_ref();
    let left = read_nested_key(&mut input)?.to_vec();
    let right = read_nested_key(&mut input)?.to_vec();
    Some(Edge {
        key: member.segment.clone(),
        left,
        right,
    })
}

/// The option word of relationship definition `definition`
/// (file-metadata.md 4.1), if it reads.
fn edge_definition_options(definition: &[u8]) -> Option<u32> {
    crate::meta::Relationship::decode(0, definition, &[]).map(|r| r.options)
}

/// Relationship `member`'s match pairs with their operators, as
/// [`SchemaDraft::predicates`] writes them; `None` when one does not read.
fn stored_matches(member: &OwnedNode) -> Option<Vec<RelationshipMatch>> {
    member
        .child(&[PREDICATES])?
        .children
        .iter()
        .map(|leaf| {
            let value = leaf.assembled()?;
            let (&operator, mut rest) = value.split_first()?;
            let left = serial::decode(read_nested_key(&mut rest)?)?;
            let right = serial::decode(read_nested_key(&mut rest)?)?;
            Some(RelationshipMatch {
                left_field: left,
                right_field: right,
                operator,
            })
        })
        .collect()
}

/// A relationship's match fields: (left field, right field) per pair.
type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

/// The fields relationship `member` matches on, on its left and right;
/// `None` when a predicate does not read.
fn predicate_fields(member: &OwnedNode) -> Option<Pairs> {
    // Every relationship FileMaker stores has its predicates `03`, each an
    // operator 0 to 6 and two fields (file-metadata.md 4.2), as the
    // provider's decoder requires; anything else does not read.
    member
        .child(&[PREDICATES])?
        .children
        .iter()
        .map(|predicate| {
            let bytes = predicate.assembled()?;
            let (&operator, mut rest) = bytes.split_first()?;
            if operator > 6 {
                return None;
            }
            let left = read_nested_key(&mut rest)?.to_vec();
            let right = read_nested_key(&mut rest)?.to_vec();
            rest.is_empty().then_some((left, right))
        })
        .collect()
}

/// Every relationship of `catalog` with its match fields, or a refusal when
/// one does not read: a change that removes or relies on what a
/// relationship names is refused rather than made around a relationship
/// OpenMaker cannot see.
fn readable_graph(catalog: Option<&OwnedNode>) -> Result<Vec<(Edge, Pairs)>, DataError> {
    let Some(catalog) = catalog else {
        return Ok(Vec::new());
    };
    members(catalog)
        .iter()
        .map(|member| {
            edge_of(member)
                .zip(predicate_fields(member))
                .ok_or_else(|| {
                    unsupported(
                        "The file has a relationship OpenMaker cannot read, so it does not \
                         delete what that relationship may use.",
                    )
                })
        })
        .collect()
}

/// The occurrences connected to each occurrence (the graph is a forest:
/// FileMaker allows one path between two occurrences).
fn components(edges: &[Edge]) -> BTreeMap<Vec<u8>, usize> {
    let mut neighbours: BTreeMap<&[u8], Vec<&[u8]>> = BTreeMap::new();
    for edge in edges {
        neighbours.entry(&edge.left).or_default().push(&edge.right);
        neighbours.entry(&edge.right).or_default().push(&edge.left);
    }
    let mut component = BTreeMap::new();
    let mut next = 0;
    for start in neighbours.keys() {
        if component.contains_key(*start) {
            continue;
        }
        let mut queue = VecDeque::from([*start]);
        while let Some(node) = queue.pop_front() {
            if component.insert(node.to_vec(), next).is_some() {
                continue;
            }
            for other in &neighbours[node] {
                if !component.contains_key(*other) {
                    queue.push_back(other);
                }
            }
        }
        next += 1;
    }
    component
}

impl SchemaDraft {
    pub fn new(base: SchemaCatalogs) -> Self {
        Self {
            current: base.clone(),
            base,
            created: BTreeSet::new(),
            modified: BTreeSet::new(),
            changed: BTreeSet::new(),
            new_catalogs: BTreeSet::new(),
            deleted_tables: BTreeSet::new(),
            work: BTreeMap::new(),
            data_changed: BTreeSet::new(),
            field_data_changed: BTreeSet::new(),
            formulas: FormulaSettings::default(),
            empty_tables: BTreeSet::new(),
            host_converts: false,
        }
    }

    /// Compile and show formulas with the file's decimal separator and
    /// its custom and plug-in functions.
    pub fn with_formulas(mut self, formulas: FormulaSettings) -> Self {
        self.formulas = formulas;
        self
    }

    /// The tables (by key) that hold no record: a field of theirs that is
    /// not global changes its type or repetitions as a field made in this
    /// edit does, since it holds no value the host would have to convert or
    /// delete. The caller says so only where it can make sure at the save
    /// that they are still empty ([`SchemaDraft::relies_on_empty_tables`]).
    pub fn with_empty_tables(mut self, tables: BTreeSet<Vec<u8>>) -> Self {
        self.empty_tables = tables;
        self
    }

    /// The host rewrites stored values for a type change to date or
    /// timestamp, deletes them for one from a container, and deletes the
    /// repetitions above a reduced maximum, as FileMaker Server does
    /// (schema-path.md 7.2, 7.2.1): so a field that may hold values changes
    /// such a type, or its repetitions, as any field does. The caller says
    /// so for a file on this computer, whose host is OpenMaker's own; a
    /// hosted file's may be one that does not (#1267).
    pub fn with_host_conversion(mut self, converts: bool) -> Self {
        self.host_converts = converts;
        self
    }

    /// Whether a field's type change from `old` to `new` needs it to hold
    /// no value: its values would be left as the host cannot be relied on
    /// to leave them ([`ValueWork`]).
    fn needs_no_value(&self, old: u8, new: u8) -> bool {
        match value_work(old, new) {
            ValueWork::Kept => false,
            ValueWork::Converted => !self.host_converts,
            ValueWork::NotEstablished => true,
        }
    }

    /// The tables the draft's changes rely on holding no record, as they
    /// end up against the file: a field of theirs the file has gets
    /// another type that needs it to hold no value
    /// ([`Self::needs_no_value`]), or another number of repetitions where
    /// the host is not known to delete those above it. The save must check,
    /// under the tables' locks, that they still hold none.
    pub fn relies_on_empty_tables(&self) -> BTreeSet<Vec<u8>> {
        let mut tables = BTreeSet::new();
        for table in &self.empty_tables {
            let (Some(base), Some(current)) =
                (self.base.fields.get(table), self.current.fields.get(table))
            else {
                continue;
            };
            let relies = members(base).iter().any(|member| {
                let Some(now) = current.at(&[&[MEMBERS], &member.segment]) else {
                    return false;
                };
                let (Ok(old), Ok(new)) = (Self::definition_of(member), Self::definition_of(now))
                else {
                    return false;
                };
                old[0] == KIND_NORMAL
                    && (self.needs_no_value(old[1], new[1])
                        || (old[REPETITIONS] != new[REPETITIONS] && !self.host_converts))
            });
            if relies {
                tables.insert(table.clone());
            }
        }
        tables
    }

    /// Whether field `field` of `table`, which this edit did not make,
    /// holds no value: its table held no record when the edit began
    /// ([`SchemaDraft::with_empty_tables`]) and it is not global, in the
    /// file or in the draft (a global value is kept in the table's global
    /// record whatever records the table has, schema-path.md 7.2.1).
    fn holds_no_value(&self, table: &[u8], field: &[u8]) -> bool {
        let global = |catalogs: &SchemaCatalogs| {
            catalogs
                .fields
                .get(table)
                .and_then(|c| c.at(&[&[MEMBERS], field]))
                .and_then(|m| Self::definition_of(m).ok())
                .is_some_and(|d| {
                    u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes")) & GLOBAL != 0
                })
        };
        self.empty_tables.contains(table) && !global(&self.base) && !global(&self.current)
    }

    /// The file's decimal separator and functions this draft's formulas use.
    pub fn formulas(&self) -> &FormulaSettings {
        &self.formulas
    }

    /// The catalogs as the draft now has them.
    pub fn catalogs(&self) -> &SchemaCatalogs {
        &self.current
    }

    /// The catalogs the draft started from.
    pub fn base(&self) -> &SchemaCatalogs {
        &self.base
    }

    /// Whether any change has been made.
    pub fn is_changed(&self) -> bool {
        !self.changed.is_empty() || !self.deleted_tables.is_empty()
    }

    /// Apply `change` whole, or leave the draft as it was and say why.
    pub fn apply(&mut self, change: &SchemaChange) -> Result<(), DataError> {
        // A lookup OpenMaker's record rules perform stays one they perform,
        // whatever changes its field, match fields, copied field or
        // relationship: otherwise its table would become read-only.
        let before = self.performable_options();
        let mut next = self.clone();
        next.apply_inner(change)?;
        next.keep_options(&before)?;
        *self = next;
        Ok(())
    }

    /// The fields of every table whose lookup or formulas over related
    /// fields OpenMaker's record rules perform as they are.
    fn performable_options(&self) -> BTreeSet<(Vec<u8>, Vec<u8>)> {
        let mut performable = BTreeSet::new();
        for (table, catalog) in &self.current.fields {
            for member in members(catalog) {
                if self.related_options(table, &member.segment) == Some(true) {
                    performable.insert((table.clone(), member.segment.clone()));
                }
            }
        }
        performable
    }

    /// Whether the record rules perform field `field` of `table`'s options
    /// that read related records, its lookup and its auto-entered and
    /// validation formulas' related fields ([`Self::check_lookup`],
    /// [`Self::check_related_read`]); `None` when it has none.
    fn related_options(&self, table: &[u8], field: &[u8]) -> Option<bool> {
        let member = self.field_member(table, field).ok()?;
        let definition = Self::definition_of(member).ok()?;
        if definition[0] != KIND_NORMAL {
            return None;
        }
        let mut parts = Vec::new();
        let options_word = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        if options_word & field_options::AUTO_LOOKUP != 0 {
            let mut options = FieldOptions::decode(&definition, None, None)?;
            let names = om_formula::NameTable::default();
            options.read_members(&DraftMembers {
                member,
                names: &names,
                decimal_comma: false,
            });
            parts.push(match &options.auto_enter.value {
                AutoEnterValue::LookedUp(Some(lookup)) => self
                    .check_lookup(table, &definition, &options, lookup)
                    .is_ok(),
                _ => false,
            });
        }
        let catalog = self.fields(table).ok()?;
        for (list, formula) in [
            (FIELD_DEPENDENCIES, FORMULA),
            (VALIDATION_DEPENDENCIES, VALIDATION_FORMULA),
        ] {
            let Some(triggers) = catalog.at(&[&[INDEX], &[DEPENDENCIES], list, field]) else {
                continue;
            };
            let context = member
                .at(&[&[formula], &[FORMULA_CONTEXT]])
                .and_then(|n| n.assembled())
                .and_then(|k| om_formula::Key(k.into_owned()).id());
            for trigger in &triggers.children {
                if let Some((occurrence, read)) = calc_triggers::related_input(&trigger.segment) {
                    parts.push(context.is_some_and(|c| {
                        self.check_related_read(table, c, occurrence, read, "")
                            .is_ok()
                    }));
                }
            }
        }
        (!parts.is_empty()).then(|| parts.iter().all(|ok| *ok))
    }

    /// Refuse a change that leaves one of the fields `before` with a
    /// lookup or formula over related fields the record rules no longer
    /// perform.
    fn keep_options(&self, before: &BTreeSet<(Vec<u8>, Vec<u8>)>) -> Result<(), DataError> {
        for (table, field) in before {
            if self.related_options(table, field) == Some(false) {
                let name = self
                    .field_member(table, field)
                    .ok()
                    .and_then(member_name)
                    .unwrap_or_default();
                return Err(unsupported(format!(
                    "OpenMaker cannot yet make this change: the lookup or calculation of \
                     “{name}” would no longer be one OpenMaker performs, and its table would \
                     become read-only to OpenMaker."
                )));
            }
        }
        Ok(())
    }

    fn apply_inner(&mut self, change: &SchemaChange) -> Result<(), DataError> {
        match change {
            SchemaChange::CreateTable { name, comment } => self.create_table(name, comment),
            SchemaChange::ChangeTable {
                table_id,
                name,
                comment,
            } => self.change_table(&key_of(*table_id), name, comment),
            SchemaChange::DeleteTable {
                table_id,
                remove_occurrences,
            } => self.delete_table(&key_of(*table_id), *remove_occurrences),
            SchemaChange::CreateField {
                table_id,
                name,
                data_type,
                comment,
            } => {
                check_name(name, "field")?;
                self.create_field(&key_of(*table_id), name, *data_type, comment)
            }
            SchemaChange::ChangeField {
                table_id,
                field_id,
                name,
                data_type,
                comment,
            } => self.change_field(
                &key_of(*table_id),
                &key_of(*field_id),
                name,
                *data_type,
                comment,
            ),
            SchemaChange::DeleteField { table_id, field_id } => {
                self.delete_field(&key_of(*table_id), &key_of(*field_id))
            }
            SchemaChange::DuplicateField { table_id, field_id } => {
                self.duplicate_field(&key_of(*table_id), &key_of(*field_id))
            }
            SchemaChange::SetFieldOptions {
                table_id,
                field_id,
                options,
            } => self.set_field_options(&key_of(*table_id), &key_of(*field_id), options),
            SchemaChange::CreateCalculationField {
                table_id,
                name,
                comment,
                calculation,
            } => {
                check_name(name, "field")?;
                self.create_calculation_field(&key_of(*table_id), name, comment, calculation)
            }
            SchemaChange::ChangeCalculation {
                table_id,
                field_id,
                calculation,
            } => self.change_calculation(&key_of(*table_id), &key_of(*field_id), calculation),
            SchemaChange::CreateSummaryField {
                table_id,
                name,
                comment,
                summary,
            } => {
                check_name(name, "field")?;
                self.create_summary_field(&key_of(*table_id), name, comment, summary)
            }
            SchemaChange::ChangeSummary {
                table_id,
                field_id,
                summary,
            } => self.change_summary(&key_of(*table_id), &key_of(*field_id), summary),
            SchemaChange::CreateOccurrence { table_id, name } => {
                self.create_occurrence(&key_of(*table_id), name)
            }
            SchemaChange::RenameOccurrence {
                occurrence_id,
                name,
            } => self.rename_occurrence(&key_of(*occurrence_id), name),
            SchemaChange::DeleteOccurrence { occurrence_id } => {
                self.delete_occurrence(&key_of(*occurrence_id))
            }
            SchemaChange::CreateRelationship {
                left_occurrence,
                right_occurrence,
                matches,
                left,
                right,
            } => self.create_relationship(
                (&key_of(*left_occurrence), &key_of(*right_occurrence)),
                matches,
                left.bits(false) | right.bits(true),
                (None, [left.order.as_ref(), right.order.as_ref()]),
            ),
            SchemaChange::ChangeRelationship {
                relationship_id,
                matches,
                left,
                right,
            } => self.change_relationship(
                &key_of(*relationship_id),
                matches,
                left.bits(false) | right.bits(true),
                [left.order.as_ref(), right.order.as_ref()],
            ),
            SchemaChange::DeleteRelationship { relationship_id } => {
                self.delete_relationship(&key_of(*relationship_id))
            }
            SchemaChange::ArrangeOccurrence {
                occurrence_id,
                left,
                top,
                width,
                height,
            } => self.arrange_occurrence(&key_of(*occurrence_id), [*left, *top, *width, *height]),
            SchemaChange::ColorOccurrence {
                occurrence_id,
                color,
            } => self.color_occurrence(&key_of(*occurrence_id), *color),
            SchemaChange::DuplicateOccurrences { occurrence_ids } => {
                let sources: Vec<Vec<u8>> = occurrence_ids.iter().map(|id| key_of(*id)).collect();
                self.duplicate_occurrences(&sources)
            }
            SchemaChange::ArrangeOccurrences { frames } => {
                for (occurrence_id, frame) in frames {
                    self.arrange_occurrence(&key_of(*occurrence_id), *frame)?;
                }
                Ok(())
            }
        }
    }

    fn catalog_mut(&mut self, which: &CatalogPath) -> &mut OwnedNode {
        let segment = |s: u8| move || OwnedNode::list(vec![s], Vec::new());
        match which {
            CatalogPath::Tables => self.current.tables.get_or_insert_with(segment(TABLES)),
            CatalogPath::Occurrences => self
                .current
                .occurrences
                .get_or_insert_with(segment(OCCURRENCES)),
            CatalogPath::Relationships => self
                .current
                .relationships
                .get_or_insert_with(segment(RELATIONSHIPS)),
            CatalogPath::Fields(table) => self
                .current
                .fields
                .entry(table.clone())
                .or_insert_with(segment(FIELDS)),
        }
    }

    fn catalog(&mut self, which: CatalogPath) -> Catalog<'_> {
        self.changed.insert(which.clone());
        Catalog {
            node: self.catalog_mut(&which),
        }
    }

    fn touch(&mut self, catalog: CatalogPath, key: &[u8]) {
        let member = MemberRef {
            catalog,
            key: key.to_vec(),
        };
        if !self.created.contains(&member) {
            self.modified.insert(member);
        }
    }

    fn forget(&mut self, catalog: CatalogPath, key: &[u8]) {
        let member = MemberRef {
            catalog,
            key: key.to_vec(),
        };
        self.created.remove(&member);
        self.modified.remove(&member);
    }

    fn table(&self, table: &[u8]) -> Result<&OwnedNode, DataError> {
        self.current
            .tables
            .as_ref()
            .and_then(|c| c.at(&[&[MEMBERS], table]))
            .ok_or_else(|| DataError::with_code(TABLE_MISSING, "The table no longer exists."))
    }

    fn occurrence(&self, occurrence: &[u8]) -> Result<&OwnedNode, DataError> {
        self.current
            .occurrences
            .as_ref()
            .and_then(|c| c.at(&[&[MEMBERS], occurrence]))
            .ok_or_else(|| {
                DataError::with_code(TABLE_MISSING, "The table occurrence no longer exists.")
            })
    }

    /// The base table key an occurrence's definition names, if local.
    fn occurrence_table(member: &OwnedNode) -> Option<Vec<u8>> {
        let definition = member.child(&[DEFINITION])?.assembled()?;
        let mut input = definition.as_ref();
        let source = read_nested_key(&mut input)?;
        let table = read_nested_key(&mut input)?;
        (source == [0]).then(|| table.to_vec())
    }

    /// The occurrences' dependency index entry of table `table`.
    fn dependency_entry(&mut self, table: &[u8]) -> Option<&mut OwnedNode> {
        self.current
            .occurrences
            .as_mut()?
            .at_mut(&[&[INDEX], &[DEPENDENCIES], table])
    }

    fn create_table(&mut self, name: &str, comment: &str) -> Result<(), DataError> {
        check_name(name, "table")?;
        let catalog = self.catalog_mut(&CatalogPath::Tables);
        if let Some(other) = named(catalog, name, None) {
            return Err(taken(format!(
                "The table name “{}” is already used.",
                member_name(other).unwrap_or_default()
            )));
        }
        let key = Catalog { node: catalog }.next_key()?;
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(om_scripts::text_node(&[NAME], name));
        set_comment(&mut member, comment);
        member.set_child(OwnedNode::leaf(vec![RELATED], 1u32.to_be_bytes().to_vec()));
        self.catalog(CatalogPath::Tables)
            .add(&key, member, Some(name))?;
        self.created.insert(MemberRef {
            catalog: CatalogPath::Tables,
            key: key.clone(),
        });
        // Its empty field catalog, made with the table.
        let mut fields = OwnedNode::list(vec![FIELDS], Vec::new());
        fields.put(
            &[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES],
            OwnedNode::list(FIELD_DEPENDENCIES.to_vec(), Vec::new()),
        );
        fields.put(&[&[MEMBERS]], OwnedNode::list(vec![MEMBERS], Vec::new()));
        self.current.fields.insert(key.clone(), fields);
        self.new_catalogs.insert(key.clone());
        self.changed.insert(CatalogPath::Fields(key.clone()));
        // Its entry in the occurrences' dependency index.
        let mut entry = OwnedNode::list(key.clone(), Vec::new());
        entry.set_child(OwnedNode::list(ORDER_LIST.to_vec(), Vec::new()));
        entry.set_child(OwnedNode::leaf(
            TABLE_MARKER.to_vec(),
            NEW_TABLE_MARKER.to_vec(),
        ));
        self.catalog(CatalogPath::Occurrences)
            .index()
            .ensure(&[&[DEPENDENCIES]])
            .set_child(entry);
        // And an occurrence of it, named as the table unless an
        // occurrence has that name.
        let taken: Vec<String> = self
            .current
            .occurrences
            .iter()
            .flat_map(|c| members(c).iter().filter_map(member_name))
            .collect();
        let occurrence = crate::schema::unused_name(name, taken.iter());
        self.create_occurrence(&key, &occurrence)
    }

    fn change_table(&mut self, table: &[u8], name: &str, comment: &str) -> Result<(), DataError> {
        let old = member_name(self.table(table)?).unwrap_or_default();
        let old_comment = comment_of(self.table(table)?);
        if old == name && old_comment == comment {
            return Ok(());
        }
        if old != name {
            check_name(name, "table")?;
            let catalog = self.catalog_mut(&CatalogPath::Tables);
            if let Some(other) = named(catalog, name, Some(table)) {
                return Err(taken(format!(
                    "The table name “{}” is already used.",
                    member_name(other).unwrap_or_default()
                )));
            }
            self.catalog(CatalogPath::Tables).rename(table, name)?;
            // FileMaker Pro renames the occurrences that had the table's
            // name, unless another occurrence has the new one.
            let occurrences = self
                .current
                .occurrences
                .as_ref()
                .map(|c| {
                    let free = named(c, name, None).is_none();
                    members(c)
                        .iter()
                        .filter(|o| {
                            free && member_name(o).as_deref() == Some(old.as_str())
                                && Self::occurrence_table(o).as_deref() == Some(table)
                        })
                        .map(|o| o.segment.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for occurrence in occurrences {
                self.catalog(CatalogPath::Occurrences)
                    .rename(&occurrence, name)?;
                self.touch(CatalogPath::Occurrences, &occurrence);
            }
        }
        if let Some(member) = self
            .catalog(CatalogPath::Tables)
            .node
            .at_mut(&[&[MEMBERS], table])
        {
            set_comment(member, comment);
        }
        self.touch(CatalogPath::Tables, table);
        Ok(())
    }

    fn delete_table(&mut self, table: &[u8], remove_occurrences: bool) -> Result<(), DataError> {
        self.table(table)?;
        let outside: Vec<_> = self
            .related_readers(None, Some(table), None)
            .into_iter()
            .filter(|(reader, _)| reader != table)
            .collect();
        self.read_through(&outside)?;
        if remove_occurrences {
            let occurrences: Vec<Vec<u8>> = self
                .current
                .occurrences
                .iter()
                .flat_map(|c| members(c).iter())
                .filter(|o| Self::occurrence_table(o).as_deref() == Some(table))
                .map(|o| o.segment.clone())
                .collect();
            for occurrence in occurrences {
                self.delete_occurrence(&occurrence)?;
            }
        }
        self.catalog(CatalogPath::Tables).remove(table)?;
        self.forget(CatalogPath::Tables, table);
        let made_here = self.new_catalogs.remove(table);
        self.current.fields.remove(table);
        self.changed.remove(&CatalogPath::Fields(table.to_vec()));
        self.work.retain(|(t, _), _| t != table);
        self.data_changed.remove(table);
        self.field_data_changed.retain(|(t, _)| t != table);
        self.created
            .retain(|m| m.catalog != CatalogPath::Fields(table.to_vec()));
        self.modified
            .retain(|m| m.catalog != CatalogPath::Fields(table.to_vec()));
        if !made_here {
            self.deleted_tables.insert(table.to_vec());
        }
        // Its dependency index entry keeps only what its occurrences'
        // relationships derive (the key fields' reach and markers), as
        // FileMaker Pro leaves it while those occurrences stay.
        if let Some(entry) = self.dependency_entry(table) {
            entry.children.retain(|c| {
                c.segment == [REACH]
                    || (!c.segment.starts_with(&[0xff]) && c.child(&[KEY_MARKER]).is_some())
            });
            for child in &mut entry.children {
                if child.segment != [REACH] {
                    child.children.retain(|c| c.segment == [KEY_MARKER]);
                }
            }
            let empty = entry.children.is_empty();
            self.changed.insert(CatalogPath::Occurrences);
            if empty {
                if let Some(index) = self
                    .current
                    .occurrences
                    .as_mut()
                    .and_then(|c| c.at_mut(&[&[INDEX], &[DEPENDENCIES]]))
                {
                    index.remove(&[table]);
                }
            }
        }
        Ok(())
    }

    /// Table `table`'s field catalog, which every table FileMaker made has.
    fn fields(&self, table: &[u8]) -> Result<&OwnedNode, DataError> {
        self.table(table)?;
        self.current
            .fields
            .get(table)
            .ok_or_else(|| unsupported("The table has no field catalog [T, 3] OpenMaker can edit."))
    }

    /// The most common index language of the table's fields (English, 21,
    /// in a table without fields), as `field_catalog::plan` chooses it.
    fn language(catalog: &OwnedNode) -> u32 {
        let mut languages: Vec<u32> = members(catalog)
            .iter()
            .filter_map(|m| m.child(&[DEFINITION])?.value.clone())
            .filter(|d| d.len() == 26)
            .map(|d| u32::from_be_bytes(d[4..8].try_into().expect("4 bytes")))
            .collect();
        languages.sort_unstable();
        languages
            .chunk_by(|a, b| a == b)
            .max_by_key(|run| run.len())
            .map_or(21, |run| run[0])
    }

    fn create_field(
        &mut self,
        table: &[u8],
        name: &str,
        data_type: FieldKind,
        comment: &str,
    ) -> Result<(), DataError> {
        let catalog = self.fields(table)?;
        if let Some(other) = named(catalog, name, None) {
            return Err(taken(format!(
                "The table already has a field named “{}”.",
                member_name(other).unwrap_or_default()
            )));
        }
        let language = Self::language(catalog);
        let mut definition = field_catalog::definition(field_catalog::ScalarType::Text, language);
        definition[1] = type_code(data_type);
        let key = next_key(catalog)?;
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
        set_comment(&mut member, comment);
        member.set_child(om_scripts::text_node(&[NAME], name));
        let which = CatalogPath::Fields(table.to_vec());
        let has_typed = {
            let catalog = self.catalog_mut(&which);
            members(catalog).is_empty() || catalog.at(&[&[INDEX], &[TYPED_NAMES]]).is_some()
        };
        let typed_entry = typed(KIND_NORMAL, type_code(data_type), false, name)?;
        check_free(self.catalog_mut(&which), TYPED_NAMES, &typed_entry, &key)?;
        let mut fields = self.catalog(which.clone());
        fields.add(&key, member, Some(name))?;
        if has_typed {
            fields
                .index()
                .ensure(&[&[TYPED_NAMES]])
                .set_child(OwnedNode::leaf(typed_entry, key_string(&key)));
        }
        self.created.insert(MemberRef {
            catalog: which,
            key: key.clone(),
        });
        if data_type == FieldKind::Container {
            self.list_container(table, &key, true);
        }
        Ok(())
    }

    /// Add field `field` to, or take it from, the table's container list
    /// in the dependency index (`[3, 17, 1, 0e, T, fffa]`).
    fn list_container(&mut self, table: &[u8], field: &[u8], container: bool) {
        let mut occurrences = self.catalog(CatalogPath::Occurrences);
        let index = occurrences.index().ensure(&[&[DEPENDENCIES]]);
        if container {
            index
                .ensure(&[table, CONTAINERS])
                .set_child(OwnedNode::leaf(field.to_vec(), Vec::new()));
        } else if let Some(list) = index.at_mut(&[table, CONTAINERS]) {
            list.children.retain(|c| c.segment != field);
            if list.children.is_empty() {
                if let Some(entry) = index.child_mut(table) {
                    entry.children.retain(|c| c.segment != CONTAINERS);
                }
            }
        }
    }

    fn field_member(&self, table: &[u8], field: &[u8]) -> Result<&OwnedNode, DataError> {
        self.fields(table)?
            .at(&[&[MEMBERS], field])
            .ok_or_else(|| DataError::with_code(FIELD_MISSING, "The field no longer exists."))
    }

    /// The 26-byte definition of a field, which FileMaker always stores.
    fn definition_of(member: &OwnedNode) -> Result<Vec<u8>, DataError> {
        member
            .child(&[DEFINITION])
            .and_then(|d| d.value.clone())
            .filter(|d| d.len() == 26)
            .ok_or_else(|| unsupported("The field's definition cannot be read."))
    }

    fn change_field(
        &mut self,
        table: &[u8],
        field: &[u8],
        name: &str,
        data_type: FieldKind,
        comment: &str,
    ) -> Result<(), DataError> {
        let member = self.field_member(table, field)?;
        let old = member_name(member).unwrap_or_default();
        let old_comment = comment_of(member);
        let mut definition = Self::definition_of(member)?;
        let old_type = definition[1];
        let new_type = type_code(data_type);
        if old == name && old_comment == comment && old_type == new_type {
            return Ok(());
        }
        let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        let which = CatalogPath::Fields(table.to_vec());
        let created = self.created.contains(&MemberRef {
            catalog: which.clone(),
            key: field.to_vec(),
        });
        if old_type != new_type {
            // Furigana goes from a text field into a text field (5.9): a
            // field with it, or the field receiving another's reading,
            // stays text.
            let text = type_code(FieldKind::Text);
            if new_type != text
                && (options & FURIGANA_OPTION != 0 || self.furigana_target(table, field))
            {
                return Err(unsupported(
                    "OpenMaker cannot yet change the type of a field with furigana, or of the \
                     field receiving another field's furigana: furigana goes from text into \
                     text (#1301).",
                ));
            }
            // A change that needs the field to hold no value is made only
            // on one made in this edit, or one of a table with no record.
            if self.needs_no_value(old_type, new_type)
                && !created
                && !self.holds_no_value(table, field)
            {
                return Err(unsupported(
                    if value_work(old_type, new_type) == ValueWork::Converted {
                        "OpenMaker cannot yet change a field that may hold values to a date or \
                         a timestamp, or from a container, in a hosted file: it cannot make sure \
                         the host converts the stored values (#1267)."
                    } else {
                        "OpenMaker cannot yet change a field that may hold values to a \
                         container: what its stored text would read as there is not \
                         established (#1268)."
                    },
                ));
            }
            if definition[0] != KIND_NORMAL {
                return Err(unsupported(
                    "Calculation and summary fields are changed in their own dialogs, which \
                     OpenMaker does not have yet.",
                ));
            }
            if options & AUTO_ENTER != 0 {
                return Err(unsupported(
                    "OpenMaker cannot yet change the type of a field with auto-enter options.",
                ));
            }
            // A summary's type follows what it summarizes (5.11).
            if self.summary_uses(table, field) {
                return Err(unsupported(
                    "OpenMaker cannot yet change the type of a field a summary field uses.",
                ));
            }
        }
        if old != name {
            check_name(name, "field")?;
            if let Some(other) = named(self.fields(table)?, name, Some(field)) {
                return Err(taken(format!(
                    "The table already has a field named “{}”.",
                    member_name(other).unwrap_or_default()
                )));
            }
        }
        let mut work = 0;
        if old != name || old_comment != comment {
            work |= WORK_MEMBER;
        }
        let typed_list = self
            .fields(table)?
            .at(&[&[INDEX], &[TYPED_NAMES]])
            .is_some();
        let typed_entry = if typed_list && (old != name || old_type != new_type) {
            let entry = typed(definition[0], new_type, options & GLOBAL != 0, name)?;
            check_free(self.fields(table)?, TYPED_NAMES, &entry, field)?;
            Some(entry)
        } else {
            None
        };
        {
            let mut fields = self.catalog(which.clone());
            if old != name {
                fields.rename(field, name)?;
            }
            if let Some(entry) = typed_entry {
                let list = fields.index().ensure(&[&[TYPED_NAMES]]);
                unlist_member(list, field);
                list.set_child(OwnedNode::leaf(entry, key_string(field)));
            }
            if old_type != new_type {
                definition[1] = new_type;
            }
            let member = fields
                .node
                .at_mut(&[&[MEMBERS], field])
                .expect("checked above");
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
            set_comment(member, comment);
        }
        if old_type != new_type {
            // What FileMaker Pro writes for a data type change (7.2): the
            // stored values' dependents recalculated, the definition
            // changed, an indexed field reindexed under the new type.
            work |= WORK_CONVERT | WORK_DEFINITION;
            if options & INDEXED != 0 {
                work |= WORK_INDEX;
            }
            if options & GLOBAL != 0 {
                work |= WORK_GLOBAL;
            }
            // Text, number and time are rewritten as a date or timestamp
            // (7.2.1); a date and a timestamp keep theirs; a container's
            // values go. What the host does follows the type the file has
            // and the one the field ends with, whatever this edit gave it
            // between (below): `0x400` from a change since undone would
            // rewrite text the field keeps.
            let from = self.base_type(table, field).unwrap_or(old_type);
            let textual = matches!(from, 1 | 2 | 4);
            if textual && new_type == 3 {
                work |= WORK_TO_DATE;
            }
            if textual && new_type == 5 {
                work |= WORK_TO_TIMESTAMP;
            }
            if from == 6 && new_type != 6 {
                work |= WORK_CONTAINER;
            }
            if (old_type == 6) != (new_type == 6) {
                self.list_container(table, field, new_type == 6);
            }
            if !created {
                self.data_changed.insert(table.to_vec());
                self.field_data_changed
                    .insert((table.to_vec(), field.to_vec()));
            }
        }
        if !created && (work != 0 || old_type != new_type) {
            let entry = self
                .work
                .entry((table.to_vec(), field.to_vec()))
                .or_default();
            if old_type != new_type {
                // An earlier type change's conversion goes; a container's
                // fewer repetitions keep theirs (7.2, `0x4 | 0x8`).
                *entry &= !(WORK_TO_DATE | WORK_TO_TIMESTAMP);
                if *entry & (WORK_DELETE | WORK_REPETITIONS) == 0 {
                    *entry &= !WORK_CONTAINER;
                }
            }
            *entry |= work;
        }
        self.touch(which, field);
        Ok(())
    }

    /// The work the host is asked for on field `field` of `table` at the
    /// save: `work` as the edit's steps left it, with a normal field's
    /// deleting of values reckoned, as FileMaker Pro does, from the
    /// definition the file holds and the final one (schema-path.md 7.2): a
    /// container whose type changes loses every repetition (`0x8` without
    /// `0x4`, whatever its repetitions became); fewer repetitions delete
    /// those above the new maximum (`0x4`), releasing a container's
    /// (`0x8`); and a field global in the file has that work done on its
    /// global value (`0x2`). A deleted field or a storage change (`0x1`)
    /// keeps its work, with `0x8` whenever the file holds it as a
    /// container, whatever type the edit gave it before: the storage
    /// change reckons its own bits from the type at that step.
    fn host_work(&self, table: &[u8], field: &[u8], work: u32) -> u32 {
        let definition = |catalogs: &SchemaCatalogs| {
            let member = catalogs.fields.get(table)?.at(&[&[MEMBERS], field])?;
            Self::definition_of(member).ok()
        };
        let base = definition(&self.base);
        if work & WORK_DELETE != 0 {
            let container = base
                .as_ref()
                .is_some_and(|d| d[1] == type_code(FieldKind::Container));
            return if container {
                work | WORK_CONTAINER
            } else {
                work
            };
        }
        let (Some(was), Some(now)) = (base, definition(&self.current)) else {
            return work;
        };
        if was[0] != KIND_NORMAL || now[0] != KIND_NORMAL {
            return work;
        }
        let repetitions = |d: &[u8]| {
            d.get(REPETITIONS)
                .and_then(|r| r.try_into().ok())
                .map(u16::from_be_bytes)
        };
        let container = was[1] == type_code(FieldKind::Container);
        let mut work = work & !(WORK_REPETITIONS | WORK_CONTAINER);
        if container && now[1] != was[1] {
            work |= WORK_CONTAINER;
        } else if matches!((repetitions(&now), repetitions(&was)), (Some(n), Some(w)) if n < w) {
            work |= WORK_REPETITIONS;
            if container {
                work |= WORK_CONTAINER;
            }
        }
        let global = was
            .get(OPTIONS)
            .and_then(|o| o.try_into().ok())
            .map(u32::from_be_bytes)
            .is_some_and(|o| o & GLOBAL != 0);
        if global && work & (WORK_REPETITIONS | WORK_CONTAINER) != 0 {
            work |= WORK_GLOBAL;
        }
        work
    }

    /// The data type field `field` of `table` has in the file, before this
    /// edit; `None` for a field the edit made.
    fn base_type(&self, table: &[u8], field: &[u8]) -> Option<u8> {
        let member = self.base.fields.get(table)?.at(&[&[MEMBERS], field])?;
        Self::definition_of(member)
            .ok()
            .map(|definition| definition[1])
    }

    /// Options for Field (schema-path.md 5.5, 5.8, 5.9, 5.13): the
    /// definition's option and validation words, serial increment, byte
    /// limit and repetitions; the members `0c` (next serial value), `0d`
    /// (data), `05` (an auto-entered calculation), `07` (value list), `18`
    /// (custom message), `19` (validation calculation), `21` and `22`
    /// (range); the table's evaluation order and `fffc` list in the
    /// dependency index; and the field's triggers in its field catalog's
    /// `fff0` and `fff1`.
    fn set_field_options(
        &mut self,
        table: &[u8],
        field: &[u8],
        options: &FieldOptions,
    ) -> Result<(), DataError> {
        let member = self.field_member(table, field)?;
        let mut definition = Self::definition_of(member)?;
        if definition[0] != KIND_NORMAL {
            return Err(unsupported(
                "Calculation and summary fields have their own options, which OpenMaker does \
                 not change yet.",
            ));
        }
        let next = member.child(&[NEXT_SERIAL]).and_then(text_of);
        let data = member.child(&[AUTO_DATA]).and_then(text_of);
        let mut old = FieldOptions::decode(&definition, next.as_deref(), data.as_deref())
            .ok_or_else(|| unsupported("The field's definition cannot be read."))?;
        let names = self.formula_names();
        old.read_members(&DraftMembers {
            member,
            names: &names,
            decimal_comma: self.formulas.decimal_comma,
        });
        let mut new = options.clone();
        let text = definition[1] == type_code(FieldKind::Text);
        // Unique and Existing need the value index, which FileMaker Pro
        // turns on with them (E35, E36).
        if (new.validation.unique || new.validation.existing)
            && new.storage.indexing == Indexing::None
        {
            new.storage.indexing = if text {
                Indexing::Minimal
            } else {
                Indexing::All
            };
        }
        if new == old {
            return Ok(());
        }
        let which = CatalogPath::Fields(table.to_vec());
        let created = self.created.contains(&MemberRef {
            catalog: which.clone(),
            key: field.to_vec(),
        });
        let old_options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        let empty = self.holds_no_value(table, field);
        check_options(
            &old,
            &new,
            old_options,
            definition[1],
            created,
            empty || self.host_converts,
        )?;
        let definition_type = definition[1];
        // A field that other definitions use keeps its storage: FileMaker
        // Pro makes the stored calculations that read a field turned global
        // unstored (5.9, E38), which OpenMaker does not yet; a calculation
        // over a global field keeps its listings only while the field stays
        // global (5.16). A field made in this edit too.
        if old.storage.global != new.storage.global {
            if let Some(why) = self.deletion_blocked(table, field) {
                let name = member_name(self.field_member(table, field)?).unwrap_or_default();
                return Err(unsupported(format!(
                    "OpenMaker cannot yet change the global storage of “{name}”: {why}."
                )));
            }
        }

        // The auto-entered calculation's formula and the field's triggers
        // in `fff0`: its inputs and creation (5.13), else its events.
        let old_events = old.auto_enter.value.evaluation();
        let new_events = new.auto_enter.value.evaluation();
        let (auto_formula, triggers) = match (&old.auto_enter.value, &new.auto_enter.value) {
            (AutoEnterValue::Calculated(before), AutoEnterValue::Calculated(after))
                if before.formula == after.formula =>
            {
                (None, None)
            }
            (_, AutoEnterValue::Calculated(after)) => {
                let (compiled, mut inputs) =
                    self.option_formula(table, field, &after.formula, "auto-entered")?;
                inputs.insert(calc_triggers::CREATION.to_vec(), 1);
                (Some((after.formula.context, compiled)), Some(inputs))
            }
            (AutoEnterValue::Calculated(_), _) => (None, Some(event_triggers(new_events))),
            _ if old_events != new_events => (None, Some(event_triggers(new_events))),
            _ => (None, None),
        };
        // The validation calculation and its inputs in `fff1` (5.13).
        let (validation_formula, validation_inputs) =
            match (&old.validation.calculation, &new.validation.calculation) {
                (Some(before), Some(after)) if before.formula == after.formula => (None, None),
                (_, Some(after)) => {
                    let (compiled, inputs) =
                        self.option_formula(table, field, &after.formula, "validation")?;
                    (Some((after.formula.context, compiled)), Some(inputs))
                }
                (Some(_), None) => (None, Some(Triggers::new())),
                (None, None) => (None, None),
            };
        // A calculated custom message: its formula in `20`, no inputs listed
        // (a message lists nothing, 5.13), and in `18` the text older
        // versions show (5.9, E34). Only one the record rules can evaluate
        // over the record alone is written (`rules::CustomMessage`).
        let message_formula = match (&old.validation.message, &new.validation.message) {
            (Some(Message::Calculation(before)), Some(Message::Calculation(after)))
                if before == after =>
            {
                None
            }
            (_, Some(Message::Calculation(after))) => {
                let (compiled, inputs) = self.option_formula(table, field, after, "message")?;
                let key = om_formula::Key::from_id(after.context).0;
                let cannot = |problem: &str| {
                    unsupported(format!(
                        "OpenMaker cannot yet evaluate this message calculation: {problem}"
                    ))
                };
                if let Some(problem) = crate::rules::message_formula_problem(&compiled.polish, &key)
                {
                    return Err(cannot(&problem));
                }
                // What the rules' one-record evaluation of a message cannot
                // read (`rules::RecordPlan`): it would show their wording.
                for input in inputs.keys().filter(|key| !calc_triggers::is_event(key)) {
                    let Ok(definition) = self
                        .field_member(table, input)
                        .and_then(Self::definition_of)
                    else {
                        continue;
                    };
                    let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4"));
                    let repetitions =
                        u16::from_be_bytes(definition[REPETITIONS].try_into().expect("2"));
                    if definition[0] == KIND_SUMMARY
                        || definition[1] == type_code(FieldKind::Container)
                        || options & GLOBAL != 0 && repetitions > 1
                    {
                        return Err(cannot(
                            "it reads a summary, a container or a repeating global field.",
                        ));
                    }
                }
                Some((after.context, compiled))
            }
            _ => None,
        };
        // A lookup's `fb` node, from which the rebuild derives its triggers
        // (5.13): set for a new or changed lookup, removed with one.
        // A changed or removed calculated value or lookup rewrites the
        // field's node: a calculated value's related occurrences from its
        // context (a04, a13), a lookup's two occurrences (l01).
        let reads = match (&old.auto_enter.value, &new.auto_enter.value) {
            (AutoEnterValue::LookedUp(before), AutoEnterValue::LookedUp(after))
                if before == after =>
            {
                None
            }
            (AutoEnterValue::Calculated(before), AutoEnterValue::Calculated(after))
                if before.formula == after.formula =>
            {
                None
            }
            (_, AutoEnterValue::LookedUp(Some(lookup))) => {
                self.check_lookup(table, &definition, &new, lookup)?;
                Some(vec![(lookup.from, lookup.to)])
            }
            (_, AutoEnterValue::Calculated(after)) => Some(
                triggers
                    .iter()
                    .flat_map(|t| t.keys())
                    .filter_map(|key| calc_triggers::related_input(key))
                    .map(|(occurrence, _)| (after.formula.context, occurrence))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            ),
            (AutoEnterValue::LookedUp(_) | AutoEnterValue::Calculated(_), _) => Some(Vec::new()),
            _ => None,
        };
        let lookup = match &new.auto_enter.value {
            AutoEnterValue::LookedUp(Some(lookup)) if reads.is_some() => {
                Some(lookup.encode().ok_or_else(|| {
                    unsupported("OpenMaker cannot yet write this lookup's definition.")
                })?)
            }
            _ => None,
        };
        let rebuild = triggers.is_some() || reads.is_some();
        if rebuild {
            self.check_triggers(table)?;
        }
        let value_list = match new.validation.value_list {
            Some(id) if old.validation.value_list != Some(id) => {
                // OpenMaker's record rules check values against the list;
                // one they cannot check would leave the table read-only.
                match self
                    .formulas
                    .value_lists
                    .iter()
                    .find(|(list, _)| *list == id)
                {
                    None => return Err(refuse("The file has no such value list.")),
                    Some((_, Some(problem))) => {
                        return Err(unsupported(format!(
                            "OpenMaker cannot yet validate by this value list: {problem}"
                        )))
                    }
                    Some((_, None)) => {}
                }
                field_options::value_list_reference(id)
                    .ok_or_else(|| refuse("Choose a value list."))?;
                Some(id)
            }
            _ => None,
        };
        // Furigana (#1301): the field that receives the reading, one of this
        // table's stored normal text fields other than this one, as the
        // dialog offers them [I], checked as it is chosen or turned on.
        let (was, furigana) = (&old.furigana, &new.furigana);
        if furigana.on && (!was.on || furigana.field != was.field) {
            let target = om_hbam::slot::uint_segment(furigana.field);
            let text = self
                .field_member(table, &target)
                .ok()
                .filter(|_| target != field)
                .and_then(|m| Self::definition_of(m).ok())
                .is_some_and(|d| {
                    let options = u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes"));
                    d[0] == KIND_NORMAL
                        && d[1] == type_code(FieldKind::Text)
                        && options & GLOBAL == 0
                });
            if !text {
                return Err(refuse(
                    "Choose a text field of this table, other than this one, to receive the \
                     furigana.",
                ));
            }
        }
        // Member `07` as the edit leaves it: the value list in its first
        // part, the furigana field and translation in its second, every
        // other byte kept (5.13). `None`: unchanged.
        let list_member = {
            // Read as the options were (`DraftMembers::bytes`): a member
            // in pieces too; one that cannot be put together is refused.
            let stored = match self.field_member(table, field)?.child(&[VALUE_LIST]) {
                None => None,
                Some(node) => Some(
                    node.assembled()
                        .ok_or_else(|| {
                            unsupported(
                                "The field's value list and furigana member cannot be read, so \
                                 OpenMaker does not change it.",
                            )
                        })?
                        .into_owned(),
                ),
            };
            let mut list = stored.clone();
            if let Some(id) = value_list {
                list = Some(
                    field_options::value_list_member(list.as_deref(), id).ok_or_else(|| {
                        unsupported(
                            "OpenMaker cannot write this value list into the field's \
                             definition: its member is shorter than FileMaker Pro writes it.",
                        )
                    })?,
                );
            }
            if furigana.on && (furigana.field, furigana.translation) != (was.field, was.translation)
            {
                list = Some(
                    field_options::furigana_member(
                        list.as_deref(),
                        furigana.field,
                        furigana.translation,
                    )
                    .ok_or_else(|| {
                        unsupported(
                            "OpenMaker cannot write this furigana field into the field's \
                             definition.",
                        )
                    })?,
                );
            }
            list.filter(|list| Some(list) != stored.as_ref())
        };

        new.encode(&mut definition);
        let word = |d: &[u8], range: std::ops::Range<usize>| {
            u32::from_be_bytes(d[range].try_into().expect("4 bytes"))
        };
        let new_options = word(&definition, OPTIONS);
        let new_validation = word(&definition, VALIDATION);

        let listed_before = self
            .current
            .occurrences
            .as_ref()
            .and_then(|c| c.at(&[&[INDEX], &[DEPENDENCIES], table]))
            .map(|entry| dependencies::listing(entry, field))
            .unwrap_or_default();
        // The validation options that list a field in `fffc`, and Always
        // in `fffc, 00` (5.13); a field listed for an option whose listing
        // is not established stays listed while it has that option.
        let unknown = new_validation & FieldOptions::UNKNOWN_LISTING != 0;
        let listed =
            new_validation & FieldOptions::LISTED_VALIDATION != 0 || (listed_before.0 && unknown);
        let always = if unknown {
            listed && listed_before.1
        } else {
            listed && new_validation & FieldOptions::ALWAYS != 0
        };
        if (listed, always) != listed_before {
            self.catalog(CatalogPath::Occurrences);
            let entry = self.dependency_entry(table).ok_or_else(|| {
                unsupported("The table has no entry in the dependency index OpenMaker can edit.")
            })?;
            dependencies::set_validated(entry, field, listed, always);
        }
        let typed_list = {
            let mut fields = self.catalog(which.clone());
            if let Some(triggers) = &triggers {
                let depends = fields
                    .index()
                    .ensure(&[&[DEPENDENCIES], FIELD_DEPENDENCIES]);
                calc_triggers::write(depends, field, triggers);
            }
            if let Some(reads) = reads {
                let index = fields.index().ensure(&[&[DEPENDENCIES]]);
                let node = index.ensure(&[&[RELATED_READS]]);
                node.children.retain(|c| c.segment != field);
                for (from, to) in reads {
                    node.ensure(&[field, &key_of(from), &key_of(to)]);
                }
                if node.children.is_empty() {
                    index.children.retain(|c| c.segment != [RELATED_READS]);
                }
            }
            if let Some(inputs) = &validation_inputs {
                let depends = fields
                    .index()
                    .ensure(&[&[DEPENDENCIES], VALIDATION_DEPENDENCIES]);
                calc_triggers::write(depends, field, inputs);
                if depends.children.is_empty() {
                    fields
                        .index()
                        .ensure(&[&[DEPENDENCIES]])
                        .children
                        .retain(|c| c.segment != VALIDATION_DEPENDENCIES);
                }
            }
            fields.node.at(&[&[INDEX], &[TYPED_NAMES]]).is_some()
        };
        if old.storage.global != new.storage.global && typed_list {
            let name = member_name(self.field_member(table, field)?).unwrap_or_default();
            let entry = typed(definition[0], definition[1], new.storage.global, &name)?;
            check_free(self.fields(table)?, TYPED_NAMES, &entry, field)?;
            let mut fields = self.catalog(which.clone());
            let list = fields.index().ensure(&[&[TYPED_NAMES]]);
            unlist_member(list, field);
            list.set_child(OwnedNode::leaf(entry, key_string(field)));
        }
        {
            let fields = self.catalog(which.clone());
            let member = fields
                .node
                .at_mut(&[&[MEMBERS], field])
                .expect("checked above");
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
            // FileMaker Pro writes the next serial value whenever its
            // Auto-Enter tab was shown, "1" for a field without a serial
            // (E11, E30, E33); a value it had is kept.
            match &new.auto_enter.value {
                AutoEnterValue::Serial(serial) => {
                    member.set_child(om_scripts::text_node(&[NEXT_SERIAL], &serial.next));
                }
                _ if member.child(&[NEXT_SERIAL]).is_none() => {
                    member.set_child(om_scripts::text_node(&[NEXT_SERIAL], "1"));
                }
                _ => {}
            }
            // The data is kept when another value is chosen, as the dialog
            // shows it again [I]; emptied, it goes. So are the members of
            // the other options: FileMaker's files keep a value list, a
            // message and a lookup on fields with the option off (5.13).
            if let AutoEnterValue::Data(text) = &new.auto_enter.value {
                if text.is_empty() {
                    member.children.retain(|c| c.segment != [AUTO_DATA]);
                } else {
                    member.set_child(om_scripts::text_node(&[AUTO_DATA], text));
                }
            }
            if message_formula.is_some() {
                member.set_child(om_scripts::text_node(&[MESSAGE], OLDER_VERSIONS_MESSAGE));
            }
            for (key, formula) in [
                (FORMULA, auto_formula),
                (VALIDATION_FORMULA, validation_formula),
                (MESSAGE_FORMULA, message_formula),
            ] {
                if let Some((context, compiled)) = formula {
                    let mut node = Self::formula_node(context, &compiled);
                    node.segment = vec![key];
                    member.set_child(node);
                }
            }
            if let Some(list) = list_member {
                member.set_child(OwnedNode::leaf(vec![VALUE_LIST], list));
            }
            if let Some(definition) = lookup {
                member.set_child(OwnedNode::leaf(vec![LOOKUP], definition));
                if let AutoEnterValue::LookedUp(Some(Lookup {
                    no_match: NoMatch::Constant(text),
                    ..
                })) = &new.auto_enter.value
                {
                    member.set_child(om_scripts::text_node(&[LOOKUP_CONSTANT], text));
                }
            }
            let rules = (&old.validation, &new.validation);
            if let Some((from, to)) = rules
                .1
                .range
                .as_ref()
                .filter(|_| rules.0.range != rules.1.range)
            {
                member.set_child(om_scripts::text_node(&[RANGE_FROM], from));
                member.set_child(om_scripts::text_node(&[RANGE_TO], to));
            }
            if let Some(Message::Text(text)) = rules
                .1
                .message
                .as_ref()
                .filter(|_| rules.0.message != rules.1.message)
            {
                member.set_child(om_scripts::text_node(&[MESSAGE], text));
            }
        }
        // The field's place in the table's evaluation lists (5.9, 5.10).
        if rebuild {
            self.rebuild_triggers(table)?;
        }
        // A changed index raises the table's data version (E13, E34-E36,
        // E38, E41); the host builds or drops it (op73, 7.2).
        let indexes = crate::field_options::VALUE_INDEX | crate::field_options::WORD_INDEX;
        let mut work = WORK_MEMBER;
        // Global storage turned on or off on a field that may hold values
        // (schema-path.md 7.2): the host deletes every record's value, or
        // the global value (`0x2`) when the field was global, and a
        // container's library members with them; dependents are none (the
        // change is refused above otherwise). The work is reckoned from
        // the storage the field had when the edit began, which holds its
        // values: storage turned back asks for nothing. The field's data
        // mod count goes up, as E38 gave a field made global `fb` = `01 01`.
        let mut storage_work = None;
        if old.storage.global != new.storage.global && !created {
            let was = self.base_global(table, field).unwrap_or(old.storage.global);
            let mut storage = 0;
            if was != new.storage.global {
                storage = WORK_DELETE | WORK_CONVERT;
                if was {
                    storage |= WORK_GLOBAL;
                }
                if definition_type == type_code(FieldKind::Container) {
                    storage |= WORK_CONTAINER;
                }
                // `finish` raises the field's data mod count.
                self.data_changed.insert(table.to_vec());
                self.field_data_changed
                    .insert((table.to_vec(), field.to_vec()));
            }
            storage_work = Some(storage);
        }
        let old_indexes = old_options & indexes;
        let new_indexes = new_options & indexes;
        // Repetitions other than the field had when the edit began (on a
        // field that holds no value, checked above). Their work is added
        // after the storage work below, which replaces the bits they share.
        let base_repetitions = self.base_repetitions(table, field).filter(|_| !created);
        let was_repetitions = base_repetitions.filter(|was| *was != new.storage.repetitions);
        if was_repetitions.is_some() {
            self.data_changed.insert(table.to_vec());
            self.field_data_changed
                .insert((table.to_vec(), field.to_vec()));
        }
        if old_indexes != new_indexes {
            self.data_changed.insert(table.to_vec());
            // A rebuild also keeps existing on-demand indexes. Drop the old
            // field index first when a kind was removed, then build only the
            // kinds the new definition requests (All -> Minimal, in particular).
            if old_indexes & !new_indexes != 0 {
                work |= WORK_DROP_INDEX;
            }
            if new_indexes != 0 {
                work |= WORK_INDEX;
            }
        }
        // Another index language: the host keys the field's indexes in it
        // again (`0x10`, 7.2), an index built on demand included, which
        // sets no option bit.
        if old.storage.language != new.storage.language {
            self.data_changed.insert(table.to_vec());
            work |= WORK_INDEX;
        }
        if !created {
            let entry = self
                .work
                .entry((table.to_vec(), field.to_vec()))
                .or_default();
            if let Some(storage) = storage_work {
                *entry = (*entry & !STORAGE_WORK) | storage;
            }
            *entry |= work;
            // By 7.2's rules, as for a calculation (`calculation_work`):
            // dependents recalculated (`0x40`); fewer repetitions delete
            // those above the new maximum (`0x4 | 0x1000`, `0x8` for a
            // container) unless the values are deleted anyway (`0x1`); an
            // indexed field reindexed (`0x10`). The field's data mod count
            // and the table's data version go up (above). The deleting
            // bits follow the repetitions as they end.
            if base_repetitions.is_some() {
                *entry &= !(WORK_REPETITIONS | WORK_FEWER_REPETITIONS);
            }
            if let Some(was) = was_repetitions {
                *entry |= WORK_CONVERT;
                if new.storage.repetitions < was {
                    *entry |= WORK_FEWER_REPETITIONS;
                    if *entry & WORK_DELETE == 0 {
                        *entry |= WORK_REPETITIONS;
                        if definition_type == type_code(FieldKind::Container) {
                            *entry |= WORK_CONTAINER;
                        }
                    }
                }
                if new_indexes != 0 {
                    *entry |= WORK_INDEX;
                }
            }
        }
        self.touch(which, field);
        Ok(())
    }

    /// Whether `field` of `table` was a container when the edit began, so
    /// its values hold container library references.
    fn base_container(&self, table: &[u8], field: &[u8]) -> bool {
        self.base
            .fields
            .get(table)
            .and_then(|c| c.at(&[&[MEMBERS], field]))
            .and_then(|m| Self::definition_of(m).ok())
            .is_some_and(|d| d[1] == 6)
    }

    /// The repetitions `field` of `table` had when the edit began; `None`
    /// for a field the edit made.
    fn base_repetitions(&self, table: &[u8], field: &[u8]) -> Option<u16> {
        let member = self.base.fields.get(table)?.at(&[&[MEMBERS], field])?;
        let definition = Self::definition_of(member).ok()?;
        Some(u16::from_be_bytes(
            definition.get(REPETITIONS)?.try_into().ok()?,
        ))
    }

    /// Whether `field` of `table` was global when the edit began, so where
    /// its values are; `None` for a field the edit made.
    fn base_global(&self, table: &[u8], field: &[u8]) -> Option<bool> {
        let member = self.base.fields.get(table)?.at(&[&[MEMBERS], field])?;
        let definition = Self::definition_of(member).ok()?;
        let options = u32::from_be_bytes(definition.get(OPTIONS)?.try_into().ok()?);
        Some(options & GLOBAL != 0)
    }

    /// Refuse a lookup OpenMaker's record rules could not perform, so the
    /// table stays writable to OpenMaker (`rules::lookup_matches`): one
    /// copying an exact match or nothing into a text, number or date field,
    /// a stored text, number or date of one repetition (a date only into a
    /// date field) of another table through one relationship of `=`
    /// predicates
    /// on plain stored text fields, unsorted or sorted by stored fields of
    /// that table (`check_hop`); a lookup that does not start
    /// from an occurrence of the field's table is refused as FileMaker Pro
    /// does not offer it.
    fn check_lookup(
        &self,
        table: &[u8],
        definition: &[u8],
        options: &FieldOptions,
        lookup: &Lookup,
    ) -> Result<(), DataError> {
        let cannot = |why: &str| {
            Err(unsupported(format!(
                "OpenMaker cannot yet perform this lookup: {why}"
            )))
        };
        let from = self.occurrence(&key_of(lookup.from))?;
        if Self::occurrence_table(from).as_deref() != Some(table) {
            return Err(refuse(
                "A lookup starts from an occurrence of the field's own table.",
            ));
        }
        let to_table = Self::occurrence_table(self.occurrence(&key_of(lookup.to))?)
            .ok_or_else(|| unsupported("The table the lookup reads is not in this file."))?;
        if lookup.no_match != NoMatch::DoNotCopy {
            return cannot("it copies only an exact match (\"do not copy\" otherwise).");
        }
        // The copied text is stored as it is (auto-entry.md, Lookups,
        // battery s43), for the pairs `rules::typed_copy` reads exactly.
        let into = definition[1];
        if ![FieldKind::Text, FieldKind::Number, FieldKind::Date]
            .iter()
            .any(|kind| type_code(*kind) == into)
        {
            return cannot("it looks up into text, number and date fields only.");
        }
        if options.auto_enter.prohibit_modification {
            return cannot("it does not combine a lookup with \"Prohibit modification\".");
        }
        if options.storage.repetitions != 1 || options.storage.global {
            return cannot("it looks up into a field of one stored value only.");
        }
        if to_table == table {
            return cannot("it looks up from another table only.");
        }
        // A stored text, number or date of one repetition, as the related
        // record holds it; a date field takes dates only.
        let copied = self
            .current
            .fields
            .get(&to_table)
            .and_then(|c| c.at(&[&[MEMBERS], &key_of(lookup.copied), &[DEFINITION]]))
            .and_then(|d| d.value.clone())
            .filter(|d| d.len() == 26);
        let date = type_code(FieldKind::Date);
        let readable = [FieldKind::Text, FieldKind::Number, FieldKind::Date].map(type_code);
        if !copied.is_some_and(|d| {
            stored_value_of(&d) && readable.contains(&d[1]) && (into != date || d[1] == date)
        }) {
            return cannot(
                "it copies a stored text, number or date field of one repetition, and only a \
                 date into a date field.",
            );
        }
        self.check_hop(table, lookup.from, lookup.to, &to_table, false)
            .or_else(cannot)?;
        Ok(())
    }

    /// Why OpenMaker's record rules cannot follow the relationship from
    /// occurrence `from` (of `table`) to `to` (of `to_table`) for a lookup
    /// or, with `formula`, a formula over related fields, if they cannot
    /// (`rules::lookup_matches`, `rules::related_hop`): one relationship of
    /// `=` predicates on plain stored text fields of `table` and stored
    /// text fields of `to_table`, unsorted or sorted by stored fields of
    /// `to_table` (the first record in its order, auto-entry.md, Lookups,
    /// s42); for a lookup `table`'s match fields may also be keys
    /// auto-entered as `Get ( UUID )` (s41).
    fn check_hop(
        &self,
        table: &[u8],
        from: u64,
        to: u64,
        to_table: &[u8],
        formula: bool,
    ) -> Result<(), &'static str> {
        let word = |d: &[u8]| u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes"));
        let plain = crate::rules::INDEX_OPTIONS | crate::rules::BOOKKEEPING_OPTIONS;
        let definition_of = |t: &[u8], f: &[u8]| {
            self.current
                .fields
                .get(t)
                .and_then(|c| c.at(&[&[MEMBERS], f, &[DEFINITION]]))
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        let relationship = self
            .current
            .relationships
            .as_ref()
            .into_iter()
            .flat_map(members)
            .filter_map(|member| {
                let id = id_of(&member.segment)?;
                let definition = member.child(&[DEFINITION])?.assembled()?;
                let predicates: Vec<Vec<u8>> = member
                    .child(&[PREDICATES])?
                    .children
                    .iter()
                    .map(|p| p.assembled().map(|b| b.into_owned()))
                    .collect::<Option<_>>()?;
                let predicates: Vec<&[u8]> = predicates.iter().map(Vec::as_slice).collect();
                crate::meta::Relationship::decode(id, &definition, &predicates)
            })
            .find(|r| (r.left, r.right) == (from, to) || (r.left, r.right) == (to, from));
        let Some(relationship) = relationship else {
            return Err("it follows one relationship from its starting occurrence only.");
        };
        // The first related record is read in the relationship's order
        // (`rules::ordered_as_filemaker`); the draft does not read value
        // lists, so it also refuses an order by one.
        let text = [type_code(FieldKind::Text), type_code(FieldKind::Container)];
        let orderable = relationship.order_of(to).is_none_or(|sort| {
            sort.fields.iter().all(|entry| {
                entry.field.occurrence.is_none_or(|o| o == to)
                    && entry.summary.is_none()
                    && entry.value_list.is_none()
                    && definition_of(to_table, &key_of(entry.field.field)).is_some_and(|d| {
                        let language = u32::from_be_bytes(d[4..8].try_into().expect("4 bytes"));
                        (d[0] == KIND_NORMAL
                            || (d[0] == calc_field::KIND_CALCULATION
                                && word(&d) & (0x2_0000 | GLOBAL) == 0))
                            && (!text.contains(&d[1])
                                || crate::rules::sort_language(
                                    entry.locale,
                                    language,
                                    self.formulas.language,
                                ) != crate::rules::JAPANESE_SORT)
                    })
            })
        });
        if !orderable {
            return Err(
                "its relationship is sorted only by stored fields of the related \
                 table, without a summary or value list order or Japanese text.",
            );
        }
        // A key whose only auto-entry is `Get ( UUID )` (`rules::fresh_key`):
        // an auto-entered calculation, replacing or not, prohibiting
        // modification or not, whose Polish stream is `9b 65`.
        let fresh_key = |d: &[u8], f: &[u8]| {
            const ALLOWED: u32 = 0x8 | 0x20 | 0x80 | 0x100;
            let options = word(d) & !plain;
            !formula
                && options & 0x8 != 0
                && options & !ALLOWED == 0
                && self
                    .current
                    .fields
                    .get(table)
                    .and_then(|c| c.at(&[&[MEMBERS], f, &[FORMULA], &[FORMULA_POLISH]]))
                    .and_then(|n| n.assembled())
                    .is_some_and(|code| *code == [0x9b, 0x65])
        };
        for predicate in &relationship.predicates {
            let (ours, theirs) = if relationship.left == from {
                (predicate.left, predicate.right)
            } else {
                (predicate.right, predicate.left)
            };
            let ours_key = key_of(ours);
            let ours = definition_of(table, &ours_key).is_some_and(|d| {
                d[0] == KIND_NORMAL
                    && d[1] == type_code(FieldKind::Text)
                    && u16::from_be_bytes([d[24], d[25]]) == 1
                    && (word(&d) & !plain == 0 || fresh_key(&d, &ours_key))
            });
            let theirs = definition_of(to_table, &key_of(theirs))
                .is_some_and(|d| stored_value_of(&d) && d[1] == type_code(FieldKind::Text));
            if predicate.op != crate::meta::JoinOp::Equal || !ours || !theirs {
                return Err(
                    "it matches with = on stored text fields without auto-entered values only.",
                );
            }
        }
        if relationship.predicates.is_empty() {
            return Err("it matches with = on stored text fields only.");
        }
        Ok(())
    }

    /// Refuse a formula's read of field `field` of `occurrence` from
    /// `context` (of `table`) that OpenMaker's record rules could not make
    /// (`rules::related_hop`): another table's stored value of one
    /// repetition, not a container, through a relationship
    /// [`Self::check_hop`] accepts.
    fn check_related_read(
        &self,
        table: &[u8],
        context: u64,
        occurrence: u64,
        field: u64,
        what: &str,
    ) -> Result<(), DataError> {
        let cannot = |why: &str| {
            Err(unsupported(format!(
                "OpenMaker cannot yet evaluate this {what} calculation: {why}"
            )))
        };
        let Some(to_table) = Self::occurrence_table(self.occurrence(&key_of(occurrence))?) else {
            return cannot("it reads a table of another file.");
        };
        if to_table == table {
            return cannot("it reads related fields of another table only.");
        }
        let stored = self
            .current
            .fields
            .get(&to_table)
            .and_then(|c| c.at(&[&[MEMBERS], &key_of(field), &[DEFINITION]]))
            .and_then(|d| d.value.clone())
            .filter(|d| d.len() == 26)
            .is_some_and(|d| stored_value_of(&d) && d[1] != type_code(FieldKind::Container));
        if !stored {
            return cannot("it reads related fields that hold stored values only.");
        }
        self.check_hop(table, context, occurrence, &to_table, true)
            .or_else(cannot)
    }

    /// Compile an auto-entered or validation formula of field `field` of
    /// `table`, as Specify Calculation's OK does, and its field inputs by
    /// the rules of a calculation field's (5.13). Its context must be an
    /// occurrence of the field's table, and OpenMaker's record rules must
    /// be able to evaluate it, else the table would be read-only to
    /// OpenMaker: no related fields, repetitions or plug-ins yet.
    fn option_formula(
        &self,
        table: &[u8],
        field: &[u8],
        formula: &OptionFormula,
        what: &str,
    ) -> Result<(CompiledFormula, Triggers), DataError> {
        let text = formula
            .text
            .as_deref()
            .ok_or_else(|| unsupported(format!("The {what} formula has no text to edit.")))?;
        if text.trim().is_empty() {
            return Err(refuse("Specify the calculation."));
        }
        let context = self.occurrence(&key_of(formula.context))?;
        if Self::occurrence_table(context).as_deref() != Some(table) {
            return Err(refuse(
                "A field's calculation is evaluated from an occurrence of its own table.",
            ));
        }
        let names = self.formula_names();
        let compiled =
            calc_field::compile(text, formula.context, &names, self.formulas.decimal_comma)?;
        let key = om_formula::Key::from_id(formula.context).0;
        if let Some(problem) = crate::rules::option_formula_problem(&compiled.polish, &key) {
            return Err(unsupported(format!(
                "OpenMaker cannot yet evaluate this {what} calculation: {problem}"
            )));
        }
        let inputs = calc_triggers::option_inputs(
            &compiled.expr,
            formula.context,
            &|occurrence, id| self.resolve_field(occurrence, id),
            field,
        )
        .map_err(|problem| {
            unsupported(format!(
                "OpenMaker cannot yet evaluate this {what} calculation: {problem}"
            ))
        })?;
        for key in inputs.keys() {
            if let Some((occurrence, read)) = calc_triggers::related_input(key) {
                self.check_related_read(table, formula.context, occurrence, read, what)?;
            }
        }
        Ok((compiled, inputs))
    }

    /// The names this draft's formulas compile against: its occurrences
    /// and fields as edited so far, and the file's functions.
    fn formula_names(&self) -> om_formula::NameTable {
        let mut names = self.formulas.functions.clone();
        for (table, catalog) in &self.current.fields {
            let Some(table_id) = id_of(table) else {
                continue;
            };
            for member in members(catalog) {
                let (Some(id), Some(name)) = (id_of(&member.segment), member_name(member)) else {
                    continue;
                };
                names.add_field(table_id, id, &name);
                let kind = member
                    .child(&[DEFINITION])
                    .and_then(|d| d.value.as_deref())
                    .and_then(|d| d.first().copied());
                names.set_summary_field(table_id, id, kind == Some(KIND_SUMMARY));
            }
        }
        if let Some(occurrences) = &self.current.occurrences {
            for member in members(occurrences) {
                let table = Self::occurrence_table(member).and_then(|t| id_of(&t));
                if let (Some(id), Some(name), Some(table)) =
                    (id_of(&member.segment), member_name(member), table)
                {
                    names.add_occurrence(id, &name, table);
                }
            }
        }
        names
    }

    /// The base table of occurrence `occurrence` and the 26-byte
    /// definition of its field `field`, as the draft has them.
    fn resolve_field(&self, occurrence: u64, field: u64) -> Option<(Vec<u8>, Vec<u8>)> {
        let member = self
            .current
            .occurrences
            .as_ref()?
            .at(&[&[MEMBERS], &key_of(occurrence)])?;
        let table = Self::occurrence_table(member)?;
        let definition = self
            .current
            .fields
            .get(&table)?
            .at(&[&[MEMBERS], &key_of(field), &[DEFINITION]])?
            .value
            .clone()
            .filter(|d| d.len() == 26)?;
        Some((table, definition))
    }

    /// Write `calculation` into `definition` (5.10, 5.12): a formula over
    /// a related field or an unstored calculation is not stored, as
    /// FileMaker makes it, with byte 2 left 0 (an unstored storage chosen
    /// in the dialog writes 1; one kept on a change keeps its byte);
    /// `0x80000` when every input it reads with a type other than 0 is
    /// such a calculation itself.
    fn encode_calculation(
        &self,
        table: &[u8],
        calculation: &mut CalculationField,
        definition: &mut [u8],
        inputs: &calc_triggers::FormulaInputs,
        old: Option<&CalculationField>,
    ) {
        let chosen = calculation.storage.unstored;
        if inputs.forces_unstored && !calculation.storage.global {
            calculation.storage.unstored = true;
            calculation.storage.indexing = Indexing::None;
        }
        let before = definition[2];
        calculation.encode(definition);
        match old {
            // Byte 2 is kept unless the storage changes: FileMaker's files
            // have unstored calculations with 0 and with 1 there (5.10).
            Some(old) if old.stored() == calculation.stored() => definition[2] = before,
            _ if inputs.forces_unstored && !chosen => definition[2] = 0,
            _ => {}
        }
        let is_pure = |key: &[u8]| {
            let definition = match calc_triggers::related_input(key) {
                Some((occurrence, field)) => self
                    .resolve_field(occurrence, field)
                    .map(|(_, definition)| definition),
                None => self
                    .current
                    .fields
                    .get(table)
                    .and_then(|c| c.at(&[&[MEMBERS], key, &[DEFINITION]]))
                    .and_then(|d| d.value.clone()),
            };
            definition.is_some_and(|d| {
                d.len() == 26
                    && d[0] == calc_field::KIND_CALCULATION
                    && u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes")) & 0x8_0000 != 0
            })
        };
        set_pure(definition, calc_triggers::pure(&inputs.triggers, &is_pure));
    }

    /// Compile `calculation`'s formula for a field of `table`, as
    /// Specify Calculation's OK does. Its context must be an occurrence of
    /// the field's own table, the only ones FileMaker Pro offers.
    fn compile_calculation(
        &self,
        table: &[u8],
        calculation: &CalculationField,
    ) -> Result<CompiledFormula, DataError> {
        calculation.check().map_err(refuse)?;
        let context = self.occurrence(&key_of(calculation.context))?;
        if Self::occurrence_table(context).as_deref() != Some(table) {
            return Err(refuse(
                "A calculation field is evaluated from an occurrence of its own table.",
            ));
        }
        let names = self.formula_names();
        calc_field::compile(
            &calculation.formula,
            calculation.context,
            &names,
            self.formulas.decimal_comma,
        )
    }

    /// The member `05` holding a calculation's context and streams.
    fn formula_node(context: u64, compiled: &CompiledFormula) -> OwnedNode {
        OwnedNode::list(
            vec![FORMULA],
            vec![
                OwnedNode::leaf(vec![FORMULA_CONTEXT], om_formula::Key::from_id(context).0),
                OwnedNode::leaf(vec![FORMULA_TEXT], compiled.formatted.clone()),
                OwnedNode::leaf(vec![FORMULA_POLISH], compiled.polish.clone()),
            ],
        )
    }

    /// Specify Calculation's OK for a new calculation field (schema-path.md
    /// 5.10): the definition, the formula, the name, the comment and the
    /// field's data mod count `fb` = `01 01`; the name lists; and the
    /// field's entries in the dependency index.
    fn create_calculation_field(
        &mut self,
        table: &[u8],
        name: &str,
        comment: &str,
        calculation: &CalculationField,
    ) -> Result<(), DataError> {
        let catalog = self.fields(table)?;
        if let Some(other) = named(catalog, name, None) {
            return Err(taken(format!(
                "The table already has a field named “{}”.",
                member_name(other).unwrap_or_default()
            )));
        }
        let compiled = self.compile_calculation(table, calculation)?;
        self.check_triggers(table)?;
        let catalog = self.fields(table)?;
        let language = Self::language(catalog);
        let key = next_key(catalog)?;
        let mut calculation = calculation.clone();
        calculation.storage.language = language;
        let inputs = calc_triggers::formula_triggers(
            &compiled.expr,
            calculation.context,
            calculation.skip_when_empty,
            &|occurrence, field| self.resolve_field(occurrence, field),
            &key,
        )
        .map_err(unsupported)?;
        let mut definition = field_catalog::definition(field_catalog::ScalarType::Text, language);
        self.encode_calculation(table, &mut calculation, &mut definition, &inputs, None);
        let uses_get = inputs.uses_get;
        let triggers = inputs.triggers;
        self.check_computable(table, &key, &calculation, &compiled, &triggers)?;
        self.check_global_summary_inputs(table, &triggers, &definition, uses_get)?;
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
        member.set_child(Self::formula_node(calculation.context, &compiled));
        set_comment(&mut member, comment);
        member.set_child(om_scripts::text_node(&[NAME], name));
        member.set_child(OwnedNode::leaf(
            vec![RELATED],
            key_string(&serial::encode(1)),
        ));
        let which = CatalogPath::Fields(table.to_vec());
        let has_typed = {
            let catalog = self.catalog_mut(&which);
            members(catalog).is_empty() || catalog.at(&[&[INDEX], &[TYPED_NAMES]]).is_some()
        };
        {
            let typed_entry = typed(
                calc_field::KIND_CALCULATION,
                definition[1],
                calculation.storage.global,
                name,
            )?;
            check_free(self.catalog_mut(&which), TYPED_NAMES, &typed_entry, &key)?;
            let mut fields = self.catalog(which.clone());
            fields.add(&key, member, Some(name))?;
            if has_typed {
                fields
                    .index()
                    .ensure(&[&[TYPED_NAMES]])
                    .set_child(OwnedNode::leaf(typed_entry, key_string(&key)));
            }
        }
        self.created.insert(MemberRef {
            catalog: which,
            key: key.clone(),
        });
        self.list_summary_reader(table, &key, &triggers)?;
        self.write_calculation_dependencies(table, &key, &triggers)
    }

    /// Specify Calculation's OK for an existing calculation field: its
    /// definition and formula, its key in the name list by type, its
    /// entries in the dependency index, and the host's work on the stored
    /// values (schema-path.md 7.2).
    fn change_calculation(
        &mut self,
        table: &[u8],
        field: &[u8],
        calculation: &CalculationField,
    ) -> Result<(), DataError> {
        let member = self.field_member(table, field)?;
        let old_definition = Self::definition_of(member)?;
        if old_definition[0] != calc_field::KIND_CALCULATION {
            return Err(unsupported(
                "Only a calculation field has a formula Specify Calculation changes.",
            ));
        }
        let name = member_name(member).unwrap_or_default();
        let old_formula = member.child(&[FORMULA]).cloned();
        let compiled = self.compile_calculation(table, calculation)?;
        let formula = Self::formula_node(calculation.context, &compiled);
        let old_options = u32::from_be_bytes(old_definition[OPTIONS].try_into().expect("4 bytes"));
        let old = CalculationField::decode(&old_definition, String::new(), calculation.context)
            .ok_or_else(|| unsupported("The field's definition cannot be read."))?;
        let mut calculation = calculation.clone();
        // The index language is not edited (OpenMaker does not rebuild an
        // index in a new language).
        calculation.storage.language = old.storage.language;
        self.check_triggers(table)?;
        let inputs = calc_triggers::formula_triggers(
            &compiled.expr,
            calculation.context,
            calculation.skip_when_empty,
            &|occurrence, id| self.resolve_field(occurrence, id),
            field,
        )
        .map_err(unsupported)?;
        let mut definition = old_definition.clone();
        self.encode_calculation(
            table,
            &mut calculation,
            &mut definition,
            &inputs,
            Some(&old),
        );
        let uses_get = inputs.uses_get;
        let triggers = inputs.triggers;
        self.check_computable(table, field, &calculation, &compiled, &triggers)?;
        self.check_global_summary_inputs(table, &triggers, &definition, uses_get)?;
        let same_formula = old_formula.as_ref() == Some(&formula);
        if same_formula && definition == old_definition {
            return Ok(());
        }
        // What other definitions read was admitted as it was: a
        // calculation over a global or summary field only beside stored
        // inputs, and no definition reading one (5.16 measured neither
        // change).
        let unstored_now =
            u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes")) & 0x2_0000 != 0;
        if unstored_now && self.read_by_global_or_summary_reader(table, field) {
            return Err(unsupported(format!(
                "OpenMaker cannot yet leave “{name}” unstored: a calculation over a global or \
                 summary field reads it (not measured)."
            )));
        }
        if self.reads_own_global_or_summary(table, &triggers) {
            if let Some(why) = self.use_blocked(table, field) {
                return Err(unsupported(format!(
                    "OpenMaker cannot yet make “{name}” read a global or summary field: {why} \
                     (not measured)."
                )));
            }
        }
        let which = CatalogPath::Fields(table.to_vec());
        let created = self.created.contains(&MemberRef {
            catalog: which.clone(),
            key: field.to_vec(),
        });
        let new_options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        let work = calculation_work(&old, &calculation, old_options, new_options, same_formula);
        if !created && work & (WORK_DELETE | WORK_REPETITIONS | WORK_CONVERT) != 0 {
            // OpenMaker Server recomputes a stored calculation whose
            // formula or options changed and drops values a calculation no
            // longer stores; a global value it does not compute.
            if old.storage.global || calculation.storage.global {
                return Err(unsupported(
                    "OpenMaker cannot yet change the global storage, formula or result of an \
                     existing global calculation field.",
                ));
            }
        }
        {
            let typed_list = self
                .fields(table)?
                .at(&[&[INDEX], &[TYPED_NAMES]])
                .is_some();
            let typed_entry = if typed_list
                && (old_definition[1] != definition[1]
                    || old.storage.global != calculation.storage.global)
            {
                let entry = typed(
                    definition[0],
                    definition[1],
                    calculation.storage.global,
                    &name,
                )?;
                check_free(self.fields(table)?, TYPED_NAMES, &entry, field)?;
                Some(entry)
            } else {
                None
            };
            let mut fields = self.catalog(which.clone());
            if let Some(entry) = typed_entry {
                let list = fields.index().ensure(&[&[TYPED_NAMES]]);
                unlist_member(list, field);
                list.set_child(OwnedNode::leaf(entry, key_string(field)));
            }
            let member = fields
                .node
                .at_mut(&[&[MEMBERS], field])
                .expect("checked above");
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
            member.set_child(formula);
        }
        self.list_summary_reader(table, field, &triggers)?;
        self.write_calculation_dependencies(table, field, &triggers)?;
        if !created {
            if work & (WORK_DELETE | WORK_REPETITIONS | WORK_CONVERT) != 0 {
                self.field_data_changed
                    .insert((table.to_vec(), field.to_vec()));
            }
            if work & (WORK_DELETE | WORK_REPETITIONS | WORK_CONVERT | WORK_INDEX | WORK_DROP_INDEX)
                != 0
            {
                self.data_changed.insert(table.to_vec());
            }
            *self
                .work
                .entry((table.to_vec(), field.to_vec()))
                .or_default() |= work;
        }
        self.touch(which, field);
        Ok(())
    }

    /// Refuse a calculation OpenMaker cannot keep computed, and a change
    /// that leaves another stored calculation unable to be computed:
    /// OpenMaker Server recomputes a stored calculation only when record
    /// rules can evaluate it (`rules::recalc`; a typed result, one
    /// repetition, stored data or stored calculation inputs of its own
    /// table), and a table with one it cannot compute is read-only to
    /// OpenMaker. Nothing computes a global calculation's value.
    /// A calculation over its table's global or summary fields, in the
    /// forms FileMaker's engine was measured to write (5.16): with neither
    /// related inputs, nor `IsEmpty` and its kind or "Do not evaluate if all
    /// referenced fields are empty" off (events), nor unstored calculations
    /// beside them; and no calculation reads another that reads a global or
    /// summary field. Other forms are refused.
    fn check_global_summary_inputs(
        &self,
        table: &[u8],
        triggers: &Triggers,
        definition: &[u8],
        uses_get: bool,
    ) -> Result<(), DataError> {
        let refuse = |why: &str| {
            Err(unsupported(format!(
                "OpenMaker cannot yet write this calculation: {why} (not measured)."
            )))
        };
        // A calculation it reads, here or through a relationship, that
        // reads a global or summary field (5.16 has none such).
        if triggers
            .keys()
            .any(|k| self.input_reads_global_or_summary(table, k))
        {
            return refuse("it reads a calculation over a global or summary field");
        }
        let fields = self.fields(table)?;
        let definition_of = |key: &[u8]| {
            fields
                .at(&[&[MEMBERS], key, &[DEFINITION]])
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        let over =
            |key: &[u8]| definition_of(key).is_some_and(|d| calc_triggers::summary_or_global(&d));
        if !triggers.keys().any(|k| over(k)) {
            return Ok(());
        }
        // What runs g01-g04 measured: every input read as a value (type
        // 1) of the calculation's own table, no event, a calculation asked
        // to be stored and made unstored with byte 2 `00`.
        if triggers
            .keys()
            .any(|k| calc_triggers::is_event(k) || k.first() == Some(&0))
        {
            return refuse(
                "beside a global or summary field it reads a related field, uses IsEmpty or a \
                 function of its kind, or evaluates when all fields are empty",
            );
        }
        if uses_get {
            return refuse("beside a global or summary field it uses a Get function");
        }
        if triggers.values().any(|&t| t != 1) {
            return refuse(
                "it reads a field as a field reference or in an aggregate beside a global or \
                 summary field",
            );
        }
        let unstored = |key: &[u8]| {
            definition_of(key).is_some_and(|d| {
                d[0] == calc_field::KIND_CALCULATION
                    && u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes")) & 0x2_0000 != 0
            })
        };
        if triggers.keys().any(|k| unstored(k)) {
            return refuse("beside a global or summary field it reads an unstored calculation");
        }
        let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        if options & 0x2_0000 == 0 || definition[2] != 0 {
            return refuse(
                "a calculation over a global or summary field chosen to be unstored or global, or \
                 left unstored by an earlier definition",
            );
        }
        Ok(())
    }

    /// Whether `triggers`, a formula's of `table`, include a global or
    /// summary field of `table`.
    fn reads_own_global_or_summary(&self, table: &[u8], triggers: &Triggers) -> bool {
        let Ok(fields) = self.fields(table) else {
            return false;
        };
        triggers.keys().any(|k| {
            fields
                .at(&[&[MEMBERS], k, &[DEFINITION]])
                .and_then(|d| d.value.as_deref())
                .filter(|d| d.len() == 26)
                .is_some_and(calc_triggers::summary_or_global)
        })
    }

    /// Whether field `field` of `table` is a calculation whose direct
    /// triggers include a global or summary field of its own table (5.16).
    fn reads_global_or_summary(&self, table: &[u8], field: &[u8]) -> bool {
        let Ok(fields) = self.fields(table) else {
            return false;
        };
        let definition_of = |key: &[u8]| {
            fields
                .at(&[&[MEMBERS], key, &[DEFINITION]])
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        definition_of(field).is_some_and(|d| d[0] == calc_field::KIND_CALCULATION)
            && fields
                .at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES, field])
                .is_some_and(|triggers| {
                    triggers.children.iter().any(|t| {
                        definition_of(&t.segment)
                            .is_some_and(|d| calc_triggers::summary_or_global(&d))
                    })
                })
    }

    /// Whether trigger `key` of a formula in `table` names a calculation
    /// over a global or summary field: one of `table`, or one read through
    /// a relationship, resolved to its own table.
    fn input_reads_global_or_summary(&self, table: &[u8], key: &[u8]) -> bool {
        if calc_triggers::is_event(key) {
            return false;
        }
        match calc_triggers::related_input(key) {
            Some((occurrence, field)) => self
                .occurrence(&key_of(occurrence))
                .ok()
                .and_then(Self::occurrence_table)
                .is_some_and(|t| self.reads_global_or_summary(&t, &key_of(field))),
            None => self.reads_global_or_summary(table, key),
        }
    }

    /// Whether a calculation over a global or summary field reads field
    /// `field` of `table`, here or through a relationship.
    fn read_by_global_or_summary_reader(&self, table: &[u8], field: &[u8]) -> bool {
        let own: Vec<(Vec<u8>, Vec<u8>)> = self
            .fields(table)
            .ok()
            .and_then(|f| f.at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES]))
            .map(|depends| {
                depends
                    .children
                    .iter()
                    .filter(|r| r.segment != field && r.child(field).is_some())
                    .map(|r| (table.to_vec(), r.segment.clone()))
                    .collect()
            })
            .unwrap_or_default();
        own.into_iter()
            .chain(self.related_readers(None, Some(table), id_of(field)))
            .any(|(t, f)| self.reads_global_or_summary(&t, &f))
    }

    /// List calculation `field` in its table's summary list `fffb` while it
    /// reads a summary field, as FileMaker's engine lists such a
    /// calculation with the summaries (5.16), and take it out when it no
    /// longer does; the list goes once empty.
    fn list_summary_reader(
        &mut self,
        table: &[u8],
        field: &[u8],
        triggers: &Triggers,
    ) -> Result<(), DataError> {
        let fields = self.fields(table)?;
        let reads_summary = triggers.keys().any(|key| {
            fields
                .at(&[&[MEMBERS], key, &[DEFINITION]])
                .and_then(|d| d.value.as_deref())
                .is_some_and(|d| d.len() == 26 && d[0] == KIND_SUMMARY)
        });
        let listed = self
            .dependency_entry_of(table)
            .and_then(|entry| entry.at(&[SUMMARIES, field]))
            .is_some();
        if reads_summary == listed {
            return Ok(());
        }
        self.catalog(CatalogPath::Occurrences);
        let entry = self
            .dependency_entry(table)
            .ok_or_else(|| unsupported("The table has no entry in the dependency index."))?;
        if reads_summary {
            entry
                .ensure(&[SUMMARIES])
                .set_child(OwnedNode::leaf(field.to_vec(), Vec::new()));
        } else if let Some(list) = entry.child_mut(SUMMARIES) {
            list.children.retain(|c| c.segment != field);
            if list.children.is_empty() {
                entry.remove(&[SUMMARIES]);
            }
        }
        Ok(())
    }

    fn check_computable(
        &self,
        table: &[u8],
        field: &[u8],
        calculation: &CalculationField,
        compiled: &CompiledFormula,
        triggers: &Triggers,
    ) -> Result<(), DataError> {
        let refuse = |reason: &str| {
            Err(unsupported(format!(
                "OpenMaker cannot yet keep this calculation computed: {reason}"
            )))
        };
        if calculation.storage.global {
            return refuse("nothing computes a global calculation's value yet.");
        }
        if calculation.repetitions != 1 {
            return refuse("repeating calculations are not computed yet.");
        }
        let fields = self.fields(table)?;
        let definition = |key: &[u8]| {
            fields
                .at(&[&[MEMBERS], key, &[DEFINITION]])
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        if calculation.stored() {
            if calculation.result == FieldKind::Container {
                return refuse("a stored container result is not computed.");
            }
            let reads_container = triggers
                .keys()
                .filter_map(|key| definition(key))
                .any(|d| d[1] == type_code(FieldKind::Container));
            if reads_container {
                return refuse(
                    "a stored calculation that reads a container field is not computed.",
                );
            }
            let context = om_formula::Key::from_id(calculation.context).0;
            if let Some(problem) = crate::rules::stored_formula_problem(&compiled.polish, &context)
            {
                return refuse(&problem);
            }
        }
        // Stored calculations that read this field need it to stay a
        // stored value they can read.
        if calculation.stored() && calculation.result != FieldKind::Container {
            return Ok(());
        }
        let depends = fields.at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES]);
        let readers = depends.map_or(&[][..], |d| d.children.as_slice());
        for reader in readers {
            if reader.segment == field || reader.child(field).is_none() {
                continue;
            }
            let Some(def) = definition(&reader.segment) else {
                continue;
            };
            let options = u32::from_be_bytes(def[OPTIONS].try_into().expect("4 bytes"));
            if def[0] == calc_field::KIND_CALCULATION && options & (GLOBAL | 0x2_0000) == 0 {
                let name = fields
                    .at(&[&[MEMBERS], &reader.segment])
                    .and_then(member_name)
                    .unwrap_or_default();
                return refuse(&format!(
                    "the stored calculation “{name}” reads this field, and could no longer be \
                     computed."
                ));
            }
        }
        Ok(())
    }

    /// The result type of summary `def` for a field of `table`, checking
    /// what it summarizes and its options (schema-path.md 5.11): the
    /// summarized field is one of the table's own; a restarting summary's
    /// break field is one of the table's own through one of its
    /// occurrences.
    fn summary_result(
        &self,
        table: &[u8],
        def: &crate::meta::SummaryDef,
    ) -> Result<FieldKind, DataError> {
        crate::summary_field::check_options(def).map_err(unsupported)?;
        let fields = self.fields(table)?;
        let definition = |id: u64| {
            fields
                .at(&[&[MEMBERS], &key_of(id), &[DEFINITION]])
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        let spec = def.field.expect("checked");
        if spec.occurrence.is_some() || spec.repetition != 1 {
            return Err(unsupported("The summarized field is read from this table."));
        }
        let summarized = definition(spec.field).ok_or_else(|| {
            DataError::with_code(FIELD_MISSING, "The summarized field no longer exists.")
        })?;
        if summarized[0] == KIND_SUMMARY {
            return Err(refuse(
                "A summary field cannot summarize another summary field.",
            ));
        }
        let data_type = calc_field::data_type(summarized[1])
            .ok_or_else(|| unsupported("The summarized field's type cannot be read."))?;
        let result = crate::summary_field::kind_result(def.kind, data_type).map_err(refuse)?;
        if let Some(extra) = def.additional {
            let occurrence = extra
                .occurrence
                .ok_or_else(|| unsupported("The second field is read through an occurrence."))?;
            if Self::occurrence_table(self.occurrence(&key_of(occurrence))?).as_deref()
                != Some(table)
                || extra.repetition != 1
            {
                return Err(unsupported(
                    "OpenMaker reads a summary's second field only from its own table.",
                ));
            }
            let second = definition(extra.field).ok_or_else(|| {
                DataError::with_code(FIELD_MISSING, "The second field no longer exists.")
            })?;
            // A weight is a number; groups are by a field that sorts. As
            // the dialog offers them [I: FileMaker Pro's own refusals are
            // unmeasured].
            let second_type = calc_field::data_type(second[1]);
            let weighted = def.kind == crate::meta::SummaryKind::Average;
            if second[0] == KIND_SUMMARY
                || weighted && second_type != Some(FieldKind::Number)
                || second_type == Some(FieldKind::Container)
            {
                return Err(refuse(if weighted {
                    "A weighted average is weighted by a number field that is not a summary."
                } else {
                    "A summary's groups are by a field that is not a summary or container field."
                }));
            }
        }
        Ok(result)
    }

    /// Options for Summary Field's OK for a new summary field
    /// (schema-path.md 5.11): the definition, `09`, the name, the comment
    /// and `fb` = `01 01`; the name lists; its trigger on the summarized
    /// field (type 0) in `fff0`, its entry in `fffb` and the rebuilt
    /// evaluation lists.
    fn create_summary_field(
        &mut self,
        table: &[u8],
        name: &str,
        comment: &str,
        def: &crate::meta::SummaryDef,
    ) -> Result<(), DataError> {
        let catalog = self.fields(table)?;
        if let Some(other) = named(catalog, name, None) {
            return Err(taken(format!(
                "The table already has a field named “{}”.",
                member_name(other).unwrap_or_default()
            )));
        }
        let result = self.summary_result(table, def)?;
        self.check_triggers(table)?;
        let catalog = self.fields(table)?;
        let language = Self::language(catalog);
        let mut definition = field_catalog::definition(field_catalog::ScalarType::Text, language);
        definition[0] = KIND_SUMMARY;
        definition[1] = type_code(result);
        let key = next_key(catalog)?;
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
        member.set_child(OwnedNode::leaf(vec![SUMMARY], def.encode().to_vec()));
        set_comment(&mut member, comment);
        member.set_child(om_scripts::text_node(&[NAME], name));
        member.set_child(OwnedNode::leaf(
            vec![RELATED],
            key_string(&serial::encode(1)),
        ));
        let which = CatalogPath::Fields(table.to_vec());
        let has_typed = {
            let catalog = self.catalog_mut(&which);
            members(catalog).is_empty() || catalog.at(&[&[INDEX], &[TYPED_NAMES]]).is_some()
        };
        {
            let typed_entry = typed(KIND_SUMMARY, definition[1], false, name)?;
            check_free(self.catalog_mut(&which), TYPED_NAMES, &typed_entry, &key)?;
            let mut fields = self.catalog(which.clone());
            fields.add(&key, member, Some(name))?;
            if has_typed {
                fields
                    .index()
                    .ensure(&[&[TYPED_NAMES]])
                    .set_child(OwnedNode::leaf(typed_entry, key_string(&key)));
            }
        }
        self.created.insert(MemberRef {
            catalog: which,
            key: key.clone(),
        });
        self.write_summary_dependencies(table, &key, def)
    }

    /// Options for Summary Field's OK for an existing summary field: its
    /// definition `09`, its type, and its dependency entries. A summary
    /// stores no values; the host is told the definition changed
    /// (schema-path.md 7.2: `0x40`, `0x80`, `0x200`).
    fn change_summary(
        &mut self,
        table: &[u8],
        field: &[u8],
        def: &crate::meta::SummaryDef,
    ) -> Result<(), DataError> {
        let member = self.field_member(table, field)?;
        let mut definition = Self::definition_of(member)?;
        if definition[0] != KIND_SUMMARY {
            return Err(unsupported("Only a summary field has summary options."));
        }
        let name = member_name(member).unwrap_or_default();
        let stored = member.child(&[SUMMARY]).and_then(|n| n.value.clone());
        let encoded = def.encode().to_vec();
        if stored.as_deref() == Some(encoded.as_slice()) {
            return Ok(());
        }
        let result = self.summary_result(table, def)?;
        self.check_triggers(table)?;
        let old_type = definition[1];
        definition[1] = type_code(result);
        let which = CatalogPath::Fields(table.to_vec());
        let created = self.created.contains(&MemberRef {
            catalog: which.clone(),
            key: field.to_vec(),
        });
        {
            let typed_entry = if old_type != definition[1]
                && self
                    .fields(table)?
                    .at(&[&[INDEX], &[TYPED_NAMES]])
                    .is_some()
            {
                let entry = typed(KIND_SUMMARY, definition[1], false, &name)?;
                check_free(self.fields(table)?, TYPED_NAMES, &entry, field)?;
                Some(entry)
            } else {
                None
            };
            let mut fields = self.catalog(which.clone());
            if let Some(entry) = typed_entry {
                let list = fields.index().ensure(&[&[TYPED_NAMES]]);
                unlist_member(list, field);
                list.set_child(OwnedNode::leaf(entry, key_string(field)));
            }
            let member = fields
                .node
                .at_mut(&[&[MEMBERS], field])
                .expect("checked above");
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
            member.set_child(OwnedNode::leaf(vec![SUMMARY], encoded));
        }
        self.write_summary_dependencies(table, field, def)?;
        if !created {
            *self
                .work
                .entry((table.to_vec(), field.to_vec()))
                .or_default() |= WORK_MEMBER | WORK_DEFINITION | WORK_CONVERT;
        }
        self.touch(which, field);
        Ok(())
    }

    /// A summary's trigger on the field it summarizes (type 0) in `fff0`,
    /// its entry in the table's summary list `fffb`, and the rebuilt
    /// evaluation lists (schema-path.md 5.11).
    fn write_summary_dependencies(
        &mut self,
        table: &[u8],
        field: &[u8],
        def: &crate::meta::SummaryDef,
    ) -> Result<(), DataError> {
        let summarized = key_of(def.field.expect("checked").field);
        let mut triggers: Triggers = [(summarized, 0)].into();
        // A weighted average and a subtotaled fraction also read their
        // second field, through its occurrence, with type 2; a restarting
        // summary's break field is in its definition only (5.11, runs s03,
        // s10 and s13).
        let reads_second = matches!(
            def.kind,
            crate::meta::SummaryKind::Average | crate::meta::SummaryKind::Fraction
        );
        if reads_second && def.has(crate::meta::summary_flag::OPTION) {
            if let Some(extra) = def.additional {
                let occurrence = extra.occurrence.expect("checked by summary_result");
                triggers.insert(
                    calc_triggers::related_key(&key_of(occurrence), &key_of(extra.field)),
                    2,
                );
            }
        }
        self.catalog(CatalogPath::Occurrences);
        self.dependency_entry(table)
            .expect("checked by check_triggers")
            .ensure(&[SUMMARIES])
            .set_child(OwnedNode::leaf(field.to_vec(), Vec::new()));
        self.write_calculation_dependencies(table, field, &triggers)
    }

    /// The table's field catalog's `fff0` and its entry in the dependency
    /// index, which every table FileMaker made has.
    fn triggers_of(&self, table: &[u8]) -> Result<(OwnedNode, OwnedNode), DataError> {
        let depends = self
            .fields(table)?
            .at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES])
            .cloned()
            .unwrap_or_else(|| OwnedNode::list(FIELD_DEPENDENCIES.to_vec(), Vec::new()));
        let entry = self
            .current
            .occurrences
            .as_ref()
            .and_then(|c| c.at(&[&[INDEX], &[DEPENDENCIES], table]))
            .cloned()
            .ok_or_else(|| {
                unsupported("The table has no entry in the dependency index OpenMaker can edit.")
            })?;
        Ok((depends, entry))
    }

    /// The triggers `table`'s lookups and auto-entered formulas over
    /// related fields have besides `fff0` (5.13): for each node
    /// `[T, 3, 1, 0e, fb, F, <context>, <occurrence>]`, the fields on the
    /// context's side of every predicate of the first relationship on the
    /// way to the occurrence, its route `[3, 17, 1, 08, <context>,
    /// <occurrence>]`. A route or relationship that does not read is
    /// refused.
    fn derived_triggers(&self, table: &[u8]) -> Result<calc_triggers::Derived, DataError> {
        let mut derived = calc_triggers::Derived::new();
        let Some(reads) = self
            .fields(table)?
            .at(&[&[INDEX], &[DEPENDENCIES], &[RELATED]])
        else {
            return Ok(derived);
        };
        let unreadable =
            || unsupported("A lookup's or a related formula's relationship cannot be read.");
        let graph = readable_graph(self.current.relationships.as_ref())?;
        let routes = self
            .current
            .occurrences
            .as_ref()
            .and_then(|c| c.at(&[&[INDEX], &[ROUTES]]));
        for field in &reads.children {
            let matches = derived.entry(field.segment.clone()).or_default();
            for context in &field.children {
                for occurrence in &context.children {
                    let key = routes
                        .and_then(|r| r.at(&[&context.segment, &occurrence.segment]))
                        .and_then(|n| n.value.as_deref())
                        .and_then(key_string_key)
                        .ok_or_else(unreadable)?;
                    let (edge, pairs) = graph
                        .iter()
                        .find(|(edge, _)| edge.key == key)
                        .ok_or_else(unreadable)?;
                    let left = if edge.left == context.segment {
                        true
                    } else if edge.right == context.segment {
                        false
                    } else {
                        return Err(unreadable());
                    };
                    for (l, r) in pairs {
                        matches.insert(if left { l.clone() } else { r.clone() });
                    }
                }
            }
        }
        Ok(derived)
    }

    /// Refuse to rewrite a table's evaluation lists unless rebuilding them
    /// from its fields gives back exactly what they hold (5.10, 5.13).
    fn check_triggers(&self, table: &[u8]) -> Result<(), DataError> {
        let (depends, entry) = self.triggers_of(table)?;
        let derived = self.derived_triggers(table)?;
        if calc_triggers::reproduces(&entry, &depends, &derived, self.fields(table)?) {
            Ok(())
        } else {
            Err(unsupported(
                "OpenMaker cannot yet change the calculations of this table: its dependency \
                 index has entries OpenMaker does not rebuild.",
            ))
        }
    }

    /// Write calculation field `field`'s direct triggers into its field
    /// catalog's `fff0` and rebuild the table's evaluation lists in the
    /// dependency index (`[3, 17, 1, 0e, T]`, schema-path.md 5.10).
    fn write_calculation_dependencies(
        &mut self,
        table: &[u8],
        field: &[u8],
        triggers: &Triggers,
    ) -> Result<(), DataError> {
        let (mut depends, _) = self.triggers_of(table)?;
        calc_triggers::write(&mut depends, field, triggers);
        {
            let mut catalog = self.catalog(CatalogPath::Fields(table.to_vec()));
            let index = catalog.index().ensure(&[&[DEPENDENCIES]]);
            index.children.retain(|c| c.segment != FIELD_DEPENDENCIES);
            index.set_child(depends);
        }
        self.rebuild_triggers(table)
    }

    /// Recompute option `0x80000` on every calculation of `table`, as the
    /// engine does at each rebuild (5.10, 5.12): a calculation is pure
    /// when every input it reads with a type other than 0 is a pure
    /// calculation. A changed definition is uploaded with its member.
    fn refresh_purity(&mut self, table: &[u8]) -> Result<bool, DataError> {
        const PURE: u32 = 0x8_0000;
        let fields = self.fields(table)?;
        let direct = fields
            .at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES])
            .map(calc_triggers::read)
            .transpose()
            .map_err(unsupported)?
            .unwrap_or_default();
        let definition = |key: &[u8]| {
            fields
                .at(&[&[MEMBERS], key, &[DEFINITION]])
                .and_then(|d| d.value.clone())
                .filter(|d| d.len() == 26)
        };
        let is_calc = |d: &[u8]| d[0] == calc_field::KIND_CALCULATION;
        let options = |d: &[u8]| u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes"));
        // Related inputs keep the purity their own table has.
        let related_pure = |key: &[u8]| {
            calc_triggers::related_input(key)
                .and_then(|(o, f)| self.resolve_field(o, f))
                .is_some_and(|(_, d)| is_calc(&d) && options(&d) & PURE != 0)
        };
        let mut pure: BTreeMap<Vec<u8>, bool> = BTreeMap::new();
        // Least fixed point from "every calculation is pure": a field
        // turns impure when one of its inputs is not pure.
        for field in direct.keys() {
            if definition(field).is_some_and(|d| is_calc(&d)) {
                pure.insert(field.clone(), true);
            }
        }
        loop {
            let mut changed = false;
            for (field, triggers) in &direct {
                if pure.get(field) != Some(&true) {
                    continue;
                }
                let impure = triggers.iter().any(|(key, typ)| {
                    if *typ == 0 || (key.len() == 2 && key[0] == 0xff) {
                        return false;
                    }
                    if let Some((o, f)) = calc_triggers::related_input(key) {
                        // Through another occurrence of this same table,
                        // the field's purity is the one being computed.
                        let same = self
                            .occurrence(&key_of(o))
                            .ok()
                            .and_then(Self::occurrence_table)
                            .is_some_and(|t| t == table);
                        if same {
                            return !pure.get(&key_of(f)).copied().unwrap_or(false);
                        }
                        return !related_pure(key);
                    }
                    !pure.get(key).copied().unwrap_or(false)
                });
                if impure {
                    pure.insert(field.clone(), false);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let updates: Vec<(Vec<u8>, Vec<u8>)> = pure
            .iter()
            .filter_map(|(field, pure)| {
                let mut d = definition(field)?;
                let before = options(&d);
                let after = if *pure { before | PURE } else { before & !PURE };
                (before != after).then(|| {
                    d[OPTIONS].copy_from_slice(&after.to_be_bytes());
                    (field.clone(), d)
                })
            })
            .collect();
        let which = CatalogPath::Fields(table.to_vec());
        let changed = !updates.is_empty();
        for (field, d) in updates {
            if let Some(member) = self
                .catalog(which.clone())
                .node
                .at_mut(&[&[MEMBERS], &field])
            {
                member.set_child(OwnedNode::leaf(vec![DEFINITION], d));
            }
            self.touch(which.clone(), &field);
        }
        Ok(changed)
    }

    /// Rebuild the table's evaluation lists in the dependency index from
    /// its field catalog's `fff0` and definitions (5.10).
    fn rebuild_triggers(&mut self, table: &[u8]) -> Result<(), DataError> {
        // Purity follows inputs across relationships: recompute it in this
        // table and every table that reads it, until nothing changes, then
        // rebuild the entries whose listings it changed.
        let mut rebuild = BTreeSet::from([table.to_vec()]);
        // The edited field's own bit is already set: its readers elsewhere
        // are checked at once.
        let mut check: BTreeSet<Vec<u8>> = self
            .related_readers(None, Some(table), None)
            .into_iter()
            .map(|(reader, _)| reader)
            .collect();
        check.insert(table.to_vec());
        for _ in 0..64 {
            let mut changed = BTreeSet::new();
            for t in &check {
                if self.refresh_purity(t)? {
                    changed.insert(t.clone());
                }
            }
            if changed.is_empty() {
                break;
            }
            rebuild.extend(changed.iter().cloned());
            check = changed
                .iter()
                .flat_map(|t| self.related_readers(None, Some(t), None))
                .map(|(reader, _)| reader)
                .collect();
        }
        for t in &rebuild {
            if t != table && self.fields(t).is_ok() {
                self.rebuild_entry(t)?;
            }
        }
        self.rebuild_entry(table)
    }

    /// Rebuild one table's evaluation lists as they are.
    fn rebuild_entry(&mut self, table: &[u8]) -> Result<(), DataError> {
        let (depends, mut entry) = self.triggers_of(table)?;
        let derived = self.derived_triggers(table)?;
        calc_triggers::rebuild(&mut entry, &depends, &derived, self.fields(table)?)
            .map_err(unsupported)?;
        self.catalog(CatalogPath::Occurrences);
        *self.dependency_entry(table).expect("read above") = entry;
        Ok(())
    }

    /// The calculations that read fields through occurrence `occurrence`
    /// (`None`: any) of table `table` (`None`: any), field `field`
    /// (`None`: any): their related triggers in every table's `fff0`
    /// (5.12), as (table, field) of the reader.
    fn related_readers(
        &self,
        occurrence: Option<&[u8]>,
        table: Option<&[u8]>,
        field: Option<u64>,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut readers = Vec::new();
        for (reader_table, catalog) in &self.current.fields {
            let Some(depends) = catalog.at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES]) else {
                continue;
            };
            for reader in &depends.children {
                let reads = reader.children.iter().any(|t| {
                    let Some((o, f)) = calc_triggers::related_input(&t.segment) else {
                        return false;
                    };
                    let o = key_of(o);
                    occurrence.is_none_or(|x| x == o.as_slice())
                        && field.is_none_or(|x| x == f)
                        && table.is_none_or(|x| {
                            self.occurrence(&o)
                                .ok()
                                .and_then(Self::occurrence_table)
                                .as_deref()
                                == Some(x)
                        })
                });
                if reads {
                    readers.push((reader_table.clone(), reader.segment.clone()));
                }
            }
        }
        // Lookups and related auto-entered formulas, which read through
        // their `fb` node along a path of relationships (a lookup also
        // copies the field its definition names, 5.13).
        for read in self.fb_reads() {
            let catalog = &self.current.fields[&read.table];
            let copied = catalog
                .at(&[&[MEMBERS], &read.field, &[LOOKUP]])
                .and_then(|n| n.assembled())
                .and_then(|b| Lookup::decode(&b, None))
                .map(|l| l.copied);
            // The occurrences it goes through, the start only for the
            // occurrence itself (its own table's).
            let table_of = |o: &Vec<u8>| self.occurrence(o).ok().and_then(Self::occurrence_table);
            // A table anywhere on the path; a field only of the table read
            // (field ids are a table's own).
            let in_table = match field {
                None => table.is_none_or(|x| {
                    read.through
                        .iter()
                        .any(|o| table_of(o).as_deref() == Some(x))
                }),
                Some(f) => {
                    copied == Some(f)
                        && table.is_none_or(|x| {
                            read.through.last().and_then(table_of).as_deref() == Some(x)
                        })
                }
            };
            let hit = occurrence
                .is_none_or(|x| x == read.context || read.through.iter().any(|o| o == x))
                && in_table;
            let entry = (read.table, read.field);
            if hit && !readers.contains(&entry) {
                readers.push(entry);
            }
        }
        readers
    }

    /// Every `[T, 3, 1, 0e, fb, F, <context>, <occurrence>]` node with the
    /// path of relationships from the context to the occurrence: the
    /// relationships' keys and the occurrences after the context. A node
    /// whose occurrences the graph does not connect has an empty path.
    fn fb_reads(&self) -> Vec<FbRead> {
        let graph = edges(self.current.relationships.as_ref());
        let mut reads = Vec::new();
        for (table, catalog) in &self.current.fields {
            let Some(nodes) = catalog.at(&[&[INDEX], &[DEPENDENCIES], &[RELATED_READS]]) else {
                continue;
            };
            for field in &nodes.children {
                for context in &field.children {
                    for occurrence in &context.children {
                        let (relationships, through) =
                            graph_path(&graph, &context.segment, &occurrence.segment)
                                .unwrap_or_else(|| (Vec::new(), vec![occurrence.segment.clone()]));
                        reads.push(FbRead {
                            table: table.clone(),
                            field: field.segment.clone(),
                            context: context.segment.clone(),
                            relationships,
                            through,
                        });
                    }
                }
            }
        }
        reads
    }

    /// The lookups and related auto-entered formulas whose path to an
    /// occurrence they read goes through relationship `key` (5.13): the
    /// route's first relationship gives their derived triggers, and the
    /// path ends where it is cut.
    fn routed_readers(&self, key: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut readers = Vec::new();
        for read in self.fb_reads() {
            let entry = (read.table, read.field);
            if read.relationships.iter().any(|r| r == key) && !readers.contains(&entry) {
                readers.push(entry);
            }
        }
        readers
    }

    /// Why a deletion would leave a calculation reading through a missing
    /// occurrence, table or field (5.12), if it would.
    fn read_through(&self, readers: &[(Vec<u8>, Vec<u8>)]) -> Result<(), DataError> {
        let Some((table, field)) = readers.first() else {
            return Ok(());
        };
        let name = self
            .fields(table)
            .ok()
            .and_then(|c| c.at(&[&[MEMBERS], field]))
            .and_then(member_name)
            .unwrap_or_default();
        Err(unsupported(format!(
            "OpenMaker cannot yet delete this: the field “{name}” reads it through a \
             relationship."
        )))
    }

    /// Whether a summary field of `table` summarizes field `field` or
    /// restarts by it (its `09`, file-metadata.md 5.1).
    fn summary_uses(&self, table: &[u8], field: &[u8]) -> bool {
        let (Some(fields), Some(id)) = (self.current.fields.get(table), id_of(field)) else {
            return false;
        };
        members(fields).iter().any(|member| {
            let summary = member
                .child(&[DEFINITION])
                .and_then(|d| d.value.as_deref())
                .is_some_and(|d| d.first() == Some(&KIND_SUMMARY));
            summary
                && member
                    .child(&[SUMMARY])
                    .and_then(|n| n.value.as_deref())
                    .map(crate::meta::SummaryDef::decode)
                    .is_some_and(|def| {
                        [def.field, def.additional]
                            .iter()
                            .flatten()
                            .any(|spec| spec.field == id)
                    })
        })
    }

    /// Why field `field` of `table` cannot be deleted or have its storage
    /// changed yet, if it cannot: a field that other definitions use
    /// ([`Self::use_blocked`]), or one with options or dependency data of
    /// its own.
    fn deletion_blocked(&self, table: &[u8], field: &[u8]) -> Option<&'static str> {
        let member = self.field_member(table, field).ok()?;
        let definition = Self::definition_of(member).ok()?;
        let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        let validation = u32::from_be_bytes(definition[VALIDATION].try_into().expect("4 bytes"));
        if definition[0] != KIND_NORMAL
            || options & AUTO_ENTER != 0
            || validation & 0x1 != 0
            || member.child(&[0x05]).is_some()
        {
            return Some(
                "it is a calculation or summary field, or has auto-enter or calculated options",
            );
        }
        if let Some(why) = self.use_blocked(table, field) {
            return Some(why);
        }
        if self.listed_in_entry(table, field) {
            return Some("a calculation, an option or a relationship uses it");
        }
        None
    }

    /// Whether a field of `table` with furigana on writes its reading into
    /// `field` (option `0x40`, member `07`'s second part, 5.9).
    fn furigana_target(&self, table: &[u8], field: &[u8]) -> bool {
        let Some(id) = id_of(field) else {
            return false;
        };
        let Some(catalog) = self.current.fields.get(table) else {
            return false;
        };
        members(catalog).iter().any(|member| {
            let on = Self::definition_of(member).is_ok_and(|d| {
                u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes")) & FURIGANA_OPTION != 0
            });
            on && member
                .child(&[VALUE_LIST])
                .and_then(|n| n.assembled())
                .and_then(|bytes| bytes.get(6..).and_then(crate::meta::key_string))
                == Some(id)
        })
    }

    /// Why field `field` of `table` cannot be deleted, if another definition
    /// uses it: a summary, a calculation or option of another field, a
    /// calculation or lookup through a relationship, or a relationship's
    /// match pair. FileMaker's engine leaves such a field in place (runs d03
    /// and e05, schema-path.md 5.15); what FileMaker Pro writes for the
    /// definitions it leaves reading a missing field is not established.
    fn use_blocked(&self, table: &[u8], field: &[u8]) -> Option<&'static str> {
        if self.summary_uses(table, field) {
            return Some("a summary field summarizes it or restarts by it");
        }
        if self.furigana_target(table, field) {
            return Some("another field's furigana writes its reading into it");
        }
        // Its own formula reading it through a self-join goes with it.
        if self
            .related_readers(None, Some(table), id_of(field))
            .iter()
            .any(|(t, f)| !(t.as_slice() == table && f.as_slice() == field))
        {
            return Some("a calculation or a lookup reads it through a relationship");
        }
        if self.related_validators(table, field) {
            return Some("a validation calculation reads it through a relationship");
        }
        if let (Some(t), Some(f)) = (id_of(table), id_of(field)) {
            if self.formulas.value_list_fields.contains(&(t, f)) {
                return Some("a value list takes its values from it");
            }
        }
        if self.relationship_sorts_by(table, field) {
            return Some("a relationship sorts its records by it");
        }
        let fields = self.current.fields.get(table)?;
        for list in [FIELD_DEPENDENCIES, VALIDATION_DEPENDENCIES] {
            if let Some(depends) = fields.at(&[&[INDEX], &[DEPENDENCIES], list]) {
                let uses = depends
                    .children
                    .iter()
                    .any(|d| d.segment != field && d.child(field).is_some());
                if uses {
                    return Some("a calculation or an option of another field uses it");
                }
            }
        }
        if let Some(entry) = self.dependency_entry_of(table) {
            // The fields that read `field` are listed under it; a formula
            // reading `Self` lists its own field there too.
            let own = nested_key_string(field);
            let readers = entry.child(field).is_some_and(|list| {
                list.children
                    .iter()
                    .any(|e| !e.value.as_deref().is_some_and(|v| v.ends_with(&own)))
            });
            if readers {
                return Some("a calculation or an option of another field uses it");
            }
        }
        let occurrences: BTreeSet<Vec<u8>> = self
            .current
            .occurrences
            .iter()
            .flat_map(|c| members(c).iter())
            .filter(|o| Self::occurrence_table(o).as_deref() == Some(table))
            .map(|o| o.segment.clone())
            .collect();
        let Ok(graph) = readable_graph(self.current.relationships.as_ref()) else {
            return Some("the file has a relationship OpenMaker cannot read");
        };
        let related = graph.iter().any(|(edge, pairs)| {
            pairs.iter().any(|(left, right)| {
                (occurrences.contains(&edge.left) && left == field)
                    || (occurrences.contains(&edge.right) && right == field)
            })
        });
        if related {
            return Some("a relationship matches on it");
        }
        None
    }

    /// Whether a validation calculation of any table reads field `field`
    /// of `table` through a relationship: a related trigger in its `fff1`
    /// (5.13), whatever the path, which `related_readers` does not scan.
    /// The field's own validation (through a self-join) goes with it.
    fn related_validators(&self, table: &[u8], field: &[u8]) -> bool {
        let Some(id) = id_of(field) else {
            return false;
        };
        self.current.fields.iter().any(|(reader_table, catalog)| {
            catalog
                .at(&[&[INDEX], &[DEPENDENCIES], VALIDATION_DEPENDENCIES])
                .is_some_and(|depends| {
                    depends.children.iter().any(|reader| {
                        let own = reader_table.as_slice() == table && reader.segment == field;
                        !own && reader.children.iter().any(|t| {
                            calc_triggers::related_input(&t.segment).is_some_and(|(o, f)| {
                                f == id
                                    && self
                                        .occurrence(&key_of(o))
                                        .ok()
                                        .and_then(Self::occurrence_table)
                                        .as_deref()
                                        == Some(table)
                            })
                        })
                    })
                })
        })
    }

    /// Whether a relationship's sort order, of either side and also kept
    /// while "Sort records" is off, names field `field` of `table`, as the
    /// sort field or the summary it reorders by.
    fn relationship_sorts_by(&self, table: &[u8], field: &[u8]) -> bool {
        let Some(id) = id_of(field) else {
            return false;
        };
        let table_of = |o: u64| {
            self.occurrence(&key_of(o))
                .ok()
                .and_then(Self::occurrence_table)
        };
        self.current
            .relationships
            .as_ref()
            .into_iter()
            .flat_map(members)
            .filter_map(|member| {
                let id = id_of(&member.segment)?;
                let definition = member.child(&[DEFINITION])?.assembled()?;
                let predicates: Vec<Vec<u8>> = member
                    .child(&[PREDICATES])?
                    .children
                    .iter()
                    .map(|p| p.assembled().map(|b| b.into_owned()))
                    .collect::<Option<_>>()?;
                let predicates: Vec<&[u8]> = predicates.iter().map(Vec::as_slice).collect();
                crate::meta::Relationship::decode(id, &definition, &predicates)
            })
            .any(|r| {
                // The left-to-right order sorts the right side's records.
                [(&r.left_to_right, r.right), (&r.right_to_left, r.left)]
                    .into_iter()
                    .any(|(sort, side)| {
                        sort.fields.iter().any(|entry| {
                            [Some(&entry.field), entry.summary.as_ref()]
                                .into_iter()
                                .flatten()
                                .any(|spec| {
                                    spec.field == id
                                        && table_of(spec.occurrence.unwrap_or(side)).as_deref()
                                            == Some(table)
                                })
                        })
                    })
            })
    }

    /// Table `table`'s entry in the dependency index.
    fn dependency_entry_of(&self, table: &[u8]) -> Option<&OwnedNode> {
        self.current
            .occurrences
            .as_ref()
            .and_then(|c| c.at(&[&[INDEX], &[DEPENDENCIES], table]))
    }

    /// Whether table `table`'s dependency entry lists field `field`: in the
    /// evaluation order `ff` or the validated `fffc`, keyed by field, under
    /// a field it reads, or in `ff00`-`ff02` and the other lists keyed by
    /// position, which name the field in their values as a nested key
    /// string (`02 01 F`).
    fn listed_in_entry(&self, table: &[u8], field: &[u8]) -> bool {
        let Some(entry) = self.dependency_entry_of(table) else {
            return false;
        };
        let keyed = [ORDER_LIST, VALIDATED, SUMMARIES]
            .iter()
            .any(|list| entry.child(list).and_then(|l| l.child(field)).is_some());
        let named = entry
            .children
            .iter()
            .filter(|c| {
                c.segment.starts_with(&[0xff])
                    && ![ORDER_LIST, VALIDATED, CONTAINERS, TABLE_MARKER, SUMMARIES]
                        .contains(&c.segment.as_slice())
            })
            .any(|list| {
                list.children.iter().any(|e| {
                    e.value
                        .as_deref()
                        .is_some_and(|v| v.ends_with(&nested_key_string(field)))
                })
            });
        entry.child(field).is_some() || keyed || named
    }

    /// Duplicate (schema-path.md 5.14): FileMaker Pro copies the field's
    /// member under a new key and the name [`crate::schema::duplicate_name`]
    /// gives, clears the container library flags, and commits it as a new
    /// field. OpenMaker makes the copy through the paths Create and the
    /// options dialogs take, which write the catalog's indexes as the engine
    /// does: a normal field created and given the original's options (their
    /// formulas compiled again from their text), a calculation or summary
    /// field created with its definition. What those paths refuse is
    /// refused, and so is a copy that would not hold what FileMaker's copy
    /// holds ([`duplicate_difference`]).
    fn duplicate_field(&mut self, table: &[u8], field: &[u8]) -> Result<(), DataError> {
        enum Original {
            Normal(FieldKind, FieldOptions),
            Calculation(CalculationField),
            Summary(crate::meta::SummaryDef),
        }
        let member = self.field_member(table, field)?;
        // What FileMaker's copy holds, but for its own name, UUID and
        // bookkeeping.
        let mut expected = member.clone();
        if let Some(tags) = member.child(&[TAGS]).and_then(text_of) {
            match duplicate_tags(&tags) {
                Some(tags) => expected.set_child(om_scripts::text_node(&[TAGS], &tags)),
                None => {
                    expected.remove(&[&[TAGS]]);
                }
            }
        }
        let definition = Self::definition_of(member)?;
        let name = member_name(member).unwrap_or_default();
        let comment = comment_of(member);
        let names = self.formula_names();
        let members = DraftMembers {
            member,
            names: &names,
            decimal_comma: self.formulas.decimal_comma,
        };
        let unreadable =
            || unsupported("OpenMaker cannot read this field's definition to copy it.");
        let copy = match definition[0] {
            KIND_NORMAL => {
                let data_type = calc_field::data_type(definition[1]).ok_or_else(unreadable)?;
                let text = |key| member.child(&[key]).and_then(text_of);
                let mut options = FieldOptions::decode(
                    &definition,
                    text(NEXT_SERIAL).as_deref(),
                    text(AUTO_DATA).as_deref(),
                )
                .ok_or_else(unreadable)?;
                options.read_members(&members);
                Original::Normal(data_type, options)
            }
            calc_field::KIND_CALCULATION => {
                let formula = members.formula(FORMULA).ok_or_else(unreadable)?;
                let text = formula.text.ok_or_else(|| {
                    unsupported("The calculation has no text to copy (calc-encoding.md 1).")
                })?;
                Original::Calculation(
                    CalculationField::decode(&definition, text, formula.context)
                        .ok_or_else(unreadable)?,
                )
            }
            KIND_SUMMARY => {
                let bytes = member
                    .child(&[SUMMARY])
                    .and_then(|n| n.assembled())
                    .ok_or_else(unreadable)?;
                Original::Summary(crate::meta::SummaryDef::decode(&bytes))
            }
            _ => return Err(unreadable()),
        };
        let catalog = self.fields(table)?;
        let copied = crate::schema::duplicate_name(&name, |n| named(catalog, n, None).is_some())
            .ok_or_else(|| unsupported(format!("No name is left for a copy of “{name}”.")))?;
        // FileMaker's copy keeps the original's characters (it trims only
        // spaces at either end), so a name calculations write as `${…}`
        // gives a copy named so too; OpenMaker refuses those only when
        // typed.
        check_name_as(&copied, "field", om_formula::name_needs_quotes(&name))?;
        let created = |draft: &Self| {
            named(draft.fields(table)?, &copied, None)
                .map(|m| m.segment.clone())
                .ok_or_else(unreadable)
        };
        let key = match copy {
            Original::Normal(data_type, mut options) => {
                self.create_field(table, &copied, data_type, &comment)?;
                let key = created(self)?;
                // A new field takes the table's index language; a copy in
                // another one is refused below rather than re-indexed.
                let new = Self::definition_of(self.field_member(table, &key)?)?;
                options.storage.language =
                    u32::from_be_bytes(new[4..8].try_into().expect("4 bytes"));
                self.set_field_options(table, &key, &options)?;
                key
            }
            Original::Calculation(calculation) => {
                self.create_calculation_field(table, &copied, &comment, &calculation)?;
                created(self)?
            }
            Original::Summary(def) => {
                self.create_summary_field(table, &copied, &comment, &def)?;
                created(self)?
            }
        };
        // Members no index holds are FileMaker's copy's: its tags, and the
        // next serial value, which the options path writes "1" as the
        // Auto-Enter tab does.
        let fields = self.catalog(CatalogPath::Fields(table.to_vec()));
        let member = fields
            .node
            .at_mut(&[&[MEMBERS], &key])
            .expect("created above");
        for plain in [TAGS, NEXT_SERIAL] {
            match expected.child(&[plain]) {
                Some(node) => member.set_child(node.clone()),
                None => {
                    member.remove(&[&[plain]]);
                }
            }
        }
        // FileMaker's summary definitions hold bytes no setting reads
        // (padding the template files fill with `fe 7f`, `be bf ef`...),
        // which its copy keeps and OpenMaker writes as zeros: the copy keeps
        // the original's when both read as the same summary.
        let summary = |node: &OwnedNode| {
            node.child(&[SUMMARY])
                .and_then(|n| n.assembled())
                .map(|bytes| crate::meta::SummaryDef::decode(&bytes))
        };
        if let Some(original) = expected.child(&[SUMMARY]) {
            if summary(&expected).is_some() && summary(&expected) == summary(member) {
                member.set_child(original.clone());
            }
        }
        // A lookup definition `08` holds padding no setting reads either:
        // the templates carry other bytes there (`7f`, `be 33 11 00`) than
        // FileMaker Pro 26 writes (5.5), and Duplicate copies the member as
        // it is (5.14). The copy keeps the original's when both read as the
        // same lookup.
        let lookup = |node: &OwnedNode| {
            let constant = node.child(&[LOOKUP_CONSTANT]).and_then(text_of);
            node.child(&[LOOKUP])
                .and_then(|n| n.assembled())
                .and_then(|bytes| Lookup::decode(&bytes, constant))
        };
        if let Some(original) = expected.child(&[LOOKUP]) {
            if lookup(&expected).is_some() && lookup(&expected) == lookup(member) {
                member.set_child(original.clone());
            }
        }
        // Byte 2 (`mAccessLevel`) is 0 or 1 on FileMaker's unstored
        // calculations alike (5.10) and kept on edits; the copy holds the
        // original's.
        let access = expected
            .child(&[DEFINITION])
            .and_then(|n| n.value.as_deref())
            .and_then(|d| d.get(2).copied());
        if let (Some(access), Some(definition)) = (
            access,
            member
                .at_mut(&[&[DEFINITION]])
                .and_then(|n| n.value.as_mut())
                .filter(|d| d.len() > 2),
        ) {
            definition[2] = access;
        }
        match duplicate_difference(&expected, self.field_member(table, &key)?) {
            None => Ok(()),
            Some(what) => Err(unsupported(format!(
                "OpenMaker cannot duplicate “{name}” yet: the copy would not keep {what} as \
                 FileMaker Pro's copy does."
            ))),
        }
    }

    fn delete_field(&mut self, table: &[u8], field: &[u8]) -> Result<(), DataError> {
        let member = self.field_member(table, field)?;
        let definition = Self::definition_of(member)?;
        let name = member_name(member).unwrap_or_default();
        if let Some(why) = self.use_blocked(table, field) {
            return Err(unsupported(format!(
                "OpenMaker cannot delete the field “{name}” yet: {why}."
            )));
        }
        // A field with dependency data of its own (a calculation, a
        // summary, an auto-entered or validation formula, a lookup, an
        // entry option or a validation) leaves the table's evaluation lists
        // rebuilt without it, as FileMaker's engine does (5.15): only
        // where the rebuild reproduces them as they are.
        let own =
            self.deletion_blocked(table, field).is_some() || self.listed_in_entry(table, field);
        if own {
            self.check_triggers(table).map_err(|_| {
                unsupported(format!(
                    "OpenMaker cannot delete the field “{name}” yet: the table's dependency \
                     index has entries OpenMaker does not rebuild."
                ))
            })?;
        }
        let which = CatalogPath::Fields(table.to_vec());
        {
            let mut fields = self.catalog(which.clone());
            fields.remove(field)?;
            if let Some(list) = fields.node.at_mut(&[&[INDEX], &[TYPED_NAMES]]) {
                unlist_member(list, field);
            }
            // Its triggers, validation triggers and related reads; the
            // lists stay, also empty (runs e02, e04).
            for list in [
                FIELD_DEPENDENCIES,
                VALIDATION_DEPENDENCIES,
                &[RELATED_READS],
            ] {
                if let Some(depends) = fields.node.at_mut(&[&[INDEX], &[DEPENDENCIES], list]) {
                    depends.children.retain(|c| c.segment != field);
                }
            }
        }
        if definition[1] == 6 {
            self.list_container(table, field, false);
        }
        if own {
            // Out of the summary list, which goes once empty (e01), and the
            // validated list; then the evaluation lists are rebuilt.
            self.catalog(CatalogPath::Occurrences);
            let entry = self
                .dependency_entry(table)
                .expect("checked by check_triggers");
            if let Some(summaries) = entry.child_mut(SUMMARIES) {
                summaries.children.retain(|c| c.segment != field);
                if summaries.children.is_empty() {
                    entry.remove(&[SUMMARIES]);
                }
            }
            // `fffc, F`, and `fffc, 00, F` for a field validated Always,
            // whose list goes once empty (e07).
            if let Some(validated) = entry.child_mut(VALIDATED) {
                validated.children.retain(|c| c.segment != field);
                if let Some(always) = validated.child_mut(&[0x00]) {
                    always.children.retain(|c| c.segment != field);
                    if always.children.is_empty() {
                        validated.remove(&[&[0x00]]);
                    }
                }
            }
            self.rebuild_triggers(table)?;
        }
        let created = self.created.contains(&MemberRef {
            catalog: which.clone(),
            key: field.to_vec(),
        });
        self.forget(which, field);
        self.field_data_changed
            .remove(&(table.to_vec(), field.to_vec()));
        let key = (table.to_vec(), field.to_vec());
        if created {
            self.work.remove(&key);
        } else {
            let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
            let mut work = WORK_DELETE | WORK_DELETED;
            // The values are where the storage the edit began with keeps
            // them, whatever the field was turned to since.
            if self
                .base_global(table, field)
                .unwrap_or(options & GLOBAL != 0)
            {
                work |= WORK_GLOBAL;
            }
            // Container cleanup the edit already asked for (a container
            // retyped in it) stays.
            let pending = self.work.get(&key).copied().unwrap_or(0) & WORK_CONTAINER;
            if definition[1] == 6 || self.base_container(table, field) {
                work |= WORK_CONTAINER;
            }
            self.work.insert(key, work | pending);
            self.data_changed.insert(table.to_vec());
        }
        Ok(())
    }

    fn create_occurrence(&mut self, table: &[u8], name: &str) -> Result<(), DataError> {
        self.table(table)?;
        check_name(name, "table occurrence")?;
        let catalog = self.catalog_mut(&CatalogPath::Occurrences);
        if let Some(other) = named(catalog, name, None) {
            return Err(taken(format!(
                "The table occurrence name “{}” is already used.",
                member_name(other).unwrap_or_default()
            )));
        }
        let key = Catalog { node: catalog }.next_key()?;
        // This file, the base table, an empty graph box that FileMaker Pro
        // places when it next shows the graph, the two words FileMaker Pro
        // writes as 0 for a new occurrence, and its colour.
        let mut definition = vec![0x02, 0x01, 0x00];
        definition.extend(nested_key_string(table));
        definition.extend([0; 16 + 4 + 4]);
        definition.extend(NEW_OCCURRENCE_COLOR);
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
        member.set_child(om_scripts::text_node(&[NAME], name));
        self.catalog(CatalogPath::Occurrences)
            .add(&key, member, Some(name))?;
        self.created.insert(MemberRef {
            catalog: CatalogPath::Occurrences,
            key,
        });
        Ok(())
    }

    /// The graph's Duplicate ([`SchemaChange::DuplicateOccurrences`]):
    /// each occurrence's definition copied whole (its data source and
    /// table, the two words, its colour) with its box moved by
    /// [`DUPLICATE_OFFSET`] (an empty box, which the graph places, stays
    /// empty), named as a new occurrence of its table is; then each
    /// relationship joining two of them made between the copies from its
    /// stored definition (options, sort orders) and match pairs, as a new
    /// relationship is written.
    fn duplicate_occurrences(&mut self, sources: &[Vec<u8>]) -> Result<(), DataError> {
        if sources.is_empty() {
            return Err(refuse("Select the table occurrences to duplicate."));
        }
        // Copies are made from every relationship among them: all must read.
        readable_graph(self.current.relationships.as_ref())?;
        let mut copies: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for source in sources {
            if copies.contains_key(source) {
                continue;
            }
            let original = self.occurrence(source)?;
            let name = member_name(original).unwrap_or_default();
            let mut definition = original
                .child(&[DEFINITION])
                .and_then(|d| d.value.clone())
                .ok_or_else(|| {
                    unsupported("The occurrence has no definition OpenMaker can read.")
                })?;
            // A definition that ends after its keys has no box yet: the copy
            // gets a new occurrence's (an empty box the graph places, the
            // two words, the default colour). Any other tail must read.
            let tail = {
                let mut rest = definition.as_slice();
                read_nested_key(&mut rest)
                    .and_then(|_| read_nested_key(&mut rest))
                    .map(|_| rest.len())
            };
            if tail == Some(0) {
                definition.extend([0; 16 + 4 + 4]);
                definition.extend(NEW_OCCURRENCE_COLOR);
            }
            let at = Self::box_at(&definition)
                .ok_or_else(|| unsupported("OpenMaker cannot read this occurrence's graph box."))?;
            let float = |bytes: &[u8], at: usize| {
                bytes
                    .get(at..at + 4)
                    .and_then(|b| b.try_into().ok())
                    .map(f32::from_be_bytes)
                    .unwrap_or(0.0)
            };
            if float(&definition, at + 8) > 0.0 && float(&definition, at + 12) > 0.0 {
                for corner in [at, at + 4] {
                    let moved = float(&definition, corner) + DUPLICATE_OFFSET as f32;
                    definition[corner..corner + 4].copy_from_slice(&moved.to_be_bytes());
                }
            }
            // As a new occurrence of a table already shown: "name 2", ...
            let catalog = self.catalog_mut(&CatalogPath::Occurrences);
            let copy_name = (2..)
                .map(|n| format!("{name} {n}"))
                .find(|candidate| named(catalog, candidate, None).is_none())
                .ok_or_else(|| refuse("No table occurrence name is free."))?;
            let key = Catalog { node: catalog }.next_key()?;
            let mut member = OwnedNode::list(key.clone(), Vec::new());
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
            member.set_child(om_scripts::text_node(&[NAME], &copy_name));
            self.catalog(CatalogPath::Occurrences)
                .add(&key, member, Some(&copy_name))?;
            self.created.insert(MemberRef {
                catalog: CatalogPath::Occurrences,
                key: key.clone(),
            });
            copies.insert(source.clone(), key);
        }
        let joined: Vec<OwnedNode> = self
            .current
            .relationships
            .as_ref()
            .map(|catalog| {
                members(catalog)
                    .iter()
                    .filter(|member| {
                        edge_of(member).is_some_and(|e| {
                            copies.contains_key(&e.left) && copies.contains_key(&e.right)
                        })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for member in joined {
            let unread = || unsupported("OpenMaker cannot read this relationship's definition.");
            let edge = edge_of(&member).ok_or_else(unread)?;
            let definition = member
                .child(&[DEFINITION])
                .and_then(|d| d.assembled())
                .map(|d| d.into_owned())
                .ok_or_else(unread)?;
            let options = edge_definition_options(&definition).ok_or_else(unread)?;
            let matches = stored_matches(&member).ok_or_else(unread)?;
            // A sort order names the occurrence whose fields it sorts by:
            // the copy's, in the copy. One that does not read, or names an
            // occurrence not duplicated, is not copied.
            let unsorted = || unsupported("OpenMaker cannot copy this relationship's sort orders.");
            let stored = stored_orders(&definition).ok_or_else(unsorted)?;
            let mut orders: [Option<om_scripts::specs::SortSpec>; 2] = [None, None];
            for (side, bytes) in stored.iter().enumerate() {
                let (mut order, _) =
                    om_scripts::specs::SortSpec::read(bytes).ok_or_else(unsorted)?;
                // Rewritten only where it reads back to its own bytes: a form
                // the codec would normalize is not copied.
                let lossless = order.encode() == *bytes;
                let mut named = false;
                for entry in &mut order.fields {
                    for reference in std::iter::once(&mut entry.field).chain(entry.summary.as_mut())
                    {
                        if let Some(id) = reference.occurrence {
                            let copy = copies.get(&key_of(id)).and_then(|key| id_of(key));
                            reference.occurrence = Some(copy.ok_or_else(unsorted)?);
                            named = true;
                        }
                    }
                }
                if named {
                    if !lossless {
                        return Err(unsorted());
                    }
                    orders[side] = Some(order);
                }
            }
            self.create_relationship(
                (&copies[&edge.left], &copies[&edge.right]),
                &matches,
                options,
                (Some(&definition), [orders[0].as_ref(), orders[1].as_ref()]),
            )?;
        }
        Ok(())
    }

    fn rename_occurrence(&mut self, occurrence: &[u8], name: &str) -> Result<(), DataError> {
        let old = member_name(self.occurrence(occurrence)?).unwrap_or_default();
        if old == name {
            return Ok(());
        }
        check_name(name, "table occurrence")?;
        let catalog = self.catalog_mut(&CatalogPath::Occurrences);
        if let Some(other) = named(catalog, name, Some(occurrence)) {
            return Err(taken(format!(
                "The table occurrence name “{}” is already used.",
                member_name(other).unwrap_or_default()
            )));
        }
        self.catalog(CatalogPath::Occurrences)
            .rename(occurrence, name)?;
        self.touch(CatalogPath::Occurrences, occurrence);
        Ok(())
    }

    /// Delete an occurrence with its relationships, as FileMaker Pro does:
    /// the relationships go from `[3, fb]`, the other occurrences' lists of
    /// relationships lose them, and the routes `[3, 17, 1, 08]` and the key
    /// fields' reach in the dependency index lose every occurrence that is
    /// no longer connected (schema-path.md 5.3, 5.4).
    fn delete_occurrence(&mut self, occurrence: &[u8]) -> Result<(), DataError> {
        self.occurrence(occurrence)?;
        self.read_through(&self.related_readers(Some(occurrence), None, None))?;
        let doomed: Vec<Edge> = readable_graph(self.current.relationships.as_ref())?
            .into_iter()
            .map(|(edge, _)| edge)
            .filter(|e| e.left == occurrence || e.right == occurrence)
            .collect();
        for edge in &doomed {
            self.remove_edge(edge)?;
        }
        self.catalog(CatalogPath::Occurrences).remove(occurrence)?;
        self.forget(CatalogPath::Occurrences, occurrence);
        if !doomed.is_empty() {
            self.prune_graph();
        }
        Ok(())
    }

    /// Remove relationship `edge` and take it from its occurrences' lists
    /// of relationships `[3, 17, 5, O, fb]`.
    fn remove_edge(&mut self, edge: &Edge) -> Result<(), DataError> {
        self.catalog(CatalogPath::Relationships).remove(&edge.key)?;
        self.forget(CatalogPath::Relationships, &edge.key);
        for (side, other) in [(&edge.left, &edge.right), (&edge.right, &edge.left)] {
            let occurrences = self.catalog(CatalogPath::Occurrences);
            let Some(member) = occurrences.node.at_mut(&[&[MEMBERS], side]) else {
                continue;
            };
            if let Some(list) = member.child_mut(&[RELATED]) {
                list.children.retain(|c| c.segment != *other);
                if list.children.is_empty() {
                    member.children.retain(|c| c.segment != [RELATED]);
                }
            }
            self.touch(CatalogPath::Occurrences, side);
        }
        Ok(())
    }

    /// After relationships or occurrences went: the routes
    /// `[3, 17, 1, 08]` and key-field reach `[3, 17, 1, 0e, T, 00]` keep
    /// only occurrences that are still connected (the graph is a forest,
    /// so a path that remains is the same path), and a field that is no
    /// longer a key loses its marker `[3, 17, 1, 0e, T, F, 00]`, as
    /// FileMaker Pro left them (schema-path.md 5.8, E8).
    fn prune_graph(&mut self) {
        let edges = edges(self.current.relationships.as_ref());
        let after = components(&edges);
        let occurrences: BTreeSet<Vec<u8>> = self
            .current
            .occurrences
            .iter()
            .flat_map(|c| members(c).iter().map(|m| m.segment.clone()))
            .collect();
        let connected = |a: &[u8], b: &[u8]| {
            a != b
                && occurrences.contains(a)
                && occurrences.contains(b)
                && after.get(a).is_some_and(|c| after.get(b) == Some(c))
        };
        self.changed.insert(CatalogPath::Occurrences);
        let Some(index) = self
            .current
            .occurrences
            .as_mut()
            .and_then(|c| c.child_mut(&[INDEX]))
        else {
            return;
        };
        if let Some(routes) = index.child_mut(&[ROUTES]) {
            for from in &mut routes.children {
                let source = from.segment.clone();
                from.children.retain(|to| connected(&source, &to.segment));
            }
            routes.children.retain(|from| !from.children.is_empty());
        }
        if let Some(dependencies) = index.child_mut(&[DEPENDENCIES]) {
            for entry in &mut dependencies.children {
                let mut keys_left = BTreeSet::new();
                if let Some(reach) = entry.child_mut(&[REACH]) {
                    // Related inputs' entries (`00, <occurrence>, ...`,
                    // 5.12) belong to calculations, not to the graph.
                    for key_field in reach
                        .children
                        .iter_mut()
                        .filter(|k| !occurrences.contains(&k.segment))
                    {
                        for from in &mut key_field.children {
                            let source = from.segment.clone();
                            from.children.retain(|to| connected(&source, &to.segment));
                        }
                        key_field.children.retain(|from| !from.children.is_empty());
                        if !key_field.children.is_empty() {
                            keys_left.insert(key_field.segment.clone());
                        }
                    }
                    reach.children.retain(|f| !f.children.is_empty());
                }
                if entry.child(&[REACH]).is_some_and(|r| r.children.is_empty()) {
                    entry.children.retain(|c| c.segment != [REACH]);
                }
                // A field that is no longer a key loses its marker; a
                // related input's record `<occurrence>, 00` stays.
                for child in &mut entry.children {
                    if child.segment != [REACH]
                        && !child.segment.starts_with(&[0xff])
                        && !keys_left.contains(&child.segment)
                        && !occurrences.contains(&child.segment)
                    {
                        child.children.retain(|c| c.segment != [KEY_MARKER]);
                    }
                }
                entry.children.retain(|c| {
                    c.value.is_some() || !c.children.is_empty() || c.segment.starts_with(&[0xff])
                });
            }
            dependencies.children.retain(|e| !e.children.is_empty());
        }
    }

    /// The base table key of occurrence `occurrence`, which must show one
    /// of this file's tables.
    fn table_of(&self, occurrence: &[u8]) -> Result<Vec<u8>, DataError> {
        let table = Self::occurrence_table(self.occurrence(occurrence)?).ok_or_else(|| {
            unsupported("OpenMaker relates only occurrences of this file's tables.")
        })?;
        self.table(&table)?;
        Ok(table)
    }

    /// Check a relationship's match pairs: at least one, operators 0 to 6,
    /// each field one of its occurrence's table.
    fn check_matches(
        &self,
        left: &[u8],
        right: &[u8],
        matches: &[RelationshipMatch],
    ) -> Result<(Vec<u8>, Vec<u8>), DataError> {
        let (left_table, right_table) = (self.table_of(left)?, self.table_of(right)?);
        if matches.is_empty() {
            return Err(refuse("A relationship needs at least one pair of fields."));
        }
        for m in matches {
            if m.operator > 6 {
                return Err(refuse("That is not a relationship operator."));
            }
            for (table, field) in [(&left_table, m.left_field), (&right_table, m.right_field)] {
                self.field_member(table, &key_of(field))?;
            }
        }
        Ok((left_table, right_table))
    }

    /// The relationship definition `[3, fb, 5, R, 02]` (file-metadata.md
    /// 4.1): the two occurrences, the two sort orders (kept from `old`, else
    /// empty) and the option word.
    fn definition(
        left: &[u8],
        right: &[u8],
        options: u32,
        old: Option<&[u8]>,
        orders: [Option<&om_scripts::specs::SortSpec>; 2],
    ) -> Result<Vec<u8>, DataError> {
        let mut out = nested_key_string(left);
        out.extend(nested_key_string(right));
        // The two sort orders between the occurrences and the option word:
        // first the right occurrence's records' (`mLeftToRightOrder`), then
        // the left's (`mRightToLeftOrder`), file-metadata.md 4.1. An order
        // not given is kept byte for byte; one that does not read is
        // refused rather than replaced.
        let unreadable = || unsupported("OpenMaker cannot read this relationship's sort orders.");
        let stored = match old {
            Some(old) => {
                let mut rest = old;
                read_nested_key(&mut rest)
                    .and_then(|_| read_nested_key(&mut rest))
                    .and_then(|_| rest.get(..rest.len().checked_sub(4)?))
                    .filter(|sorts| sorts.len() >= 8)
                    .ok_or_else(unreadable)?
                    .to_vec()
            }
            None => vec![0; 8],
        };
        let [left_order, right_order] = orders;
        let sorts = if left_order.is_none() && right_order.is_none() {
            stored
        } else {
            // One side's order is replaced: the other is split off and kept.
            let (_, first) = om_scripts::specs::SortSpec::read(&stored).ok_or_else(unreadable)?;
            let (_, second) =
                om_scripts::specs::SortSpec::read(&stored[first..]).ok_or_else(unreadable)?;
            if first + second != stored.len() {
                return Err(unreadable());
            }
            let mut sorts = right_order.map_or_else(|| stored[..first].to_vec(), |o| o.encode());
            sorts.extend(left_order.map_or_else(|| stored[first..].to_vec(), |o| o.encode()));
            sorts
        };
        // The side checkboxes own only these bits; every other stored bit
        // must survive an edit, even when OpenMaker does not decode it.
        let options = match old {
            Some(old) => {
                let stored = old.last_chunk::<4>().ok_or_else(|| {
                    unsupported("OpenMaker cannot read this relationship's options.")
                })?;
                (u32::from_be_bytes(*stored) & !0xf3) | (options & 0xf3)
            }
            None => options,
        };
        out.extend(sorts);
        out.extend(options.to_be_bytes());
        Ok(out)
    }

    /// Refuse a sort order for `occurrence`'s records that sorts by
    /// anything but that occurrence's own fields (OpenMaker's limit; the
    /// order's other forms are kept when stored).
    fn check_order(
        &self,
        occurrence: &[u8],
        order: Option<&om_scripts::specs::SortSpec>,
    ) -> Result<(), DataError> {
        let Some(order) = order else {
            return Ok(());
        };
        let table = self.table_of(occurrence)?;
        let fields = self.fields(&table)?;
        let id = id_of(occurrence);
        for entry in &order.fields {
            if entry.field.occurrence != id
                || entry.field.repetition != 1
                || fields
                    .at(&[&[MEMBERS], &key_of(entry.field.field)])
                    .is_none()
            {
                return Err(unsupported(
                    "OpenMaker sorts a relationship's records only by fields of that \
                     occurrence.",
                ));
            }
            if entry.summary.is_some() {
                return Err(unsupported(
                    "OpenMaker cannot yet reorder a relationship's records by a summary field.",
                ));
            }
        }
        Ok(())
    }

    /// Relationship `key`'s predicates node `03`: one leaf per pair, `n` =
    /// 1, 2, ... (file-metadata.md 4.2).
    fn predicates(matches: &[RelationshipMatch]) -> OwnedNode {
        let leaves = matches
            .iter()
            .enumerate()
            .map(|(n, m)| {
                let mut value = vec![m.operator];
                value.extend(nested_key_string(&key_of(m.left_field)));
                value.extend(nested_key_string(&key_of(m.right_field)));
                OwnedNode::leaf(serial::encode(n as u64 + 1), value)
            })
            .collect();
        OwnedNode::list(vec![PREDICATES], leaves)
    }

    /// `like`: a stored definition whose sort orders and other option bits
    /// the new one keeps (a duplicated relationship's).
    fn create_relationship(
        &mut self,
        (left, right): (&[u8], &[u8]),
        matches: &[RelationshipMatch],
        options: u32,
        (like, orders): (Option<&[u8]>, [Option<&om_scripts::specs::SortSpec>; 2]),
    ) -> Result<(), DataError> {
        self.check_matches(left, right, matches)?;
        self.check_order(left, orders[0])?;
        self.check_order(right, orders[1])?;
        if left == right {
            return Err(refuse(
                "A relationship joins two different table occurrences. Add another occurrence \
                 of the table to relate it to itself.",
            ));
        }
        // Every relationship must read, or a path through one OpenMaker
        // cannot see could be duplicated.
        readable_graph(self.current.relationships.as_ref())?;
        let graph = components(&edges(self.current.relationships.as_ref()));
        if graph.get(left).is_some_and(|c| graph.get(right) == Some(c)) {
            return Err(refuse(
                "There cannot be more than one relational path between any two tables in the \
                 graph. Add another occurrence of one of the tables instead.",
            ));
        }
        let catalog = self.catalog_mut(&CatalogPath::Relationships);
        let key = next_key(catalog)?;
        let mut member = OwnedNode::list(key.clone(), Vec::new());
        member.set_child(OwnedNode::leaf(
            vec![DEFINITION],
            Self::definition(left, right, options, like, orders)?,
        ));
        member.set_child(Self::predicates(matches));
        self.catalog(CatalogPath::Relationships)
            .add(&key, member, None)?;
        self.created.insert(MemberRef {
            catalog: CatalogPath::Relationships,
            key: key.clone(),
        });
        self.link(
            &Edge {
                key,
                left: left.to_vec(),
                right: right.to_vec(),
            },
            matches,
        )
    }

    /// Join two trees of the graph with `edge`: the occurrences' lists of
    /// relationships, the routes between every pair it connects, and the
    /// key fields' markers and reach, as FileMaker Pro writes them for a
    /// new relationship (schema-path.md 5.8, E20, E21).
    fn link(&mut self, edge: &Edge, matches: &[RelationshipMatch]) -> Result<(), DataError> {
        let before = edges(self.current.relationships.as_ref())
            .into_iter()
            .filter(|e| e.key != edge.key)
            .collect::<Vec<_>>();
        let tree = |start: &[u8]| -> Vec<Vec<u8>> {
            let mut seen = vec![start.to_vec()];
            let mut queue = VecDeque::from([start.to_vec()]);
            while let Some(node) = queue.pop_front() {
                for e in &before {
                    let next = if e.left == node {
                        &e.right
                    } else if e.right == node {
                        &e.left
                    } else {
                        continue;
                    };
                    if !seen.contains(next) {
                        seen.push(next.clone());
                        queue.push_back(next.clone());
                    }
                }
            }
            seen
        };
        let (left_tree, right_tree) = (tree(&edge.left), tree(&edge.right));
        let (left_table, right_table) = (self.table_of(&edge.left)?, self.table_of(&edge.right)?);
        let id = key_string(&edge.key);
        for (side, other) in [(&edge.left, &edge.right), (&edge.right, &edge.left)] {
            self.catalog(CatalogPath::Occurrences)
                .node
                .ensure(&[&[MEMBERS], side, &[RELATED]])
                .set_child(OwnedNode::leaf(other.clone(), id.clone()));
            self.touch(CatalogPath::Occurrences, side);
        }
        let mut occurrences = self.catalog(CatalogPath::Occurrences);
        let index = occurrences.index();
        // Routes: from each side's tree to the other's, the first
        // relationship on the way.
        let routes = index.ensure(&[&[ROUTES]]);
        for (near, here, far) in [
            (&left_tree, &edge.left, &right_tree),
            (&right_tree, &edge.right, &left_tree),
        ] {
            for a in near {
                let first = if a == here {
                    id.clone()
                } else {
                    routes
                        .at(&[a, here])
                        .and_then(|n| n.value.clone())
                        .ok_or_else(|| unsupported("The graph's routes are incomplete."))?
                };
                for b in far {
                    routes
                        .ensure(&[a])
                        .set_child(OwnedNode::leaf(b.clone(), first.clone()));
                }
            }
        }
        let dependencies = index.ensure(&[&[DEPENDENCIES]]);
        // Reach that ran up to either side now runs on into the other tree.
        // Both directions are read from the reach as it was, so what one
        // direction adds is never extended again by the other.
        let mut extend = Vec::new();
        for (here, far) in [(&edge.left, &right_tree), (&edge.right, &left_tree)] {
            for entry in &dependencies.children {
                let Some(reach) = entry.child(&[REACH]) else {
                    continue;
                };
                for key_field in &reach.children {
                    for from in &key_field.children {
                        if from.segment != *here && from.child(here).is_some() {
                            extend.push((
                                entry.segment.clone(),
                                key_field.segment.clone(),
                                from.segment.clone(),
                                far,
                            ));
                        }
                    }
                }
            }
        }
        for (table, field, from, far) in extend {
            let reach = dependencies.ensure(&[&table, &[REACH], &field, &from]);
            for x in far.iter().filter(|x| **x != from) {
                reach.set_child(OwnedNode::list(x.clone(), Vec::new()));
            }
        }
        // The new key fields: their reach through this relationship, and
        // their markers.
        for (table, at, far, fields) in [
            (
                &left_table,
                &edge.left,
                &right_tree,
                matches.iter().map(|m| m.left_field).collect::<Vec<_>>(),
            ),
            (
                &right_table,
                &edge.right,
                &left_tree,
                matches.iter().map(|m| m.right_field).collect::<Vec<_>>(),
            ),
        ] {
            for field in fields {
                let field = key_of(field);
                let reach = dependencies.ensure(&[table, &[REACH], &field, at]);
                for x in far {
                    reach.set_child(OwnedNode::list(x.clone(), Vec::new()));
                }
                let entry = dependencies.ensure(&[table, &field]);
                if entry.child(&[KEY_MARKER]).is_none() {
                    let mut marker = KEY_MARKER_PREFIX.to_vec();
                    marker.extend(nested_key_string(&field));
                    entry.set_child(OwnedNode::leaf(vec![KEY_MARKER], marker));
                }
            }
        }
        Ok(())
    }

    fn change_relationship(
        &mut self,
        key: &[u8],
        matches: &[RelationshipMatch],
        options: u32,
        orders: [Option<&om_scripts::specs::SortSpec>; 2],
    ) -> Result<(), DataError> {
        let member = self
            .current
            .relationships
            .as_ref()
            .and_then(|c| c.at(&[&[MEMBERS], key]))
            .cloned()
            .ok_or_else(|| refuse("The relationship no longer exists."))?;
        readable_graph(self.current.relationships.as_ref())?;
        let edge = edge_of(&member)
            .ok_or_else(|| unsupported("OpenMaker cannot read this relationship's definition."))?;
        self.check_matches(&edge.left, &edge.right, matches)?;
        self.check_order(&edge.left, orders[0])?;
        self.check_order(&edge.right, orders[1])?;
        // A long definition (sort orders) may be stored in chunks.
        let old = member
            .child(&[DEFINITION])
            .and_then(|d| d.assembled())
            .map(|d| d.into_owned())
            .ok_or_else(|| unsupported("OpenMaker cannot read this relationship's definition."))?;
        // An order Edit Relationship sends back as the definition stores it,
        // byte for byte, is no change: a lookup or auto-entered formula
        // following the relationship is not affected (Assets' relationship
        // 1 sorts both sides). Bytes, not the decoded order: decoding does
        // not tell every stored form apart (a value list key `00` and
        // `02 01 00` both read as none), so an order that only reads the
        // same is a change.
        let stored = stored_orders(&old);
        let orders = [0, 1]
            .map(|side| orders[side].filter(|o| stored.is_none_or(|s| o.encode() != s[side])));
        let definition = Self::definition(&edge.left, &edge.right, options, Some(&old), orders)?;
        let same_pairs = member.child(&[PREDICATES]) == Some(&Self::predicates(matches));
        // A lookup or related auto-entered formula following it keeps its
        // match fields and its records' order: OpenMaker does not rewrite
        // their triggers for others, nor check the lookup again (5.13).
        let sorts = |o: u32| {
            o & (crate::meta::relation_option::SORT_LEFT_TO_RIGHT
                | crate::meta::relation_option::SORT_RIGHT_TO_LEFT)
        };
        let old_options = edge_definition_options(&old);
        if (!same_pairs
            || orders.iter().any(Option::is_some)
            || old_options.is_none_or(|o| sorts(o) != sorts(options)))
            && !self.routed_readers(key).is_empty()
        {
            return Err(unsupported(
                "OpenMaker cannot yet change the match fields or sort order of a relationship \
                 a lookup or an auto-entered formula follows.",
            ));
        }
        let mut updated = member.clone();
        updated.set_child(om_scripts::bytes_node(&[DEFINITION], &definition));
        updated.set_child(Self::predicates(matches));
        if same_pairs {
            self.catalog(CatalogPath::Relationships)
                .node
                .put(&[&[MEMBERS], key], updated);
            self.touch(CatalogPath::Relationships, key);
            return Ok(());
        }
        // Other match fields: unlink the relationship and link it again,
        // keeping its id and every other key.
        let created = self.created.contains(&MemberRef {
            catalog: CatalogPath::Relationships,
            key: key.to_vec(),
        });
        self.remove_edge(&edge)?;
        self.prune_graph();
        self.catalog(CatalogPath::Relationships)
            .add(key, updated, None)?;
        let reference = MemberRef {
            catalog: CatalogPath::Relationships,
            key: key.to_vec(),
        };
        if created {
            self.created.insert(reference);
        } else {
            self.modified.insert(reference);
        }
        self.link(&edge, matches)
    }

    fn delete_relationship(&mut self, key: &[u8]) -> Result<(), DataError> {
        readable_graph(self.current.relationships.as_ref())?;
        self.read_through(&self.routed_readers(key))?;
        let edge = self
            .current
            .relationships
            .as_ref()
            .and_then(|c| c.at(&[&[MEMBERS], key]))
            .and_then(edge_of)
            .ok_or_else(|| refuse("The relationship no longer exists."))?;
        self.remove_edge(&edge)?;
        self.prune_graph();
        Ok(())
    }

    /// Where an occurrence's graph box starts in its definition: after
    /// the data source and the base table (schema-path.md 5.3), with 28
    /// bytes left (box, two words, colour).
    fn box_at(definition: &[u8]) -> Option<usize> {
        let mut rest = definition;
        read_nested_key(&mut rest)?;
        read_nested_key(&mut rest)?;
        (rest.len() == 28).then(|| definition.len() - 28)
    }

    /// Rewrite part of an occurrence's definition at `offset` past its box
    /// start.
    fn edit_box(
        &mut self,
        occurrence: &[u8],
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), DataError> {
        let mut definition = self
            .occurrence(occurrence)?
            .child(&[DEFINITION])
            .and_then(|d| d.value.clone())
            .ok_or_else(|| unsupported("The occurrence has no definition OpenMaker can read."))?;
        let at = Self::box_at(&definition)
            .ok_or_else(|| unsupported("OpenMaker cannot read this occurrence's graph box."))?;
        definition[at + offset..at + offset + bytes.len()].copy_from_slice(bytes);
        self.catalog(CatalogPath::Occurrences)
            .node
            .ensure(&[&[MEMBERS], occurrence])
            .set_child(OwnedNode::leaf(vec![DEFINITION], definition));
        self.touch(CatalogPath::Occurrences, occurrence);
        Ok(())
    }

    fn arrange_occurrence(&mut self, occurrence: &[u8], frame: [i32; 4]) -> Result<(), DataError> {
        if frame[2] <= 0 || frame[3] <= 0 {
            return Err(refuse(
                "A table occurrence's box needs a width and a height.",
            ));
        }
        let bytes: Vec<u8> = frame
            .iter()
            .flat_map(|v| (*v as f32).to_be_bytes())
            .collect();
        self.edit_box(occurrence, 0, &bytes)
    }

    fn color_occurrence(&mut self, occurrence: &[u8], color: [u8; 4]) -> Result<(), DataError> {
        self.edit_box(occurrence, 24, &color)
    }

    /// The save: stamps, UUIDs and deferred work added to the changed
    /// catalogs, which are listed in FileMaker Pro's order. `uuid` gives a
    /// fresh random UUID for each new member and catalog.
    pub fn finish(&self, stamp: &Stamp, uuid: &mut dyn FnMut() -> [u8; 16]) -> CommitPlan {
        let mut current = self.current.clone();
        // A key string: the time's key is shorter than 8 bytes before
        // 00:16:40 (schema-path.md 3.1).
        let when = key_string(&stamp.time);
        let stamped = |node: &mut OwnedNode, count: bool| {
            if count {
                let next = node
                    .child(MOD_COUNT)
                    .and_then(|n| n.value.as_deref())
                    .and_then(key_string_key)
                    .and_then(serial::decode)
                    .unwrap_or(0)
                    .saturating_add(1);
                node.set_child(OwnedNode::leaf(
                    MOD_COUNT.to_vec(),
                    key_string(&serial::encode(next)),
                ));
            }
            node.set_child(om_scripts::text_node(MOD_USER, &stamp.user));
            node.set_child(om_scripts::text_node(MOD_ACCOUNT, &stamp.account));
            node.set_child(OwnedNode::leaf(MOD_TIME.to_vec(), when.clone()));
        };
        fn node_of<'a>(
            current: &'a mut SchemaCatalogs,
            which: &CatalogPath,
        ) -> Option<&'a mut OwnedNode> {
            match which {
                CatalogPath::Tables => current.tables.as_mut(),
                CatalogPath::Occurrences => current.occurrences.as_mut(),
                CatalogPath::Relationships => current.relationships.as_mut(),
                CatalogPath::Fields(t) => current.fields.get_mut(t),
            }
        }
        for member in &self.created {
            if let Some(node) = node_of(&mut current, &member.catalog)
                .and_then(|c| c.at_mut(&[&[MEMBERS], &member.key]))
            {
                node.set_child(OwnedNode::leaf(vec![UNIQUE_ID], uuid().to_vec()));
                stamped(node, false);
            }
        }
        for member in &self.modified {
            if let Some(node) = node_of(&mut current, &member.catalog)
                .and_then(|c| c.at_mut(&[&[MEMBERS], &member.key]))
            {
                stamped(node, true);
            }
        }
        // Data versions: the table's (a u32) and each field's (a serial
        // key string), raised for changes that affect stored values.
        for table in &self.data_changed {
            if let Some(member) = current
                .tables
                .as_mut()
                .and_then(|c| c.at_mut(&[&[MEMBERS], table]))
            {
                let version = member
                    .child(&[RELATED])
                    .and_then(|n| n.value.as_deref())
                    .and_then(|v| <[u8; 4]>::try_from(v).ok())
                    .map_or(0, u32::from_be_bytes);
                member.set_child(OwnedNode::leaf(
                    vec![RELATED],
                    version.wrapping_add(1).to_be_bytes().to_vec(),
                ));
            }
        }
        let mut changed = self.changed.clone();
        if !self.data_changed.is_empty() {
            changed.insert(CatalogPath::Tables);
        }
        for (table, field) in &self.field_data_changed {
            if let Some(member) = current
                .fields
                .get_mut(table)
                .and_then(|c| c.at_mut(&[&[MEMBERS], field]))
            {
                let version = member
                    .child(&[RELATED])
                    .and_then(|n| n.value.as_deref())
                    .and_then(key_string_key)
                    .and_then(serial::decode)
                    .unwrap_or(0);
                member.set_child(OwnedNode::leaf(
                    vec![RELATED],
                    key_string(&serial::encode(version.saturating_add(1))),
                ));
            }
        }
        for ((table, field), work) in &self.work {
            let work = self.host_work(table, field, *work);
            if let Some(catalog) = current.fields.get_mut(table) {
                catalog.put(
                    &[&[INDEX], &[DEFERRED], field],
                    OwnedNode::leaf(field.clone(), work.to_be_bytes().to_vec()),
                );
                changed.insert(CatalogPath::Fields(table.clone()));
            }
        }
        // Each catalog that goes up gets its index stamped; a new field
        // catalog gets its UUID instead of a count (schema-path.md 8.1).
        for which in &changed {
            let new = matches!(which, CatalogPath::Fields(t) if self.new_catalogs.contains(t));
            if let Some(catalog) = node_of(&mut current, which) {
                let index = catalog.ensure(&[&[INDEX]]);
                // Every catalog index FileMaker made has one; a new one
                // gets its own.
                if new || index.child(&[UNIQUE_ID]).is_none() {
                    index.set_child(OwnedNode::leaf(vec![UNIQUE_ID], uuid().to_vec()));
                }
                stamped(index, !new);
            }
        }
        let mut catalogs = Vec::new();
        for which in [
            CatalogPath::Tables,
            CatalogPath::Relationships,
            CatalogPath::Occurrences,
        ] {
            if changed.contains(&which) {
                if let Some(node) = node_of(&mut current, &which) {
                    catalogs.push((which, node.clone()));
                }
            }
        }
        let mut field_tables = Vec::new();
        for which in &changed {
            if let CatalogPath::Fields(table) = which {
                if let Some(node) = current.fields.get(table) {
                    catalogs.push((which.clone(), node.clone()));
                    field_tables.push(table.clone());
                }
            }
        }
        CommitPlan {
            deleted_tables: self.deleted_tables.iter().cloned().collect(),
            catalogs,
            field_tables,
        }
    }
}

/// The host's work on a calculation field's values when Specify
/// Calculation changes it, by schema-path.md 7.2's rules (from the binary;
/// not compared with FileMaker Pro's saves). `same_formula` is whether the
/// context and both streams are unchanged.
fn calculation_work(
    old: &CalculationField,
    new: &CalculationField,
    old_options: u32,
    new_options: u32,
    same_formula: bool,
) -> u32 {
    const ALWAYS_EVALUATE: u32 = 0x20;
    let storage_changed =
        (old.storage.global, old.storage.unstored) != (new.storage.global, new.storage.unstored);
    let fewer = new.repetitions < old.repetitions;
    let type_changed = old.result != new.result;
    let formula_changed = !same_formula || (old_options ^ new_options) & ALWAYS_EVALUATE != 0;
    let mut work = WORK_MEMBER;
    if !old.storage.unstored {
        if storage_changed {
            work |= WORK_DELETE;
        }
        if fewer && work & WORK_DELETE == 0 {
            work |= WORK_REPETITIONS;
        }
        if type_changed || storage_changed || old.repetitions != new.repetitions {
            work |= WORK_CONVERT;
        }
    }
    if old.result == FieldKind::Container
        && (type_changed || work & (WORK_DELETE | WORK_REPETITIONS) != 0)
    {
        work |= WORK_CONTAINER;
    }
    if formula_changed {
        work |= WORK_CONVERT;
    }
    if type_changed || !same_formula {
        work |= WORK_DEFINITION;
    }
    if fewer {
        work |= WORK_FEWER_REPETITIONS;
    }
    if old.storage.global && work & (WORK_DELETE | WORK_REPETITIONS | WORK_CONTAINER) != 0 {
        work |= WORK_GLOBAL;
    }
    let indexes = crate::field_options::VALUE_INDEX | crate::field_options::WORD_INDEX;
    let (old_indexes, new_indexes) = (old_options & indexes, new_options & indexes);
    let data = WORK_DELETE | WORK_REPETITIONS | WORK_CONTAINER | WORK_CONVERT;
    if new_indexes & !old_indexes != 0 || (new_indexes != 0 && work & data != 0) {
        work |= WORK_INDEX;
    }
    if old_indexes & !new_indexes != 0 {
        work |= WORK_DROP_INDEX;
    }
    work
}

/// Option `0x80000`, which the engine sets on a calculation whose formula
/// reads no field (5.10).
/// Whether 26-byte definition `d` holds one stored value per record: a
/// normal field or a stored calculation, not global, one repetition.
fn stored_value_of(d: &[u8]) -> bool {
    let options = u32::from_be_bytes(d[OPTIONS].try_into().expect("4 bytes"));
    (d[0] == KIND_NORMAL || (d[0] == calc_field::KIND_CALCULATION && options & 0x2_0000 == 0))
        && options & GLOBAL == 0
        && u16::from_be_bytes([d[24], d[25]]) == 1
}

fn set_pure(definition: &mut [u8], pure: bool) {
    const PURE: u32 = 0x8_0000;
    let mut options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
    options &= !PURE;
    if pure {
        options |= PURE;
    }
    definition[OPTIONS].copy_from_slice(&options.to_be_bytes());
}

/// A field's own events as its `fff0` triggers (5.9): creation `ff00` and
/// modification `ff02`, each type 1.
fn event_triggers(events: u8) -> Triggers {
    [
        (1, calc_triggers::CREATION),
        (2, calc_triggers::MODIFICATION),
    ]
    .into_iter()
    .filter(|(bit, _)| events & bit != 0)
    .map(|(_, event)| (event.to_vec(), 1))
    .collect()
}

/// A field member in the draft, read for [`FieldOptions::read_members`]:
/// formulas are shown with the draft's names, as the provider shows them.
struct DraftMembers<'a> {
    member: &'a OwnedNode,
    names: &'a om_formula::NameTable,
    decimal_comma: bool,
}

impl OptionMembers for DraftMembers<'_> {
    fn text(&self, key: u8) -> Option<String> {
        self.member.child(&[key]).and_then(text_of)
    }

    fn bytes(&self, key: u8) -> Option<Vec<u8>> {
        Some(self.member.child(&[key])?.assembled()?.into_owned())
    }

    fn formula(&self, key: u8) -> Option<OptionFormula> {
        let node = self.member.child(&[key])?;
        let part = |child: u8| Some(node.child(&[child])?.assembled()?.into_owned());
        let context = om_formula::Key(part(FORMULA_CONTEXT)?).id()?;
        let text = part(FORMULA_TEXT)
            .and_then(|f| calc_field::formula_text(&f, self.names, self.decimal_comma));
        Some(OptionFormula { text, context })
    }
}

/// What Options for Field may change (error 3 otherwise); `old_options`
/// is the definition's option word and `data_type` its type code.
/// `repetitions_free`: the field's repetitions and index language may
/// change as a new field's do, as it holds no value
/// ([`SchemaDraft::holds_no_value`]) or has a host that deletes the
/// repetitions above a reduced maximum and rebuilds its index in a new
/// language ([`SchemaDraft::with_host_conversion`]).
fn check_options(
    old: &FieldOptions,
    new: &FieldOptions,
    old_options: u32,
    data_type: u8,
    created: bool,
    repetitions_free: bool,
) -> Result<(), DataError> {
    // Decoding picks one source, while encoding an editable value clears
    // the whole source mask. Even an unrelated option edit would otherwise
    // discard another stored source that the model cannot represent.
    if (old_options & AUTO_SOURCES).count_ones() > 1 {
        return Err(unsupported(
            "The field enters more than one kind of value, which OpenMaker does not change.",
        ));
    }
    let value = &new.auto_enter.value;
    if *value != old.auto_enter.value && (!value.editable() || !old.auto_enter.value.editable()) {
        return Err(unsupported(
            "The field's lookup cannot be read, so OpenMaker does not change it.",
        ));
    }
    let container = data_type == type_code(FieldKind::Container);
    if let AutoEnterValue::Calculated(calculation) = value {
        // OpenMaker's record rules compute an auto-entered calculation of
        // one value of a type they type (`rules::plan`).
        if container {
            return Err(unsupported(
                "OpenMaker cannot yet compute an auto-entered calculation for a container field.",
            ));
        }
        if new.storage.repetitions != 1 {
            return Err(unsupported(
                "OpenMaker cannot yet compute an auto-entered calculation for a repeating field.",
            ));
        }
        if calculation.formula.text.is_none() && *value != old.auto_enter.value {
            return Err(unsupported("The calculation has no text to edit."));
        }
    }
    if new.auto_enter.prohibit_modification
        && !old.auto_enter.prohibit_modification
        && *value == AutoEnterValue::None
    {
        return Err(refuse(
            "Prohibit modification applies to a value the field enters by itself.",
        ));
    }
    let (a, b) = (&old.validation, &new.validation);
    // Furigana as FileMaker Pro 26.0.3 was seen to write it (#1301): only
    // the translations it wrote, on a text field. Turning it off keeps the
    // field and the translation.
    let (was, furigana) = (&old.furigana, &new.furigana);
    if furigana.on && was != furigana {
        if data_type != type_code(FieldKind::Text) {
            return Err(unsupported(
                "OpenMaker sets furigana on text fields only: what FileMaker Pro does for \
                 another type is not established (#1301).",
            ));
        }
        if furigana.field == 0 {
            return Err(refuse("Choose the field that receives the furigana."));
        }
        // A translation newly chosen, or none: only those Pro was seen to
        // write. A field's own, read from the file, stays.
        let chosen = furigana.translation != was.translation || furigana.translation == 0;
        if chosen && !crate::field_options::Furigana::CONFIRMED.contains(&furigana.translation) {
            return Err(unsupported(
                "OpenMaker writes only the furigana translations FileMaker Pro was seen to \
                 write: Full-Width Katakana and Half-Width Roman (#1334).",
            ));
        }
    }
    if let Some(Message::Calculation(formula)) = &b.message {
        if formula.text.is_none() && a.message != b.message {
            return Err(unsupported("The message calculation has no text to edit."));
        }
    }
    if let Some(Message::Text(text)) = &b.message {
        if text.trim().is_empty() {
            return Err(refuse("Enter the custom message."));
        }
        if text.chars().count() > 30_000 {
            return Err(refuse("A custom message has at most 30000 characters."));
        }
    }
    if b.value_list == Some(0) && a.value_list != b.value_list {
        return Err(refuse("Choose a value list."));
    }
    if let Some((from, to)) = &b.range {
        if a.range != b.range {
            if from.trim().is_empty() || to.trim().is_empty() {
                return Err(refuse("Enter both ends of the range."));
            }
            if from.chars().count() > 1000 || to.chars().count() > 1000 {
                return Err(refuse("A range's ends have at most 1000 characters."));
            }
        }
    }
    if let Some(calculation) = &b.calculation {
        if calculation.formula.text.is_none() && a.calculation != b.calculation {
            return Err(unsupported(
                "The validation calculation has no text to edit.",
            ));
        }
    }
    // The index language, definition bytes 4-7 (schema-path.md 5.5): only
    // one whose keys a host writes as FileMaker does
    // (`rules::written_text_keys`: Unicode raw, English, English
    // international, System and Unicode standard; #159), and on a field
    // that may hold values only where the host rebuilds the index in it
    // (`0x10`, 7.2): OpenMaker's own for a file on this computer. A hosted
    // file's may be an OpenMaker Server that keyed every index in English.
    if old.storage.language != new.storage.language {
        let language = new.storage.language;
        if crate::rules::written_text_keys(language, 0).is_none() {
            return Err(unsupported(format!(
                "OpenMaker does not build index keys in index language {language} yet, so a \
                 field does not take it (#1279, #1280)."
            )));
        }
        if !created && !repetitions_free {
            return Err(unsupported(
                "OpenMaker cannot yet change the index language of a field that may hold \
                 values in a hosted file: it cannot make sure the host rebuilds the index in \
                 the new language (#1267).",
            ));
        }
    }
    if old.storage.repetitions != new.storage.repetitions && !created && !repetitions_free {
        return Err(unsupported(
            "OpenMaker cannot yet change the repetitions of a field that may hold values in a \
             hosted file: it cannot make sure the host deletes those above a smaller number \
             (#1267).",
        ));
    }
    if !(1..=32_000).contains(&new.storage.repetitions) {
        return Err(refuse("A field has from 1 to 32000 repetitions."));
    }
    let text = data_type == type_code(FieldKind::Text);
    let rules = &new.validation;
    if new.storage.global
        && (*value != AutoEnterValue::None
            || new.storage.indexing != Indexing::None
            || rules.not_empty
            || rules.unique
            || rules.existing
            || rules.value_list.is_some()
            || rules.range.is_some()
            || rules.calculation.is_some())
    {
        return Err(unsupported(
            "OpenMaker does not give a field with global storage auto-enter options, \
             required values or an index.",
        ));
    }
    if container && new.storage.indexing != old.storage.indexing {
        return Err(refuse("Container fields are not indexed."));
    }
    if !text && new.storage.indexing == Indexing::Minimal {
        return Err(refuse("Minimal indexing applies to text fields."));
    }
    // The record rules number and look up only a field of one repetition
    // (`rules::plan`); the table would become read-only to OpenMaker.
    if new.storage.repetitions != 1 {
        match value {
            AutoEnterValue::Serial(_) => {
                return Err(unsupported(
                    "OpenMaker cannot yet enter a serial number in a repeating field.",
                ))
            }
            AutoEnterValue::LookedUp(_) => {
                return Err(unsupported(
                    "OpenMaker cannot yet look up a value into a repeating field.",
                ))
            }
            _ => {}
        }
    }
    if let AutoEnterValue::Serial(serial) = value {
        // The record rules number only text and number fields
        // (`rules::plan`, #960); in a field of another type the table would
        // become read-only to OpenMaker.
        if !matches!(data_type, 1 | 2) {
            return Err(unsupported(
                "OpenMaker cannot yet enter a serial number in a date, time, timestamp or \
                 container field; it numbers text and number fields.",
            ));
        }
        if serial.increment == 0 {
            return Err(refuse("A serial number's increment is at least 1."));
        }
        if serial.next.trim().is_empty() || serial.next.chars().count() > 1000 {
            return Err(refuse(
                "A serial number's next value has from 1 to 1000 characters.",
            ));
        }
    }
    if let AutoEnterValue::Data(data) = value {
        if data.chars().count() > 1000 {
            return Err(refuse("Auto-entered data has at most 1000 characters."));
        }
    }
    if new.validation.maximum_characters == Some(0) {
        return Err(refuse("The maximum number of characters is at least 1."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Expectations are FileMaker Pro 26.0.3's own saves of the same change
    //! in a copy of the Contacts starter solution (schema-path.md 5.8),
    //! on a fixture holding the parts of its catalogs the changes touch.
    use super::*;

    fn hex_string(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A tree from dump lines: `path` for a list, `path = hex` for a value.
    fn build(lines: &[&str]) -> OwnedNode {
        let mut root = OwnedNode::list(Vec::new(), Vec::new());
        for line in lines {
            let (path, value) = match line.split_once(" = ") {
                Some((path, value)) => (path, Some(hex(value))),
                None => (*line, None),
            };
            let segments: Vec<Vec<u8>> = path.split('/').map(hex).collect();
            let refs: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
            match value {
                Some(value) => root.put(&refs, OwnedNode::leaf(Vec::new(), value)),
                None => {
                    root.ensure(&refs);
                }
            }
        }
        root
    }

    fn text(segment: &[u8], text: &str) -> OwnedNode {
        om_scripts::text_node(segment, text)
    }

    /// The Contacts starter solution's schema: four tables, an occurrence
    /// of each, Contacts related to the other three on PrimaryKey =
    /// ForeignKey; of Contacts' fields, Photo (container), Website (plain)
    /// and PrimaryKey (auto-entered, validated).
    fn contacts() -> SchemaCatalogs {
        let mut tables = build(&[
            "10/01/00 = 028005",
            "10/01/03/00000001 = 028002",
            "10/01/03/00000002 = 028003",
            "10/01/03/00000003 = 028004",
            "10/01/03/00000004 = 028005",
            "10/01/04 = 00000004",
            "10/01/fc = 0102",
        ])
        .children
        .remove(0);
        let mut occurrences = build(&[
            "11/01/00 = 03d00005",
            "11/01/03/00000001 = 03d00002",
            "11/01/03/00000002 = 03d00003",
            "11/01/03/00000003 = 03d00004",
            "11/01/03/00000004 = 03d00005",
            "11/01/04 = 00000004",
            "11/01/08/d00002/d00003 = 0101",
            "11/01/08/d00002/d00004 = 0102",
            "11/01/08/d00002/d00005 = 0103",
            "11/01/08/d00003/d00002 = 0101",
            "11/01/08/d00003/d00004 = 0101",
            "11/01/08/d00003/d00005 = 0101",
            "11/01/08/d00004/d00002 = 0102",
            "11/01/08/d00004/d00003 = 0102",
            "11/01/08/d00004/d00005 = 0102",
            "11/01/08/d00005/d00002 = 0103",
            "11/01/08/d00005/d00003 = 0103",
            "11/01/08/d00005/d00004 = 0103",
            "11/01/0e/8002/00/09/d00002/d00003",
            "11/01/0e/8002/00/09/d00002/d00004",
            "11/01/0e/8002/00/09/d00002/d00005",
            "11/01/0e/8002/09/00 = 0321020109",
            "11/01/0e/8002/ff/09 = 0101",
            // An evaluation position equal to Website's id, which is not
            // a use of Website.
            "11/01/0e/8002/ff/0d = 0107",
            // Creation evaluates PrimaryKey first, at position 7 (Website's
            // id, which is not a use of Website).
            "11/01/0e/8002/ff00/07 = 0101020109",
            "11/01/0e/8002/ff03 = 02000a",
            "11/01/0e/8002/fffa/03 = ",
            "11/01/0e/8002/fffc/09 = ",
            "11/01/0e/8003/00/0a/d00003/d00002",
            "11/01/0e/8003/00/0a/d00003/d00004",
            "11/01/0e/8003/00/0a/d00003/d00005",
            "11/01/0e/8003/0a/00 = 032102010a",
            "11/01/0e/8003/ff03 = 02000a",
            "11/01/0e/8004/00/05/d00004/d00002",
            "11/01/0e/8004/00/05/d00004/d00003",
            "11/01/0e/8004/00/05/d00004/d00005",
            "11/01/0e/8004/05/00 = 0321020105",
            "11/01/0e/8004/ff03 = 02000a",
            "11/01/0e/8005/00/05/d00005/d00002",
            "11/01/0e/8005/00/05/d00005/d00003",
            "11/01/0e/8005/00/05/d00005/d00004",
            "11/01/0e/8005/05/00 = 0321020105",
            "11/01/0e/8005/ff/03 = 0101",
            "11/01/0e/8005/ff00/02 = 0101020103",
            "11/01/0e/8005/ff03 = 02000a",
            "11/01/0e/8005/fffc/03 = ",
            "11/01/fc = 0105",
            "11/05/d00002/02 = 0201000302800241a0000041a000004303000043620000000000b600000000787878ff",
            "11/05/d00002/fb/d00003 = 0101",
            "11/05/d00002/fb/d00004 = 0102",
            "11/05/d00002/fb/d00005 = 0103",
            "11/05/d00002/fc = 0104",
            "11/05/d00003/02 = 020100030280034352000041a000004303000042ec00000000004a00000001787878ff",
            "11/05/d00003/fb/d00002 = 0101",
            "11/05/d00004/02 = 0201000302800443520000429a000043030000431a00000000006e00000001787878ff",
            "11/05/d00004/fb/d00002 = 0102",
            "11/05/d00005/02 = 0201000302800543520000430a00004303000042ec00000000004a00000001787878ff",
            "11/05/d00005/fb/d00002 = 0103",
        ])
        .children
        .remove(0);
        let relationships = build(&[
            "fb/01/00 = 0103",
            "fb/01/04 = 00000003",
            "fb/01/fc = 0101",
            "fb/05/01/02 = 0403d000020403d00003000000000000000000000010",
            "fb/05/01/03/01 = 0002010902010a",
            "fb/05/02/02 = 0403d000020403d00004000000000000000000000010",
            "fb/05/02/03/01 = 00020109020105",
            "fb/05/03/02 = 0403d000020403d00005000000000000000000000010",
            "fb/05/03/03/01 = 00020109020105",
        ])
        .children
        .remove(0);
        let names = [
            (0x8002, "Contacts"),
            (0x8003, "Addresses"),
            (0x8004, "Phone Numbers"),
            (0x8005, "Email Addresses"),
        ];
        for (id, name) in names {
            let key = key_of(id);
            let mut member = OwnedNode::list(key.clone(), vec![]);
            member.set_child(text(&[NAME], name));
            member.set_child(OwnedNode::leaf(vec![RELATED], vec![0, 0, 0, 1]));
            tables.put(&[&[MEMBERS], &key], member);
            tables.put(
                &[&[INDEX], &[NAMES], &listed(name).unwrap()],
                OwnedNode::leaf(vec![], key_string(&key)),
            );
            let to = key_of(id - 0x8002 + 0xd00002);
            occurrences
                .at_mut(&[&[MEMBERS], &to])
                .unwrap()
                .set_child(text(&[NAME], name));
            occurrences.put(
                &[&[INDEX], &[NAMES], &listed(name).unwrap()],
                OwnedNode::leaf(vec![], key_string(&to)),
            );
        }
        let contacts = build(&[
            "03/01/00 = 010d",
            "03/01/03/00000001 = 0103",
            "03/01/03/00000002 = 0107",
            "03/01/03/00000003 = 0109",
            "03/01/04 = 00000003",
            "03/01/06/011813b312d3138e1433138e0000 = 0103",
            "03/01/06/0104148d126b1225141012ec1433126b0000 = 0107",
            "03/01/06/010413b313da12ec135f120f13da149c131e126b149c0000 = 0109",
            "03/01/0e/fff0/09/ff00 = 01",
            "03/01/fc = 0102",
            "03/05/03/02 = 0106000000000015000000000000000000000001000000000001",
            "03/05/07/02 = 0101000000000015000000000000000000000001000000000001",
            "03/05/09/02 = 01010000000000158000018800000001c0000000100000000001",
            "03/05/09/05/06 = 9b65",
        ])
        .children
        .remove(0);
        let mut contacts = contacts;
        for (key, name) in [(3u8, "Photo"), (7, "Website"), (9, "PrimaryKey")] {
            contacts
                .at_mut(&[&[MEMBERS], &[key]])
                .unwrap()
                .set_child(text(&[NAME], name));
            contacts.put(
                &[&[INDEX], &[NAMES], &listed(name).unwrap()],
                OwnedNode::leaf(vec![], key_string(&[key])),
            );
        }
        let foreign = |id: u8| {
            let mut catalog = OwnedNode::list(vec![FIELDS], vec![]);
            catalog.put(
                &[&[INDEX], &[LAST_ID]],
                OwnedNode::leaf(vec![], hex("010d")),
            );
            catalog.put(
                &[&[INDEX], &[COUNT]],
                OwnedNode::leaf(vec![], vec![0, 0, 0, 1]),
            );
            catalog.put(
                &[&[INDEX], &[ORDER], &[0, 0, 0, 1]],
                OwnedNode::leaf(vec![], key_string(&[id])),
            );
            let mut member = OwnedNode::list(vec![id], vec![]);
            member.set_child(OwnedNode::leaf(
                vec![DEFINITION],
                hex("0101000000000015000000000000000000000001000000000001"),
            ));
            member.set_child(text(&[NAME], "ForeignKey"));
            catalog.put(&[&[MEMBERS], &[id]], member);
            catalog.put(
                &[&[INDEX], &[NAMES], &listed("ForeignKey").unwrap()],
                OwnedNode::leaf(vec![], key_string(&[id])),
            );
            catalog
        };
        SchemaCatalogs {
            tables: Some(tables),
            occurrences: Some(occurrences),
            relationships: Some(relationships),
            fields: [
                (key_of(0x8002), contacts),
                (key_of(0x8003), foreign(0x0a)),
                (key_of(0x8004), foreign(0x05)),
                (key_of(0x8005), foreign(0x05)),
            ]
            .into(),
        }
    }

    fn value<'a>(node: &'a OwnedNode, path: &str) -> Option<&'a [u8]> {
        let segments: Vec<Vec<u8>> = path.split('/').map(hex).collect();
        let refs: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
        node.at(&refs)?.value.as_deref()
    }

    fn has(node: &OwnedNode, path: &str) -> bool {
        let segments: Vec<Vec<u8>> = path.split('/').map(hex).collect();
        let refs: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
        node.at(&refs).is_some()
    }

    fn stamp() -> Stamp {
        Stamp {
            user: "Tester".into(),
            account: "Admin".into(),
            time: hex("9faacb8005777560"),
        }
    }

    fn uuids() -> impl FnMut() -> [u8; 16] {
        let mut n = 0u8;
        move || {
            n += 1;
            [n; 16]
        }
    }

    #[test]
    fn a_new_table_gets_an_occurrence_an_empty_field_catalog_and_its_dependency_entry() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::CreateTable {
                name: "Widgets".into(),
                comment: String::new(),
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let paths: Vec<_> = plan.catalogs.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(
            paths,
            [
                CatalogPath::Tables,
                CatalogPath::Occurrences,
                CatalogPath::Fields(hex("8006"))
            ]
        );
        let tables = &plan.catalogs[0].1;
        // FileMaker Pro's own keys for "Widgets" (E1).
        assert_eq!(value(tables, "01/00"), Some(&hex("028006")[..]));
        assert_eq!(
            value(tables, "01/01/148d12ec125012b0126b14331410000000"),
            Some(&hex("028006")[..])
        );
        assert_eq!(value(tables, "01/03/00000005"), Some(&hex("028006")[..]));
        assert_eq!(value(tables, "01/04"), Some(&hex("00000005")[..]));
        assert_eq!(value(tables, "01/fc"), Some(&hex("0103")[..]));
        assert_eq!(value(tables, "05/8006/fb"), Some(&hex("00000001")[..]));
        assert!(has(tables, "05/8006/d8") && has(tables, "05/8006/fc03"));
        let occurrences = &plan.catalogs[1].1;
        assert_eq!(value(occurrences, "01/00"), Some(&hex("03d00006")[..]));
        assert_eq!(
            value(occurrences, "01/01/148d12ec125012b0126b14331410000000"),
            Some(&hex("03d00006")[..])
        );
        assert_eq!(
            value(occurrences, "05/d00006/02"),
            Some(
                &hex("02010003028006000000000000000000000000000000000000000000000000777777ff")[..]
            )
        );
        assert!(has(occurrences, "01/0e/8006/ff"));
        assert_eq!(
            value(occurrences, "01/0e/8006/ff03"),
            Some(&hex("02000a")[..])
        );
        let fields = &plan.catalogs[2].1;
        assert!(has(fields, "01/0e/fff0") && has(fields, "01/d8") && has(fields, "05"));
        assert!(!has(fields, "01/fc"), "a new catalog has no count yet");
        assert_eq!(plan.field_tables, [hex("8006")]);
        assert!(plan.deleted_tables.is_empty());
    }

    /// A field of a table the provider found empty holds no value, so its
    /// type and repetitions change as a new field's do (#441), unless it is
    /// global, whose value the table's global record keeps. The tables the
    /// save must find still empty follow the changes as they end, and a
    /// repetitions change asks for the host's work of schema-path.md 7.2.
    #[test]
    fn an_empty_tables_field_holds_no_value_unless_global() {
        let empty = BTreeSet::from([hex("8002")]);
        let website = |data_type| SchemaChange::ChangeField {
            table_id: 0x8002,
            field_id: 7,
            name: "Website".into(),
            data_type,
            comment: String::new(),
        };
        let mut draft = SchemaDraft::new(contacts()).with_empty_tables(empty.clone());
        draft.apply(&website(FieldKind::Date)).unwrap();
        assert_eq!(draft.relies_on_empty_tables(), empty);
        draft.apply(&website(FieldKind::Text)).unwrap();
        assert!(draft.relies_on_empty_tables().is_empty());
        let mut options = options_of(&draft, 7);
        options.storage.repetitions = 3;
        let repeating = SchemaChange::SetFieldOptions {
            table_id: 0x8002,
            field_id: 7,
            options: Box::new(options),
        };
        draft.apply(&repeating).unwrap();
        assert_eq!(draft.relies_on_empty_tables(), empty);

        let mut base = contacts();
        let member = base
            .fields
            .get_mut(&hex("8002"))
            .unwrap()
            .at_mut(&[&[MEMBERS], &[7]])
            .unwrap();
        let mut definition = SchemaDraft::definition_of(member).unwrap();
        definition[9] |= 0x01; // global, 0x1_0000 in the option word
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
        let mut draft = SchemaDraft::new(base).with_empty_tables(empty.clone());
        let mut options = options_of(&draft, 7);
        assert!(options.storage.global);
        options.storage.repetitions = 3;
        let repeating = SchemaChange::SetFieldOptions {
            table_id: 0x8002,
            field_id: 7,
            options: Box::new(options),
        };
        for change in [website(FieldKind::Date), repeating] {
            let refused = draft.apply(&change).unwrap_err();
            assert_eq!(refused.code, Some(UNSUPPORTED), "{refused:?}");
            assert!(refused.message.contains("#1267"), "{refused:?}");
        }
        // Nor once global storage is turned off in the edit: the file still
        // holds the global value until the host's work at the save.
        let mut options = options_of(&draft, 7);
        options.storage.global = false;
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            })
            .unwrap();
        let refused = draft.apply(&website(FieldKind::Date)).unwrap_err();
        assert!(refused.message.contains("#1267"), "{refused:?}");

        // The host's work, by 7.2's rules, and the data mod counts: more
        // repetitions recalculate dependents (`0x40`); fewer also delete
        // the ones above the new maximum (`0x4 | 0x1000`).
        let repetitions = |draft: &SchemaDraft, count: u16| {
            let mut options = options_of(draft, 7);
            options.storage.repetitions = count;
            SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            }
        };
        let finished = |draft: SchemaDraft| {
            let plan = draft.finish(&stamp(), &mut uuids());
            plan.catalogs
                .into_iter()
                .find(|(p, _)| *p == CatalogPath::Fields(hex("8002")))
                .unwrap()
                .1
        };
        let mut draft = SchemaDraft::new(contacts()).with_empty_tables(empty.clone());
        draft.apply(&repetitions(&draft, 3)).unwrap();
        let fields = finished(draft);
        assert_eq!(value(&fields, "01/fd/07"), Some(&hex("00000240")[..]));
        assert_eq!(value(&fields, "05/07/fb"), Some(&hex("0101")[..]));
        let mut base = contacts();
        let member = base
            .fields
            .get_mut(&hex("8002"))
            .unwrap()
            .at_mut(&[&[MEMBERS], &[7]])
            .unwrap();
        let mut definition = SchemaDraft::definition_of(member).unwrap();
        definition[24..26].copy_from_slice(&3u16.to_be_bytes());
        member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
        let mut draft = SchemaDraft::new(base.clone()).with_empty_tables(empty.clone());
        draft.apply(&repetitions(&draft, 1)).unwrap();
        let fields = finished(draft);
        assert_eq!(value(&fields, "01/fd/07"), Some(&hex("00001244")[..]));
        assert_eq!(value(&fields, "05/07/fb"), Some(&hex("0101")[..]));
        // Made global with it, the values are deleted anyway (`0x1`, no
        // `0x4`); global again no more, the repetitions' work is back.
        let mut draft = SchemaDraft::new(base).with_empty_tables(empty.clone());
        let global = |draft: &SchemaDraft, global: bool| {
            let mut options = options_of(draft, 7);
            options.storage.repetitions = 1;
            options.storage.global = global;
            SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            }
        };
        draft.apply(&global(&draft, true)).unwrap();
        assert_eq!(
            draft.work.get(&(hex("8002"), vec![7])),
            Some(&0x1241),
            "{:x?}",
            draft.work
        );
        draft.apply(&global(&draft, false)).unwrap();
        let fields = finished(draft);
        assert_eq!(value(&fields, "01/fd/07"), Some(&hex("00001244")[..]));

        // A host that deletes them (a file on this computer) takes a cut of
        // a field that may hold values: a global of three repetitions has
        // its global value cut (`0x4 | 0x2`, 7.2). A container of three
        // made text loses every repetition (`0x8` without `0x4`), whichever
        // change comes first; cut and still a container, those above the
        // new maximum go with their data (`0x4 | 0x8`).
        let with = |data_type: u8, global: bool| {
            let mut base = contacts();
            let member = base
                .fields
                .get_mut(&hex("8002"))
                .unwrap()
                .at_mut(&[&[MEMBERS], &[7]])
                .unwrap();
            let mut definition = SchemaDraft::definition_of(member).unwrap();
            definition[1] = data_type;
            if global {
                definition[9] |= 0x01;
            }
            definition[24..26].copy_from_slice(&3u16.to_be_bytes());
            member.set_child(OwnedNode::leaf(vec![DEFINITION], definition));
            SchemaDraft::new(base).with_host_conversion(true)
        };
        let work = |fields: &OwnedNode| {
            u32::from_be_bytes(value(fields, "01/fd/07").unwrap().try_into().unwrap())
        };
        let mut draft = with(1, true);
        draft.apply(&repetitions(&draft, 1)).unwrap();
        assert_eq!(work(&finished(draft)) & 0x100f, 0x1006);
        for retyped_first in [true, false] {
            let mut draft = with(6, false);
            if retyped_first {
                draft.apply(&website(FieldKind::Text)).unwrap();
            }
            draft.apply(&repetitions(&draft, 1)).unwrap();
            if !retyped_first {
                draft.apply(&website(FieldKind::Text)).unwrap();
            }
            assert_eq!(work(&finished(draft)) & 0x100f, 0x1008, "{retyped_first}");
        }
        let mut draft = with(6, false);
        draft.apply(&repetitions(&draft, 1)).unwrap();
        assert_eq!(work(&finished(draft)) & 0x100f, 0x100c);
        // Made text and then global, its values are deleted (`0x1`), the
        // container data with them (`0x8`), though the storage change came
        // when the field was already text.
        let mut draft = with(6, false);
        draft.apply(&website(FieldKind::Text)).unwrap();
        draft.apply(&global(&draft, true)).unwrap();
        assert_eq!(work(&finished(draft)) & 0xf, 0x9);

        // A serial number in a repeating field, which the record rules do
        // not enter, is refused (the table would become read-only); one
        // repetition takes it.
        let fixture = options_fixture();
        let options_base = &fixture["base"];
        let catalogs = SchemaCatalogs {
            tables: options_base.at(&[&[3], &[TABLES]]).cloned(),
            occurrences: options_base.at(&[&[3], &[OCCURRENCES]]).cloned(),
            relationships: None,
            fields: [(
                key_of(0x8002),
                options_base
                    .at(&[&[0x80, 0x02], &[FIELDS]])
                    .unwrap()
                    .clone(),
            )]
            .into(),
        };
        let mut draft = SchemaDraft::new(catalogs).with_empty_tables(empty);
        let serial = |draft: &SchemaDraft, count: u16| {
            let mut options = options_of(draft, 7);
            options.storage.repetitions = count;
            options.auto_enter.value = AutoEnterValue::Serial(crate::field_options::Serial {
                increment: 1,
                next: "1".into(),
                ..Default::default()
            });
            SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            }
        };
        let refused = draft.apply(&serial(&draft, 3)).unwrap_err();
        assert!(refused.message.contains("serial"), "{refused:?}");
        draft.apply(&serial(&draft, 1)).unwrap();
    }

    #[test]
    fn fields_are_added_changed_and_deleted_as_filemaker_does() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "Attachment".into(),
                data_type: FieldKind::Container,
                comment: String::new(),
            })
            .unwrap();
        // Website: renamed and commented (E3, without its type change).
        draft
            .apply(&SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 7,
                name: "Web Site".into(),
                data_type: FieldKind::Text,
                comment: "The company site".into(),
            })
            .unwrap();
        // A field that may hold values changes type only among text,
        // number and time, where every stored text is kept (#441): to a
        // date each value would be rewritten. One made in this edit may
        // change it.
        let refused = draft
            .apply(&SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 7,
                name: "Web Site".into(),
                data_type: FieldKind::Date,
                comment: "The company site".into(),
            })
            .unwrap_err();
        assert_eq!(refused.code, Some(UNSUPPORTED));
        draft
            .apply(&SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 0x0e,
                name: "Attachment".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        draft
            .apply(&SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 0x0e,
                name: "Attachment".into(),
                data_type: FieldKind::Container,
                comment: String::new(),
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let fields = &plan
            .catalogs
            .iter()
            .find(|(p, _)| *p == CatalogPath::Fields(hex("8002")))
            .unwrap()
            .1;
        // E2: the new container field, id 0e after the last issued 0d.
        assert_eq!(
            value(fields, "05/0e/02"),
            Some(&hex("0106000000000015000000000000000000000001000000000001")[..])
        );
        assert_eq!(
            value(
                fields,
                "01/06/0118120f14331433120f123d12d3135f126b136d14330000"
            ),
            Some(&hex("010e")[..])
        );
        assert!(!has(
            fields,
            "01/06/0104120f14331433120f123d12d3135f126b136d14330000"
        ));
        assert_eq!(value(fields, "01/03/00000004"), Some(&hex("010e")[..]));
        assert_eq!(value(fields, "01/04"), Some(&hex("00000004")[..]));
        assert!(!has(fields, "01/fd/0e"), "a new field holds no values");
        // E3: the renamed field's keys.
        assert!(!has(fields, "01/01/148d126b1225141012ec1433126b000000"));
        assert_eq!(
            value(fields, "01/01/148d126b1225020a141012ec1433126b000000"),
            Some(&hex("0107")[..])
        );
        assert!(!has(fields, "01/06/0104148d126b1225141012ec1433126b0000"));
        assert_eq!(
            value(fields, "01/06/0104148d126b1225020a141012ec1433126b0000"),
            Some(&hex("0107")[..])
        );
        assert_eq!(value(fields, "05/07/fc"), Some(&hex("0101")[..]));
        assert_eq!(value(fields, "01/fd/07"), Some(&hex("00000200")[..]));
        // No stored value changes, so no data version does.
        assert!(!has(fields, "05/07/fb"));
        assert!(plan.catalogs.iter().all(|(p, _)| *p != CatalogPath::Tables));
        // The container is listed in the table's dependency entry.
        let occurrences = &plan
            .catalogs
            .iter()
            .find(|(p, _)| *p == CatalogPath::Occurrences)
            .unwrap()
            .1;
        assert!(has(occurrences, "01/0e/8002/fffa/0e"));

        // E5: deleting Website renumbers the display order without gaps.
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::DeleteField {
                table_id: 0x8002,
                field_id: 7,
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let fields = &plan.catalogs[1].1;
        assert!(!has(fields, "05/07"));
        assert!(!has(fields, "01/01/148d126b1225141012ec1433126b000000"));
        assert!(!has(fields, "01/06/0104148d126b1225141012ec1433126b0000"));
        assert_eq!(value(fields, "01/03/00000002"), Some(&hex("0109")[..]));
        assert!(!has(fields, "01/03/00000003"));
        assert_eq!(value(fields, "01/04"), Some(&hex("00000002")[..]));
        assert_eq!(value(fields, "01/00"), Some(&hex("010d")[..]));
        assert_eq!(value(fields, "01/fd/07"), Some(&hex("000001e1")[..]));
    }

    /// Global storage turned on and off on a field that may hold values
    /// (schema-path.md 7.2): the host is asked to delete every record's
    /// value (`0x1`), or the global value when the field was global
    /// (`0x2`), with `0x40` for the storage change and `0x200`; the field's
    /// data mod count goes up (`01 01` as in E38) and its key moves in the
    /// name list by type. Storage turned back in the same edit asks for
    /// nothing, and a field deleted after a change is deleted where its
    /// values are. A field a calculation reads keeps its storage.
    #[test]
    fn global_storage_changes_ask_the_host_to_delete_the_values() {
        let fixture = options_fixture();
        let base = &fixture["base"];
        let catalogs = || SchemaCatalogs {
            tables: base.at(&[&[3], &[TABLES]]).cloned(),
            occurrences: base.at(&[&[3], &[OCCURRENCES]]).cloned(),
            relationships: None,
            fields: [(
                key_of(0x8002),
                base.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap().clone(),
            )]
            .into(),
        };
        let set = |draft: &mut SchemaDraft, field: u8, global: bool| {
            let mut options = options_of(draft, field);
            options.storage.global = global;
            options.storage.indexing = Indexing::None;
            draft.apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: u64::from(field),
                options: Box::new(options),
            })
        };
        let mut draft = SchemaDraft::new(catalogs());
        set(&mut draft, 0x07, true).unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let fields = &plan
            .catalogs
            .iter()
            .find(|(p, _)| *p == CatalogPath::Fields(key_of(0x8002)))
            .unwrap()
            .1;
        assert_eq!(value(fields, "01/fd/07"), Some(&hex("00000241")[..]));
        assert_eq!(value(fields, "05/07/fb"), Some(&hex("0101")[..]));
        let definition = value(fields, "05/07/02").unwrap();
        assert_ne!(definition[9] & 0x01, 0, "global storage");
        // The name list by type keys a global text field `01 05` (E38).
        let typed = fields.at(&[&[INDEX], &[TYPED_NAMES]]).unwrap();
        let website = typed
            .children
            .iter()
            .find(|c| c.value.as_deref() == Some(&[1, 7][..]))
            .unwrap();
        assert_eq!(&website.segment[..2], &[0x01, 0x05]);

        // A global field made a stored one again: its global value goes.
        let mut stored = catalogs();
        let website = stored
            .fields
            .get_mut(&key_of(0x8002))
            .unwrap()
            .at_mut(&[&[MEMBERS], &[0x07], &[DEFINITION]])
            .unwrap();
        let mut definition = website.value.clone().unwrap();
        definition[9] |= 0x01;
        website.value = Some(definition);
        let mut draft = SchemaDraft::new(stored.clone());
        set(&mut draft, 0x07, false).unwrap();
        assert_eq!(draft.work[&(key_of(0x8002), vec![0x07])], 0x243);
        // Turned back in the same edit, its value stays; deleted after
        // being made stored, it is the global value that goes.
        set(&mut draft, 0x07, true).unwrap();
        assert_eq!(draft.work[&(key_of(0x8002), vec![0x07])], 0x200);
        set(&mut draft, 0x07, false).unwrap();
        draft
            .apply(&SchemaChange::DeleteField {
                table_id: 0x8002,
                field_id: 0x07,
            })
            .unwrap();
        assert_eq!(draft.work[&(key_of(0x8002), vec![0x07])], 0x1e3);
        // A stored field made global and stored again keeps its values.
        let mut draft = SchemaDraft::new(catalogs());
        set(&mut draft, 0x07, true).unwrap();
        set(&mut draft, 0x07, false).unwrap();
        assert_eq!(draft.work[&(key_of(0x8002), vec![0x07])], 0x200);

        // First Name, which the calculation Initial reads, stays.
        let mut draft = SchemaDraft::new(catalogs());
        let error = set(&mut draft, 0x01, true).unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED), "{}", error.message);
        assert!(!draft.is_changed());
    }

    #[test]
    fn an_accented_field_is_listed_as_filemaker_pro_lists_it() {
        // FileMaker Pro 26.0.3's keys for the Text field `Größe` it added as
        // 0e to a German Contacts copy (schema-path.md 5.6).
        let listed = "12b013da138e14101410126b000001010128013a0101000101010103031e01";
        let typed = "010412b013da138e14101410126b000001010128013a0101";
        let mut draft = SchemaDraft::new(contacts());
        let create = |name: &str| SchemaChange::CreateField {
            table_id: 0x8002,
            name: name.into(),
            data_type: FieldKind::Text,
            comment: String::new(),
        };
        draft.apply(&create("Größe")).unwrap();
        let fields = |draft: &SchemaDraft| {
            let plan = draft.clone().finish(&stamp(), &mut uuids());
            plan.catalogs
                .into_iter()
                .find(|(p, _)| *p == CatalogPath::Fields(hex("8002")))
                .unwrap()
                .1
        };
        let catalog = fields(&draft);
        assert_eq!(
            value(&catalog, &format!("01/01/{listed}")),
            Some(&hex("010e")[..])
        );
        assert_eq!(
            value(&catalog, &format!("01/06/{typed}")),
            Some(&hex("010e")[..])
        );
        // Case does not tell names apart, nor, in OpenMaker, accents.
        for taken in ["GRÖSSE", "Grosse"] {
            let refused = draft.apply(&create(taken)).unwrap_err();
            assert_eq!(refused.code, Some(NAME_EXISTS), "{taken}");
        }
        // The catalog language keys `ı` as `i`, so they are one name: the
        // second would take the first's entries in both lists.
        draft.apply(&create("i")).unwrap();
        let refused = draft.apply(&create("ı")).unwrap_err();
        assert_eq!(refused.code, Some(NAME_EXISTS));
        // Renamed, it leaves no entry under the old keys.
        draft
            .apply(&SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 0x0e,
                name: "Maß".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let catalog = fields(&draft);
        for (list, old) in [(NAMES, listed), (TYPED_NAMES, typed)] {
            let entries: Vec<&OwnedNode> = catalog
                .at(&[&[INDEX], &[list]])
                .unwrap()
                .children
                .iter()
                .filter(|e| e.value.as_deref() == Some(&hex("010e")[..]))
                .collect();
            assert_eq!(entries.len(), 1, "list {list:02x}");
            assert_ne!(entries[0].segment, hex(old), "list {list:02x}");
        }
    }

    #[test]
    fn fields_that_options_or_relationships_use_are_not_deleted() {
        let mut draft = SchemaDraft::new(contacts());
        for (table_id, field_id) in [(0x8002, 9), (0x8003, 0x0a)] {
            let error = draft
                .apply(&SchemaChange::DeleteField { table_id, field_id })
                .unwrap_err();
            assert_eq!(error.code, Some(UNSUPPORTED));
        }
        assert!(!draft.is_changed());
        // A name FileMaker takes for another field's, and a bad one.
        for name in ["website", "a+b"] {
            assert!(draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type: FieldKind::Text,
                    comment: String::new(),
                })
                .is_err());
        }
        assert!(!draft.is_changed());
    }

    #[test]
    fn a_renamed_table_takes_its_occurrence_with_it() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::ChangeTable {
                table_id: 0x8003,
                name: "Locations".into(),
                comment: "Mailing addresses".into(),
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let (tables, occurrences) = (&plan.catalogs[0].1, &plan.catalogs[1].1);
        // E5's keys for "Locations".
        let key = "1330138e123d120f143312ec138e136d1410000000";
        assert_eq!(
            value(tables, &format!("01/01/{key}")),
            Some(&hex("028003")[..])
        );
        assert!(!has(
            tables,
            "01/01/120f1250125013da126b14101410126b1410000000"
        ));
        assert_eq!(
            value(occurrences, &format!("01/01/{key}")),
            Some(&hex("03d00003")[..])
        );
        let member = tables.at(&[&[MEMBERS], &hex("8003")]).unwrap();
        assert_eq!(member_name(member).as_deref(), Some("Locations"));
        assert_eq!(comment_of(member), "Mailing addresses");
        assert_eq!(value(tables, "05/8003/fc"), Some(&hex("0101")[..]));
        assert_eq!(value(occurrences, "05/d00003/fc"), Some(&hex("0101")[..]));
    }

    #[test]
    fn a_deleted_table_keeps_its_occurrences_unless_they_go_too() {
        // E7: the occurrence stays, showing a missing table; the table's
        // dependency entry keeps only what its relationship derives.
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::DeleteTable {
                table_id: 0x8005,
                remove_occurrences: false,
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        assert_eq!(plan.deleted_tables, [hex("8005")]);
        assert!(plan.field_tables.is_empty());
        let tables = &plan.catalogs[0].1;
        assert!(!has(tables, "05/8005"));
        assert!(!has(tables, "01/03/00000004"));
        assert_eq!(value(tables, "01/04"), Some(&hex("00000003")[..]));
        let occurrences = &plan.catalogs[1].1;
        assert!(has(occurrences, "05/d00005"));
        assert!(has(occurrences, "01/0e/8005/00/05/d00005/d00002"));
        assert_eq!(
            value(occurrences, "01/0e/8005/05/00"),
            Some(&hex("0321020105")[..])
        );
        for gone in ["ff", "ff00", "ff03", "fffc"] {
            assert!(!has(occurrences, &format!("01/0e/8005/{gone}")), "{gone}");
        }

        // E8: with its occurrences, the relationship, routes, reach and
        // relationship lists that name them go too.
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::DeleteTable {
                table_id: 0x8005,
                remove_occurrences: true,
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let order: Vec<_> = plan.catalogs.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(
            order,
            [
                CatalogPath::Tables,
                CatalogPath::Relationships,
                CatalogPath::Occurrences
            ]
        );
        let relationships = &plan.catalogs[1].1;
        assert!(!has(relationships, "05/03"));
        assert_eq!(value(relationships, "01/04"), Some(&hex("00000002")[..]));
        assert_eq!(value(relationships, "01/00"), Some(&hex("0103")[..]));
        let occurrences = &plan.catalogs[2].1;
        assert!(!has(occurrences, "05/d00005"));
        assert!(!has(occurrences, "05/d00002/fb/d00005"));
        assert!(has(occurrences, "05/d00002/fb/d00004"));
        assert_eq!(value(occurrences, "05/d00002/fc"), Some(&hex("0105")[..]));
        assert!(!has(occurrences, "01/08/d00005"));
        assert!(!has(occurrences, "01/08/d00002/d00005"));
        assert!(!has(occurrences, "01/08/d00004/d00005"));
        assert_eq!(
            value(occurrences, "01/08/d00004/d00003"),
            Some(&hex("0102")[..])
        );
        assert!(!has(occurrences, "01/0e/8002/00/09/d00002/d00005"));
        assert!(has(occurrences, "01/0e/8002/00/09/d00002/d00004"));
        assert!(!has(occurrences, "01/0e/8003/00/0a/d00003/d00005"));
        assert!(!has(occurrences, "01/0e/8005"));
        assert_eq!(value(occurrences, "01/04"), Some(&hex("00000003")[..]));
        assert!(!has(occurrences, "01/03/00000004"));
    }

    #[test]
    fn changes_made_and_undone_in_one_session_leave_nothing_to_delete() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::CreateTable {
                name: "Scratch".into(),
                comment: String::new(),
            })
            .unwrap();
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8006,
                name: "Note".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        draft
            .apply(&SchemaChange::DeleteTable {
                table_id: 0x8006,
                remove_occurrences: true,
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        assert!(plan.deleted_tables.is_empty());
        assert!(plan.field_tables.is_empty());
        let tables = &plan.catalogs[0].1;
        assert!(!has(tables, "05/8006"));
        // The id stays issued.
        assert_eq!(value(tables, "01/00"), Some(&hex("028006")[..]));
    }
    #[test]
    fn an_unreadable_relationship_or_a_full_counter_refuses_the_change() {
        let mut catalogs = contacts();
        // Relationship 3's predicate no longer reads.
        catalogs.relationships.as_mut().unwrap().put(
            &[&[MEMBERS], &[3], &[PREDICATES], &[1]],
            OwnedNode::leaf(vec![1], vec![0, 9]),
        );
        // ... or has no predicates at all, or an operator above 6.
        let mut without = contacts();
        without
            .relationships
            .as_mut()
            .unwrap()
            .remove(&[&[MEMBERS], &[3], &[PREDICATES]]);
        let mut operator = contacts();
        operator.relationships.as_mut().unwrap().put(
            &[&[MEMBERS], &[3], &[PREDICATES], &[1]],
            OwnedNode::leaf(vec![1], hex("07020109020105")),
        );
        for catalogs in [catalogs, without, operator] {
            let mut draft = SchemaDraft::new(catalogs);
            for change in [
                SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: 7,
                },
                SchemaChange::DeleteOccurrence {
                    occurrence_id: 0xd00004,
                },
            ] {
                let error = draft.apply(&change).unwrap_err();
                assert_eq!(error.code, Some(UNSUPPORTED), "{change:?}");
            }
            assert!(!draft.is_changed());
        }

        let mut catalogs = contacts();
        catalogs.tables.as_mut().unwrap().put(
            &[&[INDEX], &[COUNT]],
            OwnedNode::leaf(vec![COUNT], vec![0xff; 4]),
        );
        let mut draft = SchemaDraft::new(catalogs);
        let error = draft
            .apply(&SchemaChange::CreateTable {
                name: "Widgets".into(),
                comment: String::new(),
            })
            .unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED));
        assert!(!draft.is_changed());
    }

    /// The Contacts table as FileMaker Pro stored it before each Options for
    /// Field run, and what it stored after (schema-path.md 5.8).
    fn options_fixture() -> BTreeMap<String, OwnedNode> {
        let text = include_str!("../tests/fixtures/contacts-field-options.txt");
        let mut sections = BTreeMap::new();
        let mut name = String::new();
        let mut lines: Vec<&str> = Vec::new();
        for line in text.lines().chain(["[end]"]) {
            if line.starts_with('#') {
                continue;
            }
            if let Some(next) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if !name.is_empty() {
                    sections.insert(name.clone(), build(&lines));
                }
                name = next.to_owned();
                lines.clear();
            } else {
                lines.push(line);
            }
        }
        sections
    }

    fn options_of(draft: &SchemaDraft, field: u8) -> FieldOptions {
        let member = draft.current.fields[&key_of(0x8002)]
            .at(&[&[MEMBERS], &[field]])
            .unwrap();
        let text = |key| member.child(&[key]).and_then(text_of);
        FieldOptions::decode(
            &SchemaDraft::definition_of(member).unwrap(),
            text(NEXT_SERIAL).as_deref(),
            text(AUTO_DATA).as_deref(),
        )
        .unwrap()
    }

    #[test]
    fn field_options_are_saved_as_filemaker_pro_saved_them() {
        use crate::field_options::{AutoEnterValue as Value, EntryValue, Serial};
        let fixture = options_fixture();
        let base = &fixture["base"];
        let catalogs = || SchemaCatalogs {
            tables: base.at(&[&[3], &[TABLES]]).cloned(),
            occurrences: base.at(&[&[3], &[OCCURRENCES]]).cloned(),
            relationships: None,
            fields: [(
                key_of(0x8002),
                base.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap().clone(),
            )]
            .into(),
        };
        let set = |draft: &mut SchemaDraft, field: u8, change: &dyn Fn(&mut FieldOptions)| {
            let mut options = options_of(draft, field);
            change(&mut options);
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: u64::from(field),
                    options: Box::new(options),
                })
                .unwrap();
        };
        type Run<'a> = (&'a str, &'a [u8], Box<dyn Fn(&mut SchemaDraft)>);
        let runs: Vec<Run> = vec![
            (
                "e16",
                &[0x07],
                Box::new(move |d| set(d, 0x07, &|o| o.validation.not_empty = true)),
            ),
            (
                "e30",
                &[0x04, 0x07],
                Box::new(move |d| {
                    set(d, 0x07, &|o| o.auto_enter.value = Value::Data("abc".into()));
                    set(d, 0x04, &|o| o.auto_enter.value = Value::LastVisited);
                }),
            ),
            (
                "e33",
                &[0x0b],
                Box::new(move |d| set(d, 0x0b, &|o| o.auto_enter.value = Value::None)),
            ),
            (
                "e41",
                &[0x0a, 0x0e],
                Box::new(move |d| {
                    set(d, 0x0a, &|o| o.validation.not_empty = false);
                    d.apply(&SchemaChange::CreateField {
                        table_id: 0x8002,
                        name: "N2".into(),
                        data_type: FieldKind::Number,
                        comment: String::new(),
                    })
                    .unwrap();
                    set(d, 0x0e, &|o| {
                        o.auto_enter.value = Value::Serial(Serial {
                            on_commit: true,
                            next: "100".into(),
                            increment: 5,
                        });
                        o.storage.indexing = Indexing::All;
                    });
                }),
            ),
            (
                "e43",
                &[0x0a],
                Box::new(move |d| {
                    set(d, 0x0a, &|o| {
                        o.auto_enter.value = Value::Modification(EntryValue::AccountName)
                    })
                }),
            ),
        ];
        for (run, fields, edit) in runs {
            let expected = &fixture[run];
            let mut draft = SchemaDraft::new(catalogs());
            edit(&mut draft);
            let current = draft.catalogs();
            assert_eq!(
                current.occurrences.as_ref().unwrap().at(&[
                    &[INDEX],
                    &[DEPENDENCIES],
                    &[0x80, 0x02]
                ]),
                expected.at(&[
                    &[3],
                    &[OCCURRENCES],
                    &[INDEX],
                    &[DEPENDENCIES],
                    &[0x80, 0x02]
                ]),
                "{run}: the dependency entry"
            );
            let catalog = &current.fields[&key_of(0x8002)];
            assert_eq!(
                catalog.at(&[&[INDEX], &[DEPENDENCIES]]),
                expected.at(&[&[0x80, 0x02], &[FIELDS], &[INDEX], &[DEPENDENCIES]]),
                "{run}: fff0"
            );
            for &field in fields {
                let member = catalog.at(&[&[MEMBERS], &[field]]).unwrap();
                let want = |key: u8| {
                    expected
                        .at(&[&[0x80, 0x02], &[FIELDS], &[MEMBERS], &[field], &[key]])
                        .and_then(|n| n.value.clone())
                };
                let got = |key: u8| member.child(&[key]).and_then(|n| n.value.clone());
                assert_eq!(
                    got(DEFINITION),
                    want(DEFINITION),
                    "{run}: {field:02x} definition"
                );
                assert_eq!(got(AUTO_DATA), want(AUTO_DATA), "{run}: {field:02x} data");
                // FileMaker Pro writes `0c` when its Auto-Enter tab was
                // shown, which OpenMaker's dialog always does ("1").
                let serial = want(NEXT_SERIAL).unwrap_or_else(|| vec![b'1' ^ 0x5a]);
                assert_eq!(
                    got(NEXT_SERIAL),
                    Some(serial),
                    "{run}: {field:02x} next serial"
                );
            }
        }
    }

    /// A calculated custom message is written as FileMaker Pro 26's dialog
    /// writes one (schema-path.md 5.9, E34): validation `0x80` with
    /// `0x8000`, the formula in `20`, and in `18` the text older versions
    /// show. A text message replacing it is `0x80` and `18` alone.
    #[test]
    fn a_calculated_custom_message_is_written_as_filemaker_pro_26_writes_it() {
        use crate::field_options::{Message, OptionFormula};
        let (base, _) = engine_runs(include_str!(
            "../tests/fixtures/contacts-option-formulas.txt"
        ));
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "V1".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let set = |draft: &mut SchemaDraft, message: Message| {
            let mut options = options_of(draft, 0x0e);
            options.validation.not_empty = true;
            options.validation.message = Some(message);
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0e,
                    options: Box::new(options),
                })
                .unwrap();
        };
        let member = |draft: &SchemaDraft| {
            draft.current.fields[&key_of(0x8002)]
                .at(&[&[MEMBERS], &[0x0e]])
                .unwrap()
                .clone()
        };
        let validation = |member: &OwnedNode| {
            let definition = SchemaDraft::definition_of(member).unwrap();
            u32::from_be_bytes(definition[12..16].try_into().unwrap())
        };
        let text = "\"Enter \" & Self";
        set(
            &mut draft,
            Message::Calculation(OptionFormula::new(text, 0xd00002)),
        );
        let written = member(&draft);
        assert_eq!(validation(&written) & 0x8088, 0x8088);
        assert_eq!(
            written.child(&[MESSAGE]).and_then(text_of).as_deref(),
            Some(OLDER_VERSIONS_MESSAGE)
        );
        let formula = written.child(&[MESSAGE_FORMULA]).unwrap();
        assert!(formula.child(&[4]).is_some() && formula.child(&[6]).is_some());
        let mut options = options_of(&draft, 0x0e);
        options.read_members(&DraftMembers {
            member: &written,
            names: &draft.formula_names(),
            decimal_comma: false,
        });
        assert_eq!(
            options.validation.message,
            Some(Message::Calculation(OptionFormula::new(text, 0xd00002)))
        );
        set(&mut draft, Message::Text("Enter a value".into()));
        let written = member(&draft);
        assert_eq!(validation(&written) & 0x8080, 0x80);
        assert_eq!(
            written.child(&[MESSAGE]).and_then(text_of).as_deref(),
            Some("Enter a value")
        );
    }

    #[test]
    fn options_openmaker_cannot_save_are_refused() {
        let fixture = options_fixture();
        let base = &fixture["base"];
        let mut draft = SchemaDraft::new(SchemaCatalogs {
            tables: base.at(&[&[3], &[TABLES]]).cloned(),
            occurrences: base.at(&[&[3], &[OCCURRENCES]]).cloned(),
            relationships: None,
            fields: [(
                key_of(0x8002),
                base.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap().clone(),
            )]
            .into(),
        });
        let refused = |draft: &mut SchemaDraft, field: u8, change: &dyn Fn(&mut FieldOptions)| {
            let mut options = options_of(draft, field);
            change(&mut options);
            let error = draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: u64::from(field),
                    options: Box::new(options),
                })
                .unwrap_err();
            assert!(!draft.is_changed(), "{error:?}");
            error
        };
        // A field that may hold values made repeating; another index
        // language.
        for (field, change) in [
            (
                0x07,
                &(|o: &mut FieldOptions| o.storage.repetitions = 3) as &dyn Fn(&mut FieldOptions),
            ),
            (0x07, &|o: &mut FieldOptions| o.storage.language = 77),
        ] {
            let error = refused(&mut draft, field, change);
            assert_eq!(error.code, Some(UNSUPPORTED), "{field:02x}: {error:?}");
        }
        // What FileMaker Pro's dialog does not offer: an indexed container,
        // a serial counting by 0.
        let error = refused(&mut draft, 0x03, &|o| o.storage.indexing = Indexing::All);
        assert_eq!(error.code, None);
        let error = refused(&mut draft, 0x07, &|o| {
            o.auto_enter.value = AutoEnterValue::Serial(crate::field_options::Serial {
                increment: 0,
                ..Default::default()
            })
        });
        assert_eq!(error.code, None);
        // A serial number in a date field, which the record rules do not
        // enter (#960): error 3, as the table would become read-only to
        // OpenMaker. One in a number field is taken.
        let mut typed = draft.clone();
        let serial = |draft: &SchemaDraft, field: u8| {
            let mut options = options_of(draft, field);
            options.auto_enter.value = AutoEnterValue::Serial(crate::field_options::Serial {
                increment: 1,
                next: "1".into(),
                ..Default::default()
            });
            SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: u64::from(field),
                options: Box::new(options),
            }
        };
        for (name, data_type, taken) in [
            ("Due", FieldKind::Date, false),
            ("Rank", FieldKind::Number, true),
        ] {
            typed
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
            let field = members(&typed.current.fields[&key_of(0x8002)])
                .iter()
                .find(|m| member_name(m).as_deref() == Some(name))
                .unwrap()
                .segment[0];
            let result = typed.apply(&serial(&typed, field));
            match taken {
                true => result.unwrap(),
                false => assert_eq!(result.unwrap_err().code, Some(UNSUPPORTED), "{name}"),
            }
        }
        // An unrelated edit must not canonicalize an unrepresentable
        // Data + LastVisited source combination to Data alone.
        let mut ambiguous = draft.clone();
        let definition = ambiguous
            .current
            .fields
            .get_mut(&key_of(0x8002))
            .unwrap()
            .at_mut(&[&[MEMBERS], &[7], &[DEFINITION]])
            .unwrap()
            .value
            .as_mut()
            .unwrap();
        let flags = u32::from_be_bytes(definition[OPTIONS].try_into().unwrap());
        definition[OPTIONS].copy_from_slice(&((flags & !AUTO_SOURCES) | 0x14).to_be_bytes());
        let before = ambiguous.catalogs().to_root().to_wire();
        let mut options = options_of(&ambiguous, 7);
        assert!(matches!(options.auto_enter.value, AutoEnterValue::Data(_)));
        options.validation.not_empty = !options.validation.not_empty;
        let error = ambiguous
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            })
            .unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED));
        assert_eq!(ambiguous.catalogs().to_root().to_wire(), before);

        // Unique turns the value index on, as in E35.
        let mut options = options_of(&draft, 0x07);
        options.validation.unique = true;
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            })
            .unwrap();
        assert_eq!(options_of(&draft, 0x07).storage.indexing, Indexing::Minimal);
        assert_eq!(
            draft.work[&(key_of(0x8002), vec![7])],
            WORK_MEMBER | WORK_INDEX
        );
        assert!(draft.data_changed.contains(&key_of(0x8002)));
        // Reducing All to Minimal with automatic indexing off must discard
        // the old word index before op73 builds the requested value index.
        let mut options = options_of(&draft, 7);
        options.storage.indexing = Indexing::All;
        options.storage.automatic_indexes = false;
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            })
            .unwrap();
        assert_eq!(draft.work[&(key_of(0x8002), vec![7])] & WORK_DROP_INDEX, 0);
        let mut options = options_of(&draft, 7);
        options.storage.indexing = Indexing::Minimal;
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 7,
                options: Box::new(options),
            })
            .unwrap();
        assert_eq!(options_of(&draft, 7).storage.indexing, Indexing::Minimal);
        assert_eq!(
            draft.work[&(key_of(0x8002), vec![7])],
            WORK_MEMBER | WORK_DROP_INDEX | WORK_INDEX
        );
    }

    /// The calculation fields FileMaker Server's engine added to the
    /// Contacts starter solution (schema-path.md 5.10, runs c01-c20),
    /// made through the draft from the same formulas and options: the
    /// field catalog and the dependency entry come out key for key as the
    /// engine wrote them.
    /// An engine-run fixture: the base tree and each run's added (`+`) and
    /// removed (`-`) keys.
    fn engine_runs(text: &str) -> (OwnedNode, Vec<(String, Vec<&str>)>) {
        let mut base = Vec::new();
        let mut runs: Vec<(String, Vec<&str>)> = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if name != "base" {
                    runs.push((name.to_owned(), Vec::new()));
                }
            } else if let Some((_, lines)) = runs.last_mut() {
                lines.push(line);
            } else {
                base.push(line);
            }
        }
        (build(&base), runs)
    }

    /// The catalogs in an engine-run fixture's base: the tables, the
    /// occurrences and every table's field catalog it holds.
    fn engine_catalogs(base: &OwnedNode) -> SchemaCatalogs {
        SchemaCatalogs {
            tables: base.at(&[&[3], &[TABLES]]).cloned(),
            occurrences: base.at(&[&[3], &[OCCURRENCES]]).cloned(),
            relationships: base.at(&[&[3], &[RELATIONSHIPS]]).cloned(),
            fields: base
                .children
                .iter()
                .filter_map(|t| Some((t.segment.clone(), t.child(&[FIELDS])?.clone())))
                .collect(),
        }
    }

    /// The base with a run's changes.
    fn after_run(base: &OwnedNode, changes: &[&str]) -> OwnedNode {
        let mut expected = base.clone();
        for line in changes {
            if let Some(path) = line.strip_prefix('-') {
                let segments: Vec<Vec<u8>> = path.split('/').map(hex).collect();
                let refs: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
                expected.remove(&refs);
                continue;
            }
            let added = build(&[line.strip_prefix('+').unwrap()]);
            let mut stack = vec![(Vec::<Vec<u8>>::new(), &added)];
            while let Some((path, node)) = stack.pop() {
                for child in &node.children {
                    let mut at = path.clone();
                    at.push(child.segment.clone());
                    let refs: Vec<&[u8]> = at.iter().map(Vec::as_slice).collect();
                    match &child.value {
                        Some(value) if child.children.is_empty() => {
                            expected.put(&refs, OwnedNode::leaf(Vec::new(), value.clone()))
                        }
                        _ => {
                            expected.ensure(&refs);
                        }
                    }
                    stack.push((at, child));
                }
            }
        }
        expected
    }

    /// The draft's Contacts field catalog and dependency entry equal the
    /// engine's after `run`.
    fn assert_engine_wrote(draft: &SchemaDraft, expected: &OwnedNode, run: &str) {
        let current = draft.catalogs();
        same_keys(
            &current.fields[&key_of(0x8002)],
            expected.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap(),
            &format!("{run}: the field catalog"),
        );
        fn entry(o: &OwnedNode) -> &OwnedNode {
            o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]]).unwrap()
        }
        same_keys(
            entry(current.occurrences.as_ref().unwrap()),
            entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
            &format!("{run}: the dependency entry"),
        );
    }

    /// Fails with the keys that differ, as hex paths, rather than two whole
    /// trees.
    fn same_keys(written: &OwnedNode, engine: &OwnedNode, what: &str) {
        fn leaves(node: &OwnedNode, path: &str, out: &mut BTreeSet<String>) {
            let path = format!("{path}/{}", hex(&node.segment));
            if let Some(value) = &node.value {
                out.insert(format!("{path} = {}", hex(value)));
            } else if node.children.is_empty() {
                out.insert(path.clone());
            }
            for child in &node.children {
                leaves(child, &path, out);
            }
        }
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
        let (mut ours, mut theirs) = (BTreeSet::new(), BTreeSet::new());
        leaves(written, "", &mut ours);
        leaves(engine, "", &mut theirs);
        let extra: Vec<_> = ours.difference(&theirs).collect();
        let missing: Vec<_> = theirs.difference(&ours).collect();
        assert!(
            extra.is_empty() && missing.is_empty(),
            "{what}: written but not by the engine {extra:#?}; written by the engine but not here {missing:#?}"
        );
    }

    /// Calculation fields over a global or a summary field (schema-path.md
    /// 5.16), made through Create, Options for Field, Options for Summary
    /// Field and Specify Calculation: runs g01-g04 come out key for key as
    /// FileMaker's engine wrote them, the calculation unstored, listed in
    /// `fffb` when it reads a summary, and listed under its inputs as the
    /// engine lists it. Another table's global, and a calculation over C1,
    /// are refused (not measured).
    #[test]
    fn calculations_over_globals_and_summaries_are_written_as_the_engine_writes_them() {
        use crate::calc_field::CalculationField;
        use crate::meta::{SummaryDef, SummaryKind};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-global-summary.txt"
        ));
        let run = |name: &str| runs.iter().find(|(r, _)| r == name).unwrap().1.clone();
        let calc = |draft: &mut SchemaDraft, name: &str, formula: &str, result| {
            draft.apply(&SchemaChange::CreateCalculationField {
                table_id: 0x8002,
                name: name.into(),
                comment: String::new(),
                calculation: CalculationField {
                    result,
                    ..CalculationField::new(formula, 0xd00002)
                },
            })
        };
        let field = |draft: &mut SchemaDraft, name: &str, data_type| {
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
        };
        let global = |draft: &mut SchemaDraft, id| {
            let mut options = options_of(draft, id);
            options.storage.global = true;
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: id.into(),
                    options: Box::new(options),
                })
                .unwrap_or_else(|e| panic!("global: {}", e.message));
        };
        let summary = |draft: &mut SchemaDraft, kind, of| {
            draft
                .apply(&SchemaChange::CreateSummaryField {
                    table_id: 0x8002,
                    name: "S".into(),
                    comment: String::new(),
                    summary: SummaryDef::new(kind, of),
                })
                .unwrap_or_else(|e| panic!("summary: {}", e.message));
        };
        for (name, formula) in [("g01", "First Name & G"), ("g02", "G & \"x\"")] {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            field(&mut draft, "G", FieldKind::Text);
            global(&mut draft, 0x0e);
            let c = if name == "g01" { "C1" } else { "C2" };
            calc(&mut draft, c, formula, FieldKind::Text)
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
            // G itself: the engine's importer made it global when it added
            // it, with byte 2 `01` (storage asked for, 5.10); here Options
            // for Field turns it global as FileMaker Pro does (E38), which
            // keeps byte 2 `00` and gives the field a next serial.
            let mut lines = run(name);
            for line in &mut lines {
                if line.starts_with("+8002/03/05/0e/02 = ") {
                    *line =
                        "+8002/03/05/0e/02 = 0101000000000015000100000000000000000001000000000001";
                }
            }
            lines.push("+8002/03/05/0e/0c = 6b");
            assert_engine_wrote(&draft, &after_run(&base, &lines), name);
            if name == "g01" {
                // A calculation over C1: not measured.
                let refused = calc(&mut draft, "C9", "C1 & \"!\"", FieldKind::Text).unwrap_err();
                assert!(
                    refused.message.contains("not measured"),
                    "{}",
                    refused.message
                );
            }
        }
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        field(&mut draft, "N", FieldKind::Number);
        summary(&mut draft, SummaryKind::Total, 0x0e);
        calc(&mut draft, "C3", "N / S", FieldKind::Number)
            .unwrap_or_else(|e| panic!("g03: {}", e.message));
        // The importer left non-zero bytes in slots of S's definition that
        // 5.11's runs write as zeros (5.16); S is not what these runs
        // measure, so its definition is the one Options for Summary Field
        // writes.
        let summary_as_made = |run_name: &str, key: &'static str, made: &'static str| {
            let mut lines = run(run_name);
            for line in &mut lines {
                if line.starts_with(key) {
                    *line = made;
                }
            }
            lines
        };
        let lines = summary_as_made(
            "g03",
            "+8002/03/05/0f/09 = ",
            "+8002/03/05/0f/09 = 010002010000000002010e00000000010201000000000201000000000001",
        );
        assert_engine_wrote(&draft, &after_run(&base, &lines), "g03");
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        summary(&mut draft, SummaryKind::Count, 0x01);
        calc(&mut draft, "C4", "S", FieldKind::Number)
            .unwrap_or_else(|e| panic!("g04: {}", e.message));
        let lines = summary_as_made(
            "g04",
            "+8002/03/05/0e/09 = ",
            "+8002/03/05/0e/09 = 030002010000000002010100000000010201000000000201000000000001",
        );
        assert_engine_wrote(&draft, &after_run(&base, &lines), "g04");
    }

    /// What runs g01-g04 did not measure stays refused (#872's review):
    /// a calculation over a global chosen unstored, or reading one with a
    /// `Get` function or through an aggregate; turning off the global
    /// storage a calculation reads, also of a field made in the same edit;
    /// leaving unstored, or making read a global, a calculation another
    /// calculation over a global reads; and a calculation over one read
    /// through a relationship.
    #[test]
    fn calculations_over_globals_and_summaries_refuse_what_was_not_measured() {
        use crate::calc_field::{CalcStorage, CalculationField};
        use crate::meta::{SummaryDef, SummaryKind};
        let (base, _) = engine_runs(include_str!(
            "../tests/fixtures/contacts-global-summary.txt"
        ));
        let fresh = || SchemaDraft::new(engine_catalogs(&base));
        let calc_in = |draft: &mut SchemaDraft,
                       (table, context): (u64, u64),
                       name: &str,
                       formula: &str,
                       unstored: bool| {
            draft.apply(&SchemaChange::CreateCalculationField {
                table_id: table,
                name: name.into(),
                comment: String::new(),
                calculation: CalculationField {
                    storage: CalcStorage {
                        unstored,
                        ..CalcStorage::default()
                    },
                    ..CalculationField::new(formula, context)
                },
            })
        };
        let calc = |draft: &mut SchemaDraft, name: &str, formula: &str| {
            calc_in(draft, (0x8002, 0xd00002), name, formula, false)
        };
        let field = |draft: &mut SchemaDraft, name: &str, data_type| {
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
        };
        let set_global = |draft: &mut SchemaDraft, id: u8, global: bool| {
            let mut options = options_of(draft, id);
            options.storage.global = global;
            draft.apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: id.into(),
                options: Box::new(options),
            })
        };
        let change = |draft: &mut SchemaDraft, id: u64, formula: &str, unstored: bool| {
            draft.apply(&SchemaChange::ChangeCalculation {
                table_id: 0x8002,
                field_id: id,
                calculation: CalculationField {
                    storage: CalcStorage {
                        unstored,
                        ..CalcStorage::default()
                    },
                    ..CalculationField::new(formula, 0xd00002)
                },
            })
        };
        let refused = |result: Result<_, DataError>, what: &str| {
            let error = result.err().unwrap_or_else(|| panic!("{what}: accepted"));
            assert!(
                error.message.contains("not measured"),
                "{what}: {}",
                error.message
            );
        };
        // G (0x0e), a global text field, in each draft below.
        let with_global = || {
            let mut draft = fresh();
            field(&mut draft, "G", FieldKind::Text);
            set_global(&mut draft, 0x0e, true).unwrap();
            draft
        };

        // "Do not store calculation results" chosen: byte 2 `01`, which
        // g01-g04 (all asked to be stored) did not write; also kept by a
        // change of a calculation chosen unstored before.
        let mut draft = with_global();
        refused(
            calc_in(&mut draft, (0x8002, 0xd00002), "C", "First Name & G", true),
            "chosen unstored",
        );
        calc_in(&mut draft, (0x8002, 0xd00002), "U", "First Name", true).unwrap();
        refused(
            change(&mut draft, 0x0f, "First Name & G", true),
            "kept unstored",
        );

        // A Get function or an aggregate beside the global: no trigger of
        // type 1 tells them apart from what was measured.
        let mut draft = with_global();
        refused(calc(&mut draft, "C", "G & Get ( AccountName )"), "Get");
        let mut draft = fresh();
        field(&mut draft, "N", FieldKind::Number);
        draft
            .apply(&SchemaChange::CreateSummaryField {
                table_id: 0x8002,
                name: "S".into(),
                comment: String::new(),
                summary: SummaryDef::new(SummaryKind::Total, 0x0e),
            })
            .unwrap();
        refused(calc(&mut draft, "C", "Sum ( S )"), "aggregate");

        // G made in this edit, read, then made not global.
        let mut draft = with_global();
        calc(&mut draft, "C", "G").unwrap();
        let error = set_global(&mut draft, 0x0e, false).unwrap_err();
        assert!(
            error.message.contains("global storage"),
            "{}",
            error.message
        );

        // K (0x10), a stored calculation read beside G by C (0x11):
        // neither leaving K unstored nor making it read G.
        let mut draft = with_global();
        field(&mut draft, "N", FieldKind::Number);
        calc(&mut draft, "K", "N").unwrap();
        calc(&mut draft, "C", "G & K").unwrap();
        refused(
            change(&mut draft, 0x10, "N", true),
            "reader's input unstored",
        );
        refused(
            change(&mut draft, 0x10, "N & G", false),
            "reader's input over G",
        );
        // K read by D, chosen unstored and over no global: K may not come
        // to read G, which would leave D reading a calculation over one.
        let mut draft = with_global();
        field(&mut draft, "N", FieldKind::Number);
        calc(&mut draft, "K", "N").unwrap();
        calc_in(&mut draft, (0x8002, 0xd00002), "D", "K", true).unwrap();
        refused(
            change(&mut draft, 0x10, "N & G", false),
            "input of another calculation over G",
        );

        // C1 = G in Contacts, read from Addresses through a relationship.
        let mut draft = with_global();
        calc(&mut draft, "C1", "G").unwrap();
        refused(
            calc_in(&mut draft, (0x8003, 0xd00003), "C2", "Contacts::C1", false),
            "related reader",
        );
    }

    #[test]
    fn calculation_fields_are_written_as_the_engine_writes_them() {
        use crate::calc_field::{CalcStorage, CalculationField};
        let (base, runs) = engine_runs(include_str!("../tests/fixtures/contacts-calculations.txt"));
        let catalogs = || engine_catalogs(&base);
        let calc = |formula: &str, result| CalculationField {
            result,
            ..CalculationField::new(formula, 0xd00002)
        };
        let storage = |f: &dyn Fn(&mut CalcStorage)| {
            let mut storage = CalcStorage::default();
            f(&mut storage);
            storage
        };
        let made: Vec<Vec<CalculationField>> = vec![
            vec![calc("First Name & \" \" & Last Name", FieldKind::Text)],
            vec![CalculationField {
                skip_when_empty: false,
                ..calc("First Name & \" \" & Last Name", FieldKind::Text)
            }],
            vec![calc(
                "If ( IsEmpty ( Title ) ; \"none\" ; Title )",
                FieldKind::Text,
            )],
            vec![calc("If ( Title = \"x\" ; 1 ; 2 )", FieldKind::Number)],
            vec![CalculationField {
                storage: storage(&|s| s.unstored = true),
                ..calc("First Name & Last Name", FieldKind::Text)
            }],
            vec![calc("Initial & \".\"", FieldKind::Text)],
            vec![calc("Get ( CurrentDate )", FieldKind::Date)],
            vec![calc("1", FieldKind::Number)],
            vec![calc("Let ( x = Title ; x & \"!\" )", FieldKind::Text)],
            vec![CalculationField {
                storage: storage(&|s| s.global = true),
                ..calc("Title & \"g\"", FieldKind::Text)
            }],
            vec![CalculationField {
                storage: storage(&|s| s.indexing = Indexing::All),
                ..calc("Title & \"i\"", FieldKind::Text)
            }],
            vec![CalculationField {
                repetitions: 3,
                ..calc("Title & \"r\"", FieldKind::Text)
            }],
            vec![calc("Length ( Title ) + 1", FieldKind::Number)],
            vec![calc(
                "Case ( Title = \"a\" ; \"A\" ; \"B\" )",
                FieldKind::Text,
            )],
            vec![
                calc("Title & \"1\"", FieldKind::Text),
                calc("C1 & \"2\"", FieldKind::Text),
            ],
            vec![CalculationField {
                skip_when_empty: false,
                storage: storage(&|s| s.unstored = true),
                ..calc("Title & \"u\"", FieldKind::Text)
            }],
            vec![calc("Upper ( Title )", FieldKind::Text)],
            vec![calc("Title & PrimaryKey", FieldKind::Text)],
            vec![CalculationField {
                storage: storage(&|s| s.indexing = Indexing::Minimal),
                ..calc("Title & \"x\"", FieldKind::Text)
            }],
            vec![CalculationField {
                storage: storage(&|s| s.automatic_indexes = false),
                ..calc("Title & \"n\"", FieldKind::Text)
            }],
        ];
        assert_eq!(runs.len(), made.len());
        for ((run, changes), fields) in runs.iter().zip(made) {
            let expected = after_run(&base, changes);
            let mut draft = SchemaDraft::new(catalogs());
            for (n, calculation) in fields.into_iter().enumerate() {
                let made = draft.apply(&SchemaChange::CreateCalculationField {
                    table_id: 0x8002,
                    name: format!("C{}", n + 1),
                    comment: String::new(),
                    calculation,
                });
                // OpenMaker Server computes neither a global calculation
                // nor a repeating one, so those are refused.
                if matches!(run.as_str(), "c10" | "c12") {
                    assert_eq!(made.unwrap_err().code, Some(UNSUPPORTED), "{run}");
                    continue;
                }
                made.unwrap_or_else(|e| panic!("{run}: {}", e.message));
            }
            if !matches!(run.as_str(), "c10" | "c12") {
                assert_engine_wrote(&draft, &expected, run);
            }
        }
        // A calculation that a stored one reads cannot stop being a stored
        // value: OpenMaker Server could no longer compute the reader.
        let mut draft = SchemaDraft::new(catalogs());
        for (name, formula) in [("C1", "Title & \"1\""), ("C2", "C1 & \"2\"")] {
            draft
                .apply(&SchemaChange::CreateCalculationField {
                    table_id: 0x8002,
                    name: name.into(),
                    comment: String::new(),
                    calculation: calc(formula, FieldKind::Text),
                })
                .unwrap();
        }
        let before = draft.catalogs().clone();
        let refused = draft.apply(&SchemaChange::ChangeCalculation {
            table_id: 0x8002,
            field_id: 0x0e,
            calculation: CalculationField {
                storage: storage(&|s| s.unstored = true),
                ..calc("Upper ( Title )", FieldKind::Text)
            },
        });
        assert_eq!(refused.unwrap_err().code, Some(UNSUPPORTED));
        assert_eq!(draft.catalogs(), &before);
    }

    /// The summary fields FileMaker Server's engine added to the Contacts
    /// table (schema-path.md 5.11, runs s01-s16), made through the draft
    /// from the same definitions: the field catalog and the dependency
    /// entry come out key for key as the engine wrote them, the weighted
    /// average (s03) and the subtotaled fraction (s10) included.
    #[test]
    fn summary_fields_are_written_as_the_engine_writes_them() {
        use crate::meta::{summary_flag as flag, SummaryDef, SummaryField, SummaryKind as Kind};
        let (base, runs) = engine_runs(include_str!("../tests/fixtures/contacts-summaries.txt"));
        let (title, n, w, nr, created) = (0x04, 0x0e, 0x0f, 0x10, 0x0c);
        let summary = |kind, field, flags, additional: Option<u64>| SummaryDef {
            flags,
            additional: additional.map(|field| SummaryField {
                occurrence: Some(0xd00002),
                field,
                repetition: 1,
            }),
            ..SummaryDef::new(kind, field)
        };
        let made = [
            summary(Kind::Total, n, 0, None),
            summary(Kind::Average, n, 0, None),
            summary(Kind::Average, n, flag::OPTION, Some(w)),
            summary(Kind::Count, title, 0, None),
            summary(Kind::Minimum, n, 0, None),
            summary(Kind::Maximum, created, 0, None),
            summary(Kind::StdDeviation, n, 0, None),
            summary(Kind::StdDeviation, n, flag::OPTION, None),
            summary(Kind::Fraction, n, 0, None),
            summary(Kind::Fraction, n, flag::OPTION, Some(title)),
            summary(Kind::List, title, 0, None),
            summary(Kind::Total, n, flag::RUNNING, None),
            summary(Kind::Total, n, flag::RUNNING | flag::RESTART, Some(title)),
            summary(Kind::Count, title, flag::RUNNING, None),
            summary(Kind::Total, nr, flag::INDIVIDUAL_REPETITIONS, None),
            summary(Kind::Count, 0x01, 0, None),
        ];
        assert_eq!(runs.len(), made.len());
        for ((run, changes), summary) in runs.iter().zip(made) {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            let result = draft.apply(&SchemaChange::CreateSummaryField {
                table_id: 0x8002,
                name: "S".into(),
                comment: String::new(),
                summary,
            });
            result.unwrap_or_else(|e| panic!("{run}: {}", e.message));
            assert_engine_wrote(&draft, &after_run(&base, changes), run);
            // The restart field cannot go while the summary uses it.
            if run == "s13" {
                let refused = draft.apply(&SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: title,
                });
                assert_eq!(refused.unwrap_err().code, Some(UNSUPPORTED));
            }
        }
        // A weight is a number field and not a summary (the Total made
        // first, 0x11); a standard deviation has no second field.
        let weight = |field| {
            Some(SummaryField {
                occurrence: Some(0xd00002),
                field,
                repetition: 1,
            })
        };
        for (def, why) in [
            (
                SummaryDef {
                    additional: weight(title),
                    ..summary(Kind::Average, n, flag::OPTION, None)
                },
                "weighted by a number field",
            ),
            (
                SummaryDef {
                    additional: weight(0x11),
                    ..summary(Kind::Average, n, flag::OPTION, None)
                },
                "weighted by a number field",
            ),
            (
                summary(Kind::StdDeviation, n, flag::OPTION, Some(w)),
                "no second field",
            ),
        ] {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            draft
                .apply(&SchemaChange::CreateSummaryField {
                    table_id: 0x8002,
                    name: "Weight".into(),
                    comment: String::new(),
                    summary: summary(Kind::Total, n, 0, None),
                })
                .unwrap();
            let refused = draft.apply(&SchemaChange::CreateSummaryField {
                table_id: 0x8002,
                name: "S".into(),
                comment: String::new(),
                summary: def,
            });
            let message = refused.unwrap_err().message;
            assert!(message.contains(why), "{def:?}: {message}");
        }
        // Nor can a summarized field change type, which would leave the
        // summary's type behind.
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        let new_field = |name: &str| SchemaChange::CreateField {
            table_id: 0x8002,
            name: name.into(),
            data_type: FieldKind::Number,
            comment: String::new(),
        };
        draft.apply(&new_field("Input")).unwrap();
        draft
            .apply(&SchemaChange::CreateSummaryField {
                table_id: 0x8002,
                name: "Least".into(),
                comment: String::new(),
                summary: summary(Kind::Minimum, 0x11, 0, None),
            })
            .unwrap();
        let refused = draft.apply(&SchemaChange::ChangeField {
            table_id: 0x8002,
            field_id: 0x11,
            name: "Input".into(),
            data_type: FieldKind::Date,
            comment: String::new(),
        });
        assert_eq!(refused.unwrap_err().code, Some(UNSUPPORTED));
    }

    /// The auto-entered calculations and validation options FileMaker
    /// Server's engine gave a new field of the Contacts table
    /// (schema-path.md 5.13, runs a01-a11 and v01-v24), made through the
    /// draft as Create and Options for Field make them: the field catalog
    /// and the dependency entry come out key for key as the engine wrote
    /// them, with the next serial value `0c` FileMaker Pro writes for the
    /// Auto-Enter tab (5.9) and the value list reference as FileMaker
    /// Pro's files hold it.
    /// Auto-entered and validation formulas over a global field of their
    /// own table (schema-path.md 5.17), made through Create, Options for
    /// Field (G made global as FileMaker Pro makes it, E38) and the
    /// formula's own Options for Field: runs o01-o04 come out key for key
    /// as FileMaker's engine wrote them, the global an ordinary input. A
    /// summary field in such a formula, and another table's global, stay
    /// refused (not measured).
    #[test]
    fn option_formulas_over_a_global_are_written_as_the_engine_writes_them() {
        use crate::field_options::{AutoCalculation, OptionFormula, ValidationCalculation};
        use crate::meta::{SummaryDef, SummaryKind};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-option-globals.txt"
        ));
        let formula = |text: &str| OptionFormula::new(text, 0xd00002);
        let auto = |text: &str| {
            let mut options = FieldOptions::default();
            options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
                formula: formula(text),
                replace_existing: false,
                skip_when_empty: true,
            });
            options
        };
        let check = |text: &str| {
            let mut options = FieldOptions::default();
            options.validation.calculation = Some(ValidationCalculation {
                formula: formula(text),
                only_if_modified: true,
            });
            options
        };
        // G (0e), global, and the field over it (0f).
        let with_global = || {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            for name in ["G", "F"] {
                draft
                    .apply(&SchemaChange::CreateField {
                        table_id: 0x8002,
                        name: name.into(),
                        data_type: FieldKind::Text,
                        comment: String::new(),
                    })
                    .unwrap();
            }
            let mut options = options_of(&draft, 0x0e);
            options.storage.global = true;
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0e,
                    options: Box::new(options),
                })
                .unwrap();
            draft
        };
        let made = [
            ("o01", "A", auto("G")),
            ("o02", "A", auto("First Name & G")),
            ("o03", "V", check("Self <> G")),
            ("o04", "V", check("First Name = G")),
        ];
        assert_eq!(runs.len(), made.len());
        for ((run, changes), (name, field, options)) in runs.iter().zip(made) {
            assert_eq!(run, name);
            let mut draft = with_global();
            draft
                .apply(&SchemaChange::ChangeField {
                    table_id: 0x8002,
                    field_id: 0x0f,
                    name: field.into(),
                    data_type: FieldKind::Text,
                    comment: String::new(),
                })
                .unwrap();
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0f,
                    options: Box::new(options),
                })
                .unwrap_or_else(|e| panic!("{run}: {}", e.message));
            // G: the importer made it global as it added it (byte 2 `01`);
            // Options for Field does as FileMaker Pro does (E38). Both
            // fields get a next serial, as Create gives one.
            let mut lines = changes.clone();
            for line in &mut lines {
                if line.starts_with("+8002/03/05/0e/02 = ") {
                    *line =
                        "+8002/03/05/0e/02 = 0101000000000015000100000000000000000001000000000001";
                }
            }
            lines.push("+8002/03/05/0e/0c = 6b");
            lines.push("+8002/03/05/0f/0c = 6b");
            assert_engine_wrote(&draft, &after_run(&base, &lines), run);
        }
        // Not measured: a summary field in such a formula, another table's
        // global.
        let mut draft = with_global();
        draft
            .apply(&SchemaChange::CreateSummaryField {
                table_id: 0x8002,
                name: "S".into(),
                comment: String::new(),
                summary: SummaryDef::new(SummaryKind::Count, 0x01),
            })
            .unwrap();
        for text in ["S & \"\"", "G & S"] {
            let error = draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0f,
                    options: Box::new(auto(text)),
                })
                .unwrap_err();
            assert!(
                error.message.contains("summary"),
                "{text}: {}",
                error.message
            );
        }
        // Nor a container global or a repeating one (#897's review): the
        // record rules would read neither.
        let mut draft = with_global();
        for (name, data_type, repetitions) in
            [("GC", FieldKind::Container, 1), ("GR", FieldKind::Text, 2)]
        {
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
            let id = if name == "GC" { 0x10 } else { 0x11 };
            let mut options = options_of(&draft, id);
            options.storage.global = true;
            options.storage.repetitions = repetitions;
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: id.into(),
                    options: Box::new(options),
                })
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        }
        for text in ["GetAsText ( GC )", "GetRepetition ( GR ; 2 )"] {
            let error = draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0f,
                    options: Box::new(auto(text)),
                })
                .unwrap_err();
            assert!(
                error.message.contains("container or repeating global"),
                "{text}: {}",
                error.message
            );
        }
        let mut draft = with_global();
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8003,
                name: "H".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let h = members(&draft.current.fields[&key_of(0x8003)])
            .iter()
            .find(|m| member_name(m).as_deref() == Some("H"))
            .and_then(|m| id_of(&m.segment))
            .unwrap();
        let mut options = FieldOptions::default();
        options.validation.calculation = Some(ValidationCalculation {
            formula: OptionFormula::new("Contacts::G = Self", 0xd00003),
            only_if_modified: true,
        });
        let error = draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8003,
                field_id: h,
                options: Box::new(options),
            })
            .unwrap_err();
        assert!(error.message.contains("another table"), "{}", error.message);
    }

    #[test]
    fn option_formulas_are_written_as_the_engine_writes_them() {
        use crate::field_options::{
            AutoCalculation, Message, OptionFormula, ValidationCalculation,
        };
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-option-formulas.txt"
        ));
        let formula = |text: &str| OptionFormula::new(text, 0xd00002);
        let auto = |text: &str, replace: bool, skip: bool| {
            let mut options = FieldOptions::default();
            options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
                formula: formula(text),
                replace_existing: replace,
                skip_when_empty: skip,
            });
            options
        };
        let rules = |f: &dyn Fn(&mut crate::field_options::Validation)| {
            let mut options = FieldOptions::default();
            f(&mut options.validation);
            options
        };
        let check = |text: &str, only_if_modified: bool| {
            Some(ValidationCalculation {
                formula: formula(text),
                only_if_modified,
            })
        };
        let full = "First Name & \" \" & Last Name";
        let length = "Length ( Self ) < 10";
        let made: Vec<(&str, FieldKind, FieldOptions)> = vec![
            ("A1", FieldKind::Text, auto(full, true, true)),
            ("A1", FieldKind::Text, auto(full, false, true)),
            ("A1", FieldKind::Text, auto("\"x\"", false, true)),
            (
                "A1",
                FieldKind::Text,
                auto("Initial & Get ( UUID )", false, true),
            ),
            (
                "A1",
                FieldKind::Text,
                auto("If ( IsEmpty ( Self ) ; Title ; Self )", false, true),
            ),
            ("A1", FieldKind::Text, auto("First Name", false, false)),
            (
                "A1",
                FieldKind::Number,
                auto("IsEmpty ( First Name )", false, true),
            ),
            (
                "A1",
                FieldKind::Date,
                auto("Get ( CurrentDate )", false, true),
            ),
            ("A1", FieldKind::Text, auto("\"x\"", false, false)),
            ("V1", FieldKind::Text, rules(&|v| v.value_list = Some(3))),
            (
                "V1",
                FieldKind::Number,
                rules(&|v| v.range = Some(("1".into(), "10".into()))),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| v.calculation = check(length, true)),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| v.calculation = check(length, false)),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.not_empty = true;
                    v.message = Some(Message::Text("Too long".into()));
                }),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.calculation = check(length, true);
                    v.message = Some(Message::Text("Too long".into()));
                }),
            ),
            (
                "V1",
                FieldKind::Number,
                rules(&|v| {
                    v.always = true;
                    v.not_empty = true;
                    v.range = Some(("1".into(), "10".into()));
                }),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.calculation = check("Length ( Self ) < 10 and First Name ≠ \"\"", true)
                }),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.always = true;
                    v.not_empty = true;
                }),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.always = true;
                    v.calculation = check(length, true);
                }),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| {
                    v.always = true;
                    v.value_list = Some(3);
                }),
            ),
            (
                "V1",
                FieldKind::Date,
                rules(&|v| v.range = Some(("1/1/2020".into(), "2020-12-31".into()))),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| v.calculation = check("not IsEmpty ( Title )", true)),
            ),
            (
                "V1",
                FieldKind::Text,
                rules(&|v| v.calculation = check("Get ( CurrentDate ) > 0", true)),
            ),
        ];
        assert_eq!(runs.len(), made.len());
        let member = |key: &str| format!("8002/03/05/0e/{key}");
        for ((run, changes), (name, data_type, options)) in runs.iter().zip(made) {
            let mut changes = changes.clone();
            let serial = format!("+{} = 6b", member("0c"));
            changes.push(serial.as_str());
            let list = field_options::value_list_reference(3).unwrap();
            let list = format!("+{} = {}", member("07"), hex_string(&list));
            if changes
                .iter()
                .any(|l| l.starts_with(&format!("+{}", member("07"))))
            {
                changes.retain(|l| !l.starts_with(&format!("+{}", member("07"))));
                changes.push(list.as_str());
            }
            let expected = after_run(&base, &changes);
            // The starter solution's value list 3, Title, is custom values.
            let mut draft =
                SchemaDraft::new(engine_catalogs(&base)).with_formulas(FormulaSettings {
                    value_lists: vec![(3, None)],
                    ..FormulaSettings::default()
                });
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap_or_else(|e| panic!("{run}: {}", e.message));
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8002,
                    field_id: 0x0e,
                    options: Box::new(options),
                })
                .unwrap_or_else(|e| panic!("{run}: {}", e.message));
            assert_engine_wrote(&draft, &expected, run);
        }
        // What OpenMaker's record rules could not evaluate or check is
        // refused, so the table stays writable: Evaluate, a repeating or
        // container field's calculated value, an unstored calculation's
        // value, the field's own name (a cycle to them; Self is read), a
        // value list they do not check; and a missing value list.
        let mut draft = SchemaDraft::new(engine_catalogs(&base)).with_formulas(FormulaSettings {
            value_lists: vec![(3, None), (4, Some("related values".into()))],
            ..FormulaSettings::default()
        });
        for (name, data_type) in [("A1", FieldKind::Text), ("A2", FieldKind::Container)] {
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8002,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
        }
        let mut unstored = CalculationField::new("Title & \"u\"", 0xd00002);
        unstored.storage.unstored = true;
        draft
            .apply(&SchemaChange::CreateCalculationField {
                table_id: 0x8002,
                name: "U".into(),
                comment: String::new(),
                calculation: unstored,
            })
            .unwrap();
        let before = draft.catalogs().clone();
        let mut refusals: Vec<(u64, FieldOptions, Option<i32>)> = [
            (0x0e, "Evaluate ( \"Title\" )", 1),
            (0x0e, "Title", 3),
            (0x0f, "Photo", 1),
            (0x0e, "U & \"!\"", 1),
            (0x0e, "A1 & \"x\"", 1),
        ]
        .into_iter()
        .map(|(field, text, repetitions)| {
            let mut options = auto(text, false, true);
            options.storage.repetitions = repetitions;
            (field, options, Some(UNSUPPORTED))
        })
        .collect();
        refusals.push((
            0x0e,
            rules(&|v| v.calculation = check("U ≠ \"\"", true)),
            Some(UNSUPPORTED),
        ));
        refusals.push((0x0e, rules(&|v| v.value_list = Some(4)), Some(UNSUPPORTED)));
        // Messages the record rules could not evaluate over the record alone.
        refusals.push((
            0x0e,
            rules(&|v| {
                v.not_empty = true;
                v.message = Some(Message::Calculation(formula("\"See \" & A2")));
            }),
            Some(UNSUPPORTED),
        ));
        refusals.push((
            0x0e,
            rules(&|v| {
                v.not_empty = true;
                v.message = Some(Message::Calculation(formula(
                    "\"Bad \" & Get ( RecordNumber )",
                )));
            }),
            Some(UNSUPPORTED),
        ));
        refusals.push((0x0e, rules(&|v| v.value_list = Some(9)), None));
        for (field, options, code) in refusals {
            let refused = draft.apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: field,
                options: Box::new(options.clone()),
            });
            assert_eq!(refused.unwrap_err().code, code, "{options:?}");
            assert_eq!(draft.catalogs(), &before);
        }
    }

    /// The tables FileMaker Server's engine gave an auto-entered formula
    /// over related fields or a lookup (schema-path.md 5.13, runs a04, a05,
    /// a12-a14, l01-l04, a15, v28, and p01's lookup from a parent table,
    /// battery s41): rebuilding their evaluation lists from `fff0`
    /// and the triggers derived from the `fb` nodes gives back exactly
    /// what the engine wrote, so OpenMaker may change such tables.
    #[test]
    fn related_option_triggers_are_derived_as_the_engine_derives_them() {
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        assert_eq!(runs.len(), 12);
        for (run, changes) in &runs {
            let after = after_run(&base, changes);
            let draft = SchemaDraft::new(engine_catalogs(&after));
            for table in [0x8002, 0x8004] {
                let table = key_of(table);
                let derived = draft.derived_triggers(&table).unwrap();
                let (depends, entry) = draft.triggers_of(&table).unwrap();
                let mut rebuilt = entry.clone();
                calc_triggers::rebuild(
                    &mut rebuilt,
                    &depends,
                    &derived,
                    draft.fields(&table).unwrap(),
                )
                .unwrap();
                assert_eq!(sorted(&rebuilt), sorted(&entry), "{run}: {table:02x?}");
                // Without the derived triggers it would not reproduce.
                if !derived.is_empty() {
                    let mut without = entry.clone();
                    calc_triggers::rebuild(
                        &mut without,
                        &depends,
                        &calc_triggers::Derived::new(),
                        draft.fields(&table).unwrap(),
                    )
                    .unwrap();
                    assert_ne!(sorted(&without), sorted(&entry), "{run}: {table:02x?}");
                }
            }
        }
    }

    /// Deleting a field with dependency data of its own (schema-path.md
    /// 5.15): from FileMaker's own file, the field deleted through the
    /// draft leaves the field catalog and the dependency entry key for key
    /// as FileMaker's engine left them: a calculation (d01), an entry
    /// option with a validation (d02), a summary (e01), a validation
    /// calculation (e02), an auto-entered calculation over the record's
    /// own fields (e03) and over a related field (e04), and a lookup
    /// (e06), and a field validated Always (e07). A field another
    /// definition reads stays, as the engine kept it (d03, e05), and so do
    /// a relationship's match field (d04), a field a relationship sorts by,
    /// one a value list takes its values from, and one another table's
    /// validation reads; deleting a field keeps the container cleanup the
    /// edit asked for.
    #[test]
    fn deletions_are_written_as_the_engine_writes_them() {
        let (base, runs) = engine_runs(include_str!("../tests/fixtures/contacts-deletions.txt"));
        let run = |name: &str| runs.iter().find(|(r, _)| r == name).unwrap().1.clone();
        let before =
            |added: Option<&str>| added.map_or_else(|| base.clone(), |a| after_run(&base, &run(a)));
        let entry =
            |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]]).unwrap());
        for (added, deleted, field) in [
            (None, "d01", 0x08),
            (None, "d02", 0x0a),
            (Some("s02"), "e01", 0x11),
            (Some("v09"), "e02", 0x0e),
            (Some("a01"), "e03", 0x0e),
            (Some("a04"), "e04", 0x0e),
            (Some("p01"), "e06", 0x0e),
            (Some("v17"), "e07", 0x0e),
        ] {
            let mut draft = SchemaDraft::new(engine_catalogs(&before(added)));
            draft
                .apply(&SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: field,
                })
                .unwrap_or_else(|e| panic!("{deleted}: {}", e.message));
            let expected = after_run(&base, &run(deleted));
            let current = draft.catalogs();
            assert_eq!(
                sorted(&current.fields[&key_of(0x8002)]),
                sorted(expected.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap()),
                "{deleted}: the field catalog"
            );
            assert_eq!(
                entry(current.occurrences.as_ref().unwrap()),
                entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
                "{deleted}: the dependency entry"
            );
        }
        for (added, field, why) in [
            (None, 0x01, "another field uses it"),
            (Some("s02"), 0x0e, "a summary field summarizes it"),
            (None, 0x09, "a relationship matches on it"),
        ] {
            let mut draft = SchemaDraft::new(engine_catalogs(&before(added)));
            let refused = draft
                .apply(&SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: field,
                })
                .unwrap_err();
            assert!(refused.message.contains(why), "{}", refused.message);
        }
        let refused = |draft: &mut SchemaDraft, table_id, field_id| {
            draft
                .apply(&SchemaChange::DeleteField { table_id, field_id })
                .unwrap_err()
                .message
        };
        // Phone Numbers sorted by Number through relationship 2.
        {
            use om_scripts::specs::{SortEntry, SortSpec, SpecField};
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            let order = SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: vec![SortEntry {
                    field: SpecField {
                        occurrence: Some(0xd00004),
                        field: 2,
                        repetition: 1,
                    },
                    summary: None,
                    value_list: None,
                    descending: false,
                    locale: 0,
                }],
            };
            draft
                .apply(&SchemaChange::ChangeRelationship {
                    relationship_id: 2,
                    matches: vec![RelationshipMatch {
                        left_field: 9,
                        operator: 0,
                        right_field: 5,
                    }],
                    left: crate::RelationshipSide::default(),
                    right: crate::RelationshipSide {
                        allow_creation: true,
                        sort: true,
                        order: Some(order),
                        ..crate::RelationshipSide::default()
                    },
                })
                .unwrap();
            let why = refused(&mut draft, 0x8004, 2);
            assert!(
                why.contains("a relationship sorts its records by it"),
                "{why}"
            );
        }
        // A value list of Initial's values.
        {
            let mut draft =
                SchemaDraft::new(engine_catalogs(&base)).with_formulas(FormulaSettings {
                    value_list_fields: vec![(0x8002, 0x08)],
                    ..FormulaSettings::default()
                });
            let why = refused(&mut draft, 0x8002, 0x08);
            assert!(
                why.contains("a value list takes its values from it"),
                "{why}"
            );
        }
        // A Phone Numbers validation reading Contacts::Initial (a related
        // trigger `00 <Contacts> <Initial>` in its `fff1`).
        {
            let mut catalogs = engine_catalogs(&base);
            let reader = OwnedNode::list(
                vec![0x02],
                vec![OwnedNode::leaf(
                    vec![0x00, 0x03, 0xd0, 0x00, 0x02, 0x01, 0x08],
                    vec![0x01],
                )],
            );
            catalogs.fields.get_mut(&key_of(0x8004)).unwrap().put(
                &[&[INDEX], &[DEPENDENCIES], VALIDATION_DEPENDENCIES, &[0x02]],
                reader,
            );
            let mut draft = SchemaDraft::new(catalogs);
            let why = refused(&mut draft, 0x8002, 0x08);
            assert!(why.contains("a validation calculation reads it"), "{why}");
        }
        // Initial's own validation reading Initial through a relationship
        // (as through a self-join) goes with it: the deletion goes ahead.
        {
            let mut catalogs = engine_catalogs(&base);
            let reader = OwnedNode::list(
                vec![0x08],
                vec![OwnedNode::leaf(
                    vec![0x00, 0x03, 0xd0, 0x00, 0x02, 0x01, 0x08],
                    vec![0x01],
                )],
            );
            catalogs.fields.get_mut(&key_of(0x8002)).unwrap().put(
                &[&[INDEX], &[DEPENDENCIES], VALIDATION_DEPENDENCIES, &[0x08]],
                reader,
            );
            let mut draft = SchemaDraft::new(catalogs);
            draft
                .apply(&SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: 0x08,
                })
                .unwrap_or_else(|e| panic!("{}", e.message));
        }
        // Container cleanup the edit asked for before the deletion stays.
        {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            let key = (key_of(0x8002), key_of(0x07));
            draft.work.insert(key.clone(), WORK_CONTAINER);
            draft
                .apply(&SchemaChange::DeleteField {
                    table_id: 0x8002,
                    field_id: 0x07,
                })
                .unwrap();
            assert_ne!(draft.work[&key] & WORK_CONTAINER, 0);
        }
    }

    /// A lookup from a parent table, keyed on its auto-entered
    /// `Get ( UUID )` (auto-entry.md, Lookups, battery s41): run p01's
    /// Contacts::L2, `Phone Numbers::Number` looked up from Contacts, made
    /// through Create and Options for Field, comes out key for key as the
    /// engine wrote it (the field catalog with its `fb` node, and the
    /// dependency entry with PrimaryKey's derived trigger), but `08` in
    /// FileMaker Pro's own padding and the `0c` of the Auto-Enter tab.
    #[test]
    fn parent_lookups_are_written_as_the_engine_writes_them() {
        use crate::field_options::{Lookup, NoMatch};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let (_, changes) = runs.iter().find(|(run, _)| run == "p01").unwrap();
        let lookup = Lookup {
            from: 0xd00002,
            to: 0xd00004,
            copied: 2,
            copy_empty: true,
            no_match: NoMatch::DoNotCopy,
        };
        let engine = changes
            .iter()
            .find_map(|l| l.strip_prefix("+8002/03/05/0e/08 = "))
            .unwrap();
        assert_eq!(Lookup::decode(&hex(engine), None), Some(lookup.clone()));
        let mut changes = changes.clone();
        let native = format!(
            "+8002/03/05/0e/08 = {}",
            hex_string(&lookup.encode().unwrap())
        );
        changes.retain(|l| !l.starts_with("+8002/03/05/0e/08 "));
        changes.push(native.as_str());
        changes.push("+8002/03/05/0e/0c = 6b");
        let expected = after_run(&base, &changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "L2".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let mut options = FieldOptions::default();
        options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup));
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 0x0e,
                options: Box::new(options),
            })
            .unwrap_or_else(|e| panic!("{}", e.message));
        let current = draft.catalogs();
        assert_eq!(
            sorted(&current.fields[&key_of(0x8002)]),
            sorted(expected.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap()),
            "the field catalog"
        );
        let entry =
            |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]]).unwrap());
        assert_eq!(
            entry(current.occurrences.as_ref().unwrap()),
            entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
            "the dependency entry"
        );
    }

    /// A lookup through a sorted relationship (auto-entry.md, Lookups,
    /// battery s42): run p02's Assets::LA, `Assignments::Note` looked up
    /// from Assets through the starter's relationship sorted by
    /// Assignments::Sort By, made through Create and Options for Field,
    /// comes out key for key as the engine wrote it, but `08` in FileMaker
    /// Pro's own padding and the `0c` of the Auto-Enter tab. With the
    /// relationship sorted by Note in Japanese, which the record rules do
    /// not order as FileMaker, the lookup is refused.
    #[test]
    fn sorted_lookups_are_written_as_the_engine_writes_them() {
        use crate::field_options::{Lookup, NoMatch};
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let (base, runs) = engine_runs(include_str!("../tests/fixtures/assets-sorted-lookup.txt"));
        let (_, changes) = runs.iter().find(|(run, _)| run == "p02").unwrap();
        let lookup = Lookup {
            from: 0xd00002,
            to: 0xd00005,
            copied: 4,
            copy_empty: true,
            no_match: NoMatch::DoNotCopy,
        };
        let engine = changes
            .iter()
            .find_map(|l| l.strip_prefix("+8002/03/05/0f/08 = "))
            .unwrap();
        assert_eq!(Lookup::decode(&hex(engine), None), Some(lookup.clone()));
        let mut changes = changes.clone();
        let native = format!(
            "+8002/03/05/0f/08 = {}",
            hex_string(&lookup.encode().unwrap())
        );
        changes.retain(|l| !l.starts_with("+8002/03/05/0f/08 "));
        changes.push(native.as_str());
        changes.push("+8002/03/05/0f/0c = 6b");
        let expected = after_run(&base, &changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "LA".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let mut options = FieldOptions::default();
        options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup));
        let set_options = SchemaChange::SetFieldOptions {
            table_id: 0x8002,
            field_id: 0x0f,
            options: Box::new(options),
        };
        draft
            .apply(&set_options)
            .unwrap_or_else(|e| panic!("{}", e.message));
        let current = draft.catalogs();
        assert_eq!(
            sorted(&current.fields[&key_of(0x8002)]),
            sorted(expected.at(&[&[0x80, 0x02], &[FIELDS]]).unwrap()),
            "the field catalog"
        );
        let entry =
            |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]]).unwrap());
        assert_eq!(
            entry(current.occurrences.as_ref().unwrap()),
            entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
            "the dependency entry"
        );

        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        let note_in_japanese = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00005),
                    field: 4,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 31,
            }],
        };
        draft
            .apply(&SchemaChange::ChangeRelationship {
                relationship_id: 2,
                matches: vec![RelationshipMatch {
                    left_field: 10,
                    operator: 0,
                    right_field: 1,
                }],
                left: crate::RelationshipSide::default(),
                right: crate::RelationshipSide {
                    allow_creation: true,
                    sort: true,
                    order: Some(note_in_japanese),
                    ..crate::RelationshipSide::default()
                },
            })
            .unwrap();
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "LA".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let refused = draft.apply(&set_options).unwrap_err();
        assert!(refused.message.contains("Japanese"), "{}", refused.message);
    }

    /// Lookups of other types than text (auto-entry.md, Lookups, battery
    /// s43): run t02's five Assignments lookups from Assets (Number from a
    /// number, Date from a date, Text from a number and from a date, Number
    /// from text), made through Create and Options for Field, come out key
    /// for key as the engine wrote them, but each lookup's `08` in FileMaker
    /// Pro's own padding and the `0c` of the Auto-Enter tab. A timestamp
    /// field, text into a date field, and a timestamp copied are refused:
    /// OpenMaker does not read those as their stored text (`rules::typed_copy`).
    #[test]
    fn typed_lookups_are_written_as_the_engine_writes_them() {
        use crate::field_options::{Lookup, NoMatch};
        let (base, runs) = engine_runs(include_str!("../tests/fixtures/assets-typed-lookups.txt"));
        let (_, changes) = runs.iter().find(|(run, _)| run == "t02").unwrap();
        // (field, name, type, copied Assets field)
        let lookups = [
            (0x0c, "LN", FieldKind::Number, 5),
            (0x0d, "LD", FieldKind::Date, 7),
            (0x0e, "LXN", FieldKind::Text, 5),
            (0x0f, "LXD", FieldKind::Text, 7),
            (0x10, "LNT", FieldKind::Number, 1),
        ];
        let lookup = |copied| Lookup {
            from: 0xd00005,
            to: 0xd00002,
            copied,
            copy_empty: true,
            no_match: NoMatch::DoNotCopy,
        };
        let mut changes = changes.clone();
        let mut native = Vec::new();
        for (field, _, _, copied) in lookups {
            let key = format!("+8005/03/05/{field:02x}/08 ");
            let engine = changes
                .iter()
                .find_map(|l| l.strip_prefix(key.as_str()))
                .and_then(|l| l.strip_prefix("= "))
                .unwrap();
            assert_eq!(Lookup::decode(&hex(engine), None), Some(lookup(copied)));
            native.push(format!(
                "{key}= {}",
                hex_string(&lookup(copied).encode().unwrap())
            ));
            native.push(format!("+8005/03/05/{field:02x}/0c = 6b"));
            changes.retain(|l| !l.starts_with(key.as_str()));
        }
        changes.extend(native.iter().map(String::as_str));
        let expected = after_run(&base, &changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        let make = |draft: &mut SchemaDraft, field, name: &str, data_type, copied| {
            draft
                .apply(&SchemaChange::CreateField {
                    table_id: 0x8005,
                    name: name.into(),
                    data_type,
                    comment: String::new(),
                })
                .unwrap();
            let mut options = FieldOptions::default();
            options.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup(copied)));
            draft.apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8005,
                field_id: field,
                options: Box::new(options),
            })
        };
        for (field, name, data_type, copied) in lookups {
            make(&mut draft, field, name, data_type, copied)
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        }
        let current = draft.catalogs();
        for table in [0x8002, 0x8005] {
            let [high, low] = (table as u16).to_be_bytes();
            assert_eq!(
                sorted(&current.fields[&key_of(table)]),
                sorted(expected.at(&[&[high, low], &[FIELDS]]).unwrap()),
                "the field catalog of {table:x}"
            );
            let entry =
                |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[high, low]]).unwrap());
            assert_eq!(
                entry(current.occurrences.as_ref().unwrap()),
                entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
                "the dependency entry of {table:x}"
            );
        }
        // Into a timestamp field; text into a date field; a timestamp
        // (CreationTimestamp, 0d) into text.
        for (field, name, data_type, copied, why) in [
            (
                0x11,
                "LTS",
                FieldKind::Timestamp,
                0x0d,
                "text, number and date fields only",
            ),
            (
                0x12,
                "LDT",
                FieldKind::Date,
                1,
                "only a date into a date field",
            ),
            (
                0x13,
                "LXT",
                FieldKind::Text,
                0x0d,
                "only a date into a date field",
            ),
        ] {
            let refused = make(&mut draft, field, name, data_type, copied).unwrap_err();
            assert!(refused.message.contains(why), "{name}: {}", refused.message);
        }
    }

    /// The lookup FileMaker Server's engine gave Phone Numbers in run l01
    /// (schema-path.md 5.13: Contacts::Company looked up through Contacts,
    /// "do not copy", "Don't copy contents if empty"), made through Create
    /// and Options for Field: the field catalog and the dependency entry
    /// come out key for key as the engine wrote them, with `08` in
    /// FileMaker Pro's own padding (5.5) and the `0c` it writes for the
    /// Auto-Enter tab. The no-match choices of l02-l04 are refused, as
    /// OpenMaker's record rules copy only an exact match; with the lookup
    /// in place the table's other options still change, and what it reads
    /// cannot be deleted.
    #[test]
    fn lookups_are_written_as_the_engine_writes_them() {
        use crate::field_options::{Lookup, NoMatch};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let (_, changes) = runs.iter().find(|(run, _)| run == "l01").unwrap();
        let lookup = |no_match| Lookup {
            from: 0xd00004,
            to: 0xd00002,
            copied: 5,
            copy_empty: false,
            no_match,
        };
        let mut changes = changes.clone();
        let native = format!(
            "+8004/03/05/09/08 = {}",
            hex_string(&lookup(NoMatch::DoNotCopy).encode().unwrap())
        );
        changes.retain(|l| !l.starts_with("+8004/03/05/09/08 "));
        changes.push(native.as_str());
        changes.push("+8004/03/05/09/0c = 6b");
        let expected = after_run(&base, &changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8004,
                name: "L1".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let set = |draft: &mut SchemaDraft, field: u64, f: &dyn Fn(&mut FieldOptions)| {
            let mut options = FieldOptions::default();
            f(&mut options);
            draft.apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8004,
                field_id: field,
                options: Box::new(options),
            })
        };
        for refused in [
            NoMatch::NextLower,
            NoMatch::NextHigher,
            NoMatch::Constant("none".into()),
        ] {
            let error = set(&mut draft, 9, &|o| {
                o.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup(refused.clone())))
            })
            .unwrap_err();
            assert_eq!(error.code, Some(UNSUPPORTED), "{refused:?}");
        }
        set(&mut draft, 9, &|o| {
            o.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup(NoMatch::DoNotCopy)))
        })
        .unwrap();
        let current = draft.catalogs();
        assert_eq!(
            sorted(&current.fields[&key_of(0x8004)]),
            sorted(expected.at(&[&[0x80, 0x04], &[FIELDS]]).unwrap()),
            "the field catalog"
        );
        let entry =
            |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x04]]).unwrap());
        assert_eq!(
            entry(current.occurrences.as_ref().unwrap()),
            entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
            "the dependency entry"
        );
        // Another field of the table still takes options that rebuild
        // the lists (a creation value on Type).
        set(&mut draft, 1, &|o| {
            o.auto_enter.value = AutoEnterValue::Creation(crate::field_options::EntryValue::Name)
        })
        .unwrap();
        let before = draft.catalogs().clone();
        // The lookup stays one OpenMaker's record rules perform: no
        // Prohibit modification on it, no auto-entered value on its match
        // field, no repetitions on a new destination.
        let lookup_set = |o: &mut FieldOptions| {
            o.auto_enter.value = AutoEnterValue::LookedUp(Some(lookup(NoMatch::DoNotCopy)))
        };
        let error = set(&mut draft, 9, &|o| {
            lookup_set(o);
            o.auto_enter.prohibit_modification = true;
        })
        .unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED));
        let error = set(&mut draft, 5, &|o| {
            o.auto_enter.value = AutoEnterValue::Data("x".into());
            o.storage.indexing = Indexing::All;
        })
        .unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED), "{}", error.message);
        assert!(error.message.contains("of “L1”"), "{}", error.message);
        assert_eq!(draft.catalogs(), &before);
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8004,
                name: "L2".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let error = set(&mut draft, 0x0a, &|o| {
            lookup_set(o);
            o.storage.repetitions = 2;
        })
        .unwrap_err();
        assert_eq!(error.code, Some(UNSUPPORTED));
        draft
            .apply(&SchemaChange::DeleteField {
                table_id: 0x8004,
                field_id: 0x0a,
            })
            .unwrap();
        let before = draft.catalogs().clone();
        // What the lookup reads stays: the relationship it follows, the
        // occurrence it reads and the field it copies.
        for change in [
            SchemaChange::DeleteRelationship { relationship_id: 2 },
            SchemaChange::DeleteOccurrence {
                occurrence_id: 0xd00002,
            },
            SchemaChange::DeleteField {
                table_id: 0x8002,
                field_id: 5,
            },
        ] {
            let error = draft.apply(&change).unwrap_err();
            assert_eq!(error.code, Some(UNSUPPORTED), "{change:?}");
            assert_eq!(draft.catalogs(), &before);
        }
    }

    /// The formulas over a related field FileMaker Server's engine gave
    /// Phone Numbers in runs a15 and v28 (schema-path.md 5.13: an
    /// auto-entered `Contacts::Company & "!"` and a validation by
    /// `Contacts::Company ≠ ""`, both from Phone Numbers through its
    /// ForeignKey), made through Create and Options for Field: the field
    /// catalog and the dependency entry come out key for key as the engine
    /// wrote them, with the `0c` FileMaker Pro writes for the Auto-Enter
    /// tab. An aggregate of related values (#1036) comes out as the
    /// engine's a05 shows: as the plain read (a04) but for its input's
    /// type. A related read OpenMaker's record rules could not make is
    /// refused: through two relationships (a12's `Addresses::City`).
    #[test]
    fn related_formulas_are_written_as_the_engine_writes_them() {
        use crate::field_options::{AutoCalculation, OptionFormula, ValidationCalculation};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let phones = 0xd00004;
        let formula = |text: &str| OptionFormula::new(text, phones);
        let made: Vec<(&str, &str, FieldOptions)> = vec![
            ("a15", "A1", {
                let mut options = FieldOptions::default();
                options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
                    replace_existing: true,
                    ..AutoCalculation::new(formula("Contacts::Company & \"!\""))
                });
                options
            }),
            ("v28", "V1", {
                let mut options = FieldOptions::default();
                options.validation.calculation = Some(ValidationCalculation {
                    formula: formula("Contacts::Company ≠ \"\""),
                    only_if_modified: true,
                });
                options
            }),
        ];
        let new_field = |name: &str| SchemaChange::CreateField {
            table_id: 0x8004,
            name: name.into(),
            data_type: FieldKind::Text,
            comment: String::new(),
        };
        for (run, name, options) in made {
            let (_, changes) = runs.iter().find(|(r, _)| r == run).unwrap();
            let mut changes = changes.clone();
            changes.push("+8004/03/05/09/0c = 6b");
            let expected = after_run(&base, &changes);
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            draft.apply(&new_field(name)).unwrap();
            draft
                .apply(&SchemaChange::SetFieldOptions {
                    table_id: 0x8004,
                    field_id: 9,
                    options: Box::new(options),
                })
                .unwrap_or_else(|e| panic!("{run}: {}", e.message));
            let current = draft.catalogs();
            assert_eq!(
                sorted(&current.fields[&key_of(0x8004)]),
                sorted(expected.at(&[&[0x80, 0x04], &[FIELDS]]).unwrap()),
                "{run}: the field catalog"
            );
            let entry =
                |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x04]]).unwrap());
            assert_eq!(
                entry(current.occurrences.as_ref().unwrap()),
                entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
                "{run}: the dependency entry"
            );
        }
        // The engine's a05 (Contacts' `Count ( Phone Numbers::Number )`)
        // differs from its plain read a04 over the same relationship only in
        // the related input's type, `00` inside an aggregate (5.13), with
        // the same dependency entry. a05's relationship matches an
        // auto-entered key on Contacts' side, which OpenMaker's rules
        // refuse, so the aggregate is made from Phone Numbers against a15's
        // run with that type: everything but the formula's own bytes.
        let (_, changes) = runs.iter().find(|(r, _)| r == "a15").unwrap();
        let typed = "+8004/03/01/0e/fff0/09/0003d000020105 = 01";
        assert!(changes.contains(&typed));
        let mut changes: Vec<&str> = changes
            .iter()
            .map(|line| match *line {
                l if l == typed => "+8004/03/01/0e/fff0/09/0003d000020105 = 00",
                l => l,
            })
            .collect();
        changes.push("+8004/03/05/09/0c = 6b");
        let expected = after_run(&base, &changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft.apply(&new_field("A1")).unwrap();
        let mut options = FieldOptions::default();
        options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
            replace_existing: true,
            ..AutoCalculation::new(formula("Count ( Contacts::Company )"))
        });
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8004,
                field_id: 9,
                options: Box::new(options),
            })
            .unwrap_or_else(|e| panic!("Count: {}", e.message));
        let current = draft.catalogs();
        assert_eq!(
            sorted(current.fields[&key_of(0x8004)].at(&[&[1]]).unwrap()),
            sorted(expected.at(&[&[0x80, 0x04], &[FIELDS], &[1]]).unwrap()),
            "Count: the field index and its inputs"
        );
        let entry =
            |o: &OwnedNode| sorted(o.at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x04]]).unwrap());
        assert_eq!(
            entry(current.occurrences.as_ref().unwrap()),
            entry(expected.at(&[&[3], &[OCCURRENCES]]).unwrap()),
            "Count: the dependency entry"
        );
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        draft.apply(&new_field("A1")).unwrap();
        let mut options = FieldOptions::default();
        options.auto_enter.value =
            AutoEnterValue::Calculated(AutoCalculation::new(formula("Addresses::City")));
        let refused = draft.apply(&SchemaChange::SetFieldOptions {
            table_id: 0x8004,
            field_id: 9,
            options: Box::new(options),
        });
        assert_eq!(refused.unwrap_err().code, Some(UNSUPPORTED));
    }

    /// Run a12's auto-entered `Addresses::City` in Phone Numbers reads
    /// through Contacts (schema-path.md 5.13): both relationships on its
    /// path and the occurrence between stay, and once the calculated value
    /// is removed its `fb` node goes with it, and with it the triggers the
    /// rebuild derived.
    #[test]
    fn a_related_formula_keeps_its_whole_path() {
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let (_, changes) = runs.iter().find(|(run, _)| run == "a12").unwrap();
        let after = after_run(&base, changes);
        let mut draft = SchemaDraft::new(engine_catalogs(&after));
        let before = draft.catalogs().clone();
        for change in [
            SchemaChange::DeleteRelationship { relationship_id: 1 },
            SchemaChange::DeleteRelationship { relationship_id: 2 },
            SchemaChange::DeleteOccurrence {
                occurrence_id: 0xd00002,
            },
        ] {
            let error = draft.apply(&change).unwrap_err();
            assert_eq!(error.code, Some(UNSUPPORTED), "{change:?}");
            assert_eq!(draft.catalogs(), &before);
        }
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8004,
                field_id: 9,
                options: Box::new(FieldOptions::default()),
            })
            .unwrap();
        let fields = &draft.catalogs().fields[&key_of(0x8004)];
        assert!(fields
            .at(&[&[INDEX], &[DEPENDENCIES], &[RELATED_READS]])
            .is_none());
        assert!(draft.derived_triggers(&key_of(0x8004)).unwrap().is_empty());
        draft
            .apply(&SchemaChange::DeleteRelationship { relationship_id: 1 })
            .unwrap();
    }

    /// Duplicate in the Contacts table of the engine runs' base: PrimaryKey
    /// (an auto-entered `Get ( UUID )`, Not empty and Unique, indexed,
    /// tagged `#^FMI* #_FMI_0`) becomes "PrimaryKey Copy" with the same
    /// definition, formula, `fff0` triggers and `fffc` listing, tagged
    /// `#^FMI_`, and again "PrimaryKey Copy2"; the calculation Initial
    /// becomes "Initial Copy" with its formula and triggers (5.14).
    #[test]
    fn duplicated_fields_keep_their_definitions_and_triggers() {
        let (base, _) = engine_runs(include_str!(
            "../tests/fixtures/contacts-option-formulas.txt"
        ));
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        for field in [0x09, 0x09, 0x08] {
            draft
                .apply(&SchemaChange::DuplicateField {
                    table_id: 0x8002,
                    field_id: field,
                })
                .unwrap_or_else(|e| panic!("{field:02x}: {}", e.message));
        }
        let fields = &draft.catalogs().fields[&key_of(0x8002)];
        let name = |key: u8| fields.at(&[&[MEMBERS], &[key]]).and_then(member_name);
        assert_eq!(name(0x0e).as_deref(), Some("PrimaryKey Copy"));
        assert_eq!(name(0x0f).as_deref(), Some("PrimaryKey Copy2"));
        assert_eq!(name(0x10).as_deref(), Some("Initial Copy"));
        let tags = |key: u8| fields.at(&[&[MEMBERS], &[key], &[TAGS]]).and_then(text_of);
        assert_eq!(tags(0x09).as_deref(), Some("#^FMI* #_FMI_0 "));
        assert_eq!(tags(0x0e).as_deref(), Some("#^FMI_ "));
        let part = |key: u8, path: &[&[u8]]| {
            let key = [key];
            let mut at: Vec<&[u8]> = vec![&[MEMBERS], &key];
            at.extend_from_slice(path);
            fields.at(&at).cloned().map(|n| sorted(&n))
        };
        let triggers = |key: u8| {
            fields
                .at(&[&[INDEX], &[DEPENDENCIES], FIELD_DEPENDENCIES, &[key]])
                .map(|n| sorted(&OwnedNode::list(Vec::new(), n.children.clone())))
        };
        for (original, copy) in [(0x09, 0x0e), (0x09, 0x0f), (0x08, 0x10)] {
            for path in [&[&[DEFINITION][..]][..], &[&[FORMULA][..]][..]] {
                assert_eq!(
                    part(copy, path),
                    part(original, path),
                    "{copy:02x} {path:?}"
                );
            }
            assert_eq!(triggers(copy), triggers(original), "{copy:02x} triggers");
        }
        let entry = draft
            .catalogs()
            .occurrences
            .as_ref()
            .unwrap()
            .at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]])
            .unwrap();
        assert!(
            dependencies::listing(entry, &[0x0e]).0,
            "validated as PrimaryKey is"
        );
    }

    /// A lookup field's copy keeps its definition `08` byte for byte, the
    /// padding FileMaker's files carry included (5.14): run l01's engine
    /// wrote `5e` and `00 00 00 00` where OpenMaker's encoder writes `00`
    /// and FileMaker Pro 26's `00 11 01 00`. The copy reads through the
    /// same relationship as the original, with its own `fb` catalog node
    /// (5.13).
    #[test]
    fn a_duplicated_lookup_keeps_its_definition_as_it_is() {
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let (_, changes) = runs.iter().find(|(run, _)| run == "l01").unwrap();
        let mut draft = SchemaDraft::new(engine_catalogs(&after_run(&base, changes)));
        draft
            .apply(&SchemaChange::DuplicateField {
                table_id: 0x8004,
                field_id: 0x09,
            })
            .unwrap_or_else(|e| panic!("{}", e.message));
        let fields = &draft.catalogs().fields[&key_of(0x8004)];
        let lookup = |key: u8| {
            fields
                .at(&[&[MEMBERS], &[key], &[LOOKUP]])
                .and_then(|n| n.value.clone())
        };
        let original = hex("00000403d000025e02010500000000010403d0000400");
        assert_eq!(lookup(0x09), Some(original.clone()));
        assert_eq!(lookup(0x0a), Some(original));
        let reads = |key: u8| {
            fields
                .at(&[
                    &[INDEX],
                    &[DEPENDENCIES],
                    &[RELATED_READS],
                    &[key],
                    &[0xd0, 0x00, 0x04],
                    &[0xd0, 0x00, 0x02],
                ])
                .is_some()
        };
        assert!(reads(0x09) && reads(0x0a));
    }

    /// A relationship's two sort orders as FileMaker's engine stored them
    /// for the design probe's relationship 100 (FMUpgradeTool, from the
    /// Tasks template; file-metadata.md 4.1): the right occurrence's
    /// records by N descending then Task, the left's by Status in value
    /// list 2's order; both sorted, creation on the right, deletion on
    /// both sides.
    #[test]
    fn relationship_sort_orders_are_written_as_the_engine_writes_them() {
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let entry = |occurrence: u64, field: u64, descending, value_list| SortEntry {
            field: SpecField {
                occurrence: Some(occurrence),
                field,
                repetition: 1,
            },
            summary: None,
            value_list,
            descending,
            locale: 0,
        };
        let spec = |fields| SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields,
        };
        let (tasks, p_b) = (0xd00002, 0xd0000d);
        let left = spec(vec![entry(tasks, 4, false, Some(2))]);
        let right = spec(vec![
            entry(p_b, 100, true, None),
            entry(p_b, 1, false, None),
        ]);
        let definition = SchemaDraft::definition(
            &key_of(tasks),
            &key_of(p_b),
            0xd3,
            None,
            [Some(&left), Some(&right)],
        )
        .unwrap();
        // The engine's bytes (dump of the probe's `[3, fb, 5, 64, 02]`).
        assert_eq!(
            definition,
            hex(concat!(
                "0403d000020403d0000d000000020403d0000d0201640001020100020100000100",
                "01000000000403d0000d0201010001020100020100000100000000000000000001",
                "0403d00002020104000102010002010000010201020000000000000000d3"
            ))
        );
        // Changing one side's order keeps the other's as stored.
        let first = 10 + SortSpec::read(&definition[10..]).unwrap().1;
        let kept = SchemaDraft::definition(
            &key_of(tasks),
            &key_of(p_b),
            0x93,
            Some(&definition),
            [Some(&spec(vec![])), None],
        )
        .unwrap();
        assert_eq!(kept[..first], definition[..first]);
        assert_eq!(&kept[first..], hex("0000000000000093").as_slice());
    }

    /// Edit Relationship OK'd with no change on a relationship that sorts
    /// its records and that a lookup follows (Assets' relationship 1 in the
    /// starter survey): the orders it sends back are the stored ones, so the
    /// definition keeps its bytes and the change is accepted. Another order
    /// is still refused, since the lookup's triggers follow it.
    #[test]
    fn an_unchanged_sorted_relationship_a_lookup_follows_is_accepted() {
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let (base, _) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        let by = |field| SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00004),
                    field,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        };
        // Relationship 2, Contacts::PrimaryKey = Phone Numbers::ForeignKey,
        // with Phone Numbers' records sorted by `field`.
        let sorted_by = |draft: &mut SchemaDraft, field| {
            draft.apply(&SchemaChange::ChangeRelationship {
                relationship_id: 2,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    operator: 0,
                    right_field: 5,
                }],
                left: crate::RelationshipSide::default(),
                right: crate::RelationshipSide {
                    allow_creation: true,
                    sort: true,
                    order: Some(by(field)),
                    ..crate::RelationshipSide::default()
                },
            })
        };
        sorted_by(&mut draft, 2).unwrap();
        // A lookup in Contacts that follows it: Phone Numbers::Number.
        draft
            .apply(&SchemaChange::CreateField {
                table_id: 0x8002,
                name: "L".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        let mut options = FieldOptions::default();
        options.auto_enter.value = AutoEnterValue::LookedUp(Some(Lookup {
            from: 0xd00002,
            to: 0xd00004,
            copied: 2,
            copy_empty: false,
            no_match: crate::field_options::NoMatch::DoNotCopy,
        }));
        draft
            .apply(&SchemaChange::SetFieldOptions {
                table_id: 0x8002,
                field_id: 0x0e,
                options: Box::new(options),
            })
            .unwrap_or_else(|e| panic!("lookup: {}", e.message));
        let definition = |draft: &SchemaDraft| {
            draft
                .current
                .relationships
                .as_ref()
                .and_then(|c| c.at(&[&[MEMBERS], &key_of(2), &[DEFINITION]]))
                .and_then(|n| n.assembled())
                .map(|b| b.into_owned())
        };
        let before = definition(&draft);
        sorted_by(&mut draft, 2).unwrap_or_else(|e| panic!("unchanged: {}", e.message));
        assert_eq!(definition(&draft), before);
        let refused = sorted_by(&mut draft, 1).unwrap_err();
        assert!(
            refused
                .message
                .contains("a lookup or an auto-entered formula follows"),
            "{}",
            refused.message
        );
        assert_eq!(definition(&draft), before);
    }

    /// An order that only reads as the stored one is a change (#945's
    /// review): a stored value list key `02 01 00` decodes as no value
    /// list, as `00` does, so the order Edit Relationship sends with none
    /// reads the same; the definition then takes the order sent rather than
    /// keeping the stored bytes.
    #[test]
    fn an_order_that_only_reads_as_the_stored_one_is_written() {
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let (base, _) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-options.txt"
        ));
        let mut draft = SchemaDraft::new(engine_catalogs(&base));
        let order = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00004),
                    field: 2,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        };
        let change = |draft: &mut SchemaDraft| {
            draft.apply(&SchemaChange::ChangeRelationship {
                relationship_id: 2,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    operator: 0,
                    right_field: 5,
                }],
                left: crate::RelationshipSide::default(),
                right: crate::RelationshipSide {
                    allow_creation: true,
                    sort: true,
                    order: Some(order.clone()),
                    ..crate::RelationshipSide::default()
                },
            })
        };
        change(&mut draft).unwrap();
        // The stored order with its value list key as `02 01 00`: the key
        // sits before the entry's order byte and locale (5 bytes).
        let encoded = order.encode();
        let at = encoded.len() - 6;
        assert_eq!(encoded[at], 0x00);
        let sentinel = [&encoded[..at], &[0x02, 0x01, 0x00][..], &encoded[at + 1..]].concat();
        assert_eq!(SortSpec::decode(&sentinel), Some(order.clone()));
        let path: [&[u8]; 3] = [&[MEMBERS], &key_of(2), &[DEFINITION]];
        let definition = |draft: &SchemaDraft| {
            draft
                .current
                .relationships
                .as_ref()
                .and_then(|c| c.at(&path))
                .and_then(|n| n.assembled())
                .map(|b| b.into_owned())
                .unwrap()
        };
        let written = definition(&draft);
        let start = (0..written.len())
            .find(|&i| written[i..].starts_with(&encoded))
            .unwrap();
        let patched = [
            &written[..start],
            &sentinel[..],
            &written[start + encoded.len()..],
        ]
        .concat();
        draft
            .catalog(CatalogPath::Relationships)
            .node
            .at_mut(&[&[MEMBERS], &key_of(2)])
            .unwrap()
            .set_child(OwnedNode::leaf(vec![DEFINITION], patched.clone()));
        assert_eq!(definition(&draft), patched);
        change(&mut draft).unwrap();
        assert_eq!(
            definition(&draft),
            written,
            "the order sent, not the stored bytes"
        );
    }

    /// The calculations over related fields and aggregates FileMaker
    /// Server's engine added to the Contacts table (schema-path.md 5.12,
    /// runs r01-r15), made through the draft from the same formulas: the
    /// field catalog and the dependency entry come out key for key as the
    /// engine wrote them, unstored where the engine made them unstored.
    #[test]
    fn related_calculations_are_written_as_the_engine_writes_them() {
        use crate::calc_field::{CalcStorage, CalculationField};
        let (base, runs) = engine_runs(include_str!(
            "../tests/fixtures/contacts-related-calculations.txt"
        ));
        let calc = |formula: &str, result| CalculationField {
            result,
            ..CalculationField::new(formula, 0xd00002)
        };
        let text = FieldKind::Text;
        let number = FieldKind::Number;
        let made: Vec<Vec<CalculationField>> = vec![
            vec![calc("Phone Numbers::Number", text)],
            vec![calc("Count ( Phone Numbers::Number )", number)],
            vec![calc("List ( Addresses::City )", text)],
            vec![calc("Sum ( Addresses::Postal Code )", number)],
            vec![calc("Title & \" \" & Phone Numbers::Type", text)],
            vec![
                calc("Count ( Phone Numbers::Number )", number),
                calc("C1 + 1", number),
            ],
            vec![calc("Phone Numbers::Number & Addresses::City", text)],
            vec![calc("IsEmpty ( Phone Numbers::Number )", number)],
            vec![calc("Count ( Phone Numbers::Number ) & Title", text)],
            vec![calc(
                "Sum ( Addresses::Postal Code ) + Count ( Addresses::City )",
                number,
            )],
            vec![CalculationField {
                skip_when_empty: false,
                ..calc("Phone Numbers::Number", text)
            }],
            vec![CalculationField {
                storage: CalcStorage {
                    unstored: true,
                    ..CalcStorage::default()
                },
                ..calc("Phone Numbers::Number", text)
            }],
            vec![calc("Sum ( Title ) & First Name", text)],
            vec![calc("Count ( Title )", number)],
            vec![
                calc("Title & \"1\"", text),
                calc("C1 & Phone Numbers::Number", text),
            ],
        ];
        assert_eq!(runs.len(), made.len());
        for ((run, changes), fields) in runs.iter().zip(made) {
            let mut draft = SchemaDraft::new(engine_catalogs(&base));
            for (n, calculation) in fields.into_iter().enumerate() {
                draft
                    .apply(&SchemaChange::CreateCalculationField {
                        table_id: 0x8002,
                        name: format!("C{}", n + 1),
                        comment: String::new(),
                        calculation,
                    })
                    .unwrap_or_else(|e| panic!("{run}: {}", e.message));
            }
            assert_engine_wrote(&draft, &after_run(&base, changes), run);
            if run == "r06" {
                related_edits_keep_the_calculations_whole(draft);
            }
        }
    }

    /// On r06 (`C1 = Count ( Phone Numbers::Number )`, `C2 = C1 + 1`): a
    /// relationship edit keeps the related inputs' entries; the related
    /// field, its occurrence and its table cannot be deleted while read;
    /// and when C1 reads a field as a value, C2 is no longer pure.
    fn related_edits_keep_the_calculations_whole(mut draft: SchemaDraft) {
        use crate::calc_field::CalculationField;
        let entry = |d: &SchemaDraft| {
            sorted(
                d.catalogs()
                    .occurrences
                    .as_ref()
                    .unwrap()
                    .at(&[&[INDEX], &[DEPENDENCIES], &[0x80, 0x02]])
                    .unwrap(),
            )
        };
        let before = entry(&draft);
        let side = crate::RelationshipSide::from_options(0, false);
        draft
            .apply(&SchemaChange::ChangeRelationship {
                relationship_id: 2,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    operator: 1,
                    right_field: 5,
                }],
                left: side.clone(),
                right: side,
            })
            .unwrap();
        assert_eq!(entry(&draft), before, "the related inputs' entries");
        for change in [
            SchemaChange::DeleteField {
                table_id: 0x8004,
                field_id: 2,
            },
            SchemaChange::DeleteOccurrence {
                occurrence_id: 0xd00004,
            },
            SchemaChange::DeleteTable {
                table_id: 0x8004,
                remove_occurrences: false,
            },
        ] {
            assert_eq!(
                draft.apply(&change).unwrap_err().code,
                Some(UNSUPPORTED),
                "{change:?}"
            );
        }
        let pure = |d: &SchemaDraft, field: u8| {
            let definition = d.catalogs().fields[&key_of(0x8002)]
                .at(&[&[MEMBERS], &[field], &[DEFINITION]])
                .and_then(|n| n.value.clone())
                .unwrap();
            u32::from_be_bytes(definition[OPTIONS].try_into().unwrap()) & 0x8_0000 != 0
        };
        assert!(pure(&draft, 0x0f), "C2 over a pure C1");
        // A calculation of Phone Numbers reading C2 through Contacts.
        draft
            .apply(&SchemaChange::CreateCalculationField {
                table_id: 0x8004,
                name: "Owner Count".into(),
                comment: String::new(),
                calculation: CalculationField {
                    result: FieldKind::Number,
                    ..CalculationField::new("Contacts::C2", 0xd00004)
                },
            })
            .unwrap();
        let far = |d: &SchemaDraft| {
            let catalog = &d.catalogs().fields[&key_of(0x8004)];
            let member = members(catalog)
                .iter()
                .find(|m| member_name(m).as_deref() == Some("Owner Count"))
                .unwrap();
            let definition = member.child(&[DEFINITION]).unwrap().value.clone().unwrap();
            u32::from_be_bytes(definition[OPTIONS].try_into().unwrap()) & 0x8_0000 != 0
        };
        assert!(far(&draft), "a related reader of a pure C2");
        // C2 itself stops being pure: its reader in Phone Numbers follows.
        draft
            .apply(&SchemaChange::ChangeCalculation {
                table_id: 0x8002,
                field_id: 0x0f,
                calculation: CalculationField {
                    result: FieldKind::Number,
                    ..CalculationField::new("C1 + Length ( Title )", 0xd00002)
                },
            })
            .unwrap();
        assert!(!pure(&draft, 0x0f) && !far(&draft), "C2's related reader");
        draft
            .apply(&SchemaChange::ChangeCalculation {
                table_id: 0x8002,
                field_id: 0x0e,
                calculation: CalculationField {
                    result: FieldKind::Number,
                    ..CalculationField::new("Length ( Phone Numbers::Number )", 0xd00002)
                },
            })
            .unwrap();
        assert!(!pure(&draft, 0x0e) && !pure(&draft, 0x0f));
    }

    fn relate(left: u64, right: u64, field: u64, operator: u8) -> SchemaChange {
        SchemaChange::CreateRelationship {
            left_occurrence: left,
            right_occurrence: right,
            matches: vec![RelationshipMatch {
                left_field: field,
                operator,
                right_field: field,
            }],
            left: crate::RelationshipSide {
                allow_creation: true,
                ..Default::default()
            },
            right: Default::default(),
        }
    }

    #[test]
    fn a_new_relationship_links_routes_lists_and_reach_as_filemaker_does() {
        // E20: a second occurrence of Contacts, related to the first on a
        // field of Contacts with "Allow creation" on the left.
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::CreateOccurrence {
                table_id: 0x8002,
                name: "Contacts 2".into(),
            })
            .unwrap();
        draft.apply(&relate(0xd00002, 0xd00006, 7, 0)).unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let relationships = &plan.catalogs[0].1;
        assert_eq!(plan.catalogs[0].0, CatalogPath::Relationships);
        assert_eq!(
            value(relationships, "05/04/02"),
            Some(&hex("0403d000020403d00006000000000000000000000020")[..])
        );
        assert_eq!(
            value(relationships, "05/04/03/01"),
            Some(&hex("00020107020107")[..])
        );
        assert_eq!(value(relationships, "01/00"), Some(&hex("0104")[..]));
        assert_eq!(value(relationships, "01/04"), Some(&hex("00000004")[..]));
        let occurrences = &plan.catalogs[1].1;
        assert_eq!(
            value(occurrences, "05/d00002/fb/d00006"),
            Some(&hex("0104")[..])
        );
        assert_eq!(
            value(occurrences, "05/d00006/fb/d00002"),
            Some(&hex("0104")[..])
        );
        for (from, to, via) in [
            ("d00002", "d00006", "0104"),
            ("d00003", "d00006", "0101"),
            ("d00004", "d00006", "0102"),
            ("d00005", "d00006", "0103"),
            ("d00006", "d00002", "0104"),
            ("d00006", "d00005", "0104"),
        ] {
            assert_eq!(
                value(occurrences, &format!("01/08/{from}/{to}")),
                Some(&hex(via)[..]),
                "{from} to {to}"
            );
        }
        for reach in [
            "01/0e/8002/00/07/d00002/d00006",
            "01/0e/8002/00/07/d00006/d00002",
            "01/0e/8002/00/07/d00006/d00005",
            "01/0e/8003/00/0a/d00003/d00006",
            "01/0e/8005/00/05/d00005/d00006",
            // The key field of the first relationships reaches no further
            // than before from where it starts.
            "01/0e/8002/00/09/d00002/d00003",
        ] {
            assert!(has(occurrences, reach), "{reach}");
        }
        assert!(!has(occurrences, "01/0e/8002/00/09/d00002/d00006"));
        assert_eq!(
            value(occurrences, "01/0e/8002/07/00"),
            Some(&hex("0321020107")[..])
        );
        // Reach never runs back to where it starts, through either side.
        for own in [
            "01/0e/8003/00/0a/d00003/d00003",
            "01/0e/8004/00/05/d00004/d00004",
            "01/0e/8005/00/05/d00005/d00005",
            "01/0e/8002/00/07/d00002/d00002",
            "01/0e/8002/00/07/d00006/d00006",
        ] {
            assert!(!has(occurrences, own), "{own}");
        }

        // FileMaker allows one path between two occurrences.
        let error = draft
            .apply(&SchemaChange::CreateRelationship {
                left_occurrence: 0xd00003,
                right_occurrence: 0xd00006,
                matches: vec![RelationshipMatch {
                    left_field: 0x0a,
                    operator: 1,
                    right_field: 7,
                }],
                left: Default::default(),
                right: Default::default(),
            })
            .unwrap_err();
        assert!(
            error.message.contains("more than one relational path"),
            "{error}"
        );
    }

    /// The graph's Duplicate (#430): Contacts and Addresses copied as
    /// "Contacts 2" and "Addresses 2", their definitions kept with their
    /// boxes moved 20 points right and down, and the relationship between
    /// them made between the copies with its definition (options, sort
    /// orders) and pair; those to occurrences not duplicated are not.
    #[test]
    fn duplicated_occurrences_keep_the_relationship_between_them() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::DuplicateOccurrences {
                occurrence_ids: vec![0xd00002, 0xd00003],
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let catalog = |which: CatalogPath| {
            plan.catalogs
                .iter()
                .find(|(path, _)| *path == which)
                .map(|(_, node)| node)
                .unwrap()
        };
        let occurrences = catalog(CatalogPath::Occurrences);
        for (key, name, definition) in [
            (
                "d00006",
                "Contacts 2",
                "02010003028002422000004220000043030000436200000000\
                 00b600000000787878ff",
            ),
            (
                "d00007",
                "Addresses 2",
                "0201000302800343660000422000004303000042ec00000000\
                 004a00000001787878ff",
            ),
        ] {
            assert_eq!(
                value(occurrences, &format!("05/{key}/02")),
                Some(&hex(definition)[..]),
                "{name}"
            );
            let member = occurrences.at(&[&[MEMBERS], &hex(key)]).unwrap();
            assert_eq!(member_name(member).as_deref(), Some(name));
        }
        let relationships = catalog(CatalogPath::Relationships);
        assert_eq!(
            value(relationships, "05/04/02"),
            Some(&hex("0403d000060403d00007000000000000000000000010")[..])
        );
        assert_eq!(
            value(relationships, "05/04/03/01"),
            Some(&hex("0002010902010a")[..])
        );
        assert_eq!(members(relationships).len(), 4);
        assert_eq!(
            value(occurrences, "05/d00006/fb/d00007"),
            Some(&hex("0104")[..])
        );

        // A sorted relationship: Contacts' records sorted by its field 9.
        // The copy's order sorts the copy's (d00006), not the original's.
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let order = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00002),
                    field: 9,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        };
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::ChangeRelationship {
                relationship_id: 1,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    right_field: 0x0a,
                    operator: 0,
                }],
                left: crate::schema::RelationshipSide {
                    sort: true,
                    order: Some(order),
                    ..Default::default()
                },
                right: Default::default(),
            })
            .unwrap();
        draft
            .apply(&SchemaChange::DuplicateOccurrences {
                occurrence_ids: vec![0xd00002, 0xd00003],
            })
            .unwrap();
        let copy = draft
            .current
            .relationships
            .as_ref()
            .and_then(|c| c.at(&[&[MEMBERS], &[4]]))
            .and_then(|m| m.child(&[DEFINITION]))
            .and_then(|d| d.assembled())
            .unwrap()
            .into_owned();
        let [left, _] = stored_orders(&copy).unwrap();
        let (left, _) = SortSpec::read(left).unwrap();
        assert_eq!(left.fields[0].field.occurrence, Some(0xd00006));
    }

    #[test]
    fn a_deleted_relationship_leaves_its_occurrences_unconnected() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::DeleteRelationship { relationship_id: 3 })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let (relationships, occurrences) = (&plan.catalogs[0].1, &plan.catalogs[1].1);
        assert!(!has(relationships, "05/03"));
        assert_eq!(value(relationships, "01/04"), Some(&hex("00000002")[..]));
        // As E8 left them, but the occurrence stays.
        assert!(has(occurrences, "05/d00005"));
        assert!(!has(occurrences, "05/d00005/fb"));
        assert!(!has(occurrences, "05/d00002/fb/d00005"));
        assert!(!has(occurrences, "01/08/d00005"));
        assert!(!has(occurrences, "01/08/d00003/d00005"));
        assert!(!has(occurrences, "01/0e/8002/00/09/d00002/d00005"));
        assert!(!has(occurrences, "01/0e/8005/00"));
        assert!(!has(occurrences, "01/0e/8005/05/00"));
        // Email Addresses' own evaluation lists stay.
        assert!(has(occurrences, "01/0e/8005/ff00/02"));
    }

    #[test]
    fn options_change_alone_and_boxes_move_and_colour() {
        let mut draft = SchemaDraft::new(contacts());
        draft
            .apply(&SchemaChange::ChangeRelationship {
                relationship_id: 1,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    operator: 0,
                    right_field: 0x0a,
                }],
                left: Default::default(),
                right: crate::RelationshipSide {
                    allow_creation: true,
                    delete_related: true,
                    sort: false,
                    order: None,
                },
            })
            .unwrap();
        draft
            .apply(&SchemaChange::ArrangeOccurrence {
                occurrence_id: 0xd00003,
                left: 300,
                top: 40,
                width: 150,
                height: 200,
            })
            .unwrap();
        draft
            .apply(&SchemaChange::ColorOccurrence {
                occurrence_id: 0xd00003,
                color: [0x99, 0xcc, 0xff, 0xff],
            })
            .unwrap();
        let plan = draft.finish(&stamp(), &mut uuids());
        let (relationships, occurrences) = (&plan.catalogs[0].1, &plan.catalogs[1].1);
        assert_eq!(
            value(relationships, "05/01/02"),
            Some(&hex("0403d000020403d00003000000000000000000000011")[..])
        );
        assert_eq!(value(relationships, "05/01/fc"), Some(&hex("0101")[..]));
        // Routes and reach are as they were.
        assert_eq!(
            value(occurrences, "01/08/d00003/d00004"),
            Some(&hex("0101")[..])
        );
        assert_eq!(
            value(occurrences, "05/d00003/02"),
            Some(
                &hex(
                    "02010003028003 43960000 42200000 43160000 43480000 0000004a 00000001 99ccffff"
                        .replace(' ', "")
                        .as_str()
                )[..]
            )
        );
    }
    #[test]
    fn a_relationship_made_and_deleted_leaves_the_graph_as_it_was() {
        let base = contacts();
        let mut draft = SchemaDraft::new(base.clone());
        draft
            .apply(&SchemaChange::CreateOccurrence {
                table_id: 0x8002,
                name: "Contacts 2".into(),
            })
            .unwrap();
        draft.apply(&relate(0xd00002, 0xd00006, 7, 0)).unwrap();
        draft
            .apply(&SchemaChange::DeleteRelationship { relationship_id: 4 })
            .unwrap();
        let index = |c: &SchemaCatalogs, key: u8| {
            c.occurrences
                .as_ref()
                .unwrap()
                .at(&[&[INDEX], &[key]])
                .cloned()
        };
        for key in [ROUTES, DEPENDENCIES] {
            assert_eq!(index(draft.catalogs(), key), index(&base, key), "{key:02x}");
        }
    }

    #[test]
    fn a_chunked_definition_keeps_its_sort_orders_and_unknown_options() {
        let mut catalogs = contacts();
        // Relationship 1 with long sort orders, stored in chunks as
        // FileMaker stores values over 1000 bytes. The side checkboxes may
        // change their known bits, but the undecoded 0x100 must survive.
        let mut long = hex("0403d000020403d00003");
        long.extend((0..1100).map(|i| (i % 251) as u8));
        long.extend(hex("00000140"));
        catalogs.relationships.as_mut().unwrap().put(
            &[&[MEMBERS], &[1], &[DEFINITION]],
            om_scripts::bytes_node(&[DEFINITION], &long),
        );
        assert!(catalogs
            .relationships
            .as_ref()
            .unwrap()
            .at(&[&[MEMBERS], &[1], &[DEFINITION]])
            .unwrap()
            .value
            .is_none());
        let mut draft = SchemaDraft::new(catalogs);
        draft
            .apply(&SchemaChange::ChangeRelationship {
                relationship_id: 1,
                matches: vec![RelationshipMatch {
                    left_field: 9,
                    operator: 0,
                    right_field: 0x0a,
                }],
                left: Default::default(),
                right: crate::RelationshipSide {
                    allow_creation: true,
                    delete_related: true,
                    sort: false,
                    order: None,
                },
            })
            .unwrap();
        let stored = draft
            .catalogs()
            .relationships
            .as_ref()
            .unwrap()
            .at(&[&[MEMBERS], &[1], &[DEFINITION]])
            .unwrap()
            .assembled()
            .unwrap()
            .into_owned();
        let mut expected = long.clone();
        let at = expected.len() - 4;
        expected[at..].copy_from_slice(&hex("00000111"));
        assert_eq!(stored, expected);
    }
}
