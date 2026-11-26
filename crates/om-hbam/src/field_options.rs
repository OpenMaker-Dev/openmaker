//! Read-only metadata from a field's fixed 26-byte definition,
//! `<table>/3/5/<field>/2` (`docs/re/auto-entry.md`, Fixed field definition).
//!
//! This describes auto-entry requirements; it does not evaluate calculations,
//! reserve serial numbers, validate records or imply that raw server uploads
//! run the desktop client's auto-entry engine. See `docs/re/auto-entry.md`.

/// The selected built-in auto-entry variable, even when its option is disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryVariable {
    CreationDate,
    CreationTime,
    CreationTimestamp,
    CreationUserName,
    CreationAccountName,
    ModificationDate,
    ModificationTime,
    ModificationTimestamp,
    ModificationUserName,
    ModificationAccountName,
    Unknown(u8),
}

impl EntryVariable {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::CreationDate,
            1 => Self::CreationTime,
            2 => Self::CreationTimestamp,
            3 => Self::CreationUserName,
            4 => Self::CreationAccountName,
            5 => Self::ModificationDate,
            6 => Self::ModificationTime,
            7 => Self::ModificationTimestamp,
            8 => Self::ModificationUserName,
            9 => Self::ModificationAccountName,
            other => Self::Unknown(other),
        }
    }

    pub fn is_modification(self) -> Option<bool> {
        match self {
            Self::CreationDate
            | Self::CreationTime
            | Self::CreationTimestamp
            | Self::CreationUserName
            | Self::CreationAccountName => Some(false),
            Self::ModificationDate
            | Self::ModificationTime
            | Self::ModificationTimestamp
            | Self::ModificationUserName
            | Self::ModificationAccountName => Some(true),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldOptions {
    /// Byte 0: normal 1, calculation 2, summary 3. Other values remain raw.
    pub field_kind: u8,
    /// Byte 1: the same type identifier decoded by `catalog::DataType`.
    pub data_type: u8,
    pub entry_variable: EntryVariable,
    /// Big-endian word at offset 8. Unknown option bits remain intact.
    pub options: u32,
    /// Big-endian validation word at offset 12; interpretation is separate.
    pub validation_options: u32,
    pub max_repetitions: u16,
    /// The whole original definition, including fields not interpreted here.
    pub raw: [u8; 26],
}

impl FieldOptions {
    /// Reject unrecognized sizes rather than interpreting a partial/new format.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let raw: [u8; 26] = bytes.try_into().ok()?;
        Some(Self {
            field_kind: raw[0],
            data_type: raw[1],
            entry_variable: EntryVariable::from_byte(raw[3]),
            options: u32::from_be_bytes(raw[8..12].try_into().ok()?),
            validation_options: u32::from_be_bytes(raw[12..16].try_into().ok()?),
            max_repetitions: u16::from_be_bytes(raw[24..26].try_into().ok()?),
            raw,
        })
    }

    /// A zero variable code means CreationDate, not "no auto-entry". Option
    /// bit 0 and the normal field kind decide whether the variable is active.
    pub fn auto_entry_variable(&self) -> Option<EntryVariable> {
        (self.field_kind == 1 && self.options & 1 != 0).then_some(self.entry_variable)
    }

    /// An auto-entered serial number, generated on creation (`0x2`) or on
    /// commit (`0x200`, set instead of `0x2`; schema-path.md 5.9, E41).
    pub fn auto_entry_serial(&self) -> bool {
        self.field_kind == 1 && self.options & 0x202 != 0
    }

    /// The serial number's increment (`mSerialIncrement`, offset 16;
    /// schema-path.md 5.5).
    pub fn serial_increment(&self) -> u32 {
        u32::from_be_bytes([self.raw[16], self.raw[17], self.raw[18], self.raw[19]])
    }

    pub fn auto_entry_constant(&self) -> bool {
        self.field_kind == 1 && self.options & 4 != 0
    }

    pub fn auto_entry_calculation(&self) -> bool {
        self.field_kind == 1 && self.options & 8 != 0
    }

    /// A measured local Container definition awaiting transfer from
    /// external to embedded storage. Existing cells can still refer to
    /// external data (containers.md 8.1).
    pub fn pending_embedded_container_transfer(&self) -> bool {
        self.field_kind == 1
            && self.data_type == 6
            && matches!(
                (self.options, self.max_repetitions),
                (0x6000, 1) | (0x4000, 24)
            )
    }

    pub fn calculation_replaces_existing(&self) -> bool {
        self.auto_entry_calculation() && self.options & 0x80 != 0
    }

    /// FileMaker's auto-entry "Prohibit modification" option. This does not
    /// describe account privileges or authorize an engine to bypass validation.
    pub fn auto_entry_editing_prohibited(&self) -> bool {
        self.field_kind == 1 && self.options & 0x100 != 0
    }

    /// The language the field's indexes key text in: 21 English, 77 English
    /// international (query-program.md 7.2).
    pub fn index_language(&self) -> u32 {
        u32::from_be_bytes([self.raw[4], self.raw[5], self.raw[6], self.raw[7]])
    }

    /// Whether the field has a value index, `<table>/1/<field>/0`
    /// (schema-path.md 5.5; auto-entry.md, Index bits).
    pub fn value_indexed(&self) -> bool {
        self.options & 0x8000_0000 != 0
    }

    /// Whether the field has a word index, `<table>/1/<field>/1`.
    pub fn word_indexed(&self) -> bool {
        self.options & 0x2000_0000 != 0
    }

    pub fn validation_not_empty(&self) -> bool {
        self.validation_options & 8 != 0
    }

    pub fn validation_unique(&self) -> bool {
        self.validation_options & 0x10 != 0
    }

    pub fn validation_strict(&self) -> bool {
        self.validation_options & 4 != 0
    }

    /// A calculation whose result is not stored (`kNoStorage`,
    /// schema-path.md 5.5): it has no index, so it cannot match records.
    pub fn unstored_calculation(&self) -> bool {
        self.field_kind == 2 && self.options & 0x2_0000 != 0
    }
}

/// A complete, single Get() expression in an unformatted calculation stream.
/// This is metadata recognition, not a general calculation evaluator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimpleGetSelector {
    CurrentDate,
    CurrentTime,
    UserName,
    AccountName,
    ScriptParameter,
    CurrentTimestamp,
    Uuid,
    Unknown(u8),
}

impl SimpleGetSelector {
    pub fn decode(code: &[u8]) -> Option<Self> {
        let [0x9b, selector] = *code else {
            return None;
        };
        Some(match selector {
            2 => Self::CurrentDate,
            29 => Self::CurrentTime,
            31 => Self::UserName,
            32 => Self::AccountName,
            62 => Self::ScriptParameter,
            63 => Self::CurrentTimestamp,
            101 => Self::Uuid,
            other => Self::Unknown(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition(variable: u8, options: u32) -> [u8; 26] {
        let mut bytes = [0; 26];
        bytes[0] = 1;
        bytes[1] = 1;
        bytes[3] = variable;
        bytes[8..12].copy_from_slice(&options.to_be_bytes());
        bytes[24..26].copy_from_slice(&1u16.to_be_bytes());
        bytes
    }

    #[test]
    fn auto_entry_is_enabled_by_options_not_the_zero_default_variable() {
        let inactive = FieldOptions::decode(&definition(0, 0)).unwrap();
        assert_eq!(inactive.entry_variable, EntryVariable::CreationDate);
        assert_eq!(inactive.auto_entry_variable(), None);
        let created = FieldOptions::decode(&definition(4, 0x101)).unwrap();
        assert_eq!(
            created.auto_entry_variable(),
            Some(EntryVariable::CreationAccountName)
        );
        let modified = FieldOptions::decode(&definition(7, 0x101)).unwrap();
        assert_eq!(
            modified.auto_entry_variable(),
            Some(EntryVariable::ModificationTimestamp)
        );
        assert_eq!(modified.entry_variable.is_modification(), Some(true));
        assert!(!modified.auto_entry_calculation());
    }

    #[test]
    fn index_bits_and_language() {
        // test project's Company and PrimaryKey (value and word index), First
        // Name (value index only), Title (none); all English (21).
        let mut bytes = definition(0, 0xa000_0000);
        bytes[4..8].copy_from_slice(&21u32.to_be_bytes());
        let both = FieldOptions::decode(&bytes).unwrap();
        assert!(both.value_indexed() && both.word_indexed());
        assert_eq!(both.index_language(), 21);
        let value = FieldOptions::decode(&definition(0, 0x8000_0000)).unwrap();
        assert!(value.value_indexed() && !value.word_indexed());
        let none = FieldOptions::decode(&definition(0, 0)).unwrap();
        assert!(!none.value_indexed() && !none.word_indexed());
    }

    #[test]
    fn calculation_flags_unknown_bits_and_unrecognized_formats_are_preserved() {
        let mut bytes = definition(19, 0x8000_0188);
        bytes[12..16].copy_from_slice(&0x1cu32.to_be_bytes());
        let options = FieldOptions::decode(&bytes).unwrap();
        assert!(options.auto_entry_calculation());
        assert!(options.calculation_replaces_existing());
        assert!(options.auto_entry_editing_prohibited());
        assert_eq!(options.auto_entry_variable(), None);
        assert_eq!(options.entry_variable, EntryVariable::Unknown(19));
        assert_eq!(options.validation_options, 0x1c);
        assert!(options.validation_not_empty());
        assert!(options.validation_unique());
        assert!(options.validation_strict());
        assert_eq!(options.raw, bytes);
        assert_eq!(FieldOptions::decode(&bytes[..25]), None);
        let mut calculated_field = definition(4, 1);
        calculated_field[0] = 2;
        assert_eq!(
            FieldOptions::decode(&calculated_field)
                .unwrap()
                .auto_entry_variable(),
            None
        );
        let mut unvalidated = definition(0, 0x80);
        unvalidated[12..16].copy_from_slice(&0x2000u32.to_be_bytes());
        let unvalidated = FieldOptions::decode(&unvalidated).unwrap();
        assert!(!unvalidated.calculation_replaces_existing());
        assert!(!unvalidated.auto_entry_editing_prohibited());
        assert!(!unvalidated.validation_not_empty());
        assert!(!unvalidated.validation_unique());
        assert!(!unvalidated.validation_strict());
    }

    #[test]
    fn only_whole_single_get_expressions_are_classified() {
        assert_eq!(
            SimpleGetSelector::decode(&[0x9b, 0x65]),
            Some(SimpleGetSelector::Uuid)
        );
        assert_eq!(
            SimpleGetSelector::decode(&[0x9b, 0x20]),
            Some(SimpleGetSelector::AccountName)
        );
        assert_eq!(
            SimpleGetSelector::decode(&[0x9b, 0x3f]),
            Some(SimpleGetSelector::CurrentTimestamp)
        );
        assert_eq!(
            SimpleGetSelector::decode(&[0x9b, 0xff]),
            Some(SimpleGetSelector::Unknown(0xff))
        );
        assert_eq!(SimpleGetSelector::decode(&[0x9b]), None);
        assert_eq!(SimpleGetSelector::decode(&[0x9b, 0x65, 0]), None);
    }
}
