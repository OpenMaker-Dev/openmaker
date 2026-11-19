//! FMPXMLRESULT (`XMLE`, grammar 1), as FileMaker Server writes it (oracle
//! x01, x03): one line without a final newline, `encoding="UTF-8"` in the
//! declaration whatever the character set (a UTF-16 file is little-endian
//! without a byte order mark), `&`, `<`, `>`, `"`, `'` as entities,
//! characters beyond U+FFFF as numeric references, control characters
//! other than tab, return and line feed left out. `RECORDS` is the table's
//! record count, `FOUND` the found set's; each repetition, and each related
//! record of a related field, is one `DATA`; a related field without a
//! related record is an empty `COL`.

use super::{Cell, Table};
use crate::FieldType;

/// Escape character data and attribute values.
pub(crate) fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\r' | '\n' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c if (c as u32) > 0xffff => {
                out.push_str("&#");
                out.push_str(&(c as u32).to_string());
                out.push(';');
            }
            c => out.push(c),
        }
    }
}

fn attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape(value, out);
    out.push('"');
}

fn type_name(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Text => "TEXT",
        FieldType::Number => "NUMBER",
        FieldType::Date => "DATE",
        FieldType::Time => "TIME",
        FieldType::Timestamp => "TIMESTAMP",
        FieldType::Container => "CONTAINER",
    }
}

pub(crate) fn write(table: &Table) -> Vec<u8> {
    let cx = &table.context;
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><FMPXMLRESULT xmlns=\"http://www.filemaker.com/fmpxmlresult\"><ERRORCODE>0</ERRORCODE><PRODUCT",
    );
    attr(&mut s, "BUILD", &cx.product_build);
    attr(&mut s, "NAME", &cx.product_name);
    attr(&mut s, "VERSION", &cx.product_version);
    s.push_str("/><DATABASE");
    attr(&mut s, "DATEFORMAT", &cx.date_format);
    attr(&mut s, "LAYOUT", &cx.layout_name);
    attr(&mut s, "NAME", &cx.file_name);
    attr(&mut s, "RECORDS", &cx.total_records.to_string());
    attr(&mut s, "TIMEFORMAT", &cx.time_format);
    s.push_str("/><METADATA>");
    for c in &table.columns {
        s.push_str("<FIELD");
        attr(
            &mut s,
            "EMPTYOK",
            if c.info.empty_ok { "YES" } else { "NO" },
        );
        attr(&mut s, "MAXREPEAT", &c.info.repetitions.max(1).to_string());
        attr(&mut s, "NAME", &c.xml_name());
        attr(&mut s, "TYPE", type_name(c.info.ty));
        s.push_str("/>");
    }
    s.push_str("</METADATA><RESULTSET");
    attr(&mut s, "FOUND", &table.found.to_string());
    s.push('>');
    for row in &table.rows {
        s.push_str("<ROW");
        attr(&mut s, "MODID", &row.record.mod_count.to_string());
        attr(&mut s, "RECORDID", &row.record.id.to_string());
        s.push('>');
        for cell in &row.cells {
            s.push_str("<COL>");
            match cell {
                Cell::Absent => {}
                Cell::Values(values) if values.is_empty() => s.push_str("<DATA></DATA>"),
                Cell::Values(values) => {
                    // A `?` has no `DATA` (oracle x05).
                    for v in values.iter().filter(|v| !v.is_invalid()) {
                        s.push_str("<DATA>");
                        escape(&v.text(), &mut s);
                        s.push_str("</DATA>");
                    }
                }
            }
            s.push_str("</COL>");
        }
        s.push_str("</ROW>");
    }
    s.push_str("</RESULTSET></FMPXMLRESULT>");
    table.charset.encode(&s)
}
