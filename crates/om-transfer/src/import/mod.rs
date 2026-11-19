//! Import Records (35) without a host (oracle x02 on FileMaker Server
//! 26.0.3).
//!
//! Rows. The source's rows up to and including its field names row (the
//! profile extension's 0-based row; -1 for none) are not imported; a merge
//! file always starts with one. Columns map to target fields by position
//! through the stored import order, or by name when "match field names"
//! is on (columns without a field of that name are skipped). Columns past
//! the order, columns marked "don't import", calculation and summary fields
//! are skipped. A repeating field takes the column's repetitions up to its
//! own count; another field takes the first.
//!
//! Actions. Add makes a record per row. Update matching compares the match
//! fields with the records of the found set only and updates every record
//! that matches, comparing values as the field's index keys them (oracle
//! x04: text without regard to case, numbers, dates and times by value);
//! with "add remaining data as new records" an unmatched row is added, and
//! a matched record takes the row's match values too. Updating the found set writes row n into its record n; rows
//! past the found set are added with "add remaining", else dropped. An
//! update writes every mapped field, empty values too, and keeps the
//! others. Afterwards the found set holds the records added and updated
//! (all of the found set for an update of the found set).
//!
//! "Preserve target field data if source field is empty" (FileMaker
//! 26.1.1, flag `0x40`): an update leaves a field whose source value is
//! empty as it is and still writes the row's other values (Claris). Which
//! values count is inferred, unmeasured on FileMaker: an empty text or a
//! missing column, repetition by repetition; a record the import adds is
//! written in full, as without the option. With "perform auto-enter
//! options" too, an update that maps a field with an auto-enter option
//! fails with error 3 before any row: the field's import write is what
//! keeps auto-entry from replacing it, so leaving it unwritten could let a
//! calculation or lookup overwrite the very value the option keeps (how
//! FileMaker orders the two is unmeasured). An import with a flag
//! OpenMaker does not know fails with error 3 before any row.
//!
//! Entry options. With "perform auto-enter options" fields that are not
//! imported get their auto-entered values; an imported field keeps the
//! imported value (empty too). Validation set to "always" is checked and a
//! row that fails is not imported; the step then ends with error 729 after
//! importing the others. "Only during data entry" validation is not
//! checked. Values are stored as the source spells them, except dates with
//! a year of less than four digits, which are stored completed
//! (`1/1/24` is `01/01/2024`).
//!
//! A host that interrupts the import ([`ImportTarget::interrupted`], its
//! script was aborted) stops it before the next row with error 1.

pub mod limit;
pub mod read;

use std::collections::{HashMap, HashSet};

use om_calc::Locale;

use crate::spec::{
    import_flag, FieldFlags, FileType, ImportAction, ImportOptions, Mapping, Profile, ProfileBody,
    ProfileEx,
};
use crate::{code, FieldKind, FieldType, RecordId, Result, TransferError};
pub use read::{Source, SourceRow};

/// A field of the target table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetField {
    pub id: u64,
    pub name: String,
    pub ty: FieldType,
    pub kind: FieldKind,
    pub repetitions: u32,
    /// The field has an auto-enter option of its own (a creation or
    /// modification value, a serial number, data, a calculation, the last
    /// visited record's value or a lookup).
    pub auto_enters: bool,
}

/// One cell an imported row writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldWrite {
    pub field: u64,
    /// 1-based.
    pub repetition: u32,
    pub text: String,
}

/// The host's records and writes.
pub trait ImportTarget {
    /// The fields of the target occurrence's table; error 102 when the
    /// occurrence is missing.
    fn fields(&mut self, occurrence: u64) -> Result<Vec<TargetField>>;

    /// The found set the update actions work on.
    fn found_set(&mut self, occurrence: u64) -> Result<Vec<RecordId>>;

    /// A field's first value in a record, for matching.
    fn value(&mut self, occurrence: u64, record: RecordId, field: u64) -> Result<String>;

    /// Make a record with `writes`, applying auto-entry to the other fields
    /// when `auto_enter` and the "always" validation of the record. `Err`
    /// refuses the row (its code is reported).
    fn create(
        &mut self,
        occurrence: u64,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> Result<RecordId>;

    /// Write `writes` into a record (modification auto-entry with
    /// `auto_enter`), validated as `create`.
    fn update(
        &mut self,
        occurrence: u64,
        record: RecordId,
        writes: &[FieldWrite],
        auto_enter: bool,
    ) -> Result<()>;

    /// Show these records as the found set after the import.
    fn set_found_set(&mut self, occurrence: u64, records: &[RecordId]) -> Result<()>;

    /// Whether to stop: the host's script was aborted or ran out of time.
    /// Checked before every row and record; the import then ends with
    /// error 1, and the host writes none of its rows.
    fn interrupted(&mut self) -> bool {
        false
    }
}

/// The stored specs of an Import Records step (`84`, `86`, `87`, `f0`), or
/// the same chosen in the import dialogs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRequest {
    pub options: ImportOptions,
    pub profile: Profile,
    pub profile_ex: Option<ProfileEx>,
    pub field_flags: Option<FieldFlags>,
}

/// A row the import could not write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowError {
    /// 1-based source row (counting rows before the data).
    pub row: usize,
    pub code: i32,
    pub message: String,
}

/// What an import did (FileMaker Pro's Import Summary).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Data rows read.
    pub total: usize,
    pub added: usize,
    pub updated: usize,
    /// Rows refused (validation and other write errors).
    pub errors: Vec<RowError>,
    /// The found set afterwards.
    pub found_set: Vec<RecordId>,
}

impl ImportSummary {
    /// The step's error: 729 when some row could not be imported.
    pub fn error_code(&self) -> i32 {
        if self.errors.is_empty() {
            0
        } else {
            code::IMPORT_ERRORS
        }
    }

    /// The import log FileMaker Pro keeps (`Import.log`), one line per
    /// refused row.
    pub fn log(&self, source: &str, target: &str) -> String {
        let mut out = format!(
            "Import of \"{source}\" into \"{target}\": {} rows, {} added, {} updated, {} skipped\r",
            self.total,
            self.added,
            self.updated,
            self.errors.len()
        );
        for e in &self.errors {
            out.push_str(&format!("Row {}: error {}: {}\r", e.row, e.code, e.message));
        }
        out
    }
}

/// Parse a source file in the profile's format, within [`limit`].
pub fn parse(request: &ImportRequest, bytes: &[u8]) -> Result<Source> {
    let unreadable = match request.profile.file_type {
        FileType::XmlImport => code::XML_PARSE,
        FileType::Excel2007 | FileType::ExcelExport => 730,
        _ => 708,
    };
    if bytes.len() > limit::SOURCE_BYTES {
        return Err(TransferError::new(
            unreadable,
            "The source is larger than OpenMaker imports.",
        ));
    }
    let charset = request.options.charset;
    let delimiter = |default: char| {
        request
            .profile_ex
            .as_ref()
            .and_then(|ex| {
                let mut c = ex.delimiter.chars();
                match (c.next(), c.next()) {
                    (Some(d), None) => Some(d),
                    _ => None,
                }
            })
            .unwrap_or(default)
    };
    Ok(match request.profile.file_type {
        FileType::TabSeparated => Source {
            names: None,
            rows: read::delimited(&charset.decode(bytes), delimiter('\t'))?,
        },
        FileType::CommaSeparated => Source {
            names: None,
            rows: read::comma(&charset.decode(bytes), delimiter(','))?,
        },
        FileType::Merge => read::merge(&charset.decode(bytes))?,
        FileType::XmlImport => read::fmpxmlresult(bytes, charset)?,
        FileType::Excel2007 | FileType::ExcelExport => {
            let (sheet, worksheet) = match &request.profile.body {
                ProfileBody::Excel {
                    sheet, worksheet, ..
                } => (*sheet as usize, worksheet.as_str()),
                _ => (0, ""),
            };
            Source {
                names: None,
                rows: read::workbook(bytes, sheet, worksheet)?,
            }
        }
        FileType::DBase => read::dbase(bytes, charset)?,
        other => {
            return Err(TransferError::new(
                711,
                format!("Import from {other:?} is not supported."),
            ))
        }
    })
}

/// The 0-based names row of a text source; `None` for none.
fn names_row(request: &ImportRequest) -> Option<usize> {
    match &request.profile_ex {
        Some(ex) => usize::try_from(ex.field_names_row).ok(),
        None => (request.profile.field_names != 0).then_some(0),
    }
}

/// Where a source column goes.
#[derive(Clone, Debug)]
struct Target {
    field: TargetField,
    matching: bool,
}

/// Import `bytes` into the target occurrence (module docs).
pub fn import(
    target: &mut dyn ImportTarget,
    request: &ImportRequest,
    bytes: &[u8],
    locale: &Locale,
) -> Result<ImportSummary> {
    let mut source = parse(request, bytes)?;
    if source.rows.is_empty() && source.names.is_none() {
        return Err(TransferError::new(
            code::IMPORT_ORDER_INVALID,
            "The source has no data.",
        ));
    }
    let mut first_row = 1;
    if source.names.is_none() {
        if let Some(n) = names_row(request) {
            let names = source.rows.get(n).map(|r| {
                r.iter()
                    .map(|v| v.first().cloned().unwrap_or_default())
                    .collect()
            });
            let skip = (n + 1).min(source.rows.len());
            source.rows.drain(..skip);
            source.names = names;
            first_row = n + 2;
        }
    } else if request.profile.file_type == FileType::Merge {
        first_row = 2;
    }
    import_rows(target, request, source, first_row, locale)
}

/// Import parsed rows; `first_row` numbers the first data row.
pub fn import_rows(
    target: &mut dyn ImportTarget,
    request: &ImportRequest,
    source: Source,
    first_row: usize,
    locale: &Locale,
) -> Result<ImportSummary> {
    let options = &request.options;
    if options.flags & !import_flag::KNOWN != 0 {
        return Err(TransferError::new(
            code::UNAVAILABLE,
            format!(
                "Import option flags {:#04x} are not supported.",
                options.flags & !import_flag::KNOWN
            ),
        ));
    }
    let occurrence = options.table.id().ok_or_else(|| {
        TransferError::new(code::FIELD_MISSING, "The import has no target table.")
    })?;
    let fields = target.fields(occurrence)?;
    let columns = map_columns(options, &source, &fields);
    let auto_enter = options.has(import_flag::AUTO_ENTER);
    let add_remaining = options.has(import_flag::ADD_REMAINING);
    let preserve = options.has(import_flag::PRESERVE_TARGET);
    if preserve
        && auto_enter
        && options.action != ImportAction::Add
        && columns.iter().flatten().any(|t| t.field.auto_enters)
    {
        return Err(TransferError::new(
            code::UNAVAILABLE,
            "Preserving target data while performing auto-enter options into a field \
             that auto-enters is not supported.",
        ));
    }
    let mut summary = ImportSummary {
        total: source.rows.len(),
        ..ImportSummary::default()
    };
    // The mapped columns: a row costs what it imports, not the source's
    // width.
    let mapped: Vec<(usize, &Target)> = columns
        .iter()
        .enumerate()
        .filter_map(|(i, c)| Some((i, c.as_ref()?)))
        .collect();
    // `existing`: the writes update a record (module docs, "Preserve
    // target field data").
    let writes_of = |row: &SourceRow, with_match: bool, existing: bool| -> Vec<FieldWrite> {
        let mut out = Vec::new();
        for &(i, t) in &mapped {
            if t.matching && !with_match {
                continue;
            }
            let values = row.get(i).cloned().unwrap_or_else(|| vec![String::new()]);
            let reps = t.field.repetitions.max(1) as usize;
            for r in 0..reps {
                let text = values.get(r).cloned().unwrap_or_default();
                if reps == 1 && r > 0 {
                    break;
                }
                if preserve && existing && text.is_empty() {
                    continue;
                }
                out.push(FieldWrite {
                    field: t.field.id,
                    repetition: r as u32 + 1,
                    text: stored_text(&text, t.field.ty, locale),
                });
            }
        }
        out
    };
    let mut found = Vec::new();
    let add = |target: &mut dyn ImportTarget,
               summary: &mut ImportSummary,
               found: &mut Vec<RecordId>,
               index: usize,
               writes: &[FieldWrite]| {
        match target.create(occurrence, writes, auto_enter) {
            Ok(id) => {
                summary.added += 1;
                found.push(id);
            }
            Err(e) => summary.errors.push(RowError {
                row: first_row + index,
                code: e.code,
                message: e.message,
            }),
        }
    };
    match options.action {
        ImportAction::Add => {
            for (i, row) in source.rows.iter().enumerate() {
                if target.interrupted() {
                    return Err(crate::interrupted());
                }
                let writes = writes_of(row, true, false);
                add(target, &mut summary, &mut found, i, &writes);
            }
        }
        ImportAction::UpdateFoundSet => {
            let records = target.found_set(occurrence)?;
            for (i, row) in source.rows.iter().enumerate() {
                if target.interrupted() {
                    return Err(crate::interrupted());
                }
                match records.get(i) {
                    Some(&record) => {
                        let writes = writes_of(row, true, true);
                        match target.update(occurrence, record, &writes, auto_enter) {
                            Ok(()) => summary.updated += 1,
                            Err(e) => summary.errors.push(RowError {
                                row: first_row + i,
                                code: e.code,
                                message: e.message,
                            }),
                        }
                    }
                    None if add_remaining => {
                        let writes = writes_of(row, true, false);
                        add(target, &mut summary, &mut found, i, &writes);
                    }
                    None => break,
                }
            }
            let mut all = records;
            all.append(&mut found);
            found = all;
        }
        ImportAction::UpdateMatching => {
            let records = target.found_set(occurrence)?;
            let keys: Vec<(usize, u64, FieldType)> = columns
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    c.as_ref()
                        .filter(|t| t.matching)
                        .map(|t| (i, t.field.id, t.field.ty))
                })
                .collect();
            if keys.is_empty() {
                return Err(TransferError::new(
                    code::IMPORT_ORDER_INVALID,
                    "An update of matching records needs a match field.",
                ));
            }
            // The found set's records by their match values, in found set
            // order.
            let mut by_key: HashMap<Vec<Vec<u8>>, Vec<RecordId>> = HashMap::new();
            for &record in &records {
                if target.interrupted() {
                    return Err(crate::interrupted());
                }
                let mut k = Vec::with_capacity(keys.len());
                for (_, field, ty) in &keys {
                    let text = target.value(occurrence, record, *field)?;
                    k.push(match_key(&text, *ty, locale));
                }
                by_key.entry(k).or_default().push(record);
            }
            let mut updated = HashSet::new();
            for (i, row) in source.rows.iter().enumerate() {
                if target.interrupted() {
                    return Err(crate::interrupted());
                }
                let row_key: Vec<Vec<u8>> = keys
                    .iter()
                    .map(|(c, _, ty)| {
                        let text = row.get(*c).and_then(|v| v.first());
                        match_key(text.map_or("", String::as_str), *ty, locale)
                    })
                    .collect();
                let matches = by_key.get(&row_key).map_or(&[][..], Vec::as_slice);
                if matches.is_empty() {
                    if add_remaining {
                        let writes = writes_of(row, true, false);
                        add(target, &mut summary, &mut found, i, &writes);
                    }
                    continue;
                }
                // With "add remaining" a matched record takes the row's
                // match values too (`alpha` over `Alpha`, oracle x04).
                let writes = writes_of(row, add_remaining, true);
                for &record in matches {
                    match target.update(occurrence, record, &writes, auto_enter) {
                        Ok(()) => {
                            summary.updated += 1;
                            if updated.insert(record) {
                                found.push(record);
                            }
                        }
                        Err(e) => summary.errors.push(RowError {
                            row: first_row + i,
                            code: e.code,
                            message: e.message,
                        }),
                    }
                }
            }
        }
    }
    target.set_found_set(occurrence, &found)?;
    summary.found_set = found;
    Ok(summary)
}

/// The target of each source column (module docs).
fn map_columns(
    options: &ImportOptions,
    source: &Source,
    fields: &[TargetField],
) -> Vec<Option<Target>> {
    let width = source
        .rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(source.names.as_ref().map_or(0, Vec::len));
    let importable = |f: &TargetField| {
        !matches!(f.kind, FieldKind::Calculation | FieldKind::Summary)
            && f.ty != FieldType::Container
    };
    let by_id = |id: u64| fields.iter().find(|f| f.id == id);
    let stored = |i: usize| -> Option<Target> {
        let entry = options.map.get(i)?;
        let field = by_id(entry.field.id()?)?;
        match entry.mapping {
            Mapping::Import | Mapping::Match if importable(field) => Some(Target {
                field: field.clone(),
                matching: entry.mapping == Mapping::Match
                    && options.action == ImportAction::UpdateMatching,
            }),
            _ => None,
        }
    };
    if options.has(import_flag::MATCH_NAMES) {
        if let Some(names) = &source.names {
            // Each field's first entry in the import order.
            let mut entries = HashMap::new();
            for e in &options.map {
                if let Some(id) = e.field.id() {
                    entries.entry(id).or_insert(e);
                }
            }
            return (0..width)
                .map(|i| {
                    let name = names.get(i)?;
                    let field = fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))?;
                    if !importable(field) {
                        return None;
                    }
                    let entry = entries.get(&field.id).copied();
                    let matching = entry.is_some_and(|e| e.mapping == Mapping::Match)
                        && options.action == ImportAction::UpdateMatching;
                    if entry.is_some_and(|e| {
                        matches!(e.mapping, Mapping::DontImport | Mapping::Unimportable)
                    }) {
                        return None;
                    }
                    Some(Target {
                        field: field.clone(),
                        matching,
                    })
                })
                .collect();
        }
    }
    (0..width).map(stored).collect()
}

/// How update matching compares a match value (oracle x04): as the field's
/// index keys it. Text by its English index key, so case does not matter
/// (`alpha` matches `Alpha`); numbers, dates, times and timestamps by
/// value (`007` matches 7, `1e1` 10, `2.50` 2.5); an empty value, and one
/// that is not of the field's type, by its text's key.
fn match_key(text: &str, ty: FieldType, locale: &Locale) -> Vec<u8> {
    use crate::text;
    let typed = match ty {
        _ if text.is_empty() => None,
        FieldType::Number => {
            let n = om_calc::Number::parse_text(text, '.', ',');
            (!n.is_empty() && !n.is_nan()).then(|| (b'N', text::number_text(&n)))
        }
        FieldType::Date => text::parse_date(text, locale).map(|d| (b'D', text::date_text(d))),
        FieldType::Time => text::parse_time(text, locale).map(|t| (b'H', text::time_text(t))),
        FieldType::Timestamp => {
            text::parse_timestamp(text, locale).map(|t| (b'S', text::timestamp_text(t)))
        }
        FieldType::Text | FieldType::Container => None,
    };
    let (kind, key) = match typed {
        Some((kind, canonical)) => (kind, canonical.into_bytes()),
        None => (b'T', om_collate::text_key(text)),
    };
    [vec![kind], key].concat()
}

/// The text a field stores for an imported value (module docs).
pub fn stored_text(text: &str, ty: FieldType, locale: &Locale) -> String {
    if ty != FieldType::Date || text.is_empty() {
        return text.to_owned();
    }
    let year = text
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("");
    if year.len() >= 4 {
        return text.to_owned();
    }
    match crate::text::parse_date(text, locale) {
        Some(d) => crate::text::date_text(d),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle x04: matching compares as the field's index does.
    #[test]
    fn match_values_compare_as_the_fields_index_keys_them() {
        let en = Locale::en_us();
        let same = |a: &str, b: &str, ty| match_key(a, ty, &en) == match_key(b, ty, &en);
        assert!(same("alpha", "Alpha", FieldType::Text));
        assert!(same("BETA", "Beta", FieldType::Text));
        assert!(!same("Alpha", "Alpha2", FieldType::Text));
        for (a, b) in [("007", "7"), ("2.50", "2.5"), ("-3.0", "-3"), ("1e1", "10")] {
            assert!(same(a, b, FieldType::Number), "{a} {b}");
        }
        assert!(!same("7", "8", FieldType::Number));
        assert!(same("01/02/2026", "1/2/2026", FieldType::Date));
        assert!(!same("1/3/26", "1/2/2026", FieldType::Date));
        assert!(same("", "", FieldType::Number));
        assert!(!same("", "0", FieldType::Number));
    }

    #[test]
    fn dates_with_short_years_are_completed() {
        let en = Locale::en_us();
        assert_eq!(stored_text("1/1/24", FieldType::Date, &en), "01/01/2024");
        assert_eq!(stored_text("2/29/2024", FieldType::Date, &en), "2/29/2024");
        assert_eq!(
            stored_text("13/45/2020", FieldType::Date, &en),
            "13/45/2020"
        );
        assert_eq!(stored_text("1/1/24", FieldType::Text, &en), "1/1/24");
    }
}
