//! Tab-separated text (`TABS`), comma-separated text (`COMS`) and merge
//! files (`MRGE`), as FileMaker Server writes them (oracle x01, x03):
//!
//! - every row ends with a carriage return, the last one too;
//! - repetitions (and nothing else) are separated by the group separator
//!   (0x1D);
//! - a return in a value is written as a vertical tab (0x0B);
//! - tab: a tab in a value is a space; values are not quoted;
//! - comma: every value is quoted, quotes doubled, tabs kept; a number
//!   field writes its number; a related field without a related record and
//!   a `?` value ([`ExportValue::is_invalid`]) are empty without quotes;
//! - merge: comma-separated values with number fields as stored, after a
//!   header row of the field names (no occurrence; `S by Company` for a
//!   summary grouped by a break field), comma-separated and unquoted.

use super::{Cell, Table};
use crate::text::ExportValue;
use crate::FieldType;

const GS: char = '\u{1d}';
const VT: char = '\u{0b}';

fn tab_text(v: &ExportValue) -> String {
    if v.is_invalid() {
        return String::new();
    }
    v.text()
        .chars()
        .map(|c| match c {
            '\t' => ' ',
            '\r' => VT,
            c => c,
        })
        .collect()
}

fn quoted_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\"\""),
            '\r' => out.push(VT),
            c => out.push(c),
        }
    }
}

fn encode(table: &Table, text: &str) -> Vec<u8> {
    let mut out = table.charset.preamble().to_vec();
    table.charset.encode_into(text, &mut out);
    out
}

pub(crate) fn tab(table: &Table) -> Vec<u8> {
    let mut text = String::new();
    for (_, cells) in table.expanded() {
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            if let Some(Cell::Values(values)) = cell {
                let parts: Vec<String> = values.iter().map(tab_text).collect();
                text.push_str(&parts.join(&GS.to_string()));
            }
        }
        text.push('\r');
    }
    encode(table, &text)
}

fn quoted_rows(table: &Table, numbers: bool, text: &mut String) {
    for (_, cells) in table.expanded() {
        for (i, (cell, column)) in cells.iter().zip(&table.columns).enumerate() {
            if i > 0 {
                text.push(',');
            }
            match cell {
                Some(Cell::Absent) => {}
                // A `?` alone is an unquoted empty field (oracle x05).
                Some(Cell::Values(values))
                    if !values.is_empty() && values.iter().all(ExportValue::is_invalid) => {}
                None => text.push_str("\"\""),
                Some(Cell::Values(values)) => {
                    text.push('"');
                    for (r, v) in values.iter().enumerate() {
                        if r > 0 {
                            text.push(GS);
                        }
                        if v.is_invalid() {
                            continue;
                        }
                        if numbers && column.info.ty == FieldType::Number {
                            quoted_text(&v.number_text(), text);
                        } else {
                            quoted_text(&v.text(), text);
                        }
                    }
                    text.push('"');
                }
            }
        }
        text.push('\r');
    }
}

pub(crate) fn comma(table: &Table) -> Vec<u8> {
    let mut text = String::new();
    quoted_rows(table, true, &mut text);
    encode(table, &text)
}

pub(crate) fn merge(table: &Table) -> Vec<u8> {
    let names: Vec<String> = table.columns.iter().map(|c| c.merge_name()).collect();
    let mut text = names.join(",");
    text.push('\r');
    quoted_rows(table, false, &mut text);
    encode(table, &text)
}
