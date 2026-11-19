//! The Design functions replayed against FileMaker Server's own answers
//! (calc-functions.md 5.8, "Design functions on FileMaker Server"): every
//! line of `tests/data/design/*.tsv` is an expression and the text the
//! server's script engine gave for it over the same file
//! (`tools/fms-script-harness`, batteries `s30-design*`). The host is the
//! one the desktop client uses (`om_data::host::FileHost` over the file's
//! calculation schema), on the file read locally, with no records.
//!
//! Ignored by default: the files are the shipped templates, FileMaker
//! Server's sample and generated probe files, which exist only on the
//! analysis host (`OM_HARNESS_FILES`, default the harness's `files`
//! directory, `re/work/fms-harness/files` in the repository). Run with
//!   cargo test -p om-data --test design_oracle -- --ignored --nocapture

use std::path::{Path, PathBuf};

use om_data::calc::eval::{evaluate_text, Context, Variables};
use om_data::calc::{Ctx, Date};
use om_data::host::{
    stored_value, CalcSchema, FileCalc, FileHost, MemorySource, RecordValues, SessionInfo,
};
use om_hbam::{Catalog, HbamFile};

const FILES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../re/work/fms-harness/files"
);

fn files_dir() -> PathBuf {
    std::env::var_os("OM_HARNESS_FILES").map_or_else(|| PathBuf::from(FILES), PathBuf::from)
}

/// One fixture: the file, the window's layout and the expected answers.
struct Fixture {
    name: String,
    file: String,
    layout: String,
    /// `after`: evaluated with no table occurrence; `script`: on the
    /// default layout's occurrence.
    in_script: bool,
    rows: Vec<(String, String)>,
}

/// The harness's escapes (`\r`, `\n`, `\t`, `\\`, `\uXXXX`) decoded.
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn fixtures() -> Vec<Fixture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/design");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/data/design")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut fixture = Fixture {
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                file: String::new(),
                layout: String::new(),
                in_script: path.to_string_lossy().ends_with(".script.tsv"),
                rows: Vec::new(),
            };
            for line in text.lines() {
                if let Some(header) = line.strip_prefix("# ") {
                    match header.split_once('\t') {
                        Some(("file", file)) => fixture.file = file.into(),
                        Some(("layout", layout)) => fixture.layout = layout.into(),
                        _ => {}
                    }
                } else if let Some((expression, expected)) = line.split_once('\t') {
                    fixture.rows.push((expression.into(), unescape(expected)));
                }
            }
            fixture
        })
        .collect()
}

/// The file's calculation schema, named `work` as the harness's copy is,
/// and its records' first repetitions (what value lists read).
fn open(path: &Path, today: Date) -> (CalcSchema, MemorySource) {
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tree = file.key_tree().unwrap();
    let catalog = Catalog::from_tree(&tree).unwrap();
    let schema = om_data::rules::schema::calc_schema(&tree, &catalog, "work");
    let cx = Ctx::new(&schema.locale, today);
    let mut source = MemorySource::new();
    for table in schema.tables() {
        for record in om_hbam::record::records(&tree, table.id) {
            let mut values = RecordValues::default();
            for def in table.fields.values() {
                if let Some(Ok(text)) = record.text(def.id) {
                    values.set(def.id, 1, stored_value(&text, def.value_type, &cx));
                }
            }
            source.put_record(table.id, record.id, values);
        }
    }
    (schema, source)
}

#[test]
#[ignore = "requires the harness's sample and probe files"]
fn design_functions_answer_as_filemaker_server() {
    let dir = files_dir();
    let mut failures = Vec::new();
    let mut total = 0;
    for fixture in fixtures() {
        let path = dir.join(&fixture.file);
        assert!(
            path.exists(),
            "{}: {} is missing",
            fixture.name,
            path.display()
        );
        let today = Date::from_ymd(2026, 10, 29).unwrap();
        let (schema, source) = open(&path, today);
        let session = SessionInfo {
            file_name: "work".into(),
            window_name: "work".into(),
            layout_name: fixture.layout.clone(),
            ..SessionInfo::default()
        };
        // In a script, the window's occurrence and its first record.
        let context = match schema
            .layout_named(&fixture.layout)
            .and_then(|l| l.occurrence)
            .filter(|_| fixture.in_script)
        {
            Some(occurrence) => {
                let table = schema.table_of(occurrence).unwrap().id;
                let first = om_data::host::CalcSource::records(&source, table)
                    .unwrap()
                    .first()
                    .copied();
                Context {
                    occurrence: Some(occurrence),
                    record: first,
                    ..Context::new()
                }
            }
            None => Context::new(),
        };
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let mut host = FileHost::new(
            FileCalc::new(&schema, &source, today),
            &session,
            &mut locals,
            &mut globals,
        );
        let cx = Ctx::new(&schema.locale, today);
        for (expression, expected) in &fixture.rows {
            total += 1;
            let result = evaluate_text(expression, &mut host, &context);
            let text = result.value.to_text(&cx).into_string();
            // The harness writes a failed value as `#<error> <text>`, the
            // in-script probe as `#<error>`.
            let got = match result.code() {
                0 => text,
                code if expected.ends_with(' ') || !expected.contains(' ') => {
                    if expected.contains(' ') {
                        format!("#{code} {text}")
                    } else {
                        format!("#{code}")
                    }
                }
                code => format!("#{code} {text}"),
            };
            if &got != expected {
                failures.push(format!(
                    "{}: {expression}\n  want {expected:?}\n  got  {got:?}",
                    fixture.name
                ));
            }
        }
    }
    for f in &failures {
        println!("{f}");
    }
    println!("{} of {total} rows match", total - failures.len());
    assert!(failures.is_empty(), "{} rows differ", failures.len());
}

/// `GetLayoutObjectAttribute ( "name" ; "attribute" { ; repetition ; row } )`
/// split into its arguments (the fixtures' literal form).
fn layout_object_call(expression: &str) -> Option<(String, String, u32, u32)> {
    let inner = expression
        .strip_prefix("GetLayoutObjectAttribute ( ")?
        .strip_suffix(" )")?;
    let parts: Vec<&str> = inner.split(" ; ").collect();
    let quoted = |s: &str| s.trim_matches('"').to_owned();
    let number = |i: usize| parts.get(i).and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((quoted(parts[0]), quoted(parts[1]), number(2), number(3)))
}

#[test]
#[ignore = "requires the harness's sample files"]
fn layout_objects_answer_as_filemaker_server() {
    use om_data::host::layout_objects::{layout_object_attribute, NoRecord, ObjectWindow};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/layout-objects");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    let (mut total, mut failures) = (0, Vec::new());
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap();
        let header = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("# {key}\t")))
                .unwrap()
                .to_owned()
        };
        let today = Date::from_ymd(2026, 10, 29).unwrap();
        let (schema, _) = open(&files_dir().join(header("file")), today);
        let entry = schema.layout_named(&header("layout")).unwrap();
        let layout = entry.layout.as_deref().unwrap();
        // No records: a list's body is not drawn.
        let list = layout.def.as_ref().is_some_and(|d| d.flags & 0x4200 != 0);
        let window = ObjectWindow {
            layout,
            screen: None,
            focus: None,
            body_shown: !list,
        };
        let cx = Ctx::new(&schema.locale, today);
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let (expression, expected) = line.split_once('\t').unwrap();
            let Some((name, attribute, repetition, row)) = layout_object_call(expression) else {
                continue;
            };
            total += 1;
            let got = layout_object_attribute(
                &window,
                &schema,
                &name,
                &attribute,
                repetition,
                row,
                &mut NoRecord,
            )
            .to_text(&cx)
            .into_string();
            let expected = unescape(expected);
            if got != expected {
                failures.push(format!(
                    "{}: {expression}: want {expected:?} got {got:?}",
                    path.display()
                ));
            }
        }
    }
    for f in &failures {
        println!("{f}");
    }
    println!("{} of {total} rows match", total - failures.len());
    assert!(failures.is_empty(), "{} rows differ", failures.len());
}
