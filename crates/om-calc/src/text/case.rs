//! Case mapping of `Upper`, `Lower`, `Proper` and the case folding of
//! `Position`, `PatternCount` and `FilterValues` (spec
//! `docs/re/calc-functions.md` 3.3, 3.7 and the text oracle answers in 9).
//!
//! FileMaker maps each UTF-16 unit on its own with a Unicode 5.2-era table
//! of simple mappings: `ß`, `ﬁ` and `ŉ` stay, `İ` lowers to `i`, the Greek
//! letters with ypogegrammeni upper to their title-case forms, and nothing
//! outside the BMP changes (`𐐨` stays). The table is measured
//! ([`super::tables`]) and the same in every locale. `Lower` alone writes a
//! final sigma: `Σ` becomes `ς` after a character that is not a space and
//! before the end or a space.

use super::tables::{LOWER, UPPER};
use super::words::word_ranges;
use super::Text;
use crate::locale::Locale;

fn map(table: &[(u16, u16)], c: char) -> char {
    let Ok(unit) = u16::try_from(u32::from(c)) else {
        return c;
    };
    match table.binary_search_by_key(&unit, |&(from, _)| from) {
        Ok(i) => char::from_u32(u32::from(table[i].1)).unwrap_or(c),
        Err(_) => c,
    }
}

/// The character `Upper` makes of `c`.
pub fn upper_char(c: char) -> char {
    map(UPPER, c)
}

/// The character `Lower` makes of `c` (without the final sigma rule).
pub fn lower_char(c: char) -> char {
    map(LOWER, c)
}

/// The case folding of `Position`, `PatternCount` and `FilterValues`: the
/// lower-case mapping, so `İ` matches `i` but `ı` does not match `I`, and
/// `ς` does not match `σ`.
pub fn fold(c: char) -> char {
    lower_char(c)
}

/// `s` with every character folded (one character for one).
pub fn fold_str(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// The characters around which `Σ` lowers to a final `ς`: tab through CR,
/// space, U+1680, U+180E, U+2000-U+200B, U+2028, U+2029, U+205F, U+3000
/// (measured; U+00A0 and U+0085 are not among them).
fn is_sigma_space(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | ' '
            | '\u{1680}'
            | '\u{180e}'
            | '\u{2000}'..='\u{200b}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `Upper`, styles kept.
pub fn upper(text: &Text) -> Text {
    text.map_chars(|c| upper_char(c).to_string())
}

/// `Lower`, styles kept.
pub fn lower(text: &Text) -> Text {
    let chars: Vec<char> = text.as_str().chars().collect();
    let mut i = 0;
    text.map_chars(|c| {
        let at = i;
        i += 1;
        let final_sigma = c == 'Σ'
            && at > 0
            && !is_sigma_space(chars[at - 1])
            && chars.get(at + 1).map_or(true, |&next| is_sigma_space(next));
        if final_sigma { 'ς' } else { lower_char(c) }.to_string()
    })
}

/// `Proper`: the first character of every word (the words of `WordCount`)
/// upper-cased and the rest of the word lower-cased; characters between
/// words are left alone (`O'neil Mcdonald-Smith 3rd`, `X..Y`, a circled `Ⓐ`
/// between words stays). Styles and run boundaries kept.
pub fn proper(text: &Text, locale: &Locale) -> Text {
    let words = word_ranges(text.as_str(), locale);
    let mut word = words.iter().peekable();
    let mut offset = 0;
    text.map_chars(|c| {
        let at = offset;
        offset += c.len_utf8();
        while word.peek().is_some_and(|w| w.end <= at) {
            word.next();
        }
        match word.peek() {
            Some(w) if w.start == at => upper_char(c),
            Some(w) if w.start < at => lower_char(c),
            _ => c,
        }
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(s: &str) -> String {
        upper(&Text::plain(s)).into_string()
    }

    fn low(s: &str) -> String {
        lower(&Text::plain(s)).into_string()
    }

    #[test]
    fn simple_mappings_of_the_bmp() {
        assert_eq!(up("straße ﬁ ŉ ǰ"), "STRAßE ﬁ ŉ ǰ");
        assert_eq!(up("ǆ ı i ɐ"), "Ǆ I I Ɐ");
        assert_eq!(low("ǅ İ I ẞ"), "ǆ i i ß");
        assert_eq!(up("\u{1f80}"), "\u{1f88}");
        // Mappings Unicode added after 5.2, and the supplementary planes.
        assert_eq!(
            up("\u{ab70} \u{a7b5} \u{10d0} 𐐨"),
            "\u{ab70} \u{a7b5} \u{10d0} 𐐨"
        );
        assert_eq!(low("\u{a78d}"), "\u{a78d}");
    }

    #[test]
    fn final_sigma() {
        assert_eq!(low("ΑΣ"), "ας");
        assert_eq!(low("Σ"), "σ");
        assert_eq!(low(" Σ"), " σ");
        assert_eq!(low("ΑΣ Α"), "ας α");
        assert_eq!(low("ΑΣ."), "ασ.");
        assert_eq!(low("1Σ"), "1ς");
        assert_eq!(low("ΣΣ"), "σς");
        assert_eq!(low("ΑΣ\u{a0}"), "ασ\u{a0}");
        assert_eq!(low("ΑΣ\u{200b}"), "ας\u{200b}");
        assert_eq!(low("ΑΣ\u{301}"), "ασ\u{301}");
    }

    #[test]
    fn proper_case() {
        let en = Locale::en_us();
        let p = |s: &str| proper(&Text::plain(s), &en).into_string();
        assert_eq!(p("o'neil mcdonald-smith 3rd"), "O'neil Mcdonald-Smith 3rd");
        assert_eq!(p("hello,world.foo;bar:baz"), "Hello,World.foo;Bar:Baz");
        assert_eq!(p("x..y 1.y x.1y a1,2b"), "X..Y 1.Y X.1y A1,2b");
        assert_eq!(p("ǆemal ΟΔΟΣ"), "Ǆemal Οδοσ");
        assert_eq!(p("e\u{301}clair \u{24b6}x"), "E\u{301}clair \u{24b6}X");
    }
}
