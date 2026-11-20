//! The keys a field's indexes hold for a record's stored values
//! (query-program.md 7.1-7.3), as a host builds them: what OpenMaker Server
//! writes into a file it hosts, and what a client keys a find with when the
//! host has no index yet ([`crate::find::ClientFind`]).
//!
//! Every key here is cut as an index holds it, to its first
//! [`om_collate::INDEX_KEY_LIMIT`] bytes (7.1), so that the keys a host
//! stores and the keys its readers compute agree for long values. A
//! record's index entries ([`record_value_entries`], [`word_entries`]) add
//! [`om_collate::TOO_LONG_KEY`] when one of its keys was cut, as FileMaker
//! stores them; readers skip that entry ([`crate::index`]).

use std::collections::BTreeSet;

use om_collate::DateOrder;
use om_hbam::DataType;

/// The value-index key of an empty value, text or typed (7.2, 7.3:
/// `DBMetaTree::KNullEntry`).
pub const EMPTY_TEXT_KEY: &[u8] = &[0x01];

/// The value-index key under which a number, date, time or timestamp index
/// lists a value FileMaker cannot read as one (7.3: `KInvalidEntry`), such as
/// `abc` in a number field or a date stored as `?`. No other key starts with
/// `fe`.
pub const INVALID_KEY: &[u8] = &[0xfe, 0xff];

// An index key must fit a key segment of the file (hbam-writer.md 2.2).
const _: () = assert!(om_collate::INDEX_KEY_LIMIT <= om_hbam::writer::MAX_SEGMENT);

/// `key` as an index holds it, and whether it was cut to do so.
fn held(mut key: Vec<u8>) -> (Vec<u8>, bool) {
    let cut = om_collate::cut_index_key(&mut key);
    (key, cut)
}

/// Keys as an index holds them: their set, and whether one was cut.
#[derive(Default)]
struct Held {
    keys: BTreeSet<Vec<u8>>,
    cut: bool,
}

impl Held {
    fn add(&mut self, key: Vec<u8>) {
        let (key, cut) = held(key);
        self.cut |= cut;
        self.keys.insert(key);
    }

    /// The keys with [`om_collate::TOO_LONG_KEY`] when one was cut, but
    /// in Unicode raw, which FileMaker never flags (7.1).
    fn entries(mut self, text: TextKeys) -> BTreeSet<Vec<u8>> {
        if self.cut && text != TextKeys::Raw {
            self.keys.insert(om_collate::TOO_LONG_KEY.to_vec());
        }
        self.keys
    }
}

/// How a text field's indexes key text: the form its index language
/// (`Locale::Type`, field definition bytes 4..7) gives (query-program.md
/// 7.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextKeys {
    /// Language 0: no key at all.
    None,
    /// English (21) and English international (77): `01`, then the
    /// DUCET 5.2.0 primary weight of each collation element
    /// ([`om_collate::text_key`]).
    #[default]
    Collated,
    /// System (1) and Unicode standard (3): the collated form, `00 00` and
    /// the secondary weights ([`om_collate::padded_text_key`]).
    CollatedPadded,
    /// Unicode raw (2): `01`, then the UTF-16BE code units.
    Raw,
    /// Japanese (31): `01`, then the Japanese tailoring's weights for kana
    /// and the kanji of JIS X 0208 level 1, the table's for every other
    /// character ([`om_collate::japanese_key`]). A text holding a kanji
    /// outside level 1 has no key in it ([`Self::key`] is `None`): a writer
    /// or a check needing that text's key must refuse it, not skip it.
    /// Not yet the form of [`Self::of_language`] 31, so no reader or writer
    /// uses it (#299).
    Japanese,
}

impl TextKeys {
    /// The form of index language `language`. `None` for the languages
    /// that tailor the table (German, Japanese and the other localized
    /// ones), whose keys are not established.
    pub fn of_language(language: u32) -> Option<TextKeys> {
        match language {
            0 => Some(TextKeys::None),
            21 | 77 => Some(TextKeys::Collated),
            1 | 3 => Some(TextKeys::CollatedPadded),
            2 => Some(TextKeys::Raw),
            _ => None,
        }
    }

    /// The key of one line or word as an index holds it: its first
    /// [`om_collate::INDEX_KEY_LIMIT`] bytes (7.1).
    pub fn key(self, text: &str) -> Option<Vec<u8>> {
        self.whole_key(text).map(|key| held(key).0)
    }

    /// The whole key of one line or word, before an index cuts it: what
    /// tells apart values that share a cut key.
    pub fn whole_key(self, text: &str) -> Option<Vec<u8>> {
        match self {
            TextKeys::None => None,
            TextKeys::Collated => Some(om_collate::text_key(text)),
            TextKeys::CollatedPadded => Some(om_collate::padded_text_key(text)),
            TextKeys::Raw => Some(
                std::iter::once(0x01)
                    .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                    .collect(),
            ),
            TextKeys::Japanese => om_collate::japanese_key(text),
        }
    }
}

/// The value-index keys of a stored value (query-program.md 7.2, 7.3): one
/// per non-empty line of a text value in the `text` form (the empty text's
/// key for empty text); for a number, date, time or timestamp its typed key,
/// [`EMPTY_TEXT_KEY`] when empty and [`INVALID_KEY`] when it is not one
/// ([`typed_value_keys`]). Each is cut as the index holds it (7.1).
pub fn value_keys(
    data_type: DataType,
    value: &str,
    order: DateOrder,
    text: TextKeys,
) -> Vec<Vec<u8>> {
    whole_value_keys(data_type, value, order, text)
        .into_iter()
        .map(|key| held(key).0)
        .collect()
}

/// [`value_keys`] before the index cuts them.
fn whole_value_keys(
    data_type: DataType,
    value: &str,
    order: DateOrder,
    text: TextKeys,
) -> Vec<Vec<u8>> {
    match data_type {
        DataType::Text => {
            let mut keys: Vec<Vec<u8>> = value
                .split(['\r', '\n'])
                .filter(|line| !line.is_empty())
                .filter_map(|line| text.whole_key(line))
                .collect();
            if keys.is_empty() {
                keys.extend(text.whole_key(""));
            }
            keys
        }
        DataType::Number | DataType::Date | DataType::Time | DataType::Timestamp => {
            typed_value_keys(data_type, value, order)
        }
        DataType::Container | DataType::Unknown(_) => Vec::new(),
    }
}

/// The value-index keys of a number, date, time or timestamp value
/// (query-program.md 7.3):
///
/// - an empty value: [`EMPTY_TEXT_KEY`];
/// - a value read as one: its key ([`om_collate::number_key`] and the
///   others);
/// - a timestamp stored as a date alone, as FileMaker stores one whose time
///   was invalid: the date's midnight key and [`INVALID_KEY`];
/// - a value with no digit at all (`abc`, `N/A`, a date FileMaker stored as
///   `?`): [`INVALID_KEY`];
/// - other text, digits among other characters (`abc12`, `1,234`), which
///   FileMaker reads by rules not built here: no key.
pub fn typed_value_keys(data_type: DataType, value: &str, order: DateOrder) -> Vec<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return vec![EMPTY_TEXT_KEY.to_vec()];
    }
    let key = match data_type {
        DataType::Number => om_collate::number_key(trimmed),
        DataType::Date => om_collate::date_key(trimmed, order),
        DataType::Time => om_collate::time_key(trimmed),
        DataType::Timestamp => match om_collate::timestamp_key(trimmed, order) {
            Some(key) => Some(key),
            None => match om_collate::date_key(trimmed, order) {
                // The date's key is also the timestamp key of its midnight.
                Some(date) => return vec![date, INVALID_KEY.to_vec()],
                None => None,
            },
        },
        _ => None,
    };
    match key {
        Some(key) => vec![key],
        None if !trimmed.bytes().any(|b| b.is_ascii_digit()) => vec![INVALID_KEY.to_vec()],
        None => Vec::new(),
    }
}

/// A record's value-index keys for one field: those of each repetition's
/// value, or of the empty value when every repetition is empty (an
/// inference for repeating fields: a find matches any repetition). Each is
/// cut as the index holds it (7.1).
pub fn record_value_keys(
    data_type: DataType,
    texts: &[String],
    order: DateOrder,
    text: TextKeys,
) -> BTreeSet<Vec<u8>> {
    held_value_keys(data_type, texts, order, text).keys
}

/// A record's entries in a field's value index, as a host stores them:
/// [`record_value_keys`] and, when one of them was cut,
/// [`om_collate::TOO_LONG_KEY`] (7.1).
pub fn record_value_entries(
    data_type: DataType,
    texts: &[String],
    order: DateOrder,
    text: TextKeys,
) -> BTreeSet<Vec<u8>> {
    held_value_keys(data_type, texts, order, text).entries(text)
}

fn held_value_keys(
    data_type: DataType,
    texts: &[String],
    order: DateOrder,
    text: TextKeys,
) -> Held {
    let filled: Vec<&String> = texts.iter().filter(|t| !t.is_empty()).collect();
    let empty = String::new();
    let values = if filled.is_empty() {
        vec![&empty]
    } else {
        filled
    };
    let mut held = Held::default();
    for value in values {
        for key in whole_value_keys(data_type, value, order, text) {
            held.add(key);
        }
    }
    held
}

/// A record's word-index keys for one field: every word of every
/// repetition (query-program.md 7.3), in the `text` form, each cut as the
/// index holds it (7.1).
pub fn word_keys(texts: &[String], text: TextKeys) -> BTreeSet<Vec<u8>> {
    held_word_keys(texts, text).keys
}

/// A record's entries in a field's word index, as a host stores them:
/// [`word_keys`] and, when one of them was cut,
/// [`om_collate::TOO_LONG_KEY`] (7.1).
pub fn word_entries(texts: &[String], text: TextKeys) -> BTreeSet<Vec<u8>> {
    held_word_keys(texts, text).entries(text)
}

fn held_word_keys(texts: &[String], text: TextKeys) -> Held {
    let mut held = Held::default();
    for word in texts.iter().flat_map(|value| om_collate::words(value)) {
        if let Some(key) = text.whole_key(word) {
            held.add(key);
        }
    }
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_values_are_keyed_per_line() {
        let order = DateOrder::MonthDayYear;
        assert_eq!(
            value_keys(DataType::Text, "", order, TextKeys::Collated),
            [EMPTY_TEXT_KEY]
        );
        assert_eq!(
            value_keys(DataType::Text, "Test", order, TextKeys::Collated),
            [om_collate::text_key("Test")]
        );
        assert_eq!(
            value_keys(DataType::Text, "a\rb", order, TextKeys::Collated).len(),
            2
        );
        assert_eq!(
            value_keys(DataType::Number, "", order, TextKeys::Collated),
            [EMPTY_TEXT_KEY]
        );
        // query-program.md 7.1: a key's first 219 bytes, and `ff 80`.
        let english = TextKeys::Collated;
        let texts = |value: &str| [value.to_string()];
        let long = "x".repeat(300);
        let cut = om_collate::text_key(&"x".repeat(109));
        let too_long = om_collate::TOO_LONG_KEY.to_vec();
        let value = |v: &str| record_value_entries(DataType::Text, &texts(v), order, english);
        assert_eq!(value(&long), [cut.clone(), too_long.clone()].into());
        assert_eq!(
            record_value_keys(DataType::Text, &texts(&long), order, english),
            [cut.clone()].into()
        );
        assert_eq!(value(&"x".repeat(109)), [cut.clone()].into());
        assert_eq!(
            value(&format!("short\r{long}")),
            [om_collate::text_key("short"), cut.clone(), too_long.clone()].into()
        );
        assert_eq!(
            word_entries(&texts(&format!("a {long}")), english),
            [om_collate::text_key("a"), cut.clone(), too_long].into()
        );
        assert_eq!(value_keys(DataType::Text, &long, order, english), [cut]);
        // Unicode raw keeps 109 code units and is never flagged.
        let raw = TextKeys::Raw.key(&long).unwrap();
        assert_eq!(raw.len(), om_collate::INDEX_KEY_LIMIT);
        let raw_value = record_value_entries(DataType::Text, &texts(&long), order, TextKeys::Raw);
        assert_eq!(raw_value, [raw.clone()].into());
        assert_eq!(word_entries(&texts(&long), TextKeys::Raw), [raw].into());
        let padded = TextKeys::CollatedPadded.key(&long).unwrap();
        assert_eq!(padded.len(), om_collate::INDEX_KEY_LIMIT);
        // A number key keeps 200 digits (7.3).
        let fraction = format!("0.{}", "1".repeat(500));
        assert_eq!(
            value_keys(DataType::Number, &fraction, order, english)[0].len(),
            2 + 100
        );
    }

    #[test]
    fn typed_keys_keep_the_order_of_their_values() {
        // Byte order is value order (7.3), empty values first and values
        // FileMaker cannot read last.
        let number = |n: &str| typed_value_keys(DataType::Number, n, DateOrder::MonthDayYear);
        let ordered = [
            "", "-100", "-25", "-2.51", "-2.5", "-2", "-1.25", "-1.2", "-0.5", "-0.05", "0",
            "0.05", "0.5", "1", "2", "2.5", "2.51", "3", "10", "10.5", "25", "100", "N/A",
        ];
        let keys: Vec<Vec<u8>> = ordered.iter().map(|n| number(n).remove(0)).collect();
        for (pair, numbers) in keys.windows(2).zip(ordered.windows(2)) {
            assert!(pair[0] < pair[1], "{:?} < {:?}", numbers[0], numbers[1]);
        }
        // Digits among other text are read by rules not built here.
        assert!(number("abc12").is_empty());
    }
}
