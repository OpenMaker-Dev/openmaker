//! A privilege set's members decoded (session-identity.md 2.1-2.3): what a
//! set `[23, 2, 5, P]` lets its accounts do with records, fields, layouts,
//! scripts and value lists, and its other privileges. Decoding authorizes
//! nothing by itself. A member this module does not know, or a value that
//! does not decode, makes the whole set unreadable (`None`), so that what
//! enforces a set fails closed rather than grant an access it cannot read.

use std::collections::BTreeMap;

use om_hbam::Node;

/// One record action's access (`DBAccess::RowLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowLevel {
    No,
    Yes,
    /// Limited by a calculation, per record ([`TableAccess::limits`]).
    Limited,
}

impl RowLevel {
    fn decode(bits: u8) -> Option<Self> {
        match bits {
            0 => Some(Self::No),
            1 => Some(Self::Yes),
            2 => Some(Self::Limited),
            _ => None,
        }
    }
}

/// An access level (`DBAccessLevel`): of a field, a layout, a script or a
/// value list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessLevel {
    No,
    /// View only; for a script, executable only.
    ViewOnly,
    Modifiable,
}

impl AccessLevel {
    fn decode(bits: u8) -> Option<Self> {
        match bits {
            1 => Some(Self::No),
            2 => Some(Self::ViewOnly),
            3 => Some(Self::Modifiable),
            _ => None,
        }
    }
}

/// A record access value (`DBAccess`, two bytes; 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordAccess {
    pub create: RowLevel,
    pub view: RowLevel,
    pub edit: RowLevel,
    pub delete: RowLevel,
    /// The access of fields without an entry of their own.
    pub fields: AccessLevel,
}

impl RecordAccess {
    /// What a missing value reads as: no action, no access to fields.
    pub const NONE: Self = Self {
        create: RowLevel::No,
        view: RowLevel::No,
        edit: RowLevel::No,
        delete: RowLevel::No,
        fields: AccessLevel::No,
    };

    /// Two bytes: byte 0 create, view, edit and delete from the high bits
    /// down, two bits each; byte 1's low two bits the fields' level. The
    /// other bits of byte 1, 0 in every observed set, are unknown.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let &[actions, fields] = bytes else {
            return None;
        };
        if fields & !0x03 != 0 {
            return None;
        }
        Some(Self {
            create: RowLevel::decode(actions >> 6)?,
            view: RowLevel::decode((actions >> 4) & 3)?,
            edit: RowLevel::decode((actions >> 2) & 3)?,
            delete: RowLevel::decode(actions & 3)?,
            fields: AccessLevel::decode(fields)?,
        })
    }
}

/// A table's own record and field access (`[23, 2, 5, P, 05, T]`; 2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableAccess {
    pub records: RecordAccess,
    /// Fields with an access of their own, by field key.
    pub fields: BTreeMap<Vec<u8>, AccessLevel>,
    /// The compiled formulas (`06` of members `06`-`09`) limiting create,
    /// view, edit and delete, for each action that is limited; `None` for
    /// one whose formula is missing, which FileMaker counts as no [C].
    pub limits: [Option<Vec<u8>>; 4],
}

/// A catalog's access entries under `09` (2.2): layouts, scripts or value
/// lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemAccess {
    /// The default for items without an entry; `None` when the set has
    /// none, which reads as no access.
    pub default: Option<Quad>,
    /// Items with an entry of their own, by the item's key.
    pub items: BTreeMap<Vec<u8>, Quad>,
}

impl ItemAccess {
    /// The access of item `key`: its entry, else the default, else none.
    pub fn of(&self, key: &[u8]) -> Quad {
        self.items
            .get(key)
            .or(self.default.as_ref())
            .copied()
            .unwrap_or(Quad::NONE)
    }
}

/// An item entry: four `DBAccessLevel` fields in one byte, from the low
/// bits up (`DBPrivileges::AccessQuad`; 2.2). For a layout the first is
/// the layout and the second records through it; for a script or a value
/// list only the first is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quad(pub u8);

impl Quad {
    /// No access: first, second and third 1, fourth 3 [C].
    pub const NONE: Self = Self(0xd5);

    /// The first field (the item itself).
    pub fn first(self) -> Option<AccessLevel> {
        AccessLevel::decode(self.0 & 3)
    }

    /// The second field (a layout's records).
    pub fn second(self) -> Option<AccessLevel> {
        AccessLevel::decode((self.0 >> 2) & 3)
    }
}

/// The other privileges `0a`, after the stored value's XOR `0x6000` (2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options(pub u16);

/// The menu commands a set allows (2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommands {
    All,
    EditingOnly,
    Minimum,
}

impl Options {
    pub const CREATE_VALUE_LISTS: u16 = 0x0001;
    pub const CHANGE_PASSWORD: u16 = 0x0004;
    pub const MANAGE_EXTENDED: u16 = 0x0008;
    pub const MANAGE_ACCOUNTS: u16 = 0x0010;
    pub const MANAGE_DATABASE: u16 = 0x0020;
    pub const MANAGE_CUSTOM_MENUS: u16 = 0x0080;
    pub const PRINT: u16 = 0x0100;
    pub const EXPORT: u16 = 0x0200;
    pub const OVERRIDE_VALIDATION: u16 = 0x0400;
    pub const CREATE_SCRIPTS: u16 = 0x0800;
    pub const CREATE_LAYOUTS: u16 = 0x1000;
    const EDIT_MENUS: u16 = 0x2000;
    const OTHER_MENUS: u16 = 0x4000;

    /// The stored big-endian value, decoded.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let &[high, low] = bytes else {
            return None;
        };
        Some(Self(u16::from_be_bytes([high, low]) ^ 0x6000))
    }

    pub fn has(self, flag: u16) -> bool {
        self.0 & flag == flag
    }

    /// Both menu bits: all; `0x2000` alone: editing only; otherwise
    /// minimum (`0x4000` alone is no dialog choice; FileMaker reports it as
    /// minimum).
    pub fn menus(self) -> MenuCommands {
        match (self.has(Self::EDIT_MENUS), self.has(Self::OTHER_MENUS)) {
            (true, true) => MenuCommands::All,
            (true, false) => MenuCommands::EditingOnly,
            _ => MenuCommands::Minimum,
        }
    }
}

/// A privilege set's members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegeSet {
    pub name: String,
    /// The record access of tables without an entry of their own (`02`).
    pub records: RecordAccess,
    /// Tables with an entry of their own, by base table key (`05, T`).
    pub tables: BTreeMap<Vec<u8>, TableAccess>,
    pub layouts: ItemAccess,
    pub scripts: ItemAccess,
    pub value_lists: ItemAccess,
    pub options: Options,
}

/// The catalogs' keys under `09`: `KLayoutDefs`, `KScriptDefs`,
/// `KValueLists`.
const LAYOUTS: u8 = 0x04;
const SCRIPTS: u8 = 0x11;
const VALUE_LISTS: u8 = 0x21;

/// The members a set may have that decide nothing about access, or that
/// other code reads: description, checksum, password lifetime and length,
/// and the catalog member's UUID and stamps.
const OTHER_MEMBERS: [&[u8]; 10] = [
    &[0x03],
    &[0x04],
    &[0x0c],
    &[0x0d],
    &[0xd8],
    &[0xd9],
    &[0xfc],
    &[0xfc, 0x01],
    &[0xfc, 0x02],
    &[0xfc, 0x03],
];

/// A plain value of `node`'s child `key`: `None` without one, `Some(None)`
/// for a child that holds no single value.
fn value<'a>(node: &Node<'a>, key: &[u8]) -> Option<Option<&'a [u8]>> {
    node.child(key)
        .map(|n| n.value.filter(|_| n.children.is_empty()))
}

/// The members of `set` (a member of `[23, 2, 5]`). `None` when its name is
/// missing or empty, a member is unknown, or a value does not decode.
pub fn decode(set: &Node<'_>) -> Option<PrivilegeSet> {
    let name = set.child(&[0x10])?.text()?.ok()?;
    if name.is_empty() {
        return None;
    }
    let records = match value(set, &[0x02]) {
        None => RecordAccess::NONE,
        Some(bytes) => RecordAccess::decode(bytes?)?,
    };
    let options = Options::decode(value(set, &[0x0a])??)?;
    let mut tables = BTreeMap::new();
    let (mut layouts, mut scripts, mut value_lists) = Default::default();
    for member in &set.children {
        match member.segment.0 {
            [0x02] | [0x0a] | [0x10] => {}
            // A list member holds its entries, never a value of its own.
            [0x05] | [0x09] if member.value.is_some() => return None,
            [0x05] => {
                for table in &member.children {
                    tables.insert(table.segment.0.to_vec(), table_access(table)?);
                }
            }
            [0x09] => {
                for entry in &member.children {
                    let &[byte] = entry.value.filter(|_| entry.children.is_empty())? else {
                        return None;
                    };
                    let quad = Quad(byte);
                    // The two fields read must be levels; the other two are
                    // not read (1 and 3 in every observed entry).
                    quad.first()?;
                    quad.second()?;
                    let (catalog, item) = entry.segment.0.split_first()?;
                    let access: &mut ItemAccess = match *catalog {
                        LAYOUTS => &mut layouts,
                        SCRIPTS => &mut scripts,
                        VALUE_LISTS => &mut value_lists,
                        _ => return None,
                    };
                    if item.is_empty() {
                        access.default = Some(quad);
                    } else {
                        access.items.insert(item.to_vec(), quad);
                    }
                }
            }
            key if OTHER_MEMBERS.contains(&key) => {}
            _ => return None,
        }
    }
    Some(PrivilegeSet {
        name,
        records,
        tables,
        layouts,
        scripts,
        value_lists,
        options,
    })
}

/// A table entry `05, T`.
fn table_access(table: &Node<'_>) -> Option<TableAccess> {
    if table.value.is_some() {
        return None;
    }
    let records = match value(table, &[0x02]) {
        None => RecordAccess::NONE,
        Some(bytes) => RecordAccess::decode(bytes?)?,
    };
    let mut fields = BTreeMap::new();
    let mut limits: [Option<Vec<u8>>; 4] = Default::default();
    for member in &table.children {
        match member.segment.0 {
            [0x02] => {}
            // List members hold entries, never a value of their own.
            [0x05] | [0x06..=0x09] if member.value.is_some() => return None,
            [0x05] => {
                for field in &member.children {
                    let &[level] = field.value.filter(|_| field.children.is_empty())? else {
                        return None;
                    };
                    fields.insert(field.segment.0.to_vec(), AccessLevel::decode(level)?);
                }
            }
            [action @ 0x06..=0x09] => {
                limits[usize::from(action - 0x06)] = member
                    .child(&[0x06])
                    .and_then(|n| n.assembled())
                    .map(|bytes| bytes.into_owned());
            }
            _ => return None,
        }
    }
    let actions = [records.create, records.view, records.edit, records.delete];
    // A formula only for a limited action: anything else is unknown.
    for (action, limit) in actions.iter().zip(&limits) {
        if limit.is_some() && *action != RowLevel::Limited {
            return None;
        }
    }
    Some(TableAccess {
        records,
        fields,
        limits,
    })
}

/// Which record actions a set allows on a table, when each is plainly
/// yes or no.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actions {
    pub create: Allow,
    /// Viewing the table's records: always, or only those where its
    /// formula is true (the host withholds the others' values).
    pub view: Allow,
    pub edit: Allow,
    pub delete: Allow,
}

impl Actions {
    /// The table's answer for `action`.
    pub fn of(&self, action: super::security::RecordAction) -> &Allow {
        use super::security::RecordAction;
        match action {
            RecordAction::Create => &self.create,
            RecordAction::Edit => &self.edit,
            RecordAction::Delete => &self.delete,
        }
    }
}

/// Whether a set allows a record action on a table: not at all, always, or
/// on a record where its formula is true (a compiled stream, evaluated by
/// `rules::recalc::RecordLimit`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Allow {
    No,
    Yes,
    Limited(Vec<u8>),
}

impl Allow {
    /// Allowed whatever the record holds.
    pub fn always(&self) -> bool {
        *self == Self::Yes
    }

    /// Allowed on some record (always, or where its formula is true).
    pub fn ever(&self) -> bool {
        *self != Self::No
    }
}

/// A table's limits OpenMaker enforces for a custom set ([`enforced`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRules {
    pub actions: Actions,
    /// The level of fields without an entry of their own: modifiable or
    /// view only.
    pub fields: AccessLevel,
    /// Fields with a level of their own, by field key.
    pub field_levels: BTreeMap<Vec<u8>, AccessLevel>,
}

impl TableRules {
    /// Field `key`'s access: its own, else the table's default.
    pub fn level(&self, key: &[u8]) -> AccessLevel {
        self.field_levels.get(key).copied().unwrap_or(self.fields)
    }

    /// Whether field `key` may be changed (view only or no access: no).
    pub fn modifies(&self, key: &[u8]) -> bool {
        self.level(key) == AccessLevel::Modifiable
    }
}

/// The limits OpenMaker enforces for a custom set: its record and field
/// access by table, the layouts, scripts and value lists it may use,
/// whether it may print and export, and its menu commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomRules {
    /// Tables without an entry of their own.
    pub default: TableRules,
    /// Tables with an entry of their own, by base table key.
    pub tables: BTreeMap<Vec<u8>, TableRules>,
    pub layouts: ItemAccess,
    pub scripts: ItemAccess,
    pub value_lists: ItemAccess,
    /// "Allow printing" (`0a` `0x0100`, 2.3).
    pub prints: bool,
    /// "Allow exporting" (`0a` `0x0200`, 2.3).
    pub exports: bool,
    /// "Available menu commands" (`0a` `0x2000`, `0x4000`, 2.3).
    pub menus: MenuCommands,
}

impl CustomRules {
    /// The rules of base table `key`.
    pub fn table(&self, key: &[u8]) -> &TableRules {
        self.tables.get(key).unwrap_or(&self.default)
    }

    /// Whether layout `id` may be shown: not when the set gives the layout
    /// no access (2.2).
    pub fn shows_layout(&self, id: u64) -> bool {
        self.layouts.of(&key_of(id)).first() != Some(AccessLevel::No)
    }

    /// Whether script `id` may be run: executable only or modifiable, not
    /// no access (2.2).
    pub fn runs_script(&self, id: u64) -> bool {
        self.scripts.of(&key_of(id)).first() != Some(AccessLevel::No)
    }

    /// The access to records through layout `id`: the second field of its
    /// entry, else of the default (2.2): modifiable, view only or no access.
    pub fn layout_records(&self, id: u64) -> AccessLevel {
        self.layouts
            .of(&key_of(id))
            .second()
            .unwrap_or(AccessLevel::No)
    }

    /// Whether value list `id`'s items may be read: view only or
    /// modifiable, not no access (2.2).
    pub fn shows_value_list(&self, id: u64) -> bool {
        self.value_lists.of(&key_of(id)).first() != Some(AccessLevel::No)
    }
}

/// An item's key under `09`: its catalog id as the shortest big-endian
/// bytes, as references give it (`80 59` for script 0x8059).
fn key_of(id: u64) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(7);
    bytes[skip..].to_vec()
}

/// Why OpenMaker cannot enforce a custom set yet: the account stays refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotEnforced {
    /// The set's default for tables without an entry limits an action by a
    /// calculation, which it has no formula for.
    FormulaLimit,
}

impl std::fmt::Display for NotEnforced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FormulaLimit => "a default record access limited by a calculation",
        })
    }
}

/// The limits of custom set `set` OpenMaker enforces, when it can enforce
/// every member: each table's create, view, edit and delete yes, no or
/// limited by a formula (view never no), every field modifiable or view
/// only, the records through every layout modifiable, view only or without
/// access, all menu commands. A layout, script or value list may have no access, and
/// printing and exporting may be denied. View only and modifiable items are enforced alike by
/// granting no design change.
/// Anything else is the first reason it cannot. The set's other options
/// (managing the database, creating layouts and so on) are never granted,
/// whatever they say.
pub fn enforced(set: &PrivilegeSet) -> Result<CustomRules, NotEnforced> {
    fn table(
        records: RecordAccess,
        fields: &BTreeMap<Vec<u8>, AccessLevel>,
        limits: Option<&[Option<Vec<u8>>; 4]>,
    ) -> Result<TableRules, NotEnforced> {
        // A limited action's formula (create `06`, edit `08`, delete `09`):
        // without one it counts as no [C]. The set's default (`02`) has no
        // formula of its own to limit by.
        let allow = |level, index: usize| match level {
            RowLevel::Yes => Ok(Allow::Yes),
            RowLevel::No => Ok(Allow::No),
            RowLevel::Limited => match limits {
                Some(limits) => Ok(limits[index]
                    .as_ref()
                    .map_or(Allow::No, |code| Allow::Limited(code.clone()))),
                None => Err(NotEnforced::FormulaLimit),
            },
        };
        let actions = Actions {
            create: allow(records.create, 0)?,
            view: allow(records.view, 1)?,
            edit: allow(records.edit, 2)?,
            delete: allow(records.delete, 3)?,
        };
        // Records that may never be viewed (a view limit without its
        // formula counts as no) and fields without access are withheld by
        // the host (`views::ViewFilter`).
        Ok(TableRules {
            actions,
            fields: records.fields,
            field_levels: fields.clone(),
        })
    }
    // A layout without access is not shown, a script without access not
    // run and a value list without access not read
    // ([`CustomRules::shows_layout`], [`CustomRules::runs_script`],
    // [`CustomRules::shows_value_list`]); a missing default gives every
    // item without an entry no access. The records through a layout are
    // modifiable, view only or without access
    // ([`CustomRules::layout_records`]). Without access, the window masks
    // what the layout shows of them, while a script reads and writes them
    // as the table's rules allow (session-identity.md 2.5, #707); a hidden
    // layout's records are never read. A quad whose records field does not
    // decode counts as no access. View-only items need nothing more, as a
    // custom set is never granted a design change.
    // Printing and exporting are enforced where the desktop prints, saves
    // records as a file or exports them; the set's other options are never
    // granted (design changes stay [Full Access] only; no other is
    // offered).
    Ok(CustomRules {
        default: table(set.records, &BTreeMap::new(), None)?,
        tables: set
            .tables
            .iter()
            .map(|(key, access)| {
                Ok((
                    key.clone(),
                    table(access.records, &access.fields, Some(&access.limits))?,
                ))
            })
            .collect::<Result<_, NotEnforced>>()?,
        layouts: set.layouts.clone(),
        scripts: set.scripts.clone(),
        value_lists: set.value_lists.clone(),
        prints: set.options.has(Options::PRINT),
        exports: set.options.has(Options::EXPORT),
        menus: set.options.menus(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::KeySegment;

    fn leaf<'a>(segment: &'a [u8], value: &'a [u8]) -> Node<'a> {
        Node {
            segment: KeySegment(segment),
            value: Some(value),
            push: false,
            children: vec![],
        }
    }
    fn list<'a>(segment: &'a [u8], children: Vec<Node<'a>>) -> Node<'a> {
        Node {
            segment: KeySegment(segment),
            value: None,
            push: true,
            children,
        }
    }

    /// What OpenMaker can enforce of a custom set, and the first reason it
    /// cannot (#156 slice a).
    #[test]
    fn custom_sets_are_enforced_only_with_plain_record_and_field_limits() {
        fn office<'a>(
            name: &'a [u8],
            records: &'a [u8],
            table: &'a [u8],
            field: &'a [u8],
            layouts: &'a [u8],
            options: &'a [u8],
        ) -> PrivilegeSet {
            decode(&list(
                &[4],
                vec![
                    leaf(&[2], records),
                    list(
                        &[5],
                        vec![list(
                            &[0x80, 0x02],
                            vec![leaf(&[2], table), list(&[5], vec![leaf(&[3], field)])],
                        )],
                    ),
                    list(
                        &[9],
                        vec![
                            leaf(&[4], layouts),
                            leaf(&[0x11], &[0xd7]),
                            leaf(&[0x21], &[0xd7]),
                        ],
                    ),
                    leaf(&[0x0a], options),
                    leaf(&[0x10], name),
                ],
            ))
            .unwrap()
        }
        let name = om_scsu::encode_scsu("Clerks");
        let all: &[u8] = &[0x9f, 0xff];
        // Contacts: create and edit, not delete; field 3 view only. Other
        // tables: view only.
        let clerks = office(&name, &[0x10, 0x03], &[0x54, 0x03], &[2], &[0xdf], all);
        let rules = enforced(&clerks).unwrap();
        assert!(rules.shows_layout(0x13) && rules.runs_script(0x8059));
        assert_eq!(rules.layout_records(0x13), AccessLevel::Modifiable);
        // Records through a layout view only (`db`) are enforced, by
        // layout, and `Get ( LayoutAccess )` answers 1 there (2.4). Without
        // access (`d7`) on a layout the set shows, the set is admitted and
        // the layout answers 0, as FileMaker Pro 26.0.3 does (#707); on a
        // hidden layout (`d5`, `dd`) they are never read, and the layout
        // answers 0 whatever its records' access.
        use crate::host::security::SetAccess;
        let set = |quad| office(&name, &[0x10, 0x03], &[0x54, 0x03], &[2], &[quad], all);
        let view_only = enforced(&set(0xdb)).unwrap();
        assert_eq!(view_only.layout_records(0x13), AccessLevel::ViewOnly);
        assert_eq!(
            SetAccess::Custom(Box::new(view_only)).layout_access(0x13),
            1
        );
        let masked = enforced(&set(0xd7)).unwrap();
        assert!(masked.shows_layout(0x13));
        assert_eq!(masked.layout_records(0x13), AccessLevel::No);
        assert_eq!(SetAccess::Custom(Box::new(masked)).layout_access(0x13), 0);
        for quad in [0xd5, 0xdd] {
            let hidden = enforced(&set(quad)).unwrap();
            assert!(!hidden.shows_layout(0x13), "{quad:02x}");
            assert_eq!(SetAccess::Custom(Box::new(hidden)).layout_access(0x13), 0);
        }
        // Records never viewable (a view limited without its formula counts
        // as no; or no for every other table) and a field without access
        // are enforced: the host withholds them.
        let never = enforced(&office(
            &name,
            &[0x10, 0x03],
            &[0x64, 0x03],
            &[3],
            &[0xdf],
            all,
        ));
        assert_eq!(never.unwrap().table(&[0x80, 0x02]).actions.view, Allow::No);
        let other = enforced(&office(
            &name,
            &[0x00, 0x03],
            &[0x54, 0x03],
            &[3],
            &[0xdf],
            all,
        ));
        assert_eq!(other.unwrap().table(&[0x80, 0x03]).actions.view, Allow::No);
        let field = enforced(&office(
            &name,
            &[0x10, 0x03],
            &[0x54, 0x03],
            &[1],
            &[0xdf],
            all,
        ));
        let contacts = field.unwrap().table(&[0x80, 0x02]).clone();
        assert_eq!(contacts.level(&[3]), AccessLevel::No);
        assert!(!contacts.modifies(&[3]));
        assert!(rules.prints && rules.exports && rules.menus == MenuCommands::All);
        // Stored `40 00` (`0x2000` after the XOR: Editing only) and `60 00`
        // (neither bit: Minimum; observed, 2.3) are enforced.
        for (options, menus) in [
            ([0x40, 0x00], MenuCommands::EditingOnly),
            ([0x60, 0x00], MenuCommands::Minimum),
        ] {
            let set = office(&name, &[0x10, 0x03], &[0x54, 0x03], &[2], &[0xdf], &options);
            assert_eq!(enforced(&set).unwrap().menus, menus, "{options:02x?}");
        }
        // Stored `00 00` (all menus, nothing else) and `02 00` (exporting,
        // all menus; observed, 2.3): enforced, printing denied.
        for (options, exports) in [([0x00, 0x00], false), ([0x02, 0x00], true)] {
            let set = office(&name, &[0x10, 0x03], &[0x54, 0x03], &[2], &[0xdf], &options);
            let denied = enforced(&set).unwrap();
            assert!(
                !denied.prints && denied.exports == exports,
                "{options:02x?}"
            );
        }
        // A script entry of its own (`09, 11 80 59`) without access.
        let mut rules = rules;
        rules.scripts.items.insert(vec![0x80, 0x59], Quad(0xd5));
        assert!(!rules.runs_script(0x8059) && rules.runs_script(0x8058));
        // A value list entry of its own (`09, 21 03`) without access.
        rules.value_lists.items.insert(vec![3], Quad(0xd5));
        assert!(!rules.shows_value_list(3) && rules.shows_value_list(4));
        // Every layout without access: none is shown, whatever its records'
        // access.
        let hidden = office(&name, &[0x10, 0x03], &[0x54, 0x03], &[2], &[0xd9], all);
        assert!(!enforced(&hidden).unwrap().shows_layout(0x13));
        let contacts = rules.table(&[0x80, 0x02]);
        assert_eq!(
            contacts.actions,
            Actions {
                create: Allow::Yes,
                view: Allow::Yes,
                edit: Allow::Yes,
                delete: Allow::No
            }
        );
        assert!(contacts.modifies(&[2]) && !contacts.modifies(&[3]));
        let other = rules.table(&[0x80, 0x03]);
        assert_eq!(
            other.actions,
            Actions {
                create: Allow::No,
                view: Allow::Yes,
                edit: Allow::No,
                delete: Allow::No
            }
        );
        // The default limits create by a formula it has none of.
        let set = office(&name, &[0x90, 0x03], &[0x54, 0x03], &[3], &[0xdf], all);
        assert_eq!(enforced(&set), Err(NotEnforced::FormulaLimit));
    }

    /// The starter solutions' built-in sets (session-identity.md 2.7).
    #[test]
    fn the_built_in_sets_decode_as_their_documented_meaning() {
        let name = om_scsu::encode_scsu("[Read-Only Access]");
        let set = list(
            &[3],
            vec![
                leaf(&[2], &[0x10, 0x03]),
                leaf(&[4], &[0; 16]),
                list(
                    &[9],
                    vec![
                        leaf(&[4], &[0xde]),
                        leaf(&[0x11], &[0xd6]),
                        leaf(&[0x21], &[0xd6]),
                    ],
                ),
                leaf(&[0x0a], &[0x03, 0x04]),
                leaf(&[0x10], &name),
            ],
        );
        let read = decode(&set).unwrap();
        assert_eq!(read.name, "[Read-Only Access]");
        assert_eq!(
            read.records,
            RecordAccess {
                create: RowLevel::No,
                view: RowLevel::Yes,
                edit: RowLevel::No,
                delete: RowLevel::No,
                fields: AccessLevel::Modifiable,
            }
        );
        assert!(read.tables.is_empty());
        // Layouts view only, their records modifiable; scripts executable
        // only; value lists view only.
        let layout = read.layouts.of(&[0x13]);
        assert_eq!(layout.first(), Some(AccessLevel::ViewOnly));
        assert_eq!(layout.second(), Some(AccessLevel::Modifiable));
        assert_eq!(
            read.scripts.of(&[0x80, 0x59]).first(),
            Some(AccessLevel::ViewOnly)
        );
        assert_eq!(
            read.value_lists.of(&[1]).first(),
            Some(AccessLevel::ViewOnly)
        );
        let options = read.options;
        for flag in [Options::PRINT, Options::EXPORT, Options::CHANGE_PASSWORD] {
            assert!(options.has(flag), "{flag:#x}");
        }
        for flag in [
            Options::MANAGE_DATABASE,
            Options::CREATE_LAYOUTS,
            Options::CREATE_SCRIPTS,
            Options::OVERRIDE_VALIDATION,
        ] {
            assert!(!options.has(flag), "{flag:#x}");
        }
        assert_eq!(options.menus(), MenuCommands::All);
        assert_eq!(Options::decode(&[0x9f, 0xff]), Some(Options(0xffff)));
    }

    /// A custom set as session-identity.md 2.1-2.3 decode the MBS examples'
    /// members: a table entry with a view-only field and delete limited by
    /// a formula, a layout and a script without access, minimum menus.
    #[test]
    fn custom_members_decode_and_unknown_ones_make_the_set_unreadable() {
        fn set<'a>(name: &'a [u8], extra: Vec<Node<'a>>, table: Node<'a>) -> Option<PrivilegeSet> {
            let mut children = vec![
                leaf(&[2], &[0x55, 0x03]),
                list(&[5], vec![table]),
                list(
                    &[9],
                    vec![
                        leaf(&[4], &[0xde]),
                        leaf(&[4, 0x13], &[0xdd]),
                        leaf(&[0x11, 0x80, 0x59], &[0xd5]),
                    ],
                ),
                leaf(&[0x0a], &[0x60, 0x00]),
                leaf(&[0x10], name),
            ];
            children.extend(extra);
            decode(&list(&[4], children))
        }
        let name = om_scsu::encode_scsu("Office");
        let formula = [0x1e, 0x01, 0x00];
        let table = || {
            list(
                &[0x80, 0x01],
                vec![
                    leaf(&[2], &[0x56, 0x03]),
                    list(&[5], vec![leaf(&[2], &[2])]),
                    list(&[9], vec![leaf(&[6], &formula)]),
                ],
            )
        };
        let office = set(&name, vec![], table()).unwrap();
        let entry = &office.tables[&vec![0x80, 0x01]];
        assert_eq!(entry.records.delete, RowLevel::Limited);
        assert_eq!(entry.fields[&vec![2]], AccessLevel::ViewOnly);
        assert_eq!(entry.limits[3].as_deref(), Some(&formula[..]));
        assert_eq!(office.layouts.of(&[0x13]).first(), Some(AccessLevel::No));
        assert_eq!(
            office.layouts.of(&[0x14]).first(),
            Some(AccessLevel::ViewOnly)
        );
        assert_eq!(
            office.scripts.of(&[0x80, 0x59]).first(),
            Some(AccessLevel::No)
        );
        // No default: no access.
        assert_eq!(office.scripts.of(&[0x80, 0x5a]), Quad::NONE);
        assert_eq!(office.value_lists.of(&[1]).first(), Some(AccessLevel::No));
        assert_eq!(office.options, Options(0));
        assert_eq!(office.options.menus(), MenuCommands::Minimum);

        // The member stamp `fc` is allowed; a value on a list member, or an
        // entry whose fields are not levels, makes the set unreadable.
        assert!(set(&name, vec![leaf(&[0xfc], &[1])], table()).is_some());
        assert!(set(&name, vec![leaf(&[5], &[0xff])], table()).is_none());
        let mut valued = list(&[9], vec![]);
        valued.value = Some(&[0xff]);
        assert!(set(&name, vec![valued], table()).is_none());
        assert!(set(
            &name,
            vec![list(&[9], vec![leaf(&[4, 0x14], &[0xdc])])],
            table()
        )
        .is_none());
        // An unknown member, an unknown catalog under 09, a formula for an
        // action that is not limited.
        assert!(set(&name, vec![leaf(&[0x0b], &[1])], table()).is_none());
        assert!(set(
            &name,
            vec![list(&[9], vec![leaf(&[0x30], &[0xd7])])],
            table()
        )
        .is_none());
        let unlimited = list(
            &[0x80, 0x01],
            vec![
                leaf(&[2], &[0x55, 0x03]),
                list(&[6], vec![leaf(&[6], &formula)]),
            ],
        );
        assert!(set(&name, vec![], unlimited).is_none());
        // A level that is no RowLevel, field bits that are unknown.
        assert_eq!(RecordAccess::decode(&[0xc0, 0x03]), None);
        assert_eq!(RecordAccess::decode(&[0x55, 0x07]), None);
        assert_eq!(RecordAccess::decode(&[0x55]), None);
    }
}
