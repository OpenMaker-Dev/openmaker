//! Adding a field to a table's field catalog `[T, 3]` as FileMaker Pro's
//! Manage Database stores one (docs/re/schema-path.md 5.1, 5.5, 5.6;
//! file-metadata.md 1): the field's id from the catalog's last issued id,
//! its 26-byte definition, its name and comment, and in the catalog's index
//! the two name lists, the display order, the member count and the last
//! issued id. Everything else in the catalog stays as it is, unknown keys
//! included. OpenMaker Server's authoring extension adds the UUIDs and the
//! modification stamps (schema-path.md 8.1) and saves the result; clients
//! plan with the same code, so a plan names the edit the host makes.
//!
//! Only normal fields with stored values of the scalar types are made, with
//! FileMaker's defaults: no auto-enter or validation, indexed as needed,
//! one repetition.

use om_hbam::slot::be_uint;
use om_hbam::{serial, OwnedNode};

/// Keys of the field catalog `[T, 3]`: its index and members, and in the
/// index the last issued id, the name list, the display order, the member
/// count and the name list ordered by type (schema-path.md 5.1, 5.5).
const INDEX: u8 = 0x01;
const MEMBERS: u8 = 0x05;
const LAST_ID: u8 = 0x00;
const NAMES: u8 = 0x01;
const ORDER: u8 = 0x03;
const COUNT: u8 = 0x04;
const TYPED_NAMES: u8 = 0x06;
/// Keys of a member: the definition, the comment and the name.
const DEFINITION: u8 = 0x02;
const COMMENT: u8 = 0x03;
const NAME: u8 = 0x10;

/// Definition byte 0 of a normal field.
const NORMAL: u8 = 1;
/// The index language FileMaker gives a field in a table without fields
/// (definition bytes 4 to 7): English.
const ENGLISH: u32 = 21;
/// Definition offsets: the index language and the option word.
const LANGUAGE: std::ops::Range<usize> = 4..8;
const OPTIONS: std::ops::Range<usize> = 8..12;
/// Options that keep a field's values out of its records: global storage,
/// unstored calculation (schema-path.md 5.5).
const NOT_STORED: u32 = 0x1_0000 | 0x2_0000;

/// The longest name list key FileMaker builds (`M_DBNameList::Update`'s
/// 210-byte key, schema-path.md 5.7). What FileMaker does with a longer
/// one is not established, so OpenMaker refuses the name.
pub(crate) const MAX_NAME_KEY: usize = 210;

/// The longest field name and comment, in characters.
pub const MAX_NAME_CHARS: usize = 100;
pub const MAX_COMMENT_CHARS: usize = 10_000;

/// A field's data type, of those this module makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
}

impl ScalarType {
    pub const ALL: [ScalarType; 5] = [
        ScalarType::Text,
        ScalarType::Number,
        ScalarType::Date,
        ScalarType::Time,
        ScalarType::Timestamp,
    ];

    /// Definition byte 1.
    pub fn code(self) -> u8 {
        match self {
            ScalarType::Text => 1,
            ScalarType::Number => 2,
            ScalarType::Date => 3,
            ScalarType::Time => 4,
            ScalarType::Timestamp => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.code() == code)
    }

    pub fn name(self) -> &'static str {
        match self {
            ScalarType::Text => "text",
            ScalarType::Number => "number",
            ScalarType::Date => "date",
            ScalarType::Time => "time",
            ScalarType::Timestamp => "timestamp",
        }
    }
}

/// The field to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewField {
    pub name: String,
    pub data_type: ScalarType,
    /// `None` for no comment.
    pub comment: Option<String>,
}

/// Why a field cannot be added; the catalog is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The name cannot be used, and why.
    Name(String),
    /// The comment cannot be used, and why.
    Comment(String),
    /// The table has a field whose name FileMaker takes for the same one:
    /// its id (as references give it) and name. Names are compared by their
    /// name keys, so case and accents do not tell them apart.
    Exists { field: u64, name: String },
    /// The catalog lacks what the field must be added to.
    Unsupported(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Name(reason) | Refusal::Comment(reason) | Refusal::Unsupported(reason) => {
                f.write_str(reason)
            }
            Refusal::Exists { name, .. } => {
                write!(f, "The table already has a field named “{name}”.")
            }
        }
    }
}

/// A field planned into a catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The field's id as references give it: the big-endian reading of its
    /// key.
    pub field: u64,
    /// Its key `[T, 3, 5, F]`: the serial key after the last issued id.
    pub key: Vec<u8>,
    /// The 26-byte definition.
    pub definition: Vec<u8>,
    /// Its place in the display order.
    pub position: u32,
    /// The catalog `[T, 3]` with the field, without UUIDs or stamps.
    pub catalog: OwnedNode,
    /// What changed in the catalog: each changed key below `[T, 3]` (as
    /// hex segments) and what it holds.
    pub changes: Vec<(String, String)>,
}

/// What OpenMaker tells two names in one catalog apart by: their primary
/// weights (`name`'s text index key without the leading `01`), so neither
/// case nor accents tell them apart. FileMaker's name list keys also hold
/// the accents ([`listed_key`]); whether FileMaker Pro takes `Grosse` and
/// `Größe` for one name is not established, so OpenMaker refuses the
/// second (schema-path.md 5.6). This is not the key a name list stores.
pub fn name_key(name: &str) -> Vec<u8> {
    om_collate::text_key(name)[1..].to_vec()
}

/// The key FileMaker lists `name` under in a catalog's name list
/// `[cat, 1, 01]` (schema-path.md 5.6): the primary, secondary and
/// tertiary levels of FileMaker's key in the catalog language, whatever
/// the file's language. `None` when OpenMaker cannot build it
/// ([`om_collate::Unkeyed`]).
pub fn listed_key(name: &str) -> Option<Vec<u8>> {
    om_collate::name_key(name, om_collate::Levels::Three).ok()
}

/// [`listed_key`], or why OpenMaker cannot build it, to follow "it cannot
/// be a … name:".
pub(crate) fn listed_key_or_why(name: &str) -> Result<Vec<u8>, String> {
    om_collate::name_key(name, om_collate::Levels::Three).map_err(|unkeyed| {
        let (om_collate::Unkeyed::Character(c) | om_collate::Unkeyed::Weight(c)) = unkeyed;
        format!(
            "OpenMaker cannot yet build the key FileMaker lists it under, since FileMaker's \
             weights for “{c}” (U+{:04X}) are not established",
            u32::from(c)
        )
    })
}

/// A field's key in the name list by type `[T, 3, 1, 06]`: its kind, its
/// type shifted left by 2 with bit 0 for global storage (E38), and the
/// primary and secondary levels of its name's key (schema-path.md 5.6).
/// `None` when OpenMaker cannot build it.
pub fn typed_key(kind: u8, data_type: u8, global: bool, name: &str) -> Option<Vec<u8>> {
    let mut key = vec![kind, data_type << 2 | u8::from(global)];
    key.extend(om_collate::name_key(name, om_collate::Levels::Two).ok()?);
    Some(key)
}

/// Check a new field's name: at most [`MAX_NAME_CHARS`] characters, no
/// control characters, written in a formula as itself (no `${…}`: no
/// operators or other delimiters, no leading digit, period or `$`, no
/// space at either end, no `and`/`or`/`xor`/`not` word, not a function
/// without arguments), and name list keys OpenMaker can build and
/// FileMaker can store. Returns its key in the name list ([`listed_key`]).
pub fn check_name(name: &str) -> Result<Vec<u8>, Refusal> {
    check_name_as(name, false)
}

/// [`check_name`], but a name calculations would write as `${…}` passes
/// when `quoted` says so: a name FileMaker gives, not one typed.
pub fn check_name_as(name: &str, quoted: bool) -> Result<Vec<u8>, Refusal> {
    let refuse = |why: &str| {
        Err(Refusal::Name(format!(
            "“{name}” cannot be a field name: {why}."
        )))
    };
    if name.is_empty() {
        return Err(Refusal::Name("A field needs a name.".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return refuse(&format!("names hold at most {MAX_NAME_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return refuse("it holds a control character or line break");
    }
    if !quoted && om_formula::name_needs_quotes(name) {
        return refuse(
            "calculations could only name it as ${…}, since it holds an operator or other \
             delimiter, starts with a digit, a period or $, has a space at either end, holds \
             and, or, xor or not as a word, or names a function",
        );
    }
    if name_key(name).is_empty() {
        return refuse("it has no character FileMaker tells names apart by");
    }
    let listed = match listed_key_or_why(name) {
        Ok(key) => key,
        Err(why) => return refuse(&why),
    };
    // A field's key in the list by type is at most one byte longer.
    if listed.len() + 1 > MAX_NAME_KEY {
        return refuse("its name key is longer than FileMaker stores");
    }
    Ok(listed)
}

fn check_comment(comment: &str) -> Result<(), Refusal> {
    if comment.chars().count() > MAX_COMMENT_CHARS {
        return Err(Refusal::Comment(format!(
            "A comment holds at most {MAX_COMMENT_CHARS} characters."
        )));
    }
    if comment.contains('\0') {
        return Err(Refusal::Comment(
            "A comment cannot hold a NUL character.".into(),
        ));
    }
    Ok(())
}

/// The 26-byte definition of a normal field of `data_type` with stored
/// values and FileMaker's defaults, indexed in `language` (schema-path.md
/// 5.5, "plain text, not indexed"): no options or validation, serial
/// increment 1, no character limit, one repetition.
pub fn definition(data_type: ScalarType, language: u32) -> Vec<u8> {
    let mut out = vec![0; 26];
    out[0] = NORMAL;
    out[1] = data_type.code();
    out[LANGUAGE].copy_from_slice(&language.to_be_bytes());
    out[16..20].copy_from_slice(&1u32.to_be_bytes());
    out[24..26].copy_from_slice(&1u16.to_be_bytes());
    out
}

/// A member's name, decoded.
pub fn member_name(member: &OwnedNode) -> Option<String> {
    member.child(&[NAME]).and_then(om_scripts::node_text)
}

/// A member's comment, decoded.
pub fn member_comment(member: &OwnedNode) -> Option<String> {
    member.child(&[COMMENT]).and_then(om_scripts::node_text)
}

/// A member's definition.
pub fn member_definition(member: &OwnedNode) -> Option<&[u8]> {
    member.child(&[DEFINITION]).and_then(|d| d.value.as_deref())
}

/// The members of a field catalog `[T, 3]`.
pub fn members(catalog: &OwnedNode) -> &[OwnedNode] {
    catalog
        .child(&[MEMBERS])
        .map_or(&[], |m| m.children.as_slice())
}

/// The member whose name has `name`'s name key, with its id.
pub fn same_name<'a>(catalog: &'a OwnedNode, name: &str) -> Option<(u64, &'a OwnedNode)> {
    let key = name_key(name);
    members(catalog).iter().find_map(|member| {
        if name_key(&member_name(member)?) != key {
            return None;
        }
        Some((be_uint(&member.segment)?, member))
    })
}

/// Whether `member` is the field `field` makes: the same name, a normal
/// field of the same type with stored values and the definition this
/// module gives it (whatever its index language), and the same comment.
pub fn is_made_by(member: &OwnedNode, field: &NewField) -> bool {
    let Some(stored) = member_definition(member) else {
        return false;
    };
    let mut expected = definition(field.data_type, 0);
    let mut stored = stored.to_vec();
    if stored.len() != expected.len() {
        return false;
    }
    stored[LANGUAGE].fill(0);
    expected[LANGUAGE].fill(0);
    stored == expected
        && member_name(member).as_deref() == Some(field.name.as_str())
        && member_comment(member) == field.comment.clone().filter(|c| !c.is_empty())
}

/// Whether the definition is a normal field's with stored values.
pub fn is_stored_normal(definition: &[u8]) -> bool {
    definition.len() == 26
        && definition[0] == NORMAL
        && u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes")) & NOT_STORED == 0
}

/// A `[u8 length][serial key]` value.
fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The serial value of a key string, `None` when it is not one.
fn key_string_value(value: &[u8]) -> Option<u64> {
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len())
        .then(|| serial::decode(key))
        .flatten()
}

fn hex(segment: &[u8]) -> String {
    segment
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Plan `field` into `catalog`, a table's fields `[T, 3]` as stored.
///
/// The id is the serial key after the larger of the index's last issued id
/// and every member's, so an id is never issued twice. The index must hold
/// a name list, a display order and a member count unless the catalog has
/// no fields yet (a file FileMaker made always has them); the name list by
/// type is kept when the index has one. A name taken by a field, in the
/// members or in the name list, is refused.
pub fn plan(catalog: Option<&OwnedNode>, field: &NewField) -> Result<Planned, Refusal> {
    let listed = check_name(&field.name)?;
    let comment = field.comment.as_deref().filter(|c| !c.is_empty());
    if let Some(comment) = comment {
        check_comment(comment)?;
    }
    let catalog = catalog.ok_or_else(|| {
        Refusal::Unsupported("The table has no field catalog [T, 3] to add a field to.".into())
    })?;
    let index = catalog.child(&[INDEX]).ok_or_else(|| {
        Refusal::Unsupported(
            "The table's field catalog has no index [T, 3, 1] (every file FileMaker made has \
             one); OpenMaker does not build one."
                .into(),
        )
    })?;
    let members = members(catalog);
    if let Some((field, member)) = same_name(catalog, &field.name) {
        return Err(Refusal::Exists {
            field,
            name: member_name(member).unwrap_or_default(),
        });
    }
    if let Some(entry) = index.child(&[NAMES]).and_then(|n| n.child(&listed)) {
        let id = entry
            .value
            .as_deref()
            .and_then(|v| v.get(1..))
            .and_then(be_uint)
            .unwrap_or(0);
        return Err(Refusal::Exists {
            field: id,
            name: field.name.clone(),
        });
    }
    let missing = |what: &str| {
        Refusal::Unsupported(format!(
            "The table's field catalog index has no {what}; OpenMaker does not rebuild one."
        ))
    };
    if !members.is_empty() {
        for (list, what) in [(NAMES, "name list"), (ORDER, "display order")] {
            if index.child(&[list]).is_none() {
                return Err(missing(what));
            }
        }
    }

    // The id: after every one issued.
    let last = match index.child(&[LAST_ID]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(key_string_value)
            .ok_or_else(|| missing("readable last issued id"))?,
        None => 0,
    };
    let highest = members
        .iter()
        .filter_map(|m| serial::decode(&m.segment))
        .max()
        .unwrap_or(0);
    let id = last.max(highest) + 1;
    let field_key = serial::encode(id);
    let raw = be_uint(&field_key).ok_or_else(|| missing("field id that fits in 8 bytes"))?;

    // The position after every listed one.
    let positions = index
        .child(&[ORDER])
        .map(|order| {
            order
                .children
                .iter()
                .map(|p| {
                    <[u8; 4]>::try_from(p.segment.as_slice())
                        .map(u32::from_be_bytes)
                        .map_err(|_| missing("display order of 4-byte positions"))
                })
                .collect::<Result<Vec<u32>, Refusal>>()
        })
        .transpose()?
        .unwrap_or_default();
    let position = positions.into_iter().max().unwrap_or(0) + 1;
    let count = match index.child(&[COUNT]) {
        Some(node) => node
            .value
            .as_deref()
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| missing("readable member count"))?,
        None if members.is_empty() => 0,
        None => return Err(missing("member count")),
    };

    // The index language most of the table's fields have.
    let mut languages: Vec<u32> = members
        .iter()
        .filter_map(member_definition)
        .filter(|d| d.len() == 26)
        .map(|d| u32::from_be_bytes(d[LANGUAGE].try_into().expect("4 bytes")))
        .collect();
    languages.sort_unstable();
    let language = languages
        .chunk_by(|a, b| a == b)
        .max_by_key(|run| run.len())
        .map_or(ENGLISH, |run| run[0]);
    let definition = definition(field.data_type, language);

    let mut member = OwnedNode::list(field_key.clone(), Vec::new());
    member.set_child(OwnedNode::leaf(vec![DEFINITION], definition.clone()));
    if let Some(comment) = comment {
        member.set_child(om_scripts::text_node(&[COMMENT], comment));
    }
    member.set_child(om_scripts::text_node(&[NAME], &field.name));

    let id_value = key_string(&field_key);
    let typed = typed_key(NORMAL, field.data_type.code(), false, &field.name).ok_or_else(|| {
        Refusal::Name(format!("“{}” has no key OpenMaker can build.", field.name))
    })?;
    let mut next = catalog.clone();
    next.put(&[&[MEMBERS], &field_key], member);
    let mut changes = vec![(
        format!("[T, 03, 05, {}]", hex(&field_key)),
        format!(
            "the field: definition {}, name{}",
            hex(&definition),
            if comment.is_some() { ", comment" } else { "" }
        ),
    )];
    let mut index_entry = |path: &[&[u8]], node: OwnedNode, what: String| {
        let mut full: Vec<&[u8]> = vec![&[INDEX]];
        full.extend(path);
        changes.push((
            format!(
                "[T, 03, {}]",
                full.iter().map(|s| hex(s)).collect::<Vec<_>>().join(", ")
            ),
            what,
        ));
        next.put(&full, node);
    };
    index_entry(
        &[&[LAST_ID]],
        OwnedNode::leaf(Vec::new(), id_value.clone()),
        format!("last issued id {id}"),
    );
    index_entry(
        &[&[NAMES], &listed],
        OwnedNode::leaf(Vec::new(), id_value.clone()),
        "the name in the name list".into(),
    );
    if members.is_empty() || index.child(&[TYPED_NAMES]).is_some() {
        index_entry(
            &[&[TYPED_NAMES], &typed],
            OwnedNode::leaf(Vec::new(), id_value.clone()),
            "the name in the name list by type".into(),
        );
    }
    index_entry(
        &[&[ORDER], &position.to_be_bytes()],
        OwnedNode::leaf(Vec::new(), id_value),
        format!("display position {position}"),
    );
    index_entry(
        &[&[COUNT]],
        OwnedNode::leaf(Vec::new(), (count + 1).to_be_bytes().to_vec()),
        format!("{} fields", count + 1),
    );
    Ok(Planned {
        field: raw,
        key: field_key,
        definition,
        position,
        catalog: next,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str) -> NewField {
        NewField {
            name: name.into(),
            data_type: ScalarType::Date,
            comment: None,
        }
    }

    /// A field catalog with fields `ids` named "F<id>", indexed as
    /// FileMaker indexes them, the last issued id `last`.
    fn catalog(ids: &[u64], last: u64) -> OwnedNode {
        let mut catalog = OwnedNode::list(vec![3], Vec::new());
        let mut index = OwnedNode::list(vec![INDEX], Vec::new());
        index.set_child(OwnedNode::leaf(
            vec![LAST_ID],
            key_string(&serial::encode(last)),
        ));
        index.set_child(OwnedNode::list(vec![NAMES], Vec::new()));
        index.set_child(OwnedNode::list(vec![ORDER], Vec::new()));
        index.set_child(OwnedNode::leaf(
            vec![COUNT],
            (ids.len() as u32).to_be_bytes().to_vec(),
        ));
        index.set_child(OwnedNode::leaf(vec![0x77], vec![1]));
        catalog.set_child(index);
        for (n, &id) in ids.iter().enumerate() {
            let key = serial::encode(id);
            let name = format!("F{id}");
            let mut member = OwnedNode::list(key.clone(), Vec::new());
            member.set_child(OwnedNode::leaf(
                vec![DEFINITION],
                definition(ScalarType::Text, 7),
            ));
            member.set_child(om_scripts::text_node(&[NAME], &name));
            catalog.put(&[&[MEMBERS], &key], member);
            let mut listed = name_key(&name);
            listed.extend([0, 0, 0]);
            catalog.put(
                &[&[INDEX], &[NAMES], &listed],
                OwnedNode::leaf(Vec::new(), key_string(&key)),
            );
            catalog.put(
                &[&[INDEX], &[ORDER], &(n as u32 + 1).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), key_string(&key)),
            );
        }
        catalog
    }

    #[test]
    fn a_field_takes_the_next_id_and_every_index_list() {
        let before = catalog(&[1, 2, 5], 6);
        let planned = plan(Some(&before), &field("Due date")).unwrap();
        assert_eq!(planned.key, [7]);
        assert_eq!(planned.field, 7);
        assert_eq!(planned.position, 4);
        // The table's index language, and the defaults.
        assert_eq!(
            planned.definition,
            [1, 3, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1]
        );
        let after = &planned.catalog;
        let member = after.at(&[&[MEMBERS], &[7]]).unwrap();
        assert_eq!(member_name(member).as_deref(), Some("Due date"));
        let index = after.child(&[INDEX]).unwrap();
        assert_eq!(index.child(&[LAST_ID]).unwrap().value, Some(vec![1, 7]));
        assert_eq!(index.child(&[COUNT]).unwrap().value, Some(vec![0, 0, 0, 4]));
        let mut listed = name_key("due DATE");
        listed.extend([0, 0, 0]);
        assert_eq!(
            index.at(&[&[NAMES], &listed]).unwrap().value,
            Some(vec![1, 7])
        );
        assert_eq!(
            index.at(&[&[ORDER], &4u32.to_be_bytes()]).unwrap().value,
            Some(vec![1, 7])
        );
        // No list by type in this index, so none is started; unknown keys
        // and every other member stay as they were.
        assert!(index.child(&[TYPED_NAMES]).is_none());
        assert_eq!(index.child(&[0x77]), before.at(&[&[INDEX], &[0x77]]));
        for old in members(&before) {
            assert_eq!(after.at(&[&[MEMBERS], &old.segment]), Some(old));
        }
        assert!(is_made_by(member, &field("Due date")));
        assert!(!is_made_by(member, &field("Due Date")));
    }

    #[test]
    fn name_keys_are_the_ones_filemaker_stores() {
        // schema-path.md 5.6, from test project's name list.
        assert_eq!(
            name_key("Contacts"),
            [
                0x12, 0x3d, 0x13, 0x8e, 0x13, 0x6d, 0x14, 0x33, 0x12, 0x0f, 0x12, 0x3d, 0x14, 0x33,
                0x14, 0x10
            ]
        );
        assert_eq!(name_key("CONTACTS"), name_key("Contacts"));
    }

    #[test]
    fn ids_are_serial_keys_after_the_last_issued() {
        // 127 is the last one-byte key; 128 is `80 00`, read as 0x8000.
        let planned = plan(Some(&catalog(&[1], 127)), &field("Later")).unwrap();
        assert_eq!(planned.key, [0x80, 0x00]);
        assert_eq!(planned.field, 0x8000);
        // A member past the counter is not issued again.
        let planned = plan(Some(&catalog(&[1, 9], 3)), &field("Later")).unwrap();
        assert_eq!(planned.key, [10]);
    }

    #[test]
    fn taken_and_unusable_names_are_refused() {
        let catalog = catalog(&[1, 2], 2);
        for taken in ["F1", "f1", "F\u{0301}2"] {
            assert!(
                matches!(
                    plan(Some(&catalog), &field(taken)),
                    Err(Refusal::Exists { .. })
                ),
                "{taken}"
            );
        }
        for bad in [
            "",
            "Total + Tax",
            "1st",
            "$amount",
            " padded",
            "This and that",
            "Pi",
            "line\nbreak",
            "a::b",
            &"x".repeat(101),
        ] {
            assert!(
                matches!(plan(Some(&catalog), &field(bad)), Err(Refusal::Name(_))),
                "{bad:?}"
            );
        }
        assert!(check_name("Due date").is_ok());
        assert!(check_name("Größe").is_ok());
    }

    #[test]
    fn catalogs_without_an_index_are_not_guessed_at() {
        let mut bare = catalog(&[1], 1);
        bare.remove(&[&[INDEX]]);
        assert!(matches!(
            plan(Some(&bare), &field("New")),
            Err(Refusal::Unsupported(_))
        ));
        assert!(matches!(
            plan(None, &field("New")),
            Err(Refusal::Unsupported(_))
        ));
        let mut unordered = catalog(&[1], 1);
        unordered.remove(&[&[INDEX], &[ORDER]]);
        assert!(matches!(
            plan(Some(&unordered), &field("New")),
            Err(Refusal::Unsupported(_))
        ));
    }
}
