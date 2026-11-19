//! FileMaker's order of ideographs and Hangul (calc-values.md 13, "Oracle
//! answers (collation gaps)").
//!
//! Every collation puts the ideographs and Hangul syllables FileMaker
//! computes weights for after the other letters, in this order: the CJK
//! Unified Ideographs U+4E00..U+9FCB, Extension A U+3400..U+4DB5, the
//! supplementary ideographs U+20000..U+2B7FF (each a two-weight expansion
//! for `Max` and `Min`), the Hangul syllables, and only then what DUCET
//! lists with ideograph weights (radicals, enclosed ideographs, the twelve
//! unified compatibility ideographs) and the unassigned code points
//! ([`implicit`]). Compatibility ideographs sort as their canonical
//! decomposition.
//!
//! Chinese, Korean and Japanese files replace part of that with tables of
//! their own ([`Han`]), whose weights come right after every other letter's:
//!
//! - Chinese (Pinyin) orders 27,508 ideographs by pinyin, then strokes,
//!   with the Unicode 5.1 and 5.2 additions to the main block first
//!   (`data/zh-han.bin`); the locale name Chinese_Stroke orders 27,967 by
//!   stroke count, then stroke types (`data/zh-stroke.bin`).
//! - Korean orders, after the other letters, compatibility forms (full
//!   width punctuation and letters, squared units, half width katakana, CJK
//!   strokes) and then, per initial consonant: its jamo, the syllables that
//!   start with it, its final jamo, its enclosed forms and the Hanja read
//!   with it; then vowels (`data/ko-hangul.bin`). A syllable has the
//!   weights of its jamo and, without a final consonant, the lowest
//!   Korean weight in place of one (so `가` sorts after `ᄀ` `ᅡ` and
//!   before `각`; index keys leave that weight out). Extension A, the main
//!   block's Unicode 4.1 to 5.2 additions and the supplementary
//!   ideographs take weights of the table by code point, and so can equal
//!   a Hanja.
//! - Japanese orders the kanji of JIS X 0208 by their code, then the
//!   IBM extension kanji of code page 932, then the other ideographs of
//!   U+4E00..U+9FA5 by code point (仝 first); Extension A, the main block's
//!   additions, the Hangul syllables and the supplementary ideographs (by
//!   high surrogate) take the weights of that sequence at fixed offsets, so
//!   `"填" = "㐀"` and `"暐" = "가"`. Circled ideographs and ideographic
//!   annotation marks sort as the kanji they stand for (radicals do not).
//!
//! The tables are FileMaker's observed order, written by
//! `tools/fms-calc-harness/cjk_tables.py` (see `data/README.md`).

use super::weights::{ducet, Weights, SHIFT};
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use std::sync::OnceLock;

/// The CJK ordering of a collation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Han {
    /// FileMaker's computed weights ([`implicit`]).
    Default,
    Chinese,
    /// The locale name Chinese_Stroke: ideographs by stroke count, then
    /// stroke types.
    ChineseStroke,
    Korean,
    Japanese,
}

/// The primary weight the language tables are numbered after: above every
/// DUCET 5.2 letter (the highest is 0x3ACA) and below [`implicit`]'s.
const TABLE: u32 = 0xF000;
/// Leads of the computed weights, below DUCET's own ideograph weights
/// (0xFB40 and up).
const URO: u32 = 0xFA40;
const EXTENSION_A: u32 = 0xFA80;
const SUPPLEMENTARY: u32 = 0xFA84;
const HANGUL: u32 = 0xFA90;

fn table_weight(rank: u32) -> Weights {
    Weights::base(TABLE << SHIFT | rank)
}

/// A supplementary ideograph's two weights: one for its high surrogate
/// (`lead`), one ordering it by code point among those that share that
/// weight.
fn surrogate_weights(lead: u32, c: char) -> Vec<Weights> {
    vec![
        Weights::base(lead),
        Weights {
            primary: (u32::from(c) - 0x1FFFF) << SHIFT,
            secondary: 0,
            tertiary: 0,
        },
    ]
}

fn high_surrogate(c: char) -> u32 {
    0xD800 + ((u32::from(c) - 0x10000) >> 10)
}

/// The weights FileMaker computes for an ideograph or Hangul syllable
/// outside the language tables.
pub(crate) fn implicit(c: char) -> Option<Vec<Weights>> {
    let code = u32::from(c);
    let single = |lead: u32, offset: u32| Some(vec![Weights::base(lead << SHIFT | offset)]);
    match code {
        0x4E00..=0x9FCB => single(URO, code - 0x4E00),
        0x3400..=0x4DB5 => single(EXTENSION_A, code - 0x3400),
        0xAC00..=0xD7A3 => single(HANGUL, code - 0xAC00),
        0x20000..=0x2B7FF => Some(surrogate_weights(
            SUPPLEMENTARY << SHIFT | (high_surrogate(c) - 0xD800),
            c,
        )),
        _ => None,
    }
}

/// A rank table written by `cjk_tables.py` (format in its documentation).
struct Table {
    /// First code point, length and offset of each range's ranks.
    ranges: Vec<(u32, u32, usize)>,
    ranks: &'static [u8],
    /// High surrogate and rank of the group before its characters.
    supplementary: Vec<(u32, u32)>,
}

impl Table {
    fn parse(bytes: &'static [u8]) -> Table {
        let u16_at = |at: usize| u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
        let count = u16_at(0) as usize;
        let mut ranges = Vec::with_capacity(count);
        let mut offset = 0;
        for i in 0..count {
            let at = 2 + 6 * i;
            let first =
                u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            let len = u16_at(at + 4);
            ranges.push((first, len, offset));
            offset += 2 * len as usize;
        }
        let start = 2 + 6 * count;
        let ranks = &bytes[start..start + offset];
        let at = start + offset;
        let supplementary = (0..u16_at(at) as usize)
            .map(|i| (u16_at(at + 2 + 4 * i), u16_at(at + 4 + 4 * i)))
            .collect();
        Table {
            ranges,
            ranks,
            supplementary,
        }
    }

    fn rank(&self, c: char) -> Option<u32> {
        let code = u32::from(c);
        let i = self.ranges.partition_point(|&(first, _, _)| first <= code);
        let &(first, len, offset) = self.ranges.get(i.checked_sub(1)?)?;
        if code - first >= len {
            return None;
        }
        let at = offset + 2 * (code - first) as usize;
        let rank = u32::from(u16::from_le_bytes([self.ranks[at], self.ranks[at + 1]]));
        (rank != 0).then_some(rank)
    }

    fn supplementary(&self, c: char) -> Option<u32> {
        let high = high_surrogate(c);
        self.supplementary
            .iter()
            .find(|&&(h, _)| h == high)
            .map(|&(_, rank)| rank)
    }

    /// A character's table weights.
    fn weights(&self, c: char) -> Option<Vec<Weights>> {
        if let Some(rank) = self.rank(c) {
            return Some(vec![table_weight(rank)]);
        }
        if ('\u{20000}'..='\u{2B7FF}').contains(&c) {
            let rank = self.supplementary(c)?;
            return Some(surrogate_weights(TABLE << SHIFT | rank, c));
        }
        None
    }
}

fn chinese() -> &'static Table {
    static CHINESE: OnceLock<Table> = OnceLock::new();
    CHINESE.get_or_init(|| Table::parse(include_bytes!("../../data/zh-han.bin")))
}

fn chinese_stroke() -> &'static Table {
    static CHINESE_STROKE: OnceLock<Table> = OnceLock::new();
    CHINESE_STROKE.get_or_init(|| Table::parse(include_bytes!("../../data/zh-stroke.bin")))
}

fn korean() -> &'static Table {
    static KOREAN: OnceLock<Table> = OnceLock::new();
    KOREAN.get_or_init(|| Table::parse(include_bytes!("../../data/ko-hangul.bin")))
}

/// The JIS X 0208 index (row * 94 + cell) of a kanji, IBM extensions
/// (rows 89 to 92 of EUC-JP) included.
fn jis_index(c: char) -> Option<u32> {
    let mut buf = [0; 4];
    let (bytes, _, unmappable) = encoding_rs::EUC_JP.encode(c.encode_utf8(&mut buf));
    match *bytes {
        [lead @ 0xb0..=0xfe, trail @ 0xa1..=0xfe] if !unmappable => {
            Some(u32::from(lead - 0xa1) * 94 + u32::from(trail - 0xa1))
        }
        _ => None,
    }
}

/// The Japanese rank of each ideograph of U+4E00..U+9FA5: 仝 first, the
/// JIS X 0208 and IBM extension kanji by code, then the rest by code point.
fn japanese_ranks() -> &'static [u16] {
    static RANKS: OnceLock<Vec<u16>> = OnceLock::new();
    RANKS.get_or_init(|| {
        let block = '\u{4E00}'..='\u{9FA5}';
        let mut ranks = vec![0u16; block.clone().count()];
        let mut jis: Vec<(u32, char)> = block
            .clone()
            .filter_map(|c| jis_index(c).map(|i| (i, c)))
            .collect();
        jis.sort_unstable();
        let order = std::iter::once('仝')
            .chain(jis.into_iter().map(|(_, c)| c))
            .chain(block.filter(|&c| c != '仝' && jis_index(c).is_none()));
        for (rank, c) in (1..).zip(order) {
            ranks[(u32::from(c) - 0x4E00) as usize] = rank;
        }
        ranks
    })
}

fn japanese(c: char) -> Option<Vec<Weights>> {
    let code = u32::from(c);
    let rank = match code {
        0x4E00..=0x9FA5 => u32::from(japanese_ranks()[(code - 0x4E00) as usize]),
        0x3400..=0x4DB5 => code - 0x2C33,
        0x9FA6..=0x9FCB => code - 0x9BFE,
        0xAC00..=0xD7A3 => code - 0x8251,
        0x20000..=0x2B7FF => {
            return Some(surrogate_weights(
                TABLE << SHIFT | (high_surrogate(c) - 0xB2BE),
                c,
            ))
        }
        0x3192..=0x319F | 0x3280..=0x32B0 => {
            // Ideographic annotation marks and circled ideographs sort as
            // their kanji, with their own third-level weight.
            let mut buf = [0; 4];
            let text = c.encode_utf8(&mut buf);
            let decomposed = DecomposingNormalizerBorrowed::new_nfkd().normalize(text);
            let mut chars = decomposed.chars();
            let (Some(d), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let mut weights = japanese(d)?;
            if let Some(e) = om_collate::collation_elements(text).first() {
                weights[0].tertiary = e.tertiary;
            }
            return Some(weights);
        }
        _ => return None,
    };
    Some(vec![table_weight(rank)])
}

/// A Hangul syllable's jamo: leading, vowel and trailing (if any).
fn jamo(c: char) -> Option<[Option<char>; 3]> {
    let s = u32::from(c).checked_sub(0xAC00).filter(|&s| s < 11172)?;
    let jamo = |base: u32, offset: u32| char::from_u32(base + offset);
    Some([
        jamo(0x1100, s / 588),
        jamo(0x1161, s % 588 / 28),
        (s % 28 != 0).then(|| jamo(0x11A7, s % 28)).flatten(),
    ])
}

fn korean_weights(c: char, index: bool) -> Option<Vec<Weights>> {
    let table = korean();
    let Some([l, v, t]) = jamo(c) else {
        return table.weights(c);
    };
    let mut out = Vec::with_capacity(3);
    for j in [l, v].into_iter().flatten() {
        out.extend(table.weights(j)?);
    }
    match t {
        Some(t) => out.extend(table.weights(t)?),
        // The lowest Korean weight stands in for a missing final.
        None if !index => out.push(table_weight(0)),
        None => {}
    }
    Some(out)
}

/// The weights of one character of the CJK blocks under `han`, or `None`
/// for a character it does not order.
fn char_weights(han: Han, c: char, index: bool) -> Option<Vec<Weights>> {
    let tailored = match han {
        Han::Default => None,
        Han::Chinese => chinese().weights(c),
        Han::ChineseStroke => chinese_stroke().weights(c),
        Han::Korean => korean_weights(c, index),
        Han::Japanese => japanese(c),
    };
    tailored.or_else(|| implicit(c))
}

/// Whether `c` is a compatibility ideograph of Unicode 5.2 (sorted as its
/// decomposition; later ones are unassigned code points to FileMaker).
fn is_compatibility_ideograph(c: char) -> bool {
    matches!(
        c,
        '\u{F900}'..='\u{FA2D}'
            | '\u{FA30}'..='\u{FA6D}'
            | '\u{FA70}'..='\u{FAD9}'
            | '\u{2F800}'..='\u{2FA1D}'
    )
}

/// The weights of `grapheme` when it holds a character `han` orders: those
/// characters by the table or [`implicit`], the others by DUCET. `None`
/// when it holds none (the caller's rules apply).
pub(crate) fn weights(han: Han, grapheme: &str, index: bool) -> Option<Vec<Weights>> {
    if grapheme.chars().any(is_compatibility_ideograph) {
        let composed: String = grapheme
            .chars()
            .flat_map(|c| {
                let mut buf = [0; 4];
                let s: &str = c.encode_utf8(&mut buf);
                if is_compatibility_ideograph(c) {
                    ComposingNormalizerBorrowed::new_nfc()
                        .normalize(s)
                        .into_owned()
                } else {
                    s.to_owned()
                }
                .chars()
                .collect::<Vec<_>>()
            })
            .collect();
        // The twelve unified compatibility ideographs have no decomposition.
        if composed != grapheme {
            return weights(han, &composed, index);
        }
    }
    let mut out = Vec::new();
    let mut found = false;
    let mut rest_start = 0;
    for (i, c) in grapheme.char_indices() {
        if let Some(w) = char_weights(han, c, index) {
            out.extend(ducet(&grapheme[rest_start..i]));
            out.extend(w);
            rest_start = i + c.len_utf8();
            found = true;
        }
    }
    if !found {
        return None;
    }
    out.extend(ducet(&grapheme[rest_start..]));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primaries(han: Han, s: &str) -> Vec<u32> {
        weights(han, s, false)
            .unwrap()
            .iter()
            .map(|w| w.primary)
            .filter(|&p| p != 0)
            .collect()
    }

    #[test]
    fn computed_weights_order_the_blocks() {
        let d = Han::Default;
        let order = ["一", "龻", "㐀", "𠀀", "\u{2B734}", "가", "힣"];
        for pair in order.windows(2) {
            assert!(primaries(d, pair[0]) < primaries(d, pair[1]), "{pair:?}");
        }
        assert_eq!(primaries(d, "豈"), primaries(d, "豈"));
        assert_eq!(primaries(d, "𠀀").len(), 2);
        assert!(weights(d, "a", false).is_none());
    }

    #[test]
    fn chinese_orders_by_pinyin() {
        let zh = Han::Chinese;
        assert!(primaries(zh, "丫") < primaries(zh, "一"));
        assert!(primaries(zh, "一") < primaries(zh, "衣"));
        assert!(primaries(zh, "唖") < primaries(zh, "亜"));
        assert!(primaries(zh, "龼") < primaries(zh, "吖"));
        assert!(primaries(zh, "吖") < primaries(zh, "龦"));
        let stroke = Han::ChineseStroke;
        let order = ["一", "丨", "乙", "二", "丁", "人", "龜", "齉"];
        for pair in order.windows(2) {
            assert!(
                primaries(stroke, pair[0]) < primaries(stroke, pair[1]),
                "{pair:?}"
            );
        }
    }

    #[test]
    fn korean_orders_by_jamo_and_reading() {
        let ko = Han::Korean;
        let order = [
            "ᄀ", "가", "각", "깋", "ᆨ", "伽", "佳", "ᄁ", "까", "다", "阿",
        ];
        for pair in order.windows(2) {
            assert!(primaries(ko, pair[0]) < primaries(ko, pair[1]), "{pair:?}");
        }
        assert_eq!(
            primaries(ko, "각"),
            primaries(ko, "\u{1100}\u{1161}\u{11A8}")
        );
        assert_ne!(primaries(ko, "가"), primaries(ko, "\u{1100}\u{1161}"));
        assert_eq!(
            weights(ko, "가", true).unwrap().len(),
            weights(ko, "\u{1100}\u{1161}", true).unwrap().len()
        );
        assert_eq!(primaries(ko, "ㄱ"), primaries(ko, "ᄀ"));
    }

    #[test]
    fn japanese_follows_jis_and_collides() {
        let ja = Han::Japanese;
        assert!(primaries(ja, "仝") < primaries(ja, "亜"));
        assert!(primaries(ja, "亜") < primaries(ja, "唖"));
        assert!(primaries(ja, "𠀋") < primaries(ja, "龥"));
        assert_eq!(primaries(ja, "填"), primaries(ja, "㐀"));
        assert_eq!(primaries(ja, "暐"), primaries(ja, "가"));
        assert_eq!(primaries(ja, "㊀"), primaries(ja, "一"));
        assert!(weights(ja, "⼀", false).is_none());
    }
}
