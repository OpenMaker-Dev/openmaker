//! Manage Value Lists' catalog of one file (file-metadata.md 6): every
//! member of `[21, 5, V]` in the catalog's display order (`[21, 1, 03, n]`,
//! section 1.1), each with its name and the source its definition names.
//! The desktop client's File > Manage > Value Lists and the authoring
//! service's `get_value_lists` both show this catalog
//! ([`crate::DataProvider::value_list_catalog`]).
//!
//! Only the file's own catalog is read: related files' lists, which the
//! calculation schema imports under client-only ids, are not this file's.
//! A member OpenMaker cannot decode is listed as [`Unreadable`], never
//! dropped, and option bits it does not know are reported.
//!
//! Manage Value Lists cannot change the catalog ([`ReadOnly`]): no writer
//! for value-list definitions is specified. The definition stream
//! `DBValueList::Definition` is known only as a reader, and the catalog
//! index `[21, 1]` is one of the host's protected upload indexes, which drop
//! an upload without `KModCount` and `KSerNum` silently (schema-path.md 2).
//! What a pop-up menu's Edit… changes, a custom list's values, is written
//! ([`plan_values`], file-metadata.md 6.5): the member's text `03`, and
//! the host's stamps on the member and the index.

use om_hbam::slot::uint_segment;
use om_hbam::{Catalog, KeyTree, Node, OwnedNode};
use om_scripts::specs::SpecField;

use crate::meta::{self, value_list_option, ValueList, ValueSource};

/// Value list member keys (file-metadata.md 6.1).
const DEFINITION: u64 = 2;
const NAME: u64 = 0x10;
/// The catalog's index `[21, 1]` and its members `[21, 5]`.
const INDEX: u8 = 1;
const MEMBERS: u8 = 5;
/// A member's custom values `03`.
const VALUES: u8 = 3;

/// The option bits [`ListDefinition`] names (file-metadata.md 6.2).
const KNOWN_OPTIONS: u16 = value_list_option::SHOW_RELATED
    | value_list_option::SORT_BY_SECOND
    | value_list_option::SECOND_ONLY;

/// One file's value lists, as Manage Value Lists shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueListCatalog {
    /// In the catalog's display order; members it does not list follow in
    /// id order.
    pub lists: Vec<CatalogList>,
    /// Why the catalog cannot be changed from OpenMaker; `Some` in this
    /// version.
    pub read_only: Option<ReadOnly>,
}

/// Why OpenMaker cannot change a file's value lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    /// No byte-exact writer for value-list definitions exists (module
    /// documentation).
    NoWriter,
}

impl ReadOnly {
    /// A stable name for agents.
    pub fn code(self) -> &'static str {
        match self {
            ReadOnly::NoWriter => "no_writer",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            ReadOnly::NoWriter => {
                "OpenMaker cannot save value lists from here yet: their stored definition \
                 has no writer, so value lists are shown read only. A pop-up menu's Edit… \
                 changes a custom list's values."
            }
        }
    }
}

/// A member of the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogList {
    /// The member's key read big-endian: the numbering every catalog reader
    /// in the workspace uses (`meta::ValueList::id`, field controls'
    /// value-list references).
    pub id: u64,
    /// The id FileMaker shows (`ValueListIDs`, XML `id`): the key's serial
    /// decode (file-metadata.md 1).
    pub shown_id: Option<u64>,
    /// The stored name; `None` when the member has none OpenMaker reads.
    pub name: Option<String>,
    pub content: Result<ListDefinition, Unreadable>,
}

/// Why a member's definition is not shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// The member has no name that decodes.
    Name,
    /// The definition names a type other than the four FileMaker defines
    /// (6.2: 0 custom values, 1 values from field, 2 external, 3
    /// calculation).
    Type(u8),
    /// The definition, or the custom values, do not decode.
    Definition,
}

impl Unreadable {
    pub fn code(self) -> &'static str {
        match self {
            Unreadable::Name => "unreadable_name",
            Unreadable::Type(_) => "unknown_type",
            Unreadable::Definition => "unreadable_definition",
        }
    }

    pub fn message(self) -> String {
        match self {
            Unreadable::Name => "OpenMaker cannot read this value list's name.".into(),
            Unreadable::Type(kind) => {
                format!("This value list's type ({kind}) is not one OpenMaker knows.")
            }
            Unreadable::Definition => {
                "OpenMaker cannot read this value list's stored definition or values.".into()
            }
        }
    }
}

/// A decoded definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDefinition {
    pub source: ListSource,
    /// Option bits other than the three [`ListSource::Field`] names
    /// (`0x8`, `kShadowValueList`, has never been seen set).
    pub unknown_options: u16,
    /// "Re-sort values based on" this language (6.2 member 9).
    pub sort_language: Option<SortLanguage>,
}

/// A `Locale::Type` and its display name (empty for a type the locale
/// catalog does not list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortLanguage {
    pub id: u32,
    pub name: String,
}

/// Where a list's values come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListSource {
    /// Custom values in order; `-` alone is a divider.
    Custom { values: Vec<String> },
    /// Values from a field.
    Field {
        primary: FieldSource,
        /// "Also display values from second field".
        secondary: Option<FieldSource>,
        /// "Include only related values starting from" this occurrence.
        related_from: Option<OccurrenceSource>,
        /// Sort values by the second field (`0x2`).
        sort_by_second: bool,
        /// Show values only from the second field (`0x4`).
        second_only: bool,
    },
    /// Another file's value list (type 2). Its file reference and list key
    /// are kept as stored, unresolved (file-metadata.md 10, U6).
    External {
        file: Option<u64>,
        list: Option<u64>,
    },
    /// A value-list calculation (type 3, only behind a debugger bit in
    /// FileMaker Pro 26.0.3). Its formula is not shown.
    Calculation,
}

/// A table occurrence a definition names, with its name in this file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OccurrenceSource {
    pub id: u64,
    /// `None` when the file has no such occurrence.
    pub name: Option<String>,
}

/// A field a definition names: occurrence, field and repetition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSource {
    /// `None` when the spec names no occurrence (an empty key).
    pub occurrence: Option<OccurrenceSource>,
    /// The field's key read big-endian, in the occurrence's base table.
    pub field: u64,
    /// `None` when this file's catalogs do not name it: a missing field,
    /// or a field of another file's table ([`Self::other_file`]).
    pub field_name: Option<String>,
    /// The occurrence shows no table of this file (another file's table),
    /// so this file's catalogs cannot name the field.
    pub other_file: bool,
    pub repetition: u16,
}

impl FieldSource {
    /// `Occurrence::Field`. A missing occurrence or field shows as
    /// FileMaker's `<Table Missing>` and `<Field Missing>`; a field of
    /// another file's table by its id.
    pub fn label(&self) -> String {
        let occurrence = self
            .occurrence
            .as_ref()
            .and_then(|o| o.name.as_deref())
            .unwrap_or("<Table Missing>");
        match (&self.field_name, self.other_file) {
            (Some(field), _) => format!("{occurrence}::{field}"),
            (None, true) => format!("{occurrence}::(field {} of another file)", self.field),
            (None, false) => format!("{occurrence}::<Field Missing>"),
        }
    }
}

impl ValueListCatalog {
    /// A file without a value-list catalog.
    pub fn empty() -> ValueListCatalog {
        ValueListCatalog {
            lists: Vec::new(),
            read_only: Some(ReadOnly::NoWriter),
        }
    }
}

/// The value lists in `tree` (which holds `[21]`), their fields and
/// occurrences named from `names`, the file's own table and occurrence
/// catalogs.
pub fn read(tree: &KeyTree<'_>, names: &Catalog) -> ValueListCatalog {
    let Some(members) = tree.at(&meta::VALUE_LISTS) else {
        return ValueListCatalog::empty();
    };
    let mut lists: Vec<CatalogList> = members
        .children
        .iter()
        .filter_map(|member| {
            let id = member.id().filter(|id| *id != 0)?;
            Some(entry(id, member, names))
        })
        .collect();
    if let Some(order) = meta::display_order(tree, &[meta::VALUE_LISTS[0]]) {
        lists.sort_by_key(|list| {
            (
                order
                    .iter()
                    .position(|id| *id == list.id)
                    .unwrap_or(usize::MAX),
                list.id,
            )
        });
    }
    ValueListCatalog {
        lists,
        read_only: Some(ReadOnly::NoWriter),
    }
}

fn entry(id: u64, member: &Node<'_>, names: &Catalog) -> CatalogList {
    let name = member.get(NAME).and_then(Node::text).and_then(Result::ok);
    let content = if name.is_none() {
        Err(Unreadable::Name)
    } else {
        match ValueList::from_node(member) {
            Some(list) => Ok(definition(list, names)),
            None => Err(
                match member
                    .get(DEFINITION)
                    .and_then(Node::assembled)
                    .and_then(|bytes| bytes.first().copied())
                {
                    Some(kind) if kind > 3 => Unreadable::Type(kind),
                    _ => Unreadable::Definition,
                },
            ),
        }
    };
    CatalogList {
        id,
        shown_id: om_hbam::serial::decode(member.segment.0),
        name,
        content,
    }
}

fn definition(list: ValueList, names: &Catalog) -> ListDefinition {
    let source = match list.source {
        ValueSource::Custom(values) => ListSource::Custom { values },
        ValueSource::Field {
            primary,
            secondary,
            start,
        } => ListSource::Field {
            primary: field(primary, names),
            secondary: secondary.map(|spec| field(spec, names)),
            related_from: start.map(|id| occurrence(id, names)),
            sort_by_second: list.options & value_list_option::SORT_BY_SECOND != 0,
            second_only: list.options & value_list_option::SECOND_ONLY != 0,
        },
        ValueSource::External { file, list: key } => ListSource::External { file, list: key },
        ValueSource::Calculation(_) => ListSource::Calculation,
    };
    ListDefinition {
        source,
        unknown_options: list.options & !KNOWN_OPTIONS,
        sort_language: (list.sort_locale != 0).then(|| SortLanguage {
            id: list.sort_locale,
            name: u16::try_from(list.sort_locale)
                .map(|id| om_calc::Language::from_id(id).display_name())
                .unwrap_or_default()
                .to_owned(),
        }),
    }
}

fn occurrence(id: u64, names: &Catalog) -> OccurrenceSource {
    OccurrenceSource {
        id,
        name: names
            .table_occurrences
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.name.clone()),
    }
}

fn field(spec: SpecField, names: &Catalog) -> FieldSource {
    let found = spec
        .occurrence
        .and_then(|id| names.table_occurrences.iter().find(|o| o.id == id));
    // An occurrence of another file's table has no base table here.
    let other_file = found.is_some_and(|o| o.base_table().is_none());
    let field_name = found
        .and_then(|o| o.base_table())
        .and_then(|table| names.table(table))
        .and_then(|table| table.field(spec.field))
        .map(|field| field.name.clone());
    FieldSource {
        occurrence: spec.occurrence.map(|id| occurrence(id, names)),
        field: spec.field,
        field_name,
        other_file,
        repetition: spec.repetition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::catalog::{DataType, Field, FieldKind, Table, TableOccurrence};
    use om_hbam::{OwnedNode, SlotReader};

    fn hex(s: &str) -> Vec<u8> {
        let digits: String = s.split_whitespace().collect();
        (0..digits.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The catalog lists every member in display order, names the fields
    /// and occurrences a field list uses from this file's own catalogs, and
    /// keeps a member it cannot decode as unreadable instead of dropping
    /// it (`meta::value_lists` drops it), each with the id FileMaker shows.
    #[test]
    fn catalog_keeps_display_order_names_sources_and_unreadable_members() {
        let name = |text: &str| OwnedNode::leaf([0x10], om_scsu::encode_scsu(text));
        let custom = OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([3], om_scsu::encode_scsu("Alpha\r-\rBeta")),
                name("Status"),
            ],
        );
        // From field 2 of occurrence d0 00 03, second field 4, related
        // from d0 00 05 (absent here), sort by second, unknown bit 0x10,
        // re-sorted by German.
        let mut definition =
            hex("01 0013 0403d00003 020102 0001 0403d00003 020104 0001 0403d00005 00 00 01");
        definition.push(0x7a);
        definition.extend(hex(
            "0000 0001 020100 020100 0001 020100 020100 0001 00 00 00000019",
        ));
        let field = OwnedNode::list([2], vec![OwnedNode::leaf([2], definition), name("Tasks")]);
        // Type 9 is not a type FileMaker defines; key 80 80 is id 256.
        let unknown = OwnedNode::list(
            [0x80, 0x80],
            vec![OwnedNode::leaf([2], hex("09 0000")), name("Odd")],
        );
        let wire = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                [0x21],
                vec![
                    OwnedNode::list(
                        [1],
                        vec![OwnedNode::list(
                            [3],
                            vec![
                                OwnedNode::leaf([0, 0, 0, 1], [2, 0x80, 0x80]),
                                OwnedNode::leaf([0, 0, 0, 2], [1, 2]),
                                OwnedNode::leaf([0, 0, 0, 3], [1, 1]),
                            ],
                        )],
                    ),
                    OwnedNode::list([5], vec![custom, field, unknown]),
                ],
            )],
        )
        .to_wire()
        .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let text_field = |id, name: &str| Field {
            id,
            name: name.into(),
            kind: FieldKind::Normal,
            data_type: DataType::Text,
            options: Vec::new(),
        };
        let names = Catalog {
            tables: vec![Table {
                id: 0x8002,
                name: "Tasks".into(),
                fields: vec![text_field(2, "Title"), text_field(4, "Due")],
            }],
            table_occurrences: vec![TableOccurrence {
                id: 0xd0_0003,
                name: "Open Tasks".into(),
                definition: hex("020100 03028002"),
            }],
            layouts: Vec::new(),
        };

        let catalog = read(&tree, &names);
        assert_eq!(catalog.read_only, Some(ReadOnly::NoWriter));
        let ids: Vec<(u64, Option<u64>)> =
            catalog.lists.iter().map(|l| (l.id, l.shown_id)).collect();
        assert_eq!(ids, [(0x8080, Some(256)), (2, Some(2)), (1, Some(1))]);

        let odd = &catalog.lists[0];
        assert_eq!(odd.name.as_deref(), Some("Odd"));
        assert_eq!(odd.content, Err(Unreadable::Type(9)));

        let tasks = catalog.lists[1].content.as_ref().unwrap();
        let ListSource::Field {
            primary,
            secondary,
            related_from,
            sort_by_second,
            second_only,
        } = &tasks.source
        else {
            panic!("{:?}", tasks.source);
        };
        assert_eq!(primary.label(), "Open Tasks::Title");
        assert_eq!(secondary.as_ref().unwrap().label(), "Open Tasks::Due");
        assert_eq!(
            *related_from,
            Some(OccurrenceSource {
                id: 0xd0_0005,
                name: None
            })
        );
        assert!(*sort_by_second && !*second_only);
        assert_eq!(tasks.unknown_options, 0x10);
        assert_eq!(
            tasks.sort_language,
            Some(SortLanguage {
                id: 25,
                name: "German".into()
            })
        );

        assert_eq!(
            catalog.lists[2].content.as_ref().unwrap().source,
            ListSource::Custom {
                values: vec!["Alpha".into(), "-".into(), "Beta".into()]
            }
        );
    }

    /// Content Management's Type list as stored (the definition FileMaker
    /// Pro 26.0.3 writes for a custom list), and Edit Value List's change
    /// that #869's twin made: a value and a divider added.
    #[test]
    fn edit_value_list_rewrites_only_the_values() {
        let definition = hex("00000002010002010000010201000201000001020100020100020100017a");
        let member = |values: &str| {
            OwnedNode::list(
                [1],
                vec![
                    OwnedNode::leaf([2], definition.clone()),
                    om_scripts::text_node(&[3], values),
                    om_scripts::text_node(&[0x10], "Type"),
                    OwnedNode::leaf([0xd8], [7; 16]),
                ],
            )
        };
        let stored = "Photo\rImage\rPDF\rAudio\rVideo\r-\rOffice\rWeb\r-";
        let index = OwnedNode::list([1], vec![OwnedNode::leaf([4], [0, 0, 0, 3])]);
        let catalog = |member: OwnedNode| {
            OwnedNode::list(
                [0x21],
                vec![index.clone(), OwnedNode::list([5], vec![member])],
            )
        };
        let before = catalog(member(stored));
        let mut values: Vec<String> = stored.split('\r').map(String::from).collect();
        values.extend(["OM869 New Value".into(), "-".into()]);
        let planned = plan_values(&before, 1, &values).unwrap();
        assert!(planned.changed);
        // The values, as Pro stores them; every other child as stored.
        assert_eq!(
            planned.member,
            member(&format!("{stored}\rOM869 New Value\r-"))
        );
        assert_eq!(planned.index, index);
        assert_eq!(
            member_values(&planned.member).unwrap(),
            ("Type".to_owned(), values.clone())
        );
        // The same values again change nothing.
        let after = catalog(planned.member);
        assert!(!plan_values(&after, 1, &values).unwrap().changed);
        // No values, or one empty item, leave no text `03`; asked again,
        // either changes nothing.
        let cleared = plan_values(&before, 1, &[String::new()]).unwrap();
        assert!(cleared.member.child(&[3]).is_none());
        let cleared = catalog(cleared.member);
        assert!(!plan_values(&cleared, 1, &[]).unwrap().changed);
        assert!(!plan_values(&cleared, 1, &[String::new()]).unwrap().changed);
        // One item per line; a list of a field's values, and a list the
        // file lacks, are refused.
        assert!(matches!(
            plan_values(&before, 1, &["Two\rlines".into()]),
            Err(ValuesRefusal::Invalid(_))
        ));
        let field_list = OwnedNode::list([1], vec![OwnedNode::leaf([2], hex("01000004 03d00002"))]);
        assert!(matches!(
            plan_values(&catalog(field_list), 1, &values),
            Err(ValuesRefusal::Unsupported(_))
        ));
        assert_eq!(
            plan_values(&before, 2, &values),
            Err(ValuesRefusal::Missing(2))
        );
    }
}

/// The longest custom values text [`plan_values`] writes, in stored
/// bytes: one piece of a stored value. A longer text is stored as a chunked
/// value with a 12-byte header (file-metadata.md 6.3), which OpenMaker's
/// reader does not read back yet.
pub const MAX_VALUES_BYTES: usize = 1000;

/// Edit Value List's change (#869): custom list `list`'s values become
/// `values`, one item per entry, `-` for a divider, if its member
/// `[21, 5, V]` is at revision `expected`
/// (`om_protocol::authoring::catalog_revision`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuesChange {
    pub list: u64,
    pub expected: [u8; 32],
    pub values: Vec<String>,
}

/// A custom list as Edit Value List starts from it: its name, its values
/// and its member's revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomList {
    pub name: String,
    pub values: Vec<String>,
    pub revision: [u8; 32],
}

/// A value list after Edit Value List ([`plan_values`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ValuesSet {
    /// The member `[21, 5, V]`, without its new stamp.
    pub member: OwnedNode,
    /// The index `[21, 1]` as stored: the host writes only its stamp.
    pub index: OwnedNode,
    /// Whether the values differ from those stored.
    pub changed: bool,
}

/// Why a list's values cannot be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValuesRefusal {
    /// The file has no such list.
    Missing(u64),
    /// The list's values do not come from custom values, or its member
    /// cannot be read.
    Unsupported(String),
    /// The values cannot be stored as given.
    Invalid(String),
}

impl std::fmt::Display for ValuesRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValuesRefusal::Missing(list) => write!(f, "The file has no value list {list}."),
            ValuesRefusal::Unsupported(why) | ValuesRefusal::Invalid(why) => f.write_str(why),
        }
    }
}

/// Custom list `list` of catalog `catalog` (`[21]`) as stored: its name,
/// values and the member itself. Refused for a list whose values do not
/// come from custom values.
pub fn custom_list(
    catalog: &OwnedNode,
    list: u64,
) -> Result<(String, Vec<String>, &OwnedNode), ValuesRefusal> {
    let member = catalog
        .at(&[&[MEMBERS], &uint_segment(list)])
        .ok_or(ValuesRefusal::Missing(list))?;
    let (name, values) = member_values(member)?;
    Ok((name, values, member))
}

/// A custom list's name and values, from its member `[21, 5, V]`. Refused
/// for a list whose values do not come from custom values.
pub fn member_values(member: &OwnedNode) -> Result<(String, Vec<String>), ValuesRefusal> {
    // A custom list without a definition is one too (file-metadata.md 6.1:
    // 110 corpus lists); a definition names its type in its first byte.
    let kind = member
        .child(&[DEFINITION as u8])
        .map(|definition| om_scripts::node_bytes(definition).and_then(|b| b.first().copied()));
    match kind {
        None | Some(Some(0)) => {}
        Some(Some(_)) => {
            return Err(ValuesRefusal::Unsupported(
                "Only a list of custom values can be edited this way; this list's values come \
                 from a field or another source."
                    .into(),
            ))
        }
        Some(None) => {
            return Err(ValuesRefusal::Unsupported(
                "OpenMaker cannot read this value list's stored definition.".into(),
            ))
        }
    }
    let name = member
        .child(&[NAME as u8])
        .and_then(om_scripts::node_text)
        .unwrap_or_default();
    let values = match member.child(&[VALUES]) {
        None => Vec::new(),
        Some(node) => om_scripts::node_text(node)
            .ok_or_else(|| {
                ValuesRefusal::Unsupported(
                    "OpenMaker cannot read this value list's stored values.".into(),
                )
            })?
            .split('\r')
            .map(str::to_owned)
            .collect(),
    };
    Ok((name, values))
}

/// Plan custom list `list`'s values as `values` in catalog `catalog`
/// (`[21]`), as FileMaker Pro 26.0.3's Edit Value List writes them
/// (file-metadata.md 6.5): the values text `03`, one item per line joined
/// by CR, every other byte of the member and the index as stored. The host
/// writes both stamps.
pub fn plan_values(
    catalog: &OwnedNode,
    list: u64,
    values: &[String],
) -> Result<ValuesSet, ValuesRefusal> {
    let (_, stored, member) = custom_list(catalog, list)?;
    // One empty item is an empty list: the text `03` either way.
    let values: &[String] = match values {
        [only] if only.is_empty() => &[],
        values => values,
    };
    let index = catalog.child(&[INDEX]).cloned().ok_or_else(|| {
        ValuesRefusal::Unsupported("The file's value-list catalog has no index [21, 1].".into())
    })?;
    if values.iter().any(|v| v.contains(['\r', '\n'])) {
        return Err(ValuesRefusal::Invalid(
            "A value list's item is one line: it holds no return.".into(),
        ));
    }
    let text = values.join("\r");
    let node = om_scripts::text_node(&[VALUES], &text);
    if om_scripts::node_bytes(&node).is_none_or(|bytes| bytes.len() > MAX_VALUES_BYTES) {
        return Err(ValuesRefusal::Invalid(format!(
            "The values are too long: OpenMaker writes at most {MAX_VALUES_BYTES} bytes of \
             custom values."
        )));
    }
    let changed = stored != values;
    let mut member = member.clone();
    if changed {
        if text.is_empty() {
            member.remove(&[&[VALUES]]);
        } else {
            member.set_child(node);
        }
    }
    Ok(ValuesSet {
        member,
        index,
        changed,
    })
}
