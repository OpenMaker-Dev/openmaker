//! What a session may read of a file when its account's custom privilege set
//! limits viewing a table's records by a calculation, gives a table's
//! records or a field no access (#156, docs/re/session-identity.md 2.5). A
//! record the view formula is not true on (every record of a table it may
//! not view) keeps its id and modification count, so it stays in found
//! sets and lists, but its field values are withheld; a field without
//! access is left out of every record served. A table with any of these
//! serves no index
//! (an index holds every record's values) and no container data (its
//! library is per table, not per record), and while any table has one the
//! file's own container library, which may hold any table's data, is
//! withheld too. Every read of record data goes
//! through [`ViewFilter::filter`]: downloads (op90, op15, op92) and query
//! programs (op86), which read a view of the file and its derived state
//! (`engine::Read`). The formula is always evaluated on the stored record.
//! Container uploads and reference counts, which answer from the libraries
//! (whether some content exists, how often it is used), are refused for
//! such an account (`connection.rs`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use om_data::containers::LIBRARY;
use om_data::host::security::{RecordView, SetAccess};
use om_data::rules::recalc::RecordLimit;
use om_data::rules::RecordEnvironment;
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;

use crate::derived::Derived;
use crate::engine::{Read, ReadError, NO_LIMIT};
use crate::modcount::MOD_COUNT;

/// Under a table `[T]`: its indexes, its records and its global values.
const INDEXES: u8 = 0x01;
const RECORDS: u8 = 0x05;
const GLOBALS: u8 = 0x08;

/// The view limits of one session's account.
pub(crate) struct ViewFilter {
    /// Each view-limited base table's formula, by table key; `None` where
    /// the formula cannot be built, which withholds every record's values.
    limits: BTreeMap<Vec<u8>, Option<RecordLimit>>,
    /// Each table's fields without access ([`hidden_fields`]).
    hidden: HiddenFields,
    environment: RecordEnvironment,
    /// The records this filter served without their values, by table and
    /// record key ([`Withheld::note`]).
    withheld: RefCell<Vec<(Vec<u8>, Vec<u8>)>>,
}

/// The cells of the fields of each base table an account's custom set
/// gives no access, by table key and cell key: each field's own key and
/// its repetitions' (`Derived::repetition_cells`). Tables with none are
/// left out.
pub(crate) type HiddenFields = BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>;

/// The fields of the base tables of the file `derived` was computed from
/// that `access` gives no access (its own entry, else the table's default
/// field access, else the set's default).
pub(crate) fn hidden_fields(derived: &Derived, access: &SetAccess) -> HiddenFields {
    let mut hidden = HiddenFields::new();
    for table in &derived.catalog().tables {
        let key = uint_segment(table.id);
        let mut fields = BTreeSet::new();
        for field in table.fields.iter().map(|field| uint_segment(field.id)) {
            if access.hides_field(&key, &field) {
                fields.extend(derived.repetition_cells(&key, &field, &BTreeSet::new()));
                fields.insert(field);
            }
        }
        if !fields.is_empty() {
            hidden.insert(key, fields);
        }
    }
    hidden
}

/// `record`, an edit of a stored record of `table` that a session uploaded,
/// with `stored`'s cells of the table's `hidden` fields in place of its own:
/// the client never had them, so what it sends for them is not the record's
/// (`writes::commit`).
pub(crate) fn keep_hidden(record: &mut OwnedNode, stored: &OwnedNode, hidden: &BTreeSet<Vec<u8>>) {
    record
        .children
        .retain(|cell| !hidden.contains(cell.segment.as_slice()));
    for cell in &stored.children {
        if hidden.contains(cell.segment.as_slice()) {
            record.set_child(cell.clone());
        }
    }
}

/// The records a session was served without their values, by table and
/// record key: its client holds them empty, so an edit of one would erase
/// them (#670). A record stays in until the session's account changes or
/// it closes the file: a later read does not show the client replaced its
/// empty copy (it may ask for some fields, or never fetch a page).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Withheld(BTreeSet<(Vec<u8>, Vec<u8>)>);

impl Withheld {
    /// Add what `view` withheld.
    pub(crate) fn note(&mut self, view: &ViewFilter) {
        self.0.extend(view.withheld.take());
    }

    /// Those of the records an op70 commit list names (`[T, 05, R]`), all
    /// that the commit asks of it (`writes::refused_action`): what an
    /// operation carries ([`crate::engine::CommitRecords`]).
    pub(crate) fn named_in(&self, list: &OwnedNode) -> Withheld {
        let mut named = BTreeSet::new();
        for table in &list.children {
            let Some(records) = table.child(&[0x05]) else {
                continue;
            };
            for entry in &records.children {
                let key = (table.segment.clone(), entry.segment.clone());
                if self.0.contains(&key) {
                    named.insert(key);
                }
            }
        }
        Withheld(named)
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn contains(&self, table: &[u8], record: &[u8]) -> bool {
        self.0.contains(&(table.to_vec(), record.to_vec()))
    }

    /// Mark record `record` of `table` withheld, as a download would.
    #[cfg(test)]
    pub(crate) fn mark(&mut self, table: &[u8], record: &[u8]) {
        self.0.insert((table.to_vec(), record.to_vec()));
    }
}

impl ViewFilter {
    /// The filter for `access` on the file `read` reads; `None` when it
    /// keeps nothing from the account, so that everything may be read. A
    /// table whose records may never be viewed (its own entry or the set's
    /// default) counts as a view limit no record meets.
    pub(crate) fn new(
        read: Read<'_>,
        access: &SetAccess,
        account: &str,
    ) -> Result<Option<Self>, ReadError> {
        if !access.limits_viewing() {
            return Ok(None);
        }
        let mut limits = BTreeMap::new();
        for table in &read.derived.catalog().tables {
            let key = uint_segment(table.id);
            match access.record_view(&key) {
                RecordView::All => {}
                RecordView::Where(code) => {
                    let limit =
                        crate::derived::record_limit(read.view, read.derived, &key, code)?.ok();
                    limits.insert(key, limit);
                }
                RecordView::Never => {
                    limits.insert(key, None);
                }
            }
        }
        Ok(Some(Self {
            limits,
            hidden: hidden_fields(read.derived, access),
            environment: crate::recalc::record_environment(account, crate::recalc::random_uuid()),
            withheld: RefCell::default(),
        }))
    }

    /// Whether the account may not read all of base table `table`: a view
    /// limit, no view at all, or a field without access. Its indexes and
    /// container library are withheld.
    pub(crate) fn limits(&self, table: &[u8]) -> bool {
        self.limits.contains_key(table) || self.hidden.contains_key(table)
    }

    /// Whether [`ViewFilter::filter`] can change the node at `path` or
    /// anything below it: the root, the file's own library and a
    /// view-limited table. Elsewhere the session reads the file as it is.
    pub(crate) fn filters(&self, path: &[&[u8]]) -> bool {
        match path {
            [] | [[LIBRARY], ..] => true,
            [table, ..] => self.limits(table),
        }
    }

    /// Whether field `field` of `table` (their keys) is hidden.
    fn hides(&self, table: &[u8], field: &[u8]) -> bool {
        self.hidden
            .get(table)
            .is_some_and(|fields| fields.contains(field))
    }

    /// `record`, a record of `table` the account may view (or the table's
    /// global values), without its hidden fields' cells.
    fn strip(&self, table: &[u8], record: &mut OwnedNode) {
        if let Some(fields) = self.hidden.get(table) {
            record
                .children
                .retain(|cell| !fields.contains(cell.segment.as_slice()));
        }
    }

    /// Whether record `record` (its key) of `table` may be viewed. The
    /// formula is evaluated on the record as `read` reads it, never on the
    /// node being served, which may hold only the fields a client asked
    /// for; a record the file does not hold is not.
    fn viewable(&self, read: Read<'_>, table: &[u8], record: &[u8]) -> Result<bool, ReadError> {
        Ok(match self.limits.get(table) {
            None => true,
            Some(None) => false,
            Some(Some(limit)) => read
                .view
                .subtree(&[table, &[RECORDS][..], record], NO_LIMIT)?
                .is_some_and(|stored| {
                    limit.allows(&crate::recalc::texts(&stored), &self.environment)
                }),
        })
    }

    /// `node`, the file's node at `path`, as the session may read it: a
    /// view-limited table's indexes and container library, and the file's
    /// own library, empty (and `None` below them), `None` for a value inside
    /// a record it may not view, otherwise a copy with those records' values
    /// withheld. `read` reads the records a path below one names.
    pub(crate) fn filter(
        &self,
        read: Read<'_>,
        path: &[&[u8]],
        node: OwnedNode,
    ) -> Result<Option<OwnedNode>, ReadError> {
        Ok(match path {
            [] => Some(self.root(read, node)?),
            // The file's own container library may hold any table's data:
            // served empty, as a client opening the file asks for it.
            [[LIBRARY]] => Some(emptied(node)),
            [[LIBRARY], ..] => None,
            [table, rest @ ..] if self.limits(table) => self.table(read, table, rest, node)?,
            _ => Some(node),
        })
    }

    fn root(&self, read: Read<'_>, mut root: OwnedNode) -> Result<OwnedNode, ReadError> {
        for child in &mut root.children {
            if child.segment == [LIBRARY] {
                *child = emptied(std::mem::take(child));
            }
        }
        for child in &mut root.children {
            if self.limits(&child.segment) {
                let table = child.segment.clone();
                *child = self.whole_table(read, &table, std::mem::take(child))?;
            }
        }
        Ok(root)
    }

    fn table(
        &self,
        read: Read<'_>,
        table: &[u8],
        rest: &[&[u8]],
        node: OwnedNode,
    ) -> Result<Option<OwnedNode>, ReadError> {
        Ok(match rest {
            [] => Some(self.whole_table(read, table, node)?),
            [[INDEXES]] | [[LIBRARY]] => Some(emptied(node)),
            [[INDEXES], ..] | [[LIBRARY], ..] => None,
            [[RECORDS]] => Some(self.records(read, table, node)?),
            [[RECORDS], record] => {
                let mut node = node;
                if self.viewable(read, table, record)? {
                    self.strip(table, &mut node);
                } else {
                    self.note(table, record);
                    withhold(&mut node);
                }
                Some(node)
            }
            [[RECORDS], record, field, ..] => {
                let viewable = self.viewable(read, table, record)?;
                if !viewable {
                    self.note(table, record);
                }
                (viewable && !self.hides(table, field)).then_some(node)
            }
            // A global field without access keeps no value either.
            [[GLOBALS]] => {
                let mut node = node;
                self.strip(table, &mut node);
                Some(node)
            }
            [[GLOBALS], field, ..] => (!self.hides(table, field)).then_some(node),
            _ => Some(node),
        })
    }

    fn whole_table(
        &self,
        read: Read<'_>,
        table: &[u8],
        mut node: OwnedNode,
    ) -> Result<OwnedNode, ReadError> {
        for child in &mut node.children {
            if child.segment == [INDEXES] || child.segment == [LIBRARY] {
                *child = emptied(std::mem::take(child));
            }
        }
        for child in &mut node.children {
            if child.segment == [RECORDS] {
                *child = self.records(read, table, std::mem::take(child))?;
            } else if child.segment == [GLOBALS] {
                self.strip(table, child);
            }
        }
        Ok(node)
    }

    fn records(
        &self,
        read: Read<'_>,
        table: &[u8],
        mut records: OwnedNode,
    ) -> Result<OwnedNode, ReadError> {
        for record in &mut records.children {
            if self.viewable(read, table, &record.segment)? {
                self.strip(table, record);
            } else {
                self.note(table, &record.segment);
                withhold(record);
            }
        }
        Ok(records)
    }

    /// Record that record `record` of `table` was served withheld.
    fn note(&self, table: &[u8], record: &[u8]) {
        self.withheld
            .borrow_mut()
            .push((table.to_vec(), record.to_vec()));
    }
}

/// `node` with nothing below it: a withheld index or container library,
/// still there for a client that asks for it.
fn emptied(mut node: OwnedNode) -> OwnedNode {
    node.children.clear();
    node.value = None;
    node
}

/// A record node without its field values: only its modification count is
/// kept, so the client still knows the record and its version.
fn withhold(record: &mut OwnedNode) {
    record.children.retain(|child| child.segment == [MOD_COUNT]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use om_hbam::slot::uint_segment;
    use om_hbam::HbamFile;

    /// What a read of `store` gets, through its tree's view.
    macro_rules! read_of {
        ($store:expr, $tree:ident) => {{
            let store: &Store = $store;
            Read {
                view: &$tree,
                derived: store.derived(),
                waiting: store.waiting(),
                read_only: false,
                writes_refused: None,
            }
        }};
    }

    /// #137's file and its `limited` set (`[23, 2, 5, 4]`), the set's rules
    /// changed by `change` (the fixture has no set hiding a table or a
    /// field).
    fn fixture_with(
        change: impl FnOnce(&mut om_data::host::privilege_set::TableRules),
    ) -> (Store, SetAccess, Vec<u8>) {
        let path = std::env::var_os("OM_CUSTOM_SETS_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../re/work/137-custom-sets/Contacts-custom.fmp12")
            });
        let file = HbamFile::open(&path).unwrap();
        let store = Store::load(&file, "Contacts-custom.fmp12").unwrap();
        let sets = file.subtree(&[&[23], &[2], &[5]]).unwrap().unwrap();
        let SetAccess::Custom(mut rules) =
            om_data::host::security::set_access(sets.child(&[4]).unwrap()).unwrap()
        else {
            panic!("`limited` is a custom set");
        };
        let contacts = uint_segment(0x8002);
        change(rules.tables.get_mut(&contacts).unwrap());
        (store, SetAccess::Custom(rules), contacts)
    }

    /// A field without access (First Name, field 1) is left out of every
    /// record served, and an edit that leaves it out keeps its stored
    /// value.
    #[test]
    #[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
    fn a_field_without_access_is_left_out_and_kept_on_edit() {
        use om_data::host::privilege_set::{AccessLevel, Allow};
        let (store, access, contacts) = fixture_with(|table| {
            table.actions.view = Allow::Yes;
            table.actions.edit = Allow::Yes;
            table.field_levels.insert(vec![1], AccessLevel::No);
        });
        let tree = store.tree_view();
        let read = read_of!(&store, tree);
        let view = ViewFilter::new(read, &access, "limited").unwrap().unwrap();
        let records: [&[u8]; 2] = [&contacts, &[RECORDS]];
        let stored = store.node(&records).unwrap().clone();
        let served = view
            .filter(read, &records, stored.clone())
            .unwrap()
            .unwrap();
        for (stored, served) in stored.children.iter().zip(&served.children) {
            assert!(stored.child(&[1]).is_some() && served.child(&[1]).is_none());
            assert_eq!(served.children.len() + 1, stored.children.len());
            // The client's edit of what it was served keeps First Name.
            let mut upload = served.clone();
            let hidden = hidden_fields(store.derived(), &access);
            keep_hidden(&mut upload, stored, &hidden[&contacts]);
            assert_eq!(&upload, stored);
        }
        // Its repetitions' cells are hidden too, and so is its global value
        // if it has one.
        let hidden = hidden_fields(store.derived(), &access);
        assert!(hidden[&contacts].contains(&vec![1, 2]));
        let globals: [&[u8]; 2] = [&contacts, &[GLOBALS]];
        let mut node = OwnedNode {
            segment: vec![GLOBALS],
            push: true,
            ..OwnedNode::default()
        };
        node.set_child(OwnedNode::leaf(vec![1], b"secret".to_vec()));
        node.set_child(OwnedNode::leaf(vec![1, 2], b"secret".to_vec()));
        node.set_child(OwnedNode::leaf(vec![2], b"kept".to_vec()));
        let served = view.filter(read, &globals, node.clone()).unwrap().unwrap();
        assert_eq!(served.children.len(), 1);
        assert_eq!(served.children[0].segment, vec![2]);
        let cell: [&[u8]; 3] = [&contacts, &[GLOBALS], &[1, 2]];
        assert!(view.filter(read, &cell, node).unwrap().is_none());
        // Nor is it read below a record, or in the table's indexes.
        let record = &stored.children[0].segment;
        let cell: [&[u8]; 4] = [&contacts, &[RECORDS], record, &[1]];
        let node = store.node(&cell).unwrap().clone();
        assert!(view.filter(read, &cell, node).unwrap().is_none());
        let indexes: [&[u8]; 2] = [&contacts, &[INDEXES]];
        if let Some(node) = store.node(&indexes) {
            assert!(view
                .filter(read, &indexes, node.clone())
                .unwrap()
                .unwrap()
                .children
                .is_empty());
        }
    }

    /// A table whose records may never be viewed has every record's values
    /// withheld, and an edit of one is refused.
    #[test]
    #[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
    fn a_table_the_account_may_not_view_withholds_every_record() {
        use om_data::host::privilege_set::Allow;
        let (store, access, contacts) = fixture_with(|table| {
            table.actions.view = Allow::No;
            table.actions.edit = Allow::Yes;
        });
        let tree = store.tree_view();
        let read = read_of!(&store, tree);
        let view = ViewFilter::new(read, &access, "limited").unwrap().unwrap();
        let records: [&[u8]; 2] = [&contacts, &[RECORDS]];
        let stored = store.node(&records).unwrap().clone();
        let served = view
            .filter(read, &records, stored.clone())
            .unwrap()
            .unwrap();
        assert_eq!(served.children.len(), stored.children.len());
        for record in &served.children {
            assert!(record
                .children
                .iter()
                .all(|cell| cell.segment == [MOD_COUNT]));
        }
    }

    /// #137's `limited` set (`[23, 2, 5, 4]`): Contacts' records viewable
    /// where First Name = "Allowed". A download of some of a record's
    /// fields (op92) is decided on the whole stored record, so "Allowed"
    /// without its First Name is still served and "Denied" without it still
    /// withheld.
    #[test]
    #[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
    fn a_view_limit_decides_on_the_stored_record_not_the_fields_asked_for() {
        let path = std::env::var_os("OM_CUSTOM_SETS_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../re/work/137-custom-sets/Contacts-custom.fmp12")
            });
        let file = HbamFile::open(&path).unwrap();
        let store = Store::load(&file, "Contacts-custom.fmp12").unwrap();
        let sets = file.subtree(&[&[23], &[2], &[5]]).unwrap().unwrap();
        let access = om_data::host::security::set_access(sets.child(&[4]).unwrap()).unwrap();
        let tree = store.tree_view();
        let read = read_of!(&store, tree);
        let view = ViewFilter::new(read, &access, "limited").unwrap().unwrap();
        let contacts = uint_segment(0x8002);
        let mut served = BTreeMap::new();
        for record in &store.node(&[&contacts, &[RECORDS]]).unwrap().children {
            let first_name = crate::recalc::texts(record).remove(&1).unwrap_or_default();
            // The record as a client asking for every field but First Name
            // (field 1) gets it.
            let mut asked = record.clone();
            asked.children.retain(|child| child.segment != [1]);
            let fields = asked.children.len();
            let path: [&[u8]; 3] = [&contacts, &[RECORDS], &record.segment];
            let reply = view.filter(read, &path, asked).unwrap().unwrap();
            served.insert(first_name, reply.children.len() == fields);
        }
        assert_eq!(
            served,
            BTreeMap::from([("Allowed".to_owned(), true), ("Denied".to_owned(), false)])
        );
    }
}
