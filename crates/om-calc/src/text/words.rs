//! Words as `WordCount`, `LeftWords`, `MiddleWords`, `RightWords` and
//! `Proper` see them (spec `docs/re/calc-functions.md` 3.4, oracle answers
//! in 9).
//!
//! Every character has a class from FileMaker's own table (measured,
//! [`super::tables`]), and a word runs on while each character may follow
//! the one before:
//!
//! - letters, digits and other word characters (controls, U+00A0, ZWJ,
//!   emoji...) follow each other;
//! - Han characters (with Hangul syllables, Yi, the private use area and
//!   most other code points from U+2E80) follow each other; so do hiragana,
//!   and katakana (full and half width), but the three groups break against
//!   each other and against letters (`漢字かな` is two words);
//! - the prolonged sound mark `ー` and the spacing voiced marks `゛` `゜`
//!   continue a kana run without changing its script (`コーヒー` is one
//!   word, `あーア` two); a `゛` also lets a digit follow, and a word that
//!   starts with `゛` takes kana and digits;
//! - a combining mark joins whatever is on either side of it;
//! - joiners keep a word together between two letters (`'` `’`: `don't`),
//!   two digits (`,` `-` `:`: `3,5`, `12:30`) or either (`.`: `x.y`, `1.5`);
//!   the character before a joiner counts through combining marks, the one
//!   after it does not;
//! - everything else separates.
//!
//! The file decides three joiner families: the `.` family joins digits only
//! where `.` is the decimal, thousands or date separator (not in fr-FR and
//! sv-SE files), the `/` family only where `/` is one (not in de-DE and
//! sv-SE files), and French files separate at apostrophes.

use super::as_lone_surrogate;
use super::tables::WORD_CLASSES;
use crate::locale::{Language, Locale};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Separator,
    Letter,
    Digit,
    Mark,
    Other,
    Han,
    Hiragana,
    Katakana,
    Prolonged,
    KanaMark,
    MidLetter,
    MidNum,
    MidNumLetter,
}

/// The class of `c` in a file of `locale` (the table's numbers are
/// `tools/fms-calc-harness/text_tables.py`'s).
fn class(c: char, locale: &Locale) -> Class {
    let code = u32::from(c);
    if as_lone_surrogate(c).is_some() {
        // A lone surrogate separates words.
        return Class::Separator;
    }
    let Ok(unit) = u16::try_from(code) else {
        // Supplementary planes: CJK ideographs from U+20000 on are Han,
        // everything else (emoji, Deseret...) other word characters.
        return if (0x20000..0x40000).contains(&code) {
            Class::Han
        } else {
            Class::Other
        };
    };
    let i = WORD_CLASSES.partition_point(|&(start, _)| start <= unit);
    let has = |sep: char| {
        locale.decimal_separator == sep
            || locale.thousands_separator == sep
            || locale.date_separator == sep
    };
    match WORD_CLASSES[i.saturating_sub(1)].1 {
        1 => Class::Letter,
        2 => Class::Digit,
        3 => Class::Mark,
        4 => Class::Other,
        5 => Class::Han,
        6 => Class::Hiragana,
        7 => Class::Katakana,
        8 => Class::Prolonged,
        9 => Class::KanaMark,
        10 => Class::MidLetter,
        11 => Class::MidNum,
        12 => Class::MidNumLetter,
        13 if has('.') => Class::MidNum,
        14 if has('/') => Class::MidNum,
        15 if has('.') => Class::MidNumLetter,
        15 => Class::MidLetter,
        16 if locale.language != Language::French => Class::MidLetter,
        _ => Class::Separator,
    }
}

/// What the characters of the open word so far let follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    /// Letters, digits, other word characters.
    Alphanumeric,
    Han,
    Hiragana,
    Katakana,
    /// A word that so far is prolonged sound marks.
    Prolonged,
    /// A word that starts with a spacing voiced mark.
    KanaMark,
    /// A word that so far is combining marks: anything follows.
    Marks,
}

impl Run {
    fn of(class: Class) -> Run {
        match class {
            Class::Han => Run::Han,
            Class::Hiragana => Run::Hiragana,
            Class::Katakana => Run::Katakana,
            Class::Prolonged => Run::Prolonged,
            Class::KanaMark => Run::KanaMark,
            _ => Run::Alphanumeric,
        }
    }

    fn is_kana(self) -> bool {
        matches!(
            self,
            Run::Hiragana | Run::Katakana | Run::Prolonged | Run::KanaMark
        )
    }
}

/// The byte ranges of the words of `text`, in order.
pub fn word_ranges(text: &str, locale: &Locale) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char, Class)> = text
        .char_indices()
        .map(|(i, c)| (i, c, class(c, locale)))
        .collect();
    let mut words = Vec::new();
    // The open word's start and run; where it ends; the class the last word
    // character counts as for joiners (marks count as what they follow);
    // whether the last character was a combining mark or a `゛`.
    let mut open: Option<(usize, Run)> = None;
    let mut end = 0;
    let mut last = Class::Other;
    let mut after_mark = false;
    let mut after_voiced = false;
    let close = |open: &mut Option<(usize, Run)>, end: usize, words: &mut Vec<Range<usize>>| {
        if let Some((start, _)) = open.take() {
            words.push(start..end);
        }
    };
    for (i, &(offset, c, class)) in chars.iter().enumerate() {
        let next_end = offset + c.len_utf8();
        match class {
            Class::Separator => close(&mut open, end, &mut words),
            Class::Mark => {
                if open.is_none() {
                    open = Some((offset, Run::Marks));
                    last = Class::Other;
                }
                end = next_end;
                after_mark = true;
                after_voiced = false;
            }
            Class::MidLetter | Class::MidNum | Class::MidNumLetter => {
                let next = chars.get(i + 1).map(|&(_, _, k)| k);
                let letters = last == Class::Letter && next == Some(Class::Letter);
                let digits = last == Class::Digit && next == Some(Class::Digit);
                let joins = open.is_some()
                    && match class {
                        Class::MidLetter => letters,
                        Class::MidNum => digits,
                        _ => letters || digits,
                    };
                if joins {
                    end = next_end;
                    after_mark = false;
                    after_voiced = false;
                } else {
                    close(&mut open, end, &mut words);
                }
            }
            _ => {
                let run = match open {
                    Some((start, run)) => {
                        let joins = after_mark
                            || run == Run::Marks
                            || match run {
                                Run::Alphanumeric => {
                                    matches!(class, Class::Letter | Class::Digit | Class::Other)
                                }
                                Run::Han => class == Class::Han,
                                Run::Hiragana | Run::Katakana | Run::Prolonged => {
                                    class == Class::Prolonged
                                        || class == Class::KanaMark
                                        || Run::of(class) == run
                                        || (after_voiced && class == Class::Digit)
                                }
                                Run::KanaMark => matches!(
                                    class,
                                    Class::Hiragana
                                        | Class::Katakana
                                        | Class::Prolonged
                                        | Class::KanaMark
                                        | Class::Digit
                                ),
                                Run::Marks => true,
                            };
                        if joins {
                            // `ー` and `゛` continue a kana run as it is.
                            let extends = run.is_kana()
                                && matches!(class, Class::Prolonged | Class::KanaMark);
                            Some((start, if extends { run } else { Run::of(class) }))
                        } else {
                            close(&mut open, end, &mut words);
                            Some((offset, Run::of(class)))
                        }
                    }
                    None => Some((offset, Run::of(class))),
                };
                open = run;
                end = next_end;
                last = class;
                after_mark = false;
                after_voiced = class == Class::KanaMark;
            }
        }
    }
    close(&mut open, end, &mut words);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words<'a>(s: &'a str, locale: &Locale) -> Vec<&'a str> {
        word_ranges(s, locale).into_iter().map(|r| &s[r]).collect()
    }

    #[test]
    fn oracle_word_counts() {
        let en = Locale::en_us();
        let count = |s: &str| word_ranges(s, &en).len();
        assert_eq!(count("don't stop-here x.y 3,5 a_b"), 7);
        assert_eq!(count("x=y=1.5"), 3);
        assert_eq!(count("The sun is rising."), 4);
        assert_eq!(count("x漢y"), 3);
        assert_eq!(count("a\u{301}'b"), 1);
        assert_eq!(count("a'\u{301}b"), 2);
        assert_eq!(count("1,\u{301}2"), 2);
        assert_eq!(count("a\u{200d}'b"), 2);
        assert_eq!(count("a1,2 1a,2 a1.2 1a.b a.1"), 7);
        assert_eq!(count("$5.00 or 5% or #5 or @x"), 7);
        assert_eq!(count("x😀y 𠀀𠀀 漢😀a"), 4);
        assert_eq!(count("'"), 0);
    }

    #[test]
    fn scripts() {
        let en = Locale::en_us();
        let count = |s: &str| word_ranges(s, &en).len();
        assert_eq!(words("漢字かな abc", &en), ["漢字", "かな", "abc"]);
        assert_eq!(count("日本語のテキストです"), 4);
        assert_eq!(count("コーヒー"), 1);
        assert_eq!(count("あーあ"), 1);
        assert_eq!(count("あーア"), 2);
        assert_eq!(count("あ゛ア"), 2);
        assert_eq!(count("ーーア"), 2);
        assert_eq!(count("゛ーア"), 1);
        assert_eq!(count("ア゛1"), 1);
        assert_eq!(count("あー1"), 2);
        assert_eq!(count("1゛2 1゛x ゛1"), 6);
        assert_eq!(count("あ\u{3099}ア"), 1);
        assert_eq!(count("1\u{301}漢"), 1);
        assert_eq!(count("\u{3099}漢"), 1);
    }

    #[test]
    fn extraction_keeps_inner_punctuation() {
        let en = Locale::en_us();
        assert_eq!(
            words("  one, two; three. four  ", &en),
            ["one", "two", "three", "four"]
        );
        assert_eq!(
            words("'a' 1,234.5 e-mail", &en),
            ["a", "1,234.5", "e", "mail"]
        );
    }

    #[test]
    fn locale_joiners() {
        let count = |s: &str, l: &Locale| word_ranges(s, l).len();
        assert_eq!(count("1/2 1.2 don't", &Locale::en_us()), 3);
        assert_eq!(count("1/2 1.2 don't", &Locale::de_de()), 4);
        assert_eq!(count("1/2 1.2 don't", &Locale::fr_fr()), 5);
        assert_eq!(count("1/2 1.2 a.b don't", &Locale::sv_se()), 6);
    }
}
