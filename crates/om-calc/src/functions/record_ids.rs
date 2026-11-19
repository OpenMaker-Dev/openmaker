//! `GetRecordIDsFromFoundSet`'s types (calc-functions.md,
//! "GetRecordIDsFromFoundSet on FileMaker Server"): a host answers the
//! function, which reads a found set; this module gives its
//! keywords and the text it writes, as FileMaker Server 26.0.3's script
//! engine writes it (script oracle s45).

use super::Keyword;
use crate::error::CalcError;
use crate::value::{Ctx, Value};

/// The named types; 1 (JSON strings) and 2 (JSON numbers) have no name.
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
/// types.
pub fn write(ids: &[u64], kind: u8) -> String {
    let json = |items: Vec<String>| format!("[{}]", items.join(","));
    match kind {
        0 => join(ids.iter().map(u64::to_string)),
        1 => json(ids.iter().map(|id| format!("\"{id}\"")).collect()),
        2 => json(ids.iter().map(u64::to_string).collect()),
        3 => join(ranges(ids).into_iter()),
        _ => json(
            ranges(ids)
                .into_iter()
                .map(|r| format!("\"{r}\""))
                .collect(),
        ),
    }
}

fn join(items: impl Iterator<Item = String>) -> String {
    items.collect::<Vec<_>>().join("\r")
}

/// Runs of ids each one more than the one before, as `first-last`; any
/// other id on its own.
fn ranges(ids: &[u64]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ids.len() {
        let mut j = i;
        while j + 1 < ids.len() && ids[j].checked_add(1) == Some(ids[j + 1]) {
            j += 1;
        }
        out.push(if j > i {
            format!("{}-{}", ids[i], ids[j])
        } else {
            ids[i].to_string()
        });
        i = j + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Script oracle s45's answers (calc-functions.md,
    /// "GetRecordIDsFromFoundSet on FileMaker Server").
    #[test]
    fn writes_each_type_as_the_server_engine_does() {
        let gaps = [1, 3, 4, 6, 7, 8];
        assert_eq!(write(&gaps, 0), "1\r3\r4\r6\r7\r8");
        assert_eq!(write(&gaps, 1), r#"["1","3","4","6","7","8"]"#);
        assert_eq!(write(&gaps, 2), "[1,3,4,6,7,8]");
        assert_eq!(write(&gaps, 3), "1\r3-4\r6-8");
        assert_eq!(write(&gaps, 4), r#"["1","3-4","6-8"]"#);
        let sorted = [7, 6, 4, 3, 1];
        assert_eq!(write(&sorted, 3), "7\r6\r4\r3\r1");
        assert_eq!(write(&[], 0), "");
        assert_eq!(write(&[], 4), "[]");
    }
}
