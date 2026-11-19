//! Reading import sources into rows of columns of repetition values
//! (oracle x02 on FileMaker Server 26.0.3):
//!
//! - Tab-separated: records end at a return, a line feed or both; fields
//!   are split at the stored delimiter (a tab); quotes are data.
//! - Comma-separated and merge: a quote turns quoting on or off wherever it
//!   is (`"q"x"y"` is `qxy`, ` "7" ` is ` 7 `); two quotes inside quotes are
//!   one; a return, line feed or both inside quotes is one return.
//! - In every text format a vertical tab is a return and the group
//!   separator (0x1D) separates repetitions; a blank line is a record of
//!   empty fields.
//! - Merge files always start with the field names row.
//!
//! Every reader keeps to [`super::limit`]: rows of at most 16,384 values,
//! a budget for what the rows take, and per format the checks below. A
//! row may end before its last column (a column it lacks is empty), and a
//! gap before a worksheet cell is a value of no repetitions.

use std::io::Read;
use std::mem::size_of;

use super::limit::{self, Budget};
use crate::charset::ImportCharset;
use crate::xml::{attr, events, Event};
use crate::{code, Result, TransferError};

/// One source record: its columns, each a list of repetition values.
pub type SourceRow = Vec<Vec<String>>;

/// A parsed source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// Field names the source names its columns with (XML metadata, a
    /// merge file's first row); a names row of a text file is taken by the
    /// engine.
    pub names: Option<Vec<String>>,
    pub rows: Vec<SourceRow>,
}

const GS: char = '\u{1d}';

/// The error of an unreadable text or dBase file.
const UNREADABLE: i32 = 708;

/// The error of an unreadable Excel workbook.
const WORKBOOK: i32 = 730;

/// The most end of central directory record signatures a workbook package
/// may hold ([`end_record_fits`]).
const END_RECORDS: usize = 16;

fn value(raw: String) -> Vec<String> {
    let raw = raw.replace('\u{0b}', "\r");
    raw.split(GS).map(str::to_owned).collect()
}

/// Rows of a text source as they are read, within the budget.
struct Text<'b> {
    rows: Vec<SourceRow>,
    row: SourceRow,
    field: String,
    budget: &'b mut Budget,
}

impl<'b> Text<'b> {
    fn new(budget: &'b mut Budget) -> Text<'b> {
        Text {
            rows: Vec::new(),
            row: Vec::new(),
            field: String::new(),
            budget,
        }
    }

    /// A character of the field being read, charged as it comes.
    fn push(&mut self, c: char) -> Result<()> {
        self.budget.charge(c.len_utf8())?;
        self.field.push(c);
        Ok(())
    }

    fn field(&mut self) -> Result<()> {
        self.budget.columns(self.row.len() + 1)?;
        let v = value(std::mem::take(&mut self.field));
        self.budget.slots(&v)?;
        self.row.push(v);
        Ok(())
    }

    fn end_row(&mut self) -> Result<()> {
        self.budget.row()?;
        self.rows.push(std::mem::take(&mut self.row));
        Ok(())
    }
}

/// Tab-separated (or another single-character delimiter).
pub fn delimited(text: &str, delimiter: char) -> Result<Vec<SourceRow>> {
    delimited_in(text, delimiter, &mut Budget::new(UNREADABLE))
}

fn delimited_in(text: &str, delimiter: char, budget: &mut Budget) -> Result<Vec<SourceRow>> {
    let mut out = Text::new(budget);
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.field()?;
            out.end_row()?;
            any = false;
        } else if c == delimiter {
            out.field()?;
        } else {
            out.push(c)?;
        }
    }
    if any {
        out.field()?;
        out.end_row()?;
    }
    Ok(out.rows)
}

/// Comma-separated with FileMaker's quoting (module docs).
pub fn comma(text: &str, delimiter: char) -> Result<Vec<SourceRow>> {
    comma_in(text, delimiter, &mut Budget::new(UNREADABLE))
}

fn comma_in(text: &str, delimiter: char, budget: &mut Budget) -> Result<Vec<SourceRow>> {
    let mut out = Text::new(budget);
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                out.push('"')?;
            }
            '"' => quoted = !quoted,
            '\r' | '\n' if quoted => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\r')?;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.field()?;
                out.end_row()?;
                any = false;
            }
            c if c == delimiter && !quoted => out.field()?,
            c => out.push(c)?,
        }
    }
    if any {
        out.field()?;
        out.end_row()?;
    }
    Ok(out.rows)
}

/// A merge file: the first row names the fields.
pub fn merge(text: &str) -> Result<Source> {
    let mut rows = comma(text, ',')?;
    let names = if rows.is_empty() {
        None
    } else {
        Some(
            rows.remove(0)
                .into_iter()
                .map(|v| v.into_iter().next().unwrap_or_default())
                .collect(),
        )
    };
    Ok(Source { names, rows })
}

fn xml_error() -> TransferError {
    TransferError::new(code::XML_PARSE, "The XML source could not be parsed.")
}

/// FMPXMLRESULT: the metadata names the columns; each `DATA` of a `COL` is
/// a repetition (or a related record's value).
pub fn fmpxmlresult(bytes: &[u8], charset: ImportCharset) -> Result<Source> {
    fmpxmlresult_in(bytes, charset, &mut Budget::new(code::XML_PARSE))
}

fn fmpxmlresult_in(bytes: &[u8], charset: ImportCharset, budget: &mut Budget) -> Result<Source> {
    let text = xml_text(bytes, charset);
    let mut names = Vec::new();
    let mut rows = Vec::new();
    let mut row: Option<SourceRow> = None;
    let mut col: Option<Vec<String>> = None;
    let mut data: Option<String> = None;
    let mut root = false;
    // A column ends: into the row, within the row's limit and the budget
    // (its values were charged as they were read).
    let end_col = |row: &mut Option<SourceRow>, col: Vec<String>, budget: &mut Budget| {
        if let Some(r) = row.as_mut() {
            budget.columns(r.len() + 1)?;
            budget.charge(size_of::<Vec<String>>())?;
            r.push(col);
        }
        Ok::<_, TransferError>(())
    };
    for e in events(&text) {
        match e.map_err(|_| xml_error())? {
            Event::Start { name, attrs, empty } => match Event::local(&name) {
                "FMPXMLRESULT" => root = true,
                "FIELD" => {
                    let name = attr(&attrs, "NAME").unwrap_or_default().to_owned();
                    budget.columns(names.len() + 1)?;
                    budget.value(std::slice::from_ref(&name))?;
                    names.push(name);
                }
                "ROW" => row = Some(Vec::new()),
                "COL" => {
                    if empty {
                        end_col(&mut row, Vec::new(), budget)?;
                    } else {
                        col = Some(Vec::new());
                    }
                }
                "DATA" => {
                    if empty {
                        if let Some(c) = col.as_mut() {
                            budget.charge(size_of::<String>())?;
                            c.push(String::new());
                        }
                    } else {
                        data = Some(String::new());
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(d) = data.as_mut() {
                    budget.charge(t.len())?;
                    d.push_str(&t);
                }
            }
            Event::End(name) => match Event::local(&name) {
                "DATA" => {
                    if let (Some(c), Some(d)) = (col.as_mut(), data.take()) {
                        budget.charge(size_of::<String>())?;
                        c.push(d.replace('\u{0b}', "\r"));
                    }
                }
                "COL" => {
                    if let Some(c) = col.take() {
                        end_col(&mut row, c, budget)?;
                    }
                }
                "ROW" => {
                    if let Some(r) = row.take() {
                        budget.row()?;
                        rows.push(r);
                    }
                }
                _ => {}
            },
        }
    }
    if !root {
        return Err(TransferError::new(
            717,
            "The XML source is not in the FMPXMLRESULT grammar.",
        ));
    }
    Ok(Source {
        names: Some(names),
        rows,
    })
}

/// An XML document's text: its byte order mark or declared encoding first,
/// then the import's character set.
fn xml_text(bytes: &[u8], charset: ImportCharset) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        return charset.decode(bytes);
    }
    if bytes.starts_with(b"<\0") {
        return ImportCharset::Utf16.decode(bytes);
    }
    if bytes.starts_with(b"\0<") {
        return ImportCharset::Utf16Be.decode(bytes);
    }
    ImportCharset::Utf8.decode(bytes)
}

/// A dBase III file: its field names and records (text trimmed, dates
/// `YYYYMMDD` as `MM/DD/YYYY`). The header must hold its field
/// descriptors and their end mark (0x0D) and lie within the file; each
/// field is 1 to 255 bytes and the record (a deletion flag and the
/// fields) fits the record length. Records are read up to the header's
/// count or the end of the data, whichever comes first.
pub fn dbase(bytes: &[u8], charset: ImportCharset) -> Result<Source> {
    dbase_in(bytes, charset, &mut Budget::new(UNREADABLE))
}

fn dbase_in(bytes: &[u8], charset: ImportCharset, budget: &mut Budget) -> Result<Source> {
    let bad = || TransferError::new(UNREADABLE, "The dBase file could not be read.");
    let le16 = |at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    if bytes.len() < 32 {
        return Err(bad());
    }
    let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let (header_len, record_len) = (le16(8), le16(10));
    let header = bytes.get(..header_len).ok_or_else(bad)?;
    let mut fields = Vec::new();
    let mut at = 32;
    while *header.get(at).ok_or_else(bad)? != 0x0d {
        let d = header.get(at..at + 32).ok_or_else(bad)?;
        let len = usize::from(d[16]);
        if len == 0 {
            return Err(bad());
        }
        let name_end = d[..11].iter().position(|&b| b == 0).unwrap_or(11);
        let name = charset.decode(&d[..name_end]);
        budget.columns(fields.len() + 1)?;
        budget.value(std::slice::from_ref(&name))?;
        fields.push((name, d[11], len));
        at += 32;
    }
    let width = 1 + fields.iter().map(|f| f.2).sum::<usize>();
    if record_len == 0 || width > record_len {
        return Err(bad());
    }
    let records = bytes[header_len..]
        .chunks_exact(record_len)
        .take(usize::try_from(count).unwrap_or(usize::MAX));
    let mut rows = Vec::new();
    for rec in records {
        if rec[0] == b'*' {
            continue;
        }
        budget.row()?;
        let mut pos = 1;
        let mut row = Vec::with_capacity(fields.len());
        for (_, kind, len) in &fields {
            let raw = &rec[pos..pos + len];
            pos += len;
            let text = charset.decode(raw).trim_end().to_owned();
            let text = match kind {
                b'D' if text.len() == 8 && text.is_ascii() => {
                    format!("{}/{}/{}", &text[4..6], &text[6..8], &text[0..4])
                }
                b'N' | b'F' => text.trim().to_owned(),
                _ => text,
            };
            let v = vec![text];
            budget.value(&v)?;
            row.push(v);
        }
        rows.push(row);
    }
    Ok(Source {
        names: Some(fields.into_iter().map(|f| f.0).collect()),
        rows,
    })
}

/// An Excel workbook's sheet (`sheet` 0-based, or by name): its rows of
/// cells as text (shared and inline strings, numbers as written, Booleans
/// as 1 and 0). Cell and row references are Excel's (`A1` to
/// `XFD1048576`); a cell or row without one follows the one before. The
/// package is checked before it is read: its end record must end the file
/// and point at its central directory, with at most
/// [`limit::ZIP_ENTRIES`] parts, and the parts read inflate to at most
/// [`limit::ZIP_RATIO`] times their size (or [`limit::ZIP_RATIO_FLOOR`])
/// and [`limit::ZIP_BYTES`] together.
pub fn workbook(bytes: &[u8], sheet: usize, worksheet: &str) -> Result<Vec<SourceRow>> {
    workbook_in(bytes, sheet, worksheet, &mut Budget::new(WORKBOOK))
}

fn bad_workbook() -> TransferError {
    TransferError::new(WORKBOOK, "The Excel workbook could not be read.")
}

/// A workbook package's parts, read within the zip limits.
struct Package<'a> {
    zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>,
    size: u64,
    /// What the parts read so far inflated to.
    inflated: u64,
}

impl<'a> Package<'a> {
    fn open(bytes: &'a [u8]) -> Result<Package<'a>> {
        if !end_record_fits(bytes) {
            return Err(bad_workbook());
        }
        let zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| bad_workbook())?;
        if zip.len() > limit::ZIP_ENTRIES {
            return Err(bad_workbook());
        }
        Ok(Package {
            zip,
            size: bytes.len() as u64,
            inflated: 0,
        })
    }

    /// A part's text; `None` when the package lacks it.
    fn part(&mut self, name: &str) -> Result<Option<String>> {
        let file = match self.zip.by_name(name) {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(_) => return Err(bad_workbook()),
        };
        // The compressed size is the entry's claim; it cannot exceed the
        // package.
        let compressed = file.compressed_size().min(self.size);
        let most = compressed
            .saturating_mul(limit::ZIP_RATIO)
            .max(limit::ZIP_RATIO_FLOOR)
            .min(limit::ZIP_BYTES - self.inflated);
        let mut data = Vec::new();
        file.take(most.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|_| bad_workbook())?;
        if data.len() as u64 > most {
            return Err(bad_workbook());
        }
        self.inflated += data.len() as u64;
        String::from_utf8(data)
            .map(Some)
            .map_err(|_| bad_workbook())
    }
}

/// Whether a package's end of central directory record is where a
/// workbook's is: the last one in the file, ending it (with its comment),
/// on one disk, right after its central directory, which starts with a
/// directory entry, and with at most [`limit::ZIP_ENTRIES`] entries (no
/// zip64). The zip reader takes the same record first. A file may hold
/// only a few record signatures ([`END_RECORDS`]; one in 2^32 positions by
/// chance), so a reader that falls back to earlier ones when a record
/// fails stays cheap: zip 2 tried every one in the file, each try costing
/// up to a pass over it; zip 9 looks only in the last 64 KiB.
fn end_record_fits(bytes: &[u8]) -> bool {
    const END: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const ENTRY: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    if bytes
        .windows(4)
        .filter(|w| *w == END)
        .nth(END_RECORDS)
        .is_some()
    {
        return false;
    }
    let from = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let Some(at) = bytes[from..]
        .windows(4)
        .rposition(|w| w == END)
        .map(|i| from + i)
    else {
        return false;
    };
    let Some(end) = bytes.get(at..at + 22) else {
        return false;
    };
    let u16_at = |i: usize| usize::from(u16::from_le_bytes([end[i], end[i + 1]]));
    let u32_at =
        |i: usize| u32::from_le_bytes([end[i], end[i + 1], end[i + 2], end[i + 3]]) as usize;
    let (disk, directory_disk) = (u16_at(4), u16_at(6));
    let (here, total) = (u16_at(8), u16_at(10));
    let (size, offset, comment) = (u32_at(12), u32_at(16), u16_at(20));
    disk == 0
        && directory_disk == 0
        && here == total
        && total <= limit::ZIP_ENTRIES
        && at + 22 + comment == bytes.len()
        && offset.checked_add(size) == Some(at)
        && (total == 0 || bytes.get(offset..offset + 4) == Some(&ENTRY[..]))
}

fn workbook_in(
    bytes: &[u8],
    sheet: usize,
    worksheet: &str,
    budget: &mut Budget,
) -> Result<Vec<SourceRow>> {
    let bad = bad_workbook;
    let mut package = Package::open(bytes)?;
    let strings = match package.part("xl/sharedStrings.xml")? {
        Some(doc) => shared_strings(&doc, budget)?,
        None => Vec::new(),
    };
    // Which sheet part: by name through the workbook and its
    // relationships, else by position.
    let mut target = format!("xl/worksheets/sheet{}.xml", sheet.saturating_add(1));
    let workbook = package.part("xl/workbook.xml")?;
    let rels = package.part("xl/_rels/workbook.xml.rels")?;
    if let (Some(wb), Some(rels)) = (workbook, rels) {
        let (mut by_name, mut by_position) = (None, None);
        let mut index = 0;
        for e in events(&wb) {
            if let Event::Start { name, attrs, .. } = e.map_err(|_| bad())? {
                if Event::local(&name) != "sheet" {
                    continue;
                }
                let id = attrs
                    .iter()
                    .find(|(k, _)| k.ends_with(":id"))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if !worksheet.is_empty() && attr(&attrs, "name") == Some(worksheet) {
                    by_name.get_or_insert(id);
                } else if index == sheet {
                    by_position = Some(id);
                }
                index += 1;
            }
        }
        if let Some(rid) = by_name.or(by_position) {
            for e in events(&rels) {
                if let Event::Start { name, attrs, .. } = e.map_err(|_| bad())? {
                    if Event::local(&name) == "Relationship" && attr(&attrs, "Id") == Some(&rid) {
                        let t = attr(&attrs, "Target").unwrap_or_default();
                        target = match t.strip_prefix('/') {
                            Some(abs) => abs.to_owned(),
                            None => format!("xl/{t}"),
                        };
                    }
                }
            }
        }
    }
    let doc = package.part(&target)?.ok_or_else(bad)?;
    sheet_rows(&doc, &strings, budget)
}

/// A worksheet's rows (see [`workbook`]).
fn sheet_rows(doc: &str, strings: &[String], budget: &mut Budget) -> Result<Vec<SourceRow>> {
    let bad = bad_workbook;
    let mut rows: Vec<SourceRow> = Vec::new();
    // The row being read, and the column of a cell without a reference.
    let mut row: Option<usize> = None;
    let mut next_column = 0;
    let mut cell: Option<(usize, String)> = None;
    let mut text: Option<String> = None;
    let mut in_value = false;
    let mut phonetic = false;
    for e in events(doc) {
        match e.map_err(|_| bad())? {
            Event::Start { name, attrs, empty } => match Event::local(&name) {
                "row" => {
                    let index = match attr(&attrs, "r") {
                        Some(r) => row_index(r).ok_or_else(bad)?,
                        None if rows.len() < limit::SHEET_ROWS => rows.len(),
                        None => return Err(bad()),
                    };
                    while rows.len() <= index {
                        budget.row()?;
                        rows.push(Vec::new());
                    }
                    row = (!empty).then_some(index);
                    next_column = 0;
                }
                "c" => {
                    // A cell belongs in a row.
                    if row.is_none() {
                        return Err(bad());
                    }
                    let column = match attr(&attrs, "r") {
                        Some(r) => column_index(r).ok_or_else(bad)?,
                        None if next_column < limit::COLUMNS => next_column,
                        None => return Err(bad()),
                    };
                    next_column = column + 1;
                    let kind = attr(&attrs, "t").unwrap_or("n").to_owned();
                    cell = (!empty).then_some((column, kind));
                    text = None;
                }
                "rPh" if !empty => phonetic = true,
                "v" | "t" if cell.is_some() && !empty && !phonetic => {
                    // Runs of rich text add up.
                    text.get_or_insert_with(String::new);
                    in_value = true;
                }
                _ => {}
            },
            Event::Text(t) => {
                if let (true, Some(s)) = (in_value, text.as_mut()) {
                    budget.charge(t.len())?;
                    s.push_str(&t);
                }
            }
            Event::End(name) => match Event::local(&name) {
                "v" | "t" => in_value = false,
                "rPh" => phonetic = false,
                "c" => {
                    let (Some(r), Some((column, kind)), Some(raw)) =
                        (row, cell.take(), text.take())
                    else {
                        continue;
                    };
                    let v = match kind.as_str() {
                        "s" => {
                            let shared = raw
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| strings.get(i))
                                .map_or("", String::as_str);
                            budget.charge(shared.len())?;
                            shared.to_owned()
                        }
                        _ => raw,
                    };
                    let v = vec![v.replace("\r\n", "\r").replace('\n', "\r")];
                    budget.value(&v)?;
                    let cells = &mut rows[r];
                    if cells.len() <= column {
                        budget.empty_values(column + 1 - cells.len())?;
                        cells.resize_with(column + 1, Vec::new);
                    }
                    cells[column] = v;
                }
                "row" => row = None,
                _ => {}
            },
        }
    }
    Ok(rows)
}

/// The shared strings of a workbook (`si`: its runs' text, without
/// phonetic guides).
fn shared_strings(doc: &str, budget: &mut Budget) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut in_t = false;
    let mut phonetic = false;
    for e in events(doc) {
        match e.map_err(|_| bad_workbook())? {
            Event::Start { name, empty, .. } => match Event::local(&name) {
                "si" => current = Some(String::new()),
                "rPh" if !empty => phonetic = true,
                "t" if !empty && !phonetic => in_t = true,
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Some(c) = current.as_mut() {
                    budget.charge(t.len())?;
                    c.push_str(&t);
                }
            }
            Event::End(name) => match Event::local(&name) {
                "t" => in_t = false,
                "rPh" => phonetic = false,
                "si" => {
                    if let Some(s) = current.take() {
                        budget.charge(size_of::<String>())?;
                        out.push(s);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(out)
}

/// The 0-based row of a row reference (`7` is 6), within Excel's rows.
fn row_index(r: &str) -> Option<usize> {
    let n: usize = r.trim().parse().ok()?;
    (1..=limit::SHEET_ROWS).contains(&n).then(|| n - 1)
}

/// The 0-based column of a cell reference (`C7` is 2), within Excel's
/// columns (`A` to `XFD`).
fn column_index(r: &str) -> Option<usize> {
    let mut n = 0usize;
    for c in r.chars().take_while(char::is_ascii_alphabetic) {
        n = n * 26 + usize::from(c.to_ascii_uppercase() as u8 - b'A' + 1);
        if n > limit::COLUMNS {
            return None;
        }
    }
    n.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(rows: &[SourceRow]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|v| v.join("|")).collect())
            .collect()
    }

    #[test]
    fn tab_records_end_at_any_line_break() {
        let rows = delimited("a\tb\x1dc\rd\x0be\r\n\rx\ny", '\t').unwrap();
        assert_eq!(
            flat(&rows),
            vec![
                vec!["a".to_owned(), "b|c".into()],
                vec!["d\re".into()],
                vec!["".into()],
                vec!["x".into()],
                vec!["y".into()],
            ]
        );
        assert!(delimited("", '\t').unwrap().is_empty());
        assert_eq!(
            flat(&delimited("\"a\tb\"\r", '\t').unwrap()),
            vec![vec!["\"a", "b\""]]
        );
    }

    #[test]
    fn comma_quotes_toggle() {
        // oracle x02, i_csv
        let src = "\"1\",\"a,b\",\"He said \"\"hi\"\"\"\r2,plain, spaced \r\"3\",\"multi\rline\",\"cr\nlf\"\r\"4\",\"crlf\r\ninside\",\"  \"\"x\"\" \"\r \"7\" ,\"q\"x\"y\",\"\",,\r";
        assert_eq!(
            flat(&comma(src, ',').unwrap()),
            vec![
                vec!["1", "a,b", "He said \"hi\""],
                vec!["2", "plain", " spaced "],
                vec!["3", "multi\rline", "cr\rlf"],
                vec!["4", "crlf\rinside", "  \"x\" "],
                vec![" 7 ", "qxy", "", "", ""],
            ]
        );
        let lf = comma("\"crlf\n\ninside\"\n", ',').unwrap();
        assert_eq!(flat(&lf), vec![vec!["crlf\r\rinside"]]);
    }

    #[test]
    fn columns() {
        assert_eq!(column_index("A1"), Some(0));
        assert_eq!(column_index("AB12"), Some(27));
        assert_eq!(column_index("xfd7"), Some(16_383));
        assert_eq!(column_index("XFE1"), None);
        assert_eq!(column_index("ZZZZZZZZZZZZZZZZZZZZ1"), None);
        assert_eq!(column_index("12"), None);
        assert_eq!(row_index("1"), Some(0));
        assert_eq!(row_index("1048576"), Some(1_048_575));
        for bad in ["0", "1048577", "-1", "", "x", "99999999999999999999999"] {
            assert_eq!(row_index(bad), None, "{bad}");
        }
    }

    /// A workbook of these parts, deflated.
    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn the_budget_bounds_every_reader() {
        let small = |code| Budget::with(code, 4096);
        // Empty fields cost more than the byte that makes each.
        let tabs = format!("{}\r", "\t".repeat(100)).repeat(10);
        assert!(delimited_in(&tabs, '\t', &mut Budget::new(708)).is_ok());
        let err = delimited_in(&tabs, '\t', &mut small(708)).unwrap_err();
        assert_eq!(err.code, 708);
        assert_eq!(
            comma_in(&tabs.replace('\t', ","), ',', &mut small(708))
                .unwrap_err()
                .code,
            708
        );
        // One long field stops where the budget does.
        let long = "x".repeat(10_000);
        assert!(delimited_in(&long, '\t', &mut small(708)).is_err());
        assert!(comma_in(&format!("\"{long}\""), ',', &mut small(708)).is_err());
        // So do blank lines and repetitions.
        assert!(delimited_in(&"\r".repeat(1000), '\t', &mut small(708)).is_err());
        assert!(delimited_in(&"\x1d".repeat(1000), '\t', &mut small(708)).is_err());
        let doc = format!(
            "<FMPXMLRESULT><RESULTSET>{}</RESULTSET></FMPXMLRESULT>",
            "<ROW><COL/><COL><DATA/></COL></ROW>".repeat(100)
        );
        let err = fmpxmlresult_in(doc.as_bytes(), ImportCharset::Utf8, &mut small(718));
        assert_eq!(err.unwrap_err().code, 718);
        let mut dbf = vec![0x03, 26, 10, 29];
        dbf.extend(1000u32.to_le_bytes());
        dbf.extend(65u16.to_le_bytes());
        dbf.extend(2u16.to_le_bytes());
        dbf.resize(32, 0);
        let mut field = [0u8; 32];
        field[0] = b'A';
        field[11] = b'C';
        field[16] = 1;
        dbf.extend(field);
        dbf.push(0x0d);
        dbf.extend(b" x".repeat(1000));
        assert_eq!(
            dbase_in(&dbf, ImportCharset::Windows, &mut Budget::new(708))
                .unwrap()
                .rows
                .len(),
            1000
        );
        assert_eq!(
            dbase_in(&dbf, ImportCharset::Windows, &mut small(708))
                .unwrap_err()
                .code,
            708
        );
    }

    #[test]
    fn shared_strings_count_each_time_a_cell_uses_one() {
        // One long shared string used by many cells: what the cells take,
        // not what the package holds, is charged.
        let long = "x".repeat(10_000);
        let cells = "<c r=\"A1\" t=\"s\"><v>0</v></c>";
        let rows: String = (1..=200)
            .map(|r| format!("<row r=\"{r}\">{cells}</row>"))
            .collect();
        let bytes = package(&[
            (
                "xl/sharedStrings.xml",
                &format!("<sst><si><t>{long}</t></si></sst>"),
            ),
            (
                "xl/worksheets/sheet1.xml",
                &format!("<worksheet><sheetData>{rows}</sheetData></worksheet>"),
            ),
        ]);
        let rows = workbook_in(&bytes, 0, "", &mut Budget::new(730)).unwrap();
        assert_eq!(rows.len(), 200);
        let err = workbook_in(&bytes, 0, "", &mut Budget::with(730, 1 << 20)).unwrap_err();
        assert_eq!(err.code, 730);
        // So does the gap before a far cell.
        let far = package(&[(
            "xl/worksheets/sheet1.xml",
            &format!(
                "<worksheet><sheetData>{}</sheetData></worksheet>",
                (1..=100)
                    .map(|r| format!("<row r=\"{r}\"><c r=\"XFD{r}\"><v>1</v></c></row>"))
                    .collect::<String>()
            ),
        )]);
        assert!(workbook_in(&far, 0, "", &mut Budget::new(730)).is_ok());
        assert!(workbook_in(&far, 0, "", &mut Budget::with(730, 1 << 20)).is_err());
    }

    #[test]
    fn rich_text_runs_add_up_without_phonetic_guides() {
        let bytes = package(&[
            (
                "xl/sharedStrings.xml",
                "<sst><si><r><t>ab</t></r><r><t>c</t></r><rPh sb=\"0\" eb=\"1\"><t>guide</t></rPh></si></sst>",
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c>\
                 <c r=\"B1\" t=\"inlineStr\"><is><r><t>d</t></r><r><t>e</t></r><rPh><t>g</t></rPh></is></c>\
                 <c r=\"C1\"><f>1+1</f><v>2</v> </c></row></sheetData></worksheet>",
            ),
        ]);
        let rows = workbook(&bytes, 0, "").unwrap();
        assert_eq!(flat(&rows), vec![vec!["abc", "de", "2"]]);
    }

    #[test]
    fn workbook_end_records() {
        let good = package(&[("xl/worksheets/sheet1.xml", "<worksheet/>")]);
        assert!(end_record_fits(&good));
        // Data after the end record, a record too many and a moved
        // directory are not a workbook's.
        let mut tail = good.clone();
        tail.push(0);
        assert!(!end_record_fits(&tail));
        let mut many = good.clone();
        let at = many.len() - 22;
        many[at + 8..at + 12].copy_from_slice(&[0x11, 0x27, 0x11, 0x27]);
        assert!(!end_record_fits(&many));
        let mut moved = good.clone();
        moved[at + 16] ^= 1;
        assert!(!end_record_fits(&moved));
        let mut signatures = [0x50, 0x4b, 0x05, 0x06].repeat(END_RECORDS);
        signatures.extend(&good);
        assert!(!end_record_fits(&signatures));
    }
}
