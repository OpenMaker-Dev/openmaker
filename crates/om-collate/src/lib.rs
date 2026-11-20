//! FileMaker's text index keys and word breaking (spec
//! `docs/re/query-program.md` 7.1, 7.2).
//!
//! A text value's index key in the English form (index languages English and
//! English international) is `01` followed by the primary weight of each of
//! its collation elements, each a big-endian u16 ([`text_key`]). FileMaker
//! takes the elements from the Unicode Collation Algorithm's default table
//! (DUCET) of UCA 5.2.0 one code point at a time, without contractions, and
//! gives code points the table does not list weights of its own
//! ([`index_elements`]). Variable elements (spaces, punctuation) keep their
//! weights; elements with a zero primary (combining marks, controls) add
//! nothing, which makes keys ignore case and accents. The padded form (System
//! and Unicode standard) adds the elements' secondary weights
//! ([`padded_text_key`]). The table ships as `data/allkeys-5.2.0.txt` from
//! unicode.org, under `data/LICENSE-UNICODE`.
//!
//! [`collation_elements`] is the table as UCA reads it (contractions and
//! UCA's implicit weights), for the calculation engine's text sorting.
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

/// The index key of `text` in the English form (spec 7.2): `01`, then the
/// primary weight of each of its [`index_elements`] that has one. The empty
/// string gives `01`.
pub fn text_key(text: &str) -> Vec<u8> {
    primary_key(&index_elements(text))
}

/// The index key of `text` in the padded form of index languages System (1)
/// and Unicode standard (3) (spec 7.2): [`text_key`], `00 00`, then one byte
/// for each element with a secondary weight, the weight minus 0x1F, its low
/// byte (DUCET's 0x159 gives `3a`). The bytes are left out when every one is
/// `01`, as for text without accents: `Test` = `01 1433 126b 1410 1433 00
/// 00`, `équipe` = the English key, `00 00`, `01 13 01 01 01 01 01`.
pub fn padded_text_key(text: &str) -> Vec<u8> {
    let elements = index_elements(text);
    let mut key = primary_key(&elements);
    key.extend([0, 0]);
    let secondary: Vec<u8> = elements
        .iter()
        .filter(|e| e.secondary != 0)
        // The low byte: FileMaker's 0x159 gives 3a (`ß`, `æ`, `œ`).
        .map(|e| e.secondary.wrapping_sub(0x1F).to_be_bytes()[1])
        .collect();
    if secondary.iter().any(|&b| b != 0x01) {
        key.extend(secondary);
    }
    key
}

/// `01` and the non-zero primary weights of `elements`.
fn primary_key(elements: &[Element]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 2 * elements.len());
    key.push(TEXT_KEY_PREFIX);
    for element in elements.iter().filter(|e| e.primary != 0) {
        key.extend(element.primary.to_be_bytes());
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

/// The collation elements FileMaker's index keys read for `text` (spec 7.2,
/// `Draco::KeyBuilder`), ignorable ones included. Each code point is looked
/// up on its own, so DUCET's contractions do not apply (Thai `เก` keys in the
/// order written). Unlike UCA ([`collation_elements`]):
///
/// - the curly quotation marks U+2018..U+201B key as `'` and U+201C..U+201F
///   as `"`, with secondary weight 0x21 instead of 0x20;
/// - U+FFFE and U+FFFF add nothing;
/// - a code point the table does not list gets FileMaker's own weights
///   (spec 7.2's table), not UCA's implicit ones;
/// - a table entry that holds UCA's implicit weights for an ideograph (a CJK
///   compatibility ideograph's canonical ideograph, a Kangxi radical's)
///   holds FileMaker's weights for that ideograph instead: `豈` U+F900 keys
///   as U+8C48 (`8c48`).
///
/// FileMaker's weights for unlisted code points have no secondary weight on
/// record: here the first element of each has 0x20 and a second one none,
/// as UCA's implicit elements (inferred; only text whose other secondary
/// bytes are all `01` was observed, spec 7.2).
pub fn index_elements(text: &str) -> Vec<Element> {
    let table = table();
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        let (code_point, quote) = match u32::from(c) {
            0x2018..=0x201B => (0x27, true),
            0x201C..=0x201F => (0x22, true),
            0xFFFE | 0xFFFF => continue,
            other => (other, false),
        };
        let Some(elements) = table.singles.get(&code_point) else {
            push_filemaker_weights(code_point, 0x20, &mut out);
            continue;
        };
        let mut i = 0;
        while i < elements.len() {
            let element = elements[i];
            if let Some(ideograph) = implicit_code_point(element, elements.get(i + 1)) {
                push_filemaker_weights(ideograph, element.secondary, &mut out);
                i += 2;
                continue;
            }
            out.push(if quote {
                Element {
                    secondary: 0x21,
                    ..element
                }
            } else {
                element
            });
            i += 1;
        }
    }
    out
}

/// The code point whose UCA implicit weights `first` and `second` are
/// (UTS #10 7.1.3: `[.AAAA.0020.xxxx][.BBBB.0000.0000]`, AAAA = FB40, FB80
/// or FBC0 + (code point >> 15), BBBB = (code point & 7FFF) | 8000).
/// No element of DUCET 5.2.0 that is not implicit has a primary weight
/// from FB40.
fn implicit_code_point(first: Element, second: Option<&Element>) -> Option<u32> {
    let second = second.filter(|e| e.secondary == 0 && e.primary & 0x8000 != 0)?;
    let base = match first.primary {
        0xFB40..=0xFB7F => 0xFB40,
        0xFB80..=0xFBBF => 0xFB80,
        0xFBC0..=0xFBFF => 0xFBC0,
        _ => return None,
    };
    Some(u32::from(first.primary - base) << 15 | u32::from(second.primary & 0x7FFF))
}

/// FileMaker's primary weights for a code point its table does not list
/// (spec 7.2, `KeyBuilder::MakeKey`, each range observed): one or two.
fn filemaker_weights(code_point: u32) -> ([u16; 2], usize) {
    // Every arm fits 16 bits: the ranges bound the sums, a code point
    // (at most 10FFFF) shifted right by 16 is at most 0x10.
    let low = (code_point & 0xFFFF) as u16;
    match code_point {
        // CJK unified ideographs: the code point.
        0x4E00..=0x9FCB => ([low, 0], 1),
        // CJK extension A.
        0x3400..=0x4DB5 => ([low + 0x6FCB, 0], 1),
        // Hangul syllables.
        0xAC00..=0xD7A3 => ([low + 0x19AD, 0], 1),
        // CJK extensions B to D: the high surrogate - 0x16C0, then the low
        // surrogate.
        0x20000..=0x2B7FF => {
            let offset = code_point - 0x10000;
            let high = 0xD800 + (offset >> 10) as u16;
            let low = 0xDC00 + (offset & 0x3FF) as u16;
            ([high - 0x16C0, low], 2)
        }
        // Any other code point: FFE0 + its plane, then its low 16 bits.
        _ => ([0xFFE0 + (code_point >> 16) as u16, low], 2),
    }
}

/// Append FileMaker's elements for an unlisted code point, the first with
/// secondary weight `secondary`.
fn push_filemaker_weights(code_point: u32, secondary: u16, out: &mut Vec<Element>) {
    let (weights, count) = filemaker_weights(code_point);
    for (i, &primary) in weights[..count].iter().enumerate() {
        out.push(Element {
            primary,
            secondary: if i == 0 { secondary } else { 0 },
            tertiary: if i == 0 { 0x02 } else { 0 },
        });
    }
}

/// The DUCET 5.2.0 collation elements of `text` in order, as UCA reads the
/// table, ignorable ones included: contractions first (the longest that
/// matches), and for a code point the table does not list its two implicit
/// elements (`[.AAAA.0020.0002][.BBBB.0000.0000]`, UTS #10 7.1.3). As in
/// FileMaker's sorting, the noncharacters U+FFFE and U+FFFF are completely
/// ignorable and the curly quotation marks sort as one single and one
/// double mark (U+2018..U+201B as U+2018, U+201C..U+201F as U+201C). Index
/// keys read the table differently ([`index_elements`]).
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

/// The script classes between which the word index breaks a run of letters
/// (spec 7.1: `日本語のテキスト` gives `日本語`, `の`, `テキスト`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    Han,
    Hiragana,
    Katakana,
    HangulSyllable,
    HangulJamo,
    HangulCompatibility,
    /// Latin, digits and every other script.
    Other,
}

/// The script class of a word character. The kana and Hangul ranges follow
/// Unicode's blocks, with the kana sound marks U+309B and U+309C and the
/// prolonged sound mark among the katakana, and the iteration marks `々`
/// `〻` and `〆` among the ideographs, as the probe's keys show; `〇`, a
/// number, is not.
fn script(c: char) -> Script {
    match u32::from(c) {
        0x2E80..=0x2FDF
        | 0x3005
        | 0x3006
        | 0x303B
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0x20000..=0x3FFFF => Script::Han,
        0x3041..=0x3096 | 0x309D..=0x309F => Script::Hiragana,
        0x309B | 0x309C | 0x30A0..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => {
            Script::Katakana
        }
        0xAC00..=0xD7A3 => Script::HangulSyllable,
        0x1100..=0x11FF | 0xA960..=0xA97F | 0xD7B0..=0xD7FF => Script::HangulJamo,
        0x3130..=0x318F | 0xFFA0..=0xFFDC => Script::HangulCompatibility,
        _ => Script::Other,
    }
}

/// How the word index reads a character (spec 7.1).
enum WordChar {
    /// A letter or digit of a script class. So is a code point the table
    /// does not list (an emoji, U+0378: `a😀b` is one word), and so are
    /// the kana sound marks.
    Word(Script),
    /// Continues a word, adding no break: an accent (a character whose
    /// collation elements are accents alone, such as U+0301 or U+3099),
    /// U+FFFE and U+FFFF.
    Extend,
    /// Ends a word, unless it joins the characters around it
    /// ([`joins_word`]).
    Break,
}

fn word_char(c: char) -> WordChar {
    if matches!(c, '\u{FFFE}' | '\u{FFFF}') {
        return WordChar::Extend;
    }
    if c.is_alphanumeric() || matches!(c, '\u{309B}' | '\u{309C}') {
        return WordChar::Word(script(c));
    }
    match table().singles.get(&u32::from(c)) {
        None => WordChar::Word(script(c)),
        Some(_) if accent(c) => WordChar::Extend,
        Some(_) => WordChar::Break,
    }
}

/// A letter of the Latin script in the Latin blocks (Basic Latin to Latin
/// Extended-B, and Latin Extended Additional), which hold no combining
/// marks: all their letters but the micro sign U+00B5, whose script is
/// Common.
fn latin_letter(c: char) -> bool {
    c.is_alphabetic() && c != 'µ' && matches!(u32::from(c), 0x41..=0x24f | 0x1e00..=0x1eff)
}

/// A character the table gives no primary weight but a secondary one: an
/// accent, which keys with the letter before it.
fn accent(c: char) -> bool {
    table().singles.get(&u32::from(c)).is_some_and(|elements| {
        elements.iter().all(|e| e.primary == 0) && elements.iter().any(|e| e.secondary != 0)
    })
}

/// The characters that join two digits or two letters into one word in an
/// index language's word index (spec 7.2, "Word breaking"): `-` and `,`
/// join digits and `.` letters in every language, and these vary. The rest
/// of the word rule is the same in every language probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordBreaks {
    /// `.` joins two digits: all but French (24), Polish (33), Swedish (41,
    /// 42) and Arabic (49).
    pub dot_digits: bool,
    /// `:` joins two digits: all but Italian (30).
    pub colon_digits: bool,
    /// `/` joins two digits: 23 of the languages probed.
    pub slash_digits: bool,
    /// An apostrophe (`'` or U+2019) joins two letters: all but Catalan
    /// (16), French (24) and Italian (30).
    pub apostrophe_letters: bool,
    /// The middle dot U+00B7 joins two letters: Catalan (16) alone.
    pub middle_dot_letters: bool,
    /// The no-break space U+00A0 is part of a word, at either end too
    /// (`1`, U+00A0, `234` is one word): in every language probed.
    pub no_break_space: bool,
}

impl WordBreaks {
    /// The English rules OpenMaker keys words with today, in English, System
    /// and Unicode standard: the probe's, but the no-break space separates,
    /// as before the probe. An index an earlier OpenMaker build wrote holds
    /// such a value's words apart, and a changed record's old entries are
    /// found by the rules in force, so the no-break space stays a
    /// separator until those entries can be swept (#448).
    pub const ENGLISH: WordBreaks = WordBreaks {
        dot_digits: true,
        colon_digits: true,
        slash_digits: true,
        apostrophe_letters: true,
        middle_dot_letters: false,
        no_break_space: false,
    };

    /// The word breaks FileMaker Server's engine was probed to use in index
    /// language `language` (`Locale::Type`): 1, 3, 4, 16-65, 67 and 76, and
    /// English international (77), which FileMaker stores as English, read
    /// as English. `None` for the others; Unicode raw (2) breaks words at
    /// spaces by rules of its own, not built here. English's differ from
    /// [`Self::ENGLISH`] in the no-break space.
    pub fn of_language(language: u32) -> Option<WordBreaks> {
        let language = if language == 77 { 21 } else { language };
        if !matches!(language, 1 | 3 | 4 | 16..=65 | 67 | 76) {
            return None;
        }
        Some(WordBreaks {
            dot_digits: !matches!(language, 24 | 33 | 41 | 42 | 49),
            colon_digits: language != 30,
            slash_digits: matches!(
                language,
                1 | 3 | 4 | 16 | 21 | 24 | 27 | 30 | 31 | 39 | 40 | 45..=52 | 54 | 56..=58
            ),
            apostrophe_letters: !matches!(language, 16 | 24 | 30),
            middle_dot_letters: language == 16,
            no_break_space: true,
        })
    }
}

/// Whether `c`, between `before` and `after`, joins them into one word in
/// a language with `breaks` (spec 7.2): `-` and `,` between two digits, and
/// `.`, `:` and `/` where the language joins them (`3.14`, `12:30`,
/// `E9D8-45A3`, `50/50`); `.` between two letters (`x.y`), an apostrophe
/// (`don't`; French separates `l'équipe`) and the middle dot (Catalan
/// `l·l`) where the language joins them. None joins a digit and a letter.
fn joins_word(before: char, c: char, after: char, breaks: WordBreaks) -> bool {
    let digits = before.is_numeric() && after.is_numeric();
    let letters = before.is_alphabetic() && after.is_alphabetic();
    match c {
        '-' | ',' => digits,
        '.' => digits && breaks.dot_digits || letters,
        ':' => digits && breaks.colon_digits,
        '/' => digits && breaks.slash_digits,
        '\'' | '\u{2019}' => letters && breaks.apostrophe_letters,
        // Between two Latin letters themselves, as Catalan writes `l·l`
        // (the probe's letters are ASCII): not an accent or another
        // script's sign (U+0345, U+093F) beside it.
        '\u{B7}' => breaks.middle_dot_letters && latin_letter(before) && latin_letter(after),
        _ => false,
    }
}

/// The words of `text` as the word index keys them (spec 7.1): runs of
/// letters and digits of one script class ([`Script`]), accents and U+FFFE
/// within them, joined across the punctuation [`joins_word`] names. Every
/// other character separates words: `e-mail` gives `e`, `mail`; `U.S.A.`
/// gives `U.S.A`; `日本語abc123` gives `日本語`, `abc123`. English's
/// joiners; another language's are [`words_in`]'s.
pub fn words(text: &str) -> Vec<&str> {
    words_in(text, WordBreaks::ENGLISH)
}

/// [`words`] in a language with `breaks` ([`WordBreaks::of_language`]).
pub fn words_in(text: &str, breaks: WordBreaks) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut words = Vec::new();
    // The current word: where it starts, where its last character ends, its
    // script class, and its last letter or digit, which a joiner reads
    // through the accents after it (`e` + U+0301 + `'` + `t`).
    let mut word: Option<(usize, usize, Script, char)> = None;
    for (i, &(offset, c)) in chars.iter().enumerate() {
        let end = offset + c.len_utf8();
        let kind = if c == '\u{A0}' && breaks.no_break_space {
            WordChar::Word(Script::Other)
        } else {
            word_char(c)
        };
        match (kind, &mut word) {
            (WordChar::Word(script), Some((start, last, current, base))) => {
                if *current != script {
                    words.push(&text[*start..*last]);
                    *start = offset;
                    *current = script;
                }
                *last = end;
                *base = c;
            }
            (WordChar::Word(script), None) => word = Some((offset, end, script, c)),
            (WordChar::Extend, Some((_, last, _, _))) => *last = end,
            (WordChar::Break, Some((_, last, _, base)))
                if *last == offset
                    && chars.get(i + 1).is_some_and(|&(_, after)| {
                        script(*base) == script(after) && joins_word(*base, c, after, breaks)
                    }) =>
            {
                *last = end;
            }
            (_, current) => {
                if let Some((start, last, _, _)) = current.take() {
                    words.push(&text[start..last]);
                }
            }
        }
    }
    if let Some((start, last, _, _)) = word {
        words.push(&text[start..last]);
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
        // One weight per unified ideograph, so 109 fit (spec 7.1, `中`×109
        // against `中`×110).
        assert_eq!(cut(&"\u{4e2d}".repeat(109)), (219, false));
        assert_eq!(cut(&"\u{4e2d}".repeat(110)), (219, true));
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
        // The French FMS sample separates at an apostrophe; English joins.
        assert_eq!(
            words_in(
                "Revoir ce croquis avec l'équipe.",
                WordBreaks::of_language(24).unwrap()
            ),
            ["Revoir", "ce", "croquis", "avec", "l", "équipe"]
        );
        // The English OpenMaker writes keeps the no-break space separating
        // (#448); the probed rules keep it in the word.
        let english = WordBreaks::of_language(21).unwrap();
        assert_eq!(words("1\u{a0}234"), ["1", "234"]);
        assert_eq!(words_in("1\u{a0}234", english), ["1\u{a0}234"]);
        // Catalan joins letters across `·`, not an accent beside it.
        let catalan = WordBreaks::of_language(16).unwrap();
        assert_eq!(words_in("l·l", catalan), ["l·l"]);
        assert_eq!(words_in("1\u{345}·a", catalan), ["1\u{345}", "a"]);
        assert_eq!(words_in("a·\u{93f}", catalan), ["a", "\u{93f}"]);
        assert_eq!(words_in("N·µm", catalan), ["N", "µm"]);
        // The dot does not stay with `a`; the accent after it is a word of
        // its own, as an alphabetic accent after a separator is today.
        assert_eq!(words_in("a·\u{345}", catalan), ["a", "\u{345}"]);
        assert_eq!(
            words_in(
                "Revoir ce croquis avec l'équipe.",
                WordBreaks::of_language(24).unwrap()
            ),
            ["Revoir", "ce", "croquis", "avec", "l", "équipe"]
        );
        assert_eq!(words("avec l'équipe."), ["avec", "l'équipe"]);
        // A joiner after an accent joins as after its letter.
        assert_eq!(words("e\u{301}'t\u{e9}"), ["e\u{301}'t\u{e9}"]);
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
    fn index_keys_and_sorting_read_the_table_differently() {
        // Index keys: one code point at a time (Thai `เก` as written),
        // FileMaker's own weights where the table lists nothing, a
        // compatibility ideograph as its canonical one. Every row of
        // catalogs/index-key-vectors.tsv is checked in om-query.
        assert_eq!(hex(&text_key("\u{e40}\u{e01}")), "011faa1f70");
        assert_eq!(hex(&text_key("\u{4e2d}\u{f900}")), "014e2d8c48");
        // UCA's reading, for sorting, keeps contractions and implicit
        // weights.
        assert_eq!(
            collation_elements("\u{e40}\u{e01}")[0],
            collation_elements("\u{e01}")[0]
        );
        assert_eq!(collation_elements("\u{4e00}")[0].primary, 0xfb40);
    }
}
