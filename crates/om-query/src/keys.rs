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

/// The value-index key of an empty text value (7.2).
pub const EMPTY_TEXT_KEY: &[u8] = &[0x01];

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKeys {
    /// Language 0: no key at all.
    None,
    /// English (21) and English international (77): `01`, then the
    /// DUCET 5.2.0 primary weight of each collation element
    /// ([`om_collate::text_key`]).
    Collated,
    /// System (1) and Unicode standard (3): the collated form, `00 00` and
    /// the secondary weights ([`om_collate::padded_text_key`]).
    CollatedPadded,
    /// Unicode raw (2): `01`, then the UTF-16BE code units.
    Raw,
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
        }
    }
}

/// The value-index keys of a stored value (query-program.md 7.2, 7.3): one
/// per non-empty line of a text value in the `text` form (the empty text's
/// key for empty text), the typed key of a number, date, time or timestamp
/// (none when empty or not understood). Each is cut as the index holds it
/// (7.1).
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
        _ if value.trim().is_empty() => Vec::new(),
        DataType::Number => number_key(value).into_iter().collect(),
        DataType::Date => om_collate::date_key(value, order).into_iter().collect(),
        DataType::Time => om_collate::time_key(value).into_iter().collect(),
        DataType::Timestamp => om_collate::timestamp_key(value, order)
            .into_iter()
            .collect(),
        DataType::Container | DataType::Unknown(_) => Vec::new(),
    }
}

/// A number's value-index key: FileMaker's form for whole numbers from 0
/// (query-program.md 7.3), and OpenMaker's own order-keeping forms for the
/// values whose form is still open there, so that finds and ranges see
/// them: a fraction appends its digits to the integer digits
/// (the count stays the integer digits', `2.5` = `80 01 36`, `0.5` =
/// `80 00 60`); a negative number is `7f`, the count and the digit nibbles
/// complemented, then `ff` (`-2` = `7f fe cf ff`), below every
/// non-negative key and in reverse order of magnitude. Other text has no
/// key.
pub fn number_key(value: &str) -> Option<Vec<u8>> {
    let text = value.trim();
    if let Some(key) = om_collate::number_key(text) {
        return Some(key);
    }
    let (negative, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (integer, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let integer = integer.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    let count = u8::try_from(integer.len()).ok().filter(|c| *c < 0x7f)?;
    let nibbles: Vec<u8> = integer
        .bytes()
        .chain(fraction.bytes())
        .map(|d| d - b'0' + 1)
        .collect();
    if !negative || nibbles.is_empty() {
        let mut key = vec![0x80, count];
        for pair in nibbles.chunks(2) {
            key.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0));
        }
        return Some(key);
    }
    let mut key = vec![0x7f, 0xff - count];
    for pair in nibbles.chunks(2) {
        let low = pair.get(1).map_or(0xf, |n| 0xf - n);
        key.push((0xf - pair[0]) << 4 | low);
    }
    key.push(0xff);
    Some(key)
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
        assert!(value_keys(DataType::Number, "", order, TextKeys::Collated).is_empty());
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
        let fraction = format!("0.{}", "1".repeat(500));
        assert_eq!(
            value_keys(DataType::Number, &fraction, order, english)[0].len(),
            om_collate::INDEX_KEY_LIMIT
        );
    }

    #[test]
    fn number_keys_keep_the_order_of_fractions_and_negative_numbers() {
        assert_eq!(number_key("12"), om_collate::number_key("12"));
        assert_eq!(number_key("2.5").unwrap(), [0x80, 0x01, 0x36]);
        assert_eq!(number_key("2.50"), number_key("2.5"));
        assert_eq!(number_key("2.0"), number_key("2"));
        assert_eq!(number_key("-0"), number_key("0"));
        assert_eq!(number_key("-2").unwrap(), [0x7f, 0xfe, 0xcf, 0xff]);
        assert_eq!(number_key("abc"), None);
        assert_eq!(number_key("1-2"), None);
        let ordered = [
            "-100", "-25", "-2.51", "-2.5", "-2", "-0.5", "-0.05", "0", "0.05", "0.5", "1", "2",
            "2.5", "2.51", "3", "10", "10.5", "25", "100",
        ];
        let keys: Vec<Vec<u8>> = ordered.iter().map(|n| number_key(n).unwrap()).collect();
        for (pair, numbers) in keys.windows(2).zip(ordered.windows(2)) {
            assert!(pair[0] < pair[1], "{} < {}", numbers[0], numbers[1]);
        }
    }
}
