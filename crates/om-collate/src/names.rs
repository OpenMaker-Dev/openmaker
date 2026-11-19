//! The keys FileMaker lists catalog member names under (spec
//! `docs/re/schema-path.md` 5.6): a table's, occurrence's, field's,
//! layout's or script's name in its catalog's name list.
//!
//! FileMaker builds them in one fixed language (`Locale::Type` 4, the
//! catalog language, whatever the file's own language) with the levels
//! above the primary one: the primary weights, `00 00`, the secondary
//! level, `00`, then the tertiary level. A level whose bytes would all be
//! `01` (no accent, or no width or kana mark) is left out, so an ASCII
//! name's key is its primary weights and `00 00 00`. Case does not count.

use crate::{table, Element};

/// The levels a name key holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels {
    /// Primary and secondary: a field catalog's list by type
    /// `[T, 3, 1, 06]`, which has no tertiary level and no `00` after the
    /// secondary one.
    Two,
    /// Primary, secondary and tertiary: a catalog's name list
    /// `[cat, 1, 01]`.
    Three,
}

/// Why a name has no key OpenMaker can build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unkeyed {
    /// It holds a character whose weights are not established: a low or
    /// reversed quotation mark other than `„`, a CJK compatibility
    /// ideograph (keyed as its canonical ideograph, which needs a table
    /// this crate does not hold), or, in a name with a secondary or
    /// tertiary level, a code point FileMaker gives its own weights other
    /// than a CJK unified ideograph.
    Character(char),
    /// One of its secondary or tertiary weights would be a zero byte,
    /// which the key cannot tell from the end of a level; what FileMaker
    /// writes for it is not established.
    Weight(char),
}

/// The key of `name` in a catalog's name lists with `levels` (spec 5.6),
/// or why OpenMaker cannot build it.
///
/// Each code point is looked up on its own (no contractions, query-program.md
/// 7.2), U+FFFE and U+FFFF add nothing, `‘`, `’`, `“`, `”` and `„` key as the
/// straight marks with secondary weight 0x21, and the catalog language
/// keys `ı` as `i`. The primary level is each non-zero primary weight as a
/// big-endian u16; the secondary level one byte per non-zero secondary
/// weight, the weight minus 0x1F (its low byte: `ß`'s 0x159 is `3a`); the
/// tertiary level one byte per non-zero tertiary weight, the weight
/// without its case minus 1.
pub fn name_key(name: &str, levels: Levels) -> Result<Vec<u8>, Unkeyed> {
    let mut primary = Vec::new();
    let mut secondary = Vec::new();
    let mut tertiary = Vec::new();
    let mut inferred = None;
    for c in name.chars() {
        let read = elements(c)?;
        if read.inferred {
            inferred.get_or_insert(c);
        }
        for element in read.elements {
            if element.primary != 0 {
                primary.extend(element.primary.to_be_bytes());
            }
            if element.secondary != 0 {
                let byte = element.secondary.wrapping_sub(0x1F) as u8;
                if byte == 0 {
                    return Err(Unkeyed::Weight(c));
                }
                secondary.push(byte);
            }
            if element.tertiary != 0 {
                let byte = uncased(element.tertiary).wrapping_sub(1) as u8;
                if byte == 0 {
                    return Err(Unkeyed::Weight(c));
                }
                tertiary.push(byte);
            }
        }
    }
    let shown = |level: &[u8]| level.iter().any(|&b| b != 1);
    let secondary_shown = shown(&secondary);
    let tertiary_shown = levels == Levels::Three && shown(&tertiary);
    if let Some(c) = inferred.filter(|_| secondary_shown || tertiary_shown) {
        return Err(Unkeyed::Character(c));
    }
    let mut key = primary;
    key.extend([0, 0]);
    if secondary_shown {
        key.extend(&secondary);
    }
    if levels == Levels::Three {
        key.push(0);
        if tertiary_shown {
            key.extend(&tertiary);
        }
    }
    Ok(key)
}

/// What a name key reads for one character.
struct Read {
    elements: Vec<Element>,
    /// Whether they are FileMaker's own weights for a code point its table
    /// does not list, other than a CJK unified ideograph: that they add
    /// nothing to the higher levels, as an ideograph's do, is inferred.
    inferred: bool,
}

/// The collation elements a name key reads for `c`.
fn elements(c: char) -> Result<Read, Unkeyed> {
    let listed = |c: char| table().singles[&u32::from(c)].to_vec();
    let quote = |c: char, tertiary: u16| {
        let mut out = listed(c);
        out[0].secondary = 0x21;
        out[0].tertiary = tertiary;
        out
    };
    let read = |elements: Vec<Element>| Read {
        elements,
        inferred: false,
    };
    // FileMaker's own weights for a code point its table does not list
    // (query-program.md 7.2), with no secondary or tertiary weight: the
    // starter files' names that mix kanji and kana have higher levels only
    // for the kana.
    let own = |weights: &[u16], inferred: bool| Read {
        elements: weights
            .iter()
            .map(|&primary| Element {
                primary,
                secondary: 0,
                tertiary: 0,
            })
            .collect(),
        inferred,
    };
    let code = u32::from(c);
    Ok(match code {
        0xFFFE | 0xFFFF => read(Vec::new()),
        // `‘`, `’` and `“` key as the straight marks with secondary 0x21
        // (`”` is taken to as well), `„` with tertiary 0x04 too; what the
        // other low and reversed marks take is not established.
        0x2018 | 0x2019 => read(quote('\'', 0x02)),
        0x201C | 0x201D => read(quote('"', 0x02)),
        0x201E => read(quote('"', 0x04)),
        0x201A | 0x201B | 0x201F => return Err(Unkeyed::Character(c)),
        // The catalog language's one tailoring.
        0x0131 => read(listed('i')),
        0xF900..=0xFAFF | 0x2F800..=0x2FA1F => return Err(Unkeyed::Character(c)),
        _ => match table().singles.get(&code) {
            Some(elements) => read(elements.to_vec()),
            None => match code {
                0x4E00..=0x9FCB => own(&[code as u16], false),
                0x3400..=0x4DB5 => own(&[(code + 0x6FCB) as u16], true),
                0xAC00..=0xD7A3 => own(&[(code + 0x19AD) as u16], true),
                0x20000..=0x2B7FF => {
                    let mut units = [0; 2];
                    c.encode_utf16(&mut units);
                    own(&[units[0] - 0x16C0, units[1]], true)
                }
                0..=0xFFFF => own(&[0xFFE0, code as u16], true),
                _ => own(&[0xFFE0 + (code >> 16) as u16, code as u16], true),
            },
        },
    })
}

/// A tertiary weight without its case: DUCET's capital forms (UTS #10
/// 5.2.0, 7.3) as the small ones.
fn uncased(tertiary: u16) -> u16 {
    match tertiary {
        // Capital, and capital wide, compat, font and circle.
        0x08..=0x0C => tertiary - 6,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(key: Result<Vec<u8>, Unkeyed>) -> String {
        key.unwrap().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn keys_names_as_filemaker_lists_them() {
        // FileMaker Pro 26's own keys (schema-path.md 5.6): ASCII in every
        // language, an accent and `ß` in a German file, kana in a Japanese
        // one, kanji (no higher levels) beside kana, Hangul, and `„`.
        let three = |name| hex(name_key(name, Levels::Three));
        assert_eq!(three("Contacts"), "123d138e136d1433120f123d14331410000000");
        assert_eq!(three("CONTACTS"), three("contacts"));
        assert_eq!(
            three("Größe"),
            "12b013da138e14101410126b000001010128013a0101000101010103031e01"
        );
        assert_eq!(
            hex(name_key("Größe", Levels::Two)),
            "12b013da138e14101410126b000001010128013a0101"
        );
        assert_eq!(three("テスト"), "2b272b212b28000000101010");
        assert_eq!(three("主キー"), "4e3b2b1b11dd0000001001");
        assert_eq!(three("제품"), "e1c9ee35000000");
        assert_eq!(three("[\u{e002}]"), "0301ffe0e0020302000000");
        assert_eq!(three("„a“"), "02f5120f02f5000002010200030101");
    }

    #[test]
    fn refuses_what_is_not_established() {
        // A low single quotation mark, a compatibility ideograph, and a
        // private-use character beside an accent.
        assert_eq!(name_key("‚a", Levels::Three), Err(Unkeyed::Character('‚')));
        assert_eq!(
            name_key("\u{f900}", Levels::Three),
            Err(Unkeyed::Character('\u{f900}'))
        );
        assert_eq!(
            name_key("\u{e002}é", Levels::Three),
            Err(Unkeyed::Character('\u{e002}'))
        );
        assert!(name_key("\u{e002}e", Levels::Three).is_ok());
    }
}
