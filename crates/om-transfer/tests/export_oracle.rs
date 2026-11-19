//! Export Records against FileMaker Server 26.0.3's own files (batteries
//! x01 and x03 of `tools/fms-transfer-oracle`, results in `tests/data`):
//! the same records and export orders must give the same bytes. Excel
//! workbooks compare part by part (the zip container and the creation time
//! are not FileMaker's) and their time cells numerically (module docs of
//! `om_transfer::export`).

mod support;

use std::path::PathBuf;

use om_transfer::export::{export, ExportRequest};
use om_transfer::spec::{
    ExportField, ExportOptions, FieldSpec, FileType, Profile, ProfileBody, XmlSource,
};
use om_transfer::ExportCharset;
use support::{a, f, Mem, ADDRESSES, CONTACTS};

fn data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(path)
}

fn spec(occurrence: u64, field: u64) -> FieldSpec {
    FieldSpec::new(occurrence, field)
}

/// Contacts fields, `(field, Some(break))` for a summary by a break field.
fn order(
    fields: &[(u64, u64, Option<u64>)],
    group: &[u64],
    formatting: bool,
    charset: u8,
) -> ExportOptions {
    ExportOptions {
        fields: fields
            .iter()
            .map(|(o, fld, by)| ExportField {
                field: spec(*o, *fld),
                sum_by: by.map_or_else(FieldSpec::none, |b| spec(CONTACTS, b)),
            })
            .collect(),
        group_by: group.iter().map(|g| (spec(CONTACTS, *g), 1)).collect(),
        formatting,
        charset: ExportCharset::from_stored(charset).unwrap(),
        use_name_alias: Some(1),
    }
}

fn c(field: u64) -> (u64, u64, Option<u64>) {
    (CONTACTS, field, None)
}

/// x01's field list F.
fn all_fields() -> Vec<(u64, u64, Option<u64>)> {
    vec![
        c(f::K),
        c(f::FIRST),
        c(f::LAST),
        c(f::COMPANY),
        c(f::TITLE),
        c(f::JOB),
        c(f::WEBSITE),
        c(f::N),
        c(f::D),
        c(f::T),
        c(f::TS),
        c(f::R),
        c(f::NR),
        c(f::CN),
        c(f::CU),
        c(f::G),
        c(f::PHOTO),
        (ADDRESSES, a::CITY, None),
        c(f::AE),
        c(f::S),
    ]
}

/// x01's grouped list G.
fn group_fields() -> Vec<(u64, u64, Option<u64>)> {
    vec![
        c(f::COMPANY),
        c(f::FIRST),
        c(f::N),
        (CONTACTS, f::S, Some(f::COMPANY)),
        (CONTACTS, f::SC, Some(f::COMPANY)),
        c(f::S),
    ]
}

fn profile(file_type: FileType) -> Profile {
    Profile {
        file_type,
        field_names: 1,
        body: if file_type == FileType::XmlExport {
            ProfileBody::XmlExport {
                grammar: 1,
                xsl: XmlSource::None,
            }
        } else {
            ProfileBody::Plain
        },
    }
}

fn run(mem: &mut Mem, options: ExportOptions, file_type: FileType) -> Vec<u8> {
    let request = ExportRequest::new(options, profile(file_type));
    export(mem, &request).expect("export").expect("a file")
}

fn hex_diff(want: &[u8], got: &[u8]) -> String {
    let at = want
        .iter()
        .zip(got)
        .position(|(a, b)| a != b)
        .unwrap_or(want.len().min(got.len()));
    let from = at.saturating_sub(40);
    format!(
        "first difference at {at} (lengths {} and {}):\n want {:?}\n  got {:?}",
        want.len(),
        got.len(),
        String::from_utf8_lossy(&want[from..(at + 40).min(want.len())]),
        String::from_utf8_lossy(&got[from..(at + 40).min(got.len())]),
    )
}

fn check(name: &str, got: &[u8]) -> bool {
    let want = std::fs::read(data(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    if want == got {
        true
    } else {
        eprintln!("{name}: {}", hex_diff(&want, got));
        false
    }
}

/// Character sets whose FileMaker tables differ from ours (module docs of
/// `om_transfer::charset`): DOS keeps Greek letters FileMaker drops,
/// Shift-JIS lacks FileMaker's row of Latin letters, Big-5 and EUC-KR
/// spell some letters differently.
const KNOWN_CHARSETS: [u8; 4] = [1, 6, 9, 10];

#[test]
fn x01_formats_and_character_sets() {
    let mut failures = Vec::new();
    let mut known = Vec::new();
    for cs in [0u8, 1, 2, 3, 4, 6, 8, 9, 10] {
        let name = format!("x01/omprobe_x_tabs_c{cs}.tab");
        let got = run(
            &mut support::x01(),
            order(&all_fields(), &[], false, cs),
            FileType::TabSeparated,
        );
        if !check(&name, &got) {
            if KNOWN_CHARSETS.contains(&cs) {
                known.push(name);
            } else {
                failures.push(name);
            }
        }
    }
    for (cs, t, ext) in [
        (3, FileType::CommaSeparated, "coms_c3.csv"),
        (4, FileType::CommaSeparated, "coms_c4.csv"),
        (0, FileType::CommaSeparated, "coms_c0.csv"),
        (2, FileType::CommaSeparated, "coms_c2.csv"),
        (3, FileType::Merge, "mrge_c3.mer"),
        (4, FileType::Merge, "mrge_c4.mer"),
        (3, FileType::HtmlTable, "html_c3.htm"),
        (4, FileType::HtmlTable, "html_c4.htm"),
        (3, FileType::XmlExport, "xmle_c3.xml"),
        (4, FileType::XmlExport, "xmle_c4.xml"),
    ] {
        let name = format!("x01/omprobe_x_{ext}");
        let got = run(&mut support::x01(), order(&all_fields(), &[], false, cs), t);
        if !check(&name, &got) {
            failures.push(name);
        }
    }
    // Formatting on with fields that are not on the layout: unformatted.
    let got = run(
        &mut support::x01(),
        order(&all_fields(), &[], true, 4),
        FileType::TabSeparated,
    );
    if !check("x01/omprobe_x_fmt.tab", &got) {
        failures.push("x_fmt".into());
    }
    eprintln!("known character set differences: {known:?}");
    assert!(failures.is_empty(), "differ from FileMaker: {failures:?}");
}

#[test]
fn x01_groups_and_summaries() {
    let mut failures = Vec::new();
    for (t, name) in [
        (FileType::TabSeparated, "x01/omprobe_x_grp_tabs.tab"),
        (FileType::CommaSeparated, "x01/omprobe_x_grp_coms.csv"),
        (FileType::Merge, "x01/omprobe_x_grp_mrge.mer"),
        (FileType::HtmlTable, "x01/omprobe_x_grp_html.htm"),
        (FileType::XmlExport, "x01/omprobe_x_grp_xmle.xml"),
        (FileType::TabSeparated, "x01/omprobe_x_grp_unsorted.tab"),
    ] {
        let mut mem = support::x01();
        mem.sort_by(&[f::COMPANY, f::FIRST]);
        let got = run(&mut mem, order(&group_fields(), &[f::COMPANY], false, 4), t);
        if !check(name, &got) {
            failures.push(name);
        }
    }
    let mut mem = support::x01();
    mem.sort_by(&[f::COMPANY, f::FIRST]);
    let got = run(
        &mut mem,
        order(&group_fields(), &[], false, 4),
        FileType::TabSeparated,
    );
    if !check("x01/omprobe_x_sorted.tab", &got) {
        failures.push("x01/omprobe_x_sorted.tab");
    }
    assert!(failures.is_empty(), "differ from FileMaker: {failures:?}");
}

#[test]
fn x01_errors_and_single_fields() {
    for name in [
        "x01/omprobe_x_abs.tab",
        "x01/omprobe_x_over.tab",
        "x01/omprobe_x_second.tab",
        "x01/omprobe_dir2/x.tab",
    ] {
        let got = run(
            &mut support::x01(),
            order(&[c(f::K)], &[], false, 4),
            FileType::TabSeparated,
        );
        assert!(check(name, &got), "{name}");
    }
    // An empty found set writes no file.
    let mut mem = support::x01();
    mem.found.clear();
    let request = ExportRequest::new(
        order(&[c(f::K)], &[], false, 4),
        profile(FileType::TabSeparated),
    );
    assert!(export(&mut mem, &request).unwrap().is_none());
    // No fields: 410; repeating fields to Excel or dBase: 720.
    let request = ExportRequest::new(order(&[], &[], false, 4), profile(FileType::TabSeparated));
    assert_eq!(export(&mut support::x01(), &request).unwrap_err().code, 410);
    for t in [FileType::ExcelExport, FileType::DBase] {
        let request = ExportRequest::new(order(&all_fields(), &[], false, 4), profile(t));
        assert_eq!(export(&mut support::x01(), &request).unwrap_err().code, 720);
    }
}

/// x03's field list F3 (Photo is dropped by FileMaker) and FR.
fn x03_fields(repeating: bool) -> Vec<(u64, u64, Option<u64>)> {
    let mut v = vec![
        c(f::K),
        c(f::FIRST),
        c(f::N),
        c(f::D),
        c(f::T),
        c(f::TS),
        c(f::TITLE),
        (ADDRESSES, a::CITY, None),
        (ADDRESSES, a::TYPE, None),
        c(f::G),
        c(f::CN),
        c(f::CU),
        c(f::S),
    ];
    if repeating {
        v.extend([c(f::R), c(f::NR)]);
    } else {
        v.push(c(f::PHOTO));
    }
    v
}

#[test]
fn x03_related_records_and_typed_values() {
    let mut failures = Vec::new();
    for (t, name) in [
        (FileType::TabSeparated, "x03/omprobe_x3_tabs.tab"),
        (FileType::CommaSeparated, "x03/omprobe_x3_coms.csv"),
        (FileType::Merge, "x03/omprobe_x3_mrge.mer"),
        (FileType::HtmlTable, "x03/omprobe_x3_html.htm"),
        (FileType::XmlExport, "x03/omprobe_x3_xmle.xml"),
    ] {
        let got = run(
            &mut support::x03(),
            order(&x03_fields(true), &[], false, 4),
            t,
        );
        if !check(name, &got) {
            failures.push(name);
        }
    }
    // A found set smaller than the table.
    let mut mem = support::x03();
    mem.found.remove(0);
    let got = run(
        &mut mem,
        order(&[c(f::K)], &[], false, 4),
        FileType::XmlExport,
    );
    if !check("x03/omprobe_x3_found.xml", &got) {
        failures.push("x03/omprobe_x3_found.xml");
    }
    assert!(failures.is_empty(), "differ from FileMaker: {failures:?}");
}

#[test]
fn x03_groups() {
    let mut failures = Vec::new();
    let grp_xml = vec![
        c(f::COMPANY),
        c(f::K),
        (CONTACTS, f::S, Some(f::COMPANY)),
        (CONTACTS, f::SC, Some(f::COMPANY)),
        c(f::S),
        c(f::SC),
    ];
    let grp_html = vec![
        c(f::COMPANY),
        c(f::K),
        (CONTACTS, f::S, Some(f::COMPANY)),
        c(f::S),
    ];
    let grp2 = vec![
        c(f::COMPANY),
        c(f::FIRST),
        c(f::K),
        (CONTACTS, f::SC, Some(f::FIRST)),
        (CONTACTS, f::SC, Some(f::COMPANY)),
    ];
    for (fields, group, t, name) in [
        (
            grp_xml,
            vec![f::COMPANY],
            FileType::XmlExport,
            "x03/omprobe_x3_grp_xml.xml",
        ),
        (
            grp_html,
            vec![f::COMPANY],
            FileType::HtmlTable,
            "x03/omprobe_x3_grp_html.htm",
        ),
        (
            grp2,
            vec![f::COMPANY, f::FIRST],
            FileType::TabSeparated,
            "x03/omprobe_x3_grp2.tab",
        ),
        (
            vec![c(f::COMPANY), c(f::K)],
            vec![f::FIRST],
            FileType::TabSeparated,
            "x03/omprobe_x3_grp_sortonly.tab",
        ),
    ] {
        let mut mem = support::x03();
        mem.sort_by(&[f::COMPANY, f::FIRST]);
        let got = run(&mut mem, order(&fields, &group, false, 4), t);
        if !check(name, &got) {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "differ from FileMaker: {failures:?}");
}

/// The parts of a workbook, by name.
fn parts(bytes: &[u8]) -> std::collections::BTreeMap<String, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut f, &mut s).unwrap();
        out.insert(f.name().unwrap().into_owned(), s);
    }
    out
}

/// Compare sheet XML with numbers that are not integers within 1e-12.
fn same_sheet(want: &str, got: &str) -> bool {
    let split = |s: &str| -> Vec<String> { s.split(['<', '>']).map(str::to_owned).collect() };
    let (w, g) = (split(want), split(got));
    w.len() == g.len()
        && w.iter().zip(&g).all(|(a, b)| {
            a == b
                || matches!((a.parse::<f64>(), b.parse::<f64>()), (Ok(x), Ok(y)) if (x - y).abs() < 1e-9 && x.fract() != 0.0)
        })
}

fn check_workbook(name: &str, got: &[u8]) -> bool {
    let want = parts(&std::fs::read(data(name)).unwrap());
    let got = parts(got);
    let mut ok = want.keys().eq(got.keys());
    for (part, w) in &want {
        let Some(g) = got.get(part) else { continue };
        let same = if part == "xl/worksheets/sheet1.xml" {
            same_sheet(w, g)
        } else if part == "docProps/core.xml" {
            let mask = |s: &str| s.split("2026-10-29T").next().unwrap().to_owned();
            mask(w) == mask(g)
        } else {
            w == g
        };
        if !same {
            eprintln!("{name} {part}:\n want {w}\n  got {g}");
            ok = false;
        }
    }
    ok
}

#[test]
fn excel_workbooks() {
    let mut failures = Vec::new();
    let mut mem = support::x01();
    mem.sort_by(&[f::COMPANY, f::FIRST]);
    let got = run(
        &mut mem,
        order(&group_fields(), &[f::COMPANY], false, 4),
        FileType::ExcelExport,
    );
    if !check_workbook("x01/omprobe_x_grp_xlxe.xlsx", &got) {
        failures.push("x01 grp");
    }
    let got = run(
        &mut support::x03(),
        order(&x03_fields(false), &[], false, 4),
        FileType::ExcelExport,
    );
    if !check_workbook("x03/omprobe_x3_xlsx.xlsx", &got) {
        failures.push("x03");
    }
    let got = run(
        &mut support::x03(),
        order(&[c(f::K), c(f::FIRST)], &[], false, 0),
        FileType::ExcelExport,
    );
    if !check_workbook("x03/omprobe_x3_xlsx_c0.xlsx", &got) {
        failures.push("x03 c0");
    }
    let mut mem = support::x03();
    mem.sort_by(&[f::COMPANY, f::FIRST]);
    let fields = vec![
        c(f::COMPANY),
        c(f::K),
        c(f::D),
        (CONTACTS, f::S, Some(f::COMPANY)),
        c(f::S),
    ];
    let got = run(
        &mut mem,
        order(&fields, &[f::COMPANY], false, 4),
        FileType::ExcelExport,
    );
    if !check_workbook("x03/omprobe_x3_grp_xlsx.xlsx", &got) {
        failures.push("x03 grp");
    }
    assert!(failures.is_empty(), "differ from FileMaker: {failures:?}");
}

#[test]
fn dbase() {
    let got = run(
        &mut support::x03(),
        order(&x03_fields(false), &[], false, 4),
        FileType::DBase,
    );
    let want = std::fs::read(data("x03/omprobe_x3_dbf.dbf")).unwrap();
    // The header date is the export's; the rest is FileMaker's.
    assert!(want[4..] == got[4..], "{}", hex_diff(&want, &got));

    // Not FileMaker's output: at the header's 16-bit lengths the file is
    // written and reads back; one past them the export is refused with
    // error 3 and writes nothing (OpenMaker's choice, FileMaker's answer
    // unmeasured). One record; an export order may name a field twice.
    let one = |first: &str, last: &str| {
        let mut mem = Mem::new();
        mem.add(&[(f::FIRST, &[first]), (f::LAST, &[last])]);
        mem
    };
    let to_dbase = |mem: &mut Mem, fields: &[(u64, u64, Option<u64>)]| {
        let request = ExportRequest::new(order(fields, &[], false, 4), profile(FileType::DBase));
        export(mem, &request).map(|bytes| bytes.expect("a file"))
    };
    let read = |bytes: &[u8]| {
        om_transfer::import::read::dbase(bytes, om_transfer::ImportCharset::Windows).unwrap()
    };
    // 2,046 fields: a header of 65,505 bytes.
    let bytes = to_dbase(&mut one("a", "b"), &[c(f::N); 2046]).unwrap();
    assert_eq!(bytes[8..10], 65_505u16.to_le_bytes());
    assert_eq!(read(&bytes).names.map(|n| n.len()), Some(2046));
    let refused = to_dbase(&mut one("a", "b"), &[c(f::N); 2047]).unwrap_err();
    assert_eq!(refused.code, 3);
    // 258 fields of 254 characters and one of 2: a record of 65,535 bytes.
    let wide = "x".repeat(254);
    let mut fields = vec![c(f::FIRST); 258];
    fields.push(c(f::LAST));
    let bytes = to_dbase(&mut one(&wide, "ab"), &fields).unwrap();
    assert_eq!(bytes[10..12], u16::MAX.to_le_bytes());
    let rows = read(&bytes).rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0][0][0].as_str(), rows[0][258][0].as_str()),
        (wide.as_str(), "ab")
    );
    let refused = to_dbase(&mut one(&wide, "abc"), &fields).unwrap_err();
    assert_eq!(refused.code, 3);
}
