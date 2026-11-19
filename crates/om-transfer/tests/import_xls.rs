//! Excel 97-2004 workbooks (`XLS `) read as import sources, from files an
//! independent BIFF8 writer made (`tests/data/xls/make.py`, xlwt 1.3.0;
//! the values below are the ones xlrd 2.0.2, another reader, reads).

use std::path::PathBuf;

use om_transfer::import::{self, ImportRequest};
use om_transfer::spec::{ImportAction, ImportOptions, Key, Profile, ProfileBody};
use om_transfer::{FileType, ImportCharset};

fn data(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/xls")
        .join(name);
    std::fs::read(path).unwrap()
}

fn request(sheet: u32, worksheet: &str) -> ImportRequest {
    let mut profile = Profile::new(FileType::Excel2003);
    profile.body = ProfileBody::Excel {
        sheet,
        worksheet: worksheet.into(),
        file: String::new(),
    };
    ImportRequest {
        options: ImportOptions {
            table: Key::none(),
            action: ImportAction::Add,
            flags: 0,
            charset: ImportCharset::Utf8,
            map: Vec::new(),
        },
        profile,
        profile_ex: None,
        field_flags: None,
    }
}

/// Each row's cells, a gap as `-`.
fn rows(bytes: &[u8], sheet: u32, worksheet: &str) -> Vec<String> {
    import::parse(&request(sheet, worksheet), bytes)
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| match cell.as_slice() {
                    [] => "-".to_owned(),
                    values => values.join("|"),
                })
                .collect::<Vec<_>>()
                .join(" ; ")
        })
        .collect()
}

#[test]
fn a_worksheets_cells_read_as_an_xlsx_sheets_do() {
    let bytes = data("cells.xls");
    assert_eq!(
        rows(&bytes, 0, ""),
        [
            "Text ; Number ; Flag ; When",
            "plain ; 7 ; 1 ; 45351",
            "two\rlines ; 0.1 ; 0",
            "naïve – €100 😀 日本 ; -2.5",
            "- ; 123456789012",
            "- ; 1000000000000000000000",
            "",
            "- ; - ; after a gap",
        ]
    );
    // By name before position, as for an .xlsx.
    assert_eq!(rows(&bytes, 0, "Other"), ["second sheet"]);
    assert_eq!(rows(&bytes, 1, ""), ["second sheet"]);
    // A sheet the workbook lacks.
    assert_eq!(
        import::parse(&request(2, ""), &bytes).unwrap_err().code,
        730
    );
}

/// Shared strings past one SST record continue in CONTINUE records, each
/// starting with the flags of the 16-bit characters it continues.
#[test]
fn shared_strings_continue_across_records() {
    let got = rows(&data("shared.xls"), 0, "");
    let want: Vec<String> = (0..1200)
        .map(|i| {
            if i % 5 == 0 {
                format!("string {i:04}")
            } else {
                format!("строка {i:04} ✓")
            }
        })
        .collect();
    assert_eq!(got, want);
}

#[test]
fn what_is_not_a_biff8_workbook_is_refused() {
    let mut bytes = data("cells.xls");
    // Not a compound file.
    assert_eq!(
        import::parse(&request(0, ""), b"not a workbook")
            .unwrap_err()
            .code,
        730
    );
    // A truncated file: its sectors are missing.
    assert_eq!(
        import::parse(&request(0, ""), &bytes[..bytes.len() / 2])
            .unwrap_err()
            .code,
        730
    );
    // A FAT that loops: the first FAT entry of the directory chain points
    // at itself.
    let first_fat = u32::from_le_bytes(bytes[0x4c..0x50].try_into().unwrap()) as usize;
    let directory = u32::from_le_bytes(bytes[0x30..0x34].try_into().unwrap()) as usize;
    let at = (first_fat + 1) * 512 + directory * 4;
    bytes[at..at + 4].copy_from_slice(&(directory as u32).to_le_bytes());
    assert_eq!(
        import::parse(&request(0, ""), &bytes).unwrap_err().code,
        730
    );
}
