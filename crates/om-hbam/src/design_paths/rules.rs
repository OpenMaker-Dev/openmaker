//! The rule tables: which paths are Design, Data, Access, Secret, Derived
//! or in no group.
//!
//! **Path rules** ([`rules`]) are prefix rules: a rule covers the node its
//! pattern names and everything below it, and of the rules that match a
//! path the longest wins. They are written per catalog ([`CATALOGS`]):
//! every catalog of a group as a whole (accounts are Access, a table's
//! container library is Data) is one rule; a Design catalog gets one rule
//! per key of its index `[cat, 1]` and per first-level key of its members
//! `[cat, 5, id]`, each as its spec names it, with the keys every catalog
//! shares ([`COMMON_INDEX`], [`COMMON_MEMBER`]). A key the specs do not
//! name is matched by no rule, so it is in no group and shows in the
//! report ([`super::report`]).
//!
//! **Step rules** ([`STEP_SECRETS`], [`is_odbc_profile`]) find the
//! credentials a script step stores in its data. Which slot of a step's
//! data holds a password depends on the step, which the path does not
//! show: the step's id is in the script's step list or the button's
//! object, so these rules need the member ([`super::secrets`]).

use std::sync::OnceLock;

use super::Kind;
use crate::serial;

/// What a path is, for history and deploys (`docs/server-storage.md`
/// section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// Tables, occurrences, relationships, fields, layouts, themes,
    /// scripts, custom functions, custom menus, value lists, privilege
    /// sets, the catalogs' order and the other content developers change.
    Design,
    /// Records, indexes, record lists and counters, container data, and
    /// the data keys inside design members (a serial field's next value).
    Data,
    /// Accounts and file access authorizations.
    Access,
    /// A credential stored inside a design member: a password, a private
    /// key, a client secret or an API key a step keeps, and the account
    /// name stored beside it in a step that signs in.
    Secret,
    /// A value FileMaker computes from other paths of the file, and some
    /// from the file's creation stamp: a checksum, a digest, a name list,
    /// a member cache, a count. Whoever writes the paths it comes from
    /// recomputes it.
    Derived,
    /// The file as a whole, or a path no rule names.
    None,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Design => "Design",
            Group::Data => "Data",
            Group::Access => "Access",
            Group::Secret => "Secret",
            Group::Derived => "Derived",
            Group::None => "none",
        }
    }

    /// Whether the group's paths are left out of an object's content:
    /// Data, Access and Derived paths. A Secret path stays, without its
    /// value.
    pub fn left_out(self) -> bool {
        matches!(self, Group::Data | Group::Access | Group::Derived)
    }
}

/// The first occurrence id's base: occurrences are numbered from serial
/// key `d0 00 01` (1,065,089; schema-path.md 5.3, file-metadata.md 1), and
/// every id from `d0 00 00` on is taken for one [I: from the first ids the
/// files hold].
pub const OCCURRENCE_BASE: u64 = 0x10_4080;

/// The first table id: tables are numbered from serial key `80 01` (129;
/// schema-path.md 5.2, file-metadata.md 1), above the one-byte keys of the
/// top-level catalogs (file-format.md 3.4).
pub const TABLE_BASE: u64 = 0x80;

/// One segment of a rule's pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seg {
    /// Exactly these bytes.
    Key(&'static [u8]),
    /// Any one segment: a member's key, a name key, a position.
    Any,
    /// A base table's data key: a serial key (query-program.md 2.6) of any
    /// length whose id is from [`TABLE_BASE`] up to [`OCCURRENCE_BASE`].
    /// The table id is also its top-level data key (file-format.md 3.6;
    /// schema-path.md 5.2); a new table takes the key after the last one
    /// issued, `[3, 10, 1, 00]` (schema-path.md 5.1), so past `bf ff`
    /// (16,511) table keys have three bytes.
    Table,
    /// A table occurrence's key: a serial key whose id is
    /// [`OCCURRENCE_BASE`] or more (schema-path.md 5.3; file-metadata.md
    /// 1, 11).
    Occurrence,
    /// A stream of a binary data value: a four-byte type such as `PNGf`
    /// or `SVG ` (containers.md 2.3).
    Stream,
}

impl Seg {
    /// Whether `segment` is one this pattern segment stands for.
    pub fn matches(self, segment: &[u8]) -> bool {
        match self {
            Seg::Key(key) => segment == key,
            Seg::Any => true,
            Seg::Table => serial::decode(segment)
                .is_some_and(|id| (TABLE_BASE..OCCURRENCE_BASE).contains(&id)),
            Seg::Occurrence => serial::decode(segment).is_some_and(|id| id >= OCCURRENCE_BASE),
            Seg::Stream => segment.len() == 4,
        }
    }

    /// The pattern segment as the docs and the report write it: hex, `T`
    /// a table, `O` an occurrence, `*` any, `4cc` a stream type.
    pub fn text(self) -> String {
        match self {
            Seg::Key(key) => hex(key),
            Seg::Any => "*".into(),
            Seg::Table => "T".into(),
            Seg::Occurrence => "O".into(),
            Seg::Stream => "4cc".into(),
        }
    }

    /// A segment this pattern segment matches, for tests.
    pub fn example(self) -> Vec<u8> {
        match self {
            Seg::Key(key) => key.to_vec(),
            Seg::Any => vec![0x01],
            Seg::Table => serial::encode(TABLE_BASE + 1),
            Seg::Occurrence => serial::encode(OCCURRENCE_BASE + 1),
            Seg::Stream => b"PNGf".to_vec(),
        }
    }
}

/// Bytes as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One rule: the paths at and below `pattern` are in `group`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub pattern: Vec<Seg>,
    pub group: Group,
    /// What the paths hold.
    pub holds: &'static str,
    /// Where `docs/re/` says so.
    pub spec: &'static str,
    /// An index key whose children are entries keyed by member, each
    /// belonging to the member it describes (a script's cached name and
    /// flags): part of that member's object, not the catalog's.
    pub entries: bool,
}

impl Rule {
    /// The pattern as `[4, 5, *, 02]`.
    pub fn pattern_text(&self) -> String {
        pattern_text(&self.pattern)
    }

    /// Whether the pattern matches the first `pattern.len()` segments of
    /// `path`.
    pub fn covers(&self, path: &[&[u8]]) -> bool {
        path.len() >= self.pattern.len() && self.pattern.iter().zip(path).all(|(p, s)| p.matches(s))
    }
}

/// A pattern as `[4, 5, *, 02]`.
pub fn pattern_text(pattern: &[Seg]) -> String {
    let parts: Vec<String> = pattern.iter().map(|s| s.text()).collect();
    format!("[{}]", parts.join(", "))
}

/// A key of a catalog's index or members, relative to `[cat, 1]` or
/// `[cat, 5, id]`.
#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub key: Seg,
    pub group: Group,
    pub holds: &'static str,
    pub spec: &'static str,
    /// See [`Rule::entries`].
    pub entries: bool,
}

const fn keyed(group: Group, key: &'static [u8], holds: &'static str, spec: &'static str) -> Key {
    Key {
        key: Seg::Key(key),
        group,
        holds,
        spec,
        entries: false,
    }
}

/// An index key of entries keyed by member, each part of its member's
/// object ([`Rule::entries`]).
const fn entries(key: &'static [u8], holds: &'static str, spec: &'static str) -> Key {
    Key {
        entries: true,
        ..keyed(Group::Design, key, holds, spec)
    }
}

const fn design(key: &'static [u8], holds: &'static str, spec: &'static str) -> Key {
    keyed(Group::Design, key, holds, spec)
}

const fn data(key: &'static [u8], holds: &'static str, spec: &'static str) -> Key {
    keyed(Group::Data, key, holds, spec)
}

const fn derived(key: &'static [u8], holds: &'static str, spec: &'static str) -> Key {
    keyed(Group::Derived, key, holds, spec)
}

/// A catalog: a node with an index `01` and members `05` (file-format.md
/// 3.4, schema-path.md 1).
#[derive(Clone, Copy, Debug)]
pub struct Catalog {
    pub name: &'static str,
    pub path: &'static [Seg],
    /// Design for a catalog whose index and members are listed key by key;
    /// Access or Data for one that is that group as a whole.
    pub group: Group,
    /// The kind of object its members are (Design catalogs).
    pub kind: Option<Kind>,
    pub spec: &'static str,
    /// Index keys besides [`COMMON_INDEX`].
    pub index: &'static [Key],
    /// Member keys besides [`COMMON_MEMBER`].
    pub member: &'static [Key],
}

/// The keys of every catalog's index `[cat, 1]` (schema-path.md 5.1, 8.1).
/// The display order is design (scripts.md: folders are spans of it); the
/// last issued id is a counter, kept by the file that issues ids; the name
/// list and the count follow from the members.
pub const COMMON_INDEX: &[Key] = &[
    data(&[0x00], "the last issued member id", "schema-path.md 5.1"),
    derived(
        &[0x01],
        "the name list, keyed by the members' names",
        "schema-path.md 5.1, 5.6",
    ),
    design(
        &[0x03],
        "the display order, and so the folders",
        "schema-path.md 5.1; file-metadata.md 1; scripts.md",
    ),
    derived(&[0x04], "the member count", "schema-path.md 5.1"),
    design(&[0xd8], "the catalog's UUID", "schema-path.md 5.1"),
    design(&[0xfc], "the modification count", "schema-path.md 8.1"),
    design(&[0xfc, 0x01], "the modifying user", "schema-path.md 8.1"),
    design(&[0xfc, 0x02], "the modifying account", "schema-path.md 8.1"),
    design(&[0xfc, 0x03], "the modification time", "schema-path.md 8.1"),
];

/// The keys of every catalog's members `[cat, 5, id]` (file-format.md 3.4,
/// schema-path.md 5.2-5.5, 8.1).
pub const COMMON_MEMBER: &[Key] = &[
    design(
        &[0x10],
        "the name",
        "file-format.md 3.4; schema-path.md 5.2",
    ),
    design(&[0xd8], "the UUID", "schema-path.md 5.2"),
    design(&[0xd9], "the source UUID", "schema-path.md 5.2"),
    design(&[0xfc], "the modification count", "schema-path.md 8.1"),
    design(&[0xfc, 0x01], "the modifying user", "schema-path.md 8.1"),
    design(&[0xfc, 0x02], "the modifying account", "schema-path.md 8.1"),
    design(&[0xfc, 0x03], "the modification time", "schema-path.md 8.1"),
];

const fn whole(
    name: &'static str,
    path: &'static [Seg],
    group: Group,
    spec: &'static str,
) -> Catalog {
    Catalog {
        name,
        path,
        group,
        kind: None,
        spec,
        index: &[],
        member: &[],
    }
}

/// Every catalog with a rule, in key order.
pub const CATALOGS: &[Catalog] = &[
    Catalog {
        name: "base tables",
        path: &[Seg::Key(&[0x03]), Seg::Key(&[0x10])],
        group: Group::Design,
        kind: Some(Kind::Table),
        spec: "schema-path.md 5.2",
        index: &[],
        member: &[
            design(&[0x03], "the comment", "file-metadata.md 1.1"),
            data(
                &[0xfb],
                "the table's data version, raised when a change affects stored values",
                "schema-path.md 5.8",
            ),
        ],
    },
    Catalog {
        name: "table occurrences",
        path: &[Seg::Key(&[0x03]), Seg::Key(&[0x11])],
        group: Group::Design,
        kind: Some(Kind::Occurrence),
        spec: "schema-path.md 5.3",
        index: &[
            derived(
                &[0x08],
                "the routes between occurrences, computed by the client",
                "schema-path.md 5.3",
            ),
            derived(
                &[0x0e],
                "the fields' dependencies through relationships (DBDepend)",
                "schema-path.md 5.3",
            ),
        ],
        member: &[
            design(
                &[0x02],
                "the definition: base table and graph box",
                "schema-path.md 5.3",
            ),
            design(
                &[0xfb],
                "the relationships to other occurrences",
                "schema-path.md 5.3",
            ),
        ],
    },
    Catalog {
        name: "relationships",
        path: &[Seg::Key(&[0x03]), Seg::Key(&[0xfb])],
        group: Group::Design,
        kind: Some(Kind::Relationship),
        spec: "schema-path.md 5.4",
        index: &[],
        member: &[
            design(
                &[0x02],
                "the definition: occurrences, sorts, options",
                "schema-path.md 5.4; file-metadata.md 4.1",
            ),
            design(
                &[0x03],
                "the match predicates",
                "schema-path.md 5.4; file-metadata.md 4.2",
            ),
        ],
    },
    Catalog {
        name: "layouts",
        path: &[Seg::Key(&[0x04])],
        group: Group::Design,
        kind: Some(Kind::Layout),
        spec: "layout-model.md 10.5, 10.6",
        index: &[
            entries(
                &[0x07],
                "each layout's entry: its occurrence, menu set, name and flags",
                "layout-model.md 10.6, 10.11",
            ),
            design(
                &[0x20],
                "the Manage dialog's view order [I]",
                "scripts.md, \"The catalog index\"",
            ),
        ],
        member: &[
            design(
                &[0x01],
                "which objects show which fields",
                "layout-model.md 10.5",
            ),
            design(&[0x02], "the LayoutDef", "layout-model.md 4.1, 10.1"),
            design(
                &[0x03],
                "the parts and each part's objects",
                "layout-model.md 4.3, 10.5",
            ),
            design(
                &[0x05],
                "the objects' attribute records, a button's step data",
                "layout-model.md 5.3.3, 5.5, 10.5",
            ),
            design(&[0x07], "the object store", "layout-model.md 5.1, 10.1"),
            design(&[0x08], "the named objects", "layout-model.md 10.5"),
            design(&[0x0a], "the object index", "layout-model.md 10.5"),
            design(&[0x0b], "the theme", "layout-model.md 4.1"),
            design(&[0x0d], "the table view", "layout-model.md 10.14"),
            design(&[0x15], "the tab order", "layout-model.md 10.5"),
            design(
                &[0x17],
                "the layout metadata (KMetadataDefs)",
                "layout-model.md 4.4, 10.6",
            ),
            design(
                &[0x21],
                "the layout-level styles (KLayoutStyleDefs)",
                "layout-model.md 4.4, 10.1, 10.6",
            ),
            design(
                &[0x30],
                "the layout's script triggers",
                "file-metadata.md 7",
            ),
            design(&[0xf0], "the member flags", "file-metadata.md 1.1"),
            design(&[0xf1], "the view flags", "layout-model.md 4.2"),
        ],
    },
    Catalog {
        name: "themes",
        path: &[Seg::Key(&[0x06])],
        group: Group::Design,
        kind: Some(Kind::Theme),
        spec: "layout-model.md 7.5",
        index: &[
            entries(
                &[0x07],
                "an empty entry for each theme",
                "layout-model.md 7.5",
            ),
            design(
                &[0x18],
                "the theme a new layout gets [I]",
                "layout-model.md 7.5",
            ),
            design(
                &[0x20],
                "the Manage dialog's view order [I]",
                "layout-model.md 7.5; scripts.md, \"The catalog index\"",
            ),
        ],
        member: &[
            design(&[0x0a], "the display name", "layout-model.md 7.5"),
            design(&[0x0b], "the version", "layout-model.md 7.5"),
            design(&[0x0c], "the locale", "layout-model.md 7.5"),
            design(&[0x0d], "the group", "layout-model.md 7.5"),
            design(
                &[0x0e],
                "the resources: preview and images",
                "layout-model.md 7.5",
            ),
            design(&[0x0f], "the platform", "layout-model.md 7.5"),
            design(&[0x16], "the styles (ThemeStyles)", "layout-model.md 7.5"),
            design(
                &[0x17],
                "the metadata (ThemeMetadata)",
                "layout-model.md 7.5",
            ),
            design(
                &[0x18],
                "the base theme's name (KBaseThemeName) [I]",
                "layout-model.md 7.5",
            ),
            design(
                &[0x19],
                "the base theme's version (KBaseThemeVersion) [I]",
                "layout-model.md 7.5",
            ),
        ],
    },
    Catalog {
        name: "custom functions",
        path: &[Seg::Key(&[0x10])],
        group: Group::Design,
        kind: Some(Kind::CustomFunction),
        spec: "calc-encoding.md 8",
        index: &[],
        member: &[
            design(&[0x03], "the description", "calc-encoding.md 8"),
            design(&[0x20], "the parameter count", "calc-encoding.md 8"),
            design(&[0x21], "the parameter names", "calc-encoding.md 8"),
            design(&[0x23], "the formula", "calc-encoding.md 8"),
        ],
    },
    Catalog {
        name: "scripts",
        path: &[Seg::Key(&[0x11])],
        group: Group::Design,
        kind: Some(Kind::Script),
        spec: "scripts.md; script-steps.md 1",
        index: &[
            entries(
                &[0x07],
                "each script's name and flags as Script Workspace shows them (its full-access badge)",
                "scripts.md, \"The catalog index\"",
            ),
            derived(
                &[0x08],
                "the digest of the member cache",
                "scripts.md, \"The catalog index\"",
            ),
            design(
                &[0x20],
                "the Manage dialog's view order [I]",
                "scripts.md, \"The catalog index\"",
            ),
        ],
        member: &[
            design(&[0x04], "the step list", "script-steps.md 1"),
            design(&[0x05], "the steps' data", "script-steps.md 1, 4.2"),
            design(&[0xf0], "the member flags", "script-steps.md 1"),
            design(&[0xf1], "the private member flags", "script-steps.md 1"),
        ],
    },
    whole(
        "accounts",
        &[Seg::Key(&[0x17]), Seg::Key(&[0x01])],
        Group::Access,
        "session-identity.md 1, 3; local-files.md 3",
    ),
    Catalog {
        name: "privilege sets",
        path: &[Seg::Key(&[0x17]), Seg::Key(&[0x02])],
        group: Group::Design,
        kind: Some(Kind::PrivilegeSet),
        spec: "session-identity.md 2",
        index: &[derived(
            &[0x08],
            "the sets by checksum",
            "session-identity.md 4.1",
        )],
        member: &[
            design(
                &[0x02],
                "the record access of other tables",
                "session-identity.md 2.1",
            ),
            design(&[0x03], "the description", "session-identity.md 2"),
            derived(
                &[0x04],
                "the checksum, over the set and the file's creation stamp",
                "session-identity.md 4.1, 4.4",
            ),
            design(
                &[0x05],
                "each table's record and field access",
                "session-identity.md 2.1",
            ),
            design(
                &[0x09],
                "the layout, script and value list access",
                "session-identity.md 2.2",
            ),
            design(&[0x0a], "the other privileges", "session-identity.md 2.3"),
            design(
                &[0x0c],
                "how often a password must change",
                "session-identity.md 2",
            ),
            design(
                &[0x0d],
                "the minimum password length",
                "session-identity.md 2",
            ),
        ],
    },
    Catalog {
        name: "extended privileges",
        path: &[Seg::Key(&[0x17]), Seg::Key(&[0x03])],
        group: Group::Design,
        kind: Some(Kind::ExtendedPrivilege),
        spec: "session-identity.md 4.2",
        index: &[],
        member: &[
            design(&[0x02], "the sharing state", "session-identity.md 4.2"),
            design(&[0x03], "the description", "session-identity.md 4.2"),
            derived(&[0x04], "the checksum", "session-identity.md 4.2, 4.4"),
            design(
                &[0x05],
                "the privilege sets that have it",
                "session-identity.md 4.2",
            ),
        ],
    },
    whole(
        "file access",
        &[Seg::Key(&[0x17]), Seg::Key(&[0x04])],
        Group::Access,
        "session-identity.md 4.3",
    ),
    Catalog {
        name: "fonts",
        path: &[Seg::Key(&[0x19])],
        group: Group::Design,
        kind: Some(Kind::Font),
        spec: "layout-model.md 8",
        index: &[],
        member: &[],
    },
    Catalog {
        name: "file library",
        path: &[Seg::Key(&[0x1f])],
        group: Group::Design,
        kind: Some(Kind::LibraryItem),
        spec: "containers.md 1",
        index: &[
            entries(&[0x06], "the glyph index: the members that are glyphs", "containers.md 1.2"),
            entries(&[0x07], "the member list", "containers.md 1.2"),
            derived(&[0x08], "the MD5 index", "containers.md 1.2"),
        ],
        member: &[
            design(&[0x02], "the stream directory", "containers.md 1.3, 2.1"),
            derived(&[0x04], "the MD5 of the data", "containers.md 1.3, 2.2"),
            data(&[0x12], "the reference count", "containers.md 1.3, 5"),
            design(
                &[0xf1],
                "the private member flags: a glyph",
                "containers.md 1.3",
            ),
            Key {
                key: Seg::Stream,
                group: Group::Design,
                holds: "a stream of the data",
                spec: "containers.md 1.3, 2.3",
                entries: false,
            },
        ],
    },
    Catalog {
        name: "data sources",
        path: &[Seg::Key(&[0x20])],
        group: Group::Design,
        kind: Some(Kind::DataSource),
        spec: "file-metadata.md 1.1",
        index: &[],
        member: &[design(
            &[0x82],
            "the file reference",
            "file-metadata.md 1.1",
        )],
    },
    Catalog {
        name: "value lists",
        path: &[Seg::Key(&[0x21])],
        group: Group::Design,
        kind: Some(Kind::ValueList),
        spec: "file-metadata.md 6",
        index: &[design(
            &[0x20],
            "the Manage dialog's view order [I]",
            "scripts.md, \"The catalog index\"",
        )],
        member: &[
            design(&[0x02], "the definition", "file-metadata.md 6.1, 6.2"),
            design(&[0x03], "the custom values", "file-metadata.md 6.1"),
            design(&[0x05], "the values calculation", "file-metadata.md 6.1"),
        ],
    },
    Catalog {
        name: "base directories",
        path: &[Seg::Key(&[0x26])],
        group: Group::Design,
        kind: Some(Kind::BaseDirectory),
        spec: "containers.md 8.1",
        index: &[],
        member: &[design(
            &[0xf0],
            "the flags: relative to the file",
            "containers.md 8.1",
        )],
    },
    Catalog {
        name: "custom menu sets",
        path: &[Seg::Key(&[0x40])],
        group: Group::Design,
        kind: Some(Kind::CustomMenuSet),
        spec: "file-format.md 3.4 [I]",
        index: &[
            design(&[0x18], "the default menu set [I]", "file-format.md 3.7"),
            design(
                &[0x20],
                "the Manage dialog's view order [I]",
                "file-format.md 3.7; scripts.md, \"The catalog index\"",
            ),
        ],
        member: &[],
    },
    Catalog {
        name: "custom menus",
        path: &[Seg::Key(&[0x41])],
        group: Group::Design,
        kind: Some(Kind::CustomMenu),
        spec: "calc-encoding.md 8",
        index: &[],
        member: &[design(
            &[0x25],
            "the install-when calculation",
            "calc-encoding.md 8",
        )],
    },
    Catalog {
        name: "fields",
        path: &[Seg::Table, Seg::Key(&[0x03])],
        group: Group::Design,
        kind: Some(Kind::Field),
        spec: "schema-path.md 5.5",
        index: &[
            derived(&[0x06], "the name list by type", "schema-path.md 5.5"),
            derived(
                &[0x0e],
                "the fields' triggers and dependencies",
                "schema-path.md 5.5, 5.10",
            ),
            data(
                &[0xfd],
                "work pending on the table's records, which the host does and deletes",
                "schema-path.md 7.2",
            ),
        ],
        member: &[
            design(&[0x02], "the definition", "schema-path.md 5.5"),
            design(&[0x03], "the comment", "schema-path.md 5.5"),
            design(&[0x05], "the calculation", "schema-path.md 5.5"),
            design(
                &[0x07],
                "the validation value list, furigana",
                "schema-path.md 5.5, 5.9",
            ),
            design(&[0x08], "the lookup", "schema-path.md 5.5"),
            design(
                &[0x09],
                "the summary definition",
                "schema-path.md 5.5; file-metadata.md 5",
            ),
            design(&[0x0b], "the lookup's constant", "schema-path.md 5.5, 5.13"),
            data(
                &[0x0c],
                "the next serial number",
                "schema-path.md 5.5; write-path.md 3.2",
            ),
            design(&[0x0d], "the auto-enter constant", "schema-path.md 5.5"),
            design(
                &[0x18],
                "the validation message",
                "schema-path.md 5.5, 5.13",
            ),
            design(
                &[0x19],
                "the validation calculation",
                "schema-path.md 5.5, 5.13",
            ),
            design(
                &[0x20],
                "the validation message calculation",
                "schema-path.md 5.5, 5.13",
            ),
            design(
                &[0x21],
                "the range's lower bound",
                "schema-path.md 5.5, 5.13",
            ),
            design(
                &[0x22],
                "the range's upper bound",
                "schema-path.md 5.5, 5.13",
            ),
            design(
                &[0x26],
                "the base directory of external storage",
                "containers.md 8.1",
            ),
            design(
                &[0x27],
                "the open storage path calculation",
                "containers.md 8.1",
            ),
            design(
                &[0x29],
                "the advanced flags",
                "schema-path.md 5.5; file-metadata.md 1.2",
            ),
            design(
                &[0x30],
                "the annotation",
                "schema-path.md 5.5; file-metadata.md 1.2",
            ),
            design(
                &[0x31],
                "the display names calculation",
                "schema-path.md 5.5; file-metadata.md 1.2",
            ),
            design(&[0xda], "the tags", "schema-path.md 5.5"),
            data(
                &[0xfb],
                "the field's data version, raised when a change affects stored values",
                "schema-path.md 5.5, 5.8",
            ),
        ],
    },
    whole(
        "table libraries",
        &[Seg::Table, Seg::Key(&[0x1f])],
        Group::Data,
        "containers.md 1.1",
    ),
];

/// Rules for paths outside the catalogs.
const TOP: &[(&[Seg], Group, &str, &str)] = &[
    (
        &[Seg::Key(&[0x02])],
        Group::None,
        "the file's status: format, version, history, locale, File Options, icons, page setup, \
         creation stamp",
        "file-format.md 3.4; file-metadata.md 2, 3",
    ),
    (
        &[Seg::Key(&[0x1a])],
        Group::None,
        "the file's own icon image (KOrigThumbImg)",
        "file-format.md 3.4; containers.md 2.3",
    ),
    (
        &[Seg::Table, Seg::Key(&[0x00])],
        Group::Data,
        "the table's last issued record id",
        "write-path.md 5.1",
    ),
    (
        &[Seg::Table, Seg::Key(&[0x01])],
        Group::Data,
        "the field value indexes",
        "write-path.md 5.5",
    ),
    (
        &[Seg::Table, Seg::Key(&[0x05])],
        Group::Data,
        "the records",
        "write-path.md 5.2, 5.4",
    ),
    (
        &[Seg::Table, Seg::Key(&[0x08])],
        Group::Data,
        "the global fields' stored values",
        "calc-values.md, \"Global values\"; script-runtime.md, \"Globals\"",
    ),
    (
        &[Seg::Table, Seg::Key(&[0x0d])],
        Group::Data,
        "the record list",
        "write-path.md 5.3",
    ),
    (
        &[Seg::Occurrence, Seg::Key(&[0x07])],
        Group::Data,
        "the saved found set, sort, current record and last find",
        "file-metadata.md 11",
    ),
];

/// A step whose data holds credentials, and which slots
/// (`[00 n]`, script-steps.md 4.2) hold them.
#[derive(Clone, Copy, Debug)]
pub struct StepSecret {
    /// The step's id (`mStepID`, script-steps.md 2).
    pub step: u16,
    pub name: &'static str,
    /// Each slot and what it holds.
    pub slots: &'static [(u16, &'static str)],
    pub spec: &'static str,
}

/// The steps whose data the specs show holding credentials. A step keeps
/// its data at `[17, 5, S, 5, K]` in a script and at `[4, 5, L, 5, O, 11]`
/// on a button (script-steps.md 4.2; layout-model.md 5.3.3); each slot
/// here is Secret with everything below it (its calculation or text).
pub const STEP_SECRETS: &[StepSecret] = &[
    StepSecret {
        step: 63,
        name: "Send Mail",
        slots: &[
            (10, "the SMTP user name"),
            (11, "the SMTP password"),
            (16, "the Google OAuth service account"),
            (17, "the Google OAuth private key"),
            (20, "the Microsoft OAuth client id"),
            (21, "the Microsoft OAuth client secret"),
        ],
        spec: "script-steps.md 12.3; catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 83,
        name: "Change Password",
        slots: &[(0, "the old password"), (1, "the new password")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 134,
        name: "Add Account",
        slots: &[(1, "the password")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 136,
        name: "Reset Account Password",
        slots: &[(1, "the new password")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 138,
        name: "Re-Login",
        slots: &[(0, "the account name"), (1, "the password")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 144,
        name: "Save Records as PDF",
        slots: &[
            (7, "the password to open [I]"),
            (8, "the password to control [I]"),
        ],
        spec: "script-steps.md 11.7, 12.2 [I]",
    },
    StepSecret {
        step: 211,
        name: "Trigger Claris Connect Flow",
        slots: &[(3, "the authentication")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 212,
        name: "Configure AI Account",
        slots: &[(3, "the API key")],
        spec: "script-steps.md 12.2; catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 227,
        name: "Configure RAG Account",
        slots: &[(3, "the API key")],
        spec: "script-steps.md 12.2; catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 242,
        name: "Print PDF",
        slots: &[(1, "the password")],
        spec: "catalogs/script-step-params.csv",
    },
    StepSecret {
        step: 243,
        name: "Create PDF",
        slots: &[(7, "the password to open"), (8, "the password to control")],
        spec: "script-steps.md 12.2",
    },
    StepSecret {
        step: 244,
        name: "Append PDF",
        slots: &[(7, "the PDF's password")],
        spec: "script-steps.md 12.2",
    },
    StepSecret {
        step: 246,
        name: "Open PDF",
        slots: &[(7, "the PDF's password")],
        spec: "script-steps.md 12.2",
    },
];

/// The step key holding an import or export profile (`KProfile`,
/// script-steps.md 4.2).
pub const PROFILE: &[u8] = &[0x86];

/// Whether a step's profile `86` is an ODBC connection profile: its value
/// starts with the type `ODBC` and holds the data source name, the user
/// name and the password (script-steps.md 11.5). The whole profile is
/// Secret.
pub fn is_odbc_profile(value: &[u8]) -> bool {
    value.starts_with(b"ODBC")
}

impl Catalog {
    /// The rules this catalog contributes.
    fn rules(&self, out: &mut Vec<Rule>) {
        if self.group != Group::Design {
            out.push(Rule {
                pattern: self.path.to_vec(),
                group: self.group,
                holds: self.name,
                spec: self.spec,
                entries: false,
            });
            return;
        }
        let mut put = |under: &[Seg], key: &Key| {
            let mut pattern = self.path.to_vec();
            pattern.extend_from_slice(under);
            pattern.push(key.key);
            out.push(Rule {
                pattern,
                group: key.group,
                holds: key.holds,
                spec: key.spec,
                entries: key.entries,
            });
        };
        for key in COMMON_INDEX.iter().chain(self.index) {
            put(&[Seg::Key(&[0x01])], key);
        }
        for key in COMMON_MEMBER.iter().chain(self.member) {
            put(&[Seg::Key(&[0x05]), Seg::Any], key);
        }
    }

    /// The catalog's path with its table, for a catalog under a table.
    pub fn at(&self, table: Option<&[u8]>) -> super::Path {
        self.path
            .iter()
            .map(|seg| match seg {
                Seg::Key(key) => key.to_vec(),
                _ => table.unwrap_or(&[]).to_vec(),
            })
            .collect()
    }

    /// Whether the catalog sits under a table's data key.
    pub fn per_table(&self) -> bool {
        self.path.first() == Some(&Seg::Table)
    }

    /// The catalog whose members are of `kind`.
    pub fn of(kind: Kind) -> &'static Catalog {
        CATALOGS
            .iter()
            .find(|c| c.kind == Some(kind))
            .expect("every kind has a catalog")
    }
}

/// Every path rule, the catalogs' first, each catalog's in the order it
/// lists them.
pub fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut out = Vec::new();
        for catalog in CATALOGS {
            catalog.rules(&mut out);
        }
        for (pattern, group, holds, spec) in TOP {
            out.push(Rule {
                pattern: pattern.to_vec(),
                group: *group,
                holds,
                spec,
                entries: false,
            });
        }
        out
    })
}

/// The path rules at catalog level as a Markdown table, as
/// `docs/server-storage.md` section 4 shows them (a test keeps the two
/// equal): each catalog with its group and the kind of its members, the
/// keys of Design catalogs that are another group, and the paths outside
/// the catalogs. Each catalog's other keys are its Design rules.
pub fn markdown_table() -> String {
    let mut out =
        String::from("| Paths | Holds | Group | Objects | Spec |\n|---|---|---|---|---|\n");
    // The common index keys that are not Design, once for every catalog.
    for key in COMMON_INDEX.iter().filter(|k| k.group != Group::Design) {
        out.push_str(&format!(
            "| `[cat, 01, {}]` | {}, in every catalog's index | {} |  | {} |\n",
            key.key.text(),
            key.holds,
            key.group.name(),
            key.spec
        ));
    }
    let mut row = |pattern: &[Seg], holds: &str, group: Group, kind: Option<Kind>, spec: &str| {
        out.push_str(&format!(
            "| `{}` | {holds} | {} | {} | {spec} |\n",
            pattern_text(pattern),
            group.name(),
            kind.map_or("", Kind::name),
        ));
    };
    for catalog in CATALOGS {
        row(
            catalog.path,
            catalog.name,
            catalog.group,
            catalog.kind,
            catalog.spec,
        );
        let under = |region: &[Seg], key: &Key| {
            let mut pattern = catalog.path.to_vec();
            pattern.extend_from_slice(region);
            pattern.push(key.key);
            pattern
        };
        for key in catalog
            .index
            .iter()
            .filter(|k| k.group != Group::Design || k.entries)
        {
            let holds = if key.entries {
                format!("{}; each entry is part of its member's object", key.holds)
            } else {
                key.holds.to_string()
            };
            row(
                &under(&[Seg::Key(&[0x01])], key),
                &holds,
                key.group,
                None,
                key.spec,
            );
        }
        for key in catalog.member.iter().filter(|k| k.group != Group::Design) {
            let pattern = under(&[Seg::Key(&[0x05]), Seg::Any], key);
            row(&pattern, key.holds, key.group, None, key.spec);
        }
    }
    for (pattern, group, holds, spec) in TOP {
        row(pattern, holds, *group, None, spec);
    }
    out
}

/// The step rules as a Markdown table, as `docs/server-storage.md`
/// section 4 shows them (a test keeps the two equal).
pub fn markdown_secrets() -> String {
    let mut out = String::from(
        "| Step | Slots whose calculation `05` (and text `01`) is Secret | Spec |\n|---|---|---|\n",
    );
    for step in STEP_SECRETS {
        let slots: Vec<String> = step
            .slots
            .iter()
            .map(|(slot, holds)| format!("{slot} {holds}"))
            .collect();
        out.push_str(&format!(
            "| {} {} | {} | {} |\n",
            step.step,
            step.name,
            slots.join("; "),
            step.spec
        ));
    }
    out.push_str(
        "| any step | in its profile `86`, when an ODBC connection profile, the user name and \
         password strings; the whole profile when it does not read so | script-steps.md 11.0, 11.5 |\n",
    );
    out.push_str(
        "| a step OpenMaker cannot identify | every slot, whole: no step list, one that does not \
         read, a data key no step names, a step id the step catalog does not have | \
         script-steps.md 2, 4.2; catalogs/script-steps.csv |\n",
    );
    out
}

/// The path rule for `path`: the longest of those that cover it.
pub(crate) fn rule_for(path: &[&[u8]]) -> Option<&'static Rule> {
    rules()
        .iter()
        .filter(|r| r.covers(path))
        .max_by_key(|r| r.pattern.len())
}

/// The group of `path` by the path rules alone, for this module only: it
/// cannot see a Secret path, which needs the step the path's data belongs
/// to, so callers use [`super::group_in`] with the tree.
pub(crate) fn group_of(path: &[&[u8]]) -> Group {
    rule_for(path).map_or(Group::None, |r| r.group)
}

/// Whether two patterns of the same length can match one path.
#[cfg(test)]
pub(crate) fn overlap(a: &[Seg], b: &[Seg]) -> bool {
    fn one(a: Seg, b: Seg) -> bool {
        match (a, b) {
            (Seg::Key(x), Seg::Key(y)) => x == y,
            (Seg::Key(x), other) | (other, Seg::Key(x)) => other.matches(x),
            (Seg::Any, _) | (_, Seg::Any) => true,
            (x, y) => x == y,
        }
    }
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| one(*x, *y))
}
