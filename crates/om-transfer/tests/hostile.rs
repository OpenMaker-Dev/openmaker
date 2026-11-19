//! Hostile import sources (the review's F3 and F4, and their kin): each is
//! refused with its format's import error (708 text and dBase, 718
//! FMPXMLRESULT, 730 Excel), quickly and without a panic or an allocation
//! the source does not pay for (script-steps.md, "Implementation notes
//! (import/export)", Import limits).

use std::io::Write;
use std::time::{Duration, Instant};

use om_transfer::import::{self, read, ImportRequest, Source};
use om_transfer::spec::{ImportAction, ImportOptions, Key};
use om_transfer::{FileType, ImportCharset, Profile};

fn request(file_type: FileType) -> ImportRequest {
    ImportRequest {
        options: ImportOptions {
            table: Key::from_id(1),
            action: ImportAction::Add,
            flags: 0,
            charset: ImportCharset::Utf8,
            map: Vec::new(),
        },
        profile: Profile::new(file_type),
        profile_ex: None,
        field_flags: None,
    }
}

fn parse(file_type: FileType, bytes: &[u8]) -> Result<Source, i32> {
    import::parse(&request(file_type), bytes).map_err(|e| e.code)
}

/// A zip package of these parts, deflated.
fn package(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// A workbook of just this first sheet.
fn workbook(sheet: &str) -> Vec<u8> {
    package(&[("xl/worksheets/sheet1.xml", sheet.as_bytes())])
}

fn sheet_rows(sheet: &str) -> Result<Vec<Vec<Vec<String>>>, i32> {
    read::workbook(&workbook(sheet), 0, "").map_err(|e| e.code)
}

/// Run `f`, which must finish well within `limit`.
fn quickly<T>(limit: Duration, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    assert!(start.elapsed() < limit, "took {:?}", start.elapsed());
    out
}

// F3: worksheet row and column references past Excel's limits.

#[test]
fn rows_past_excels_last_are_730() {
    for r in ["999999999", "1048577", "18446744073709551616", "0", "-1"] {
        let sheet = format!(
            "<worksheet><sheetData><row r=\"{r}\"><c r=\"A1\"><v>1</v></c></row></sheetData></worksheet>"
        );
        assert_eq!(sheet_rows(&sheet), Err(730), "row {r}");
    }
}

#[test]
fn columns_past_excels_last_are_730() {
    for c in ["ZZZZZZZZZZZZZZZZ1", "XFE1", "AAAA1"] {
        let sheet = format!(
            "<worksheet><sheetData><row r=\"1\"><c r=\"{c}\"><v>1</v></c></row></sheetData></worksheet>"
        );
        assert_eq!(sheet_rows(&sheet), Err(730), "cell {c}");
    }
}

#[test]
fn excels_last_column_and_row_are_read() {
    let rows = sheet_rows(
        "<worksheet><sheetData><row r=\"1\"><c r=\"XFD1\"><v>last</v></c></row>\
         <row r=\"1048576\"><c r=\"B1048576\"><v>bottom</v></c></row></sheetData></worksheet>",
    )
    .unwrap();
    assert_eq!(rows.len(), 1_048_576);
    assert_eq!(rows[0].len(), 16_384);
    assert_eq!(rows[0][16_383], vec!["last".to_owned()]);
    assert_eq!(rows[1_048_575][1], vec!["bottom".to_owned()]);
}

#[test]
fn cells_without_references_follow_each_other() {
    let rows = sheet_rows(
        "<worksheet><sheetData><row><c><v>a</v></c><c><v>b</v></c><c r=\"E1\"><v>e</v></c>\
         <c><v>f</v></c></row><row><c><v>2</v></c></row></sheetData></worksheet>",
    )
    .unwrap();
    let text: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(|v| v.join("|")).collect())
        .collect();
    assert_eq!(text, vec![vec!["a", "b", "", "", "e", "f"], vec!["2"]]);
}

// F4: worksheet structure.

#[test]
fn a_cell_outside_a_row_is_730() {
    assert_eq!(
        sheet_rows("<worksheet><c r=\"A1\"><v>1</v></c></worksheet>"),
        Err(730)
    );
}

// Zip limits.

#[test]
fn a_zip_bomb_is_730() {
    // 64 MiB of spaces deflate to about 64 KiB: past the compression ratio.
    let mut sheet = b"<worksheet><sheetData><row r=\"1\"><c r=\"A1\"><v>".to_vec();
    sheet.resize(sheet.len() + (64 << 20), b' ');
    sheet.extend(b"</v></c></row></sheetData></worksheet>");
    let bytes = package(&[("xl/worksheets/sheet1.xml", &sheet)]);
    assert!(bytes.len() < 1 << 20);
    let got = quickly(Duration::from_secs(10), || read::workbook(&bytes, 0, ""));
    assert_eq!(got.map_err(|e| e.code), Err(730));
}

#[test]
fn a_package_of_too_many_parts_is_730() {
    let names: Vec<String> = (0..10_001).map(|i| format!("p{i}")).collect();
    let mut parts: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    let sheet = b"<worksheet><sheetData/></worksheet>";
    parts.push(("xl/worksheets/sheet1.xml", sheet));
    assert_eq!(
        read::workbook(&package(&parts), 0, "").map_err(|e| e.code),
        Err(730)
    );
}

#[test]
fn many_end_records_do_not_stall_the_reader() {
    // Every 22 bytes an end-of-central-directory record pointing at offset
    // 0, where no central directory is.
    let mut eocd = vec![0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0, 46, 0, 0, 0];
    eocd.extend([0, 0, 0, 0, 0, 0]);
    let bytes: Vec<u8> = eocd.iter().copied().cycle().take(22 * 100_000).collect();
    let got = quickly(Duration::from_secs(10), || read::workbook(&bytes, 0, ""));
    assert_eq!(got.map_err(|e| e.code), Err(730));
}

#[test]
fn a_truncated_or_foreign_package_is_730() {
    let good = workbook("<worksheet><sheetData/></worksheet>");
    for bytes in [&good[..good.len() / 2], b"PK", b"", b"not a zip at all"] {
        assert_eq!(parse(FileType::Excel2007, bytes).map(|_| ()), Err(730));
    }
}

// XML limits.

#[test]
fn xml_nested_too_deep_is_refused() {
    let deep = |n: usize| format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
    let doc = format!("<FMPXMLRESULT>{}</FMPXMLRESULT>", deep(300));
    assert_eq!(
        parse(FileType::XmlImport, doc.as_bytes()).map(|_| ()),
        Err(718)
    );
    let ok = format!("<FMPXMLRESULT>{}</FMPXMLRESULT>", deep(100));
    assert!(parse(FileType::XmlImport, ok.as_bytes()).is_ok());
    let sheet = format!("<worksheet>{}</worksheet>", deep(300));
    assert_eq!(sheet_rows(&sheet), Err(730));
}

#[test]
fn xml_entities_are_never_expanded() {
    // A document type's entities are not read: using one is an error.
    let doc = "<!DOCTYPE r [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;\">]>\
               <FMPXMLRESULT><RESULTSET><ROW><COL><DATA>&b;</DATA></COL></ROW></RESULTSET></FMPXMLRESULT>";
    assert_eq!(
        parse(FileType::XmlImport, doc.as_bytes()).map(|_| ()),
        Err(718)
    );
    // Character references outside Unicode, and overlong ones.
    for data in ["&#x110000;", "&#99999999999999999999;", "&#xD800;", "&#;"] {
        let doc = format!("<FMPXMLRESULT><RESULTSET><ROW><COL><DATA>{data}</DATA></COL></ROW></RESULTSET></FMPXMLRESULT>");
        assert_eq!(
            parse(FileType::XmlImport, doc.as_bytes()).map(|_| ()),
            Err(718),
            "{data}"
        );
    }
}

#[test]
fn fmpxmlresult_with_too_many_columns_is_718() {
    let cols = "<COL><DATA>x</DATA></COL>".repeat(16_385);
    let doc = format!("<FMPXMLRESULT><RESULTSET><ROW>{cols}</ROW></RESULTSET></FMPXMLRESULT>");
    assert_eq!(
        parse(FileType::XmlImport, doc.as_bytes()).map(|_| ()),
        Err(718)
    );
    let cols = "<COL><DATA>x</DATA></COL>".repeat(16_384);
    let doc = format!("<FMPXMLRESULT><RESULTSET><ROW>{cols}</ROW></RESULTSET></FMPXMLRESULT>");
    assert_eq!(
        parse(FileType::XmlImport, doc.as_bytes()).unwrap().rows[0].len(),
        16_384
    );
}

// dBase header and record validation (F4).

/// A dBase III header: record count, header and record lengths, and field
/// descriptors (name, type, length).
fn dbase(count: u32, header: u16, record: u16, fields: &[(&str, u8, u8)]) -> Vec<u8> {
    let mut out = vec![0x03, 26, 10, 29];
    out.extend(count.to_le_bytes());
    out.extend(header.to_le_bytes());
    out.extend(record.to_le_bytes());
    out.resize(32, 0);
    for (name, kind, len) in fields {
        let mut d = [0u8; 32];
        d[..name.len()].copy_from_slice(name.as_bytes());
        d[11] = *kind;
        d[16] = *len;
        out.extend(d);
    }
    out.push(0x0d);
    out
}

fn dbase_code(bytes: &[u8]) -> Result<Source, i32> {
    read::dbase(bytes, ImportCharset::Windows).map_err(|e| e.code)
}

#[test]
fn a_dbase_record_length_of_zero_is_708() {
    // The review's probe: a bare 32-byte header, one record of length 0.
    let mut bare = vec![0u8; 32];
    bare[4] = 1;
    bare[8] = 32;
    assert_eq!(dbase_code(&bare).map(|_| ()), Err(708));
    let mut bytes = dbase(1, 65, 0, &[("A", b'C', 3)]);
    bytes.extend(b" abc");
    assert_eq!(dbase_code(&bytes).map(|_| ()), Err(708));
}

#[test]
fn dbase_headers_must_fit_their_file_and_fields_their_record() {
    // Header length past the end of the file.
    let bytes = dbase(0, 4000, 4, &[("A", b'C', 3)]);
    assert_eq!(dbase_code(&bytes).map(|_| ()), Err(708));
    // Header length shorter than the descriptors read.
    let mut bytes = dbase(1, 40, 4, &[("A", b'C', 3)]);
    bytes.extend(b" abc");
    assert_eq!(dbase_code(&bytes).map(|_| ()), Err(708));
    // Fields wider than the record.
    let mut bytes = dbase(1, 65, 4, &[("A", b'C', 200)]);
    bytes.extend(b" abc");
    assert_eq!(dbase_code(&bytes).map(|_| ()), Err(708));
    // A field of no width.
    let mut bytes = dbase(1, 97, 4, &[("A", b'C', 3), ("B", b'C', 0)]);
    bytes.extend(b" abc");
    assert_eq!(dbase_code(&bytes).map(|_| ()), Err(708));
    // A good one, for contrast.
    let mut bytes = dbase(1, 65, 4, &[("A", b'C', 3)]);
    bytes.extend(b" abc\x1a");
    let source = dbase_code(&bytes).unwrap();
    assert_eq!(source.rows, vec![vec![vec!["abc".to_owned()]]]);
}

#[test]
fn a_dbase_record_count_past_the_data_reads_what_is_there() {
    let mut bytes = dbase(u32::MAX, 65, 4, &[("A", b'C', 3)]);
    bytes.extend(b" abc xyz");
    let source = quickly(Duration::from_secs(2), || dbase_code(&bytes)).unwrap();
    assert_eq!(source.rows.len(), 2);
}

#[test]
fn a_dbase_date_of_other_characters_does_not_panic() {
    // Dates whose text is 8 bytes of UTF-8 but not 8 characters: "abc€de"
    // and "202€01" (€ is 0x80 in Windows-1252), "éééé".
    for date in [&b"abc\x80de  "[..], b"202\x8001  ", b"\xe9\xe9\xe9\xe9    "] {
        let mut bytes = dbase(1, 65, 9, &[("D", b'D', 8)]);
        bytes.push(b' ');
        bytes.extend(date);
        assert!(dbase_code(&bytes).is_ok());
    }
}

// Text sources.

#[test]
fn a_text_line_with_too_many_fields_is_708() {
    for file_type in [
        FileType::TabSeparated,
        FileType::CommaSeparated,
        FileType::Merge,
    ] {
        let delimiter = if file_type == FileType::TabSeparated {
            "\t"
        } else {
            ","
        };
        let wide = format!("a{}\r", delimiter.repeat(16_384));
        assert_eq!(
            parse(file_type, wide.as_bytes()).map(|_| ()),
            Err(708),
            "{file_type:?}"
        );
        let widest = format!("a{}\r", delimiter.repeat(16_383));
        assert!(parse(file_type, widest.as_bytes()).is_ok(), "{file_type:?}");
    }
}

#[test]
fn a_source_past_the_size_limit_is_refused() {
    let big = vec![b'a'; import::limit::SOURCE_BYTES + 1];
    for (file_type, code) in [
        (FileType::TabSeparated, 708),
        (FileType::CommaSeparated, 708),
        (FileType::Merge, 708),
        (FileType::DBase, 708),
        (FileType::XmlImport, 718),
        (FileType::Excel2007, 730),
    ] {
        let got = quickly(Duration::from_secs(5), || parse(file_type, &big));
        assert_eq!(got.map(|_| ()), Err(code), "{file_type:?}");
    }
}

#[test]
fn import_reports_the_readers_error() {
    let mut target = Refusing;
    let got = import::import(
        &mut target,
        &request(FileType::Excel2007),
        &workbook("<worksheet><c r=\"A1\"><v>1</v></c></worksheet>"),
        &om_calc::Locale::en_us(),
    );
    assert_eq!(got.map_err(|e| e.code), Err(730));
}

/// A target no row reaches.
struct Refusing;

impl import::ImportTarget for Refusing {
    fn fields(&mut self, _: u64) -> om_transfer::Result<Vec<import::TargetField>> {
        Ok(Vec::new())
    }
    fn found_set(&mut self, _: u64) -> om_transfer::Result<Vec<u64>> {
        Ok(Vec::new())
    }
    fn value(&mut self, _: u64, _: u64, _: u64) -> om_transfer::Result<String> {
        unreachable!()
    }
    fn create(&mut self, _: u64, _: &[import::FieldWrite], _: bool) -> om_transfer::Result<u64> {
        unreachable!()
    }
    fn update(
        &mut self,
        _: u64,
        _: u64,
        _: &[import::FieldWrite],
        _: bool,
    ) -> om_transfer::Result<()> {
        unreachable!()
    }
    fn set_found_set(&mut self, _: u64, _: &[u64]) -> om_transfer::Result<()> {
        Ok(())
    }
}
