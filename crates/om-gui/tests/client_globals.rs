//! Global fields through the GUI end to end: scripts run on the provider
//! worker against the native provider and an in-process `om-server`
//! hosting the in-memory Tasks file of `om-server/tests/fixtures/tasks.rs`,
//! whose global Setting is stored as "stored". As FileMaker does for a
//! hosted file (Claris "Global fields"), Set Field gives the global a value
//! of this session's, in Browse and in Find mode (where it is no criterion,
//! script-runtime.md 13.19); the session starts from the stored value,
//! keeps its own across scripts, and neither the host's file nor another
//! client sees it.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use fixture::{calc, OCCURRENCE, SETTING, TABLE};
use om_data::host::CalcNeed;
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
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

/// Set Field By Name [name; text].
fn set_field_by_name(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(147);
    step.flags |= om_scripts::flag::FIELD_SPEC_BY_CALC;
    step.set_calculation(0, Some(&calc(text)));
    step.set_calculation(1, Some(&calc(name)));
    step
}

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Show Custom Dialog [ "Globals" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Globals\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

fn scripts() -> Vec<Script> {
    vec![
        fixture::script(
            1,
            "Set globals",
            vec![
                fixture::on(86, true),
                set_variable("$start", "Tasks::Setting"),
                set_field(SETTING, "\"browse\""),
                set_variable("$browse", "Get ( LastError ) & \":\" & Tasks::Setting"),
                ScriptStep::new(22),
                set_field(SETTING, "\"find\""),
                set_variable("$find", "Get ( LastError )"),
                ScriptStep::new(55),
                show(
                    "\"start \" & $start & \", browse \" & $browse & \", find \" & $find & \
                     \", after \" & Tasks::Setting",
                ),
            ],
        ),
        fixture::script(2, "Read global", vec![show("\"now \" & Tasks::Setting")]),
        fixture::script(
            3,
            "Set by name",
            vec![
                fixture::on(86, true),
                set_field_by_name("\"Tasks::Setting\"", "\"by name\""),
                show("Get ( LastError ) & \" \" & Tasks::Setting"),
            ],
        ),
    ]
}

/// The fixture's file with the global Setting stored as "stored"
/// (`[T, 08, F]`), hosted.
fn host() -> hosted::Hosted {
    let mut root = fixture::tree(&scripts(), true);
    let (t, f) = (uint_segment(TABLE), uint_segment(SETTING));
    root.put(&[&t, &[8], &f], om_scripts::text_node(&[], "stored"));
    let file = fixture::TempFile::from_tree("gui-client-globals", &root);
    hosted::host_file(
        "client-globals",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    )
}

/// Run `script`, wait for its dialog to show `expected`, and close it.
fn expect(app: &mut common::App, script: &str, expected: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, script);
    common::wait_for_label(app, expected);
    common::click(app, "OK");
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Setting as another client's calculations read it.
fn setting_elsewhere(hosted: &hosted::Hosted) -> String {
    let mut other = hosted.opened();
    let data = other
        .calc_data(&[CalcNeed::Globals { table: TABLE }])
        .unwrap();
    let values = &data.globals[&TABLE];
    let text = values.get(SETTING, 1).map(|v| {
        v.to_text(&om_data::calc::Ctx::new(
            &om_data::calc::Locale::en_us(),
            om_data::calc::Date::from_ymd(2026, 10, 29).unwrap(),
        ))
        .into_string()
    });
    other.close();
    text.unwrap_or_default()
}

#[test]
fn scripts_set_global_fields_for_their_session() {
    let hosted = host();
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    // The session starts from the stored value; Set Field sets it in
    // Browse mode and in Find mode (no 201).
    expect(
        &mut app,
        "Set globals",
        "start stored, browse 0:browse, find 0, after find",
    );
    // The value lasts for the session, across scripts.
    expect(&mut app, "Read global", "now find");
    // Set Field By Name sets a global's value as Set Field does (before,
    // 201).
    expect(&mut app, "Set by name", "0 by name");
    // Nothing reached the host: another client starts from the stored
    // value.
    assert_eq!(setting_elsewhere(&hosted), "stored");
}
