//! Export Records (36) and Save Records as Excel (143) without a host: the
//! rows FileMaker writes for a found set and an export order, and the file
//! formats (oracle x01, x03 on FileMaker Server 26.0.3; script-steps.md,
//! "Implementation notes (import/export)").
//!
//! Rows. Each record of the found set is one row, its fields in export
//! order; a repeating field holds all its repetitions. Container fields are
//! left out of every format. A related field holds one value per related
//! record: XML keeps them in one row, the other formats write the first on
//! the record's row and one more row per further related record, holding
//! only related values.
//!
//! Groups. With "group by" fields and a sorted found set, the sort fields up
//! to the last group-by field are group levels and each row is the first
//! record of a group of the finest level: a level field shows its value
//! only on the row that starts its group, a summary field with a break
//! field shows the summary of the break field's group on the row starting
//! it, and a summary field without one shows the grand summary on the first
//! row only. Without groups a summary field shows its value on the row's
//! record ([`ExportSource::record_summaries`]): a total or count the found
//! set's on every row, a running total so far and a fraction of total the
//! record's own (measured on FileMaker Pro 26.0.3 without restart or
//! subtotal); one with a break field is empty.

mod dbf;
mod delimited;
mod html;
mod xlsx;
mod xml;

use om_calc::Locale;

use crate::spec::{ExportOptions, FieldSpec, FileType, Profile, ProfileBody};
use crate::text::ExportValue;
use crate::{code, ExportCharset, FieldKind, FieldType, RecordId, Result, TransferError};

/// A field of the export order as the host describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    /// The table occurrence's name (`Addresses` of `Addresses::City`).
    pub occurrence: String,
    /// Through a relationship: not the layout's table occurrence.
    pub related: bool,
    pub ty: FieldType,
    pub kind: FieldKind,
    /// Maximum repetitions (1 for a field that does not repeat).
    pub repetitions: u32,
    /// No "not empty" validation (FMPXMLRESULT `EMPTYOK`).
    pub empty_ok: bool,
}

/// A record of the found set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportRecord {
    pub id: RecordId,
    /// The modification count (FMPXMLRESULT `MODID`).
    pub mod_count: u64,
}

/// What FileMaker writes about the file and application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportContext {
    /// The database file name with its extension.
    pub file_name: String,
    /// The layout name FMPXMLRESULT shows (empty on FileMaker Server).
    pub layout_name: String,
    /// The number of records in the table (FMPXMLRESULT `RECORDS`).
    pub total_records: u64,
    /// `PRODUCT BUILD`, `NAME`, `VERSION` (`08-26-2026`, `FileMaker`,
    /// `Server 26.0.3`).
    pub product_build: String,
    pub product_name: String,
    pub product_version: String,
    /// FMPXMLRESULT `DATEFORMAT` and `TIMEFORMAT` (`M/d/yyyy`, `h:mm:ss a`).
    pub date_format: String,
    pub time_format: String,
    /// The file's locale (dates in Excel cells).
    pub locale: Locale,
    /// The export's time in UTC, `2026-10-29T14:17:34Z` (Excel properties).
    pub created: String,
}

/// The host's records: the found set of the current window's layout.
pub trait ExportSource {
    fn context(&mut self) -> Result<ExportContext>;

    /// Describe a field of an occurrence; error 102 when it is missing.
    fn field(&mut self, occurrence: u64, field: u64) -> Result<FieldInfo>;

    /// The found set in its order.
    fn records(&mut self) -> Result<Vec<ExportRecord>>;

    /// The fields the found set is sorted by (occurrence, field), in
    /// priority order; empty when unsorted.
    fn sort_fields(&mut self) -> Result<Vec<(u64, u64)>>;

    /// A field's values for a record: for a field of the layout's table one
    /// entry holding its repetitions; for a related field one entry per
    /// related record, in the relationship's order (none without one).
    fn values(
        &mut self,
        record: RecordId,
        occurrence: u64,
        field: u64,
    ) -> Result<Vec<Vec<ExportValue>>>;

    /// A summary field's value over `records`: the found set's grand
    /// summary (`by` none; `GetSummary ( s ; s )`, a running total's total
    /// and a fraction's 1, as FileMaker Pro 26.0.3 writes a grouped
    /// export's first row) or the group of records sharing the break field
    /// `by` (the found set is sorted by it; `records` are the group's, the
    /// first starts it).
    fn summary(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[RecordId],
        by: Option<(u64, u64)>,
    ) -> Result<ExportValue>;

    /// A summary field's value on each of `records` (the found set's, or
    /// the current record), one per record, as the record shows it in the
    /// found set: a running summary so far, the record's fraction of the
    /// total, the found set's summary otherwise. Read in one pass over the
    /// found set, not once per record.
    fn record_summaries(
        &mut self,
        occurrence: u64,
        field: u64,
        records: &[RecordId],
    ) -> Result<Vec<ExportValue>>;

    /// "Apply current layout's data formatting": a value's text as the
    /// current layout formats the field, `None` when the field is not on
    /// the layout (the value exports unformatted).
    fn formatted(&mut self, _occurrence: u64, _field: u64, _value: &ExportValue) -> Option<String> {
        None
    }

    /// Whether to stop: the host's script was aborted or ran out of time.
    /// Checked before every record and group row; the export then ends
    /// with error 1 and writes no file.
    fn interrupted(&mut self) -> bool {
        false
    }
}

/// An export: the stored order and the output type (`85`, `86`), or the
/// same built by a dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRequest {
    pub options: ExportOptions,
    pub profile: Profile,
    /// Save Records as Excel's "Use field names as column names" (Export
    /// Records writes no header row).
    pub field_names_row: bool,
    /// Excel worksheet name and document properties (Save Records as
    /// Excel), `Sheet1` and empty by default.
    pub worksheet: String,
    pub title: String,
    pub subject: String,
    pub author: String,
}

impl ExportRequest {
    pub fn new(options: ExportOptions, profile: Profile) -> ExportRequest {
        ExportRequest {
            options,
            profile,
            field_names_row: false,
            worksheet: "Sheet1".into(),
            title: String::new(),
            subject: String::new(),
            author: String::new(),
        }
    }
}

/// What a column shows on group rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Plain,
    /// A sort field that is a group level.
    Level(usize),
    /// A summary field broken by the group level.
    SummaryBy(usize),
    /// A summary field broken by a field that is not a level.
    SummaryOff,
    /// A summary field without a break field.
    Summary,
}

/// One exported column.
#[derive(Clone, Debug)]
pub(crate) struct Column {
    pub occurrence: u64,
    pub field: u64,
    pub info: FieldInfo,
    /// The break field's name of a summary field grouped by it.
    pub by: Option<String>,
    role: Role,
}

impl Column {
    /// The header merge files and Excel write (`S by Company`).
    pub fn merge_name(&self) -> String {
        match &self.by {
            Some(by) => format!("{} by {}", self.info.name, by),
            None => self.info.name.clone(),
        }
    }

    /// The name FMPXMLRESULT writes (`Addresses::City`).
    pub fn xml_name(&self) -> String {
        if self.info.related {
            format!("{}::{}", self.info.occurrence, self.info.name)
        } else {
            self.info.name.clone()
        }
    }
}

/// A cell of a row.
#[derive(Clone, Debug)]
pub(crate) enum Cell {
    /// A related field without a related record.
    Absent,
    /// A field's repetitions, or a related field's values (one per related
    /// record).
    Values(Vec<ExportValue>),
}

impl Cell {
    fn empty() -> Cell {
        Cell::Values(Vec::new())
    }
}

/// One row: a record or a group's first record.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub record: ExportRecord,
    pub cells: Vec<Cell>,
}

/// The columns and rows of an export.
pub(crate) struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// The found set's record count.
    pub found: usize,
    pub context: ExportContext,
    pub charset: ExportCharset,
}

impl Table {
    /// Rows as text formats write them: one more row per further related
    /// record, holding only related cells (`None` for the other cells).
    pub fn expanded(&self) -> Vec<(bool, Vec<Option<Cell>>)> {
        let mut out = Vec::new();
        for row in &self.rows {
            let extra = row
                .cells
                .iter()
                .zip(&self.columns)
                .filter(|(_, c)| c.info.related)
                .map(|(cell, _)| match cell {
                    Cell::Values(v) => v.len().saturating_sub(1),
                    Cell::Absent => 0,
                })
                .max()
                .unwrap_or(0);
            let first = row
                .cells
                .iter()
                .zip(&self.columns)
                .map(|(cell, c)| {
                    Some(match (cell, c.info.related) {
                        (Cell::Values(v), true) if !v.is_empty() => {
                            Cell::Values(vec![v[0].clone()])
                        }
                        (cell, _) => cell.clone(),
                    })
                })
                .collect();
            out.push((false, first));
            for i in 1..=extra {
                let cells = row
                    .cells
                    .iter()
                    .zip(&self.columns)
                    .map(|(cell, c)| match (cell, c.info.related) {
                        (Cell::Values(v), true) => Some(match v.get(i) {
                            Some(x) => Cell::Values(vec![x.clone()]),
                            None => Cell::Absent,
                        }),
                        (_, true) => Some(Cell::Absent),
                        (_, false) => None,
                    })
                    .collect();
                out.push((true, cells));
            }
        }
        out
    }
}

/// Export the found set: the file's bytes. Errors: 410 without a field to
/// export, 720 for a repeating field in Excel or dBase, 3 for a dBase file
/// past its header's 16-bit lengths (`dbf`), and the source's.
/// An empty found set writes no file (`Ok(None)`), as FileMaker Server
/// does.
pub fn export(source: &mut dyn ExportSource, request: &ExportRequest) -> Result<Option<Vec<u8>>> {
    let table = build(source, request)?;
    if table.rows.is_empty() {
        return Ok(None);
    }
    let bytes = match request.profile.file_type {
        FileType::TabSeparated => delimited::tab(&table),
        FileType::CommaSeparated => delimited::comma(&table),
        FileType::Merge => delimited::merge(&table),
        FileType::HtmlTable => html::write(&table),
        FileType::XmlExport => {
            let grammar = match &request.profile.body {
                ProfileBody::XmlExport { grammar, .. } => *grammar,
                _ => 1,
            };
            if grammar != 1 {
                return Err(TransferError::new(
                    code::UNAVAILABLE,
                    "Only the FMPXMLRESULT grammar is supported.",
                ));
            }
            xml::write(&table)
        }
        FileType::ExcelExport | FileType::Excel2007 => {
            repeating_check(&table)?;
            xlsx::write(&table, request)?
        }
        FileType::DBase => {
            repeating_check(&table)?;
            dbf::write(&table)?
        }
        other => {
            return Err(TransferError::new(
                code::UNAVAILABLE,
                format!("Export to {other:?} is not supported."),
            ))
        }
    };
    Ok(Some(bytes))
}

fn repeating_check(table: &Table) -> Result<()> {
    if table.columns.iter().any(|c| c.info.repetitions > 1) {
        return Err(TransferError::new(
            code::REPEATING_FIELDS,
            "This format does not support repeating fields.",
        ));
    }
    Ok(())
}

fn ids(spec: &FieldSpec) -> Option<(u64, u64)> {
    spec.ids()
}

/// The columns and rows (module docs).
pub(crate) fn build(source: &mut dyn ExportSource, request: &ExportRequest) -> Result<Table> {
    let context = source.context()?;
    let options = &request.options;
    let sort = source.sort_fields()?;
    let group_by: Vec<(u64, u64)> = options
        .group_by
        .iter()
        .filter_map(|(f, _)| ids(f))
        .collect();
    // Levels: the sort fields up to the last one that is a group-by field.
    let levels: Vec<(u64, u64)> = match sort.iter().rposition(|s| group_by.contains(s)) {
        Some(last) => sort[..=last].to_vec(),
        None => Vec::new(),
    };
    let mut columns = Vec::new();
    for entry in &options.fields {
        let Some((occurrence, field)) = ids(&entry.field) else {
            continue;
        };
        let info = source.field(occurrence, field)?;
        if info.ty == FieldType::Container {
            continue;
        }
        let by_ids = ids(&entry.sum_by);
        let by = match by_ids {
            Some((o, f)) if info.kind == FieldKind::Summary => Some(source.field(o, f)?.name),
            _ => None,
        };
        let role = if info.kind == FieldKind::Summary {
            match by_ids {
                Some(b) => levels
                    .iter()
                    .position(|l| *l == b)
                    .map_or(Role::SummaryOff, Role::SummaryBy),
                None => Role::Summary,
            }
        } else {
            match levels.iter().position(|l| *l == (occurrence, field)) {
                Some(l) => Role::Level(l),
                None => Role::Plain,
            }
        };
        columns.push(Column {
            occurrence,
            field,
            info,
            by,
            role,
        });
    }
    if columns.is_empty() {
        return Err(TransferError::new(
            code::EXPORT_ORDER_INVALID,
            "The export order has no fields.",
        ));
    }
    let records = source.records()?;
    let formatting = options.formatting;
    let mut rows = Vec::new();
    if levels.is_empty() {
        // Each summary column's value on every row's record, read at once.
        let ids: Vec<RecordId> = records.iter().map(|r| r.id).collect();
        let mut summaries: Vec<Vec<ExportValue>> = vec![Vec::new(); columns.len()];
        for (i, c) in columns.iter().enumerate() {
            if c.role == Role::Summary {
                let values = source.record_summaries(c.occurrence, c.field, &ids)?;
                if values.len() != ids.len() {
                    return Err(TransferError::new(
                        code::UNAVAILABLE,
                        "The host read a summary for some of the records only.",
                    ));
                }
                summaries[i] = values;
            }
        }
        for (row, record) in records.iter().enumerate() {
            if source.interrupted() {
                return Err(crate::interrupted());
            }
            let mut cells = Vec::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                cells.push(match c.role {
                    Role::Summary => Cell::Values(vec![summaries[i][row].clone()]),
                    Role::SummaryBy(_) | Role::SummaryOff => Cell::empty(),
                    _ => field_cell(source, record.id, c, formatting)?,
                });
            }
            rows.push(Row {
                record: *record,
                cells,
            });
        }
    } else {
        // Group keys per record and level.
        let mut keys: Vec<Vec<String>> = Vec::with_capacity(records.len());
        for record in &records {
            if source.interrupted() {
                return Err(crate::interrupted());
            }
            let mut k = Vec::with_capacity(levels.len());
            for (o, f) in &levels {
                let v = source.values(record.id, *o, *f)?;
                k.push(
                    v.first()
                        .and_then(|r| r.first())
                        .map(ExportValue::text)
                        .unwrap_or_default(),
                );
            }
            keys.push(k);
        }
        // The finest groups: runs of equal keys.
        let mut starts = Vec::new();
        for i in 0..records.len() {
            if i == 0 || keys[i] != keys[i - 1] {
                starts.push(i);
            }
        }
        let all: Vec<RecordId> = records.iter().map(|r| r.id).collect();
        for (g, &start) in starts.iter().enumerate() {
            if source.interrupted() {
                return Err(crate::interrupted());
            }
            // The highest level (smallest index) at which this row starts a
            // new group: 0 for the first row.
            let new_from = if g == 0 {
                0
            } else {
                let prev = &keys[starts[g - 1]];
                (0..levels.len())
                    .find(|&l| keys[start][l] != prev[l])
                    .unwrap_or(levels.len())
            };
            let mut cells = Vec::with_capacity(columns.len());
            for c in &columns {
                cells.push(match c.role {
                    Role::Level(l) if l < new_from => Cell::empty(),
                    Role::Level(_) | Role::Plain => {
                        field_cell(source, records[start].id, c, formatting)?
                    }
                    Role::SummaryBy(l) if l >= new_from => {
                        // The level-l group: records sharing keys 0..=l.
                        let mut members = Vec::new();
                        let mut i = start;
                        while i < records.len() && keys[i][..=l] == keys[start][..=l] {
                            members.push(records[i].id);
                            i += 1;
                        }
                        Cell::Values(vec![source.summary(
                            c.occurrence,
                            c.field,
                            &members,
                            Some(levels[l]),
                        )?])
                    }
                    Role::SummaryBy(_) | Role::SummaryOff => Cell::empty(),
                    Role::Summary if g == 0 => {
                        Cell::Values(vec![source.summary(c.occurrence, c.field, &all, None)?])
                    }
                    Role::Summary => Cell::empty(),
                });
            }
            rows.push(Row {
                record: records[start],
                cells,
            });
        }
    }
    Ok(Table {
        columns,
        rows,
        found: records.len(),
        context,
        charset: options.charset,
    })
}

fn field_cell(
    source: &mut dyn ExportSource,
    record: RecordId,
    c: &Column,
    formatting: bool,
) -> Result<Cell> {
    let values = source.values(record, c.occurrence, c.field)?;
    let format = |source: &mut dyn ExportSource, v: ExportValue| {
        if formatting {
            if let Some(text) = source.formatted(c.occurrence, c.field, &v) {
                return ExportValue::Stored(text);
            }
        }
        v
    };
    if c.info.related {
        if values.is_empty() {
            return Ok(Cell::Absent);
        }
        let mut out = Vec::with_capacity(values.len());
        for related in values {
            let v = related
                .into_iter()
                .next()
                .unwrap_or_else(ExportValue::empty);
            out.push(format(source, v));
        }
        return Ok(Cell::Values(out));
    }
    let mut reps = values.into_iter().next().unwrap_or_default();
    let n = c.info.repetitions.max(1) as usize;
    reps.truncate(n);
    while reps.len() < n {
        reps.push(ExportValue::empty());
    }
    let reps = reps.into_iter().map(|v| format(source, v)).collect();
    Ok(Cell::Values(reps))
}
