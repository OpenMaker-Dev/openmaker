//! Record rules decoded from a file's schema (auto-entry.md,
//! record-writes.md): auto-entered values, stored calculations recomputed
//! when a field they reference changes, and validation, with calculations
//! evaluated by `om-calc` on the record being written. The desktop client
//! applies them to what the user types; OpenMaker Server to what its
//! scripts write ([`Entry`]).
//!
//! A new record's serial numbers are reserved by the host before the write
//! is computed ([`RecordPlan::serials_for`], [`RecordEnvironment::serials`]).
//! Unknown definitions or expressions are rejected before allocating or locking
//! a record: lookups other than the exact-match form FileMaker Pro
//! was seen to write (their related values are read by the host first,
//! [`RecordPlan::read_lookups`]), entry from the previous record, unknown option
//! or validation bits, most repeating calculations, calculations that read
//! globals, summaries, unstored calculations or related fields,
//! plug-in functions, custom functions that read fields, `Get` selectors
//! that need a window, and cyclic dependencies. A calculation the host cannot
//! answer refuses the write rather than storing a guess. This module plans
//! field values; it does not send protocol requests or write records.
//! Refusals carry the FileMaker error a script step reports
//! ([`DataError::code`]).
mod global_text_self;
pub use global_text_self::global_text_assignment;
pub mod recalc;
pub mod schema;
mod typing;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use om_calc::eval::{
    display, evaluate_formula_with_calculation_budget, CalcLimits, Context, Lcg, Pending, Variables,
};
use om_calc::ops::{self, CompareOp};
use om_calc::{Ctx, Date, Locale, Number, Time, Timestamp, Value, ValueType};
use om_collate::DateOrder;
use om_formula::{DecodeErrorKind, Expr, Formula, Key, Opcode};
use om_hbam::catalog::{DataType, Field, FieldKind};
use om_hbam::{EntryVariable, FieldOptions, KeyTree, Node, Table};

use self::global_text_self::GlobalTextSelf;
use crate::containers::{self, MemberInfo};
use crate::meta::{ValueList, ValueSource};

use crate::host::session::selector;
use crate::host::{
    stored_value, CalcSchema, CalcSource, FileCalc, FileHost, MemorySource, RecordValues,
    SessionInfo,
};
use crate::DataError;

/// The option bits that only describe a field's indexes: the value and word
/// index and their relaxed forms, and automatic indexing turned off
/// ("prevent index" `0x40000000`, E38) (schema-path.md 5.5; auto-entry.md,
/// Index bits). They say nothing about entry or calculation. Whether a field
/// has an index is its own bit's (`0x80000000`), never inferred from these.
pub(crate) const INDEX_OPTIONS: u32 = 0xf800_0000;
/// Dependency and storage bookkeeping FileMaker keeps on fields, which a
/// client write does not act on: computed on the client `0x40000`, has pure
/// dependency `0x80000`, primary key `0x100000`, has no dependents
/// `0x200000`, no style runs `0x400000` (schema-path.md 5.5).
pub(crate) const BOOKKEEPING_OPTIONS: u32 = 0x007c_0000;
/// Auto-entry: a creation or modification value, a serial number generated
/// on creation (`0x2`) or on commit (`0x200`), a constant, a calculation;
/// replacing an existing value; prohibiting modification.
const AUTO_ENTRY_OPTIONS: u32 = 0x38f;
/// A serial number generated when the record is committed, set instead of
/// `0x2` (schema-path.md 5.9, E41).
const SERIAL_ON_COMMIT: u32 = 0x200;
/// Auto-entry sources (schema-path.md 5.9, E40). The planner rejects
/// multiple sources except for Creation Date with a calculated value.
const AUTO_ENTRY_MODES: u32 = 0x20f;
/// Auto-enter options measured on global Text fields (#987): a constant,
/// a calculation or a serial generated on creation.
const GLOBAL_AUTO_ENTRY_MODES: u32 = 0x00e;
/// "Do not evaluate if all referenced fields are empty" is off.
const ALWAYS_EVALUATE: u32 = 0x20;
const GLOBAL: u32 = 0x1_0000;
const UNSTORED: u32 = 0x2_0000;
/// Auto-enter a looked-up value (`08`, schema-path.md 5.5).
const LOOKUP: u32 = 0x400;
/// "Use Furigana Field" on a normal field ([`PlannedField::furigana`]).
const FURIGANA: u32 = 0x40;

/// Validation bits (`mValidationSet`, schema-path.md 5.5).
const VALIDATE_BY_CALCULATION: u32 = 0x1;
/// "Validate only if field has been modified" off (XML `alwaysValidate`):
/// the calculation also checks values the write did not change.
const VALIDATE_CALCULATION_ALWAYS: u32 = 0x2;
const VALIDATE_EXISTING: u32 = 0x20;
const VALIDATE_RANGE: u32 = 0x40;
const VALIDATE_MESSAGE: u32 = 0x80;
const VALIDATE_VALUE_LIST: u32 = 0x100;
const VALIDATE_MAX_CHARACTERS: u32 = 0x200;
const VALIDATE_ALWAYS: u32 = 0x400;
/// "Strict data type": numeric only, 4-digit year date, time of day (XML
/// `<Strict>Numeric</Strict>`, `FourDigitYear`, `Time`).
const VALIDATE_NUMERIC: u32 = 0x1000;
const VALIDATE_FOUR_DIGIT_YEAR: u32 = 0x2000;
const VALIDATE_TIME_OF_DAY: u32 = 0x4000;
/// The custom message is a calculation, member `20` (with `0x80`; how
/// FileMaker Pro 26 writes every custom message, schema-path.md 5.9 E34).
const VALIDATE_MESSAGE_CALCULATION: u32 = 0x8000;
/// Strict, not empty and unique, and the rules above.
const SUPPORTED_VALIDATION: u32 = VALIDATE_BY_CALCULATION
    | VALIDATE_CALCULATION_ALWAYS
    | 0x4
    | 0x8
    | 0x10
    | VALIDATE_EXISTING
    | VALIDATE_RANGE
    | VALIDATE_MESSAGE
    | VALIDATE_VALUE_LIST
    | VALIDATE_MAX_CHARACTERS
    | VALIDATE_ALWAYS
    | VALIDATE_NUMERIC
    | VALIDATE_FOUR_DIGIT_YEAR
    | VALIDATE_TIME_OF_DAY
    | VALIDATE_MESSAGE_CALCULATION;

/// Keys under a field definition `[T, 3, 5, F]` (schema-path.md 5.5; the
/// value list reference and the range as files FMUpgradeTool generated
/// store them, script-runtime.md 13.21).
const FIELD_FORMULA: u64 = 5;
const FIELD_VALUE_LIST: u64 = 0x07;
const FIELD_LOOKUP: u64 = 0x08;
const FIELD_LOOKUP_FALLBACK: u64 = 0x0b;
const FIELD_CONSTANT: u64 = 0x0d;
/// A child FileMaker Pro 26 left beside one lookup definition, `00000000`;
/// its meaning is unknown (schema-path.md 5.5).
const FIELD_LOOKUP_UNKNOWN: u64 = 0x26;
const FIELD_MESSAGE: u64 = 0x18;
const FIELD_VALIDATION: u64 = 0x19;
const FIELD_MESSAGE_FORMULA: u64 = 0x20;
const FIELD_RANGE_FROM: u64 = 0x21;
const FIELD_RANGE_TO: u64 = 0x22;

/// The value list catalog `[21, 5, V]` (file-metadata.md 6).
const VALUE_LISTS: [u64; 2] = [0x21, 5];

/// `Get` selectors a record rule may read: the clock, the account and user,
/// the owning file name, a new UUID, the application/platform and repetition. Others
/// describe a window or record this planner does not have, except
/// [`CONTEXT_SELECTORS`] in an auto-entered or validation formula.
const RECORD_RULE_SELECTORS: [u8; 10] = [1, 2, 5, 16, 29, 31, 32, 63, 68, 101];

/// The `Get` selectors whose answer comes from the write's operation
/// ([`WriteContext`]), not from the session's snapshot: read only by
/// auto-entered and validation formulas, which the writing client
/// evaluates. A stored calculation, which a host recalculates with no
/// operation, and a privilege set's record limit keep refusing them.
const CONTEXT_SELECTORS: [u8; 5] = [
    selector::LAYOUT_NUMBER,
    selector::RECORD_NUMBER,
    selector::CURRENT_PRIVILEGE_SET_NAME,
    selector::CURRENT_HOST_TIMESTAMP,
    selector::CURRENT_TIME_UTC_MILLISECONDS,
];

/// Whether FileMaker leaves index language `language` untailored, so that
/// its value-index keys are English's primary weights (query-program.md
/// 7.2): the languages outside the list of those whose `TailorWeights`
/// Support overrides (from bin) whose keys the probe found equal to
/// English's for every value. These are Dutch 20, French 24, German
/// dictionary 26 (`ä` keys as `a`; German, 25, is tailored), Greek 27,
/// Italian 30, Portuguese 34, Russian 36, Hebrew 47, Serbian 53,
/// Bulgarian 55 and the Indic languages 59 to 67 (Hindi, 48, is tailored).
/// Several of them break words differently from English for some of the
/// probe's values (French at an apostrophe, query-program.md 7.1; the word
/// columns of catalogs/index-key-vectors.tsv), and their full rules are not
/// established ([#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)),
/// so only their value keys are known. That the bin's list is complete is
/// not confirmed: Norwegian and Finnish are tailored though absent from
/// it, so this set is inferred.
pub fn untailored(language: u32) -> bool {
    matches!(
        language,
        20 | 24 | 26 | 27 | 30 | 34 | 36 | 47 | 53 | 55 | 59..=67
    )
}

/// How a value index in `language`, in a file whose own language is
/// `file_language`, keys text when it is read without the host: a unique
/// or existing value checked in it, a find or a join on it without the
/// host's index. The form a host writes ([`written_text_keys`]), and
/// English for the untailored languages ([`untailored`]), whose value
/// keys are English's. `None` for language 0 when the file's language is
/// unknown (`None`, its locale record unreadable,
/// [`crate::host::CalcSchema::file_language`]), and for the tailored
/// languages, whose keys
/// OpenMaker does not build (#159,
/// [#299](https://github.com/OpenMaker-Dev/openmaker/issues/299)): a field
/// validated in them keeps its table read-only.
pub fn value_text_keys(
    language: u32,
    file_language: Option<u32>,
) -> Option<om_query::keys::TextKeys> {
    let language = if language == 0 {
        file_language?
    } else {
        language
    };
    language_text_keys(language)
        .or_else(|| untailored(language).then_some(om_query::keys::TextKeys::Collated))
}

/// How a host writes the keys of a text field indexed in `language`, in a
/// file whose own language is `file_language` (query-program.md 7.2):
/// language 0 is the file's own; English and English international as
/// primary weights, System and Unicode standard padded, Unicode raw as its
/// code units. `None` for the other languages, whose forms OpenMaker does
/// not build (the tailorings, #1279; those FileMaker leaves untailored,
/// #1280): a write that would change such an index is refused rather than
/// keyed in English (#159).
pub fn written_text_keys(language: u32, file_language: u32) -> Option<om_query::keys::TextKeys> {
    language_text_keys(if language == 0 {
        file_language
    } else {
        language
    })
}

/// The form a host writes text in for `language`, language 0 already
/// resolved to the file's own ([`written_text_keys`]).
fn language_text_keys(language: u32) -> Option<om_query::keys::TextKeys> {
    om_query::keys::TextKeys::of_language(language)
        .filter(|form| *form != om_query::keys::TextKeys::None)
}

/// How a find or a lookup reads the index of a text field indexed in
/// `language`, in a file whose own language is `file_language`: the form a
/// host writes ([`written_text_keys`]), so that a find on a field indexed
/// in Unicode raw or Unicode standard meets the keys FileMaker wrote.
/// A language whose form OpenMaker does not build is read in English,
/// which keys most text as those languages do; nothing read so is written.
pub fn read_text_keys(language: u32, file_language: u32) -> om_query::keys::TextKeys {
    written_text_keys(language, file_language).unwrap_or(om_query::keys::TextKeys::Collated)
}

/// Nesting, size and stored length of the calculations record writes
/// evaluate.
const MAX_DEPTH: usize = 256;
const MAX_NODES: usize = 100_000;
const MAX_CODE: usize = 256 * 1024;

/// FileMaker errors of refused writes (catalogs/error-codes.csv).
pub mod error {
    /// A rule the planner cannot evaluate, or a calculation that failed.
    pub const UNAVAILABLE: i32 = 3;
    pub const FIELD_MISSING: i32 = 102;
    /// "Field cannot be modified".
    pub const FIELD_LOCKED: i32 = 201;
    /// "Date value does not meet validation entry options" (4-digit year).
    pub const DATE: i32 = 500;
    /// "Time value does not meet validation entry options" (time of day).
    pub const TIME: i32 = 501;
    /// "Number value does not meet validation entry options" (numeric
    /// only).
    pub const NUMBER: i32 = 502;
    /// "Value in field is not within the range specified in validation
    /// entry options".
    pub const RANGE: i32 = 503;
    /// "Value in field is not unique, as required in validation entry
    /// options".
    pub const NOT_UNIQUE: i32 = 504;
    /// "Value in field is not an existing value in the database file, as
    /// required in validation entry options".
    pub const NOT_EXISTING: i32 = 505;
    /// "Value in field is not listed on the value list specified in
    /// validation entry option".
    pub const NOT_IN_LIST: i32 = 506;
    /// "Value in field failed calculation test of validation entry option".
    pub const VALIDATION_CALCULATION: i32 = 507;
    /// "Field requires a valid value" (not empty).
    pub const REQUIRED: i32 = 509;
    /// "Value in field exceeds maximum field size" (maximum number of
    /// characters; Set Field's own error on FileMaker Server, script
    /// oracle s17).
    pub const TOO_LONG: i32 = 511;
    /// "No validation was specified but data cannot fit into the field".
    pub const CANNOT_FIT: i32 = 513;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Create,
    Update,
}

/// Who writes a record and with which options; that decides which rules
/// apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry<'a> {
    /// A script step (Set Field, ...) rather than a user typing: any normal
    /// field of a supported type may be set, also one whose auto-entered
    /// value prohibits modification during data entry (scripts ignore that
    /// option, as the Data API's `prohibitMode` "script" says).
    pub script: bool,
    /// Commit Records' or Open Transaction's "Skip data entry validation":
    /// only fields validated Always are checked (`commit-records-requests.html`).
    pub skip_data_entry_validation: bool,
    /// Open Transaction's "Skip auto-enter options".
    pub skip_auto_enter: bool,
    /// Auto-enter never replaces a value the write itself sets: Import
    /// Records' mapped fields keep the imported value, empty or not, also
    /// where an auto-entered value would replace an existing one (oracle
    /// x02).
    pub keep_written: bool,
    /// Cells this import maps when `changes` also contains earlier staged
    /// values. By default all cells in `changes` are explicitly set.
    pub mapped_cells: Option<&'a BTreeSet<u64>>,
}

impl Entry<'_> {
    /// A script's write with the given Skip options.
    pub fn script(skip_data_entry_validation: bool, skip_auto_enter: bool) -> Self {
        Entry {
            script: true,
            skip_data_entry_validation,
            skip_auto_enter,
            keep_written: false,
            mapped_cells: None,
        }
    }

    /// An imported row (oracle x02, script-steps.md "Implementation notes
    /// (import/export)"): any normal field may be set, "only during data
    /// entry" validation is not checked, auto-enter applies only with
    /// "Perform auto-enter options" and then only to the fields the row
    /// does not set.
    pub fn import(auto_enter: bool) -> Self {
        Entry {
            script: true,
            skip_data_entry_validation: true,
            skip_auto_enter: !auto_enter,
            keep_written: true,
            mapped_cells: None,
        }
    }

    /// Whether auto-enter leaves `cell`, which the write sets in `changes`,
    /// as written ([`Entry::keep_written`]).
    fn keeps(&self, changes: &BTreeMap<u64, Option<String>>, cell: u64) -> bool {
        self.keep_written && self.sets(changes, cell)
    }

    fn sets(&self, changes: &BTreeMap<u64, Option<String>>, cell: u64) -> bool {
        self.mapped_cells.map_or_else(
            || changes.contains_key(&cell),
            |cells| cells.contains(&cell),
        )
    }
}

/// The layout a write goes through, for `Get ( LayoutNumber )` in its
/// formulas (calc-functions.md 6.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteLayout {
    /// Not supplied: a write whose formula reads the selector is refused.
    #[default]
    Unknown,
    /// A calculation of the file with no window (a host's recalculation):
    /// FileMaker answers empty.
    NoWindow,
    /// The layout's number among the file's listed layouts, from 1, as the
    /// window answers it; 0 for one the account's privilege set hides
    /// (session-identity.md 2.5).
    Number(u64),
}

/// The source of a write's `Get ( RecordNumber )` answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordNumber {
    /// A window position for auto-entry or validation, such as an
    /// unsorted portal creation row or a duplicate.
    Window(u64),
    /// The position at ordinary New Record in an unsorted window of a
    /// local file. A stored Number calculation can also use this answer.
    LocalNewRecord(u64),
    /// Ordinary local New Record in a sorted found set. The held row
    /// follows the current row before any later re-sort.
    LocalSortedNewRecord { position: u64, after: u64 },
}

impl RecordNumber {
    pub fn value(self) -> u64 {
        match self {
            Self::Window(number) | Self::LocalNewRecord(number) => number,
            Self::LocalSortedNewRecord { position, .. } => position,
        }
    }
}

/// The UTC millisecond samples a new record's formulas have drawn. Clones
/// identify the same creation so a preview and its commit replay the
/// calls in their field's evaluation order.
#[derive(Clone, Debug, Default)]
pub struct CreationUtcClock(Arc<Mutex<BTreeMap<u64, Vec<Value>>>>);

impl PartialEq for CreationUtcClock {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CreationUtcClock {}

impl CreationUtcClock {
    fn sample(&self, field: u64, call: usize) -> Result<Value, om_calc::CalcError> {
        // A loop or recursion cannot keep growing the record's sample
        // log. Refuse large sequences rather than replay a partial one.
        const MAX_CLOCK_CALLS: usize = 64;
        if call >= MAX_CLOCK_CALLS {
            return Err(om_calc::CalcError::NOT_SUPPORTED);
        }
        let mut by_field = self
            .0
            .lock()
            .map_err(|_| om_calc::CalcError::NOT_SUPPORTED)?;
        let samples = by_field.entry(field).or_default();
        if let Some(value) = samples.get(call) {
            return Ok(value.clone());
        }
        let value = crate::host::session::utc_milliseconds()?;
        samples.push(value.clone());
        Ok(value)
    }
}

/// How much the writer can establish about the order of local edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteSequence {
    /// A host, routed write, or other writer without a measured sequence.
    #[default]
    Unmeasured,
    /// A write through a layout of the opened local file.
    Local,
    /// A single field-setting step on an otherwise unchanged local row.
    SingleLocalFieldStep,
}

/// The owning local file's script variables at the start of a write.
#[derive(Clone)]
pub struct WriteFileVariables(Arc<Variables>);

impl WriteFileVariables {
    pub fn new(variables: Arc<Variables>) -> Self {
        Self(variables)
    }
}

impl std::fmt::Debug for WriteFileVariables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteFileVariables").finish_non_exhaustive()
    }
}

impl PartialEq for WriteFileVariables {
    fn eq(&self, other: &Self) -> bool {
        self.0.revision() == other.0.revision()
    }
}

impl Eq for WriteFileVariables {}

/// Local-session Text globals read from other tables by auto-entry.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct OtherGlobalValues(Arc<BTreeMap<(u64, u64), String>>);

impl std::fmt::Debug for OtherGlobalValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtherGlobalValues").finish_non_exhaustive()
    }
}

/// What the operation making a write knows that its formulas may read
/// ([`CONTEXT_SELECTORS`]). A value left out refuses a write whose
/// evaluated formula reads it; it is never answered empty in its place.
/// The operation supplies only what [`RecordPlan::context_reads`] names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteContext {
    /// Evidence for auto-entry whose result depends on entry order.
    pub sequence: WriteSequence,
    /// `Get ( LayoutNumber )`.
    pub layout: WriteLayout,
    /// `Get ( CurrentPrivilegeSetName )`: the session's current set, the
    /// account's or [Full Access] in a step of a script running with full
    /// access.
    pub privilege_set: Option<String>,
    /// `Get ( CurrentHostTimestamp )`: the clock of the host serving the
    /// file, in the storage form of [`RecordEnvironment::timestamp`].
    pub host_timestamp: Option<String>,
    /// `Get ( RecordNumber )` as the operation answers it. A stored
    /// calculation requires [`RecordNumber::LocalNewRecord`].
    pub record_number: Option<RecordNumber>,
    /// The clock samples of a local New Record, retained by its preview.
    pub creation_utc_clock: Option<CreationUtcClock>,
    /// The local file's `$$` variables for directly reading a Text
    /// auto-enter value. New Record's preview retains this snapshot.
    pub file_variables: Option<WriteFileVariables>,
    /// The local session's globals when the write started. A held new
    /// record reuses its first snapshot through subsequent edits.
    pub other_globals: Option<OtherGlobalValues>,
}

impl WriteContext {
    /// A host's clock reading, op82's date-time key (schema-path.md 3.1),
    /// in the form [`WriteContext::host_timestamp`] holds; `None` for bytes
    /// that are not such a key.
    pub fn host_timestamp_of(key: &[u8]) -> Option<String> {
        let (date, time) = crate::new_file::clock_text(key, &Locale::en_us())?;
        Some(format!("{date} {time}"))
    }
}

/// One snapshot supplied by the session. Dates/times use the hosted file's
/// storage representation. Keep account data out of debug output.
#[derive(Clone)]
pub struct RecordEnvironment {
    pub account_name: String,
    pub user_name: String,
    pub date: String,
    pub time: String,
    pub timestamp: String,
    pub uuid: String,
    /// What calculations read of container fields; without one a
    /// container field reads as the text its cell holds.
    pub containers: Option<Arc<dyn ContainerReader + Send + Sync>>,
    /// Whether the write's calculations must stop
    /// (`om_calc::eval::CalcHost::interrupted`): the server script making
    /// the write was aborted or ran out of time. A stopped calculation
    /// fails, which refuses the write. `None`: nothing stops them.
    pub interrupted: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// What the write's calculations may build
    /// (`om_calc::eval::CalcHost::limits`): the limits of the server script
    /// making the write, else [`CalcLimits::DEFAULT`]. A calculation past
    /// them fails, which refuses the write.
    pub limits: CalcLimits,
    /// What the write's lookups copy, read from the host's records before
    /// the write is computed ([`RecordPlan::read_lookups`]). A lookup the
    /// write fires without its value here refuses the write.
    pub lookups: Lookups,
    /// The serial numbers the host reserved for a new record, by field
    /// ([`RecordPlan::serials_for`]): a new record whose serial is missing
    /// here is refused.
    pub serials: BTreeMap<u64, String>,
    /// The session's values of the table's global fields, laid out like a
    /// record: what an auto-entered or validation formula reads of a global
    /// field ([`RecordPlan::reads_globals`], schema-path.md 5.17). A write
    /// whose formula reads one without them here is refused.
    pub globals: Option<Arc<RecordValues>>,
    /// What the write's operation supplies for the `Get` selectors of its
    /// layout, privilege set and host clock.
    pub context: WriteContext,
    /// The records the write's open transaction holds, which its lookups
    /// and related reads see over the stored ones ([`HeldSource`]): a
    /// parent record made in the same transaction, its serial key among
    /// its cells, is found, not read as missing. `None` outside a
    /// transaction.
    pub held: Option<Arc<HeldRecords>>,
    /// Tables of which the write's commit holds records besides the written
    /// one, for a host whose own reads already see them and that gives no
    /// [`Self::held`] (OpenMaker Server's transaction): a self-join's
    /// answer that depends on which records are committed is then refused,
    /// as with held records of that table.
    pub others_held: BTreeSet<u64>,
    /// Where a formula records a related read it needs again with a match
    /// key the write itself computes ([`RelatedNeeds`]); `None`: such a
    /// read refuses the write.
    pub related_needs: Option<Arc<RelatedNeeds>>,
    /// What the write's calculations read of the clock and the random
    /// source besides [`Self::date`], [`Self::time`] and
    /// [`Self::timestamp`]; `None`: the process's own (the system clock,
    /// `Random`'s generator shared by the process).
    pub sources: Option<WriteSources>,
}

/// What a write's calculations read of the world besides the clock
/// reading [`RecordEnvironment`] gives as text, supplied by the host so
/// that a write can be repeated with the same answers: OpenMaker Server
/// fills it from its engine's environment (docs/server-storage.md 5, 12).
/// Clones share the generator, so every formula of one write draws from
/// one sequence.
#[derive(Clone)]
pub struct WriteSources {
    /// The write's clock reading, the instant of
    /// [`RecordEnvironment::timestamp`], in microseconds since 1/1/1970
    /// UTC: its UTC date is today for the calculations' conversions (a
    /// year text leaves out, the two-digit year window, calc-values.md
    /// 4.6), as the system clock's is without it. Record rules answer no
    /// UTC clock selector (`RECORD_RULE_SELECTORS`).
    pub utc_micros: i128,
    /// The generator `Random` draws from: FileMaker's (calc-functions.md
    /// 3.9), with a state of its own.
    random: Arc<Mutex<Lcg>>,
    /// What a time budget of the write's calculations is measured on (a
    /// record limit's, `recalc::RecordLimit::allows`): the host's elapsed
    /// monotonic time, or a seeded environment's virtual time that advances
    /// by a fixed step at each reading.
    pub stopwatch: Stopwatch,
}

/// A clock a time budget is measured on: each call answers the time since
/// an origin of its own, never less than the call before.
pub type Stopwatch = Arc<dyn Fn() -> Duration + Send + Sync>;

impl WriteSources {
    /// The sources of a write whose clock reads `utc_micros`, whose `Random`
    /// sequence starts from `seed`, and whose budgets run on `stopwatch`.
    pub fn new(utc_micros: i128, seed: u64, stopwatch: Stopwatch) -> Self {
        WriteSources {
            utc_micros,
            random: Arc::new(Mutex::new(Lcg(seed))),
            stopwatch,
        }
    }
}

/// The related reads a write's formulas need with match keys the write
/// computes itself (#1035). Related records are read before the write is
/// computed, with the keys the write leaves then
/// ([`RecordPlan::read_lookups_with`]); a formula that follows a
/// relationship whose key its own auto-entry changed since records the
/// read here, and the host reads it again with that key and computes the
/// record again.
#[derive(Debug, Default)]
pub struct RelatedNeeds(std::sync::Mutex<BTreeMap<(u64, u64), Vec<String>>>);

impl RelatedNeeds {
    /// The reads needed since the last call, by (context, occurrence): the
    /// match keys to read with.
    pub fn take(&self) -> BTreeMap<(u64, u64), Vec<String>> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn need(&self, read: (u64, u64), keys: Vec<String>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(read, keys);
    }
}

/// The record a write computes, as a self-join of its table sees it (#1035):
/// its stored row is left out of the related records read (the write
/// replaces those values), and its place in creation order decides which
/// related record is first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Written {
    /// A record not stored yet, the newest of its table: `Some` the id the
    /// host reserved for it (a transaction's New Record), `None` before one
    /// is reserved.
    New(Option<u64>),
    /// A stored record.
    Stored(u64),
    /// Not said: no self-join of its table is answered.
    #[default]
    Unknown,
}

impl Written {
    /// A record a transaction holds, `record` its id: a new one's is the id
    /// the host reserved at New Record.
    pub fn held(mode: WriteMode, record: u64) -> Self {
        match mode {
            WriteMode::Create => Self::New(Some(record)),
            _ => Self::Stored(record),
        }
    }

    fn id(self) -> Option<u64> {
        match self {
            Self::New(id) => id,
            Self::Stored(id) => Some(id),
            Self::Unknown => None,
        }
    }
}

/// A field's auto-entered serial number ([`RecordPlan::serials_for`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Serial {
    pub field: u64,
    /// `mSerialIncrement`, at least 1.
    pub increment: i32,
    /// Generated when the record is committed (`0x200`), not when it is
    /// created (`0x2`).
    pub on_commit: bool,
}

/// What the lookups a write fires found ([`RecordPlan::read_lookups`]):
/// for each lookup field, the match values it was read for and the copied
/// field's value on the first related record, `None` when no record is
/// related.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookups {
    copied: BTreeMap<u64, (Vec<String>, Option<String>)>,
    /// For each lookup field that found a record, that record's id: whose
    /// value a preview's unknown values are checked on (#1063).
    copied_from: BTreeMap<u64, u64>,
    /// For each (context, occurrence) a formula reads related fields
    /// through: the match values it was read for, and the related records'
    /// ids with the fields read and matched on as text, in the
    /// relationship's order: the first only, or every one for a formula
    /// that reads them all ([`RelatedRead::whole`]); none when none is
    /// related.
    related: Arc<RelatedRecords>,
    /// Session globals read by a formula from another table in the file.
    other_globals: OtherGlobalValues,
    /// The record the reads were made for ([`Written`]).
    written: Written,
}

/// [`Lookups::related`].
type RelatedRecords = BTreeMap<(u64, u64), (Vec<String>, Vec<(u64, BTreeMap<u64, String>)>)>;

/// One occurrence a table's formulas read through from one context
/// ([`RecordPlan::related_reads`]).
struct RelatedRead {
    matches: Vec<u64>,
    fields: BTreeSet<u64>,
    /// A formula reads every related record, not only the first
    /// ([`Expression::whole`]).
    whole: bool,
}

/// How many related records a write reads for a formula that reads them
/// all (#1036): a larger related set refuses the write.
const MAX_WHOLE_READ: usize = 10_000;

/// Related records read for a write, with their values, in the
/// relationship's order ([`RecordPlan::related_records`]).
type ReadRecords = Vec<(u64, Arc<RecordValues>)>;

/// What record rules read of container fields (containers.md 1.4): stored
/// and auto-enter calculations see a container's data, as on FileMaker
/// Server's engine (script oracle s34-containers-calc), not the reference
/// its cell holds.
///
/// A cell naming a library member is read only when a calculation reads
/// the field, and then only the member's header ([`Self::member`]); its
/// streams only when a function reads them ([`Self::member_data`]). A
/// reader that downloads what it has not read yet answers it with error
/// 1224 and counts it in [`Self::pending`] instead of waiting: its caller
/// fetches what was asked for and runs the rules again, keeping only a run
/// that read nothing pending.
pub trait ContainerReader {
    /// The value of a container field of `table` whose cell holds `text`,
    /// data a host holds for a write not yet stored; `None`: the text
    /// alone. A cell naming a library member is read through
    /// [`Self::member`] instead.
    fn value(&self, table: u64, text: &str) -> Option<om_calc::Container>;
    /// The data of member `member` of `table`'s library.
    fn data(&self, table: u64, member: &[u8]) -> Option<om_calc::eval::StoredContainer>;

    /// The header of member `member` of `table`'s library (its file name
    /// and MD5); `None` when the library has no such member. By default
    /// the [`Self::value`] of its reference.
    fn member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, om_calc::CalcError> {
        let read = self.value(table, &containers::reference_text(member));
        Ok(read.and_then(|c| {
            let library = c
                .library
                .filter(|l| l.table == table && l.member == member)?;
            Some(Arc::new(MemberInfo {
                owner: library.owner,
                name: c.text.as_str().to_owned(),
                md5: library.md5,
                path_type: c.path_type.unwrap_or_default().to_owned(),
            }))
        }))
    }

    /// [`Self::data`], for a function that reads the member's streams.
    fn member_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<om_calc::eval::StoredContainer>>, om_calc::CalcError> {
        Ok(self.data(table, member).map(Arc::new))
    }

    /// The reads answered with 1224 so far ([`om_calc::eval::CalcHost::pending`]);
    /// none for a reader that has everything at hand.
    fn pending(&self) -> Pending {
        Pending::default()
    }
}

/// The repetition a formula is evaluated in, and how a field that does not
/// repeat reads there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InRepetition {
    /// Repetition n of a field's validation or calculated message: a field
    /// that does not repeat reads as it is, however the formula names it
    /// (script oracle s17, s19).
    Validation(u32),
    /// Repetition n of a calculation: a field that does not repeat reads
    /// empty above repetition 1 unless the formula extends it
    /// (calc-values.md 8.6). A field's own calculation is repetition 1.
    Calculation(u32),
}

/// Why a formula of a write has no result ([`RecordPlan::run`]).
enum RunError {
    /// It was not evaluated: the write lacks what it reads (an answer of
    /// its operation, a related record, the session's globals). The write
    /// is refused with 3, a validation calculation's too.
    Unread(DataError),
    /// It was evaluated and failed (a division by zero).
    Failed(DataError),
    /// It exhausted the record rule's unstored calculation allowance.
    Exhausted(DataError),
}

impl RunError {
    fn into_error(self) -> DataError {
        match self {
            Self::Unread(error) | Self::Failed(error) | Self::Exhausted(error) => error,
        }
    }
}

/// What a write's calculations read besides the record: `Get` answers and
/// container data.
pub(crate) struct Evaluation {
    info: SessionInfo,
    /// Today for conversions, when the write's sources fix it
    /// ([`Self::today`]).
    today: Option<Date>,
    /// [`WriteSources::random`]; `None`: the process's generator.
    random: Option<Arc<Mutex<Lcg>>>,
    containers: Option<Arc<dyn ContainerReader + Send + Sync>>,
    interrupted: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    limits: CalcLimits,
    /// The related records the write's formulas read ([`Lookups`]).
    related: Arc<RelatedRecords>,
    /// The session's global values ([`RecordEnvironment::globals`]).
    globals: Option<Arc<RecordValues>>,
    creation_utc_clock: Option<CreationUtcClock>,
    other_globals: OtherGlobalValues,
    /// `Get ( RecordNumber )` ([`WriteContext::record_number`]).
    record_number: Option<RecordNumber>,
    file_variables: Option<WriteFileVariables>,
    /// [`RecordEnvironment::related_needs`].
    related_needs: Option<Arc<RelatedNeeds>>,
    /// The written record ([`Lookups::written`]) and its stored row, empty
    /// for a new one: what a self-join's answer is checked against (#1035).
    written: Written,
    stored: BTreeMap<u64, String>,
    /// The records the commit holds ([`RecordEnvironment::held`]).
    held: Option<Arc<HeldRecords>>,
    /// [`RecordEnvironment::others_held`].
    others_held: BTreeSet<u64>,
    /// The computed fields whose values are final in the row being
    /// computed: those evaluated, or passed over, before the formula now
    /// evaluated, and not left out ([`RecordPlan::compute_inner`]).
    settled: BTreeSet<u64>,
    /// The [`CONTEXT_SELECTORS`] the operation could not answer.
    unanswered: BTreeSet<u8>,
    /// A host's recomputation of stored values with no session
    /// ([`super::host::FileHost::file_context`]), not a write.
    file_context: bool,
}

impl Evaluation {
    fn calculation_budget(&self) -> u64 {
        self.limits.calculations.min(MAX_UNSTORED_EVALUATIONS)
    }

    /// Today's date for a conversion of the write's calculations: the one
    /// its sources fix ([`RecordEnvironment::today`]), else the system
    /// clock's, read again at each use as without sources.
    fn today(&self) -> Date {
        self.today.unwrap_or_else(|| crate::host::session::today(0))
    }
}

impl RecordEnvironment {
    /// Today's date for the conversions of the write's calculations: the
    /// UTC date of the supplied clock ([`WriteSources::utc_micros`]), else
    /// of the system clock.
    pub(crate) fn today(&self) -> Date {
        match &self.sources {
            Some(sources) => crate::host::session::date_at(sources.utc_micros, 0),
            None => crate::host::session::today(0),
        }
    }

    /// What a time budget of the write's calculations is measured on: the
    /// supplied stopwatch ([`WriteSources::stopwatch`]), else one on the
    /// process's monotonic clock.
    pub(crate) fn stopwatch(&self) -> Stopwatch {
        match &self.sources {
            Some(sources) => Arc::clone(&sources.stopwatch),
            None => {
                let started = std::time::Instant::now();
                Arc::new(move || started.elapsed())
            }
        }
    }

    /// What `Get` answers while the record's values are computed: one
    /// clock reading and one UUID for the whole write. A supplied clock
    /// whose reading is not a FileMaker date answers the clock selectors
    /// with error 500, never with the system clock.
    pub(crate) fn session(&self) -> Evaluation {
        let locale = Locale::en_us();
        let today = self.today();
        let context = &self.context;
        // A host clock that does not read as a timestamp is no answer.
        let host_now = context
            .host_timestamp
            .as_deref()
            .and_then(|text| Timestamp::parse(text, &locale, today).ok().flatten());
        let now = Timestamp::parse(&self.timestamp, &locale, today)
            .ok()
            .flatten();
        let unanswered = CONTEXT_SELECTORS
            .into_iter()
            .filter(|selector| match *selector {
                selector::LAYOUT_NUMBER => context.layout == WriteLayout::Unknown,
                selector::RECORD_NUMBER => context.record_number.is_none(),
                selector::CURRENT_TIME_UTC_MILLISECONDS => context.creation_utc_clock.is_none(),
                selector::CURRENT_PRIVILEGE_SET_NAME => context.privilege_set.is_none(),
                _ => host_now.is_none(),
            })
            .collect();
        Evaluation {
            info: SessionInfo {
                account_name: self.account_name.clone(),
                user_name: self.user_name.clone(),
                now,
                now_unrepresentable: self.sources.is_some() && now.is_none(),
                host_now,
                uuid: Some(self.uuid.clone()),
                layout_number: match context.layout {
                    WriteLayout::Number(number) => Some(number),
                    WriteLayout::Unknown | WriteLayout::NoWindow => None,
                },
                privilege_set_name: context.privilege_set.clone().unwrap_or_default(),
                ..SessionInfo::for_this_machine()
            },
            today: self.sources.as_ref().map(|_| today),
            random: self
                .sources
                .as_ref()
                .map(|sources| Arc::clone(&sources.random)),
            containers: self.containers.clone(),
            interrupted: self.interrupted.clone(),
            limits: self.limits,
            related: self.lookups.related.clone(),
            globals: self.globals.clone(),
            other_globals: context
                .other_globals
                .clone()
                .unwrap_or_else(|| self.lookups.other_globals.clone()),
            record_number: context.record_number,
            creation_utc_clock: context.creation_utc_clock.clone(),
            file_variables: context.file_variables.clone(),
            related_needs: self.related_needs.clone(),
            written: self.lookups.written,
            stored: BTreeMap::new(),
            held: self.held.clone(),
            others_held: self.others_held.clone(),
            settled: BTreeSet::new(),
            unanswered,
            file_context: false,
        }
    }
}

/// A write's record over a [`MemorySource`], with container data from its
/// [`ContainerReader`] as calculations read it.
struct RuleSource<'a> {
    inner: MemorySource,
    /// The written record's table: its scans leave the written record (0)
    /// out, as a self-join's answer is given only where it cannot matter
    /// (`RecordPlan::answers_own`).
    own: u64,
    containers: Option<&'a (dyn ContainerReader + Send + Sync)>,
    /// Occurrences whose related records were read with keys the write has
    /// computed since (#1035): following one fails, and it is `followed`.
    blocked: BTreeSet<u64>,
    followed: std::cell::RefCell<BTreeSet<u64>>,
}

impl RuleSource<'_> {
    /// Error 3 for a relationship to a `blocked` occurrence, noted.
    fn follow(&self, to: u64) -> Result<(), om_calc::CalcError> {
        if self.blocked.contains(&to) {
            self.followed.borrow_mut().insert(to);
            return Err(om_calc::CalcError::from_code(3).expect("nonzero"));
        }
        Ok(())
    }
}

impl CalcSource for RuleSource<'_> {
    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<Arc<RecordValues>>, om_calc::CalcError> {
        self.inner.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
        let all = self.inner.records(table)?;
        if table != self.own {
            return Ok(all);
        }
        Ok(all.iter().copied().filter(|r| *r != 0).collect())
    }

    fn related(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, om_calc::CalcError> {
        self.follow(to)?;
        crate::host::related_by_scan(self, schema, from, record, to, cx)
    }

    fn related_ids(
        &self,
        schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        cx: &Ctx,
    ) -> Result<Arc<[u64]>, om_calc::CalcError> {
        self.related(schema, from, record, to, cx)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, om_calc::CalcError> {
        self.inner.globals(table)
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
        self.inner.found_set(occurrence, table)
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.inner.found_sort(occurrence)
    }

    fn library_member(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<MemberInfo>>, om_calc::CalcError> {
        match self.containers {
            Some(c) => c.member(table, member),
            None => Ok(None),
        }
    }

    fn library_data(
        &self,
        table: u64,
        member: &[u8],
    ) -> Result<Option<Arc<om_calc::eval::StoredContainer>>, om_calc::CalcError> {
        match self.containers {
            Some(c) => c.member_data(table, member),
            None => Ok(None),
        }
    }

    fn pending(&self) -> Pending {
        self.containers
            .map_or_else(Pending::default, |c| c.pending())
    }
}

pub struct PreparedRecord {
    /// Changed field cells, with `None` denoting an absent/cleared cell.
    /// Apply these over the original subtree to preserve nested container cells
    /// and engine bookkeeping that this evaluator does not own.
    pub values: BTreeMap<u64, Option<Vec<u8>>>,
    pub changed_fields: Vec<u64>,
}

pub struct RecordPlan {
    table: u64,
    fields: BTreeMap<u64, PlannedField>,
    evaluation_order: Vec<u64>,
    /// (auto-entered result, equality key, inequality key) for #297.
    grouped_self_joins: BTreeSet<(u64, u64, u64)>,
    /// The file's calculation schema with this table's current fields.
    schema: Arc<CalcSchema>,
}

struct PlannedField {
    name: String,
    options: FieldOptions,
    data_type: DataType,
    editable: bool,
    /// A stored calculation or an auto-enter calculation.
    expression: Option<Expression>,
    /// This local global's auto-entry reads Self.
    global_text_self: GlobalTextSelf,
    /// Creation Date supplies the value on create. The calculation's
    /// dependencies identify updates with an unmeasured result.
    creation_date_with_calculation: bool,
    /// Auto-entered constant data (`0d`).
    constant: Option<String>,
    validation: Option<Expression>,
    message: Option<CustomMessage>,
    /// "In range": the lower and upper bounds as stored (`21`, `22`),
    /// empty when not set.
    range: Option<(String, String)>,
    /// "Member of value list" (`07`).
    list: Option<ListRule>,
    /// An auto-entered serial number.
    serial: Option<Serial>,
    /// An auto-enter lookup (`08`).
    lookup: Option<Lookup>,
    /// The index language of a stored text field the host keeps an index
    /// of, when OpenMaker does not build that language's keys
    /// ([`written_text_keys`]): a write that changes its text is refused.
    unkeyed_index: Option<u32>,
    /// "Use Furigana Field" (option `0x40`): FileMaker Pro writes the
    /// reading of the text entered here into another field, which
    /// OpenMaker does not (#1301, #1302), so a write that changes its
    /// value is refused rather than leave the reading behind.
    furigana: bool,
}

/// "Display custom message if validation fails" (`0x80`).
#[derive(Clone, Debug)]
enum CustomMessage {
    /// The text in member `18`.
    Text(String),
    /// A calculation in member `20` (`0x8000`), its text evaluated where the
    /// field fails ([`RecordPlan::invalid_in`]). `None` when the rules cannot
    /// evaluate it (it reads related fields, or what record rules do not):
    /// the refusal then has OpenMaker's own wording, never member `18`, which
    /// holds the text older versions show in its place (E34).
    Calculation(Option<Expression>),
}

impl PlannedField {
    fn auto_enters_global(&self) -> bool {
        self.options.options & GLOBAL != 0 && self.options.options & GLOBAL_AUTO_ENTRY_MODES != 0
    }

    /// The field's auto-entered or stored calculation and its validation
    /// calculation.
    fn formulas(&self) -> impl Iterator<Item = &Expression> {
        self.expression.iter().chain(self.validation.iter())
    }
}

/// An auto-enter lookup the rules perform (auto-entry.md, Lookups): an
/// exact match that copies nothing without one, the copied field's stored
/// text as it is into a field of any type but a container (battery s43),
/// from the first record,
/// in the relationship's sort order when it has one, related through one
/// relationship of `=` predicates on stored text fields, as FileMaker
/// Pro 26 and FileMaker Server's script engine were seen to.
#[derive(Clone, Debug)]
struct Lookup {
    /// The occurrence it starts from (of the field's table) and the one it
    /// looks up from.
    from: u64,
    to: u64,
    /// The field it copies, of `to`'s table.
    copied: u64,
    /// "Don't copy contents if empty" off: an empty value is copied too.
    copy_empty: bool,
    /// Value to copy when there is no matching record, including an empty value.
    fallback: Option<String>,
    /// The field's table's match fields: a write changing one fires it.
    matches: Vec<u64>,
}

/// A lookup definition `08` (schema-path.md 5.5): byte 0 `00` ("do not
/// copy") or `03` ("use" the fallback from `0b`, empty when absent),
/// byte 1 "don't copy contents if empty" (`00` on, `01` off), then three
/// fixed-size key buffers, each a key string's length, the key and padding:
/// the occurrence
/// looked up from (6 bytes), the field copied (7), a byte `01` whose
/// meaning is unknown, and the starting occurrence (6), 22 bytes in all.
/// The padding is whatever the writer left there (`00`, `7f`, `5e` and
/// others, which change no setting FileMaker Pro's dialog shows), so any
/// value is read; every other byte must be as stated, and each key minimally
/// encoded within its buffer. Anything else is `None`.
fn exact_lookup(bytes: &[u8], fallback: Option<String>) -> Option<Lookup> {
    fn expect(bytes: &[u8], at: &mut usize, expected: &[u8]) -> Option<()> {
        let end = at.checked_add(expected.len())?;
        (bytes.get(*at..end)? == expected).then(|| *at = end)
    }
    // A key string in a buffer of `size` bytes: its length, its id
    // minimally encoded, then padding to the buffer's end.
    fn key(bytes: &[u8], at: &mut usize, size: usize) -> Option<u64> {
        let buffer = bytes.get(*at..at.checked_add(size)?)?;
        let length = usize::from(buffer[0]);
        let key = Key(buffer.get(1..1 + length)?.to_vec());
        let id = key.id()?;
        (Key::from_id(id) == key).then(|| *at += size)?;
        Some(id)
    }
    let fallback = match bytes.first()? {
        0 => None,
        3 => Some(fallback?),
        _ => return None,
    };
    let mut at = 1;
    let copy_empty = match *bytes.get(at)? {
        0 => false,
        1 => true,
        _ => return None,
    };
    at += 1;
    let to = key(bytes, &mut at, 6)?;
    let copied = key(bytes, &mut at, 7)?;
    expect(bytes, &mut at, &[1])?;
    let from = key(bytes, &mut at, 6)?;
    (at == bytes.len()).then_some(Lookup {
        from,
        to,
        copied,
        copy_empty,
        fallback,
        matches: Vec::new(),
    })
}

/// Whether a lookup copying a value of type `from` into a field of type
/// `into` gives what FileMaker gives (auto-entry.md, Lookups, battery s43):
/// the stored text as it is. Both hosts read text, numbers and dates with
/// their stored text (`Value::NumberText`, `Value::DateText`); a time or
/// timestamp is read as its value, and text that is no date reads in a
/// date field as text in calculations, where FileMaker reads an invalid
/// date. So text, numbers and dates go into text and number fields, and a
/// date field takes dates only.
fn typed_copy(from: ValueType, into: DataType) -> bool {
    let read = matches!(from, ValueType::Text | ValueType::Number | ValueType::Date);
    read && match into {
        DataType::Text | DataType::Number => true,
        DataType::Date => from == ValueType::Date,
        _ => false,
    }
}

/// The match fields of `lookup` of `table`, once its relationship is one
/// the rules can follow; otherwise why not.
fn lookup_matches(
    schema: &CalcSchema,
    table: u64,
    fields: &BTreeMap<u64, PlannedField>,
    lookup: &Lookup,
    into: DataType,
) -> Result<Vec<u64>, &'static str> {
    use crate::host::{FieldClass, FieldDef};
    use crate::meta::JoinOp;
    // A stored value of one repetition, as the related record holds it.
    let stored = |def: &FieldDef| {
        def.stored
            && !def.global
            && def.repetitions == 1
            && matches!(def.class, FieldClass::Data | FieldClass::Calculation)
    };
    let stored_text = |def: &FieldDef| stored(def) && def.value_type == ValueType::Text;
    let from = schema
        .occurrence(lookup.from)
        .ok_or("its starting occurrence is missing.")?;
    if from.table != table {
        return Err("it starts from another table's occurrence.");
    }
    let to = schema
        .occurrence(lookup.to)
        .ok_or("it looks up from an occurrence of another file or a missing one.")?;
    if to.table == table {
        return Err("lookups from the field's own table are unsupported.");
    }
    let to_table = schema
        .table(to.table)
        .ok_or("the table it looks up from is missing.")?;
    let copied = to_table
        .fields
        .get(&lookup.copied)
        .filter(|def| stored(def))
        .map(|def| def.value_type);
    if !copied.is_some_and(|ty| typed_copy(ty, into)) {
        return Err(
            "lookups need a stored text, number or date field of one repetition, copied into a \
             text or number field, or a date into a date field.",
        );
    }
    let path = schema
        .path(lookup.from, lookup.to)
        .ok_or("its occurrences are not related.")?;
    let [hop] = path.as_slice() else {
        return Err("lookups through more than one relationship are unsupported.");
    };
    if hop.predicates.is_empty() || hop.predicates.iter().any(|p| p.op != JoinOp::Equal) {
        return Err("lookups through a relationship with other than = predicates are unsupported.");
    }
    // A sorted relationship gives its first record in its order
    // (auto-entry.md, Lookups, battery s42), as `related_record` reads it.
    if !ordered_as_filemaker(schema, hop, to_table) {
        return Err(
            "lookups through a relationship sorted by a field OpenMaker cannot order by \
             (another table's, a summary or an unstored calculation, or by a summary or a \
             value list of field values) are unsupported.",
        );
    }
    let mut matches = Vec::new();
    for predicate in &hop.predicates {
        // Plain Text and calculated Text stored in the row. An auto-entry
        // calculation may depend on this row but cannot read a relationship.
        let ours = fields.get(&predicate.left).is_some_and(|f| {
            f.data_type == DataType::Text
                && f.options.max_repetitions == 1
                && f.in_row()
                && (f.stored_calculation() && f.expression.is_some()
                    || f.options.field_kind == 1
                        && (f.options.options & !(INDEX_OPTIONS | BOOKKEEPING_OPTIONS) == 0
                            || f.fresh_key()
                            || f.options.auto_entry_calculation()
                                && f.expression.as_ref().is_some_and(|expression| {
                                    expression.related.is_empty() && expression.whole.is_empty()
                                })))
        });
        if !ours
            || !to_table
                .fields
                .get(&predicate.right)
                .is_some_and(stored_text)
        {
            return Err(
                "lookups need stored Text match fields: plain fields or supported calculated keys.",
            );
        }
        matches.push(predicate.left);
    }
    Ok(matches)
}

/// The record a write's lookup reads its related records from: the
/// written record ([`WRITTEN`]) as the write leaves it, over the host's
/// records. The host's index narrows the related table's records to
/// candidates; the scan's comparisons still decide which relate.
struct LookupSource<'a> {
    inner: &'a dyn CalcSource,
    table: u64,
    record: Arc<RecordValues>,
    /// The related table and a match field of it with the value to find.
    related: u64,
    key: Option<(u64, String)>,
    /// A record of the related table left out (the written record's stored
    /// row, for a self-join, #1035).
    exclude: Option<u64>,
}

/// The id the written record has while its lookups read: none of a
/// table's records has it.
const WRITTEN: u64 = u64::MAX;

/// How many records a lookup's read asks its host to prefetch at once,
/// checking between them whether the host's script was stopped: the
/// provider downloads 500 a request (`om-provider`'s `SCAN_BATCH`).
const LOOKUP_PREFETCH: usize = 500;

impl LookupSource<'_> {
    /// Error 1 once the host's script was stopped or ran out of time: the
    /// lookup's read ends at its next record, list or download, before any
    /// value is copied.
    fn check(&self) -> Result<(), om_calc::CalcError> {
        if self.inner.interrupted() {
            Err(om_calc::CalcError::CANCELED)
        } else {
            Ok(())
        }
    }

    /// `table`'s records, the related table's narrowed by the value index
    /// of the first equality predicate.
    fn narrowed(&self, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
        self.check()?;
        let all = self.inner.records(table)?;
        if table != self.related {
            return Ok(all);
        }
        let Some((field, text)) = &self.key else {
            return Ok(all);
        };
        // An empty value matches nothing (file-metadata.md 4.3).
        if text.is_empty() {
            return Ok(Arc::from([]));
        }
        self.check()?;
        let Some(candidates) = self.inner.lookup(table, *field, text)? else {
            return Ok(all);
        };
        // The candidates in the table's own order: creation order.
        let candidates: BTreeSet<u64> = candidates.iter().copied().collect();
        let mut narrowed = Vec::new();
        for ids in all.chunks(LOOKUP_PREFETCH) {
            self.check()?;
            narrowed.extend(ids.iter().copied().filter(|r| candidates.contains(r)));
        }
        Ok(narrowed.into())
    }
}

impl CalcSource for LookupSource<'_> {
    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<Arc<RecordValues>>, om_calc::CalcError> {
        self.check()?;
        if (table, record) == (self.table, WRITTEN) {
            return Ok(Some(Arc::clone(&self.record)));
        }
        self.inner.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
        let all = self.narrowed(table)?;
        match self.exclude.filter(|_| table == self.related) {
            Some(exclude) => Ok(all.iter().copied().filter(|r| *r != exclude).collect()),
            None => Ok(all),
        }
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, om_calc::CalcError> {
        self.check()?;
        self.inner.globals(table)
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        let records: Vec<u64> = records.iter().copied().filter(|r| *r != WRITTEN).collect();
        for chunk in records.chunks(LOOKUP_PREFETCH) {
            // A stopped script downloads nothing more; the next read fails.
            if self.inner.interrupted() {
                return;
            }
            self.inner.prefetch_records(table, chunk);
        }
    }

    fn interrupted(&self) -> bool {
        self.inner.interrupted()
    }

    fn pending(&self) -> Pending {
        self.inner.pending()
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.inner.need_occurrence(occurrence);
    }

    fn need_layout(&self, layout: u32) {
        self.inner.need_layout(layout);
    }
}

/// Records an open transaction holds, as a write's lookups and related
/// reads see them (#187): a new record New Record reserved, with its cells
/// (the serial numbers taken then among them); an edited one as its stored
/// record with the held cells over it; a deleted one, which no read finds.
/// Rows are by cell as the rules write them (a field's id, `F * 256 + n`
/// for repetition n), in stored text; tables in the reading file's ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldRecords {
    tables: BTreeMap<u64, BTreeMap<u64, Option<BTreeMap<u64, String>>>>,
}

impl HeldRecords {
    /// Record `record` of `table` holds `row` (every cell it has).
    pub fn hold(&mut self, table: u64, record: u64, row: BTreeMap<u64, String>) {
        self.tables
            .entry(table)
            .or_default()
            .insert(record, Some(row));
    }

    /// Record `record` of `table` is deleted.
    pub fn delete(&mut self, table: u64, record: u64) {
        self.tables.entry(table).or_default().insert(record, None);
    }

    pub fn is_empty(&self) -> bool {
        self.tables.values().all(BTreeMap::is_empty)
    }

    /// Whether a record of `table` other than `record` is held.
    pub fn holds_other(&self, table: u64, record: Option<u64>) -> bool {
        self.of(table)
            .is_some_and(|held| held.keys().any(|id| Some(*id) != record))
    }

    fn of(&self, table: u64) -> Option<&BTreeMap<u64, Option<BTreeMap<u64, String>>>> {
        self.tables.get(&table).filter(|held| !held.is_empty())
    }
}

/// `inner`'s records with an open transaction's [`HeldRecords`] over them:
/// a held record reads as held, a new one is among its table's records
/// after the stored ones (its id was reserved after theirs: creation
/// order), a deleted one is none of them. Relationships scan through it
/// (the default `related`); kept scans are not offered, as they hold only
/// stored rows.
pub struct HeldSource<'a> {
    inner: &'a dyn CalcSource,
    held: &'a HeldRecords,
    schema: &'a CalcSchema,
    cx: Ctx<'a>,
}

impl<'a> HeldSource<'a> {
    /// `held`'s rows typed by `schema`, the reading file's.
    pub fn new(inner: &'a dyn CalcSource, held: &'a HeldRecords, schema: &'a CalcSchema) -> Self {
        let cx = Ctx::new(&schema.locale, crate::host::session::today(0));
        HeldSource {
            inner,
            held,
            schema,
            cx,
        }
    }
}

impl CalcSource for HeldSource<'_> {
    fn record(
        &self,
        table: u64,
        record: u64,
    ) -> Result<Option<Arc<RecordValues>>, om_calc::CalcError> {
        match self.held.of(table).and_then(|held| held.get(&record)) {
            Some(Some(row)) => Ok(Some(Arc::new(row_values(
                self.schema,
                table,
                row,
                &self.cx,
            )))),
            Some(None) => Ok(None),
            None => self.inner.record(table, record),
        }
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
        let stored = self.inner.records(table)?;
        let Some(held) = self.held.of(table) else {
            return Ok(stored);
        };
        let mut ids: Vec<u64> = stored
            .iter()
            .copied()
            .filter(|id| !matches!(held.get(id), Some(None)))
            .collect();
        let known: BTreeSet<u64> = ids.iter().copied().collect();
        ids.extend(
            held.iter()
                .filter(|(id, row)| row.is_some() && !known.contains(id))
                .map(|(id, _)| *id),
        );
        Ok(ids.into())
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, om_calc::CalcError> {
        self.inner.globals(table)
    }

    fn prefetch_records(&self, table: u64, records: &[u64]) {
        let held = self.held.of(table);
        let stored: Vec<u64> = records
            .iter()
            .copied()
            .filter(|id| held.is_none_or(|held| !held.contains_key(id)))
            .collect();
        self.inner.prefetch_records(table, &stored);
    }

    fn has_record(&self, table: u64, record: u64) -> bool {
        self.held
            .of(table)
            .is_some_and(|held| held.contains_key(&record))
            || self.inner.has_record(table, record)
    }

    /// The stored candidates, less deleted records, with every held one:
    /// its held value may match where its stored one does not.
    fn lookup(
        &self,
        table: u64,
        field: u64,
        text: &str,
    ) -> Result<Option<Arc<[u64]>>, om_calc::CalcError> {
        let Some(stored) = self.inner.lookup(table, field, text)? else {
            return Ok(None);
        };
        let Some(held) = self.held.of(table) else {
            return Ok(Some(stored));
        };
        let mut ids: BTreeSet<u64> = stored
            .iter()
            .copied()
            .filter(|id| !matches!(held.get(id), Some(None)))
            .collect();
        ids.extend(
            held.iter()
                .filter(|(_, row)| row.is_some())
                .map(|(id, _)| *id),
        );
        Ok(Some(ids.into_iter().collect()))
    }

    fn interrupted(&self) -> bool {
        self.inner.interrupted()
    }

    fn pending(&self) -> Pending {
        self.inner.pending()
    }

    fn need_occurrence(&self, occurrence: u64) {
        self.inner.need_occurrence(occurrence);
    }

    fn need_layout(&self, layout: u32) {
        self.inner.need_layout(layout);
    }
}

/// The values "member of value list" accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListRule {
    /// A custom list's values (dividers left out).
    Custom(Vec<String>),
    /// The values of a field: the host knows them (its value index).
    Field { occurrence: u64, field: u64 },
    /// One field's values related to the record ("Include only related
    /// values starting from"): the host computes the list for the record as
    /// the write leaves it and checks it at the commit
    /// ([`RecordPlan::related_value_list`]).
    Related(Box<ValueList>),
    /// A list the rules cannot check yet ([`value_list_problem`]: another
    /// file's, a calculation, related values only, a second field), and
    /// why. The table stays writable: a write whose rules check the field's
    /// value is refused (3), never let through unchecked.
    Unchecked(String),
}

impl PlannedField {
    fn dependencies(&self) -> impl Iterator<Item = &u64> {
        self.expression
            .iter()
            .flat_map(|e| e.dependencies.iter())
            .chain(self.lookup.iter().flat_map(|l| l.matches.iter()))
    }

    fn stored_calculation(&self) -> bool {
        self.options.field_kind == 2
    }

    /// A field whose value the record's row holds: a normal field or a
    /// stored calculation, not global and not unstored.
    fn in_row(&self) -> bool {
        matches!(self.options.field_kind, 1 | 2) && self.options.options & (GLOBAL | UNSTORED) == 0
    }

    /// A normal field whose value a write gives: no auto-entry, lookup or
    /// global storage.
    fn given(&self) -> bool {
        self.options.field_kind == 1
            && self.options.options & (AUTO_ENTRY_MODES | LOOKUP | GLOBAL) == 0
    }

    /// Error 3 for a value the field's rules check against a value list
    /// they cannot check yet ([`ListRule::Unchecked`]).
    fn unchecked_list(&self) -> Result<(), DataError> {
        match &self.list {
            Some(ListRule::Unchecked(reason)) => Err(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {}: its value is checked against a value list OpenMaker cannot check \
                     yet ({}) No values were changed.",
                    self.name,
                    reason.trim_end_matches('.')
                ),
            )),
            _ => Ok(()),
        }
    }

    /// Whether a script may set the field: a normal, non-global field, in
    /// any of its repetitions. A container's text is the reference its
    /// host stores for the data (containers.md 1.4) or text a script set
    /// into it (script oracle s34-containers-setfield).
    fn settable_by_script(&self) -> bool {
        self.options.field_kind == 1
            && self.options.options & GLOBAL == 0
            // Text, number, date, time, timestamp and container.
            && matches!(self.options.data_type, 1..=6)
    }

    /// Whether the field's validation applies to a write: always, unless
    /// the write skips data entry validation and the field validates only
    /// during data entry.
    fn validated(&self, entry: Entry) -> bool {
        !entry.skip_data_entry_validation || self.validates_always()
    }

    /// Validated Always, not only during data entry.
    fn validates_always(&self) -> bool {
        self.options.validation_options & VALIDATE_ALWAYS != 0
    }

    /// A direct `$$` read by the measured Text auto-enter form (#1037).
    fn auto_entry_file_variable(&self) -> Option<&str> {
        if self.options.field_kind != 1
            || self.options.max_repetitions != 1
            || !self.in_row()
            || self.data_type != DataType::Text
            || !self.options.auto_entry_calculation()
        {
            return None;
        }
        match &self.expression.as_ref()?.formula.polish {
            Expr::Variable(name) if name.starts_with("$$") => Some(name),
            _ => None,
        }
    }

    /// Whether the rules FileMaker checks as a value is entered (maximum
    /// characters, strict data types, range, value list) apply to `entry`:
    /// a script's Set Field is checked only against a field validated
    /// Always (script oracle s18: a field validated only during data entry
    /// takes any value from it), a user's entry like the field's other
    /// rules.
    fn entry_validated(&self, entry: Entry) -> bool {
        if entry.script {
            self.validates_always()
        } else {
            self.validated(entry)
        }
    }

    /// The first rule checked as a value is entered that `value` (stored
    /// text, not empty) breaks: its error and what it requires. A value
    /// list of a field's values is the host's to check
    /// ([`RecordPlan::value_list_field`]).
    fn entry_error(&self, value: &str, cx: &Ctx) -> Option<(i32, String)> {
        let rules = self.options.validation_options;
        if let Some(limit) = self.exceeded_limit(value) {
            return Some((error::TOO_LONG, format!("at most {limit} characters")));
        }
        if rules & VALIDATE_NUMERIC != 0 && !numeric(value, cx) {
            return Some((error::NUMBER, "a number".into()));
        }
        if rules & VALIDATE_FOUR_DIGIT_YEAR != 0 && !four_digit_year(value, cx) {
            return Some((error::DATE, "a date with a four-digit year".into()));
        }
        if rules & VALIDATE_TIME_OF_DAY != 0 && !time_of_day(value, cx) {
            return Some((error::TIME, "a time of day".into()));
        }
        if let Some((from, to)) = &self.range {
            if !in_range(self.data_type, value, from, to, cx) {
                return Some((error::RANGE, format!("a value from {from} to {to}")));
            }
        }
        if let Some(ListRule::Custom(values)) = &self.list {
            if !values
                .iter()
                .any(|v| same_entry(self.data_type, v, value, cx))
            {
                return Some((error::NOT_IN_LIST, "a value of its value list".into()));
            }
        }
        None
    }

    /// A key whose only auto-entry is `Get ( UUID )` (with "Do not
    /// replace existing value" off or on, "Prohibit modification" or not):
    /// set by a write, it is a match field like any other; made with a new
    /// record, no related record holds it. FileMaker performs a lookup then
    /// and copies nothing, which is what not performing it leaves
    /// (auto-entry.md, Lookups, battery s41).
    fn fresh_key(&self) -> bool {
        // An auto-entered calculation `0x8`, replacing `0x80`, prohibiting
        // modification `0x100`, always evaluated `0x20`.
        const CALCULATED: u32 = 0x8;
        const ALLOWED: u32 = CALCULATED | ALWAYS_EVALUATE | 0x80 | 0x100;
        let options = self.options.options & !(INDEX_OPTIONS | BOOKKEEPING_OPTIONS);
        options & CALCULATED != 0 && options & !ALLOWED == 0 && self.generated_uuid()
    }

    /// The value is `Get ( UUID )`, which uniqueness checks as a UUID.
    fn generated_uuid(&self) -> bool {
        self.expression
            .as_ref()
            .is_some_and(|e| matches!(e.formula.polish, Expr::Get(101)))
    }

    /// The maximum number of characters (`mByteLimit`) when `value` is
    /// longer than the field allows.
    fn exceeded_limit(&self, value: &str) -> Option<u32> {
        if self.options.validation_options & VALIDATE_MAX_CHARACTERS == 0 {
            return None;
        }
        let limit = u32::from_be_bytes(self.options.raw[20..24].try_into().expect("4"));
        (value.chars().count() > limit as usize).then_some(limit)
    }

    /// How many repetitions the field stores (at most 255, the cells
    /// `F * 256 + n` can name).
    fn repetitions(&self) -> u32 {
        u32::from(self.options.max_repetitions.clamp(1, 255))
    }
}

/// The cell of repetition `repetition` of `field`: the field's own id for
/// the first, `F * 256 + n` for the others (execute-sql.md, reading
/// `.fmp12` files). `None` when that key would pass `u64::MAX`: the field
/// id comes from the file, and such a repetition cannot have a cell, so a
/// read of it is empty, never another field's cell the key would wrap to.
pub fn cell_of(field: u64, repetition: u32) -> Option<u64> {
    if repetition <= 1 {
        Some(field)
    } else {
        field.checked_mul(256)?.checked_add(u64::from(repetition))
    }
}

/// The measured repeating auto-entry form: a bare reference to a plain
/// Container's matching repetition, with no other inputs (auto-entry.md).
fn repeating_container_filename(
    table: &Table,
    target: &Field,
    options: &FieldOptions,
    expression: &Expression,
) -> bool {
    if target.kind != FieldKind::Normal
        || target.data_type != DataType::Text
        || options.max_repetitions != 24
        || options.options & !(INDEX_OPTIONS | BOOKKEEPING_OPTIONS) != 0x88
        || options.validation_options != 0
    {
        return false;
    }
    // Opcode 3500 is GetContainerAttribute (calc-encoding.md).
    let Expr::Call {
        op: Opcode(3500),
        args,
    } = &expression.formula.polish
    else {
        return false;
    };
    let [Expr::Field(source), Expr::Text(attribute)] = args.as_slice() else {
        return false;
    };
    if source.flags != 0
        || source.repetition != 1
        || source.occurrence.id().is_none()
        || source.occurrence.id() != expression.formula.occurrence()
        || !attribute.value.eq_ignore_ascii_case("filename")
    {
        return false;
    }
    let Some(id) = source.field.id() else {
        return false;
    };
    if expression.dependencies.len() != 1 || !expression.dependencies.contains(&id) {
        return false;
    }
    table.fields.iter().any(|field| {
        field.id == id
            && field.kind == FieldKind::Normal
            && field.data_type == DataType::Container
            && field.decoded_options().is_some_and(|source| {
                source.max_repetitions == 24
                    && (source.options == 0 || source.pending_embedded_container_transfer())
            })
    })
}

/// Whether a value of another record may not be what that record's commit
/// stores ([`RecordPlan::pending_row`], #1063): by its table, its record,
/// and its field. With no record (`None`), whether that field is unknown
/// on any record of the table: a match or sort field, which decides which
/// records are read, or a value of records not named.
pub type UnknownValue<'a> = &'a dyn Fn(u64, Option<u64>, u64) -> bool;

/// What [`RecordPlan::compute_inner`] computed: the record's cells as the
/// write leaves them, the cells it changed, and the fields a preview left
/// out.
struct Computed {
    row: BTreeMap<u64, String>,
    dirty: BTreeSet<u64>,
    deferred: BTreeSet<u64>,
    /// The fields whose value an auto-entered variable or a formula gave.
    evaluated: BTreeSet<u64>,
    /// The fields whose formula was not evaluated because the unstored
    /// calculations it reads were empty: a volatile one may not be at the
    /// commit ([`RecordPlan::pending_row`]). Its formula read nothing else,
    /// so a commit's check of what it read passes it over.
    probed: BTreeSet<u64>,
}

/// A stored formula and the fields of its context occurrence it reads.
#[derive(Clone, Debug)]
struct Expression {
    formula: Arc<Formula>,
    dependencies: BTreeSet<u64>,
    /// The fields of other occurrences it reads, (occurrence, field): an
    /// auto-entered or validation formula's related fields, read through
    /// one relationship from its context on its first record, or on every
    /// one for [`Self::whole`] (auto-entry.md, "Calculations over related
    /// fields").
    related: BTreeSet<(u64, u64)>,
    /// Readable global fields by base table and field id, separate from
    /// relationships. Only the local same-file auto-enter case is planned.
    other_globals: BTreeSet<(u64, u64)>,
    /// The occurrences of which it reads every related record: a related
    /// field passed where a field is required, to `Max`, `Count`, `List`,
    /// `Last`, `GetNthRecord` and the other aggregates (#1036), rather than
    /// read as the first record's value.
    whole: BTreeSet<u64>,
    /// How it reads each related occurrence ([`Uses`]): what decides
    /// whether a self-join's answer depends on the written record (#1035).
    uses: BTreeMap<u64, Uses>,
    /// Fields among [`Self::dependencies`] it does not read itself: the
    /// inputs of the unstored calculations it reads ([`UnstoredGraph::reads`]).
    /// A change of one evaluates an auto-entered formula again, as one of
    /// its own (script oracle s44), but they are not among the fields its
    /// "Do not evaluate if all referenced fields are empty" looks at.
    through_unstored: BTreeSet<u64>,
    /// The formulas of those unstored calculations: a `Random` draw, the
    /// clock or a UUID in one makes this formula's value change when it is
    /// evaluated again, as one of its own would.
    through_formulas: Vec<Arc<Formula>>,
}

/// How a formula reads one related occurrence.
#[derive(Clone, Debug, Default)]
struct Uses {
    /// A field read as a value: the first related record's.
    first: bool,
    /// The fields passed to an aggregate (`Sum`, `Count`, `Max`, `List` and
    /// the others), which skips empty values (calc-functions.md,
    /// "Aggregate").
    aggregated: BTreeSet<u64>,
    /// A field passed to another function (`Last`, `GetNthRecord`, `Lookup`
    /// and the others), which reads records by their place.
    placed: bool,
}

/// The aggregate functions (om-formula's `is_aggregate`, calc-functions.md
/// "Aggregate"): Average, Count, Max, Min, StDev, StDevP, Sum, List,
/// Variance, VarianceP.
fn is_aggregate(op: om_formula::Opcode) -> bool {
    matches!(op.0, 0x7f..=0x85 | 0x87 | 0xa0 | 0xa1)
}

/// Whether a user types into field `field` by its definition alone: a
/// normal field of text, number, date, time or timestamp, not global, of
/// supported repetitions, whose auto-entry does not prohibit editing.
/// [`RecordPlan::editable_fields`] holds these once the table's rules plan;
/// a window may offer them before, while a related file its rules read is
/// not open yet (the record capabilities, #1139).
pub fn user_editable(field: &om_hbam::Field) -> bool {
    let Some(options) = field.decoded_options() else {
        return false;
    };
    field.kind == FieldKind::Normal
        && matches!(
            field.data_type,
            DataType::Text
                | DataType::Number
                | DataType::Date
                | DataType::Time
                | DataType::Timestamp
        )
        && options.options & GLOBAL == 0
        && options.max_repetitions <= 255
        && !options.auto_entry_editing_prohibited()
}

fn supports_creation_time_text(locale: &Locale) -> bool {
    locale.twelve_hour
        && !locale.pad_hour
        && !locale.am_pm_before
        && locale.time_separator == ':'
        && locale.am == " AM"
        && locale.pm == " PM"
}

impl RecordPlan {
    /// The rules of `table` from its definitions in `tree` alone (no other
    /// tables, occurrences or custom functions).
    #[cfg(test)]
    pub fn from_schema(table: &Table, tree: &KeyTree<'_>) -> Result<Self, DataError> {
        Self::plan(table, tree, None, false, false, None)
    }

    /// The rules of `table` from its current definitions in `tree`, within
    /// the file's calculation schema `file` (names, occurrences, custom
    /// functions).
    pub fn from_file_schema(
        table: &Table,
        tree: &KeyTree<'_>,
        file: &CalcSchema,
    ) -> Result<Self, DataError> {
        Self::plan(table, tree, Some(file), false, false, None)
    }

    /// The desktop's provider handles the session-global side of New
    /// Record explicitly; the other planner callers only write rows.
    /// `local` permits a stored Number to use the local creation position
    /// and a Text creation-time field to use the measured desktop clock format.
    pub fn from_desktop_file_schema(
        table: &Table,
        tree: &KeyTree<'_>,
        file: &CalcSchema,
        local: bool,
    ) -> Result<Self, DataError> {
        static SYSTEM: std::sync::LazyLock<Option<Locale>> =
            std::sync::LazyLock::new(crate::new_file::configured_system_locale);
        let system = if local { SYSTEM.as_ref() } else { None };
        Self::plan(table, tree, Some(file), true, local, system)
    }

    fn plan(
        table: &Table,
        tree: &KeyTree<'_>,
        file: Option<&CalcSchema>,
        global_defaults: bool,
        local: bool,
        system: Option<&Locale>,
    ) -> Result<Self, DataError> {
        let mut schema = file
            .cloned()
            .unwrap_or_else(|| CalcSchema::new("", Locale::en_us()));
        schema.add_table(table.id, table.name.clone());
        schema.clear_fields(table.id);
        let file_language = u32::from(schema.locale.language.id());
        // Unknown when the file's locale record does not decode: a unique
        // check in language 0 is then refused, not keyed in English.
        let own_language = schema.file_language();
        let mut fields = BTreeMap::new();
        let mut contexts = BTreeSet::new();
        // The table's unstored calculations a formula of the table may read
        // (`UnstoredGraph`), or why one cannot be evaluated for a write.
        let mut unstored_formulas: BTreeMap<u64, Result<Expression, String>> = BTreeMap::new();
        for field in &table.fields {
            let error = |reason: &str| DataError::new(format!("Field {}: {reason}", field.name));
            let options = field
                .decoded_options()
                .ok_or_else(|| error("unrecognized field definition."))?;
            let kind = field.kind;
            if !matches!(
                kind,
                FieldKind::Normal | FieldKind::Calculation | FieldKind::Summary
            ) {
                return Err(error("unknown field kinds are unsupported."));
            }
            // Numeric tree lookups scan siblings: resolve this field once
            // for both its calculation definition and its entry rules.
            let field_node = tree.at(&[table.id, 3, 5, field.id]);
            let def = schema::field_def_at(field_node, field, &options)
                .ok_or_else(|| error("unrecognized field definition."))?;
            schema.add_field(table.id, def.clone());
            let global = options.options & GLOBAL != 0;
            let unstored =
                kind == FieldKind::Calculation && (options.options & UNSTORED != 0 || global);
            let auto_calculation = options.auto_entry_calculation();
            // FileMaker used the creation date at New Record even with
            // calculation replacement on. The test had editing allowed.
            let creation_date_with_calculation = !global
                && field.data_type == DataType::Date
                && options.max_repetitions == 1
                && options.options & AUTO_ENTRY_MODES == 0x9
                && options.options & ALWAYS_EVALUATE == 0
                && !options.auto_entry_editing_prohibited()
                && options.auto_entry_variable() == Some(EntryVariable::CreationDate);
            let looked_up = options.options & LOOKUP != 0;
            let allowed = match kind {
                // An unstored calculation is computed when read, and
                // nothing the planner computes reads it: formulas of its
                // own table are refused over it (below), related reads,
                // lookups and related sorts need a stored value
                // (`related_hop`, `lookup_matches`, `ordered_as_filemaker`).
                // Auto-entry, editing and script writes need a normal
                // field. Only a lookup (`looked_up`) would write into it,
                // so that bit is checked; its others, such as a stray
                // `0x80` (auto-entry.md, Index bits), cannot change a
                // write and are not. A stored calculation's all are.
                FieldKind::Calculation if options.options & UNSTORED != 0 => !LOOKUP,
                FieldKind::Calculation => {
                    INDEX_OPTIONS | BOOKKEEPING_OPTIONS | ALWAYS_EVALUATE | UNSTORED
                }
                FieldKind::Summary => INDEX_OPTIONS | BOOKKEEPING_OPTIONS,
                // While this container's transfer is pending, other cells
                // may be written without touching its payload or definition.
                FieldKind::Normal if local && options.pending_embedded_container_transfer() => {
                    options.options
                }
                // A lookup alone: with other auto-entry its order is unmeasured.
                // Furigana leaves a normal field's table writable: a change
                // of its value alone is refused (`refuse_unwritable_changes`).
                _ if looked_up => INDEX_OPTIONS | BOOKKEEPING_OPTIONS | LOOKUP | FURIGANA,
                _ if auto_calculation => {
                    INDEX_OPTIONS
                        | BOOKKEEPING_OPTIONS
                        | AUTO_ENTRY_OPTIONS
                        | ALWAYS_EVALUATE
                        | GLOBAL
                        | FURIGANA
                }
                _ => INDEX_OPTIONS | BOOKKEEPING_OPTIONS | AUTO_ENTRY_OPTIONS | GLOBAL | FURIGANA,
            };
            if options.options & !allowed != 0 {
                return Err(error("unsupported field options."));
            }
            let auto_enters_global = global && options.options & AUTO_ENTRY_OPTIONS & !0x100 != 0;
            if auto_enters_global
                && (!global_defaults
                    || options.options & AUTO_ENTRY_MODES & !GLOBAL_AUTO_ENTRY_MODES != 0
                    || options.options & GLOBAL_AUTO_ENTRY_MODES == 0
                    || field.data_type != DataType::Text
                    || options.max_repetitions != 1
                    || options.validation_options & !VALIDATE_ALWAYS != 0)
            {
                return Err(error("auto-entry into a global field is unsupported."));
            }
            if options.validation_options & !SUPPORTED_VALIDATION != 0 {
                return Err(error("unsupported validation rules."));
            }
            // The host reserves the value (op72, write-path.md 3.2), which
            // advances the text's last number; a field that does not hold
            // the text as it is, or an increment that would not advance it,
            // is refused.
            let serial = if options.auto_entry_serial() {
                if !matches!(field.data_type, DataType::Text | DataType::Number)
                    || options.max_repetitions != 1
                {
                    return Err(error(
                        "serial numbers are supported only in text and number fields of one \
                         repetition.",
                    ));
                }
                let increment = i32::try_from(options.serial_increment())
                    .ok()
                    .filter(|by| *by > 0)
                    .ok_or_else(|| error("unsupported serial number increment."))?;
                Some(Serial {
                    field: field.id,
                    increment,
                    on_commit: options.options & SERIAL_ON_COMMIT != 0,
                })
            } else {
                None
            };
            let lookup = if looked_up {
                // The copied text is stored as it is (auto-entry.md,
                // Lookups, battery s43), into a field whose values OpenMaker
                // reads with their stored text (`typed_copy`).
                if !matches!(
                    field.data_type,
                    DataType::Text | DataType::Number | DataType::Date
                ) || options.max_repetitions != 1
                {
                    return Err(error(
                        "lookups into time, timestamp, container or repeating fields are \
                         unsupported.",
                    ));
                }
                let definition = field_node
                    .and_then(|n| n.get(FIELD_LOOKUP))
                    .and_then(|n| n.assembled())
                    .ok_or_else(|| error("lookup definition is missing."))?;
                // Absent, or as FileMaker Pro 26 was seen to leave it.
                let seen = match field_node.and_then(|n| n.get(FIELD_LOOKUP_UNKNOWN)) {
                    None => true,
                    Some(n) => n.assembled().is_some_and(|b| *b == [0u8; 4]),
                };
                let fallback =
                    if definition.first() == Some(&3) && field.data_type == DataType::Text {
                        Some(
                            match field_node.and_then(|n| n.get(FIELD_LOOKUP_FALLBACK)) {
                                // FileMaker omits the member when the fallback box is blank.
                                None => String::new(),
                                Some(node) => node
                                    .text()
                                    .and_then(Result::ok)
                                    .ok_or_else(|| error("lookup fallback is unreadable."))?,
                            },
                        )
                    } else {
                        None
                    };
                Some(
                    exact_lookup(&definition, fallback)
                        .filter(|_| seen)
                        .ok_or_else(|| error("unsupported lookup options."))?,
                )
            } else {
                None
            };
            let constant = if options.auto_entry_constant() {
                Some(
                    field_node
                        .and_then(|n| n.get(FIELD_CONSTANT))
                        .and_then(|n| n.text())
                        .and_then(Result::ok)
                        .ok_or_else(|| error("auto-entered constant data is missing."))?,
                )
            } else {
                None
            };
            let formula_at = |key: u64, usage: FormulaUse| -> Result<Expression, DataError> {
                let formula = field_node.and_then(|node| node.get(key));
                let context = formula
                    .and_then(|node| node.get(4))
                    .and_then(|n| n.assembled())
                    .ok_or_else(|| error("calculation context is missing."))?;
                let code = formula
                    .and_then(|node| node.get(6))
                    .and_then(|n| n.assembled())
                    .ok_or_else(|| error("calculation code is missing."))?;
                parse_formula(&code, &context, usage).map_err(|e| error(&e))
            };
            let computed = (kind == FieldKind::Calculation && !unstored) || auto_calculation;
            let expression = if computed {
                if field.data_type == DataType::Container
                    || matches!(field.data_type, DataType::Unknown(_))
                {
                    return Err(error(
                        "only text, number, date and time calculation results are supported.",
                    ));
                }
                let usage = if auto_calculation {
                    FormulaUse::Options
                } else if field.data_type == DataType::Number {
                    FormulaUse::StoredNumber
                } else {
                    FormulaUse::RecordOnly
                };
                let formula = formula_at(FIELD_FORMULA, usage)?;
                if options.max_repetitions != 1
                    && !(local && repeating_container_filename(table, field, &options, &formula))
                {
                    return Err(error(
                        "repeating calculated fields are not supported for native writes.",
                    ));
                }
                if matches!(usage, FormulaUse::StoredNumber)
                    && matches!(formula.formula.polish, Expr::Get(selector::RECORD_NUMBER))
                    && !local
                {
                    return Err(DataError::with_code(
                        error::UNAVAILABLE,
                        format!(
                            "Field {}: a stored record number requires a local desktop file.",
                            field.name
                        ),
                    ));
                }
                if auto_enters_global
                    && (!formula.dependencies.is_empty() || !formula.related.is_empty())
                {
                    return Err(error(
                        "auto-entry into a global that reads fields is unsupported.",
                    ));
                }
                Some(formula)
            } else {
                None
            };
            let global_text_self = if auto_enters_global {
                expression
                    .as_ref()
                    .map(|formula| {
                        global_text_self::classify(
                            &def,
                            Some(&options),
                            &formula.formula.polish,
                            local,
                        )
                    })
                    .unwrap_or_default()
            } else {
                GlobalTextSelf::Unused
            };
            let validation = if options.validation_options & VALIDATE_BY_CALCULATION != 0 {
                Some(formula_at(FIELD_VALIDATION, FormulaUse::Options)?)
            } else {
                None
            };
            // Parsed as a stored calculation's formula: its own table's
            // fields only, which a write's record holds.
            if kind == FieldKind::Calculation && options.options & UNSTORED != 0 && !global {
                let readable = if field.data_type == DataType::Container
                    || matches!(field.data_type, DataType::Unknown(_))
                {
                    Err(format!("Field {} has a container result.", field.name))
                } else if options.max_repetitions != 1 {
                    Err(format!("Field {} repeats.", field.name))
                } else {
                    formula_at(FIELD_FORMULA, FormulaUse::RecordOnly).map_err(|e| e.to_string())
                };
                unstored_formulas.insert(field.id, readable);
            }
            for expr in expression.iter().chain(validation.iter()) {
                if let Some(id) = expr.formula.occurrence() {
                    contexts.insert(id);
                }
            }
            if let Some(variable) = options.auto_entry_variable() {
                if matches!(variable, EntryVariable::Unknown(_)) {
                    return Err(error("unknown auto-entry variable."));
                }
                let expected = match variable {
                    EntryVariable::CreationDate | EntryVariable::ModificationDate => DataType::Date,
                    EntryVariable::CreationTime | EntryVariable::ModificationTime => DataType::Time,
                    EntryVariable::CreationTimestamp | EntryVariable::ModificationTimestamp => {
                        DataType::Timestamp
                    }
                    _ => DataType::Text,
                };
                if field.data_type != expected {
                    // Both time formats matched in the local native check.
                    // The provider gives the formula the file's clock text.
                    let text_creation_time = local
                        && !schema.locale_unreadable
                        && variable == EntryVariable::CreationTime
                        && field.data_type == DataType::Text
                        && options.max_repetitions == 1
                        && supports_creation_time_text(&schema.locale)
                        && system.is_some_and(supports_creation_time_text);
                    if !text_creation_time {
                        return Err(error(
                            "auto-entry variable storage conversion is unsupported.",
                        ));
                    }
                }
            }
            if (options.options & AUTO_ENTRY_MODES).count_ones() > 1
                && !creation_date_with_calculation
            {
                return Err(error("multiple auto-entry modes are unsupported."));
            }
            if options.validation_unique() || options.validation_options & VALIDATE_EXISTING != 0 {
                if field.data_type != DataType::Text {
                    return Err(error(
                        "unique and existing values are currently supported only for text fields.",
                    ));
                }
                // The server checks a unique or existing value in the
                // field's value index (query-program.md 8.4), keyed in its
                // index language.
                if !options.value_indexed()
                    || value_text_keys(options.index_language(), own_language).is_none()
                {
                    return Err(error(
                        "unique and existing values require a value index in English, System, Unicode standard, Unicode raw or a language FileMaker leaves untailored.",
                    ));
                }
            }
            let text_at = |key: u64| -> String {
                field_node
                    .and_then(|n| n.get(key))
                    .and_then(|n| n.text())
                    .and_then(Result::ok)
                    .unwrap_or_default()
            };
            let range = (options.validation_options & VALIDATE_RANGE != 0)
                .then(|| (text_at(FIELD_RANGE_FROM), text_at(FIELD_RANGE_TO)));
            let list = if options.validation_options & VALIDATE_VALUE_LIST != 0 {
                Some(
                    list_rule(tree, field_node, schema.value_lists())
                        .map_err(|reason| error(&reason))?,
                )
            } else {
                None
            };
            // A user types into text, number, date, time and timestamp
            // fields. Text and numbers are stored as typed, as Set Field
            // stores them (script-runtime.md 13.21); dates, times and
            // timestamps as data entry reads them in the file's locale
            // (`typing`). Globals and later repetitions stay display only
            // (#185).
            let editable = user_editable(field);
            // A stored text field with an index, one it is defined with or
            // one the host built on demand (`[T, 01, F]`, which sets no
            // option bit), in a language whose keys OpenMaker does not
            // build: its host would key a change in English (#159).
            let unkeyed_index = {
                const GLOBAL: u32 = 0x1_0000;
                const UNSTORED: u32 = 0x2_0000;
                let index: [&[u8]; 3] = [
                    &om_hbam::slot::uint_segment(table.id),
                    &[1],
                    &om_hbam::slot::uint_segment(field.id),
                ];
                let indexed = options.value_indexed()
                    || options.word_indexed()
                    || tree.node(&index).is_some();
                (field.data_type == DataType::Text
                    && options.options & (GLOBAL | UNSTORED) == 0
                    && indexed)
                    .then(|| options.index_language())
                    .filter(|language| written_text_keys(*language, file_language).is_none())
            };
            let furigana = options.options & FURIGANA != 0;
            let message = if options.validation_options & VALIDATE_MESSAGE == 0 {
                None
            } else if options.validation_options & VALIDATE_MESSAGE_CALCULATION != 0 {
                // A message the rules cannot evaluate leaves the table
                // writable: only the refusal's wording is at stake.
                let formula = formula_at(FIELD_MESSAGE_FORMULA, FormulaUse::RecordOnly)
                    .ok()
                    .filter(|e| {
                        check_custom_functions(
                            &schema,
                            &e.formula.polish,
                            true,
                            false,
                            &mut BTreeSet::new(),
                        )
                        .is_ok()
                    })
                    .filter(|e| {
                        e.formula.occurrence().is_none_or(|occurrence| {
                            schema
                                .occurrence(occurrence)
                                .is_none_or(|o| o.table == table.id)
                        })
                    });
                if let Some(occurrence) = formula.as_ref().and_then(|e| e.formula.occurrence()) {
                    contexts.insert(occurrence);
                }
                Some(CustomMessage::Calculation(formula))
            } else {
                field_node
                    .and_then(|n| n.get(FIELD_MESSAGE))
                    .and_then(|n| n.text())
                    .and_then(Result::ok)
                    .map(CustomMessage::Text)
            };
            fields.insert(
                field.id,
                PlannedField {
                    name: field.name.clone(),
                    options,
                    data_type: field.data_type,
                    editable,
                    expression,
                    global_text_self,
                    creation_date_with_calculation,
                    constant,
                    validation,
                    message,
                    range,
                    list,
                    serial,
                    lookup,
                    unkeyed_index,
                    furigana,
                },
            );
        }
        // A lookup follows the file's relationships from its own table.
        let looked_up: Vec<u64> = fields
            .iter()
            .filter(|(_, f)| f.lookup.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in looked_up {
            let field = &fields[&id];
            let lookup = field.lookup.as_ref().expect("a lookup");
            let matches = lookup_matches(&schema, table.id, &fields, lookup, field.data_type)
                .map_err(|reason| DataError::new(format!("Field {}: {reason}", field.name)))?;
            // Creation skips mode-0 lookups when it generates a replacing UUID.
            // Whether FileMaker applies a fallback in this case is unmeasured.
            if lookup.fallback.is_some()
                && matches.iter().any(|key| {
                    let field = &fields[key];
                    field.fresh_key() && field.options.calculation_replaces_existing()
                })
            {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: a lookup fallback with a replacing Get ( UUID ) key is unsupported.",
                        field.name
                    ),
                ));
            }
            if let Some(lookup) = fields.get_mut(&id).and_then(|f| f.lookup.as_mut()) {
                lookup.matches = matches;
            }
        }
        // A formula over related fields reads one relationship from its
        // context, and an auto-entered one is evaluated again when its
        // match fields are set (auto-entry.md, "Calculations over related
        // fields"): they join its dependencies.
        let reading: Vec<u64> = fields
            .iter()
            .filter(|(_, f)| f.formulas().any(|e| !e.related.is_empty()))
            .map(|(id, _)| *id)
            .collect();
        let mut grouped_self_joins = BTreeSet::new();
        for id in reading {
            let field = &fields[&id];
            // Only the auto-entered formula's own reads evaluate it again;
            // a validation calculation's are checked at the write.
            let mut matches = BTreeSet::new();
            let mut global_refs = BTreeMap::new();
            for expression in field.formulas() {
                let context = expression.formula.occurrence().unwrap_or_default();
                let own = field
                    .expression
                    .as_ref()
                    .is_some_and(|e| std::ptr::eq(e, expression));
                for (occurrence, read) in &expression.related {
                    // Claris documents that a global field is readable
                    // from an unrelated table occurrence in the same file.
                    let local_auto_enter = local
                        && own
                        && field.in_row()
                        && field.data_type == DataType::Text
                        && field.options.max_repetitions == 1
                        && field.options.auto_entry_calculation()
                        && !expression.whole.contains(occurrence);
                    let target = schema.table_of(*occurrence).filter(|target| {
                        local_auto_enter
                            && target.id != table.id
                            && schema.table_file(target.id) == schema.table_file(table.id)
                            && schema.path(context, *occurrence).is_none()
                            && target.fields.get(read).is_some_and(|def| {
                                def.class == crate::host::FieldClass::Data
                                    && def.global
                                    && def.stored
                                    && def.repetitions == 1
                                    && def.value_type == ValueType::Text
                            })
                    });
                    if let Some(target) = target {
                        global_refs.insert((*occurrence, *read), (target.id, *read));
                        continue;
                    }
                    let first_auto_enter = own
                        && !field.stored_calculation()
                        && !field.options.calculation_replaces_existing()
                        && expression.uses.get(occurrence).is_some_and(|uses| {
                            uses.first && !uses.placed && uses.aggregated.is_empty()
                        });
                    let hop = related_hop(
                        &schema,
                        &fields,
                        context,
                        (*occurrence, *read),
                        first_auto_enter,
                    )
                    .map_err(|reason| DataError::new(format!("Field {}: {reason}", field.name)))?;
                    if let Some((group, key)) = hop.grouped_keys {
                        grouped_self_joins.insert((id, group, key));
                    }
                    if own {
                        matches.extend(hop.match_fields);
                    }
                }
            }
            if let Some(expression) = fields.get_mut(&id).and_then(|f| f.expression.as_mut()) {
                expression.dependencies.extend(matches);
                expression
                    .related
                    .retain(|read| !global_refs.contains_key(read));
                expression.other_globals.extend(global_refs.into_values());
            }
        }
        // The records read are scanned again by the repetition 1 of each
        // field they carry (`read_lookups`): where a formula reads every
        // record related through a table's occurrence, no relationship the
        // formulas read that table through may sort by a later repetition.
        let mut wholly = BTreeSet::new();
        let mut sorted_later = Vec::new();
        for field in fields.values() {
            for expression in field.formulas() {
                let context = expression.formula.occurrence().unwrap_or_default();
                for (occurrence, _) in &expression.related {
                    let Some(to) = schema.table_of(*occurrence).map(|t| t.id) else {
                        continue;
                    };
                    if expression.whole.contains(occurrence) {
                        wholly.insert(to);
                    }
                    let later = schema
                        .path(context, *occurrence)
                        .and_then(|p| p.first().cloned())
                        .and_then(|hop| hop.sort)
                        .is_some_and(|sort| sort.fields.iter().any(|e| e.field.repetition > 1));
                    if later {
                        sorted_later.push((to, field.name.clone()));
                    }
                }
            }
        }
        if let Some((_, name)) = sorted_later.iter().find(|(to, _)| wholly.contains(to)) {
            return Err(DataError::new(format!(
                "Field {name}: calculations over every related record are unsupported where a \
                 relationship to that table sorts by a later repetition."
            )));
        }
        // Formulas read the table through their context occurrence.
        for occurrence in contexts {
            match schema.occurrence(occurrence) {
                Some(o) if o.table != table.id => {
                    return Err(DataError::new(format!(
                        "A calculation of table {} is evaluated from another table's occurrence.",
                        table.name
                    )));
                }
                Some(_) => {}
                None => schema.add_occurrence(occurrence, table.name.clone(), table.id),
            }
        }
        let calculations: BTreeSet<u64> = table
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Calculation)
            .map(|f| f.id)
            .collect();
        // The inputs of the unstored calculations each auto-entered
        // (`true`) or validation (`false`) formula reads, by field, and
        // each calculation's own (`UnstoredGraph`).
        let mut through: BTreeMap<(u64, bool), UnstoredReads> = BTreeMap::new();
        let mut unstored = UnstoredGraph::new(&unstored_formulas, &schema, table.id);
        for (id, field) in &fields {
            // A stored calculation's formula, or an auto-entered or
            // validation formula, which may read a global field (5.17).
            let stored_calculation = calculations.contains(id);
            let formulas = field
                .expression
                .iter()
                .map(|e| (e, stored_calculation, true))
                .chain(field.validation.iter().map(|e| (e, false, false)));
            for (expression, calculation, own) in formulas {
                // How many times it reads each field, counted once a
                // dependency is an unstored calculation.
                let mut counts = None;
                check_custom_functions(
                    &schema,
                    &expression.formula.polish,
                    !calculation,
                    false,
                    &mut BTreeSet::new(),
                )
                .map_err(|reason| DataError::new(format!("Field {}: {reason}", field.name)))?;
                if context_selectors(&schema, &expression.formula.polish)
                    .contains(&selector::CURRENT_TIME_UTC_MILLISECONDS)
                    && !(local
                        && own
                        && !calculation
                        && !field.auto_enters_global()
                        && matches!(field.data_type, DataType::Text | DataType::Number))
                {
                    return Err(DataError::with_code(
                        error::UNAVAILABLE,
                        format!(
                            "Field {}: Get ( CurrentTimeUTCMilliseconds ) requires local Text or Number auto-entry.",
                            field.name
                        ),
                    ));
                }
                for dependency in &expression.dependencies {
                    let Some(referenced) = fields.get(dependency) else {
                        return Err(DataError::new(format!(
                            "Field {} references an unavailable field.",
                            field.name
                        )));
                    };
                    let def = schema
                        .table(table.id)
                        .and_then(|t| t.fields.get(dependency));
                    // A container is read through the write's
                    // [`ContainerReader`] (its data, not its cell's text).
                    let global = referenced.options.options & GLOBAL != 0;
                    // An unstored calculation an auto-entered or validation
                    // formula reads is evaluated on the write's record, as
                    // FileMaker evaluates it (script oracle s44); a stored
                    // calculation over one would be unstored itself.
                    if !calculation && !global && def.is_some_and(|d| d.is_unstored_calculation()) {
                        let reads =
                            unstored
                                .reads(*dependency, &mut Vec::new())
                                .map_err(|reason| {
                                    DataError::new(format!(
                                "Field {}: it reads an unstored calculation a write cannot \
                                 evaluate: {reason}",
                                field.name
                            ))
                                })?;
                        let times = counts
                            .get_or_insert_with(|| field_read_counts(&expression.formula.polish))
                            .get(dependency)
                            .copied()
                            .unwrap_or(1);
                        let entry = through.entry((*id, own)).or_default();
                        entry.inputs.extend(reads.inputs);
                        entry.calculations.extend(reads.calculations);
                        entry.contexts.extend(reads.contexts);
                        entry.cost = entry.cost.saturating_add(times.saturating_mul(reads.cost));
                        if entry.cost > MAX_UNSTORED_EVALUATIONS {
                            return Err(DataError::new(format!(
                                "Field {}: it evaluates unstored calculations too many times.",
                                field.name
                            )));
                        }
                        continue;
                    }
                    let unsupported = (global
                        && (calculation
                            || referenced.data_type == DataType::Container
                            || referenced.options.max_repetitions > 1))
                        || def.is_some_and(|d| {
                            d.class == crate::host::FieldClass::Summary
                                || d.is_unstored_calculation()
                        });
                    if unsupported {
                        return Err(DataError::new(format!(
                            "Field {} depends on unsupported non-text storage.",
                            field.name
                        )));
                    }
                }
            }
        }
        // A change of an input of an unstored calculation a formula reads
        // evaluates an auto-entered one again (script oracle s44); the
        // inputs also order the formula after them.
        for ((id, own), reads) in through {
            for occurrence in &reads.contexts {
                schema.add_occurrence(*occurrence, table.name.clone(), table.id);
            }
            let Some(field) = fields.get_mut(&id) else {
                continue;
            };
            let expression = if own {
                field.expression.as_mut()
            } else {
                field.validation.as_mut()
            };
            if let Some(expression) = expression {
                for input in reads.inputs {
                    if expression.dependencies.insert(input) {
                        expression.through_unstored.insert(input);
                    }
                }
                expression.through_formulas.extend(
                    reads
                        .calculations
                        .iter()
                        .filter_map(|c| unstored_formulas.get(c)?.as_ref().ok())
                        .map(|e| Arc::clone(&e.formula)),
                );
            }
        }
        // A message calculation reading what its one-record evaluation
        // cannot give (a summary, an unstored calculation, a container, a
        // repeating or container global, a field the table lacks) is not
        // evaluated: the refusal keeps OpenMaker's wording.
        let unanswerable: Vec<u64> = fields
            .iter()
            .filter(|(_, field)| {
                let Some(CustomMessage::Calculation(Some(expression))) = &field.message else {
                    return false;
                };
                expression.dependencies.iter().any(|dependency| {
                    let def = schema
                        .table(table.id)
                        .and_then(|t| t.fields.get(dependency));
                    fields.get(dependency).is_none_or(|referenced| {
                        referenced.data_type == DataType::Container
                            || referenced.options.options & GLOBAL != 0
                                && referenced.options.max_repetitions > 1
                    }) || def.is_some_and(|d| {
                        d.class == crate::host::FieldClass::Summary || d.is_unstored_calculation()
                    })
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in unanswerable {
            if let Some(field) = fields.get_mut(&id) {
                field.message = Some(CustomMessage::Calculation(None));
            }
        }
        let evaluation_order = evaluation_order(&fields)?;
        Ok(Self {
            table: table.id,
            fields,
            evaluation_order,
            grouped_self_joins,
            schema: Arc::new(schema),
        })
    }

    /// The file's calculation schema with this table's current fields: the
    /// occurrences and relationships its lookups read through.
    pub fn schema(&self) -> &CalcSchema {
        &self.schema
    }

    /// The base table the rules are of.
    pub fn table(&self) -> u64 {
        self.table
    }

    /// These values belong to the file's globals, outside the new record.
    pub fn has_global_auto_entry(&self, entry: Entry) -> bool {
        !entry.skip_auto_enter && self.fields.values().any(PlannedField::auto_enters_global)
    }

    /// Prepare the constant and calculated defaults, and the serial fields
    /// for which New Record must take a value. A calculated default with
    /// replacement off keeps a nonempty global. Without `allow_nonempty`,
    /// a nonempty global refuses the creation before any serial is reserved.
    pub fn new_record_globals(
        &self,
        entry: Entry,
        globals: &RecordValues,
        environment: &RecordEnvironment,
        allow_nonempty: bool,
    ) -> Result<(crate::GlobalChanges, Vec<Serial>), DataError> {
        let mut values = crate::GlobalChanges::new();
        let mut serials = Vec::new();
        if entry.skip_auto_enter {
            return Ok((values, serials));
        }
        let session = environment.session();
        let mut remaining = session.calculation_budget();
        let row = BTreeMap::new();
        for (&id, field) in &self.fields {
            if !field.auto_enters_global() {
                continue;
            }
            let existing = globals.get(id, 1);
            let nonempty = existing.is_some_and(|value| !value.is_empty());
            if nonempty && !allow_nonempty {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: auto-entry into a nonempty global requires New Record in a local file.",
                        field.name
                    ),
                ));
            }
            if let Some(serial) = field.serial {
                serials.push(serial);
            } else if let Some(value) = &field.constant {
                values.insert((id, 1), value.clone());
            } else if let Some(expression) = &field.expression {
                if nonempty && !field.options.calculation_replaces_existing() {
                    continue;
                }
                let value = match field.global_text_self {
                    GlobalTextSelf::Supported if allow_nonempty => self
                        .run(
                            id,
                            expression,
                            &row,
                            &session,
                            Some(existing.cloned().unwrap_or_else(|| Value::text(""))),
                            Some(ValueType::Text),
                            InRepetition::Calculation(1),
                            &mut remaining,
                        )
                        .map(|(_, text)| text),
                    GlobalTextSelf::Unused => {
                        self.evaluate_repetition(id, 1, expression, &row, &session, &mut remaining)
                    }
                    _ => return Err(global_text_self::unavailable(&field.name)),
                }
                .map_err(RunError::into_error)?;
                values.insert((id, 1), value);
            }
        }
        Ok((values, serials))
    }

    /// The serial number fields of a new record written by `entry`: each
    /// one, unless the write skips auto-enter. The host reserves a value
    /// for each (op72 advancing the field's next value, write-path.md 3.2)
    /// and puts it in the write's [`RecordEnvironment::serials`]: those
    /// generated on creation when it creates the record, as FileMaker Pro
    /// does at New Record (write-path.md 1.1), so a script reads them
    /// before the commit; the others when it computes the record. A field
    /// the write sets takes a value too, which the write's value replaces
    /// ([`Self::keeps_serial`]): the sequence moves past it, as in
    /// FileMaker, where the value was taken before the script set the
    /// field.
    pub fn serials_for(&self, mode: WriteMode, entry: Entry) -> Vec<Serial> {
        if mode != WriteMode::Create || entry.skip_auto_enter {
            return Vec::new();
        }
        self.fields
            .values()
            .filter(|field| !field.auto_enters_global())
            .filter_map(|field| field.serial)
            .collect()
    }

    /// Whether a new record keeps the value the write sets in serial field
    /// `id` instead of taking a serial number: a script's or an import's
    /// value is entered after the record was created and its serial
    /// assigned, so it replaces the serial, as Set Field's does in
    /// FileMaker. A user's creation sets values only when it duplicates a
    /// record, whose auto-entered values FileMaker enters anew
    /// (`duplicate-record-request.html`).
    fn keeps_serial(entry: Entry, changes: &BTreeMap<u64, Option<String>>, id: u64) -> bool {
        entry.script && entry.sets(changes, id)
    }

    /// Whether a field of the table auto-enters a looked-up value.
    pub fn has_lookups(&self) -> bool {
        self.fields.values().any(|f| {
            f.lookup.is_some()
                || f.formulas()
                    .any(|e| !e.related.is_empty() || !e.other_globals.is_empty())
        })
    }

    /// The tables (of this file's schema) the write's lookups and related
    /// reads read records of: those whose held records a write in a
    /// transaction must see ([`RecordEnvironment::held`]).
    pub fn related_tables(&self) -> BTreeSet<u64> {
        let lookups = self
            .fields
            .values()
            .filter_map(|f| f.lookup.as_ref())
            .map(|lookup| lookup.to);
        let reads = self.related_reads().into_keys().map(|(_, to)| to);
        lookups
            .chain(reads)
            .filter_map(|occurrence| self.schema.table_of(occurrence).map(|t| t.id))
            .collect()
    }

    /// Fields a held write can still compute. Literal changes are already
    /// present in its overlay; these values must not be read there until
    /// preparation supplies their final row. A reserved serial is known
    /// only when the provisional cell agrees with it; a script override
    /// and a write skipping auto-entry keep their literal cells instead.
    pub fn unprepared_fields(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
        serials: &BTreeMap<u64, String>,
    ) -> Result<BTreeSet<u64>, DataError> {
        let (row, mut dirty) = self.written(existing, changes, entry)?;
        if mode == WriteMode::Update
            && dirty.is_empty()
            && !self.fields.iter().any(|(id, f)| {
                f.lookup
                    .as_ref()
                    .is_some_and(|l| self.fires(*id, l, mode, &dirty, changes, entry))
            })
        {
            return Ok(BTreeSet::new());
        }
        let mut pending = BTreeSet::new();
        for (id, field) in &self.fields {
            if entry.skip_auto_enter || entry.keeps(changes, *id) {
                continue;
            }
            if field
                .options
                .auto_entry_variable()
                .is_some_and(|v| mode == WriteMode::Create || v.is_modification() == Some(true))
                || mode == WriteMode::Create
                    && (field.constant.is_some()
                        || field.serial.is_some()
                            && !Self::keeps_serial(entry, changes, *id)
                            && serials.get(id).is_none_or(|reserved| {
                                changes
                                    .get(id)
                                    .is_some_and(|value| value.as_ref() != Some(reserved))
                            }))
            {
                pending.insert(*id);
                dirty.insert(*id);
            }
        }
        for id in &self.evaluation_order {
            let field = &self.fields[id];
            let lookup = field
                .lookup
                .as_ref()
                .is_some_and(|lookup| self.fires(*id, lookup, mode, &dirty, changes, entry));
            let expression = field.expression.as_ref().is_some_and(|expression| {
                (field.stored_calculation() || !entry.skip_auto_enter && !entry.keeps(changes, *id))
                    && (mode == WriteMode::Create
                        || expression.dependencies.iter().any(|d| dirty.contains(d)))
                    && (field.stored_calculation()
                        || field.options.calculation_replaces_existing()
                        || row.get(id).is_none_or(String::is_empty))
            });
            if lookup || expression {
                pending.insert(*id);
                dirty.insert(*id);
            }
        }
        Ok(pending)
    }

    /// Whether a held record's rule reads are final, before preparing it.
    /// Only deterministic preview values are computed to discover final
    /// match keys. This reserves nothing, validates nothing and draws no
    /// random value. Unknown peer values are never used for those keys.
    pub fn held_reads_ready(
        &self,
        (mode, existing, changes): (
            WriteMode,
            &BTreeMap<u64, String>,
            &BTreeMap<u64, Option<String>>,
        ),
        environment: &RecordEnvironment,
        entry: Entry,
        unknown: UnknownValue<'_>,
    ) -> Result<bool, DataError> {
        let pending =
            self.unprepared_fields(mode, existing, changes, entry, &environment.serials)?;
        let masked = self.masked_fields(unknown, &environment.lookups);
        let fields = self
            .preview_fields(true)
            .difference(&masked)
            .copied()
            .collect();
        let computed =
            self.compute_inner(mode, existing, changes, environment, entry, Some(&fields))?;
        let validators: BTreeSet<u64> = self
            .fields
            .iter()
            .filter(|(id, field)| {
                field.validated(entry)
                    && (field.options.validation_options & VALIDATE_CALCULATION_ALWAYS != 0
                        || pending.contains(id)
                        || self.cells(**id).any(|cell| {
                            computed.row.get(&cell).map_or("", String::as_str)
                                != existing.get(&cell).map_or("", String::as_str)
                        }))
            })
            .map(|(id, _)| *id)
            .collect();
        // An inactive formula does not need final match keys. In particular
        // an unchanged field's validation must not demand a random key the
        // write will draw only during final preparation.
        let active: BTreeSet<(u64, u64)> = self
            .fields
            .iter()
            .flat_map(|(id, field)| {
                field
                    .expression
                    .iter()
                    .filter(|_| pending.contains(id))
                    .chain(field.validation.iter().filter(|_| validators.contains(id)))
            })
            .flat_map(|expression| {
                expression
                    .formula
                    .occurrence()
                    .into_iter()
                    .flat_map(|context| {
                        expression
                            .related
                            .iter()
                            .map(move |(occurrence, _)| (context, *occurrence))
                    })
            })
            .collect();
        let mut ready = true;
        let mut keys_for = |context, occurrence, matches: &[u64], read: Option<&Vec<String>>| {
            if matches.iter().any(|id| {
                computed.deferred.contains(id) || pending.contains(id) && !fields.contains(id)
            }) {
                ready = false;
                return;
            }
            let keys: Vec<_> = matches
                .iter()
                .map(|id| computed.row.get(id).cloned().unwrap_or_default())
                .collect();
            if read != Some(&keys) {
                ready = false;
                if let Some(needs) = &environment.related_needs {
                    needs.need((context, occurrence), keys);
                }
            }
        };
        for ((context, occurrence), read) in self.related_reads() {
            if !active.contains(&(context, occurrence)) {
                continue;
            }
            keys_for(
                context,
                occurrence,
                &read.matches,
                environment
                    .lookups
                    .related
                    .get(&(context, occurrence))
                    .map(|(keys, _)| keys),
            );
        }
        let (_, dirty) = self.written(existing, changes, entry)?;
        for (id, field) in &self.fields {
            if let Some(lookup) = field
                .lookup
                .as_ref()
                .filter(|lookup| self.fires(*id, lookup, mode, &dirty, changes, entry))
            {
                keys_for(
                    lookup.from,
                    lookup.to,
                    &lookup.matches,
                    environment.lookups.copied.get(id).map(|(keys, _)| keys),
                );
            }
        }
        Ok(ready
            && masked.is_disjoint(&pending)
            && !self
                .reading_unknown(unknown, &environment.lookups, true)
                .iter()
                .any(|id| validators.contains(id)))
    }

    /// Each occurrence the table's formulas read related fields through,
    /// by the context they read it from: the relationship's match fields
    /// of this table and the fields read there and matched on.
    fn related_reads(&self) -> BTreeMap<(u64, u64), RelatedRead> {
        let mut reads: BTreeMap<(u64, u64), RelatedRead> = BTreeMap::new();
        for field in self.fields.values() {
            for expression in field.formulas() {
                let Some(context) = expression.formula.occurrence() else {
                    continue;
                };
                for (occurrence, read) in &expression.related {
                    let Some(hop) = self
                        .schema
                        .path(context, *occurrence)
                        .and_then(|p| p.first().cloned())
                    else {
                        continue;
                    };
                    // Its match and sort fields too: what the evaluation's
                    // rescan needs to find and order the records read
                    // (`run`).
                    let entry =
                        reads
                            .entry((context, *occurrence))
                            .or_insert_with(|| RelatedRead {
                                matches: hop.predicates.iter().map(|p| p.left).collect(),
                                fields: hop
                                    .predicates
                                    .iter()
                                    .map(|p| p.right)
                                    .chain(sort_fields(&hop))
                                    .collect(),
                                whole: false,
                            });
                    entry.fields.insert(*read);
                    entry.whole |= expression.whole.contains(occurrence);
                }
            }
        }
        reads
    }

    /// Read the local Text globals needed by this plan. An unreadable or
    /// styled value is omitted so only an evaluation that reads it fails.
    pub fn read_other_globals(
        &self,
        mut read: impl FnMut(u64) -> Option<Arc<RecordValues>>,
    ) -> Option<OtherGlobalValues> {
        let mut tables = BTreeMap::<u64, BTreeSet<u64>>::new();
        for (table, field) in self
            .fields
            .values()
            .flat_map(PlannedField::formulas)
            .flat_map(|expression| expression.other_globals.iter().copied())
        {
            tables.entry(table).or_default().insert(field);
        }
        if tables.is_empty() {
            return None;
        }
        let mut texts = BTreeMap::new();
        for (table, fields) in tables {
            let Some(values) = read(table) else {
                continue;
            };
            for field in fields {
                let text = match values.get(field, 1) {
                    None => String::new(),
                    Some(Value::Text(value)) if value.is_plain() => value.as_str().to_owned(),
                    _ => continue,
                };
                texts.insert((table, field), text);
            }
        }
        Some(OtherGlobalValues(Arc::new(texts)))
    }

    /// What the lookups `entry`'s write fires copy (auto-entry.md,
    /// Lookups), read from `source`, the host's records: for each lookup
    /// one of whose match fields the write changes, the copied field on the
    /// first record related to the record as the write leaves it (in
    /// creation order), or nothing related. The host puts them in the
    /// write's [`RecordEnvironment::lookups`]; a write that fires a lookup
    /// without its value is refused.
    pub fn read_lookups(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
        source: &dyn CalcSource,
    ) -> Result<Lookups, DataError> {
        self.read_lookups_with(
            mode,
            existing,
            changes,
            entry,
            source,
            (&BTreeMap::new(), Written::Unknown),
        )
    }

    /// [`Self::read_lookups`], reading the related records of each
    /// (context, occurrence) in `keyed` with those match keys: the ones the
    /// write computed, which a previous computation asked for
    /// ([`RelatedNeeds`], #1035), instead of the ones it leaves before it is
    /// computed. `written` is the record the write computes: its stored row
    /// is left out of its own table's related records.
    pub fn read_lookups_with(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
        source: &dyn CalcSource,
        (keyed, written): (&BTreeMap<(u64, u64), Vec<String>>, Written),
    ) -> Result<Lookups, DataError> {
        let mut lookups = Lookups {
            written,
            ..Lookups::default()
        };
        if !self.has_lookups() {
            return Ok(lookups);
        }
        lookups.other_globals = self
            .read_other_globals(|table| source.globals(table).ok())
            .unwrap_or_default();
        let (row, dirty) = self.written(existing, changes, entry)?;
        for (id, field) in &self.fields {
            let Some(lookup) = &field.lookup else {
                continue;
            };
            // A final-key demand can come from a key recalculated by an
            // audit field, even when the original inputs did not fire it.
            if !self.lookup_allowed(*id, lookup, mode, changes, entry)
                || !keyed.contains_key(&(lookup.from, lookup.to))
                    && !self.fires(*id, lookup, mode, &dirty, changes, entry)
            {
                continue;
            }
            let keys = match keyed.get(&(lookup.from, lookup.to)) {
                Some(keys) => keys.clone(),
                // Wait for calculated keys to be evaluated. A caller may
                // supply those final keys on its next read.
                None if lookup.matches.iter().any(|m| {
                    let field = &self.fields[m];
                    field.stored_calculation()
                        || field.options.auto_entry_calculation() && !field.fresh_key()
                }) =>
                {
                    continue
                }
                None => Self::lookup_keys(lookup, &row),
            };
            let found = self.first_related(lookup, &keys, source).map_err(|e| {
                let reason = if e == om_calc::CalcError::CANCELED {
                    "the script was stopped while its related record was read"
                } else if e.code() == error::UNAVAILABLE {
                    "the related value does not read as its field's type, which OpenMaker \
                     does not copy yet"
                } else {
                    "its related record could not be read"
                };
                DataError::with_code(
                    e.code(),
                    format!("Field {}: {reason}. No values were changed.", field.name),
                )
            })?;
            if let Some((record, _)) = &found {
                lookups.copied_from.insert(*id, *record);
            }
            lookups
                .copied
                .insert(*id, (keys, found.map(|(_, value)| value)));
        }
        let reads = self.related_reads();
        // The records of one table read through several occurrences meet in
        // the evaluation's one source (`run`), where each occurrence is
        // scanned again: each record carries what every read of its table
        // needs, so that each scan finds and orders them as the host did.
        let mut needed: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        for ((_, occurrence), read) in &reads {
            if let Some(table) = self.schema.table_of(*occurrence) {
                needed
                    .entry(table.id)
                    .or_default()
                    .extend(read.fields.iter().copied());
            }
        }
        let mut related = RelatedRecords::new();
        for ((context, occurrence), read) in reads {
            let keys: Vec<String> = match keyed.get(&(context, occurrence)) {
                Some(keys) => keys.clone(),
                None => read
                    .matches
                    .iter()
                    .map(|m| row.get(m).cloned().unwrap_or_default())
                    .collect(),
            };
            let own = self
                .schema
                .table_of(occurrence)
                .is_some_and(|t| t.id == self.table);
            let found = self
                .related_records(
                    (context, occurrence),
                    (&read.matches, &keys),
                    (read.whole, own.then(|| written.id()).flatten()),
                    source,
                )
                .map_err(|e| {
                    DataError::with_code(
                        e.code(),
                        "A related record a calculation reads could not be read. No values \
                         were changed.",
                    )
                })?
                .ok_or_else(|| {
                    DataError::with_code(
                        error::UNAVAILABLE,
                        format!(
                            "A calculation reads more than {MAX_WHOLE_READ} related records, \
                             more than OpenMaker reads for a write. No values were changed."
                        ),
                    )
                })?;
            let cx = Ctx::new(&self.schema.locale, crate::host::session::today(0));
            let carried = self
                .schema
                .table_of(occurrence)
                .and_then(|t| needed.get(&t.id))
                .unwrap_or(&read.fields);
            let found = found
                .into_iter()
                .map(|(record, values)| {
                    let fields = carried
                        .iter()
                        .filter_map(|f| {
                            let value = values.get(*f, 1)?;
                            Some((*f, value.to_text(&cx).into_string()))
                        })
                        .collect();
                    (record, fields)
                })
                .collect();
            related.insert((context, occurrence), (keys, found));
        }
        lookups.related = Arc::new(related);
        Ok(lookups)
    }

    /// Whether the write fires lookup field `id`: it sets a match field,
    /// also to the value it had (auto-entry.md, Lookups, battery s41), and
    /// performs auto-entry, and does not set the field itself (a value the
    /// write sets is kept: which came last is not known). A new record's
    /// key auto-entered as a replacing `Get ( UUID )` is replaced by a new
    /// UUID after the lookups, whatever the write gave it (a duplicate's
    /// copied key), unless the write keeps what it sets (an import): no
    /// related record holds the new key, so a lookup with such a match
    /// field, alone or with others, fires nothing.
    #[allow(clippy::too_many_arguments)]
    fn fires(
        &self,
        id: u64,
        lookup: &Lookup,
        mode: WriteMode,
        dirty: &BTreeSet<u64>,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
    ) -> bool {
        self.lookup_allowed(id, lookup, mode, changes, entry)
            && lookup.matches.iter().any(|m| {
                if dirty.contains(m) || changes.contains_key(m) {
                    return true;
                }
                // An auto-entered key can retain its old value even after
                // an input edit. A stored calculation still follows its inputs.
                if !self.fields[m].stored_calculation() {
                    return false;
                }
                let inputs = self.match_inputs(*m);
                mode == WriteMode::Create
                    || changes.keys().any(|cell| {
                        self.cell_field(*cell)
                            .is_some_and(|(field, _)| inputs.contains(&field))
                    })
            })
    }

    fn lookup_allowed(
        &self,
        id: u64,
        lookup: &Lookup,
        mode: WriteMode,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
    ) -> bool {
        let replaced = |m: &u64| {
            mode == WriteMode::Create
                && !entry.keeps(changes, *m)
                && self
                    .fields
                    .get(m)
                    .is_some_and(|f| f.fresh_key() && f.options.options & 0x80 != 0)
        };
        // Any match field replaced so: the whole relationship matches
        // nothing (its predicates are all `=`).
        !entry.skip_auto_enter && !entry.sets(changes, id) && !lookup.matches.iter().any(replaced)
    }

    /// Whether a change of `cell` can change what the rules compute for a
    /// record: its field has rules of its own (an auto-entered value, a
    /// lookup, a calculation), a formula or a lookup reads it, or it decides
    /// a relationship a formula reads through. A cell of no known field
    /// answers yes.
    pub fn computes_from(&self, cell: u64) -> bool {
        let Some((field, _)) = self.cell_field(cell) else {
            return true;
        };
        if self.has_rules(cell) {
            return true;
        }
        self.fields.values().any(|f| {
            f.dependencies().any(|d| *d == field)
                || f.expression.as_ref().is_some_and(|e| {
                    let Some(context) = e.formula.occurrence() else {
                        return false;
                    };
                    e.related
                        .iter()
                        .map(|(occurrence, _)| *occurrence)
                        .chain(e.whole.iter().copied())
                        .any(|occurrence| {
                            self.deciding_fields(context, occurrence).contains(&field)
                        })
                })
        })
    }

    /// Whether `cell`'s field has rules of its own that give its value: an
    /// auto-entered value, a lookup or a calculation.
    pub fn has_rules(&self, cell: u64) -> bool {
        self.cell_field(cell)
            .is_some_and(|(field, _)| !self.fields[&field].given())
    }

    /// Relookup Field Contents' target, field `name` of the table: one a
    /// script may set (a normal field, not global), that a lookup the rules
    /// perform matches through, an editable input of a calculated match
    /// field included ([`Self::is_lookup_match`]), and that enters no value
    /// of its own: writing it back would let an auto-entry (a variable such
    /// as the modifying account, a serial, a constant, a lookup, a
    /// calculation) replace it. Its id, or why not: 102 for no such field,
    /// 201 for one a script cannot set (a calculation, a summary, a
    /// global), 3 for one no lookup matches through or one with
    /// auto-entry (OpenMaker's codes; FileMaker's are unmeasured).
    pub fn relookup_target(&self, name: &str) -> Result<u64, DataError> {
        let (&id, field) = self
            .fields
            .iter()
            .find(|(_, f)| f.name == name)
            .ok_or_else(|| {
                DataError::with_code(
                    error::FIELD_MISSING,
                    format!("The table has no field named “{name}”."),
                )
            })?;
        if !field.settable_by_script() {
            return Err(DataError::with_code(
                error::FIELD_LOCKED,
                format!(
                    "“{name}” is not a field a script sets: relook up through an editable \
                     field the match uses."
                ),
            ));
        }
        if !self.is_lookup_match(id) {
            return Err(DataError::with_code(
                error::UNAVAILABLE,
                format!("No lookup of this table matches through “{name}”."),
            ));
        }
        let enters = field.options.auto_entry_variable().is_some()
            || field.options.auto_entry_serial()
            || field.constant.is_some()
            || field.lookup.is_some()
            || field.expression.is_some();
        if enters {
            return Err(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "“{name}” enters a value of its own, which writing it again could \
                     replace: OpenMaker does not relook up through it."
                ),
            ));
        }
        Ok(id)
    }

    /// Whether `field` is a match field of a lookup the rules perform: a
    /// script that sets it, also to the value it has, fires the lookup
    /// (auto-entry.md, Lookups, battery s41).
    pub fn is_lookup_match(&self, field: u64) -> bool {
        let Some((field, _)) = self.cell_field(field) else {
            return false;
        };
        self.fields
            .values()
            .filter_map(|f| f.lookup.as_ref())
            .any(|lookup| {
                lookup
                    .matches
                    .iter()
                    .any(|m| self.match_inputs(*m).contains(&field))
            })
    }

    // Include inputs of stored calculations for same-value writes and
    // Relookup. Auto-enter keys require a value change, so their inputs
    // cannot relookup by writing the current value back.
    fn match_inputs(&self, field: u64) -> BTreeSet<u64> {
        let mut inputs = BTreeSet::from([field]);
        let mut pending = vec![field];
        while let Some(id) = pending.pop() {
            let field = &self.fields[&id];
            if !field.stored_calculation() {
                continue;
            }
            for dependency in field.dependencies() {
                if inputs.insert(*dependency) {
                    pending.push(*dependency);
                }
            }
        }
        inputs
    }

    /// What `lookup`'s match fields hold in `row`.
    fn lookup_keys(lookup: &Lookup, row: &BTreeMap<u64, String>) -> Vec<String> {
        lookup
            .matches
            .iter()
            .map(|m| row.get(m).cloned().unwrap_or_default())
            .collect()
    }

    /// The first record `lookup` relates to a record whose match fields
    /// hold `keys`, through the relationship the scan of every calculation
    /// follows (`related_by_scan`), and its copied field; `None` when none
    /// is.
    fn first_related(
        &self,
        lookup: &Lookup,
        keys: &[String],
        source: &dyn CalcSource,
    ) -> Result<Option<(u64, String)>, om_calc::CalcError> {
        let cx = Ctx::new(&self.schema.locale, crate::host::session::today(0));
        let found = self
            .related_records(
                (lookup.from, lookup.to),
                (&lookup.matches, keys),
                (false, None),
                source,
            )?
            .and_then(|records| records.into_iter().next());
        let Some((record, values)) = found else {
            return Ok(None);
        };
        // A related value that does not read as its field's type (a date
        // field holding other text; OpenMaker Server's scripts read it as
        // `?`) is not copied: the write is refused rather than store text
        // FileMaker would not copy.
        let declared = self
            .schema
            .table_of(lookup.to)
            .and_then(|table| table.fields.get(&lookup.copied))
            .map(|def| def.value_type);
        let value = values.get(lookup.copied, 1);
        let unreadable = value.is_some_and(|value| {
            value.is_invalid()
                || (declared == Some(ValueType::Date)
                    && !value.is_empty()
                    && !matches!(value, Value::Date(_) | Value::DateText(..)))
        });
        if unreadable {
            return Err(om_calc::CalcError::from_code(3).expect("nonzero"));
        }
        let value = value
            .map(|value| value.to_text(&cx).into_string())
            .unwrap_or_default();
        Ok(Some((record, value)))
    }

    /// The records of `to` related to the record as the write leaves it,
    /// whose `matches` hold `keys`, through the relationship the scan of
    /// every calculation follows (`related_by_scan`, in the relationship's
    /// order), with their values: the first, or with `whole` every one;
    /// `None` when there are more than [`MAX_WHOLE_READ`]. Record `exclude`
    /// of `to`'s table is left out (the written record's stored row).
    fn related_records(
        &self,
        (from, to): (u64, u64),
        (matches, keys): (&[u64], &[String]),
        (whole, exclude): (bool, Option<u64>),
        source: &dyn CalcSource,
    ) -> Result<Option<ReadRecords>, om_calc::CalcError> {
        let related = self
            .schema
            .table_of(to)
            .ok_or_else(crate::host::relationship_missing)?
            .id;
        let cx = Ctx::new(&self.schema.locale, crate::host::session::today(0));
        // Each key as its field's type, so a number matches by its value
        // (`5`, `5.0`), as a number index keys it (query-program.md 7.3).
        let own = self.schema.table(self.table);
        let mut record = RecordValues::default();
        for (field, key) in matches.iter().zip(keys) {
            let value_type = own
                .and_then(|t| t.fields.get(field))
                .map_or(ValueType::Text, |d| d.value_type);
            record.set(*field, 1, stored_value(key, value_type, &cx));
        }
        let path = self
            .schema
            .path(from, to)
            .ok_or_else(crate::host::relationship_missing)?;
        // Only equality can use an exact value-index lookup. A preceding
        // inequality is checked by the scan over those candidates.
        let key = path.first().and_then(|hop| {
            hop.predicates
                .iter()
                .zip(keys)
                .find(|(predicate, _)| predicate.op == crate::meta::JoinOp::Equal)
                .map(|(predicate, value)| (predicate.right, value.clone()))
        });
        let overlay = LookupSource {
            inner: source,
            table: self.table,
            record: Arc::new(record),
            related,
            key,
            exclude,
        };
        let all = crate::host::related_by_scan(&overlay, &self.schema, from, WRITTEN, to, &cx)?;
        let records = match whole {
            false => &all[..all.len().min(1)],
            true if all.len() > MAX_WHOLE_READ => return Ok(None),
            true => &all[..],
        };
        if whole {
            overlay.prefetch_records(related, records);
        }
        records
            .iter()
            .map(|&record| {
                let values = overlay.record(related, record)?.unwrap_or_default();
                Ok((record, values))
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }

    /// Whether field `id` of the table is an unstored calculation.
    fn is_unstored(&self, id: u64) -> bool {
        self.schema
            .table(self.table)
            .and_then(|t| t.fields.get(&id))
            .is_some_and(|f| f.is_unstored_calculation())
    }

    /// What unstored calculation `id` reads as on `row` for `reader`, a
    /// formula of the table that reads it: evaluated in the reader's
    /// context occurrence, with its own "Do not evaluate if all referenced
    /// fields are empty" (`FileCalc::read_field`).
    fn unstored_value(
        &self,
        id: u64,
        reader: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        remaining: &mut u64,
    ) -> Result<String, RunError> {
        let occurrence = reader.formula.occurrence().unwrap_or_default();
        let read = Expression {
            formula: Arc::new(Formula {
                context: reader.formula.context.clone(),
                formatted: None,
                polish: Expr::Field(om_formula::Field::new(occurrence, id, true)),
            }),
            dependencies: BTreeSet::from([id]),
            related: BTreeSet::new(),
            other_globals: BTreeSet::new(),
            whole: BTreeSet::new(),
            uses: BTreeMap::new(),
            through_unstored: BTreeSet::new(),
            through_formulas: Vec::new(),
        };
        self.evaluate_repetition(id, 1, &read, row, session, remaining)
    }

    /// The empty-input rule reads direct references, including the values
    /// of unstored calculations, and charges those probes to the reader.
    fn all_inputs_empty(
        &self,
        expression: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        globals: Option<&RecordValues>,
        remaining: &mut u64,
    ) -> Result<bool, RunError> {
        if expression.dependencies.is_empty() && expression.other_globals.is_empty() {
            return Ok(false);
        }
        for field in &expression.other_globals {
            let text = session
                .other_globals
                .0
                .get(field)
                .ok_or_else(missing_other_globals)?;
            if !text.is_empty() {
                return Ok(false);
            }
        }
        for id in expression
            .dependencies
            .difference(&expression.through_unstored)
        {
            let empty = match globals {
                Some(globals) if self.is_global(*id) => {
                    globals.get(*id, 1).is_none_or(Value::is_empty)
                }
                _ if self.is_unstored(*id) => {
                    match self.unstored_value(*id, expression, row, session, remaining) {
                        Ok(value) => value.is_empty(),
                        Err(error @ RunError::Exhausted(_)) => return Err(error),
                        // The formula itself decides how to handle a failed read.
                        Err(_) => false,
                    }
                }
                _ => row.get(id).map_or(true, String::is_empty),
            };
            if !empty {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether field `id` of the table has global storage.
    fn is_global(&self, id: u64) -> bool {
        self.schema
            .table(self.table)
            .and_then(|t| t.fields.get(&id))
            .is_some_and(|f| f.global)
    }

    /// The session's global values when `expression`, field `field`'s,
    /// reads a global field (5.17); refused when the write was not given
    /// them.
    fn session_globals(
        &self,
        field: u64,
        expression: &Expression,
        session: &Evaluation,
    ) -> Result<Option<Arc<RecordValues>>, DataError> {
        if !expression.dependencies.iter().any(|d| self.is_global(*d)) {
            return Ok(None);
        }
        let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
        session.globals.clone().map(Some).ok_or_else(|| {
            DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {name}: the session's values of the global fields its calculation \
                     reads were not given. No values were changed."
                ),
            )
        })
    }

    /// Whether an auto-entered or validation formula of the table, or a
    /// calculated custom message, reads a global field (schema-path.md
    /// 5.17): a write must then carry the session's global values
    /// ([`RecordEnvironment::globals`]).
    pub fn reads_globals(&self) -> bool {
        let table = self.schema.table(self.table);
        self.fields
            .values()
            .flat_map(|field| {
                let message = match &field.message {
                    Some(CustomMessage::Calculation(Some(expression))) => Some(expression),
                    _ => None,
                };
                field.formulas().chain(message)
            })
            .flat_map(|e| e.dependencies.iter())
            .any(|d| {
                table
                    .and_then(|t| t.fields.get(d))
                    .is_some_and(|f| f.global)
            })
    }

    /// Reject an unsupported stored record position before a create or
    /// preview reserves an ID or a serial number.
    pub fn check_creation_context(&self, context: &WriteContext) -> Result<(), DataError> {
        if matches!(
            context.record_number,
            Some(RecordNumber::LocalNewRecord(_) | RecordNumber::LocalSortedNewRecord { .. })
        ) {
            return Ok(());
        }
        if let Some(field) = self.fields.values().find(|field| {
            field.stored_calculation()
                && field.expression.as_ref().is_some_and(|expression| {
                    matches!(
                        expression.formula.polish,
                        Expr::Get(selector::RECORD_NUMBER)
                    )
                })
        }) {
            return Err(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {}: a stored record number requires ordinary New Record in a local window with a known insertion position.",
                    field.name
                ),
            ));
        }
        Ok(())
    }

    /// Whether a direct Text auto-enter reads the local file's variables.
    pub fn reads_auto_entry_file_variables(&self) -> bool {
        self.fields
            .values()
            .any(|field| field.auto_entry_file_variable().is_some())
    }

    /// The [`CONTEXT_SELECTORS`] the table's auto-entered, stored and
    /// validation formulas read, through the custom functions they call:
    /// what a write's operation must supply in its [`WriteContext`].
    pub fn context_reads(&self) -> BTreeSet<u8> {
        self.fields
            .values()
            .flat_map(PlannedField::formulas)
            .flat_map(|e| context_selectors(&self.schema, &e.formula.polish))
            .collect()
    }

    pub fn editable_fields(&self) -> BTreeSet<String> {
        self.fields
            .values()
            .filter(|f| f.editable)
            .map(|f| f.name.clone())
            .collect()
    }

    /// The unique fields a user's write checks with the server.
    pub fn unique_fields(&self) -> Vec<u64> {
        self.unique_fields_for(Entry::default())
    }

    /// The unique fields `entry` checks (uniqueness is a validation option:
    /// skipping data entry validation skips it unless the field validates
    /// Always).
    pub fn unique_fields_for(&self, entry: Entry) -> Vec<u64> {
        self.fields
            .iter()
            .filter_map(|(id, field)| {
                (field.options.validation_unique() && field.validated(entry)).then_some(*id)
            })
            .collect()
    }

    /// The fields validated as "existing value" whose values `entry`
    /// checks: another record must hold a changed value, which the host
    /// knows (its value index; error 505). Checked when the record is
    /// committed, for fields validated only during data entry too, unless
    /// the write skips data entry validation (script oracle s18).
    pub fn existing_fields_for(&self, entry: Entry) -> Vec<u64> {
        self.fields
            .iter()
            .filter_map(|(id, field)| {
                (field.options.validation_options & VALIDATE_EXISTING != 0
                    && field.validated(entry))
                .then_some(*id)
            })
            .collect()
    }

    /// The field whose values a value entered into `cell` must be among
    /// (a value list of a field's values), when `entry` checks it: the
    /// host looks it up (error 506), as Set Field does (script oracle s18).
    pub fn value_list_field(&self, cell: u64, entry: Entry) -> Option<(u64, u64)> {
        let field = &self.fields[&self.cell_field(cell)?.0];
        match &field.list {
            Some(ListRule::Field {
                occurrence,
                field: id,
            }) if field.entry_validated(entry) => Some((*occurrence, *id)),
            _ => None,
        }
    }

    /// Whether a field of the table may be checked against a related-values
    /// list ([`Self::related_value_list`]).
    pub fn checks_related_lists(&self) -> bool {
        self.fields
            .values()
            .any(|field| matches!(field.list, Some(ListRule::Related(_))))
    }

    /// Whether a user's typing into `cell`, a repetition of global field
    /// `field`, can be checked as it is entered, with no record's commit
    /// after it: the cell is a repetition the field defines, and a value
    /// list the field is validated by is a custom list or a field's values,
    /// not a related-values list ([`ListRule::Related`], checked at a
    /// record's commit) or one the rules cannot check
    /// ([`ListRule::Unchecked`]).
    pub fn checks_global_entry(&self, field: u64, cell: u64) -> bool {
        self.cell_field(cell).is_some_and(|(at, _)| at == field)
            && self.fields.get(&field).is_some_and(|planned| {
                !matches!(
                    planned.list,
                    Some(ListRule::Related(_) | ListRule::Unchecked(_))
                )
            })
    }

    /// Whether this global's own entry rules are supported when another
    /// field's rules have deferred the table plan. This only classifies
    /// the editor. A save still builds the full plan before validation.
    pub fn offers_global_while_plan_deferred(
        table: u64,
        field: u64,
        options: &FieldOptions,
        tree: &KeyTree<'_>,
        file: &CalcSchema,
    ) -> bool {
        if options.validation_options & !SUPPORTED_VALIDATION != 0 {
            return false;
        }
        if options.validation_options & VALIDATE_VALUE_LIST == 0 {
            return true;
        }
        let node = tree.at(&[table, 3, 5, field]);
        match list_rule(tree, node, file.value_lists()) {
            Ok(ListRule::Custom(_)) => true,
            Ok(ListRule::Field { occurrence, .. }) => {
                file.data_source(occurrence).is_none()
                    && file
                        .table_of(occurrence)
                        .is_some_and(|table| file.table_context(table.id).is_none())
            }
            _ => false,
        }
    }

    /// The related-values list a write by `entry` checks a changed value of
    /// `cell` against ([`ListRule::Related`]): the host computes it for the
    /// record as the write leaves it and checks the value at the commit
    /// (506), refusing (3) where it cannot. Set Field's own check
    /// ([`Self::check_entry`]) has no record to compute it for: such a
    /// value is checked at the commit.
    pub fn related_value_list(&self, cell: u64, entry: Entry) -> Option<&ValueList> {
        let field = &self.fields[&self.cell_field(cell)?.0];
        match &field.list {
            Some(ListRule::Related(list)) if field.entry_validated(entry) => Some(list),
            _ => None,
        }
    }

    /// The error a host's own check of `field` (unique, existing, value
    /// list) refuses a write with: `code`, with the field's validation
    /// message when it has one.
    pub fn refusal(&self, field: u64, code: i32) -> DataError {
        let requirement = match code {
            error::NOT_UNIQUE => "a unique value",
            error::NOT_EXISTING => "an existing value",
            error::NOT_IN_LIST => "a value of its value list",
            error::REQUIRED => "a value",
            _ => "a valid value",
        };
        match self.fields.get(&field) {
            Some(planned) => self.invalid(planned, code, requirement),
            None => DataError::with_code(code, format!("The field requires {requirement}.")),
        }
    }

    /// The cells of `field`'s repetitions, the first one's first: a
    /// record's values of a repeating field. A unique field's value must
    /// not be in any repetition of another record, in none of the
    /// repetitions (script oracle).
    pub fn cells(&self, field: u64) -> impl Iterator<Item = u64> {
        let repetitions = self.fields.get(&field).map_or(1, PlannedField::repetitions);
        (1..=repetitions).filter_map(move |n| cell_of(field, n))
    }

    /// The text `text` a user typed into `cell` (a field or one of its
    /// repetitions) stores, as a user's write stores it: a date, time or
    /// timestamp read the way data entry reads it (`typing`), refused when
    /// it does not parse; other types as typed. For a record a script
    /// writes that the user typed into (`ScriptWrite::typed`).
    pub fn typed_text(&self, cell: u64, text: &str) -> Result<String, DataError> {
        match self.cell_field(cell) {
            Some((field, _)) => self.typed(&self.fields[&field], text, Entry::default()),
            None => Ok(text.to_owned()),
        }
    }

    /// What Set Field checks before it stores `text` in `cell` (a field or
    /// one of its repetitions), when `entry` checks the field's entry
    /// rules (a script: only for a field validated Always): maximum
    /// characters 511, strict data types (numeric only 502, 4-digit year
    /// 500, time of day 501), range 503 and a custom value list 506.
    /// FileMaker Server's Set Field fails so and leaves the field as it
    /// was (script oracle s17, s18); a value list of a field's values is
    /// the host's ([`Self::value_list_field`]). Empty values pass.
    pub fn check_entry(&self, cell: u64, text: &str, entry: Entry) -> Result<(), DataError> {
        let Some(field) = self.cell_field(cell).map(|(id, _)| &self.fields[&id]) else {
            return Ok(());
        };
        if text.is_empty() || !field.entry_validated(entry) {
            return Ok(());
        }
        field.unchecked_list()?;
        let today = crate::host::session::today(0);
        let cx = Ctx::new(&self.schema.locale, today);
        match field.entry_error(text, &cx) {
            Some((code, requirement)) => Err(self.invalid(field, code, &requirement)),
            None => Ok(()),
        }
    }

    /// A user's write: the changed cells, encoded for upload.
    pub fn prepare(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
    ) -> Result<PreparedRecord, DataError> {
        self.prepare_entry(mode, existing, changes, environment, Entry::default())
    }

    /// A write by `entry` (a transaction's commit with its Skip options):
    /// the changed cells, encoded for upload.
    /// [`Self::prepare_entry`], refused (3) when the write evaluates a field
    /// reading a value of another record that `unknown` says that record's
    /// commit may not store ([`Self::masked_fields`]'s reads): a related
    /// file's commit, before the opened file's, reading the opened file's
    /// held records (#1006).
    pub fn prepare_entry_over(
        &self,
        (mode, existing, changes): (
            WriteMode,
            &BTreeMap<u64, String>,
            &BTreeMap<u64, Option<String>>,
        ),
        environment: &RecordEnvironment,
        entry: Entry,
        unknown: UnknownValue<'_>,
    ) -> Result<PreparedRecord, DataError> {
        let refused = |id: &u64| {
            DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {}: it reads a value of another file's record that its commit, \
                     after this one, may store otherwise. No values were changed.",
                    self.fields[id].name
                ),
            )
        };
        // A validation formula reading one, of a field validated for this
        // write, where it runs as the commit runs it: with "Validate only if
        // field has been modified" off, on the field unmodified too.
        let validators: Vec<u64> = self
            .reading_unknown(unknown, &environment.lookups, true)
            .into_iter()
            .filter(|id| self.fields[id].validated(entry))
            .collect();
        let unmodified_too = |id: &&u64| {
            self.fields[*id].options.validation_options & VALIDATE_CALCULATION_ALWAYS != 0
        };
        if let Some(id) = validators.iter().find(unmodified_too) {
            return Err(refused(id));
        }
        let readers = self.reads_unknown(unknown, &environment.lookups);
        if readers.is_empty() && validators.is_empty() {
            return self.prepare_entry(mode, existing, changes, environment, entry);
        }
        // The computation the record is written from: a reader it evaluates,
        // or a validated value it modifies, refuses it.
        let computed = self.compute_inner(mode, existing, changes, environment, entry, None)?;
        let modified = |id: &&u64| {
            self.cells(**id).any(|cell| {
                computed.row.get(&cell).map_or("", String::as_str)
                    != existing.get(&cell).map_or("", String::as_str)
            })
        };
        let reading = computed.evaluated.iter().find(|id| readers.contains(id));
        if let Some(id) = reading.or_else(|| validators.iter().find(modified)) {
            return Err(refused(id));
        }
        // As `compute_values` refuses on every other way to a write.
        let changed = Self::changed_values(computed, existing, |_, _| false);
        self.refuse_unwritable_changes(&changed)?;
        Self::prepared(changed)
    }

    pub fn prepare_entry(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
    ) -> Result<PreparedRecord, DataError> {
        let computed = self.compute(mode, existing, changes, environment, entry)?;
        Self::prepared(computed)
    }

    /// The changed cells `computed`, encoded for upload.
    fn prepared(computed: BTreeMap<u64, Option<String>>) -> Result<PreparedRecord, DataError> {
        let values = computed
            .into_iter()
            .map(|(id, text)| {
                let encoded = text.map(|text| om_scsu::encode_scsu(&text));
                if encoded.as_ref().is_some_and(|value| value.len() > 65_535) {
                    return Err(DataError::with_code(
                        error::CANNOT_FIT,
                        "Generated field value exceeds the supported single-cell size.",
                    ));
                }
                Ok((id, encoded))
            })
            .collect::<Result<BTreeMap<_, _>, DataError>>()?;
        let changed_fields = values.keys().copied().collect();
        Ok(PreparedRecord {
            values,
            changed_fields,
        })
    }

    /// The new text of every field a write changes (`None`: cleared): the
    /// `changes` themselves, auto-entered values and recomputed stored
    /// calculations, after validation. `existing` is the record as stored
    /// (empty for a new one); `changes` are text in the file's storage
    /// formats.
    pub fn compute(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        self.compute_values(mode, existing, changes, environment, entry, None)
    }

    /// Compute changes for a draft with copied cells or earlier edits.
    /// A rule can restore the stored value and still change the draft
    /// if the cell was copied or an import leaves an earlier edit unmapped.
    pub fn compute_with_staged_cells(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
        was_copied: impl Fn(u64) -> bool,
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        let computed = self.compute_inner(mode, existing, changes, environment, entry, None)?;
        let changed = Self::changed_values(computed, existing, |id, value| {
            let unmapped =
                entry.keep_written && entry.mapped_cells.is_some_and(|cells| !cells.contains(&id));
            (was_copied(id) || unmapped)
                && changes
                    .get(&id)
                    .is_some_and(|text| value != text.as_deref().unwrap_or_default())
        });
        self.refuse_unwritable_changes(&changed)?;
        Ok(changed)
    }

    /// The fields a preview computes (#1047): those the rules give the same
    /// value each time they are computed with the same environment and
    /// records, so not a `Random` draw (itself or in a custom function the
    /// formula calls); and, unless the window's unsaved records are known
    /// (`others`, #1063), those the written record gives alone, so not a
    /// lookup, a related field or an aggregate of related records, which
    /// read another record the window may hold unsaved where the host's
    /// stored one is all a caller could read. A field computed from one
    /// left out, directly or through others, is left out too.
    pub fn preview_fields(&self, others: bool) -> BTreeSet<u64> {
        let left_out = |field: &PlannedField| {
            !others && field.lookup.is_some()
                || field.expression.as_ref().is_some_and(|e| {
                    !others && (!e.related.is_empty() || !e.whole.is_empty())
                        || e.reads_any(
                            &self.schema,
                            &|x| matches!(x, Expr::Call { op, .. } if op.0 == 0x31),
                        )
                })
        };
        let mut other: BTreeSet<u64> = self
            .fields
            .iter()
            .filter(|(_, field)| left_out(field))
            .map(|(id, _)| *id)
            .collect();
        // A field's inputs are fields of its own table: at most one pass
        // per field.
        loop {
            let more: Vec<u64> = self
                .fields
                .iter()
                .filter(|(id, field)| {
                    !other.contains(id) && field.dependencies().any(|d| other.contains(d))
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                break;
            }
            other.extend(more);
        }
        self.fields
            .keys()
            .filter(|id| !other.contains(id))
            .copied()
            .collect()
    }

    /// Preview known changed values of `fields` and their calculation dependencies.
    /// Validation and calculations with incomplete inputs remain deferred until
    /// commit. Failed calculations and their dependents provide no candidate;
    /// independent, computable values can still be checked before taking locks.
    #[allow(clippy::too_many_arguments)]
    pub fn preview_entry(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
        fields: &BTreeSet<u64>,
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        let mut needed = fields.clone();
        let mut pending: Vec<_> = fields.iter().copied().collect();
        while let Some(id) = pending.pop() {
            if let Some(field) = self.fields.get(&id) {
                for dependency in field.dependencies() {
                    if needed.insert(*dependency) {
                        pending.push(*dependency);
                    }
                }
            }
        }
        let mut values =
            self.compute_values(mode, existing, changes, environment, entry, Some(&needed))?;
        values.retain(|cell, _| {
            self.cell_field(*cell)
                .is_some_and(|(field, _)| fields.contains(&field))
        });
        Ok(values)
    }

    /// The record's cells once the write's own values are in, and the cells
    /// it changed; a cell `entry` may not set refuses the write.
    fn written(
        &self,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        entry: Entry,
    ) -> Result<(BTreeMap<u64, String>, BTreeSet<u64>), DataError> {
        let mut row = existing.clone();
        let mut dirty = BTreeSet::new();
        for (id, value) in changes {
            let (field_id, repetition) = self.cell_field(*id).ok_or_else(|| {
                DataError::with_code(error::FIELD_MISSING, "A changed field no longer exists.")
            })?;
            let field = &self.fields[&field_id];
            if field.options.pending_embedded_container_transfer() {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {} cannot be changed while its container storage transfer is pending.",
                        field.name
                    ),
                ));
            }
            if field.expression.is_some() && field.options.max_repetitions > 1 {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: direct writes to repeated filenames are unsupported.",
                        field.name
                    ),
                ));
            }
            // A container's text is its host's reference to the data
            // (containers.md 1.4), set by the client for any writer.
            let allowed = if entry.script {
                field.settable_by_script()
            } else {
                field.editable || field.settable_by_script() && field.options.data_type == 6
            };
            if !allowed {
                return Err(DataError::with_code(
                    error::FIELD_LOCKED,
                    format!("Field {} cannot be edited.", field.name),
                ));
            }
            let text = value.as_deref().unwrap_or_default();
            let text = if entry.script {
                text.to_owned()
            } else {
                self.typed(field, text, entry)?
            };
            assign(&mut row, &mut dirty, *id, text);
            if repetition > 1 && dirty.contains(id) {
                // Calculations read a field's repetitions through the field
                // (`Sum ( F )`, `Last ( F )`); its first value is unchanged,
                // so it is not reported as changed below.
                dirty.insert(field_id);
            }
        }
        Ok((row, dirty))
    }

    /// The text `field` stores for a user's typed `text` (`typing`): a date,
    /// time or timestamp as data entry reads it, anything else as typed.
    /// A four-digit-year rule sees the year as typed, before data entry
    /// completes it (else `1/2/26` would always pass it; inferred).
    fn typed(&self, field: &PlannedField, text: &str, entry: Entry) -> Result<String, DataError> {
        let cx = Ctx::new(&self.schema.locale, crate::host::session::today(0));
        let typed = typing::entered(field.data_type, text, &cx).map_err(|code| {
            let kind = match field.data_type {
                DataType::Time => "time",
                DataType::Timestamp => "date and time",
                _ => "date",
            };
            DataError::with_code(
                code,
                format!("Field {} requires a valid {kind}.", field.name),
            )
        })?;
        if matches!(field.data_type, DataType::Date | DataType::Timestamp)
            && field.options.validation_options & VALIDATE_FOUR_DIGIT_YEAR != 0
            && field.entry_validated(entry)
            && !typed.is_empty()
            && !four_digit_year(&typing::clean(text), &cx)
        {
            return Err(self.invalid(field, error::DATE, "a date with a four-digit year"));
        }
        Ok(typed)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_values(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
        preview: Option<&BTreeSet<u64>>,
    ) -> Result<BTreeMap<u64, Option<String>>, DataError> {
        let computed = self.compute_inner(mode, existing, changes, environment, entry, preview)?;
        let changed = Self::changed_values(computed, existing, |_, _| false);
        if preview.is_none() {
            self.refuse_unwritable_changes(&changed)?;
        }
        Ok(changed)
    }

    /// Refuse (3) a write that changes the text of a field OpenMaker cannot
    /// write as FileMaker does, its own or one it enters or computes
    /// (AGENTS.md rule 4: never a silent difference). `changed` holds only
    /// cells whose text changes:
    /// - a field indexed in a language whose keys OpenMaker does not build
    ///   ([`PlannedField::unkeyed_index`]): the host would refuse it too
    ///   rather than key it in English (#159);
    /// - a field with furigana ([`PlannedField::furigana`]): its reading
    ///   field would be left as it was (#1302). Whether FileMaker Pro fills
    ///   the reading for a change by a script, an import or auto-entry too
    ///   is unmeasured; each is refused.
    fn refuse_unwritable_changes(
        &self,
        changed: &BTreeMap<u64, Option<String>>,
    ) -> Result<(), DataError> {
        for cell in changed.keys() {
            let Some((id, _)) = self.cell_field(*cell) else {
                continue;
            };
            let field = &self.fields[&id];
            if let Some(language) = field.unkeyed_index {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: OpenMaker cannot yet change its value: it is indexed in \
                         index language {language}, whose keys OpenMaker does not build \
                         (#1279).",
                        field.name
                    ),
                ));
            }
            if field.furigana {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: OpenMaker cannot yet change its value: it uses furigana, and \
                         OpenMaker does not write the reading into its furigana field (#1302).",
                        field.name
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The cells `computed` changes from `existing` or a staged input,
    /// excluding deferred cells: stored text, `None` empty.
    fn changed_values(
        computed: Computed,
        existing: &BTreeMap<u64, String>,
        differs_from_staged: impl Fn(u64, &str) -> bool,
    ) -> BTreeMap<u64, Option<String>> {
        let Computed {
            row,
            dirty,
            deferred,
            ..
        } = computed;
        dirty
            .into_iter()
            .filter(|id| !deferred.contains(id))
            .filter(|id| {
                let value = row.get(id).map_or("", String::as_str);
                value != existing.get(id).map_or("", String::as_str)
                    || differs_from_staged(*id, value)
            })
            .map(|id| {
                let text = row.get(&id).map_or("", String::as_str);
                (id, (!text.is_empty()).then(|| text.to_owned()))
            })
            .collect()
    }

    /// A record the window holds unsaved (#1063) as its commit would compute
    /// it, for another record's preview to read: `existing` (its stored row,
    /// empty for a new one) with `changes` and what its rules compute from
    /// them, validation left to its commit; every cell, by cell. With it,
    /// the fields whose value in it may not be what its commit stores
    /// (unknown): a field its rules leave out (a formula they cannot answer
    /// yet, a serial number not reserved); one they evaluate from a volatile
    /// source its commit draws again ([`Self::volatile_fields`]; `kept`:
    /// whether its commit keeps this environment's clock and UUID, and its
    /// host clock), or from a value of another record `unknown` says its
    /// commit may not store ([`Self::masked_fields`]); and every field
    /// computed from one, whether evaluated now or not (an input that comes
    /// out the same now may not at its commit). A source not evaluated now
    /// (a creation date an edit keeps, an auto-entered value a write keeps)
    /// is the stored value.
    pub fn pending_row(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        (entry, kept, unknown): (Entry, (bool, bool), UnknownValue<'_>),
    ) -> Result<(BTreeMap<u64, String>, BTreeSet<u64>), DataError> {
        let all: BTreeSet<u64> = self.fields.keys().copied().collect();
        let computed =
            self.compute_inner(mode, existing, changes, environment, entry, Some(&all))?;
        let sources = self.volatile_fields(kept);
        let readers = self.reads_unknown(unknown, &environment.lookups);
        let seeds: BTreeSet<u64> = computed
            .evaluated
            .iter()
            .filter(|id| sources.contains(id) || readers.contains(id))
            .chain(computed.probed.iter().filter(|id| sources.contains(id)))
            .chain(&computed.deferred)
            .copied()
            .collect();
        Ok((computed.row, self.own_unknowns(seeds)))
    }

    /// `seeds`, fields of the record unknown, with every field computed
    /// from one, and every field reading one through a relationship to its
    /// own table, where the record may be among those read (#1035): as the
    /// read field, or as a match or sort field there.
    fn own_unknowns(&self, seeds: BTreeSet<u64>) -> BTreeSet<u64> {
        let mut unknown = self.computed_from(seeds);
        loop {
            let more: BTreeSet<u64> = self
                .fields
                .iter()
                .filter(|(id, field)| {
                    !unknown.contains(id)
                        && field.expression.as_ref().is_some_and(|e| {
                            let Some(context) = e.formula.occurrence() else {
                                return false;
                            };
                            e.related.iter().any(|(occurrence, read)| {
                                let own = self
                                    .schema
                                    .table_of(*occurrence)
                                    .is_some_and(|t| t.id == self.table);
                                own && std::iter::once(*read)
                                    .chain(self.deciding_fields(context, *occurrence))
                                    .any(|f| unknown.contains(&f))
                            })
                        })
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                return unknown;
            }
            unknown = self.computed_from(unknown.into_iter().chain(more).collect());
        }
    }

    /// The fields whose value reads one of another record that `unknown`
    /// says may not be what that record's commit stores (#1063), through a
    /// relationship (a field read there, or its match or sort fields there)
    /// or a lookup (the field copied, the match fields there), and every
    /// field computed from one, directly or through others: what a new
    /// record's preview leaves out, every creation formula being evaluated.
    /// `lookups`: the related records the rules read, whose own values are
    /// the ones checked.
    pub fn masked_fields(&self, unknown: UnknownValue<'_>, lookups: &Lookups) -> BTreeSet<u64> {
        self.computed_from(self.reads_unknown(unknown, lookups))
    }

    /// The fields whose formula or lookup reads a value of another record
    /// `unknown` says may not be what its commit stores
    /// ([`Self::masked_fields`]). A field read is checked on the records
    /// `lookups` read for it, when they were read: the match and sort fields
    /// on any record of the table, since any may be related or come first.
    fn reads_unknown(&self, unknown: UnknownValue<'_>, lookups: &Lookups) -> BTreeSet<u64> {
        self.reading_unknown(unknown, lookups, false)
    }

    /// [`Self::reads_unknown`] of the fields' auto-entered and stored
    /// formulas and lookups, or with `validation` of their validation
    /// formulas.
    fn reading_unknown(
        &self,
        unknown: UnknownValue<'_>,
        lookups: &Lookups,
        validation: bool,
    ) -> BTreeSet<u64> {
        let reads = |(context, occurrence): (u64, u64), read: u64, records: Option<Vec<u64>>| {
            let Some(table) = self.schema.table_of(occurrence).map(|t| t.id) else {
                return false;
            };
            self.deciding_fields(context, occurrence)
                .into_iter()
                .any(|field| unknown(table, None, field))
                || match records {
                    Some(records) => records.iter().any(|r| unknown(table, Some(*r), read)),
                    None => unknown(table, None, read),
                }
        };
        self.fields
            .iter()
            .filter(|(id, field)| {
                let expression = match validation {
                    true => field.validation.as_ref(),
                    false => field.expression.as_ref(),
                };
                let related = expression.is_some_and(|e| {
                    e.formula.occurrence().is_some_and(|context| {
                        e.related.iter().any(|(occurrence, read)| {
                            let records = lookups
                                .related
                                .get(&(context, *occurrence))
                                .map(|(_, found)| found.iter().map(|(r, _)| *r).collect());
                            reads((context, *occurrence), *read, records)
                        })
                    })
                });
                // A lookup that fired copied from the record it found, or
                // from none.
                let looked_up = !validation
                    && field.lookup.as_ref().is_some_and(|lookup| {
                        let records = lookups
                            .copied
                            .contains_key(id)
                            .then(|| lookups.copied_from.get(id).copied().into_iter().collect());
                        reads((lookup.from, lookup.to), lookup.copied, records)
                    });
                related || looked_up
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The fields of `occurrence`'s table that decide which of its records
    /// a formula of `context` reads, and which comes first: the match and
    /// sort fields of the one relationship between (`related_hop`).
    fn deciding_fields(&self, context: u64, occurrence: u64) -> Vec<u64> {
        let hop = self
            .schema
            .path(context, occurrence)
            .and_then(|path| path.first().cloned());
        hop.iter()
            .flat_map(|hop| {
                hop.predicates
                    .iter()
                    .map(|p| p.right)
                    .chain(sort_fields(hop))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// `fields` with every field computed from one, directly or through
    /// others.
    fn computed_from(&self, mut fields: BTreeSet<u64>) -> BTreeSet<u64> {
        loop {
            let more: Vec<u64> = self
                .fields
                .iter()
                .filter(|(id, field)| {
                    !fields.contains(id) && field.dependencies().any(|d| fields.contains(d))
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                return fields;
            }
            fields.extend(more);
        }
    }

    /// The fields whose value, when evaluated, a later evaluation can change
    /// (#1063): a `Random` draw; unless its commit keeps the clock and UUID
    /// (`kept.0`), a creation or modification date, time or timestamp and a
    /// formula reading the date, time, timestamp or a UUID; unless it keeps
    /// the host clock (`kept.1`), a formula reading it.
    fn volatile_fields(&self, (stamp, host): (bool, bool)) -> BTreeSet<u64> {
        use crate::host::session::selector;
        let clock = [
            selector::CURRENT_DATE,
            selector::CURRENT_TIME,
            selector::CURRENT_TIMESTAMP,
            selector::UUID,
            selector::UUID_NUMBER,
            selector::CURRENT_TIME_UTC_MILLISECONDS,
            selector::CURRENT_TIME_UTC_MICROSECONDS,
        ];
        let volatile_expr = |expr: &Expr| match expr {
            Expr::Call { op, .. } => op.0 == 0x31,
            Expr::Get(selector) => {
                !stamp && clock.contains(selector)
                    || !host && *selector == selector::CURRENT_HOST_TIMESTAMP
            }
            _ => false,
        };
        let volatile: BTreeSet<u64> = self
            .fields
            .iter()
            .filter(|(_, field)| {
                let variable = field.options.auto_entry_variable().is_some_and(|v| {
                    !stamp
                        && matches!(
                            v,
                            EntryVariable::CreationDate
                                | EntryVariable::CreationTime
                                | EntryVariable::CreationTimestamp
                                | EntryVariable::ModificationDate
                                | EntryVariable::ModificationTime
                                | EntryVariable::ModificationTimestamp
                        )
                });
                variable
                    || field
                        .expression
                        .as_ref()
                        .is_some_and(|e| e.reads_any(&self.schema, &volatile_expr))
            })
            .map(|(id, _)| *id)
            .collect();
        volatile
    }

    /// Preserve the measured stored result when evaluation is deferred to
    /// commit. A group set and then cleared is still an assignment, even
    /// if it would appear unchanged in the final row.
    fn check_grouped_self_join_write(
        &self,
        mode: WriteMode,
        stored: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        row: &BTreeMap<u64, String>,
        environment: &RecordEnvironment,
    ) -> Result<(), DataError> {
        for &(result, group, key) in &self.grouped_self_joins {
            let single_line = [stored, row].into_iter().all(|values| {
                [group, key]
                    .into_iter()
                    .all(|id| values.get(&id).is_none_or(|value| single_line_key(value)))
            });
            let stored_group_empty = stored.get(&group).is_none_or(String::is_empty);
            let group_assigned = changes.contains_key(&group);
            let keeps_result = stored.get(&result).is_some_and(|value| !value.is_empty())
                && !changes.contains_key(&result);
            let safe_without_assignment = !group_assigned
                && !changes.contains_key(&key)
                && (stored_group_empty || keeps_result);
            let measured_assignment = mode == WriteMode::Update
                && environment.context.sequence == WriteSequence::SingleLocalFieldStep
                && stored_group_empty
                && group_assigned
                && changes.len() == 1
                && row.get(&group).is_some_and(|value| !value.is_empty());
            if environment.context.sequence == WriteSequence::Unmeasured
                || !single_line
                || !(safe_without_assignment || measured_assignment)
            {
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: this grouped self-join needs a single local group-key assignment with single-line keys. No values were changed.",
                        self.fields[&result].name
                    ),
                ));
            }
        }
        Ok(())
    }

    /// [`Self::compute_values`]'s whole computation: the record's cells as
    /// the write leaves them, those it changed, and those a preview
    /// (`preview`) left out.
    fn compute_inner(
        &self,
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        environment: &RecordEnvironment,
        entry: Entry,
        preview: Option<&BTreeSet<u64>>,
    ) -> Result<Computed, DataError> {
        if mode == WriteMode::Create {
            self.check_creation_context(&environment.context)?;
        }
        let validate = preview.is_none();
        let mut deferred = BTreeSet::new();
        let mut evaluated = BTreeSet::new();
        let mut probed = BTreeSet::new();
        let mut supplied_dependencies = None;
        let (mut row, mut dirty) = self.written(existing, changes, entry)?;
        if !entry.skip_auto_enter {
            self.check_grouped_self_join_write(mode, existing, changes, &row, environment)?;
        }
        // A write that changes nothing changes nothing, unless it sets a
        // lookup's match field: FileMaker performs the lookup then, also on
        // the value the field had (auto-entry.md, Lookups, battery s41).
        let fires_lookup = self.fields.iter().any(|(id, field)| {
            field
                .lookup
                .as_ref()
                .is_some_and(|lookup| self.fires(*id, lookup, mode, &dirty, changes, entry))
        });
        if mode == WriteMode::Update && dirty.is_empty() && !fires_lookup {
            return Ok(Computed {
                row,
                dirty,
                deferred,
                evaluated,
                probed,
            });
        }
        for (id, field) in &self.fields {
            if entry.skip_auto_enter {
                break;
            }
            if preview.is_some_and(|needed| !needed.contains(id)) {
                continue;
            }
            if field.auto_enters_global() || entry.keeps(changes, *id) {
                continue;
            }
            if let Some(variable) = field.options.auto_entry_variable() {
                if mode == WriteMode::Create || variable.is_modification() == Some(true) {
                    let value =
                        match variable {
                            EntryVariable::CreationDate | EntryVariable::ModificationDate => {
                                &environment.date
                            }
                            EntryVariable::CreationTime | EntryVariable::ModificationTime => {
                                &environment.time
                            }
                            EntryVariable::CreationTimestamp
                            | EntryVariable::ModificationTimestamp => &environment.timestamp,
                            EntryVariable::CreationUserName
                            | EntryVariable::ModificationUserName => &environment.user_name,
                            EntryVariable::CreationAccountName
                            | EntryVariable::ModificationAccountName => &environment.account_name,
                            EntryVariable::Unknown(_) => {
                                if !validate {
                                    deferred.insert(*id);
                                    continue;
                                }
                                return Err(DataError::with_code(
                                    error::UNAVAILABLE,
                                    "Unknown auto-entry variable.",
                                ));
                            }
                        };
                    evaluated.insert(*id);
                    assign(&mut row, &mut dirty, *id, value.clone());
                }
            }
            if let Some(constant) = &field.constant {
                if mode == WriteMode::Create && row.get(id).map_or(true, String::is_empty) {
                    assign(&mut row, &mut dirty, *id, constant.clone());
                }
            }
            if field.serial.is_some()
                && mode == WriteMode::Create
                && !Self::keeps_serial(entry, changes, *id)
            {
                match environment.serials.get(id) {
                    Some(value) => assign(&mut row, &mut dirty, *id, value.clone()),
                    None if !validate => {
                        deferred.insert(*id);
                    }
                    None => {
                        return Err(DataError::with_code(
                            error::UNAVAILABLE,
                            format!(
                                "Field {}: its serial number was not reserved. No values were \
                                 changed.",
                                field.name
                            ),
                        ))
                    }
                }
            }
        }
        let mut session = environment.session();
        if mode != WriteMode::Create {
            session.creation_utc_clock = None;
            session
                .unanswered
                .insert(selector::CURRENT_TIME_UTC_MILLISECONDS);
        }
        session.stored = existing.clone();
        // Each field's turn passes before the next: its value is final then,
        // unless it was left out, or a preview passed over it uncomputed.
        let mut previous: Option<u64> = None;
        let mut passed_over = BTreeSet::new();
        for id in &self.evaluation_order {
            if let Some(previous) = previous.replace(*id) {
                if !deferred.contains(&previous) && !passed_over.contains(&previous) {
                    session.settled.insert(previous);
                }
            }
            if preview.is_some_and(|needed| !needed.contains(id)) {
                passed_over.insert(*id);
                continue;
            }
            let field = &self.fields[id];
            if field.auto_enters_global() {
                continue;
            }
            if let Some(lookup) = &field.lookup {
                if !self.fires(*id, lookup, mode, &dirty, changes, entry) {
                    continue;
                }
                if lookup
                    .matches
                    .iter()
                    .any(|m| deferred.contains(m) || passed_over.contains(m))
                {
                    if !validate {
                        deferred.insert(*id);
                        continue;
                    }
                    return Err(DataError::with_code(
                        error::UNAVAILABLE,
                        format!(
                            "Field {}: its lookup match key is not final. No values were changed.",
                            field.name
                        ),
                    ));
                }
                evaluated.insert(*id);
                let keys = Self::lookup_keys(lookup, &row);
                match environment.lookups.copied.get(id) {
                    Some((read_keys, found)) if *read_keys == keys => {
                        let value = match found {
                            Some(value) => {
                                Some(value).filter(|v| lookup.copy_empty || !v.is_empty())
                            }
                            None => lookup.fallback.as_ref(),
                        };
                        if let Some(value) = value {
                            assign(&mut row, &mut dirty, *id, value.clone());
                        }
                    }
                    _ => {
                        if let Some(needs) = &environment.related_needs {
                            needs.need((lookup.from, lookup.to), keys);
                        }
                        if !validate {
                            deferred.insert(*id);
                            continue;
                        }
                        return Err(DataError::with_code(error::UNAVAILABLE,
                            format!("Field {}: its looked-up value was not read for its final match key. No values were changed.", field.name)));
                    }
                }
                continue;
            }
            let Some(expression) = &field.expression else {
                continue;
            };
            let combined_create = field.creation_date_with_calculation && mode == WriteMode::Create;
            if !field.stored_calculation()
                && (entry.skip_auto_enter || (entry.keeps(changes, *id) && !combined_create))
            {
                continue;
            }
            if field.options.max_repetitions > 1 {
                match self.repeating_filename_updates(*id, expression, &row, &session, mode) {
                    Ok(updates) => {
                        if !updates.is_empty() {
                            evaluated.insert(*id);
                        }
                        for (cell, value) in updates {
                            assign(&mut row, &mut dirty, cell, value);
                            if dirty.contains(&cell) {
                                dirty.insert(*id);
                            }
                        }
                    }
                    Err(_) if !validate => {
                        deferred.insert(*id);
                    }
                    Err(error) => return Err(error.into_error()),
                }
                continue;
            }
            let depends_on_deferred = expression
                .dependencies
                .iter()
                .any(|id| deferred.contains(id));
            let should_evaluate = mode == WriteMode::Create
                || depends_on_deferred
                || expression.dependencies.iter().any(|id| dirty.contains(id));
            let can_replace = field.stored_calculation()
                || field.options.calculation_replaces_existing()
                || row.get(id).map_or(true, String::is_empty);
            if !should_evaluate || !can_replace {
                continue;
            }
            if combined_create {
                // New Record alone uses the creation date. A held create
                // can also carry later edits before the first commit. Only
                // those supplied inputs, not creation defaults, make this
                // an unmeasured re-evaluation.
                let affected = supplied_dependencies.get_or_insert_with(|| {
                    self.computed_from(
                        changes
                            .keys()
                            .filter_map(|cell| self.cell_field(*cell))
                            .map(|(field, _)| field)
                            .collect(),
                    )
                });
                if !expression
                    .dependencies
                    .iter()
                    .any(|dep| dep != id && affected.contains(dep))
                {
                    continue;
                }
            }
            if depends_on_deferred {
                deferred.insert(*id);
                continue;
            }
            if field.creation_date_with_calculation {
                if !validate {
                    deferred.insert(*id);
                    continue;
                }
                return Err(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: recalculating the auto-entry combined with Creation Date is unsupported for this write. No values were changed.",
                        field.name
                    ),
                ));
            }
            let globals = self.session_globals(*id, expression, &session)?;
            let mut remaining = session.calculation_budget();
            let result = (|| {
                if field.options.options & ALWAYS_EVALUATE == 0
                    && self.all_inputs_empty(
                        expression,
                        &row,
                        &session,
                        globals.as_deref(),
                        &mut remaining,
                    )?
                {
                    return Ok(None);
                }
                self.evaluate_repetition(*id, 1, expression, &row, &session, &mut remaining)
                    .map(Some)
            })();
            let value = match result {
                Ok(Some(value)) => {
                    evaluated.insert(*id);
                    value
                }
                Ok(None) => {
                    // A volatile probe can give a different answer at commit.
                    if !expression.through_formulas.is_empty() {
                        probed.insert(*id);
                    }
                    // Replacing calculations clear the value (script oracle
                    // s34-containers-calc, when its container is cleared).
                    if !field.stored_calculation() && !field.options.calculation_replaces_existing()
                    {
                        continue;
                    }
                    String::new()
                }
                Err(_) if !validate => {
                    deferred.insert(*id);
                    continue;
                }
                Err(error) => return Err(error.into_error()),
            };
            assign(&mut row, &mut dirty, *id, value);
        }
        if let Some(previous) = previous {
            if !deferred.contains(&previous) && !passed_over.contains(&previous) {
                session.settled.insert(previous);
            }
        }
        // Validation (script oracle s17 against FileMaker Server's engine;
        // Claris: "In a repeating field, FileMaker Pro validates each
        // repetition individually"): not empty holds when any repetition
        // has a value; maximum characters checks each repetition the write
        // changed; the calculation each changed repetition, empty or not,
        // and with "Validate only if field has been modified" off also
        // repetition 1 and every repetition holding a value. Validating
        // Always only means the write's "Skip data entry validation" does
        // not apply ([`PlannedField::validated`]).
        for (id, field) in &self.fields {
            if !validate || !field.validated(entry) {
                continue;
            }
            let text = |cell: u64| row.get(&cell).map_or("", String::as_str);
            // The field's custom message is shown for any rule it breaks.
            if field.options.validation_not_empty() && self.cells(*id).all(|c| text(c).is_empty()) {
                return Err(self.invalid_in(
                    *id,
                    field,
                    1,
                    error::REQUIRED,
                    "a value",
                    &row,
                    &session,
                ));
            }
            let value = text(*id);
            // Whether another record holds the value is the server's to
            // answer, under a lock (the provider's `check_unique`).
            if field.options.validation_unique()
                && field.generated_uuid()
                && dirty.contains(id)
                && !canonical_uuid(value)
            {
                return Err(DataError::with_code(
                    error::REQUIRED,
                    format!("Field {} requires a valid generated UUID.", field.name),
                ));
            }
            let unmodified_too =
                field.options.validation_options & VALIDATE_CALCULATION_ALWAYS != 0;
            let cx = Ctx::new(&self.schema.locale, session.today());
            for repetition in 1..=field.repetitions() {
                // A repetition that cannot have a cell is empty and
                // unchanged: nothing to check.
                let Some(cell) = cell_of(*id, repetition) else {
                    continue;
                };
                let value = text(cell);
                let modified = value != existing.get(&cell).map_or("", String::as_str);
                // The entry rules check a changed value again (an
                // auto-entered one; Set Field has checked a script's).
                if modified && !value.is_empty() && field.entry_validated(entry) {
                    field.unchecked_list()?;
                    if let Some((code, requirement)) = field.entry_error(value, &cx) {
                        return Err(self.invalid_in(
                            *id,
                            field,
                            repetition,
                            code,
                            &requirement,
                            &row,
                            &session,
                        ));
                    }
                }
                let Some(validation) = &field.validation else {
                    continue;
                };
                let checked =
                    modified || (unmodified_too && (repetition == 1 || !value.is_empty()));
                if checked && !self.validate(*id, repetition, validation, &row, &session)? {
                    return Err(self.invalid_in(
                        *id,
                        field,
                        repetition,
                        error::VALIDATION_CALCULATION,
                        "a value its validation calculation accepts",
                        &row,
                        &session,
                    ));
                }
            }
        }
        Ok(Computed {
            row,
            dirty,
            deferred,
            evaluated,
            probed,
        })
    }

    /// The field and repetition a cell id names: the field itself, or
    /// repetition n (2 to the field's repetitions, at most 255) stored
    /// under `F * 256 + n` (execute-sql.md, reading `.fmp12` files).
    /// The name of the first field a write's own `changes` set that
    /// `modifies` (by field id) says may not be changed: a field the
    /// account's privilege set gives view-only access (session-identity.md
    /// 2.1). Values the rules enter themselves (auto-enter, lookups) are not
    /// among the changes, so they are not checked.
    pub fn unmodifiable_change(
        &self,
        changes: &BTreeMap<u64, Option<String>>,
        modifies: impl Fn(u64) -> bool,
    ) -> Option<&str> {
        changes
            .keys()
            .filter_map(|cell| self.cell_field(*cell))
            .find(|(field, _)| !modifies(*field))
            .and_then(|(field, _)| self.fields.get(&field))
            .map(|field| field.name.as_str())
    }

    fn cell_field(&self, cell: u64) -> Option<(u64, u32)> {
        if self.fields.contains_key(&cell) {
            return Some((cell, 1));
        }
        let (field, repetition) = (cell >> 8, (cell & 0xff) as u32);
        let planned = self.fields.get(&field)?;
        (repetition >= 2 && repetition <= u32::from(planned.options.max_repetitions))
            .then_some((field, repetition))
    }

    /// The refusal of `field`'s value where the record is not at hand (a
    /// value checked as it is entered, a host's answer): the field's custom
    /// message, a calculated one only when it is text alone
    /// ([`literal_text`]), else OpenMaker's own wording.
    fn invalid(&self, field: &PlannedField, code: i32, requirement: &str) -> DataError {
        let message = match &field.message {
            Some(CustomMessage::Text(text)) => Some(text.clone()),
            Some(CustomMessage::Calculation(Some(expression))) => {
                literal_text(&expression.formula.polish).filter(|text| !text.is_empty())
            }
            Some(CustomMessage::Calculation(None)) | None => None,
        };
        DataError::with_code(
            code,
            message.unwrap_or_else(|| format!("Field {} requires {requirement}.", field.name)),
        )
    }

    /// The refusal of repetition `repetition` of field `id` in a write
    /// whose record is `row`: a calculated custom message is evaluated in
    /// it, as the validation calculation is (`Self` the repetition's value).
    /// One that fails, or gives no text, leaves OpenMaker's own wording
    /// [I: what FileMaker shows then is unmeasured].
    #[allow(clippy::too_many_arguments)]
    fn invalid_in(
        &self,
        id: u64,
        field: &PlannedField,
        repetition: u32,
        code: i32,
        requirement: &str,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
    ) -> DataError {
        let Some(CustomMessage::Calculation(Some(expression))) = &field.message else {
            return self.invalid(field, code, requirement);
        };
        let cx = Ctx::new(&self.schema.locale, session.today());
        let value = cell_of(id, repetition)
            .and_then(|cell| row.get(&cell))
            .map_or("", String::as_str);
        let value_type = self
            .schema
            .table(self.table)
            .and_then(|t| t.fields.get(&id))
            .map_or(ValueType::Text, |d| d.value_type);
        let self_value = stored_value(value, value_type, &cx);
        match self.run(
            id,
            expression,
            row,
            session,
            Some(self_value),
            Some(ValueType::Text),
            InRepetition::Validation(repetition),
            &mut session.calculation_budget(),
        ) {
            Ok((_, text)) if !text.is_empty() => DataError::with_code(code, text),
            _ => DataError::with_code(
                code,
                format!("Field {} requires {requirement}.", field.name),
            ),
        }
    }

    /// The row's values for calculations, typed by the table's fields. A
    /// cell naming a library member keeps its reference until a
    /// calculation reads the field ([`RuleSource::library_member`]).
    /// Whether a self-join read through `occurrence` from `context` gives
    /// the formula the same answer whether the written record is not among
    /// its own related records, or is with the values the write leaves, or
    /// is with its stored values: that is unmeasured (#1035). When the
    /// record, as written or as stored, is related to the record as written,
    /// only:
    /// - a first related record read through an unsorted self-join, with a
    ///   related record made before it (the first in creation order either
    ///   way: a record not reserved yet is the newest, a reserved or stored
    ///   one has its id's place);
    /// - fields given to aggregates, which skip empty values, that the
    ///   record leaves empty, stored and as written;
    ///
    /// and nothing that reads records by their place. A new row's auto-entry
    /// also answers the measured cases below (C126, s47). A record the write
    /// did not name ([`Written::Unknown`]) answers no self-join.
    fn answers_own(
        &self,
        ((context, occurrence), auto_entry): ((u64, u64), Option<bool>),
        row: &BTreeMap<u64, String>,
        found: &[(u64, BTreeMap<u64, String>)],
        uses: &Uses,
        session: &Evaluation,
    ) -> Result<bool, RunError> {
        if session.written == Written::Unknown {
            return Ok(false);
        }
        let path = self.schema.path(context, occurrence);
        let hop = path.as_ref().and_then(|path| path.first());
        let unequal = hop.is_some_and(grouped_unequal_self_join);
        if unequal
            && hop.is_some_and(|hop| {
                hop.predicates.iter().any(|predicate| {
                    row.get(&predicate.left)
                        .is_some_and(|value| !single_line_key(value))
                })
            })
        {
            return Ok(false);
        }
        // An empty match value relates nothing (file-metadata.md 4.3, rule
        // 2 [O]): with a match field on the record's side
        // empty, as the write computes it, the related set is empty, the
        // record itself among none. That value is final here: a key the
        // write computes is computed before what reads through it, a reader
        // of a key left out is left out itself, and one read with other keys
        // is not answered. Only a field the record's row holds counts (not a
        // global or unstored calculation, whose value is not in it).
        let empty_key = hop.is_some_and(|hop| {
            hop.predicates.iter().any(|p| {
                self.fields.get(&p.left).is_some_and(PlannedField::in_row)
                    && self
                        .cells(p.left)
                        .all(|cell| row.get(&cell).is_none_or(String::is_empty))
            })
        });
        if empty_key && found.is_empty() {
            return Ok(true);
        }
        // The host's related reads can include pending writes and omit
        // pending deletes. s47 read committed membership, so a new row
        // with held peers cannot use these reads, even in the fallbacks
        // that are independent of the written row's own membership.
        if unequal || matches!(session.written, Written::New(_)) {
            let others = session.others_held.contains(&self.table)
                || session
                    .held
                    .as_ref()
                    .is_some_and(|held| held.holds_other(self.table, session.written.id()));
            if others {
                return Ok(false);
            }
        }
        // Both sides must be final before accepting a computed self-key.
        let final_value = |field: &u64| {
            self.fields.get(field).is_some_and(|f| {
                f.in_row()
                    && (f.given() || f.expression.is_some() && session.settled.contains(field))
            })
        };
        let final_keys = hop.is_some_and(|hop| {
            hop.predicates
                .iter()
                .all(|p| final_value(&p.left) && final_value(&p.right))
        });
        // Measured: a new record's auto-entry reading a one-hop self-join
        // by final match fields on both sides reads the other records
        // committed with its key, never itself. #1035's native cases (a
        // local file) read the relationship as empty with no other match
        // (Count 0, List empty, the first related record's fields empty),
        // also with a computed key; C126's aggregate read the one older
        // match's values only, with a computed key. FileMaker Server's
        // engine (s47, records a script made) read zero to three older
        // matches of an unsorted equality join, with given keys, in an
        // auto-entry that replaces an existing value: Count, List and the
        // first related record read them in creation order. Not measured:
        // reads by place, sorted or non-equality joins with matches, and
        // the stored result when auto-entry keeps its creation-time value.
        if let (false, Some(replaces), Written::New(own)) = (unequal, auto_entry, session.written) {
            let unsorted_equal = hop.is_some_and(|hop| {
                !hop.sorted()
                    && hop
                        .predicates
                        .iter()
                        .all(|p| p.op == crate::host::JoinOp::Equal)
            });
            let older = found
                .iter()
                .all(|(record, _)| own.is_none_or(|id| *record < id));
            let one_older_match = found.len() == 1
                && older
                && !uses.first
                && !uses.placed
                && !uses.aggregated.is_empty()
                && unsorted_equal
                && hop.is_some_and(|hop| {
                    hop.predicates.iter().any(|p| {
                        self.fields
                            .get(&p.left)
                            .is_some_and(|f| !f.stored_calculation() && f.expression.is_some())
                    })
                });
            let older_matches = replaces && older && !uses.placed && unsorted_equal;
            if final_keys && (found.is_empty() || one_older_match || older_matches) {
                return Ok(true);
            }
        }
        let cx = Ctx::new(&self.schema.locale, session.today());
        // The record as written (0) and as stored (1, an existing one's).
        let mut itself = MemorySource::new();
        itself.put_record(self.table, 0, self.row_values(row, &cx, None));
        if !session.stored.is_empty() {
            itself.put_record(self.table, 1, self.row_values(&session.stored, &cx, None));
        }
        let related =
            crate::host::related_by_scan(&itself, &self.schema, context, 0, occurrence, &cx)
                .map_err(|e| {
                    RunError::Unread(DataError::with_code(
                        e.code(),
                        "A self-join a calculation reads could not be followed. No values \
                         were changed.",
                    ))
                })?;
        // Not related to itself as written (the relationship's own
        // comparison, over the record's row): the answer is the other
        // records' whether or not a record is among its own, while every
        // match field it is compared by, on either side, holds its final
        // value: one the write gives, or a computed one whose turn has
        // passed and that was not left out. A field the row does not hold
        // (a global, an unstored calculation) never counts as final.
        if related.is_empty() && final_keys {
            return Ok(true);
        }
        // For the grouped unequal case (#297), the record must be
        // excluded both as written and as stored. Do not infer its
        // membership from the cases measured for equality-only joins.
        if unequal {
            return Ok(false);
        }
        let earlier = match session.written {
            Written::New(None) => !found.is_empty(),
            Written::New(Some(id)) | Written::Stored(id) => {
                found.first().is_some_and(|(record, _)| *record < id)
            }
            Written::Unknown => false,
        };
        let sorted = hop.is_none_or(crate::host::Hop::sorted);
        let empty = |field: &u64| {
            row.get(field).is_none_or(String::is_empty)
                && session.stored.get(field).is_none_or(String::is_empty)
        };
        Ok(!uses.placed
            && (!uses.first || (earlier && !sorted))
            && uses.aggregated.iter().all(empty))
    }

    fn row_values(
        &self,
        row: &BTreeMap<u64, String>,
        cx: &Ctx,
        containers: Option<&(dyn ContainerReader + Send + Sync)>,
    ) -> RecordValues {
        let mut values = row_values(&self.schema, self.table, row, cx);
        let Some(containers) = containers else {
            return values;
        };
        for (cell, text) in self.container_cells(row) {
            if containers::reference(text).is_some() {
                continue;
            }
            if let Some(read) = containers.value(self.table, text) {
                let (field, rep) = match self.schema.table(self.table) {
                    Some(t) if t.fields.contains_key(cell) => (*cell, 1),
                    _ => (cell >> 8, (cell & 0xff) as u32),
                };
                values.set(field, rep, Value::Container(read));
            }
        }
        values
    }

    /// Whether a stored or auto-enter calculation of the table reads one of
    /// its container fields: a write then needs container data
    /// ([`RecordEnvironment::containers`]).
    pub fn reads_containers(&self) -> bool {
        let Some(table) = self.schema.table(self.table) else {
            return false;
        };
        self.fields.values().any(|f| {
            f.expression.iter().chain(f.validation.iter()).any(|e| {
                e.dependencies.iter().any(|d| {
                    table
                        .fields
                        .get(d)
                        .is_some_and(|f| f.value_type == ValueType::Container)
                })
            })
        })
    }

    /// The table's container fields' cells among `row`'s (repetition n of
    /// field F is `F * 256 + n`), with their texts.
    pub fn container_cells<'r>(
        &self,
        row: impl IntoIterator<Item = (&'r u64, &'r String)>,
    ) -> Vec<(&'r u64, &'r String)> {
        let Some(table) = self.schema.table(self.table) else {
            return Vec::new();
        };
        let container = |id: u64| {
            table
                .fields
                .get(&id)
                .is_some_and(|f| f.value_type == ValueType::Container)
        };
        row.into_iter()
            .filter(|(cell, _)| container(**cell) || (**cell > 0xff && container(**cell >> 8)))
            .collect()
    }

    /// Evaluate `expression` on `row` with `Self` and result type, in
    /// `repetition` (repetition 1 of a calculation for a field's own
    /// value); a calculation the host cannot answer refuses the write.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        field: u64,
        expression: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        self_value: Option<Value>,
        result_type: Option<ValueType>,
        repetition: InRepetition,
        remaining: &mut u64,
    ) -> Result<(Value, String), RunError> {
        // An answer the write's operation did not supply is never read as
        // empty: the write is refused.
        if let Some(missing) = context_selectors(&self.schema, &expression.formula.polish)
            .into_iter()
            .find(|selector| session.unanswered.contains(selector))
        {
            let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
            return Err(RunError::Unread(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {name}: {} is not known for this write. No values were changed.",
                    selector_name(missing)
                ),
            )));
        }
        let today = session.today();
        let cx = Ctx::new(&self.schema.locale, today);
        let containers = session.containers.as_deref();
        let values = self.row_values(row, &cx, containers);
        let mut inner = MemorySource::new();
        inner.put_record(self.table, 0, values);
        // The first related record of each occurrence the formula reads
        // through from its context, as the write's lookups read it: the
        // scan then finds it as it finds a related record in a host's
        // records. Two occurrences may relate the same record: what each
        // read of it is merged.
        let context = expression.formula.occurrence().unwrap_or_default();
        let occurrences: BTreeSet<u64> = expression.related.iter().map(|(o, _)| *o).collect();
        let reads = self.related_reads();
        let mut records: BTreeMap<(u64, u64), RecordValues> = BTreeMap::new();
        // Reads whose keys the write has computed since (#1035), and their
        // occurrences, which the evaluation then may not follow.
        let mut stale = Vec::new();
        let mut unfinished = BTreeSet::new();
        let mut blocked = BTreeSet::new();
        for occurrence in occurrences {
            let Some(read) = reads.get(&(context, occurrence)) else {
                continue;
            };
            if read.matches.iter().any(|key| {
                self.fields.get(key).is_some_and(|f| f.stored_calculation())
                    && !session.settled.contains(key)
            }) {
                unfinished.insert(occurrence);
                blocked.insert(occurrence);
                continue;
            }
            let keys: Vec<String> = read
                .matches
                .iter()
                .map(|m| row.get(m).cloned().unwrap_or_default())
                .collect();
            let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
            let unread = || {
                RunError::Unread(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {name}: the related record its calculation reads was not read. \
                         No values were changed."
                    ),
                ))
            };
            let (read_keys, found) = session
                .related
                .get(&(context, occurrence))
                .ok_or_else(unread)?;
            let Some(table) = self.schema.table_of(occurrence) else {
                continue;
            };
            // Read with other keys than the write now leaves: a match field
            // the write computed itself. Only a branch that follows the
            // relationship needs it again.
            if *read_keys != keys {
                blocked.insert(occurrence);
                stale.push(((context, occurrence), keys));
                continue;
            }
            // A self-join, answered only where the written record's own
            // membership cannot change the result (#1035).
            if table.id == self.table {
                let uses = expression
                    .uses
                    .get(&occurrence)
                    .cloned()
                    .unwrap_or_default();
                let auto_entered = self.fields.get(&field).filter(|f| {
                    !f.stored_calculation()
                        && f.expression
                            .as_ref()
                            .is_some_and(|e| std::ptr::eq(e, expression))
                });
                let entry = auto_entered.map(|f| f.options.calculation_replaces_existing());
                let read = ((context, occurrence), entry);
                if !self.answers_own(read, row, found, &uses, session)? {
                    blocked.insert(occurrence);
                    continue;
                }
            }
            for (record, texts) in found {
                let values = records.entry((table.id, *record)).or_default();
                for (id, text) in texts {
                    let value_type = table
                        .fields
                        .get(id)
                        .map_or(ValueType::Text, |d| d.value_type);
                    values.set(*id, 1, stored_value(text, value_type, &cx));
                }
            }
        }
        for ((table, record), values) in records {
            inner.put_record(table, record, values);
        }
        // A global field reads as the session's value (5.17).
        if let Some(globals) = self
            .session_globals(field, expression, session)
            .map_err(RunError::Unread)?
        {
            inner.set_globals(self.table, (*globals).clone());
        }
        let mut global_values = BTreeMap::<u64, RecordValues>::new();
        for field in &expression.other_globals {
            let text = session
                .other_globals
                .0
                .get(field)
                .ok_or_else(missing_other_globals)?;
            global_values
                .entry(field.0)
                .or_default()
                .set(field.1, 1, Value::text(text.clone()));
        }
        for (table, values) in global_values {
            inner.set_globals(table, values);
        }
        let source = RuleSource {
            inner,
            own: self.table,
            containers,
            blocked,
            followed: std::cell::RefCell::default(),
        };
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        if let Some(snapshot) = &session.file_variables {
            let name = self.fields.get(&field).and_then(|rule| {
                rule.expression
                    .as_ref()
                    .filter(|e| std::ptr::eq(*e, expression))
                    .and_then(|_| rule.auto_entry_file_variable())
            });
            if let Some((name, value)) =
                name.and_then(|name| snapshot.0.get(name, 1).map(|value| (name, value)))
            {
                if !matches!(value, Value::Text(_)) {
                    return Err(RunError::Unread(DataError::with_code(
                        error::UNAVAILABLE,
                        "A non-Text script variable cannot be auto-entered into Text.",
                    )));
                }
                globals.set(name, 1, value.clone());
            }
        }
        // Rules belong to the table's file, including when a root layout
        // edits an imported table. Caller layout/script identity stays outside
        // this record-only evaluation.
        let mut info = session.info.clone();
        info.file_name = self.schema.file_name.clone();
        let mut host = FileHost::new(
            FileCalc::new(&self.schema, &source, today).with_random(session.random.as_deref()),
            &info,
            &mut locals,
            &mut globals,
        );
        host.interrupted = session
            .interrupted
            .as_deref()
            .map(|f| f as &dyn Fn() -> bool);
        host.limits = session.limits;
        host.file_context = session.file_context;
        let call = std::cell::Cell::new(0);
        let sample_clock = || {
            let index = call.get();
            call.set(index + 1);
            session
                .creation_utc_clock
                .as_ref()
                .ok_or(om_calc::CalcError::NOT_SUPPORTED)?
                .sample(field, index)
        };
        if session.creation_utc_clock.is_some() {
            host.creation_utc_sample = Some(&sample_clock);
        }
        let (repetition, extend_fields) = match repetition {
            InRepetition::Validation(n) => (n, n > 1),
            InRepetition::Calculation(n) => (n, false),
        };
        let context = Context {
            occurrence: expression.formula.occurrence(),
            record: Some(0),
            repetition,
            result_type,
            self_value,
            extend_fields,
            // `Get ( RecordNumber )` answers the operation's number, as a
            // portal row's does; the record's position in this one-record
            // source is no answer. Without one, a formula reading it was
            // refused above.
            portal_row_number: session.record_number.map(RecordNumber::value),
            ..Context::default()
        };
        let result = evaluate_formula_with_calculation_budget(
            &expression.formula,
            &mut host,
            &context,
            remaining,
        )
        .map_err(|_| {
            let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
            let code = om_calc::CalcError::OUT_OF_MEMORY.code();
            RunError::Exhausted(DataError::with_code(
                code,
                format!(
                    "Field {name}: its calculation exceeded the unstored evaluation allowance \
                     (error {code}). No values were changed."
                ),
            ))
        })?;
        let followed = source.followed.take();
        if !followed.is_empty() {
            let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
            if followed
                .iter()
                .any(|occurrence| unfinished.contains(occurrence))
            {
                return Err(RunError::Unread(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {name}: its related match key is not final. No values were changed."
                    ),
                )));
            }
            let stale: Vec<_> = stale
                .into_iter()
                .filter(|(read, _)| followed.contains(&read.1))
                .collect();
            // Only self-joins whose answer depends on the written record.
            if stale.is_empty() {
                return Err(RunError::Unread(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {name}: whether this record is among its own related records \
                         through a self-join is not known for this write (#1035). No values \
                         were changed."
                    ),
                )));
            }
            let Some(needs) = &session.related_needs else {
                return Err(RunError::Unread(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {name}: its calculation follows a relationship by a match field \
                         the write computes, which this host does not read again. No values \
                         were changed."
                    ),
                )));
            };
            // Only the relationships this evaluation followed: another one
            // read with a stale key is not read again for nothing.
            for (read, keys) in stale {
                needs.need(read, keys);
            }
            return Err(RunError::Unread(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {name}: its calculation follows a relationship by a match field the \
                     write computes; it was not read again. No values were changed."
                ),
            )));
        }
        if let Some(failure) = result.error {
            let name = self.fields.get(&field).map_or("", |f| f.name.as_str());
            return Err(RunError::Failed(DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {name}: its calculation failed (error {}). No values were changed.",
                    failure.code()
                ),
            )));
        }
        let text = display(&result.value, &host);
        Ok((result.value, text))
    }

    fn repeating_filename_updates(
        &self,
        target: u64,
        expression: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        mode: WriteMode,
    ) -> Result<Vec<(u64, String)>, RunError> {
        let source = *expression
            .dependencies
            .first()
            .expect("measured one-field rule");
        let field = &self.fields[&target];
        let mut budget = session.calculation_budget();
        let mut updates = Vec::new();
        for repetition in 1..=u32::from(field.options.max_repetitions) {
            let cell = |id| {
                cell_of(id, repetition)
                    .filter(|cell| self.cell_field(*cell) == Some((id, repetition)))
                    .ok_or_else(|| {
                        RunError::Unread(DataError::with_code(
                            error::UNAVAILABLE,
                            format!("Field {}: ambiguous repetition cell.", field.name),
                        ))
                    })
            };
            let input = cell(source)?;
            let proposed = row.get(&input).map_or("", String::as_str);
            if mode == WriteMode::Create && !proposed.is_empty() {
                return Err(RunError::Unread(DataError::with_code(
                    error::UNAVAILABLE,
                    format!(
                        "Field {}: creation with a container already set is unsupported.",
                        field.name
                    ),
                )));
            }
            if proposed != session.stored.get(&input).map_or("", String::as_str) {
                let value = if proposed.is_empty() {
                    String::new()
                } else {
                    self.evaluate_repetition(
                        target,
                        repetition,
                        expression,
                        row,
                        session,
                        &mut budget,
                    )?
                };
                updates.push((cell(target)?, value));
            }
        }
        Ok(updates)
    }

    /// The new stored text of repetition `repetition` of calculated field
    /// `id`, evaluated in that repetition (calc-values.md 8.6).
    fn evaluate_repetition(
        &self,
        id: u64,
        repetition: u32,
        expression: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        remaining: &mut u64,
    ) -> Result<String, RunError> {
        self.evaluate_repetition_value(id, repetition, expression, row, session, remaining)
            .map(|(_, text)| text)
    }

    /// The typed value and stored text of repetition `repetition` of
    /// calculated field `id`, evaluated in that repetition.
    fn evaluate_repetition_value(
        &self,
        id: u64,
        repetition: u32,
        expression: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
        remaining: &mut u64,
    ) -> Result<(Value, String), RunError> {
        let def = self
            .schema
            .table(self.table)
            .and_then(|t| t.fields.get(&id));
        let result_type = def.map(|d| d.value_type);
        let repetition = InRepetition::Calculation(repetition);
        self.run(
            id,
            expression,
            row,
            session,
            None,
            result_type,
            repetition,
            remaining,
        )
    }

    /// Whether repetition `repetition` of field `id` passes its validation
    /// calculation, evaluated in that repetition: `Self`, the field and
    /// the table's other repeating fields are repetition n's, `Get (
    /// CalculationRepetitionNumber )` is n.
    fn validate(
        &self,
        id: u64,
        repetition: u32,
        validation: &Expression,
        row: &BTreeMap<u64, String>,
        session: &Evaluation,
    ) -> Result<bool, DataError> {
        let cx = Ctx::new(&self.schema.locale, session.today());
        let def = self
            .schema
            .table(self.table)
            .and_then(|t| t.fields.get(&id));
        let value = cell_of(id, repetition)
            .and_then(|cell| row.get(&cell))
            .map_or("", String::as_str);
        let self_value = stored_value(value, def.map_or(ValueType::Text, |d| d.value_type), &cx);
        // A calculation that fails (a division by zero) accepts nothing:
        // 507, as FileMaker Server answers (script oracle s17), where a
        // field's calculation that fails refuses the write with 3. One the
        // write cannot evaluate (an answer or related record it lacks) is
        // no failure of the value: the write is refused with 3.
        match self.run(
            id,
            validation,
            row,
            session,
            Some(self_value),
            None,
            InRepetition::Validation(repetition),
            &mut session.calculation_budget(),
        ) {
            Ok((result, _)) => Ok(result.is_true(&cx)),
            Err(RunError::Failed(_)) => Ok(false),
            Err(error) => Err(error.into_error()),
        }
    }
}

/// A row of stored text as typed values for `table`'s fields; ids the
/// table does not define read as text.
fn row_values(
    schema: &CalcSchema,
    table: u64,
    row: &BTreeMap<u64, String>,
    cx: &Ctx,
) -> RecordValues {
    let mut values = RecordValues::default();
    let defs = schema.table(table).map(|t| &t.fields);
    for (cell, text) in row {
        let (field, rep) = match defs.and_then(|d| d.get(cell)) {
            Some(_) => (*cell, 1),
            // `F ++ [n]`: repetition n of field F (calc.rs, `cell_key`).
            None if *cell > 0xff && defs.is_some_and(|d| d.contains_key(&(cell >> 8))) => {
                (cell >> 8, (cell & 0xff) as u32)
            }
            None => (*cell, 1),
        };
        let ty = defs
            .and_then(|d| d.get(&field))
            .map_or(ValueType::Text, |d| d.value_type);
        values.set(field, rep, stored_value(text, ty, cx));
    }
    values
}

/// "Numeric only": the text is a number in the pure form of
/// `Number::parse_text` (`1.5`, `-3`, `1e3`; not `12abc`, script oracle
/// s18).
fn numeric(text: &str, cx: &Ctx) -> bool {
    let locale = cx.locale;
    Number::parse_pure_text(text, locale.decimal_separator, locale.thousands_separator)
        .is_some_and(|n| !n.is_nan())
}

/// "4-digit year date": the text reads as a date (a timestamp's date
/// part) whose year is written with four digits or more: `1/2/2026` and
/// `1/2/0026`, not `1/2/26` (script oracle s18). The year is the number in
/// the date order's year place.
fn four_digit_year(text: &str, cx: &Ctx) -> bool {
    let date = text.split_whitespace().next().unwrap_or_default();
    if !matches!(Date::parse(date, cx.locale, cx.today), Ok(Some(_))) {
        return false;
    }
    let numbers: Vec<&str> = date
        .split(|c: char| !c.is_ascii_digit())
        .filter(|n| !n.is_empty())
        .collect();
    let year = match cx.locale.date_order {
        DateOrder::YearMonthDay => numbers.first(),
        _ => numbers.get(2),
    };
    year.is_some_and(|y| y.len() >= 4)
}

/// "Time of day": the text reads as a time from 0:00 to before 24:00
/// (`25:00` and `-1:00` are not, script oracle s18).
fn time_of_day(text: &str, cx: &Ctx) -> bool {
    match Time::parse(text, cx.locale, cx.today) {
        Ok(Some(time)) => (0..24 * 3_600_000_000).contains(&time.micros()),
        _ => false,
    }
}

/// The stored text `text` of a field of `data_type` as the value its rules
/// compare: a number, date, time or timestamp converted, `None` when it
/// does not convert (`abc` in a number field); text as it is.
fn typed(data_type: DataType, text: &str, cx: &Ctx) -> Option<Value> {
    let ty = match data_type {
        DataType::Number => ValueType::Number,
        DataType::Date => ValueType::Date,
        DataType::Time => ValueType::Time,
        DataType::Timestamp => ValueType::Timestamp,
        _ => return Some(Value::text(text)),
    };
    let value = Value::text(text).convert_declared(ty, cx);
    match ty {
        ValueType::Number => {
            let number = value.to_number(cx);
            (!number.is_nan() && !number.is_empty()).then_some(Value::Number(number))
        }
        _ => (!value.is_invalid() && !value.is_empty()).then_some(value),
    }
}

/// "In range": the value, converted to the field's type, is from `from`
/// to `to` (an empty bound is open); a value that does not convert is out
/// of range (`abc` in a number field: 503, script oracle s18).
fn in_range(data_type: DataType, text: &str, from: &str, to: &str, cx: &Ctx) -> bool {
    let Some(value) = typed(data_type, text, cx) else {
        return false;
    };
    let within = |bound: &str, op: CompareOp| {
        bound.is_empty()
            || typed(data_type, bound, cx)
                .is_some_and(|b| ops::compare_op(&value, op, &b, cx).is_true(cx))
    };
    within(from, CompareOp::Ge) && within(to, CompareOp::Le)
}

/// Whether `text`, a value of a field of `data_type`, is the value `listed`
/// of a list of a field's values, as a value list check compares them
/// (script-runtime.md 13.21): other types by value, text as the list's
/// field keys it in its index, `keys` (its index language's form,
/// [`value_text_keys`]; case does not matter in English, script oracle
/// s18). `None` when they compare as text and the field's language has no
/// form OpenMaker builds (`keys` `None`, the tailored ones, #299): English
/// keys could both miss its value and match another (#571).
pub fn same_list_value(
    data_type: DataType,
    listed: &str,
    text: &str,
    cx: &Ctx,
    keys: Option<om_query::keys::TextKeys>,
) -> Option<bool> {
    match (
        data_type,
        typed(data_type, listed, cx),
        typed(data_type, text, cx),
    ) {
        (DataType::Text, ..) | (_, None, _) | (_, _, None) => {
            let keys = keys?;
            Some(keys.whole_key(listed) == keys.whole_key(text))
        }
        (_, Some(a), Some(b)) => Some(ops::compare_op(&a, CompareOp::Eq, &b, cx).is_true(cx)),
    }
}

/// Whether an entered value is a custom value list's value: text compares
/// as an English index key (case and accents folded: `mr.` is `Mr.`,
/// script oracle s18), other types by value.
fn same_entry(data_type: DataType, listed: &str, text: &str, cx: &Ctx) -> bool {
    match (
        data_type,
        typed(data_type, listed, cx),
        typed(data_type, text, cx),
    ) {
        (DataType::Text, ..) | (_, None, _) | (_, _, None) => {
            om_collate::text_key(listed) == om_collate::text_key(text)
        }
        (_, Some(a), Some(b)) => ops::compare_op(&a, CompareOp::Eq, &b, cx).is_true(cx),
    }
}

/// The value list a field's "member of value list" names (`07`, a key
/// string of the list's id, then bookkeeping) and what it accepts: a
/// custom list's values, or the values of the list's field (file-metadata.md
/// 6). The list is the file's (`lists`, the calculation schema's), else the
/// tree's. Lists of another file, a calculation, related values or a
/// second field are not checked yet ([`ListRule::Unchecked`]).
fn list_rule(
    tree: &KeyTree<'_>,
    field: Option<&Node<'_>>,
    lists: &[ValueList],
) -> Result<ListRule, String> {
    let reference = field
        .and_then(|n| n.get(FIELD_VALUE_LIST))
        .and_then(|n| n.assembled())
        .ok_or("its value list is missing.")?;
    let id = crate::meta::key_string(&reference).ok_or("its value list is missing.")?;
    let path = [VALUE_LISTS[0], VALUE_LISTS[1], id];
    let list = lists
        .iter()
        .find(|l| l.id == id)
        .cloned()
        .or_else(|| tree.at(&path).and_then(|n| ValueList::from_node(n)))
        .ok_or("its value list is missing.")?;
    Ok(rule_of(list).unwrap_or_else(ListRule::Unchecked))
}

/// Why record rules cannot check a field's values against value list
/// `list`, if they cannot: they check custom values and one field's values
/// only.
pub fn value_list_problem(list: &ValueList) -> Option<String> {
    rule_of(list.clone()).err()
}

fn rule_of(list: ValueList) -> Result<ListRule, String> {
    match &list.source {
        ValueSource::Custom(values) => Ok(ListRule::Custom(
            values.iter().filter(|v| v.trim() != "-").cloned().collect(),
        )),
        ValueSource::Field {
            primary,
            secondary: None,
            start: None,
        } => Ok(ListRule::Field {
            occurrence: primary
                .occurrence
                .ok_or("its value list's field has no table occurrence.")?,
            field: primary.field,
        }),
        ValueSource::Field {
            primary,
            secondary: None,
            start: Some(_),
        } if primary.occurrence.is_some() => Ok(ListRule::Related(Box::new(list))),
        _ => {
            Err("value lists other than custom values or a field's values are unsupported.".into())
        }
    }
}

fn missing_other_globals() -> RunError {
    RunError::Unread(DataError::with_code(
        error::UNAVAILABLE,
        "Global fields read by auto-entry were not supplied. No values were changed.",
    ))
}

fn assign(row: &mut BTreeMap<u64, String>, dirty: &mut BTreeSet<u64>, id: u64, value: String) {
    if row.get(&id).map_or("", String::as_str) != value {
        row.insert(id, value);
        dirty.insert(id);
    }
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn evaluation_order(fields: &BTreeMap<u64, PlannedField>) -> Result<Vec<u64>, DataError> {
    fn visit(
        id: u64,
        fields: &BTreeMap<u64, PlannedField>,
        pending: &mut BTreeSet<u64>,
        done: &mut BTreeSet<u64>,
        order: &mut Vec<u64>,
    ) -> Result<(), DataError> {
        if done.contains(&id) {
            return Ok(());
        }
        if !pending.insert(id) {
            return Err(DataError::new(
                "Cyclic stored calculation dependencies are unsupported.",
            ));
        }
        {
            for dependency in fields[&id].dependencies() {
                // An auto-entered formula reads its own value before this
                // evaluation. Keep the dependency for dirty/empty checks,
                // but it does not order the field after itself (#1156).
                if *dependency == id
                    && fields[&id].expression.is_some()
                    && !fields[&id].stored_calculation()
                {
                    continue;
                }
                visit(*dependency, fields, pending, done, order)?;
            }
        }
        pending.remove(&id);
        done.insert(id);
        order.push(id);
        Ok(())
    }
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut pending = BTreeSet::new();
    for id in fields.keys() {
        visit(*id, fields, &mut pending, &mut done, &mut order)?;
    }
    Ok(order)
}

impl Expression {
    /// Whether this formula, or an unstored calculation it reads
    /// ([`Self::through_formulas`]), itself or through the custom functions
    /// it calls, has a part `test` holds for.
    fn reads_any(&self, schema: &CalcSchema, test: &dyn Fn(&Expr) -> bool) -> bool {
        reads_any(schema, &self.formula.polish, test)
            || self
                .through_formulas
                .iter()
                .any(|formula| reads_any(schema, &formula.polish, test))
    }

    /// Evaluate on a row of stored text whose fields are all text, as the
    /// field's calculation would on a record of a table of text fields.
    #[cfg(test)]
    fn evaluate(
        &self,
        row: &BTreeMap<u64, String>,
        env: &RecordEnvironment,
    ) -> Result<String, DataError> {
        let mut schema = CalcSchema::new("", Locale::en_us());
        let table = 1;
        schema.add_table(table, "Table");
        for id in row.keys().chain(self.dependencies.iter()) {
            schema.add_field(
                table,
                crate::host::FieldDef::data(*id, format!("f{id}"), ValueType::Text),
            );
        }
        if let Some(occurrence) = self.formula.occurrence() {
            schema.add_occurrence(occurrence, "Table", table);
        }
        let plan = RecordPlan {
            table,
            fields: BTreeMap::new(),
            evaluation_order: Vec::new(),
            grouped_self_joins: BTreeSet::new(),
            schema: Arc::new(schema),
        };
        let first = InRepetition::Calculation(1);
        let session = env.session();
        plan.run(
            0,
            self,
            row,
            &session,
            None,
            Some(ValueType::Text),
            first,
            &mut session.calculation_budget(),
        )
        .map(|(_, text)| text)
        .map_err(RunError::into_error)
    }
}

/// How many fields and calculations the unstored readings of one plan
/// may hold together ([`UnstoredGraph`]): a hostile file's graph of
/// unstored calculations is refused rather than planned without bound.
const MAX_UNSTORED_READS: usize = 1 << 20;

/// How many evaluations of unstored calculations one record rule may
/// take: the calculation host evaluates one on every read, so a chain that
/// reads each step twice doubles at each. Planning refuses a formula whose
/// reads take more ([`UnstoredReads::cost`]); a loop or the repetitions of
/// an aggregate read again, which planning does not count. The runtime
/// allowance covers both the empty-input probes and the formula body.
const MAX_UNSTORED_EVALUATIONS: u64 = 1 << 16;

/// How many times `polish` reads each field of its own occurrence.
fn field_read_counts(polish: &Expr) -> BTreeMap<u64, u64> {
    let mut reads = BTreeMap::new();
    walk(polish, &mut |e| {
        if let Expr::Field(field) | Expr::FieldRef(field) = e {
            if let Some(id) = field.field.id() {
                *reads.entry(id).or_insert(0u64) += 1;
            }
        }
    });
    reads
}

/// The table's unstored calculations a write's formulas may read, as they
/// read each other ([`Self::reads`]).
struct UnstoredGraph<'a> {
    /// Each one's formula parsed as a stored formula, or why it cannot be.
    formulas: &'a BTreeMap<u64, Result<Expression, String>>,
    schema: &'a CalcSchema,
    table: u64,
    /// Each calculation's answer, so that one many calculations read is
    /// followed once.
    known: BTreeMap<u64, Result<UnstoredReads, String>>,
    /// What [`MAX_UNSTORED_READS`] leaves to hand out.
    budget: usize,
}

impl<'a> UnstoredGraph<'a> {
    fn new(
        formulas: &'a BTreeMap<u64, Result<Expression, String>>,
        schema: &'a CalcSchema,
        table: u64,
    ) -> Self {
        Self {
            formulas,
            schema,
            table,
            known: BTreeMap::new(),
            budget: MAX_UNSTORED_READS,
        }
    }

    /// The fields of the table that its unstored calculation `id` reads,
    /// through the unstored calculations it reads in turn, below the
    /// calculations of `stack`: an auto-entered formula reading `id` is
    /// evaluated again when one of them changes, as FileMaker Server's
    /// script engine evaluates it (auto-entry.md, "Formulas over an
    /// unstored calculation or a summary field"). Refused, with the reason,
    /// when `id` cannot be evaluated on a write's record: it reads another
    /// occurrence, a global, summary or container field, a missing field,
    /// `Get` functions a write does not answer, a custom function that
    /// reads fields, or itself; when the chain of calculations is deeper
    /// than the calculation host evaluates them (`MAX_READ_DEPTH`, where
    /// they would read `?`); or past [`MAX_UNSTORED_READS`].
    fn reads(&mut self, id: u64, stack: &mut Vec<u64>) -> Result<UnstoredReads, String> {
        let answer = match self.known.get(&id) {
            Some(answer) => answer.clone(),
            None => {
                let answer = self.reads_of(id, stack);
                self.known.insert(id, answer.clone());
                answer
            }
        }?;
        // A kept answer is checked against this reader's own chain too.
        if stack.len() + answer.depth > crate::host::MAX_READ_DEPTH as usize {
            return Err(
                "unstored calculations read each other more deeply than a write \
                        evaluates them."
                    .into(),
            );
        }
        let size = answer.inputs.len() + answer.calculations.len() + answer.contexts.len();
        self.budget = self.budget.checked_sub(size).ok_or_else(|| {
            "the table's unstored calculations read each other too widely to plan.".to_owned()
        })?;
        Ok(answer)
    }

    /// [`Self::reads`] of a calculation not followed yet.
    fn reads_of(&mut self, id: u64, stack: &mut Vec<u64>) -> Result<UnstoredReads, String> {
        let schema = self.schema;
        let fields = schema
            .table(self.table)
            .map(|t| &t.fields)
            .ok_or("its table is missing.")?;
        let def = fields.get(&id).ok_or("a field it reads is missing.")?;
        if stack.contains(&id) {
            return Err(format!("Field {} reads itself.", def.name));
        }
        if stack.len() >= crate::host::MAX_READ_DEPTH as usize {
            return Err(
                "unstored calculations read each other more deeply than a write \
                        evaluates them."
                    .into(),
            );
        }
        let expression = match self.formulas.get(&id) {
            Some(Ok(expression)) => expression,
            Some(Err(reason)) => return Err(reason.clone()),
            None => return Err(format!("Field {} cannot be evaluated.", def.name)),
        };
        // Evaluated from an occurrence of its own table, as the formulas
        // that read it; one the schema lacks is added once a reader is
        // planned.
        let context = expression.formula.occurrence();
        if let Some(occurrence) = context.and_then(|o| schema.occurrence(o)) {
            if occurrence.table != self.table {
                return Err(format!(
                    "Field {} is evaluated from another table's occurrence.",
                    def.name
                ));
            }
        }
        check_custom_functions(
            schema,
            &expression.formula.polish,
            false,
            false,
            &mut BTreeSet::new(),
        )
        .map_err(|reason| format!("Field {}: {reason}", def.name))?;
        stack.push(id);
        let mut reads = UnstoredReads {
            inputs: BTreeSet::new(),
            calculations: BTreeSet::from([id]),
            contexts: context
                .filter(|o| schema.occurrence(*o).is_none())
                .into_iter()
                .collect(),
            depth: 1,
            cost: 1,
        };
        let counts = field_read_counts(&expression.formula.polish);
        for dependency in &expression.dependencies {
            let input = fields
                .get(dependency)
                .ok_or_else(|| format!("Field {} references an unavailable field.", def.name))?;
            if input.global
                || input.class == crate::host::FieldClass::Summary
                || input.value_type == ValueType::Container
            {
                return Err(format!(
                    "Field {} reads a global, summary or container field.",
                    def.name
                ));
            }
            if input.is_unstored_calculation() {
                let read = self.reads(*dependency, stack)?;
                reads.inputs.extend(read.inputs);
                reads.calculations.extend(read.calculations);
                reads.contexts.extend(read.contexts);
                reads.depth = reads.depth.max(read.depth + 1);
                let times = counts.get(dependency).copied().unwrap_or(1);
                reads.cost = reads.cost.saturating_add(times.saturating_mul(read.cost));
                if reads.cost > MAX_UNSTORED_EVALUATIONS {
                    return Err(format!(
                        "Field {} evaluates unstored calculations too many times.",
                        def.name
                    ));
                }
            } else {
                reads.inputs.insert(*dependency);
            }
        }
        stack.pop();
        Ok(reads)
    }
}

/// What [`UnstoredGraph::reads`] gives: the stored fields an unstored
/// calculation reads, the unstored calculations on the way, itself
/// included, their context occurrences the schema lacks, how many
/// calculations deep its longest chain is, and how many evaluations a
/// read of it takes.
#[derive(Clone, Debug, Default)]
struct UnstoredReads {
    inputs: BTreeSet<u64>,
    calculations: BTreeSet<u64>,
    contexts: BTreeSet<u64>,
    depth: usize,
    /// How many unstored calculations one read of it evaluates, itself
    /// and each read of another counted.
    cost: u64,
}

/// Custom functions must obey the same record-only limits as their callers.
/// Field reads are also refused because their dependencies are not the
/// formula's own (custom function bodies have no context).
fn check_custom_functions(
    schema: &CalcSchema,
    expr: &Expr,
    option: bool,
    design: bool,
    seen: &mut BTreeSet<u64>,
) -> Result<(), String> {
    let mut result = Ok(());
    walk(expr, &mut |e| {
        if result.is_err() {
            return;
        }
        if let Expr::CustomCall { function, .. } = e {
            let Some(id) = function.id() else {
                result = Err("unknown custom function.".into());
                return;
            };
            if !seen.insert(id) {
                return;
            }
            let Some(body) = schema.custom_function(id) else {
                result = Err("calls a custom function this file does not define.".into());
                return;
            };
            let mut problem = None;
            walk(&body.body, &mut |inner| {
                if problem.is_some() {
                    return;
                }
                if matches!(inner, Expr::Field(_) | Expr::FieldRef(_))
                    || resolves_names_at_run_time(inner)
                {
                    problem = Some("calls a custom function that reads fields.".to_owned());
                } else if let Some(reason) = unsupported_rule_operation(inner, option, design) {
                    problem = Some(format!("calls a custom function: {reason}"));
                }
            });
            if let Some(problem) = problem {
                result = Err(problem);
                return;
            }
            if let Err(e) = check_custom_functions(schema, &body.body, option, design, seen) {
                result = Err(e);
            }
        }
    });
    result
}

/// The [`CONTEXT_SELECTORS`] `expr` reads, itself or through the custom
/// functions it calls.
fn context_selectors(schema: &CalcSchema, expr: &Expr) -> BTreeSet<u8> {
    fn visit(schema: &CalcSchema, expr: &Expr, seen: &mut BTreeSet<u64>, out: &mut BTreeSet<u8>) {
        walk(expr, &mut |e| match e {
            Expr::Get(selector) if CONTEXT_SELECTORS.contains(selector) => {
                out.insert(*selector);
            }
            Expr::CustomCall { function, .. } => {
                if let Some(body) = function
                    .id()
                    .filter(|id| seen.insert(*id))
                    .and_then(|id| schema.custom_function(id))
                {
                    visit(schema, &body.body, seen, out);
                }
            }
            _ => {}
        });
    }
    let mut out = BTreeSet::new();
    visit(schema, expr, &mut BTreeSet::new(), &mut out);
    out
}

/// What a field computed when read reads of records (#1063): a calculation
/// the host does not store, or a summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldReads {
    /// Fields of its own record; of every record of its set for a summary
    /// ([`Self::every_record`]).
    pub own: BTreeSet<u64>,
    /// Fields of other tables' records, by table and field: those read, and
    /// the match and sort fields of every relationship on the way there.
    pub other: BTreeSet<(u64, u64)>,
    /// A summary: it reads its fields on every record of its found or
    /// related set, not only its own.
    pub every_record: bool,
    /// It reads what it cannot name before it runs: a custom function,
    /// `Evaluate`, `GetField`, `ExecuteSQL`, or the reading context (its
    /// record number, the found set).
    pub opaque: bool,
    /// It answers differently each time: `Random`, the clock or a UUID.
    pub volatile: bool,
}

/// What field `field` of table `table` reads ([`FieldReads`]) when it is
/// computed when read: an unstored calculation or a summary. `None` for
/// any other field, whose value is its record's.
pub fn field_reads(schema: &CalcSchema, table: u64, field: u64) -> Option<FieldReads> {
    let def = schema.table(table)?.fields.get(&field)?;
    let mut reads = FieldReads::default();
    if let Some(summary) = &def.summary {
        reads.every_record = true;
        for source in summary.field.iter().chain(summary.additional.iter()) {
            match source.occurrence.and_then(|o| schema.table_of(o)) {
                Some(other) if other.id != table => {
                    reads.other.insert((other.id, source.field));
                }
                _ => {
                    reads.own.insert(source.field);
                }
            }
        }
        return Some(reads);
    }
    if !def.is_unstored_calculation() || def.global {
        return None;
    }
    let Some(formula) = def.formula.as_ref() else {
        reads.opaque = true;
        return Some(reads);
    };
    let context = formula.occurrence();
    reads.volatile = draws_random(schema, &formula.polish) || reads_clock(schema, &formula.polish);
    reads.opaque = !context_selectors(schema, &formula.polish).is_empty();
    walk(&formula.polish, &mut |e| match e {
        Expr::Field(reference) | Expr::FieldRef(reference) => {
            let (Some(occurrence), Some(id)) = (reference.occurrence.id(), reference.field.id())
            else {
                reads.opaque = true;
                return;
            };
            if context.is_none_or(|context| context == occurrence) {
                reads.own.insert(id);
                return;
            }
            let path = context.and_then(|context| schema.path(context, occurrence));
            let (Some(path), Some(to)) = (path, schema.table_of(occurrence)) else {
                reads.opaque = true;
                return;
            };
            reads.other.insert((to.id, id));
            // Every relationship on the way: the records between are found
            // by their match fields, and ordered by their sort fields.
            for (at, hop) in path.iter().enumerate() {
                let (Some(from), Some(there)) =
                    (schema.table_of(hop.from), schema.table_of(hop.to))
                else {
                    reads.opaque = true;
                    return;
                };
                for predicate in &hop.predicates {
                    // The first from its own record, the others from the
                    // records between.
                    match at {
                        0 => reads.own.insert(predicate.left),
                        _ => reads.other.insert((from.id, predicate.left)),
                    };
                    reads.other.insert((there.id, predicate.right));
                }
                reads.other.extend(sort_fields(hop).map(|f| (there.id, f)));
            }
        }
        Expr::CustomCall { .. } => reads.opaque = true,
        e if resolves_names_at_run_time(e) => reads.opaque = true,
        _ => {}
    });
    Some(reads)
}

/// Whether field `field` of table `table` reads as a value its commit may
/// not store, on record `record` (`None`: on any record of the table), by
/// `unknown` (#1063): one `unknown` names, or one computed when read
/// ([`field_reads`]) from such a value, through its own record or another,
/// or from the clock, a UUID or `Random`. Its reads of other records are
/// checked on any record of their tables, and a summary's on any record of
/// its own: which records it reads is not known here. What it cannot name
/// is unknown when `any` value is.
pub fn unknown_when_read(
    schema: &CalcSchema,
    (unknown, any): (UnknownValue<'_>, bool),
    (table, record, field): (u64, Option<u64>, u64),
) -> bool {
    fn at(
        schema: &CalcSchema,
        (unknown, any): (UnknownValue<'_>, bool),
        (table, record, field): (u64, Option<u64>, u64),
        seen: &mut BTreeSet<(u64, Option<u64>, u64)>,
    ) -> bool {
        if unknown(table, record, field) {
            return true;
        }
        if !seen.insert((table, record, field)) {
            return false;
        }
        let Some(reads) = field_reads(schema, table, field) else {
            return false;
        };
        if reads.volatile || reads.opaque && any {
            return true;
        }
        let own = match reads.every_record {
            true => None,
            false => record,
        };
        let unknown = (unknown, any);
        reads
            .own
            .iter()
            .any(|f| at(schema, unknown, (table, own, *f), seen))
            || reads
                .other
                .iter()
                .any(|(t, f)| at(schema, unknown, (*t, None, *f), seen))
    }
    at(
        schema,
        (unknown, any),
        (table, record, field),
        &mut BTreeSet::new(),
    )
}

/// Whether the records found through `path` from record `record` of
/// `table` (`None`: any of its records) may not be the ones a commit finds,
/// by `unknown` (#1063): a match or sort field along it reads as unknown
/// ([`unknown_when_read`]). The first relationship's match fields are the
/// record's own, a calculated one with what it reads; the others' are any
/// record's of their tables.
pub fn path_reads_unknown(
    schema: &CalcSchema,
    (unknown, any): (UnknownValue<'_>, bool),
    (table, record): (u64, Option<u64>),
    path: &[crate::host::Hop],
) -> bool {
    let reads = |at: (u64, Option<u64>, u64)| unknown_when_read(schema, (unknown, any), at);
    path.iter().enumerate().any(|(at, hop)| {
        let (Some(from), Some(to)) = (schema.table_of(hop.from), schema.table_of(hop.to)) else {
            return true;
        };
        let source = match at {
            0 => (table, record),
            _ => (from.id, None),
        };
        hop.predicates
            .iter()
            .any(|p| reads((source.0, source.1, p.left)) || reads((to.id, None, p.right)))
            || sort_fields(hop).any(|f| reads((to.id, None, f)))
    })
}

/// Whether `expr`, itself or through the custom functions it calls, reads
/// the clock, the host's clock or a UUID.
fn reads_clock(schema: &CalcSchema, expr: &Expr) -> bool {
    use crate::host::session::selector;
    let clock = [
        selector::CURRENT_DATE,
        selector::CURRENT_TIME,
        selector::CURRENT_TIMESTAMP,
        selector::CURRENT_HOST_TIMESTAMP,
        selector::UUID,
        selector::UUID_NUMBER,
        selector::CURRENT_TIME_UTC_MILLISECONDS,
        selector::CURRENT_TIME_UTC_MICROSECONDS,
    ];
    reads_any(
        schema,
        expr,
        &|e| matches!(e, Expr::Get(selector) if clock.contains(selector)),
    )
}

/// Whether `expr`, itself or through the custom functions it calls, calls
/// `Random` (om-calc's opcode 0x31), which answers a new number each time.
fn draws_random(schema: &CalcSchema, expr: &Expr) -> bool {
    reads_any(
        schema,
        expr,
        &|e| matches!(e, Expr::Call { op, .. } if op.0 == 0x31),
    )
}

/// Whether `expr`, itself or through the custom functions it calls, has a
/// part `test` holds for.
fn reads_any(schema: &CalcSchema, expr: &Expr, test: &dyn Fn(&Expr) -> bool) -> bool {
    fn visit(
        schema: &CalcSchema,
        expr: &Expr,
        test: &dyn Fn(&Expr) -> bool,
        seen: &mut BTreeSet<u64>,
    ) -> bool {
        let mut found = false;
        walk(expr, &mut |e| {
            if found {
                return;
            }
            if test(e) {
                found = true;
            } else if let Expr::CustomCall { function, .. } = e {
                if let Some(body) = function
                    .id()
                    .filter(|id| seen.insert(*id))
                    .and_then(|id| schema.custom_function(id))
                {
                    found = visit(schema, &body.body, test, seen);
                }
            }
        });
        found
    }
    visit(schema, expr, test, &mut BTreeSet::new())
}

/// How a refusal names a [`CONTEXT_SELECTORS`] selector.
fn selector_name(selector: u8) -> &'static str {
    match selector {
        selector::LAYOUT_NUMBER => "Get ( LayoutNumber )",
        selector::RECORD_NUMBER => "Get ( RecordNumber )",
        selector::CURRENT_PRIVILEGE_SET_NAME => "Get ( CurrentPrivilegeSetName )",
        selector::CURRENT_TIME_UTC_MILLISECONDS => "Get ( CurrentTimeUTCMilliseconds )",
        _ => "Get ( CurrentHostTimestamp )",
    }
}

const DYNAMIC_REFERENCES: &str =
    "Evaluate, GetField and ExecuteSQL are unsupported in record rules.";

/// Expression operations the record-only host cannot evaluate faithfully.
/// Apply this to both the formula and every reachable custom-function body.
/// `window_reads`: whether the operation can supply a selector from
/// [`CONTEXT_SELECTORS`]. `design`: a host's recomputation of stored values,
/// which answers the Design functions as the file's own calculation does
/// ([`crate::host::FileHost::file_context`]).
fn unsupported_rule_operation(
    expr: &Expr,
    window_reads: bool,
    design: bool,
) -> Option<&'static str> {
    match expr {
        Expr::PluginCall { function, .. } if design && is_design_function(*function) => None,
        Expr::PluginCall { .. } => Some("plug-in functions are unsupported in record rules."),
        expr if resolves_names_at_run_time(expr) => Some(DYNAMIC_REFERENCES),
        Expr::Get(selector)
            if !(RECORD_RULE_SELECTORS.contains(selector)
                || window_reads && CONTEXT_SELECTORS.contains(selector)) =>
        {
            Some("unsupported Get() selector.")
        }
        _ => None,
    }
}

/// Whether `function` is one of FileMaker's Design functions, which the
/// Polish stream stores as calls of the built-in plug-in `FMDR`
/// (calc-encoding.md 7).
fn is_design_function(function: om_formula::PluginFunctionId) -> bool {
    function.plugin == *b"FMDR" && om_formula::catalog::builtin(function).is_some()
}

/// Forms whose field reads are only known when they run (`Evaluate`,
/// `GetField`, `ExecuteSQL`/`ExecuteSQLe`): their dependencies cannot be
/// checked before a write, so a rule using them keeps the table read-only
/// rather than reading a global or related field as empty.
fn resolves_names_at_run_time(expr: &Expr) -> bool {
    match expr {
        Expr::Evaluate { .. } | Expr::GetField { .. } => true,
        Expr::Call { op, .. } => matches!(op.name(), Some("ExecuteSQL" | "ExecuteSQLe")),
        _ => false,
    }
}

/// Whether `expr` reads a field of an occurrence other than `context`, at
/// any depth (`R::f[1]` is `GetRepetition ( R::f ; 1 )`).
fn reads_other_occurrence(expr: &Expr, context: &[u8]) -> bool {
    let mut found = false;
    walk(expr, &mut |e| {
        if let Expr::Field(f) | Expr::FieldRef(f) = e {
            found |= f.occurrence.0 != context;
        }
    });
    found
}

/// The fields of `hop.to` its relationship sorts by.
fn sort_fields(hop: &crate::host::Hop) -> impl Iterator<Item = u64> + '_ {
    hop.sort.iter().flat_map(move |sort| {
        sort.fields
            .iter()
            .filter(move |e| e.field.occurrence.is_none_or(|o| o == hop.to))
            .map(|e| e.field.field)
    })
}

/// The text of a formula made of quoted text joined by `&` and nothing
/// else: a calculated custom message whose text needs no record.
fn literal_text(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Text(text) => Some(text.value.clone()),
        Expr::Call { op, args } if op.name() == Some("&") => match args.as_slice() {
            [left, right] => Some(literal_text(left)? + &literal_text(right)?),
            _ => None,
        },
        _ => None,
    }
}

/// Visit every node of `expr`.
fn walk(expr: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(expr);
    for child in children(expr) {
        walk(child, f);
    }
}

/// The direct parts of `expr`, in order.
fn children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::List(items)
        | Expr::Call { args: items, .. }
        | Expr::CustomCall { args: items, .. }
        | Expr::PluginCall { args: items, .. }
        | Expr::Evaluate { args: items, .. } => items.iter().collect(),
        Expr::Case(pairs) => pairs
            .iter()
            .flat_map(|(test, value)| [test, value])
            .collect(),
        Expr::Choose { test, results } => std::iter::once(&**test).chain(results).collect(),
        Expr::Let { bindings, body } => bindings.iter().chain([&**body]).collect(),
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => initial
            .iter()
            .chain([&**condition])
            .chain(logic)
            .chain([&**result])
            .collect(),
        Expr::GetField { arg, .. } => vec![&**arg],
        _ => Vec::new(),
    }
}

/// Decode a field's stored Polish stream (`om_formula::polish`) for
/// `om-calc`, with the stored context occurrence `context`. Fields must be
/// read through that occurrence (one-byte or longer numeric ids, written
/// without repetitions); plug-in functions and `Get` selectors that need a
/// window are refused.
/// Why record rules cannot evaluate a stored calculation's Polish stream
/// `code` from occurrence key `context`, if they cannot: the formula-level
/// checks OpenMaker Server's recalculation also makes (`recalc::plan`).
pub(crate) fn stored_formula_problem(code: &[u8], context: &[u8]) -> Option<String> {
    parse_code(code, context).err()
}

/// [`stored_formula_problem`] for an auto-entered or validation formula,
/// which may also read fields of other occurrences; whether the
/// relationships it reads through are ones the rules follow is the plan's
/// to say (`related_hop`).
pub(crate) fn option_formula_problem(code: &[u8], context: &[u8]) -> Option<String> {
    parse_formula(code, context, FormulaUse::Options).err()
}

/// [`option_formula_problem`] for a calculated custom message, which the
/// rules evaluate over the record alone ([`CustomMessage::Calculation`]):
/// no related field, and no answer only the write's context gives.
pub(crate) fn message_formula_problem(code: &[u8], context: &[u8]) -> Option<String> {
    parse_code(code, context).err()
}

/// The occurrence (its key as the formula holds it) of the first field
/// `code` reads: the context of a formula stored without one, as a custom
/// privilege set's limit is (session-identity.md 2.8).
pub(crate) fn first_field_occurrence(code: &[u8]) -> Option<Vec<u8>> {
    if code.len() > MAX_CODE {
        return None;
    }
    let limits = om_formula::polish::Limits {
        depth: MAX_DEPTH,
        nodes: MAX_NODES,
    };
    let polish = om_formula::polish::decode_with(code, limits).ok()?;
    let mut found = None;
    walk(&polish, &mut |e| {
        if let (None, Expr::Field(field) | Expr::FieldRef(field)) = (&found, e) {
            found = Some(field.occurrence.0.to_vec());
        }
    });
    found
}

fn parse_code(code: &[u8], context: &[u8]) -> Result<Expression, String> {
    parse_formula(code, context, FormulaUse::RecordOnly)
}

#[derive(Clone, Copy)]
enum FormulaUse {
    RecordOnly,
    /// At creation, a stored Number calculation can capture the record's
    /// position (auto-entry.md, "Get ( RecordNumber )").
    StoredNumber,
    /// An auto-entered or validation formula can read related fields and
    /// the operation's [`CONTEXT_SELECTORS`].
    Options,
    /// The host's recomputation can answer the Design functions.
    Recompute,
}

/// [`parse_code`] for a host's recomputation of a stored calculation, which
/// also answers the Design functions ([`unsupported_rule_operation`]).
fn parse_recomputed(code: &[u8], context: &[u8]) -> Result<Expression, String> {
    parse_formula(code, context, FormulaUse::Recompute)
}

/// Decode a formula with the field reads and selectors its caller can answer.
fn parse_formula(code: &[u8], context: &[u8], usage: FormulaUse) -> Result<Expression, String> {
    if code.len() > MAX_CODE {
        return Err("calculation is too large.".into());
    }
    // Refuse anything larger while decoding, before a hostile stream can
    // exhaust the worker thread's stack.
    let limits = om_formula::polish::Limits {
        depth: MAX_DEPTH,
        nodes: MAX_NODES,
    };
    let polish = om_formula::polish::decode_with(code, limits).map_err(|e| match e.kind {
        DecodeErrorKind::Malformed("nesting depth" | "too many nodes") => {
            "calculation complexity exceeds the supported limit.".to_owned()
        }
        DecodeErrorKind::Truncated => "truncated calculation.".to_owned(),
        DecodeErrorKind::TrailingBytes => "unsupported trailing calculation data.".to_owned(),
        DecodeErrorKind::InvalidOpcode(op) => format!("unsupported calculation opcode {op:#04x}."),
        _ => format!("invalid calculation: {e}."),
    })?;
    let related_reads = matches!(usage, FormulaUse::Options);
    let window_reads = related_reads
        || matches!(
            (usage, &polish),
            (FormulaUse::StoredNumber, Expr::Get(selector::RECORD_NUMBER))
        );
    let design = matches!(usage, FormulaUse::Recompute);
    let mut dependencies = BTreeSet::new();
    let mut related = BTreeSet::new();
    let mut whole = BTreeSet::new();
    let mut uses: BTreeMap<u64, Uses> = BTreeMap::new();
    // The related field references an aggregate is given.
    let mut aggregated: BTreeSet<*const Expr> = BTreeSet::new();
    walk(&polish, &mut |e| {
        if let Expr::Call { op, args } = e {
            if is_aggregate(*op) {
                aggregated.extend(
                    args.iter()
                        .filter(|a| matches!(a, Expr::FieldRef(_)))
                        .map(|a| a as *const Expr),
                );
            }
        }
    });
    let mut problem: Option<String> = None;
    walk(&polish, &mut |e| {
        if problem.is_some() {
            return;
        }
        match e {
            Expr::Field(field) | Expr::FieldRef(field) => {
                if field.occurrence.0 != context {
                    // A related field read as a value is the first related
                    // record's; passed as a field, every related record's.
                    let every = matches!(e, Expr::FieldRef(_));
                    match (
                        related_reads && field.repetition == 1,
                        field.occurrence.id(),
                        field.field.id(),
                    ) {
                        (true, Some(occurrence), Some(id)) => {
                            related.insert((occurrence, id));
                            let used = uses.entry(occurrence).or_default();
                            if every {
                                whole.insert(occurrence);
                                if aggregated.contains(&(e as *const Expr)) {
                                    used.aggregated.insert(id);
                                } else {
                                    used.placed = true;
                                }
                            } else {
                                used.first = true;
                            }
                        }
                        _ => {
                            problem =
                                Some("related-field calculation context is unsupported.".into())
                        }
                    }
                } else if field.repetition != 1 {
                    problem =
                        Some("repeating or indirect field references are unsupported.".into());
                } else {
                    match field.field.id() {
                        Some(id) => {
                            dependencies.insert(id);
                        }
                        None => problem = Some("unsupported field-reference encoding.".into()),
                    }
                }
            }
            // With nothing related, LookupNext copies the next lower or
            // higher match value from the host's index, which a write's
            // related records do not hold.
            Expr::Call { op, args }
                if *op == om_formula::Opcode::LOOKUP_NEXT
                    && args.iter().any(|a| reads_other_occurrence(a, context)) =>
            {
                problem = Some("LookupNext over related fields is unsupported.".into());
            }
            _ => {
                if let Some(reason) = unsupported_rule_operation(e, window_reads, design) {
                    problem = Some(reason.into());
                }
            }
        }
    });
    if let Some(problem) = problem {
        return Err(problem);
    }
    Ok(Expression {
        formula: Arc::new(Formula {
            context: Some(Key(context.to_vec())),
            formatted: None,
            polish,
        }),
        dependencies,
        related,
        other_globals: BTreeSet::new(),
        whole,
        uses,
        through_unstored: BTreeSet::new(),
        through_formulas: Vec::new(),
    })
}

struct RelatedWriteHop {
    match_fields: Vec<u64>,
    /// The equality and inequality keys of the measured self-join.
    grouped_keys: Option<(u64, u64)>,
}

/// The match fields of this table through which a formula of `table`,
/// evaluated from `context`, reads field `field` of `occurrence`, once the
/// rules can follow it: another table's stored value of one repetition,
/// through one relationship of `=` predicates each of two fields of one
/// type, text or number: this table's a stored field, not global, which the
/// write itself may compute (it is read again with that value, #1035), the
/// other table's a stored one, auto-entered or not (its stored value is
/// matched; a serial key, #187). A keep-existing auto-entry can also read
/// the first row through the grouped unequal self-join when the membership
/// check in [`RecordPlan::answers_own`] can settle it. Otherwise why not.
fn related_hop(
    schema: &CalcSchema,
    fields: &BTreeMap<u64, PlannedField>,
    context: u64,
    (occurrence, field): (u64, u64),
    first_auto_enter: bool,
) -> Result<RelatedWriteHop, &'static str> {
    use crate::host::FieldClass;
    use crate::meta::JoinOp;
    let to = schema
        .occurrence(occurrence)
        .ok_or("a related occurrence it reads is missing or of another file.")?;
    // A self-join too: the written record's own membership is decided where
    // its answer depends on it (`RecordPlan::answers_own`, #1035).
    let to_table = schema
        .table(to.table)
        .ok_or("a related table it reads is missing.")?;
    let stored = to_table.fields.get(&field).is_some_and(|d| {
        d.stored
            && !d.global
            && d.repetitions == 1
            && d.value_type != ValueType::Container
            && matches!(d.class, FieldClass::Data | FieldClass::Calculation)
    });
    if !stored {
        return Err(
            "calculations reading a related field that is not a stored value are unsupported.",
        );
    }
    let path = schema
        .path(context, occurrence)
        .ok_or("a related occurrence it reads is not related.")?;
    let [hop] = path.as_slice() else {
        return Err(
            "calculations over related fields through more than one relationship are unsupported.",
        );
    };
    let grouped_unequal = first_auto_enter
        && schema
            .occurrence(context)
            .is_some_and(|from| from.table == to.table)
        && grouped_unequal_self_join(hop);
    if hop.predicates.is_empty()
        || (!grouped_unequal && hop.predicates.iter().any(|p| p.op != JoinOp::Equal))
    {
        return Err(
            "calculations over related fields through other than = predicates are unsupported.",
        );
    }
    if !ordered_as_filemaker(schema, hop, to_table) {
        return Err(
            "calculations over related fields through a relationship sorted by a field \
             OpenMaker cannot order by (another table's, a summary or an unstored calculation, \
             or by a summary or a value list of field values) are unsupported.",
        );
    }
    let grouped_keys = if grouped_unequal {
        let group = hop
            .predicates
            .iter()
            .find(|p| p.op == JoinOp::Equal)
            .expect("grouped join has an equality key")
            .left;
        let key = hop
            .predicates
            .iter()
            .find(|p| p.op == JoinOp::NotEqual)
            .expect("grouped join has an inequality key")
            .left;
        if !fields.get(&group).is_some_and(PlannedField::given) {
            return Err("the group field of an unequal self-join must be written explicitly.");
        }
        Some((group, key))
    } else {
        None
    };
    let mut matches = Vec::new();
    for predicate in &hop.predicates {
        // Auto-entered and stored calculation keys participate in the
        // reader's dependency order. Reads demand their settled values.
        let ours = fields
            .get(&predicate.left)
            .filter(|f| {
                f.in_row()
                    && f.options.max_repetitions == 1
                    && (!f.stored_calculation() || f.expression.is_some())
            })
            .and_then(|f| match f.data_type {
                DataType::Text => Some(ValueType::Text),
                DataType::Number => Some(ValueType::Number),
                _ => None,
            });
        let theirs = to_table
            .fields
            .get(&predicate.right)
            .filter(|d| d.stored && !d.global && d.repetitions == 1)
            .map(|d| d.value_type);
        if ours.is_none() || ours != theirs {
            return Err(
                "calculations over related fields need match fields of one type, text or \
                 number, stored, not global on this table's side.",
            );
        }
        if grouped_unequal && ours != Some(ValueType::Text) {
            return Err("auto-entry through an unequal self-join needs stored text match fields.");
        }
        matches.push(predicate.left);
    }
    Ok(RelatedWriteHop {
        match_fields: matches,
        grouped_keys,
    })
}

fn single_line_key(value: &str) -> bool {
    value
        .split(['\r', '\n'])
        .filter(|line| !line.is_empty())
        .nth(1)
        .is_none()
}

/// Unsorted, same-field predicates on a group key and a distinct row key.
/// The caller establishes that both occurrences are of the same table.
fn grouped_unequal_self_join(hop: &crate::host::Hop) -> bool {
    use crate::meta::JoinOp::{Equal, NotEqual};
    let [a, b] = hop.predicates.as_slice() else {
        return false;
    };
    !hop.sorted()
        && matches!((a.op, b.op), (Equal, NotEqual) | (NotEqual, Equal))
        && a.left == a.right
        && b.left == b.right
        && a.left != b.left
}

/// The sort language whose text order OpenMaker's collation does not
/// reproduce: Japanese (calc-values.md, "Japanese": small kana, `ん` and
/// the voiced marks are not modelled).
pub(crate) const JAPANESE_SORT: u32 = 31;

/// The language a sort level orders text in, as `host::sort` resolves it:
/// the level's own (`entry`), else the field's, else the file's; 0 none.
pub(crate) fn sort_language(entry: u32, field: u32, file: u32) -> u32 {
    [entry, field, file]
        .into_iter()
        .find(|l| *l != 0)
        .unwrap_or(0)
}

/// Whether `host::sort` orders `hop`'s related records as FileMaker does,
/// so the first related record a lookup or a formula reads is FileMaker's:
/// unsorted, or sorted only by `to_table`'s own data fields and stored
/// calculations named through `hop.to`, each in its own or a custom value
/// list's order, text not in Japanese. `host::sort` sorts a summary or an
/// unstored calculation as empty, ignores "Reorder based on summary field"
/// and the order of a value list of field values, skips a field it cannot
/// find, and orders some Japanese text otherwise than FileMaker.
fn ordered_as_filemaker(
    schema: &CalcSchema,
    hop: &crate::host::Hop,
    to_table: &crate::host::TableDef,
) -> bool {
    use crate::host::FieldClass;
    let Some(sort) = &hop.sort else {
        return true;
    };
    let file_language = u32::from(schema.locale.language.id());
    sort.fields.iter().all(|entry| {
        entry.field.occurrence.is_none_or(|o| o == hop.to)
            && entry.summary.is_none()
            && entry.value_list.is_none_or(|id| {
                schema.shows_value_list(id)
                    && schema
                        .value_lists()
                        .iter()
                        .any(|l| l.id == id && matches!(l.source, ValueSource::Custom(_)))
            })
            && to_table.fields.get(&entry.field.field).is_some_and(|d| {
                d.class != FieldClass::Summary
                    && !d.is_unstored_calculation()
                    && (!matches!(d.value_type, ValueType::Text | ValueType::Container)
                        || sort_language(entry.locale, d.language, file_language) != JAPANESE_SORT)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::catalog::Field;
    use om_hbam::KeySegment;

    pub(super) const CONTEXT: &[u8] = &[3, 0xd0, 0, 2];
    const UUID: &str = "4C32B318-4200-4E07-90A2-4C2BB78F3250";

    fn reference(id: u8) -> Vec<u8> {
        let mut bytes = vec![0x16, 0, 4];
        bytes.extend(CONTEXT);
        bytes.extend([2, 1, id, 0, 1]);
        bytes
    }
    fn one() -> Vec<u8> {
        vec![0x10, 2, 0, 1, 0, 16, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
    }
    pub(super) fn initials(first: u8, last: u8) -> Vec<u8> {
        let mut bytes = vec![0x4c, 0, 2];
        for field in [last, first] {
            bytes.extend([0x1e, 0x4e]);
            bytes.extend(reference(field));
            bytes.push(0x57);
            bytes.extend(reference(field));
            bytes.extend(one());
        }
        bytes
    }
    pub(super) fn field(
        id: u64,
        kind: FieldKind,
        data_type: DataType,
        variable: u8,
        flags: u32,
        validation: u32,
    ) -> Field {
        let mut options = vec![0; 26];
        options[0] = if kind == FieldKind::Calculation { 2 } else { 1 };
        options[1] = match data_type {
            DataType::Text => 1,
            DataType::Number => 2,
            DataType::Date => 3,
            DataType::Time => 4,
            DataType::Timestamp => 5,
            DataType::Container => 6,
            DataType::Unknown(value) => value,
        };
        options[3] = variable;
        options[4..8].copy_from_slice(&21u32.to_be_bytes());
        options[8..12].copy_from_slice(&flags.to_be_bytes());
        options[12..16].copy_from_slice(&validation.to_be_bytes());
        options[24..26].copy_from_slice(&1u16.to_be_bytes());
        Field {
            id,
            name: format!("Field{id}"),
            kind,
            data_type,
            options,
        }
    }
    fn table() -> Table {
        Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(3, FieldKind::Normal, DataType::Container, 0, 0, 0),
                field(8, FieldKind::Calculation, DataType::Text, 0, 0, 0),
                field(9, FieldKind::Normal, DataType::Text, 0, 0x8000_0188, 0x1c),
                field(10, FieldKind::Normal, DataType::Text, 4, 0x101, 0xc),
                field(11, FieldKind::Normal, DataType::Text, 9, 0x101, 0xc),
                field(12, FieldKind::Normal, DataType::Timestamp, 2, 0x101, 0x200c),
                field(13, FieldKind::Normal, DataType::Timestamp, 7, 0x101, 0x200c),
            ],
        }
    }
    fn plan(table: &Table, code: &[u8]) -> Result<RecordPlan, DataError> {
        let mut tree = KeyTree::default();
        let prefix = [KeySegment(&[0x80, 2]), KeySegment(&[3]), KeySegment(&[5])];
        for (id, code) in [(&[8][..], code), (&[9][..], &[0x9b, 0x65][..])] {
            let mut path = prefix.to_vec();
            path.extend([KeySegment(id), KeySegment(&[5]), KeySegment(&[4])]);
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(CONTEXT))
                .unwrap();
            *path.last_mut().unwrap() = KeySegment(&[6]);
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(code))
                .unwrap();
        }
        RecordPlan::from_schema(table, &tree)
    }
    fn environment() -> RecordEnvironment {
        RecordEnvironment {
            account_name: "session-account".into(),
            user_name: "display user".into(),
            date: "10/28/2026".into(),
            time: "1:20:29 PM".into(),
            timestamp: "10/28/2026 1:20:29 PM".into(),
            uuid: UUID.into(),
            containers: None,
            interrupted: None,
            limits: CalcLimits::DEFAULT,
            lookups: Lookups::default(),
            serials: BTreeMap::new(),
            globals: None,
            context: WriteContext::default(),
            held: None,
            others_held: BTreeSet::new(),
            related_needs: None,
            sources: None,
        }
    }
    fn decoded(prepared: &PreparedRecord, id: u64) -> Option<String> {
        prepared
            .values
            .get(&id)
            .and_then(|v| v.as_ref())
            .map(|v| om_scsu::decode_scsu(v).unwrap())
    }
    fn existing() -> BTreeMap<u64, String> {
        BTreeMap::from([
            (1, "Hello".into()),
            (2, "World".into()),
            (8, "W".into()),
            (9, UUID.into()),
            (10, "original-account".into()),
            (11, "previous-account".into()),
            (12, "10/27/2026 12:00:00 PM".into()),
            (13, "10/27/2026 12:00:00 PM".into()),
        ])
    }

    #[test]
    fn creation_computes_uuid_audit_and_stored_initial_from_schema() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        assert_eq!(
            plan.editable_fields(),
            BTreeSet::from(["Field1".into(), "Field2".into()])
        );
        let prepared = plan
            .prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::from([(1, Some("Hello".into())), (2, Some("World".into()))]),
                &environment(),
            )
            .unwrap();
        assert_eq!(decoded(&prepared, 8).as_deref(), Some("W"));
        assert_eq!(decoded(&prepared, 9).as_deref(), Some(UUID));
        assert_eq!(decoded(&prepared, 10).as_deref(), Some("session-account"));
        assert_eq!(decoded(&prepared, 11).as_deref(), Some("session-account"));
        assert_eq!(
            decoded(&prepared, 12).as_deref(),
            Some("10/28/2026 1:20:29 PM")
        );
        assert_eq!(decoded(&prepared, 13), decoded(&prepared, 12));
        assert_eq!(prepared.changed_fields, vec![1, 2, 8, 9, 10, 11, 12, 13]);
    }

    #[test]
    fn creation_time_in_text_keeps_the_local_twelve_hour_clock() {
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 1, 1, 0),
            ],
        };
        let tree = KeyTree::default();
        let system = Locale::en_us();
        let mut file = CalcSchema::new("Example", system.clone());
        let plan = RecordPlan::plan(&table, &tree, Some(&file), true, true, Some(&system)).unwrap();
        let mut clock = environment();
        clock.time = "1:26:37 AM".into();
        let created = plan
            .prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &clock,
            )
            .unwrap();
        let created_text = decoded(&created, 2).unwrap();
        assert_eq!(created_text, "1:26:37 AM");

        clock.time = "1:26:43 AM".into();
        let updated = plan
            .prepare(
                WriteMode::Update,
                &BTreeMap::from([(2, created_text)]),
                &BTreeMap::from([(1, Some("Changed".into()))]),
                &clock,
            )
            .unwrap();
        assert_eq!(updated.changed_fields, [1]);

        assert!(RecordPlan::from_file_schema(&table, &tree, &file).is_err());
        assert!(RecordPlan::from_desktop_file_schema(&table, &tree, &file, false).is_err());
        let german = Locale::de_de();
        assert!(RecordPlan::plan(&table, &tree, Some(&file), true, true, Some(&german)).is_err());
        assert!(RecordPlan::plan(&table, &tree, Some(&file), true, true, None).is_err());
        file.locale.twelve_hour = false;
        assert!(RecordPlan::plan(&table, &tree, Some(&file), true, true, Some(&system)).is_err());
    }

    /// A record the window holds unsaved (#1063): its values are known only
    /// where its commit stores what its rules give now, not with a UUID or
    /// clock its commit draws again, nor ever with a `Random` draw.
    #[test]
    fn a_pending_row_is_known_only_where_its_commit_stores_the_same() {
        let changes = BTreeMap::from([(1, Some("Hello".into())), (2, Some("World".into()))]);
        let known = |_: u64, _: Option<u64>, _: u64| false;
        let pending = |plan: &RecordPlan, kept| {
            let entry = (Entry::default(), kept, &known as UnknownValue<'_>);
            plan.pending_row(
                WriteMode::Create,
                &BTreeMap::new(),
                &changes,
                &environment(),
                entry,
            )
            .unwrap()
        };
        let initials = plan(&table(), &initials(1, 2)).unwrap();
        let (row, unknown) = pending(&initials, (false, false));
        assert_eq!(unknown, BTreeSet::from([9, 12, 13]));
        assert_eq!(row.get(&8).map(String::as_str), Some("W"));
        let (row, unknown) = pending(&initials, (true, true));
        assert_eq!(unknown, BTreeSet::new());
        assert_eq!(row.get(&9).map(String::as_str), Some(UUID));
        let random = om_formula::polish::encode(&Expr::Call {
            op: om_formula::Opcode(0x31),
            args: Vec::new(),
        });
        let random = plan(&table(), &random).unwrap();
        let (_, unknown) = pending(&random, (true, true));
        assert_eq!(unknown, BTreeSet::from([8]));
    }

    /// A write over values another file's commit may store otherwise
    /// (`prepare_entry_over`, #1006) is refused where it changes a field
    /// with furigana, as every other write is (#1302): Code auto-enters
    /// `S::seen`, which that commit may store otherwise, and the edit of
    /// Name leaves Code unevaluated, so the write is computed the direct
    /// way.
    #[test]
    fn a_write_over_unknown_values_refuses_changing_a_field_with_furigana() {
        use crate::host::RelationshipDef;
        use crate::meta::{JoinOp, JoinPredicate};
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0006, "T", 0x8006);
        names.add_occurrence(0xd0_0007, "S", 0x8006);
        for (id, name) in [(1, "k"), (2, "Name"), (3, "Code"), (9, "seen")] {
            names.add_field(0x8006, id, name);
        }
        let mut file = CalcSchema::new("Self", Locale::en_us());
        file.add_occurrence(0xd0_0006, "T", 0x8006);
        file.add_occurrence(0xd0_0007, "S", 0x8006);
        let by_k = vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 1,
        }];
        file.add_relationship(RelationshipDef::new(4, 0xd0_0006, 0xd0_0007, by_k));
        let table = Table {
            id: 0x8006,
            name: "T".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, FURIGANA, 0),
                // Auto-enters `S::seen` (0x8): evaluated as a record is
                // made, not as Name changes.
                field(3, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
                field(9, FieldKind::Normal, DataType::Text, 0, 0, 0),
            ],
        };
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0006);
        let expr = om_formula::compile("S::seen", &cx).unwrap().expr;
        let entries: Vec<(Vec<Vec<u8>>, Vec<u8>)> = [
            (4, om_formula::Key::from_id(0xd0_0006).0),
            (6, om_formula::polish::encode(&expr)),
        ]
        .into_iter()
        .map(|(key, value)| {
            let path = [0x8006, 3, 5, 3, 5, key]
                .iter()
                .map(|k| om_hbam::slot::uint_segment(*k))
                .collect();
            (path, value)
        })
        .collect();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let plan = RecordPlan::from_file_schema(&table, &tree, &file).unwrap();
        let existing = BTreeMap::from([
            (1, "a".to_owned()),
            (2, "Ship".to_owned()),
            (3, String::new()),
        ]);
        let changes = BTreeMap::from([(2, Some("やまだ".to_owned()))]);
        let marked = |t: u64, _: Option<u64>, f: u64| (t, f) == (0x8006, 9);
        let refused = plan
            .prepare_entry_over(
                (WriteMode::Update, &existing, &changes),
                &environment(),
                Entry::default(),
                &marked,
            )
            .err()
            .expect("refused");
        assert_eq!(refused.code, Some(error::UNAVAILABLE), "{refused:?}");
        assert!(refused.message.contains("#1302"), "{refused:?}");
    }

    /// A field computed when read is unknown only through what it reads
    /// (#1063): a row's unstored `Shown` over its unstored `Label`, which
    /// reads the row's `Note` and its job's `Title`, as a related list's
    /// field may read its parent. Unknown timestamps of the job or the rows
    /// leave it known (not when any value of its table is unknown), the
    /// job's unknown `Title` does not; the row's own unknown `Note` only on
    /// that row.
    #[test]
    fn a_field_computed_when_read_is_unknown_only_through_what_it_reads() {
        use crate::host::{FieldDef, RelationshipDef};
        use crate::meta::{JoinOp, JoinPredicate};
        let (rows, jobs, tags) = (0x8001, 0x8002, 0x8003);
        let mut schema = CalcSchema::new("f", om_calc::Locale::en_us());
        schema.add_table(rows, "Rows");
        schema.add_table(jobs, "Jobs");
        schema.add_table(tags, "Tags");
        for (table, field, name) in [
            (rows, 1, "Key"),
            (rows, 2, "Note"),
            (rows, 3, "Stamp"),
            (jobs, 1, "Key"),
            (jobs, 2, "Title"),
            (jobs, 3, "Modified"),
            (tags, 1, "Key"),
        ] {
            schema.add_field(table, FieldDef::data(field, name, ValueType::Text));
        }
        schema.add_occurrence(7, "Rows", rows);
        schema.add_occurrence(8, "Jobs", jobs);
        schema.add_occurrence(9, "Tags", tags);
        let equal = |left, right| {
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left,
                right,
            }]
        };
        schema.add_relationship(RelationshipDef::new(1, 7, 8, equal(1, 1)));
        // Rows find Tags by a key computed from the job's Title.
        schema.add_relationship(RelationshipDef::new(2, 7, 9, equal(8, 1)));
        for (id, name, text) in [
            (4, "Label", "Note & Jobs::Title"),
            (5, "Shown", "Label & \"!\""),
            (6, "Late", "Jobs::Modified"),
            (7, "Drawn", "Random"),
            (8, "Code", "Jobs::Title"),
        ] {
            let mut ctx = om_formula::Context::new(schema.names());
            ctx.occurrence = Some(7);
            let formula = om_formula::Formula {
                context: Some(om_formula::Key::from_id(7)),
                formatted: None,
                polish: om_formula::compile(text, &ctx).unwrap().expr,
            };
            let field = FieldDef::calculation(id, name, ValueType::Text, formula, false);
            schema.add_field(rows, field);
        }
        let check = |marked: &[(u64, u64, u64)], (record, field): (u64, u64)| {
            let unknown = |t, r: Option<u64>, f| {
                marked
                    .iter()
                    .any(|&(mt, mr, mf)| (mt, mf) == (t, f) && r.is_none_or(|r| r == mr))
            };
            let any = !marked.is_empty();
            unknown_when_read(&schema, (&unknown, any), (rows, Some(record), field))
        };
        // The job's modification timestamp: only what reads it.
        let stamp = [(jobs, 1, 3)];
        assert!(!check(&stamp, (5, 5)), "Shown");
        assert!(check(&stamp, (5, 6)), "Late");
        // The rows' own timestamps, which nothing computed reads.
        assert!(!check(&[(rows, 5, 3), (rows, 6, 3)], (5, 5)));
        // The job's Title: Label and Shown, on every row.
        let title = [(jobs, 1, 2)];
        assert!(check(&title, (5, 5)) && check(&title, (6, 5)));
        // Row 5's Note: its own Shown, not row 6's.
        let note = [(rows, 5, 2)];
        assert!(check(&note, (5, 5)));
        assert!(!check(&note, (6, 5)));
        // Each read answers another number.
        assert!(check(&[], (5, 7)));
        // Which Tags a row finds: by its Code, computed from its job's
        // Title, and by the Tags' Key.
        let path = schema.path(7, 9).unwrap();
        let finds = |marked: &[(u64, u64, u64)]| {
            let unknown = |t, r: Option<u64>, f| {
                marked
                    .iter()
                    .any(|&(mt, mr, mf)| (mt, mf) == (t, f) && r.is_none_or(|r| r == mr))
            };
            path_reads_unknown(&schema, (&unknown, true), (rows, Some(5)), &path)
        };
        assert!(finds(&title), "the job's Title");
        assert!(!finds(&stamp), "the job's timestamp");
        assert!(finds(&[(tags, 3, 1)]), "a Tag's Key");
        assert!(!finds(&[(rows, 6, 2)]), "another row's Note");
    }

    /// An edit drawing `Random` again for an auto-entered A leaves A, and B
    /// computed from it, unknown, even when A comes out as stored now (B is
    /// then not evaluated, but its commit can draw another A).
    #[test]
    fn a_pending_edit_drawing_again_leaves_what_is_computed_from_it_unknown() {
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0002, "Example", 0x8002);
        for id in [1, 2] {
            names.add_field(0x8002, id, &format!("Field{id}"));
        }
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0002);
        let code =
            |text: &str| om_formula::polish::encode(&om_formula::compile(text, &cx).unwrap().expr);
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Number, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Number, 0, 0xa8, 0),
                field(8, FieldKind::Calculation, DataType::Number, 0, 0, 0),
            ],
        };
        let mut tree = KeyTree::default();
        let formulas = [
            // `Random * 0`: a draw that comes out as stored now, every time.
            (
                2u8,
                code("If ( Field1 > 0 ; Round ( Random * 0 ; 0 ) ; 0 )"),
            ),
            (8, code("Field2 * 2")),
        ];
        for (id, code) in &formulas {
            let mut path = vec![
                KeySegment(&[0x80, 2]),
                KeySegment(&[3]),
                KeySegment(&[5]),
                KeySegment(std::slice::from_ref(id)),
                KeySegment(&[5]),
                KeySegment(&[4]),
            ];
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(CONTEXT))
                .unwrap();
            *path.last_mut().unwrap() = KeySegment(&[6]);
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(code))
                .unwrap();
        }
        let plan = RecordPlan::from_schema(&table, &tree).unwrap();
        let existing = BTreeMap::from([
            (1, "1".to_owned()),
            (2, "0".to_owned()),
            (8, "0".to_owned()),
        ]);
        let changes = BTreeMap::from([(1, Some("2".to_owned()))]);
        let known = |_: u64, _: Option<u64>, _: u64| false;
        let entry = (Entry::default(), (true, true), &known as UnknownValue<'_>);
        let (_, unknown) = plan
            .pending_row(
                WriteMode::Update,
                &existing,
                &changes,
                &environment(),
                entry,
            )
            .unwrap();
        assert!(unknown.contains(&2) && unknown.contains(&8), "{unknown:?}");
    }

    /// A creation timestamp an edit keeps is the record's stored value, not
    /// one its commit draws again: a pending edit recomputing a calculation
    /// over it knows it; created now, both are unknown.
    #[test]
    fn a_pending_edit_keeps_a_creation_value_it_does_not_evaluate() {
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0002, "Example", 0x8002);
        for id in [1, 12] {
            names.add_field(0x8002, id, &format!("Field{id}"));
        }
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0002);
        let formula = om_formula::compile("Field1 & \"|\" & Field12", &cx).unwrap();
        let code = om_formula::polish::encode(&formula.expr);
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(8, FieldKind::Calculation, DataType::Text, 0, 0, 0),
                field(12, FieldKind::Normal, DataType::Timestamp, 2, 0x101, 0x200c),
            ],
        };
        let plan = plan(&table, &code).unwrap();
        let created = "10/27/2026 12:00:00 PM";
        let existing = BTreeMap::from([
            (1, "Hello".to_owned()),
            (8, format!("Hello|{created}")),
            (12, created.to_owned()),
        ]);
        let changes = BTreeMap::from([(1, Some("Bye".to_owned()))]);
        let known = |_: u64, _: Option<u64>, _: u64| false;
        let pending = |mode, existing: &BTreeMap<u64, String>| {
            let entry = (Entry::default(), (false, false), &known as UnknownValue<'_>);
            plan.pending_row(mode, existing, &changes, &environment(), entry)
                .unwrap()
        };
        let (row, unknown) = pending(WriteMode::Update, &existing);
        assert_eq!(unknown, BTreeSet::new());
        assert!(row[&8].starts_with("Bye|"), "{row:?}");
        assert_eq!(row[&12], created);
        let (_, unknown) = pending(WriteMode::Create, &BTreeMap::new());
        assert_eq!(unknown, BTreeSet::from([8, 12]));
    }

    #[test]
    fn blank_creation_still_generates_required_values() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        let prepared = plan
            .prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &environment(),
            )
            .unwrap();
        assert_eq!(decoded(&prepared, 9).as_deref(), Some(UUID));
        assert_eq!(prepared.changed_fields, vec![9, 10, 11, 12, 13]);
    }

    #[test]
    fn updates_refresh_modification_values_preserve_creation_and_follow_dependencies() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        let mut env = environment();
        env.uuid = "C82C1E88-8085-4A2A-A698-72A11F356445".into();
        let prepared = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(2, Some("Nash".into()))]),
                &env,
            )
            .unwrap();
        assert_eq!(prepared.changed_fields, vec![2, 8, 11, 13]);
        assert_eq!(decoded(&prepared, 8).as_deref(), Some("N"));
        assert!(!prepared.values.contains_key(&9));
        assert!(!prepared.values.contains_key(&10));
        assert!(!prepared.values.contains_key(&12));
        let first_only = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(1, Some("Zed".into()))]),
                &env,
            )
            .unwrap();
        assert_eq!(first_only.changed_fields, vec![1, 11, 13]);
    }

    #[test]
    fn clearing_recalculates_fallback_with_unicode_and_never_clears_other_values() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        let prepared = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(1, Some("Åse".into())), (2, None)]),
                &environment(),
            )
            .unwrap();
        assert_eq!(decoded(&prepared, 8).as_deref(), Some("Å"));
        assert_eq!(prepared.values.get(&2), Some(&None));
        assert!(!prepared.values.contains_key(&3));
    }

    #[test]
    fn noop_update_does_not_touch_audit_values() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        let prepared = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(1, Some("Hello".into()))]),
                &environment(),
            )
            .unwrap();
        assert!(prepared.values.is_empty());
    }

    #[test]
    fn protected_fields_required_audit_and_generated_uuids_are_checked() {
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        let err = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(9, Some("changed".into()))]),
                &environment(),
            )
            .err()
            .unwrap();
        assert!(err.to_string().contains("cannot be edited"));
        let mut env = environment();
        env.account_name.clear();
        assert!(plan
            .prepare(WriteMode::Create, &BTreeMap::new(), &BTreeMap::new(), &env,)
            .is_err());
        let mut env = environment();
        env.uuid = "not a UUID".into();
        assert!(plan
            .prepare(WriteMode::Create, &BTreeMap::new(), &BTreeMap::new(), &env)
            .err()
            .unwrap()
            .to_string()
            .contains("valid generated UUID"));
    }

    #[test]
    fn unique_fields_need_a_value_index_keyed_in_a_form_openmaker_builds() {
        let mut table = table();
        let primary_key = 4;
        assert_eq!(table.fields[primary_key].id, 9);
        table.fields[primary_key].options[8..12].copy_from_slice(&0x188u32.to_be_bytes());
        assert!(plan(&table, &initials(1, 2))
            .err()
            .unwrap()
            .to_string()
            .contains("require a value index"));
        table.fields[primary_key].options[8..12].copy_from_slice(&0x8000_0188u32.to_be_bytes());
        // German (25) and Swedish (41) are tailorings OpenMaker does not key.
        for language in [25u32, 41] {
            table.fields[primary_key].options[4..8].copy_from_slice(&language.to_be_bytes());
            assert!(plan(&table, &initials(1, 2)).is_err(), "{language}");
        }
        // English international, System and Unicode standard; French,
        // German dictionary, Russian and an Indic language, which FileMaker
        // leaves untailored (their value keys are English's); and language
        // 0, the file's own (English here).
        for language in [77u32, 1, 3, 24, 26, 36, 59, 0] {
            table.fields[primary_key].options[4..8].copy_from_slice(&language.to_be_bytes());
            assert!(plan(&table, &initials(1, 2)).is_ok(), "{language}");
        }
        // FileMaker Pro 26's own blank file indexes PrimaryKey so (#725).
        table.fields[primary_key].options[4..8].copy_from_slice(&2u32.to_be_bytes());
        assert!(plan(&table, &initials(1, 2)).is_ok());
        // Language 0 needs the file's language, which a locale record that
        // does not decode leaves unknown: refused, not keyed in English.
        let mut file = CalcSchema::new("", Locale::en_us());
        assert_eq!(file.file_language(), Some(21));
        file.locale_unreadable = true;
        assert_eq!(file.file_language(), None);
        assert_eq!(value_text_keys(0, file.file_language()), None);
        assert_eq!(
            value_text_keys(24, None),
            Some(om_query::keys::TextKeys::Collated)
        );
    }

    #[test]
    fn prefix_parser_rejects_truncation_unknown_opcodes_related_context_and_cycles() {
        let code = initials(6, 7);
        let expr = parse_code(&code, CONTEXT).unwrap();
        let row = BTreeMap::from([(6, "Alpha".into()), (7, "Beta".into())]);
        assert_eq!(expr.evaluate(&row, &environment()).unwrap(), "B");
        for end in 0..code.len() {
            assert!(
                parse_code(&code[..end], CONTEXT).is_err(),
                "accepted truncated code at {end}"
            );
        }
        assert!(parse_code(&[0xff], CONTEXT).is_err());
        assert!(parse_code(&code, &[3, 0xd0, 0, 3]).is_err());
        assert!(plan(&table(), &reference(8))
            .err()
            .unwrap()
            .to_string()
            .contains("Cyclic"));
        let mut trailing = code;
        trailing.push(0);
        assert!(parse_code(&trailing, CONTEXT).is_err());
    }

    #[test]
    fn deep_calculations_are_refused_while_decoding() {
        // 1,500 nested Not overflowed a worker thread's 2 MB stack in a
        // debug build when the whole tree was built before the complexity
        // check.
        let mut code = vec![0x1e; 1500];
        code.extend([0x9b, 2]);
        let error = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || parse_code(&code, CONTEXT))
            .unwrap()
            .join()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("complexity"), "{error}");
    }

    #[test]
    fn literals_must_end_on_their_declared_unit_count() {
        let mut literal = vec![0x13, 1];
        literal.extend(om_scsu::encode_scsu("😀"));
        literal.push(0);
        assert!(parse_code(&literal, CONTEXT).is_err());
    }

    #[test]
    fn unknown_options_and_validation_modes_never_grant_edit_permission() {
        let mut table = table();
        table.fields[0].options[8..12].copy_from_slice(&0x8000_0020u32.to_be_bytes());
        assert!(plan(&table, &initials(1, 2)).is_err());
        // `kNoEquiFindSupport` (schema-path.md 5.5) is not a rule the
        // planner knows. "Prevent index" is an index bit (#994; above).
        table.fields[0].options[8..12].copy_from_slice(&0x0200_0000u32.to_be_bytes());
        assert!(plan(&table, &initials(1, 2)).is_err());
        table.fields[0].options[8..12].copy_from_slice(&0u32.to_be_bytes());
        // A validation bit the rules do not know.
        table.fields[0].options[12..16].copy_from_slice(&0x1_0000u32.to_be_bytes());
        assert!(plan(&table, &initials(1, 2)).is_err());
    }

    #[test]
    fn observed_plain_indexed_text_remains_editable() {
        let mut table = table();
        // Automatic indexing off, alone and with a value index (#994).
        for index in [
            0x2000_0000u32,
            0x8000_0000,
            0xa000_0000,
            0x4000_0000,
            0xc000_0000,
            INDEX_OPTIONS,
        ] {
            table.fields[0].options[8..12].copy_from_slice(&index.to_be_bytes());
            let plan = plan(&table, &initials(1, 2)).unwrap();
            assert!(plan.editable_fields().contains("Field1"));
        }
    }

    #[test]
    fn built_indexes_preserve_auto_entry_protection_and_calculation_scheduling() {
        let mut table = table();
        // Automatic indexing off on the auto-entered calculation and the
        // modifier and modification values, as a solution FileMaker Server
        // hosts has it on such fields (`0xc0000008`, `0x40000001`; #994).
        for (id, index) in [
            (1, 0xa000_0000u32),
            (2, 0x8000_0000),
            (9, 0xe000_0000),
            (10, 0x2000_0000),
            (11, 0x4000_0000),
            (12, 0x8000_0000),
            (13, 0x4000_0000),
        ] {
            let field = table
                .fields
                .iter_mut()
                .find(|field| field.id == id)
                .unwrap();
            let original = u32::from_be_bytes(field.options[8..12].try_into().unwrap());
            field.options[8..12].copy_from_slice(&(original | index).to_be_bytes());
        }
        let plan = plan(&table, &initials(1, 2)).unwrap();
        assert_eq!(
            plan.editable_fields(),
            BTreeSet::from(["Field1".into(), "Field2".into()])
        );
        let prepared = plan
            .prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(2, Some("Nash".into()))]),
                &environment(),
            )
            .unwrap();
        assert_eq!(prepared.changed_fields, vec![2, 8, 11, 13]);
        assert_eq!(decoded(&prepared, 8).as_deref(), Some("N"));
        assert!(!prepared.values.contains_key(&9));
        assert!(!prepared.values.contains_key(&10));
        assert!(!prepared.values.contains_key(&12));
    }

    #[test]
    fn a_pending_container_transfer_leaves_other_local_cells_writable() {
        let tree = KeyTree::default();
        let file = CalcSchema::new("Items", Locale::en_us());
        let env = environment();
        for (options, repetitions, cell) in [(0x6000, 1u16, 2), (0x4000, 24, 2 * 256 + 2)] {
            let mut container = field(2, FieldKind::Normal, DataType::Container, 0, options, 0);
            container.options[24..26].copy_from_slice(&repetitions.to_be_bytes());
            let table = Table {
                id: 0x8002,
                name: "Items".into(),
                fields: vec![
                    field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                    container,
                ],
            };
            assert!(RecordPlan::from_desktop_file_schema(&table, &tree, &file, false).is_err());
            let plan = RecordPlan::from_desktop_file_schema(&table, &tree, &file, true).unwrap();
            let existing =
                BTreeMap::from([(1, "Before".into()), (cell, "Existing reference".into())]);
            let changes = BTreeMap::from([(1, Some("Example".into()))]);
            for (mode, row) in [
                (WriteMode::Create, &BTreeMap::new()),
                (WriteMode::Update, &existing),
            ] {
                let prepared = plan.prepare(mode, row, &changes, &env).unwrap();
                assert_eq!(prepared.changed_fields, [1]);
                assert_eq!(decoded(&prepared, 1).as_deref(), Some("Example"));
                assert!(!prepared.values.contains_key(&cell));
            }
            let refused = plan.compute(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(cell, None)]),
                &env,
                Entry::script(false, false),
            );
            assert_eq!(refused.err().and_then(|e| e.code), Some(error::UNAVAILABLE));
        }
    }

    #[test]
    fn container_dependencies_plan_and_oversized_cells_fail_before_transport() {
        // A calculation reading a container plans: a write's container
        // reader gives it the data (script oracle s34-containers-calc).
        let reading = plan(&table(), &initials(3, 2)).unwrap();
        assert!(reading.reads_containers());
        let plan = plan(&table(), &initials(1, 2)).unwrap();
        assert!(!plan.reads_containers());
        let result = plan.prepare(
            WriteMode::Create,
            &BTreeMap::new(),
            &BTreeMap::from([(1, Some("a".repeat(65_536)))]),
            &environment(),
        );
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("single-cell size"));
    }

    #[test]
    fn repeated_filenames_follow_only_the_changed_container_repetition() {
        struct FileNames;
        impl ContainerReader for FileNames {
            fn value(&self, _: u64, text: &str) -> Option<om_calc::Container> {
                Some(om_calc::Container::file(text, vec![1]))
            }

            fn data(&self, _: u64, _: &[u8]) -> Option<om_calc::eval::StoredContainer> {
                None
            }
        }

        let mut table = table();
        table.fields.retain(|field| field.id <= 2);
        let mut attachment = field(3, FieldKind::Normal, DataType::Container, 0, 0, 0);
        attachment.options[24..26].copy_from_slice(&24u16.to_be_bytes());
        let mut filename = field(4, FieldKind::Normal, DataType::Text, 0, 0x2000_0088, 0);
        filename.options[24..26].copy_from_slice(&24u16.to_be_bytes());
        table.fields.extend([attachment, filename]);
        let formulas = [(4, 5, "GetContainerAttribute ( Field3 ; \"Filename\" )")];
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        assert!(plan_with(&table, &formulas, &[], Vec::new(), false).is_err());
        assert!(plan_of(
            &table,
            &[(4, 5, "GetContainerAttribute ( Field3 ; \"Width\" )")],
            &[]
        )
        .is_err());
        let mut env = environment();
        env.containers = Some(Arc::new(FileNames));
        let prepare = |mode, row: &BTreeMap<_, _>, changes: &BTreeMap<_, _>| {
            plan.prepare(mode, row, changes, &env).unwrap()
        };
        let title = BTreeMap::from([(1, Some("One".into()))]);
        let new = prepare(WriteMode::Create, &BTreeMap::new(), &title);
        assert_eq!(new.changed_fields, [1]);

        let second_attachment = cell_of(3, 2).unwrap();
        let second_filename = cell_of(4, 2).unwrap();
        let populated = BTreeMap::from([(3, Some("red.png".into()))]);
        let refused = plan.prepare(WriteMode::Create, &BTreeMap::new(), &populated, &env);
        assert_eq!(refused.err().and_then(|e| e.code), Some(error::UNAVAILABLE));
        let unrelated = BTreeMap::from([(3, "red.png".into()), (4, "red.png".into())]);
        let inserted = prepare(
            WriteMode::Update,
            &unrelated,
            &BTreeMap::from([(second_attachment, Some("green.png".into()))]),
        );
        assert_eq!(
            inserted.changed_fields,
            [second_attachment, second_filename]
        );
        assert_eq!(
            decoded(&inserted, second_filename).as_deref(),
            Some("green.png")
        );

        let mut two = unrelated;
        two.extend([
            (second_attachment, "green.png".into()),
            (second_filename, "green.png".into()),
        ]);
        let replaced = prepare(
            WriteMode::Update,
            &two,
            &BTreeMap::from([(second_attachment, Some("blue.png".into()))]),
        );
        assert_eq!(
            replaced.changed_fields,
            [second_attachment, second_filename]
        );
        assert_eq!(
            decoded(&replaced, second_filename).as_deref(),
            Some("blue.png")
        );
        let cleared = prepare(
            WriteMode::Update,
            &two,
            &BTreeMap::from([(second_attachment, None)]),
        );
        assert_eq!(cleared.changed_fields, [second_attachment, second_filename]);
        assert_eq!(cleared.values.get(&second_filename), Some(&None));
        assert_eq!(prepare(WriteMode::Update, &two, &title).changed_fields, [1]);
    }

    #[test]
    #[ignore = "requires the user-owned local Contacts template; read-only"]
    fn actual_template_preflights_and_computes_stored_initial() {
        let path = std::env::var_os("OPENMAKER_CONTACTS_TEMPLATE")
            .expect("set OPENMAKER_CONTACTS_TEMPLATE");
        let file = om_hbam::HbamFile::open(path).unwrap();
        let tree = file.key_tree().unwrap();
        let catalog = om_hbam::Catalog::from_tree(&tree).unwrap();
        let table = catalog.table(0x8002).unwrap();
        let plan = RecordPlan::from_schema(table, &tree).unwrap();
        let prepared = plan
            .prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::from([(1, Some("Alpha".into())), (2, Some("Beta".into()))]),
                &environment(),
            )
            .unwrap();
        assert_eq!(decoded(&prepared, 8).as_deref(), Some("B"));
        assert_eq!(decoded(&prepared, 9).as_deref(), Some(UUID));
        let existing = prepared
            .values
            .iter()
            .filter_map(|(id, value)| Some((*id, om_scsu::decode_scsu(value.as_ref()?).unwrap())))
            .collect();
        let mut environment = environment();
        environment.account_name = "second-account".into();
        environment.timestamp = "10/28/2026 2:00:00 PM".into();
        let updated = plan
            .prepare(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(2, Some("Gamma".into()))]),
                &environment,
            )
            .unwrap();
        assert_eq!(updated.changed_fields, vec![2, 8, 11, 13]);
        assert_eq!(decoded(&updated, 8).as_deref(), Some("G"));
        assert_eq!(decoded(&updated, 11).as_deref(), Some("second-account"));
        assert!(!updated.values.contains_key(&9));
        assert!(!updated.values.contains_key(&10));
        assert!(!updated.values.contains_key(&12));
    }

    /// Compile `source` in the test table's occurrence to its Polish bytes.
    fn polish(table: &Table, source: &str) -> Vec<u8> {
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0002, "Example", table.id);
        for f in &table.fields {
            names.add_field(table.id, f.id, &f.name);
        }
        let mut ctx = om_formula::Context::new(&names);
        ctx.occurrence = Some(0xd0_0002);
        ctx.support_self = true;
        let expr = om_formula::compile(source, &ctx)
            .unwrap_or_else(|e| panic!("{source}: {e:?}"))
            .expr;
        om_formula::polish::encode(&expr)
    }

    /// A plan whose fields have `formulas` (field, attribute key, source)
    /// and `leaves` under their definitions (field, key, bytes).
    fn plan_of(
        table: &Table,
        formulas: &[(u64, u64, &str)],
        leaves: &[(u64, u64, Vec<u8>)],
    ) -> Result<RecordPlan, DataError> {
        plan_with(table, formulas, leaves, Vec::new(), true)
    }

    /// [`plan_of`] with other keys of the file (value lists).
    fn plan_with(
        table: &Table,
        formulas: &[(u64, u64, &str)],
        leaves: &[(u64, u64, Vec<u8>)],
        mut entries: Vec<(Vec<Vec<u8>>, Vec<u8>)>,
        local: bool,
    ) -> Result<RecordPlan, DataError> {
        let path = |field: u64, rest: &[u64]| -> Vec<Vec<u8>> {
            [table.id, 3, 5, field]
                .iter()
                .chain(rest)
                .map(|k| om_hbam::slot::uint_segment(*k))
                .collect()
        };
        for (field, key, source) in formulas {
            entries.push((path(*field, &[*key, 4]), CONTEXT.to_vec()));
            entries.push((path(*field, &[*key, 6]), polish(table, source)));
        }
        for (field, key, bytes) in leaves {
            entries.push((path(*field, &[*key]), bytes.clone()));
        }
        entries.sort();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        RecordPlan::plan(table, &tree, None, false, local, None)
    }

    #[test]
    fn creation_date_is_used_with_a_calculated_auto_entry_value() {
        // The closed FileMaker Pro copies held both option words with
        // selector 0 and a saved formula.
        for options in [0x0000_0009, 0x0000_0089] {
            let mut table = table();
            table
                .fields
                .push(field(20, FieldKind::Normal, DataType::Date, 0, options, 0));
            let mut formulas = base_formulas();
            formulas.push((20, 5, "Date ( 1 ; 15 ; 2030 )"));
            let plan = plan_of(&table, &formulas, &[]).unwrap();
            let environment = environment();
            let created = plan
                .prepare(
                    WriteMode::Create,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &environment,
                )
                .unwrap();
            assert_eq!(
                decoded(&created, 20).as_deref(),
                Some(environment.date.as_str())
            );

            let explicit = "3/3/2031";
            let saved = plan
                .prepare_entry(
                    WriteMode::Create,
                    &BTreeMap::new(),
                    &BTreeMap::from([(20, Some(explicit.into()))]),
                    &environment,
                    Entry {
                        keep_written: true,
                        ..Entry::script(false, false)
                    },
                )
                .unwrap();
            assert_eq!(decoded(&saved, 20).as_deref(), Some(explicit));

            for extra in [ALWAYS_EVALUATE, 0x100] {
                *table.fields.last_mut().unwrap() =
                    field(20, FieldKind::Normal, DataType::Date, 0, options | extra, 0);
                assert!(plan_of(&table, &formulas, &[])
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("multiple auto-entry modes"));
            }
        }
    }

    #[test]
    fn combined_creation_date_refuses_inputs_set_before_first_commit() {
        let mut table = table();
        table.fields.retain(|field| field.id <= 2);
        table.fields.extend([
            field(20, FieldKind::Normal, DataType::Date, 0, 0x0000_0089, 0),
            field(21, FieldKind::Calculation, DataType::Number, 0, 0, 0),
        ]);
        let environment = environment();
        let entry = Entry {
            keep_written: true,
            ..Entry::script(false, false)
        };
        for expression in [
            "Date ( 1 ; 15 ; 2030 ) + Length ( Field1 )",
            "Date ( 1 ; 15 ; 2030 ) + Field21",
        ] {
            let formulas = [(20, 5, expression), (21, 5, "Length ( Field1 )")];
            let plan = plan_of(&table, &formulas, &[]).unwrap();
            let created = plan
                .prepare_entry(
                    WriteMode::Create,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &environment,
                    entry,
                )
                .unwrap();
            assert_eq!(
                decoded(&created, 20).as_deref(),
                Some(environment.date.as_str())
            );

            let result = plan.prepare_entry(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::from([(1, Some("abc".into()))]),
                &environment,
                entry,
            );
            assert_eq!(
                result.err().and_then(|error| error.code),
                Some(error::UNAVAILABLE)
            );
        }
    }

    #[test]
    fn combined_creation_date_refuses_updates_that_would_run_its_calculation() {
        for (options, target_update, should_refuse) in [
            (0x0000_0089, (1, Some("abc".into())), true),
            (0x0000_0009, (20, None), true),
            (0x0000_0009, (1, Some("abc".into())), false),
            (0x0000_0089, (2, Some("unrelated".into())), false),
        ] {
            let mut table = table();
            table.fields.retain(|field| field.id <= 2);
            table
                .fields
                .push(field(20, FieldKind::Normal, DataType::Date, 0, options, 0));
            let formulas = [(
                20,
                5,
                "If ( IsEmpty ( Field20 ) ; Date ( 1 ; 15 ; 2030 ) ; \
                 Date ( 1 ; 15 ; 2030 ) + Length ( Field1 ) )",
            )];
            let plan = plan_of(&table, &formulas, &[]).unwrap();
            let stored = BTreeMap::from([(1, "previous".into()), (20, "11/26/2026".into())]);
            let result = plan.prepare_entry(
                WriteMode::Update,
                &stored,
                &BTreeMap::from([target_update]),
                &environment(),
                Entry::script(false, false),
            );
            if should_refuse {
                assert_eq!(result.err().unwrap().code, Some(error::UNAVAILABLE));
            } else {
                assert_eq!(decoded(&result.unwrap(), 20), None, "date is unchanged");
            }
        }
    }

    const INITIALS: &str = "Case ( not IsEmpty ( Field2 ) ; Left ( Field2 ; 1 ) ; \
                            not IsEmpty ( Field1 ) ; Left ( Field1 ; 1 ) )";

    fn base_formulas() -> Vec<(u64, u64, &'static str)> {
        vec![(8, 5, INITIALS), (9, 5, "Get ( UUID )")]
    }

    fn create(plan: &RecordPlan, changes: &[(u64, &str)]) -> Result<PreparedRecord, DataError> {
        let changes = changes
            .iter()
            .map(|(id, v)| (*id, Some(v.to_string())))
            .collect();
        plan.prepare(
            WriteMode::Create,
            &BTreeMap::new(),
            &changes,
            &environment(),
        )
    }

    #[test]
    fn stored_calculations_of_any_type_and_get_selectors_evaluate_with_om_calc() {
        let mut table = table();
        // Bookkeeping bits FileMaker sets on calculations keep them plannable.
        table.fields.extend([
            field(20, FieldKind::Calculation, DataType::Number, 0, 0x8_0000, 0),
            field(21, FieldKind::Calculation, DataType::Text, 0, 0x4_0000, 0),
            field(22, FieldKind::Calculation, DataType::Date, 0, 0, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.extend([
            (20, 5, "Length ( Field1 ) * 10"),
            (21, 5, "Upper ( Get ( AccountName ) ) & \"!\""),
            (22, 5, "Date ( 10 ; 28 ; 2026 ) + Length ( Field1 )"),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let created = create(&plan, &[(1, "Hello"), (2, "World")]).unwrap();
        assert_eq!(decoded(&created, 8).as_deref(), Some("W"));
        assert_eq!(decoded(&created, 20).as_deref(), Some("50"));
        assert_eq!(decoded(&created, 21).as_deref(), Some("SESSION-ACCOUNT!"));
        assert_eq!(decoded(&created, 22).as_deref(), Some("11/2/2026"));

        let existing = created
            .values
            .iter()
            .filter_map(|(id, v)| Some((*id, om_scsu::decode_scsu(v.as_ref()?).unwrap())))
            .collect();
        let updated = plan
            .prepare(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(1, Some("Hi".into()))]),
                &environment(),
            )
            .unwrap();
        assert_eq!(decoded(&updated, 20).as_deref(), Some("20"));
        assert_eq!(decoded(&updated, 22).as_deref(), Some("10/30/2026"));
        // Field 21 reads no field: it stays as it was computed at creation.
        assert!(!updated.values.contains_key(&21));
    }

    #[test]
    fn stored_record_number_uses_creation_position_and_keeps_it_on_edit() {
        let mut table = table();
        table.fields.push(field(
            23,
            FieldKind::Calculation,
            DataType::Number,
            0,
            0x80000,
            0,
        ));
        let mut formulas = base_formulas();
        formulas.push((23, 5, "Get ( RecordNumber )"));
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let changes = BTreeMap::from([(1, Some("Ada".into()))]);
        let mut at_creation = environment();
        at_creation.context.record_number = Some(RecordNumber::LocalNewRecord(3));
        let created = plan
            .prepare(WriteMode::Create, &BTreeMap::new(), &changes, &at_creation)
            .unwrap();
        assert_eq!(decoded(&created, 23).as_deref(), Some("3"));
        for number in [None, Some(RecordNumber::Window(3))] {
            at_creation.context.record_number = number;
            let error = plan
                .prepare(WriteMode::Create, &BTreeMap::new(), &changes, &at_creation)
                .err()
                .unwrap();
            assert_eq!(error.code, Some(error::UNAVAILABLE));
        }
        let hosted = plan_with(&table, &formulas, &[], Vec::new(), false)
            .err()
            .unwrap();
        assert_eq!(hosted.code, Some(error::UNAVAILABLE));

        let mut row = existing();
        row.insert(23, "3".into());
        let edited = plan
            .prepare(WriteMode::Update, &row, &changes, &environment())
            .unwrap();
        assert!(
            !edited.values.contains_key(&23),
            "preserve the stored number"
        );

        for (data_type, source) in [
            (DataType::Number, "Get ( RecordNumber ) + 0"),
            (DataType::Text, "Get ( RecordNumber )"),
        ] {
            *table.fields.last_mut().unwrap() =
                field(23, FieldKind::Calculation, data_type, 0, 0x80000, 0);
            *formulas.last_mut().unwrap() = (23, 5, source);
            assert!(plan_of(&table, &formulas, &[]).is_err(), "{source}");
        }
    }

    /// `Get ( LayoutNumber )`, `Get ( RecordNumber )`,
    /// `Get ( CurrentPrivilegeSetName )` and `Get ( CurrentHostTimestamp )`
    /// in auto-entered calculations answer from the write's operation (#187,
    /// #1038; calc-functions.md 6.3): a layout's number, empty with no
    /// window, the record's number its window gave, the session's current
    /// set, the host's clock apart from this machine's. One the operation
    /// leaves out refuses a write that evaluates it, and only such a write.
    #[test]
    fn context_selectors_answer_from_the_writes_operation() {
        let mut table = table();
        table.fields.extend([
            field(20, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
            field(21, FieldKind::Normal, DataType::Timestamp, 0, 0x8, 0),
            field(23, FieldKind::Normal, DataType::Number, 0, 0x8, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.extend([
            (
                20,
                5,
                "Get ( LayoutNumber ) & \"/\" & Get ( CurrentPrivilegeSetName )",
            ),
            (21, 5, "Get ( CurrentHostTimestamp )"),
            (23, 5, "Get ( RecordNumber )"),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        assert_eq!(
            plan.context_reads(),
            BTreeSet::from([
                selector::LAYOUT_NUMBER,
                selector::RECORD_NUMBER,
                selector::CURRENT_PRIVILEGE_SET_NAME,
                selector::CURRENT_HOST_TIMESTAMP
            ])
        );
        // The host's clock as op82 gave it (live:s0/r274_op82, the key
        // OpenMaker Server's host_time_is_a_timestamp_key test pins).
        let host =
            WriteContext::host_timestamp_of(&[0x9f, 0xaa, 0x9c, 0x80, 0x05, 0x65, 0x87, 0x30]);
        assert_eq!(host.as_deref(), Some("10/28/2026 3:12:42 PM"));
        assert_eq!(WriteContext::host_timestamp_of(&[0x9f, 0xaa]), None);
        let write = |context: WriteContext, mode: WriteMode| {
            let environment = RecordEnvironment {
                context,
                ..environment()
            };
            let existing = match mode {
                WriteMode::Create => BTreeMap::new(),
                _ => existing(),
            };
            plan.prepare(
                mode,
                &existing,
                &BTreeMap::from([(1, Some("Ada".into()))]),
                &environment,
            )
        };
        let known = WriteContext {
            layout: WriteLayout::Number(3),
            privilege_set: Some("[Data Entry Only]".into()),
            host_timestamp: host.clone(),
            // A row typed after two related rows of an unsorted portal
            // (#1038).
            record_number: Some(RecordNumber::Window(3)),
            creation_utc_clock: None,
            sequence: WriteSequence::Unmeasured,
            file_variables: None,
            other_globals: None,
        };
        let created = write(known.clone(), WriteMode::Create).unwrap();
        assert_eq!(
            decoded(&created, 20).as_deref(),
            Some("3/[Data Entry Only]")
        );
        assert_eq!(
            decoded(&created, 21).as_deref(),
            Some("10/28/2026 3:12:42 PM")
        );
        assert_eq!(decoded(&created, 23).as_deref(), Some("3"));
        let no_window = WriteContext {
            layout: WriteLayout::NoWindow,
            ..known.clone()
        };
        let created = write(no_window, WriteMode::Create).unwrap();
        assert_eq!(decoded(&created, 20).as_deref(), Some("/[Data Entry Only]"));
        for (missing, name) in [
            (
                WriteContext {
                    layout: WriteLayout::Unknown,
                    ..known.clone()
                },
                "Get ( LayoutNumber )",
            ),
            (
                WriteContext {
                    record_number: None,
                    ..known.clone()
                },
                "Get ( RecordNumber )",
            ),
            (
                WriteContext {
                    privilege_set: None,
                    ..known.clone()
                },
                "Get ( CurrentPrivilegeSetName )",
            ),
            (
                WriteContext {
                    host_timestamp: Some("not a clock".into()),
                    ..known.clone()
                },
                "Get ( CurrentHostTimestamp )",
            ),
        ] {
            let refused = write(missing.clone(), WriteMode::Create).err().unwrap();
            assert!(refused.to_string().contains(name), "{refused}");
            // An edit evaluates neither creation-only formula.
            let edit = write(missing, WriteMode::Update);
            assert!(edit.is_ok(), "{name}: {:?}", edit.err());
        }
        // A validation calculation the write cannot evaluate refuses it
        // with 3, as an auto-entered one does; one that evaluates false is
        // 507.
        let mut validated = table.clone();
        validated
            .fields
            .push(field(24, FieldKind::Normal, DataType::Text, 0, 0, 0x3));
        let mut checks = formulas.clone();
        checks.push((24, FIELD_VALIDATION, "Get ( RecordNumber ) > 0"));
        let validated = plan_of(&validated, &checks, &[]).unwrap();
        let validate = |record_number: Option<u64>| {
            let environment = RecordEnvironment {
                context: WriteContext {
                    record_number: record_number.map(RecordNumber::Window),
                    ..known.clone()
                },
                ..environment()
            };
            validated.prepare(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(1, Some("Ada".into()))]),
                &environment,
            )
        };
        assert!(validate(Some(3)).is_ok());
        let code = |record_number| validate(record_number).err().and_then(|e| e.code);
        assert_eq!(code(None), Some(error::UNAVAILABLE));
        assert_eq!(code(Some(0)), Some(error::VALIDATION_CALCULATION));
        // A stored calculation, which a host recalculates with no
        // operation, still keeps the table read-only.
        let mut stored = table.clone();
        stored
            .fields
            .push(field(22, FieldKind::Calculation, DataType::Number, 0, 0, 0));
        formulas.push((22, 5, "Get ( LayoutNumber )"));
        let refused = plan_of(&stored, &formulas, &[]).err().unwrap().to_string();
        assert!(refused.contains("Get() selector"), "{refused}");
    }

    #[test]
    fn calculations_that_resolve_names_at_run_time_keep_the_table_read_only() {
        // GetField, Evaluate and ExecuteSQL can read a global or related
        // field the preflight never saw; such a read must not become empty
        // text in a write (record-writes.md).
        for source in [
            "\"prefix:\" & GetField ( \"Example::Field1\" )",
            "Evaluate ( \"Field1\" )",
            "ExecuteSQL ( \"SELECT 1 FROM Example\" ; \"\" ; \"\" )",
        ] {
            let mut table = table();
            table
                .fields
                .push(field(20, FieldKind::Calculation, DataType::Text, 0, 0, 0));
            let mut formulas = base_formulas();
            formulas.push((20, 5, source));
            let error = plan_of(&table, &formulas, &[]).err().unwrap().to_string();
            assert!(
                error.contains("Evaluate, GetField and ExecuteSQL"),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn calculations_wait_for_a_referenced_value_unless_always_evaluated() {
        let mut table = table();
        table.fields.extend([
            field(20, FieldKind::Calculation, DataType::Text, 0, 0, 0),
            field(
                21,
                FieldKind::Calculation,
                DataType::Text,
                0,
                ALWAYS_EVALUATE,
                0,
            ),
        ]);
        let mut formulas = base_formulas();
        formulas.extend([(20, 5, "\"x\" & Field1"), (21, 5, "\"y\" & Field1")]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let blank = create(&plan, &[]).unwrap();
        assert_eq!(decoded(&blank, 20), None, "all referenced fields are empty");
        assert_eq!(decoded(&blank, 21).as_deref(), Some("y"));
        let filled = create(&plan, &[(1, "a")]).unwrap();
        assert_eq!(decoded(&filled, 20).as_deref(), Some("xa"));
        assert_eq!(decoded(&filled, 21).as_deref(), Some("ya"));
    }

    #[test]
    fn auto_entered_self_reference_reads_current_value_and_keeps_its_triggers() {
        let mut table = table();
        table.fields.retain(|f| f.id <= 2);
        table.fields.extend([
            field(20, FieldKind::Normal, DataType::Text, 0, 0x88, 0),
            field(21, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
        ]);
        let mut formulas = vec![
            (20, 5, "Field20 & Field1 & \"!\""),
            (21, 5, "Field21 & Field1 & \"!\""),
        ];
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let blank = create(&plan, &[]).unwrap();
        assert_eq!(decoded(&blank, 20), None, "skip all empty, including self");
        let filled = create(&plan, &[(20, "a"), (21, "kept")]).unwrap();
        assert_eq!(decoded(&filled, 20).as_deref(), Some("a!"));
        assert_eq!(decoded(&filled, 21).as_deref(), Some("kept"));
        let row = BTreeMap::from([(20, "a!".into()), (21, "kept".into())]);
        for (changes, expected) in [
            (vec![(20, Some("b".into()))], Some("b!")),
            (vec![(1, Some("c".into()))], Some("a!c!")),
            (vec![(2, Some("unrelated".into()))], None),
            (vec![(20, None)], None),
        ] {
            let updated = plan
                .prepare(
                    WriteMode::Update,
                    &row,
                    &changes.into_iter().collect(),
                    &environment(),
                )
                .unwrap();
            assert_eq!(decoded(&updated, 20).as_deref(), expected);
            assert_eq!(decoded(&updated, 21), None, "do not replace");
        }
        // A stored self-reference and a cycle between two auto-entered
        // fields still refuse the table.
        *table.fields.iter_mut().find(|f| f.id == 20).unwrap() =
            field(20, FieldKind::Calculation, DataType::Text, 0, 0, 0);
        assert!(plan_of(&table, &formulas, &[])
            .err()
            .unwrap()
            .to_string()
            .contains("Cyclic"));
        *table.fields.iter_mut().find(|f| f.id == 20).unwrap() =
            field(20, FieldKind::Normal, DataType::Text, 0, 0x88, 0);
        formulas.retain(|(id, _, _)| ![20, 21].contains(id));
        formulas.extend([(20, 5, "Field21"), (21, 5, "Field20")]);
        assert!(plan_of(&table, &formulas, &[])
            .err()
            .unwrap()
            .to_string()
            .contains("Cyclic"));
    }

    /// Auto-entered and validation formulas over an unstored calculation
    /// of the table, as FileMaker Server's script engine evaluates them
    /// (script oracle s44): the calculation is evaluated on the record
    /// with its own "Do not evaluate if all referenced fields are empty", a
    /// change of its input evaluates a replacing formula again, and the
    /// validation reads it as the record holds it. One the write cannot
    /// evaluate still refuses the table.
    #[test]
    fn formulas_over_an_unstored_calculation_follow_its_inputs() {
        let mut table = table();
        table.fields.retain(|f| f.id <= 2);
        let always = VALIDATE_BY_CALCULATION | VALIDATE_CALCULATION_ALWAYS | VALIDATE_ALWAYS;
        table.fields.extend([
            field(14, FieldKind::Calculation, DataType::Text, 0, UNSTORED, 0),
            field(15, FieldKind::Normal, DataType::Text, 0, 0x88, 0),
            field(16, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
            field(17, FieldKind::Normal, DataType::Text, 0, 0, always),
        ]);
        let mut formulas = vec![
            (14, 5, "Field1 & \"!\""),
            (15, 5, "Field14 & \"?\""),
            (16, 5, "Field14 & \"?\""),
            (17, 0x19, "Field14 <> \"x!\""),
        ];
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let blank = create(&plan, &[]).unwrap();
        assert_eq!(decoded(&blank, 15), None, "the calculation reads empty");
        assert_eq!(decoded(&blank, 16), None);
        let made = create(&plan, &[(1, "a")]).unwrap();
        assert_eq!(decoded(&made, 15).as_deref(), Some("a!?"));
        assert_eq!(decoded(&made, 16).as_deref(), Some("a!?"));
        let row = BTreeMap::from([(1, "a".into()), (15, "a!?".into()), (16, "a!?".into())]);
        let update = |changes: &[(u64, &str)]| {
            let changes = changes
                .iter()
                .map(|(id, v)| (*id, Some(v.to_string())))
                .collect();
            plan.prepare(WriteMode::Update, &row, &changes, &environment())
        };
        let changed = update(&[(1, "b")]).unwrap();
        assert_eq!(decoded(&changed, 15).as_deref(), Some("b!?"));
        assert_eq!(decoded(&changed, 16), None, "do not replace");
        let other = update(&[(2, "z")]).unwrap();
        assert_eq!(decoded(&other, 15), None, "not an input");
        assert_eq!(update(&[(1, "x")]).err().unwrap().code, Some(507));
        // A `Random` draw in the calculation makes the formula over it
        // change when evaluated again: no preview shows its value.
        let mut drawn = formulas.clone();
        drawn[0] = (14, 5, "Field1 & Random");
        let plan = plan_of(&table, &drawn, &[]).unwrap();
        assert!(!plan.preview_fields(true).contains(&15));
        assert!(plan.preview_fields(true).contains(&1));
        // So is a skip it decides: whether the calculation reads empty is
        // drawn again at the commit.
        drawn[0] = (14, 5, "If ( Random < 2 ; \"\" ; \"x\" )");
        let plan = plan_of(&table, &drawn, &[]).unwrap();
        let known = |_: u64, _: Option<u64>, _: u64| false;
        let entry = (Entry::default(), (true, true), &known as UnknownValue<'_>);
        let (_, unknown) = plan
            .pending_row(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &environment(),
                entry,
            )
            .unwrap();
        assert!(unknown.contains(&15), "{unknown:?}");
        // Over another occurrence or a global field, the write could not
        // evaluate it.
        table
            .fields
            .push(field(18, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0));
        formulas[0] = (14, 5, "Field1 & Field18");
        let refused = plan_of(&table, &formulas, &[]).err().unwrap();
        assert!(
            refused
                .to_string()
                .contains("reads an unstored calculation a write cannot evaluate"),
            "{refused}"
        );
    }

    #[test]
    fn unstored_budget_covers_empty_probes_and_rule_body() {
        let mut table = table();
        table.fields.retain(|f| f.id == 1);
        table.fields.extend([
            field(
                2,
                FieldKind::Normal,
                DataType::Text,
                0,
                0,
                VALIDATE_BY_CALCULATION | VALIDATE_ALWAYS,
            ),
            field(14, FieldKind::Calculation, DataType::Text, 0, UNSTORED, 0),
            field(15, FieldKind::Calculation, DataType::Text, 0, UNSTORED, 0),
            field(16, FieldKind::Normal, DataType::Text, 0, 0x88, 0),
        ]);
        let plan = plan_of(
            &table,
            &[
                (14, 5, "\"\""),
                (15, 5, "Field14 & Field1"),
                (16, 5, "Field14 & Field15 & \"!\""),
                (2, 0x19, "Field15 <> \"reject\""),
            ],
            &[],
        )
        .unwrap();
        let with_budget = |calculations| {
            let mut env = environment();
            env.limits.calculations = calculations;
            env
        };
        let create = |changes: &BTreeMap<u64, Option<String>>, limit| {
            plan.prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                changes,
                &with_budget(limit),
            )
        };

        // The probes cost one read of Field14 and two through Field15.
        let blank = BTreeMap::new();
        assert_eq!(create(&blank, 2).err().unwrap().code, Some(7));
        assert_eq!(decoded(&create(&blank, 3).unwrap(), 16), None);

        // A nonempty result runs the body, which needs three more reads.
        let input = BTreeMap::from([(1, Some("value".into())), (2, Some("edited".into()))]);
        assert_eq!(create(&input, 5).err().unwrap().code, Some(7));
        let preview = plan
            .preview_entry(
                WriteMode::Create,
                &BTreeMap::new(),
                &input,
                &with_budget(5),
                Entry::default(),
                &plan.preview_fields(true),
            )
            .unwrap();
        assert!(!preview.contains_key(&16));
        // Validation still has its own allowance after auto-entry uses all six.
        assert_eq!(
            decoded(&create(&input, 6).unwrap(), 16).as_deref(),
            Some("value!")
        );

        // Updating only Field2 evaluates validation without firing auto-entry.
        let existing = BTreeMap::from([(1, "reject".into())]);
        let changes = BTreeMap::from([(2, Some("edited".into()))]);
        let update =
            |limit| plan.prepare(WriteMode::Update, &existing, &changes, &with_budget(limit));
        assert_eq!(update(1).err().unwrap().code, Some(7));
        assert_eq!(update(2).err().unwrap().code, Some(507));
    }

    /// A ladder of unstored calculations that each read both of the level
    /// below has 2^30 paths over 60 fields: planning a formula over its top
    /// follows each calculation once (a hostile file must not hang it), and
    /// refuses the table, as its evaluation would take 2^30 reads.
    #[test]
    fn a_shared_graph_of_unstored_calculations_is_followed_once() {
        let mut table = table();
        table.fields.retain(|f| f.id <= 2);
        let levels = 30u64;
        let mut formulas = Vec::new();
        let sources: Vec<String> = (0..levels)
            .flat_map(|level| {
                let below = |side: u64| {
                    if level == 0 {
                        format!("Field{}", side + 1)
                    } else {
                        format!("Field{}", 100 + 2 * (level - 1) + side)
                    }
                };
                [0, 1].map(|_| format!("{} & {}", below(0), below(1)))
            })
            .collect();
        for (i, source) in sources.iter().enumerate() {
            let id = 100 + i as u64;
            table.fields.push(field(
                id,
                FieldKind::Calculation,
                DataType::Text,
                0,
                UNSTORED,
                0,
            ));
            formulas.push((id, 5, source.as_str()));
        }
        table
            .fields
            .push(field(300, FieldKind::Normal, DataType::Text, 0, 0x88, 0));
        let top = format!("Field{}", 100 + 2 * (levels - 1));
        formulas.push((300, 5, top.as_str()));
        let started = std::time::Instant::now();
        let refused = plan_of(&table, &formulas, &[]).err().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(refused.to_string().contains("too many times"), "{refused}");
    }

    /// A loop reads an unstored calculation on each turn, which planning
    /// does not count: two loops of 1,000 turns over a chain plan as four
    /// evaluations, and the write's evaluation ends with error 7 past the
    /// same 65,536 evaluations instead of running a million; a loop of 10
    /// turns over the inner one evaluates.
    #[test]
    fn a_loop_reading_unstored_calculations_again_stops_the_write() {
        let mut table = table();
        table.fields.retain(|f| f.id <= 2);
        for id in [100, 101, 102] {
            table.fields.push(field(
                id,
                FieldKind::Calculation,
                DataType::Text,
                0,
                UNSTORED,
                0,
            ));
        }
        table
            .fields
            .push(field(300, FieldKind::Normal, DataType::Text, 0, 0x88, 0));
        let looped = |turns: u32, read: u64| {
            format!(
                "While ( [ i = 0 ; r = \"\" ] ; i < {turns} ; [ i = i + 1 ; r = Field{read} ] ; r )"
            )
        };
        let (inner, outer) = (looped(1000, 100), looped(1000, 101));
        let mut formulas = vec![
            (100, 5, "Field1 & \"!\""),
            (101, 5, inner.as_str()),
            (102, 5, outer.as_str()),
            (300, 5, "Field102"),
        ];
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let started = std::time::Instant::now();
        let refused = create(&plan, &[(1, "a")]).err().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(refused.to_string().contains("error 7"), "{refused}");
        let few = looped(10, 101);
        formulas[2] = (102, 5, few.as_str());
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let made = create(&plan, &[(1, "a")]).unwrap();
        assert_eq!(decoded(&made, 300).as_deref(), Some("a!"));
    }

    /// A chain of unstored calculations deeper than the calculation host
    /// evaluates them is refused, also where a formula read part of it
    /// first: 20 calculations, read by one formula, then 15 more over them
    /// read by another (35 deep).
    #[test]
    fn a_chain_of_unstored_calculations_deeper_than_evaluated_is_refused() {
        let mut table = table();
        table.fields.retain(|f| f.id <= 2);
        let mut sources = Vec::new();
        for i in 0..35u64 {
            let id = 100 + i;
            table.fields.push(field(
                id,
                FieldKind::Calculation,
                DataType::Text,
                0,
                UNSTORED,
                0,
            ));
            let below = if i == 0 { 1 } else { id - 1 };
            sources.push((id, format!("Field{below} & \"!\"")));
        }
        table
            .fields
            .push(field(50, FieldKind::Normal, DataType::Text, 0, 0x88, 0));
        let mut formulas: Vec<(u64, u64, &str)> =
            sources.iter().map(|(id, s)| (*id, 5, s.as_str())).collect();
        formulas.push((50, 5, "Field119"));
        let shallow = plan_of(&table, &formulas, &[]);
        assert!(shallow.is_ok(), "{:?}", shallow.err());
        table
            .fields
            .push(field(300, FieldKind::Normal, DataType::Text, 0, 0x88, 0));
        formulas.push((300, 5, "Field134"));
        let refused = plan_of(&table, &formulas, &[]).err().unwrap();
        assert!(refused.to_string().contains("more deeply"), "{refused}");
    }

    #[test]
    fn validation_by_calculation_refuses_values_with_the_files_message() {
        let mut table = table();
        table.fields[0].options[12..16].copy_from_slice(&0x1u32.to_be_bytes());
        table.fields[1].options[12..16].copy_from_slice(&0x81u32.to_be_bytes());
        let mut formulas = base_formulas();
        formulas.extend([
            (1, 0x19, "Length ( Self ) < 5"),
            (2, 0x19, "Self <> \"nope\""),
        ]);
        let message = vec![(2, 0x18, om_scsu::encode_scsu("Pick another family name."))];
        let plan = plan_of(&table, &formulas, &message).unwrap();
        assert!(create(&plan, &[(1, "Hey"), (2, "Fine")]).is_ok());
        let error = create(&plan, &[(1, "Hello world")]).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("Field1 requires a value its validation"),
            "{error}"
        );
        let error = create(&plan, &[(2, "nope")]).err().unwrap();
        assert_eq!(error.to_string(), "Pick another family name.");
    }

    /// A custom message as FileMaker Pro 26 writes it (`0x8000` with `0x80`,
    /// the formula in `20`, in `18` the text older versions show;
    /// schema-path.md 5.9 E34) leaves the table writable, and a refusal
    /// shows the formula's result over the record (#602). Where the record
    /// is not at hand, only a message that is text alone is shown.
    #[test]
    fn a_calculated_custom_message_is_evaluated_in_the_refused_record() {
        let mut table = table();
        // Field1: not empty. Field2: by calculation, at most 3 characters.
        table.fields[0].options[12..16].copy_from_slice(&0x8088u32.to_be_bytes());
        table.fields[1].options[12..16].copy_from_slice(&0x8281u32.to_be_bytes());
        table.fields[1].options[20..24].copy_from_slice(&3u32.to_be_bytes());
        let mut formulas = base_formulas();
        formulas.extend([
            (1, 0x20, "\"Field1 is needed beside \" & Field2"),
            (2, 0x19, "Self <> \"no\""),
            (2, 0x20, "\"Pick \" & \"another\""),
        ]);
        let older = om_scsu::encode_scsu("The custom validation message is not available.");
        let leaves = vec![(1, 0x18, older.clone()), (2, 0x18, older)];
        let plan = plan_of(&table, &formulas, &leaves).unwrap();
        let error = create(&plan, &[(2, "abc")]).err().unwrap();
        assert_eq!(error.to_string(), "Field1 is needed beside abc");
        let error = create(&plan, &[(1, "x"), (2, "no")]).err().unwrap();
        assert_eq!(error.to_string(), "Pick another");
        let typed = plan.check_entry(2, "four", Entry::default()).unwrap_err();
        assert_eq!(typed.to_string(), "Pick another");
        // A message reading the record, with no record: OpenMaker's wording.
        assert_eq!(
            plan.refusal(1, error::NOT_UNIQUE).to_string(),
            "Field Field1 requires a unique value."
        );
        assert!(!plan.reads_globals());
        // An empty message, one reading what a one-record evaluation cannot
        // give (a container), and one reading a global, which the write
        // must then carry.
        let mut global = field(14, FieldKind::Normal, DataType::Text, 0, 0, 0);
        global.name = "Greeting".into();
        global.options[8..12].copy_from_slice(&0x1_0000u32.to_be_bytes());
        table.fields.push(global);
        table.fields[0].options[12..16].copy_from_slice(&0x8088u32.to_be_bytes());
        let mut formulas = base_formulas();
        formulas.extend([
            (1, 0x20, "\"Not \" & Field3"),
            (2, 0x19, "Self <> \"no\""),
            (2, 0x20, "\"\" & \"\""),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let error = create(&plan, &[(2, "abc")]).err().unwrap();
        assert_eq!(error.to_string(), "Field Field1 requires a value.");
        let typed = plan.check_entry(2, "four", Entry::default()).unwrap_err();
        assert_eq!(
            typed.to_string(),
            "Field Field2 requires at most 3 characters."
        );
        table.fields[1].options[12..16].copy_from_slice(&0u32.to_be_bytes());
        let mut formulas = base_formulas();
        formulas.push((1, 0x20, "Greeting"));
        assert!(plan_of(&table, &formulas, &[]).unwrap().reads_globals());
    }

    /// What FileMaker Server's script engine does with a repeating field
    /// (script oracle, script-runtime.md "Implementation notes (PSOS)"): not
    /// empty holds when any repetition has a value; maximum characters and a
    /// validation calculation check each changed repetition (each one when
    /// validated Always), the calculation in that repetition (`Self`, the
    /// field and `Get ( CalculationRepetitionNumber )` are repetition n's,
    /// a non-repeating field reads its value).
    #[test]
    fn validation_checks_each_repetition_as_filemaker_server_does() {
        let mut table = table();
        let repeating = |id, validation: u32, limit: u32| {
            let mut f = field(id, FieldKind::Normal, DataType::Text, 0, 0, validation);
            f.options[20..24].copy_from_slice(&limit.to_be_bytes());
            f.options[24..26].copy_from_slice(&3u16.to_be_bytes());
            f
        };
        table.fields.extend([
            // Not empty, by calculation, at most 3 characters, Always.
            repeating(30, 0x40c, 0),
            repeating(31, 0x405, 0),
            repeating(32, 0x604, 3),
            repeating(33, 0x405, 0),
            // By calculation, only during data entry.
            repeating(34, 0x005, 0),
            // By calculation, also when not modified (`0x2`), Always.
            repeating(35, 0x407, 0),
            repeating(36, 0x405, 0),
            repeating(37, 0x405, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.extend([
            (31, 0x19, "Length ( Self ) <= 3"),
            (
                33,
                0x19,
                "not IsEmpty ( Field1 ) and Get ( CalculationRepetitionNumber ) < 3 \
                 and Field33 = Self",
            ),
            (34, 0x19, "Length ( Self ) <= 3"),
            (35, 0x19, "not IsEmpty ( Self )"),
            (36, 0x19, "not IsEmpty ( Self )"),
            (37, 0x19, "Length ( Self ) / 0 < 1"),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let rep = |field: u64, n: u64| field * 256 + n;
        let mut existing = existing();
        existing.extend([
            (30, "x".to_string()),
            (rep(30, 3), "z".to_string()),
            // Stored before the rule: not checked unless changed.
            (34, "toolong".to_string()),
            (35, "a".to_string()),
            (rep(35, 2), "b".to_string()),
            (36, "a".to_string()),
        ]);
        let write_on = |existing: &BTreeMap<u64, String>, changes: &[(u64, Option<&str>)]| {
            let changes = changes
                .iter()
                .map(|(cell, text)| (*cell, text.map(str::to_string)))
                .collect();
            plan.compute(
                WriteMode::Update,
                existing,
                &changes,
                &environment(),
                Entry::script(false, false),
            )
            .map(|_| ())
            .map_err(|e| e.code)
        };
        let write = |changes: &[(u64, Option<&str>)]| write_on(&existing, changes);
        // Not empty: repetition 3 still holds a value.
        assert_eq!(write(&[(30, None)]), Ok(()));
        assert_eq!(write(&[(30, None), (rep(30, 3), None)]), Err(Some(509)));
        assert_eq!(write(&[(rep(31, 2), Some("toolong"))]), Err(Some(507)));
        assert_eq!(write(&[(rep(31, 2), Some("abc"))]), Ok(()));
        assert_eq!(write(&[(rep(32, 3), Some("abcd"))]), Err(Some(511)));
        assert_eq!(write(&[(rep(32, 3), Some("abc"))]), Ok(()));
        assert_eq!(write(&[(rep(33, 2), Some("v"))]), Ok(()));
        assert_eq!(write(&[(rep(33, 3), Some("v"))]), Err(Some(507)));
        assert_eq!(write(&[(rep(34, 2), Some("ab"))]), Ok(()));
        assert_eq!(write(&[(rep(34, 2), Some("abcd"))]), Err(Some(507)));
        // A cleared repetition is checked, empty as it is.
        assert_eq!(write(&[(36, None)]), Err(Some(507)));
        // `0x2`: repetition 1 and the repetitions holding a value are
        // checked unchanged (another field's write fails when repetition 1
        // is empty), the empty repetitions after them are not.
        assert_eq!(write(&[(1, Some("Changed"))]), Ok(()));
        let mut empty_first = existing.clone();
        empty_first.remove(&35);
        assert_eq!(
            write_on(&empty_first, &[(1, Some("Changed"))]),
            Err(Some(507))
        );
        // A calculation that fails accepts nothing.
        assert_eq!(write(&[(37, Some("a"))]), Err(Some(507)));
        // Maximum characters is also Set Field's own check.
        assert_eq!(
            plan.check_entry(rep(32, 2), "abcd", Entry::script(false, false))
                .map_err(|e| e.code),
            Err(Some(511))
        );
        assert!(plan
            .check_entry(rep(32, 2), "abc", Entry::script(false, false))
            .is_ok());
        assert_eq!(
            plan.cells(31).collect::<Vec<_>>(),
            [31, rep(31, 2), rep(31, 3)]
        );
    }

    /// The review's C7: while repetition n of a repeating field is
    /// validated, a field that does not repeat reads its value however the
    /// formula names it: bare, as its repetition 1, through `GetRepetition`
    /// or `Extend` (Claris: Extend makes a non-repeating field's value
    /// available in every repetition).
    #[test]
    fn validation_in_a_later_repetition_reads_non_repeating_fields() {
        let mut table = table();
        let repeating = |id| {
            let mut f = field(id, FieldKind::Normal, DataType::Text, 0, 0, 0x405);
            f.options[24..26].copy_from_slice(&3u16.to_be_bytes());
            f
        };
        table.fields.extend([40, 41, 42, 43].map(repeating));
        let mut formulas = base_formulas();
        formulas.extend([
            (40, 0x19, "Self = Field1"),
            (41, 0x19, "Self = Field1[1]"),
            (42, 0x19, "Self = GetRepetition ( Field1 ; 1 )"),
            (43, 0x19, "Self = Extend ( Field1 )"),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let write = |cell: u64, text: &str| {
            plan.compute(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(cell, Some(text.to_string()))]),
                &environment(),
                Entry::script(false, false),
            )
            .map(|_| ())
            .map_err(|e| e.code)
        };
        for field in [40, 41, 42, 43] {
            // Field1 is "Hello".
            assert_eq!(write(field * 256 + 2, "Hello"), Ok(()), "{field}");
            assert_eq!(write(field * 256 + 3, "Hello"), Ok(()), "{field}");
            assert_eq!(write(field * 256 + 2, "Other"), Err(Some(507)), "{field}");
            assert_eq!(write(field, "Hello"), Ok(()), "{field}");
        }
    }

    /// Value list `id` of the catalog `[21, 5, id]`: custom values, or the
    /// values of `field` of occurrence `0xd0_0002`.
    fn value_list(
        id: u64,
        name: &str,
        custom: Option<&str>,
        field: u64,
    ) -> Vec<(Vec<Vec<u8>>, Vec<u8>)> {
        let path = |key: u64, rest: &[u64]| -> Vec<Vec<u8>> {
            [0x21, 5, id, key]
                .iter()
                .chain(rest)
                .map(|k| om_hbam::slot::uint_segment(*k))
                .collect()
        };
        let mut entries = vec![(path(0x10, &[]), om_scsu::encode_scsu(name))];
        match custom {
            Some(values) => entries.push((path(3, &[]), om_scsu::encode_scsu(values))),
            None => {
                // Kind 1 (a field), no options, the field spec, no second
                // field, start, file or list, the separator.
                let mut definition = vec![1, 0, 0, 4, 3, 0xd0, 0, 2, 2, 1, field as u8, 0, 1];
                definition.extend([0, 0, 0, 0, 0, 0, 0]);
                definition.extend(om_scsu::encode_counted(" "));
                entries.push((path(2, &[]), definition));
            }
        }
        entries
    }

    /// A field's reference to value list `id` (`07`): a key string, then
    /// bookkeeping (as FMUpgradeTool writes it, script oracle s18).
    fn list_reference(id: u8) -> Vec<u8> {
        vec![2, 1, id, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff]
    }

    #[test]
    fn entry_rules_are_set_fields_own_for_fields_validated_always() {
        // Script oracle s18 (script-runtime.md 13.21): strict data types,
        // range and value lists, on fields with three repetitions or one.
        let mut table = table();
        let with = |id, ty, validation: u32, repetitions: u16, options: u32| {
            let mut f = field(id, FieldKind::Normal, ty, 0, options, validation);
            f.options[24..26].copy_from_slice(&repetitions.to_be_bytes());
            f
        };
        table.fields.extend([
            // Numeric only, strict and Always; overridable, data entry only.
            with(30, DataType::Number, 0x1404, 3, 0),
            with(31, DataType::Number, 0x1000, 3, 0),
            // 4-digit year on a date and a timestamp, time of day.
            with(32, DataType::Date, 0x2404, 1, 0),
            with(33, DataType::Timestamp, 0x2400, 1, 0),
            with(34, DataType::Time, 0x4400, 1, 0),
            // In range 1...10.
            with(35, DataType::Number, 0x444, 1, 0),
            // Member of a custom list (Always, three repetitions; data
            // entry only), of a field's values, an existing value.
            with(36, DataType::Text, 0x504, 3, 0),
            with(37, DataType::Text, 0x100, 1, 0),
            with(38, DataType::Text, 0x500, 1, 0),
            with(39, DataType::Text, 0, 1, 0x8000_0000),
            with(40, DataType::Text, 0x424, 1, 0x8000_0000),
            with(41, DataType::Text, 0x20, 1, 0x8000_0000),
            // Member of a two-field list, which the rules cannot check
            // yet, and of a related-values list, the host's to check, both
            // during data entry only.
            with(46, DataType::Text, 0x100, 1, 0),
            with(47, DataType::Text, 0x100, 1, 0),
            // A date, a time and a timestamp without validation.
            with(43, DataType::Date, 0, 1, 0),
            with(44, DataType::Time, 0, 1, 0),
            with(45, DataType::Timestamp, 0, 1, 0),
        ]);
        let leaves = [
            (35, 0x21, om_scsu::encode_scsu("1")),
            (35, 0x22, om_scsu::encode_scsu("10")),
            (36, 0x07, list_reference(3)),
            (37, 0x07, list_reference(3)),
            (38, 0x07, list_reference(5)),
            (46, 0x07, list_reference(6)),
            (47, 0x07, list_reference(7)),
        ];
        let mut lists = value_list(3, "Title", Some("Mr.\rMrs.\rMs.\r-\rProf.\r-"), 0);
        lists.extend(value_list(5, "Company", None, 39));
        // Field 39's values with field 1's beside them: a second field.
        let mut two = value_list(6, "Two", None, 39);
        two[1].1.splice(13..17, [4, 3, 0xd0, 0, 2, 2, 1, 1, 0, 1]);
        lists.extend(two);
        // Field 39's values, "Include only related values starting from"
        // occurrence `0xd0_0002` (options 0x0001, the start's key).
        let mut related = value_list(7, "Related", None, 39);
        let definition = &mut related[1].1;
        definition[2] = 1;
        // The start's empty key, before the file's, the list's and the
        // separator.
        let start = definition.len() - om_scsu::encode_counted(" ").len() - 3;
        definition.splice(start..start + 1, [4, 3, 0xd0, 0, 2]);
        lists.extend(related);
        let plan = plan_with(&table, &base_formulas(), &leaves, lists, true).unwrap();
        let script = Entry::script(false, false);
        let rep = |field: u64, n: u64| field * 256 + n;
        let check =
            |cell, text: &str, entry| plan.check_entry(cell, text, entry).map_err(|e| e.code);
        let refused = |code| Err(Some(code));
        for (cell, text, expected) in [
            (30, "abc", refused(error::NUMBER)),
            (30, "12abc", refused(error::NUMBER)),
            (rep(30, 2), "abc", refused(error::NUMBER)),
            (30, "1.5", Ok(())),
            (30, "-3", Ok(())),
            (30, "1e3", Ok(())),
            (30, "", Ok(())),
            // Only during data entry: a script's value is not checked.
            (31, "abc", Ok(())),
            (32, "1/2/26", refused(error::DATE)),
            (32, "abc", refused(error::DATE)),
            (32, "1/2/2026", Ok(())),
            (32, "1/2/0026", Ok(())),
            (33, "1/2/26 10:00", refused(error::DATE)),
            (33, "1/2/2026 10:00", Ok(())),
            (34, "25:00", refused(error::TIME)),
            (34, "-1:00", refused(error::TIME)),
            (34, "abc", refused(error::TIME)),
            (34, "10:30", Ok(())),
            (35, "11", refused(error::RANGE)),
            (35, "abc", refused(error::RANGE)),
            (35, "5", Ok(())),
            (35, "10", Ok(())),
            (36, "Dr.", refused(error::NOT_IN_LIST)),
            (rep(36, 2), "Dr.", refused(error::NOT_IN_LIST)),
            (36, "mr.", Ok(())),
            (36, "Prof.", Ok(())),
            (37, "Dr.", Ok(())),
        ] {
            assert_eq!(check(cell, text, script), expected, "{cell} {text:?}");
        }
        // A user's entry is checked during data entry too.
        assert_eq!(check(31, "abc", Entry::default()), refused(error::NUMBER));
        assert_eq!(
            check(37, "Dr.", Entry::default()),
            refused(error::NOT_IN_LIST)
        );
        // A field's values are the host's to look up; existing values too,
        // at the commit, also when validated only during data entry,
        // unless that is skipped.
        assert_eq!(plan.value_list_field(38, script), Some((0xd0_0002, 39)));
        assert_eq!(plan.value_list_field(36, script), None);
        // A list the rules cannot check: the table plans, a script's value
        // for data entry only is not checked, a user's is refused (3).
        assert_eq!(check(46, "x", script), Ok(()));
        assert_eq!(check(46, "x", Entry::default()), refused(3));
        assert_eq!(check(46, "", Entry::default()), Ok(()));
        assert_eq!(plan.value_list_field(46, Entry::default()), None);
        // A related-values list is the host's to compute for the record at
        // the commit: Set Field's own check has no record.
        assert_eq!(check(47, "x", Entry::default()), Ok(()));
        assert!(plan.related_value_list(47, Entry::default()).is_some());
        assert!(plan.related_value_list(47, script).is_none());
        assert_eq!(plan.value_list_field(47, Entry::default()), None);
        // Typing into a global is checked as it is entered, with no commit
        // after it: against a custom list or a field's values, in a
        // repetition the field has, not against a related-values list or
        // one the rules cannot check.
        assert!(plan.checks_global_entry(36, 36));
        assert!(plan.checks_global_entry(36, rep(36, 3)));
        assert!(!plan.checks_global_entry(36, rep(36, 4)));
        assert!(plan.checks_global_entry(38, 38));
        assert!(!plan.checks_global_entry(38, 36));
        assert!(!plan.checks_global_entry(46, 46));
        assert!(!plan.checks_global_entry(47, 47));
        assert_eq!(plan.existing_fields_for(script), vec![40, 41]);
        assert_eq!(
            plan.existing_fields_for(Entry::script(true, false)),
            vec![40]
        );
        assert_eq!(plan.refusal(40, error::NOT_EXISTING).code, Some(505));

        // The commit does not check a script's value against a rule only
        // for data entry, and checks an auto-entered or user's value.
        let compute = |entry, changes: &[(u64, &str)]| {
            plan.compute(
                WriteMode::Update,
                &existing(),
                &changes
                    .iter()
                    .map(|(cell, text)| (*cell, Some(text.to_string())))
                    .collect(),
                &environment(),
                entry,
            )
            .map(|_| ())
            .map_err(|e| e.code)
        };
        assert_eq!(compute(script, &[(31, "abc"), (37, "Dr.")]), Ok(()));
        assert_eq!(compute(script, &[(46, "x")]), Ok(()));
        assert_eq!(compute(Entry::default(), &[(46, "x")]), refused(3));
        assert_eq!(compute(Entry::default(), &[(47, "x")]), Ok(()));
        assert_eq!(compute(script, &[(35, "11")]), refused(error::RANGE));
        assert_eq!(
            compute(Entry::default(), &[(37, "Dr.")]),
            refused(error::NOT_IN_LIST)
        );

        // A user's entry into stored number, date, time and timestamp
        // fields (#185): a number is stored as typed, a date, time or
        // timestamp as data entry reads it (`typing`), and the entry rules
        // refuse what they refuse for Set Field. A repeating field's later
        // repetitions take entry too, each in its own cell.
        let editable = plan.editable_fields();
        for name in [
            "Field30", "Field32", "Field33", "Field34", "Field35", "Field43",
        ] {
            assert!(editable.contains(name), "{name}");
        }
        let typed = |cell: u64, text: &str| {
            plan.compute(
                WriteMode::Update,
                &existing(),
                &BTreeMap::from([(cell, Some(text.to_string()))]),
                &environment(),
                Entry::default(),
            )
            .map(|values| values.get(&cell).cloned().flatten())
            .map_err(|e| (e.code, e.message))
        };
        let stored = |text: &str| Ok(Some(text.to_string()));
        assert_eq!(typed(35, "007"), stored("007"));
        assert_eq!(typed(rep(30, 2), "7"), stored("7"));
        assert_eq!(
            typed(rep(30, 2), "abc").map_err(|e| e.0),
            Err(Some(error::NUMBER))
        );
        assert_eq!(typed(43, "1/2/26"), stored("1/2/2026"));
        assert_eq!(typed(44, " 10:30 "), stored("10:30"));
        assert_eq!(typed(45, "1/2/26 13:30"), stored("1/2/2026 1:30 PM"));
        let refusal = |code, kind: &str, field: u64| {
            Err((
                Some(code),
                format!("Field Field{field} requires a valid {kind}."),
            ))
        };
        assert_eq!(typed(43, "abc"), refusal(error::DATE, "date", 43));
        assert_eq!(typed(44, "abc"), refusal(error::TIME, "time", 44));
        assert_eq!(
            typed(45, "1/2/2026"),
            refusal(error::DATE, "date and time", 45)
        );
        // A four-digit-year rule reads the year as typed, before entry
        // completes it; time of day is checked on the stored text.
        assert_eq!(typed(32, "1/2/26").map_err(|e| e.0), Err(Some(error::DATE)));
        assert_eq!(typed(32, "1/2/2026"), stored("1/2/2026"));
        assert_eq!(
            typed(33, "1/2/26 10:00").map_err(|e| e.0),
            Err(Some(error::DATE))
        );
        assert_eq!(typed(34, "25:00").map_err(|e| e.0), Err(Some(error::TIME)));
        assert_eq!(
            compute(Entry::default(), &[(35, "11")]),
            refused(error::RANGE)
        );

        // A value list the planner cannot read keeps the table read-only.
        let mut missing = table.clone();
        missing.fields.push(with(42, DataType::Text, 0x100, 1, 0));
        assert!(plan_of(&missing, &base_formulas(), &[]).is_err());
    }

    #[test]
    fn auto_entered_constants_fill_only_new_records() {
        let mut table = table();
        table
            .fields
            .push(field(23, FieldKind::Normal, DataType::Text, 0, 0x4, 0));
        let leaves = vec![(23, 0x0d, om_scsu::encode_scsu("Lead"))];
        let plan = plan_of(&table, &base_formulas(), &leaves).unwrap();
        assert!(plan.editable_fields().contains("Field23"));
        let created = create(&plan, &[(1, "Ada")]).unwrap();
        assert_eq!(decoded(&created, 23).as_deref(), Some("Lead"));
        let mut existing = existing();
        existing.insert(23, "Customer".into());
        let updated = plan
            .prepare(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(1, Some("Zed".into()))]),
                &environment(),
            )
            .unwrap();
        assert!(!updated.values.contains_key(&23));
    }

    /// The two lookups FileMaker Pro 26.0.3 saved and ran (auto-entry.md,
    /// Lookups; schema-path.md 5.5): A::k = B::k, copying B::x, "do not
    /// copy" without a match, "don't copy contents if empty" off (field 2)
    /// and on (field 3). Any padding in the definition's key buffers reads
    /// the same; other option bytes or a broken buffer keep the table
    /// read-only, and a write that fires a lookup it did not read fails.
    #[test]
    fn local_auto_entry_reads_a_global_from_an_unrelated_table_without_records() {
        use crate::host::FieldDef;

        let table = Table {
            id: 0x8002,
            name: "Requests".into(),
            fields: vec![field(8, FieldKind::Normal, DataType::Text, 0, 0x8, 0)],
        };
        let mut file = CalcSchema::new("Example", Locale::en_us());
        file.add_table(0x8003, "Settings");
        let mut marker = FieldDef::data(1, "Marker", ValueType::Text);
        marker.global = true;
        file.add_field(0x8003, marker);
        file.add_occurrence(0xd0_0002, "Requests", table.id);
        file.add_occurrence(0xd0_0003, "Settings", 0x8003);

        let formula =
            om_formula::polish::encode(&Expr::Field(om_formula::Field::new(0xd0_0003, 1, true)));
        let mut tree = KeyTree::default();
        let prefix = [
            KeySegment(&[0x80, 2]),
            KeySegment(&[3]),
            KeySegment(&[5]),
            KeySegment(&[8]),
            KeySegment(&[5]),
        ];
        for (child, bytes) in [(&[4][..], CONTEXT), (&[6][..], formula.as_slice())] {
            let mut path = prefix.to_vec();
            path.push(KeySegment(child));
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(bytes))
                .unwrap();
        }
        let plan = RecordPlan::from_desktop_file_schema(&table, &tree, &file, true).unwrap();
        let mut source = MemorySource::new();
        let mut globals = RecordValues::default();
        globals.set(1, 1, Value::text("alpha"));
        source.set_globals(0x8003, globals);
        let existing = BTreeMap::new();
        let changes = BTreeMap::new();
        let mut environment = environment();
        assert_eq!(
            plan.prepare(WriteMode::Create, &existing, &changes, &environment)
                .err()
                .unwrap()
                .code,
            Some(error::UNAVAILABLE)
        );
        environment.lookups = plan
            .read_lookups(
                WriteMode::Create,
                &existing,
                &changes,
                Entry::default(),
                &source,
            )
            .unwrap();
        let created = plan
            .prepare(WriteMode::Create, &existing, &changes, &environment)
            .unwrap();
        assert_eq!(decoded(&created, 8).as_deref(), Some("alpha"));
        assert!(RecordPlan::from_desktop_file_schema(&table, &tree, &file, false).is_err());
    }

    #[test]
    fn exact_lookups_copy_the_first_related_value_as_filemaker_pro_did() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        const OFF: &[u8] = &[
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x00, 0x02, 0x01, 0x02, 0x00, 0x11, 0x01,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x00,
        ];
        let table = Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x400, 0),
                field(3, FieldKind::Normal, DataType::Text, 0, 0x400, 0),
            ],
        };
        let mut file = CalcSchema::new("Lookups", Locale::en_us());
        file.add_table(0x8007, "B");
        file.add_field(0x8007, FieldDef::data(1, "k", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(2, "x", ValueType::Text));
        file.add_occurrence(0xd0_0006, "A", 0x8006);
        file.add_occurrence(0xd0_0007, "B", 0x8007);
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        ));
        let plan_with_fallback = |off: &[u8], fallback: Option<&str>| {
            let mut on = OFF.to_vec();
            on[1] = 0;
            let mut leaves = vec![(2, 8, off)];
            let stored = fallback.map(om_scsu::encode_scsu);
            if let Some(stored) = &stored {
                leaves.push((2, FIELD_LOOKUP_FALLBACK, stored.as_slice()));
            }
            leaves.extend([(2, 0x26, &[0; 4][..]), (3, 8, &on[..])]);
            let paths: Vec<Vec<Vec<u8>>> = leaves
                .iter()
                .map(|(field, key, _)| {
                    [0x8006, 3, 5, *field, *key]
                        .iter()
                        .map(|k| om_hbam::slot::uint_segment(*k))
                        .collect()
                })
                .collect();
            let mut tree = KeyTree::default();
            for (path, (_, _, bytes)) in paths.iter().zip(leaves) {
                let path: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
                tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(bytes))
                    .unwrap();
            }
            RecordPlan::from_file_schema(&table, &tree, &file)
        };
        let plan_for = |off: &[u8]| plan_with_fallback(off, None);
        // Assets' form, with this test's keys: FileMaker Pro's dialog shows
        // its settings as ON's (#549), and it reads as the same lookup.
        let assets = [
            0x00, 0x00, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x7f, 0x02, 0x01, 0x02, 0xbe, 0x33, 0x11,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x7f,
        ];
        let mut on = OFF.to_vec();
        on[1] = 0;
        assert_eq!(
            format!("{:?}", exact_lookup(&assets, None).unwrap()),
            format!("{:?}", exact_lookup(&on, None).unwrap())
        );
        assert!(plan_for(&assets)
            .unwrap()
            .editable_fields()
            .contains("Field2"));
        // The padding after each key is read whatever it holds: the bytes
        // seen after the looked-up occurrence, after the copied field and at
        // the end, in the engine's runs (schema-path.md 5.5) and in the 29
        // lookups of a solution FileMaker Server hosts (#993), mixed with
        // each other and with Assets' form.
        let after_occurrence = [0x00, 0x7f, 0x5e, 0x5f, 0x5a, 0xff, 0x24];
        let after_field: [[u8; 4]; 9] = [
            [0x00, 0x11, 0x01, 0x00],
            [0xbe, 0x33, 0x11, 0x00],
            [0x00, 0x00, 0x00, 0x00],
            [0x00, 0x02, 0x00, 0x00],
            [0x68, 0x76, 0x68, 0x00],
            [0x01, 0x04, 0x00, 0x00],
            [0xff, 0x80, 0x7e, 0x00],
            [0xff, 0xd0, 0x15, 0x00],
            [0x00, 0x02, 0x02, 0x00],
        ];
        let last = [0x00, 0x7f, 0x01, 0x20, 0x40];
        for (i, padding) in after_field.iter().enumerate() {
            let mut padded = OFF.to_vec();
            padded[7] = after_occurrence[i % after_occurrence.len()];
            padded[11..15].copy_from_slice(padding);
            padded[21] = last[i % last.len()];
            assert_eq!(
                format!("{:?}", exact_lookup(&padded, None)),
                format!("{:?}", exact_lookup(OFF, None)),
                "{padded:02x?}"
            );
        }
        // One of that solution's definitions as it is stored.
        let hosted = [
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x03, 0x00, 0x02, 0x01, 0x17, 0x00, 0x02, 0x00,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x01, 0x00,
        ];
        let read = exact_lookup(&hosted, None).unwrap();
        assert_eq!(
            (read.from, read.to, read.copied, read.copy_empty),
            (0xd0_0001, 0xd0_0003, 0x17, true)
        );
        // The option bytes are not padding: "copy next lower value", a
        // "don't copy contents if empty" byte of neither form and the byte
        // before the starting occurrence stay refused, as do a key longer
        // than its buffer, a key not minimally encoded, a definition cut
        // short and one with a byte more.
        let changed = |at: usize, byte: u8| {
            let mut bytes = OFF.to_vec();
            bytes[at] = byte;
            bytes
        };
        let mut longer = OFF.to_vec();
        longer.push(0);
        for unsupported in [
            changed(0, 1),
            changed(1, 2),
            changed(15, 0),
            changed(2, 6),
            changed(8, 3),
            changed(9, 0),
            OFF[..21].to_vec(),
            longer,
        ] {
            assert!(
                exact_lookup(&unsupported, None).is_none(),
                "{unsupported:02x?}"
            );
            let refused = plan_for(&unsupported).err().unwrap();
            assert!(refused.to_string().contains("unsupported lookup options"));
        }
        let plan = plan_for(OFF).unwrap();
        assert!(plan.editable_fields().contains("Field2"));
        // What Options for Field writes for this lookup is FileMaker Pro's
        // own form, which these rules perform (schema_draft).
        let written = crate::field_options::Lookup {
            from: 0xd0_0006,
            to: 0xd0_0007,
            copied: 2,
            copy_empty: true,
            no_match: crate::field_options::NoMatch::DoNotCopy,
        }
        .encode()
        .unwrap();
        assert_eq!(written, OFF);

        let mut b = MemorySource::new();
        for (id, k, x) in [(1, "K_MATCH", "COPIED_VALUE"), (2, "K_EMPTY", "")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(k));
            values.set(2, 1, Value::text(x));
            b.put_record(0x8007, id, values);
        }
        let write = |plan: &RecordPlan, mode, existing: &[(u64, &str)], key: Option<&str>| {
            let existing = existing.iter().map(|(f, v)| (*f, v.to_string())).collect();
            let changes = key
                .into_iter()
                .map(|key| (1, (!key.is_empty()).then(|| key.to_owned())))
                .collect();
            let mut environment = environment();
            environment.lookups = plan
                .read_lookups(mode, &existing, &changes, Entry::default(), &b)
                .unwrap();
            let computed = plan
                .compute(mode, &existing, &changes, &environment, Entry::default())
                .unwrap();
            (computed.get(&2).cloned(), computed.get(&3).cloned())
        };
        let copied = Some(Some("COPIED_VALUE".to_string()));
        assert_eq!(
            write(&plan, WriteMode::Create, &[], Some("K_MATCH")),
            (copied.clone(), copied)
        );
        let manual = [(1, "K_MATCH"), (2, "MANUAL_OFF"), (3, "MANUAL_ON")];
        assert_eq!(
            write(&plan, WriteMode::Update, &manual, Some("K_NO_MATCH")),
            (None, None)
        );
        let manual = [(1, "K_NO_MATCH"), (2, "MANUAL_OFF"), (3, "MANUAL_ON")];
        assert_eq!(
            write(&plan, WriteMode::Update, &manual, Some("K_EMPTY")),
            (Some(None), None)
        );

        let with_fallback = plan_with_fallback(&changed(0, 3), Some("Unknown")).unwrap();
        assert_eq!(
            write(
                &with_fallback,
                WriteMode::Update,
                &manual,
                Some("K_MISSING")
            ),
            (Some(Some("Unknown".into())), None)
        );
        assert_eq!(
            write(&with_fallback, WriteMode::Update, &manual, Some("K_EMPTY")),
            (Some(None), None)
        );

        assert_eq!(
            write(&with_fallback, WriteMode::Create, &[], None),
            (None, None)
        );
        assert_eq!(
            write(&with_fallback, WriteMode::Update, &manual, Some("")),
            (Some(Some("Unknown".into())), None)
        );
        let mut guard = changed(0, 3);
        guard[1] = 0;
        let guarded = plan_with_fallback(&guard, Some("Unknown")).unwrap();
        assert_eq!(
            write(&guarded, WriteMode::Update, &manual, Some("K_EMPTY")),
            (None, None)
        );
        let blank = plan_for(&changed(0, 3)).unwrap();
        assert_eq!(
            write(&blank, WriteMode::Update, &manual, Some("K_MISSING")),
            (Some(None), None)
        );

        let unread = plan.compute(
            WriteMode::Create,
            &BTreeMap::new(),
            &BTreeMap::from([(1, Some("K_MATCH".into()))]),
            &environment(),
            Entry::default(),
        );
        assert_eq!(unread.err().unwrap().code, Some(error::UNAVAILABLE));

        // A script stopped while its lookup reads the related table: the
        // read ends at the next record with error 1, before reading the
        // other candidates, and there is nothing to copy.
        struct Stopping {
            inner: MemorySource,
            reads: std::cell::Cell<usize>,
        }
        impl CalcSource for Stopping {
            fn record(
                &self,
                table: u64,
                record: u64,
            ) -> Result<Option<Arc<RecordValues>>, om_calc::CalcError> {
                self.reads.set(self.reads.get() + 1);
                self.inner.record(table, record)
            }
            fn records(&self, table: u64) -> Result<Arc<[u64]>, om_calc::CalcError> {
                self.inner.records(table)
            }
            fn globals(&self, table: u64) -> Result<Arc<RecordValues>, om_calc::CalcError> {
                self.inner.globals(table)
            }
            fn interrupted(&self) -> bool {
                self.reads.get() >= 3
            }
        }
        let mut many = MemorySource::new();
        for id in 1..=1000 {
            let mut values = RecordValues::default();
            let k = if id == 1000 {
                "K_MATCH".into()
            } else {
                format!("K{id}")
            };
            values.set(1, 1, Value::text(&k));
            values.set(2, 1, Value::text("COPIED_VALUE"));
            many.put_record(0x8007, id, values);
        }
        let stopping = Stopping {
            inner: many,
            reads: std::cell::Cell::new(0),
        };
        let stopped = plan.read_lookups(
            WriteMode::Create,
            &BTreeMap::new(),
            &BTreeMap::from([(1, Some("K_MATCH".into()))]),
            Entry::default(),
            &stopping,
        );
        assert_eq!(stopped.err().unwrap().code, Some(1));
        assert_eq!(stopping.reads.get(), 3);
    }

    /// A lookup from a parent table keyed on its auto-entered
    /// `Get ( UUID )` (auto-entry.md, Lookups, battery s41): the table
    /// stays writable; a new record copies nothing, as FileMaker's lookup on
    /// a key nothing holds; a write that sets the key copies what the first
    /// related record holds, also when the key keeps its value. With
    /// another auto-entry on the key the table stays read-only.
    #[test]
    fn parent_lookups_are_performed_when_their_key_is_set() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        const LOOKUP: &[u8] = &[
            0x00, 0x00, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x00, 0x02, 0x01, 0x02, 0x00, 0x11, 0x01,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x00,
        ];
        let table = |key_options: u32, variable| Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(
                    1,
                    FieldKind::Normal,
                    DataType::Text,
                    variable,
                    key_options,
                    0x1c,
                ),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x400, 0),
            ],
        };
        let mut file = CalcSchema::new("Lookups", Locale::en_us());
        file.add_table(0x8007, "B");
        file.add_field(0x8007, FieldDef::data(1, "k", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(2, "x", ValueType::Text));
        file.add_occurrence(0xd0_0006, "A", 0x8006);
        file.add_occurrence(0xd0_0007, "B", 0x8007);
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        ));
        let plan_for = |key_options: u32, variable, key_code: &[u8], lookup: &[u8]| {
            let leaves: [(u64, &[u64], &[u8]); 3] = [
                (1, &[5, 4], &[3, 0xd0, 0, 6]),
                (1, &[5, 6], key_code),
                (2, &[8], lookup),
            ];
            let paths: Vec<Vec<Vec<u8>>> = leaves
                .iter()
                .map(|(field, keys, _)| {
                    [0x8006, 3, 5, *field]
                        .iter()
                        .chain(keys.iter())
                        .map(|k| om_hbam::slot::uint_segment(*k))
                        .collect()
                })
                .collect();
            let mut tree = KeyTree::default();
            for (path, (_, _, bytes)) in paths.iter().zip(leaves) {
                let path: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
                tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(bytes))
                    .unwrap();
            }
            RecordPlan::from_file_schema(&table(key_options, variable), &tree, &file)
        };
        // A starter's PrimaryKey: Get ( UUID ), replacing, prohibiting
        // modification, indexed.
        let plan = plan_for(0x8000_0188, 0, &[0x9b, 0x65], LOOKUP).unwrap();
        assert!(plan.editable_fields().contains("Field2"));
        let mut fallback = LOOKUP.to_vec();
        fallback[0] = 3;
        let refused_fallback = plan_for(0x8000_0188, 0, &[0x9b, 0x65], &fallback).err();
        assert_eq!(
            refused_fallback.and_then(|e| e.code),
            Some(error::UNAVAILABLE)
        );
        // Relookup Field Contents' target: not this key, whose auto-entry
        // a write of it could replace (3); not the looked-up field, which
        // no lookup matches through (3); not a field the table lacks (102).
        let refused = |name: &str| plan.relookup_target(name).unwrap_err();
        let key = refused("Field1");
        assert_eq!(key.code, Some(error::UNAVAILABLE));
        assert!(key.message.contains("enters a value"), "{}", key.message);
        let looked_up = refused("Field2");
        assert!(
            looked_up.message.contains("No lookup"),
            "{}",
            looked_up.message
        );
        assert_eq!(refused("Missing").code, Some(error::FIELD_MISSING));
        // Another auto-entry on the key (the creator's account name) keeps
        // the table read-only.
        let other = plan_for(0x8000_0101, 4, &[0x9b, 0x65], LOOKUP);
        assert!(other
            .err()
            .is_some_and(|e| e.message.contains("stored Text match fields")));

        // The key is validated as a generated UUID (509 otherwise).
        const K0: &str = "0F3C1D2E-5A6B-4C7D-8E9F-A0B1C2D3E4F5";
        // Not the environment's new UUID (`UUID`).
        const K1: &str = "1A2B3C4D-5E6F-4A7B-8C9D-0E1F2A3B4C5D";
        let mut b = MemorySource::new();
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text(K1));
        values.set(2, 1, Value::text("555"));
        b.put_record(0x8007, 1, values);
        let write_as = |entry: Entry, mode, existing: &[(u64, &str)], changes: &[(u64, &str)]| {
            let existing = existing.iter().map(|(f, v)| (*f, v.to_string())).collect();
            let changes = changes
                .iter()
                .map(|(f, v)| (*f, Some(v.to_string())))
                .collect();
            let mut environment = environment();
            environment.lookups = plan
                .read_lookups(mode, &existing, &changes, entry, &b)
                .unwrap();
            plan.compute(mode, &existing, &changes, &environment, entry)
                .unwrap()
        };
        // Set Field in a script, as s41 sets the key, which prohibits
        // modification during data entry.
        let write = |mode, existing: &[(u64, &str)], changes: &[(u64, &str)]| {
            write_as(Entry::script(false, false), mode, existing, changes)
        };
        // A new record: its key is a new UUID, the lookup copies nothing.
        let made = write(WriteMode::Create, &[], &[]);
        assert!(made
            .get(&1)
            .is_some_and(|v| v.as_ref().is_some_and(|k| k.len() == 36)));
        assert_eq!(made.get(&2), None);
        // Its key set, to a related record's or to the value it had.
        let set = write(WriteMode::Update, &[(1, K0), (2, "")], &[(1, K1)]);
        assert_eq!(set.get(&2), Some(&Some("555".to_string())));
        let same = write(WriteMode::Update, &[(1, K1), (2, "typed")], &[(1, K1)]);
        assert_eq!(same.get(&2), Some(&Some("555".to_string())));
        // A new record given the key of another, as a duplicate is: the
        // replacing auto-entry gives it a new UUID, so the lookup copies
        // nothing; an import keeps the key it writes, and copies.
        let duplicate = write(WriteMode::Create, &[], &[(1, K1)]);
        assert!(duplicate.get(&1).is_some_and(|k| k.as_deref() != Some(K1)));
        assert_eq!(duplicate.get(&2), None);
        let imported = write_as(Entry::import(true), WriteMode::Create, &[], &[(1, K1)]);
        assert_eq!(imported.get(&2), Some(&Some("555".to_string())));
    }

    /// A lookup through a sorted relationship copies the first related
    /// record in the relationship's sort order (auto-entry.md, Lookups,
    /// battery s42): sorted by `d` descending, the record made last;
    /// ascending, the one made first. Sorted by an unstored calculation,
    /// which `host::sort` orders as empty, or by text in Japanese, which it
    /// orders otherwise than FileMaker in places, the table stays read-only.
    #[test]
    fn a_lookup_uses_the_auto_entered_key_when_it_changes() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};

        const FROM: u64 = 0xd0_0006;
        const TO: u64 = 0xd0_0007;
        let mut file = CalcSchema::new("Lookups", Locale::en_us());
        file.add_table(0x8007, "Directory");
        file.add_field(0x8007, FieldDef::data(1, "Key", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(2, "Label", ValueType::Text));
        file.add_occurrence(FROM, "Requests", 0x8006);
        file.add_occurrence(TO, "Directory", 0x8007);
        file.add_relationship(RelationshipDef::new(
            4,
            FROM,
            TO,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 2,
                right: 1,
            }],
        ));
        let mut directory = MemorySource::new();
        for (id, key, label) in [(1, "A", "First"), (2, "B", "Second")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(key));
            values.set(2, 1, Value::text(label));
            directory.put_record(0x8007, id, values);
        }

        let mut names = om_formula::NameTable::default();
        names.add_occurrence(FROM, "Requests", 0x8006);
        names.add_field(0x8006, 1, "Field1");
        let mut compiler = om_formula::Context::new(&names);
        compiler.occurrence = Some(FROM);
        let plan_for = |formula, options| {
            let table = Table {
                id: 0x8006,
                name: "Requests".into(),
                fields: vec![
                    field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                    field(2, FieldKind::Normal, DataType::Text, 0, options, 0),
                    field(3, FieldKind::Normal, DataType::Text, 0, LOOKUP, 0),
                ],
            };
            let formula = om_formula::compile(formula, &compiler).unwrap().expr;
            let encoded = om_formula::polish::encode(&formula);
            let context = om_formula::Key::from_id(FROM);
            let definition = crate::field_options::Lookup {
                from: FROM,
                to: TO,
                copied: 2,
                copy_empty: true,
                no_match: crate::field_options::NoMatch::DoNotCopy,
            }
            .encode()
            .unwrap();
            let leaves: [(&[u64], &[u8]); 3] = [
                (&[2, FIELD_FORMULA, 4], &context.0),
                (&[2, FIELD_FORMULA, 6], &encoded),
                (&[3, FIELD_LOOKUP], &definition),
            ];
            let paths: Vec<Vec<Vec<u8>>> = leaves
                .iter()
                .map(|(suffix, _)| {
                    [table.id, 3, 5]
                        .into_iter()
                        .chain(suffix.iter().copied())
                        .map(om_hbam::slot::uint_segment)
                        .collect()
                })
                .collect();
            let mut tree = KeyTree::default();
            for (keys, (_, value)) in paths.iter().zip(leaves) {
                let path: Vec<KeySegment<'_>> = keys.iter().map(|key| KeySegment(key)).collect();
                tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(value))
                    .unwrap();
            }
            RecordPlan::from_file_schema(&table, &tree, &file).unwrap()
        };

        let write = |plan: &RecordPlan, mode, existing: &[(u64, &str)], input: Option<&str>| {
            let existing = existing
                .iter()
                .map(|(id, text)| (*id, (*text).into()))
                .collect();
            let changes = input
                .into_iter()
                .map(|text| (1, Some(text.into())))
                .collect();
            let mut environment = environment();
            let needs = Arc::new(RelatedNeeds::default());
            environment.related_needs = Some(Arc::clone(&needs));
            let entry = Entry::script(false, false);
            let mut keyed = BTreeMap::new();
            for pass in 0..2 {
                environment.lookups = plan
                    .read_lookups_with(
                        mode,
                        &existing,
                        &changes,
                        entry,
                        &directory,
                        (&keyed, Written::Unknown),
                    )
                    .unwrap();
                let result = plan.compute(mode, &existing, &changes, &environment, entry);
                keyed = needs.take();
                if keyed.is_empty() {
                    return result.unwrap();
                }
                assert_eq!(pass, 0, "lookup reread must use the final key");
                assert_eq!(result.unwrap_err().code, Some(error::UNAVAILABLE));
            }
            unreachable!("each write computes at most one final key");
        };

        let from_constant = plan_for(r#""A""#, 0x8);
        let created = write(&from_constant, WriteMode::Create, &[], None);
        assert_eq!(created.get(&2), Some(&Some("A".into())));
        assert_eq!(created.get(&3), Some(&Some("First".into())));

        let replace = plan_for("Field1", 0x88);
        assert!(write(&replace, WriteMode::Create, &[], None).is_empty());
        let entered = write(&replace, WriteMode::Update, &[], Some("A"));
        assert_eq!(entered.get(&2), Some(&Some("A".into())));
        assert_eq!(entered.get(&3), Some(&Some("First".into())));

        let existing = [(1, "A"), (2, "A"), (3, "Prior")];
        let replaced = write(&replace, WriteMode::Update, &existing, Some("B"));
        assert_eq!(replaced.get(&2), Some(&Some("B".into())));
        assert_eq!(replaced.get(&3), Some(&Some("Second".into())));
        let keep = plan_for("Field1", 0x8);
        let kept = write(&keep, WriteMode::Update, &existing, Some("B"));
        assert_eq!(kept, BTreeMap::from([(1, Some("B".into()))]));
        for plan in [&replace, &keep] {
            assert_eq!(
                plan.relookup_target("Field1").unwrap_err().code,
                Some(error::UNAVAILABLE)
            );
        }
    }

    #[test]
    fn sorted_lookups_copy_the_first_record_in_the_sort() {
        use crate::host::{FieldClass, FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        const LOOKUP: &[u8] = &[
            0x00, 0x00, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x00, 0x02, 0x01, 0x02, 0x00, 0x11, 0x01,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x00,
        ];
        let table = Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x400, 0),
            ],
        };
        let path: Vec<Vec<u8>> = [0x8006, 3, 5, 2, 8]
            .iter()
            .map(|k| om_hbam::slot::uint_segment(*k))
            .collect();
        let path: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
        let mut tree = KeyTree::default();
        tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(LOOKUP))
            .unwrap();
        let mut b = MemorySource::new();
        for (record, x, d) in [(1, "first", "2020"), (2, "second", "2099")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text("K"));
            values.set(2, 1, Value::text(x));
            values.set(3, 1, Value::text(d));
            b.put_record(0x8007, record, values);
        }
        let plan_for = |sorted_by, descending, locale| {
            let mut file = CalcSchema::new("Lookups", Locale::en_us());
            file.add_table(0x8007, "B");
            file.add_field(0x8007, FieldDef::data(1, "k", ValueType::Text));
            file.add_field(0x8007, FieldDef::data(2, "x", ValueType::Text));
            file.add_field(0x8007, FieldDef::data(3, "d", ValueType::Text));
            file.add_field(
                0x8007,
                FieldDef {
                    class: FieldClass::Calculation,
                    stored: false,
                    ..FieldDef::data(4, "u", ValueType::Text)
                },
            );
            file.add_occurrence(0xd0_0006, "A", 0x8006);
            file.add_occurrence(0xd0_0007, "B", 0x8007);
            let sort = SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: vec![SortEntry {
                    field: SpecField {
                        occurrence: Some(0xd0_0007),
                        field: sorted_by,
                        repetition: 1,
                    },
                    summary: None,
                    value_list: None,
                    descending,
                    locale,
                }],
            };
            file.add_relationship(RelationshipDef {
                sort_right: Some(sort),
                options: 0x40,
                ..RelationshipDef::new(
                    4,
                    0xd0_0006,
                    0xd0_0007,
                    vec![JoinPredicate {
                        op: JoinOp::Equal,
                        left: 1,
                        right: 1,
                    }],
                )
            });
            RecordPlan::from_file_schema(&table, &tree, &file)
        };
        let copied = |descending| {
            let plan = plan_for(3, descending, 0).unwrap();
            let existing = BTreeMap::from([(1, String::new()), (2, String::new())]);
            let changes = BTreeMap::from([(1, Some("K".to_string()))]);
            let mut environment = environment();
            environment.lookups = plan
                .read_lookups(WriteMode::Update, &existing, &changes, Entry::default(), &b)
                .unwrap();
            plan.compute(
                WriteMode::Update,
                &existing,
                &changes,
                &environment,
                Entry::default(),
            )
            .unwrap()
            .remove(&2)
            .flatten()
        };
        assert_eq!(copied(true).as_deref(), Some("second"));
        assert_eq!(copied(false).as_deref(), Some("first"));
        for (sorted_by, locale) in [(4, 0), (3, 31)] {
            assert!(plan_for(sorted_by, true, locale).err().is_some_and(|e| e
                .message
                .contains("sorted by a field OpenMaker cannot order by")));
        }
    }

    /// A self-join is answered only where whether the written record is
    /// among its own related records, and with which values, cannot change
    /// the result, or as measured (#1035, s47). T has two stored records, 1 (job 1,
    /// doc `a`, n 1, top 5) and 2 (a `x`, b `p`, n 2); S relates T on `[job,
    /// doc]`, S2 on `a = b`.
    /// - `S::n` in a new record of job 1, doc `a`: record 1 came first, 1.
    ///   In a new job it would be the record itself or nothing: refused, as
    ///   for a record the write does not name.
    /// - `Max ( S::top ) + 1`, not replacing a value: the record's own `top`
    ///   is empty whenever it is computed, so its membership adds nothing: 6
    ///   after record 1, 1 in a new job.
    /// - `Count ( S::n )` in a new record, replacing an existing value: 1,
    ///   record 1 alone, whether or not its own `n` is set: a new record
    ///   reads the older matches, never itself (s47). Keeping an existing
    ///   value, refused while its own `n` is set.
    /// - `S2::n` as record 2 changes `a` to `p` and `b` to `q`: no longer
    ///   related to itself as written, but as stored (`b` = `p`) it is:
    ///   refused. Changing `a` to `z` instead, related neither way: empty.
    /// - `S::n` in a new record of job 1, doc `c`, which record 5 has: 5,
    ///   unless the record was reserved as 3, before record 5 was made.
    #[test]
    fn a_self_join_is_answered_where_the_record_cannot_change_it() {
        use crate::host::{JoinOp, JoinPredicate, RelationshipDef};
        let mut source = MemorySource::new();
        for (record, cells) in [
            (1, vec![(1, "1"), (2, "a"), (3, "1"), (5, "5")]),
            (2, vec![(7, "x"), (8, "p"), (3, "2")]),
            (5, vec![(1, "1"), (2, "c"), (3, "5")]),
        ] {
            let mut values = RecordValues::default();
            for (field, text) in cells {
                values.set(field, 1, Value::text(text));
            }
            source.put_record(0x8006, record, values);
        }
        let plain = [
            (1, DataType::Number, "job"),
            (2, DataType::Text, "doc"),
            (3, DataType::Number, "n"),
            (5, DataType::Number, "top"),
            (7, DataType::Text, "a"),
            (8, DataType::Text, "b"),
        ];
        let mut names = om_formula::NameTable::default();
        for (occurrence, name) in [(0xd0_0006, "T"), (0xd0_0007, "S"), (0xd0_0008, "S2")] {
            names.add_occurrence(occurrence, name, 0x8006);
        }
        for (id, _, name) in plain {
            names.add_field(0x8006, id, name);
        }
        names.add_field(0x8006, 9, "seen");
        let mut file = CalcSchema::new("Self", Locale::en_us());
        file.add_occurrence(0xd0_0006, "T", 0x8006);
        file.add_occurrence(0xd0_0007, "S", 0x8006);
        file.add_occurrence(0xd0_0008, "S2", 0x8006);
        let equal = |left, right| JoinPredicate {
            op: JoinOp::Equal,
            left,
            right,
        };
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![equal(1, 1), equal(2, 2)],
        ));
        file.add_relationship(RelationshipDef::new(
            5,
            0xd0_0006,
            0xd0_0008,
            vec![equal(7, 8)],
        ));
        // A plan whose field `id` auto-enters `formula` (`options`), and the
        // field's value as `written` computes `changes` over `existing`.
        let write = |(id, options, formula): (u64, u32, &str),
                     (mode, written): (WriteMode, Written),
                     existing: &[(u64, &str)],
                     changes: &[(u64, &str)]| {
            let mut fields: Vec<_> = plain
                .iter()
                .map(|(id, kind, _)| field(*id, FieldKind::Normal, *kind, 0, 0, 0))
                .collect();
            fields.push(field(
                id,
                FieldKind::Normal,
                DataType::Number,
                0,
                options,
                0,
            ));
            let table = Table {
                id: 0x8006,
                name: "T".into(),
                fields,
            };
            let mut cx = om_formula::Context::new(&names);
            cx.occurrence = Some(0xd0_0006);
            let expr = om_formula::compile(formula, &cx).unwrap().expr;
            let entries: Vec<(Vec<Vec<u8>>, Vec<u8>)> = [
                (4, om_formula::Key::from_id(0xd0_0006).0),
                (6, om_formula::polish::encode(&expr)),
            ]
            .into_iter()
            .map(|(key, value)| {
                let path = [0x8006, 3, 5, id, 5, key]
                    .iter()
                    .map(|k| om_hbam::slot::uint_segment(*k))
                    .collect();
                (path, value)
            })
            .collect();
            let mut tree = KeyTree::default();
            for (path, value) in &entries {
                let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
                tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                    .unwrap();
            }
            let plan = RecordPlan::from_file_schema(&table, &tree, &file).unwrap();
            let existing: BTreeMap<u64, String> =
                existing.iter().map(|(f, t)| (*f, t.to_string())).collect();
            let changes: BTreeMap<u64, Option<String>> = changes
                .iter()
                .map(|(f, t)| (*f, Some(t.to_string())))
                .collect();
            let mut environment = environment();
            environment.lookups = plan
                .read_lookups_with(
                    mode,
                    &existing,
                    &changes,
                    Entry::default(),
                    &source,
                    (&BTreeMap::new(), written),
                )
                .unwrap();
            plan.prepare(mode, &existing, &changes, &environment)
                .map(|prepared| decoded(&prepared, id))
                .map_err(|e| e.message)
        };
        let new = (WriteMode::Create, Written::New(None));
        let unknown = (WriteMode::Create, Written::Unknown);
        let first = (9, 0x8, "S::n");
        let earlier = &[(1, "1"), (2, "a"), (3, "9")][..];
        let new_job = &[(1, "2"), (2, "b"), (3, "9")][..];
        assert_eq!(write(first, new, &[], earlier), Ok(Some("1".into())));
        // A new group's first record, no other related and its keys given:
        // read as empty, as measured (#1035's native cases).
        assert_eq!(write(first, new, &[], new_job), Ok(None));
        let refused = write(first, unknown, &[], earlier).unwrap_err();
        assert!(refused.contains("its own related records"), "{refused}");
        let top = (9, 0x8, "Max ( S::top ) + 1");
        assert_eq!(write(top, new, &[], earlier), Ok(Some("6".into())));
        assert_eq!(write(top, new, &[], new_job), Ok(Some("1".into())));
        let count = (9, 0x88, "Count ( S::n )");
        assert_eq!(write(count, new, &[], earlier), Ok(Some("1".into())));
        // Keeping an existing value, it was entered before the keys: not
        // answered with the older matches (s47 measured a replacing one).
        let kept = (9, 0x8, "Count ( S::n )");
        assert!(write(kept, new, &[], earlier).is_err());
        let empty_n = &[(1, "1"), (2, "a")][..];
        assert_eq!(write(count, new, &[], empty_n), Ok(Some("1".into())));
        let back = (9, 0x88, "S2::n");
        let stored = &[(7, "x"), (8, "p"), (3, "2")][..];
        let record_2 = (WriteMode::Update, Written::Stored(2));
        let refused = write(back, record_2, stored, &[(7, "p"), (8, "q")]).unwrap_err();
        assert!(refused.contains("its own related records"), "{refused}");
        let unrelated = &[(7, "z"), (8, "q")][..];
        assert_eq!(write(back, record_2, stored, unrelated), Ok(None));
        let refused = write(
            back,
            (WriteMode::Update, Written::Unknown),
            stored,
            unrelated,
        );
        assert!(refused.is_err());
        let doc_c = &[(1, "1"), (2, "c"), (3, "9")][..];
        assert_eq!(write(first, new, &[], doc_c), Ok(Some("5".into())));
        let reserved = (WriteMode::Create, Written::New(Some(3)));
        assert!(write(first, reserved, &[], doc_c).is_err());
    }

    /// A self-join the record cannot satisfy itself reads the other records
    /// (#1035): `seen` counts and lists G through `job = job` and
    /// `lead = num`, both keys computed (`lead` from `pick`, `num` from `own`)
    /// before it. A later record of job 1 whose final `lead` 1 differs from
    /// its own `num` 2 reads record 1 alone, whether or not a record is among
    /// its own related records. With equal final keys, new-row auto-entry
    /// also reads the one older match (C126), unless its context is unknown
    /// or another row of the table is held.
    #[test]
    fn a_self_join_its_record_cannot_satisfy_reads_the_others() {
        use crate::host::{JoinOp, JoinPredicate, RelationshipDef};
        let table = Table {
            id: 0x8006,
            name: "T".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Number, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Number, 0, 0, 0),
                field(3, FieldKind::Normal, DataType::Number, 0, 0x8, 0),
                field(4, FieldKind::Normal, DataType::Number, 0, 0, 0),
                field(5, FieldKind::Normal, DataType::Number, 0, 0x8, 0),
                field(6, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
                field(7, FieldKind::Normal, DataType::Text, 0, 0, 0),
            ],
        };
        let mut source = MemorySource::new();
        let mut values = RecordValues::default();
        for (field, text) in [(1, "1"), (5, "1"), (7, "first")] {
            values.set(field, 1, Value::text(text));
        }
        source.put_record(0x8006, 1, values);
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0006, "T", 0x8006);
        names.add_occurrence(0xd0_0007, "G", 0x8006);
        for (id, name) in [
            (1, "job"),
            (2, "pick"),
            (3, "lead"),
            (4, "own"),
            (5, "num"),
            (6, "seen"),
            (7, "name"),
        ] {
            names.add_field(0x8006, id, name);
        }
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0006);
        let mut entries: Vec<(Vec<Vec<u8>>, Vec<u8>)> = Vec::new();
        let seen = "Count ( G::job ) & \"|\" & List ( G::name )";
        for (field, source) in [(3, "pick"), (5, "own"), (6, seen)] {
            let expr = om_formula::compile(source, &cx).unwrap().expr;
            for (key, value) in [
                (4, om_formula::Key::from_id(0xd0_0006).0),
                (6, om_formula::polish::encode(&expr)),
            ] {
                let path = [0x8006, 3, 5, field, 5, key]
                    .iter()
                    .map(|k| om_hbam::slot::uint_segment(*k))
                    .collect();
                entries.push((path, value));
            }
        }
        entries.sort();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let mut file = CalcSchema::new("Self", Locale::en_us());
        file.add_occurrence(0xd0_0006, "T", 0x8006);
        file.add_occurrence(0xd0_0007, "G", 0x8006);
        let equal = |left, right| JoinPredicate {
            op: JoinOp::Equal,
            left,
            right,
        };
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![equal(1, 1), equal(3, 5)],
        ));
        let plan = RecordPlan::from_file_schema(&table, &tree, &file).unwrap();
        // As the host runs a write: read again with the key it computes
        // (`lead`), then computed again ([`RelatedNeeds`]).
        let create_with =
            |pick: &str, own: &str, written: Written, held: Option<Arc<HeldRecords>>| {
                let changes: BTreeMap<u64, Option<String>> =
                    [(1, "1"), (2, pick), (4, own), (7, "new")]
                        .into_iter()
                        .map(|(field, text)| (field, Some(text.to_string())))
                        .collect();
                let needs = Arc::new(RelatedNeeds::default());
                let mut keyed = BTreeMap::new();
                loop {
                    let mut environment = environment();
                    environment.held = held.clone();
                    environment.related_needs = Some(Arc::clone(&needs));
                    environment.lookups = plan
                        .read_lookups_with(
                            WriteMode::Create,
                            &BTreeMap::new(),
                            &changes,
                            Entry::default(),
                            &source,
                            (&keyed, written),
                        )
                        .unwrap();
                    let prepared =
                        plan.prepare(WriteMode::Create, &BTreeMap::new(), &changes, &environment);
                    let more = needs.take();
                    if more.is_empty() {
                        return prepared;
                    }
                    keyed.extend(more);
                }
            };
        let create = |pick, own| create_with(pick, own, Written::New(None), None);
        let later = create("1", "2").unwrap();
        assert_eq!(decoded(&later, 6).as_deref(), Some("1|first"));
        // Final computed equal keys, no other match: the native C126
        // witness reads Count 0 and List empty. The matching follow-up
        // sees one older row, not this row's nonempty job/name values.
        let first = create("2", "2").unwrap();
        assert_eq!(decoded(&first, 6).as_deref(), Some("0|"));
        let matching = create("1", "1").unwrap();
        assert_eq!(decoded(&matching, 6).as_deref(), Some("1|first"));
        let mut held = HeldRecords::default();
        held.hold(0x8006, 2, BTreeMap::new());
        for (written, held) in [
            (Written::Unknown, None),
            (Written::New(None), Some(Arc::new(held))),
        ] {
            let refused = create_with("1", "1", written, held).err().unwrap();
            assert_eq!(refused.code, Some(error::UNAVAILABLE), "{refused:?}");
        }
        // A preview of `seen` alone computes `lead` but passes over `num`:
        // not final there, so equal keys are not read as unequal.
        let changes: BTreeMap<u64, Option<String>> = [(1, "1"), (2, "1"), (4, "1")]
            .into_iter()
            .map(|(field, text)| (field, Some(text.to_string())))
            .collect();
        let keyed = BTreeMap::from([((0xd0_0006, 0xd0_0007), vec!["1".into(), "1".into()])]);
        let mut environment = environment();
        environment.lookups = plan
            .read_lookups_with(
                WriteMode::Create,
                &BTreeMap::new(),
                &changes,
                Entry::default(),
                &source,
                (&keyed, Written::New(None)),
            )
            .unwrap();
        let preview = plan
            .preview_entry(
                WriteMode::Create,
                &BTreeMap::new(),
                &changes,
                &environment,
                Entry::default(),
                &BTreeSet::from([6]),
            )
            .unwrap();
        assert!(!preview.contains_key(&6), "{preview:?}");
    }

    /// A match field the write computes itself (#1035). A's `k` auto-enters
    /// `Upper ( code )` and `seen` auto-enters `If ( code = "skip" ; C::x ;
    /// B::x )`, through `A::k = B::k` and `A::code = C::k`, two occurrences of
    /// one table. Related records are read before the write is computed, with
    /// `k` empty: `seen`, following B by the computed `ABC`, asks for it
    /// ([`RelatedNeeds`]) and is computed again over that read. A branch that
    /// follows only C, read by its current key, needs nothing, though B's read
    /// is stale. Without a way to ask, following B refuses the write.
    #[test]
    fn a_computed_match_key_is_read_again() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        let table = Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
                field(3, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
            ],
        };
        let mut b = MemorySource::new();
        for (record, k, x) in [(1, "ABC", "found"), (2, "skip", "plain")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(k));
            values.set(2, 1, Value::text(x));
            b.put_record(0x8007, record, values);
        }
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(0xd0_0006, "A", 0x8006);
        names.add_occurrence(0xd0_0007, "B", 0x8007);
        names.add_occurrence(0xd0_0008, "C", 0x8007);
        for (table, id, name) in [
            (0x8006, 1, "code"),
            (0x8006, 2, "k"),
            (0x8006, 3, "seen"),
            (0x8007, 1, "k"),
            (0x8007, 2, "x"),
        ] {
            names.add_field(table, id, name);
        }
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0006);
        let mut entries: Vec<(Vec<Vec<u8>>, Vec<u8>)> = Vec::new();
        for (field, source) in [
            (2, "Upper ( code )"),
            (3, "If ( code = \"skip\" ; C::x ; B::x )"),
        ] {
            let expr = om_formula::compile(source, &cx).unwrap().expr;
            for (key, value) in [
                (4, om_formula::Key::from_id(0xd0_0006).0),
                (6, om_formula::polish::encode(&expr)),
            ] {
                let path = [0x8006, 3, 5, field, 5, key]
                    .iter()
                    .map(|k| om_hbam::slot::uint_segment(*k))
                    .collect();
                entries.push((path, value));
            }
        }
        entries.sort();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let mut file = CalcSchema::new("Keys", Locale::en_us());
        file.add_table(0x8007, "B");
        for (id, name) in [(1, "k"), (2, "x")] {
            file.add_field(0x8007, FieldDef::data(id, name, ValueType::Text));
        }
        file.add_occurrence(0xd0_0006, "A", 0x8006);
        file.add_occurrence(0xd0_0007, "B", 0x8007);
        file.add_occurrence(0xd0_0008, "C", 0x8007);
        for (id, to, left) in [(4, 0xd0_0007, 2), (5, 0xd0_0008, 1)] {
            file.add_relationship(RelationshipDef::new(
                id,
                0xd0_0006,
                to,
                vec![JoinPredicate {
                    op: JoinOp::Equal,
                    left,
                    right: 1,
                }],
            ));
        }
        let plan = RecordPlan::from_file_schema(&table, &tree, &file).unwrap();
        let needs = Arc::new(RelatedNeeds::default());
        let create = |code: &str, keyed: &BTreeMap<(u64, u64), Vec<String>>, asking: bool| {
            let changes = BTreeMap::from([(1, Some(code.to_string()))]);
            let mut environment = environment();
            environment.related_needs = asking.then(|| Arc::clone(&needs));
            environment.lookups = plan
                .read_lookups_with(
                    WriteMode::Create,
                    &BTreeMap::new(),
                    &changes,
                    Entry::default(),
                    &b,
                    (keyed, Written::New(None)),
                )
                .unwrap();
            plan.prepare(WriteMode::Create, &BTreeMap::new(), &changes, &environment)
        };
        let refused = create("abc", &BTreeMap::new(), true).err().unwrap();
        assert_eq!(refused.code, Some(error::UNAVAILABLE));
        let keyed = needs.take();
        assert_eq!(
            keyed,
            BTreeMap::from([((0xd0_0006, 0xd0_0007), vec!["ABC".to_string()])])
        );
        let created = create("abc", &keyed, true).unwrap();
        assert_eq!(decoded(&created, 2).as_deref(), Some("ABC"));
        assert_eq!(decoded(&created, 3).as_deref(), Some("found"));
        assert!(needs.take().is_empty());
        let skipped = create("skip", &BTreeMap::new(), true).unwrap();
        assert_eq!(decoded(&skipped, 3).as_deref(), Some("plain"));
        assert!(needs.take().is_empty());
        let skipped = create("skip", &BTreeMap::new(), false).unwrap();
        assert_eq!(decoded(&skipped, 3).as_deref(), Some("plain"));
        let refused = create("abc", &BTreeMap::new(), false).err().unwrap();
        assert!(refused.message.contains("does not read again"), "{refused}");
    }

    /// An aggregate brings every record related through its occurrence into
    /// the evaluation, where a plain read through another occurrence of the
    /// same table is scanned again (#1036): each record carries what both
    /// reads need, so `B::x`, sorted by `p` ascending, still reads record 2
    /// (`two`, `p` 10) beside `Count ( C::x )` over both records. Where a
    /// relationship to that table sorts by a later repetition, which the
    /// records read do not carry, the table stays read-only.
    #[test]
    fn an_aggregate_keeps_another_occurrences_first_record() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        use om_scripts::specs::{SortEntry, SortSpec, SpecField};
        let table = Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
            ],
        };
        let mut b = MemorySource::new();
        for (record, x, p) in [(1, "one", "20"), (2, "two", "10")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text("K"));
            values.set(2, 1, Value::text(x));
            values.set(3, 1, Value::text(p));
            b.put_record(0x8007, record, values);
        }
        let mut names = om_formula::NameTable::default();
        for (occurrence, name, table) in [
            (0xd0_0006, "A", 0x8006),
            (0xd0_0007, "B", 0x8007),
            (0xd0_0008, "C", 0x8007),
        ] {
            names.add_occurrence(occurrence, name, table);
        }
        for (table, id, name) in [
            (0x8006, 1, "k"),
            (0x8006, 2, "seen"),
            (0x8007, 1, "k"),
            (0x8007, 2, "x"),
            (0x8007, 3, "p"),
        ] {
            names.add_field(table, id, name);
        }
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(0xd0_0006);
        let expr = om_formula::compile("B::x & \"/\" & Count ( C::x )", &cx)
            .unwrap()
            .expr;
        let entries: Vec<(Vec<Vec<u8>>, Vec<u8>)> = [
            (4, om_formula::Key::from_id(0xd0_0006).0),
            (6, om_formula::polish::encode(&expr)),
        ]
        .into_iter()
        .map(|(key, value)| {
            let path = [0x8006, 3, 5, 2, 5, key]
                .iter()
                .map(|k| om_hbam::slot::uint_segment(*k))
                .collect();
            (path, value)
        })
        .collect();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&segments, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let plan_for = |repetition| {
            let mut file = CalcSchema::new("Aggregates", Locale::en_us());
            file.add_table(0x8007, "B");
            for (id, name) in [(1, "k"), (2, "x"), (3, "p")] {
                file.add_field(0x8007, FieldDef::data(id, name, ValueType::Text));
            }
            file.add_occurrence(0xd0_0006, "A", 0x8006);
            file.add_occurrence(0xd0_0007, "B", 0x8007);
            file.add_occurrence(0xd0_0008, "C", 0x8007);
            let by_k = || {
                vec![JoinPredicate {
                    op: JoinOp::Equal,
                    left: 1,
                    right: 1,
                }]
            };
            let sort = SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: vec![SortEntry {
                    field: SpecField {
                        occurrence: Some(0xd0_0007),
                        field: 3,
                        repetition,
                    },
                    summary: None,
                    value_list: None,
                    descending: false,
                    locale: 0,
                }],
            };
            file.add_relationship(RelationshipDef {
                sort_right: Some(sort),
                options: 0x40,
                ..RelationshipDef::new(4, 0xd0_0006, 0xd0_0007, by_k())
            });
            file.add_relationship(RelationshipDef::new(5, 0xd0_0006, 0xd0_0008, by_k()));
            RecordPlan::from_file_schema(&table, &tree, &file)
        };
        let plan = plan_for(1).unwrap();
        let existing = BTreeMap::from([(1, String::new()), (2, String::new())]);
        let changes = BTreeMap::from([(1, Some("K".to_string()))]);
        let mut environment = environment();
        environment.lookups = plan
            .read_lookups(WriteMode::Update, &existing, &changes, Entry::default(), &b)
            .unwrap();
        let seen = plan
            .compute(
                WriteMode::Update,
                &existing,
                &changes,
                &environment,
                Entry::default(),
            )
            .unwrap()
            .remove(&2)
            .flatten();
        assert_eq!(seen.as_deref(), Some("two/2"));
        assert!(plan_for(2)
            .err()
            .is_some_and(|e| e.message.contains("later repetition")));
    }

    /// Lookups of other types than text copy the related field's stored
    /// text as it is (auto-entry.md, Lookups, battery s43): a price stored
    /// as `12.50` into a number field, text into a number field (which
    /// reads its digits, 42), and a date into a text field. A time field
    /// and text into a date field keep the table read-only (`typed_copy`),
    /// and a related date field holding other text refuses the write.
    #[test]
    fn typed_lookups_copy_the_stored_text() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        // The exact-match form, copying B's field `copied`.
        let lookup = |copied: u8| -> Vec<u8> {
            vec![
                0x00, 0x00, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x00, 0x02, 0x01, copied, 0x00, 0x11,
                0x01, 0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x00,
            ]
        };
        let mut file = CalcSchema::new("Lookups", Locale::en_us());
        file.add_table(0x8007, "B");
        file.add_field(0x8007, FieldDef::data(1, "k", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(2, "price", ValueType::Number));
        file.add_field(0x8007, FieldDef::data(3, "name", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(4, "bought", ValueType::Date));
        file.add_occurrence(0xd0_0006, "A", 0x8006);
        file.add_occurrence(0xd0_0007, "B", 0x8007);
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left: 1,
                right: 1,
            }],
        ));
        // A's field 1 the key, then one lookup field per (type, copied).
        let plan_for = |lookups: &[(DataType, u8)]| {
            let mut fields = vec![field(1, FieldKind::Normal, DataType::Text, 0, 0, 0)];
            let leaves: Vec<(u64, Vec<u8>)> = lookups
                .iter()
                .enumerate()
                .map(|(i, (data_type, copied))| {
                    let id = 2 + i as u64;
                    fields.push(field(id, FieldKind::Normal, *data_type, 0, 0x400, 0));
                    (id, lookup(*copied))
                })
                .collect();
            let paths: Vec<Vec<Vec<u8>>> = leaves
                .iter()
                .map(|(id, _)| {
                    [0x8006, 3, 5, *id, 8]
                        .iter()
                        .map(|k| om_hbam::slot::uint_segment(*k))
                        .collect()
                })
                .collect();
            let mut tree = KeyTree::default();
            for (path, (_, bytes)) in paths.iter().zip(&leaves) {
                let path: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
                tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(bytes))
                    .unwrap();
            }
            let table = Table {
                id: 0x8006,
                name: "A".into(),
                fields,
            };
            RecordPlan::from_file_schema(&table, &tree, &file)
        };
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, crate::host::session::today(0));
        let source = |bought: &str| {
            let mut b = MemorySource::new();
            let mut values = RecordValues::default();
            for (field, text, ty) in [
                (1, "K", ValueType::Text),
                (2, "12.50", ValueType::Number),
                (3, "Widget 42", ValueType::Text),
                (4, bought, ValueType::Date),
            ] {
                values.set(field, 1, crate::host::stored_value(text, ty, &cx));
            }
            b.put_record(0x8007, 1, values);
            b
        };
        let existing = BTreeMap::from([(1, String::new())]);
        let changes = BTreeMap::from([(1, Some("K".to_string()))]);
        let read = |plan: &RecordPlan, b: &MemorySource| {
            plan.read_lookups(WriteMode::Update, &existing, &changes, Entry::default(), b)
        };
        let plan = plan_for(&[
            (DataType::Number, 2),
            (DataType::Number, 3),
            (DataType::Text, 4),
            (DataType::Date, 4),
        ])
        .unwrap();
        let mut environment = environment();
        environment.lookups = read(&plan, &source("1/2/2020")).unwrap();
        let row = plan
            .compute(
                WriteMode::Update,
                &existing,
                &changes,
                &environment,
                Entry::default(),
            )
            .unwrap();
        let text = |id: u64| row.get(&id).cloned().flatten().unwrap_or_default();
        assert_eq!(text(2), "12.50");
        assert_eq!(text(3), "Widget 42");
        let number = crate::host::stored_value(&text(3), ValueType::Number, &cx);
        assert_eq!(
            Value::Number(number.to_number(&cx))
                .to_text(&cx)
                .into_string(),
            "42"
        );
        assert_eq!(text(4), "1/2/2020");
        assert_eq!(text(5), "1/2/2020");
        // A related date field holding text that is no date: refused.
        let refused = read(&plan, &source("Widget 42")).unwrap_err();
        assert!(
            refused
                .message
                .contains("does not read as its field's type"),
            "{}",
            refused.message
        );
        // A time field, and text into a date field: read-only.
        for lookups in [[(DataType::Time, 4)], [(DataType::Date, 3)]] {
            assert!(plan_for(&lookups).is_err(), "{lookups:?}");
        }
    }

    /// A lookup through two `=` predicates, a replacing `Get ( UUID )` key
    /// and a plain field: a new record given another's key, as a
    /// duplicate is, gets a new key, so the relationship matches nothing
    /// and the lookup copies nothing, even though the other match field
    /// is set.
    #[test]
    fn a_replaced_key_suppresses_a_lookup_with_other_match_fields() {
        use crate::host::{FieldDef, JoinOp, JoinPredicate, RelationshipDef};
        const LOOKUP: &[u8] = &[
            0x00, 0x00, 0x04, 0x03, 0xd0, 0x00, 0x07, 0x00, 0x02, 0x01, 0x02, 0x00, 0x11, 0x01,
            0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x06, 0x00,
        ];
        const K1: &str = "1A2B3C4D-5E6F-4A7B-8C9D-0E1F2A3B4C5D";
        let table = Table {
            id: 0x8006,
            name: "A".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0x8000_0188, 0x1c),
                field(2, FieldKind::Normal, DataType::Text, 0, 0x400, 0),
                field(3, FieldKind::Normal, DataType::Text, 0, 0, 0),
            ],
        };
        let mut file = CalcSchema::new("Lookups", Locale::en_us());
        file.add_table(0x8007, "B");
        file.add_field(0x8007, FieldDef::data(1, "k", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(2, "x", ValueType::Text));
        file.add_field(0x8007, FieldDef::data(3, "kind", ValueType::Text));
        file.add_occurrence(0xd0_0006, "A", 0x8006);
        file.add_occurrence(0xd0_0007, "B", 0x8007);
        let equal = |left, right| JoinPredicate {
            op: JoinOp::Equal,
            left,
            right,
        };
        file.add_relationship(RelationshipDef::new(
            4,
            0xd0_0006,
            0xd0_0007,
            vec![equal(1, 1), equal(3, 3)],
        ));
        let leaves: [(u64, &[u64], &[u8]); 3] = [
            (1, &[5, 4], &[3, 0xd0, 0, 6]),
            (1, &[5, 6], &[0x9b, 0x65]),
            (2, &[8], LOOKUP),
        ];
        let paths: Vec<Vec<Vec<u8>>> = leaves
            .iter()
            .map(|(field, keys, _)| {
                [0x8006, 3, 5, *field]
                    .iter()
                    .chain(keys.iter())
                    .map(|k| om_hbam::slot::uint_segment(*k))
                    .collect()
            })
            .collect();
        let mut tree = KeyTree::default();
        for (path, (_, _, bytes)) in paths.iter().zip(leaves) {
            let path: Vec<KeySegment<'_>> = path.iter().map(|s| KeySegment(s)).collect();
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(bytes))
                .unwrap();
        }
        let plan = RecordPlan::from_file_schema(&table, &tree, &file).unwrap();
        let mut b = MemorySource::new();
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text(K1));
        values.set(2, 1, Value::text("555"));
        values.set(3, 1, Value::text("home"));
        b.put_record(0x8007, 1, values);
        let existing = BTreeMap::new();
        let changes = BTreeMap::from([(1, Some(K1.to_string())), (3, Some("home".to_string()))]);
        let entry = Entry::script(false, false);
        let mut environment = environment();
        environment.lookups = plan
            .read_lookups(WriteMode::Create, &existing, &changes, entry, &b)
            .unwrap();
        let made = plan
            .compute(WriteMode::Create, &existing, &changes, &environment, entry)
            .unwrap();
        assert!(made.get(&1).is_some_and(|k| k.as_deref() != Some(K1)));
        assert_eq!(made.get(&2), None);
    }

    #[test]
    fn unstored_global_summary_and_repeating_fields_keep_a_table_writable() {
        let mut table = table();
        let mut repeating = field(27, FieldKind::Normal, DataType::Text, 0, 0, 0);
        repeating.options[24..26].copy_from_slice(&3u16.to_be_bytes());
        table.fields.extend([
            // An unstored calculation is computed when read, never written,
            // whatever else its options carry: a stray `0x80`, as on one
            // production file's Date calculation (auto-entry.md).
            field(24, FieldKind::Calculation, DataType::Text, 0, 0x6_0000, 0),
            field(28, FieldKind::Calculation, DataType::Date, 0, 0x6_0080, 0),
            field(25, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0),
            field(26, FieldKind::Summary, DataType::Number, 0, 0, 0),
            repeating,
        ]);
        let plan = plan_of(&table, &base_formulas(), &[]).unwrap();
        // The repeating field takes entry, each repetition in its own cell
        // (#185); the global, summary and calculations don't.
        assert_eq!(
            plan.editable_fields(),
            BTreeSet::from(["Field1".into(), "Field2".into(), "Field27".into()])
        );
        let mut existing = existing();
        existing.insert(27, "first".into());
        existing.insert(27 * 256 + 2, "second".into());
        let updated = plan
            .prepare(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(2, Some("Nash".into()))]),
                &environment(),
            )
            .unwrap();
        assert_eq!(updated.changed_fields, vec![2, 8, 11, 13]);
        assert!(plan
            .prepare(
                WriteMode::Update,
                &existing,
                &BTreeMap::from([(25, Some("x".into()))]),
                &environment(),
            )
            .is_err());
        // A stored calculation is computed and written, and a lookup
        // writes its field: their bits are checked.
        for options in [0x80, UNSTORED | LOOKUP] {
            let mut table = table.clone();
            table.fields.push(field(
                29,
                FieldKind::Calculation,
                DataType::Number,
                0,
                options,
                0,
            ));
            let Err(refused) = plan_of(&table, &base_formulas(), &[]) else {
                panic!("a calculation with options {options:#x} planned");
            };
            assert!(
                refused.message.contains("unsupported field options"),
                "{refused}"
            );
        }
    }

    #[test]
    fn scripts_set_any_normal_field_and_skip_data_entry_rules_as_asked() {
        let mut table = table();
        table.fields.extend([
            field(30, FieldKind::Normal, DataType::Number, 0, 0, 0),
            field(31, FieldKind::Normal, DataType::Container, 0, 0, 0),
            // Not empty during data entry only, and Always.
            field(32, FieldKind::Normal, DataType::Text, 0, 0, 0x8),
            field(33, FieldKind::Normal, DataType::Text, 0, 0, 0x408),
        ]);
        let plan = plan(&table, &initials(1, 2)).unwrap();
        let script = Entry::script(false, false);
        let mut existing = existing();
        existing.extend([(32, "x".to_string()), (33, "y".to_string())]);
        let changes = |pairs: &[(u64, Option<&str>)]| -> BTreeMap<u64, Option<String>> {
            pairs
                .iter()
                .map(|(id, v)| (*id, v.map(str::to_owned)))
                .collect()
        };
        let compute = |entry, pairs: &[(u64, Option<&str>)]| {
            plan.compute(
                WriteMode::Update,
                &existing,
                &changes(pairs),
                &environment(),
                entry,
            )
        };
        // A user types into text and number fields that allow it, a number
        // as typed (#185); only a script sets a field whose auto-entered
        // value prohibits modification.
        let user = compute(Entry::default(), &[(30, Some("5"))]).unwrap();
        assert_eq!(user.get(&30), Some(&Some("5".into())));
        let other_uuid = "C82C1E88-8085-4A2A-A698-72A11F356445";
        let protected = compute(Entry::default(), &[(9, Some(other_uuid))]).unwrap_err();
        assert_eq!(protected.code, Some(error::FIELD_LOCKED));
        let set = compute(script, &[(30, Some("5")), (9, Some(other_uuid))]).unwrap();
        assert_eq!(set.get(&30), Some(&Some("5".into())));
        assert_eq!(set.get(&9), Some(&Some(other_uuid.into())));
        // Modification values follow a script's write too.
        assert_eq!(set.get(&11), Some(&Some("session-account".into())));
        // A container holds its host's reference text, or text.
        let container = compute(script, &[(31, Some("01\u{e000}"))]).unwrap();
        assert_eq!(container.get(&31), Some(&Some("01\u{e000}".into())));

        // Not empty: 509, unless data entry validation is skipped for a
        // field validated only during data entry.
        let cleared = compute(script, &[(32, None)]).unwrap_err();
        assert_eq!(cleared.code, Some(error::REQUIRED));
        assert!(compute(Entry::script(true, false), &[(32, None)]).is_ok());
        let always = compute(Entry::script(true, false), &[(33, None)]).unwrap_err();
        assert_eq!(always.code, Some(error::REQUIRED));

        // Skipping auto-enter leaves creation and modification values out;
        // stored calculations are still computed.
        let created = plan
            .compute(
                WriteMode::Create,
                &BTreeMap::new(),
                &changes(&[(2, Some("World")), (32, Some("a")), (33, Some("b"))]),
                &environment(),
                Entry::script(true, true),
            )
            .unwrap();
        assert_eq!(created.get(&8), Some(&Some("W".into())));
        for audit in [9, 10, 11, 12, 13] {
            assert!(!created.contains_key(&audit), "{audit}");
        }
    }

    /// Import Records' rows (oracle x02, script-steps.md "Implementation
    /// notes (import/export)"): with "perform auto-enter options" the
    /// fields the row does not set get their auto-entered values and a
    /// field it sets keeps the imported value, empty or not; stored
    /// calculations see both; "only during data entry" validation is not
    /// checked. Without auto-enter nothing is auto-entered.
    #[test]
    fn imports_keep_their_values_over_auto_enter_and_skip_data_entry_rules() {
        let mut table = table();
        table.fields.extend([
            field(20, FieldKind::Calculation, DataType::Text, 0, 0, 0),
            // By calculation, only during data entry; and Always.
            field(21, FieldKind::Normal, DataType::Text, 0, 0, 0x1),
            field(22, FieldKind::Normal, DataType::Text, 0, 0, 0x401),
        ]);
        let mut formulas = base_formulas();
        formulas.extend([
            (20, 5, "Field10 & \"/\" & Field11 & \"/\" & Field1"),
            (21, 0x19, "Length ( Self ) <= 3"),
            (22, 0x19, "Length ( Self ) <= 3"),
        ]);
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let changes = |pairs: &[(u64, Option<&str>)]| -> BTreeMap<u64, Option<String>> {
            pairs
                .iter()
                .map(|(id, v)| (*id, v.map(str::to_owned)))
                .collect()
        };
        let compute = |mode, existing: &BTreeMap<u64, String>, entry, pairs: &[_]| {
            plan.compute(mode, existing, &changes(pairs), &environment(), entry)
        };
        let none = BTreeMap::new();
        let text = |m: &BTreeMap<u64, Option<String>>, id| m.get(&id).cloned().flatten();

        // Auto-enter on: Field10 (creation account) is auto-entered, the
        // imported Field11 (modification account) is kept, and the stored
        // calculation reads both.
        let row = [
            (1, Some("Alpha")),
            (11, Some("imported")),
            (21, Some("too long")),
        ];
        let added = compute(WriteMode::Create, &none, Entry::import(true), &row).unwrap();
        assert_eq!(text(&added, 10).as_deref(), Some("session-account"));
        assert_eq!(text(&added, 11).as_deref(), Some("imported"));
        assert_eq!(
            text(&added, 20).as_deref(),
            Some("session-account/imported/Alpha")
        );
        assert_eq!(text(&added, 21).as_deref(), Some("too long"));
        assert_eq!(text(&added, 9).as_deref(), Some(UUID));
        // An empty imported value is kept too.
        let empty = compute(
            WriteMode::Create,
            &none,
            Entry::import(true),
            &[(1, Some("Alpha")), (11, None)],
        )
        .unwrap();
        assert_eq!(text(&empty, 11), None);
        assert_eq!(text(&empty, 20).as_deref(), Some("session-account//Alpha"));
        // Validated Always is still checked.
        let refused = compute(
            WriteMode::Create,
            &none,
            Entry::import(true),
            &[(22, Some("too long"))],
        )
        .unwrap_err();
        assert_eq!(refused.code, Some(error::VALIDATION_CALCULATION));

        // Auto-enter off: nothing auto-entered.
        let plain = compute(
            WriteMode::Create,
            &none,
            Entry::import(false),
            &[(1, Some("Alpha"))],
        )
        .unwrap();
        for audit in [9, 10, 11, 12, 13] {
            assert!(!plain.contains_key(&audit), "{audit}");
        }
        assert_eq!(text(&plain, 20).as_deref(), Some("//Alpha"));

        // An update: modification values and the calculation follow.
        let updated = compute(
            WriteMode::Update,
            &existing(),
            Entry::import(true),
            &[(1, Some("Beta"))],
        )
        .unwrap();
        assert_eq!(text(&updated, 11).as_deref(), Some("session-account"));
        assert_eq!(
            text(&updated, 20).as_deref(),
            Some("original-account/session-account/Beta")
        );
        assert!(!updated.contains_key(&10));
    }

    #[test]
    fn refusals_carry_filemakers_errors() {
        let mut table = table();
        table.fields[0].options[12..16].copy_from_slice(&0x201u32.to_be_bytes());
        table.fields[0].options[20..24].copy_from_slice(&4u32.to_be_bytes());
        let mut formulas = base_formulas();
        formulas.push((1, 0x19, "Self <> \"nope\""));
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let code = |text: &str| create(&plan, &[(1, text)]).err().and_then(|e| e.code);
        assert_eq!(code("nope"), Some(error::VALIDATION_CALCULATION));
        assert_eq!(code("longer"), Some(error::TOO_LONG));
        assert_eq!(code("ok"), None);
    }

    #[test]
    fn calculations_the_client_cannot_evaluate_still_refuse_writes() {
        let mut table = table();
        table.fields.extend([
            field(25, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0),
            field(28, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
        ]);
        let with = |source: &'static str| {
            let mut formulas = base_formulas();
            formulas.push((28, 5, source));
            plan_of(&table, &formulas, &[])
        };
        let error = |source| with(source).err().unwrap().to_string();
        assert!(error("Get ( RecordID )").contains("Get() selector"));
        // An auto-entered formula reads a global field as the session's
        // value (schema-path.md 5.17): a write without those values is
        // refused, not computed over an empty global.
        let plan = with("Field25 & \"!\"").unwrap();
        assert!(plan.reads_globals());
        let refused = create(&plan, &[(1, "a")]).err().unwrap();
        assert_eq!(refused.code, Some(error::UNAVAILABLE));
        let mut globals = RecordValues::default();
        globals.set(25, 1, Value::text("G"));
        let made = plan
            .prepare(
                WriteMode::Create,
                &BTreeMap::new(),
                &BTreeMap::from([(1, Some("a".to_owned()))]),
                &RecordEnvironment {
                    globals: Some(Arc::new(globals)),
                    ..environment()
                },
            )
            .unwrap();
        assert_eq!(decoded(&made, 28).as_deref(), Some("G!"));
        // A stored calculation over a global is not one FileMaker stores
        // (5.16): still refused.
        let mut stored = super::tests::table();
        stored.fields.extend([
            field(25, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0),
            field(29, FieldKind::Calculation, DataType::Text, 0, 0, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.push((29, 5, "Field25"));
        let error = plan_of(&stored, &formulas, &[]).err().unwrap().to_string();
        assert!(error.contains("non-text storage"), "{error}");
        // Nor an auto-entered formula over a repeating global (#897's
        // review): which repetitions it reads is not tracked.
        let mut repeating = super::tests::table();
        let mut global = field(25, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0);
        global.options[24..26].copy_from_slice(&2u16.to_be_bytes());
        repeating.fields.extend([
            global,
            field(28, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.push((28, 5, "GetRepetition ( Field25 ; 2 ) & \"!\""));
        let error = plan_of(&repeating, &formulas, &[])
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("non-text storage"), "{error}");
        // A calculation that fails when evaluated refuses the write.
        let plan = with("Length ( Field1 ) / 0").unwrap();
        let failed = create(&plan, &[(1, "a")]).err().unwrap().to_string();
        assert!(failed.contains("error 15"), "{failed}");
    }

    /// Serial numbers on creation (`0x2`) and on commit (`0x200`): a new
    /// record takes the values the host reserved, before the calculations
    /// that read them; a duplicate's copied value is replaced, a script's
    /// own value kept; edits take none.
    #[test]
    fn a_new_record_takes_the_serial_numbers_the_host_reserved() {
        let serial = |id, data_type, flags, increment: u32| {
            let mut field = field(id, FieldKind::Normal, data_type, 0, flags, 0);
            field.options[16..20].copy_from_slice(&increment.to_be_bytes());
            field
        };
        let mut table = table();
        table.fields.extend([
            serial(30, DataType::Number, 0x8000_0002, 1),
            // On commit, modification prohibited.
            serial(31, DataType::Text, 0x300, 5),
            field(32, FieldKind::Normal, DataType::Text, 0, 0x8, 0),
        ]);
        let mut formulas = base_formulas();
        formulas.push((32, 5, "\"#\" & Field31"));
        let plan = plan_of(&table, &formulas, &[]).unwrap();
        let stored = BTreeMap::new();
        let create = WriteMode::Create;
        let reserved = |field, increment, on_commit| Serial {
            field,
            increment,
            on_commit,
        };
        let both = [reserved(30, 1, false), reserved(31, 5, true)];
        assert_eq!(plan.serials_for(create, Entry::default()), both);
        assert!(plan
            .serials_for(WriteMode::Update, Entry::default())
            .is_empty());
        assert!(plan
            .serials_for(create, Entry::script(false, true))
            .is_empty());

        let mut environment = environment();
        environment.serials = BTreeMap::from([(30, "7".into()), (31, "INV0100".into())]);
        let copied = BTreeMap::from([(30, Some("3".to_string()))]);
        let computed = plan
            .compute(create, &stored, &copied, &environment, Entry::default())
            .unwrap();
        assert_eq!(computed[&30].as_deref(), Some("7"));
        assert_eq!(computed[&31].as_deref(), Some("INV0100"));
        assert_eq!(computed[&32].as_deref(), Some("#INV0100"));
        let created: BTreeMap<u64, String> = computed
            .into_iter()
            .filter_map(|(cell, text)| Some((cell, text?)))
            .collect();
        // A script's Set Field after New Record replaces the serial, which
        // is still taken: the sequence moves past it.
        let script = Entry::script(false, false);
        assert_eq!(plan.serials_for(create, script), both);
        let computed = plan
            .compute(create, &stored, &copied, &environment, script)
            .unwrap();
        assert_eq!(computed[&30].as_deref(), Some("3"));
        // Without a reserved value nothing is written.
        environment.serials.remove(&31);
        let refused = plan.compute(create, &stored, &copied, &environment, script);
        assert_eq!(refused.err().and_then(|e| e.code), Some(error::UNAVAILABLE));
        let edit = BTreeMap::from([(1, Some("b".to_string()))]);
        let computed = plan
            .compute(
                WriteMode::Update,
                &created,
                &edit,
                &environment,
                Entry::default(),
            )
            .unwrap();
        assert!(!computed.contains_key(&30) && !computed.contains_key(&31));

        // A date serial, an increment that does not advance, both kinds at
        // once and a global serial keep the table read-only.
        for refused in [
            serial(33, DataType::Date, 0x2, 1),
            serial(33, DataType::Text, 0x2, 0),
            serial(33, DataType::Text, 0x2, 0x8000_0000),
            serial(33, DataType::Text, 0x202, 1),
            serial(33, DataType::Text, GLOBAL | 0x200, 1),
        ] {
            let mut table = table.clone();
            table.fields.push(refused);
            assert!(plan_of(&table, &formulas, &[]).is_err());
        }
    }

    /// A repeating field whose id is above `u64::MAX / 256` (a crafted file
    /// or catalog upload; FileMaker issues small ids): its repetitions after
    /// the first cannot have a cell, as `F * 256 + n` would pass
    /// `u64::MAX`, and they read as empty, not as the cells the key would
    /// wrap around to (2 and 3, Field2's and Field3's, #388). Validated not
    /// empty, the field refuses a write while it is empty, though Field2
    /// holds a value.
    #[test]
    fn repetitions_past_the_largest_cell_key_read_as_empty() {
        let big = u64::MAX / 256 + 1;
        let mut table = table();
        // Not empty, Always; three repetitions.
        let mut repeating = field(big, FieldKind::Normal, DataType::Text, 0, 0, 0x408);
        repeating.options[24..26].copy_from_slice(&3u16.to_be_bytes());
        table.fields.push(repeating);
        let plan = plan_of(&table, &base_formulas(), &[]).unwrap();
        assert_eq!(plan.cells(big).collect::<Vec<_>>(), [big]);
        let write = |existing: &BTreeMap<u64, String>| {
            let changes = BTreeMap::from([(1, Some("Changed".to_string()))]);
            plan.compute(
                WriteMode::Update,
                existing,
                &changes,
                &environment(),
                Entry::script(false, false),
            )
            .map(|_| ())
            .map_err(|e| e.code)
        };
        assert_eq!(write(&existing()), Err(Some(509)));
        let mut held = existing();
        held.insert(big, "x".into());
        assert_eq!(write(&held), Ok(()));
    }
}
