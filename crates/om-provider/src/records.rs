//! Native record writes as FileMaker Pro makes them (write-path.md): New
//! Record, editing and Delete Record. The server's commit reply carries no
//! status, so every write is checked by reading the record back.

use super::*;
use om_data::rules::{
    error, Entry, RecordEnvironment, RecordPlan, WriteContext, WriteLayout, WriteMode, Written,
};
use om_data::FieldChanges;
use om_hbam::{Cell, OwnedNode};
use om_protocol::write::{self, Conflict, LockReply};
use om_query::keys::{record_value_keys, TextKeys};

/// Field values a write stores: masked text by field id, `None` to remove.
type EncodedChanges = BTreeMap<u64, Option<Vec<u8>>>;

/// Field `fc` of a record: its modification count (write-path.md 5.4).
pub(crate) const MOD_COUNT: u64 = 0xfc;
/// Key of a table's indexes, `[T, 01]` (query-program.md 7.1).
const INDEXES: u8 = 0x01;
/// ExistsInIndex, the uniqueness check (query-program.md 8.4).
const EXISTS_IN_INDEX: u8 = 18;
/// The result spot of the uniqueness check, `[3, 3]`.
const EXISTS_RESULT: u8 = 0x03;
/// GetKey's independent result spot, `[3, 4]`.
const INDEX_COUNTS_RESULT: u8 = 0x04;
/// The most index keys one op86 ExistsInIndex asks (a unique field's
/// changed repetitions are asked together): enough for every repetition of
/// a field, small enough for one request.
const EXISTS_KEYS: usize = 64;
/// Finite work limits for a validation without the host's value index.
const VALIDATION_SCAN_RECORDS: usize = 100_000;
const VALIDATION_SCAN_BYTES: usize = 64 << 20;
const VALIDATION_SCAN_CELLS: usize = 1_000_000;
const VALIDATION_SCAN_BATCH: usize = 128;

/// An op90 confirmation is one whole record, not a continuation into its
/// siblings. BEGIN can recur at page boundaries inside that subtree.
fn validate_confirmation(slots: &[u8]) -> Result<(), DataError> {
    use om_hbam::slot::{ChunkKind, Chunks, BEGIN};
    let invalid = || {
        DataError::with_code(
            om_data::found::code::UNSUPPORTED,
            "The validation confirmation is not one complete record.",
        )
    };
    if slots.first() != Some(&BEGIN) {
        return Err(invalid());
    }
    let mut depth = 0usize;
    let mut closed = false;
    for chunk in Chunks::new(slots) {
        let chunk = chunk.map_err(|e| failure("decode validation confirmation", e))?;
        if closed {
            return Err(invalid());
        }
        match chunk.kind {
            ChunkKind::Push => depth += 1,
            ChunkKind::Pop if depth == 0 => closed = true,
            ChunkKind::Pop => depth -= 1,
            _ => {}
        }
    }
    if !closed {
        return Err(invalid());
    }
    Ok(())
}

/// The modification count of a stored record. FileMaker Pro writes `fc` as a
/// one-segment key, a length byte and then a serial key (`01 00` = 0,
/// `01 01` = 1, `02 80 00` = 128); a record never modified stores none,
/// which counts as 0 (write-path.md 5.4).
pub(crate) fn mod_count(record: &om_hbam::Record) -> Result<u64, DataError> {
    match record.cells.get(&MOD_COUNT) {
        None => Ok(0),
        Some(Cell::Value(value)) => parse_mod_count(value),
        Some(Cell::Nested(_)) => Err(malformed_mod_count()),
    }
}

/// A modification count in its stored form, which is also how op60 returns
/// it (write-path.md 4.2).
fn parse_mod_count(value: &[u8]) -> Result<u64, DataError> {
    let (&len, key) = value.split_first().ok_or_else(malformed_mod_count)?;
    if usize::from(len) != key.len() {
        return Err(malformed_mod_count());
    }
    serial::decode(key).ok_or_else(malformed_mod_count)
}

pub(super) fn mod_count_value(count: u64) -> Vec<u8> {
    let key = serial::encode(count);
    let mut value = Vec::with_capacity(1 + key.len());
    value.push(key.len() as u8);
    value.extend(key);
    value
}

fn malformed_mod_count() -> DataError {
    DataError::new("The server holds malformed modification metadata for this record.")
}

/// A record node's stored cells by field id.
pub(super) fn stored_record(id: u64, node: &OwnedNode) -> om_hbam::Record {
    let cells = node
        .children
        .iter()
        .filter_map(|field| {
            let id = om_hbam::slot::be_uint(&field.segment)?;
            Some((id, Cell::from_owned(field)))
        })
        .collect();
    om_hbam::Record { id, cells }
}

/// The text of every stored field but the modification count: the inputs of
/// the client's field rules.
pub(super) fn text_values(record: &om_hbam::Record) -> BTreeMap<u64, String> {
    record
        .cells
        .iter()
        .filter(|(id, _)| **id != MOD_COUNT)
        .filter_map(|(id, cell)| Some((*id, cell.text()?.ok()?)))
        .collect()
}

/// Store `values` in a record node, keeping its other cells and nested data.
/// Each value is masked SCSU text, stored in the form FileMaker writes
/// (hbam-writer.md 2.3): one leaf for text of one byte per character up to
/// 1000 bytes, otherwise a node with a header child. FileMaker Pro does not
/// read a container reference (`01` and U+E000) stored as a leaf (#911).
pub(super) fn apply_values(node: &mut OwnedNode, values: EncodedChanges) {
    for (id, value) in values {
        let key = key_bytes(id);
        match value {
            Some(value) => node.set_child(om_data::scripts::encoded_text_node(&key, &value)),
            None => {
                node.remove(&[&key]);
            }
        }
    }
}

/// FileMaker's "Record is in use by another user", a script step's error
/// for a lock refusal.
pub(super) const RECORD_IN_USE: i32 = 301;
/// "Record modification ID does not match": another client changed it.
pub(super) const STALE_RECORD: i32 = 306;
/// "Record is missing".
pub(super) const RECORD_MISSING: i32 = 101;

/// The error for an op60 that did not grant the lock (write-path.md 4.2).
fn lock_refused(what: &str, reply: LockReply) -> DataError {
    let message = match reply {
        LockReply::Refused {
            status: write::LOCK_CONFLICT,
            conflict: Conflict { users, .. },
        } if !users.is_empty() => {
            format!("{what} is being modified by {users}. Try again when they are done.")
        }
        LockReply::Refused { status, .. } => {
            format!("The server refused to lock {what} (error {status}).")
        }
        LockReply::MustWait | LockReply::Granted { .. } => {
            format!("{what} is locked. Try again in a moment.")
        }
    };
    DataError::with_code(RECORD_IN_USE, message)
}

/// GetKey of the value-index counts, then ExistsInIndex in the same op86
/// (query-program.md 7.1, 8.4, 8.5). As in checked_find, the check must
/// precede the query: an absent index can answer false before the host
/// builds it. The key is in `[2, 1]`, exclusions in `[2, 2]`, the answer
/// in `[3, 3]` and counts in `[3, 4]`.
fn exists_in_index_program(
    file_id: u32,
    connection_key: &[u8; 16],
    table: u64,
    field: u64,
    keys: &[Vec<u8>],
    ignore: u64,
) -> Option<Vec<u8>> {
    let leaf = |key: u8, value: Vec<u8>| OwnedNode::leaf(vec![key], value);
    // A key operand is its segment as `[u8 len][bytes]` (2.2).
    let segment = |bytes: Vec<u8>| [vec![bytes.len() as u8], bytes].concat();
    let check = OwnedNode::list(
        serial::encode(1),
        vec![
            leaf(0x00, vec![42]),
            leaf(
                0x01,
                [
                    segment(key_bytes(table)),
                    vec![1, INDEXES],
                    segment(key_bytes(field)),
                    vec![1, 0x10],
                ]
                .concat(),
            ),
            leaf(0x02, vec![1, 3, 1, INDEX_COUNTS_RESULT]),
            leaf(0x04, file_id.to_be_bytes().to_vec()),
        ],
    );
    let instruction = OwnedNode::list(
        serial::encode(2),
        vec![
            leaf(0x00, vec![EXISTS_IN_INDEX]),
            leaf(0x01, vec![1, 2, 1, 1]),
            leaf(0x02, vec![1, 3, 1, EXISTS_RESULT]),
            leaf(0x04, file_id.to_be_bytes().to_vec()),
            leaf(0x05, segment(key_bytes(table))),
            leaf(
                0x07,
                om_crypto::query_access_key(2, connection_key, file_id).to_vec(),
            ),
            leaf(0x09, segment(key_bytes(field))),
            leaf(0x10, vec![1, 2, 1, 2]),
        ],
    );
    // The keys as value-less leaves of one spot, any of which answers true
    // (query-program.md 8.4, live:exists2_two); the record to ignore as a
    // one-record RID list (5.1).
    let asked = keys
        .iter()
        .map(|key| OwnedNode::leaf(key.clone(), Vec::new()))
        .collect();
    let upload = OwnedNode::list(
        vec![2],
        vec![
            OwnedNode::list(vec![1], asked),
            OwnedNode::list(
                vec![2],
                vec![OwnedNode::leaf(serial::encode(ignore), Vec::new())],
            ),
        ],
    );
    OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(vec![1], vec![check, instruction]), upload],
    )
    .to_wire()
}

/// The changed values among `values` of `fields`, each repetition's: a
/// field's own cell for its first, `field * 256 + n` for repetition n of
/// 2 to 255 (`om_data::rules::cell_of`). A unique value is checked in each
/// (script-runtime.md, the unique oracle's later repetitions).
pub(super) fn field_cells<'v>(
    table: &Table,
    fields: &[u64],
    values: &'v EncodedChanges,
) -> Vec<(u64, &'v [u8])> {
    values
        .iter()
        .filter_map(|(cell, value)| {
            let field = match table.field(*cell) {
                Some(_) => *cell,
                None if *cell & 0xff >= 2 => *cell >> 8,
                None => return None,
            };
            Some((field, value.as_deref()?)).filter(|(field, _)| fields.contains(field))
        })
        .collect()
}

/// How the value index of `table`'s `field`, validated as unique or as an
/// existing value, keys the text checked in it: its index language's form
/// (query-program.md 7.2), which the record rules require to be one
/// OpenMaker builds exactly.
pub(super) fn checked_keys(
    table: &Table,
    field: u64,
    file_language: Option<u32>,
) -> Result<TextKeys, DataError> {
    table
        .field(field)
        .and_then(|f| f.decoded_options())
        .and_then(|o| om_data::rules::value_text_keys(o.index_language(), file_language))
        .ok_or_else(|| {
            DataError::with_code(
                om_data::found::code::UNSUPPORTED,
                format!("Field {field} has no index OpenMaker can check a value in."),
            )
        })
}

/// How a value checked in a field's value index is keyed: text in the
/// index's form (query-program.md 7.2), or a number, date, time or
/// timestamp in its type's key (7.3), which no index language changes,
/// dates read in the file's order (#591).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexKeys {
    Text(TextKeys),
    /// The type and the file's date order.
    Typed(om_hbam::DataType, om_collate::DateOrder),
}

impl IndexKeys {
    /// The key `text` is looked up under, as the index holds it; for a
    /// typed source `None` when it is not one of the type's values
    /// OpenMaker keys (empty, no digit, digits among other characters).
    fn key(self, text: &str) -> Option<Vec<u8>> {
        match self {
            IndexKeys::Text(keys) => keys.key(text),
            IndexKeys::Typed(data_type, order) => {
                om_query::keys::value_keys(data_type, text, order, TextKeys::Collated)
                    .into_iter()
                    .find(|key| {
                        key != om_query::keys::EMPTY_TEXT_KEY && key != om_query::keys::INVALID_KEY
                    })
            }
        }
    }

    /// The keys a stored value's repetitions `texts` have in the index.
    fn stored(self, texts: &[String]) -> std::collections::BTreeSet<Vec<u8>> {
        match self {
            // Text keys do not depend on the file's date order.
            IndexKeys::Text(keys) => record_value_keys(
                om_hbam::DataType::Text,
                texts,
                om_collate::DateOrder::MonthDayYear,
                keys,
            ),
            IndexKeys::Typed(data_type, order) => {
                record_value_keys(data_type, texts, order, TextKeys::Collated)
            }
        }
    }
}

/// How the value index of `table`'s `field`, the source of a value list,
/// keys a value looked up in it. A text source keys it in its value form
/// ([`om_data::rules::value_text_keys`], language 0 the file's own); one in
/// a language OpenMaker does not key (the tailored ones, #299, or language
/// 0 in a file whose locale record cannot be read) refuses the check (3),
/// as a unique or existing-value check does, rather than keying the value
/// in English, which can both miss the host's key and match another value
/// (#571). A number, date, time or timestamp source keys it as its type,
/// whatever its language, dates in `order`, the file's (#591); a date or
/// timestamp source refuses the check (3) when that order is not known
/// (`None`: the file's locale record does not decode), rather than read
/// day and month the wrong way round.
fn list_keys(
    catalog: &Catalog,
    table: u64,
    field: u64,
    (file_language, order, decimal): (Option<u32>, Option<om_collate::DateOrder>, char),
) -> Result<IndexKeys, DataError> {
    use om_hbam::DataType;
    let field_def = catalog.table(table).and_then(|t| t.field(field));
    // A number in a file whose decimal separator is not a point is not keyed
    // as the file reads it, neither here nor by OpenMaker Server's index
    // builder (`1.000` is 1000 in German, keyed as 1), until #415: it is
    // looked up as text, as before #591.
    let typed = |f: &&om_hbam::Field| match f.data_type {
        DataType::Number => decimal == '.',
        DataType::Date | DataType::Time | DataType::Timestamp => true,
        _ => false,
    };
    if let Some(def) = field_def.filter(typed) {
        let dated = matches!(def.data_type, DataType::Date | DataType::Timestamp);
        return match (order, dated) {
            (Some(order), _) => Ok(IndexKeys::Typed(def.data_type, order)),
            // Numbers and times read no date order.
            (None, false) => Ok(IndexKeys::Typed(def.data_type, Default::default())),
            (None, true) => Err(DataError::unsupported(format!(
                "Field {field}, the source of a value list, holds dates in an order OpenMaker \
                 does not know: the file's locale record cannot be read."
            ))),
        };
    }
    let keys = field_def
        .and_then(|f| f.decoded_options())
        .and_then(|o| om_data::rules::value_text_keys(o.index_language(), file_language));
    if field_def.is_some_and(|f| f.data_type != DataType::Text) {
        return Ok(IndexKeys::Text(keys.unwrap_or(TextKeys::Collated)));
    }
    keys.map(IndexKeys::Text).ok_or_else(|| {
        DataError::unsupported(format!(
            "Field {field}, the source of a value list, has no index OpenMaker can check a \
             value in."
        ))
    })
}

/// ExistsInIndex's answer, `[3, 3, 00]` = `01 01` (another record holds the
/// key) or `01 00`; a failed program returns the error slot `00` instead
/// (query-program.md 11.2).
/// `None` means the index was missing, so even a false answer proves
/// nothing. Malformed counts are an error, never evidence of absence.
fn exists_in_index_result(slots: &[u8]) -> Result<Option<bool>, DataError> {
    let tree = KeyTree::from_slots(SlotReader::new(slots))
        .map_err(|e| failure("decode the uniqueness check", e))?;
    if let Some(error) = tree.root.child(&[0x00]) {
        let code = match error.value {
            Some([2, high, low]) => u16::from_be_bytes([*high, *low]),
            _ => u16::MAX,
        };
        return Err(DataError::new(format!(
            "The server could not check a unique value (error {code})."
        )));
    }
    let answer = tree
        .root
        .child(&[EXISTS_RESULT])
        .and_then(|spot| spot.child(&[0x00]))
        .and_then(|flag| flag.value);
    let answer = match answer {
        Some([1, 1]) => true,
        Some([1, 0]) => false,
        _ => {
            return Err(DataError::new(
                "The server answered the uniqueness check in an unknown form.",
            ))
        }
    };
    let counts = tree
        .root
        .child(&[INDEX_COUNTS_RESULT])
        .and_then(|spot| spot.child(&[0]))
        .and_then(|counts| counts.value);
    match counts {
        Some(counts) if counts.len() == 16 => Ok(Some(answer)),
        None | Some([]) => Ok(None),
        _ => Err(DataError::new(
            "The server returned malformed value-index counts.",
        )),
    }
}

impl NativeProvider {
    pub(super) fn session_mut(&mut self) -> Result<&mut Session, DataError> {
        self.session
            .as_mut()
            .ok_or_else(|| DataError::new("No hosted file is open."))
    }

    /// The layout's base table with freshly read field definitions, and the
    /// client's rules for them. Cached layout capabilities do not authorize
    /// a write against definitions that changed since.
    pub(super) fn record_plan(&mut self, layout_id: u32) -> Result<(Table, RecordPlan), DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?.id;
        self.table_plan(table)
    }

    /// [`Self::record_plan`] of base table `table` (a related record's).
    pub(super) fn table_plan(&mut self, table: u64) -> Result<(Table, RecordPlan), DataError> {
        let (mut table, data) = self.table_catalog(table)?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        table.fields = om_hbam::catalog::fields(&tree, table.id)
            .map_err(|e| failure("decode server fields", e))?;
        let plan = self.plan_table(&table, Some(&tree))?;
        Ok((table, plan))
    }

    /// Base table `table` with its fields as the host has them now, and no
    /// rules planned: what a held deletion reads (its containers'
    /// references). A deletion evaluates no field-entry rules, so a table
    /// whose rules OpenMaker cannot plan still deletes, as the ordinary
    /// deletion does.
    pub(super) fn table_fields(&mut self, table: u64) -> Result<Table, DataError> {
        let (mut table, data) = self.table_catalog(table)?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        table.fields = om_hbam::catalog::fields(&tree, table.id)
            .map_err(|e| failure("decode server fields", e))?;
        Ok(table)
    }

    /// Base table `table` of the catalog, and its field catalog downloaded
    /// now.
    fn table_catalog(&mut self, table: u64) -> Result<(Table, Download), DataError> {
        self.refuse_imported_table(table)?;
        let table = self
            .catalog
            .table(table)
            .ok_or_else(|| {
                DataError::with_code(
                    om_data::rules::error::FIELD_MISSING,
                    format!("The server schema has no base table 0x{table:x}."),
                )
            })?
            .clone();
        let data = download(self.session_mut()?, &[table.id, 3])?;
        Ok((table, data))
    }

    /// The rules of `table` ([`RecordPlan::from_file_schema`]) over
    /// `plan_schema`, from `tree`, its field catalog as just downloaded,
    /// or, when `None`, from the catalogs read at open. A write's plan
    /// (`table_plan`), a typed value's check (`entry_plan`) and a script's
    /// held write (`hold_script`) plan through it: the operations that
    /// write. The window's record capabilities, asked before a layout is
    /// drawn, plan without opening and defer to them
    /// (`record_capabilities`). The related files its rules
    /// read open first, as a read's do (#1139): planned
    /// over a schema still waiting for one, a rule reading it would be
    /// refused as reading a missing file. Not for an owning file's routed
    /// plan, whose other related files stay unresolved (#187).
    pub(super) fn plan_table(
        &mut self,
        table: &Table,
        tree: Option<&KeyTree<'_>>,
    ) -> Result<RecordPlan, DataError> {
        if self.routed_plan.is_none() {
            self.open_rule_files(table, tree)?;
        }
        let file = self.plan_schema()?;
        match tree {
            Some(tree) => RecordPlan::from_file_schema(table, tree, &file),
            None => {
                let tree = download_tree(&self.schema_downloads)?;
                RecordPlan::from_file_schema(table, &tree, &file)
            }
        }
    }

    /// The schema this file's records are planned over: its calculation
    /// schema, or while the opened file writes them as their owner, that
    /// schema with the opened file's tables imported (`routed_plan`, #187).
    pub(super) fn plan_schema(
        &mut self,
    ) -> Result<std::sync::Arc<om_data::host::CalcSchema>, DataError> {
        match &self.routed_plan {
            Some(plan) => Ok(std::sync::Arc::clone(&plan.schema)),
            None => self.calc_schema_cached(),
        }
    }

    /// Imported ids are client-only: refuse before using the root connection.
    pub(super) fn refuse_imported_table(&mut self, table: u64) -> Result<(), DataError> {
        let schema = self.calc_schema_cached()?;
        if schema.table_context(table).is_some() {
            return Err(DataError::with_code(
                om_data::found::code::UNSUPPORTED,
                format!("Table “{}” belongs to a related file; writing its records is not supported yet.",
                    schema.table(table).map_or("", |t| t.name.as_str())),
            ));
        }
        Ok(())
    }

    pub(super) fn unsupported_peer_transaction() -> DataError {
        DataError::with_code(
            3,
            "Related-file edits inside an explicit transaction are not supported.",
        )
    }

    /// Reuse the whole native engine without changing the caller's file context.
    /// Version keys remain in the client namespace outside the owning provider.
    pub(super) fn with_existing_record_owner<T>(
        &mut self,
        table: u64,
        record: i64,
        operation: impl FnOnce(&mut Self, u64) -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        if self.in_transaction() {
            return Err(Self::unsupported_peer_transaction());
        }
        let id = u64::try_from(record).map_err(|_| DataError::new("Invalid record ID."))?;
        let route = self.external.route(table).ok_or_else(|| {
            DataError::with_code(3, "The related file holding this table is unavailable.")
        })?;
        let seen = self.observed_version((table, id))?;
        let result = self.with_owner_reading_root(route, |owner, wire| {
            owner.require_writable()?;
            owner.invalidate_calc_data();
            owner.mod_counts.remove(&(wire, id));
            if let Some(seen) = seen {
                owner.mod_counts.insert((wire, id), seen);
            }
            let value = operation(owner, wire)?;
            Ok((value, owner.mod_counts.get(&(wire, id)).copied()))
        });
        let (value, version) = result?;
        // Confirmed deletion drops the client's version too. A refused or
        // ambiguous operation above retains it, as the local path does.
        self.wrote_version((table, id), version);
        Ok(value)
    }

    /// Create through the owning engine, with the same guards as
    /// [`Self::with_existing_record_owner`] but no source record: the owner
    /// issues the id and stores every value. Its verified version and the
    /// active window's found set are kept in the client table namespace.
    pub(super) fn with_new_record_owner(
        &mut self,
        table: u64,
        create: impl FnOnce(&mut Self, u64) -> Result<Record, DataError>,
    ) -> Result<Record, DataError> {
        if self.in_transaction() {
            return Err(Self::unsupported_peer_transaction());
        }
        let route = self.external.route(table).ok_or_else(|| {
            DataError::with_code(3, "The related file holding this table is unavailable.")
        })?;
        let (record, id, version) = self.with_owner_reading_root(route, |owner, wire| {
            owner.require_writable()?;
            owner.invalidate_calc_data();
            let record = create(owner, wire)?;
            let id = u64::try_from(record.id).map_err(|_| DataError::new("Invalid record ID."))?;
            Ok((record, id, owner.mod_counts.get(&(wire, id)).copied()))
        })?;
        if let Some(version) = version {
            // The creating window has seen what it wrote.
            self.wrote_version((table, id), Some(version));
        }
        self.found_add(table, id);
        Ok(record)
    }

    /// The cells a write through another file's owner takes: each
    /// repetition of an ordinary stored text, number, date, time, timestamp
    /// or container field, cell `field * 256 + n` for repetition n of 2 and
    /// up as the owner's own writes name it (`om_data::rules::cell_of`); no
    /// global or calculation. Checked before rules, locks and uploads,
    /// including Set Field's repetitions.
    pub(super) fn check_scalar_cells(
        table: &Table,
        cells: &BTreeMap<u64, Option<String>>,
    ) -> Result<(), DataError> {
        for cell in cells.keys() {
            // A first repetition is the field's own cell, never `F * 256
            // + 1`, which the owner does not take.
            let (field, repetition) = match table.field(*cell) {
                Some(field) => (Some(field), 1),
                None if *cell & 0xff >= 2 => (
                    table.field(*cell >> 8),
                    u16::try_from(*cell & 0xff).unwrap_or(0),
                ),
                None => (None, 0),
            };
            if !field.is_some_and(|field| {
                Self::ordinary_stored(field)
                    && field.decoded_options().is_some_and(|options| {
                        (1..=options.max_repetitions.max(1)).contains(&repetition)
                    })
            }) {
                return Err(DataError::with_code(
                    3,
                    "Related-file edits support only ordinary stored scalar or container fields.",
                ));
            }
        }
        Ok(())
    }

    /// Whether `field` is an ordinary stored text, number, date, time,
    /// timestamp or container field, not a global.
    fn ordinary_stored(field: &om_hbam::Field) -> bool {
        field.kind == om_hbam::FieldKind::Normal
            && matches!(
                field.data_type,
                om_hbam::DataType::Text
                    | om_hbam::DataType::Number
                    | om_hbam::DataType::Date
                    | om_hbam::DataType::Time
                    | om_hbam::DataType::Timestamp
                    | om_hbam::DataType::Container
            )
            && field
                .decoded_options()
                .is_some_and(|options| options.options & 0x1_0000 == 0)
    }

    /// Whether Browse edits `field` of another file's table on a layout:
    /// one a routed write takes ([`Self::check_scalar_cells`]) that does
    /// not repeat. Browse edits a later repetition of the layout's own
    /// record through its related path, which a routed table does not
    /// take yet.
    pub(super) fn routed_browse_field(field: &om_hbam::Field) -> bool {
        Self::ordinary_stored(field)
            && field
                .decoded_options()
                .is_some_and(|options| options.max_repetitions <= 1)
    }

    /// A new transaction id for one record write (write-path.md 1.2): the
    /// connection's, which its related files share, since a host keys a
    /// lock's holder by connection and transaction.
    pub(super) fn new_transaction(&mut self) -> u32 {
        if let Some(session) = self.session.as_mut() {
            return session.take_transaction();
        }
        let txn = self.next_transaction;
        self.next_transaction = self.next_transaction.wrapping_add(1).max(1);
        txn
    }

    pub(super) fn writable_values(
        table: &Table,
        plan: &RecordPlan,
        values: &FieldChanges,
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        let editable = plan.editable_fields();
        let mut changes = BTreeMap::new();
        for (name, value) in values {
            let field = table.field_by_name(name).ok_or_else(|| {
                DataError::with_code(
                    om_data::rules::error::FIELD_MISSING,
                    format!("Unknown field {name}."),
                )
            })?;
            // A container's reference text is the client's to set
            // (containers.md 1.4), whoever writes; users never type it.
            let container = field.data_type == om_hbam::DataType::Container
                && field.kind == om_hbam::FieldKind::Normal;
            if !editable.contains(name) && !container {
                // A script's step error (Set Field into a calculation
                // field), also inside a transaction.
                return Err(DataError::with_code(
                    om_data::rules::error::FIELD_LOCKED,
                    format!("Field {name} cannot be edited."),
                ));
            }
            changes.insert(field.id, value.clone());
        }
        Ok(changes)
    }

    pub(super) fn record_environment(&self) -> Result<RecordEnvironment, DataError> {
        let now = chrono::Local::now();
        let mut uuid = [0u8; 16];
        getrandom::getrandom(&mut uuid)
            .map_err(|_| DataError::new("Cannot obtain randomness for a new UUID."))?;
        uuid[6] = (uuid[6] & 0x0f) | 0x40;
        uuid[8] = (uuid[8] & 0x3f) | 0x80;
        let hex: String = uuid.iter().map(|byte| format!("{byte:02X}")).collect();
        let date = now.format("%-m/%-d/%Y").to_string();
        let time = now.format("%-I:%M:%S %p").to_string();
        Ok(RecordEnvironment {
            account_name: self.account_name.clone(),
            user_name: om_data::host::machine::user_name(),
            timestamp: format!("{date} {time}"),
            date,
            time,
            uuid: format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ),
            containers: None,
            interrupted: None,
            limits: om_data::calc::eval::CalcLimits::DEFAULT,
            lookups: Default::default(),
            serials: Default::default(),
            globals: None,
            // A write's operation supplies it (`Self::write_context`).
            context: WriteContext::default(),
            held: None,
            others_held: Default::default(),
            related_needs: None,
        })
    }

    /// [`Self::record_environment`] for a write by `plan` to `table`, with
    /// the session's global values when one of its formulas reads a global
    /// field (schema-path.md 5.17), and what its operation supplies
    /// (`context`, [`Self::write_context`]).
    pub(super) fn rule_environment(
        &mut self,
        table: u64,
        plan: &RecordPlan,
        context: WriteContext,
    ) -> Result<RecordEnvironment, DataError> {
        let mut environment = self.record_environment()?;
        environment.globals = self.record_globals(table, plan)?;
        environment.context = context;
        Ok(environment)
    }

    /// What `plan`'s formulas read of the operation making a write
    /// ([`WriteContext`], `RecordPlan::context_reads`), and only that:
    /// - the number of `layout`, the layout of this file the write goes
    ///   through, as the window numbers it (its place in
    ///   [`DataProvider::layouts`], 0 for one the account's set hides);
    ///   unknown with none, as in a write routed here from another file's
    ///   window, which is no window of this file;
    /// - the current privilege set, as `set` says whose it is;
    /// - the host's clock, from one op82 HostTimeStamp on this file's own
    ///   session, sent only when a formula reads it; unknown when the host
    ///   does not answer.
    ///
    /// A value left unknown refuses only a write that evaluates a formula
    /// reading it (`om_data::rules`).
    pub(crate) fn write_context(
        &mut self,
        plan: &RecordPlan,
        layout: Option<u32>,
        set: CurrentSet,
    ) -> Result<WriteContext, DataError> {
        use om_data::host::session::selector;
        let reads = plan.context_reads();
        let mut context = WriteContext::default();
        if let Some(layout) = layout.filter(|_| reads.contains(&selector::LAYOUT_NUMBER)) {
            context.layout = self.layout_number(layout)?;
        }
        if reads.contains(&selector::CURRENT_PRIVILEGE_SET_NAME) {
            context.privilege_set = match set {
                CurrentSet::Account => self
                    .privileges
                    .account
                    .as_ref()
                    .map(|set| set.account_set_name.clone()),
                CurrentSet::Step(name) => name,
            }
            .filter(|name| !name.is_empty());
        }
        if reads.contains(&selector::CURRENT_HOST_TIMESTAMP) {
            context.host_timestamp = self
                .session_mut()?
                .host_timestamp()
                .ok()
                .and_then(|key| WriteContext::host_timestamp_of(&key));
        }
        Ok(context)
    }

    /// [`Self::write_context`] of a script's write from `origin`, with the
    /// record number its window gave.
    pub(crate) fn origin_context(
        &mut self,
        plan: &RecordPlan,
        origin: Origin,
    ) -> Result<WriteContext, DataError> {
        if let Some(id) = origin.prepared {
            return self.preview_context(id, plan.table(), &origin);
        }
        let mut context = self.write_context(plan, origin.layout, origin.set)?;
        context.record_number = origin.record_number;
        Ok(context)
    }

    /// `Get ( LayoutNumber )` of layout `layout` as the window answers it.
    fn layout_number(&mut self, layout: u32) -> Result<WriteLayout, DataError> {
        if !self.shows_layout(u64::from(layout)) {
            return Ok(WriteLayout::Number(0));
        }
        let layouts = DataProvider::layouts(self)?;
        Ok(layouts
            .iter()
            .position(|listed| listed.id == layout)
            .map_or(WriteLayout::Unknown, |at| {
                WriteLayout::Number(at as u64 + 1)
            }))
    }

    /// The session's values of `table`'s global fields when `plan` reads
    /// one: the host's stored values with this session's changes, as the
    /// window's calculations read them.
    pub(super) fn record_globals(
        &mut self,
        table: u64,
        plan: &RecordPlan,
    ) -> Result<Option<std::sync::Arc<om_data::host::RecordValues>>, DataError> {
        if !plan.reads_globals() {
            return Ok(None);
        }
        let data = self.read_calc_data(&[om_data::host::CalcNeed::Globals { table }])?;
        data.globals.get(&table).cloned().map(Some).ok_or_else(|| {
            DataError::with_code(
                error::UNAVAILABLE,
                "The session's values of the table's global fields could not be read. \
                 No values were changed.",
            )
        })
    }

    /// The record `[T, 05, R]` as the server holds it now, `None` when there
    /// is no such record.
    pub(super) fn read_record(
        &mut self,
        table: u64,
        record: u64,
    ) -> Result<Option<OwnedNode>, DataError> {
        let path = vec![key_bytes(table), vec![RECORDS], serial::encode(record)];
        let Some(data) = download_node(self.session_mut()?, path)? else {
            return Ok(None);
        };
        let tree = download_tree(std::slice::from_ref(&data))?;
        let refs: Vec<&[u8]> = data.path.iter().map(Vec::as_slice).collect();
        // A record that stores no field downloads as an empty node.
        Ok(Some(tree.node(&refs).map_or_else(
            || OwnedNode::list(serial::encode(record), Vec::new()),
            |node| node.to_owned_node(),
        )))
    }

    /// Read the record back and check that the server kept `expected` and
    /// the modification count `mod_count`.
    pub(super) fn verify_record(
        &mut self,
        table: &Table,
        record: u64,
        expected: &FieldChanges,
        mod_count: u64,
    ) -> Result<Record, DataError> {
        let node = self.read_record(table.id, record)?.ok_or_else(|| {
            DataError::new("The server did not keep the record. The write could not be verified.")
        })?;
        let stored = stored_record(record, &node);
        let values = stored.values_for(table);
        for (name, value) in expected {
            let index = table
                .fields
                .iter()
                .position(|f| f.name == *name)
                .ok_or_else(|| DataError::new("Field changed while committing."))?;
            if values[index].as_deref().unwrap_or("") != value.as_deref().unwrap_or("") {
                return Err(DataError::new(format!(
                    "The server did not keep the new value of {name}. Refresh before editing again."
                )));
            }
        }
        if self::mod_count(&stored)? != mod_count {
            return Err(DataError::new(
                "The server did not keep the expected record version. Refresh before editing \
                 again.",
            ));
        }
        self.wrote_version((table.id, record), Some(mod_count));
        // The record as a page shows it (`record_page`): a window shows the
        // saved values until its page is read again (#783), and a value
        // stored with character styles is its text, not `<nested>`.
        Ok(Record {
            id: i64::try_from(record)
                .map_err(|_| DataError::new("Record ID exceeds the supported range."))?,
            values: super::page_values(&stored, table),
        })
    }

    /// Whether the host serves a new record holding `values` back without
    /// its values (om-server `views.rs`), and an empty read-back can only
    /// be that: the account's set never views the table's records, or its
    /// view formula, judged as the provider judges a page's records
    /// (`no_access`), is true neither on what was written nor on an empty
    /// record. Where it is true on an empty record, a host that kept no
    /// values would serve the record as viewable and as empty, so the
    /// values are checked as any record's are (and the create is reported
    /// as unverified).
    fn withholds_new(&mut self, table: &Table, values: &EncodedChanges) -> Result<bool, DataError> {
        use om_data::host::security::RecordView;
        let Some(access) = self.account_info.set_access.clone() else {
            return Ok(false);
        };
        let code = match access.record_view(&om_hbam::slot::uint_segment(table.id)) {
            RecordView::All => return Ok(false),
            RecordView::Never => return Ok(true),
            RecordView::Where(code) => code.to_vec(),
        };
        let mut row = BTreeMap::new();
        for (id, bytes) in values {
            if let Some(bytes) = bytes {
                let text = om_scsu::decode_scsu(bytes)
                    .map_err(|e| failure("decode generated field", format!("{e:?}")))?;
                row.insert(*id, text);
            }
        }
        let mut allows =
            |row: &BTreeMap<u64, String>| self.formula_allows(table.id, &code, row).unwrap_or(true);
        Ok(!allows(&row) && !allows(&BTreeMap::new()))
    }

    /// Read back a record the host withholds from this account and check
    /// what it serves of one: the record, at modification count
    /// `mod_count`, with no values. `None` when the host served values
    /// after all: they are checked as any record's are.
    fn verify_withheld(
        &mut self,
        table: &Table,
        record: u64,
        mod_count: u64,
    ) -> Result<Option<Record>, DataError> {
        let node = self.read_record(table.id, record)?.ok_or_else(|| {
            DataError::new("The server did not keep the record. The write could not be verified.")
        })?;
        let stored = stored_record(record, &node);
        if stored.values_for(table).iter().any(Option::is_some) {
            return Ok(None);
        }
        if self::mod_count(&stored)? != mod_count {
            return Err(DataError::new(
                "The server did not keep the expected record version. Refresh before editing \
                 again.",
            ));
        }
        self.wrote_version((table.id, record), Some(mod_count));
        Ok(Some(Record {
            id: i64::try_from(record)
                .map_err(|_| DataError::new("Record ID exceeds the supported range."))?,
            values: super::page_values(&stored, table),
        }))
    }

    /// Write-lock record `R` for `txn` and return its modification count,
    /// `None` when there is no such record (write-path.md 4.2).
    pub(super) fn lock_record(
        &mut self,
        table: u64,
        record: u64,
        txn: u32,
    ) -> Result<Option<u64>, DataError> {
        let record_key = serial::encode(record);
        let path: [&[u8]; 3] = [&key_bytes(table), &[RECORDS], &record_key];
        let reply = self
            .session_mut()?
            .lock(&path, txn, true)
            .map_err(|e| failure("lock record", e))?;
        match reply {
            LockReply::Granted {
                mod_count: Some(value),
            } => {
                if value.is_empty() {
                    Ok(None)
                } else {
                    parse_mod_count(&value).map(Some)
                }
            }
            LockReply::Granted { mod_count: None } => Err(DataError::new(
                "The server did not return the record's version. No values were changed.",
            )),
            refused => Err(lock_refused("This record", refused)),
        }
    }

    /// FileMaker Pro's check of a unique field before it commits a value
    /// (write-path.md 1.3): write-lock the field's index `[T, 01, F]` for
    /// `txn`, then ask the server whether a record other than `record`
    /// already holds the value. The lock is kept until the transaction ends.
    /// A field's changed repetitions are checked together: its index locked
    /// once, each distinct value asked once, and any scan of the stored
    /// records made once for all of them.
    pub(super) fn check_unique(
        &mut self,
        table: &Table,
        fields: Vec<u64>,
        values: &EncodedChanges,
        record: u64,
        txn: u32,
    ) -> Result<(), DataError> {
        let file_language = self.file_language()?;
        let mut changed: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for (field, value) in field_cells(table, &fields, values) {
            let text = om_scsu::decode_scsu(value)
                .map_err(|e| failure("decode a unique value", format!("{e:?}")))?;
            changed.entry(field).or_default().insert(text);
        }
        for (field, texts) in changed {
            let name = table.field(field).map_or("", |f| f.name.as_str());
            let field_key = key_bytes(field);
            let path: [&[u8]; 3] = [&key_bytes(table.id), &[INDEXES], &field_key];
            let session = self.session_mut()?;
            match session
                .lock(&path, txn, false)
                .map_err(|e| failure("lock a unique field", e))?
            {
                LockReply::Granted { .. } => {}
                refused => return Err(lock_refused(&format!("Field {name}"), refused)),
            }
            // Keep the index lock across both the queries and any scan,
            // including checks of pending transaction records.
            let keys = checked_keys(table, field, file_language)?;
            let texts: Vec<String> = texts.into_iter().collect();
            if self.any_in_index(table.id, field, &texts, record, keys)? {
                return Err(DataError::with_code(
                    om_data::rules::error::NOT_UNIQUE,
                    format!("Field {name} requires a unique value."),
                ));
            }
        }
        Ok(())
    }

    /// The other rules the server answers from value indexes (as for a
    /// unique value, write-path.md 1.3): a changed value of a field
    /// validated as an existing value must be held by a record other than
    /// `record` (505); one of a field validated by a value list of a
    /// field's values must be among that field's values (506). Both as
    /// FileMaker Server's engine checks them (script oracle s18,
    /// script-runtime.md 13.21); text compares as English index keys, as
    /// the planner requires of these fields.
    pub(super) fn check_lookups(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        entry: Entry,
        values: &EncodedChanges,
        record: u64,
    ) -> Result<(), DataError> {
        let file_language = self.file_language()?;
        let existing = plan.existing_fields_for(entry);
        for (&cell, value) in values {
            let Some(value) = value else {
                continue;
            };
            let text = om_scsu::decode_scsu(value)
                .map_err(|e| failure("decode a validated value", format!("{e:?}")))?;
            if text.is_empty() {
                continue;
            }
            let field = if table.field(cell).is_some() {
                cell
            } else {
                cell >> 8
            };
            if existing.contains(&field)
                && !self.in_index(
                    table.id,
                    field,
                    &text,
                    record,
                    IndexKeys::Text(checked_keys(table, field, file_language)?),
                )?
            {
                return Err(plan.refusal(field, error::NOT_EXISTING));
            }
            if let Some((occurrence, source)) = plan.value_list_field(cell, entry) {
                self.refuse_related_lists(table, plan, entry, [cell])?;
                let schema = self.calc_schema_cached()?;
                let source_table = schema
                    .table_of(occurrence)
                    .map(|t| t.id)
                    .ok_or_else(|| DataError::new("A value list names a missing table."))?;
                let file_language = self.file_language()?;
                let order = (!schema.locale_unreadable).then_some(schema.locale.date_order);
                let decimal = schema.locale.decimal_separator;
                let keys = list_keys(
                    &self.catalog,
                    source_table,
                    source,
                    (file_language, order, decimal),
                )?;
                if !self.in_index(source_table, source, &text, 0, keys)? {
                    return Err(plan.refusal(field, error::NOT_IN_LIST));
                }
            }
        }
        Ok(())
    }

    /// Error 3 when a value entered into one of `cells` must be among the
    /// values of a related file's field (a value list `entry` checks): the
    /// host would look it up in that file's index, which this file's
    /// connection cannot ask (external.rs). Checked before a write locks,
    /// creates or finds anything.
    pub(super) fn refuse_related_lists(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        entry: Entry,
        cells: impl IntoIterator<Item = u64>,
    ) -> Result<(), DataError> {
        let mut cells = cells.into_iter().peekable();
        if cells.peek().is_none() {
            return Ok(());
        }
        let schema = self.calc_schema_cached()?;
        for cell in cells {
            let Some((occurrence, _)) = plan.value_list_field(cell, entry) else {
                continue;
            };
            if shows_related_file(&schema, occurrence) {
                let field = table.field(cell).or_else(|| table.field(cell >> 8));
                return Err(DataError::with_code(
                    om_data::found::code::UNSUPPORTED,
                    format!(
                        "Field “{}” is validated by a value list of a related file's field, \
                         which OpenMaker cannot check yet. No values were changed.",
                        field.map_or("", |f| f.name.as_str())
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Whether a record of `table` other than `ignore` holds `text` in
    /// `field`, by op86 ExistsInIndex on its value index.
    fn in_index(
        &mut self,
        table: u64,
        field: u64,
        text: &str,
        ignore: u64,
        keys: IndexKeys,
    ) -> Result<bool, DataError> {
        // The key as the index holds it (query-program.md 7.1-7.3). A value
        // a typed source's type does not read as one OpenMaker keys (a
        // decimal comma, digits among other characters, until #415) is
        // looked up as text, as before #591.
        let key = match (keys, keys.key(text)) {
            (_, Some(key)) => key,
            (IndexKeys::Typed(..), None) => {
                return self.in_index(
                    table,
                    field,
                    text,
                    ignore,
                    IndexKeys::Text(TextKeys::Collated),
                )
            }
            (IndexKeys::Text(_), None) => {
                return Err(DataError::new("A validated field's index holds no keys."))
            }
        };
        match self.index_answer(table, field, std::slice::from_ref(&key), ignore)? {
            Some(answer) => Ok(answer),
            None => {
                // An unkeyed stored value of a typed source compares as text.
                let as_text = match keys {
                    IndexKeys::Typed(..) => TextKeys::Collated.key(text),
                    IndexKeys::Text(_) => None,
                };
                self.scan_index_keys(
                    table,
                    field,
                    std::slice::from_ref(&key),
                    ignore,
                    keys,
                    as_text.as_deref(),
                )
            }
        }
    }

    /// [`Self::in_index`] for any of `texts`: their distinct index keys
    /// asked together (at most [`EXISTS_KEYS`] in one op86), and the stored
    /// records scanned once for every key the host's index could not
    /// answer.
    fn any_in_index(
        &mut self,
        table: u64,
        field: u64,
        texts: &[String],
        ignore: u64,
        keys: TextKeys,
    ) -> Result<bool, DataError> {
        // The keys as the index holds them (query-program.md 7.1, 7.2):
        // texts that differ only in case, or share a cut key, ask once.
        let asked = texts
            .iter()
            .map(|text| keys.key(text))
            .collect::<Option<BTreeSet<Vec<u8>>>>()
            .ok_or_else(|| DataError::new("A validated field's index holds no keys."))?;
        let asked: Vec<Vec<u8>> = asked.into_iter().collect();
        let mut unanswered = Vec::new();
        for chunk in asked.chunks(EXISTS_KEYS) {
            match self.index_answer(table, field, chunk, ignore)? {
                Some(true) => return Ok(true),
                Some(false) => {}
                None => unanswered.extend_from_slice(chunk),
            }
        }
        if unanswered.is_empty() {
            return Ok(false);
        }
        self.scan_index_keys(
            table,
            field,
            &unanswered,
            ignore,
            IndexKeys::Text(keys),
            None,
        )
    }

    /// The host's op86 ExistsInIndex answer for `asked` in `field`'s value
    /// index: whether a record other than `ignore` holds any of them;
    /// `None` when the index's counts were missing.
    fn index_answer(
        &mut self,
        table: u64,
        field: u64,
        asked: &[Vec<u8>],
        ignore: u64,
    ) -> Result<Option<bool>, DataError> {
        let session = self.session_mut()?;
        let file_id = session
            .opened_file()
            .ok_or_else(|| DataError::new("No hosted file is open."))?
            .reply
            .file_id;
        let program = exists_in_index_program(
            file_id,
            session.connection_key(),
            table,
            field,
            asked,
            ignore,
        )
        .ok_or_else(|| DataError::new("A validated value is too long to check."))?;
        let reply = session
            .query(&program)
            .map_err(|e| failure("check a validated value", e))?;
        exists_in_index_result(&reply.slots)
    }

    /// The same key membership question over stored records when the
    /// host's counts were missing: whether a record other than `ignore`
    /// holds any of `wanted`. The caller's unique-index lock, if any, stays
    /// held. This sees committed records, like op18; pending transaction
    /// duplicates remain checked by transaction::note_unique. For a typed
    /// lookup, `as_text` is the candidate's text key for unkeyed stored values.
    fn scan_index_keys(
        &mut self,
        table: u64,
        field: u64,
        wanted: &[Vec<u8>],
        ignore: u64,
        keys: IndexKeys,
        as_text: Option<&[u8]>,
    ) -> Result<bool, DataError> {
        use om_hbam::{DataType, FieldKind};
        let file_language = self.file_language()?;
        let unsupported = |reason: &str| {
            DataError::with_code(
                om_data::found::code::UNSUPPORTED,
                format!(
                    "Cannot validate field {field} of table {table} without the host's value index: \
                     {reason}"
                ),
            )
        };
        // Re-read the source definition: a field-backed list can name a
        // different table, not covered by the write's fresh RecordPlan.
        let data = download(self.session_mut()?, &[table, 3])?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let fields = om_hbam::catalog::fields(&tree, table)
            .map_err(|e| failure("decode validation source fields", e))?;
        let field = fields
            .iter()
            .find(|f| f.id == field)
            .ok_or_else(|| unsupported("the source field is missing."))?;
        let options = field
            .decoded_options()
            .ok_or_else(|| unsupported("the field definition is unreadable."))?;
        // A typed source's text fallback compares each stored repetition
        // whole, skipping absent cells, as OpenMaker Server's scan does.
        let whole = matches!(keys, IndexKeys::Text(_)) && field.data_type != DataType::Text;
        // The caller keyed the value as `keys`: stored values are keyed
        // the same, a text source only in that form, a typed one as its
        // own type (#591).
        let same_form = match (keys, field.data_type) {
            (IndexKeys::Text(form), DataType::Text) => {
                om_data::rules::value_text_keys(options.index_language(), file_language)
                    == Some(form)
            }
            // A typed source's value looked up as text (one its type does
            // not read as one OpenMaker keys, or a number in a file whose
            // decimal separator is not a point): its stored values compare
            // as text too, as OpenMaker Server's scan compares them.
            (
                IndexKeys::Text(form),
                DataType::Number | DataType::Date | DataType::Time | DataType::Timestamp,
            ) => {
                form == TextKeys::Collated
                    || om_data::rules::value_text_keys(options.index_language(), file_language)
                        == Some(form)
            }
            (IndexKeys::Text(_), _) => false,
            (IndexKeys::Typed(data_type, _), _) => field.data_type == data_type,
        };
        if !same_form
            || !matches!(field.kind, FieldKind::Normal | FieldKind::Calculation)
            || options.options & 0x3_0000 != 0
        {
            return Err(unsupported(
                "only stored values keyed as the checked value is are supported.",
            ));
        }
        // Repetition n is the cell `F * 256 + n` only up to 255; a later one
        // would be another field's cell (execute-sql.md).
        if options.max_repetitions > 255 {
            return Err(unsupported("repetitions past 255 are not read."));
        }
        let repetitions = usize::from(options.max_repetitions.max(1));
        let session = self.session_mut()?;
        let ids = record_ids(session, table)?;
        if ids.len() > VALIDATION_SCAN_RECORDS
            || ids.len().saturating_mul(repetitions) > VALIDATION_SCAN_CELLS
        {
            return Err(unsupported(
                "the bounded record/cell scan limit was exceeded.",
            ));
        }
        let ids: Vec<u64> = ids.iter().filter(|id| *id != ignore).collect();
        let mut bytes = 0usize;
        for batch in ids.chunks(VALIDATION_SCAN_BATCH) {
            let data = download_records(session, table, batch)?;
            bytes = bytes.saturating_add(data.slots.len());
            if bytes > VALIDATION_SCAN_BYTES {
                return Err(unsupported("the 64 MiB scan limit was exceeded."));
            }
            let tree = download_tree(std::slice::from_ref(&data))?;
            let mut rows = BTreeMap::new();
            if let Some(records) = tree.at(&[table, u64::from(RECORDS)]) {
                if records.value.is_some() {
                    return Err(unsupported("the record list is not readable."));
                }
                for node in &records.children {
                    let id = serial::decode(node.segment.0)
                        .filter(|id| batch.contains(id))
                        .ok_or_else(|| unsupported("the scan returned an unexpected record."))?;
                    if !node.push || node.value.is_some() {
                        return Err(unsupported("a stored record is not a field list."));
                    }
                    rows.insert(id, node.to_owned_node());
                }
            }
            for &id in batch {
                let mut node = rows.remove(&id);
                // op92 may omit a disappeared row. An absent source cell
                // can also be a partial reply rather than an empty value.
                // Confirm either case with an independent op90 read before
                // concluding absence; keep the caller's unique lock held.
                let needs_confirmation = node.as_ref().map_or(true, |node| {
                    (1..=repetitions).any(|rep| {
                        // A repetition that cannot have a cell is empty.
                        om_data::rules::cell_of(field.id, rep as u32)
                            .is_some_and(|cell| node.child(&key_bytes(cell)).is_none())
                    })
                });
                if needs_confirmation {
                    let path = vec![key_bytes(table), vec![RECORDS], serial::encode(id)];
                    let Some(data) = download_node(session, path)? else {
                        // A stale master list can still name a deleted row.
                        continue;
                    };
                    bytes = bytes.saturating_add(data.slots.len());
                    if bytes > VALIDATION_SCAN_BYTES {
                        return Err(unsupported("the 64 MiB scan limit was exceeded."));
                    }
                    validate_confirmation(&data.slots)?;
                    let tree = download_tree(std::slice::from_ref(&data))?;
                    let refs: Vec<&[u8]> = data.path.iter().map(Vec::as_slice).collect();
                    // NonTerminal plus a complete empty list establishes a
                    // real empty record; a terminal record is refused by
                    // download_node before it can become an empty cell map.
                    node = Some(tree.node(&refs).map_or_else(
                        || OwnedNode::list(serial::encode(id), Vec::new()),
                        |node| node.to_owned_node(),
                    ));
                }
                let node = node.expect("present or confirmed record");
                if node.value.is_some() {
                    return Err(unsupported("a stored record is not a field list."));
                }
                let row = stored_record(id, &node);
                let texts = (1..=repetitions)
                    .filter_map(|rep| {
                        let cell = om_data::rules::cell_of(field.id, rep as u32);
                        let Some(value) = cell.and_then(|cell| row.cells.get(&cell)) else {
                            return (!whole).then(|| Ok(String::new()));
                        };
                        Some(match value.text() {
                            Some(text) => {
                                text.map_err(|e| failure("decode a validation source value", e))
                            }
                            None => Err(unsupported("a stored cell is not readable text.")),
                        })
                    })
                    .collect::<Result<Vec<_>, DataError>>()?;
                if whole {
                    if texts
                        .iter()
                        .any(|text| keys.key(text).is_some_and(|key| wanted.contains(&key)))
                    {
                        return Ok(true);
                    }
                    continue;
                }
                let (texts, unkeyed): (Vec<String>, Vec<String>) = match keys {
                    IndexKeys::Typed(..) => texts
                        .into_iter()
                        .partition(|text| text.is_empty() || keys.key(text).is_some()),
                    IndexKeys::Text(_) => (texts, Vec::new()),
                };
                let held = keys.stored(&texts);
                if wanted.iter().any(|key| held.contains(key))
                    || as_text.is_some_and(|key| {
                        unkeyed
                            .iter()
                            .any(|text| TextKeys::Collated.key(text).as_deref() == Some(key))
                    })
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Upload the whole record (op14) and commit `entry` (op70) under `txn`
    /// (write-path.md 3.3, 3.4).
    fn commit_record(
        &mut self,
        table: u64,
        record: OwnedNode,
        entry: OwnedNode,
        txn: u32,
    ) -> Result<(), DataError> {
        let session = self.session_mut()?;
        session
            .upload_records(&key_bytes(table), vec![record])
            .map_err(|e| failure("upload record", e))?;
        session
            .commit_records(&key_bytes(table), vec![entry], txn)
            .map_err(|e| failure("commit record", e))
    }

    /// End transaction `txn`, releasing every lock it holds (op65,
    /// write-path.md 4.4), as FileMaker Pro does when the user leaves the
    /// record. Best effort: the server also drops them at disconnect.
    pub(super) fn end_transaction(&mut self, txn: u32) {
        if let Some(session) = self.session.as_mut() {
            let _ = session.kill_all_locks(txn);
        }
    }

    /// New Record and its first commit (write-path.md 1.1).
    pub(super) fn insert_native_record(
        &mut self,
        layout_id: u32,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_open()?;
        let (table, plan) = self.record_plan(layout_id)?;
        let changes = Self::writable_values(&table, &plan, &values)?;
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Account)?;
        self.insert_table_record(&table, &plan, &changes, context)
    }

    /// The shared ordinary creation engine, with an explicitly owning
    /// table/plan.
    pub(super) fn insert_table_record(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        changes: &BTreeMap<u64, Option<String>>,
        context: WriteContext,
    ) -> Result<Record, DataError> {
        let environment = self.rule_environment(table.id, plan, context)?;
        // Compute and check every value before reserving a record id.
        let prepared = self.prepare_record(
            plan,
            (WriteMode::Create, Entry::default(), Written::New(None)),
            &BTreeMap::new(),
            changes,
            &environment,
        )?;
        let txn = self.new_transaction();
        let result = self.create(table, plan, prepared.values, txn);
        self.end_transaction(txn);
        result
    }

    fn create(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        values: EncodedChanges,
        txn: u32,
    ) -> Result<Record, DataError> {
        self.create_with(
            table,
            plan,
            values,
            txn,
            (Entry::default(), None, &BTreeSet::new()),
        )
    }

    /// [`Self::create`] of a write by `entry`: its unique, existing and
    /// value list rules are the ones `entry` checks. An import's cells
    /// (repetitions too) are checked cell by cell when read back.
    /// `prepared`: the preview the record is written from (preview.rs),
    /// used up once op71 may create the record, not by a refusal before.
    fn create_with(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        values: EncodedChanges,
        txn: u32,
        (entry, prepared, typed): (Entry, Option<om_data::PreparedId>, &BTreeSet<u64>),
    ) -> Result<Record, DataError> {
        self.refuse_related_lists(table, plan, entry, entered(&values))?;
        // Before a record is reserved: a new record's list does not hold it
        // (where it would, the write is refused), so it needs no id.
        self.check_related_lists(
            table,
            plan,
            (entry, typed),
            (&BTreeMap::new(), &values),
            (Written::New(None), crate::related_lists::Holding::none()),
        )?;
        let expected = (!checks_cells(entry))
            .then(|| expected_values(table, &values))
            .transpose()?;
        let withheld = self.withholds_new(table, &values)?;
        if let Some(id) = prepared {
            self.use_up_preview(id);
        }
        let reply = self
            .session_mut()?
            .create_record(&key_bytes(table.id), txn)
            .map_err(|e| failure("create record", e))?;
        if reply.status != 0 {
            let detail = reply
                .conflict
                .map(|c| format!(" The table is in use by {}.", c.users))
                .unwrap_or_default();
            return Err(DataError::new(format!(
                "The server refused to create a record (error {}).{detail}",
                reply.status
            )));
        }
        let record = match reply.record.as_slice() {
            [key] => serial::decode(key),
            _ => None,
        }
        .ok_or_else(|| DataError::new("The server returned an invalid record ID."))?;
        // A refusal before the commit leaves only a reserved id, which no
        // record uses (write-path.md 2): it is the write's refusal, with its
        // code, not a record created and unverified.
        self.check_unique(table, plan.unique_fields_for(entry), &values, record, txn)?;
        self.check_lookups(table, plan, entry, &values, record)?;
        (|| {
            // A new record has no modification count (write-path.md 5.4).
            let mut node = OwnedNode::list(serial::encode(record), Vec::new());
            apply_values(&mut node, values.clone());
            let commit = OwnedNode::leaf(serial::encode(record), vec![write::COMMIT_NEW]);
            self.store_pending_assets(table, &text_values(&stored_record(record, &node)), None);
            self.commit_record(table.id, node, commit, txn)?;
            // A record the set may not view comes back as its id and
            // modification count only: FileMaker Pro 26.0.3 creates it all
            // the same (#137's reading).
            if withheld {
                if let Some(record) = self.verify_withheld(table, record, 0)? {
                    return Ok(record);
                }
            }
            match &expected {
                Some(expected) => self.verify_record(table, record, expected, 0),
                None => self.verify_cells(table, record, &values, 0),
            }
        })()
        .map_err(|error| {
            DataError::new(format!(
                "Record {record} was created, but saving it could not be verified: {error} \
                 Refresh before retrying."
            ))
        })
    }

    /// Commit edits to an existing record under its lock (write-path.md 1.1).
    pub(super) fn edit_native_record(
        &mut self,
        layout_id: u32,
        record_id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_open()?;
        let (table, plan) = self.record_plan(layout_id)?;
        let changes = Self::writable_values(&table, &plan, &values)?;
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Account)?;
        self.edit_table_record(&table, &plan, record_id, &changes, context)
    }

    /// The shared ordinary edit engine, with an explicitly owning table/plan.
    pub(super) fn edit_table_record(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        record_id: i64,
        changes: &BTreeMap<u64, Option<String>>,
        context: WriteContext,
    ) -> Result<Record, DataError> {
        let environment = self.rule_environment(table.id, plan, context)?;
        let record = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let txn = self.new_transaction();
        let result = self.edit(table, plan, record, changes, &environment, txn);
        self.end_transaction(txn);
        result
    }

    fn edit(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        record: u64,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        txn: u32,
    ) -> Result<Record, DataError> {
        self.edit_with(
            table,
            plan,
            record,
            changes,
            environment,
            txn,
            (Entry::default(), &BTreeSet::new()),
        )
    }

    /// [`Self::edit`] of a write by `entry` (see [`Self::create_with`]).
    #[allow(clippy::too_many_arguments)]
    fn edit_with(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        record: u64,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        txn: u32,
        (entry, typed): (Entry, &BTreeSet<u64>),
    ) -> Result<Record, DataError> {
        // Prepare from a read-only snapshot before taking the record lock.
        // The lock's modification count below must still match this snapshot;
        // generated values are computed once and reused for the actual write.
        let mut node = self
            .read_record(table.id, record)?
            .ok_or_else(|| DataError::with_code(RECORD_MISSING, "This record no longer exists."))?;
        let stored = stored_record(record, &node);
        // As the host served it just now: a record its view formula is not
        // true on arrived without its values (#670).
        self.require_viewable(table.id, &text_values(&stored))?;
        let prepared = self.prepare_record(
            plan,
            (WriteMode::Update, entry, Written::Stored(record)),
            &text_values(&stored),
            changes,
            environment,
        )?;
        self.refuse_related_lists(table, plan, entry, entered(&prepared.values))?;
        let locked = self
            .lock_record(table.id, record, txn)?
            .ok_or_else(|| DataError::with_code(RECORD_MISSING, "This record no longer exists."))?;
        if self
            .observed_version((table.id, record))?
            .is_some_and(|seen| seen != locked)
        {
            return Err(DataError::with_code(
                STALE_RECORD,
                "Another client changed this record. Revert or refresh to load its changes \
                 before saving.",
            ));
        }
        if mod_count(&stored)? != locked {
            return Err(DataError::new(
                "The record changed while locking. Refresh before editing again.",
            ));
        }
        if prepared.changed_fields.is_empty() {
            let verified = self.verify_record(table, record, &FieldChanges::new(), locked)?;
            let cells: Vec<_> = changes.keys().copied().collect();
            self.store_pending_assets(table, &text_values(&stored), Some(&cells));
            return Ok(verified);
        }
        self.check_unique(
            table,
            plan.unique_fields_for(entry),
            &prepared.values,
            record,
            txn,
        )?;
        self.check_lookups(table, plan, entry, &prepared.values, record)?;
        self.check_related_lists(
            table,
            plan,
            (entry, typed),
            (&text_values(&stored), &prepared.values),
            (
                Written::Stored(record),
                crate::related_lists::Holding::none(),
            ),
        )?;
        let expected = (!checks_cells(entry))
            .then(|| expected_values(table, &prepared.values))
            .transpose()?;
        let written = prepared.values.clone();
        let next = locked.checked_add(1).ok_or_else(|| {
            DataError::new("The record's modification count is exhausted. No values were changed.")
        })?;
        // The stored container values the changed cells held: released
        // once readback verifies the commit (containers.md 5.1, 7.1).
        let replaced = super::containers::references(
            table,
            &text_values(&stored),
            Some(&prepared.changed_fields),
        );
        // The whole record with its new values and modification count; the
        // commit names the fields that changed (write-path.md 3.3, 3.4).
        apply_values(&mut node, prepared.values);
        node.set_child(OwnedNode::leaf(key_bytes(MOD_COUNT), mod_count_value(next)));
        let changed = changed_fields(table, &prepared.changed_fields)
            .into_iter()
            .map(|field| OwnedNode::leaf(key_bytes(field), Vec::new()))
            .collect();
        let commit = OwnedNode::list(serial::encode(record), changed);
        self.store_pending_assets(
            table,
            &text_values(&stored_record(record, &node)),
            Some(&prepared.changed_fields),
        );
        self.commit_record(table.id, node, commit, txn)?;
        let verified = match &expected {
            Some(expected) => self.verify_record(table, record, expected, next),
            None => self.verify_cells(table, record, &written, next),
        }?;
        self.release_members(table.id, replaced);
        Ok(verified)
    }

    /// Import Records' write of one row (`DataProvider::import_record`,
    /// script-steps.md "Implementation notes (import/export)"): `cells`
    /// into a new record or record `record`, with the import's entry
    /// options ([`Entry::import`]): every normal field of any type and
    /// repetition, "only during data entry" validation not checked,
    /// auto-enter only with `auto_enter` and never over a value the row
    /// sets. Inside a transaction the row is held as its rules left it.
    pub(super) fn import_native_record(
        &mut self,
        layout_id: u32,
        record: Option<i64>,
        cells: &BTreeMap<u64, Option<String>>,
        auto_enter: bool,
    ) -> Result<u64, DataError> {
        self.require_open()?;
        if self.in_transaction() {
            return self.hold_import(layout_id, record, cells, auto_enter);
        }
        let (table, plan) = self.record_plan(layout_id)?;
        let entry = Entry::import(auto_enter);
        // An import may be a script's step: its set is not known.
        let context = self.write_context(&plan, Some(layout_id), CurrentSet::Step(None))?;
        let environment = self.rule_environment(table.id, &plan, context)?;
        let txn = self.new_transaction();
        let result = match record {
            None => {
                self.prepare_record(
                    &plan,
                    (WriteMode::Create, entry, Written::New(None)),
                    &BTreeMap::new(),
                    cells,
                    &environment,
                )
                .and_then(|ready| {
                    let none = BTreeSet::new();
                    self.create_with(&table, &plan, ready.values, txn, (entry, None, &none))
                })
                .and_then(|r| u64::try_from(r.id).map_err(|_| DataError::new("Invalid record ID.")))
            }
            Some(id) => {
                let id = u64::try_from(id).map_err(|_| DataError::new("Invalid record ID."))?;
                let none = BTreeSet::new();
                self.edit_with(&table, &plan, id, cells, &environment, txn, (entry, &none))
                    .map(|_| id)
            }
        };
        self.end_transaction(txn);
        result
    }

    /// A script's write of one record (`DataProvider::write_record`):
    /// `cells` of a new record or record `record` of base table `table`,
    /// with a script's rules ([`Entry::script`]: any normal field and
    /// repetition; the entry rules only for fields validated Always, which
    /// Set Field checked; "Skip data entry validation" from Commit
    /// Records). Inside a transaction the record is held instead. `origin`:
    /// what the window it comes from answers for its formulas
    /// ([`Self::origin_context`]). Returns the record as stored. `typed`:
    /// the cells the user typed, checked against a related-values list by a
    /// user's rules (#1074).
    pub(crate) fn script_native_record(
        &mut self,
        table: u64,
        record: Option<i64>,
        (cells, typed): (&BTreeMap<u64, Option<String>>, &BTreeSet<u64>),
        skip_validation: bool,
        origin: Origin,
    ) -> Result<Record, DataError> {
        self.require_open()?;
        if self.in_transaction() {
            return self.hold_script(table, record, (cells, typed), origin);
        }
        let (table, plan) = self.table_plan(table)?;
        let prepared = origin.prepared;
        let context = self.origin_context(&plan, origin)?;
        let write = (record, prepared);
        let cells = (cells, typed);
        self.script_table_record(&table, &plan, write, cells, skip_validation, context)
    }

    /// Script entry rules over the same freshly planned owning context.
    /// `prepared`: the preview of the new record it creates (preview.rs),
    /// whose serial numbers, clock and UUID it takes, used up once op71 may
    /// create the record.
    pub(super) fn script_table_record(
        &mut self,
        table: &Table,
        plan: &RecordPlan,
        (record, prepared): (Option<i64>, Option<om_data::PreparedId>),
        (cells, typed): (&BTreeMap<u64, Option<String>>, &BTreeSet<u64>),
        skip_validation: bool,
        context: WriteContext,
    ) -> Result<Record, DataError> {
        let entry = script_entry(skip_validation, record.is_none());
        let mut environment = self.rule_environment(table.id, plan, context)?;
        if let Some(id) = prepared {
            let on_creation = plan
                .serials_for(WriteMode::Create, entry)
                .into_iter()
                .filter(|serial| !serial.on_commit)
                .map(|serial| serial.field)
                .collect();
            let reuse = self.preview_reuse(id, table.id, &on_creation)?;
            environment.serials = reuse.serials;
            reuse.stamp.apply(&mut environment);
        }
        let txn = self.new_transaction();
        let result = match record {
            None => self
                .prepare_record(
                    plan,
                    (WriteMode::Create, entry, Written::New(None)),
                    &BTreeMap::new(),
                    cells,
                    &environment,
                )
                .and_then(|ready| {
                    self.create_with(table, plan, ready.values, txn, (entry, prepared, typed))
                }),
            Some(id) => {
                let id = u64::try_from(id).map_err(|_| DataError::new("Invalid record ID."))?;
                self.edit_with(table, plan, id, cells, &environment, txn, (entry, typed))
            }
        };
        self.end_transaction(txn);
        result
    }

    /// Set Field's own checks (`DataProvider::check_entry`): the rules
    /// checked as a value is entered, for a field of `table` validated
    /// Always, with the open transaction's "Skip data entry validation";
    /// a value list of a field's values is looked up on the host (506).
    /// The plan comes from the schema read at open (no request), as
    /// FileMaker Pro checks against the definitions it holds.
    pub(super) fn check_script_entry(
        &mut self,
        table: u64,
        cell: u64,
        text: &str,
    ) -> Result<(), DataError> {
        let skip = self
            .transaction
            .as_ref()
            .is_some_and(|t| t.skips_validation());
        self.check_entry_by(table, cell, text, Entry::script(skip, false))
    }

    /// The user's typing into `cell` of a record a script writes
    /// (`ScriptWrite::typed`): the text it stores, a date, time or
    /// timestamp read as `update_record` reads it
    /// (`RecordPlan::typed_text`), then checked by a user's rules as a
    /// value is entered, as `update_record` checks it.
    pub(super) fn check_typed_entry(
        &mut self,
        table: u64,
        cell: u64,
        text: &str,
    ) -> Result<String, DataError> {
        let stored = self.entry_plan(table)?.typed_text(cell, text)?;
        self.check_entry_by(table, cell, &stored, Entry::default())?;
        Ok(stored)
    }

    /// The error `field` of `table` refuses an entry with: `code`, with the
    /// field's validation message when it has one.
    pub(super) fn entry_refusal(
        &mut self,
        table: u64,
        field: u64,
        code: i32,
    ) -> Result<DataError, DataError> {
        Ok(self.entry_plan(table)?.refusal(field, code))
    }

    /// The rules of `table`, from the schema read at open (with the opened
    /// file's tables, as `plan_schema` gives it, for an owner's write).
    pub(super) fn entry_plan(&mut self, table: u64) -> Result<RecordPlan, DataError> {
        self.refuse_imported_table(table)?;
        let table = self
            .catalog
            .table(table)
            .ok_or_else(|| {
                DataError::with_code(error::FIELD_MISSING, "The field's table is missing.")
            })?
            .clone();
        self.plan_table(&table, None)
    }

    /// The rules checked as a value is entered by `entry` (see
    /// [`Self::check_script_entry`]).
    fn check_entry_by(
        &mut self,
        table: u64,
        cell: u64,
        text: &str,
        entry: Entry,
    ) -> Result<(), DataError> {
        let plan = self.entry_plan(table)?;
        let file = self.calc_schema_cached()?;
        plan.check_entry(cell, text, entry)?;
        if text.is_empty() {
            return Ok(());
        }
        if let Some((occurrence, source)) = plan.value_list_field(cell, entry) {
            let table_def = self.catalog.table(table).expect("planned table").clone();
            self.refuse_related_lists(&table_def, &plan, entry, [cell])?;
            let source_table = file
                .table_of(occurrence)
                .map(|t| t.id)
                .ok_or_else(|| DataError::new("A value list names a missing table."))?;
            let file_language = self.file_language()?;
            let order = (!file.locale_unreadable).then_some(file.locale.date_order);
            let decimal = file.locale.decimal_separator;
            let keys = list_keys(
                &self.catalog,
                source_table,
                source,
                (file_language, order, decimal),
            )?;
            if !self.in_index(source_table, source, text, 0, keys)? {
                let field = if self
                    .catalog
                    .table(table)
                    .and_then(|t| t.field(cell))
                    .is_some()
                {
                    cell
                } else {
                    cell >> 8
                };
                return Err(plan.refusal(field, error::NOT_IN_LIST));
            }
        }
        Ok(())
    }

    /// Read the record back and check that the server kept each of
    /// `values` (cells by id, repetitions too) and modification count
    /// `mod_count`.
    pub(super) fn verify_cells(
        &mut self,
        table: &Table,
        record: u64,
        values: &EncodedChanges,
        mod_count: u64,
    ) -> Result<Record, DataError> {
        let node = self.read_record(table.id, record)?.ok_or_else(|| {
            DataError::new("The server did not keep the record. The write could not be verified.")
        })?;
        let stored = stored_record(record, &node);
        for (cell, value) in values {
            let kept = stored.cells.get(cell).and_then(|c| c.text()?.ok());
            let sent = value
                .as_ref()
                .map(|v| {
                    om_scsu::decode_scsu(v)
                        .map_err(|e| failure("decode a written value", format!("{e:?}")))
                })
                .transpose()?;
            if kept.as_deref().unwrap_or("") != sent.as_deref().unwrap_or("") {
                return Err(DataError::new(
                    "The server did not keep a written value. Refresh before editing again.",
                ));
            }
        }
        if self::mod_count(&stored)? != mod_count {
            return Err(DataError::new(
                "The server did not keep the expected record version. Refresh before editing \
                 again.",
            ));
        }
        self.wrote_version((table.id, record), Some(mod_count));
        Ok(Record {
            id: i64::try_from(record)
                .map_err(|_| DataError::new("Record ID exceeds the supported range."))?,
            values: stored.values_for(table),
        })
    }

    /// Delete Record: lock the record, commit it as deleted and release
    /// (write-path.md 7).
    pub(super) fn remove_native_record(
        &mut self,
        table: u64,
        record_id: i64,
    ) -> Result<(), DataError> {
        self.require_open()?;
        let record = u64::try_from(record_id).map_err(|_| DataError::new("Invalid record ID."))?;
        let txn = self.new_transaction();
        let result = self.delete(table, record, txn);
        self.end_transaction(txn);
        let held = result?;
        // The deleted record's container values (containers.md 7.3).
        if let (Some(texts), Some(def)) = (held, self.catalog.table(table).cloned()) {
            let members = super::containers::references(&def, &texts, None);
            self.release_members(table, members);
        }
        Ok(())
    }

    fn delete(
        &mut self,
        table: u64,
        record: u64,
        txn: u32,
    ) -> Result<Option<BTreeMap<u64, String>>, DataError> {
        if self.lock_record(table, record, txn)?.is_none() {
            return Err(DataError::with_code(
                RECORD_MISSING,
                "This record no longer exists.",
            ));
        }
        // The lock protects the ownership snapshot: another client may
        // have replaced a container before we acquired it.
        let held = self
            .read_record(table, record)?
            .map(|node| text_values(&stored_record(record, &node)));
        let entry = OwnedNode::leaf(serial::encode(record), vec![write::COMMIT_DELETED]);
        self.session_mut()?
            .commit_records(&key_bytes(table), vec![entry], txn)
            .map_err(|e| failure("commit record deletion", e))?;
        if self.read_record(table, record)?.is_some() {
            return Err(DataError::new("The server did not delete this record."));
        }
        self.wrote_version((table, record), None);
        Ok(held)
    }
}

/// A script's write: on a new record its values come after the record's
/// creation, so the creation's auto-entered values never replace them
/// (FileMaker enters them when New Record creates the record, before the
/// script's Set Field).
pub(super) fn script_entry(skip_validation: bool, create: bool) -> Entry<'static> {
    Entry {
        keep_written: create,
        ..Entry::script(skip_validation, false)
    }
}

/// Whether a write by `entry` is read back cell by cell: an import's and a
/// script's set repetitions too, which [`expected_values`] cannot name.
fn checks_cells(entry: Entry) -> bool {
    entry.keep_written || entry.script
}

/// The cells of `values` given a value: those a write checks.
pub(super) fn entered<V: AsRef<[u8]>>(
    values: &BTreeMap<u64, Option<V>>,
) -> impl Iterator<Item = u64> + '_ {
    values
        .iter()
        .filter(|(_, value)| value.as_ref().is_some_and(|v| !v.as_ref().is_empty()))
        .map(|(cell, _)| *cell)
}

/// The fields whose cells `cells` names (repetition n of field F is the
/// cell `F * 256 + n`), for op70's list of changed fields.
pub(super) fn changed_fields(table: &Table, cells: &[u64]) -> Vec<u64> {
    let mut fields: Vec<u64> = cells
        .iter()
        .map(|&cell| {
            if table.field(cell).is_some() {
                cell
            } else {
                cell >> 8
            }
        })
        .collect();
    fields.sort_unstable();
    fields.dedup();
    fields
}

pub(super) fn expected_values(
    table: &Table,
    values: &EncodedChanges,
) -> Result<FieldChanges, DataError> {
    values
        .iter()
        .map(|(id, bytes)| {
            let field = table
                .field(*id)
                .ok_or_else(|| DataError::new("A generated field no longer exists."))?;
            let text = bytes
                .as_ref()
                .map(|bytes| {
                    om_scsu::decode_scsu(bytes)
                        .map_err(|e| failure("decode generated field", format!("{e:?}")))
                })
                .transpose()?;
            Ok((field.name.clone(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn confirmations_end_at_the_requested_record_boundary() {
        for slots in [
            &[0x80, 0x40][..],
            &[0x80, 0x00, 1, 0x40],
            &[0x80, 0x20, 1, 0x80, 0x00, 2, 0x80, 0x00, 3, 0x40, 0x40],
        ] {
            super::validate_confirmation(slots).unwrap();
        }
        for slots in [
            &[][..],
            &[0x40],
            &[0x80],
            &[0x80, 0x40, 0x00, 0x7f],
            &[0x80, 0x40, 0x80, 0x40],
            &[0x80, 0x40, 0x40],
            &[0x80, 0x20, 1, 0x40],
        ] {
            assert!(super::validate_confirmation(slots).is_err(), "{slots:02x?}");
        }
    }

    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn record_with(cells: Vec<OwnedNode>) -> om_hbam::Record {
        stored_record(1, &OwnedNode::list(vec![1], cells))
    }

    #[test]
    fn mod_counts_are_a_length_byte_and_a_serial_key() {
        // write-path.md 5.4 and 4.2: cap:936 `01 00`, cap:942 `01 01`.
        for (count, value) in [
            (0, &[1, 0][..]),
            (1, &[1, 1]),
            (127, &[1, 0x7f]),
            (128, &[2, 0x80, 0x00]),
            (0x4080, &[3, 0xc0, 0, 0]),
        ] {
            assert_eq!(mod_count_value(count), value);
            assert_eq!(parse_mod_count(value).unwrap(), count);
        }
        for invalid in [
            &[][..],
            &[0],
            &[1],
            &[2, 0x01, 0x00],
            &[1, 0x80],
            &[1, 1, 0],
        ] {
            assert!(parse_mod_count(invalid).is_err(), "{invalid:02x?}");
        }
    }

    #[test]
    fn a_record_never_modified_counts_zero_and_bad_metadata_fails() {
        assert_eq!(mod_count(&record_with(vec![])).unwrap(), 0);
        let count =
            |value: Vec<u8>| mod_count(&record_with(vec![OwnedNode::leaf(vec![0xfc], value)]));
        assert_eq!(count(vec![1, 3]).unwrap(), 3);
        assert!(count(vec![]).is_err());
        assert!(count(vec![1]).is_err());
        let nested = OwnedNode::list(vec![0xfc], vec![OwnedNode::leaf(vec![7], vec![1])]);
        assert!(mod_count(&record_with(vec![nested])).is_err());
    }

    #[test]
    fn edits_keep_untouched_cells_nested_data_and_unicode() {
        let nested = OwnedNode::list(vec![3], vec![OwnedNode::leaf(vec![1], vec![1, 2, 3])]);
        let mut node = OwnedNode::list(
            vec![9],
            vec![
                OwnedNode::leaf(vec![2], om_scsu::encode_scsu("Old")),
                nested.clone(),
                OwnedNode::leaf(vec![0xfc], vec![1, 1]),
            ],
        );
        apply_values(
            &mut node,
            BTreeMap::from([
                (1, Some(om_scsu::encode_scsu("新しい"))),
                (2, None),
                (300, Some(om_scsu::encode_scsu("Kept"))),
            ]),
        );
        let segments: Vec<&[u8]> = node.children.iter().map(|c| c.segment.as_slice()).collect();
        // Field ids key as the catalog's big-endian numbers: 300 is `01 2c`.
        assert_eq!(segments, [&[1][..], &[0x01, 0x2c], &[3], &[0xfc]]);
        assert_eq!(node.child(&[3]), Some(&nested));
        let stored = stored_record(9, &node);
        assert_eq!(
            text_values(&stored),
            BTreeMap::from([(1, "新しい".into()), (300, "Kept".into())])
        );
        assert_eq!(mod_count(&stored).unwrap(), 1);
    }

    /// A container reference as FileMaker Pro 26.0.3 stored it (#911): `01`
    /// and U+E000, five masked SCSU bytes, under a header of their byte
    /// length and three UTF-16 units. Stored as one leaf, Pro shows the
    /// reference text instead of the picture.
    #[test]
    fn a_container_reference_is_stored_as_filemaker_pro_stores_it() {
        let reference = om_scsu::encode_scsu("01\u{e000}");
        assert_eq!(reference, hex("6a 6b 41 32 da"));
        let plain = om_scsu::encode_scsu("Plain");
        let mut node = OwnedNode::list(vec![9], Vec::new());
        apply_values(
            &mut node,
            BTreeMap::from([(5, Some(reference.clone())), (6, Some(plain.clone()))]),
        );
        let cell = node.child(&[5]).unwrap();
        assert_eq!(cell.value, None);
        let children: Vec<(&[u8], Option<&[u8]>)> = cell
            .children
            .iter()
            .map(|c| (c.segment.as_slice(), c.value.as_deref()))
            .collect();
        assert_eq!(
            children,
            [
                (&[0][..], Some(&hex("00000005 00000003")[..])),
                (&[1][..], Some(&reference[..]))
            ]
        );
        // Text of one byte per character stays one leaf.
        assert_eq!(node.child(&[6]).unwrap().value.as_deref(), Some(&plain[..]));
        assert_eq!(
            text_values(&stored_record(9, &node)),
            BTreeMap::from([(5, "01\u{e000}".into()), (6, "Plain".into())])
        );
    }

    #[test]
    fn chunked_text_is_read_as_a_rule_input() {
        let text = om_scsu::encode_scsu("Ångström 東京");
        let chunked = OwnedNode::list(
            key_bytes(300),
            vec![
                OwnedNode::leaf(vec![0], (text.len() as u32).to_be_bytes().to_vec()),
                OwnedNode::leaf(vec![1], text[..4].to_vec()),
                OwnedNode::leaf(vec![2], text[4..].to_vec()),
            ],
        );
        let stored = record_with(vec![chunked]);
        assert_eq!(
            text_values(&stored),
            BTreeMap::from([(300, "Ångström 東京".into())])
        );
    }

    #[test]
    fn uniqueness_check_matches_what_filemaker_pro_sends() {
        // FileMaker Pro's ExistsInIndex for table 80 04, field 06 (write-path.md
        // 1.3), up to its access key: the same leaves in the same slot codes.
        let program =
            exists_in_index_program(1, &[7; 16], 0x8004, 6, &[om_collate::text_key("a")], 2)
                .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&program)).unwrap();
        let check = tree.node(&[&[1], &[1]]).unwrap();
        assert_eq!(check.child(&[0]).unwrap().value, Some(&[42][..]));
        assert_eq!(
            check.child(&[1]).unwrap().value,
            Some(&hex("02 8004 01 01 01 06 01 10")[..])
        );
        assert_eq!(check.child(&[2]).unwrap().value, Some(&[1, 3, 1, 4][..]));
        assert!(check.child(&[7]).is_none());
        let instruction = tree.node(&[&[1], &[2]]).unwrap();
        let access = om_crypto::query_access_key(2, &[7; 16], 1);
        assert_eq!(instruction.child(&[0]).unwrap().value, Some(&[18][..]));
        assert_eq!(instruction.child(&[0x07]).unwrap().value, Some(&access[..]));
        assert_eq!(instruction.child(&[0x09]).unwrap().value, Some(&[1, 6][..]));
        assert_eq!(
            instruction.child(&[0x10]).unwrap().value,
            Some(&[1, 2, 1, 2][..])
        );
        let key = tree.node(&[&[2], &[1]]).unwrap();
        assert_eq!(key.children[0].segment.0, [0x01, 0x12, 0x0f]);
        let ignored = tree.node(&[&[2], &[2]]).unwrap();
        assert_eq!(ignored.children[0].segment.0, [0x02]);
        assert_eq!(ignored.children[0].value, Some(&[][..]));

        // Literal op18 answers (8.4), with counts already present before
        // it ran. Zero counts still identify an existing, empty index.
        for (flag, expected) in [("0101", true), ("0100", false)] {
            let slots = hex(&format!(
                "80 2003 80 0200 {flag} 40 2004 80 0600 10 00000000000000000000000000000000 40 40"
            ));
            assert_eq!(exists_in_index_result(&slots).unwrap(), Some(expected));
            let missing = hex(&format!("80 2003 80 0200 {flag} 40 40"));
            assert_eq!(exists_in_index_result(&missing).unwrap(), None);
        }
        assert!(
            exists_in_index_result(&hex("80 2003 80 0200 0100 40 2004 80 0100 01 40 40")).is_err()
        );
        let error = exists_in_index_result(&hex("80 0600 03 020066 40")).unwrap_err();
        assert!(error.to_string().contains("error 102"), "{error}");
        assert!(exists_in_index_result(&hex("80 40")).is_err());
    }

    #[test]
    fn refused_locks_name_who_holds_them() {
        let conflict = LockReply::Refused {
            status: write::LOCK_CONFLICT,
            conflict: Conflict {
                self_conflict: false,
                users: "openmaker (admin)".into(),
            },
        };
        assert_eq!(
            lock_refused("This record", conflict).to_string(),
            "This record is being modified by openmaker (admin). Try again when they are done."
        );
        let other = LockReply::Refused {
            status: 301,
            conflict: Conflict {
                self_conflict: false,
                users: String::new(),
            },
        };
        assert!(lock_refused("This record", other)
            .to_string()
            .contains("error 301"));
        assert!(lock_refused("This record", LockReply::MustWait)
            .to_string()
            .contains("locked"));
    }

    #[test]
    fn native_writes_require_an_open_authenticated_session() {
        let mut provider = NativeProvider::new(NativeConfig {
            client_cert_pem: vec![],
            client_key_pem: vec![],
            additional_ca_pem: vec![],
            callback_host: String::new(),
            machine: MachineInfo::default(),
        });
        let create = provider
            .insert_native_record(1, FieldChanges::new())
            .unwrap_err();
        let update = provider
            .edit_native_record(1, 1, FieldChanges::new())
            .unwrap_err();
        let delete = provider.remove_native_record(1, 1).unwrap_err();
        for error in [create, update, delete] {
            assert!(error.to_string().contains("No hosted file is open"));
        }
        assert!(provider.session.is_none());
    }
}

/// What a script's write knows of the window it comes from, for its
/// formulas ([`NativeProvider::origin_context`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    /// The window's layout of this file; `None` for a write routed here
    /// from another file's window.
    pub layout: Option<u32>,
    /// Whose privilege set is current.
    pub set: CurrentSet,
    /// The written record's `Get ( RecordNumber )` in that window
    /// (`ScriptWrite::record_number`, #1038), the same in the file a
    /// routed write reaches.
    pub record_number: Option<u64>,
    /// The preview of the new record the write creates
    /// (`ScriptWrite::prepared`, preview.rs): its context, serial numbers,
    /// clock and UUID are the write's.
    pub prepared: Option<om_data::PreparedId>,
}

/// Whose privilege set a write's formulas read as current
/// (`Get ( CurrentPrivilegeSetName )`, [`NativeProvider::write_context`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CurrentSet {
    /// This file's own session's account set: a user's write, which is
    /// never a step running with full access, and a write routed here from
    /// another file, whose script's elevation is that file's, not this
    /// one's.
    Account,
    /// The set the script step runs with, as its window gave it
    /// (`ScriptWrite::privilege_set`): [Full Access] in a step running with
    /// full access. `None` when not known, as for an import, which then
    /// refuses a write whose formula reads it.
    Step(Option<String>),
}
