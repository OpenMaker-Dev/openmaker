//! FileMaker's text index keys and word breaking (spec
//! `docs/re/query-program.md` 7.1, 7.2).
//!
//! A text value's index key in the English form (index languages English and
//! English international) is `01` followed by the primary weight of each of
//! its collation elements in the Unicode Collation Algorithm's default table
//! (DUCET) of UCA 5.2.0, each a big-endian u16. Variable elements (spaces,
//! punctuation) keep their weights; elements with a zero primary (combining
//! marks, controls) add nothing, which makes keys ignore case and accents.
//! The table ships as `data/allkeys-5.2.0.txt` from unicode.org, under
//! `data/LICENSE-UNICODE`.
//!
//! The word index keys each word of a value the same way; [`words`] splits a
//! value as FileMaker does. [`name_key`] is the key a catalog's name list
//! holds a member's name under (spec `docs/re/schema-path.md` 5.6), with
//! the levels above the primary one. Numbers, dates, times and timestamps have keys of
//! their own ([`number_key`], [`date_key`], [`time_key`], [`timestamp_key`]).
//!
//! An index holds at most the first [`INDEX_KEY_LIMIT`] bytes of a key and
//! lists each record whose key it cut under [`TOO_LONG_KEY`] as well
//! (spec 7.1); [`cut_index_key`] and [`text_index_key`] give a key as an
//! index holds it. [`text_key`] is the whole key, for comparing values.

use std::collections::HashMap;
use std::sync::OnceLock;

mod names;
mod typed;

pub use names::{name_key, Levels, Unkeyed};
pub use typed::{date_key, number_key, time_key, timestamp_key, DateOrder};

/// The first byte of every text key.
pub const TEXT_KEY_PREFIX: u8 = 0x01;

/// The longest key a field index holds (spec 7.1): FileMaker keeps the
/// first 219 bytes of a longer key, which is also the longest key segment
/// a file can hold (hbam-writer.md 2.2).
pub const INDEX_KEY_LIMIT: usize = 219;

/// The key under which a field index also lists every record whose key (a
/// line's, or a word's in the word index) it cut to [`INDEX_KEY_LIMIT`]
/// (spec 7.1). It is not the key of any value.
pub const TOO_LONG_KEY: [u8; 2] = [0xff, 0x80];

const ALLKEYS: &str = include_str!("../data/allkeys-5.2.0.txt");

/// A DUCET 5.2.0 collation element: its primary, secondary and tertiary
/// weights (the fourth field of `allkeys.txt` is not kept).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Element {
    pub primary: u16,
    pub secondary: u16,
    pub tertiary: u16,
}

impl Element {
    /// A completely ignorable element.
    pub const IGNORABLE: Element = Element {
        primary: 0,
        secondary: 0,
        tertiary: 0,
    };
}

/// DUCET 5.2.0's collation elements: per code point, and per contraction (a
/// sequence of code points with its own elements).
struct Table {
    singles: HashMap<u32, Box<[Element]>>,
    contractions: HashMap<Box<[u32]>, Box<[Element]>>,
    longest_contraction: usize,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table {
            singles: HashMap::new(),
            contractions: HashMap::new(),
            longest_contraction: 1,
        };
        let weight = |field: &str| u16::from_str_radix(field, 16).expect("DUCET weight");
        for line in ALLKEYS.lines() {
            // `0061  ; [.15EF.0020.0002.0061] # LATIN SMALL LETTER A`, with
            // `*` instead of `.` marking a variable element.
            let line = line.split('#').next().unwrap_or_default();
            let Some((code_points, elements)) = line.split_once(';') else {
                continue;
            };
            let code_points: Vec<u32> = code_points
                .split_whitespace()
                .map(|c| u32::from_str_radix(c, 16).expect("DUCET code point"))
                .collect();
            let elements: Box<[Element]> = elements
                .split('[')
                .skip(1)
                .map(|element| Element {
                    primary: weight(&element[1..5]),
                    secondary: weight(&element[6..10]),
                    tertiary: weight(&element[11..15]),
                })
                .collect();
            match code_points.as_slice() {
                [] => {}
                [single] => {
                    table.singles.insert(*single, elements);
                }
                sequence => {
                    table.longest_contraction = table.longest_contraction.max(sequence.len());
                    table.contractions.insert(sequence.into(), elements);
                }
            }
        }
        table
    })
}

/// The implicit primary weights UCA 5.2.0 gives a code point the table does
/// not list (UTS #10, 7.1.3): CJK ideographs and unassigned code points sort
/// by code point after everything listed.
fn implicit_weights(c: u32) -> [u16; 2] {
    const CJK_COMPATIBILITY_IDEOGRAPHS: [u32; 12] = [
        0xFA0E, 0xFA0F, 0xFA11, 0xFA13, 0xFA14, 0xFA1F, 0xFA21, 0xFA23, 0xFA24, 0xFA27, 0xFA28,
        0xFA29,
    ];
    let base: u32 = match c {
        0x4E00..=0x9FCB => 0xFB40,
        _ if CJK_COMPATIBILITY_IDEOGRAPHS.contains(&c) => 0xFB40,
        0x3400..=0x4DB5 | 0x20000..=0x2A6D6 | 0x2A700..=0x2B734 => 0xFB80,
        _ => 0xFBC0,
    };
    // Both values fit in 16 bits: `c >> 15` is at most 0x21 for a code point.
    [(base + (c >> 15)) as u16, ((c & 0x7FFF) | 0x8000) as u16]
}

/// The index key of `text` in the English form (spec 7.2). The empty string
/// gives `01`. Curly quotation marks key as the straight ones
/// ([`index_char`]).
pub fn text_key(text: &str) -> Vec<u8> {
    let folded: String = text.chars().map(index_char).collect();
    let weights = primary_weights(&folded);
    let mut key = Vec::with_capacity(1 + 2 * weights.len());
    key.push(TEXT_KEY_PREFIX);
    for weight in weights {
        key.extend(weight.to_be_bytes());
    }
    key
}

/// Cut `key` to what a field index holds of it, its first
/// [`INDEX_KEY_LIMIT`] bytes (spec 7.1). Returns whether it was longer: the
/// index then also lists the record under [`TOO_LONG_KEY`]. Every key that
/// meets an index must be cut here, or the index and its readers disagree
/// about long values.
pub fn cut_index_key(key: &mut Vec<u8>) -> bool {
    let cut = key.len() > INDEX_KEY_LIMIT;
    key.truncate(INDEX_KEY_LIMIT);
    cut
}

/// [`text_key`] as a field index holds it ([`cut_index_key`]).
pub fn text_index_key(text: &str) -> Vec<u8> {
    let mut key = text_key(text);
    cut_index_key(&mut key);
    key
}

/// The character an index key reads for `c`: FileMaker's index keys (and
/// `UniqueValues`) take the single curly quotation marks U+2018..U+201B as
/// `'` and the double ones U+201C..U+201F as `"`, which sorting keeps
/// apart.
pub fn index_char(c: char) -> char {
    match c {
        '\u{2018}'..='\u{201B}' => '\'',
        '\u{201C}'..='\u{201F}' => '"',
        other => other,
    }
}

/// The DUCET 5.2.0 primary weights of `text`'s collation elements in order:
/// contractions first, implicit weights for unlisted code points, elements
/// with a zero primary left out. [`text_key`] is these weights after `01`.
pub fn primary_weights(text: &str) -> Vec<u16> {
    collation_elements(text)
        .into_iter()
        .map(|element| element.primary)
        .filter(|&primary| primary != 0)
        .collect()
}

/// The DUCET 5.2.0 collation elements of `text` in order, ignorable ones
/// included: contractions first (the longest that matches), and for a code
/// point the table does not list its two implicit elements
/// (`[.AAAA.0020.0002][.BBBB.0000.0000]`, UTS #10 7.1.3). As in
/// FileMaker, the noncharacters U+FFFE and U+FFFF are completely ignorable
/// and the curly quotation marks sort as one single and one double mark
/// (U+2018..U+201B as U+2018, U+201C..U+201F as U+201C).
pub fn collation_elements(text: &str) -> Vec<Element> {
    let table = table();
    let code_points: Vec<u32> = text
        .chars()
        .map(|c| match u32::from(c) {
            0x2019..=0x201B => 0x2018,
            0x201D..=0x201F => 0x201C,
            other => other,
        })
        .collect();
    let mut out = Vec::with_capacity(code_points.len());
    let mut i = 0;
    while i < code_points.len() {
        let rest = &code_points[i..];
        // The longest contraction starting here, else the single code point.
        let contraction = (2..=table.longest_contraction.min(rest.len()))
            .rev()
            .find_map(|n| table.contractions.get(&rest[..n]).map(|e| (n, e)));
        let (consumed, elements): (usize, &[Element]) = match contraction {
            Some((n, elements)) => (n, elements),
            None => match table.singles.get(&rest[0]) {
                Some(elements) => (1, elements),
                None if matches!(rest[0], 0xFFFE | 0xFFFF) => (1, &[Element::IGNORABLE][..]),
                None => {
                    let [first, second] = implicit_weights(rest[0]);
                    out.push(Element {
                        primary: first,
                        secondary: 0x20,
                        tertiary: 0x02,
                    });
                    out.push(Element {
                        primary: second,
                        secondary: 0,
                        tertiary: 0,
                    });
                    i += 1;
                    continue;
                }
            },
        };
        out.extend_from_slice(elements);
        i += consumed;
    }
    out
}

/// Whether `c` is part of a word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The words of `text` as the word index keys them (spec 7.1): runs of
/// letters and digits. A hyphen between two digits joins them ("E9D8-45A3"
/// splits, "45A3-83DD" does not); everything else, apostrophes included,
/// separates words.
pub fn words(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &(offset, c)) in chars.iter().enumerate() {
        let joins = c == '-'
            && start.is_some()
            && i > 0
            && chars[i - 1].1.is_numeric()
            && chars.get(i + 1).is_some_and(|&(_, next)| next.is_numeric());
        if is_word_char(c) || joins {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            words.push(&text[begin..offset]);
        }
    }
    if let Some(begin) = start {
        words.push(&text[begin..]);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn matches_filemaker_server_index_keys() {
        // test project's value indexes (spec 7.1) and live:sortA_* keys.
        assert_eq!(hex(&text_key("Test")), "011433126b14101433");
        assert_eq!(hex(&text_key("Hello")), "0112d3126b13301330138e");
        assert_eq!(hex(&text_key("Ima")), "0112ec135f120f");
        assert_eq!(
            hex(&text_key("Computer")),
            "01123d138e135f13b314531433126b13da"
        );
        assert_eq!(hex(&text_key("")), "01");
        assert_eq!(
            hex(&text_key("392E36CA-E9D8-45A3-83DD-66A6EFF95ADE")),
            "011208120e1207126b1208120b123d120f0223126b120e1250120d02231209120a120f1208\
             0223120d1208125012500223120b120b120f120b126b12a312a3120e120a120f1250126b"
        );
        // A space, from the word-index probe live:ai_word_first.
        assert_eq!(
            hex(&text_key("openmaker capture test")),
            "01138e13b3126b136d135f120f131e126b13da020a123d120f13b31433145313da126b020a\
             1433126b14101433"
        );
        // Case and accents do not count.
        assert_eq!(text_key("TEST"), text_key("test"));
        assert_eq!(text_key("équipe"), text_key("Equipe"));
        // Spec 7.1: FileMaker's indexes hold keys of up to 217 bytes (108
        // weights) whole and keep the first 219 bytes of longer ones.
        let cut = |text: &str| {
            let mut key = text_key(text);
            let was_cut = cut_index_key(&mut key);
            (key.len(), was_cut)
        };
        assert_eq!(cut(&"x".repeat(108)), (217, false));
        assert_eq!(cut(&"x".repeat(109)), (219, false));
        assert_eq!(cut(&"x".repeat(110)), (219, true));
        assert_eq!(cut(&"x".repeat(300)), (219, true));
        assert_eq!(text_index_key(&"x".repeat(300)), text_key(&"x".repeat(109)));
        // Two weights per ideograph: the cut keeps the first weight of the
        // 55th (the byte rule applied; no cut CJK key was observed).
        assert_eq!(cut(&"\u{4e00}".repeat(55)), (219, true));
        assert_eq!(
            hex(&text_index_key(&"\u{4e00}".repeat(55))[215..]),
            "ce00fb40"
        );
        assert_eq!(text_index_key("Test"), text_key("Test"));
    }

    #[test]
    fn splits_words_as_the_word_index_does() {
        assert_eq!(
            words("392E36CA-E9D8-45A3-83DD-66A6EFF95ADE"),
            ["392E36CA", "E9D8-45A3-83DD", "66A6EFF95ADE"]
        );
        assert_eq!(
            words("240F5BD8-319A-49D0-8BA3-07C7E599E604"),
            ["240F5BD8-319A", "49D0-8BA3-07C7E599E604"]
        );
        assert_eq!(
            words("Revoir ce croquis avec l'équipe."),
            ["Revoir", "ce", "croquis", "avec", "l", "équipe"]
        );
        assert_eq!(words("  Ima  "), ["Ima"]);
        assert!(words("").is_empty());
        assert!(words("- ' .").is_empty());
        assert_eq!(words("-1-2-"), ["1-2"]);
    }

    #[test]
    fn exposes_secondary_and_tertiary_weights() {
        let e = |p, s, t| Element {
            primary: p,
            secondary: s,
            tertiary: t,
        };
        assert_eq!(collation_elements("a"), [e(0x120f, 0x20, 0x02)]);
        assert_eq!(collation_elements("A"), [e(0x120f, 0x20, 0x08)]);
        assert_eq!(
            collation_elements("\u{e4}"),
            [e(0x120f, 0x20, 0x02), e(0, 0x47, 0x02)]
        );
        assert_eq!(collation_elements("\u{200b}"), [e(0, 0, 0)]);
        assert_eq!(
            collation_elements("\u{4e00}"),
            [e(0xfb40, 0x20, 0x02), e(0xce00, 0, 0)]
        );
    }

    #[test]
    fn index_keys_read_curly_quotes_as_straight_ones() {
        assert_eq!(text_key("it\u{2019}s"), text_key("it's"));
        assert_eq!(text_key("\u{201C}a\u{201D}"), text_key("\"a\""));
        assert_ne!(text_key("\u{2032}"), text_key("'"));
        assert_eq!(text_key("a\u{FFFE}\u{FFFF}"), text_key("a"));
        assert_eq!(
            collation_elements("\u{201B}"),
            collation_elements("\u{2018}")
        );
        assert_ne!(collation_elements("\u{2018}"), collation_elements("'"));
    }

    #[test]
    fn unlisted_code_points_get_implicit_weights() {
        // U+4E00 (a CJK ideograph) and an unassigned code point.
        assert_eq!(hex(&text_key("\u{4e00}")), "01fb40ce00");
        assert_eq!(hex(&text_key("\u{e0000}")), "01fbdc8000");
    }
}
