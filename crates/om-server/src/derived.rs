//! Derived state (docs/server-storage.md 5, "Derived state"): what readers
//! need of a hosted file besides its tree, computed from a view of it
//! ([`crate::engine::View`]) and kept by the version it was computed from.
//!
//! [`Derived`] holds the decoded schema catalog, the file's accounts, the
//! file's locale and the order of its stored dates, the calculation schema
//! a server script's record rules and calculations read ([`CalcCatalogs`]),
//! two facts about the file that are not in its tree but that compiling
//! its calculations needs, its name and whether it is served to a desktop
//! client, and one that an export needs, the file's page 0 as it was
//! loaded (`engine::export`). How each engine keeps it is its own: the
//! file engine computes it when it loads a file, reads the catalog and the
//! calculation schema again after each change of the schema, as its store
//! always read the catalog (`Store::load`, `Store::reread_catalog`), and
//! decodes the accounts again when an operation moved the schema
//! generation (`Store::follow_accounts`), and hands the same value to
//! every reader until then. A roll-back puts back the state from before
//! the change whole (`store::Journal::schema_from`).
//!
//! The functions below read a file through a view and derived state: the
//! schema catalogs, the keys an index on demand holds, the next member key
//! of a catalog, and the inputs of a calculation. The file engine's own work
//! on its store calls them with a view of its tree, so readers and writers
//! share one implementation.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::Arc;

use log::warn;
use om_calc::Locale;
use om_collate::{DateOrder, NumberFormat};
use om_hbam::catalog::keys as catalog_keys;
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{
    serial, Catalog, Cell, DataType, FieldKind, KeyTree, OwnedNode, RidList, SlotReader,
};
use om_query::keys::{record_value_entries, value_keys, word_entries, Reading, TextKeys};

use crate::engine::{Direction, ReadError, Version, View, Waiting, NO_LIMIT};
use crate::hosting::FileAccount;
use crate::modcount::{self, MOD_COUNT};
use crate::store::{keys, IssuedKeysAre};

/// The file's locale record `[2, 0d]` (file-metadata.md 2), decoded with
/// the shared decoder every host uses.
pub(crate) const LOCALE: [&[u8]; 2] = [&[2], &[0x0d]];

/// The English text form (query-program.md 7.2): how this server reads an
/// index whose language has no form in OpenMaker yet (the tailored
/// languages, #1279), for a find or a lookup, and the form date-order
/// detection compares under. It writes no key in it for such a language
/// (#159).
pub(crate) const ENGLISH: TextKeys = TextKeys::Collated;

/// A catalog's index `[C, 01]` and, in it, the last issued member id
/// (schema-path.md 5.1).
pub(crate) const CATALOG_INDEX: u8 = 0x01;
pub(crate) const LAST_MEMBER_ID: u8 = 0x00;

/// The counts leaf of an index: `10` for the value index, `11` for the word
/// index (write-path.md 5.5).
pub(crate) fn counts_key(index: u8) -> u8 {
    if index == keys::VALUE_INDEX {
        keys::VALUE_COUNTS
    } else {
        keys::WORD_COUNTS
    }
}

/// A file's accounts as [`read_accounts`] decodes them, in key order, or
/// why they could not be read. The two differ: an error is a read of the
/// security catalog that failed (the view's); an account record that was
/// read but does not decode is left out, or holds `None` for a part that
/// does not (its kind, status or privilege set) and an unavailable
/// password for a verifier this server cannot check. Shared: replacing
/// them copies none.
pub type Accounts = Result<Arc<[Arc<FileAccount>]>, ReadError>;

/// What readers need of a file besides its tree (the module doc).
#[derive(Clone, Debug)]
pub struct Derived {
    /// The load of the file it was computed for ([`Derived::instance`]).
    instance: u64,
    /// The version of the view it was computed from.
    version: Version,
    /// Shared, so that replacing the accounts copies no catalog.
    catalog: Arc<Catalog>,
    /// The accounts of the file's security catalog ([`read_accounts`]).
    /// Accounts that could not be read refuse every login, as neither a
    /// wrong password nor a success (docs/server-storage.md 5, "Views");
    /// they are not kept from an earlier version, which could let in an
    /// account since deleted or changed.
    accounts: Accounts,
    /// The file's locale and formats (`[2, 0d]`), when it decodes.
    locale: Option<Locale>,
    /// The file has a locale record that does not decode: its language is
    /// unknown ([`Derived::file_language`]).
    locale_unreadable: bool,
    /// The calculation schema of a server script's record rules and
    /// calculations ([`CalcCatalogs`]); `None` when its catalogs cannot be
    /// read into one.
    calc: Option<Arc<CalcCatalogs>>,
    /// The order of the parts of the file's stored dates (om-collate).
    date_order: DateOrder,
    /// The file's name, as calculations see it (`Get ( FileName )`).
    name: String,
    /// Served privately to a desktop client ([`crate::LocalHost`]): its
    /// calculations are FileMaker Pro's, which has the location functions
    /// FileMaker Server's engine lacks (`om_data::host::device`).
    desktop: bool,
    /// The file's page 0 as it was loaded ([`Derived::page_zero`]); empty
    /// until the engine sets it.
    page_zero: Arc<[u8]>,
}

impl Derived {
    /// The state of the file named `name` at `view`'s version, its schema
    /// `catalog` read already: its accounts ([`read_accounts`]), the
    /// locale its record `[2, 0d]` gives, and the date order of that
    /// locale, or for a file without one the order its date indexes are
    /// keyed in ([`detect_date_order`]). Accounts that cannot be read are
    /// kept as such ([`Derived::accounts`]).
    pub fn new(view: &dyn View, catalog: Catalog, name: &str) -> Result<Self, ReadError> {
        Self::with_parts(view, catalog, read_accounts(view), name)
    }

    /// [`Derived::new`] with `accounts` given, not read from `view` (the
    /// test view's, which reads them only for a reader of them,
    /// `engine::copies`).
    pub(crate) fn with_parts(
        view: &dyn View,
        catalog: Catalog,
        accounts: Accounts,
        name: &str,
    ) -> Result<Self, ReadError> {
        let record = view.subtree(&LOCALE, NO_LIMIT)?;
        let locale = record
            .as_ref()
            .and_then(|node| node.assembled().map(Cow::into_owned))
            .and_then(|record| Locale::from_record(&record));
        let locale_unreadable = locale.is_none() && record.is_some();
        let date_order = match &locale {
            Some(locale) => locale.date_order,
            None => {
                warn!("{name}: no file locale record [2, 0d] this server reads; assuming en-US formats");
                detect_date_order(view, &catalog, name)?
            }
        };
        // Each state computed here starts a load of its own.
        static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut derived = Derived {
            instance: LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            version: view.version(),
            catalog: Arc::new(catalog),
            accounts,
            locale,
            locale_unreadable,
            calc: None,
            date_order,
            name: name.to_string(),
            desktop: false,
            page_zero: Arc::from(&[][..]),
        };
        derived.calc = CalcCatalogs::read(view, &derived)?;
        Ok(derived)
    }

    /// This state after a change of the schema: `catalog`, read again
    /// through `view`, and the calculation schema with it. The locale and
    /// date order stay the ones the file was loaded with, as the store
    /// always kept them; whether its record is there to read is read again.
    pub fn with_catalog(&self, view: &dyn View, catalog: Catalog) -> Result<Self, ReadError> {
        let mut derived = Derived {
            version: view.version(),
            catalog: Arc::new(catalog),
            locale_unreadable: self.locale.is_none() && view.node(&LOCALE)?.is_some(),
            ..self.clone()
        };
        derived.calc = CalcCatalogs::read(view, &derived)?;
        Ok(derived)
    }

    /// This state with `accounts`, read again at `version`.
    pub fn with_accounts(&self, version: Version, accounts: Accounts) -> Self {
        Derived {
            version,
            accounts,
            ..self.clone()
        }
    }

    /// The file is served privately to a desktop client.
    pub(crate) fn serve_desktop(&mut self) {
        self.desktop = true;
    }

    /// The file's page 0 as it was loaded is `page`.
    pub(crate) fn set_page_zero(&mut self, page: &[u8]) {
        self.page_zero = Arc::from(page);
    }

    /// The file's page 0 as it was loaded: its header, which no write of
    /// the tree changes, with the clean-close flag and the change number it
    /// was loaded with (hbam-writer.md 5.1). An export writes it with the
    /// fields a writer maintains set as a save sets them
    /// (`engine::export`); empty for a store made without a file.
    pub fn page_zero(&self) -> &[u8] {
        &self.page_zero
    }

    /// The version of the view this state was computed from.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Which load of the file this state belongs to, a number no other
    /// state computed by [`Derived::new`] has; [`Derived::with_catalog`]
    /// keeps it. Versions compare only within one load: a file closed and
    /// opened again is a new store whose versions start over.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// The file's schema.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The file's schema, shared.
    pub fn shared_catalog(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog)
    }

    /// The calculation schema of a server script's record rules and
    /// calculations; `None` when its catalogs could not be read into one.
    pub fn calc_catalogs(&self) -> Option<&Arc<CalcCatalogs>> {
        self.calc.as_ref()
    }

    /// The file's own language: its locale record's, en-US's for a file
    /// without one; `None` when it has one that does not decode.
    pub fn file_language(&self) -> Option<u32> {
        (!self.locale_unreadable).then(|| u32::from(self.locale().language.id()))
    }

    /// The file's accounts, or why they could not be read.
    pub fn accounts(&self) -> &Accounts {
        &self.accounts
    }

    /// The file's account named `name`, matched as FileMaker matches
    /// account names ([`crate::hosting::account_named`]): `None` for none,
    /// also for one whose record does not decode; an error when the
    /// security catalog could not be read ([`Accounts`]).
    pub fn account(&self, name: &str) -> Result<Option<Arc<FileAccount>>, ReadError> {
        let accounts = self.accounts.as_ref().map_err(ReadError::clone)?;
        Ok(crate::hosting::account_named(accounts, name))
    }

    /// The file's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the file is served privately to a desktop client.
    pub fn desktop(&self) -> bool {
        self.desktop
    }

    /// The file's locale and formats: its record when it decodes, else
    /// en-US with the date order its stored dates use.
    pub fn locale(&self) -> Locale {
        self.locale.clone().unwrap_or_else(|| {
            let mut locale = Locale::en_us();
            locale.date_order = self.date_order;
            locale
        })
    }

    /// The order of the parts of the file's stored dates.
    pub fn date_order(&self) -> DateOrder {
        self.date_order
    }

    /// How the file's stored numbers and dates are read for their index
    /// keys (query-program.md 7.3): its date order, and its locale's decimal
    /// and thousands separators (en-US when it records no locale).
    pub fn reading(&self) -> Reading {
        let numbers = self
            .locale
            .as_ref()
            .map(|l| NumberFormat {
                decimal: l.decimal_separator,
                thousands: l.thousands_separator,
            })
            .unwrap_or_default();
        Reading {
            order: self.date_order,
            numbers,
        }
    }

    /// Whether `table` (a one-segment key) is a base table of the file.
    pub fn has_table(&self, table: &[u8]) -> bool {
        be_uint(table).is_some_and(|id| self.catalog.table(id).is_some())
    }

    /// The data type of `field` in `table`, if the field exists.
    pub fn field_type(&self, table: &[u8], field: &[u8]) -> Option<DataType> {
        let table = self.catalog.table(be_uint(table)?)?;
        Some(table.field(be_uint(field)?)?.data_type)
    }

    /// Whether `cell` of a record of `table` is a container field's value
    /// (repetition n of field F is the cell `F * 256 + n`).
    pub fn container_cell(&self, table: &[u8], cell: &[u8]) -> bool {
        let Some(id) = be_uint(cell) else {
            return false;
        };
        let field = match self.field_type(table, cell) {
            Some(_) => id,
            None if id & 0xff >= 2 => id >> 8,
            None => return false,
        };
        self.field_type(table, &uint_segment(field)) == Some(DataType::Container)
    }

    /// How many repetitions `field` of `table` has (1 when unknown).
    pub fn repetitions(&self, table: &[u8], field: &[u8]) -> u16 {
        self.defined_repetitions(table, field)
            .map_or(1, |n| n.max(1))
    }

    /// The repetitions `field` of `table`'s definition gives, if it reads.
    pub fn defined_repetitions(&self, table: &[u8], field: &[u8]) -> Option<u16> {
        be_uint(table)
            .and_then(|t| self.catalog.table(t))
            .and_then(|t| t.field(be_uint(field)?))
            .and_then(|f| f.decoded_options())
            .map(|o| o.max_repetitions)
    }

    /// The cell keys of `field`'s repetitions 2 to 255 in a record of
    /// `table` or among its global values: `field` followed by the byte n
    /// (calc-values.md, "Repetitions in records"), but none that is
    /// another field's own key, in the catalog or in `others` (fields the
    /// same work names, which may be gone from the catalog).
    pub fn repetition_cells(
        &self,
        table: &[u8],
        field: &[u8],
        others: &BTreeSet<Vec<u8>>,
    ) -> BTreeSet<Vec<u8>> {
        (2u8..=255)
            .map(|n| [field, &[n]].concat())
            .filter(|cell| self.field_type(table, cell).is_none() && !others.contains(cell))
            .collect()
    }

    /// The value-index keys of `text` in `field` of `table`, a field of
    /// `data_type`, as this file's indexes hold them (query-program.md 7.2,
    /// 7.3).
    pub fn value_keys(
        &self,
        table: &[u8],
        field: &[u8],
        data_type: DataType,
        text: &str,
    ) -> Vec<Vec<u8>> {
        value_keys(
            data_type,
            text,
            self.reading(),
            self.read_text_keys(table, field),
        )
    }

    /// How `field` of `table` keys text in its indexes: the form of its
    /// index language (query-program.md 7.2,
    /// [`om_data::rules::written_text_keys`]), so that a Unicode raw index
    /// keeps the code units FileMaker keys it with. Language 0 is the
    /// file's own. `None` for a language with no form in OpenMaker yet,
    /// whose keys this server does not write (#159, #1279).
    pub fn text_keys(&self, table: &[u8], field: &[u8]) -> Option<TextKeys> {
        let file_language = self
            .locale
            .as_ref()
            .map_or(21, |l| u32::from(l.language.id()));
        let language = be_uint(table)
            .and_then(|t| self.catalog.table(t))
            .and_then(|t| t.field(be_uint(field)?))
            .and_then(|f| f.decoded_options())
            .map(|o| o.index_language())?;
        om_data::rules::written_text_keys(language, file_language)
    }

    /// [`Self::text_keys`] for reading an index (a find, a lookup): a
    /// language with no form is read in English, which keys most text as
    /// the tailorings do; nothing read so is written.
    pub fn read_text_keys(&self, table: &[u8], field: &[u8]) -> TextKeys {
        self.text_keys(table, field).unwrap_or(ENGLISH)
    }

    /// The field `field` of `table` when its index `kind` can be built on
    /// demand: a stored field that does not prohibit indexing, of a type
    /// with that index (schema-path.md 5.5).
    pub fn indexable(&self, table: &[u8], field: &[u8], kind: u8) -> Option<DataType> {
        const GLOBAL: u32 = 0x1_0000;
        const UNSTORED: u32 = 0x2_0000;
        const PREVENT_INDEX: u32 = 0x4000_0000;
        let field = self
            .catalog
            .table(be_uint(table)?)?
            .field(be_uint(field)?)?;
        let options = u32::from_be_bytes(field.options.get(8..12)?.try_into().ok()?);
        let stored = matches!(field.kind, FieldKind::Normal | FieldKind::Calculation)
            && options & (GLOBAL | UNSTORED | PREVENT_INDEX) == 0;
        let typed = match field.data_type {
            DataType::Text => true,
            DataType::Number | DataType::Date | DataType::Time | DataType::Timestamp => {
                kind == keys::VALUE_INDEX
            }
            DataType::Container | DataType::Unknown(_) => false,
        };
        (stored && typed && (kind == keys::VALUE_INDEX || kind == keys::WORD_INDEX))
            .then_some(field.data_type)
    }

    /// The catalog, to change in a test.
    #[cfg(test)]
    pub(crate) fn catalog_mut(&mut self) -> &mut Catalog {
        Arc::make_mut(&mut self.catalog)
    }

    /// Whether `other` is the same state, whatever version each was
    /// computed at, its accounts too when `accounts` (the test view's
    /// check, `engine::copies`). Passwords compare as they are stored;
    /// their `Debug` form, in the check's message, is redacted
    /// (`om_crypto::account::StoredPassword`).
    #[cfg(debug_assertions)]
    pub(crate) fn same_as(&self, other: &Derived, accounts: bool) -> bool {
        self.catalog == other.catalog
            && (!accounts || self.accounts == other.accounts)
            && self.locale == other.locale
            && self.locale_unreadable == other.locale_unreadable
            && self.calc == other.calc
            && self.date_order == other.date_order
            && self.name == other.name
            && self.desktop == other.desktop
            && self.page_zero == other.page_zero
    }

    /// Whether `other` holds the catalog this state holds, the same one
    /// and not only an equal one: whether it was read again between them
    /// (the test view's, `engine::copies`).
    #[cfg(debug_assertions)]
    pub(crate) fn shares_catalog(&self, other: &Derived) -> bool {
        Arc::ptr_eq(&self.catalog, &other.catalog)
    }
}

/// The calculation schema of a server script's record rules and
/// calculations (`scripts::host`): the catalogs they read, the schema `[3]`
/// (occurrences, relationships), the custom functions `[16]`, the value
/// lists `[21]` and each table's field catalog `[T, 03]` (without its
/// deferred work and its fields' next serial values, which change without a
/// change of the schema), as one slot list, and the schema compiled from
/// them with the file's locale, without the
/// functions FileMaker Server's engine lacks
/// (`om_data::host::device::withhold_server_lacks`). Computed with the rest
/// of the derived state and kept by its version, so every session of the
/// version shares one, and a roll-back puts back the one before.
pub struct CalcCatalogs {
    /// The catalogs as one slot list, which a table's record rules decode
    /// again (`om_data::rules::RecordPlan::from_file_schema`).
    pub wire: Vec<u8>,
    pub schema: Arc<om_data::host::CalcSchema>,
}

impl CalcCatalogs {
    /// The calculation schema of the file `view` reads, whose state is
    /// `derived` (its catalog and locale); `None` when its catalogs cannot
    /// be written as a slot list or read back.
    fn read(view: &dyn View, derived: &Derived) -> Result<Option<Arc<Self>>, ReadError> {
        let mut children: Vec<OwnedNode> = Vec::new();
        for key in [3u8, 16, 0x21] {
            if let Some(node) = view.subtree(&[&[key]], NO_LIMIT)? {
                children.push(node.into_owned());
            }
        }
        for table in &derived.catalog.tables {
            let t = uint_segment(table.id);
            if let Some(fields) = view.subtree(&[&t, &[keys::FIELDS]], NO_LIMIT)? {
                let mut fields = fields.into_owned();
                // What changes without a change of the schema, and which
                // neither the record rules nor the calculations read: the
                // deferred field work (op73 finishes it) and each field's
                // next serial value (op72 advances it; the Design
                // functions read the stored one, `scripts::host::design`).
                // Left out, the schema stays the same between changes of
                // the schema, whenever it is computed.
                fields.remove(&[&[CATALOG_INDEX], &[keys::DEFERRED]]);
                if let Some(definitions) = fields.child_mut(&[keys::FIELD_DEFINITIONS]) {
                    for definition in &mut definitions.children {
                        definition.remove(&[&[keys::SERIAL_VALUE]]);
                    }
                }
                children.push(OwnedNode::list(t, vec![fields]));
            }
        }
        let Some(wire) = OwnedNode::list(Vec::new(), children).to_wire() else {
            return Ok(None);
        };
        let Ok(tree) = KeyTree::from_slots(SlotReader::new(&wire)) else {
            return Ok(None);
        };
        let name = derived.name();
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        let mut schema = om_data::rules::schema::calc_schema(&tree, &derived.catalog, stem);
        schema.locale = derived.locale();
        // The record rules refuse a unique check in language 0 when the
        // file's language is unknown.
        schema.locale_unreadable = derived.locale_unreadable;
        om_data::host::device::withhold_server_lacks(schema.names_mut());
        drop(tree);
        Ok(Some(Arc::new(CalcCatalogs {
            wire,
            schema: Arc::new(schema),
        })))
    }
}

/// Two are the same when their catalogs are: the schema is compiled from
/// them alone, with the state that holds them.
impl PartialEq for CalcCatalogs {
    fn eq(&self, other: &Self) -> bool {
        self.wire == other.wire
    }
}

impl std::fmt::Debug for CalcCatalogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalcCatalogs")
            .field("wire", &self.wire.len())
            .finish_non_exhaustive()
    }
}

/// The accounts of the file `view` reads ([`crate::hosting::read_accounts`]).
pub fn read_accounts(view: &dyn View) -> Accounts {
    crate::hosting::read_accounts(view).map(|accounts| accounts.into_iter().map(Arc::new).collect())
}

/// The schema catalogs of the tree `view` reads: the catalogs `[3]` and
/// layouts `[4]`, and each table's field catalog `[T, 03]`.
pub fn read_catalog(view: &dyn View) -> om_hbam::Result<Catalog> {
    let mut schema = Vec::new();
    for child in view.children(&[], Bound::Unbounded, Direction::Forward) {
        let segment = child?.segment;
        match be_uint(&segment) {
            Some(catalog_keys::SCHEMA | catalog_keys::LAYOUTS) => {
                if let Some(node) = view.subtree(&[&segment], NO_LIMIT)? {
                    schema.push(node.into_owned());
                }
            }
            Some(table) if table >= 0x8000 => {
                let fields = [catalog_keys::TABLE_FIELDS as u8];
                if let Some(node) = view.subtree(&[&segment, &fields], NO_LIMIT)? {
                    schema.push(OwnedNode::list(segment.to_vec(), vec![node.into_owned()]));
                }
            }
            _ => {}
        }
    }
    let wire = OwnedNode::list(Vec::new(), schema)
        .to_wire()
        .ok_or(om_hbam::Error::Write(
            "the schema has a key that cannot be written",
        ))?;
    Catalog::from_tree(&KeyTree::from_slots(SlotReader::new(&wire))?)
}

/// The order the file stores dates in: the one whose keys match the
/// existing date and timestamp indexes, else month first.
fn detect_date_order(
    view: &dyn View,
    catalog: &Catalog,
    name: &str,
) -> Result<DateOrder, ReadError> {
    let orders = [
        DateOrder::MonthDayYear,
        DateOrder::DayMonthYear,
        DateOrder::YearMonthDay,
    ];
    let mut matches = [0usize; 3];
    for table in &catalog.tables {
        let t = uint_segment(table.id);
        for field in &table.fields {
            if !matches!(field.data_type, DataType::Date | DataType::Timestamp) {
                continue;
            }
            let f = uint_segment(field.id);
            let index: [&[u8]; 4] = [&t, &[keys::INDEXES], &f, &[keys::VALUE_INDEX]];
            if view.node(&index)?.is_none() {
                continue;
            }
            for (i, order) in orders.iter().enumerate() {
                for key in view.subtrees(&index, Bound::Unbounded, Direction::Forward, NO_LIMIT) {
                    let key = key?;
                    let Ok(records) = RidList::from_owned(&key) else {
                        continue;
                    };
                    let Some(id) = records.iter().next() else {
                        continue;
                    };
                    let value = stored_text(view, &t, id, &f)?.unwrap_or_default();
                    if value_keys(field.data_type, &value, *order, ENGLISH).contains(&key.segment) {
                        matches[i] += 1;
                    }
                }
            }
        }
    }
    let best = (0..3).max_by_key(|&i| matches[i]).expect("three orders");
    if matches[best] == 0 {
        return Ok(DateOrder::default());
    }
    if matches.iter().filter(|&&m| m == matches[best]).count() > 1 {
        warn!(
            "{name}: stored date order is ambiguous; assuming {:?}",
            orders[best]
        );
    }
    Ok(orders[best])
}

/// A record's stored value of a field as text, if it stores one.
fn stored_text(
    view: &dyn View,
    table: &[u8],
    record: u64,
    field: &[u8],
) -> Result<Option<String>, ReadError> {
    let record = serial::encode(record);
    Ok(view
        .subtree(&[table, &[keys::RECORDS], &record, field], NO_LIMIT)?
        .and_then(|node| cell_text(&node)))
}

/// A stored value as text (XOR 0x5A + SCSU), if it is a single value.
pub(crate) fn cell_text(node: &OwnedNode) -> Option<String> {
    Cell::from_owned(node).text()?.ok()
}

/// The texts of `field`'s repetitions in a record: the first under the
/// field's key, repetition n under `F * 256 + n` (execute-sql.md, reading
/// `.fmp12` files); empty for a repetition the record does not store. The
/// field id comes from the file: for one above `u64::MAX / 256` no
/// repetition but the first has a cell key, so the others read as empty
/// (#361).
pub(crate) fn field_texts(record: &OwnedNode, field: &[u8], repetitions: u16) -> Vec<String> {
    let text = |key: &[u8]| record.child(key).and_then(cell_text).unwrap_or_default();
    let mut texts = vec![text(field)];
    if let Some(id) = be_uint(field).filter(|_| repetitions > 1) {
        let base = id.checked_mul(256);
        for n in 2..=repetitions.min(255) {
            // `base + n` cannot overflow: `base` is at most `u64::MAX - 255`.
            let key = base.map(|base| uint_segment(base + u64::from(n)));
            texts.push(key.map(|key| text(&key)).unwrap_or_default());
        }
    }
    texts
}

/// The form `field` of `table`, of `data_type`, is written in when its
/// index is built and stored: its language's ([`Derived::text_keys`]), or
/// for a text field whose language has none, `None` while a record holds
/// text in it (no key would be FileMaker's) and any form while none does
/// (no key at all). The form keys only text.
pub(crate) fn written_form(
    view: &dyn View,
    derived: &Derived,
    table: &[u8],
    field: &[u8],
    data_type: DataType,
) -> Result<Option<TextKeys>, ReadError> {
    if data_type != DataType::Text {
        return Ok(Some(ENGLISH));
    }
    if let Some(form) = derived.text_keys(table, field) {
        return Ok(Some(form));
    }
    let repetitions = derived.repetitions(table, field);
    let records: [&[u8]; 2] = [table, &[keys::RECORDS]];
    for record in view.subtrees(&records, Bound::Unbounded, Direction::Forward, NO_LIMIT) {
        let record = record?;
        if field_texts(&record, field, repetitions)
            .iter()
            .any(|text| !text.is_empty())
        {
            return Ok(None);
        }
    }
    Ok(Some(ENGLISH))
}

/// The keys of `field`'s value or word index over every record of `table`,
/// each with the records holding it (query-program.md 7.2, 7.3); with
/// `accounted`, `None` when a record holds a value in the field that the
/// current rules give no value-index key, which another rule (FileMaker's)
/// may have given one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn index_entries(
    view: &dyn View,
    derived: &Derived,
    table: &[u8],
    field: &[u8],
    data_type: DataType,
    kind: u8,
    form: TextKeys,
    accounted: bool,
) -> Result<Option<BTreeMap<Vec<u8>, RidList>>, ReadError> {
    let mut index: BTreeMap<Vec<u8>, RidList> = BTreeMap::new();
    let repetitions = derived.repetitions(table, field);
    let reading = derived.reading();
    let records: [&[u8]; 2] = [table, &[keys::RECORDS]];
    for record in view.subtrees(&records, Bound::Unbounded, Direction::Forward, NO_LIMIT) {
        let record = record?;
        let Some(id) = serial::decode(&record.segment) else {
            continue;
        };
        let texts = field_texts(&record, field, repetitions);
        let keys: BTreeSet<Vec<u8>> = if kind == keys::VALUE_INDEX {
            record_value_entries(data_type, &texts, reading, form)
        } else {
            word_entries(&texts, form)
        };
        if accounted
            && kind == keys::VALUE_INDEX
            && keys.is_empty()
            && texts.iter().any(|t| !t.trim().is_empty())
        {
            return Ok(None);
        }
        for key in keys {
            index.entry(key).or_default().insert(id);
        }
    }
    Ok(Some(index))
}

/// Build a missing index from the stored records for a query that reads
/// it, and note it in `waiting` as due to be stored (`Store::store_indexes`
/// stores it afterwards, under the store's write lock). A field whose
/// index exists but has no keys (only its counts) reads as empty.
pub(crate) fn index_on_demand(
    view: &dyn View,
    derived: &Derived,
    waiting: &Waiting,
    table: &[u8],
    field: &[u8],
    kind: u8,
) -> Result<Option<OwnedNode>, om_query::QueryError> {
    let counts: [&[u8]; 4] = [table, &[keys::INDEXES], field, &[counts_key(kind)]];
    if view.node(&counts).map_err(om_hbam::Error::from)?.is_some() {
        return Ok(Some(OwnedNode::list(vec![kind], Vec::new())));
    }
    let Some(data_type) = derived.indexable(table, field, kind) else {
        return Ok(None);
    };
    // A text field in an index language whose keys this server does
    // not build, holding text: its index would hold keys OpenMaker
    // cannot make, and English ones would answer the query with
    // another found set (a German `Äpfel` criterion missing records,
    // French words broken at the wrong places). The query is refused,
    // as the desktop refuses a find without the host's index (#572).
    let form =
        written_form(view, derived, table, field, data_type).map_err(om_hbam::Error::from)?;
    let Some(form) = form else {
        warn!(
            "{}: a query needs an index this server cannot build in its field's language; \
             refused",
            derived.name()
        );
        return Err(om_query::QueryError::UnkeyedIndex);
    };
    let index = index_entries(view, derived, table, field, data_type, kind, form, false)
        .map_err(om_hbam::Error::from)?
        .unwrap_or_default();
    waiting.note_index((table.to_vec(), field.to_vec(), kind));
    let entries = index
        .into_iter()
        .map(|(key, ids)| ids.to_node(key))
        .collect();
    Ok(Some(OwnedNode::list(vec![kind], entries)))
}

/// The path of the first node along `path`, the node at `path` included,
/// that holds a value (`om_hbam::writer::value_on_path`, read through a
/// view): a value of its own, or one split into pieces below it (a list
/// with no value of its own whose first child is the header `0` of a
/// chunked value, `OwnedNode::assembled`), which only then is read whole.
pub(crate) fn value_on_path(
    view: &dyn View,
    path: &[&[u8]],
) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
    for depth in 1..=path.len() {
        let at = &path[..depth];
        let Some(head) = view.node(at)? else {
            return Ok(None);
        };
        let holds = match &head.value {
            Some(value) => !value.is_empty(),
            None if head.list => {
                let first = view
                    .children(at, Bound::Unbounded, Direction::Forward)
                    .next();
                match first.transpose()? {
                    Some(child)
                        if child.segment.as_ref() == [0]
                            && child
                                .head
                                .value
                                .as_ref()
                                .is_some_and(|v| matches!(v.len(), 4 | 8 | 12)) =>
                    {
                        view.subtree(at, NO_LIMIT)?
                            .is_some_and(|node| om_hbam::writer::holds_value(&node))
                    }
                    _ => false,
                }
            }
            None => false,
        };
        if holds {
            return Ok(Some(at.iter().map(|s| s.to_vec()).collect()));
        }
    }
    Ok(None)
}

/// Why `[catalog, 01, 00]` and `[catalog, 01, fc]` (a catalog's last
/// member id and count, schema-path.md 3.4) cannot be written in the
/// tree `view` reads (`om_hbam::writer::put_refusals`), for a log line.
pub(crate) fn member_key_refusal(
    view: &dyn View,
    catalog: &[&[u8]],
) -> Result<Option<String>, ReadError> {
    let mut parents = catalog.to_vec();
    parents.push(&[CATALOG_INDEX]);
    let refused = match value_on_path(view, &parents)? {
        Some(path) => vec![om_hbam::writer::Unwritable {
            path,
            reason: "a value cannot become a list",
        }],
        None => {
            let leaf = OwnedNode::leaf(vec![MOD_COUNT], vec![0; 10]);
            om_hbam::writer::unwritable(&parents, &leaf, 3)
        }
    };
    Ok((!refused.is_empty()).then(|| crate::store::describe(&refused)))
}

/// The last member id and modification count the index of the catalog at
/// `path` holds (`[C, 01, 00]`, `[C, 01, fc]`; 0 when absent).
pub(crate) fn stored_member_key(view: &dyn View, path: &[&[u8]]) -> Result<(u64, u64), ReadError> {
    let index = [CATALOG_INDEX];
    let stored = |key: u8| -> Result<u64, ReadError> {
        let key = [key];
        let mut at = path.to_vec();
        at.extend([&index[..], &key[..]]);
        Ok(view
            .node(&at)?
            .and_then(|head| head.value)
            .and_then(|value| modcount::parse(&value))
            .unwrap_or(0))
    };
    Ok((stored(LAST_MEMBER_ID)?, stored(MOD_COUNT)?))
}

/// GenerateMemberKey (schema-path.md 3.4): issue the catalog's next member
/// id and modification count, one more than its last issued id
/// `[C, 01, 00]` and count `[C, 01, fc]` (or than what `waiting` says was
/// issued since), and note them there as issued. The tree is only read
/// here; op86 writes them (`engine::schema`). `None` for a catalog the
/// tree cannot take them under as it is, and past the largest a serial key
/// holds.
pub(crate) fn issue_member_key(
    view: &dyn View,
    waiting: &Waiting,
    name: &str,
    catalog: &[Vec<u8>],
) -> Result<Option<(u64, u64)>, ReadError> {
    let path: Vec<&[u8]> = catalog.iter().map(Vec::as_slice).collect();
    if member_key_refusal(view, &path)?.is_some() {
        warn!("{name}: no member key for the catalog {path:02x?}, which the file cannot hold");
        return Ok(None);
    }
    let stored = stored_member_key(view, &path)?;
    Ok(waiting.issue_member_key(catalog, stored))
}

/// What became of the member keys a request's GenerateMemberKey issued
/// (each catalog with its id), asked once the request submitted its own
/// write (docs/server-storage.md 5, "Keys a request issued"): the file
/// takes no more changes, for `refused` (a save's outcome was unknown, the
/// one that wrote them or a later one), one of them is not in its
/// catalog's index (it waits, because a save failed and it waits again or
/// because the file became read-only, or a write dropped it, its catalog
/// no longer able to take it), or they are stored.
pub(crate) fn issued_keys<'r>(
    view: &dyn View,
    refused: Option<&'r str>,
    issued: &[(Vec<Vec<u8>>, u64)],
) -> Result<IssuedKeysAre<'r>, ReadError> {
    if let Some(why) = refused {
        return Ok(IssuedKeysAre::Unknown(why));
    }
    for (catalog, id) in issued {
        let path: Vec<&[u8]> = catalog.iter().map(Vec::as_slice).collect();
        // The index records the last id issued: every id up to it is taken.
        if stored_member_key(view, &path)?.0 < *id {
            return Ok(IssuedKeysAre::NotStored);
        }
    }
    Ok(IssuedKeysAre::Stored)
}

/// `f` of the catalogs a calculation of `table` reads, as a key tree
/// (occurrences and relationships `[3]`, custom functions `[16]`, the
/// table's fields `[T, 03]`), and of the file's calculation schema; `None`
/// when they cannot be read.
pub(crate) fn with_calc_inputs<R>(
    view: &dyn View,
    derived: &Derived,
    table: &[u8],
    f: impl FnOnce(&KeyTree<'_>, &om_data::host::CalcSchema) -> R,
) -> Result<Option<R>, ReadError> {
    let mut parts: Vec<OwnedNode> = Vec::new();
    for key in [3u8, 16] {
        if let Some(node) = view.subtree(&[&[key]], NO_LIMIT)? {
            parts.push(node.into_owned());
        }
    }
    if let Some(fields) = view.subtree(&[table, &[keys::FIELDS]], NO_LIMIT)? {
        parts.push(OwnedNode::list(table.to_vec(), vec![fields.into_owned()]));
    }
    let wire = OwnedNode::list(Vec::new(), parts).to_wire();
    let Some(tree) = wire
        .as_deref()
        .and_then(|w| KeyTree::from_slots(SlotReader::new(w)).ok())
    else {
        return Ok(None);
    };
    let name = derived.name();
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let mut calc = om_data::rules::schema::calc_schema(&tree, derived.catalog(), stem);
    calc.locale = derived.locale();
    if !derived.desktop() {
        om_data::host::device::withhold_server_lacks(calc.names_mut());
    }
    Ok(Some(f(&tree, &calc)))
}

/// The evaluator of a custom privilege set's formula `code` limiting a
/// record action on base table `table` (its key); why not, when the table
/// or its catalogs cannot be read or the formula cannot be evaluated here
/// (`RecordLimit::new`).
pub(crate) fn record_limit(
    view: &dyn View,
    derived: &Derived,
    table: &[u8],
    code: &[u8],
) -> Result<Result<om_data::rules::recalc::RecordLimit, String>, ReadError> {
    let Some(id) = be_uint(table) else {
        return Ok(Err("the table key does not decode.".into()));
    };
    let Some(definition) = derived.catalog().table(id) else {
        return Ok(Err("the table is not in the catalog.".into()));
    };
    let limit = with_calc_inputs(view, derived, table, |tree, calc| {
        om_data::rules::recalc::RecordLimit::new(definition, tree, calc, code, None)
    })?;
    Ok(limit.unwrap_or_else(|| Err("the field catalog could not be read.".into())))
}
