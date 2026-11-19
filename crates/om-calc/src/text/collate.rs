//! Text equality and ordering (spec `docs/re/calc-values.md` 5.3 and the
//! oracle answers at its end).
//!
//! Both texts are walked character by character (legacy grapheme
//! clusters); each character's key is the primary weights of its collation
//! elements (DUCET 5.2.0 through `om-collate`), so case, accents and width
//! do not count. Two texts are equal when they have as many characters and
//! each pair has equal keys (`ß` is not `ss`, `"a" = "a" & Char(9)` is 0);
//! ordering compares the keys pair by pair and then the lengths (`"ss" <
//! "ß"`, `"ß" < "st"` is 0, `"a" < "a "`).
//!
//! The file's language tailors the keys. Observed with the eleven Contacts
//! templates: German keys `ä`, `ö`, `ü` as `a e`, `o e`, `u e` (so `æ` =
//! `ä`, `ä` < `b`); Swedish sorts `å` < `ä` < `ö` right after `z`, keys `ø`
//! as `ö`, `ü` as `y` and `w` as `v`; Spanish sorts `ñ` after `n`;
//! Chinese, Korean and Japanese order ideographs and Hangul with tables of
//! their own, and every language puts the ideographs and Hangul syllables
//! after the other letters in FileMaker's block order ([`super::cjk`]).
//! French, Italian, Dutch and Portuguese compare like English. A
//! decomposed letter (`a` + U+0308) gets the key of its precomposed form.
//!
//! `Max` and `Min` order text with the same keys but put a character whose
//! key is an expansion (several primary weights: `æ`, `œ`, `ß`, `½`,
//! German `ä`, a supplementary ideograph, a Korean syllable) after every
//! character with a single weight, expansions among themselves by their
//! number of weights and then by the weights ([`extreme_cmp`]).

use super::as_lone_surrogate;
use super::cjk::{self, Han};
use super::grapheme::graphemes;
use super::weights::{ducet, primary_of, Weights};
use crate::locale::Language;
use icu_normalizer::ComposingNormalizerBorrowed;
use std::cmp::Ordering;

/// A collation element's primary weight ([`Weights::primary`]).
type Element = u32;

fn primaries(weights: Vec<Weights>) -> Vec<Element> {
    weights
        .into_iter()
        .map(|w| w.primary)
        .filter(|&p| p != 0)
        .collect()
}

/// The tailored elements of a lowercase letter, if the language tailors it.
fn tailored(language: Language, letter: char) -> Option<Vec<Element>> {
    let w = primary_of;
    match (language, letter) {
        (Language::German, 'ä') => Some(vec![w('a'), w('e')]),
        (Language::German, 'ö') => Some(vec![w('o'), w('e')]),
        (Language::German, 'ü') => Some(vec![w('u'), w('e')]),
        (Language::Swedish, 'å') => Some(vec![w('z') | 1]),
        (Language::Swedish, 'ä') => Some(vec![w('z') | 2]),
        (Language::Swedish, 'ö' | 'ø') => Some(vec![w('z') | 3]),
        (Language::Swedish, 'ü') => Some(vec![w('y')]),
        (Language::Swedish, 'w') => Some(vec![w('v')]),
        (Language::Spanish, 'ñ') => Some(vec![w('n') | 1]),
        _ => None,
    }
}

/// The CJK ordering of a file's language.
fn han(language: Language) -> Han {
    match language {
        Language::Chinese => Han::Chinese,
        Language::Korean => Han::Korean,
        Language::Japanese => Han::Japanese,
        _ => Han::Default,
    }
}

/// The key of one character.
fn key(grapheme: &str, language: Language) -> Vec<Element> {
    if grapheme.chars().all(|c| as_lone_surrogate(c).is_some()) {
        // A lone surrogate has no weight: all are equal, and less than any
        // letter.
        return Vec::new();
    }
    if let Some(weights) = cjk::weights(han(language), grapheme, false) {
        return primaries(weights);
    }
    if matches!(
        language,
        Language::German | Language::Swedish | Language::Spanish
    ) {
        let composed = ComposingNormalizerBorrowed::new_nfc().normalize(grapheme);
        let mut chars = composed.chars();
        if let Some(first) = chars.next() {
            let lower = first.to_lowercase().next().unwrap_or(first);
            if let Some(mut elements) = tailored(language, lower) {
                elements.extend(primaries(ducet(chars.as_str())));
                return elements;
            }
        }
    }
    primaries(ducet(grapheme))
}

fn keys<'a>(text: &'a str, language: Language) -> impl Iterator<Item = Vec<Element>> + 'a {
    graphemes(text)
        .into_iter()
        .map(move |range| key(&text[range], language))
}

/// Text equality of `=` (spec 5.3).
pub fn text_eq(a: &str, b: &str, language: Language) -> bool {
    let (ga, gb) = (graphemes(a), graphemes(b));
    ga.len() == gb.len()
        && ga
            .into_iter()
            .zip(gb)
            .all(|(x, y)| key(&a[x], language) == key(&b[y], language))
}

/// What [`text_eq`] compares, built once per text: each character's key,
/// preceded by its length. `text_eq(a, b, language)` holds exactly when
/// the two texts' keys are equal (as many characters, each pair of keys
/// equal), so a set of keys finds the texts `=` takes as one.
pub fn text_eq_key(text: &str, language: Language) -> Vec<u32> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for key in keys(text, language) {
        out.push(u32::try_from(key.len()).unwrap_or(u32::MAX));
        out.extend(key);
    }
    out
}

/// Text ordering of `<` and `>` (spec 5.3).
pub fn text_cmp(a: &str, b: &str, language: Language) -> Ordering {
    let mut ka = keys(a, language);
    let mut kb = keys(b, language);
    loop {
        match (ka.next(), kb.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
}

/// Text ordering of `Max` and `Min` (see the module documentation).
pub fn extreme_cmp(a: &str, b: &str, language: Language) -> Ordering {
    let rank = |key: Vec<Element>| {
        let expansion = key.len() > 1;
        (expansion, if expansion { key.len() } else { 0 }, key)
    };
    let mut ka = keys(a, language).map(rank);
    let mut kb = keys(b, language).map(rank);
    loop {
        match (ka.next(), kb.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text_eq`, checked against [`text_eq_key`] on every case.
    fn eq(a: &str, b: &str, l: Language) -> bool {
        let equal = text_eq(a, b, l);
        let keyed = text_eq_key(a, l) == text_eq_key(b, l);
        assert_eq!(
            equal, keyed,
            "{a:?} = {b:?} in {l:?}: text_eq and the keys differ"
        );
        equal
    }

    fn lt(a: &str, b: &str, l: Language) -> bool {
        eq(a, b, l);
        text_cmp(a, b, l) == Ordering::Less
    }

    #[test]
    fn english() {
        let en = Language::English;
        assert!(eq("abc", "ABC", en));
        assert!(eq("é", "e", en));
        assert!(eq("e\u{301}", "é", en));
        assert!(eq("Ａ", "A", en));
        assert!(eq("ア", "ｱ", en));
        assert!(eq("か", "カ", en));
        assert!(eq("①", "1", en));
        assert!(!eq("ss", "ß", en));
        assert!(!eq("æ", "ae", en));
        assert!(!eq("a", "a ", en));
        assert!(!eq("ı", "i", en));
        assert!(eq("ǅ", "ǆ", en));
        assert!(!eq("a", "a\t", en));
        assert!(eq("ä", "a", en));
        assert!(lt("a", "B", en));
        assert!(lt("1", "a", en));
        assert!(lt("-", "a", en));
        assert!(lt("a-b", "ab", en));
        assert!(lt("10", "2", en));
        assert!(lt("a", "a ", en));
        assert!(lt("ss", "ß", en));
        assert!(!lt("ß", "st", en));
        assert!(lt("ae", "æ", en));
        assert!(!lt("æ", "af", en));
        assert!(!lt("Apple", "apple", en) && !lt("apple", "Apple", en));
        assert!(lt("a 1", "a1", en));
        assert!(lt("", "\u{300}", en));
    }

    #[test]
    fn german() {
        let de = Language::German;
        assert!(!eq("ä", "a", de));
        assert!(eq("a\u{308}", "ä", de));
        assert!(eq("Ä", "ä", de));
        assert!(lt("ä", "b", de));
        assert!(lt("ae", "ä", de));
        assert!(!lt("ä", "af", de));
        assert!(eq("æ", "ä", de));
        assert!(lt("ø", "ö", de));
        assert!(eq("å", "a", de));
        assert!(!lt("z", "ä", de));
    }

    #[test]
    fn swedish() {
        let sv = Language::Swedish;
        assert!(!eq("ä", "a", sv));
        assert!(lt("z", "å", sv));
        assert!(lt("å", "ä", sv));
        assert!(lt("ä", "ö", sv));
        assert!(eq("ø", "ö", sv));
        assert!(!eq("æ", "ä", sv));
        assert!(eq("ü", "y", sv));
        assert!(eq("v", "w", sv));
        assert!(lt("aa", "å", sv));
        assert!(lt("å", "þ", sv));
        assert!(lt("æ", "b", sv));
        assert!(eq("é", "e", sv));
    }

    #[test]
    fn spanish_and_japanese() {
        let es = Language::Spanish;
        assert!(lt("n", "ñ", es) && !eq("n", "ñ", es));
        assert!(lt("nz", "ña", es));
        assert!(lt("ñ", "o", es));
        assert!(lt("ch", "cz", es));
        let ja = Language::Japanese;
        assert!(lt("阿", "一", ja) && lt("一", "二", ja) && lt("二", "八", ja));
        assert!(lt("七", "丁", ja));
        assert!(lt("乂", "丂", ja));
        assert!(lt("あ", "阿", ja) && lt("八", "가", ja));
        assert!(lt("一", "阿", Language::English));
        assert!(lt("𠀋", "龥", ja) && eq("填", "㐀", ja));
    }

    #[test]
    fn max_and_min_put_expansions_last() {
        let gt = |a: &str, b: &str, l| extreme_cmp(a, b, l) == Ordering::Greater;
        let en = Language::English;
        assert!(gt("æ", "나", en) && gt("æ", "z", en));
        assert!(gt("œ", "æ", en) && gt("ß", "œ", en) && gt("½", "ß", en));
        assert!(gt("aæ", "ae", en) && gt("b", "aæ", en));
        assert!(gt("가", "一", en));
        let de = Language::German;
        assert!(gt("ä", "b", de) && gt("ö", "z", de) && gt("ü", "y", de));
        assert!(gt("ä", "ae", de) && gt("ö", "ø", de));
        assert_eq!(extreme_cmp("æ", "ä", de), Ordering::Equal);
        assert!(!gt("å", "z", de));
        let sv = Language::Swedish;
        assert!(gt("æ", "ä", sv) && gt("ä", "z", sv));
        assert_eq!(extreme_cmp("ü", "y", sv), Ordering::Equal);
    }
}
