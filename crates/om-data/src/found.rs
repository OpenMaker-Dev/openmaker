//! Found sets: the records a window shows of its layout's table, as finds,
//! omits and sorts leave them (query-program.md 5, 6, 10; script-steps.md
//! 7 tier 2, 11.1).
//!
//! A provider keeps one found set per base table; `records` pages through
//! it. [`DataProvider::change_found_set`](crate::DataProvider) changes it.
//! [`MemoryFoundSet`] is the reference implementation over records held in
//! memory (the mock provider, tests): criteria are FileMaker's find
//! operators (`om_query::criteria`), the ones a host's finders express
//! compared as index keys (query-program.md 6.4, 7.2, 7.3), and sorts
//! compare the same keys (10.3) with value-list orders, language overrides
//! and summary reordering ([`sort_rows`]).
//!
//! What a find does with a field's criterion ([`findable`],
//! script-runtime.md 13.19): stored fields are found through the host's
//! index; an unstored calculation is evaluated on the client for each
//! candidate ([`filter_evaluated`]; the "local find evaluator" of
//! architecture.md 7); summary and global fields are never criteria.

use std::cmp::Ordering;
use std::collections::HashMap;

use om_collate::DateOrder;
use om_hbam::DataType;
use om_query::criteria::{Criterion, FindContext, Ymd};

use crate::{DataError, Field, FieldKind, Record, RecordPage};

pub use om_scripts::runtime::FindKind;

/// FileMaker errors a found-set change reports (`DataError::code`).
pub mod code {
    /// A criterion this client cannot express.
    pub const UNSUPPORTED: i32 = 3;
    /// "Field is missing": a criterion or sort names no field of the table.
    pub const FIELD_MISSING: i32 = 102;
    /// "Find criteria are empty".
    pub const EMPTY_CRITERIA: i32 = 400;
    /// "No records match the request".
    pub const NO_MATCH: i32 = 401;
}

/// One Find mode request: a criterion per field of the layout's table, by
/// field name, and whether it omits the records it matches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindRequest {
    pub omit: bool,
    /// In the order the fields were set; setting a field again replaces its
    /// criterion.
    pub criteria: Vec<(String, String)>,
}

impl FindRequest {
    pub fn set(&mut self, field: &str, text: impl Into<String>) {
        let text = text.into();
        match self.criteria.iter_mut().find(|(f, _)| f == field) {
            Some(criterion) => criterion.1 = text,
            None => self.criteria.push((field.to_owned(), text)),
        }
    }

    /// The criterion typed into `field`.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.criteria
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, t)| t.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.criteria.iter().all(|(_, text)| text.trim().is_empty())
    }
}

/// `DBSortSpec::Entry::SortOrder` (query-program.md 10.2): by value list and
/// custom orders sort ascending here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Ascending,
    Descending,
}

/// What a summary field computes, for "Reorder based on summary field"
/// (file-metadata.md 5.1 kinds 1-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryFunction {
    Total,
    Average,
    Count,
    Minimum,
    Maximum,
}

/// "Reorder based on summary field": the groups of equal sort values are
/// ordered by the summary of `field` over each group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryOrder {
    pub function: SummaryFunction,
    /// The summarized field, by name in the sorted table.
    pub field: String,
}

/// One sort field, by name in the layout's table, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortField {
    pub field: String,
    pub order: SortOrder,
    /// Empty values last in both directions (the sort's `blanksLast`,
    /// script-steps.md 11.1).
    pub blanks_last: bool,
    /// "Custom order based on value list": the list's values in order,
    /// dividers left out; values not in the list sort after them
    /// (script-steps.md 11.1.3).
    pub value_list: Option<Vec<String>>,
    /// "Override field's language for sort": a `Locale::Type`
    /// (script-steps.md 11.1.2) whose collation replaces the field's.
    pub language: Option<u32>,
    pub summary: Option<SummaryOrder>,
}

impl SortField {
    pub fn new(field: impl Into<String>, order: SortOrder) -> Self {
        SortField {
            field: field.into(),
            order,
            blanks_last: false,
            value_list: None,
            language: None,
            summary: None,
        }
    }
}

/// What a found-set step or command does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FoundSetChange {
    /// Show All Records: every record, unsorted.
    ShowAll,
    /// Perform Find, Constrain or Extend Found Set: the requests apply in
    /// order, a find request adding the records it matches and an omit
    /// request removing them. No criteria is error 400; no match leaves an
    /// empty found set and reports 401.
    Find {
        requests: Vec<FindRequest>,
        kind: FindKind,
    },
    /// Perform Find, Constrain or Extend Found Set run from the window, not
    /// by a script (Claris "Making a find request"): as [`Self::Find`], but
    /// requests that match nothing report 401 and leave the found set as
    /// it was, its sort too, for the dialog's Modify Find and Cancel
    /// (client-ui.md, "Dialogs").
    ManualFind {
        requests: Vec<FindRequest>,
        kind: FindKind,
    },
    /// Quick Find ([`crate::quick_find`]): one group of requests per word
    /// of the search text; a record is found when one request of every
    /// group matches it, and the found set is replaced, unsorted. No match
    /// (an empty group included) reports 401 and leaves the found set as
    /// it was, its sort too (query-program.md 6.6).
    QuickFind(Vec<Vec<FindRequest>>),
    /// Omit Record and Omit Multiple Records: these records leave the found
    /// set (a sort is kept).
    Omit(Vec<i64>),
    /// Show Omitted Only: the table's records not in the found set.
    ShowOmitted,
    /// Sort Records by these fields.
    Sort(Vec<SortField>),
    /// Unsort Records: creation order.
    Unsort,
    /// Exactly these records, in this order (Go to List of Records, Go to
    /// Related Record); ids the table lacks are left out.
    Records(Vec<i64>),
}

/// A found set's size after a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FoundSet {
    pub found: u64,
    /// Every record of the table.
    pub total: u64,
    pub sorted: bool,
}

/// One table's found set over records held in memory.
#[derive(Clone, Debug, Default)]
pub struct MemoryFoundSet {
    /// `None`: every record of the table, in creation order.
    ids: Option<Vec<i64>>,
    sort: Vec<SortField>,
}

impl MemoryFoundSet {
    /// Whether the found set is sorted (`Get ( SortState )`).
    pub fn is_sorted(&self) -> bool {
        !self.sort.is_empty()
    }

    /// The found set's records out of `all` (the table's records in
    /// creation order), in display order. Records created since the last
    /// change join an explicit found set at its end, as FileMaker shows a
    /// new record.
    pub fn select<'r>(&self, fields: &[Field], all: &'r [Record]) -> Vec<&'r Record> {
        let mut rows: Vec<&Record> = match &self.ids {
            None => all.iter().collect(),
            Some(ids) => ids
                .iter()
                .filter_map(|id| all.iter().find(|r| r.id == *id))
                .collect(),
        };
        if !self.sort.is_empty() {
            sort_rows(fields, &mut rows, &self.sort);
        }
        rows
    }

    /// A page of the found set.
    pub fn page(&self, fields: Vec<Field>, all: &[Record], offset: u64, limit: u32) -> RecordPage {
        let rows = self.select(&fields, all);
        let total = rows.len() as u64;
        let records = rows
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(limit as usize)
            .cloned()
            .collect();
        RecordPage {
            versions: Default::default(),
            fields,
            records,
            total,
        }
    }

    /// A record created in the window joins an explicit found set.
    pub fn add(&mut self, id: i64) {
        if let Some(ids) = &mut self.ids {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    /// Apply `change` over `all`, the table's records in creation order.
    pub fn apply(
        &mut self,
        fields: &[Field],
        all: &[Record],
        change: FoundSetChange,
    ) -> Result<FoundSet, DataError> {
        let current: Vec<i64> = self.select(fields, all).iter().map(|r| r.id).collect();
        let manual = matches!(change, FoundSetChange::ManualFind { .. });
        let mut no_match = false;
        match change {
            FoundSetChange::ShowAll => {
                self.ids = None;
                self.sort.clear();
            }
            FoundSetChange::Unsort => self.sort.clear(),
            FoundSetChange::Sort(sort) => {
                for field in &sort {
                    let names = std::iter::once(&field.field)
                        .chain(field.summary.as_ref().map(|s| &s.field));
                    for name in names {
                        if !fields.iter().any(|f| f.name == *name) {
                            return Err(field_missing(name));
                        }
                    }
                }
                self.sort = sort;
            }
            FoundSetChange::Omit(records) => {
                self.ids = Some(
                    current
                        .into_iter()
                        .filter(|id| !records.contains(id))
                        .collect(),
                );
            }
            FoundSetChange::ShowOmitted => {
                self.ids = Some(
                    all.iter()
                        .map(|r| r.id)
                        .filter(|id| !current.contains(id))
                        .collect(),
                );
                self.sort.clear();
            }
            FoundSetChange::Records(records) => {
                self.ids = Some(
                    records
                        .into_iter()
                        .filter(|id| all.iter().any(|r| r.id == *id))
                        .collect(),
                );
                self.sort.clear();
            }
            FoundSetChange::Find { requests, kind }
            | FoundSetChange::ManualFind { requests, kind } => {
                let matched = find(fields, all, &requests)?;
                let ids: Vec<i64> = match kind {
                    FindKind::Replace => matched,
                    FindKind::Constrain => current
                        .iter()
                        .copied()
                        .filter(|id| matched.contains(id))
                        .collect(),
                    FindKind::Extend => all
                        .iter()
                        .map(|r| r.id)
                        .filter(|id| current.contains(id) || matched.contains(id))
                        .collect(),
                };
                if manual && ids.is_empty() {
                    return Err(no_records());
                }
                no_match = ids.is_empty();
                self.ids = Some(ids);
                self.sort.clear();
            }
            FoundSetChange::QuickFind(groups) => {
                let mut ids: Vec<i64> = all.iter().map(|r| r.id).collect();
                for group in &groups {
                    let matched = if group.is_empty() {
                        Vec::new()
                    } else {
                        find(fields, all, group)?
                    };
                    ids.retain(|id| matched.contains(id));
                }
                if groups.is_empty() || ids.is_empty() {
                    return Err(no_records());
                }
                self.ids = Some(ids);
                self.sort.clear();
            }
        }
        let found = self.select(fields, all).len() as u64;
        if no_match {
            return Err(no_records());
        }
        Ok(FoundSet {
            found,
            total: all.len() as u64,
            sorted: self.is_sorted(),
        })
    }
}

/// 401, with FileMaker's message.
pub fn no_records() -> DataError {
    DataError::with_code(code::NO_MATCH, "No records match this find criteria.")
}

/// The criterion Find Matching Records (155) puts in its field for the
/// current record's `value`: `=` for an empty value, else `==` and the
/// value, in a text field with every find operator taken literally (`\`
/// before it), so only records holding that whole value are found. This
/// is OpenMaker's reading of Claris's description of the step (finding
/// the records that match the field's value); the request FileMaker builds
/// is not captured. Other kinds take the value as stored, which their
/// operators cannot appear in, and so does a field whose kind the caller
/// does not know (`None`).
pub fn matching_criterion(value: &str, kind: Option<FieldKind>) -> String {
    if value.is_empty() {
        return "=".into();
    }
    let mut out = String::with_capacity(value.len() + 4);
    out.push_str("==");
    for c in value.chars() {
        if kind == Some(FieldKind::Text) && is_find_operator(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A character Find mode reads as an operator (`om_query::criteria`).
pub(crate) fn is_find_operator(c: char) -> bool {
    matches!(
        c,
        '\\' | '"'
            | '='
            | '*'
            | '@'
            | '#'
            | '!'
            | '?'
            | '~'
            | '<'
            | '>'
            | '≤'
            | '≥'
            | '.'
            | '…'
            | '/'
            | '{'
            | '}'
    )
}

fn field_missing(name: &str) -> DataError {
    DataError::with_code(
        code::FIELD_MISSING,
        format!("The table has no field named {name:?}."),
    )
}

/// The index data type of a field kind.
pub fn data_type(kind: FieldKind) -> DataType {
    match kind {
        FieldKind::Text => DataType::Text,
        FieldKind::Number => DataType::Number,
        FieldKind::Date => DataType::Date,
        FieldKind::Time => DataType::Time,
        FieldKind::Timestamp => DataType::Timestamp,
        FieldKind::Container => DataType::Container,
    }
}

/// Today's date on this machine, for `//` and dates without a year: UTC
/// shifted by `utc_offset` seconds.
pub fn today(utc_offset: i64) -> Ymd {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    om_query::criteria::civil_date(now + utc_offset)
}

/// A criterion typed into a field of `kind`, read as find mode reads it
/// (dates month/day/year); `None` when empty.
pub fn criterion(text: &str, kind: FieldKind, today: Ymd) -> Result<Option<Criterion>, DataError> {
    let context = FindContext::new(DateOrder::MonthDayYear, Some(today));
    Criterion::parse(text, data_type(kind), &context).map_err(|_| unreadable(text))
}

/// A criterion the field's type cannot read.
pub fn unreadable(text: &str) -> DataError {
    DataError::with_code(
        code::UNSUPPORTED,
        format!("The find criterion {text:?} is not valid for this field."),
    )
}

/// The value key `!` compares: the first 100 characters of a text value
/// (Claris "Finding duplicate values"), the whole value otherwise.
pub fn duplicate_key(kind: FieldKind, value: &str) -> Vec<u8> {
    let value = value.trim();
    match kind {
        FieldKind::Text => {
            let cut: String = value.chars().take(100).collect();
            om_collate::text_key(&cut)
        }
        _ => value_key(kind, value).unwrap_or_else(|| om_collate::text_key(value)),
    }
}

/// How a find reads a criterion on a field (script-runtime.md 13.19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Findable {
    /// From the host's index: data fields and stored calculations.
    Indexed,
    /// On the client, from each candidate's evaluated value: an unstored
    /// calculation (FileMaker's finder option `Unstored`/`ClientOnly`,
    /// query-program.md 6.2).
    Evaluated,
    /// Not a criterion: a summary field (Set Field into one in Find mode
    /// is 201) or a global field (Find mode sets its value).
    Ignored,
}

/// Field option bits (schema-path.md 5.5): global storage, unstored.
const GLOBAL: u32 = 0x1_0000;
const UNSTORED: u32 = 0x2_0000;

/// How a find reads a criterion on `field` of a hosted file's table.
pub fn findable(field: &om_hbam::Field) -> Findable {
    let options = field.decoded_options().map_or(0, |o| o.options);
    match field.kind {
        om_hbam::FieldKind::Summary => Findable::Ignored,
        _ if options & GLOBAL != 0 => Findable::Ignored,
        om_hbam::FieldKind::Calculation if options & UNSTORED != 0 => Findable::Evaluated,
        _ => Findable::Indexed,
    }
}

/// The `candidates` every criterion accepts on the evaluated value of its
/// field, in order: `value(record, i)` is the text of criterion `i`'s
/// field on `record` (an unstored calculation computed there). A
/// duplicates criterion (`!`) compares with the values of every record of
/// `all`, as [`find`] does with stored values.
pub fn filter_evaluated<E>(
    criteria: &[Criterion],
    candidates: impl IntoIterator<Item = u64>,
    all: &[u64],
    order: DateOrder,
    mut value: impl FnMut(u64, usize) -> Result<String, E>,
) -> Result<Vec<u64>, E> {
    // For `!`: every record's value, read once, and the keys held by
    // several records.
    let mut known: Vec<HashMap<u64, String>> = vec![HashMap::new(); criteria.len()];
    let mut counts: Vec<HashMap<Vec<u8>, usize>> = vec![HashMap::new(); criteria.len()];
    for (i, criterion) in criteria.iter().enumerate() {
        if !criterion.finds_duplicates() {
            continue;
        }
        let kind = field_kind(criterion.data_type());
        for &record in all {
            let text = value(record, i)?;
            if !text.trim().is_empty() {
                *counts[i].entry(duplicate_key(kind, &text)).or_insert(0) += 1;
            }
            known[i].insert(record, text);
        }
    }
    let mut kept = Vec::new();
    'records: for record in candidates {
        for (i, criterion) in criteria.iter().enumerate() {
            let text = match known[i].remove(&record) {
                Some(text) => text,
                None => value(record, i)?,
            };
            let kind = field_kind(criterion.data_type());
            let shared = |v: &str| {
                counts[i]
                    .get(&duplicate_key(kind, v))
                    .is_some_and(|n| *n > 1)
            };
            if !criterion.matches(&text, order, &shared) {
                continue 'records;
            }
        }
        kept.push(record);
    }
    Ok(kept)
}

/// The record field kind a criterion of `data_type` compares as.
fn field_kind(data_type: DataType) -> FieldKind {
    match data_type {
        DataType::Number => FieldKind::Number,
        DataType::Date => FieldKind::Date,
        DataType::Time => FieldKind::Time,
        DataType::Timestamp => FieldKind::Timestamp,
        DataType::Container => FieldKind::Container,
        DataType::Text | DataType::Unknown(_) => FieldKind::Text,
    }
}

/// The records of `all` the requests select, in creation order: a find
/// request adds its matches, an omit request removes them (query-program.md
/// 6.5).
///
/// Uses today's UTC date for relative date criteria. Use [`find_on`] to
/// supply the date of another calendar, such as a client's local one.
pub fn find(
    fields: &[Field],
    all: &[Record],
    requests: &[FindRequest],
) -> Result<Vec<i64>, DataError> {
    find_on(fields, all, requests, today(0))
}

/// As [`find`], with an explicit date for `//` and dates without a year.
pub fn find_on(
    fields: &[Field],
    all: &[Record],
    requests: &[FindRequest],
    today: Ymd,
) -> Result<Vec<i64>, DataError> {
    if requests.iter().all(FindRequest::is_empty) {
        return Err(DataError::with_code(
            code::EMPTY_CRITERIA,
            "Provide at least one criterion to find records.",
        ));
    }
    // The values shared by several records, per field (`!`).
    let mut shared: HashMap<usize, HashMap<Vec<u8>, usize>> = HashMap::new();
    // A first request that omits starts from every record.
    let mut selected = vec![requests[0].omit; all.len()];
    for request in requests {
        let mut tests = Vec::new();
        for (name, text) in &request.criteria {
            let index = fields
                .iter()
                .position(|f| f.name == *name)
                .ok_or_else(|| field_missing(name))?;
            if let Some(criterion) = criterion(text, fields[index].kind, today)? {
                tests.push((index, criterion));
            }
        }
        for (index, _) in &tests {
            let kind = fields[*index].kind;
            shared.entry(*index).or_insert_with(|| {
                let mut counts = HashMap::new();
                for record in all {
                    let value = cell(record, *index);
                    if !value.trim().is_empty() {
                        *counts.entry(duplicate_key(kind, value)).or_insert(0) += 1;
                    }
                }
                counts
            });
        }
        for (i, record) in all.iter().enumerate() {
            let matches = tests.iter().all(|(index, criterion)| {
                let kind = fields[*index].kind;
                let counts = &shared[index];
                let is_shared = |value: &str| {
                    counts
                        .get(&duplicate_key(kind, value))
                        .is_some_and(|n| *n > 1)
                };
                criterion.matches(cell(record, *index), DateOrder::MonthDayYear, &is_shared)
            });
            if matches {
                selected[i] = !request.omit;
            }
        }
    }
    Ok(all
        .iter()
        .zip(selected)
        .filter(|(_, keep)| *keep)
        .map(|(r, _)| r.id)
        .collect())
}

/// The value-index key of `text` in a field of `kind` (query-program.md
/// 7.2, 7.3). Dates are read month/day/year.
pub fn value_key(kind: FieldKind, text: &str) -> Option<Vec<u8>> {
    value_key_with_date_order(kind, text, DateOrder::MonthDayYear)
}

fn value_key_with_date_order(kind: FieldKind, text: &str, order: DateOrder) -> Option<Vec<u8>> {
    match kind {
        FieldKind::Text | FieldKind::Container => Some(om_collate::text_key(text)),
        FieldKind::Number => om_collate::number_key(text),
        FieldKind::Date => om_collate::date_key(text, order),
        FieldKind::Time => om_collate::time_key(text),
        FieldKind::Timestamp => om_collate::timestamp_key(text, order),
    }
}

fn cell(record: &Record, index: usize) -> &str {
    record
        .values
        .get(index)
        .and_then(Option::as_deref)
        .unwrap_or("")
}

/// The collation name of a `Locale::Type` (script-steps.md 11.1.2), as
/// `om_calc`'s collations are named.
fn collation_name(language: u32) -> &'static str {
    match language {
        2 => "Unicode_Raw",
        3 => "Unicode_Standard",
        16 => "Catalan",
        17 => "Croatian",
        18 => "Czech",
        19 => "Danish",
        20 => "Dutch",
        22 => "Finnish",
        23 => "Finnish_Custom",
        24 => "French",
        25 => "German",
        26 => "German_Dictionary",
        27 => "Greek",
        28 => "Hungarian",
        29 => "Icelandic",
        30 => "Italian",
        31 => "Japanese",
        32 => "Norwegian",
        33 => "Polish",
        34 => "Portuguese",
        35 => "Romanian",
        36 => "Russian",
        37 => "Slovak",
        38 => "Slovenian",
        39 => "Spanish",
        40 => "Spanish_Traditional",
        41 => "Swedish",
        42 => "Swedish_Custom",
        43 => "Turkish",
        44 => "Ukrainian",
        45 => "Chinese",
        46 => "Chinese_Stroke",
        47 => "Hebrew",
        48 => "Hindi",
        49 => "Arabic",
        50 => "Estonian",
        51 => "Lithuanian",
        52 => "Latvian",
        53 => "Serbian",
        54 => "Persian",
        55 => "Bulgarian",
        56 => "Vietnamese",
        57 => "Thai",
        58 => "Greek_Mixed",
        59 => "Bengali",
        60 => "Telugu",
        61 => "Marathi",
        62 => "Tamil",
        63 => "Gujarati",
        64 => "Kannada",
        65 => "Malayalam",
        66 => "Oriya",
        67 => "Panjabi",
        68 => "Sinhala",
        69 => "Urdu",
        70 => "Divehi",
        71 => "Burmese",
        72 => "Sanskrit",
        73 => "Lao",
        74 => "Khmer",
        75 => "Tibetan",
        76 => "Korean",
        _ => "English",
    }
}

/// One sort level of one row, ready to compare.
struct LevelKey {
    blank: bool,
    /// The row's group's summary (summary reordering).
    summary: Option<f64>,
    /// Position in the value list; `None` after the list's values.
    rank: Option<usize>,
    value: ValueKey,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ValueKey {
    Empty,
    Number(om_query::criteria::Number),
    Key(Vec<u8>),
    Collated(om_calc::text::sorting::SortKey),
}

fn value_sort_key(
    kind: FieldKind,
    value: &str,
    language: Option<u32>,
    date_order: DateOrder,
) -> ValueKey {
    let value = value.trim();
    if value.is_empty() {
        return ValueKey::Empty;
    }
    if let (FieldKind::Text, Some(language)) = (kind, language) {
        use om_calc::text::sorting::Collation;
        let collation = Collation::named(collation_name(language)).unwrap_or(Collation::English);
        return ValueKey::Collated(collation.sort_key(value));
    }
    if kind == FieldKind::Number {
        if let Some(n) = om_query::criteria::Number::parse(value) {
            return ValueKey::Number(n);
        }
    }
    match value_key_with_date_order(kind, value, date_order) {
        Some(key) => ValueKey::Key(key),
        None => ValueKey::Key(om_collate::text_key(value)),
    }
}

/// The summary of `values` (numbers; Count counts the non-empty ones).
fn summarize(function: SummaryFunction, values: &[&str]) -> Option<f64> {
    let numbers: Vec<f64> = values
        .iter()
        .filter_map(|v| om_query::criteria::Number::parse(v))
        .map(|n| n.to_f64())
        .collect();
    let total: f64 = numbers.iter().sum();
    match function {
        SummaryFunction::Count => {
            Some(values.iter().filter(|v| !v.trim().is_empty()).count() as f64)
        }
        SummaryFunction::Total => Some(total),
        SummaryFunction::Average => (!numbers.is_empty()).then(|| total / numbers.len() as f64),
        SummaryFunction::Minimum => numbers.iter().copied().reduce(f64::min),
        SummaryFunction::Maximum => numbers.iter().copied().reduce(f64::max),
    }
}

/// Sort `rows` by `sort` (query-program.md 10.3, script-steps.md 11.1.3):
/// each field's value key; empty values first ascending and last
/// descending, last in both with `blanks_last`; numbers by value; a
/// value-list order puts the list's values first, in the list's order;
/// a language override compares with that language's collation; a summary
/// reorders the groups of equal values by the summary over each group
/// (within the groups of the fields before it). Ties fall to the next
/// field, then keep the rows' order.
pub fn sort_rows(fields: &[Field], rows: &mut [&Record], sort: &[SortField]) {
    sort_rows_with_date_order(fields, rows, sort, DateOrder::MonthDayYear);
}

/// The same local sorter, with dates and timestamps read in their owning
/// file's stored date order. Other key, option and stable-tie rules are shared
/// with [`sort_rows`].
pub fn sort_rows_with_date_order(
    fields: &[Field],
    rows: &mut [&Record],
    sort: &[SortField],
    date_order: DateOrder,
) {
    sort_rows_with_rank_text(fields, rows, sort, date_order, |_, _| None);
}

/// Sort normalized values, optionally using their original text for custom
/// value-list ranks. `rank_text` receives the record ID and field index.
pub fn sort_rows_with_rank_text<'a>(
    fields: &[Field],
    rows: &mut [&Record],
    sort: &[SortField],
    date_order: DateOrder,
    rank_text: impl Fn(i64, usize) -> Option<&'a str>,
) {
    if rows.len() < 2 {
        return;
    }
    let columns: Vec<(usize, FieldKind, &SortField)> = sort
        .iter()
        .filter_map(|s| {
            let i = fields.iter().position(|f| f.name == s.field)?;
            Some((i, fields[i].kind, s))
        })
        .collect();
    // Value lists are shared by every row of a sort column. Normalize
    // them once, preserving the first rank for collating-equal entries.
    let ranks: Vec<Option<HashMap<Vec<u8>, usize>>> = columns
        .iter()
        .map(|(_, _, field)| {
            field.value_list.as_ref().map(|list| {
                let mut ranks = HashMap::with_capacity(list.len());
                for (rank, item) in list.iter().enumerate() {
                    ranks
                        .entry(om_collate::text_key(item.trim()))
                        .or_insert(rank);
                }
                ranks
            })
        })
        .collect();
    // Each row's keys, level by level.
    let mut keys: Vec<Vec<LevelKey>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(level, &(index, kind, field))| {
                    let value = cell(row, index);
                    let rank = ranks[level].as_ref().and_then(|ranks| {
                        let text = rank_text(row.id, index).unwrap_or(value);
                        let key = om_collate::text_key(text.trim());
                        ranks.get(&key).copied()
                    });
                    LevelKey {
                        blank: value.trim().is_empty(),
                        summary: None,
                        rank,
                        value: value_sort_key(kind, value, field.language, date_order),
                    }
                })
                .collect()
        })
        .collect();
    // Summaries per group: the values of every level up to this one.
    for (level, &(_, _, field)) in columns.iter().enumerate() {
        let Some(summary) = &field.summary else {
            continue;
        };
        let Some(summarized) = fields.iter().position(|f| f.name == summary.field) else {
            continue;
        };
        let mut groups: Vec<(Vec<&ValueKey>, Vec<usize>)> = Vec::new();
        for (row, row_keys) in keys.iter().enumerate() {
            let group: Vec<&ValueKey> = row_keys[..=level].iter().map(|k| &k.value).collect();
            match groups.iter_mut().find(|(g, _)| *g == group) {
                Some((_, members)) => members.push(row),
                None => groups.push((group, vec![row])),
            }
        }
        let totals: Vec<(Vec<usize>, Option<f64>)> = groups
            .into_iter()
            .map(|(_, members)| {
                let values: Vec<&str> =
                    members.iter().map(|&r| cell(rows[r], summarized)).collect();
                let total = summarize(summary.function, &values);
                (members, total)
            })
            .collect();
        for (members, total) in totals {
            for row in members {
                keys[row][level].summary = total;
            }
        }
    }
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        for (level, &(_, _, field)) in columns.iter().enumerate() {
            let (x, y) = (&keys[a][level], &keys[b][level]);
            if field.blanks_last {
                match (x.blank, y.blank) {
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => {}
                }
            }
            let mut ordering = Ordering::Equal;
            if field.summary.is_some() {
                ordering = match (&x.summary, &y.summary) {
                    (Some(p), Some(q)) => p.total_cmp(q),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                };
            }
            if ordering == Ordering::Equal && field.value_list.is_some() {
                ordering = match (x.rank, y.rank) {
                    (Some(p), Some(q)) => p.cmp(&q),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                };
            }
            if ordering == Ordering::Equal {
                ordering = x.value.cmp(&y.value);
            }
            let ordering = match field.order {
                SortOrder::Ascending => ordering,
                SortOrder::Descending => ordering.reverse(),
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
    let sorted: Vec<&Record> = order.iter().map(|&i| rows[i]).collect();
    rows.copy_from_slice(&sorted);
}

/// FileMaker's sort order of two stored values of a field of `kind`.
pub fn compare_values(kind: FieldKind, a: &str, b: &str) -> Ordering {
    let order = DateOrder::MonthDayYear;
    value_sort_key(kind, a, None, order).cmp(&value_sort_key(kind, b, None, order))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<Field> {
        vec![
            Field {
                name: "Name".into(),
                kind: FieldKind::Text,
            },
            Field {
                name: "Age".into(),
                kind: FieldKind::Number,
            },
        ]
    }

    fn records() -> Vec<Record> {
        [
            ("Ada Lovelace", "36"),
            ("Grace Hopper", "85"),
            ("Alan Turing", "41"),
            ("", "41"),
        ]
        .iter()
        .enumerate()
        .map(|(i, (name, age))| Record {
            id: i as i64 + 1,
            values: vec![
                (!name.is_empty()).then(|| name.to_string()),
                Some(age.to_string()),
            ],
        })
        .collect()
    }

    fn request(field: &str, text: &str) -> FindRequest {
        let mut r = FindRequest::default();
        r.set(field, text);
        r
    }

    #[test]
    fn criteria_follow_the_find_mode_operators() {
        let (f, all) = (fields(), records());
        let ids = |reqs: &[FindRequest]| find(&f, &all, reqs).unwrap();
        assert_eq!(ids(&[request("Name", "a")]), [1, 3]);
        assert_eq!(ids(&[request("Name", "hop")]), [2]);
        assert_eq!(ids(&[request("Name", "=Ada")]), [1]);
        assert_eq!(ids(&[request("Name", "=")]), [4]);
        assert_eq!(ids(&[request("Age", ">40")]), [2, 3, 4]);
        assert_eq!(ids(&[request("Age", "36...41")]), [1, 3, 4]);
        assert_eq!(ids(&[request("Age", "!")]), [3, 4]);
        assert_eq!(ids(&[request("Age", "85")]), [2]);
        // Wildcards, phrases and non-empty.
        assert_eq!(ids(&[request("Name", "A*")]), [1, 3]);
        assert_eq!(ids(&[request("Name", "Gr@ce")]), [2]);
        assert_eq!(ids(&[request("Name", "*\"ring\"")]), [3]);
        assert_eq!(ids(&[request("Name", "*")]), [1, 2, 3]);
        assert_eq!(ids(&[request("Age", "4#")]), [3, 4]);
        // A find request then an omit request.
        let mut omit = request("Name", "turing");
        omit.omit = true;
        assert_eq!(ids(&[request("Name", "a"), omit.clone()]), [1]);
        assert_eq!(ids(&[omit]), [1, 2, 4]);
        let empty = find(&f, &all, &[FindRequest::default()]).unwrap_err();
        assert_eq!(empty.code, Some(code::EMPTY_CRITERIA));
        let invalid = find(&f, &all, &[request("Age", "many")]).unwrap_err();
        assert_eq!(invalid.code, Some(code::UNSUPPORTED));
        // Find Matching Records: the whole value, its operators literal.
        let names = ["a*b...c", "axxb...c", "a*b", "a*b...c d", ""];
        let named: Vec<Record> = (1..)
            .zip(names)
            .map(|(id, name)| Record {
                id,
                values: vec![(!name.is_empty()).then(|| name.into()), None],
            })
            .collect();
        let matching = |value: &str| {
            let criterion = matching_criterion(value, Some(FieldKind::Text));
            find(&f, &named, &[request("Name", &criterion)]).unwrap()
        };
        assert_eq!(matching("a*b...c"), [1]);
        assert_eq!(matching(""), [5]);
        assert_eq!(matching_criterion("1.5", Some(FieldKind::Number)), "==1.5");
    }

    #[test]
    fn relative_date_criteria_use_the_supplied_calendar_date() {
        let fields = [Field {
            name: "Due".into(),
            kind: FieldKind::Date,
        }];
        let records = [
            Record {
                id: 1,
                values: vec![Some("12/31/2026".into())],
            },
            Record {
                id: 2,
                values: vec![Some("1/1/2027".into())],
            },
        ];
        // UTC and the client's local calendar can differ in day and year.
        let today = [request("Due", "//")];
        assert_eq!(
            find_on(&fields, &records, &today, (2026, 12, 31)).unwrap(),
            [1]
        );
        assert_eq!(
            find_on(&fields, &records, &today, (2027, 1, 1)).unwrap(),
            [2]
        );
        let yearless = [request("Due", "12/31")];
        assert_eq!(
            find_on(&fields, &records, &yearless, (2026, 12, 31)).unwrap(),
            [1]
        );
        assert!(find_on(&fields, &records, &yearless, (2027, 1, 1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn found_sets_find_constrain_extend_omit_and_sort() {
        let (f, all) = (fields(), records());
        let mut set = MemoryFoundSet::default();
        let found = set
            .apply(
                &f,
                &all,
                FoundSetChange::Find {
                    requests: vec![request("Name", "a")],
                    kind: FindKind::Replace,
                },
            )
            .unwrap();
        assert_eq!((found.found, found.total), (2, 4));
        set.apply(
            &f,
            &all,
            FoundSetChange::Find {
                requests: vec![request("Name", "grace")],
                kind: FindKind::Extend,
            },
        )
        .unwrap();
        let ids = |set: &MemoryFoundSet| -> Vec<i64> {
            set.select(&f, &all).iter().map(|r| r.id).collect()
        };
        assert_eq!(ids(&set), [1, 2, 3]);
        let sorted = set
            .apply(
                &f,
                &all,
                FoundSetChange::Sort(vec![SortField::new("Age", SortOrder::Descending)]),
            )
            .unwrap();
        assert!(sorted.sorted);
        assert_eq!(ids(&set), [2, 3, 1]);
        set.apply(&f, &all, FoundSetChange::Omit(vec![3])).unwrap();
        assert_eq!(ids(&set), [2, 1], "omitting keeps the sort");
        set.apply(&f, &all, FoundSetChange::ShowOmitted).unwrap();
        assert_eq!(ids(&set), [3, 4]);
        let none = set.apply(
            &f,
            &all,
            FoundSetChange::Find {
                requests: vec![request("Name", "zzz")],
                kind: FindKind::Constrain,
            },
        );
        assert_eq!(none.unwrap_err().code, Some(code::NO_MATCH));
        assert!(ids(&set).is_empty(), "no match leaves an empty found set");
        set.apply(&f, &all, FoundSetChange::ShowAll).unwrap();
        assert_eq!(ids(&set), [1, 2, 3, 4]);
        let page = set.page(f.clone(), &all, 1, 2);
        assert_eq!(page.total, 4);
        assert_eq!(page.records.len(), 2);
    }

    #[test]
    fn blanks_go_last_when_asked() {
        let (f, all) = (fields(), records());
        let mut rows: Vec<&Record> = all.iter().collect();
        let mut by_name = SortField::new("Name", SortOrder::Ascending);
        sort_rows(&f, &mut rows, std::slice::from_ref(&by_name));
        assert_eq!(rows[0].id, 4, "empty first ascending");
        by_name.order = SortOrder::Descending;
        sort_rows(&f, &mut rows, std::slice::from_ref(&by_name));
        assert_eq!(rows[3].id, 4, "empty last descending");
        by_name.order = SortOrder::Ascending;
        by_name.blanks_last = true;
        sort_rows(&f, &mut rows, &[by_name]);
        assert_eq!(rows[3].id, 4, "blanks last ascending");
    }

    #[test]
    fn sorts_put_empty_values_first_and_compare_numbers_by_value() {
        assert_eq!(
            compare_values(FieldKind::Number, "-2", "10"),
            Ordering::Less
        );
        assert_eq!(
            compare_values(FieldKind::Number, "2.5", "10"),
            Ordering::Less
        );
        assert_eq!(compare_values(FieldKind::Text, "", "a"), Ordering::Less);
        assert_eq!(
            compare_values(FieldKind::Text, "apple", "Banana"),
            Ordering::Less
        );
        assert_eq!(
            compare_values(FieldKind::Date, "2/1/2026", "10/1/2025"),
            Ordering::Greater
        );
    }

    /// The Tasks probe of script-steps.md 11.1.3: (Task, Status, Priority,
    /// Category, N).
    fn tasks() -> (Vec<Field>, Vec<Record>) {
        let text = |name: &str| Field {
            name: name.into(),
            kind: FieldKind::Text,
        };
        let fields = vec![
            text("Task"),
            text("Status"),
            text("Priority"),
            text("Category"),
            Field {
                name: "N".into(),
                kind: FieldKind::Number,
            },
        ];
        let rows = [
            ("alpha", "Pending", "High", "Home", "5"),
            ("beta", "In Progress", "Low", "Business", "1"),
            ("gamma", "Completed", "Medium", "", "4"),
            ("Alpha beta", "In Progress", "High", "Other", "2"),
            ("", "Canceled", "Low", "Home", ""),
            ("delta", "Pending", "Medium", "Business", "3"),
            ("Äpfel", "Postponed", "Low", "Other", "7"),
            ("zeta", "Completed", "High", "Business", "1"),
        ];
        let records = rows
            .iter()
            .enumerate()
            .map(|(i, row)| Record {
                id: i as i64 + 1,
                values: [row.0, row.1, row.2, row.3, row.4]
                    .iter()
                    .map(|v| (!v.is_empty()).then(|| v.to_string()))
                    .collect(),
            })
            .collect();
        (fields, records)
    }

    fn sorted_tasks(sort: &[SortField]) -> Vec<String> {
        let (fields, records) = tasks();
        let mut rows: Vec<&Record> = records.iter().collect();
        sort_rows(&fields, &mut rows, sort);
        rows.iter()
            .map(|r| r.values[0].clone().unwrap_or_else(|| "(empty)".into()))
            .collect()
    }

    fn list(values: &str) -> Option<Vec<String>> {
        Some(
            values
                .split(',')
                .map(|v| v.trim().to_owned())
                .filter(|v| v != "-")
                .collect(),
        )
    }

    #[test]
    fn value_list_ranks_keep_first_collation_match_and_refresh_each_sort() {
        let fields = [Field {
            name: "Name".into(),
            kind: FieldKind::Text,
        }];
        let records: Vec<_> = ["E", "é", "a", "", "z", " e "]
            .iter()
            .enumerate()
            .map(|(i, name)| Record {
                id: i as i64 + 1,
                values: vec![Some((*name).into())],
            })
            .collect();
        let mut sort = SortField {
            value_list: Some(["é", "a", "E", ""].map(String::from).to_vec()),
            ..SortField::new("Name", SortOrder::Ascending)
        };
        let ordered = |sort: &SortField| {
            let mut rows: Vec<_> = records.iter().collect();
            sort_rows(&fields, &mut rows, std::slice::from_ref(sort));
            rows.iter().map(|r| r.id).collect::<Vec<_>>()
        };
        assert_eq!(ordered(&sort), [1, 2, 6, 3, 4, 5]);
        sort.order = SortOrder::Descending;
        assert_eq!(ordered(&sort), [5, 4, 3, 1, 2, 6]);
        sort.blanks_last = true;
        assert_eq!(ordered(&sort), [5, 3, 1, 2, 6, 4]);
        sort.order = SortOrder::Ascending;
        assert_eq!(ordered(&sort), [1, 2, 6, 3, 5, 4]);
        sort.blanks_last = false;
        sort.value_list = Some(["a", "e"].map(String::from).to_vec());
        assert_eq!(ordered(&sort), [3, 1, 2, 6, 4, 5]);
        sort.value_list = Some(Vec::new());
        assert_eq!(ordered(&sort), [4, 3, 1, 2, 6, 5]);
        sort.value_list = None;
        assert_eq!(ordered(&sort), [4, 3, 1, 2, 6, 5]);
    }

    #[test]
    fn value_list_and_language_orders_match_the_filemaker_probes() {
        let task = |order| SortField::new("Task", order);
        let by_list = |field: &str, values: &str| SortField {
            value_list: list(values),
            ..SortField::new(field, SortOrder::Ascending)
        };
        // S04: Status by value list, then Task ascending.
        assert_eq!(
            sorted_tasks(&[
                by_list(
                    "Status",
                    "In Progress, Pending, Postponed, Canceled, -, Completed, -"
                ),
                task(SortOrder::Ascending),
            ]),
            [
                "Alpha beta",
                "beta",
                "alpha",
                "delta",
                "Äpfel",
                "(empty)",
                "gamma",
                "zeta"
            ]
        );
        // S05: Priority by value list, then Task descending.
        assert_eq!(
            sorted_tasks(&[
                by_list("Priority", "High, Medium, Low"),
                task(SortOrder::Descending),
            ]),
            [
                "zeta",
                "Alpha beta",
                "alpha",
                "gamma",
                "delta",
                "beta",
                "Äpfel",
                "(empty)"
            ]
        );
        // S11: Category by value list (values not in it last), then Task.
        assert_eq!(
            sorted_tasks(&[
                by_list("Category", "Business, Home, Other, -"),
                task(SortOrder::Ascending),
            ]),
            [
                "beta",
                "delta",
                "zeta",
                "(empty)",
                "alpha",
                "Alpha beta",
                "Äpfel",
                "gamma"
            ]
        );
        // S06, S07: Swedish puts Ä after Z; German sorts it as AE.
        let language = |id| SortField {
            language: Some(id),
            ..task(SortOrder::Ascending)
        };
        assert_eq!(
            sorted_tasks(&[language(41)]),
            [
                "(empty)",
                "alpha",
                "Alpha beta",
                "beta",
                "delta",
                "gamma",
                "zeta",
                "Äpfel"
            ]
        );
        assert_eq!(
            sorted_tasks(&[language(25)]),
            [
                "(empty)",
                "Äpfel",
                "alpha",
                "Alpha beta",
                "beta",
                "delta",
                "gamma",
                "zeta"
            ]
        );
        // S08 and S09 (blanks last at every level).
        let category = |order| SortField::new("Category", order);
        assert_eq!(
            sorted_tasks(&[category(SortOrder::Ascending), task(SortOrder::Descending)]),
            [
                "gamma",
                "zeta",
                "delta",
                "beta",
                "alpha",
                "(empty)",
                "Äpfel",
                "Alpha beta"
            ]
        );
        let blanks_last = |mut field: SortField| {
            field.blanks_last = true;
            field
        };
        assert_eq!(
            sorted_tasks(&[
                blanks_last(category(SortOrder::Descending)),
                blanks_last(task(SortOrder::Ascending)),
            ]),
            [
                "Alpha beta",
                "Äpfel",
                "alpha",
                "(empty)",
                "beta",
                "delta",
                "zeta",
                "gamma"
            ]
        );
    }

    #[test]
    fn summaries_reorder_the_groups() {
        // Categories by the total of N per group: Other 9, Business 5,
        // Home 5, (empty) 4. Ascending: (empty), then Business and Home
        // tied (by value), then Other.
        let summary = |function| SortField {
            summary: Some(SummaryOrder {
                function,
                field: "N".into(),
            }),
            ..SortField::new("Category", SortOrder::Ascending)
        };
        let task = SortField::new("Task", SortOrder::Ascending);
        assert_eq!(
            sorted_tasks(&[summary(SummaryFunction::Total), task.clone()]),
            [
                "gamma",
                "beta",
                "delta",
                "zeta",
                "(empty)",
                "alpha",
                "Alpha beta",
                "Äpfel"
            ]
        );
        let descending = SortField {
            order: SortOrder::Descending,
            ..summary(SummaryFunction::Count)
        };
        // Counts: Business 3, Home 1 (one N is empty), Other 2, (empty) 1.
        assert_eq!(
            sorted_tasks(&[descending, task]),
            [
                "beta",
                "delta",
                "zeta",
                "Alpha beta",
                "Äpfel",
                "(empty)",
                "alpha",
                "gamma"
            ]
        );
    }

    /// U = N * 2 of script-runtime.md 13.19 (records 1-6: 10, 2, 8, 4,
    /// empty, 6), read through the callback the way a host computes it.
    #[test]
    fn evaluated_criteria_use_the_find_operators_and_see_every_record() {
        let u = |record: u64| ["10", "2", "8", "4", "", "6"][record as usize - 1].to_string();
        let all: Vec<u64> = (1..=6).collect();
        let today = (2026, 10, 29);
        let find = |text: &str, candidates: &[u64]| -> Vec<u64> {
            let criterion = criterion(text, FieldKind::Number, today).unwrap().unwrap();
            let mut reads = 0;
            let kept = filter_evaluated(
                &[criterion],
                candidates.iter().copied(),
                &all,
                DateOrder::MonthDayYear,
                |record, _| -> Result<String, ()> {
                    reads += 1;
                    Ok(u(record))
                },
            )
            .unwrap();
            // Each value is read once, also for `!`.
            assert!(reads <= all.len().max(candidates.len()), "{text}");
            kept
        };
        assert_eq!(find("8", &all), [3]);
        assert_eq!(find(">5", &all), [1, 3, 6]);
        assert_eq!(find("#", &all), [2, 3, 4, 6]);
        assert_eq!(find("=", &all), [5]);
        assert_eq!(find("4...8", &all), [3, 4, 6]);
        assert_eq!(find("!", &all), Vec::<u64>::new());
        // Only candidates are kept; duplicates count every record.
        let doubled = |record: u64| ["2", "2", "8"][record as usize - 1].to_string();
        let criterion = criterion("!", FieldKind::Number, today).unwrap().unwrap();
        let kept = filter_evaluated(
            &[criterion],
            [2, 3],
            &[1, 2, 3],
            DateOrder::MonthDayYear,
            |record, _| -> Result<String, ()> { Ok(doubled(record)) },
        )
        .unwrap();
        assert_eq!(kept, [2]);
    }

    #[test]
    fn summary_and_global_fields_are_not_criteria() {
        let field = |kind, options: u32| {
            let mut bytes = vec![0u8; 26];
            bytes[0] = match kind {
                om_hbam::FieldKind::Calculation => 2,
                om_hbam::FieldKind::Summary => 3,
                _ => 1,
            };
            bytes[8..12].copy_from_slice(&options.to_be_bytes());
            om_hbam::Field {
                id: 1,
                name: "F".into(),
                kind,
                data_type: DataType::Number,
                options: bytes,
            }
        };
        use om_hbam::FieldKind as K;
        assert_eq!(findable(&field(K::Normal, 0)), Findable::Indexed);
        assert_eq!(findable(&field(K::Calculation, 0)), Findable::Indexed);
        assert_eq!(
            findable(&field(K::Calculation, UNSTORED)),
            Findable::Evaluated
        );
        assert_eq!(findable(&field(K::Summary, 0)), Findable::Ignored);
        assert_eq!(findable(&field(K::Normal, GLOBAL)), Findable::Ignored);
        assert_eq!(findable(&field(K::Calculation, GLOBAL)), Findable::Ignored);
    }
}
