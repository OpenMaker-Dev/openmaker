//! Export Records and Import Records in the desktop client, driven through
//! the window against the native provider and an in-process `om-server`
//! hosting the Tasks file of `om-server/tests/fixtures/tasks.rs`: a script
//! exports the found set and imports it back, imports keep the import's
//! rules (every field type and repetition, no "only during data entry"
//! validation, auto-enter as the step says, also inside Open Transaction),
//! and the export dialog, the import mapping dialog and the Import Summary
//! run an export and an import (script-steps.md "Implementation notes
//! (import/export)"). Before, a mock provider accepted anything, and the
//! desktop import refused every row mapping a number or date field (201).
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
#[path = "../../om-server/tests/fixtures/transfer.rs"]
mod transfer;

use std::path::{Path, PathBuf};

use egui_kittest::kittest::{By, NodeT as _, Queryable};
use fixture::{calc, CODE, COUNT, CREATOR, DUE, NAME, TABLE};
use om_data::host::CalcNeed;
use om_data::DataProvider;
use om_scripts::{flag, Script, ScriptStep};
use om_transfer::spec::ImportAction;
use om_transfer::FileType;

/// Tasks::Tags, a text field with three repetitions the tests add.
const TAGS: u64 = 20;

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Show Custom Dialog [ "Transfer" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Transfer\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// `$p` set to `name` in the session's temporary folder.
fn path_to(name: &str) -> ScriptStep {
    set_variable("$p", &format!("Get ( TemporaryPath ) & \"{name}\""))
}

/// A step with its dialog shown (no "No dialog").
fn with_dialog(mut step: ScriptStep) -> ScriptStep {
    step.flags &= !flag::NO_INTERACT;
    step
}

const STATE: &str = "Get ( LastError ) & \"|\" & Get ( FoundCount )";

fn scripts() -> Vec<Script> {
    vec![
        fixture::script(
            1,
            "Round trip",
            vec![
                fixture::on(86, true),
                path_to("om-gui-transfer.tab"),
                transfer::export(&["$p"], &[NAME, COUNT, DUE], FileType::TabSeparated),
                set_variable("$e", "Get ( LastError )"),
                transfer::import(&["$p"], &[NAME, COUNT, DUE], true),
                show(&format!("\"round trip \" & $e & \"/\" & {STATE}")),
            ],
        ),
        fixture::script(
            2,
            "Import rules",
            vec![
                fixture::on(86, true),
                path_to("om-gui-rules.tab"),
                transfer::import(&["$p"], &[NAME, CREATOR, CODE, TAGS], true),
                show(&format!("\"rules \" & {STATE}")),
            ],
        ),
        fixture::script(
            3,
            "Import without auto-enter",
            vec![
                fixture::on(86, true),
                path_to("om-gui-plain.tab"),
                transfer::import(&["$p"], &[NAME], false),
                show(&format!("\"plain \" & {STATE}")),
            ],
        ),
        fixture::script(
            4,
            "Import in a transaction",
            vec![
                fixture::on(86, true),
                path_to("om-gui-tx.tab"),
                ScriptStep::new(205),
                fixture::go_to_record(1),
                fixture::set_field(NAME, "\"Edited in tx\""),
                transfer::import(&["$p"], &[NAME, CREATOR, CODE], false),
                set_variable("$i", "Get ( LastError )"),
                ScriptStep::new(206),
                show("\"transaction \" & $i & \"/\" & Get ( LastError )"),
            ],
        ),
        fixture::script(
            5,
            "Update found set",
            vec![
                fixture::on(86, true),
                path_to("om-gui-match.tab"),
                transfer::import_as(&["$p"], &[NAME, COUNT], false, ImportAction::UpdateFoundSet),
                show(&format!("\"updated \" & {STATE}")),
            ],
        ),
        fixture::script(
            6,
            "Export dialog",
            vec![
                fixture::on(86, true),
                path_to("om-gui-dialog.tab"),
                with_dialog(transfer::export(
                    &["$p"],
                    &[NAME, COUNT],
                    FileType::TabSeparated,
                )),
            ],
        ),
        fixture::script(
            7,
            "Import dialog",
            vec![
                fixture::on(86, true),
                path_to("om-gui-dialog-in.tab"),
                with_dialog(transfer::import(&["$p"], &[NAME, COUNT], true)),
                show(&format!("\"dialog \" & {STATE}")),
            ],
        ),
    ]
}

/// The Tasks file with Tags, and Code validated by calculation only during
/// data entry, hosted.
fn host(name: &str) -> (hosted::Hosted, fixture::TempFile) {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_field(&mut root, TABLE, TAGS, "Tags", 1, 1, 0, 0, 0);
    fixture::set_field_repetitions(&mut root, TABLE, TAGS, 3);
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x001);
    let file = fixture::TempFile::from_tree(&format!("gui-transfer-{name}"), &root);
    let hosted = hosted::host_file(
        &format!("transfer-{name}"),
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    (hosted, file)
}

fn open(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    app
}

fn run(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
}

fn wait_idle(app: &mut common::App) {
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
}

/// Run `script`, wait for its dialog to show `expected`, and close it.
fn expect(app: &mut common::App, script: &str, expected: &str) {
    run(app, script);
    common::wait_for_label(app, expected);
    common::click(app, "OK");
    wait_idle(app);
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there (the
/// dialogs, for review).
fn shot(app: &mut common::App, name: &str) {
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(Path::new(&dir).join(format!("{name}.png")))
        .expect("write png");
}

/// The process's temporary folder, as `Get ( TemporaryPath )` names it.
fn temporary() -> PathBuf {
    std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id()))
}

fn source(name: &str, text: &str) {
    std::fs::create_dir_all(temporary()).unwrap();
    std::fs::write(temporary().join(name), text).unwrap();
}

/// A record's cells by (field, repetition), as text.
type Cells = std::collections::BTreeMap<(u64, u32), String>;

/// Tasks' records as another client reads them.
fn records(hosted: &hosted::Hosted) -> Vec<Cells> {
    let mut other = hosted.opened();
    let ids = other
        .calc_data(&[CalcNeed::Records { table: TABLE }])
        .unwrap()
        .tables[&TABLE]
        .to_vec();
    let needs: Vec<CalcNeed> = ids
        .iter()
        .map(|&record| CalcNeed::Record {
            table: TABLE,
            record,
        })
        .collect();
    let data = other.calc_data(&needs).unwrap();
    let locale = om_data::calc::Locale::en_us();
    let cx = om_data::calc::Ctx::new(
        &locale,
        om_data::calc::Date::from_ymd(2026, 10, 29).unwrap(),
    );
    let out = ids
        .iter()
        .map(|id| {
            let mut cells = Cells::new();
            if let Some(Some(values)) = data.records.get(&(TABLE, *id)) {
                for field in [NAME, COUNT, DUE, CREATOR, CODE, TAGS] {
                    for rep in 1..=3 {
                        if let Some(v) = values.get(field, rep) {
                            let text = v.to_text(&cx).into_string();
                            if !text.is_empty() {
                                cells.insert((field, rep), text);
                            }
                        }
                    }
                }
            }
            cells
        })
        .collect();
    other.close();
    out
}

fn named<'a>(records: &'a [Cells], name: &str) -> Vec<&'a Cells> {
    records
        .iter()
        .filter(|r| r.get(&(NAME, 1)).map(String::as_str) == Some(name))
        .collect()
}

fn cell(record: &Cells, field: u64, rep: u32) -> Option<&str> {
    record.get(&(field, rep)).map(String::as_str)
}

#[test]
fn a_script_exports_the_found_set_and_imports_it_back() {
    let (hosted, _file) = host("round-trip");
    let mut app = open(&hosted);
    expect(&mut app, "Round trip", "round trip 0/0|2");
    let written = std::fs::read(temporary().join("om-gui-transfer.tab")).unwrap();
    assert_eq!(written, b"Write tests\t3\t10/1/2026\rShip\t5\t\r");
    let records = records(&hosted);
    // The number and date fields imported; Creator (not mapped) took its
    // auto-entered value.
    let copies = named(&records, "Write tests");
    assert_eq!(copies.len(), 2);
    assert_eq!(cell(copies[1], COUNT, 1), Some("3"));
    assert_eq!(cell(copies[1], DUE, 1), Some("10/1/2026"));
    assert_eq!(cell(copies[1], CREATOR, 1), Some("Admin"));
    assert_eq!(cell(named(&records, "Ship")[1], COUNT, 1), Some("5"));
}

#[test]
fn imports_keep_their_own_rules() {
    let (hosted, _file) = host("rules");
    source(
        "om-gui-rules.tab",
        "Alpha\tAlice\ttoo long\ta\u{1d}b\u{1d}c\r",
    );
    source("om-gui-plain.tab", "Beta\r");
    let mut app = open(&hosted);
    // A field whose auto-entered value prohibits modification takes the
    // imported value, "only during data entry" validation is not checked,
    // and every repetition is written.
    expect(&mut app, "Import rules", "rules 0|1");
    let records = records(&hosted);
    let alpha = named(&records, "Alpha")[0];
    assert_eq!(cell(alpha, CREATOR, 1), Some("Alice"));
    assert_eq!(cell(alpha, CODE, 1), Some("too long"));
    assert_eq!(
        [1, 2, 3].map(|rep| cell(alpha, TAGS, rep)),
        [Some("a"), Some("b"), Some("c")]
    );
    // Auto-enter off: Creator stays empty.
    expect(&mut app, "Import without auto-enter", "plain 0|1");
    let records = self::records(&hosted);
    assert_eq!(cell(named(&records, "Beta")[0], CREATOR, 1), None);
}

#[test]
fn an_update_of_the_found_set_writes_number_fields() {
    let (hosted, _file) = host("update");
    source("om-gui-match.tab", "Write again\t30\rShip again\t50\r");
    let mut app = open(&hosted);
    // Row n into the found set's record n, a number field too.
    expect(&mut app, "Update found set", "updated 0|2");
    let records = records(&hosted);
    assert_eq!(
        cell(named(&records, "Write again")[0], COUNT, 1),
        Some("30")
    );
    assert_eq!(cell(named(&records, "Ship again")[0], COUNT, 1), Some("50"));
}

#[test]
fn imports_inside_a_transaction_keep_their_rules_at_commit() {
    let (hosted, _file) = host("transaction");
    source("om-gui-tx.tab", "Gamma\t\ttoo long\r");
    let mut app = open(&hosted);
    expect(&mut app, "Import in a transaction", "transaction 0/0");
    let records = records(&hosted);
    // Auto-enter off and the import's validation hold at Commit
    // Transaction; the script's own edit before the import commits too.
    let gamma = named(&records, "Gamma")[0];
    assert_eq!(cell(gamma, CREATOR, 1), None);
    assert_eq!(cell(gamma, CODE, 1), Some("too long"));
    assert_eq!(named(&records, "Edited in tx").len(), 1);
}

#[test]
fn the_export_dialog_writes_what_the_user_chose() {
    let (hosted, _file) = host("export-dialog");
    let mut app = open(&hosted);
    run(&mut app, "Export dialog");
    common::wait_for_label(&mut app, "Export Records");
    shot(&mut app, "export-dialog");
    common::click(&mut app, "Export");
    wait_idle(&mut app);
    let written = std::fs::read(temporary().join("om-gui-dialog.tab")).unwrap();
    assert_eq!(written, b"Write tests\t3\rShip\t5\r");
}

/// Drive the File menu, not a stored export step. The second window omits
/// task 1; export its field order/encoding, then the first window's full
/// found set, so neither the last provider cursor nor another window wins.
#[test]
fn the_file_menu_exports_each_windows_found_set_with_the_chosen_order_and_encoding() {
    let (hosted, _file) = host("menu-export");
    let mut app = open(&hosted);
    let output = tempfile::tempdir().unwrap();
    common::open_menu(&mut app, "Window");
    common::click(&mut app, "New Window");
    wait_idle(&mut app);
    open_front_menu(&mut app, "Records");
    common::click(&mut app, "Omit Record");
    wait_idle(&mut app);

    for (name, expected) in [
        ("subset.csv", "\"5\",\"Ship\"\r"),
        ("all.csv", "\"3\",\"Write tests\"\r\"5\",\"Ship\"\r"),
    ] {
        open_front_menu(&mut app, "File");
        common::click(&mut app, "Export Records...");
        common::wait_for_label(&mut app, "Export Records");
        common::click(&mut app, "Type:");
        common::click(&mut app, "Comma-Separated Values");
        common::click(&mut app, "Output file character set:");
        common::click(&mut app, "Unicode (UTF-16)");
        // This fixture has no layout fields: choose Name then Count,
        // and move Name below Count with the shared order controls.
        common::click(&mut app, "Name");
        common::click(&mut app, "Move");
        common::click(&mut app, "Count");
        common::click(&mut app, "Move");
        common::click(&mut app, "Down");
        let path = output.path().join(name);
        replace_path(&mut app, &path);
        common::click(&mut app, "Export");
        wait_idle(&mut app);
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        let encoded: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(expected.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(std::fs::read(&path).unwrap(), encoded);
        open_front_menu(&mut app, "Window");
        common::click(&mut app, "Tasks (127.0.0.1)");
    }
}

/// Embedded secondary windows repeat the menu bar in the same AccessKit
/// tree. Scope to the front window; the root window's buttons come first.
fn open_front_menu(app: &mut common::App, menu: &str) {
    let title = app.state().window_name();
    if title == "Tasks (127.0.0.1)" {
        app.query_all(By::new().role(egui::Role::Button).label(menu))
            .next()
            .unwrap()
            .click();
    } else {
        app.get_by_role_and_label(egui::Role::Window, &title)
            .get_by_role_and_label(egui::Role::Button, menu)
            .click();
    }
    app.run_steps(2);
}

fn replace_path(app: &mut common::App, path: &Path) {
    common::text_field(app, "Save as:").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(app, "Save as:").type_text(path.to_str().unwrap());
    app.run_steps(2);
}

#[test]
fn cancelling_a_menu_export_neither_creates_nor_replaces_a_file() {
    let (hosted, _file) = host("menu-cancel");
    let mut app = open(&hosted);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("cancel.tab");
    for existing in [false, true] {
        if existing {
            std::fs::write(&path, b"keep this file").unwrap();
        }
        common::open_menu(&mut app, "File");
        common::click(&mut app, "Export Records...");
        common::wait_for_label(&mut app, "Export Records");
        replace_path(&mut app, &path);
        common::click(&mut app, "Move All");
        if existing {
            app.key_press(egui::Key::Escape);
        } else {
            common::click(&mut app, "Cancel");
        }
        wait_idle(&mut app);
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        if existing {
            assert_eq!(std::fs::read(&path).unwrap(), b"keep this file");
        } else {
            assert!(!path.exists());
        }
    }
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::wait_for_label(&mut app, "Perform Find");
    common::open_menu(&mut app, "File");
    assert!(app
        .get_by_label("Export Records...")
        .accesskit_node()
        .is_disabled());
}

#[test]
fn the_import_dialog_maps_fields_and_shows_the_summary() {
    let (hosted, _file) = host("import-dialog");
    source("om-gui-dialog-in.tab", "Grace\t85\r");
    let mut app = open(&hosted);
    run(&mut app, "Import dialog");
    common::wait_for_label(&mut app, "Specify Import Order");
    shot(&mut app, "import-dialog");
    common::click(&mut app, "Import");
    common::wait_for_label(&mut app, "Import Summary");
    shot(&mut app, "import-summary");
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "dialog 0|1");
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    let records = records(&hosted);
    assert_eq!(cell(named(&records, "Grace")[0], COUNT, 1), Some("85"));
}

fn menu_import(app: &mut common::App) {
    open_front_menu(app, "File");
    common::click(app, "Import Records");
    common::click(app, "File...");
    common::wait_for_label(app, "Import Records from File");
}

fn import_source(app: &mut common::App, path: &Path, kind: &str) {
    common::fill(app, "Source file:", path.to_str().unwrap());
    common::click(app, "File type:");
    common::click(app, kind);
}

fn map_name_count(app: &mut common::App) {
    common::wait_for_label(app, "Specify Import Order");
    for (i, field) in ["Name", "Count"].iter().enumerate() {
        common::click(app, &format!("Target field for source column {}", i + 1));
        app.get_by_role_and_label(egui::Role::Button, field).click();
        app.run_steps(2);
    }
}

fn finish_import(app: &mut common::App) {
    common::click(app, "Import");
    common::wait_for_label(app, "Import Summary");
    common::click(app, "OK");
    wait_idle(app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn menu_import_adds_explicitly_encoded_csv_and_a_workbook_through_shared_mapping() {
    let (hosted, _file) = host("menu-import-add");
    let mut app = open(&hosted);
    let files = tempfile::tempdir().unwrap();
    let csv = files.path().join("input.csv");
    let bytes = b"Name,Count\rJos\xe9,81\r";
    std::fs::write(&csv, bytes).unwrap();
    menu_import(&mut app);
    import_source(&mut app, &csv, "Comma-Separated Values");
    common::click(&mut app, "Source character set:");
    common::click(&mut app, "Windows ANSI");
    common::click(&mut app, "Continue");
    map_name_count(&mut app);
    common::click(&mut app, "Don't import first record (contains field names)");
    finish_import(&mut app);
    let stored = records(&hosted);
    assert_eq!(stored.len(), 3);
    assert_eq!(cell(named(&stored, "José")[0], COUNT, 1), Some("81"));
    assert_eq!(std::fs::read(&csv).unwrap(), bytes);

    // Produce a synthetic workbook with the existing exporter, then use
    // the source chooser's XLSX profile (no extension-based guessing).
    let workbook = files.path().join("source.xlsx");
    open_front_menu(&mut app, "File");
    common::click(&mut app, "Export Records...");
    common::wait_for_label(&mut app, "Export Records");
    common::click(&mut app, "Type:");
    common::click(&mut app, "Excel Workbooks (.xlsx)");
    for field in ["Name", "Count"] {
        common::click(&mut app, field);
        common::click(&mut app, "Move");
    }
    replace_path(&mut app, &workbook);
    common::click(&mut app, "Export");
    wait_idle(&mut app);
    menu_import(&mut app);
    import_source(&mut app, &workbook, "Excel Workbooks (.xlsx)");
    common::click(&mut app, "Continue");
    map_name_count(&mut app);
    finish_import(&mut app);
    let stored = records(&hosted);
    assert_eq!(stored.len(), 4);
    assert_eq!(named(&stored, "José").len(), 2);
    assert!(named(&stored, "José")
        .iter()
        .all(|r| cell(r, COUNT, 1) == Some("81")));
}

#[test]
fn menu_matching_uses_separate_keys_and_the_owning_windows_found_set() {
    let (hosted, _file) = host("menu-import-match");
    let mut app = open(&hosted);
    let files = tempfile::tempdir().unwrap();
    let csv = files.path().join("matching.csv");
    std::fs::write(&csv, "Write tests,99\rship,17\r").unwrap();
    common::open_menu(&mut app, "Window");
    common::click(&mut app, "New Window");
    wait_idle(&mut app);
    open_front_menu(&mut app, "Records");
    common::click(&mut app, "Omit Record");
    wait_idle(&mut app);
    let owner = app.state().window_name();
    menu_import(&mut app);
    import_source(&mut app, &csv, "Comma-Separated Values");
    common::click(&mut app, "Continue");
    map_name_count(&mut app);
    common::click(&mut app, "Update matching records in found set");
    assert!(app.get_by_label("Import").accesskit_node().is_disabled());
    app.query_all(By::new().label("Match key"))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    // A key-only stored mapping remains usable (including add remaining).
    app.query_all(By::new().label("Match key"))
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    assert!(!app.get_by_label("Import").accesskit_node().is_disabled());
    app.query_all(By::new().label("Match key"))
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    finish_import(&mut app);
    assert_eq!(app.state().window_name(), owner);
    let stored = records(&hosted);
    assert_eq!(stored.len(), 2);
    assert_eq!(cell(named(&stored, "Write tests")[0], COUNT, 1), Some("3"));
    assert_eq!(cell(named(&stored, "Ship")[0], COUNT, 1), Some("17"));
    assert!(
        named(&stored, "ship").is_empty(),
        "match keys are not overwritten"
    );
}

#[test]
fn menu_source_mapping_cancellation_and_parse_refusal_leave_records_unchanged() {
    let (hosted, _file) = host("menu-import-cancel");
    let mut app = open(&hosted);
    let before = records(&hosted);
    let files = tempfile::tempdir().unwrap();
    let source = files.path().join("source.tab");
    let bytes = b"Unwritten\t44\r";
    std::fs::write(&source, bytes).unwrap();
    menu_import(&mut app);
    app.key_press(egui::Key::Escape);
    wait_idle(&mut app);
    menu_import(&mut app);
    import_source(&mut app, &source, "Tab-Separated Values");
    common::click(&mut app, "Continue");
    map_name_count(&mut app);
    common::click(&mut app, "Cancel");
    wait_idle(&mut app);
    assert_eq!(records(&hosted), before);
    // Explicit XLSX profile must fail, even though the path says .tab.
    menu_import(&mut app);
    import_source(&mut app, &source, "Excel Workbooks (.xlsx)");
    common::click(&mut app, "Continue");
    common::wait_until(&mut app, "parse refusal", |app| {
        app.state().error().is_some()
    });
    assert!(app.query_by_label("Specify Import Order").is_none());
    assert!(app.query_by_label("Import Summary").is_none());
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    assert_eq!(records(&hosted), before);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::wait_for_label(&mut app, "Perform Find");
    common::open_menu(&mut app, "File");
    assert!(app
        .get_by_label("Import Records")
        .accesskit_node()
        .is_disabled());
}
