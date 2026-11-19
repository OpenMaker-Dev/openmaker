//! The alpha scorecard's record transfer measure (criterion 6, its second
//! half; `docs/development.md`, "Alpha scorecard"): for each file named,
//! through the desktop window as a user works it, with the file's own main
//! table (the table of the layout the file opens on):
//!
//! 1. File > Import Records of a generated seed (three records, one column
//!    per data field without auto-entry) into a copy of the file;
//! 2. File > Export Records of the found set (the imported records) as
//!    comma-separated values, those fields, UTF-8;
//! 3. File > Save/Send Records As > Excel of the same records and fields;
//! 4. File > Import Records of that CSV into a fresh copy, then its found
//!    set exported again as in 2.
//!
//! It reports which steps succeed, the first error of each that does not,
//! and the first difference between what was imported and what was
//! exported: the seed against the first export, the first export against
//! the fresh copy's, and the workbook against the first export (read back
//! with OpenMaker's own readers). Nothing is compared with FileMaker: how
//! FileMaker formats each type on export is not measured here.
//!
//! Starter solutions hold no records, hence the seed. The files named are
//! never changed: each step works on a copy in the output folder.
mod common;

use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::App;
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use egui_kittest::Harness;
use om_data::calc::ValueType;
use om_data::host::FieldClass;
use om_data::{DataProvider, LocalFile, LocalOpen};
use om_gui::OpenMakerApp;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_transfer::import::{self, ImportRequest};
use om_transfer::spec::{Key, ProfileBody};
use om_transfer::{FileType, ImportAction, ImportCharset, ImportOptions, Profile};
use serde::Serialize;

/// How long one step may take before it counts as stuck.
const STEP: Duration = Duration::from_secs(120);

fn provider() -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    })
}

fn app() -> App {
    Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .build_eframe(|_cc| OpenMakerApp::new(Box::new(provider())))
}

#[derive(Serialize)]
struct Field {
    name: String,
    kind: &'static str,
}

#[derive(Serialize, Default)]
struct Step {
    ok: bool,
    /// The first error, or what was observed.
    detail: String,
}

#[derive(Serialize, Default)]
struct Card {
    file: String,
    layout: String,
    table: String,
    /// The fields seeded, exported and imported.
    fields: Vec<Field>,
    /// The table's other fields, and why each is left out.
    left_out: Vec<(String, String)>,
    import_seed: Option<Step>,
    export_csv: Option<Step>,
    save_excel: Option<Step>,
    import_fresh: Option<Step>,
    export_fresh: Option<Step>,
    /// The first difference of each comparison; `None` when identical or
    /// not made.
    seed_vs_export: Option<String>,
    export_vs_fresh: Option<String>,
    excel_vs_export: Option<String>,
    /// Why the measure stopped, when it did.
    error: Option<String>,
}

fn kind(t: ValueType) -> &'static str {
    match t {
        ValueType::Text => "text",
        ValueType::Number => "number",
        ValueType::Date => "date",
        ValueType::Time => "time",
        ValueType::Timestamp => "timestamp",
        ValueType::Container => "container",
    }
}

/// Row `row`'s seed value of a field of type `t`: text with a comma and
/// quotes in the first row; dates as the starter files' US date format.
fn seed_value(t: ValueType, row: usize) -> &'static str {
    match (t, row) {
        (ValueType::Number, 0) => "12.5",
        (ValueType::Number, 1) => "7",
        (ValueType::Number, _) => "1000",
        (ValueType::Date, 0) => "11/13/2026",
        (ValueType::Date, 1) => "1/2/2025",
        (ValueType::Date, _) => "12/31/2024",
        (ValueType::Time, 0) => "10:30:00",
        (ValueType::Time, 1) => "8:05:00",
        (ValueType::Time, _) => "23:59:59",
        (ValueType::Timestamp, 0) => "11/13/2026 10:30:00",
        (ValueType::Timestamp, 1) => "1/2/2025 8:05:00",
        (ValueType::Timestamp, _) => "12/31/2024 23:59:59",
        (_, 0) => "Seed one, \"quoted\"",
        (_, 1) => "Seed two",
        (_, _) => "Seed three",
    }
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Open `path` with the account the file suggests and an empty password,
/// as the scorecard does.
fn open_reader(path: &Path) -> Result<NativeProvider, String> {
    let mut provider = provider();
    let mut file = LocalFile {
        path: path.to_owned(),
        credentials: None,
    };
    let account = match provider.open_local(&file).map_err(|e| e.message)? {
        LocalOpen::Opened { .. } => return Ok(provider),
        LocalOpen::NeedsLogin { account } => account,
    };
    file.credentials = Some((account, Default::default()));
    match provider.open_local(&file).map_err(|e| e.message)? {
        LocalOpen::Opened { .. } => Ok(provider),
        LocalOpen::NeedsLogin { .. } => Err("the suggested account was refused".into()),
    }
}

/// The main table of `layout` in the file at `path` (a scratch copy), its
/// fields to seed and those left out.
fn fields(path: &Path, layout: &str, card: &mut Card) -> Result<Vec<(String, ValueType)>, String> {
    let mut provider = open_reader(path)?;
    let schema = provider
        .calc_schema()
        .map_err(|e| e.message)?
        .ok_or("no calculation schema")?;
    provider.close();
    let occurrence = schema
        .layouts()
        .iter()
        .find(|l| l.name == layout)
        .and_then(|l| l.occurrence)
        .ok_or_else(|| format!("layout “{layout}” has no table occurrence"))?;
    let table = schema.table_of(occurrence).ok_or("no base table")?;
    card.table = table.name.clone();
    // Auto-entry, read from the file as the record planner reads it.
    let file = om_hbam::HbamFile::open(path).map_err(|e| e.to_string())?;
    let tree = file.key_tree().map_err(|e| e.to_string())?;
    let auto: BTreeSet<u64> = om_hbam::catalog::fields(&tree, table.id)
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|f| {
            f.decoded_options().is_some_and(|o| {
                o.auto_entry_variable().is_some()
                    || o.auto_entry_serial()
                    || o.auto_entry_constant()
                    || o.auto_entry_calculation()
                    || o.auto_entry_editing_prohibited()
            })
        })
        .map(|f| f.id)
        .collect();
    let mut seeded = Vec::new();
    for def in table.fields.values() {
        let why = match def.class {
            FieldClass::Calculation => Some("calculation"),
            FieldClass::Summary => Some("summary"),
            _ if def.global => Some("global"),
            _ if def.value_type == ValueType::Container => Some("container"),
            _ if def.repetitions > 1 => Some("repetitions"),
            _ if auto.contains(&def.id) => Some("auto-entry"),
            _ => None,
        };
        match why {
            Some(why) => card.left_out.push((def.name.clone(), why.into())),
            None => seeded.push((def.name.clone(), def.value_type)),
        }
    }
    Ok(seeded)
}

/// Step until `done` or an error alert; `Err` with the alert's text, or a
/// timeout naming `what`.
fn wait(app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) -> Result<(), String> {
    let deadline = Instant::now() + STEP;
    loop {
        app.step();
        if let Some(error) = app.state().error() {
            return Err(error.to_owned());
        }
        if done(app) {
            app.run_steps(2);
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn shows(app: &App, label: &str) -> bool {
    app.query_by_label(label).is_some()
}

fn idle(app: &mut App) -> Result<(), String> {
    wait(app, "the step to end", |app| !app.state().script_running())
}

fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// Click the last button labelled exactly `label`: a menu's or a popup's
/// item is drawn after the layout behind it.
fn click_last(app: &mut App, label: &str) -> Result<(), String> {
    press(app, label, true)
}

/// Click a button labelled exactly `label`, the last one or the first,
/// scrolled into view first: a pointer click at the centre of an item
/// scrolled out of its list would land on whatever is drawn there.
fn press(app: &mut App, label: &str, last: bool) -> Result<(), String> {
    fn find<'a>(app: &'a App, label: &'a str, last: bool) -> Option<egui_kittest::Node<'a>> {
        let mut all = app.query_all(By::new().role(egui::Role::Button).label(label));
        if last {
            all.last()
        } else {
            all.next()
        }
    }
    find(app, label, last)
        .ok_or_else(|| format!("no button “{label}”"))?
        .scroll_to_me();
    app.run_steps(3);
    find(app, label, last)
        .ok_or_else(|| format!("no button “{label}”"))?
        .click();
    app.run_steps(2);
    Ok(())
}

/// Every node's role and label (or value), in tree order.
fn nodes(app: &App) -> Vec<(egui::Role, String)> {
    fn walk(node: egui_kittest::Node<'_>, out: &mut Vec<(egui::Role, String)>) {
        let a = node.accesskit_node();
        let text = a.label().or_else(|| a.value()).unwrap_or_default();
        out.push((a.role(), text));
        for child in node.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(app.root(), &mut out);
    out
}

/// The export dialog's chosen fields, in order.
fn export_order(app: &App) -> Vec<String> {
    let nodes = nodes(app);
    let Some(start) = nodes.iter().position(|(_, t)| t == "Fields to export") else {
        return Vec::new();
    };
    nodes[start + 1..]
        .iter()
        .filter(|(role, _)| *role == egui::Role::Button)
        .map(|(_, t)| t.clone())
        .take_while(|t| t != "Move")
        .collect()
}

/// File > Open the copy at `path`; the layout it opens on.
fn open(app: &mut App, path: &Path) -> Result<String, String> {
    common::open_menu(app, "File");
    common::click(app, "Open...");
    wait(app, "the Open dialog", |app| shows(app, "Open File"))?;
    replace(app, "Path", &path.display().to_string());
    common::click(app, "Open");
    wait(app, "the account dialog or the file", |app| {
        shows(app, "Account Name") || app.state().is_settled()
    })?;
    if shows(app, "Account Name") {
        common::click(app, "OK");
    }
    wait(app, "the file to open", |app| app.state().is_settled())?;
    app.run_steps(5);
    let layout = app
        .get_by_role_and_label(egui::Role::ComboBox, "Layout")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    Ok(layout)
}

/// File > Import Records > File... of the CSV at `path`, column `i` into
/// field `fields[i]`, adding records with the dialog's other defaults.
/// The Import Summary's counts.
fn import_csv(app: &mut App, path: &Path, fields: &[String]) -> Result<String, String> {
    common::open_menu(app, "File");
    common::click(app, "Import Records");
    common::click(app, "File...");
    wait(app, "the import source dialog", |app| {
        shows(app, "Import Records from File")
    })?;
    replace(app, "Source file:", &path.display().to_string());
    common::click(app, "File type:");
    click_last(app, "Comma-Separated Values")?;
    common::click(app, "Source character set:");
    click_last(app, "UTF-8")?;
    common::click(app, "Continue");
    wait(app, "the import order dialog", |app| {
        shows(app, "Specify Import Order")
    })?;
    for (i, field) in fields.iter().enumerate() {
        let column = format!("Target field for source column {}", i + 1);
        // The mapping list draws only the columns in view.
        app.get_by_role_and_label(egui::Role::ComboBox, &column)
            .scroll_to_me();
        app.run_steps(3);
        common::click(app, &column);
        click_last(app, field).map_err(|e| format!("mapping column {}: {e}", i + 1))?;
        let shown = app
            .get_by_role_and_label(egui::Role::ComboBox, &column)
            .value()
            .unwrap_or_default();
        if &shown != field {
            return Err(format!(
                "mapping column {}: the dialog shows “{shown}”, not “{field}”",
                i + 1
            ));
        }
    }
    click_last(app, "Import")?;
    wait(app, "the Import Summary", |app| {
        shows(app, "Import Summary")
    })?;
    let values: Vec<String> = app
        .query_all(By::new().role(egui::Role::Label))
        .filter_map(|n| n.accesskit_node().value())
        .collect();
    let mut summary = Vec::new();
    for label in [
        "Total records added / updated",
        "Records skipped due to errors",
        "Total records in source",
    ] {
        if let Some(i) = values.iter().position(|v| v == label) {
            summary.push(format!(
                "{label}: {}",
                values.get(i + 1).map_or("?", String::as_str)
            ));
        }
    }
    common::click(app, "OK");
    idle(app)?;
    Ok(summary.join("; "))
}

/// Export Records (CSV, UTF-8) or Save Records as Excel of the found set,
/// `fields` in order, to `path`.
fn export(app: &mut App, path: &Path, fields: &[String], excel: bool) -> Result<(), String> {
    common::open_menu(app, "File");
    let (heading, confirm) = if excel {
        common::click(app, "Save/Send Records As");
        click_last(app, "Excel...")?;
        ("Save Records as Excel", "Save")
    } else {
        common::click(app, "Export Records...");
        ("Export Records", "Export")
    };
    wait(app, "the export dialog", |app| shows(app, heading))?;
    if !excel {
        common::click(app, "Type:");
        click_last(app, "Comma-Separated Values")?;
        common::click(app, "Output file character set:");
        click_last(app, "Unicode (UTF-8)")?;
    }
    // The dialog starts with the layout's fields chosen: clear them.
    let mut cleared = false;
    for _ in 0..500 {
        replace(app, "Save as:", &path.display().to_string());
        if app.get_by_label(confirm).accesskit_node().is_disabled() {
            cleared = true;
            break;
        }
        common::click(app, "Clear");
    }
    if !cleared {
        return Err("the export order did not clear".into());
    }
    for field in fields {
        press(app, field, false)
            .map_err(|_| format!("field “{field}” is not offered for export"))?;
        common::click(app, "Move");
    }
    let order = export_order(app);
    if order != fields {
        return Err(format!("the export order is {order:?}"));
    }
    replace(app, "Save as:", &path.display().to_string());
    click_last(app, confirm)?;
    idle(app)?;
    if !path.exists() {
        return Err("no file was written".into());
    }
    Ok(())
}

/// The rows of a CSV (UTF-8) or workbook (first sheet) as OpenMaker's
/// import reads them.
fn rows(path: &Path, excel: bool) -> Result<Vec<Vec<String>>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut profile = Profile::new(if excel {
        FileType::Excel2007
    } else {
        FileType::CommaSeparated
    });
    if excel {
        profile.body = ProfileBody::Excel {
            sheet: 0,
            worksheet: String::new(),
            file: String::new(),
        };
    }
    let request = ImportRequest {
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
    };
    let source =
        import::parse(&request, &bytes).map_err(|e| format!("{} ({})", e.message, e.code))?;
    Ok(source
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.into_iter().next().unwrap_or_default())
                .collect()
        })
        .collect())
}

/// The first difference between two tables of rows, naming the field.
fn first_difference(
    a: &[Vec<String>],
    b: &[Vec<String>],
    fields: &[String],
    (left, right): (&str, &str),
) -> Option<String> {
    if a.len() != b.len() {
        return Some(format!(
            "{} rows in {left}, {} in {right}",
            a.len(),
            b.len()
        ));
    }
    for (r, (ra, rb)) in a.iter().zip(b).enumerate() {
        let width = ra.len().max(rb.len());
        for c in 0..width {
            let va = ra.get(c).map_or("", String::as_str);
            let vb = rb.get(c).map_or("", String::as_str);
            if va != vb {
                let field = fields.get(c).map_or("(extra column)", String::as_str);
                return Some(format!(
                    "row {}, {field}: {left} “{va}”, {right} “{vb}”",
                    r + 1
                ));
            }
        }
    }
    None
}

/// Each column's first difference between the export's rows and the
/// workbook's, or the first difference of row counts.
fn column_differences(a: &[Vec<String>], b: &[Vec<String>], fields: &[String]) -> Option<String> {
    if a.len() != b.len() {
        return first_difference(a, b, fields, ("the export", "the workbook"));
    }
    let mut out = Vec::new();
    for (c, field) in fields.iter().enumerate() {
        let cell = |rows: &[Vec<String>], r: usize| rows[r].get(c).cloned().unwrap_or_default();
        if let Some(r) = (0..a.len()).find(|&r| cell(a, r) != cell(b, r)) {
            out.push(format!(
                "{field}: the export “{}”, the workbook “{}”",
                cell(a, r),
                cell(b, r)
            ));
        }
    }
    (!out.is_empty()).then(|| out.join("; "))
}

fn step(result: Result<String, String>) -> Step {
    match result {
        Ok(detail) => Step { ok: true, detail },
        Err(detail) => Step { ok: false, detail },
    }
}

fn copy(original: &Path, dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::copy(original, &path).expect("copy the file");
    path
}

fn measure(original: &Path, dir: &Path, card: &mut Card) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let first = copy(original, dir, "first.fmp12");
    let scratch = copy(original, dir, "scratch.fmp12");

    let mut app = app();
    card.layout = open(&mut app, &first).map_err(|e| format!("opening the copy: {e}"))?;
    let layout = card.layout.clone();
    let fields = fields(&scratch, &layout, card)?;
    card.fields = fields
        .iter()
        .map(|(name, t)| Field {
            name: name.clone(),
            kind: kind(*t),
        })
        .collect();
    let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
    if names.is_empty() {
        return Err("the main table has no field to seed".into());
    }
    let seed_rows: Vec<Vec<String>> = (0..3)
        .map(|row| {
            fields
                .iter()
                .map(|(_, t)| seed_value(*t, row).to_owned())
                .collect()
        })
        .collect();
    let seed = dir.join("seed.csv");
    let text: String = seed_rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|v| csv_cell(v)).collect();
            cells.join(",") + "\r"
        })
        .collect();
    std::fs::write(&seed, text).map_err(|e| e.to_string())?;

    let seeded = import_csv(&mut app, &seed, &names);
    let ok = seeded.is_ok();
    card.import_seed = Some(step(seeded));
    if !ok {
        return Ok(());
    }
    let csv = dir.join("first.csv");
    let exported = export(&mut app, &csv, &names, false).map(|()| String::new());
    let csv_ok = exported.is_ok();
    card.export_csv = Some(step(exported));
    let xlsx = dir.join("first.xlsx");
    card.save_excel = Some(step(
        export(&mut app, &xlsx, &names, true).map(|()| String::new()),
    ));
    drop(app);
    let first_rows = if csv_ok {
        match rows(&csv, false) {
            Ok(rows) => Some(rows),
            Err(e) => {
                return Err(format!(
                    "the export does not read back ({e}); nothing compared or imported"
                ))
            }
        }
    } else {
        None
    };
    if let Some(first_rows) = &first_rows {
        card.seed_vs_export =
            first_difference(&seed_rows, first_rows, &names, ("the seed", "the export"));
        if card.save_excel.as_ref().is_some_and(|s| s.ok) {
            card.excel_vs_export = match rows(&xlsx, true) {
                Ok(mut sheet) => {
                    // Save Records as Excel writes a row of field names first.
                    let header = (!sheet.is_empty()).then(|| sheet.remove(0));
                    if header.as_deref() != Some(&names[..]) {
                        Some(format!("header row {header:?}"))
                    } else {
                        column_differences(first_rows, &sheet, &names)
                    }
                }
                Err(e) => Some(format!("the workbook does not read back: {e}")),
            };
        }
    }
    let Some(first_rows) = first_rows else {
        return Ok(());
    };

    let fresh = copy(original, dir, "fresh.fmp12");
    let mut app = self::app();
    open(&mut app, &fresh).map_err(|e| format!("opening the fresh copy: {e}"))?;
    let imported = import_csv(&mut app, &csv, &names);
    let ok = imported.is_ok();
    card.import_fresh = Some(step(imported));
    if !ok {
        return Ok(());
    }
    let again = dir.join("fresh.csv");
    let exported = export(&mut app, &again, &names, false).map(|()| String::new());
    let ok = exported.is_ok();
    card.export_fresh = Some(step(exported));
    drop(app);
    if ok {
        card.export_vs_fresh = match rows(&again, false) {
            Ok(fresh_rows) => first_difference(
                &first_rows,
                &fresh_rows,
                &names,
                ("the first export", "the fresh copy's"),
            ),
            Err(e) => Some(format!("the fresh copy's export does not read back: {e}")),
        };
        if card.export_vs_fresh.is_none() && std::fs::read(&csv).ok() != std::fs::read(&again).ok()
        {
            card.export_vs_fresh = Some("same cells, different bytes".into());
        }
    }
    Ok(())
}

fn mark(step: &Option<Step>) -> String {
    match step {
        None => "-".into(),
        Some(s) if s.ok => "yes".into(),
        Some(_) => "**no**".into(),
    }
}

#[test]
#[ignore = "requires OM_SCORECARD_FILES (paths of .fmp12 files, ':'-separated) and OM_SCORECARD_OUT (an output folder)"]
fn transfer_scorecard() {
    let files = std::env::var("OM_SCORECARD_FILES").expect("OM_SCORECARD_FILES");
    let out = PathBuf::from(std::env::var("OM_SCORECARD_OUT").expect("OM_SCORECARD_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let mut cards = Vec::new();
    for (n, path) in std::env::split_paths(&files).enumerate() {
        let name = path
            .components()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let mut card = Card {
            file: name,
            ..Card::default()
        };
        let dir = out.join(format!("transfer-{n}"));
        let result = catch_unwind(AssertUnwindSafe(|| measure(&path, &dir, &mut card)));
        card.error = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(panic) => Some(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "the measure panicked".into()),
            ),
        };
        cards.push(card);
    }
    let json = serde_json::to_string_pretty(&cards).unwrap();
    std::fs::write(out.join("transfer.json"), json).unwrap();
    let mut md = String::from(
        "| File | Main table (layout) | Fields | Import seed | Export CSV | Save as Excel | Import CSV into fresh copy | Export again | First error or difference |\n|---|---|---:|---|---|---|---|---|---|\n",
    );
    for card in &cards {
        let first = [
            &card.import_seed,
            &card.export_csv,
            &card.save_excel,
            &card.import_fresh,
            &card.export_fresh,
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.ok)
        .map(|s| s.detail.clone())
        .or_else(|| card.error.clone())
        .or_else(|| {
            card.seed_vs_export
                .clone()
                .map(|d| format!("seed vs export: {d}"))
        })
        .or_else(|| {
            card.excel_vs_export
                .clone()
                .map(|d| format!("export vs workbook: {d}"))
        })
        .or_else(|| {
            card.export_vs_fresh
                .clone()
                .map(|d| format!("export vs fresh copy: {d}"))
        })
        .unwrap_or_default();
        md.push_str(&format!(
            "| {} | {} ({}) | {} | {} | {} | {} | {} | {} | {} |\n",
            card.file,
            card.table,
            card.layout,
            card.fields.len(),
            mark(&card.import_seed),
            mark(&card.export_csv),
            mark(&card.save_excel),
            mark(&card.import_fresh),
            mark(&card.export_fresh),
            first.replace('|', "\\|").replace('\n', " "),
        ));
    }
    std::fs::write(out.join("transfer.md"), md).unwrap();
}
