//! Authenticated account and privilege metadata (session-identity.md 1-3).
//! Names describe the catalog; they never authorize an operation.

use om_hbam::Node;

use super::Privileges;

/// Exactly one framed raw segment, used by Login and account child `0b`.
/// Preserve opaque serial bytes instead of converting to a numeric id.
pub fn account_key(reference: &[u8]) -> Option<&[u8]> {
    let (&length, segment) = reference.split_first()?;
    (length != 0 && usize::from(length) == segment.len()).then_some(segment)
}

/// One of FileMaker's three built-in privilege sets (session-identity.md 2):
/// the only sets whose limits OpenMaker enforces so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltInSet {
    /// Set `01`: everything.
    FullAccess,
    /// Set `02`: records created, edited and deleted in every table; layouts
    /// view only, scripts executable only, value lists view only; printing,
    /// exporting, the user's own password, all menu commands; no schema,
    /// layout, script or account changes.
    DataEntryOnly,
    /// Set `03`: as [`Self::DataEntryOnly`], but records view only.
    ReadOnlyAccess,
}

impl BuiltInSet {
    /// The set's name in an English file.
    pub fn name(self) -> &'static str {
        match self {
            Self::FullAccess => "[Full Access]",
            Self::DataEntryOnly => "[Data Entry Only]",
            Self::ReadOnlyAccess => "[Read-Only Access]",
        }
    }

    /// Whether an account in the set may create, edit and delete records.
    pub fn edits_records(self) -> bool {
        self != Self::ReadOnlyAccess
    }

    /// Whether an account in the set may change the file's design: its
    /// schema, layouts, scripts, value lists and accounts.
    pub fn changes_design(self) -> bool {
        self == Self::FullAccess
    }
}

/// A record action a privilege set allows or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordAction {
    Create,
    Edit,
    Delete,
}

/// Which records of a table an account may view ([`SetAccess::record_view`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordView<'a> {
    All,
    /// Those this compiled formula is true on.
    Where(&'a [u8]),
    Never,
}

/// The privilege set rules OpenMaker enforces for an account: a built-in
/// set, or a custom set every member of which it enforces
/// ([`super::privilege_set::enforced`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetAccess {
    BuiltIn(BuiltInSet),
    Custom(Box<super::privilege_set::CustomRules>),
}

impl SetAccess {
    /// The built-in [Full Access] set.
    pub fn full_access(&self) -> bool {
        *self == Self::BuiltIn(BuiltInSet::FullAccess)
    }

    /// Whether the account may change the file's design: only [Full
    /// Access] so far; a custom set's design options are not enforced, so
    /// none is granted (fail closed).
    pub fn changes_design(&self) -> bool {
        self.full_access()
    }

    /// Whether the account may `action` any record of base table `table`
    /// (its key, `80 02` for table 32770): an action limited by a formula
    /// is not, as the record decides it ([`Self::limit`]).
    pub fn may(&self, table: &[u8], action: RecordAction) -> bool {
        match self {
            Self::BuiltIn(set) => set.edits_records(),
            Self::Custom(rules) => rules.table(table).actions.of(action).always(),
        }
    }

    /// Whether the account may `action` a record of every table: for a
    /// member of the file's own container library, which any table's
    /// records may hold.
    pub fn may_in_every_table(&self, action: RecordAction) -> bool {
        match self {
            Self::BuiltIn(set) => set.edits_records(),
            Self::Custom(rules) => std::iter::once(&rules.default)
                .chain(rules.tables.values())
                .all(|table| table.actions.of(action).always()),
        }
    }

    /// Whether the account may do one of `actions` to the records of every
    /// table, each table by its own rules: for a member of the file's own
    /// container library, which any table's records may hold.
    pub fn may_any_in_every_table(&self, actions: &[RecordAction]) -> bool {
        match self {
            Self::BuiltIn(set) => set.edits_records(),
            Self::Custom(rules) => std::iter::once(&rules.default)
                .chain(rules.tables.values())
                .all(|table| actions.iter().any(|a| table.actions.of(*a).always())),
        }
    }

    /// Whether field `field` of base table `table` (their keys) may be
    /// changed: not when the account's custom set gives it view-only access.
    pub fn modifies(&self, table: &[u8], field: &[u8]) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.table(table).modifies(field),
        }
    }

    /// Whether the account may `action` some record of base table `table`:
    /// always, or where the formula limiting it is true. A new record's id
    /// and serials are reserved on this; its commit decides.
    pub fn may_ever(&self, table: &[u8], action: RecordAction) -> bool {
        match self {
            Self::BuiltIn(set) => set.edits_records(),
            Self::Custom(rules) => rules.table(table).actions.of(action).ever(),
        }
    }

    /// The formula limiting `action` on base table `table`, when the
    /// account's custom set allows it only where that formula is true.
    pub fn limit(&self, table: &[u8], action: RecordAction) -> Option<&[u8]> {
        match self {
            Self::BuiltIn(_) => None,
            Self::Custom(rules) => match rules.table(table).actions.of(action) {
                super::privilege_set::Allow::Limited(code) => Some(code),
                _ => None,
            },
        }
    }

    /// The formula a record of base table `table` must meet to be viewed,
    /// when the account's custom set limits viewing it.
    pub fn view_limit(&self, table: &[u8]) -> Option<&[u8]> {
        match self.record_view(table) {
            RecordView::Where(code) => Some(code),
            _ => None,
        }
    }

    /// Which records of base table `table` the account may view: all, those
    /// a formula is true on, or none (its own entry, else the set's
    /// default; session-identity.md 2.1).
    /// Whether the set lets the account view every record of base table
    /// `table` (by its id in the file): no view limit, and not never.
    pub fn views_every_record_id(&self, table: u64) -> bool {
        matches!(
            self.record_view(&om_hbam::slot::uint_segment(table)),
            RecordView::All
        )
    }

    pub fn record_view(&self, table: &[u8]) -> RecordView<'_> {
        match self {
            Self::BuiltIn(_) => RecordView::All,
            Self::Custom(rules) => match &rules.table(table).actions.view {
                super::privilege_set::Allow::Yes => RecordView::All,
                super::privilege_set::Allow::Limited(code) => RecordView::Where(code),
                super::privilege_set::Allow::No => RecordView::Never,
            },
        }
    }

    /// [`Self::hides_field`] for base table `table` and field `field` by
    /// their ids in the file.
    pub fn hides_field_id(&self, table: u64, field: u64) -> bool {
        use om_hbam::slot::uint_segment;
        self.hides_field(&uint_segment(table), &uint_segment(field))
    }

    /// Whether field `field` of base table `table` (their keys) is hidden:
    /// the account's custom set gives it no access.
    pub fn hides_field(&self, table: &[u8], field: &[u8]) -> bool {
        match self {
            Self::BuiltIn(_) => false,
            Self::Custom(rules) => {
                rules.table(table).level(field) == super::privilege_set::AccessLevel::No
            }
        }
    }

    /// Whether the account's custom set keeps any record value from it: a
    /// view limit or a table it may not view (also by default), or a field
    /// without access.
    pub fn limits_viewing(&self) -> bool {
        match self {
            Self::BuiltIn(_) => false,
            Self::Custom(rules) => std::iter::once(&rules.default)
                .chain(rules.tables.values())
                .any(|table| {
                    table.actions.view != super::privilege_set::Allow::Yes
                        || table.fields == super::privilege_set::AccessLevel::No
                        || table
                            .field_levels
                            .values()
                            .any(|level| *level == super::privilege_set::AccessLevel::No)
                }),
        }
    }

    /// Whether layout `id` may be shown: every layout under a built-in set
    /// (view only under [Data Entry Only] and [Read-Only Access]), not one
    /// the account's custom set gives no access.
    pub fn shows_layout(&self, id: u64) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.shows_layout(id),
        }
    }

    /// Whether script `id` may be run: every script under a built-in set
    /// (executable only under [Data Entry Only] and [Read-Only Access]),
    /// not one the account's custom set gives no access.
    pub fn runs_script(&self, id: u64) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.runs_script(id),
        }
    }

    /// Whether the account may print (and save records as PDF): every
    /// built-in set may (session-identity.md 2.7), a custom set as its
    /// "Allow printing" says (2.3).
    pub fn prints(&self) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.prints,
        }
    }

    /// Whether the account may export records (and save them as Excel or
    /// PDF): every built-in set may, a custom set as its "Allow exporting"
    /// says (2.3).
    pub fn exports(&self) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.exports,
        }
    }

    /// The menu commands the account's set makes available: all under a
    /// built-in set (session-identity.md 2.7), else its "Available menu
    /// commands" (2.3).
    pub fn menus(&self) -> super::privilege_set::MenuCommands {
        match self {
            Self::BuiltIn(_) => super::privilege_set::MenuCommands::All,
            Self::Custom(rules) => rules.menus,
        }
    }

    /// `Get ( LayoutAccess )` on layout `id` (session-identity.md 2.4): 0
    /// when the layout or its records have no access, 1 when its records
    /// are view only, 2 otherwise (every built-in set).
    pub fn layout_access(&self, id: u64) -> i64 {
        use super::privilege_set::AccessLevel;
        if !self.shows_layout(id) {
            return 0;
        }
        match self.layout_records(id) {
            AccessLevel::No => 0,
            AccessLevel::ViewOnly => 1,
            AccessLevel::Modifiable => 2,
        }
    }

    /// The access to records through a layout: modifiable under a
    /// built-in set (session-identity.md 2.7: `de` and `df`), else the
    /// custom set's "records via this layout" (2.2).
    pub fn layout_records(&self, id: u64) -> super::privilege_set::AccessLevel {
        match self {
            Self::BuiltIn(_) => super::privilege_set::AccessLevel::Modifiable,
            Self::Custom(rules) => rules.layout_records(id),
        }
    }

    /// Whether value list `id`'s items may be read: every list under a
    /// built-in set (view only under [Data Entry Only] and [Read-Only
    /// Access]), not one the account's custom set gives no access.
    pub fn shows_value_list(&self, id: u64) -> bool {
        match self {
            Self::BuiltIn(_) => true,
            Self::Custom(rules) => rules.shows_value_list(id),
        }
    }

    /// Whether the account may create, edit or delete any record at all.
    pub fn edits_any_record(&self) -> bool {
        match self {
            Self::BuiltIn(set) => set.edits_records(),
            Self::Custom(rules) => std::iter::once(&rules.default)
                .chain(rules.tables.values())
                .any(|table| {
                    [
                        &table.actions.create,
                        &table.actions.edit,
                        &table.actions.delete,
                    ]
                    .iter()
                    .any(|allow| allow.ever())
                }),
        }
    }
}

/// The rules OpenMaker enforces for set `set` (a member of `[23, 2, 5]`):
/// a built-in set ([`builtin_set`]), else a custom set it can enforce;
/// otherwise why it cannot, `None` for a set that does not decode. Sets
/// `01`-`03` are FileMaker's built-in sets: one whose members are not what
/// [`builtin_set`] verifies is `None`, not read as a custom set, as
/// FileMaker's rules for it are not known (#729, #876): set `01` limited
/// would otherwise be held to limits FileMaker's full access ignores.
pub fn set_access(set: &Node<'_>) -> Result<SetAccess, Option<super::privilege_set::NotEnforced>> {
    if let Some(builtin) = builtin_set(set) {
        return Ok(SetAccess::BuiltIn(builtin));
    }
    if matches!(set.segment.0, [1] | [2] | [3]) {
        return Err(None);
    }
    let decoded = super::privilege_set::decode(set).ok_or(None)?;
    super::privilege_set::enforced(&decoded)
        .map(|rules| SetAccess::Custom(Box::new(rules)))
        .map_err(Some)
}

/// [Full Access]'s and [Data Entry Only]'s record access value, `55 03`.
const ALL_RECORDS: [u8; 2] = [0x55, 0x03];

/// The built-in set `set` (a member of `[23, 2, 5]`) is, when its key and
/// access members are those FileMaker gives it. Its name must be readable
/// but is not compared: FileMaker Pro names the sets in its own language
/// in a file it creates (`[Voller Zugriff]` in German, #876), and Manage
/// Security cannot rename them. Set `01` counts as [Full Access] by its
/// record access `55 03`, as FileMaker's full access passes every check
/// whatever the other members hold. Sets `02` and `03` must decode
/// ([`super::privilege_set::decode`]) to exactly what the starter
/// solutions store in all eleven languages [O] (session-identity.md 2.7):
/// every record action ([Data Entry Only]) or view only ([Read-Only
/// Access]) with fields modifiable, no table entry, layouts view only with
/// their records modifiable (`de`), scripts executable only and value
/// lists view only (`d6`), no item entry, and the other privileges
/// `03 04`. A changed or unreadable set is `None`, and so is every custom
/// set.
pub fn builtin_set(set: &Node<'_>) -> Option<BuiltInSet> {
    use super::privilege_set::{
        decode, AccessLevel, ItemAccess, Options, Quad, RecordAccess, RowLevel,
    };
    let kind = match set.segment.0 {
        [1] => BuiltInSet::FullAccess,
        [2] => BuiltInSet::DataEntryOnly,
        [3] => BuiltInSet::ReadOnlyAccess,
        _ => return None,
    };
    set.child(&[0x10])?.text()?.ok()?;
    if kind == BuiltInSet::FullAccess {
        let records = set.child(&[2]).filter(|n| n.children.is_empty())?.value?;
        return (records == ALL_RECORDS).then_some(kind);
    }
    let decoded = decode(set)?;
    let edits = if kind == BuiltInSet::DataEntryOnly {
        RowLevel::Yes
    } else {
        RowLevel::No
    };
    let records = RecordAccess {
        create: edits,
        view: RowLevel::Yes,
        edit: edits,
        delete: edits,
        fields: AccessLevel::Modifiable,
    };
    let items = |default| ItemAccess {
        default: Some(Quad(default)),
        items: Default::default(),
    };
    (decoded.records == records
        && decoded.tables.is_empty()
        && decoded.layouts == items(0xde)
        && decoded.scripts == items(0xd6)
        && decoded.value_lists == items(0xd6)
        && decoded.options == Options(0x6304))
    .then_some(kind)
}

/// A set's name, optional record flags, and enabled extended keywords.
/// `extended` is the members node `[23, 3, 5]`, in catalog order.
/// Layout-specific rules are not established by these members.
pub fn privilege_set(set: &Node<'_>, extended: Option<&Node<'_>>) -> Option<Privileges> {
    let account_set_name = set.child(&[0x10])?.text()?.ok()?;
    if account_set_name.is_empty() {
        return None;
    }
    let records = set
        .child(&[2])
        .and_then(|n| n.value)
        .and_then(|value| value.first())
        .copied();
    Some(Privileges {
        account_set_name,
        extended: extended
            .into_iter()
            .flat_map(|n| &n.children)
            .filter(|member| {
                member
                    .child(&[5])
                    .is_some_and(|sets| sets.child(set.segment.0).is_some())
            })
            .filter_map(|member| member.child(&[0x10])?.text()?.ok())
            .collect(),
        layout_access: None,
        // calc-functions.md 9, s33-account-privileges: the aggregate
        // record flags, not record-specific conditional permissions.
        record_access: records.map(|flags| {
            if flags & 0x45 != 0 {
                2
            } else if flags & 0x10 != 0 {
                1
            } else {
                0
            }
        }),
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

    #[test]
    fn references_preserve_one_exact_nonempty_raw_segment() {
        assert_eq!(account_key(&[2, 0x80, 0x81]), Some(&[0x80, 0x81][..]));
        assert_eq!(account_key(&[1, 3]), Some(&[3][..]));
        for invalid in [&[][..], &[0], &[2, 3], &[1, 3, 4], &[1, 3, 1, 4]] {
            assert_eq!(account_key(invalid), None, "{invalid:02x?}");
        }
    }

    /// The three sets as every starter solution of FileMaker Pro 26.0.3
    /// stores them (`[23, 2, 5]`, checksums and stamps left out), whatever
    /// their names.
    #[test]
    fn built_in_sets_are_recognized_only_with_the_starters_members() {
        fn set<'a>(
            key: &'a [u8],
            name: &'a [u8],
            records: &'a [u8],
            items: [&'a [u8]; 3],
            options: &'a [u8],
            extra: Vec<Node<'a>>,
        ) -> Option<BuiltInSet> {
            let [layouts, scripts, lists] = items;
            let mut children = vec![
                leaf(&[2], records),
                list(
                    &[9],
                    vec![
                        leaf(&[4], layouts),
                        leaf(&[0x11], scripts),
                        leaf(&[0x21], lists),
                    ],
                ),
                leaf(&[0x0a], options),
                leaf(&[0x10], name),
            ];
            children.extend(extra);
            builtin_set(&list(key, children))
        }
        let full = om_scsu::encode_scsu("[Full Access]");
        let entry = om_scsu::encode_scsu("[Data Entry Only]");
        let read = om_scsu::encode_scsu("[Read-Only Access]");
        let limited: [&[u8]; 3] = [&[0xde], &[0xd6], &[0xd6]];
        let (all, few): (&[u8], &[u8]) = (&[0x55, 3], &[0x10, 3]);
        let options: &[u8] = &[3, 4];
        let full_items: [&[u8]; 3] = [&[0xdf], &[0xd7], &[0xd7]];
        assert_eq!(
            set(&[1], &full, all, full_items, &[0x9f, 0xff], vec![]),
            Some(BuiltInSet::FullAccess)
        );
        assert_eq!(
            set(&[2], &entry, all, limited, options, vec![]),
            Some(BuiltInSet::DataEntryOnly)
        );
        assert_eq!(
            set(&[3], &read, few, limited, options, vec![]),
            Some(BuiltInSet::ReadOnlyAccess)
        );
        // The names a German FileMaker Pro 26.0.3 gives the sets in a file
        // it creates (#876): the same sets.
        let german = [
            om_scsu::encode_scsu("[Voller Zugriff]"),
            om_scsu::encode_scsu("[Nur Dateneingabe]"),
            om_scsu::encode_scsu("[Nur Lesezugriff]"),
        ];
        assert_eq!(
            set(&[1], &german[0], all, full_items, &[0x9f, 0xff], vec![]),
            Some(BuiltInSet::FullAccess)
        );
        assert_eq!(
            set(&[2], &german[1], all, limited, options, vec![]),
            Some(BuiltInSet::DataEntryOnly)
        );
        assert_eq!(
            set(&[3], &german[2], few, limited, options, vec![]),
            Some(BuiltInSet::ReadOnlyAccess)
        );
        // A table entry of its own, printing off, a changed layout default,
        // [Read-Only Access]'s records under key 2, or a custom set's key.
        let table = list(&[5], vec![list(&[0x80, 0x01], vec![leaf(&[2], all)])]);
        assert_eq!(set(&[2], &entry, all, limited, options, vec![table]), None);
        assert_eq!(set(&[3], &read, few, limited, &[2, 4], vec![]), None);
        let layouts: [&[u8]; 3] = [&[0xdf], &[0xd6], &[0xd6]];
        assert_eq!(set(&[2], &entry, all, layouts, options, vec![]), None);
        assert_eq!(set(&[2], &read, few, limited, options, vec![]), None);
        assert_eq!(set(&[4], &entry, all, limited, options, vec![]), None);
    }

    #[test]
    fn catalog_flags_preserve_unknown_levels_and_only_selected_extended_members() {
        let name = om_scsu::encode_scsu("Office");
        let zeta = om_scsu::encode_scsu("zeta");
        let alpha = om_scsu::encode_scsu("alpha");
        let extended = list(
            &[5],
            vec![
                list(
                    &[1],
                    vec![
                        list(&[5], vec![leaf(&[0x80, 0x84], &[])]),
                        leaf(&[0x10], &zeta),
                    ],
                ),
                list(
                    &[2],
                    vec![
                        list(&[5], vec![leaf(&[0x80, 0x84], &[])]),
                        leaf(&[0x10], &alpha),
                    ],
                ),
                list(
                    &[3],
                    vec![list(&[5], vec![leaf(&[4], &[])]), leaf(&[0x10], &name)],
                ),
            ],
        );
        for (flags, expected) in [
            (&[0x55, 3][..], Some(2)),
            (&[0x54, 3][..], Some(2)),
            (&[0x10, 3][..], Some(1)),
            (&[0, 3][..], Some(0)),
            (&[][..], None),
        ] {
            let set = list(&[0x80, 0x84], vec![leaf(&[2], flags), leaf(&[0x10], &name)]);
            let result = privilege_set(&set, Some(&extended)).unwrap();
            assert_eq!(result.account_set_name, "Office");
            assert_eq!(result.extended, ["zeta", "alpha"]);
            assert_eq!(result.layout_access, None);
            assert_eq!(result.record_access, expected);
        }
        let set = list(&[0x80, 0x84], vec![leaf(&[0x10], &name)]);
        assert_eq!(privilege_set(&set, None).unwrap().record_access, None);
        assert!(privilege_set(&list(&[4], vec![]), None).is_none());
        assert!(privilege_set(&list(&[4], vec![leaf(&[0x10], &[])]), None).is_none());
    }
}
