//! `GetRecordIDsFromFoundSet`'s types (calc-functions.md,
//! "GetRecordIDsFromFoundSet on FileMaker Server"): a host answers the
//! function, which reads a found set; this module gives its
//! keywords and the text it writes, as FileMaker Server 26.0.3's script
//! engine writes it (script oracle s45).

use super::Keyword;
use crate::error::CalcError;
use crate::value::{Ctx, Value};

/// The types' own names; 1 and 2 are `JSONString` and `JSONNumber`, the
/// JSON functions' keywords of those values.
pub(super) const KEYWORDS: &[Keyword] = &[
    ("ValueNumber", 0),
    ("ValueNumberRanges", 3),
    ("JSONStringRanges", 4),
];

/// Error 1708, "Parameter value is invalid": a type other than 0 to 4.
fn invalid() -> CalcError {
    CalcError::from_code(1708).expect("nonzero")
}

/// The type `value` names: a number truncated toward zero, 0 for text that
/// is no number; empty text, `?` and a number outside 0 to 4 are error
/// 1708.
pub fn kind(value: &Value, cx: &Ctx) -> Result<u8, CalcError> {
    if matches!(value, Value::Text(text) if text.is_empty()) {
        return Err(invalid());
    }
    value
        .to_number(cx)
        .to_i64()
        .and_then(|kind| u8::try_from(kind).ok())
        .filter(|kind| *kind <= 4)
        .ok_or_else(invalid)
}

/// `ids`, in their order, as type `kind` writes them: 0 one per value; 1 a
/// JSON array of their texts; 2 a JSON array of numbers; 3 one per value,
/// a run of ascending consecutive ids as `first-last`; 4 those as a JSON
/// array of texts. No id is empty text for 0 and 3 and `[]` for the JSON
/// types. Text longer than `limit` bytes fails with
/// [`CalcError::OUT_OF_MEMORY`] before it is built further, as the
/// evaluator fails text past its limit.
pub fn write(ids: &[u64], kind: u8, limit: usize) -> Result<String, CalcError> {
    use std::fmt::Write as _;
    let json = matches!(kind, 1 | 2 | 4);
    let quoted = matches!(kind, 1 | 4);
    let ranged = matches!(kind, 3 | 4);
    let mut out = String::new();
    if json {
        out.push('[');
    }
    let mut i = 0;
    while i < ids.len() {
        let mut j = i;
        while ranged && j + 1 < ids.len() && ids[j].checked_add(1) == Some(ids[j + 1]) {
            j += 1;
        }
        if i > 0 {
            out.push(if json { ',' } else { '\r' });
        }
        if quoted {
            out.push('"');
        }
        let _ = write!(out, "{}", ids[i]);
        if j > i {
            let _ = write!(out, "-{}", ids[j]);
        }
        if quoted {
            out.push('"');
        }
        if out.len() > limit {
            return Err(CalcError::OUT_OF_MEMORY);
        }
        i = j + 1;
    }
    if json {
        out.push(']');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Script oracle s45's answers (calc-functions.md,
    /// "GetRecordIDsFromFoundSet on FileMaker Server").
    #[test]
    fn writes_each_type_as_the_server_engine_does() {
        let at = |ids: &[u64], kind| write(ids, kind, usize::MAX).unwrap();
        let gaps = [1, 3, 4, 6, 7, 8];
        assert_eq!(at(&gaps, 0), "1\r3\r4\r6\r7\r8");
        assert_eq!(at(&gaps, 1), r#"["1","3","4","6","7","8"]"#);
        assert_eq!(at(&gaps, 2), "[1,3,4,6,7,8]");
        assert_eq!(at(&gaps, 3), "1\r3-4\r6-8");
        assert_eq!(at(&gaps, 4), r#"["1","3-4","6-8"]"#);
        let sorted = [7, 6, 4, 3, 1];
        assert_eq!(at(&sorted, 3), "7\r6\r4\r3\r1");
        assert_eq!(at(&[], 0), "");
        assert_eq!(at(&[], 4), "[]");
        assert_eq!(write(&gaps, 0, 6), Err(CalcError::OUT_OF_MEMORY));
    }
}
