//! FileMaker's characters: legacy grapheme clusters (spec
//! `docs/re/calc-values.md` 5.1, calc-functions.md 3.2 and the oracle
//! answers at the end of calc-values.md).
//!
//! A character is a base code point followed by every code point with a
//! non-zero canonical combining class: `e` + U+0301 and `ש` + U+05BF are one
//! character, while marks of class 0 stand alone (VS16 U+FE0F, Thai U+0E31,
//! the spacing mark U+093E, the enclosing keycap U+20E3), and so do ZWJ
//! and ZWNJ, so emoji ZWJ sequences and flag pairs are several characters.
//! A run of conjoining Hangul jamo in the order L* V* T* is one character, but
//! a precomposed syllable does not absorb following jamo. CR LF is one
//! character; CR and LF take no combining marks; a mark at the start of the
//! text is a character of its own.

use icu_normalizer::properties::CanonicalCombiningClassMapBorrowed;
use std::ops::Range;

fn combining_class(c: char) -> u8 {
    CanonicalCombiningClassMapBorrowed::new().get_u8(c)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Jamo {
    Leading,
    Vowel,
    Trailing,
}

fn jamo(c: char) -> Option<Jamo> {
    match u32::from(c) {
        0x1100..=0x115F | 0xA960..=0xA97F => Some(Jamo::Leading),
        0x1160..=0x11A7 | 0xD7B0..=0xD7C6 => Some(Jamo::Vowel),
        0x11A8..=0x11FF | 0xD7CB..=0xD7FB => Some(Jamo::Trailing),
        _ => None,
    }
}

/// The byte ranges of the characters of `text`.
pub fn graphemes(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let mut end = start + c.len_utf8();
        if c == '\r' {
            if let Some(&(_, '\n')) = chars.peek() {
                chars.next();
                end += 1;
            }
            out.push(start..end);
            continue;
        }
        if c == '\n' {
            out.push(start..end);
            continue;
        }
        if let Some(mut last) = jamo(c) {
            while let Some(&(i, next)) = chars.peek() {
                match jamo(next) {
                    Some(kind) if kind >= last => {
                        last = kind;
                        end = i + next.len_utf8();
                        chars.next();
                    }
                    _ => break,
                }
            }
        }
        while let Some(&(i, next)) = chars.peek() {
            if combining_class(next) == 0 {
                break;
            }
            end = i + next.len_utf8();
            chars.next();
        }
        out.push(start..end);
    }
    out
}

/// The number of characters, what `Length` counts.
pub fn grapheme_count(text: &str) -> usize {
    graphemes(text).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(s: &str) -> usize {
        grapheme_count(s)
    }

    #[test]
    fn oracle_lengths() {
        assert_eq!(count("😀"), 1);
        assert_eq!(count("e\u{301}"), 1);
        assert_eq!(count("a\u{300}\u{301}"), 1);
        assert_eq!(count("\u{1100}\u{1161}\u{11a8}"), 1);
        assert_eq!(count("\u{1100}\u{1100}"), 1);
        assert_eq!(count("\u{1161}\u{1161}"), 1);
        assert_eq!(count("\u{ac00}\u{11a8}"), 2);
        assert_eq!(count("\u{ac00}\u{1161}"), 2);
        assert_eq!(count("👨\u{200d}👩\u{200d}👧"), 5);
        assert_eq!(count("🇯🇵"), 2);
        assert_eq!(count("a\u{fe0f}"), 2);
        assert_eq!(count("a\u{e31}"), 2);
        assert_eq!(count("\u{915}\u{93e}"), 2);
        assert_eq!(count("a\u{20e3}"), 2);
        assert_eq!(count("a\u{5bf}"), 1);
        assert_eq!(count("a\u{3099}"), 1);
        assert_eq!(count("a\u{200c}"), 2);
        assert_eq!(count("\r\n"), 1);
        assert_eq!(count("a\r\nb"), 3);
        assert_eq!(count("\n\r"), 2);
        assert_eq!(count("\r\u{300}"), 2);
        assert_eq!(count("\t\u{300}"), 1);
        assert_eq!(count("\u{300}a"), 2);
        assert_eq!(count(""), 0);
    }
}
