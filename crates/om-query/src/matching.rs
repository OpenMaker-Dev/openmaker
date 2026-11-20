//! The Match finder (kind 8, spec `docs/re/query-program.md` 6.2, 6.3): a
//! pattern a host tests each candidate record's text against, as FileMaker
//! Pro sends it for `==` criteria, quoted text and most wildcard patterns
//! (6.4).
//!
//! The pattern is a list of regex types (`DBRegexType`) with one operand per
//! text type. Types 0 (text), 1 (any one character), 2 (one digit), 3 (any
//! run of characters) and 6 (a word break) are matched in order from the
//! start, and the value must end where the pattern ends (6.3, live1).
//! Types 4 (none or one) and 5 (a wildcard that does not match a word break)
//! were not observed and are refused.

use crate::QueryError;

/// One unit of a Match pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Unit {
    /// A character of a text operand and its collation key: by the key
    /// case and accents do not count (live1 matches `test` with `Test`); a
    /// Unicode raw field compares the character itself.
    Char(char, Vec<u8>),
    /// Type 1: any one character.
    One,
    /// Type 2: one digit.
    Digit,
    /// Type 3: any run of characters, none included.
    Any,
    /// Type 6: a word break, matching no character.
    Break,
}

/// A parsed Match pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    units: Vec<Unit>,
}

/// The most units a pattern may hold, its operands' characters included:
/// FileMaker Pro writes one per operator and character of a criterion, and a
/// criterion holds at most [`crate::criteria::MAX_CRITERION_CHARS`]
/// characters.
const MAX_UNITS: usize = crate::criteria::MAX_CRITERION_CHARS;

impl Pattern {
    /// Parse the bytes after a Match finder's repetition (6.2): `u16 n`,
    /// `n` regex types, then for each text type a quoted flag and counted
    /// text (units, masked SCSU, a styles byte or a u32 length of styles
    /// to skip). Returns the pattern and the bytes left.
    pub(crate) fn parse(bytes: &[u8]) -> Result<(Pattern, &[u8]), QueryError> {
        let truncated = || QueryError::Malformed("match pattern truncated");
        let [high, low, rest @ ..] = bytes else {
            return Err(truncated());
        };
        let count = usize::from(u16::from_be_bytes([*high, *low]));
        if count > MAX_UNITS {
            return Err(QueryError::Limit("match pattern"));
        }
        let types = rest.get(..count).ok_or_else(truncated)?;
        let mut rest = &rest[count..];
        let mut units = Vec::with_capacity(count);
        for &kind in types {
            match kind {
                // Every unit counts against the cap before the pattern grows.
                _ if units.len() >= MAX_UNITS => return Err(QueryError::Limit("match pattern")),
                0 => {
                    let (_quoted, after) = rest.split_first().ok_or_else(truncated)?;
                    let (text, used) = om_scsu::decode_counted(after)
                        .map_err(|_| QueryError::Malformed("match operand text"))?;
                    let after = &after[used..];
                    // The styles: `00`, or a u32 length of runs to skip.
                    rest = match after.split_first().ok_or_else(truncated)? {
                        (0, after) => after,
                        (_, after) => {
                            let length = after.get(..4).ok_or_else(truncated)?;
                            let length = u32::from_be_bytes(length.try_into().expect("4 bytes"));
                            after.get(4 + length as usize..).ok_or_else(truncated)?
                        }
                    };
                    // Quoted and unquoted text compare alike: live1 found the
                    // same records for both, and how `IsMatchFoldingAll`
                    // differs is not measured.
                    if units.len() + text.chars().count() > MAX_UNITS {
                        return Err(QueryError::Limit("match pattern"));
                    }
                    units.extend(text.chars().map(|c| Unit::Char(c, char_key(c))));
                }
                1 => units.push(Unit::One),
                2 => units.push(Unit::Digit),
                3 => units.push(Unit::Any),
                6 => units.push(Unit::Break),
                other => return Err(QueryError::Unsupported("match regex type", other)),
            }
        }
        Ok((Pattern { units }, rest))
    }

    /// The pattern's length in units, for the work a test costs.
    pub(crate) fn len(&self) -> usize {
        self.units.len()
    }

    /// The pattern ready for one field's values, prepared once: by
    /// collation key, characters with no weight (accents alone, controls)
    /// are passed over, as the index ignores them; `raw` (a Unicode raw
    /// field) keeps every character as written.
    pub(crate) fn prepare(&self, raw: bool) -> Prepared<'_> {
        Prepared {
            units: self
                .units
                .iter()
                .filter(|u| raw || !matches!(u, Unit::Char(_, key) if key.is_empty()))
                .collect(),
            breaks: self.units.contains(&Unit::Break),
            raw,
        }
    }

    /// Whether `text` matches the whole pattern ([`Prepared::matches`]).
    #[cfg(test)]
    pub(crate) fn matches(&self, text: &str, raw: bool) -> bool {
        self.prepare(raw).matches(text)
    }
}

/// A [`Pattern`] prepared for a field's values ([`Pattern::prepare`]).
pub(crate) struct Prepared<'p> {
    units: Vec<&'p Unit>,
    /// Whether a unit is a word break, so values need their word edges.
    breaks: bool,
    raw: bool,
}

impl Prepared<'_> {
    /// The units a value is matched against, for the work a test costs.
    pub(crate) fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether `text` matches the whole pattern (6.3: from its start,
    /// ending where the pattern ends), characters with no weight passed
    /// over unless the field is raw. Linear in the text to prepare, then at
    /// most the units times the text's steps.
    pub(crate) fn matches(&self, text: &str) -> bool {
        let raw = self.raw;
        let chars: Vec<(usize, char, Vec<u8>)> = text
            .char_indices()
            .map(|(offset, c)| (offset, c, char_key(c)))
            .filter(|(_, _, key)| raw || !key.is_empty())
            .collect();
        // Word edges only for a pattern that asks for them.
        let breaks = if self.breaks {
            word_breaks(text, &chars)
        } else {
            vec![false; chars.len() + 1]
        };
        matches_at(&self.units, &chars, &breaks, raw)
    }
}

/// A character's collation key, its primary weights (`om_collate::text_key`
/// without the leading `01`); empty for an ignorable one.
fn char_key(c: char) -> Vec<u8> {
    let mut buffer = [0u8; 4];
    om_collate::text_key(c.encode_utf8(&mut buffer))[1..].to_vec()
}

/// For each place between the kept characters (one more than there are),
/// whether a word of the word index starts or ends there. An edge between
/// characters that were passed over (after `e` and U+0301) marks the place
/// before the next kept one. The words come in order, so one cursor walks
/// the characters once.
fn word_breaks(text: &str, chars: &[(usize, char, Vec<u8>)]) -> Vec<bool> {
    let mut breaks = vec![false; chars.len() + 1];
    let mut at = 0;
    for word in om_collate::words(text) {
        let start = word.as_ptr() as usize - text.as_ptr() as usize;
        for edge in [start, start + word.len()] {
            while chars.get(at).is_some_and(|(offset, _, _)| *offset < edge) {
                at += 1;
            }
            breaks[at] = true;
        }
    }
    breaks[chars.len()] = true;
    breaks
}

/// The usual iterative wildcard match, `Break` matching no character where
/// a word starts or ends: at most the pattern's length times the text's
/// steps, however many `Any` there are.
fn matches_at(
    units: &[&Unit],
    chars: &[(usize, char, Vec<u8>)],
    breaks: &[bool],
    raw: bool,
) -> bool {
    let (mut p, mut c) = (0, 0);
    // The latest `Any` and the character its match ends at.
    let mut star: Option<(usize, usize)> = None;
    loop {
        match units.get(p).copied() {
            Some(Unit::Any) => {
                star = Some((p, c));
                p += 1;
                continue;
            }
            Some(Unit::Break) if breaks[c] => {
                p += 1;
                continue;
            }
            Some(unit) if c < chars.len() && takes(unit, &chars[c], raw) => {
                p += 1;
                c += 1;
                continue;
            }
            None if c == chars.len() => return true,
            _ => {}
        }
        match star {
            Some((at, end)) if end < chars.len() => {
                star = Some((at, end + 1));
                p = at + 1;
                c = end + 1;
            }
            _ => return false,
        }
    }
}

fn takes(unit: &Unit, (_, c, key): &(usize, char, Vec<u8>), raw: bool) -> bool {
    match unit {
        Unit::Char(expected, _) if raw => c == expected,
        Unit::Char(_, expected) => key == expected,
        Unit::One => true,
        Unit::Digit => c.is_ascii_digit(),
        Unit::Any | Unit::Break => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pattern's bytes: `u16 n`, the types, then each text operand
    /// unquoted.
    fn pattern(parts: &[Result<&str, u8>]) -> Vec<u8> {
        let mut bytes = (parts.len() as u16).to_be_bytes().to_vec();
        bytes.extend(parts.iter().map(|p| match p {
            Ok(_) => 0,
            Err(kind) => *kind,
        }));
        for text in parts.iter().filter_map(|p| p.ok()) {
            bytes.push(0);
            bytes.extend(om_scsu::encode_counted(text));
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn patterns_match_as_live1_did() {
        // Spec 6.2: the unquoted operand "Test".
        let test = pattern(&[Ok("Test")]);
        assert_eq!(test, [0, 1, 0, 0, 0x04, 0x0e, 0x3f, 0x29, 0x2e, 0x00]);
        let parse = |bytes: &[u8]| Pattern::parse(bytes).unwrap().0;
        // live1's First Name values: records 1, 2, 3, 10, 12 (6.3, 12).
        let values = ["Test", "Hello", "Ima", "A long text, Test", "One Test here"];
        let found = |p: &Pattern| -> Vec<usize> {
            (0..values.len())
                .filter(|&i| p.matches(values[i], false))
                .collect()
        };
        assert_eq!(found(&parse(&test)), [0]);
        assert_eq!(found(&parse(&pattern(&[Ok("test")]))), [0]);
        assert_eq!(found(&parse(&pattern(&[Ok("Te"), Err(3)]))), [0]);
        assert_eq!(found(&parse(&pattern(&[Err(3), Ok("est")]))), [0, 3]);
        assert!(found(&parse(&pattern(&[Ok("Qqqq")]))).is_empty());
        // One character, a digit, and a word break.
        assert!(parse(&pattern(&[Ok("T"), Err(1), Ok("st")])).matches("Test", false));
        assert!(parse(&pattern(&[Ok("A"), Err(2)])).matches("A7", false));
        assert!(!parse(&pattern(&[Ok("A"), Err(2)])).matches("Ab", false));
        let words = parse(&pattern(&[Err(3), Err(6), Ok("Test"), Err(6), Err(3)]));
        assert!(words.matches("A long text, Test", false) && !words.matches("Tester", false));
        // A word break after a combining mark (`e` + U+0301 ends the word).
        let cafe = parse(&pattern(&[Ok("Cafe"), Err(6), Err(3)]));
        assert!(cafe.matches("Cafe\u{301} noir", false));
        // Unicode raw compares characters as written.
        assert!(parse(&test).matches("Test", true) && !parse(&test).matches("test", true));
        // Unobserved types, and truncated bytes, are refused.
        assert!(matches!(
            Pattern::parse(&[0, 1, 4]),
            Err(QueryError::Unsupported(_, 4))
        ));
        assert!(Pattern::parse(&test[..6]).is_err());
        // The unit cap counts operators too: a literal at the cap, then `*`.
        let full = "a".repeat(MAX_UNITS);
        assert!(Pattern::parse(&pattern(&[Ok(&full)])).is_ok());
        assert!(matches!(
            Pattern::parse(&pattern(&[Ok(&full), Err(3)])),
            Err(QueryError::Limit(_))
        ));
    }
}
