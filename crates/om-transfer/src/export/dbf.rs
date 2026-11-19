//! dBase III files (`DBF `), as FileMaker Server writes them (oracle x03):
//! field names upper-cased with spaces as underscores (10 characters at
//! most); text, time and timestamp fields as `C` as wide as their longest
//! value in characters (a related field: the longest last related value),
//! longer values written whole; numbers as `N` 19 wide, rounded to whole numbers,
//! left-aligned, past 18 digits the 18 most significant (a summary: its
//! text cut at 19 characters); dates as `D` (`YYYYMMDD`, the year without
//! leading zeros, unpadded when shorter). Values
//! are in the export's character set even when that makes a record longer
//! than its declared length, as FileMaker does with UTF-8. The end mark is
//! followed by the found set's count (32-bit little-endian). The header date
//! is the year's last two digits, month and day.
//!
//! The header holds its own length and the record length in 16 bits each,
//! so a file has at most 2,046 fields and records of at most 65,535 bytes.
//! Past either, the export is refused with error 3 before any output, so no
//! file is written. That is OpenMaker's choice: FileMaker's answer there is
//! unmeasured (script-steps.md, "Implementation notes (import/export)").

use super::{Cell, Table};
use crate::text::{parse_date, ExportValue};
use crate::{code, FieldType, Result, TransferError};

/// The most fields whose descriptors fit the header length (32 bytes each,
/// after the 32-byte header and before its 1-byte end).
const MAX_FIELDS: usize = (u16::MAX as usize - 33) / 32;

/// The header and record lengths of `fields` (kind, width), refused past
/// what their 16 bits hold.
fn lengths(fields: &[(u8, usize)]) -> Result<(u16, u16)> {
    let header = u16::try_from(32 + 32 * fields.len() + 1).map_err(|_| {
        TransferError::new(
            code::UNAVAILABLE,
            format!(
                "A dBase file holds at most {MAX_FIELDS} fields; this export has {}.",
                fields.len()
            ),
        )
    })?;
    let record = 1 + fields.iter().map(|f| f.1).sum::<usize>();
    let record = u16::try_from(record).map_err(|_| {
        TransferError::new(
            code::UNAVAILABLE,
            format!("A dBase record holds at most 65,535 bytes; this export's are {record}."),
        )
    })?;
    Ok((header, record))
}

fn name(n: &str) -> [u8; 11] {
    let mut out = [0u8; 11];
    let upper: String = n
        .chars()
        .map(|c| if c == ' ' { '_' } else { c })
        .flat_map(char::to_uppercase)
        .collect();
    for (i, b) in upper.bytes().take(10).enumerate() {
        out[i] = b;
    }
    out
}

/// A number: rounded to a whole number; past 18 digits only the 18 most
/// significant (rounded), as FileMaker writes them; a summary's text cut at
/// 19 characters.
fn number(v: &ExportValue, summary: bool) -> String {
    let Some(n) = v.number() else {
        return String::new();
    };
    if summary {
        return crate::text::number_text(&n).chars().take(19).collect();
    }
    let mut s = crate::text::number_text(&n.round(0));
    if s == "-0" {
        s = "0".into();
    }
    let digits = s.trim_start_matches('-').len();
    if digits > 18 {
        let shifted = n.round(18 - digits as i64);
        let t = crate::text::number_text(&shifted);
        let sign = if t.starts_with('-') { "-" } else { "" };
        let d: String = t.trim_start_matches('-').chars().take(18).collect();
        s = format!("{sign}{d}");
    }
    s
}

fn date(v: &ExportValue, table: &Table) -> String {
    match parse_date(&v.text(), &table.context.locale) {
        Some(d) => {
            let (y, m, day) = d.ymd();
            format!("{y}{m:02}{day:02}")
        }
        None => String::new(),
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_owned();
    let n = out.chars().count();
    out.extend(std::iter::repeat(' ').take(width.saturating_sub(n)));
    out
}

pub(crate) fn write(table: &Table) -> Result<Vec<u8>> {
    let rows = table.expanded();
    let value = |cell: &Option<Cell>| match cell {
        Some(Cell::Values(v)) => v.first().cloned(),
        _ => None,
    };
    // Field kinds and widths.
    let mut fields: Vec<(u8, usize)> = Vec::new();
    for (i, c) in table.columns.iter().enumerate() {
        fields.push(match c.info.ty {
            FieldType::Number => (b'N', 19),
            FieldType::Date => (b'D', 8),
            _ => {
                // A related field is as wide as each record's last related
                // value (FileMaker's scan ends on it); longer values are
                // written whole.
                let width = table
                    .rows
                    .iter()
                    .filter_map(|row| match &row.cells[i] {
                        Cell::Values(v) if c.info.related => v.last().cloned(),
                        Cell::Values(v) => v.first().cloned(),
                        Cell::Absent => None,
                    })
                    .map(|v| v.text().chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(1);
                (b'C', width.min(254))
            }
        });
    }
    let (header_len, record_len) = lengths(&fields)?;
    let mut out = Vec::new();
    out.push(0x03);
    let (year, month, day) = created_date(&table.context.created);
    out.extend([(year % 100) as u8, month, day]);
    out.extend((rows.len() as u32).to_le_bytes());
    out.extend(header_len.to_le_bytes());
    out.extend(record_len.to_le_bytes());
    out.extend([0u8; 16]);
    out.extend([0x00, 0x02, 0x00, 0x00]);
    for (c, (kind, width)) in table.columns.iter().zip(&fields) {
        out.extend(name(&c.info.name));
        out.push(*kind);
        out.extend([0u8; 4]);
        out.push(*width as u8);
        out.push(0);
        out.extend([0u8; 14]);
    }
    out.push(0x0d);
    for (_, cells) in &rows {
        out.push(b' ');
        for ((cell, column), (kind, width)) in cells.iter().zip(&table.columns).zip(&fields) {
            let text = match value(cell) {
                None => String::new(),
                Some(v) => match kind {
                    b'N' => number(&v, column.info.kind == crate::FieldKind::Summary),
                    b'D' => date(&v, table),
                    _ => v.text(),
                },
            };
            // A date shorter than eight characters (a year before 1000) is
            // written unpadded.
            let cell = if *kind == b'D' && !text.is_empty() {
                text
            } else {
                pad(&text, *width)
            };
            table.charset.encode_into(&cell, &mut out);
        }
    }
    out.push(0x1a);
    out.extend((table.found as u32).to_le_bytes());
    Ok(out)
}

/// Year, month, day of an export time `2026-10-29T14:17:34Z`.
fn created_date(created: &str) -> (u32, u8, u8) {
    let part = |range: std::ops::Range<usize>| -> u32 {
        created.get(range).and_then(|s| s.parse().ok()).unwrap_or(0)
    };
    (part(0..4), part(5..7) as u8, part(8..10) as u8)
}
