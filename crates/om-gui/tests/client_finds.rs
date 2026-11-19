//! Finds on fields the host has no index for, through the GUI end to end
//! (script-runtime.md 13.19): scripts run on the provider worker against
//! the native provider and an in-process `om-server` hosting the in-memory
//! Tasks file of `om-server/tests/fixtures/tasks.rs` with its unstored
//! calculations, summaries and global field (`add_calculated`). A criterion
//! on the unstored Label is computed on the client for each candidate; a
//! summary field takes no criterion (201); Set Field into a global field in
//! Find mode sets its value and adds no criterion. Each script shows what
//! it found in a custom dialog.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use fixture::{calculated_calc as calc, LABEL, NAME, OCCURRENCE, SETTING, TOTAL};
use om_scripts::{FieldSpec, Script, ScriptStep, StoredKey};

/// Set Field [Tasks::field; text].
fn set_field(field: u64, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(field).unwrap(),
            0,
        )),
    );
    step
}

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Show Custom Dialog [ "Found" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Found\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// What the dialogs show: the error the criterion's Set Field stored, the
/// find's, the found count and the found records' labels (Labels, a list
/// summary of Label).
const REPORT: &str = "\"set \" & $$set & \", find \" & $$find & \", \" & Get ( FoundCount ) & \
    \" found: \" & Substitute ( Tasks::Labels ; \"¶\" ; \", \" )";

/// Enter Find Mode, the criterion into `field`, a Name criterion when
/// given, Perform Find, the report.
fn find(id: u64, name: &str, field: u64, criterion: &str, also_name: Option<&str>) -> Script {
    let mut steps = vec![
        fixture::on(86, true),
        ScriptStep::new(22),
        set_field(field, criterion),
        set_variable("$$set", "Get ( LastError )"),
    ];
    if let Some(text) = also_name {
        steps.push(set_field(NAME, text));
    }
    steps.extend([
        ScriptStep::new(28),
        set_variable("$$find", "Get ( LastError )"),
        show(REPORT),
    ]);
    fixture::script(id, name, steps)
}

fn scripts() -> Vec<Script> {
    vec![
        find(1, "Find by label", LABEL, "\"ship\"", None),
        find(2, "Find by summary", TOTAL, "\"8\"", Some("\"write\"")),
        find(3, "Find by global", SETTING, "\"x\"", Some("\"ship\"")),
    ]
}

/// The fixture's file (records 1 "Write tests", Count 3, and 2 "Ship", 5)
/// with its calculated fields, hosted.
fn host() -> hosted::Hosted {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_calculated(&mut root);
    let file = fixture::TempFile::from_tree("gui-client-finds", &root);
    hosted::host_file(
        "client-finds",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    )
}

fn run(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
}

/// Run `script`, wait for its dialog to show `expected`, and close it.
fn expect(app: &mut common::App, script: &str, expected: &str) {
    run(app, script);
    common::wait_for_label(app, expected);
    common::click(app, "OK");
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn finds_compute_unstored_calculations_on_the_client() {
    let hosted = host();
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    // Labels: "Write tests (3)", "Ship (5)".
    expect(
        &mut app,
        "Find by label",
        "set 0, find 0, 1 found: Ship (5)",
    );
    // The summary takes no criterion; Name does.
    expect(
        &mut app,
        "Find by summary",
        "set 201, find 0, 1 found: Write tests (3)",
    );
    expect(
        &mut app,
        "Find by global",
        "set 0, find 0, 1 found: Ship (5)",
    );
}
