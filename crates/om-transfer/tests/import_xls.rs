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

// ---------------------------------------------- synthetic workbooks, by hand

/// A BIFF8 record.
fn rec(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_le_bytes().to_vec();
    out.extend((data.len() as u16).to_le_bytes());
    out.extend(data);
    out
}

fn bof(kind: u16) -> Vec<u8> {
    let mut data = 0x0600u16.to_le_bytes().to_vec();
    data.extend(kind.to_le_bytes());
    data.extend([0; 12]);
    rec(0x0809, &data)
}

fn eof() -> Vec<u8> {
    rec(0x000a, &[])
}

fn number(row: u16, column: u16, value: f64) -> Vec<u8> {
    let mut data = row.to_le_bytes().to_vec();
    data.extend(column.to_le_bytes());
    data.extend([0, 0]);
    data.extend(value.to_le_bytes());
    rec(0x0203, &data)
}

/// A `Workbook` stream: `globals` after the BOF, then one worksheet named
/// "Sheet" holding `cells`, padded past the mini stream's cutoff.
fn biff(globals: &[u8], cells: &[u8]) -> Vec<u8> {
    let sheet = |at: u32| {
        let mut data = at.to_le_bytes().to_vec();
        data.extend([0, 0, 5, 0]);
        data.extend(b"Sheet");
        rec(0x0085, &data)
    };
    let mut out = bof(0x0005);
    out.extend(globals);
    let at = (out.len() + sheet(0).len() + eof().len()) as u32;
    out.extend(sheet(at));
    out.extend(eof());
    out.extend(bof(0x0010));
    out.extend(cells);
    out.extend(eof());
    out.resize(out.len().max(4096).next_multiple_of(512), 0);
    out
}

/// A directory entry: its name, type (1 storage, 2 stream, 5 root), left,
/// right and child ids, and a stream's data.
struct Entry<'a>(&'a str, u8, u32, u32, u32, Option<&'a [u8]>);

const NONE: u32 = 0xffff_ffff;

/// A version 3 compound file of `entries` (the root first): sector 0 the
/// FAT, then the directory, then each stream's sectors in order. Returns
/// the file and each stream's last sector.
fn compound(entries: &[Entry<'_>]) -> (Vec<u8>, Vec<u32>) {
    let directory_sectors = entries.len().div_ceil(4) as u32;
    let mut fat = vec![0xffff_fffdu32];
    for s in 1..=directory_sectors {
        fat.push(if s == directory_sectors {
            0xffff_fffe
        } else {
            s + 1
        });
    }
    let mut body = Vec::new();
    let mut starts = Vec::new();
    let mut lasts = Vec::new();
    for entry in entries {
        let Some(data) = entry.5 else {
            starts.push(0xffff_fffe);
            continue;
        };
        let first = fat.len() as u32;
        let count = data.len().div_ceil(512) as u32;
        for s in first..first + count {
            fat.push(if s == first + count - 1 {
                0xffff_fffe
            } else {
                s + 1
            });
        }
        starts.push(first);
        lasts.push(first + count - 1);
        let mut data = data.to_vec();
        data.resize(count as usize * 512, 0);
        body.extend(data);
    }
    assert!(fat.len() <= 128);
    fat.resize(128, 0xffff_ffff);
    let mut file = vec![0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
    file.resize(0x18, 0);
    for (at, value) in [
        (0x18, 0x003eu16),
        (0x1a, 3),
        (0x1c, 0xfffe),
        (0x1e, 9),
        (0x20, 6),
    ] {
        file.resize(at, 0);
        file.extend(value.to_le_bytes());
    }
    file.resize(0x2c, 0);
    for value in [1u32, 1, 0, 4096, 0xffff_fffe, 0, 0xffff_fffe, 0, 0] {
        file.extend(value.to_le_bytes());
    }
    for _ in 1..109 {
        file.extend(0xffff_ffffu32.to_le_bytes());
    }
    assert_eq!(file.len(), 512);
    file.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
    let mut directory = Vec::new();
    for (entry, start) in entries.iter().zip(starts) {
        let mut raw = [0u8; 128];
        let name: Vec<u16> = entry.0.encode_utf16().collect();
        for (i, unit) in name.iter().enumerate() {
            raw[2 * i..2 * i + 2].copy_from_slice(&unit.to_le_bytes());
        }
        raw[64..66].copy_from_slice(&(2 * (name.len() as u16 + 1)).to_le_bytes());
        raw[66] = entry.1;
        raw[67] = 1;
        raw[68..72].copy_from_slice(&entry.2.to_le_bytes());
        raw[72..76].copy_from_slice(&entry.3.to_le_bytes());
        raw[76..80].copy_from_slice(&entry.4.to_le_bytes());
        raw[116..120].copy_from_slice(&start.to_le_bytes());
        let size = entry.5.map_or(0, |d| d.len() as u64);
        raw[120..128].copy_from_slice(&size.to_le_bytes());
        directory.extend(raw);
    }
    directory.resize(directory_sectors as usize * 512, 0);
    file.extend(directory);
    file.extend(body);
    (file, lasts)
}

/// A file of one workbook stream.
fn workbook(stream: &[u8]) -> (Vec<u8>, Vec<u32>) {
    compound(&[
        Entry("Root Entry", 5, NONE, NONE, 1, None),
        Entry("Workbook", 2, NONE, NONE, NONE, Some(stream)),
    ])
}

/// What the hand-made workbooks hold that the reader must not take for the
/// sheet's cells, or must refuse ([MS-XLS], [MS-CFB]).
#[test]
fn embedded_substreams_storages_and_loops_are_not_the_sheet() {
    let first = |bytes: &[u8]| rows(bytes, 0, "");
    // An embedded chart's cached point is not cell A1, and its EOF does not
    // end the sheet.
    let chart = [bof(0x0020), number(0, 0, 42.0), eof()].concat();
    let cells = [number(0, 0, 10.0), chart, number(1, 0, 20.0)].concat();
    assert_eq!(first(&workbook(&biff(&[], &cells)).0), ["10", "20"]);
    // A dialog sheet, stored as a worksheet with WsBool's fDialog.
    let dialog = [rec(0x0081, &[0x10, 0x00]), number(0, 0, 1.0)].concat();
    let refused = import::parse(&request(0, ""), &workbook(&biff(&[], &dialog)).0);
    assert_eq!(refused.unwrap_err().code, 730);
    // An embedded object's own Workbook, listed before the root's.
    let (root, embedded) = (biff(&[], &number(0, 0, 1.0)), biff(&[], &number(0, 0, 2.0)));
    let (nested, _) = compound(&[
        Entry("Root Entry", 5, NONE, NONE, 2, None),
        Entry("Workbook", 2, NONE, NONE, NONE, Some(&embedded)),
        Entry("MBD0000ABCD", 1, NONE, 3, 1, None),
        Entry("Workbook", 2, NONE, NONE, NONE, Some(&root)),
    ]);
    assert_eq!(first(&nested), ["1"]);
    // A long run of empty CONTINUE records after an empty SST.
    let sst = [
        rec(0x00fc, &[0; 8]),
        std::iter::repeat_n(rec(0x003c, &[]), 1_000)
            .flatten()
            .collect(),
    ]
    .concat();
    assert_eq!(first(&workbook(&biff(&sst, &number(0, 0, 3.0))).0), ["3"]);
    // A stream whose last sector links back to itself.
    let (mut file, lasts) = workbook(&biff(&[], &number(0, 0, 1.0)));
    let at = 512 + 4 * lasts[0] as usize;
    file[at..at + 4].copy_from_slice(&lasts[0].to_le_bytes());
    assert_eq!(import::parse(&request(0, ""), &file).unwrap_err().code, 730);
}
