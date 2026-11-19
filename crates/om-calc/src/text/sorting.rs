//! The text order of `SortValues` and the text equality of `UniqueValues`
//! (spec `docs/re/calc-functions.md` 4, oracle answers in 9).
//!
//! Unlike `=` and `<` (per character, [`super::text_cmp`]), these collate
//! whole values with DUCET 5.2.0 ([`om_collate::collation_elements`]):
//!
//! - `SortValues` compares primary weights, then secondary weights (accents;
//!   French from the end of the value; one byte each, [`secondary_byte`]),
//!   and nothing further, so values equal that far keep their order
//!   (`GREEN¶green¶Green` stays); Japanese also compares a third level
//!   (lower case, hiragana and wide forms first, small kana last), and so
//!   does Catalog (without case and width for basic Latin letters);
//! - `UniqueValues` compares the values' index keys: primary weights
//!   (Unicode_Standard also secondary weights, Japanese the voicing marks,
//!   small kana and katakana, Unicode_Raw the UTF-16 units), curly
//!   quotation marks read as straight ones.
//!
//! Tailorings, per character after composition (calc-values.md 13,
//! value-core and collation-gaps answers): German keys `ä`, `ö`, `ü` as
//! `ae`, `oe`, `ue` (phonebook order, `ä` before `ae` when the letters tie);
//! the other languages FileMaker tailors place letters and contractions of
//! their own ([`Letter`], anchored where FileMaker puts them): Spanish `ñ`,
//! Swedish `å ä ö` after the `z` family, Turkish `ç ğ ı ö ş ü` (and `I` as
//! the capital of `ı`), Czech `č ch ř š ž`, Vietnamese `ă â đ ê ô ơ ư`,
//! Persian's letters and harakat... Thai makes space, `-`, `.` and its
//! signs third-level differences and sorts `x` as a symbol, Greek_Mixed
//! sorts the basic Latin letters with their Greek counterparts. Chinese,
//! Korean and Japanese order ideographs and Hangul with tables of their own
//! ([`super::cjk`]). A tailoring applies to the letter as written:
//! Lithuanian moves `y`, not `ý`. Other named locales collate as English,
//! `German_Dictionary` included.

use super::cjk::{self, Han};
use super::grapheme::graphemes;
use super::weights::{ducet, primary_of, Weights, SHIFT};
use super::{as_lone_surrogate, utf16_units};
use crate::locale::Language;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use std::cmp::Ordering;

/// The collation a locale name selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collation {
    English,
    French,
    German,
    Swedish,
    /// Swedish and Finnish without `w` = `v` (`Swedish_Custom`).
    SwedishCustom,
    Japanese,
    /// Chinese (Pinyin).
    Chinese,
    ChineseStroke,
    Korean,
    Thai,
    Vietnamese,
    /// Latin letters sorted with their Greek counterparts.
    GreekMixed,
    Arabic,
    Persian,
    UnicodeStandard,
    UnicodeRaw,
    Spanish,
    SpanishTraditional,
    Turkish,
    Danish,
    Czech,
    Slovak,
    Polish,
    Croatian,
    Slovenian,
    Hungarian,
    Estonian,
    Latvian,
    Lithuanian,
    Icelandic,
    Romanian,
    Catalan,
    /// `Catalog`: `ı` sorts as `i`, and a third level (forms other than
    /// case and width) counts.
    Catalog,
    Ukrainian,
}

/// A letter or contraction a language places itself: `text` (lower case,
/// precomposed) takes the primary weight right after `after`'s, ordered by
/// `step` among the letters placed after the same one, or with `step` 0
/// `after`'s own (and so, without an accent, `after`'s weights); `accent`
/// (1, 2, ...) then sorts it after the unaccented letter at the second
/// level, or with [`LAST`] + n after every accented form of it, or with
/// [`TINY`] + n follows `after`'s weight with a primary weight n below
/// every other (between `after` alone and anything longer that starts with
/// it). `flags` ([`CASED`], [`DOUBLE`], [`COLLIDES`]) refine it.
#[derive(Clone, Copy)]
struct Letter {
    text: &'static str,
    after: char,
    step: u32,
    accent: u32,
    flags: u8,
}

const fn letter(text: &'static str, after: char, step: u32, accent: u32) -> Letter {
    Letter {
        text,
        after,
        step,
        accent,
        flags: 0,
    }
}

impl Letter {
    const fn with(self, flags: u8) -> Letter {
        Letter { flags, ..self }
    }
}

/// The letter is matched as written, not in lower case.
const CASED: u8 = 1;
/// The contraction sorts as its letter twice (Hungarian `ccs` as `cs` `cs`).
const DOUBLE: u8 = 2;
/// The letter's index key is its anchor's own: FileMaker placed it after a
/// letter Unicode 5.1 then inserted there, so `UniqueValues` takes it for
/// that letter (Latvian `č` = `ꜿ`).
const COLLIDES: u8 = 4;

/// An accent above every accent DUCET gives.
const LAST: u32 = 0xC0;

/// Marks a letter given a second, tiny primary weight (see [`Letter`]).
const TINY: u32 = 0x1000;

// The anchors are FileMaker's (vc-sortnames-chars): a letter "after c"
// comes after `c` and everything that starts with its weight (`㏇`, "Co."),
// or, in some languages, after the last letter DUCET orders between that
// letter and the next (Latvian `č` after `ꜿ`, Estonian `š` after `ʆ`).
const SPANISH: &[Letter] = &[letter("ñ", 'n', 1, 0)];
const SPANISH_TRADITIONAL: &[Letter] = &[
    letter("ch", 'c', 1, 0),
    letter("ll", 'l', 1, 0),
    letter("ñ", 'n', 1, 0),
];
const TURKISH: &[Letter] = &[
    letter("ç", 'c', 1, 0),
    letter("ğ", 'g', 1, 0),
    letter("ı", 'h', 1, 0),
    // `İ` is the capital of `i`, without the dot DUCET gives it.
    letter("i", 'i', 0, 0),
    letter("ö", 'o', 1, 0),
    letter("ş", 's', 1, 0),
    letter("ü", 'u', 1, 0),
];
const DANISH: &[Letter] = &[
    letter("ü", 'y', 0, 1),
    letter("ű", 'y', 0, 2),
    letter("æ", 'z', 1, 0),
    letter("ӕ", 'z', 1, 0),
    letter("ǽ", 'z', 1, 1),
    letter("ä", 'z', 1, 2),
    letter("ø", 'z', 2, 0),
    letter("ǿ", 'z', 2, 1),
    letter("ö", 'z', 2, 2),
    letter("ő", 'z', 2, 3),
    letter("å", 'z', 3, 0),
    letter("ǻ", 'z', 3, 1),
    letter("aa", 'z', 3, LAST + 1),
];
const SWEDISH: &[Letter] = &[
    letter("w", 'v', 0, 1),
    letter("ｗ", 'v', 0, 1),
    letter("ü", 'y', 0, 1),
    letter("ű", 'y', 0, 2),
    letter("å", 'z', 1, 0),
    letter("ä", 'z', 2, 0),
    letter("ö", 'z', 3, 0),
    letter("ő", 'z', 3, 1),
    letter("ø", 'z', 3, 2),
];
/// Swedish without `w` = `v`.
const SWEDISH_CUSTOM: &[Letter] = &[
    letter("ü", 'y', 0, 1),
    letter("ű", 'y', 0, 2),
    letter("å", 'z', 1, 0),
    letter("ä", 'z', 2, 0),
    letter("ö", 'z', 3, 0),
    letter("ő", 'z', 3, 1),
    letter("ø", 'z', 3, 2),
];
const CZECH: &[Letter] = &[
    letter("č", 'c', 1, 0),
    letter("ch", 'h', 1, 0),
    letter("ř", 'r', 1, 0),
    letter("š", 's', 1, 0),
    letter("ž", 'z', 1, 0),
];
const SLOVAK: &[Letter] = &[
    letter("ä", 'a', 1, 0),
    letter("č", 'c', 1, 0),
    letter("ch", 'h', 1, 0),
    letter("ô", 'o', 1, 0),
    letter("ř", 'r', 1, 0),
    letter("š", 's', 1, 0),
    letter("ž", 'z', 1, 0),
];
const POLISH: &[Letter] = &[
    letter("ą", 'a', 1, 0),
    letter("ć", 'c', 1, 0),
    letter("ę", 'e', 1, 0),
    letter("ń", 'n', 1, 0),
    letter("ó", 'o', 1, 0),
    letter("ś", 's', 1, 0),
    letter("ź", 'z', 1, 0),
    letter("ż", 'z', 2, 0),
];
const CROATIAN: &[Letter] = &[
    letter("č", 'c', 1, 0),
    letter("ć", 'c', 2, 0),
    letter("ǉ", 'ʫ', 1, 0),
    letter("ǌ", 'n', 1, 0),
    letter("š", 's', 1, 0),
    letter("ž", 'z', 1, 0),
];
const SLOVENIAN: &[Letter] = &[
    letter("č", 'c', 1, 0),
    letter("š", 's', 1, 0),
    letter("ž", 'z', 1, 0),
];
const HUNGARIAN: &[Letter] = &[
    letter("cs", 'c', 1, 0),
    letter("dz", 'd', 1, 0),
    letter("dzs", 'd', 2, 0),
    letter("ccs", 'c', 1, 0).with(DOUBLE),
    letter("ddz", 'd', 1, 0).with(DOUBLE),
    letter("ddzs", 'd', 2, 0).with(DOUBLE),
    letter("ggy", 'g', 1, 0).with(DOUBLE),
    letter("lly", 'l', 1, 0).with(DOUBLE),
    letter("nny", 'n', 1, 0).with(DOUBLE),
    letter("ssz", 's', 1, 0).with(DOUBLE),
    letter("tty", 't', 1, 0).with(DOUBLE),
    letter("zzs", 'z', 1, 0).with(DOUBLE),
    letter("gy", 'g', 1, 0),
    letter("ly", 'l', 1, 0),
    letter("ny", 'n', 1, 0),
    letter("ö", 'o', 1, 0),
    letter("ő", 'o', 1, 1),
    letter("sz", 's', 1, 0),
    letter("ty", 't', 1, 0),
    letter("ü", 'u', 1, 0),
    letter("ű", 'u', 1, 1),
    letter("zs", 'z', 1, 0),
];
const ESTONIAN: &[Letter] = &[
    letter("š", 'ʆ', 1, 0),
    letter("z", 'ʆ', 2, 0),
    letter("ž", 'ʆ', 3, 0),
    letter("w", 'v', 0, LAST + 1),
    // FileMaker gives `õ` the weight of `ʍ`.
    letter("õ", 'ʍ', 0, 0),
    letter("ä", 'ʍ', 1, 0),
    letter("ö", 'ʍ', 2, 0),
    letter("ü", 'ʍ', 3, 0),
];
const LATVIAN: &[Letter] = &[
    letter("č", 'ꜿ', 1, 0).with(COLLIDES),
    letter("ģ", 'ƣ', 1, 0),
    letter("y", 'ᵼ', 1, 0).with(COLLIDES),
    letter("ķ", 'ʞ', 1, 0),
    letter("ļ", 'ʎ', 1, 0),
    letter("ņ", 'ŋ', 1, 0),
    letter("ŗ", 'ꝝ', 1, 0).with(COLLIDES),
    letter("š", 'ʆ', 1, 0),
    letter("ž", 'ꝣ', 1, 0).with(COLLIDES),
];
const LITHUANIAN: &[Letter] = &[
    // Only the small letter: `Č` keeps DUCET's place.
    letter("č", 'ꜿ', 1, 0).with(CASED),
    letter("y", 'ᵼ', 1, 0).with(COLLIDES),
    letter("š", 'ʆ', 1, 0),
    letter("ž", 'ꝣ', 1, 0).with(COLLIDES),
];
const ICELANDIC: &[Letter] = &[
    letter("á", 'a', 1, 0),
    letter("é", 'e', 1, 0),
    letter("í", 'i', 1, 0),
    letter("ó", 'o', 1, 0),
    letter("ú", 'u', 1, 0),
    letter("ý", 'y', 1, 0),
    letter("æ", 'þ', 1, 0),
    letter("ӕ", 'þ', 1, 0),
    letter("œ", 'þ', 1, 1),
    letter("ö", 'þ', 2, 0),
    letter("ø", 'þ', 2, 1),
];
const ROMANIAN: &[Letter] = &[
    letter("ă", 'a', 1, 0),
    letter("î", 'i', 1, 0),
    letter("ş", 's', 1, 0),
    letter("ţ", 'ꜩ', 1, 0),
];
const CATALAN: &[Letter] = &[letter("l·l", 'l', 0, 0)];
const CATALOG: &[Letter] = &[letter("ı", 'i', 0, 0)];
const UKRAINIAN: &[Letter] = &[letter("ь", 'я', 1, 0)];
/// Vietnamese letters are their base letter's weight and a tiny second
/// one: `á` < `a` < `az` < `ă` < `âa` < `ꜳ` < `æ` < `áz` (accented letters
/// keep DUCET's weights: `ắ` sorts and indexes as `á`).
const VIETNAMESE: &[Letter] = &[
    letter("a", 'a', 0, TINY + 1),
    letter("ă", 'a', 0, TINY + 2),
    letter("â", 'a', 0, TINY + 3),
    letter("d", 'd', 0, TINY + 1),
    letter("đ", 'd', 0, TINY + 2),
    letter("e", 'e', 0, TINY + 1),
    letter("ê", 'e', 0, TINY + 2),
    letter("o", 'o', 0, TINY + 1),
    letter("ô", 'o', 0, TINY + 2),
    letter("ơ", 'o', 0, TINY + 3),
    letter("u", 'u', 0, TINY + 1),
    letter("ư", 'u', 0, TINY + 2),
];
const ARABIC: &[Letter] = &[letter("إ", 'أ', 0, 0), letter("ة", 'ت', 0, 0)];
/// Persian (anchors and the weights FileMaker shares with other
/// characters as observed).
const PERSIAN: &[Letter] = &[
    letter("ا", '\u{81B}', 0, 0),
    letter("آ", '\u{81B}', 1, 0),
    letter("ٱ", '\u{81B}', 2, 0),
    letter("أ", '۽', 1, 0),
    letter("ٲ", '۽', 2, 0),
    letter("إ", '۽', 3, 0),
    letter("ٳ", '۽', 4, 0),
    letter("ؤ", '۽', 5, 0),
    letter("ئ", '۽', 6, 0),
    letter("ك", 'ګ', 1, 0),
    letter("ڭ", 'ڬ', 1, 0),
    letter("ڮ", 'ڬ', 2, 0),
    letter("ۏ", 'ݩ', 0, 0),
    letter("ە", 'ه', 0, LAST + 1),
    letter("ہ", 'ه', 0, LAST + 2),
    letter("ة", 'ه', 0, LAST + 3),
    letter("ۃ", 'ه', 0, LAST + 4),
    letter("ۀ", 'ه', 0, LAST + 5),
    letter("ھ", 'ه', 0, LAST + 6),
    letter("ى", 'ی', 0, LAST + 1),
    letter("ے", 'ی', 0, LAST + 2),
    letter("ي", 'ی', 0, LAST + 3),
    letter("ې", 'ی', 0, LAST + 4),
    letter("ۑ", 'ی', 0, LAST + 5),
];

/// Persian's order of the harakat (`fatha << kasra << damma << fathatan <<
/// kasratan << dammatan`): their second-level weights, numbered on from
/// fatha's (kasratan takes the isolated kasra's).
fn persian_haraka(c: char) -> Option<u32> {
    Some(match c {
        '\u{650}' => 0xA8,
        '\u{64B}' => 0xAA,
        '\u{64D}' => 0xAB,
        '\u{64C}' => 0xAC,
        _ => return None,
    })
}

/// Greek_Mixed sorts each basic Latin letter after the Greek letter given
/// here (or after the Latin letter placed there before it), capitals and
/// full width forms before the small letter, at the second level: `α` <
/// `A` < `a` < `β` < `B` ... (accented Latin letters keep their place).
const GREEK_MIXED: [(char, char, u32); 26] = [
    ('a', 'α', 0),
    ('b', 'β', 0),
    ('c', 'γ', 0),
    ('d', 'δ', 0),
    ('e', 'ε', 0),
    ('f', 'ζ', 1),
    ('g', 'ζ', 2),
    ('h', 'η', 0),
    ('i', 'ι', 0),
    ('j', 'ι', 1),
    ('k', 'κ', 0),
    ('l', 'λ', 0),
    ('m', 'μ', 0),
    ('n', 'ν', 0),
    ('o', 'ο', 0),
    ('p', 'π', 0),
    ('q', 'ρ', 0),
    ('r', 'ρ', 1),
    ('s', 'σ', 0),
    ('t', 'τ', 0),
    ('u', 'υ', 1),
    ('v', 'φ', 0),
    ('w', 'φ', 1),
    ('x', 'χ', 0),
    ('y', 'υ', 0),
    ('z', 'ζ', 0),
];

fn greek_mixed(c: char) -> Option<Vec<Weights>> {
    if c == 'ß' {
        // With `ω`, after its accented forms.
        return Some(vec![
            Weights::base(primary_of('ω')),
            Weights::accent(0x20 + LAST + 1),
        ]);
    }
    let (small, capital) = match c {
        // `œ` sorts as `o`.
        'œ' => ('o', false),
        'a'..='z' => (c, false),
        'A'..='Z' => (c.to_ascii_lowercase(), true),
        'ａ'..='ｚ' | 'Ａ'..='Ｚ' => {
            let offset = u32::from(c) - if c >= 'ａ' { 0xFF41 } else { 0xFF21 };
            (char::from_u32(u32::from('a') + offset)?, true)
        }
        _ => return None,
    };
    let &(_, greek, n) = GREEK_MIXED.iter().find(|(l, _, _)| *l == small)?;
    Some(vec![
        Weights::base(primary_of(greek)),
        Weights::accent(0x20 + LAST + 2 * n + if capital { 1 } else { 2 }),
    ])
}

/// A basic Latin letter, full width or not (or the Kelvin sign, `K`).
fn is_basic_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, 'Ａ'..='Ｚ' | 'ａ'..='ｚ' | '\u{212A}')
}

/// A lone surrogate ([`super::lone_surrogate`]): the low ones after
/// U+DC00 are ignorable; the high ones and U+DC00 sort with a second-level
/// weight whose byte is 0 (like U+0EC9's 0x11F), and index keys tell them
/// apart by unit. (FileMaker's `SortValues` does not order them
/// consistently against Lao and other marks of such weights.)
fn lone_surrogate_weights(unit: u16, index: bool) -> Vec<Weights> {
    match (unit > 0xDC00, index) {
        (true, _) => Vec::new(),
        (false, true) => vec![Weights::base(u32::from(unit))],
        (false, false) => vec![Weights::accent(0x11F)],
    }
}

/// Thai: space, `-`, `.` and the signs ฯ ๆ ๏ ๚ ๛ differ only at the third
/// level, and `x` sorts as a symbol after `♯`.
fn thai(c: char) -> Option<Vec<Weights>> {
    let ignorable = [' ', '-', '.', 'ฯ', 'ๆ', '๏', '๚', '๛'];
    if let Some(i) = ignorable.iter().position(|&x| x == c) {
        // Right after the second-level weight of U+A881.
        return Some(vec![Weights {
            primary: 0,
            secondary: 0x10C | SUB,
            tertiary: 0x40 + i as u16,
        }]);
    }
    match c {
        'x' | 'X' => Some(vec![Weights {
            primary: primary_of('♯') | 1,
            secondary: 0x20,
            tertiary: if c == 'X' { 0x08 } else { 0x02 },
        }]),
        _ => None,
    }
}

/// The locale names `SortValues` and `UniqueValues` accept (Claris docs).
const NAMES: &[&str] = &[
    "Arabic",
    "Bengali",
    "Bulgarian",
    "Catalan",
    "Catalog",
    "Chinese",
    "Chinese_Stroke",
    "Croatian",
    "Czech",
    "Danish",
    "Dutch",
    "English",
    "Estonian",
    "Finnish",
    "Finnish_Custom",
    "French",
    "German",
    "German_Dictionary",
    "Greek",
    "Greek_Mixed",
    "Gujarati",
    "Hebrew",
    "Hindi",
    "Hungarian",
    "Icelandic",
    "Italian",
    "Japanese",
    "Kannada",
    "Korean",
    "Latvian",
    "Lithuanian",
    "Malayalam",
    "Marathi",
    "Norwegian",
    "Panjabi",
    "Persian",
    "Polish",
    "Portuguese",
    "Romanian",
    "Russian",
    "Serbian",
    "Slovak",
    "Slovenian",
    "Spanish",
    "Spanish_Traditional",
    "Swedish",
    "Swedish_Custom",
    "Tamil",
    "Telugu",
    "Thai",
    "Turkish",
    "Ukrainian",
    "Unicode_Raw",
    "Unicode_Standard",
    "Vietnamese",
];

impl Collation {
    /// The collation of a locale name, matched without regard to case; `None`
    /// for a name FileMaker does not know (the functions then return `?`).
    /// Languages without a tailoring here collate as English.
    pub fn named(name: &str) -> Option<Collation> {
        let name = NAMES.iter().find(|n| n.eq_ignore_ascii_case(name))?;
        Some(match *name {
            "French" => Collation::French,
            "German" => Collation::German,
            "Swedish" | "Finnish" => Collation::Swedish,
            "Swedish_Custom" | "Finnish_Custom" => Collation::SwedishCustom,
            "Japanese" => Collation::Japanese,
            "Chinese" => Collation::Chinese,
            "Chinese_Stroke" => Collation::ChineseStroke,
            "Korean" => Collation::Korean,
            "Thai" => Collation::Thai,
            "Vietnamese" => Collation::Vietnamese,
            "Greek_Mixed" => Collation::GreekMixed,
            "Arabic" => Collation::Arabic,
            "Persian" => Collation::Persian,
            "Unicode_Standard" => Collation::UnicodeStandard,
            "Unicode_Raw" => Collation::UnicodeRaw,
            "Spanish" => Collation::Spanish,
            "Spanish_Traditional" => Collation::SpanishTraditional,
            "Turkish" => Collation::Turkish,
            "Danish" | "Norwegian" => Collation::Danish,
            "Czech" => Collation::Czech,
            "Slovak" => Collation::Slovak,
            "Polish" => Collation::Polish,
            "Croatian" => Collation::Croatian,
            "Slovenian" => Collation::Slovenian,
            "Hungarian" => Collation::Hungarian,
            "Estonian" => Collation::Estonian,
            "Latvian" => Collation::Latvian,
            "Lithuanian" => Collation::Lithuanian,
            "Icelandic" => Collation::Icelandic,
            "Romanian" => Collation::Romanian,
            "Catalan" => Collation::Catalan,
            "Catalog" => Collation::Catalog,
            "Ukrainian" => Collation::Ukrainian,
            _ => Collation::English,
        })
    }

    /// The letters this collation places itself.
    fn letters(self) -> &'static [Letter] {
        match self {
            Collation::Swedish => SWEDISH,
            Collation::SwedishCustom => SWEDISH_CUSTOM,
            Collation::Spanish => SPANISH,
            Collation::SpanishTraditional => SPANISH_TRADITIONAL,
            Collation::Turkish => TURKISH,
            Collation::Danish => DANISH,
            Collation::Czech => CZECH,
            Collation::Slovak => SLOVAK,
            Collation::Polish => POLISH,
            Collation::Croatian => CROATIAN,
            Collation::Slovenian => SLOVENIAN,
            Collation::Hungarian => HUNGARIAN,
            Collation::Estonian => ESTONIAN,
            Collation::Latvian => LATVIAN,
            Collation::Lithuanian => LITHUANIAN,
            Collation::Icelandic => ICELANDIC,
            Collation::Romanian => ROMANIAN,
            Collation::Catalan => CATALAN,
            Collation::Catalog => CATALOG,
            Collation::Ukrainian => UKRAINIAN,
            Collation::Vietnamese => VIETNAMESE,
            Collation::Arabic => ARABIC,
            Collation::Persian => PERSIAN,
            _ => &[],
        }
    }

    /// The collation of a file's language.
    pub fn of(language: Language) -> Collation {
        match language {
            Language::French => Collation::French,
            Language::German => Collation::German,
            Language::Swedish => Collation::Swedish,
            Language::Japanese => Collation::Japanese,
            Language::Spanish => Collation::Spanish,
            Language::Chinese => Collation::Chinese,
            Language::Korean => Collation::Korean,
            Language::English | Language::Other(_) => Collation::English,
        }
    }

    /// How this collation orders ideographs and Hangul.
    fn han(self) -> Han {
        match self {
            Collation::Chinese => Han::Chinese,
            Collation::ChineseStroke => Han::ChineseStroke,
            Collation::Korean => Han::Korean,
            Collation::Japanese => Han::Japanese,
            _ => Han::Default,
        }
    }

    /// The sort key of `text`; keys compare as `SortValues` orders.
    pub fn sort_key(self, text: &str) -> SortKey {
        if self == Collation::UnicodeRaw {
            return SortKey {
                levels: [utf16_units(text).map(u32::from).collect(), vec![], vec![]],
                filled: !text.is_empty(),
            };
        }
        let elements = self.elements(text, false);
        let mut primary = Vec::new();
        let mut secondary = Vec::new();
        let mut tertiary = Vec::new();
        for e in &elements {
            if e.primary != 0 {
                primary.push(e.primary);
            }
            if e.secondary != 0 {
                secondary.push(secondary_byte(e.secondary));
            }
            if self == Collation::Japanese && e.tertiary != 0 {
                tertiary.push(japanese_tertiary(e));
            }
            if self == Collation::Catalog && e.tertiary != 0 {
                tertiary.push(u32::from(e.tertiary));
            }
        }
        if self == Collation::French {
            secondary.reverse();
        }
        SortKey {
            levels: [primary, secondary, tertiary],
            filled: !text.is_empty(),
        }
    }

    /// The index key of `text`: equal keys are duplicates for `UniqueValues`.
    /// Curly quotation marks read as straight ones
    /// ([`om_collate::index_char`]).
    pub fn index_key(self, text: &str) -> Vec<u32> {
        if self == Collation::UnicodeRaw {
            return utf16_units(text).map(u32::from).collect();
        }
        let folded: String = text.chars().map(om_collate::index_char).collect();
        let elements = self.elements(&folded, true);
        let mut key: Vec<u32> = elements
            .iter()
            .filter(|e| e.primary != 0)
            .map(|e| e.primary)
            .collect();
        match self {
            Collation::UnicodeStandard => {
                key.push(0);
                key.extend(
                    elements
                        .iter()
                        .filter(|e| e.secondary != 0)
                        .map(|e| e.secondary),
                );
            }
            Collation::Japanese => {
                // Voiced and semi-voiced kana, small kana and katakana count;
                // width, case and other accents do not.
                key.push(0);
                key.extend(
                    elements
                        .iter()
                        .filter(|e| matches!(e.secondary, VOICED | SEMI_VOICED))
                        .map(|e| e.secondary),
                );
                key.push(0);
                key.extend(
                    elements
                        .iter()
                        .filter(|e| e.primary != 0)
                        .map(japanese_kana_form),
                );
            }
            _ => {}
        }
        key
    }

    /// The tailored collation elements of `text`, character by character
    /// ([`Weights`]: DUCET's primary weights shifted, leaving room for
    /// letters tailored right after another).
    fn elements(self, text: &str, index: bool) -> Vec<Weights> {
        let mut out = Vec::new();
        let ranges = graphemes(text);
        let letters = self.letters();
        let mut i = 0;
        while i < ranges.len() {
            let grapheme = &text[ranges[i].clone()];
            if !letters.is_empty() {
                if let Some((n, rest, weights)) = self.letter_at(text, &ranges[i..], index) {
                    out.extend(weights);
                    out.extend(ducet(&rest));
                    i += n;
                    continue;
                }
            }
            i += 1;
            if let Some(unit) = grapheme.chars().next().and_then(as_lone_surrogate) {
                out.extend(lone_surrogate_weights(unit, index));
                out.extend(ducet(
                    &grapheme[grapheme.chars().next().map_or(0, char::len_utf8)..],
                ));
                continue;
            }
            if let Some(weights) = cjk::weights(self.han(), grapheme, index) {
                out.extend(weights);
                continue;
            }
            if let Some(weights) = self.special(grapheme, index) {
                out.extend(weights);
                continue;
            }
            if self == Collation::German {
                let composed = ComposingNormalizerBorrowed::new_nfc().normalize(grapheme);
                let mut chars = composed.chars();
                if let Some(first) = chars.next() {
                    if let Some(tailored) = self.tailored(first, index) {
                        out.extend(tailored);
                        out.extend(ducet(chars.as_str()));
                        continue;
                    }
                }
            }
            out.extend(ducet(grapheme));
        }
        out
    }

    /// The tailored letter or contraction starting at the first of
    /// `graphemes`: how many graphemes it takes, the combining marks left
    /// over, and its elements. Contractions match longest first, each
    /// grapheme by its first character in lower case (Turkish lowers `I` to
    /// `ı` and `İ` to `i`).
    fn letter_at(
        self,
        text: &str,
        graphemes: &[std::ops::Range<usize>],
        index: bool,
    ) -> Option<(usize, String, Vec<Weights>)> {
        let composed: Vec<String> = graphemes
            .iter()
            .take(3)
            .map(|r| {
                ComposingNormalizerBorrowed::new_nfc()
                    .normalize(&text[r.clone()])
                    .into_owned()
            })
            .collect();
        let first = composed[0].chars().next()?;
        let upper = first.is_uppercase();
        let lower = |c: char| match (self, c) {
            (Collation::Turkish, 'I') => 'ı',
            (Collation::Turkish, 'İ') => 'i',
            _ => c.to_lowercase().next().unwrap_or(c),
        };
        let letters = self.letters();
        // A character of a contraction: a plain letter, or in index keys,
        // which compare contractions by primary weight, also a letter with
        // an accent the language does not tailor, read as its base letter
        // (Hungarian `dž` is the contraction `dz`, `čs` is `cs`).
        let contraction_char = |g: &str| -> Option<char> {
            let decomposed = DecomposingNormalizerBorrowed::new_nfd().normalize(g);
            let mut parts = decomposed.chars();
            let base = parts.next()?;
            if parts.next().is_none() {
                return Some(lower(base));
            }
            let tailored = letters
                .iter()
                .any(|l| g.chars().map(lower).eq(l.text.chars()));
            (index && !tailored).then(|| lower(base))
        };
        let mut best: Option<(usize, String, &Letter)> = None;
        for letter in letters {
            let wanted: Vec<char> = letter.text.chars().collect();
            let n = if wanted.len() == 1 {
                1
            } else {
                // A contraction: one plain character per grapheme.
                if composed.len() < wanted.len()
                    || composed[..wanted.len()]
                        .iter()
                        .any(|g| contraction_char(g).is_none())
                {
                    continue;
                }
                wanted.len()
            };
            let got: Vec<char> = if n == 1 {
                let c = composed[0].chars().next().unwrap_or(' ');
                vec![if letter.flags & CASED != 0 {
                    c
                } else {
                    lower(c)
                }]
            } else {
                composed[..n]
                    .iter()
                    .filter_map(|g| contraction_char(g))
                    .collect()
            };
            let matches = if n == 1 {
                got[0] == wanted[0]
            } else {
                got == wanted
            };
            if matches && best.as_ref().map_or(true, |(m, _, _)| n > *m) {
                let rest: String = if n == 1 {
                    composed[0].chars().skip(1).collect()
                } else {
                    String::new()
                };
                best = Some((n, rest, letter));
            }
        }
        let (n, rest, letter) = best?;
        let step = if index && letter.flags & COLLIDES != 0 {
            0
        } else {
            letter.step
        };
        let mut weights = vec![Weights {
            primary: primary_of(letter.after) | step,
            secondary: 0x20,
            tertiary: if upper { 0x08 } else { 0x02 },
        }];
        if letter.flags & DOUBLE != 0 {
            weights.push(weights[0]);
        }
        if letter.accent >= TINY {
            weights.push(Weights {
                primary: letter.accent - TINY,
                secondary: 0,
                tertiary: 0,
            });
        } else if letter.accent > 0 && !index {
            weights.push(Weights::accent(0x20 + letter.accent));
        }
        if self == Collation::Catalan && index {
            // Index keys keep the middle dot: `l·l` stays apart from `ll`.
            return None;
        }
        if letter.text == "l·l" {
            weights = vec![weights[0], weights[0], Weights::accent(0x21)];
        }
        Some((n, rest, weights))
    }

    /// The elements German gives `ä`, `ö` and `ü`.
    fn tailored(self, letter: char, index: bool) -> Option<Vec<Weights>> {
        let upper = letter.is_uppercase();
        let lower = letter.to_lowercase().next().unwrap_or(letter);
        let base = |c: char, secondary: u32| -> Weights {
            Weights {
                primary: primary_of(c),
                secondary,
                tertiary: if upper { 0x08 } else { 0x02 },
            }
        };
        let vowel = match (self, lower) {
            (Collation::German, 'ä') => 'a',
            (Collation::German, 'ö') => 'o',
            (Collation::German, 'ü') => 'u',
            _ => return None,
        };
        // Phonebook order: `ä` is `a` then a bare `e` weight with no
        // accent of its own, so `ä` < `ae` when the letters tie.
        let mut e = base('e', 0);
        e.tertiary = 0;
        let mut weights = vec![base(vowel, 0x20), e];
        if index {
            // Index keys know only primary weights.
            weights.retain(|w| w.primary != 0);
        }
        Some(weights)
    }

    /// The elements of the collations that tailor single characters by
    /// rules of their own (Thai, Greek_Mixed, the Persian harakat).
    fn special(self, grapheme: &str, index: bool) -> Option<Vec<Weights>> {
        let mark = |c: char| match self {
            Collation::Persian => persian_haraka(c),
            _ => tailored_mark(self, c),
        };
        if grapheme.chars().any(|c| mark(c).is_some()) {
            // The marks reordered, the rest by the table or DUCET.
            let mut out = Vec::new();
            let mut buf = [0; 4];
            for c in grapheme.chars() {
                match mark(c) {
                    Some(secondary) if !index => out.push(Weights::accent(secondary)),
                    Some(_) => {}
                    None => out.extend(self.elements(c.encode_utf8(&mut buf), index)),
                }
            }
            return Some(out);
        }
        let mut chars = grapheme.chars();
        let first = chars.next()?;
        let rest = chars.as_str();
        let head = match self {
            Collation::Thai => thai(first)?,
            // Characters as written: the Kelvin sign is not a `K` here.
            Collation::GreekMixed => greek_mixed(first)?,
            Collation::Catalog if is_basic_letter(first) => {
                // Basic Latin letters ignore case and width at the third
                // level (`a` = `A` = `ａ`, the Kelvin sign is `K`), unlike
                // every other character.
                let composed = ComposingNormalizerBorrowed::new_nfc().normalize(grapheme);
                let mut weights = ducet(&composed);
                for w in &mut weights {
                    w.tertiary = 0x02;
                }
                weights
            }
            _ => return None,
        };
        if self == Collation::Catalog {
            return Some(head);
        }
        let mut out = head;
        if index {
            out.retain(|w| w.primary != 0);
        }
        out.extend(ducet(rest));
        Some(out)
    }
}

/// DUCET's tertiary weights of kana (small hiragana 0D, hiragana 0E, small
/// katakana 0F, small narrow katakana 10, katakana 11, narrow katakana 12,
/// circled katakana 13) in Japanese order: hiragana, narrow katakana,
/// katakana, circled katakana, then the small forms (`あ` < `ア` < `ぁ` <
/// `ァ`, `か` < `ｶ` < `カ` < `㋕`). Other tertiary weights (case and width:
/// `a` < `ａ` < `A` < `Ａ`, and the small and narrow forms of punctuation,
/// which share those of kana) keep DUCET's order.
fn japanese_tertiary(e: &Weights) -> u32 {
    let kana = (KANA_FIRST << SHIFT..=KANA_LAST << SHIFT).contains(&e.primary);
    match e.tertiary {
        0x0E if kana => 0x40,
        0x12 if kana => 0x41,
        0x11 if kana => 0x42,
        0x13 if kana => 0x43,
        0x0D if kana => 0x50,
        0x10 if kana => 0x51,
        0x0F if kana => 0x52,
        other => u32::from(other),
    }
}

/// A second-level weight as sort keys compare it: FileMaker keeps one byte,
/// the weight less 0x1F (so `ð`'s 0x159 sorts before `đ`'s 0x7D, and the
/// weights 0x100..0x11E after every other), then the step of a mark a
/// language places between two weights ([`SUB`]).
fn secondary_byte(secondary: u32) -> u32 {
    let byte = (secondary & 0xFFFF).wrapping_sub(0x1F) & 0xFF;
    byte << 8 | secondary >> 16
}

/// The unit of the steps marks are placed with after another's second-level
/// weight (in `Weights::secondary` above the weight itself).
const SUB: u32 = 1 << 16;

/// Marks a language places right after another's second-level weight:
/// Vietnamese puts the tone marks between acute and grave (`&̀ << ̉ << ̃ << ́
/// << ̣` before grave), Lithuanian the dot above after the ogonek. The marks
/// as written only: precomposed letters keep DUCET's weights.
fn tailored_mark(collation: Collation, c: char) -> Option<u32> {
    Some(match (collation, c) {
        (Collation::Vietnamese, '\u{300}') => 0x32 | SUB,
        (Collation::Vietnamese, '\u{309}') => 0x32 | (2 * SUB),
        (Collation::Vietnamese, '\u{303}') => 0x32 | (3 * SUB),
        (Collation::Vietnamese, '\u{301}') => 0x32 | (4 * SUB),
        (Collation::Vietnamese, '\u{323}') => 0x32 | (5 * SUB),
        (Collation::Lithuanian, '\u{307}') => 0x59 | SUB,
        _ => return None,
    })
}

/// DUCET's primary weights of the kana, `あ` to `ん`.
const KANA_FIRST: u32 = 0x2B15;
const KANA_LAST: u32 = 0x2B44;

/// DUCET's secondary weights of the voiced and semi-voiced sound marks.
const VOICED: u32 = 0x148;
const SEMI_VOICED: u32 = 0x149;

/// The kana form a Japanese index key tells apart: small (DUCET tertiary
/// 0D, 0F, 10) and katakana (0F-13, half width and circled included), for
/// kana only (half width Hangul letters share those weights). The
/// prolonged sound mark (primary 11DD) has no script: `ー` keys as `ｰ`.
fn japanese_kana_form(e: &Weights) -> u32 {
    if !(KANA_FIRST << SHIFT..=KANA_LAST << SHIFT).contains(&e.primary) {
        return 0;
    }
    let small = matches!(e.tertiary, 0x0D | 0x0F | 0x10);
    let katakana = matches!(e.tertiary, 0x0F..=0x13);
    u32::from(small) << 1 | u32::from(katakana)
}

/// A `SortValues` key: weights level by level, then whether the value is
/// empty (an empty value sorts before one whose characters are all
/// ignorable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    levels: [Vec<u32>; 3],
    filled: bool,
}

impl Ord for SortKey {
    fn cmp(&self, other: &SortKey) -> Ordering {
        self.levels
            .cmp(&other.levels)
            .then(self.filled.cmp(&other.filled))
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &SortKey) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(collation: Collation, values: &[&str]) -> Vec<String> {
        let mut v: Vec<&str> = values.to_vec();
        v.sort_by_key(|s| collation.sort_key(s));
        v.into_iter().map(String::from).collect()
    }

    #[test]
    fn english_sorts_by_letters_then_accents() {
        let en = Collation::English;
        assert_eq!(
            sorted(en, &["é", "e", "f", "E", "É"]),
            ["e", "E", "é", "É", "f"]
        );
        assert_eq!(
            sorted(en, &["GREEN", "green", "Green"]),
            ["GREEN", "green", "Green"]
        );
        assert_eq!(
            sorted(en, &["st", "ß", "ss", "sr"]),
            ["sr", "ss", "ß", "st"]
        );
        assert_eq!(
            sorted(en, &["ab", "a_b", "a-b", "a b", "a"]),
            ["a", "a b", "a_b", "a-b", "ab"]
        );
        assert_eq!(
            sorted(en, &["ba", "b\ta", "b\u{ad}a"]),
            ["b\ta", "b\u{ad}a", "ba"]
        );
        assert_eq!(
            sorted(en, &["ö", "ø", "o", "oe", "œ"]),
            ["o", "ö", "ø", "oe", "œ"]
        );
    }

    #[test]
    fn french_reads_accents_from_the_end() {
        let values = ["côté", "coté", "côte", "cote"];
        assert_eq!(
            sorted(Collation::English, &values),
            ["cote", "coté", "côte", "côté"]
        );
        assert_eq!(
            sorted(Collation::French, &values),
            ["cote", "côte", "coté", "côté"]
        );
    }

    #[test]
    fn german_and_swedish_tailorings() {
        let de = Collation::German;
        assert_eq!(
            sorted(de, &["ae", "ä", "af", "å", "a"]),
            ["a", "å", "ä", "ae", "af"]
        );
        assert_eq!(sorted(de, &["oe", "ö", "ø", "o"]), ["o", "ø", "ö", "oe"]);
        assert_eq!(
            sorted(de, &["äb", "ab", "af", "ae"]),
            ["ab", "ae", "äb", "af"]
        );
        assert_eq!(
            sorted(de, &["Äpfel", "Apfel", "Zebra", "Ähre"]),
            ["Ähre", "Äpfel", "Apfel", "Zebra"]
        );
        assert_eq!(de.index_key("ä"), de.index_key("ae"));
        assert_eq!(de.index_key("æ"), de.index_key("ae"));
        let sv = Collation::Swedish;
        assert_eq!(
            sorted(sv, &["ö", "ø", "ä", "æ", "å", "z", "ő"]),
            ["æ", "z", "å", "ä", "ö", "ő", "ø"]
        );
        assert_eq!(sorted(sv, &["ü", "y", "u", "z"]), ["u", "y", "ü", "z"]);
        assert_eq!(sv.index_key("v"), sv.index_key("w"));
        assert_eq!(sv.index_key("ø"), sv.index_key("ö"));
    }

    #[test]
    fn japanese_third_level() {
        let ja = Collation::Japanese;
        let kana = ["ガ", "が", "カ", "か", "ｶ", "ア", "あ", "ぁ", "ァ"];
        assert_eq!(
            sorted(ja, &kana),
            ["あ", "ア", "ぁ", "ァ", "か", "ｶ", "カ", "が", "ガ"]
        );
        assert_eq!(sorted(ja, &["Ea", "éa", "ea"]), ["ea", "Ea", "éa"]);
        assert_ne!(ja.index_key("か"), ja.index_key("カ"));
        assert_eq!(ja.index_key("カ"), ja.index_key("ｶ"));
        assert_ne!(ja.index_key("か"), ja.index_key("が"));
        assert_ne!(ja.index_key("ぁ"), ja.index_key("あ"));
        assert_eq!(ja.index_key("ァ"), ja.index_key("ｧ"));
        assert_eq!(ja.index_key("ー"), ja.index_key("ｰ"));
        assert_eq!(ja.index_key("é"), ja.index_key("E"));
        assert_eq!(ja.index_key("ß"), ja.index_key("ss"));
    }

    #[test]
    fn named_tailorings_follow_filemaker() {
        let vi = Collation::Vietnamese;
        assert_eq!(
            sorted(vi, &["â", "az", "a", "á", "æ", "ă", "áz"]),
            ["á", "a", "az", "ă", "â", "æ", "áz"]
        );
        assert_eq!(vi.index_key("á"), vi.index_key("ắ"));
        assert_ne!(vi.index_key("a"), vi.index_key("á"));
        let gm = Collation::GreekMixed;
        assert_eq!(
            sorted(gm, &["b", "β", "a", "A", "α", "á"]),
            ["á", "α", "A", "a", "β", "b"]
        );
        assert_eq!(gm.index_key("a"), gm.index_key("α"));
        let th = Collation::Thai;
        assert_eq!(sorted(th, &["1", "x", "!"]), ["!", "x", "1"]);
        assert_eq!(th.index_key("ก-"), th.index_key("ก"));
        let hu = Collation::Hungarian;
        assert_eq!(sorted(hu, &["ccs", "cs", "cz"]), ["cz", "cs", "ccs"]);
        assert_eq!(hu.index_key("dž"), hu.index_key("dz"));
        let lv = Collation::Latvian;
        assert_eq!(lv.index_key("č"), lv.index_key("ꜿ"));
        assert_eq!(sorted(lv, &["č", "ꜿ", "d"]), ["ꜿ", "č", "d"]);
        assert_eq!(sorted(lv, &["ž", "ꝣ", "ʒ"]), ["ꝣ", "ž", "ʒ"]);
        let cs = Collation::Czech;
        assert_eq!(sorted(cs, &["ᴢ", "ž", "z"]), ["z", "ž", "ᴢ"]);
        // One byte per accent: the weight less 0x1F (Lao 0x11F before 0x11E).
        assert_eq!(
            sorted(Collation::English, &["\u{EC8}", "\u{EC9}"]),
            ["\u{EC9}", "\u{EC8}"]
        );
    }

    #[test]
    fn index_keys() {
        let en = Collation::English;
        assert_eq!(en.index_key("é"), en.index_key("E"));
        assert_eq!(en.index_key("ß"), en.index_key("SS"));
        assert_ne!(en.index_key("a"), en.index_key("a\t"));
        let std = Collation::UnicodeStandard;
        assert_ne!(std.index_key("é"), std.index_key("e"));
        assert_eq!(std.index_key("e"), std.index_key("E"));
        assert_ne!(
            Collation::UnicodeRaw.index_key("e"),
            Collation::UnicodeRaw.index_key("E")
        );
        assert_eq!(Collation::named("english"), Some(Collation::English));
        assert_eq!(Collation::named(" English"), None);
    }
}
