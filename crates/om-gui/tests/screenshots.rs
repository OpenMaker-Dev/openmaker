//! Render the main screens to PNGs under `crates/om-gui/screenshots/` so the
//! chrome can be reviewed on a machine without a display and compared with
//! FileMaker's in `re/captures/fm-ui/`. Rendering is offscreen through wgpu
//! (on Linux, Mesa's lavapipe software Vulkan is enough).
//!
//! Regenerate with: `cargo test -p om-gui --test screenshots`

mod common;

use std::path::PathBuf;

use common::{
    click, connect_to_mock, fill, harness, mock, open_menu, select_layout, wait_for_label,
    wait_for_value, App,
};
use egui_kittest::kittest::Queryable;

fn save(app: &mut App, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("screenshots");
    std::fs::create_dir_all(&dir).expect("create screenshots dir");
    // Popups and modals spend their first frame on a sizing pass.
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).expect("write png");
    eprintln!("wrote {}", path.display());
}

/// Click somewhere neutral to close an open menu or popup.
fn dismiss(app: &mut App) {
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
}

#[test]
fn screenshots() {
    // Without Home, Open Network File is the window's launcher.
    let mut app = mock();
    app.run_steps(3);
    save(&mut app, "01-open-network-file");
    fill(&mut app, "Path", "fmnet:/fms.example.com/Contacts");
    fill(&mut app, "Account Name", "admin");
    fill(&mut app, "Password", "secret");
    save(&mut app, "02-open-network-file-filled");

    let mut app = mock();
    connect_to_mock(&mut app);
    // The generic Form view, for a layout without geometry.
    select_layout(&mut app, "Contacts List");
    wait_for_value(&mut app, "First0");
    save(&mut app, "03-browse-form");
    select_layout(&mut app, "Contact Details");
    wait_for_label(&mut app, "Body");

    // Move off the first record so the current-record highlight shows.
    click(&mut app, "Next");
    click(&mut app, "Next");
    click(&mut app, "List View");
    save(&mut app, "04-browse-list");

    click(&mut app, "Table View");
    save(&mut app, "05-browse-table");

    click(&mut app, "Form View");
    click(&mut app, "Layout");
    save(&mut app, "06-layout-popup");
    dismiss(&mut app);

    open_menu(&mut app, "File");
    save(&mut app, "07-file-menu");
    click(&mut app, "Save/Send Records As");
    save(&mut app, "07-save-send-records-as");
    dismiss(&mut app);
    click(&mut app, "Share");
    save(&mut app, "07-share-menu");
    dismiss(&mut app);

    open_menu(&mut app, "View");
    save(&mut app, "08-view-menu");
    dismiss(&mut app);

    open_menu(&mut app, "Records");
    save(&mut app, "09-records-menu");
    dismiss(&mut app);

    click(&mut app, "Find");
    save(&mut app, "10-find-mode");
    click(&mut app, "Cancel Find");

    click(&mut app, "Edit Layout");
    save(&mut app, "11-layout-mode");
    click(&mut app, "Exit Layout");

    click(&mut app, "Preview");
    save(&mut app, "12-preview-mode");
    click(&mut app, "Exit Preview");

    click(&mut app, "Show/Hide Text Formatting");
    click(&mut app, "Sort");
    common::wait_for_label(&mut app, "Sort Records");
    save(&mut app, "13-sort-records");
    click(&mut app, "Cancel");

    let mut app = harness(Box::new(common::FailingProvider));
    fill(&mut app, "Path", "fmnet:/fms.example.com/Contacts.fmp12");
    click(&mut app, "Open");
    wait_for_label(
        &mut app,
        "The account name and password you entered could not be authenticated.",
    );
    save(&mut app, "14-open-error");
    assert!(app.query_by_label("OK").is_some());

    // Form view drawn from the layout's geometry: part bands and object boxes.
    let mut app = mock();
    connect_to_mock(&mut app);
    save(&mut app, "15-browse-form-layout");
    click(&mut app, "Edit Layout");
    click(&mut app, "Objects");
    click(&mut app, "Field - First Name");
    save(&mut app, "16-layout-inspector");
    click(&mut app, "Data");
    save(&mut app, "17-layout-data");
    click(&mut app, "Manage");
    click(&mut app, "Layouts...");
    save(&mut app, "18-manage-layouts");

    // Calculations: the unstored Full Name, the hide condition (record 1
    // shows "Preferred contact") and conditional formatting (record 2).
    let mut app = mock();
    connect_to_mock(&mut app);
    select_layout(&mut app, "Contact Card");
    wait_for_label(&mut app, "First0 Last0");
    save(&mut app, "19-calculations");
    click(&mut app, "Next");
    wait_for_label(&mut app, "First1 Last1");
    save(&mut app, "20-conditional-formatting");
}

/// Where the Script Workspace renders go: `OM_UI_CAPTURES` (comparisons
/// keep them beside FileMaker's captures, in `re/captures/om-ui/`), else
/// `screenshots/`.
fn workspace_dir() -> PathBuf {
    std::env::var_os("OM_UI_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("screenshots"))
}

fn save_to(app: &mut App, dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).expect("create the captures dir");
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).expect("write png");
    eprintln!("wrote {}", path.display());
}

/// The Script Workspace as captures 35 and 35b show it: the Contacts
/// template's scripts, Go to Entry Field open, at FileMaker's window size.
#[test]
fn script_workspace_screenshots() {
    use common::workspace::{contacts, Provider, State};
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(State {
        scripts: contacts(),
        ..State::default()
    }));
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1672.0, 900.0))
        .renderer(common::renderer())
        .build_eframe(move |_cc| om_gui::OpenMakerApp::new(Box::new(Provider(state))));
    fill(&mut app, "Path", "fmnet:/fmhost/test project.fmp12");
    click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        app.query_by_label("Scripts").is_some()
    });
    open_menu(&mut app, "Scripts");
    click(&mut app, "Script Workspace...");
    let first = "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]";
    wait_for_label(&mut app, first);
    let dir = workspace_dir();
    save_to(&mut app, &dir, "35-script-workspace");
    // 35b: step 1 selected, its gear showing.
    click(&mut app, first);
    save_to(&mut app, &dir, "35b-script-workspace-selected-step");
    click(&mut app, "Step Options");
    common::wait_for_label(&mut app, "\"Go to Object\" Options");
    save_to(&mut app, &dir, "35b-script-workspace-go-to-object-options");
    click(&mut app, "Cancel");
    // The steps pane open on Control, with a step's description.
    click(&mut app, "Control");
    click(&mut app, "Set Variable");
    save_to(&mut app, &dir, "35c-script-workspace-steps-pane");
    // The unsaved-changes prompt.
    click(&mut app, "Line 2: Select All");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    app.run_steps(2);
    click(&mut app, "Close Window");
    wait_for_label(&mut app, "Save changes to script “Go to Entry Field”?");
    save_to(&mut app, &dir, "35d-script-workspace-save-changes");
}

/// A paste of FileMaker's XML from another file: a step that reads, one
/// naming a field this file lacks and one that does not map (both kept as
/// marked disabled comments).
#[test]
fn script_workspace_paste_screenshot() {
    use common::workspace::{contacts, Provider, State};
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(State {
        scripts: contacts(),
        ..State::default()
    }));
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1672.0, 900.0))
        .renderer(common::renderer())
        .build_eframe(move |_cc| om_gui::OpenMakerApp::new(Box::new(Provider(state))));
    fill(&mut app, "Path", "fmnet:/fmhost/test project.fmp12");
    click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        app.query_by_label("Scripts").is_some()
    });
    open_menu(&mut app, "Scripts");
    click(&mut app, "Script Workspace...");
    wait_for_label(&mut app, "Line 2: Select All");
    click(&mut app, "Line 2: Select All");
    app.event(egui::Event::Paste(
        r#"<fmxmlsnippet type="FMObjectList"><Step enable="True" id="93" name="Beep"></Step><Step enable="True" id="17" name="Go to Field"><Options>1</Options><ParameterValues membercount="1"><Parameter type="FieldReference"><FieldReference id="130" name="Nickname"><TableOccurrenceReference id="1065089" name="Contacts"></TableOccurrenceReference></FieldReference></Parameter></ParameterValues></Step><Step enable="True" id="93" name="Beep"><Sound name="Glass"></Sound></Step></fmxmlsnippet>"#
            .into(),
    ));
    app.run_steps(3);
    wait_for_label(&mut app, "Line 3: Beep");
    // Line 1 selected, so the marked lines show their tint. The rows are
    // as wide as the longest line (the comment holding a step's XML), so
    // click near the line's start rather than its row's centre.
    let row = app
        .get_by_label("Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]")
        .rect();
    let at = egui::pos2(row.left() + 120.0, row.center().y);
    let input = app.input_mut();
    input.events.push(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        input.events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    app.run_steps(3);
    save_to(
        &mut app,
        &workspace_dir(),
        "35j-script-workspace-pasted-xml",
    );
}

/// The step options' own dialogs: Specify Field over Set Field's options,
/// Edit Find Request over Perform Find's requests, and Sort Records' sort
/// order, on the Contacts template's scripts.
#[test]
fn script_workspace_option_dialogs_screenshots() {
    use common::workspace::{contacts, Provider, State};
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(State {
        scripts: contacts(),
        ..State::default()
    }));
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1672.0, 900.0))
        .renderer(common::renderer())
        .build_eframe(move |_cc| om_gui::OpenMakerApp::new(Box::new(Provider(state))));
    fill(&mut app, "Path", "fmnet:/fmhost/test project.fmp12");
    click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        app.query_by_label("Scripts").is_some()
    });
    open_menu(&mut app, "Scripts");
    click(&mut app, "Script Workspace...");
    wait_for_label(&mut app, "Line 2: Select All");
    let dir = workspace_dir();
    // New steps after Select All, typed by name.
    let type_step = |app: &mut App, name: &str| {
        click(app, "Line 2: Select All");
        app.key_press(egui::Key::Enter);
        app.run_steps(2);
        common::text_field(app, "New Step").type_text(name);
        app.run_steps(2);
        app.key_press(egui::Key::Enter);
        app.run_steps(2);
    };
    type_step(&mut app, "Set Field");
    type_step(&mut app, "Perform Find");
    wait_for_label(&mut app, "Line 4: Set Field [ ]");

    click(&mut app, "Line 4: Set Field [ ]");
    click(&mut app, "Step Options");
    wait_for_label(&mut app, "\"Set Field\" Options");
    click(&mut app, "Specify Field…");
    wait_for_label(&mut app, "Specify Field");
    click(&mut app, "Last Name");
    save_to(&mut app, &dir, "35e-script-workspace-specify-field");
    dismiss(&mut app);
    dismiss(&mut app);

    click(&mut app, "Line 3: Perform Find [ ]");
    click(&mut app, "Step Options");
    wait_for_label(&mut app, "\"Perform Find\" Options");
    click(&mut app, "Specify Find Requests…");
    wait_for_label(&mut app, "Specify Find Requests");
    click(&mut app, "New");
    wait_for_label(&mut app, "Edit Find Request");
    click(&mut app, "Last Name");
    common::text_field(&app, "Criteria").focus();
    common::text_field(&app, "Criteria").type_text("Lovelace");
    app.run_steps(2);
    click(&mut app, "Add");
    save_to(&mut app, &dir, "35f-script-workspace-edit-find-request");
    // Return answers the dialog on top (three show an OK).
    app.key_press(egui::Key::Enter);
    app.run_steps(2);
    wait_for_label(&mut app, "Find Records Contacts::Last Name: [Lovelace]");
    save_to(&mut app, &dir, "35g-script-workspace-specify-find-requests");
    dismiss(&mut app);
    dismiss(&mut app);

    click(&mut app, "Sort by Name");
    let line = "Line 1: Sort Records [ With dialog: On ]";
    wait_for_label(&mut app, line);
    click(&mut app, line);
    click(&mut app, "Step Options");
    wait_for_label(&mut app, "\"Sort Records\" Options");
    save_to(&mut app, &dir, "35h-script-workspace-sort-records-options");
    click(&mut app, "Specify Sort Order…");
    wait_for_label(&mut app, "Sort Records");
    for field in ["Last Name", "First Name"] {
        click(&mut app, field);
        click(&mut app, "Move");
    }
    click(&mut app, "Contacts::Last Name");
    save_to(&mut app, &dir, "35i-script-workspace-sort-records");
}

/// Specify Calculation (42) for Go to Entry Field's line 1: a double click
/// on the step's calculation opens it, at FileMaker's window size.
#[test]
fn specify_calculation_screenshot() {
    use common::workspace::{contacts, double_click_at, step_text_position, Provider, State};
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(State {
        scripts: contacts(),
        ..State::default()
    }));
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1672.0, 900.0))
        .renderer(common::renderer())
        .build_eframe(move |_cc| om_gui::OpenMakerApp::new(Box::new(Provider(state))));
    fill(&mut app, "Path", "fmnet:/fmhost/test project.fmp12");
    click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        app.query_by_label("Scripts").is_some()
    });
    open_menu(&mut app, "Scripts");
    click(&mut app, "Script Workspace...");
    let first = "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]";
    wait_for_label(&mut app, first);
    let at = step_text_position(&app, first, "Go to Object [ Object Name: ");
    double_click_at(&mut app, at);
    wait_for_label(&mut app, "Specify Calculation");
    save_to(&mut app, &workspace_dir(), "42-specify-calculation");
}

/// Home with a sample history in a settings file of its own, at FileMaker
/// Pro 26.1.1's Home window size (1101 by 697 points below the title bar)
/// plus the in-window menu bar, to compare side by side with FileMaker's.
#[test]
fn home_screenshots() {
    use egui_kittest::Harness;

    let start = |settings: PathBuf| -> App {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let provider = om_provider::NativeProvider::new(om_provider::NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: Vec::new(),
            callback_host: "127.0.0.1".into(),
            machine: om_protocol::MachineInfo::default(),
        });
        let mut app = Harness::builder()
            .with_size(egui::vec2(1101.0, 697.0 + 24.0))
            .renderer(common::renderer())
            .build_eframe(move |_| {
                om_gui::OpenMakerApp::with_license_store(
                    Box::new(provider),
                    Box::new(common::NoLicense),
                )
                .with_settings_file(settings)
                .with_home()
            });
        app.run_steps(3);
        app
    };
    let dir = tempfile::tempdir().unwrap();
    let mut empty = start(dir.path().join("empty.json"));
    save(&mut empty, "home-empty");

    let settings = dir.path().join("settings.json");
    let local = |name: &str| {
        serde_json::json!({
            "kind": "local",
            "path": format!("/home/demo/Documents/OpenMaker/{name}.fmp12"),
        })
    };
    let hosted = |host: &str, port: u16, file: &str| serde_json::json!({ "kind": "hosted", "host": host, "port": port, "file": file });
    let history = serde_json::json!({
        "file_history": {
            "tab": "recent",
            "recent": [
                local("Inventory"),
                hosted("studio.example.test", 5003, "Projects.fmp12"),
                local("Contacts"),
                hosted("archive.example.test", 5004, "2025 Archive.fmp12"),
                local("Assets with a name long enough to be cut at the column's end"),
            ],
            "favorites": [
                local("Inventory"),
                hosted("studio.example.test", 5003, "Projects.fmp12"),
            ],
        }
    });
    std::fs::write(&settings, history.to_string()).unwrap();
    let mut app = start(settings);
    save(&mut app, "home-recent");
    click(&mut app, "Contacts");
    save(&mut app, "home-recent-selected");
    app.get_by_label("Projects").click_secondary();
    app.run_steps(3);
    save(&mut app, "home-context-menu");
    dismiss(&mut app);
    click(&mut app, "Favorites");
    save(&mut app, "home-favorites");
}

/// Home's saved hosts on the mock provider, which lists its file for any
/// host: a host's page, its context menu, its sign-in, Add Host, Open
/// Network File, and Hosts with nothing saved. Sample data keeps Home's
/// lists in memory, so the hosts are added as a user adds them.
#[test]
fn home_hosts_screenshots() {
    use egui_kittest::Harness;

    common::use_svg_worker();
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let mut app = Harness::builder()
        .with_size(egui::vec2(1101.0, 697.0 + 24.0))
        .renderer(common::renderer())
        .build_eframe(move |_| {
            om_gui::OpenMakerApp::new(Box::new(om_data::mock::MockProvider::new()))
                .with_settings_file(settings)
                .with_home()
        });
    app.run_steps(3);
    open_menu(&mut app, "File");
    click(&mut app, "Hosts");
    click(&mut app, "Show Hosts...");
    save(&mut app, "home-hosts-empty");
    click(&mut app, "Add Host...");
    fill(&mut app, "Address", "staging.example.com");
    common::replace(&mut app, "Port", "5004");
    fill(&mut app, "Name", "Staging");
    save(&mut app, "home-add-host");
    click(&mut app, "Add");
    for (address, name) in [("fms.example.com", "Production"), ("files.example.org", "")] {
        click(&mut app, "Add Host");
        fill(&mut app, "Address", address);
        fill(&mut app, "Name", name);
        click(&mut app, "Add");
    }
    click(&mut app, "Production");
    wait_for_label(&mut app, "Contacts");
    click(&mut app, "Contacts");
    save(&mut app, "home-host");
    app.get_by_label("Staging").click_secondary();
    app.run_steps(3);
    save(&mut app, "home-host-menu");
    dismiss(&mut app);
    fill(&mut app, "Search Files", "Payroll");
    save(&mut app, "home-host-no-match");
    click(&mut app, "Open “Payroll”...");
    save(&mut app, "home-open-network-file");
    click(&mut app, "Cancel");
    // Another host's page and back: a page shows with no search.
    click(&mut app, "Staging");
    click(&mut app, "Production");
    wait_for_label(&mut app, "Contacts");
    let at = app.get_by_label("Contacts").rect().center();
    common::workspace::double_click_at(&mut app, at);
    wait_for_label(&mut app, "Open “Contacts”");
    save(&mut app, "home-sign-in");
}
