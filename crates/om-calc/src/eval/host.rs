//! The host boundary (calc-values.md 9): what the evaluator asks the file,
//! the session and the script engine. FileMaker's solver asks a
//! `DBFileUser` (variables, `Get`, layout objects) and the cursor and
//! catalog classes (fields, records, related sets); [`CalcHost`] is both.
//!
//! Records are numbered by their record ID in their base table; a table
//! occurrence names the table and, from the context occurrence, the
//! relationship path. Field ids are per base table, as in formulas.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use om_formula::catalog::Builtin;
use om_formula::{Expr, Formula, Names, NoNames, PluginFunctionId};

use super::vars::{VarScope, Variables};
use super::Context;
use crate::{CalcError, Container, Date, Locale, Value, ValueType};

/// How a field keeps its value.
#[derive(Clone, Debug)]
pub enum Storage {
    /// A value per record and repetition: data fields, stored calculations,
    /// auto-entered values. The host reads them ([`CalcHost::field_value`]).
    Stored,
    /// One value for the file (global storage), the same on every record.
    Global,
    /// An unstored calculation: the evaluator computes the repetition asked
    /// for from the formula, on the record, in the formula's table
    /// occurrence (the field's when the formula has none), and converts it
    /// to the field's result type (calc-values.md 8.8).
    Unstored(Arc<Formula>),
    /// A summary field: the host supplies its value over the found set,
    /// through [`CalcHost::field_value`] and [`CalcHost::summary`].
    Summary,
}

/// What the evaluator needs to know about a field.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    /// The data type, or a calculation's or summary's result type.
    pub value_type: ValueType,
    /// Maximum repetitions: 1 for a field that does not repeat.
    pub repetitions: u32,
    pub storage: Storage,
}

impl FieldInfo {
    /// A stored field of `value_type` with `repetitions` repetitions.
    pub fn stored(value_type: ValueType, repetitions: u32) -> FieldInfo {
        FieldInfo {
            value_type,
            repetitions: repetitions.max(1),
            storage: Storage::Stored,
        }
    }
}

/// A field as `GetSummary` names it: a table occurrence and a field of its
/// base table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldId {
    pub occurrence: u64,
    pub field: u64,
}

/// A custom function (`16/5/<id>`, calc-encoding.md 8). Its body is
/// compiled with the parameters as local slots `0..parameters.len()`
/// (calc-encoding.md 5.5).
#[derive(Clone, Debug)]
pub struct CustomFunction {
    pub name: String,
    pub parameters: Vec<String>,
    pub body: Expr,
}

/// A container field's stored data ([`CalcHost::container_data`]).
#[derive(Clone, Debug)]
pub struct StoredContainer {
    /// The data: the file name as its reference text and every stream
    /// (containers.md 2), the main data stream first.
    pub data: Container,
    /// Whether the stored data is intact: the streams match their stored
    /// MD5 (containers.md 2.2) and externally stored files exist and match
    /// (containers.md 8).
    pub intact: bool,
    /// Whether the data is stored outside the file (containers.md 8):
    /// `VerifyContainer` answers only for such data, `?` for embedded data
    /// (script oracle s34-containers-setfield).
    pub external: bool,
}

/// `LookupNext`'s direction when no record is related.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupDirection {
    Lower,
    Higher,
}

/// Data a host reads as missing while it fetches it for an evaluation that
/// will run again ([`CalcHost::pending`]). Both counts only grow while the
/// host lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Reads that found such data, a need read again included.
    pub reads: u64,
    /// The distinct needs among them: what the host will fetch.
    pub needs: u64,
}

/// How much one evaluation may build ([`CalcHost::limits`]): past either
/// limit it ends with error 7, "Running out of memory", as at the `While`
/// and custom function call limits (calc-values.md 7.1), its result `?`.
/// The limits are per top-level evaluation and cover everything it runs,
/// `Evaluate` text and custom functions included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalcLimits {
    /// The bytes one text value the evaluation builds may take: its UTF-8
    /// text and its style runs ([`crate::Text::size`]). Functions that
    /// would build more fail before they allocate it; text compiled by
    /// `Evaluate`, `GetField` and `IsValidExpression` may be a sixty-fourth
    /// of it ([`Self::formula`]).
    pub text: usize,
    /// How deep custom function calls and `Evaluate`s may nest, counted
    /// together: what the evaluator's stack grows with (about 5 KB a call
    /// and 8 KB an `Evaluate` in a release build). Tail calls do not nest.
    pub nesting: u32,
}

impl CalcLimits {
    /// FileMaker's documented maximum for text, 2 GB (its unit and
    /// whether a calculation reaches it unmeasured), and a nesting of
    /// 100,000, OpenMaker's own: FileMaker Server's engine was observed to
    /// recurse 60,000 calls deep, and how deep it nests `Evaluate` is
    /// unmeasured (calc-values.md 8.5).
    pub const DEFAULT: CalcLimits = CalcLimits {
        text: 2 << 30,
        nesting: 100_000,
    };

    /// The bytes of text `Evaluate`, `GetField` and `IsValidExpression`
    /// compile: a compiled tree takes up to about fifty times its text.
    pub fn formula(&self) -> usize {
        self.text / 64
    }
}

impl Default for CalcLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A function the host answers ([`CalcHost::call`]).
#[derive(Clone, Copy, Debug)]
pub enum HostFunction<'a> {
    /// A function of the language by its catalog name: those that need the
    /// host (`ExecuteSQL`, `ExecuteSQLe`, `GetLayoutObjectAttribute`,
    /// `GetLayoutObjectOwnerInfo`, `LayoutObjectUUID`, `GetAddonInfo`,
    /// container analysis, AI, `GetRecordIDsFromFoundSet`,
    /// `GetPersistentData`, ...) and any the library does not implement.
    Named(&'a str),
    /// A built-in plug-in function: the Design functions (`FMDR`) and the
    /// mobile functions (`FMMB`, calc-encoding.md 7).
    Builtin(&'a Builtin),
    /// A third-party plug-in function.
    Plugin(PluginFunctionId),
}

/// `Occurrence::Field` from `names`, as `GetFieldName` gives it.
pub fn qualified_field_name(names: &dyn Names, occurrence: u64, field: u64) -> Option<String> {
    names
        .occurrence_name(occurrence)
        .zip(names.field_name(occurrence, field))
        .map(|(o, f)| format!("{o}::{f}"))
}

/// The file, session and script state a calculation runs against. Only the
/// locale, the date and the variable maps are required; every other method
/// has a default for a host without that capability. The defaults are what
/// FileMaker Server's engine gives without a layout, window, record or
/// plug-in, or "not supported" (error 1225).
pub trait CalcHost {
    /// The file's locale (formats, separators, collation; calc-values.md
    /// 9.3). FileMaker Pro with "use system formats" passes the system's.
    fn locale(&self) -> &Locale;

    /// Today's date: the year text leaves out and the two-digit year window
    /// (calc-values.md 4.6).
    fn today(&self) -> Date;

    /// Read the layout object's value. A host may attach dependencies to
    /// this read; branches that do not evaluate Self do not acquire them.
    fn self_value(&mut self, context: &Context) -> Result<Value, CalcError> {
        context.self_value.clone().ok_or(CalcError::NOT_SUPPORTED)
    }

    /// The file's names, for compiling `Evaluate`, `GetField` and
    /// `IsValidExpression` text and for `GetFieldName`.
    fn names(&self) -> &dyn Names {
        &NoNames
    }

    /// The names of the file that owns a formula solved in `context` (the
    /// formula's `DBFileUser`, calc-values.md 9.1): [`Context::file`], else
    /// the file owning the context's table occurrence, so a host combining
    /// several files answers from those two alone. The default is
    /// [`Self::names`].
    fn names_for(&self, _context: &Context) -> &dyn Names {
        self.names()
    }

    /// The locale of the file that owns a formula solved in `context`
    /// (calc-values.md 9.3), chosen like [`Self::names_for`]. The default is
    /// [`Self::locale`].
    fn locale_for(&self, _context: &Context) -> &Locale {
        self.locale()
    }

    /// The file owning the fields of the base table `occurrence` shows, as
    /// [`Context::file`] names it: their formulas are solved for that file
    /// even without an occurrence of their own (calc-encoding.md 1), read
    /// through another file's occurrence of the table. `None`, the default,
    /// for a host of one file.
    fn table_file(&self, _occurrence: u64) -> Option<u64> {
        None
    }

    /// A field of the base table shown by `occurrence`; `None` when it does
    /// not exist (error 102).
    fn field(&self, _occurrence: u64, _field: u64) -> Option<FieldInfo> {
        None
    }

    /// The value of a stored, global or summary field on a record of
    /// `occurrence`'s table, repetition 1-based and within the field's
    /// repetitions. A number field's value is a number (`NumberText` when
    /// its text is not the number's own), an empty field the empty value of
    /// its type.
    fn field_value(
        &self,
        _occurrence: u64,
        _record: u64,
        _field: u64,
        _repetition: u32,
    ) -> Result<Value, CalcError> {
        Err(CalcError::FIELD_MISSING)
    }

    /// A field read from `context`, retaining the parent of a related or
    /// portal read. Summary fields use that related set; ordinary fields
    /// read the selected `record` just as [`Self::field_value`] does, which
    /// the default reads for every field.
    fn field_value_in_context(
        &self,
        _context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, CalcError> {
        self.field_value(occurrence, record, field, repetition)
    }

    /// Whether unstored calculation `field` of `occurrence` is evaluated
    /// when every field it references is empty, its "Do not evaluate if
    /// all referenced fields are empty" option clear (field option `0x20`
    /// set). When not, a record on which every field the formula actually
    /// read was empty (containers aside) gets the empty value, unless the
    /// formula ran a field inspection function such as `IsEmpty`; fields of
    /// unchosen branches are not read (calc-values.md 8.8). The default
    /// evaluates always.
    fn evaluate_always(&self, _occurrence: u64, _field: u64) -> bool {
        true
    }

    /// The record from which a relationship is followed. A portal can pin
    /// an intermediate occurrence on that path to its displayed row. The
    /// host knows the graph; the default keeps the layout's parent record.
    /// Aggregates of the portal's own occurrence still use its parent's
    /// complete related set, not only the displayed row.
    fn relationship_origin(&self, context: &Context, _related: u64) -> Option<(u64, u64)> {
        context.occurrence.zip(context.record)
    }

    /// The records of `related` related to `record` of `occurrence` through
    /// the relationship graph, in the relationship's sort order (creation
    /// order when unsorted; calc-values.md 8.7). An occurrence with no path
    /// is error 103.
    fn related_records(
        &self,
        _occurrence: u64,
        _record: u64,
        _related: u64,
    ) -> Result<Vec<u64>, CalcError> {
        Err(CalcError::from_code(103).expect("nonzero"))
    }

    /// The related record a direct reference `Related::Field` reads from
    /// record `record` of `occurrence`: the first related record
    /// ([`Self::related_record`] 0). A host whose window has an active
    /// portal row on that relationship answers that row's record instead
    /// (`Some(None)` from the host's own check means its empty creation
    /// row); `GetNthRecord` and aggregates read the related set itself.
    fn referenced_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Option<u64>, CalcError> {
        self.related_record(occurrence, record, related, 0)
    }

    /// The related record at `index` (0-based) of [`Self::related_records`];
    /// hosts with large related sets can answer without building the list.
    fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, CalcError> {
        Ok(self
            .related_records(occurrence, record, related)?
            .get(index)
            .copied())
    }

    /// The record at `index` (0-based) of the found set of `occurrence`
    /// (`GetNthRecord` on a field of the context table). On a bare record
    /// cursor FileMaker uses the table's records in creation order.
    fn found_record(&self, _occurrence: u64, _index: usize) -> Result<Option<u64>, CalcError> {
        Ok(None)
    }

    /// The record `Lookup` (`direction` `None`) or `LookupNext` copies from:
    /// by default the first related record. `LookupNext` falls back to the
    /// next lower or higher match field value when nothing is related,
    /// which needs the host's index.
    fn lookup_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        _direction: Option<LookupDirection>,
    ) -> Result<Option<u64>, CalcError> {
        self.related_record(occurrence, record, related, 0)
    }

    /// `GetSummary ( summary ; break )` for the context's record.
    fn summary(
        &self,
        _context: &Context,
        _summary: FieldId,
        _break_field: FieldId,
    ) -> Result<Value, CalcError> {
        Err(CalcError::NOT_SUPPORTED)
    }

    /// Data the host does not hold yet and reads as missing while it fetches
    /// it, for an evaluation that runs again once it arrives (a window that
    /// paints without waiting for its provider). Once an evaluation has read
    /// such data, a custom function call or `While` iteration with no new
    /// need since the previous one ends it with error 1224: its result is
    /// discarded, and recursing to the limit over missing values would only
    /// delay the fetch. The default, for a host that waits for its data or
    /// whose missing data stays missing, is none.
    fn pending(&self) -> Pending {
        Pending::default()
    }

    /// Whether the evaluation must stop now: the script it runs for was
    /// aborted or ran out of time, or the server is shutting down. The
    /// evaluator asks every few custom function calls, `While` iterations,
    /// `Evaluate`s and unstored calculations, and functions whose work
    /// grows faster than their input ask as they go ([`crate::Work`]);
    /// `true` ends the evaluation with error 1 ([`CalcError::CANCELED`]),
    /// its result `?`, which `EvaluationError` does not catch. Once `true`
    /// it should stay so. The default never stops: FileMaker has no time
    /// limit on a calculation (script-runtime.md 2.6), this is OpenMaker's
    /// own.
    fn interrupted(&self) -> bool {
        false
    }

    /// How much one evaluation may build. The default is
    /// [`CalcLimits::DEFAULT`]; OpenMaker Server gives its scripts a smaller
    /// text limit (OpenMaker's choice, so that one script cannot take the
    /// server's memory).
    fn limits(&self) -> CalcLimits {
        CalcLimits::DEFAULT
    }

    /// The base table name of an occurrence (`GetBaseTableName`).
    fn base_table_name(&self, _occurrence: u64) -> Option<&str> {
        None
    }

    /// The base table name `GetBaseTableName` answers: by default
    /// [`Self::base_table_name`]. A host whose schema can grow while a
    /// formula runs (a related file opened on demand) answers from the
    /// grown one.
    fn table_name(&self, occurrence: u64) -> Option<String> {
        self.base_table_name(occurrence).map(str::to_owned)
    }

    /// `Occurrence::Field` as `GetFieldName` answers it in `context`: by
    /// default from [`Self::names_for`]. A host whose schema can grow while
    /// a formula runs answers from the grown one.
    fn qualified_field_name(
        &self,
        context: &Context,
        occurrence: u64,
        field: u64,
    ) -> Option<String> {
        qualified_field_name(self.names_for(context), occurrence, field)
    }

    /// `Get ( selector )` (selector numbers of `get-selectors.csv`), except
    /// `CalculationRepetitionNumber`, which the evaluator answers. Script
    /// selectors (`ScriptParameter`, `LastError`, ...) come from the script
    /// engine. The default is empty text, as for selectors with no window,
    /// layout or script on the server.
    fn get(&mut self, _selector: u8, _context: &Context) -> Result<Value, CalcError> {
        Ok(Value::empty_text())
    }

    /// The `$` variables of the running script frame, or the file's `$$`
    /// variables (calc-values.md 8.3).
    fn variables(&mut self, scope: VarScope) -> &mut Variables;

    /// The key [`Self::variables`] stores variable `name` under for a
    /// formula solved in `context`. `$$` variables belong to the file that
    /// owns the formula (script-runtime.md 5.1), so a host keeping several
    /// files' `$$` variables in one map gives each file's names a key of
    /// their own. The default is `name` itself.
    fn variable_name<'n>(&self, name: &'n str, _context: &Context) -> Cow<'n, str> {
        Cow::Borrowed(name)
    }

    /// The custom function stored under `id`.
    fn custom_function(&self, _id: u64) -> Option<Arc<CustomFunction>> {
        None
    }

    /// `Random`: a double in [0, 1). The default is FileMaker's generator
    /// with one state shared by the process (calc-functions.md 3.9).
    fn random(&mut self) -> f64 {
        shared_random()
    }

    /// The stored data of a container the calculation holds only by its
    /// reference (a container field of a hosted file, whose record holds
    /// the key of a member of the table's library, containers.md 1.4;
    /// [`Container::library`]), which the functions that read container
    /// data read (`om_calc::functions::reads_container_data`). A host that
    /// keeps no container data answers error 1225, the default.
    fn container_data(
        &mut self,
        _container: &Container,
        _context: &Context,
    ) -> Result<StoredContainer, CalcError> {
        Err(CalcError::NOT_SUPPORTED)
    }

    /// Functions the host answers: `ExecuteSQL`, layout objects, Design,
    /// plug-ins and whatever the library lacks. The default answers
    /// `GetLayoutObjectAttribute` with empty text (no layout), `GetLiveText`
    /// and `GetLiveTextAsJSON` with error 3 (FileMaker Server: text
    /// recognition is the Apple platforms' Vision framework) and everything
    /// else with error 1225.
    fn call(
        &mut self,
        function: HostFunction<'_>,
        _args: &[Value],
        _context: &Context,
    ) -> Result<Value, CalcError> {
        default_call(function)
    }

    /// [`Self::call`] for functions whose value stands beside an error the
    /// evaluation reports: `ExecuteSQLe` returns the text `? ERROR: ...`
    /// and its error code together (execute-sql.md 1.2). The evaluator
    /// calls this; the default answers through [`Self::call`], an error
    /// giving `?`.
    fn call_reporting(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> (Value, Option<CalcError>) {
        match self.call(function, args, context) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

/// [`CalcHost::call`]'s default.
pub fn default_call(function: HostFunction<'_>) -> Result<Value, CalcError> {
    match function {
        HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectAttribute") => {
            Ok(Value::empty_text())
        }
        HostFunction::Named(name)
            if name.eq_ignore_ascii_case("GetLiveText")
                || name.eq_ignore_ascii_case("GetLiveTextAsJSON") =>
        {
            Err(CalcError::from_code(3).expect("nonzero"))
        }
        _ => Err(CalcError::NOT_SUPPORTED),
    }
}

/// FileMaker's random number generator (`DBDataArg::Random`,
/// calc-functions.md 3.9): a 64-bit linear congruential state advanced twice
/// per number, the result built from 53 bits of the two states.
#[derive(Clone, Copy, Debug)]
pub struct Lcg(pub u64);

impl Lcg {
    fn step(s: u64) -> u64 {
        s.wrapping_mul(0x045C_1E2F_F69E_A0FD).wrapping_add(0xC90F7)
    }

    fn value(s1: u64, s2: u64) -> f64 {
        let bits = (s1 & 0x001F_FFFF_0000_0000) | (s2 >> 32);
        bits as f64 / (1u64 << 53) as f64
    }

    /// The next number in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        let s1 = Lcg::step(self.0);
        let s2 = Lcg::step(s1);
        self.0 = s2;
        Lcg::value(s1, s2)
    }
}

static SHARED_SEED: AtomicU64 = AtomicU64::new(0);

/// The process-wide generator behind the default [`CalcHost::random`],
/// seeded from the clock on first use.
pub fn shared_random() -> f64 {
    let mut current = SHARED_SEED.load(Ordering::Relaxed);
    loop {
        let start = if current == 0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64 | 1)
        } else {
            current
        };
        let mut lcg = Lcg(start);
        let value = lcg.next_f64();
        match SHARED_SEED.compare_exchange_weak(
            current,
            lcg.0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return value,
            Err(seen) => current = seen,
        }
    }
}

/// A host with no file behind it: a locale, a date and variable maps.
/// Formulas that reference fields, records or custom functions fail as
/// they do without a table context.
#[derive(Clone, Debug)]
pub struct NullHost {
    pub locale: Locale,
    pub today: Date,
    pub locals: Variables,
    pub globals: Variables,
}

impl NullHost {
    pub fn new(locale: Locale, today: Date) -> NullHost {
        NullHost {
            locale,
            today,
            locals: Variables::new(),
            globals: Variables::new(),
        }
    }
}

impl CalcHost for NullHost {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn today(&self) -> Date {
        self.today
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        match scope {
            VarScope::Local => &mut self.locals,
            VarScope::Global => &mut self.globals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_numbers_are_in_range_with_53_bits() {
        let mut lcg = Lcg(12345);
        for _ in 0..1000 {
            let r = lcg.next_f64();
            assert!((0.0..1.0).contains(&r));
            assert_eq!((r * (1u64 << 53) as f64).fract(), 0.0);
        }
        let r = shared_random();
        assert!((0.0..1.0).contains(&r));
    }
}
