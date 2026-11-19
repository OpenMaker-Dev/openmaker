//! Excel workbooks (`XLXE`), as FileMaker Server writes them (oracle x01,
//! x03): one worksheet; numbers, dates and timestamps from 1900 on and
//! times that are not negative as numbers (day serials and fractions),
//! everything else as shared strings (deduplicated, in order of first
//! use); empty values have no cell. Each column has a cell style, and text,
//! date, time and timestamp columns their number format, numbered 168 plus
//! the column's index. Export Records writes no header row.
//!
//! The package parts are FileMaker's byte for byte except the creation
//! time; the zip container itself (compression, entry times) is not.

use std::io::Write as _;

use super::{Cell, ExportRequest, Table};
use crate::text::Typed;
use crate::xml::escape_text;
use crate::{code, FieldType, Result, TransferError};

const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

const CONTENT_TYPES: &str = "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default ContentType=\"application/vnd.openxmlformats-package.relationships+xml\" Extension=\"rels\"/><Default ContentType=\"application/xml\" Extension=\"xml\"/><Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\" PartName=\"/xl/workbook.xml\"/><Override ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\" PartName=\"/docProps/app.xml\"/><Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\" PartName=\"/xl/worksheets/sheet1.xml\"/><Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\" PartName=\"/xl/sharedStrings.xml\"/><Override ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\" PartName=\"/docProps/core.xml\"/><Override ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\" PartName=\"/xl/styles.xml\"/></Types>";

const RELS: &str = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>";

const WORKBOOK_RELS: &str = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";

const STYLE_FIXED: &str = "<fonts count=\"2\"><font><sz val=\"10\"/><color theme=\"1\"/><name val=\"Verdana\"/><family val=\"2\"/><scheme val=\"minor\"/></font><font><b/><sz val=\"10\"/><color theme=\"1\"/><name val=\"Verdana\"/><family val=\"2\"/><scheme val=\"minor\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>";

const XFS_FIXED: &str = "<xf borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"0\" xfId=\"0\"/><xf applyAlignment=\"1\" applyFont=\"1\" applyNumberFormat=\"1\" borderId=\"0\" fillId=\"0\" fontId=\"1\" numFmtId=\"0\" xfId=\"0\"><alignment horizontal=\"center\" vertical=\"top\"/></xf><xf applyAlignment=\"1\" applyFont=\"1\" applyNumberFormat=\"1\" borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"165\" xfId=\"0\"><alignment horizontal=\"left\" vertical=\"top\"/></xf><xf borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"0\" xfId=\"0\"/><xf applyAlignment=\"1\" applyFont=\"1\" applyNumberFormat=\"1\" borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"166\" xfId=\"0\"><alignment horizontal=\"right\" vertical=\"top\"/></xf><xf applyAlignment=\"1\" applyFont=\"1\" applyNumberFormat=\"1\" borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"167\" xfId=\"0\"><alignment horizontal=\"right\" vertical=\"top\"/></xf>";

/// The cell style of the header row (bold, centred).
const HEADER_STYLE: usize = 1;
/// The first column's cell style.
const FIRST_STYLE: usize = 6;
/// The first column's number format id.
const FIRST_FORMAT: usize = 168;

/// A column letter: A, B, ..., Z, AA, ...
pub(crate) fn column_name(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn number_format(ty: FieldType) -> Option<&'static str> {
    match ty {
        FieldType::Text | FieldType::Container => Some("@"),
        FieldType::Date => Some("mm/dd/yyyy"),
        FieldType::Time => Some("[hh]:mm:ss"),
        FieldType::Timestamp => Some("mm/dd/yyyy hh:mm:ss"),
        FieldType::Number => None,
    }
}

pub(crate) fn styles(types: &[FieldType]) -> String {
    let mut s = String::from(DECLARATION);
    s.push_str("<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
    s.push_str(&format!(
        "<numFmts count=\"{}\"><numFmt formatCode=\"@\" numFmtId=\"165\"/><numFmt formatCode=\"mm/dd/yyyy\" numFmtId=\"166\"/><numFmt formatCode=\"[hh]:mm:ss\" numFmtId=\"167\"/>",
        4 + types.len()
    ));
    for (i, ty) in types.iter().enumerate() {
        if let Some(f) = number_format(*ty) {
            s.push_str(&format!(
                "<numFmt formatCode=\"{f}\" numFmtId=\"{}\"/>",
                FIRST_FORMAT + i
            ));
        }
    }
    s.push_str("</numFmts>");
    s.push_str(STYLE_FIXED);
    s.push_str(&format!(
        "<cellXfs count=\"{}\">",
        FIRST_STYLE + types.len()
    ));
    s.push_str(XFS_FIXED);
    for (i, ty) in types.iter().enumerate() {
        let id = number_format(*ty).map_or(0, |_| FIRST_FORMAT + i);
        s.push_str(&format!(
            "<xf applyAlignment=\"1\" applyFont=\"1\" applyNumberFormat=\"1\" borderId=\"0\" fillId=\"0\" fontId=\"0\" numFmtId=\"{id}\" xfId=\"0\"><alignment horizontal=\"general\" vertical=\"top\"/></xf>"
        ));
    }
    s.push_str("</cellXfs></styleSheet>");
    s
}

/// Shared strings in order of first use.
#[derive(Default)]
struct Strings {
    list: Vec<String>,
    index: std::collections::HashMap<String, usize>,
}

impl Strings {
    fn add(&mut self, s: String) -> usize {
        if let Some(&i) = self.index.get(&s) {
            return i;
        }
        let i = self.list.len();
        self.index.insert(s.clone(), i);
        self.list.push(s);
        i
    }

    fn xml(&self) -> String {
        let mut s = String::from(DECLARATION);
        s.push_str(&format!(
            "<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{n}\" uniqueCount=\"{n}\">",
            n = self.list.len()
        ));
        for t in &self.list {
            let preserve = t.starts_with(char::is_whitespace) || t.ends_with(char::is_whitespace);
            s.push_str(if preserve {
                "<si><t xml:space=\"preserve\">"
            } else {
                "<si><t>"
            });
            escape_text(t, &mut s);
            s.push_str("</t></si>");
        }
        s.push_str("</sst>");
        s
    }
}

/// The package parts in FileMaker's order: (name, content).
pub(crate) fn parts(table: &Table, request: &ExportRequest) -> Vec<(&'static str, String)> {
    let types: Vec<FieldType> = table.columns.iter().map(|c| c.info.ty).collect();
    let mut strings = Strings::default();
    let mut sheet = String::from(DECLARATION);
    sheet.push_str("<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetFormatPr baseColWidth=\"21\" defaultRowHeight=\"15\"/><sheetData>");
    let mut r = 0;
    if request.field_names_row {
        r += 1;
        sheet.push_str(&format!("<row r=\"{r}\">"));
        for (i, c) in table.columns.iter().enumerate() {
            let idx = strings.add(c.merge_name());
            sheet.push_str(&format!(
                "<c r=\"{}{r}\" t=\"s\" s=\"{HEADER_STYLE}\"><v>{idx}</v></c>",
                column_name(i)
            ));
        }
        sheet.push_str("</row>");
    }
    for (_, cells) in table.expanded() {
        r += 1;
        sheet.push_str(&format!("<row r=\"{r}\">"));
        for (i, (cell, column)) in cells.iter().zip(&table.columns).enumerate() {
            let Some(Cell::Values(values)) = cell else {
                continue;
            };
            let Some(v) = values.first() else {
                continue;
            };
            let style = FIRST_STYLE + i;
            match v.typed(column.info.ty, &table.context.locale) {
                None => {}
                Some(Typed::Number(n)) => sheet.push_str(&format!(
                    "<c r=\"{}{r}\" s=\"{style}\"><v>{n}</v></c>",
                    column_name(i)
                )),
                Some(Typed::Text(t)) => {
                    let idx = strings.add(t);
                    sheet.push_str(&format!(
                        "<c r=\"{}{r}\" t=\"s\" s=\"{style}\"><v>{idx}</v></c>",
                        column_name(i)
                    ));
                }
            }
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");

    let mut worksheet = String::new();
    escape_text(&request.worksheet, &mut worksheet);
    let mut core = String::from(DECLARATION);
    core.push_str("<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:creator>");
    escape_text(&request.author, &mut core);
    core.push_str("</dc:creator><dc:title>");
    escape_text(&request.title, &mut core);
    core.push_str("</dc:title><dc:subject>");
    escape_text(&request.subject, &mut core);
    core.push_str("</dc:subject><dcterms:created xsi:type=\"dcterms:W3CDTF\">");
    core.push_str(&table.context.created);
    core.push_str("</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">");
    core.push_str(&table.context.created);
    core.push_str("</dcterms:modified></cp:coreProperties>");
    let app = format!(
        "{DECLARATION}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"><Application>Microsoft Excel</Application><DocSecurity>0</DocSecurity><ScaleCrop>false</ScaleCrop><HeadingPairs><vt:vector baseType=\"variant\" size=\"2\"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector baseType=\"lpstr\" size=\"1\"><vt:lpstr>{worksheet}</vt:lpstr></vt:vector></TitlesOfParts><LinksUpToDate>false</LinksUpToDate><SharedDoc>false</SharedDoc><HyperlinksChanged>false</HyperlinksChanged><AppVersion>12.0000</AppVersion></Properties>"
    );
    let workbook = format!(
        "{DECLARATION}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><workbookPr date1904=\"0\"/><sheets><sheet name=\"{worksheet}\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
    );
    vec![
        ("xl/worksheets/sheet1.xml", sheet),
        ("_rels/.rels", format!("{DECLARATION}{RELS}")),
        ("docProps/core.xml", core),
        ("docProps/app.xml", app),
        (
            "[Content_Types].xml",
            format!("{DECLARATION}{CONTENT_TYPES}"),
        ),
        ("xl/styles.xml", styles(&types)),
        (
            "xl/_rels/workbook.xml.rels",
            format!("{DECLARATION}{WORKBOOK_RELS}"),
        ),
        ("xl/workbook.xml", workbook),
        ("xl/sharedStrings.xml", strings.xml()),
    ]
}

pub(crate) fn write(table: &Table, request: &ExportRequest) -> Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let failed = |e: &dyn std::fmt::Display| {
        TransferError::new(
            code::CANNOT_CREATE,
            format!("Could not write the workbook: {e}"),
        )
    };
    for (name, content) in parts(table, request) {
        zip.start_file(name, options).map_err(|e| failed(&e))?;
        zip.write_all(content.as_bytes()).map_err(|e| failed(&e))?;
    }
    let cursor = zip.finish().map_err(|e| failed(&e))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }
}
