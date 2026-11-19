//! A field's options as FileMaker Pro 26's "Options for Field" dialog shows
//! them (docs/re/schema-path.md 5.5, 5.8, 5.13): auto-enter, validation and
//! storage. They are read from the field's 26-byte definition
//! `[T, 3, 5, F, 02]` and its members ([`FieldOptions::decode`],
//! [`FieldOptions::read_members`]): `0c` (the next serial value), `0d`
//! (auto-entered data), `05` (an auto-entered calculation), `08` and `0b`
//! (a lookup and its constant), `07` (the value list and the furigana field
//! with its translation), `18` (the custom message), `19` and `20` (the
//! validation and message calculations), `21` and `22` (the range). The
//! definition's words are written back by [`FieldOptions::encode`];
//! `schema_draft` writes the members ([`value_list_member`],
//! [`furigana_member`]).
//!
//! What OpenMaker cannot edit yet is decoded so the dialog can show it and
//! kept as stored: a lookup whose definition does not read. Option and
//! validation bits this module does not name are kept.

/// A value FileMaker enters on creation or modification: definition byte 3
/// (`mEntryVariable`), 0-4 on creation and 5-9 on modification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntryValue {
    #[default]
    Date,
    Time,
    Timestamp,
    Name,
    AccountName,
}

impl EntryValue {
    pub const ALL: [EntryValue; 5] = [
        EntryValue::Date,
        EntryValue::Time,
        EntryValue::Timestamp,
        EntryValue::Name,
        EntryValue::AccountName,
    ];

    /// The label in FileMaker Pro's Creation and Modification menus.
    pub fn label(self) -> &'static str {
        match self {
            EntryValue::Date => "Date",
            EntryValue::Time => "Time",
            EntryValue::Timestamp => "Timestamp (Date and Time)",
            EntryValue::Name => "Name",
            EntryValue::AccountName => "Account Name",
        }
    }

    fn code(self) -> u8 {
        match self {
            EntryValue::Date => 0,
            EntryValue::Time => 1,
            EntryValue::Timestamp => 2,
            EntryValue::Name => 3,
            EntryValue::AccountName => 4,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::ALL[usize::from(code % 5)]
    }
}

/// An auto-entered serial number. `next` is the text FileMaker keeps in
/// member `0c`; the increment is the definition's `mSerialIncrement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Serial {
    /// Generate on commit (option `0x200`) rather than on creation (`0x2`).
    pub on_commit: bool,
    pub next: String,
    pub increment: u32,
}

impl Default for Serial {
    fn default() -> Self {
        Serial {
            on_commit: false,
            next: "1".into(),
            increment: 1,
        }
    }
}

/// What a field enters by itself. FileMaker Pro's dialog allows one of
/// these at a time: checking one clears the others (schema-path.md 5.8,
/// E40).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AutoEnterValue {
    #[default]
    None,
    /// Option `0x1`, byte 3 0-4.
    Creation(EntryValue),
    /// Option `0x1`, byte 3 5-9.
    Modification(EntryValue),
    /// Option `0x2` on creation, `0x200` alone on commit (E41).
    Serial(Serial),
    /// Option `0x10`.
    LastVisited,
    /// Option `0x4`, the text in member `0d`.
    Data(String),
    /// Option `0x8`, the calculation in member `05` (5.13).
    Calculated(AutoCalculation),
    /// Option `0x400`, the lookup in member `08` (5.5, 5.13); `None` while
    /// the definition is not read, or when it does not read (then kept).
    LookedUp(Option<Lookup>),
}

/// "Looked-up value": what the Lookup for Field dialog sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    /// "Starting with table": an occurrence of the field's own table.
    pub from: u64,
    /// "Lookup from related table".
    pub to: u64,
    /// "Copy value from field", of `to`'s table.
    pub copied: u64,
    /// "Don't copy contents if empty" off: an empty value is copied too
    /// (byte 1 `01`).
    pub copy_empty: bool,
    /// "If no exact match, then" (byte 0).
    pub no_match: NoMatch,
}

/// "If no exact match, then".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NoMatch {
    #[default]
    DoNotCopy,
    NextLower,
    NextHigher,
    /// "use": the text in member `0b`.
    Constant(String),
}

impl NoMatch {
    fn code(&self) -> u8 {
        match self {
            NoMatch::DoNotCopy => 0,
            NoMatch::NextLower => 1,
            NoMatch::NextHigher => 2,
            NoMatch::Constant(_) => 3,
        }
    }
}

/// The 6-, 7- and 6-byte buffers of a lookup definition's three keys
/// (5.5): the occurrence looked up from, the field copied, the starting
/// occurrence, with `01` between the last two.
const LOOKUP_BUFFERS: [usize; 3] = [6, 7, 6];

impl Lookup {
    /// Read a lookup definition `08` and its constant `0b`, whatever its
    /// padding holds; `None` when it does not have FileMaker's 22 bytes or
    /// a key does not read.
    pub fn decode(bytes: &[u8], constant: Option<String>) -> Option<Lookup> {
        if bytes.len() != 22 || bytes[1] > 1 || bytes[15] != 1 {
            return None;
        }
        let key = |at: usize, size: usize| -> Option<u64> {
            let buffer = &bytes[at..at + size];
            let total = usize::from(*buffer.first()?);
            let inner = buffer.get(1..1 + total)?;
            let (&length, key) = inner.split_first()?;
            (usize::from(length) == key.len() && !key.is_empty())
                .then(|| key.iter().fold(0u64, |n, b| n << 8 | u64::from(*b)))
        };
        let no_match = match bytes[0] {
            0 => NoMatch::DoNotCopy,
            1 => NoMatch::NextLower,
            2 => NoMatch::NextHigher,
            3 => NoMatch::Constant(constant.unwrap_or_default()),
            _ => return None,
        };
        Some(Lookup {
            to: key(2, LOOKUP_BUFFERS[0])?,
            copied: key(8, LOOKUP_BUFFERS[1])?,
            from: key(16, LOOKUP_BUFFERS[2])?,
            copy_empty: bytes[1] == 1,
            no_match,
        })
    }

    /// The definition `08` as FileMaker Pro 26 writes it (5.5: its two
    /// native samples): the padding after the copied field's key is
    /// `00 11 01 00`, the others zero. `None` for keys of another length
    /// than those samples have (3-byte occurrences, 1-byte fields), whose
    /// padding is not established.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let nested = |id: u64| {
            let bytes = id.to_be_bytes();
            let key = &bytes[bytes.iter().take_while(|b| **b == 0).count().min(7)..];
            let mut out = vec![
                u8::try_from(key.len() + 1).ok()?,
                u8::try_from(key.len()).ok()?,
            ];
            out.extend_from_slice(key);
            Some(out)
        };
        let (to, copied, from) = (nested(self.to)?, nested(self.copied)?, nested(self.from)?);
        if to.len() != 5 || from.len() != 5 || copied.len() != 3 {
            return None;
        }
        let mut out = vec![self.no_match.code(), u8::from(self.copy_empty)];
        out.extend(to);
        out.push(0x00);
        out.extend(copied);
        out.extend([0x00, 0x11, 0x01, 0x00]);
        out.push(0x01);
        out.extend(from);
        out.push(0x00);
        Some(out)
    }
}

/// A formula among a field's options: an auto-entered calculation, a
/// validation calculation or a message calculation, as Specify Calculation
/// shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptionFormula {
    /// The formula's text with the current names; `None` when the stored
    /// formula has none to show (stripped, or not read): such a formula is
    /// kept, not edited.
    pub text: Option<String>,
    /// The occurrence it is evaluated from.
    pub context: u64,
}

impl OptionFormula {
    pub fn new(text: impl Into<String>, context: u64) -> Self {
        OptionFormula {
            text: Some(text.into()),
            context,
        }
    }
}

/// "Calculated value" on the Auto-Enter tab (5.13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoCalculation {
    pub formula: OptionFormula,
    /// "Do not replace existing value of field (if any)" off: option
    /// `0x80` (`overwriteExisting`).
    pub replace_existing: bool,
    /// "Do not evaluate if all referenced fields are empty": option `0x20`
    /// ("always evaluate") clear.
    pub skip_when_empty: bool,
}

impl AutoCalculation {
    /// A calculated value as FileMaker Pro offers it: "Do not replace
    /// existing value of field" and "Do not evaluate if all referenced
    /// fields are empty" checked [I].
    pub fn new(formula: OptionFormula) -> Self {
        AutoCalculation {
            formula,
            replace_existing: false,
            skip_when_empty: true,
        }
    }
}

impl AutoEnterValue {
    /// Whether FileMaker evaluates the field when a record is created (1)
    /// and when it is committed after a change (2): the field's lists in
    /// the dependency index (schema-path.md 5.8, E11, E14, E33, E41, E43).
    pub fn evaluation(&self) -> u8 {
        match self {
            // A lookup is evaluated by its match fields, derived from its
            // `fb` node, not by an event (5.13, l01).
            AutoEnterValue::None | AutoEnterValue::LookedUp(_) => 0,
            AutoEnterValue::Modification(_) => 1 | 2,
            AutoEnterValue::Serial(serial) if serial.on_commit => 2,
            _ => 1,
        }
    }

    /// Whether OpenMaker edits this kind of value: not a lookup whose
    /// definition does not read.
    pub fn editable(&self) -> bool {
        !matches!(self, AutoEnterValue::LookedUp(None))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoEnter {
    pub value: AutoEnterValue,
    /// "Prohibit modification of value during data entry", option `0x100`.
    pub prohibit_modification: bool,
}

/// "Strict data type": validation `0x1000`, `0x2000` or `0x4000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictType {
    NumericOnly,
    FourDigitYear,
    TimeOfDay,
}

impl StrictType {
    pub const ALL: [StrictType; 3] = [
        StrictType::NumericOnly,
        StrictType::FourDigitYear,
        StrictType::TimeOfDay,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StrictType::NumericOnly => "Numeric Only",
            StrictType::FourDigitYear => "4-Digit Year Date",
            StrictType::TimeOfDay => "Time of Day",
        }
    }

    fn bit(self) -> u32 {
        match self {
            StrictType::NumericOnly => 0x1000,
            StrictType::FourDigitYear => 0x2000,
            StrictType::TimeOfDay => 0x4000,
        }
    }
}

/// The validation word `mValidationSet` and the byte limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validation {
    /// "Always" (`0x400`) rather than "Only during data entry".
    pub always: bool,
    /// "Allow user to override during data entry": `0x4` clear.
    pub allow_override: bool,
    pub strict_type: Option<StrictType>,
    /// `0x8`.
    pub not_empty: bool,
    /// `0x10`.
    pub unique: bool,
    /// `0x20`.
    pub existing: bool,
    /// "Maximum number of characters" (`0x200`) and the definition's
    /// `mByteLimit`.
    pub maximum_characters: Option<u32>,
    /// "Member of value list" (`0x100`): the value list's id from member
    /// `07`, 0 when that does not read.
    pub value_list: Option<u64>,
    /// "In range" (`0x40`): the bounds as text, members `21` and `22`.
    pub range: Option<(String, String)>,
    /// "Validated by calculation" (`0x1`), member `19`.
    pub calculation: Option<ValidationCalculation>,
    /// "Display custom message if validation fails" (`0x80`).
    pub message: Option<Message>,
}

/// "Validated by calculation".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationCalculation {
    pub formula: OptionFormula,
    /// "Validate only if field has been modified": `0x2` clear.
    pub only_if_modified: bool,
}

/// A custom validation message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// The text in member `18`.
    Text(String),
    /// A calculation in member `20` (`0x8000`), as FileMaker Pro 26's
    /// dialog writes every custom message.
    Calculation(OptionFormula),
}

impl Default for Validation {
    fn default() -> Self {
        Validation {
            always: false,
            allow_override: true,
            strict_type: None,
            not_empty: false,
            unique: false,
            existing: false,
            maximum_characters: None,
            value_list: None,
            range: None,
            calculation: None,
            message: None,
        }
    }
}

/// Validation bits this module edits; every other bit is kept.
const VALIDATION_EDITED: u32 = BY_CALCULATION
    | CALCULATION_ALWAYS
    | 0x4
    | 0x8
    | 0x10
    | 0x20
    | IN_RANGE
    | VALUE_LIST
    | 0x200
    | 0x400
    | 0x1000
    | 0x2000
    | 0x4000;
const BY_CALCULATION: u32 = 0x1;
const CALCULATION_ALWAYS: u32 = 0x2;
const IN_RANGE: u32 = 0x40;
const CUSTOM_MESSAGE: u32 = 0x80;
const VALUE_LIST: u32 = 0x100;
const MESSAGE_CALCULATION: u32 = 0x8000;

/// Indexing on the Storage tab. "Minimal" is the value index alone, "All"
/// adds the word index for text; a field of another type has the value
/// index only, which FileMaker Pro writes for "All" (E41) and does not
/// offer "Minimal" for (E43).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Indexing {
    #[default]
    None,
    Minimal,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    /// "Use global storage", option `0x10000`.
    pub global: bool,
    /// "Maximum number of repetitions", `mMaxReps`.
    pub repetitions: u16,
    pub indexing: Indexing,
    /// "Automatically create indexes as needed": option `0x40000000` clear.
    pub automatic_indexes: bool,
    /// The index language, definition bytes 4-7 (21 English).
    pub language: u32,
    /// A container's data stored outside the file. Shown and kept.
    pub external: Option<ExternalStorage>,
}

impl Default for Storage {
    fn default() -> Self {
        Storage {
            global: false,
            repetitions: 1,
            indexing: Indexing::None,
            automatic_indexes: true,
            language: 21,
            external: None,
        }
    }
}

/// "Store container data externally": option `0x1000`, with `0x2000` for
/// open storage (schema-path.md 5.5, `kRemoteContainer`, `kOpenContainer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalStorage {
    Secure,
    Open,
}

const AUTO_VARIABLE: u32 = 0x1;
const AUTO_SERIAL: u32 = 0x2;
const AUTO_DATA: u32 = 0x4;
const AUTO_CALCULATED: u32 = 0x8;
const AUTO_LAST_VISITED: u32 = 0x10;
/// "Always evaluate": "Do not evaluate if all referenced fields are empty"
/// off.
const ALWAYS_EVALUATE: u32 = 0x20;
const REPLACE_EXISTING: u32 = 0x80;
const PROHIBIT: u32 = 0x100;
const SERIAL_ON_COMMIT: u32 = 0x200;
pub(crate) const AUTO_LOOKUP: u32 = 0x400;
/// Every auto-enter source; FileMaker Pro sets at most one.
pub const AUTO_SOURCES: u32 = AUTO_VARIABLE
    | AUTO_SERIAL
    | SERIAL_ON_COMMIT
    | AUTO_DATA
    | AUTO_CALCULATED
    | AUTO_LAST_VISITED
    | AUTO_LOOKUP;
const FURIGANA: u32 = 0x40;
const EXTERNAL: u32 = 0x1000;
const OPEN_STORAGE: u32 = 0x2000;
const GLOBAL: u32 = 0x1_0000;
pub const VALUE_INDEX: u32 = 0x8000_0000;
pub const WORD_INDEX: u32 = 0x2000_0000;
const NO_AUTOMATIC_INDEX: u32 = 0x4000_0000;

/// Definition offsets (schema-path.md 5.5).
const TYPE: usize = 1;
const VARIABLE: usize = 3;
const LANGUAGE: std::ops::Range<usize> = 4..8;
const OPTIONS: std::ops::Range<usize> = 8..12;
const VALIDATION: std::ops::Range<usize> = 12..16;
const INCREMENT: std::ops::Range<usize> = 16..20;
const BYTE_LIMIT: std::ops::Range<usize> = 20..24;
const REPETITIONS: std::ops::Range<usize> = 24..26;
/// The text data type, the only one with a word index.
const TEXT: u8 = 1;

fn word(definition: &[u8], range: std::ops::Range<usize>) -> u32 {
    u32::from_be_bytes(definition[range].try_into().expect("4 bytes"))
}

/// A field's options; see the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldOptions {
    pub auto_enter: AutoEnter,
    pub validation: Validation,
    pub storage: Storage,
    /// "Use Furigana Field" ([`Furigana`]).
    pub furigana: Furigana,
}

/// "Use Furigana Field" (schema-path.md 5.13, FileMaker Pro 26.0.3's saves
/// of #1301): option `0x40`, and the second part of member `07`, which the
/// value list shares ([`furigana_member`]). Turning it off clears the bit
/// alone; the field and the translation stay in `07`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Furigana {
    /// Option `0x40`.
    pub on: bool,
    /// The field of the same table that receives the reading (`07` byte 6
    /// on, a nested key string); 0 for none.
    pub field: u64,
    /// "Translate into" (`07` byte 12), by the menu's order
    /// ([`Furigana::TRANSLATIONS`]); 0 for none.
    pub translation: u8,
}

impl Furigana {
    /// "Translate into"'s menu, by the value FileMaker Pro writes. 2 and 5
    /// were seen ([`Furigana::CONFIRMED`]); the others are read from the
    /// menu's order.
    pub const TRANSLATIONS: [(u8, &'static str); 5] = [
        (1, "Hiragana"),
        (2, "Full-Width Katakana"),
        (3, "Half-Width Katakana"),
        (4, "Full-Width Roman"),
        (5, "Half-Width Roman"),
    ];

    /// The translations FileMaker Pro 26.0.3 was seen to write: Full-Width
    /// Katakana and Half-Width Roman. OpenMaker writes no other.
    pub const CONFIRMED: [u8; 2] = [2, 5];
}

impl FieldOptions {
    /// Read a normal field's options from its definition, which FileMaker
    /// always stores with 26 bytes, and the texts of its members `0c` and
    /// `0d` (absent ones read as FileMaker shows them: "1" and empty).
    pub fn decode(
        definition: &[u8],
        next_serial: Option<&str>,
        data: Option<&str>,
    ) -> Option<Self> {
        if definition.len() != 26 {
            return None;
        }
        let options = word(definition, OPTIONS);
        let validation = word(definition, VALIDATION);
        let variable = definition[VARIABLE];
        let value = if options & AUTO_LOOKUP != 0 {
            AutoEnterValue::LookedUp(None)
        } else if options & AUTO_CALCULATED != 0 {
            AutoEnterValue::Calculated(AutoCalculation {
                formula: OptionFormula::default(),
                replace_existing: options & REPLACE_EXISTING != 0,
                skip_when_empty: options & ALWAYS_EVALUATE == 0,
            })
        } else if options & (AUTO_SERIAL | SERIAL_ON_COMMIT) != 0 {
            AutoEnterValue::Serial(Serial {
                on_commit: options & AUTO_SERIAL == 0,
                next: next_serial.unwrap_or("1").to_owned(),
                increment: word(definition, INCREMENT),
            })
        } else if options & AUTO_DATA != 0 {
            AutoEnterValue::Data(data.unwrap_or_default().to_owned())
        } else if options & AUTO_LAST_VISITED != 0 {
            AutoEnterValue::LastVisited
        } else if options & AUTO_VARIABLE != 0 {
            if variable < 5 {
                AutoEnterValue::Creation(EntryValue::from_code(variable))
            } else {
                AutoEnterValue::Modification(EntryValue::from_code(variable))
            }
        } else {
            AutoEnterValue::None
        };
        let strict_type = StrictType::ALL
            .into_iter()
            .find(|t| validation & t.bit() != 0);
        let text = definition[TYPE] == TEXT;
        let indexing = if options & VALUE_INDEX == 0 && options & WORD_INDEX == 0 {
            Indexing::None
        } else if options & WORD_INDEX != 0 || !text {
            Indexing::All
        } else {
            Indexing::Minimal
        };
        Some(FieldOptions {
            auto_enter: AutoEnter {
                value,
                prohibit_modification: options & PROHIBIT != 0,
            },
            validation: Validation {
                always: validation & 0x400 != 0,
                allow_override: validation & 0x4 == 0,
                strict_type,
                not_empty: validation & 0x8 != 0,
                unique: validation & 0x10 != 0,
                existing: validation & 0x20 != 0,
                maximum_characters: (validation & 0x200 != 0).then(|| word(definition, BYTE_LIMIT)),
                value_list: (validation & VALUE_LIST != 0).then_some(0),
                range: (validation & IN_RANGE != 0).then(Default::default),
                calculation: (validation & BY_CALCULATION != 0).then(|| ValidationCalculation {
                    formula: OptionFormula::default(),
                    only_if_modified: validation & CALCULATION_ALWAYS == 0,
                }),
                message: if validation & MESSAGE_CALCULATION != 0 {
                    Some(Message::Calculation(OptionFormula::default()))
                } else if validation & CUSTOM_MESSAGE != 0 {
                    Some(Message::Text(String::new()))
                } else {
                    None
                },
            },
            storage: Storage {
                global: options & GLOBAL != 0,
                repetitions: u16::from_be_bytes(
                    definition[REPETITIONS].try_into().expect("2 bytes"),
                ),
                indexing,
                automatic_indexes: options & NO_AUTOMATIC_INDEX == 0,
                language: word(definition, LANGUAGE),
                external: (options & EXTERNAL != 0).then_some(if options & OPEN_STORAGE != 0 {
                    ExternalStorage::Open
                } else {
                    ExternalStorage::Secure
                }),
            },
            furigana: Furigana {
                on: options & FURIGANA != 0,
                ..Furigana::default()
            },
        })
    }

    /// Fill in what the definition only flags, from the field's members:
    /// the formulas, the value list, the range and the message (5.13).
    pub fn read_members(&mut self, members: &dyn OptionMembers) {
        match &mut self.auto_enter.value {
            AutoEnterValue::Calculated(calculation) => {
                if let Some(formula) = members.formula(FORMULA) {
                    calculation.formula = formula;
                }
            }
            AutoEnterValue::LookedUp(lookup) => {
                *lookup = members
                    .bytes(LOOKUP)
                    .and_then(|bytes| Lookup::decode(&bytes, members.text(LOOKUP_CONSTANT)));
            }
            _ => {}
        }
        let list = members.bytes(VALUE_LIST_REFERENCE);
        let rules = &mut self.validation;
        if let Some(id) = &mut rules.value_list {
            *id = list
                .as_deref()
                .and_then(crate::meta::key_string)
                .unwrap_or(0);
        }
        if let Some(bytes) = &list {
            self.furigana.field = bytes
                .get(FURIGANA_FIELD..)
                .and_then(crate::meta::key_string)
                .unwrap_or(0);
            self.furigana.translation = bytes.get(FURIGANA_TRANSLATION).copied().unwrap_or(0);
        }
        if let Some((from, to)) = &mut rules.range {
            *from = members.text(RANGE_FROM).unwrap_or_default();
            *to = members.text(RANGE_TO).unwrap_or_default();
        }
        if let Some(calculation) = &mut rules.calculation {
            if let Some(formula) = members.formula(VALIDATION_FORMULA) {
                calculation.formula = formula;
            }
        }
        match &mut rules.message {
            Some(Message::Text(text)) => *text = members.text(MESSAGE).unwrap_or_default(),
            Some(Message::Calculation(formula)) => {
                if let Some(read) = members.formula(MESSAGE_FORMULA) {
                    *formula = read;
                }
            }
            None => {}
        }
    }

    /// Write these options into `definition` (26 bytes), keeping every bit
    /// they do not describe. A looked-up value is not written:
    /// [`crate::schema_draft`] refuses to change it. Of furigana the
    /// definition holds the bit alone ([`furigana_member`]).
    pub fn encode(&self, definition: &mut [u8]) {
        assert_eq!(definition.len(), 26, "a field definition has 26 bytes");
        let mut options = word(definition, OPTIONS);
        let value = &self.auto_enter.value;
        if value.editable() {
            // "Always evaluate" belongs to a calculated value: it goes with
            // one the field stops entering, as OpenMaker's record rules
            // read it alone as an unknown option (`rules::RecordPlan`); a
            // field without one keeps it as stored.
            if options & AUTO_CALCULATED != 0 {
                options &= !ALWAYS_EVALUATE;
            }
            options &= !(AUTO_SOURCES | REPLACE_EXISTING);
            // FileMaker Pro writes 0 to byte 3 when neither Creation nor
            // Modification is chosen (E33, E30).
            definition[VARIABLE] = 0;
            match value {
                AutoEnterValue::None => {}
                AutoEnterValue::Creation(entry) => {
                    options |= AUTO_VARIABLE;
                    definition[VARIABLE] = entry.code();
                }
                AutoEnterValue::Modification(entry) => {
                    options |= AUTO_VARIABLE;
                    definition[VARIABLE] = entry.code() + 5;
                }
                AutoEnterValue::Serial(serial) => {
                    options |= if serial.on_commit {
                        SERIAL_ON_COMMIT
                    } else {
                        AUTO_SERIAL
                    };
                    definition[INCREMENT].copy_from_slice(&serial.increment.to_be_bytes());
                }
                AutoEnterValue::LastVisited => options |= AUTO_LAST_VISITED,
                AutoEnterValue::Data(_) => options |= AUTO_DATA,
                AutoEnterValue::Calculated(calculation) => {
                    options |= AUTO_CALCULATED;
                    if calculation.replace_existing {
                        options |= REPLACE_EXISTING;
                    }
                    options &= !ALWAYS_EVALUATE;
                    if !calculation.skip_when_empty {
                        options |= ALWAYS_EVALUATE;
                    }
                }
                AutoEnterValue::LookedUp(_) => options |= AUTO_LOOKUP,
            }
        }
        options &= !PROHIBIT;
        if self.auto_enter.prohibit_modification {
            options |= PROHIBIT;
        }
        options &= !(GLOBAL | VALUE_INDEX | WORD_INDEX | NO_AUTOMATIC_INDEX);
        if self.storage.global {
            options |= GLOBAL;
        }
        options |= match self.storage.indexing {
            Indexing::None => 0,
            Indexing::All if definition[TYPE] == TEXT => VALUE_INDEX | WORD_INDEX,
            Indexing::Minimal | Indexing::All => VALUE_INDEX,
        };
        if !self.storage.automatic_indexes {
            options |= NO_AUTOMATIC_INDEX;
        }
        if self.furigana.on {
            options |= FURIGANA;
        } else {
            options &= !FURIGANA;
        }
        definition[OPTIONS].copy_from_slice(&options.to_be_bytes());

        let rules = &self.validation;
        let mut validation = word(definition, VALIDATION) & !VALIDATION_EDITED;
        let calculation = rules.calculation.as_ref();
        for (on, bit) in [
            (rules.always, 0x400),
            (!rules.allow_override, 0x4),
            (rules.not_empty, 0x8),
            (rules.unique, 0x10),
            (rules.existing, 0x20),
            (rules.maximum_characters.is_some(), 0x200),
            (rules.value_list.is_some(), VALUE_LIST),
            (rules.range.is_some(), IN_RANGE),
            (calculation.is_some(), BY_CALCULATION),
            (
                calculation.is_some_and(|c| !c.only_if_modified),
                CALCULATION_ALWAYS,
            ),
        ] {
            if on {
                validation |= bit;
            }
        }
        // A new message calculation is `0x80` with `0x8000`, as FileMaker
        // Pro 26 writes every custom message (5.9, E34); one stored before
        // keeps the bits it has. A text message is `0x80`.
        match rules.message {
            None => validation &= !(CUSTOM_MESSAGE | MESSAGE_CALCULATION),
            Some(Message::Text(_)) => {
                validation = validation & !MESSAGE_CALCULATION | CUSTOM_MESSAGE;
            }
            Some(Message::Calculation(_)) if validation & MESSAGE_CALCULATION == 0 => {
                validation |= CUSTOM_MESSAGE | MESSAGE_CALCULATION;
            }
            Some(Message::Calculation(_)) => {}
        }
        if let Some(strict) = rules.strict_type {
            validation |= strict.bit();
        }
        definition[VALIDATION].copy_from_slice(&validation.to_be_bytes());
        if let Some(limit) = rules.maximum_characters {
            definition[BYTE_LIMIT].copy_from_slice(&limit.to_be_bytes());
        }
        definition[REPETITIONS].copy_from_slice(&self.storage.repetitions.to_be_bytes());
        definition[LANGUAGE].copy_from_slice(&self.storage.language.to_be_bytes());
    }

    /// The validation bits whose fields the dependency index lists under
    /// `fffc`: Not empty, Unique, Existing and a calculation did (E16,
    /// E35, E36, v09); Always, strict data type, maximum characters, a
    /// value list, a range and a message alone did not (E37, E41, v01,
    /// v02, v08, v19).
    pub const LISTED_VALIDATION: u32 = BY_CALCULATION | 0x8 | 0x10 | 0x20;
    /// Validation bits whose effect on `fffc` is not established.
    pub const UNKNOWN_LISTING: u32 = 0x800;
    /// Validation "Always": a listed field is also listed `fffc, 00, F`
    /// (E34, v15, v17, v18).
    pub const ALWAYS: u32 = 0x400;
}

/// Member keys of the options' parts (schema-path.md 5.5, 5.13).
const FORMULA: u8 = 0x05;
const LOOKUP: u8 = 0x08;
const LOOKUP_CONSTANT: u8 = 0x0b;
const VALUE_LIST_REFERENCE: u8 = 0x07;
const MESSAGE: u8 = 0x18;
const VALIDATION_FORMULA: u8 = 0x19;
const MESSAGE_FORMULA: u8 = 0x20;
const RANGE_FROM: u8 = 0x21;
const RANGE_TO: u8 = 0x22;

/// What [`FieldOptions::read_members`] reads from a field's members `key`:
/// a text (XOR `0x5A`, as `0c` and `0d`), raw bytes, a formula (as `05`).
pub trait OptionMembers {
    fn text(&self, key: u8) -> Option<String>;
    fn bytes(&self, key: u8) -> Option<Vec<u8>>;
    fn formula(&self, key: u8) -> Option<OptionFormula>;
}

/// Member `07` for value list `id` as FileMaker Pro's files hold it
/// (5.13): the list's key as a nested key string in a 6-byte buffer, then
/// `02 01 00` in another (no furigana field) and two bytes, padding zero.
/// `None` for an id whose key does not fit.
pub fn value_list_reference(id: u64) -> Option<Vec<u8>> {
    let key = nested_key(id).filter(|_| id != 0)?;
    let mut out = vec![0; 14];
    out[..key.len()].copy_from_slice(&key);
    out[FURIGANA_FIELD..FURIGANA_FIELD + 3].copy_from_slice(&[0x02, 0x01, 0x00]);
    Some(out)
}

/// `id`'s key as a nested key string, `[n + 1, n, key...]` for its `n`
/// significant bytes, as member `07` holds a value list's and a furigana
/// field's in a 6-byte buffer (5.13). `None` for a key longer than 4 bytes,
/// which does not fit.
fn nested_key(id: u64) -> Option<Vec<u8>> {
    let bytes = id.to_be_bytes();
    let key = &bytes[bytes.iter().take_while(|b| **b == 0).count().min(7)..];
    if key.len() > 4 {
        return None;
    }
    let n = u8::try_from(key.len()).expect("checked above");
    Some([&[n + 1, n][..], key].concat())
}

/// Where member `07` holds the furigana field (a nested key string in a
/// 6-byte buffer) and its translation (one byte), after the value list's
/// buffer (#1301's saves; schema-path.md 5.13).
const FURIGANA_FIELD: usize = 6;
const FURIGANA_TRANSLATION: usize = 12;

/// Member `07` with value list `id` in its first part: the list's nested
/// key string over the start of its buffer, every other byte of `existing`
/// kept (the furigana field and translation, and padding, as Pro keeps a
/// buffer's stale padding), or a new member ([`value_list_reference`]).
/// `None` for an id whose key does not fit, or an `existing` member too
/// short to hold both parts, which is refused rather than replaced.
pub fn value_list_member(existing: Option<&[u8]>, id: u64) -> Option<Vec<u8>> {
    let Some(existing) = existing else {
        return value_list_reference(id);
    };
    let key = nested_key(id).filter(|_| id != 0)?;
    if existing.len() <= FURIGANA_TRANSLATION {
        return None;
    }
    let mut out = existing.to_vec();
    out[..key.len()].copy_from_slice(&key);
    Some(out)
}

/// Member `07` with furigana `field` and `translation` in its second part,
/// as FileMaker Pro 26.0.3 writes them (#1301's saves): the field's nested
/// key string over the start of its buffer and the translation byte, every
/// other byte of `existing` kept, as Pro keeps the buffer's stale padding.
/// With no member, Pro's 13 bytes: no value list (`02 01 00`), padding
/// zero. `None` for a field whose key does not fit, or an `existing`
/// member too short to hold the part.
pub fn furigana_member(existing: Option<&[u8]>, field: u64, translation: u8) -> Option<Vec<u8>> {
    let key = nested_key(field).filter(|_| field != 0)?;
    let mut out = match existing {
        Some(existing) if existing.len() > FURIGANA_TRANSLATION => existing.to_vec(),
        Some(_) => return None,
        None => {
            let mut new = vec![0; FURIGANA_TRANSLATION + 1];
            new[..3].copy_from_slice(&[0x02, 0x01, 0x00]);
            new
        }
    };
    out[FURIGANA_FIELD..FURIGANA_FIELD + key.len()].copy_from_slice(&key);
    out[FURIGANA_TRANSLATION] = translation;
    Some(out)
}

/// The validated-field list of the dependency index `[3, 17, 1, 0e, T]`
/// (schema-path.md 5.9, 5.13). A field's triggers in its field catalog's
/// `fff0` are written by `schema_draft`, and the evaluation lists rebuilt
/// from them by `calc_triggers` (5.10).
pub(crate) mod dependencies {
    use om_hbam::OwnedNode;

    /// `fffc, F`: fields validated Not empty, Unique, Existing or by
    /// calculation.
    pub const VALIDATED: &[u8] = &[0xff, 0xfc];
    /// `fffc, 00, F`: such fields validated Always (5.13).
    const ALWAYS: &[u8] = &[0x00];

    /// List field `field` in, or take it from, its table's `fffc`, and
    /// `fffc, 00` when `always`. A list left empty goes, as a new table has
    /// none (E1).
    pub fn set_validated(entry: &mut OwnedNode, field: &[u8], listed: bool, always: bool) {
        let list = entry.ensure(&[VALIDATED]);
        for (path, on) in [(&[][..], listed), (ALWAYS, listed && always)] {
            let node = if path.is_empty() {
                &mut *list
            } else {
                list.ensure(&[path])
            };
            if on {
                node.set_child(OwnedNode::leaf(field.to_vec(), Vec::new()));
            } else {
                node.children.retain(|c| c.segment != field);
            }
        }
        list.children
            .retain(|c| c.segment != ALWAYS || !c.children.is_empty());
        if list.children.is_empty() {
            entry.children.retain(|c| c.segment != VALIDATED);
        }
    }

    /// Whether `entry` lists field `field` under `fffc`, and under
    /// `fffc, 00`.
    pub fn listing(entry: &OwnedNode, field: &[u8]) -> (bool, bool) {
        (
            entry.at(&[VALIDATED, field]).is_some(),
            entry.at(&[VALIDATED, ALWAYS, field]).is_some(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let digits: String = text.split_whitespace().collect();
        (0..digits.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Each definition FileMaker Pro 26.0.3 wrote in the oracle runs
    /// (schema-path.md 5.8) decodes to the options set in its dialog and
    /// encodes back from the field's previous definition byte for byte.
    #[test]
    fn options_set_in_filemaker_pro_round_trip() {
        let text = "01 01 00 00 00000015 00000000 00000000 00000001 00000000 0001";
        let timestamp = "01 05 00 00 00000015 00000000 00000000 00000001 00000000 0001";
        let number = "01 02 00 00 00000015 00000000 00000000 00000001 00000000 0001";
        let account = "01 01 00 04 00000015 00000101 0000000c 00000001 00000000 0001";
        let modified = "01 01 00 09 00000015 00000101 0000000c 00000001 00000000 0001";
        let cases: Vec<(&str, &str, &str, FieldOptions)> = vec![
            (
                "E11 creation timestamp",
                timestamp,
                "01 05 00 02 00000015 00000001 00000000 00000001 00000000 0001",
                FieldOptions {
                    auto_enter: AutoEnter {
                        value: AutoEnterValue::Creation(EntryValue::Timestamp),
                        prohibit_modification: false,
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E13 indexing All",
                text,
                "01 01 00 00 00000015 a0000000 00000000 00000001 00000000 0001",
                FieldOptions {
                    storage: Storage {
                        indexing: Indexing::All,
                        ..Storage::default()
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E30 data",
                text,
                "01 01 00 00 00000015 00000004 00000000 00000001 00000000 0001",
                FieldOptions {
                    auto_enter: AutoEnter {
                        value: AutoEnterValue::Data("abc".into()),
                        prohibit_modification: false,
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E33 modification off",
                modified,
                "01 01 00 00 00000015 00000100 0000000c 00000001 00000000 0001",
                FieldOptions {
                    auto_enter: AutoEnter {
                        value: AutoEnterValue::None,
                        prohibit_modification: true,
                    },
                    validation: Validation {
                        allow_override: false,
                        not_empty: true,
                        ..Validation::default()
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E37 always, strict, numeric, 20 characters",
                text,
                "01 01 00 00 00000015 00000000 00001604 00000001 00000014 0001",
                FieldOptions {
                    validation: Validation {
                        always: true,
                        allow_override: false,
                        strict_type: Some(StrictType::NumericOnly),
                        maximum_characters: Some(20),
                        ..Validation::default()
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E38 minimal, no automatic indexes",
                text,
                "01 01 00 00 00000015 c0000000 00000000 00000001 00000000 0001",
                FieldOptions {
                    storage: Storage {
                        indexing: Indexing::Minimal,
                        automatic_indexes: false,
                        ..Storage::default()
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E41 serial on commit, 100 by 5, indexed number",
                number,
                "01 02 00 00 00000015 80000200 00000000 00000005 00000000 0001",
                FieldOptions {
                    auto_enter: AutoEnter {
                        value: AutoEnterValue::Serial(Serial {
                            on_commit: true,
                            next: "100".into(),
                            increment: 5,
                        }),
                        prohibit_modification: false,
                    },
                    storage: Storage {
                        indexing: Indexing::All,
                        ..Storage::default()
                    },
                    ..FieldOptions::default()
                },
            ),
            (
                "E43 creation to modification account name",
                account,
                "01 01 00 09 00000015 00000101 0000000c 00000001 00000000 0001",
                FieldOptions {
                    auto_enter: AutoEnter {
                        value: AutoEnterValue::Modification(EntryValue::AccountName),
                        prohibit_modification: true,
                    },
                    validation: Validation {
                        allow_override: false,
                        not_empty: true,
                        ..Validation::default()
                    },
                    ..FieldOptions::default()
                },
            ),
        ];
        for (run, before, after, options) in cases {
            let after = hex(after);
            let next = match &options.auto_enter.value {
                AutoEnterValue::Serial(serial) => Some(serial.next.as_str()),
                _ => None,
            };
            let data = match &options.auto_enter.value {
                AutoEnterValue::Data(data) => Some(data.as_str()),
                _ => None,
            };
            assert_eq!(
                FieldOptions::decode(&after, next, data).as_ref(),
                Some(&options),
                "{run}"
            );
            let mut definition = hex(before);
            options.encode(&mut definition);
            assert_eq!(definition, after, "{run}");
        }
    }

    /// The lookup definitions of schema-path.md 5.5 and 5.13 decode to
    /// their dialog settings, whatever their padding holds, and FileMaker
    /// Pro's native form is written back byte for byte.
    #[test]
    fn lookup_definitions_read_and_write_as_filemaker_stores_them() {
        let native_off = hex("00 01 04 03 d0 00 07 00 02 01 02 00 11 01 00 01 04 03 d0 00 06 00");
        let native_on = hex("00 00 04 03 d0 00 07 00 02 01 02 00 11 01 00 01 04 03 d0 00 06 00");
        let assets = hex("00 00 04 03 d0 00 04 7f 02 01 08 be 33 11 00 01 04 03 d0 00 05 7f");
        let lookup = |to, copied, from, copy_empty, no_match| Lookup {
            from,
            to,
            copied,
            copy_empty,
            no_match,
        };
        let cases = [
            (
                &native_off,
                lookup(0xd00007, 2, 0xd00006, true, NoMatch::DoNotCopy),
            ),
            (
                &native_on,
                lookup(0xd00007, 2, 0xd00006, false, NoMatch::DoNotCopy),
            ),
            (
                &assets,
                lookup(0xd00004, 8, 0xd00005, false, NoMatch::DoNotCopy),
            ),
        ];
        for (bytes, expected) in &cases {
            assert_eq!(Lookup::decode(bytes, None).as_ref(), Some(expected));
        }
        assert_eq!(cases[0].1.encode().as_ref(), Some(&native_off));
        assert_eq!(cases[1].1.encode().as_ref(), Some(&native_on));
        // The engine's runs l02-l04: the no-match choice and the constant.
        let engine = |code: &str| {
            hex(&format!(
                "{code} 04 03 d0 00 02 5e 02 01 05 00 00 00 00 01 04 03 d0 00 04 00"
            ))
        };
        assert_eq!(
            Lookup::decode(&engine("01 01"), None).map(|l| l.no_match),
            Some(NoMatch::NextLower)
        );
        assert_eq!(
            Lookup::decode(&engine("02 00"), None).map(|l| l.no_match),
            Some(NoMatch::NextHigher)
        );
        assert_eq!(
            Lookup::decode(&engine("03 01"), Some("none".into())).map(|l| l.no_match),
            Some(NoMatch::Constant("none".into()))
        );
        assert_eq!(Lookup::decode(&native_on[..21], None), None);
        // A two-byte field key: its padding is not established.
        assert_eq!(
            lookup(0xd00007, 0x8000, 0xd00006, false, NoMatch::DoNotCopy).encode(),
            None
        );
    }

    /// Furigana as FileMaker Pro 26.0.3 saved it on a disposable file
    /// (#1301): Name turned on with Description (field 4) and Full-Width
    /// Katakana, then Status (field 2) and Half-Width Roman, then off. The
    /// bit is `0x40`; member `07`'s second part holds the field and the
    /// translation, with the stale padding (`00 df 4e`) Pro kept.
    #[test]
    fn furigana_reads_and_writes_as_filemaker_pro_saved_it() {
        struct Members(Vec<u8>);
        impl OptionMembers for Members {
            fn text(&self, _: u8) -> Option<String> {
                None
            }
            fn bytes(&self, key: u8) -> Option<Vec<u8>> {
                (key == VALUE_LIST_REFERENCE).then(|| self.0.clone())
            }
            fn formula(&self, _: u8) -> Option<OptionFormula> {
                None
            }
        }
        let off = hex("01 01 00 00 00000015 00000000 00000000 00000001 00000000 0001");
        let on = hex("01 01 00 00 00000015 00000040 00000000 00000001 00000000 0001");
        let first = hex("02 01 00 00 00 00 02 01 04 00 df 4e 02");
        let second = hex("02 01 00 00 00 00 02 01 02 00 df 4e 05");
        let mut options = FieldOptions::decode(&on, None, None).unwrap();
        options.read_members(&Members(first.clone()));
        let read = Furigana {
            on: true,
            field: 4,
            translation: 2,
        };
        assert_eq!(options.furigana, read);
        // Another field and translation: Pro's second save, byte for byte.
        assert_eq!(furigana_member(Some(&first), 2, 5), Some(second.clone()));
        // Turned off: the bit alone, `07` kept (Pro's last save).
        let mut options = FieldOptions::decode(&on, None, None).unwrap();
        options.read_members(&Members(second.clone()));
        options.furigana.on = false;
        let mut definition = on.clone();
        options.encode(&mut definition);
        assert_eq!(definition, off);
        // Turned on: Pro's first save's definition, and a member with zero
        // padding where Pro left stale bytes.
        let mut options = FieldOptions::decode(&off, None, None).unwrap();
        options.furigana = read;
        let mut definition = off.clone();
        options.encode(&mut definition);
        assert_eq!(definition, on);
        assert_eq!(
            furigana_member(None, 4, 2),
            Some(hex("02 01 00 00 00 00 02 01 04 00 00 00 02"))
        );
        // A value list set on the field keeps its furigana part, and the
        // padding past its own key, as Pro keeps padding.
        assert_eq!(
            value_list_member(Some(&second), 3),
            Some(hex("02 01 03 00 00 00 02 01 02 00 df 4e 05"))
        );
        let padded = hex("02 01 01 aa bb cc 02 01 02 00 df 4e 05");
        assert_eq!(
            value_list_member(Some(&padded), 2),
            Some(hex("02 01 02 aa bb cc 02 01 02 00 df 4e 05"))
        );
        // A member too short for both parts is refused, not replaced.
        let short = hex("02 01 03 00 00 00 02 01 02 00 df 4e");
        assert_eq!(value_list_member(Some(&short), 2), None);
        assert_eq!(furigana_member(Some(&short), 4, 2), None);
    }

    #[test]
    fn options_openmaker_does_not_edit_are_kept() {
        // Contacts' PrimaryKey: Get ( UUID ) calculated, replacing, unique,
        // not empty, strict, indexed.
        let primary = hex("01 01 00 00 00000015 80000188 0000001c 00000001 00000000 0001");
        let options = FieldOptions::decode(&primary, None, None).unwrap();
        assert_eq!(
            options.auto_enter.value,
            AutoEnterValue::Calculated(AutoCalculation {
                formula: OptionFormula::default(),
                replace_existing: true,
                skip_when_empty: true,
            })
        );
        let mut definition = primary.clone();
        options.encode(&mut definition);
        assert_eq!(definition, primary);
        // Value list, range, calculation and message bits survive a change
        // to the bits OpenMaker edits.
        let mut kept = hex("01 01 00 00 00000015 00000000 0000814b 00000001 00000000 0001");
        let mut options = FieldOptions::decode(&kept, None, None).unwrap();
        let rules = &options.validation;
        assert!(rules.value_list.is_some() && rules.range.is_some());
        assert!(rules
            .calculation
            .as_ref()
            .is_some_and(|c| !c.only_if_modified));
        assert!(matches!(rules.message, Some(Message::Calculation(_))));
        options.validation.not_empty = false;
        options.encode(&mut kept);
        assert_eq!(&kept[12..16], &hex("00008143")[..]);
        assert_eq!(FieldOptions::decode(&kept[..25], None, None), None);
        // A calculated value turned off takes "always evaluate" with it,
        // which alone would leave the table read-only to OpenMaker.
        let calculated = hex("01 01 00 00 00000015 00000028 00000000 00000001 00000000 0001");
        let mut options = FieldOptions::decode(&calculated, None, None).unwrap();
        options.auto_enter.value = AutoEnterValue::Data("x".into());
        let mut definition = calculated.clone();
        options.encode(&mut definition);
        assert_eq!(&definition[8..12], &hex("00000004")[..]);
    }
}
