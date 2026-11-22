//! Value lists, field index values and serial numbers of the hosted file:
//! what sorts by a value list, Insert from Index and Set Next Serial Value
//! read and write.
//!
//! - A value list is the catalog member `[21, 5, V]` (file-metadata.md 6):
//!   `03` its custom values (CR-separated, `-` a divider), `02` the
//!   definition, whose first byte 1 takes the values from a field (its
//!   occurrence and field follow as a field spec, 6.2). A field's values are
//!   its value index's distinct values in index order, which CreateValueListEx
//!   lists (query-program.md 8.7). A list sorted by its second field
//!   orders the first field's values by the second's, read from every
//!   record; a second field only shown beside the first leaves the order.
//!   Lists of related values only, whose second field is in another table,
//!   from another file or from a calculation are error 3.
//! - Insert from Index lists the field's value index the same way.
//! - Set Next Serial Value is op72 with `kReplace` (write-path.md 3.2): the
//!   host stores the text as the field's next serial value.

use super::*;
use om_collate::{DateOrder, NumberFormat};
use om_data::found::code;
use om_hbam::{Cell, DataType};
use om_protocol::write;
use om_query::keys::{TextKeys, EMPTY_TEXT_KEY};

/// The value-list catalog `[21]` and its members' key `5`.
const VALUE_LISTS: u8 = 0x21;
const MEMBERS: u8 = 0x05;
/// A member's definition `02` and custom values `03` (file-metadata.md 6.1).
const DEFINITION: u8 = 0x02;
const CUSTOM_VALUES: u8 = 0x03;
/// The most values a field's list holds here, on every path
/// (docs/value-lists-missing-index.md): past it the list is an error, never
/// its first part.
const MAX_INDEX_VALUES: u32 = 100_000;
/// Keys of a value index that are no value: the empty value `01` and the
/// long-key entry `ff 80` (query-program.md 7.1).
const OTHER_INDEX_KEYS: u64 = 2;
/// A table's indexes `[T, 01]`, and a field's value index under them.
const INDEXES: u8 = 0x01;
const VALUE_INDEX: u8 = 0x00;
const SCAN_BATCH: usize = 500;
/// Retained key and display bytes, in addition to at most 100,000 map nodes.
const VALUE_BYTES: usize = 64 << 20;

/// The host's answer to a field's CreateValueListEx
/// (`NativeProvider::listed_field_values`).
enum Listed {
    /// Every value, in index order.
    Whole(Vec<String>),
    /// The field has no value index.
    NoIndex,
    /// Not the whole list; the index holds this many keys.
    Partial(u64),
    /// The reply could not be read, or listed more values than allowed.
    Unreadable,
}

/// A field's values as they are found: each distinct value by the key it
/// sorts by, with the lowest record that holds it and its text. Display
/// ties use the lowest record id (8.7, inferred there), then the first
/// repetition and line (our deterministic policy). More than `limit`
/// values, or `budget` bytes of keys and text, is an error, never a
/// shorter list.
struct Values {
    values: BTreeMap<Vec<u8>, (u64, String)>,
    bytes: usize,
    limit: usize,
    budget: usize,
}

impl Values {
    fn new(limit: usize, budget: usize) -> Self {
        Self {
            values: BTreeMap::new(),
            bytes: 0,
            limit,
            budget,
        }
    }

    fn add(&mut self, key: Vec<u8>, id: u64, value: &str) -> Result<(), DataError> {
        if key == EMPTY_TEXT_KEY {
            return Ok(());
        }
        match self.values.get(&key) {
            Some((old_id, _)) if *old_id <= id => return Ok(()),
            None if self.values.len() == self.limit => {
                return Err(unsupported(format!(
                    "The field has more than {} distinct values, the most a list holds here.",
                    self.limit
                )))
            }
            _ => {}
        }
        let old = self
            .values
            .get(&key)
            .map_or(0, |(_, display)| key.len() + display.len());
        let bytes = self.bytes - old + key.len() + value.len();
        if bytes > self.budget {
            return Err(unsupported(
                "Listing the field's values exceeded the retained-value byte budget.",
            ));
        }
        self.values.insert(key, (id, value.to_owned()));
        self.bytes = bytes;
        Ok(())
    }

    fn finish(self) -> Vec<String> {
        self.values.into_values().map(|(_, value)| value).collect()
    }
}

/// The whole key of one line or typed value, before an index cuts it, in
/// only the native forms established by query-program.md 7.2-7.3. A typed
/// value with no digit, which FileMaker lists as invalid (`fe ff`), is not
/// a value a list shows. Whole keys tell apart values that share a 219-byte
/// cut key (7.1), and sort as the index does.
fn native_key(
    data_type: DataType,
    value: &str,
    order: DateOrder,
    numbers: NumberFormat,
    text: TextKeys,
) -> Result<Option<Vec<u8>>, DataError> {
    let key = match data_type {
        DataType::Text => text.whole_key(value),
        _ if value.is_empty() => return Ok(None),
        DataType::Number => om_collate::number_key_in(value, numbers),
        DataType::Date => om_collate::date_key(value, order),
        DataType::Time => om_collate::time_key(value),
        DataType::Timestamp => om_collate::timestamp_key(value, order),
        _ => None,
    };
    if matches!(data_type, DataType::Date | DataType::Timestamp) {
        if let Some(key) = &key {
            let date = u32::from_be_bytes([0, key[0] & 0x7f, key[1], key[2]]);
            if chrono::NaiveDate::from_ymd_opt((date >> 10) as i32, (date >> 6) & 15, date & 63)
                .is_none()
            {
                return Err(unsupported(
                    "A stored value is not a valid native calendar date.",
                ));
            }
        }
    }
    match key {
        Some(key) => Ok(Some(key)),
        None if data_type == DataType::Text && text == TextKeys::None => Ok(None),
        None => Err(unsupported("A stored value has no established native index key; the field's values cannot be listed from it.")),
    }
}

/// How a field's values are keyed, as its value index keys them: its text
/// form, and for dates and timestamps the file's date order. Either may be
/// unknown; keying then fails only when a value must be keyed.
struct Keying {
    data_type: DataType,
    field: String,
    language: u32,
    text: Option<TextKeys>,
    order: Option<DateOrder>,
    /// The file's number format, for a number field.
    numbers: Option<NumberFormat>,
}

impl Keying {
    /// The key `value` ranks by as a list's second field (file-metadata.md
    /// 6.4), as the field's value index holds it: text in the field's form,
    /// a typed value as `typed_value_keys` keys it (empty `01`, no digit
    /// `fe ff`, a timestamp stored as a date alone by its date's midnight).
    /// Error 3 when the index holds the value under no key.
    fn rank_key(&self, value: &str) -> Result<Vec<u8>, DataError> {
        if self.data_type == DataType::Text {
            return Ok(self
                .whole_key(value)?
                .unwrap_or_else(|| EMPTY_TEXT_KEY.to_vec()));
        }
        let order = match self.data_type {
            DataType::Date | DataType::Timestamp => self
                .order
                .ok_or_else(|| unsupported("The file's date order could not be read."))?,
            _ => DateOrder::MonthDayYear,
        };
        om_query::keys::typed_value_keys(self.data_type, value, order)
            .into_iter()
            .next()
            .ok_or_else(|| {
                unsupported(
                    "A stored value has no established native index key; the field's values cannot be listed from it.",
                )
            })
    }

    /// [`native_key`] of `value`, or error 3 when the field's form cannot be
    /// keyed exactly.
    fn whole_key(&self, value: &str) -> Result<Option<Vec<u8>>, DataError> {
        let text = match self.data_type {
            DataType::Text => self.text.ok_or_else(|| self.unestablished())?,
            _ => TextKeys::Collated,
        };
        let order = match self.data_type {
            DataType::Date | DataType::Timestamp => self
                .order
                .ok_or_else(|| unsupported("The file's date order could not be read."))?,
            _ => DateOrder::MonthDayYear,
        };
        let numbers = match self.data_type {
            DataType::Number => self
                .numbers
                .ok_or_else(|| unsupported("The file's number format could not be read."))?,
            _ => NumberFormat::default(),
        };
        native_key(self.data_type, value, order, numbers, text)
    }

    /// Error 3 for text that must be keyed in a form with no established
    /// keys (query-program.md 7.2).
    fn unestablished(&self) -> DataError {
        unsupported(format!(
            "Field {:?} has values that must be keyed to be listed, and its index language ({}) has no established native keys.",
            self.field, self.language
        ))
    }
}

/// A record's candidates for `field`: what its value index keys. Its
/// non-empty lines for text (the index keys each line alone,
/// query-program.md 7.1), its non-empty values otherwise, in every
/// declared repetition, repetition first.
fn candidates(
    row: &om_hbam::record::Record,
    field: &om_hbam::Field,
    repetitions: u16,
) -> Result<Vec<String>, DataError> {
    let mut out = Vec::new();
    for repetition in 1..=repetitions.max(1) {
        let cell = if repetition == 1 {
            field.id
        } else {
            field
                .id
                .checked_mul(256)
                .and_then(|id| id.checked_add(u64::from(repetition)))
                .ok_or_else(|| unsupported("The field's repetition id overflows."))?
        };
        let Some(cell) = row.cells.get(&cell) else {
            continue;
        };
        let value = cell
            .text()
            .ok_or_else(|| unsupported("A stored field value is not readable text."))?
            .map_err(|e| failure("decode a field value", e))?;
        if field.data_type == DataType::Text {
            out.extend(
                value
                    .split(['\r', '\n'])
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned),
            );
        } else if !value.is_empty() {
            out.push(value);
        }
    }
    Ok(out)
}

/// The first of `candidates` whose key, as an index holds it, is `key`;
/// an error when one before it cannot be keyed.
fn keyed<'c>(
    keying: &Keying,
    candidates: &'c [String],
    key: &[u8],
) -> Result<Option<&'c String>, DataError> {
    for candidate in candidates {
        if let Some(mut whole) = keying.whole_key(candidate)? {
            om_collate::cut_index_key(&mut whole);
            if whole == key {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// The values the index holds no list for: error 3.
fn too_many_values() -> DataError {
    unsupported(format!(
        "The field has more than {MAX_INDEX_VALUES} distinct values, the most a list holds here."
    ))
}

/// The index names a value its records do not hold.
fn index_disagrees() -> DataError {
    DataError::new("The field's index names a value that its records do not hold.")
}

fn unsupported(message: impl Into<String>) -> DataError {
    DataError::with_code(code::UNSUPPORTED, message)
}

/// A stream key `[u8 n][segments]` of one segment: its value.
fn key_value(bytes: &mut &[u8]) -> Option<Option<u64>> {
    let (&total, rest) = bytes.split_first()?;
    let key = rest.get(..usize::from(total))?;
    *bytes = &rest[usize::from(total)..];
    let Some((&len, segment)) = key.split_first() else {
        return Some(None);
    };
    if usize::from(len) != segment.len() {
        return None;
    }
    Some(om_hbam::slot::be_uint(segment))
}

/// A field value list's primary field: (occurrence, field), from the
/// definition `[u8 type][u16 options][field spec]...` (file-metadata.md
/// 6.2).
fn primary_field(definition: &[u8]) -> Option<(u64, u64)> {
    let mut rest = definition.get(3..)?;
    let occurrence = key_value(&mut rest)??;
    let field = key_value(&mut rest)??;
    Some((occurrence, field))
}

/// Value-list options (`DBValueList::Options`, file-metadata.md 6.2).
const SHOW_RELATED_VALUES: u16 = 0x1;
const SORT_BY_SECONDARY_FIELD: u16 = 0x2;

/// A field value list's options, its first field's repetition and its
/// second field ("also display values from second field": occurrence,
/// field and repetition, `None` when the definition names none, `0:0`),
/// from the definition's `u16` options and its two field specs (occurrence
/// key, field key, u16 repetition each; a repetition of 0 is the first).
struct ListOptions {
    options: u16,
    repetition: u16,
    second: Option<(u64, u64, u16)>,
}

fn list_options(definition: &[u8]) -> Option<ListOptions> {
    let u16_at = |rest: &mut &[u8]| -> Option<u16> {
        let value = u16::from_be_bytes(rest.get(..2)?.try_into().ok()?);
        *rest = &rest[2..];
        Some(value.max(1))
    };
    let options = u16::from_be_bytes(definition.get(1..3)?.try_into().ok()?);
    let mut rest = definition.get(3..)?;
    key_value(&mut rest)?;
    key_value(&mut rest)?;
    let repetition = u16_at(&mut rest)?;
    let occurrence = key_value(&mut rest)?.filter(|id| *id != 0);
    let field = key_value(&mut rest)?.filter(|id| *id != 0);
    let second_repetition = u16_at(&mut rest)?;
    Some(ListOptions {
        options,
        repetition,
        second: occurrence
            .zip(field)
            .map(|(occurrence, field)| (occurrence, field, second_repetition)),
    })
}

/// Whether the provider can read `field`'s values from its stored records:
/// a stored field or calculation, not a global, an unstored calculation, a
/// summary, a container or an unknown type.
fn scannable(field: &om_hbam::Field, options: &om_hbam::FieldOptions) -> Result<(), DataError> {
    if !matches!(
        field.kind,
        om_hbam::FieldKind::Normal | om_hbam::FieldKind::Calculation
    ) || options.options & 0x70000 != 0
        || matches!(field.data_type, DataType::Container | DataType::Unknown(_))
    {
        return Err(unsupported(
            "This field has no stored native values that can be listed without its index.",
        ));
    }
    Ok(())
}

/// One repetition's stored text of `field` in `row` (repetition 1 the
/// field's own cell); empty when the record stores none, an error when the
/// stored cell is not readable text.
fn repetition_text(
    row: &om_hbam::record::Record,
    field: u64,
    repetition: u16,
) -> Result<String, DataError> {
    let Some(cell) = om_data::rules::cell_of(field, u32::from(repetition)) else {
        return Ok(String::new());
    };
    match row.cells.get(&cell) {
        None => Ok(String::new()),
        Some(value) => value
            .text()
            .ok_or_else(|| unsupported("a stored cell is not readable text."))?
            .map_err(|e| failure("decode a value list's stored value", e)),
    }
}

impl NativeProvider {
    /// The base table an occurrence of this file's table shows; error 3
    /// for an occurrence of a related file's table.
    fn occurrence_table(&self, occurrence: u64) -> Result<u64, DataError> {
        let missing = || {
            DataError::with_code(
                om_data::rules::error::FIELD_MISSING,
                format!("The file has no table occurrence {occurrence}."),
            )
        };
        let found = self
            .catalog
            .table_occurrences
            .iter()
            .find(|o| o.id == occurrence)
            .ok_or_else(missing)?;
        match found.reference() {
            Some((0, table)) => Ok(table),
            Some(_) => Err(related_file_occurrence(&found.name)),
            None => Err(missing()),
        }
    }

    /// The distinct values of `field` of base table `table`, in index order:
    /// the host's list when it is the whole list, else read from the index,
    /// or from every record when the index is missing
    /// (docs/value-lists-missing-index.md).
    fn field_values(&mut self, table: u64, field: u64) -> Result<Vec<String>, DataError> {
        let definition = self
            .catalog
            .table(table)
            .and_then(|t| t.field(field))
            .cloned()
            .ok_or_else(|| {
                DataError::with_code(code::FIELD_MISSING, "A value list names a missing field.")
            })?;
        match self.listed_field_values(table, field)? {
            Listed::Whole(values) => Ok(values),
            Listed::NoIndex => self.field_values_without_index(table, &definition),
            Listed::Partial(keys) => self.field_values_from_index(table, &definition, keys),
            Listed::Unreadable => Err(DataError::new(
                "The host's value list reply could not be read.",
            )),
        }
    }

    /// `DataProvider::host_field_values`: the host's list of a hosted file's
    /// field when it is the whole list. A local file's in-process host is
    /// not asked yet; the window reads its records.
    pub(crate) fn host_field_values(
        &mut self,
        table: u64,
        field: u64,
    ) -> Result<Option<Vec<String>>, DataError> {
        let known = self
            .catalog
            .table(table)
            .is_some_and(|t| t.field(field).is_some());
        if self.local.is_some() || !known {
            return Ok(None);
        }
        Ok(match self.listed_field_values(table, field) {
            Ok(Listed::Whole(values)) => Some(values),
            Ok(_) => None,
            // The host refused the program: the records still read.
            Err(error) if error.code.is_some() => None,
            Err(error) => return Err(error),
        })
    }

    /// The host's answer to one CreateValueListEx for `field` of `table`
    /// with no prefix, so every value (8.7), and the value index's counts.
    fn listed_field_values(&mut self, table: u64, field: u64) -> Result<Listed, DataError> {
        let session = self.session_mut()?;
        let file_id = session
            .opened_file()
            .ok_or_else(|| DataError::new("No hosted file is open."))?
            .reply
            .file_id;
        let key = om_crypto::query_access_key(2, session.connection_key(), file_id);
        let program = om_query::find::checked_value_list_program(
            file_id,
            table,
            field,
            MAX_INDEX_VALUES,
            Some(key),
        );
        let reply = session
            .query(&program)
            .map_err(|e| failure("list a field's values", e))?;
        if let Some(code) = found::query_error(&reply.slots) {
            // A list refused on a missing index is read from the records,
            // as after FileMaker Server's list without one.
            let index = [(field, om_query::find::IndexKind::Value)];
            if found::missing_after_refusal(session, code, table, &index)?.is_some() {
                return Ok(Listed::NoIndex);
            }
            return Err(DataError::with_code(
                i32::from(code),
                format!("The host could not list the field's values (error {code})."),
            ));
        }
        // Validate both replies even when the list ran before its index existed.
        let (Some(counts), Some(listed)) = (
            om_query::find::value_list_index_counts(&reply.slots),
            om_query::find::value_list_values(&reply.slots),
        ) else {
            return Ok(Listed::Unreadable);
        };
        if listed.values.len() > MAX_INDEX_VALUES as usize {
            return Ok(Listed::Unreadable);
        }
        let Some((_, keys)) = counts else {
            return Ok(Listed::NoIndex);
        };
        // A list the host cut, or a key as long as an index holds, which may
        // stand for several values (7.1), is not the whole list (8.7).
        if listed.complete
            && listed
                .values
                .iter()
                .all(|(key, _)| key.len() < om_collate::INDEX_KEY_LIMIT)
        {
            return Ok(Listed::Whole(
                listed.values.into_iter().map(|(_, text)| text).collect(),
            ));
        }
        Ok(Listed::Partial(keys))
    }

    /// The field's values read from its value index and the records it
    /// names, when the host's list is not the whole list
    /// (docs/value-lists-missing-index.md, "Reading the index"). `keys` is
    /// the index's count of distinct keys.
    fn field_values_from_index(
        &mut self,
        table: u64,
        field: &om_hbam::Field,
        keys: u64,
    ) -> Result<Vec<String>, DataError> {
        if keys > u64::from(MAX_INDEX_VALUES) + OTHER_INDEX_KEYS {
            return Err(too_many_values());
        }
        let options = field
            .decoded_options()
            .ok_or_else(|| unsupported("The field's index metadata could not be read."))?;
        let path = vec![
            key_bytes(table),
            vec![INDEXES],
            key_bytes(field.id),
            vec![VALUE_INDEX],
        ];
        let Some(data) = download_node(self.session_mut()?, path.clone())? else {
            // The index went away since the list ran.
            return self.field_values_without_index(table, field);
        };
        let tree = download_tree(std::slice::from_ref(&data))?;
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let index = tree.node(&refs).ok_or_else(index_disagrees)?;
        // A key shorter than an index holds is one value, shown by its lowest
        // record; a key as long may be cut, and stands for every value of the
        // records under it whose key it starts (7.1).
        let mut whole: Vec<(Vec<u8>, u64)> = Vec::new();
        let mut cut: Vec<(Vec<u8>, RidList)> = Vec::new();
        for entry in &index.children {
            let key = entry.segment.0;
            if key == EMPTY_TEXT_KEY || key == om_collate::TOO_LONG_KEY {
                continue;
            }
            let records =
                RidList::from_node(entry).map_err(|e| failure("decode the field's index", e))?;
            if records.is_empty() {
                continue;
            }
            if key.len() >= om_collate::INDEX_KEY_LIMIT {
                cut.push((key.to_vec(), records));
            } else if let Some(lowest) = records.iter().next() {
                whole.push((key.to_vec(), lowest));
            }
        }
        if whole.len() + cut.len() > MAX_INDEX_VALUES as usize {
            return Err(too_many_values());
        }
        drop(tree);
        drop(data);
        // Which keys each record answers for.
        let mut lowest_of: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (at, (_, id)) in whole.iter().enumerate() {
            lowest_of.entry(*id).or_default().push(at);
        }
        let mut under: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (at, (_, records)) in cut.iter().enumerate() {
            for id in records.iter() {
                under.entry(id).or_default().push(at);
            }
        }
        let keying = self.keying(field, &options, false)?;
        let ids: Vec<u64> = lowest_of
            .keys()
            .chain(under.keys())
            .copied()
            .collect::<std::collections::BTreeSet<u64>>()
            .into_iter()
            .collect();
        let mut values = Values::new(MAX_INDEX_VALUES as usize, VALUE_BYTES);
        let mut shown = vec![false; whole.len()];
        // A cut key's values when they cannot be keyed: one text, kept only
        // while every record under the key holds that same text alone.
        let mut unkeyed: Vec<Option<Result<(u64, String), ()>>> = vec![None; cut.len()];
        let mut met = vec![false; cut.len()];
        let session = self.session_mut()?;
        for batch in ids.chunks(SCAN_BATCH) {
            let data = download_records(session, table, batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            for row in om_hbam::record::records(&tree, table) {
                let candidates = candidates(&row, field, options.max_repetitions)?;
                for &at in lowest_of.get(&row.id).into_iter().flatten() {
                    let key = &whole[at].0;
                    let text = match keyed(&keying, &candidates, key) {
                        Ok(found) => found,
                        // A record under the key whose one value cannot be
                        // keyed: that value is the key's.
                        Err(error) => match candidates.as_slice() {
                            [only] => Some(only),
                            _ => return Err(error),
                        },
                    };
                    values.add(key.clone(), row.id, text.ok_or_else(index_disagrees)?)?;
                    shown[at] = true;
                }
                for &at in under.get(&row.id).into_iter().flatten() {
                    let key = &cut[at].0;
                    if keying.text.is_none() {
                        unkeyed[at] = match (&unkeyed[at], candidates.as_slice()) {
                            (None, [only]) => Some(Ok((row.id, only.clone()))),
                            (Some(Ok((id, text))), [only]) if text == only => {
                                Some(Ok((*id.min(&row.id), text.clone())))
                            }
                            _ => Some(Err(())),
                        };
                        continue;
                    }
                    for candidate in &candidates {
                        let Some(whole) = keying.whole_key(candidate)? else {
                            continue;
                        };
                        if whole.starts_with(key) {
                            values.add(whole, row.id, candidate)?;
                            met[at] = true;
                        }
                    }
                }
            }
        }
        if shown.contains(&false) {
            return Err(index_disagrees());
        }
        for (at, (key, _)) in cut.iter().enumerate() {
            if keying.text.is_some() {
                if !met[at] {
                    return Err(index_disagrees());
                }
                continue;
            }
            match &unkeyed[at] {
                Some(Ok((id, text))) => values.add(key.clone(), *id, text)?,
                Some(Err(())) => return Err(keying.unestablished()),
                None => return Err(index_disagrees()),
            }
        }
        Ok(values.finish())
    }

    /// How `field`'s values are keyed. With `strict` (a scan, which keys
    /// every value), an unestablished text form or an unreadable date order
    /// is error 3 at once; otherwise only when a value must be keyed.
    fn keying(
        &mut self,
        field: &om_hbam::Field,
        options: &om_hbam::FieldOptions,
        strict: bool,
    ) -> Result<Keying, DataError> {
        let language = options.index_language();
        // Language 0 is the file's own, as the host writes it
        // (`om_data::rules::written_text_keys`, query-program.md 7.2); a
        // language whose form OpenMaker does not build has none, and so has
        // language 0 when the file's locale cannot be read.
        let text = if field.data_type == DataType::Text {
            // Only language 0 reads the file's locale: the other forms
            // need none, so an unreadable locale does not refuse them.
            let file = if language == 0 {
                match self.file_locale_language() {
                    Ok(file) => Some(file),
                    Err(error) if strict => return Err(error),
                    Err(_) => None,
                }
            } else {
                Some(language)
            };
            file.and_then(|file| om_data::rules::written_text_keys(language, file))
        } else {
            TextKeys::of_language(language)
        };
        if strict && field.data_type == DataType::Text && text.is_none() {
            return Err(unsupported(format!(
                "Field {:?} has no index, and its index language ({language}) has no established native keys.",
                field.name
            )));
        }
        let order = if matches!(field.data_type, DataType::Date | DataType::Timestamp) {
            match self.file_date_order() {
                Ok(order) => Some(order),
                Err(error) if strict => return Err(error),
                Err(_) => None,
            }
        } else {
            None
        };
        // Stored numbers read with the file's separators (`1,5` is 1.5 in
        // de-DE, 15 in en-US: calc-values.md 2.8).
        let numbers = if field.data_type == DataType::Number {
            match self.file_number_format() {
                Ok(format) => Some(format),
                Err(error) if strict => return Err(error),
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Keying {
            data_type: field.data_type,
            field: field.name.clone(),
            language,
            text,
            order,
            numbers,
        })
    }

    /// The file's decimal and thousands separators, read from its locale
    /// `[02, 0d]`, as [`Self::file_date_order`] reads its date order.
    fn file_number_format(&mut self) -> Result<NumberFormat, DataError> {
        let data = download_node(self.session_mut()?, vec![vec![2]])?
            .ok_or_else(|| unsupported("The file's number format could not be read."))?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let locale = om_data::meta::locale(&tree)
            .ok_or_else(|| unsupported("The file's number format could not be read."))?;
        Ok(NumberFormat {
            decimal: locale.decimal_separator,
            thousands: locale.thousands_separator,
        })
    }

    /// The file's own language from its stored locale (`[02, 0d]`,
    /// file-metadata.md 2), which index language 0 keys text in. Error 3
    /// when the file holds a locale that cannot be read, rather than
    /// English, which would key a German file's `a` and `ä` alike. A file
    /// with no locale record at all (none FileMaker writes) reads as the
    /// schema's default.
    fn file_locale_language(&mut self) -> Result<u32, DataError> {
        let unreadable = || unsupported("The file's language could not be read.");
        let Some(data) = download_node(self.session_mut()?, vec![vec![2]])? else {
            return self.file_language()?.ok_or_else(unreadable);
        };
        let tree = download_tree(std::slice::from_ref(&data))?;
        if tree.at(&[2, 13]).is_none() {
            return self.file_language()?.ok_or_else(unreadable);
        }
        let locale = om_data::meta::locale(&tree).ok_or_else(unreadable)?;
        Ok(u32::from(locale.language.id()))
    }

    /// The file's date order, read explicitly from its locale `[02, 0d]`:
    /// a failed or absent locale must not become the calculation schema's
    /// default MDY order for date keys.
    fn file_date_order(&mut self) -> Result<DateOrder, DataError> {
        let data = download_node(self.session_mut()?, vec![vec![2]])?
            .ok_or_else(|| unsupported("The file's date order could not be read."))?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let locale = om_data::meta::locale(&tree)
            .ok_or_else(|| unsupported("The file's date order could not be read."))?;
        let raw = tree
            .at(&[2, 13])
            .and_then(|n| n.assembled())
            .ok_or_else(|| unsupported("The file's date order could not be read."))?;
        if !matches!(raw.get(24), Some(0..=2)) {
            return Err(unsupported(
                "The file's date order has no established native key conversion.",
            ));
        }
        Ok(locale.date_order)
    }

    /// Every record's values, keyed here, when the field has no index: each
    /// distinct whole key once, in key order.
    fn field_values_without_index(
        &mut self,
        table: u64,
        field: &om_hbam::Field,
    ) -> Result<Vec<String>, DataError> {
        let options = field
            .decoded_options()
            .ok_or_else(|| unsupported("The field's index metadata could not be read."))?;
        scannable(field, &options)?;
        let keying = self.keying(field, &options, true)?;
        let session = self.session_mut()?;
        let ids = record_ids(session, table)?;
        let mut values = Values::new(MAX_INDEX_VALUES as usize, VALUE_BYTES);
        let mut batch = Vec::with_capacity(SCAN_BATCH);
        let mut ids = ids.iter();
        loop {
            batch.clear();
            batch.extend(ids.by_ref().take(SCAN_BATCH));
            if batch.is_empty() {
                break;
            }
            let data = download_records(session, table, &batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            for row in om_hbam::record::records(&tree, table) {
                for candidate in candidates(&row, field, options.max_repetitions)? {
                    if let Some(key) = keying.whole_key(&candidate)? {
                        values.add(key, row.id, &candidate)?;
                    }
                }
            }
        }
        Ok(values.finish())
    }

    /// The items of a list sorted by its second field (file-metadata.md 6.2,
    /// `kSortBySecondaryField`; 6.4, observed with `ValueListItems`): one
    /// first-field value per distinct second-field value, the first record
    /// in source order winning, ordered by the second value's key (empty
    /// values first), each first value kept where it first comes for the
    /// sort's order. Each field's value is its spec's repetition. Read from
    /// every record, as a list without an index is
    /// (docs/value-lists-missing-index.md), within the same limits.
    fn field_values_by_second(
        &mut self,
        table: u64,
        (first, first_repetition): (u64, u16),
        (second, second_repetition): (u64, u16),
    ) -> Result<Vec<String>, DataError> {
        let definition = self
            .catalog
            .table(table)
            .ok_or_else(|| unsupported("The value list's table is missing."))?
            .clone();
        let field_of = |id: u64| -> Result<(om_hbam::Field, om_hbam::FieldOptions), DataError> {
            let field = definition
                .field(id)
                .cloned()
                .ok_or_else(|| unsupported("The value list's field is missing."))?;
            let options = field
                .decoded_options()
                .ok_or_else(|| unsupported("The field's index metadata could not be read."))?;
            scannable(&field, &options)?;
            Ok((field, options))
        };
        let (first, first_options) = field_of(first)?;
        let (second, second_options) = field_of(second)?;
        let first_keying = self.keying(&first, &first_options, true)?;
        let second_keying = self.keying(&second, &second_options, true)?;
        let session = self.session_mut()?;
        let ids = record_ids(session, table)?;
        // Second key -> the first value of the first record holding it.
        let mut items: BTreeMap<Vec<u8>, (Vec<u8>, String)> = BTreeMap::new();
        let mut bytes = 0usize;
        let mut batch = Vec::with_capacity(SCAN_BATCH);
        let mut ids = ids.iter();
        loop {
            batch.clear();
            batch.extend(ids.by_ref().take(SCAN_BATCH));
            if batch.is_empty() {
                break;
            }
            let data = download_records(session, table, &batch)?;
            let tree = download_tree(std::slice::from_ref(&data))?;
            // Records come in id order, the source order.
            for row in om_hbam::record::records(&tree, table) {
                // Keyed as stored: trimming would make values a raw index
                // keeps apart (`A`, `A `) one.
                let second_value = repetition_text(&row, second.id, second_repetition)?;
                let second_key = second_keying.rank_key(&second_value)?;
                if items.contains_key(&second_key) {
                    continue;
                }
                let first_value = repetition_text(&row, first.id, first_repetition)?;
                let Some(line) = first_value
                    .split(['\r', '\n'])
                    .find(|l| !l.trim().is_empty())
                else {
                    continue;
                };
                let Some(key) = first_keying.whole_key(line)? else {
                    continue;
                };
                if items.len() == MAX_INDEX_VALUES as usize {
                    return Err(too_many_values());
                }
                bytes += second_key.len() + key.len() + line.len();
                if bytes > VALUE_BYTES {
                    return Err(unsupported(
                        "Listing the field's values exceeded the retained-value byte budget.",
                    ));
                }
                items.insert(second_key, (key, line.to_owned()));
            }
        }
        // One item per distinct second value, a first value repeating when
        // two second values' first records share it (6.4).
        Ok(items.into_values().map(|(_, value)| value).collect())
    }

    /// Value list `id`'s items; refused with 9 when the account's custom set
    /// gives the list no access (session-identity.md 2.2).
    pub(crate) fn read_value_list(&mut self, id: u64) -> Result<Vec<String>, DataError> {
        self.require_open()?;
        if !self.shows_value_list(id) {
            return Err(DataError::with_code(
                9,
                "This account's privilege set gives no access to that value list.",
            ));
        }
        let path = vec![vec![VALUE_LISTS], vec![MEMBERS], key_bytes(id)];
        let data = download_node(self.session_mut()?, path.clone())?
            .ok_or_else(|| unsupported(format!("The file has no value list {id}.")))?;
        let tree = download_tree(std::slice::from_ref(&data))?;
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let member = tree
            .node(&refs)
            .ok_or_else(|| unsupported(format!("The file has no value list {id}.")))?;
        let definition = member
            .child(&[DEFINITION])
            .and_then(|n| n.value)
            .map(<[u8]>::to_vec);
        match definition.as_deref().and_then(|d| d.first()) {
            None | Some(0) => {
                let values = member
                    .child(&[CUSTOM_VALUES])
                    .map(|node| Cell::from_owned(&node.to_owned_node()))
                    .and_then(|cell| cell.text())
                    .transpose()
                    .map_err(|e| failure("decode a value list", e))?
                    .unwrap_or_default();
                Ok(values
                    .split(['\r', '\n'])
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect())
            }
            Some(1) => {
                let definition = definition.unwrap_or_default();
                let unreadable = || DataError::new("A value list's field could not be read.");
                let (occurrence, field) = primary_field(&definition).ok_or_else(unreadable)?;
                let ListOptions {
                    options,
                    repetition,
                    second,
                } = list_options(&definition).ok_or_else(unreadable)?;
                // Related values depend on a record to start from, which a
                // sort by the list has none of.
                if options & SHOW_RELATED_VALUES != 0 {
                    return Err(unsupported(
                        "A value list of related values only cannot be listed here.",
                    ));
                }
                let table = self.occurrence_table(occurrence)?;
                match second {
                    Some((second_occurrence, second, second_repetition))
                        if options & SORT_BY_SECONDARY_FIELD != 0 =>
                    {
                        if self.occurrence_table(second_occurrence)? != table {
                            return Err(unsupported(
                                "A value list whose second field is in another table cannot be listed here.",
                            ));
                        }
                        self.field_values_by_second(
                            table,
                            (field, repetition),
                            (second, second_repetition),
                        )
                    }
                    // A second field shown beside the first does not change
                    // the order of the first field's values.
                    _ => self.field_values(table, field),
                }
            }
            Some(kind) => Err(unsupported(format!(
                "Value lists of type {kind} (another file's or calculated) are not supported."
            ))),
        }
    }

    pub(crate) fn read_index_values(
        &mut self,
        layout_id: u32,
        field: &str,
    ) -> Result<Vec<String>, DataError> {
        let layout = self.decoded_layout(layout_id)?;
        let table = self.table_for_layout(&layout)?;
        let (table, field) = (
            table.id,
            table
                .field_by_name(field)
                .ok_or_else(|| {
                    DataError::with_code(
                        om_data::rules::error::FIELD_MISSING,
                        format!("The table has no field named {field:?}."),
                    )
                })?
                .id,
        );
        self.field_values(table, field)
    }

    /// A value for each of `serials` of a new record of `table`
    /// (`RecordPlan::serials_for`), reserved on the host as FileMaker Pro
    /// reserves it: op72 advancing the field's next value by its
    /// increment, the reply holding the value before (write-path.md 3.2).
    /// The host hands each value out once; a write that fails afterwards
    /// leaves a gap, as a reverted transaction does in FileMaker
    /// (`revert-transaction.html`).
    pub(crate) fn reserve_serials(
        &mut self,
        table: u64,
        serials: impl IntoIterator<Item = om_data::rules::Serial>,
    ) -> Result<BTreeMap<u64, String>, DataError> {
        let mut reserved = BTreeMap::new();
        for serial in serials {
            let (field, increment) = (serial.field, serial.increment);
            let reply = self
                .session_mut()?
                .get_serial(
                    &key_bytes(table),
                    &key_bytes(field),
                    "",
                    increment,
                    write::SERIAL_INCREMENT,
                )
                .map_err(|e| failure("reserve a serial number", e))?;
            if reply.status != 0 {
                return Err(serial_refused(reply));
            }
            self.retain_creation_serial(table, field, &reply.value);
            reserved.insert(field, reply.value);
        }
        Ok(reserved)
    }

    pub(crate) fn write_next_serial(
        &mut self,
        occurrence: u64,
        field: u64,
        value: &str,
    ) -> Result<(), DataError> {
        self.require_open()?;
        let table = self.occurrence_table(occurrence)?;
        let reply = self
            .session_mut()?
            .get_serial(
                &key_bytes(table),
                &key_bytes(field),
                value,
                1,
                write::SERIAL_REPLACE,
            )
            .map_err(|e| failure("set the next serial value", e))?;
        match reply.status {
            0 => Ok(()),
            _ => Err(serial_refused(reply)),
        }
    }
}

/// An op72 refusal: 102 field missing, 303 the field's definition locked by
/// another user (write-path.md 3.2).
fn serial_refused(reply: write::GetSerialReply) -> DataError {
    let status = reply.status;
    DataError::with_code(
        status as i32,
        match reply.conflict {
            Some(conflict) => format!(
                "The field's definition is in use by {} (error {status}).",
                conflict.users
            ),
            None => format!("The host refused the serial value (error {status})."),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_past_its_limit_is_an_error_and_the_lowest_record_shows_a_value() {
        let mut list = Values::new(2, VALUE_BYTES);
        list.add(vec![3], 10, "second").unwrap();
        list.add(vec![2], 10, "first").unwrap();
        list.add(EMPTY_TEXT_KEY.to_vec(), 1, "empty key").unwrap();
        // A lower record shows the value; a later candidate of the same
        // record does not.
        list.add(vec![2], 1, "winner").unwrap();
        list.add(vec![2], 1, "same record later repetition")
            .unwrap();
        list.add(vec![3], 11, "higher record").unwrap();
        // A third value is past the limit: an error, not the first two. The
        // empty value's key `01` is no value, so it never counts.
        assert_eq!(list.add(vec![4], 12, "third").unwrap_err().code, Some(3));
        list.add(EMPTY_TEXT_KEY.to_vec(), 13, "empty key").unwrap();
        assert_eq!(list.finish(), ["winner", "second"]);
    }

    #[test]
    fn retained_bytes_fail_explicitly_instead_of_truncating_the_result() {
        let mut list = Values::new(100_000, 8);
        list.add(vec![2], 2, "ok").unwrap();
        assert_eq!(list.add(vec![3], 3, "too large").unwrap_err().code, Some(3));
        assert_eq!(
            list.add(vec![2], 1, "larger replacement").unwrap_err().code,
            Some(3)
        );
    }
}

#[cfg(test)]
mod native_key_tests {
    use super::*;

    #[test]
    fn fallback_keys_are_the_native_forms() {
        for (kind, value, order, language, key) in [
            (
                DataType::Number,
                "12",
                DateOrder::MonthDayYear,
                21,
                vec![0x80, 2, 0x23],
            ),
            (
                DataType::Date,
                "17/02/2014",
                DateOrder::DayMonthYear,
                21,
                vec![0x9f, 0x78, 0x91, 0x80, 0],
            ),
            (
                DataType::Time,
                "21:26:57",
                DateOrder::MonthDayYear,
                21,
                vec![0x80, 0, 0, 0x80, 5, 0x88, 0x32, 0x80],
            ),
            (
                DataType::Number,
                "-2",
                DateOrder::MonthDayYear,
                21,
                vec![0x7f, 0xfe, 0xcf],
            ),
            // Read as FileMaker reads a number (query-program.md 7.3).
            (
                DataType::Number,
                "1,234",
                DateOrder::MonthDayYear,
                21,
                vec![0x80, 4, 0x23, 0x45],
            ),
            (
                DataType::Timestamp,
                "27/10/2026 21:26:57",
                DateOrder::DayMonthYear,
                21,
                vec![0x9f, 0xaa, 0x9b, 0x80, 5, 0x88, 0x32, 0x80],
            ),
            (
                DataType::Text,
                "Test",
                DateOrder::MonthDayYear,
                21,
                vec![1, 0x14, 0x33, 0x12, 0x6b, 0x14, 0x10, 0x14, 0x33],
            ),
            (
                DataType::Text,
                "Test",
                DateOrder::MonthDayYear,
                1,
                vec![1, 0x14, 0x33, 0x12, 0x6b, 0x14, 0x10, 0x14, 0x33, 0, 0],
            ),
            (
                DataType::Text,
                "Test",
                DateOrder::MonthDayYear,
                2,
                vec![1, 0, 0x54, 0, 0x65, 0, 0x73, 0, 0x74],
            ),
        ] {
            assert_eq!(
                native_key(
                    kind,
                    value,
                    order,
                    NumberFormat::default(),
                    TextKeys::of_language(language).unwrap()
                )
                .unwrap(),
                Some(key)
            );
        }
        // A number field's text with no number in it, and an impossible
        // date.
        for (kind, value) in [(DataType::Number, "N/A"), (DataType::Date, "2/31/2026")] {
            assert_eq!(
                native_key(
                    kind,
                    value,
                    DateOrder::MonthDayYear,
                    NumberFormat::default(),
                    TextKeys::Collated
                )
                .unwrap_err()
                .code,
                Some(3)
            );
        }
        // Long text keys whole, not cut as the index holds them: values
        // that share a cut key stay apart.
        assert_eq!(
            native_key(
                DataType::Text,
                &"a".repeat(300),
                DateOrder::MonthDayYear,
                NumberFormat::default(),
                TextKeys::Collated
            )
            .unwrap(),
            Some(om_collate::text_key(&"a".repeat(300)))
        );
    }
}
