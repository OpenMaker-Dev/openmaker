//! Calculations on the hosted file's data (calc-values.md 8.6-8.8): the
//! file's [`CalcSchema`] from its downloaded catalogs
//! (`om_data::rules::schema`), a [`CalcSource`] that downloads
//! records, record lists, related tables and global values with the same
//! op90/op92 paths as record pages, and the values of unstored
//! calculations, summaries and global fields in record pages.
//!
//! ExecuteSQL (`om_data::host::sql`) reads through the same source: a
//! found set's records in batches of [`SCAN_BATCH`] per op92, and
//! `field = value` as an op86 SimpleFind for the whole value
//! (query-program.md 6.4) where the field's value index can answer it.
//!
//! The tables of related files (external.rs) are read the same way on the
//! related file's own connection, under the table's id in that file: their
//! client-only ids never reach a connection.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use om_data::calc::eval::{display, StoredContainer, Variables};
use om_data::calc::{CalcError, Ctx, Date, Value, ValueType};
use om_data::containers::MemberInfo;
use om_data::host::{
    stored_value, CalcCancel, CalcData, CalcNeed, CalcSchema, CalcSource, FieldClass, FieldDef,
    FileCalc, FileHost, LookupKey, RecordValues, Recorder, SessionInfo, TableDef,
};
pub(crate) use om_data::rules::schema::calc_schema;
use om_data::{DataError, Record, RecordPage};
use om_hbam::{Cell, Node};
use om_session::Session;

use crate::library::LibraryCache;
use crate::{
    download_node, download_records, download_tree, key_bytes, record_ids, NativeProvider,
};

mod demand;
pub(crate) use demand::rule_reads;
mod related;

/// Soft limits for the provider's working set, which stays across pages and
/// batches until the data changes (#1061). An oversized evaluation still
/// returns its complete answer; subsequent batches reread. The id budget
/// holds the record lists calculations read (scans, `×` relationships,
/// found sets; the index path read them for creation order until #1100):
/// about 1.6 million ids for one record of a measured solution, of 8 bytes
/// each (#1067).
const PAGE_RECORDS: usize = 16_384;
const PAGE_IDS: usize = 4_194_304;
const PAGE_SETS: usize = 1024;

fn bounded_page(data: CalcData) -> CalcData {
    // A list several keys share is held once: the window's found set is
    // stored under every occurrence of its table (#1067).
    let mut held = HashSet::new();
    let ids: usize = data
        .tables
        .values()
        .chain(data.found.values())
        .chain(data.related_ids.values())
        .chain(data.lookups.values().flatten())
        .filter(|ids| held.insert(Arc::as_ptr(ids).cast::<u64>()))
        .map(|ids| ids.len())
        .sum();
    if data.records.len() > PAGE_RECORDS
        || ids > PAGE_IDS
        || data.lookups.len() > PAGE_SETS
        || data.related_ids.len() > PAGE_SETS
        || data.globals.len() > PAGE_SETS
    {
        CalcData::default()
    } else {
        data
    }
}

/// The table a record page is read from: its record list and the window's
/// found set, and what calculations read before that is still valid
/// (#1042), the start of the page's calculation data.
pub(crate) struct PageSets<'a> {
    pub all: &'a [u64],
    pub found: &'a [u64],
    pub kept: CalcData,
}

/// A table's global values `[T, 08, F]` (calc-values.md implementation notes).
const GLOBALS: u8 = 0x08;
/// Custom functions `[16, 5, id]` (calc-encoding.md 8).
const CUSTOM_FUNCTIONS: u64 = 16;
/// The file's metadata (file-metadata.md): its locale `[2, 0d]`, document
/// preferences `[2, 18]`, value lists `[21]` and scripts `[17]`.
const FILE: u64 = 2;
const VALUE_LISTS: u64 = 0x21;
const SCRIPTS: u64 = 17;
const LAYOUTS: u64 = 4;
/// External data sources `[32]` (file-metadata.md 1.1).
const DATA_SOURCES: u64 = 0x20;

/// The script catalog's index `[17, 1]` (scripts.md, "The catalog index").
fn script_index() -> Vec<Vec<u8>> {
    vec![key_bytes(SCRIPTS), vec![1]]
}
/// Records an op92 asks for at once while scanning a related table or a
/// query's found set.
const SCAN_BATCH: usize = 500;

/// A cell's text: a plain or chunked value, or styled text, whose chunks
/// sit beside a style subtree `ff` (calc-values.md implementation notes).
pub(crate) fn cell_text(cell: &Cell) -> Option<String> {
    let bytes = match cell {
        Cell::Value(bytes) => std::borrow::Cow::Borrowed(bytes.as_slice()),
        Cell::Nested(entries) => {
            let header = entries
                .iter()
                .find(|(path, _)| path.len() == 1 && path[0] == [0])?
                .1
                .as_ref()?;
            let total = u32::from_be_bytes(*header.first_chunk()?) as usize;
            // The total is the host's to claim: nothing is reserved until
            // the pieces present add up to it (#216).
            let mut pieces = Vec::new();
            for n in 1u64.. {
                let key = om_hbam::serial::encode(n);
                let Some((_, Some(piece))) = entries
                    .iter()
                    .find(|(path, _)| path.len() == 1 && path[0] == key)
                else {
                    break;
                };
                pieces.push(piece.as_slice());
            }
            if pieces.iter().map(|piece| piece.len()).sum::<usize>() != total {
                return None;
            }
            std::borrow::Cow::Owned(pieces.concat())
        }
    };
    om_scsu::decode_scsu(&bytes).ok()
}

fn stored_cell(
    cell: &Cell,
    ty: ValueType,
    cx: &Ctx,
    fonts: &BTreeMap<u64, String>,
) -> Option<om_data::calc::Value> {
    om_layout::StyledText::from_cell(cell)
        .map(|mut text| {
            text.resolve_fonts(fonts);
            om_data::host::styled_value(&text, ty, cx)
        })
        // Keep the existing text fallback for future/unsupported style forms.
        .or_else(|| cell_text(cell).map(|text| stored_value(&text, ty, cx)))
}

/// The key of repetition `rep` of field `field` among a record's cells:
/// the field's own key for repetition 1, its key followed by the
/// repetition byte for 2 to 255 (`0f 02` is field 15, repetition 2).
pub(crate) fn cell_key(field: u64, rep: u32) -> Option<u64> {
    match rep {
        1 => Some(field),
        2..=255 => field.checked_mul(256).map(|k| k + u64::from(rep)),
        _ => None,
    }
}

/// A stored record's values for calculations.
pub(crate) fn record_values(
    table: &TableDef,
    record: &om_hbam::Record,
    cx: &Ctx,
    fonts: &BTreeMap<u64, String>,
) -> RecordValues {
    let mut values = RecordValues {
        mod_count: crate::records::mod_count(record).ok(),
        ..RecordValues::default()
    };
    for def in table.fields.values() {
        if def.global || def.class == FieldClass::Summary || def.is_unstored_calculation() {
            continue;
        }
        for rep in 1..=def.repetitions {
            let Some(cell) = cell_key(def.id, rep).and_then(|k| record.cells.get(&k)) else {
                continue;
            };
            if let Some(value) = stored_cell(cell, def.value_type, cx, fonts) {
                values.set(def.id, rep, value);
            }
        }
    }
    values
}

/// A table's global values from `[T, 08]`, laid out like a record.
fn global_values(
    table: &TableDef,
    node: Option<&Node<'_>>,
    cx: &Ctx,
    fonts: &BTreeMap<u64, String>,
) -> RecordValues {
    let mut values = RecordValues::default();
    let Some(node) = node else {
        return values;
    };
    let cells: std::collections::BTreeMap<u64, Cell> = node
        .children
        .iter()
        .filter_map(|c| Some((c.id()?, Cell::from_node(c))))
        .collect();
    for def in table.fields.values().filter(|d| d.global) {
        for rep in 1..=def.repetitions {
            let Some(cell) = cell_key(def.id, rep).and_then(|k| cells.get(&k)) else {
                continue;
            };
            if let Some(value) = stored_cell(cell, def.value_type, cx, fonts) {
                values.set(def.id, rep, value);
            }
        }
    }
    values
}

/// Whether a layout download goes into the calculation schema's key tree:
/// the layout catalog (whole, its index or a member, #1019) and the fonts.
fn calc_catalog(download: &crate::Download) -> bool {
    download
        .path
        .first()
        .is_some_and(|key| *key == key_bytes(LAYOUTS) || *key == key_bytes(25))
}

/// The provider's calculation state for one connection.
#[derive(Default)]
pub(crate) struct CalcState {
    pub generation: u64,
    pub lost_connections: BTreeSet<[u8; 16]>,
    pub schema: Option<Arc<CalcSchema>>,
    /// Separate own catalogs used to build this schema. None means an optional
    /// read failed, so recovery cannot prove unchanged definitions. Related
    /// owners already retain these in schema_downloads and use Some(empty).
    pub recovery_catalogs: Option<Vec<om_hbam::OwnedNode>>,
    /// The page-scoped calculation working set, reused across client batches.
    pub page: CalcData,
    /// The hosted file's name without its extension, and its host.
    pub file_name: String,
    pub host_name: String,
    /// `Get ( FilePath )` of a file opened from this computer; a hosted
    /// file's is its `fmnet` path.
    pub file_path: Option<String>,
    /// The global field values this session set, as stored text by
    /// (table, field, repetition): in a hosted file a client's global
    /// values are its own, start from the values the file was stored with
    /// and are gone when it closes the file (Claris "Global fields"), so
    /// they are never sent to the host.
    pub globals: SessionGlobals,
    /// Container data read from the file's libraries ([`LibraryCache`]).
    pub library: RefCell<LibraryCache>,
    /// The file's own font families (`[25]`), by the id styled text names
    /// them with. The file the user opened reads them at open, with its
    /// layout catalogs. A related file's open does not read them, so its
    /// provider reads them the first time styled text of its records
    /// names a font (#1098). `None` until then.
    pub fonts: Fonts,
    /// What relationship scans read, kept across calls ([`KeptScans`]).
    pub scans: RefCell<KeptScans>,
    /// While related files open on demand, the opened file's own schema
    /// the calculation schema grows from ([`demand`]).
    pub root: Option<demand::RootSchema>,
    /// The schema grew since the window last asked
    /// ([`om_data::DataProvider::take_grown_schema`]).
    pub grown: bool,
}

/// A file's font families once read ([`CalcState::fonts`]).
pub(crate) type Fonts = RefCell<Option<Arc<BTreeMap<u64, String>>>>;

/// Whether `cell` is styled text one of whose runs names a font: only then
/// is its file's font catalog needed (#1098).
fn names_font(cell: &Cell) -> bool {
    matches!(cell, Cell::Nested(entries)
        if entries.iter().any(|(path, _)| path.first().is_some_and(|key| key == &[0xff])))
        && om_layout::StyledText::from_cell(cell).is_some_and(|text| {
            text.character_runs
                .iter()
                .any(|run| run.style.font_id.is_some())
        })
}

/// A file's font families, read from its font catalog `[25]` on `session`:
/// none when it has no catalog.
pub(crate) fn read_fonts(session: &mut Session) -> Result<BTreeMap<u64, String>, DataError> {
    match download_node(session, vec![key_bytes(25)])? {
        Some(fonts) => Ok(om_data::rules::schema::font_families(&download_tree(
            std::slice::from_ref(&fonts),
        )?)),
        None => Ok(BTreeMap::new()),
    }
}

/// What relationship scans read of each table, by table and target fields
/// ([`om_data::host::ScanRows`]), so the window's next record and its List
/// view rows match against them instead of downloading the table again
/// (#804). They go with the rest of the calculation data
/// ([`NativeProvider::invalidate_calc_data`]): on a change notification of
/// any file of the session, the window's own writes and a lost
/// connection; Refresh Window and Refresh Object drop them when they flush
/// cached join results.
#[derive(Default)]
pub(crate) struct KeptScans {
    rows: HashMap<(u64, Vec<u64>), (om_data::host::ScanRows, usize)>,
    /// Rows and bytes ([`om_data::host::scan_values_bytes`]) kept in all.
    count: usize,
    bytes: usize,
}

/// The most rows and bytes [`KeptScans`] holds in all; a scan past either
/// is not kept, and keeping one that does not fit beside the others drops
/// those.
const MAX_KEPT_SCAN_ROWS: usize = 1 << 21;
const MAX_KEPT_SCAN_BYTES: usize = 256 << 20;

impl KeptScans {
    fn get(&self, table: u64, fields: &[u64]) -> Option<om_data::host::ScanRows> {
        self.rows
            .get(&(table, fields.to_vec()))
            .map(|(rows, _)| Arc::clone(rows))
    }

    fn keep(&mut self, table: u64, fields: &[u64], rows: om_data::host::ScanRows, bytes: usize) {
        if rows.len() > MAX_KEPT_SCAN_ROWS || bytes > MAX_KEPT_SCAN_BYTES {
            return;
        }
        if let Some((old, old_bytes)) = self.rows.remove(&(table, fields.to_vec())) {
            self.count -= old.len();
            self.bytes -= old_bytes;
        }
        if self.count + rows.len() > MAX_KEPT_SCAN_ROWS || self.bytes + bytes > MAX_KEPT_SCAN_BYTES
        {
            self.clear();
        }
        self.count += rows.len();
        self.bytes += bytes;
        self.rows.insert((table, fields.to_vec()), (rows, bytes));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.rows.clear();
        self.count = 0;
        self.bytes = 0;
    }
}

/// Global field values of a session, by (table, field, repetition).
pub(crate) type SessionGlobals = BTreeMap<(u64, u64, u32), String>;

/// Downloaded records by (table, record); `None` for a record the table
/// does not have.
type RecordCache = HashMap<(u64, u64), Option<Arc<RecordValues>>>;
type RelatedCache = HashMap<(u64, u64, u64), Arc<[u64]>>;

/// A source over the hosted file. Downloads are cached for its lifetime
/// (one provider call). A table whose download failed, or that the schema
/// lacks, reads as missing for the rest of the call, so the calculations
/// that needed it show `?`; other tables still supply their data. The first
/// failure, and the first of each table, are kept for the caller.
pub(crate) struct NativeSource<'a> {
    session: RefCell<&'a mut Session>,
    /// The related files' connections, for imported tables.
    external: Option<&'a crate::external::ExternalFiles>,
    schema: &'a CalcSchema,
    today: Date,
    records: RefCell<RecordCache>,
    tables: RefCell<HashMap<u64, Arc<[u64]>>>,
    /// The window's found sets by occurrence, and their sort fields.
    found: RefCell<HashMap<u64, Arc<[u64]>>>,
    sorts: RefCell<HashMap<u64, Arc<[u64]>>>,
    globals: RefCell<HashMap<u64, Arc<RecordValues>>>,
    related: RefCell<RelatedCache>,
    /// The record ids `related` holds from this source's own reads.
    related_held: std::cell::Cell<usize>,
    lookups: RefCell<HashMap<LookupKey, Option<Arc<[u64]>>>>,
    /// The session's own global values, over the stored ones.
    session_globals: SessionGlobals,
    /// The connection's container data, when given; else this source's.
    library: Option<&'a RefCell<LibraryCache>>,
    own_library: RefCell<LibraryCache>,
    calculation_session: Option<SessionInfo>,
    failure: RefCell<Option<DataError>>,
    /// The tables whose data could not be read in this call, with the
    /// first failure of each.
    failed_tables: RefCell<BTreeMap<u64, DataError>>,
    /// The tables whose window found set could not be read, with why
    /// (`NativeProvider::read_calc_batch`): their found-set needs fail.
    pub(crate) unseeded: RefCell<BTreeMap<u64, DataError>>,
    /// Relationship scans kept across calls, when given.
    scans: Option<&'a RefCell<KeptScans>>,
    /// The window's token for this read, when it may stop needing it
    /// (#987): once cancelled, no further request goes out.
    cancel: Option<&'a CalcCancel>,
    /// The provider's own font families ([`CalcState::fonts`]), when it
    /// gives them: read here the first time styled text of its own tables
    /// names a font, if it has not read them yet.
    fonts: Option<&'a Fonts>,
    /// This source's own, when the provider gives none: the schema's, or
    /// read here as above (a related file's owner, whose schema has none).
    own_fonts: Fonts,
    /// The layouts whose members a calculation asked for (#1019): listed
    /// from the catalog's index only, so read as having no objects here.
    /// Whoever evaluated reads them and evaluates again
    /// ([`NativeProvider::grow_for`]).
    layouts: RefCell<BTreeSet<u32>>,
    /// The occurrences of related files not open yet a calculation read
    /// (#1125): read as missing here. Whoever evaluated over the
    /// provider's own schema opens them and evaluates again, as the
    /// window's reads open them ([`NativeProvider::grow_for`]).
    occurrences: RefCell<BTreeSet<u64>>,
}

/// What a calculation the provider evaluated asked for that the schema did
/// not hold yet: layout members listed from the catalog's index only
/// (#1019) and occurrences of related files not open yet (#1125)
/// ([`NativeSource::take_needs`]).
#[derive(Debug, Default)]
pub(crate) struct SchemaNeeds {
    pub layouts: BTreeSet<u32>,
    pub occurrences: BTreeSet<u64>,
}

impl<'a> NativeSource<'a> {
    pub fn new(session: &'a mut Session, schema: &'a CalcSchema, today: Date) -> Self {
        NativeSource {
            session: RefCell::new(session),
            external: None,
            schema,
            today,
            records: RefCell::default(),
            tables: RefCell::default(),
            found: RefCell::default(),
            sorts: RefCell::default(),
            globals: RefCell::default(),
            related: RefCell::default(),
            related_held: std::cell::Cell::new(0),
            lookups: RefCell::default(),
            session_globals: SessionGlobals::new(),
            library: None,
            own_library: RefCell::default(),
            calculation_session: None,
            failure: RefCell::default(),
            failed_tables: RefCell::default(),
            unseeded: RefCell::default(),
            scans: None,
            cancel: None,
            fonts: None,
            own_fonts: RefCell::default(),
            layouts: RefCell::default(),
            occurrences: RefCell::default(),
        }
    }

    /// What the calculations asked for that the schema did not hold
    /// (`layouts`, `occurrences`), taken.
    /// The related record ids this source holds from its own reads, every
    /// relationship a calculation followed included: what a long reader
    /// (a related sort) bounds.
    pub(crate) fn related_held(&self) -> usize {
        self.related_held.get()
    }

    pub(crate) fn take_needs(&self) -> SchemaNeeds {
        SchemaNeeds {
            layouts: std::mem::take(&mut self.layouts.borrow_mut()),
            occurrences: std::mem::take(&mut self.occurrences.borrow_mut()),
        }
    }

    /// Read container data through the connection's cache.
    pub fn with_library(mut self, library: &'a RefCell<LibraryCache>) -> Self {
        self.library = Some(library);
        self
    }

    /// Decode styled text of the provider's own tables with its font
    /// families, read once when first needed ([`CalcState::fonts`]).
    pub fn with_fonts(mut self, fonts: &'a Fonts) -> Self {
        self.fonts = Some(fonts);
        self
    }

    /// The font families styled text in `cells`, of `table`, names
    /// (#1098). They are read only when one of the cells names a font:
    /// - for another file's table, from its owner, which reads them once;
    /// - for the provider's own tables, from its fonts ([`Self::with_fonts`]),
    ///   else from the schema's when it has them (the file the user
    ///   opened), else read here once for this source (a related file's
    ///   owner given no fonts).
    ///
    /// A read that fails fails the table's read, rather than leaving styled
    /// text without its fonts.
    fn fonts_for<'c>(
        &self,
        table: u64,
        mut cells: impl Iterator<Item = &'c Cell>,
    ) -> Result<Arc<BTreeMap<u64, String>>, CalcError> {
        if let Some((external, route)) = self.external.and_then(|e| Some((e, e.route(table)?))) {
            if !cells.any(names_font) {
                return Ok(Arc::default());
            }
            return external.fonts(route).map_err(|e| self.fail_table(table, e));
        }
        let fonts = self.fonts.unwrap_or(&self.own_fonts);
        if let Some(read) = fonts.borrow().as_ref() {
            return Ok(Arc::clone(read));
        }
        if self.fonts.is_none() {
            let known = self.schema.fonts_for_table(table);
            if !known.is_empty() {
                let known = Arc::new(known.clone());
                *fonts.borrow_mut() = Some(Arc::clone(&known));
                return Ok(known);
            }
        }
        if !cells.any(names_font) {
            return Ok(Arc::default());
        }
        let read = Arc::new(
            read_fonts(&mut self.session.borrow_mut()).map_err(|e| self.fail_table(table, e))?,
        );
        *fonts.borrow_mut() = Some(Arc::clone(&read));
        Ok(read)
    }

    fn library_cache(&self) -> &RefCell<LibraryCache> {
        self.library.unwrap_or(&self.own_library)
    }

    /// Connection metadata for source-side calculated relationship keys.
    pub fn with_calculation_session(mut self, session: SessionInfo) -> Self {
        self.calculation_session = Some(session);
        self
    }

    /// Read the session's own global values over the file's.
    pub fn with_session_globals(mut self, globals: SessionGlobals) -> Self {
        self.session_globals = globals;
        self
    }

    /// Read imported tables on their related files' connections.
    pub fn with_external(mut self, external: &'a crate::external::ExternalFiles) -> Self {
        self.external = Some(external);
        self
    }

    /// Match relationship scans against `scans`, and keep new ones there.
    pub fn with_scans(mut self, scans: &'a RefCell<KeptScans>) -> Self {
        self.scans = Some(scans);
        self
    }

    /// Stop sending requests once `cancel` is cancelled (#987).
    pub fn with_cancel(mut self, cancel: Option<&'a CalcCancel>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Whether the window stopped needing this read (#987).
    fn stopped(&self) -> bool {
        self.cancel.is_some_and(CalcCancel::cancelled)
    }

    /// Start from data already read (the last record page).
    pub fn seed(&self, data: &CalcData) {
        self.records
            .borrow_mut()
            .extend(data.records.iter().map(|(k, v)| (*k, v.clone())));
        self.tables
            .borrow_mut()
            .extend(data.tables.iter().map(|(k, v)| (*k, Arc::clone(v))));
        self.found
            .borrow_mut()
            .extend(data.found.iter().map(|(k, v)| (*k, Arc::clone(v))));
        let mut sorts = self.sorts.borrow_mut();
        for occurrence in data.found.keys() {
            sorts.remove(occurrence);
        }
        sorts.extend(data.sorts.iter().map(|(k, v)| (*k, Arc::clone(v))));
        self.globals.borrow_mut().extend(data.globals.clone());
        self.related.borrow_mut().extend(data.related.clone());
        self.related.borrow_mut().extend(data.related_ids.clone());
        self.lookups.borrow_mut().extend(data.lookups.clone());
    }

    /// Snapshot successful non-library reads, including candidate rows used
    /// internally by relationship resolution. Library headers are deliberately
    /// revalidated by LibraryCache at each request; do not seed that cache.
    fn snapshot(&self) -> CalcData {
        CalcData {
            records: self.records.borrow().clone(),
            tables: self.tables.borrow().clone(),
            found: self.found.borrow().clone(),
            sorts: self.sorts.borrow().clone(),
            globals: self.globals.borrow().clone(),
            related_ids: self.related.borrow().clone(),
            // A missing index can be built by the first find. Retry such
            // answers in the next batch instead of freezing the cold path.
            lookups: self
                .lookups
                .borrow()
                .iter()
                .filter(|(_, ids)| ids.is_some())
                .map(|(key, ids)| (key.clone(), ids.clone()))
                .collect(),
            ..CalcData::default()
        }
    }

    // Unstored source keys and sort calculations may read time, UUIDs or
    // other session metadata. Reevaluate them each call; successful stored
    // candidate rows and lookups can still be reused.
    fn reusable_related(&self, from: u64, to: u64) -> bool {
        self.schema.path(from, to).is_some_and(|path| {
            path.iter().all(|hop| {
                let keys = hop.predicates.iter().all(|predicate| {
                    predicate.op == om_data::host::JoinOp::Cartesian
                        || self
                            .schema
                            .field(hop.from, predicate.left)
                            .is_some_and(|field| !field.is_unstored_calculation())
                });
                let sorted = hop.sort.as_ref().map_or(true, |sort| {
                    sort.fields.iter().all(|entry| {
                        let occurrence = entry.field.occurrence.unwrap_or(hop.to);
                        // A stored sort value on another table is reached
                        // through a relationship whose keys can be volatile.
                        // Recompute that ordering; candidate rows and lookups
                        // remain reusable. Same-table aliases read this row,
                        // as sort_records does, without following a path.
                        self.schema
                            .table_of(occurrence)
                            .zip(self.schema.table_of(hop.to))
                            .is_some_and(|(field_table, target)| field_table.id == target.id)
                            && self
                                .schema
                                .field(occurrence, entry.field.field)
                                .is_some_and(|field| field.class == FieldClass::Data)
                    })
                });
                keys && sorted
            })
        })
    }

    /// The first download that failed.
    pub fn take_failure(&self) -> Option<DataError> {
        self.failure.borrow_mut().take()
    }

    /// The tables that could not be read so far, with why.
    pub fn table_failures(&self) -> BTreeMap<u64, DataError> {
        self.failed_tables.borrow().clone()
    }

    fn fail(&self, error: DataError) -> CalcError {
        self.failure.borrow_mut().get_or_insert(error);
        om_data::host::record_missing()
    }

    /// [`Self::fail`], and read `table` as missing from now on.
    fn fail_table(&self, table: u64, error: DataError) -> CalcError {
        self.failed_tables
            .borrow_mut()
            .entry(table)
            .or_insert_with(|| error.clone());
        self.fail(error)
    }

    /// The table's definition, or a failure of the table when the schema
    /// lacks it: reading its data by id would be a guess.
    fn table_def(&self, table: u64) -> Result<&'a TableDef, CalcError> {
        // Its field catalog not read yet (#772): read as no fields, its
        // records would be cached empty. An occurrence of it is asked for,
        // which reads the catalog, and the read waits (1224).
        if self.schema.are_fields_pending(table) {
            if let Some(occurrence) = self.schema.occurrences().find(|o| o.table == table) {
                CalcSource::need_occurrence(self, occurrence.id);
            }
            return Err(CalcError::from_code(1224).expect("nonzero"));
        }
        if self.failed_tables.borrow().contains_key(&table) {
            // One failure is enough to report; do not repeat it for every
            // record.
            return Err(om_data::host::record_missing());
        }
        self.schema.table(table).ok_or_else(|| {
            self.fail_table(
                table,
                DataError::new(format!("The calculation schema has no table 0x{table:x}.")),
            )
        })
    }

    /// The formats of the file holding base table `table`, which its
    /// stored values are read in (calc-values.md 9.2).
    fn cx(&self, table: u64) -> Ctx<'a> {
        let locale = self
            .schema
            .table_context(table)
            .map_or(&self.schema.locale, |file| &file.locale);
        Ctx::new(locale, self.today)
    }

    /// `read` on the connection holding base table `table`, with the
    /// table's id there: a related file's for an imported table, else the
    /// opened file's. A failure fails the table.
    fn with_table<T>(
        &self,
        table: u64,
        read: impl FnOnce(&mut Session, u64) -> Result<T, DataError>,
    ) -> Result<T, CalcError> {
        // A table that failed in this call reads as missing: no request
        // tries it again (a batch's failed join, #982). A read the window
        // stopped sends nothing more either, and fails nothing (#987): its
        // answer is superseded as a whole.
        if self.stopped() || self.failed_tables.borrow().contains_key(&table) {
            return Err(om_data::host::record_missing());
        }
        let route = self.external.and_then(|e| Some((e, e.route(table)?)));
        let result = match route {
            Some((external, route)) => external.with_session(route, read),
            // A client-only id means nothing on the opened file's
            // connection.
            None if self.schema.table_context(table).is_some() => Err(DataError::new(
                "The related file holding this table is not open.",
            )),
            None => read(&mut self.session.borrow_mut(), table),
        };
        result.map_err(|e| self.fail_table(table, e))
    }

    /// Download the records `ids` of `table` the cache lacks.
    fn download_ahead(&self, table: u64, ids: &[u64]) -> Result<(), CalcError> {
        let def = self.table_def(table)?;
        let missing: Vec<u64> = {
            let cache = self.records.borrow();
            ids.iter()
                .copied()
                .filter(|id| !cache.contains_key(&(table, *id)))
                .collect()
        };
        let cx = self.cx(table);
        for batch in missing.chunks(SCAN_BATCH) {
            let (data, wire) = self.with_table(table, |session, wire| {
                Ok((download_records(session, wire, batch)?, wire))
            })?;
            let tree = download_tree(std::slice::from_ref(&data))
                .map_err(|e| self.fail_table(table, e))?;
            let rows = om_hbam::record::records(&tree, wire);
            // After the records' own read: the fonts are read on the same
            // connection, never inside it.
            let fonts = self.fonts_for(table, rows.iter().flat_map(|r| r.cells.values()))?;
            let fonts = fonts.as_ref();
            let mut cache = self.records.borrow_mut();
            for id in batch {
                cache.insert((table, *id), None);
            }
            for row in &rows {
                cache.insert(
                    (table, row.id),
                    Some(Arc::new(record_values(def, row, &cx, fonts))),
                );
            }
        }
        Ok(())
    }
}

impl CalcSource for NativeSource<'_> {
    fn need_layout(&self, layout: u32) {
        self.layouts.borrow_mut().insert(layout);
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.occurrences.borrow_mut().insert(occurrence);
    }

    fn source_match_values(
        &self,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        field: u64,
        cx: &Ctx,
    ) -> Result<Vec<Value>, CalcError> {
        om_data::host::source_match_values(
            self,
            schema,
            occurrence,
            record,
            field,
            cx,
            self.calculation_session.as_ref(),
        )
    }

    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        if let Some(r) = self.records.borrow().get(&(table, record)) {
            return Ok(r.clone());
        }
        self.download_ahead(table, &[record])?;
        Ok(self
            .records
            .borrow()
            .get(&(table, record))
            .cloned()
            .flatten())
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        if let Some(found) = self.found.borrow().get(&occurrence) {
            return Ok(Arc::clone(found));
        }
        // The window's found set could not be read: never every record.
        if let Some(error) = self.unseeded.borrow().get(&table).cloned() {
            return Err(self.fail(error));
        }
        self.records(table)
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.sorts.borrow().get(&occurrence).cloned()
    }

    fn scan_rows(&self, table: u64, fields: &[u64]) -> Option<om_data::host::ScanRows> {
        self.scans?.borrow().get(table, fields)
    }

    fn scan_keep_budget(&self) -> usize {
        if self.scans.is_some() {
            MAX_KEPT_SCAN_BYTES
        } else {
            0
        }
    }

    fn keep_scan_rows(
        &self,
        table: u64,
        fields: &[u64],
        rows: om_data::host::ScanRows,
        bytes: usize,
    ) {
        if let Some(scans) = self.scans {
            scans.borrow_mut().keep(table, fields, rows, bytes);
        }
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        // A failed download is kept for the caller and reads as missing.
        let _ = self.prefetch(table, records);
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        if let Some(ids) = self.tables.borrow().get(&table) {
            return Ok(Arc::clone(ids));
        }
        self.table_def(table)?;
        let ids = self.with_table(table, record_ids)?;
        let ids: Arc<[u64]> = ids.iter().collect();
        self.tables.borrow_mut().insert(table, Arc::clone(&ids));
        Ok(ids)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        if let Some(values) = self.globals.borrow().get(&table) {
            return Ok(Arc::clone(values));
        }
        let def = self.table_def(table)?;
        let cx = self.cx(table);
        let values = if def.fields.values().any(|f| f.global) {
            let (data, wire) = self.with_table(table, |session, wire| {
                let path = vec![key_bytes(wire), vec![GLOBALS]];
                Ok((download_node(session, path)?, wire))
            })?;
            let node = match &data {
                Some(data) => Some(
                    download_tree(std::slice::from_ref(data))
                        .map_err(|e| self.fail_table(table, e))?,
                ),
                None => None,
            };
            let globals = node
                .as_ref()
                .and_then(|t| t.at(&[wire, u64::from(GLOBALS)]));
            let cells: Vec<Cell> = globals
                .into_iter()
                .flat_map(|n| &n.children)
                .map(Cell::from_node)
                .collect();
            let fonts = self.fonts_for(table, cells.iter())?;
            global_values(def, globals, &cx, &fonts)
        } else {
            RecordValues::default()
        };
        let mut values = values;
        for ((_, field, rep), text) in self
            .session_globals
            .range((table, 0, 0)..=(table, u64::MAX, u32::MAX))
        {
            if let Some(f) = def.fields.get(field) {
                values.set(*field, *rep, stored_value(text, f.value_type, &cx));
            }
        }
        let values = Arc::new(values);
        self.globals.borrow_mut().insert(table, Arc::clone(&values));
        Ok(values)
    }

    /// Related sets by scanning each table on the path, downloading its
    /// records in batches first.
    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        let key = (from, record, to);
        if let Some(ids) = self.related.borrow().get(&key) {
            return Ok(Arc::clone(ids));
        }
        let ids = if let Some(ids) = self.indexed_related(schema, from, record, to, cx)? {
            ids
        } else {
            // scan_hop batches the rows its predicates need. Preloading
            // the whole path here would also read final Cartesian bodies.
            om_data::host::related_by_scan(self, schema, from, record, to, cx)?
        };
        if self.reusable_related(from, to) {
            self.related_held
                .set(self.related_held.get().saturating_add(ids.len()));
            self.related.borrow_mut().insert(key, Arc::clone(&ids));
        }
        Ok(ids)
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        self.related(schema, from, record, to, cx)
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        self.records.borrow().contains_key(&(table, record))
    }

    fn prefetch(&self, table: u64, ids: &[u64]) -> Result<(), CalcError> {
        self.download_ahead(table, ids)
    }

    /// A SimpleFind for the whole value on the field's value index, on the
    /// connection holding the table and in its file's date order. A text's
    /// finder is made from its index key, not read as find text, so `#`,
    /// `.` or quotes in it are its own characters (#1068); a text of
    /// several lines is several index keys and is not looked up. A find
    /// the host refuses (an error) or cannot answer from an existing value
    /// index ([`crate::found::indexed_find`]) leaves the query to scan.
    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        let key = (table, field, Arc::from(text));
        if let Some(ids) = self.lookups.borrow().get(&key) {
            return Ok(ids.clone());
        }
        let result = (|| {
            let Some(def) = self.table_def(table)?.fields.get(&field) else {
                return Ok(None);
            };
            // What the finder is made from.
            enum By {
                /// A text's value-index key.
                Key(Vec<u8>),
                /// A date's or number's whole-value criterion.
                Criterion(om_hbam::DataType, String),
            }
            let by = match def.value_type {
                ValueType::Text => {
                    let text = text.trim();
                    if text.is_empty()
                        || text
                            .chars()
                            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
                    {
                        return Ok(None);
                    }
                    By::Key(om_collate::text_key(text))
                }
                ValueType::Date => By::Criterion(om_hbam::DataType::Date, format!("=={text}")),
                ValueType::Number => {
                    let Some(criterion) = related::numeric_lookup_criterion(text) else {
                        return Ok(None);
                    };
                    By::Criterion(om_hbam::DataType::Number, criterion)
                }
                _ => return Ok(None),
            };
            let date_order = self.cx(table).locale.date_order;
            // The finders name the table too: its id on that connection.
            self.with_table(table, |session, wire| {
                let finders: Vec<Vec<u8>> = match &by {
                    By::Key(key) => om_query::find::whole_value_finder(wire, field, key)
                        .into_iter()
                        .collect(),
                    By::Criterion(data_type, criterion) => {
                        let Ok(built) =
                            om_query::find::finders(wire, field, *data_type, criterion, date_order)
                        else {
                            return Ok(None);
                        };
                        built
                            .into_iter()
                            .filter(|f| f.keep)
                            .map(|f| f.bytes)
                            .collect()
                    }
                };
                if finders.is_empty() {
                    return Ok(None);
                }
                Ok(crate::found::indexed_find(session, wire, field, &finders)
                    .ok()
                    .flatten()
                    .map(|found| found.iter().collect()))
            })
        })()?;
        self.lookups.borrow_mut().insert(key, result.clone());
        Ok(result)
    }

    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, CalcError> {
        self.table_def(table)?;
        self.with_table(table, |session, wire| {
            self.library_cache()
                .borrow_mut()
                .member(session, wire, member)
        })
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<StoredContainer>>, CalcError> {
        self.table_def(table)?;
        self.with_table(table, |session, wire| {
            self.library_cache()
                .borrow_mut()
                .data_as(session, wire, member, table)
        })
    }
}

impl NativeProvider {
    /// A new cache lifetime, also visible to clients that still hold data
    /// from a previous batch or window. This changes no write/draft outcome.
    pub(crate) fn invalidate_calc_data(&mut self) {
        self.calc.page = CalcData::default();
        self.calc.scans.get_mut().clear();
        self.calc.generation = self.calc.generation.wrapping_add(1);
    }

    /// Notifications refer to owning connections, not imported client ids.
    /// Conservatively invalidate the page working set on any file's change.
    pub(crate) fn invalidate_calc_notifications(&mut self) -> bool {
        let desktop = self.sync_desktop_globals();
        let root = self
            .session
            .as_mut()
            .is_some_and(|session| !session.take_notifications().is_empty());
        let related = self.external.take_notifications();
        let mut lost = self.external.lost_connections();
        if let Some(session) = &self.session {
            if session.connection_lost().is_some() {
                lost.insert(*session.connection_key());
            }
        }
        let newly_lost = !lost.is_subset(&self.calc.lost_connections);
        self.calc.lost_connections.extend(lost);
        if root || related || newly_lost {
            self.invalidate_calc_data();
        }
        root || related || newly_lost || desktop
    }

    /// The file's calculation schema, read once per connection (and again
    /// after Manage Database re-reads the catalogs).
    pub(crate) fn calc_schema_cached(&mut self) -> Result<Arc<CalcSchema>, DataError> {
        self.require_open()?;
        if let Some(schema) = &self.calc.schema {
            return Ok(Arc::clone(schema));
        }
        // A rebuilt schema imports the related files again; nothing read
        // under the previous schema's ids is reused.
        self.invalidate_calc_data();
        *self.calc.library.get_mut() = LibraryCache::default();
        // The file's node `[2]` as the open read it before Login, for the
        // first schema after the open (`status_download`); else read now.
        let status = self.status_download.take();
        let session = self.session.as_mut().expect("open session");
        // Without the custom function catalog, calls to them fail when
        // evaluated; without the locale, calculations use en-US formats;
        // without the data sources, related files' tables are unavailable;
        // without the others, file triggers, value lists and the Design
        // functions have nothing to read. Records still read.
        // The locale `[2, 0d]` and document preferences `[2, 18]` are
        // values, which a download of their own key returns inline rather
        // than as a node: they come with the file's node `[2]`.
        // On demand, the script catalog's index only, as FileMaker Pro 26
        // reads it (network-protocol.md 5.5): the schema lists the scripts
        // by name, and their steps come with the window's script catalog
        // ([`NativeProvider::adopt_scripts`]). Read whole without an index.
        let scripts = if self.related_on_demand {
            download_node(session, script_index())
                .ok()
                .flatten()
                .or_else(|| {
                    download_node(session, vec![key_bytes(SCRIPTS)])
                        .ok()
                        .flatten()
                })
        } else {
            download_node(session, vec![key_bytes(SCRIPTS)])
                .ok()
                .flatten()
        };
        let status = match status {
            Some(status) => Some(status),
            None => download_node(session, vec![key_bytes(FILE)]).ok().flatten(),
        };
        let mut recovery_complete = true;
        let extra: Vec<crate::Download> = status
            .into_iter()
            .chain(
                [
                    vec![key_bytes(CUSTOM_FUNCTIONS)],
                    vec![key_bytes(VALUE_LISTS)],
                    vec![key_bytes(DATA_SOURCES)],
                ]
                .into_iter()
                .filter_map(|path| match download_node(session, path) {
                    Ok(download) => download,
                    Err(_) => {
                        recovery_complete = false;
                        None
                    }
                }),
            )
            .chain(scripts)
            .collect();
        let tree = {
            let mut downloads: Vec<&crate::Download> = self
                .schema_downloads
                .iter()
                .chain(self.layout_downloads.iter().filter(|d| calc_catalog(d)))
                .chain(&extra)
                .collect();
            // A key tree is built in key order.
            downloads.sort_by(|a, b| a.path.cmp(&b.path));
            crate::download_tree_refs(&downloads)?
        };
        self.recovery_creation = tree.internal_creation().ok().flatten();
        let recovery_catalogs = recovery_complete.then(|| {
            [CUSTOM_FUNCTIONS, VALUE_LISTS, DATA_SOURCES]
                .into_iter()
                .filter_map(|key| tree.node(&[&key_bytes(key)]).map(|n| n.to_owned_node()))
                .collect()
        });
        let mut schema = self.root_schema(&tree);
        // Keep the root before hydration imports any related schema.
        // Its catalogs remain available to hydration on every growth.
        let root = self.related_on_demand.then(|| schema.clone());
        // The account the file is open with, kept for its scripts' opens,
        // opens a related file first reached now, on its own host only
        // (#978).
        if let Some((account, password)) = self.held_credentials() {
            self.external.lend(&account, &password);
        }
        self.external.hydrate(
            &self.config,
            &self.close_requests,
            self.session.as_ref(),
            &mut schema,
            &self.catalog,
            &tree,
        )?;
        self.calc.root = root.map(|decoded| demand::RootSchema {
            extra,
            decoded: Some(decoded),
        });
        let schema = Arc::new(schema);
        self.register_desktop_file(Some(&schema));
        self.calc.schema = Some(Arc::clone(&schema));
        self.calc.recovery_catalogs = recovery_catalogs;
        Ok(schema)
    }

    /// The opened file's own calculation schema from `tree`, its catalogs:
    /// no related file imported yet.
    pub(crate) fn root_schema(&self, tree: &om_hbam::KeyTree<'_>) -> CalcSchema {
        let mut schema = calc_schema(tree, &self.catalog, &self.calc.file_name);
        schema.authenticated_privileges = self.privileges.account.clone();
        schema.authenticated_account = self.account_info.clone();
        schema.withhold_hidden_layouts();
        for id in &self.absent_layouts {
            if let Ok(id) = u32::try_from(*id) {
                schema.settle_layout(id);
            }
        }
        schema
    }

    /// The font families the page's `rows` of `table` name (#1098), read
    /// only when a row names a font. For another file's table (a layout on
    /// a related file's occurrence), its owner's, read once. For the
    /// provider's own, those read at open, else read now once.
    fn page_fonts(
        &mut self,
        table: u64,
        rows: &[om_hbam::Record],
    ) -> Result<Arc<BTreeMap<u64, String>>, DataError> {
        let names = || rows.iter().flat_map(|r| r.cells.values()).any(names_font);
        if let Some(route) = self.external.route(table) {
            return if names() {
                self.external.fonts(route)
            } else {
                Ok(Arc::default())
            };
        }
        if let Some(fonts) = self.calc.fonts.get_mut() {
            return Ok(Arc::clone(fonts));
        }
        if !names() {
            return Ok(Arc::default());
        }
        self.file_fonts()
    }

    /// What `Get` answers from this connection.
    pub(crate) fn session_info(&self) -> SessionInfo {
        let file = &self.calc.file_name;
        SessionInfo {
            account_name: self.account_name.clone(),
            account_info: self.account_info.clone(),
            privilege_set_name: self
                .privileges
                .account
                .as_ref()
                .map(|p| p.account_set_name.clone())
                .unwrap_or_default(),
            privileges: self.privileges.account.clone(),
            file_name: file.clone(),
            file_path: self
                .calc
                .file_path
                .clone()
                .unwrap_or_else(|| format!("fmnet:/{}/{file}.fmp12", self.calc.host_name)),
            host_name: self.calc.host_name.clone(),
            host_application_version: self.host_version.clone(),
            utc_offset: local_utc_offset(),
            ..SessionInfo::for_this_machine()
        }
    }

    /// Fill a record page's unstored calculations, summaries and global
    /// fields, evaluated on each record in the layout's occurrence (`?`
    /// where they need data that could not be read). Returns
    /// the page's records as calculation data, for
    /// [`NativeProvider::read_calc_data`].
    pub(crate) fn calculate_page(
        &mut self,
        occurrence: u64,
        table: &om_hbam::Table,
        rows: &[om_hbam::Record],
        sets: PageSets<'_>,
        page: &mut RecordPage,
        calculated_fields: Option<&[String]>,
    ) -> Result<CalcData, DataError> {
        // A calculation that read the objects of a layout listed from the
        // catalog's index only (#1019), or a related file not open yet
        // (#1125): its member or its file, then the page again, with what
        // the pass before read (the schema grows without changing an id,
        // `regrow_schema`).
        let PageSets { all, found, kept } = sets;
        let mut kept = kept;
        let mut asked = BTreeSet::new();
        loop {
            let sets = PageSets { all, found, kept };
            let (data, needs) =
                self.calculate_page_once(occurrence, table, rows, sets, page, calculated_fields)?;
            if !self.grow_for(needs, &mut asked)? {
                return Ok(data);
            }
            kept = data;
        }
    }

    /// [`Self::calculate_page`] once, with what its calculations asked for
    /// ([`NativeSource::take_needs`]).
    fn calculate_page_once(
        &mut self,
        occurrence: u64,
        table: &om_hbam::Table,
        rows: &[om_hbam::Record],
        sets: PageSets<'_>,
        page: &mut RecordPage,
        calculated_fields: Option<&[String]>,
    ) -> Result<(CalcData, SchemaNeeds), DataError> {
        let PageSets { all, found, kept } = sets;
        let schema = self.calc_schema_cached()?;
        let Some(def) = schema.table(table.id) else {
            return Ok(Default::default());
        };
        let session_info = self.session_info();
        let today = om_data::host::session::today(session_info.utc_offset);
        let locale = schema
            .table_context(table.id)
            .map_or(&schema.locale, |file| &file.locale);
        let cx = Ctx::new(locale, today);
        // What was read before and is still valid (`kept`, #1042), under
        // the page's own data read just now: the table's record list, the
        // page's records and the window's found set and sort, which a find,
        // omit or sort may have changed.
        let mut seed = kept;
        seed.found.clear();
        seed.sorts.clear();
        seed.tables.insert(table.id, all.iter().copied().collect());
        // The page may project out every calculation. Its deferred
        // summaries still need the found set's break fields, including an
        // explicit empty list when a previous sort was removed. A key of
        // an occurrence of another table keeps its place as unresolved, so
        // later keys never group without it.
        let sort: Arc<[u64]> = self
            .found_sort(table.id)
            .iter()
            .map(|key| schema.sort_key_field(table.id, key))
            .collect();
        // A window's found set belongs to the base table. An imported
        // calculation switches from the layout's external occurrence to its
        // owning occurrence; both must retain this window's set and order.
        // Relationship-scoped evaluation still supplies its explicit related
        // records and relationship sort independently of these defaults.
        let found: Arc<[u64]> = found.into();
        for alias in schema.occurrences().filter(|o| o.table == table.id) {
            seed.found.insert(alias.id, Arc::clone(&found));
            seed.sorts.insert(alias.id, Arc::clone(&sort));
        }
        let fonts = self.page_fonts(table.id, rows)?;
        for row in rows {
            seed.records.insert(
                (table.id, row.id),
                Some(Arc::new(record_values(def, row, &cx, &fonts))),
            );
        }
        let computed: Vec<(usize, &FieldDef)> = table
            .fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| Some((i, def.fields.get(&f.id)?)))
            .filter(|(_, f)| {
                (f.global || f.class == FieldClass::Summary || f.is_unstored_calculation())
                    && calculated_fields.map_or(true, |fields| fields.contains(&f.name))
            })
            .collect();
        let mut needs = SchemaNeeds::default();
        if !computed.is_empty() {
            let globals = self.desktop_globals(&schema);
            let session = self.session.as_mut().expect("open session");
            let source = NativeSource::new(session, &schema, today)
                .with_calculation_session(session_info.clone())
                .with_session_globals(globals)
                .with_library(&self.calc.library)
                .with_fonts(&self.calc.fonts)
                .with_external(&self.external);
            source.seed(&seed);
            let (mut locals, mut globals) = (Variables::new(), Variables::new());
            let summaries = om_data::host::SummaryCache::default();
            let mut host = FileHost::new(
                FileCalc::new(&schema, &source, today).with_summary_cache(&summaries),
                &session_info,
                &mut locals,
                &mut globals,
            );
            for record in &mut page.records {
                let Ok(id) = u64::try_from(record.id) else {
                    continue;
                };
                fill_record(&mut host, occurrence, id, &computed, record);
            }
            // A table this account cannot read leaves `?` in the fields
            // that needed it, as FileMaker shows a calculation that failed;
            // the page itself is complete.
            let _ = source.take_failure();
            seed.merge(source.snapshot());
            needs = source.take_needs();
        }
        Ok((bounded_page(seed), needs))
    }

    /// Data for a client's calculations: each need, read through a
    /// recorder; the legacy Related need brings its records along. A need whose
    /// table could not be read is left out, and the rest are still
    /// answered, with why each such table failed ([`CalcData::failures`]);
    /// the first failure is returned only when nothing was read.
    pub(crate) fn read_calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.read_calc_data_for(needs, None)
    }

    /// [`Self::read_calc_data`] for a window that may stop needing it
    /// (#987): once `cancel` is cancelled no further request goes out, and
    /// the answer is `CalcData::superseded()`, nothing partial and nothing
    /// marked missing or failed because of it. What was read before stays
    /// in the working set, valid across record pages until a change
    /// (#1042).
    pub(crate) fn read_calc_data_for(
        &mut self,
        needs: &[CalcNeed],
        cancel: Option<&CalcCancel>,
    ) -> Result<CalcData, DataError> {
        let stopped = || cancel.is_some_and(CalcCancel::cancelled);
        if stopped() {
            return Ok(CalcData::superseded());
        }
        // The related files these needs read open first, so the reads below
        // plan over the schema that has them (#983). A stopped read opens
        // no further file; those opened stay in the grown schema, which the
        // window takes with the superseded answer.
        self.grow_for_needs_until(needs, &stopped)?;
        if stopped() {
            return Ok(CalcData::superseded());
        }
        // Kept scans and the working set are only as fresh as the
        // notifications taken. A host sends them with its replies
        // (OpenMaker Server only so; FileMaker Server also calls a client
        // quiet for 250 ms, write-path.md 6.4), so before reusing either,
        // one HostTimeStamp (op82) on each connection of the session (a
        // related file on this computer has its own) brings any waiting
        // (#804, #1067). One that fails brings none, and may leave some
        // waiting: the kept data is dropped instead.
        if !self.calc.scans.get_mut().is_empty() || !self.calc.page.is_empty() {
            let mut fresh = true;
            let mut asked = None;
            if let Some(session) = self.session.as_mut() {
                fresh &= session.host_timestamp().is_ok();
                asked = Some(*session.connection_key());
            }
            // A read the window stopped asks no further connection (#987);
            // the next read checks freshness again.
            if stopped() {
                return Ok(CalcData::superseded());
            }
            let Some(answered) = self.external.freshen_until(asked, &stopped) else {
                return Ok(CalcData::superseded());
            };
            fresh &= answered;
            if stopped() {
                return Ok(CalcData::superseded());
            }
            if !fresh {
                self.invalidate_calc_data();
            }
        }
        self.invalidate_calc_notifications();
        let mut asked = BTreeSet::new();
        let mut changed = 0;
        loop {
            let batch = self.read_calc_batch(needs, cancel);
            if stopped() {
                return Ok(CalcData::superseded());
            }
            let (mut data, pending) = batch?;
            // Catalog needs raised during a read must settle before its
            // answer can be cached as an empty record or a missing field.
            let pending: Vec<CalcNeed> = pending
                .layouts
                .into_iter()
                .map(|layout| CalcNeed::Layout { layout })
                .chain(
                    pending
                        .occurrences
                        .into_iter()
                        .filter(|occurrence| asked.insert(*occurrence))
                        .map(|occurrence| CalcNeed::Occurrence { occurrence }),
                )
                .collect();
            if self.grow_for_needs_until(&pending, &stopped)? {
                continue;
            }
            // A reply may itself carry a change notification. Do not return
            // IDs from the old seed mixed with newly downloaded bodies.
            if !self.invalidate_calc_notifications() {
                self.mark_no_access(&mut data);
                // Stopped while it was being marked: superseded, not data.
                if stopped() {
                    return Ok(CalcData::superseded());
                }
                // What waits for an unreachable host is asked again then.
                data.retry_after = self.external.retry_in();
                return Ok(data);
            }
            changed += 1;
            if changed == 2 {
                return Err(DataError::new(
                    "Calculation data changed while reading it. Refresh to read it again.",
                ));
            }
        }
    }

    /// Mark the records in `data` the account's set may not view (#695), so
    /// that a portal row shows `<No Access>` rather than the empty values
    /// the host sent for it. A table whose records it may never view marks
    /// every record. Under a view formula, a record whose values came is
    /// one the host let it view; one that came empty is judged on the
    /// record as held, as the layout's own records are
    /// (`DataProvider::no_access`). A related file's table is judged by
    /// that file's set, its view formula not at all: evaluating it needs
    /// that file's definitions [I: left as the host sent it].
    fn mark_no_access(&mut self, data: &mut CalcData) {
        use om_data::host::security::RecordView;
        let Ok(schema) = self.calc_schema_cached() else {
            return;
        };
        let own = self.account_info.set_access.clone();
        // A view formula's verdict on an empty record, by table.
        let mut empty_rows: HashMap<u64, bool> = HashMap::new();
        let records: Vec<(u64, u64)> = data
            .records
            .iter()
            .filter(|(_, values)| values.is_some())
            .map(|(key, _)| *key)
            .collect();
        for (table, record) in records {
            let context = schema.table_context(table);
            let (access, key) = match context {
                None => (own.as_ref(), om_hbam::slot::uint_segment(table)),
                Some(file) => (
                    file.account_info.set_access.as_ref(),
                    om_hbam::slot::uint_segment(file.file_id(table)),
                ),
            };
            let Some(access) = access else {
                continue;
            };
            let hidden = match access.record_view(&key) {
                RecordView::All => false,
                RecordView::Never => true,
                RecordView::Where(_) if context.is_some() => false,
                RecordView::Where(code) => {
                    let empty = data.records[&(table, record)]
                        .as_ref()
                        .is_some_and(|values| values.values.is_empty());
                    if empty {
                        let code = code.to_vec();
                        !*empty_rows.entry(table).or_insert_with(|| {
                            self.formula_allows(table, &code, &BTreeMap::new())
                                .unwrap_or(false)
                        })
                    } else {
                        false
                    }
                }
            };
            if hidden {
                if let Some(Some(values)) = data.records.get_mut(&(table, record)) {
                    Arc::make_mut(values).no_access = true;
                }
            }
        }
    }

    fn read_calc_batch(
        &mut self,
        needs: &[CalcNeed],
        cancel: Option<&CalcCancel>,
    ) -> Result<(CalcData, SchemaNeeds), DataError> {
        let schema = self.calc_schema_cached()?;
        let session_info = self.session_info();
        let today = om_data::host::session::today(session_info.utc_offset);
        self.calc.library.get_mut().refresh();
        let globals = self.desktop_globals(&schema);
        let mut page = std::mem::take(&mut self.calc.page);
        // A found set the kept page no longer holds (its budget dropped it)
        // is the window's, never every record of its table: read once per
        // table, for all its occurrences. One that cannot be read fails
        // its needs, and the batch's other needs are still answered.
        let mut seeded = BTreeSet::new();
        let mut unseeded = BTreeMap::new();
        for need in needs {
            if let CalcNeed::FoundSet { occurrence, table } = need {
                if page.found.contains_key(occurrence) || !seeded.insert(*table) {
                    continue;
                }
                // A read the window stopped answers no need (#987).
                if cancel.is_some_and(CalcCancel::cancelled) {
                    break;
                }
                if let Err(error) = self.seed_found_set(&schema, *table, &mut page) {
                    unseeded.insert(*table, error);
                }
            }
        }
        let session = self.session.as_mut().expect("open session");
        let source = NativeSource::new(session, &schema, today)
            .with_calculation_session(session_info)
            .with_session_globals(globals)
            .with_library(&self.calc.library)
            .with_fonts(&self.calc.fonts)
            .with_external(&self.external)
            .with_scans(&self.calc.scans)
            .with_cancel(cancel);
        source.seed(&page);
        source.unseeded.replace(unseeded);
        // Records in as few op92 requests as possible (a query's found set
        // arrives as one list of needs).
        let mut ahead: std::collections::BTreeMap<u64, Vec<u64>> = Default::default();
        for need in needs {
            if let CalcNeed::Record { table, record } = need {
                ahead.entry(*table).or_default().push(*record);
            }
        }
        for (table, ids) in &ahead {
            let _ = source.download_ahead(*table, ids);
        }
        // Many records' related sets through one relationship (a footer
        // summary over the found set, #982): their candidates together, a
        // join per batch of records, before each need reads its own.
        let mut related: std::collections::BTreeMap<(u64, u64), Vec<u64>> = Default::default();
        for need in needs {
            if let CalcNeed::Related { from, record, to }
            | CalcNeed::RelatedIds { from, record, to } = need
            {
                related.entry((*from, *to)).or_default().push(*record);
            }
        }
        for ((from, to), records) in &related {
            if records.len() > 1 {
                let file = schema.file_context(*from);
                let cx = Ctx::new(file.map_or(&schema.locale, |file| &file.locale), today);
                source.prepare_related(&schema, *from, records, *to, &cx);
            }
        }
        let recorder = Recorder::new(&source);
        for need in needs {
            // A read the window stopped answers no further need (#987).
            if source.stopped() {
                break;
            }
            // In the formats a calculation reading it uses: a related set
            // compares in the file of its relationship graph (`cx_for`), a
            // table's data reads in the table's file.
            let file = match need {
                CalcNeed::Related { from, .. }
                | CalcNeed::RelatedIds { from, .. }
                | CalcNeed::FoundSet {
                    occurrence: from, ..
                }
                | CalcNeed::Occurrence { occurrence: from } => schema.file_context(*from),
                // A layout member read for it: the opened file's.
                CalcNeed::Layout { .. } => None,
                CalcNeed::Record { table, .. }
                | CalcNeed::Records { table }
                | CalcNeed::Globals { table }
                | CalcNeed::Lookup { table, .. }
                | CalcNeed::Member { table, .. }
                | CalcNeed::MemberData { table, .. } => schema.table_context(*table),
            };
            let cx = Ctx::new(file.map_or(&schema.locale, |file| &file.locale), today);
            // A failed need is left out of the answer, which the caller
            // reads as failed; the others still count.
            let _ = recorder.fetch(&schema, need, &cx);
        }
        let failure = source.take_failure();
        let mut data = recorder.take();
        data.generation = Some(self.calc.generation);
        page.merge(source.snapshot());
        self.calc.page = bounded_page(page);
        let failures = source.table_failures();
        let pending = source.take_needs();
        drop(source);
        self.overlay_held_new(&schema, needs, &mut data, today);
        self.overlay_unsettled_related(&schema, &mut data, today)?;
        match failure {
            Some(error) if data.is_empty() => Err(error),
            _ => {
                data.failures = failures;
                Ok((data, pending))
            }
        }
    }

    /// Put the window's found set of `table`, in its order, and its sort
    /// under every occurrence of the table into `page`, as a record page
    /// read puts them ([`Self::calculate_page_once`]).
    fn seed_found_set(
        &mut self,
        schema: &CalcSchema,
        table: u64,
        page: &mut CalcData,
    ) -> Result<(), DataError> {
        let (found, _) = self.found_ids(table)?;
        let sort: Arc<[u64]> = self
            .found_sort(table)
            .iter()
            .map(|key| schema.sort_key_field(table, key))
            .collect();
        let found: Arc<[u64]> = found.into();
        for alias in schema.occurrences().filter(|o| o.table == table) {
            page.found.insert(alias.id, Arc::clone(&found));
            page.sorts.insert(alias.id, Arc::clone(&sort));
        }
        Ok(())
    }

    /// Only the answer to this window's display read receives its retained
    /// prepared cells. Keep the host cache and observed versions unchanged.
    fn overlay_unsettled_related(
        &mut self,
        schema: &CalcSchema,
        data: &mut CalcData,
        today: Date,
    ) -> Result<(), DataError> {
        for ((table, record), row) in &mut data.records {
            let Some(prepared) = self.unsettled_related_record(*table, *record) else {
                continue;
            };
            let Some(prepared) = prepared else {
                *row = None;
                continue;
            };
            let Some(def) = schema.table(*table) else {
                continue;
            };
            let file = schema.table_context(*table);
            let cx = Ctx::new(file.map_or(&schema.locale, |file| &file.locale), today);
            let fonts = self.page_fonts(*table, std::slice::from_ref(&prepared))?;
            let mut values = record_values(def, &prepared, &cx, &fonts);
            // Keep the host read's version and access verdict; only displayed
            // cells come from the final prepared node, styles included.
            values.mod_count = row.as_deref().and_then(|row| row.mod_count);
            values.no_access = row.as_deref().is_some_and(|row| row.no_access);
            values.prepared_intent = true;
            *row = Some(Arc::new(values));
        }
        Ok(())
    }

    /// The records of `needs` the open transaction holds as new, which the
    /// host has none of yet: read as held (`held_new_cells`), as record
    /// pages show them, so that a script's field after New Record and the
    /// match field of a row begun in the record's portal read its values.
    /// So is the record a related set of `needs` is read from: the window
    /// scans a held record's sets from its held values
    /// (`CachedSource::related_over_overlays`), and a layout that shows
    /// only its portal asks for nothing else of it. Only the answer has
    /// them, not the working set of the host's records.
    fn overlay_held_new(
        &self,
        schema: &CalcSchema,
        needs: &[CalcNeed],
        data: &mut CalcData,
        today: Date,
    ) {
        for need in needs {
            let (table, record) = match need {
                CalcNeed::Record { table, record } => (table, record),
                CalcNeed::Related { from, record, .. }
                | CalcNeed::RelatedIds { from, record, .. } => match schema.table_of(*from) {
                    Some(table) => (&table.id, record),
                    None => continue,
                },
                _ => continue,
            };
            let Some(cells) = self
                .held_new_cells(*table, *record)
                .or_else(|| self.routed_new_cells(*table, *record))
            else {
                continue;
            };
            let Some(def) = schema.table(*table) else {
                continue;
            };
            // In the table's file's formats, a related file's its own.
            let file = schema.table_context(*table);
            let cx = Ctx::new(file.map_or(&schema.locale, |file| &file.locale), today);
            let mut values = RecordValues::default();
            for field in def.fields.values() {
                if field.global
                    || field.class == FieldClass::Summary
                    || field.is_unstored_calculation()
                {
                    continue;
                }
                for rep in 1..=field.repetitions {
                    let held = cell_key(field.id, rep).and_then(|key| cells.get(&key));
                    if let Some(Some(text)) = held {
                        values.set(field.id, rep, stored_value(text, field.value_type, &cx));
                    }
                }
            }
            values.held_new = true;
            data.records
                .insert((*table, *record), Some(Arc::new(values)));
        }
    }

    /// Fill the calculated fields of a record a write returned, and drop
    /// what the write made stale from the page's calculation data.
    pub(crate) fn calculate_record(
        &mut self,
        table: &om_hbam::Table,
        occurrence: u64,
        record: &mut Record,
    ) -> Result<(), DataError> {
        let Ok(id) = u64::try_from(record.id) else {
            return Ok(());
        };
        self.calc.page.invalidate_record(table.id, id);
        let mut page = RecordPage {
            versions: Default::default(),
            fields: Vec::new(),
            records: vec![record.clone()],
            total: 1,
        };
        let (found, rows) = self.with_read_table(table.id, |session, wire| {
            let found = record_ids(session, wire)?;
            let rows = match crate::download_node(
                session,
                vec![
                    key_bytes(wire),
                    vec![crate::RECORDS],
                    om_hbam::serial::encode(id),
                ],
            )? {
                Some(data) => {
                    let tree = download_tree(std::slice::from_ref(&data))?;
                    om_hbam::record::records(&tree, wire)
                }
                None => Vec::new(),
            };
            Ok((found, rows))
        })?;
        let all: Vec<u64> = found.iter().collect();
        let (found, _) = self.found_ids(table.id)?;
        let sets = PageSets {
            all: &all,
            found: &found,
            kept: CalcData::default(),
        };
        let data = self.calculate_page(occurrence, table, &rows, sets, &mut page, None)?;
        self.calc.page.records.extend(data.records);
        if let Some(filled) = page.records.pop() {
            *record = filled;
        }
        Ok(())
    }
}

/// Evaluate `computed` fields on `record` into its display values, each
/// as a calculation reads it (an unstored calculation in its formula's own
/// occurrence, calc-values.md 8.8).
fn fill_record(
    host: &mut FileHost<'_>,
    occurrence: u64,
    id: u64,
    computed: &[(usize, &FieldDef)],
    record: &mut Record,
) {
    for (index, def) in computed {
        let Some(slot) = record.values.get_mut(*index) else {
            continue;
        };
        let value = om_data::host::read_field(host, occurrence, id, def.id, 1);
        let text = display(&value, host);
        *slot = (!text.is_empty()).then_some(text);
    }
}

/// The local time zone's offset from UTC, in seconds.
pub fn local_utc_offset() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The working set holds the related record lists one record of a
    /// measured solution reads, the largest of about 445,000 ids, and the
    /// found set it stores under every occurrence of the layout's table
    /// counts once (#1067).
    #[test]
    fn the_working_set_holds_large_lists_and_counts_shared_ones_once() {
        let mut data = CalcData::default();
        data.tables.insert(1, Arc::from(vec![0; 445_000]));
        let found: Arc<[u64]> = Arc::from(vec![0; 80_000]);
        for occurrence in 0..100 {
            data.found.insert(occurrence, found.clone());
        }
        let kept = bounded_page(data);
        assert_eq!((kept.tables.len(), kept.found.len()), (1, 100));

        let mut data = CalcData::default();
        for table in 0..=(PAGE_IDS / 445_000) as u64 {
            data.tables.insert(table, Arc::from(vec![0; 445_000]));
        }
        assert!(bounded_page(data).tables.is_empty());
    }

    #[test]
    fn repetitions_are_the_field_key_and_a_repetition_byte() {
        assert_eq!(cell_key(0x0f, 1), Some(0x0f));
        assert_eq!(cell_key(0x0f, 2), Some(0x0f02));
        assert_eq!(cell_key(0x1a, 8), Some(0x1a08));
        assert_eq!(cell_key(0x0f, 256), None);
    }

    #[test]
    fn styled_text_reads_its_chunks_and_skips_the_style_subtree() {
        let text = om_scsu::encode_scsu("Shared notes");
        let cell = Cell::Nested(vec![
            (
                vec![vec![0]],
                Some([&(text.len() as u32).to_be_bytes()[..], &[0; 4]].concat()),
            ),
            (vec![vec![1]], Some(text[..5].to_vec())),
            (vec![vec![2]], Some(text[5..].to_vec())),
            (vec![vec![0xff]], None),
            (vec![vec![0xff], vec![1]], Some(vec![0, 0, 0, 7])),
        ]);
        assert_eq!(cell_text(&cell).as_deref(), Some("Shared notes"));
        assert_eq!(
            cell_text(&Cell::Value(om_scsu::encode_scsu("plain"))).as_deref(),
            Some("plain")
        );
        // A header claiming 4 GiB with no pieces behind it reserves nothing
        // (#216; aborts under `ulimit -v` if it does).
        let claim = Cell::Nested(vec![(vec![vec![0]], Some(vec![0xff; 4]))]);
        assert_eq!(cell_text(&claim), None);
    }
}
