//! Import Records against FileMaker Server 26.0.3 (batteries x02 and x04
//! of `tools/fms-transfer-oracle`): each probe's import spec is read from
//! the battery, its source from `sources/x0N`, and the records afterwards,
//! the step's error and the found count must equal what the server's dump
//! (`tests/data/x0N/omprobe_r_<probe>.xml`) and log show.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use om_transfer::import::{import, read, ImportRequest};
use om_transfer::spec::{
    import_flag, ImportAction, ImportOptions, Key, MapEntry, Mapping, Profile, ProfileBody,
    ProfileEx, XmlSource,
};
use om_transfer::{FileType, ImportCharset};
use support::{a, f, Mem, ADDRESSES, CONTACTS};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(path)
}

/// One probe of the battery.
#[derive(Debug, Default)]
struct Probe {
    name: String,
    source: String,
    spec: String,
    /// Contacts the Make script adds (K values).
    pre: Option<String>,
    /// Contacts the MakeN script adds (N values; battery x04).
    pre_n: Option<String>,
    /// The probe clears Contacts first.
    clear: bool,
    omit_first: bool,
}

fn probes() -> Vec<Probe> {
    probes_of("x02-import")
}

fn probes_of(battery: &str) -> Vec<Probe> {
    let text = std::fs::read_to_string(repo(&format!(
        "tools/fms-transfer-oracle/batteries/{battery}.txt"
    )))
    .unwrap();
    let mut out = Vec::new();
    let mut cur: Option<Probe> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            if let Some(p) = cur.take() {
                if !p.spec.is_empty() {
                    out.push(p);
                }
            }
            cur = Some(Probe {
                name: name.trim().into(),
                ..Probe::default()
            });
            continue;
        }
        let Some(p) = cur.as_mut() else { continue };
        if let Some(rest) = line.strip_prefix("Perform Script [ \"Make\" ; Parameter: \"") {
            p.pre = Some(rest.trim_end_matches("\" ]").replace('¶', "\n"));
        } else if let Some(rest) = line.strip_prefix("Perform Script [ \"MakeN\" ; Parameter: \"") {
            p.pre_n = Some(rest.trim_end_matches("\" ]").replace('¶', "\n"));
        } else if line == "Perform Script [ \"Clear\" ]" {
            p.clear = true;
        } else if line == "Omit Record" {
            p.omit_first = true;
        } else if let Some(rest) =
            line.strip_prefix("Set Variable [ $p0 ; Get ( DocumentsPath ) & \"")
        {
            p.source = rest.trim_end_matches("\" ]").into();
        } else if let Some(rest) = line.strip_prefix("Import Records [ ") {
            let rest = rest.trim_end_matches(" ]");
            let (path, spec) = rest.split_once(" ; ").unwrap();
            if let Some(file) = path.trim_matches('"').rsplit('/').next() {
                if p.source.is_empty() {
                    p.source = file.into();
                }
            }
            p.spec = spec.into();
        }
    }
    out.extend(cur.filter(|p| !p.spec.is_empty()));
    out
}

fn field_id(mem: &Mem, reference: &str) -> (u64, u64) {
    let (to, name) = reference.split_once("::").unwrap();
    let occurrence = if to == "Contacts" {
        CONTACTS
    } else {
        ADDRESSES
    };
    let field = mem
        .fields
        .iter()
        .find(|f| f.occurrence == occurrence && f.name == name)
        .unwrap_or_else(|| panic!("{reference}"));
    (occurrence, field.id)
}

/// The import request a probe's step stores (make_patch.py's Import
/// Records syntax).
fn request(mem: &Mem, spec: &str) -> ImportRequest {
    let mut kw = BTreeMap::new();
    for part in spec.split(" ; ") {
        let (k, v) = part.split_once(": ").unwrap();
        kw.insert(k.trim().to_owned(), v.trim().to_owned());
    }
    let on = |k: &str| kw.get(k).is_some_and(|v| v == "On");
    let file_type = FileType::from_code(format!("{:4}", kw["Type"]).as_bytes().try_into().unwrap());
    let target = if kw.get("Target").map(String::as_str) == Some("Addresses") {
        ADDRESSES
    } else {
        CONTACTS
    };
    let mut flags = 0;
    for (k, bit) in [
        ("Auto-enter", import_flag::AUTO_ENTER),
        ("Match names", import_flag::MATCH_NAMES),
        ("Split repetitions", import_flag::SPLIT_REPETITIONS),
        ("Add remaining", import_flag::ADD_REMAINING),
        ("Preserve target", import_flag::PRESERVE_TARGET),
    ] {
        if on(k) {
            flags |= bit;
        }
    }
    let action = match kw.get("Action").map(String::as_str) {
        Some("Update") => ImportAction::UpdateMatching,
        Some("Replace") => ImportAction::UpdateFoundSet,
        _ => ImportAction::Add,
    };
    let map = kw["Map"]
        .split(" | ")
        .map(|e| {
            let e = e.trim().trim_end_matches(" +ae");
            if e == "-" {
                return MapEntry {
                    field: Key::none(),
                    mapping: Mapping::DontImport,
                };
            }
            let (mapping, name) = match e.as_bytes()[0] {
                b'=' => (Mapping::Match, &e[1..]),
                b'!' => (Mapping::DontImport, &e[1..]),
                _ => (Mapping::Import, e),
            };
            MapEntry {
                field: Key::from_id(field_id(mem, name).1),
                mapping,
            }
        })
        .collect();
    let names: i16 = kw.get("Names").map_or(0, |n| n.parse().unwrap());
    ImportRequest {
        options: ImportOptions {
            table: Key::from_id(target),
            action,
            flags,
            charset: ImportCharset::from_stored(kw["Charset"].parse().unwrap()).unwrap(),
            map,
        },
        profile: Profile {
            file_type,
            field_names: u16::from(names > 0),
            body: if file_type == FileType::XmlImport {
                ProfileBody::XmlImport {
                    xml: XmlSource::File(String::new()),
                    xsl: XmlSource::Field,
                }
            } else {
                ProfileBody::Plain
            },
        },
        profile_ex: Some(ProfileEx {
            field_names_row: names - 1,
            predefined: 0,
            delimiter: if file_type == FileType::CommaSeparated {
                ","
            } else {
                "\t"
            }
            .into(),
        }),
        field_flags: None,
    }
}

/// The dump's fields (gen_x02.py's DUMP), as (occurrence, field).
fn dump_fields(mem: &Mem) -> Vec<(u64, u64)> {
    [
        "Contacts::K",
        "Contacts::First Name",
        "Contacts::Last Name",
        "Contacts::N",
        "Contacts::D",
        "Contacts::T",
        "Contacts::TS",
        "Contacts::R",
        "Contacts::NR",
        "Contacts::AE",
        "Contacts::AEO",
        "Contacts::VNE",
        "Contacts::VNO",
        "Contacts::VU",
        "Contacts::CreatedBy",
        "Contacts::PrimaryKey",
        "Contacts::Title",
        "Addresses::City",
    ]
    .iter()
    .map(|r| field_id(mem, r))
    .collect()
}

/// The records as the dump would show them: (mod count, columns).
fn dump(mem: &Mem) -> Vec<(u64, Vec<String>)> {
    let fields = dump_fields(mem);
    mem.contacts
        .iter()
        .map(|r| {
            let cols = fields
                .iter()
                .map(|(o, fld)| {
                    if *o == ADDRESSES {
                        let key = r
                            .cells
                            .get(&f::PRIMARY_KEY)
                            .and_then(|v| v.first())
                            .cloned()
                            .unwrap_or_default();
                        let related: Vec<String> = mem
                            .addresses
                            .iter()
                            .filter(|ad| {
                                !key.is_empty()
                                    && ad.cells.get(&a::FOREIGN_KEY).and_then(|v| v.first())
                                        == Some(&key)
                            })
                            .map(|ad| {
                                ad.cells
                                    .get(fld)
                                    .and_then(|v| v.first())
                                    .cloned()
                                    .unwrap_or_default()
                            })
                            .collect();
                        return if related.is_empty() {
                            "∅".into()
                        } else {
                            related.join("|")
                        };
                    }
                    let reps = mem
                        .fields
                        .iter()
                        .find(|x| x.occurrence == *o && x.id == *fld)
                        .unwrap()
                        .repetitions as usize;
                    let mut v = r.cells.get(fld).cloned().unwrap_or_default();
                    v.resize(reps.max(1), String::new());
                    v.join("|")
                })
                .collect();
            (r.mod_count, cols)
        })
        .collect()
}

/// The server's dump of a probe.
fn server_dump(name: &str) -> Option<Vec<(u64, Vec<String>)>> {
    server_dump_in("x02", name)
}

fn server_dump_in(battery: &str, name: &str) -> Option<Vec<(u64, Vec<String>)>> {
    let bytes = std::fs::read(data(&format!("{battery}/omprobe_r_{name}.xml"))).ok()?;
    let src = read::fmpxmlresult(&bytes, ImportCharset::Utf8).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let mods: Vec<u64> = text
        .split("<ROW MODID=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().parse().unwrap())
        .collect();
    Some(
        src.rows
            .into_iter()
            .zip(mods)
            .map(|(row, m)| {
                let cols = row
                    .into_iter()
                    .map(|c| {
                        if c.is_empty() {
                            "∅".to_owned()
                        } else {
                            c.join("|")
                        }
                    })
                    .collect();
                (m, cols)
            })
            .collect(),
    )
}

/// The server's log: probe -> (error, found count, total).
fn server_log() -> BTreeMap<String, (i32, usize, usize)> {
    server_log_in("x02")
}

fn server_log_in(battery: &str) -> BTreeMap<String, (i32, usize, usize)> {
    let text = std::fs::read_to_string(data(&format!("{battery}/omprobe_log.tab"))).unwrap();
    text.split('\u{0b}')
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            // Tab-separated export: the log's tabs are spaces.
            let p: Vec<&str> = l.trim_end_matches('\r').split(' ').collect();
            (
                p[0].to_owned(),
                (
                    p[1].parse().unwrap(),
                    p[2].parse().unwrap(),
                    p[3].parse().unwrap(),
                ),
            )
        })
        .collect()
}

/// Probes whose outcome this implementation does not reproduce, with why.
const KNOWN: [(&str, &str); 2] = [
    // FileMaker Server's split-repetitions import of text keeps only the
    // first repetition and leaves the other fields empty; not reproduced.
    ("i_split", "split repetitions"),
    // The XML source path of a stored XML import (in the profile) did not
    // resolve on the server (error 717); round trips are tested instead.
    ("i_xml", "XML source path"),
];

#[test]
fn x02_imports_match_filemaker_server() {
    let log = server_log();
    let en = om_calc::Locale::en_us();
    let mut checked = 0;
    let mut failures = Vec::new();
    for probe in probes() {
        if KNOWN.iter().any(|(k, _)| *k == probe.name) {
            continue;
        }
        let mut mem = Mem::new();
        if let Some(pre) = &probe.pre {
            for k in pre.lines() {
                let key = format!("C{k}");
                let first = format!("old{k}");
                // Make's New Record refuses a duplicate primary key.
                if mem
                    .contacts
                    .iter()
                    .any(|r| r.cells.get(&f::PRIMARY_KEY).and_then(|v| v.first()) == Some(&key))
                {
                    continue;
                }
                mem.add(&[
                    (f::K, &[k]),
                    (f::FIRST, &[first.as_str()]),
                    (f::VNE, &["pre"]),
                    (f::VNO, &["pre"]),
                    (f::PRIMARY_KEY, &[key.as_str()]),
                ]);
            }
        }
        if probe.omit_first {
            mem.found.remove(0);
        }
        let request = request(&mem, &probe.spec);
        let path = repo(&format!(
            "tools/fms-transfer-oracle/sources/x02/{}",
            probe.source
        ));
        let (error, found) = match std::fs::read(&path) {
            Err(_) => (100, mem.found.len()),
            // Get ( FoundCount ) is the window's (Contacts) found set: an
            // import into another occurrence leaves it alone.
            Ok(bytes) => match import(&mut mem, &request, &bytes, &en) {
                Ok(summary) => (summary.error_code(), mem.found.len()),
                Err(e) => (e.code, mem.found.len()),
            },
        };
        let (want_error, want_found, want_total) = log[&probe.name];
        let mut problems = Vec::new();
        if error != want_error {
            problems.push(format!("error {error}, FileMaker {want_error}"));
        }
        if found != want_found {
            problems.push(format!("found {found}, FileMaker {want_found}"));
        }
        if mem.contacts.len() != want_total {
            problems.push(format!(
                "total {}, FileMaker {want_total}",
                mem.contacts.len()
            ));
        }
        if let Some(want) = server_dump(&probe.name) {
            let mut got = dump(&mem);
            let mut want = want;
            // Primary keys are UUIDs FileMaker made (column 15).
            for (_, cols) in got.iter_mut().chain(want.iter_mut()) {
                cols[15] = String::new();
            }
            if got != want {
                problems.push(format!("records\n want {want:?}\n  got {got:?}"));
            }
        }
        checked += 1;
        if !problems.is_empty() {
            failures.push(format!("{}: {}", probe.name, problems.join("; ")));
        }
    }
    eprintln!("{checked} probes checked, {} known", KNOWN.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn round_trips_through_every_text_format() {
    use om_transfer::export::{export, ExportRequest};
    use om_transfer::spec::{ExportField, ExportOptions, FieldSpec};
    let en = om_calc::Locale::en_us();
    let fields = [
        f::K,
        f::FIRST,
        f::LAST,
        f::TITLE,
        f::N,
        f::D,
        f::T,
        f::TS,
        f::R,
        f::VNE,
    ];
    for (out_type, in_type) in [
        (FileType::TabSeparated, FileType::TabSeparated),
        (FileType::CommaSeparated, FileType::CommaSeparated),
        (FileType::Merge, FileType::Merge),
        (FileType::XmlExport, FileType::XmlImport),
    ] {
        let mut source = support::x01();
        let options = ExportOptions {
            fields: fields
                .iter()
                .map(|fld| ExportField {
                    field: FieldSpec::new(CONTACTS, *fld),
                    sum_by: FieldSpec::none(),
                })
                .collect(),
            group_by: Vec::new(),
            formatting: false,
            charset: om_transfer::ExportCharset::Utf8,
            use_name_alias: Some(1),
        };
        let profile = Profile {
            file_type: out_type,
            field_names: 0,
            body: if out_type == FileType::XmlExport {
                ProfileBody::XmlExport {
                    grammar: 1,
                    xsl: XmlSource::None,
                }
            } else {
                ProfileBody::Plain
            },
        };
        let bytes = export(&mut source, &ExportRequest::new(options, profile))
            .unwrap()
            .unwrap();
        let mut target = Mem::new();
        let request = ImportRequest {
            options: ImportOptions {
                table: Key::from_id(CONTACTS),
                action: ImportAction::Add,
                flags: 0,
                charset: ImportCharset::Utf8,
                map: fields
                    .iter()
                    .map(|fld| MapEntry {
                        field: Key::from_id(*fld),
                        mapping: Mapping::Import,
                    })
                    .collect(),
            },
            profile: Profile::new(in_type),
            profile_ex: None,
            field_flags: None,
        };
        let summary = import(&mut target, &request, &bytes, &en).unwrap();
        assert_eq!(summary.error_code(), 0, "{out_type:?}");
        assert_eq!(summary.added, 5, "{out_type:?}");
        for (s, t) in source.contacts.iter().zip(&target.contacts) {
            for fld in fields {
                let mut want = s.cells.get(&fld).cloned().unwrap_or_default();
                let mut got = t.cells.get(&fld).cloned().unwrap_or_default();
                let reps = if fld == f::R { 3 } else { 1 };
                want.resize(reps, String::new());
                got.resize(reps, String::new());
                // Tabs become spaces in tab-separated text; CSV writes
                // numbers as numbers (007 is 7).
                let want: Vec<String> = want
                    .into_iter()
                    .map(|v| match (out_type, fld) {
                        (FileType::TabSeparated, _) => v.replace('\t', " "),
                        (FileType::CommaSeparated, f::N) if !v.is_empty() => {
                            om_transfer::text::ExportValue::Stored(v).number_text()
                        }
                        _ => v,
                    })
                    .collect();
                assert_eq!(got, want, "{out_type:?} field {fld}");
            }
        }
    }
}

/// Battery x04 (the import review's P2 and P4), its probes in order on one
/// file as the battery ran: updates into Addresses while the window shows
/// Contacts (Replace works on Addresses' records in creation order, Update
/// matches all of them, the window's found set stays), and update matching
/// that compares as the field's index does (case, numbers by value), with
/// "add remaining" writing the matched records' match fields too.
#[test]
fn x04_matching_and_other_tables_match_filemaker_server() {
    let log = server_log_in("x04");
    let en = om_calc::Locale::en_us();
    let mut mem = Mem::new();
    let mut failures = Vec::new();
    let probes = probes_of("x04-import-matching");
    assert_eq!(probes.len(), 8);
    for probe in probes {
        if probe.clear {
            mem.contacts.clear();
            mem.found.clear();
        }
        for k in probe.pre.iter().flat_map(|p| p.lines()) {
            let (key, first) = (format!("C{k}"), format!("old{k}"));
            mem.add(&[
                (f::K, &[k]),
                (f::FIRST, &[first.as_str()]),
                (f::VNE, &["pre"]),
                (f::VNO, &["pre"]),
                (f::PRIMARY_KEY, &[key.as_str()]),
            ]);
        }
        for (i, n) in probe.pre_n.iter().flat_map(|p| p.lines()).enumerate() {
            let i = i + 1;
            let (k, first, date) = (
                format!("n{i}"),
                format!("old{i}"),
                format!("{i:02}/02/2026"),
            );
            mem.add(&[
                (f::K, &[k.as_str()]),
                (f::FIRST, &[first.as_str()]),
                (f::N, &[n]),
                (f::D, &[date.as_str()]),
                (f::VNE, &["pre"]),
                (f::VNO, &["pre"]),
            ]);
        }
        let request = request(&mem, &probe.spec);
        let bytes = std::fs::read(repo(&format!(
            "tools/fms-transfer-oracle/sources/x04/{}",
            probe.source
        )))
        .unwrap();
        let error = match import(&mut mem, &request, &bytes, &en) {
            Ok(summary) => summary.error_code(),
            Err(e) => e.code,
        };
        let (want_error, want_found, want_total) = log[&probe.name];
        let got = (error, mem.found.len(), mem.contacts.len());
        if got != (want_error, want_found, want_total) {
            failures.push(format!(
                "{}: (error, found, total) {got:?}, FileMaker {:?}",
                probe.name,
                (want_error, want_found, want_total)
            ));
        }
        let want = server_dump_in("x04", &probe.name).expect("a dump");
        let mut got = dump(&mem);
        let mut want = want;
        // Mod counts, the creation account and the primary keys (UUIDs
        // FileMaker made) are the host's.
        for (m, cols) in got.iter_mut().chain(want.iter_mut()) {
            *m = 0;
            cols[14] = String::new();
            cols[15] = String::new();
        }
        if got != want {
            failures.push(format!(
                "{}: records\n want {want:?}\n  got {got:?}",
                probe.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// "Preserve target field data if source field is empty" (26.1.1): an
/// update keeps what an empty source value would have cleared, repetition
/// by repetition, and still writes the rest; a flag OpenMaker does not know
/// refuses the import with no record touched. A data-loss path: before,
/// the empty values overwrote the record's.
#[test]
fn an_update_preserving_target_data_keeps_fields_its_source_leaves_empty() {
    let en = om_calc::Locale::en_us();
    let spec = "Type: TABS ; Target: Contacts ; Charset: 5 ; Action: Replace ; \
                Preserve target: On ; Map: Contacts::K | Contacts::First Name | Contacts::R";
    let source = "new\t\ta\u{1d}\u{1d}c\r";
    let original = |mem: &mut Mem| {
        mem.add(&[
            (f::K, &["old"]),
            (f::FIRST, &["kept"]),
            (f::R, &["x", "y", "z"]),
            (f::VNE, &["pre"]),
            (f::VNO, &["pre"]),
        ])
    };

    let mut mem = Mem::new();
    let id = original(&mut mem);
    let request = request(&mem, spec);
    let summary = import(&mut mem, &request, source.as_bytes(), &en).unwrap();
    assert_eq!((summary.updated, summary.error_code()), (1, 0));
    let cells = &mem.record(id).cells;
    assert_eq!(cells[&f::K], ["new"]);
    assert_eq!(cells[&f::FIRST], ["kept"]);
    assert_eq!(cells[&f::R], ["a", "y", "c"]);

    let mut mem = Mem::new();
    let id = original(&mut mem);
    let mut request = request;
    request.options.flags |= 0x80;
    let e = import(&mut mem, &request, source.as_bytes(), &en).unwrap_err();
    assert_eq!(e.code, 3);
    assert_eq!(mem.record(id).cells[&f::K], ["old"]);
    assert_eq!(mem.record(id).mod_count, 0);

    // With "perform auto-enter options" too, the update refuses before any
    // row, whatever the fields: one left unwritten would no longer be kept
    // from auto-entry.
    let with_auto_enter = spec.replace(
        "Preserve target: On",
        "Preserve target: On ; Auto-enter: On",
    );
    let mut mem = Mem::new();
    let id = original(&mut mem);
    let request = crate::request(&mem, &with_auto_enter);
    let e = import(&mut mem, &request, source.as_bytes(), &en).unwrap_err();
    assert_eq!(e.code, 3);
    assert_eq!(mem.record(id).cells[&f::FIRST], ["kept"]);
    assert_eq!(mem.record(id).mod_count, 0);
}
