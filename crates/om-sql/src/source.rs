//! What a query reads (execute-sql.md 4, 5.1): the file's table
//! occurrences and their fields, records and values, through
//! [`SqlSource`]; and the system tables built from the same catalog (4.2).

use std::rc::Rc;

use om_calc::{CalcError, Locale, Timestamp, Value};

use crate::error::SqlError;

/// A field's type as FQL sees it (`FieldType`, 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
}

impl FieldType {
    /// The `FieldType` word of the system tables.
    pub fn sql_name(self) -> &'static str {
        match self {
            FieldType::Text => "varchar",
            FieldType::Number => "decimal",
            FieldType::Date => "date",
            FieldType::Time => "time",
            FieldType::Timestamp => "timestamp",
            FieldType::Container => "binary",
        }
    }
}

/// `FieldClass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldClass {
    Normal,
    Calculated,
    Summary,
}

impl FieldClass {
    pub fn sql_name(self) -> &'static str {
        match self {
            FieldClass::Normal => "Normal",
            FieldClass::Calculated => "Calculated",
            FieldClass::Summary => "Summary",
        }
    }
}

/// A field of a table occurrence's base table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDef {
    /// The field id (`FieldId`), what [`SqlSource::value`] takes.
    pub id: u64,
    pub name: String,
    /// The data type, or the result type of a calculation or summary.
    pub ty: FieldType,
    pub class: FieldClass,
    /// Global storage: one value for every record.
    pub global: bool,
    /// Maximum repetitions, 1 for a field that does not repeat.
    pub repetitions: u32,
    pub mod_count: u64,
}

impl FieldDef {
    /// `FieldType` as the system tables print it: `global ` before it and
    /// `[n]` after it for n repetitions (`varchar[9]`).
    pub fn type_text(&self) -> String {
        let mut s = String::new();
        if self.global {
            s.push_str("global ");
        }
        s.push_str(self.ty.sql_name());
        if self.repetitions > 1 {
            s.push_str(&format!("[{}]", self.repetitions));
        }
        s
    }
}

/// A table occurrence: what a table name in `FROM` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDef {
    /// The source's handle for the occurrence, passed back to
    /// [`SqlSource::fields`], [`SqlSource::records`] and
    /// [`SqlSource::value`].
    pub id: u64,
    pub name: String,
    /// `FileMaker_Tables.TableId`.
    pub table_id: u64,
    /// The base table's name and file (without extension); `None` for an
    /// occurrence of a file that is not open (4.1), which has no fields or
    /// records and fails `SELECT *` with error 100.
    pub base: Option<(String, String)>,
    pub mod_count: u64,
}

/// A base table (`FileMaker_BaseTables`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseTableDef {
    pub name: String,
    pub id: u64,
    /// `<Internal>` for FileMaker tables.
    pub source: String,
    pub mod_count: u64,
    /// The fields, for `FileMaker_BaseTableFields`.
    pub fields: Vec<FieldDef>,
}

/// A value list (`FileMaker_ValueLists`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueListDef {
    pub name: String,
    pub id: u64,
    /// `Custom`, `Field` or `Related`.
    pub source: String,
    pub mod_count: u64,
}

/// A value list's values for `FileMaker_ValueList_<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueListValues {
    Values(Vec<String>),
    /// A list of related values: the one row `<Valuelist Unsupported>`.
    Unsupported,
}

/// Where a query reads a record (10.1): its table's found set, the records
/// left by the finds the `WHERE` clause's conditions on that table become.
/// Calculation fields see it as `Get ( FoundCount )` and
/// `Get ( RecordNumber )` (the FileMaker Server sample's `If ( Get (
/// FoundCount ) = 1 ; ... )` reads `1` for `WHERE id = 3` and the table's
/// count without a `WHERE`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub found_count: usize,
    /// The record's position in the found set, from 1.
    pub record_number: usize,
    /// The found set itself, in order: the records a summary field or
    /// `GetNthRecord` in a calculation sees, and those a source that
    /// downloads can read ahead.
    pub found: Option<Rc<Vec<u64>>>,
}

/// The file a query runs against. Names compare case-insensitively in
/// om-sql; the source only lists what it has.
///
/// Values are calculation values (5.1): a field's value as FileMaker
/// stores or computes it, an empty field the empty value of its type
/// (om-sql reads empty as NULL). Calculation fields are computed by the
/// source, independent of the calculation that runs the query (1.3).
pub trait SqlSource {
    /// The file's locale: parameter text, text compared with dates and
    /// times (5.4), `NUMVAL`, `DAYNAME`, `MONTHNAME`.
    fn locale(&self) -> &Locale;

    /// The current timestamp for `CURRENT_DATE`, `CURRENT_TIME`,
    /// `CURRENT_TIMESTAMP` (whole seconds; FileMaker Server uses its own
    /// clock).
    fn now(&self) -> Timestamp;

    /// The account name for `USER`.
    fn user_name(&self) -> String {
        String::new()
    }

    /// The table occurrences, in the order of the file's name index
    /// (`FileMaker_Tables` lists them so).
    fn tables(&self) -> Vec<TableDef>;

    /// The fields of an occurrence's base table in field id order (creation
    /// order).
    fn fields(&self, table: &TableDef) -> Vec<FieldDef>;

    /// The record ids of an occurrence's table in creation order.
    fn records(&mut self, table: &TableDef) -> Result<Vec<u64>, SqlError>;

    /// Repetition `repetition` (from 1, within the field's repetitions) of
    /// a field on a record read through `cursor`; global fields ignore the
    /// record, and a summary field is its summary over the cursor's found
    /// set as it shows on the record (12, S3; empty from a source that does
    /// not compute summaries).
    fn value(
        &mut self,
        table: &TableDef,
        record: u64,
        field: &FieldDef,
        repetition: u32,
        cursor: Cursor,
    ) -> Result<Value, CalcError>;

    /// A record's modification count (`ROWMODID`).
    fn mod_count(&mut self, _table: &TableDef, _record: u64) -> u64 {
        0
    }

    /// The records whose `field` (repetition 1) may equal `value`, from an
    /// index or a find on the host; `None` when the source has no index for
    /// it. om-sql checks every proposed record again with SQL's comparison
    /// (10.1), so an index may propose more records than match, never
    /// fewer. An error ends the query (a source that could not reach its
    /// host).
    fn lookup(
        &mut self,
        _table: &TableDef,
        _field: &FieldDef,
        _value: &Value,
    ) -> Result<Option<Vec<u64>>, SqlError> {
        Ok(None)
    }

    /// The base tables (`FileMaker_BaseTables`, in name order).
    fn base_tables(&self) -> Vec<BaseTableDef> {
        Vec::new()
    }

    /// The value lists (`FileMaker_ValueLists`, in name order).
    fn value_lists(&self) -> Vec<ValueListDef> {
        Vec::new()
    }

    /// The values of the value list named `name` (matched
    /// case-insensitively); `None` for no such list.
    fn value_list_values(&mut self, _name: &str) -> Option<ValueListValues> {
        None
    }

    /// Whether the query should stop now (the script that runs it was
    /// aborted or ran out of time). om-sql asks every 256 rows of a scan,
    /// join, filter, group or projection and ends the query with error 1
    /// (`Operation aborted`).
    fn interrupted(&self) -> bool {
        false
    }

    /// How many bytes the rows of one intermediate result of a query (a
    /// table's rows, a join, the rows its `WHERE` keeps, a group) may take
    /// before the query ends with error 2, `Memory allocation failure`,
    /// rather than exhaust the host's memory. [`MEMORY_BUDGET`] by default.
    fn memory_budget(&self) -> usize {
        MEMORY_BUDGET
    }
}

/// The default [`SqlSource::memory_budget`]: 256 MiB. FileMaker keeps
/// results in temporary tables on disk and has no such limit (execute-sql.md
/// 10.1); this one is OpenMaker's, so that a script's runaway cross join
/// fails instead of taking the server's memory.
pub const MEMORY_BUDGET: usize = 256 << 20;

/// The system tables (4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemTable {
    Tables,
    Fields,
    BaseTables,
    BaseTableFields,
    ValueLists,
}

/// The column types of a system table, for typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemColumn {
    Text,
    Number,
}

impl SystemTable {
    pub fn by_name(name: &str) -> Option<SystemTable> {
        let tables = [
            ("FileMaker_Tables", SystemTable::Tables),
            ("FileMaker_Fields", SystemTable::Fields),
            ("FileMaker_BaseTables", SystemTable::BaseTables),
            ("FileMaker_BaseTableFields", SystemTable::BaseTableFields),
            ("FileMaker_ValueLists", SystemTable::ValueLists),
        ];
        tables
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, t)| *t)
    }

    /// The name FQL0048 reports.
    pub fn name(self) -> &'static str {
        match self {
            SystemTable::Tables => "FileMaker_Tables",
            SystemTable::Fields => "FileMaker_Fields",
            SystemTable::BaseTables => "FileMaker_BaseTables",
            SystemTable::BaseTableFields => "FileMaker_BaseTableFields",
            SystemTable::ValueLists => "FileMaker_ValueLists",
        }
    }

    /// Columns in `SELECT *` order.
    pub fn columns(self) -> &'static [(&'static str, SystemColumn)] {
        use SystemColumn::{Number as N, Text as T};
        match self {
            SystemTable::Tables => &[
                ("TableName", T),
                ("TableId", N),
                ("BaseTableName", T),
                ("BaseFileName", T),
                ("ModCount", N),
            ],
            SystemTable::Fields => &[
                ("TableName", T),
                ("FieldName", T),
                ("FieldType", T),
                ("FieldId", N),
                ("FieldClass", T),
                ("FieldReps", N),
                ("ModCount", N),
            ],
            SystemTable::BaseTables => &[
                ("BaseTableName", T),
                ("BaseTableId", N),
                ("Source", T),
                ("ModCount", N),
            ],
            SystemTable::BaseTableFields => &[
                ("BaseTableName", T),
                ("FieldName", T),
                ("FieldType", T),
                ("FieldId", N),
                ("FieldClass", T),
                ("FieldReps", N),
                ("ModCount", N),
            ],
            SystemTable::ValueLists => &[
                ("ValueListName", T),
                ("ValueListId", N),
                ("Source", T),
                ("ModCount", N),
            ],
        }
    }

    /// The rows, as values of the columns' types.
    pub fn rows(self, source: &dyn SqlSource) -> Vec<Vec<Value>> {
        let t = |s: &str| Value::text(s);
        let n = |v: u64| Value::number(v as i64);
        let field_row = |table: &str, f: &FieldDef| {
            vec![
                t(table),
                t(&f.name),
                t(&f.type_text()),
                n(f.id),
                t(f.class.sql_name()),
                n(u64::from(f.repetitions)),
                n(f.mod_count),
            ]
        };
        match self {
            SystemTable::Tables => source
                .tables()
                .iter()
                .map(|tb| {
                    let (base, file) = tb.base.clone().unwrap_or_default();
                    vec![
                        t(&tb.name),
                        n(tb.table_id),
                        t(&base),
                        t(&file),
                        n(tb.mod_count),
                    ]
                })
                .collect(),
            SystemTable::Fields => source
                .tables()
                .iter()
                .flat_map(|tb| {
                    source
                        .fields(tb)
                        .iter()
                        .map(|f| field_row(&tb.name, f))
                        .collect::<Vec<_>>()
                })
                .collect(),
            SystemTable::BaseTables => source
                .base_tables()
                .iter()
                .map(|b| vec![t(&b.name), n(b.id), t(&b.source), n(b.mod_count)])
                .collect(),
            SystemTable::BaseTableFields => source
                .base_tables()
                .iter()
                .flat_map(|b| {
                    b.fields
                        .iter()
                        .map(|f| field_row(&b.name, f))
                        .collect::<Vec<_>>()
                })
                .collect(),
            SystemTable::ValueLists => source
                .value_lists()
                .iter()
                .map(|v| vec![t(&v.name), n(v.id), t(&v.source), n(v.mod_count)])
                .collect(),
        }
    }
}

/// The value list a `FileMaker_ValueList_<name>` table names.
pub fn value_list_table(name: &str) -> Option<&str> {
    const PREFIX: &str = "FileMaker_ValueList_";
    let head = name.get(..PREFIX.len())?;
    head.eq_ignore_ascii_case(PREFIX)
        .then(|| &name[PREFIX.len()..])
}
