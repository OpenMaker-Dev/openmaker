//! HTML table files (`HTML`), as FileMaker Server writes them (oracle x01,
//! x03): one tag per line (carriage returns), a header row of field names,
//! `<BR>` for an empty value and for each return, `&`, `<`, `>` and `"` as
//! entities, a repeating field as a nested one-row table. Only the text
//! (names and values) is in the export's character set; the markup is
//! ASCII, and the `CHARSET` of a UTF-16 file is missing (the attribute is
//! cut off, as FileMaker writes it).
//!
//! The rows a related field adds hold `<TD></TD>` for the other fields,
//! except that the cells before the first related value are wrapped in a
//! one-cell table when the previous cell written was a repeating field's.

use super::{Cell, Table};
use crate::text::ExportValue;
use crate::ExportCharset;

fn escape(text: &str) -> String {
    if text.is_empty() {
        return "<BR>".into();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("<BR>"),
            c => out.push(c),
        }
    }
    out
}

/// The `CHARSET` part of the content type (verified for UTF-8 and UTF-16
/// only).
fn charset_part(charset: ExportCharset) -> &'static str {
    match charset {
        ExportCharset::Utf8 => "CHARSET=UTF-8\"",
        ExportCharset::Utf16 => "",
        ExportCharset::Windows => "CHARSET=windows-1252\"",
        ExportCharset::Dos => "CHARSET=IBM850\"",
        ExportCharset::Macintosh => "CHARSET=macintosh\"",
        ExportCharset::ShiftJis => "CHARSET=Shift_JIS\"",
        ExportCharset::Gb => "CHARSET=GB18030\"",
        ExportCharset::Big5 => "CHARSET=Big5\"",
        ExportCharset::EucKr => "CHARSET=EUC-KR\"",
    }
}

struct Writer {
    out: Vec<u8>,
    charset: ExportCharset,
}

impl Writer {
    fn markup(&mut self, s: &str) {
        self.out.extend(s.as_bytes());
    }

    /// Text in the export's character set; markup produced by `escape`
    /// (`<BR>`, entities) stays ASCII.
    fn text(&mut self, s: &str) {
        let mut rest = s;
        while !rest.is_empty() {
            let markup = ["<BR>", "&amp;", "&lt;", "&gt;", "&quot;"]
                .iter()
                .find(|m| rest.starts_with(**m));
            match markup {
                Some(m) => {
                    self.markup(m);
                    rest = &rest[m.len()..];
                }
                None => {
                    let c = rest.chars().next().unwrap();
                    let mut buf = [0u8; 4];
                    self.charset
                        .encode_into(c.encode_utf8(&mut buf), &mut self.out);
                    rest = &rest[c.len_utf8()..];
                }
            }
        }
    }

    fn value(&mut self, v: &ExportValue) {
        let text = escape(&v.text());
        self.text(&text);
    }
}

pub(crate) fn write(table: &Table) -> Vec<u8> {
    let mut w = Writer {
        out: Vec::new(),
        charset: table.charset,
    };
    w.markup("<HTML>\r<HEAD><META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html;");
    w.markup(charset_part(table.charset));
    w.markup("></HEAD>\r<BODY><TABLE BORDER=1>\r<TR>\r");
    for c in &table.columns {
        w.markup("<TH>");
        w.text(&escape(&c.info.name));
        w.markup("</TH>\r");
    }
    w.markup("</TR>\r");
    let mut after_repetitions = false;
    for (extra, cells) in table.expanded() {
        w.markup("<TR>\r");
        for (cell, column) in cells.iter().zip(&table.columns) {
            match cell {
                None => {
                    if after_repetitions {
                        w.markup("<TD><TABLE BORDER=1><TR><TD></TD></TR></TABLE></TD>\r");
                    } else {
                        w.markup("<TD></TD>\r");
                    }
                }
                Some(Cell::Absent) => {
                    w.markup(if extra {
                        "<TD></TD>\r"
                    } else {
                        "<TD><BR></TD>\r"
                    });
                    after_repetitions = false;
                }
                Some(Cell::Values(values)) if column.info.repetitions > 1 && !extra => {
                    w.markup("<TD><TABLE BORDER=1><TR>");
                    for v in values {
                        w.markup("<TD>");
                        w.value(v);
                        w.markup("</TD>");
                    }
                    w.markup("</TR></TABLE></TD>\r");
                    after_repetitions = true;
                }
                Some(Cell::Values(values)) => {
                    w.markup("<TD>");
                    match values.first() {
                        Some(v) => w.value(v),
                        None => w.markup("<BR>"),
                    }
                    w.markup("</TD>\r");
                    after_repetitions = false;
                }
            }
        }
        w.markup("</TR>\r");
    }
    w.markup("</TABLE></BODY></HTML>\r");
    w.out
}
