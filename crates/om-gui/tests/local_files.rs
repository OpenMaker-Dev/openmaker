//! Opening a FileMaker file from this computer through the desktop window,
//! end to end with no server and no license: File > Open (or the Hosts
//! dialog's Browse), the chooser, the account the file asks for (refusals
//! keeping the file unchanged), Browse mode's edit and save, Close, and
//! opening the file again to find the edit saved in it. The file is a disposable copy of the synthetic Tasks file
//! of `om-server/tests/fixtures/tasks.rs`, with stored passwords; the
//! native provider opens it through its private embedded host. The license
//! store fails the test if anything reads it.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use std::path::{Path, PathBuf};

use common::App;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use om_data::{DataError, DataProvider};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::OpenMakerApp;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};

/// A credential store no local file may touch.
struct Untouchable;

impl LicenseStore for Untouchable {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        panic!("a local file read the license store");
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
        panic!("a local file wrote the license store");
    }
    fn remove(&mut self) -> Result<(), DataError> {
        panic!("a local file changed the license store");
    }
}

/// A clean client: a fresh TLS identity, no CA, no license.
fn clean_provider() -> NativeProvider {
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
    common::use_svg_worker();
    Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .build_eframe(|_cc| {
            OpenMakerApp::with_license_store(Box::new(clean_provider()), Box::new(Untouchable))
        })
}

/// A minimal protobuf writer for the layout's object store
/// (layout-model.md 5; `docs/re/protobuf/LayoutObject.proto`).
fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3, out);
    varint(v, out);
}

fn double(number: u32, v: f64, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 1, out);
    out.extend_from_slice(&v.to_le_bytes());
}

fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

/// Layout 1 "Tasks" drawing Tasks::Name in a 200-point Body part.
fn add_name_field(root: &mut om_hbam::OwnedNode) {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode};
    const NAME_OBJECT: u64 = 1;
    let alias = serial::decode(&uint_segment(fixture::OCCURRENCE)).unwrap();
    let layout = uint_segment(fixture::LAYOUT);
    let mut def = Vec::new();
    uint(2, alias, &mut def);
    double(8, 480.0, &mut def);
    root.put(
        &[&[4], &[5], &layout, &[2]],
        OwnedNode::leaf(Vec::new(), def),
    );
    let mut body = Vec::new();
    uint(1, 4, &mut body);
    double(4, 0.0, &mut body);
    double(5, 200.0, &mut body);
    root.put(
        &[&[4], &[5], &layout, &[3], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), body),
    );
    let mut bounds = Vec::new();
    for (number, value) in [(1, 20.0), (2, 10.0), (3, 40.0), (4, 210.0)] {
        double(number, value, &mut bounds);
    }
    let mut spec = Vec::new();
    uint(1, alias, &mut spec);
    uint(2, fixture::NAME, &mut spec);
    let mut field = Vec::new();
    bytes(2, &spec, &mut field);
    let mut object = Vec::new();
    uint(1, 1, &mut object);
    uint(2, NAME_OBJECT, &mut object);
    bytes(4, &bounds, &mut object);
    bytes(8, &field, &mut object);
    let mut store = Vec::new();
    uint(1, NAME_OBJECT, &mut store);
    bytes(2, &object, &mut store);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
}

/// A folder of its own holding the Tasks file, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("om-gui-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Real files always have a script catalog: one script.
        let scripts = [fixture::script(1, "Greet", Vec::new())];
        let mut root = fixture::tree(&scripts, true);
        fixture::add_local_accounts(&mut root);
        add_name_field(&mut root);
        fixture::write_tree(&dir.join(fixture::FILE), &root);
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join(fixture::FILE)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Replace the text of the field labelled `label`.
fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// Choose `path` in the Open dialog, already showing.
fn choose(app: &mut App, path: &Path) {
    common::wait_for_label(app, "Open File");
    replace(app, "Path", &path.display().to_string());
    common::click(app, "Open");
}

/// Answer the account dialog with `password` for the suggested account.
fn sign_in(app: &mut App, password: &str) {
    common::wait_for_label(app, "Open “Tasks”");
    assert_eq!(
        common::text_field(app, "Account Name").value().as_deref(),
        Some(fixture::ACCOUNT)
    );
    common::fill(app, "Password", password);
    common::click(app, "OK");
}

fn wait_open(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.step();
        if app.state().is_settled() && common::find_text_field(app, "Name").is_some() {
            app.run_steps(3);
            return;
        }
        if std::time::Instant::now() > deadline {
            eprintln!(
                "open {} settled {} error {:?}",
                app.state().is_open(),
                app.state().is_settled(),
                app.state().error()
            );
            eprintln!("{:#?}", app.root());
            panic!("timed out waiting for the file's first layout");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_file_on_this_computer_opens_saves_an_edit_and_reopens_with_it() {
    let folder = Folder::new("edit");
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    wait_open(&mut app);
    // A file on this computer has no host in its title.
    assert_eq!(app.state().window_name(), "Tasks");
    assert_eq!(
        common::text_field(&app, "Name").value().as_deref(),
        Some("Write tests")
    );
    replace(&mut app, "Name", "Edited in the window");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the save", |app| {
        app.state().is_settled() && !common::editing(app)
    });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    // The Hosts dialog's Browse opens the same chooser.
    common::click(&mut app, "Browse...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    wait_open(&mut app);
    assert_eq!(
        common::text_field(&app, "Name").value().as_deref(),
        Some("Edited in the window")
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// New Script in a file on this computer, through the window: named in
/// the scripts pane, given a step and saved, the script is in the file on
/// disk once the window closes it, and in the Scripts menu when the file
/// is opened again, where it runs. Then a deleted script is gone from the
/// file.
#[test]
fn a_new_script_is_saved_in_a_file_on_this_computer_and_runs_after_reopening() {
    use common::workspace::{add_script_index, create_script, scripts_on_disk};
    let folder = Folder::new("new-script");
    let scripts = [fixture::script(1, "Greet", Vec::new())];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_local_accounts(&mut root);
    add_name_field(&mut root);
    add_script_index(&mut root, &scripts, 1);
    fixture::write_tree(&folder.file(), &root);
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    wait_open(&mut app);
    create_script(
        &mut app,
        "Say Built",
        "Show Custom Dialog [ Title: \"Built\" ; Message: \"Made on this computer\" ]\n",
        "Show Custom Dialog",
    );
    common::click(&mut app, "Close Window");
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    let saved = scripts_on_disk(&folder.file());
    let names: Vec<(u64, &str)> = saved
        .scripts
        .iter()
        .map(|s| (s.id, s.name.as_str()))
        .collect();
    assert_eq!(names, [(1, "Greet"), (2, "Say Built")]);
    assert_eq!(
        saved
            .script(2)
            .unwrap()
            .steps
            .iter()
            .map(|s| s.opcode)
            .collect::<Vec<_>>(),
        [87]
    );

    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    wait_open(&mut app);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Say Built");
    common::wait_for_label(&mut app, "Made on this computer");
    common::click(&mut app, "OK");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    // Deleting a script empties it in the file.
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(&mut app, "Greet");
    for _ in 0..20 {
        app.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.get_by_label("Greet").click_secondary();
    common::wait_until(&mut app, "the context menu", |app| {
        app.query_by_label("Delete").is_some()
    });
    common::click(&mut app, "Delete");
    common::wait_for_label(
        &mut app,
        "Are you sure you want to permanently delete “Greet”?",
    );
    app.get_by_role_and_label(egui::Role::Button, "Delete")
        .click();
    common::wait_until(&mut app, "the deletion", |app| {
        app.query_by_label("Greet").is_none()
    });
    common::click(&mut app, "Close Window");
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());
    let saved = scripts_on_disk(&folder.file());
    let names: Vec<(u64, &str)> = saved
        .scripts
        .iter()
        .map(|s| (s.id, s.name.as_str()))
        .collect();
    assert_eq!(names, [(2, "Say Built")]);
}

/// A copy of a FileMaker Pro starter solution opens in the window with no
/// sign-in dialog, as FileMaker Pro opens it (File Options, local-files.md
/// 3.1). With `OM_STARTER_SHOT` naming a PNG, the opened window is saved.
#[test]
#[ignore = "requires OM_STARTER_FILE, a writable copy of a FileMaker Pro starter solution"]
fn a_starter_solution_opens_with_no_sign_in_dialog() {
    let path = PathBuf::from(std::env::var_os("OM_STARTER_FILE").expect("a file"));
    let mut app = app();
    common::click(&mut app, "Browse...");
    choose(&mut app, &path);
    common::wait_until(&mut app, "the file to open", |app| {
        assert!(
            app.query_by_label("Account Name").is_none(),
            "asked for an account"
        );
        app.state().is_open() && app.state().is_settled()
    });
    app.run_steps(10);
    // SVG icons are drawn on a background thread.
    common::wait_until(&mut app, "the icons", |app| !app.ctx.has_pending_images());
    if let Some(shot) = std::env::var_os("OM_STARTER_SHOT") {
        app.set_render_cursor(false);
        app.render().unwrap().save(shot).unwrap();
    }
}

/// An account whose password must be changed (211) is asked for a new one
/// (#157): a confirmation that does not match is caught in the dialog; the
/// new password, written as FileMaker writes it, opens the file, and opens
/// it again after it is closed, while the old one no longer does. With
/// `OM_CHANGE_SHOTS` naming a directory, the dialog is saved there.
#[test]
fn an_account_due_a_change_is_asked_for_a_new_password() {
    const NEW: &str = "fresh secret";
    let shots = std::env::var_os("OM_CHANGE_SHOTS").map(PathBuf::from);
    let folder = Folder::new("change");
    let mut app = app();
    let open_as = |app: &mut App, password: &str| {
        common::click(app, "Browse...");
        choose(app, &folder.file());
        common::wait_for_label(app, "Open “Tasks”");
        replace(app, "Account Name", fixture::NEWCOMER);
        common::fill(app, "Password", password);
        common::click(app, "OK");
    };
    open_as(&mut app, fixture::NEWCOMER_PASSWORD);
    common::wait_for_label(&mut app, "Change Password");
    if let Some(dir) = &shots {
        app.render()
            .unwrap()
            .save(dir.join("change-password-dialog.png"))
            .unwrap();
    }
    common::fill(&mut app, "New Password", NEW);
    common::fill(&mut app, "Confirm New Password", "something else");
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        "The new password and its confirmation do not match.",
    );
    assert!(!app.state().is_open());
    replace(&mut app, "Confirm New Password", NEW);
    common::click(&mut app, "OK");
    wait_open(&mut app);
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    // The old password is refused now; the new one opens the file.
    open_as(&mut app, fixture::NEWCOMER_PASSWORD);
    common::wait_for_label(
        &mut app,
        "The account name and password you entered cannot be used to access this file.",
    );
    replace(&mut app, "Password", NEW);
    common::click(&mut app, "OK");
    wait_open(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// File > Change Password… changes the open file's account's password
/// (#830): a wrong old password is refused in the dialog, the right one
/// changes it, and after the file is closed the old password no longer
/// opens it while the new one does. With `OM_CHANGE_MENU_SHOT` naming a
/// PNG, the dialog is saved.
#[test]
fn file_change_password_changes_the_accounts_password() {
    const NEW: &str = "a new secret";
    let folder = Folder::new("change-menu");
    let mut app = app();
    common::click(&mut app, "Browse...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    wait_open(&mut app);
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Change Password...");
    common::wait_for_label(&mut app, "Old Password");
    if let Some(shot) = std::env::var_os("OM_CHANGE_MENU_SHOT") {
        app.render().unwrap().save(shot).unwrap();
    }
    common::fill(&mut app, "Old Password", "not it");
    common::fill(&mut app, "New Password", NEW);
    common::fill(&mut app, "Confirm New Password", NEW);
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        "The account name and password you entered cannot be used to access this file.",
    );
    replace(&mut app, "Old Password", fixture::PASSWORD);
    replace(&mut app, "New Password", NEW);
    replace(&mut app, "Confirm New Password", NEW);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the dialog to close", |app| {
        app.query_by_label("Old Password").is_none()
    });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    common::click(&mut app, "Browse...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    common::wait_for_label(
        &mut app,
        "The account name and password you entered cannot be used to access this file.",
    );
    replace(&mut app, "Password", NEW);
    common::click(&mut app, "OK");
    wait_open(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn a_refused_password_asks_again_and_cancel_lets_the_file_go() {
    let folder = Folder::new("refused");
    let before = std::fs::read(folder.file()).unwrap();
    let mut app = app();
    common::click(&mut app, "Browse...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, "not the password");
    common::wait_for_label(
        &mut app,
        "The account name and password you entered cannot be used to access this file.",
    );
    assert!(!app.state().is_open());
    // The right password of an account in [Data Entry Only]: refused too,
    // saying why, and the dialog asks again.
    replace(&mut app, "Account Name", fixture::CLERK);
    common::fill(&mut app, "Password", fixture::CLERK_PASSWORD);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the privilege refusal", |app| {
        app.query_by_label_contains("has a limit OpenMaker does not enforce yet")
            .is_some()
    });
    // An inactive account's right password: refused as FileMaker does.
    replace(&mut app, "Account Name", fixture::FORMER);
    common::fill(&mut app, "Password", fixture::FORMER_PASSWORD);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the inactive account's refusal", |app| {
        app.query_by_label_contains("is inactive in this file")
            .is_some()
    });
    assert!(!app.state().is_open());
    assert_eq!(std::fs::read(folder.file()).unwrap(), before);
    // The file waits for an account; Cancel closes it for everyone else.
    common::click(&mut app, "Cancel");
    common::wait_until(&mut app, "the dialog to close", |app| {
        app.query_by_label("Open “Tasks”").is_none()
    });
    let mut other = clean_provider();
    let file = om_data::LocalFile {
        path: folder.file(),
        credentials: Some((fixture::ACCOUNT.into(), fixture::PASSWORD.to_owned().into())),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.step();
        match other.open_local(&file) {
            Ok(_) => break,
            Err(error) if error.code == Some(300) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the window kept the file"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    other.close();
    assert_eq!(std::fs::read(folder.file()).unwrap(), before);
}

#[test]
fn a_file_that_is_not_a_filemaker_file_is_refused_unchanged() {
    let folder = Folder::new("not-fmp12");
    let path = folder.0.join("notes.fmp12");
    std::fs::write(&path, b"plain text, not a database").unwrap();
    let mut app = app();
    common::click(&mut app, "Browse...");
    choose(&mut app, &path);
    common::wait_until(&mut app, "the refusal", |app| app.state().error().is_some());
    let error = app.state().error().unwrap().to_owned();
    assert!(error.contains("not a FileMaker Pro 12 file"), "{error}");
    assert!(!app.state().is_open());
    assert_eq!(std::fs::read(&path).unwrap(), b"plain text, not a database");
}

/// Show Custom Dialog ["File routing"; text] with an OK button.
fn show(text: &str) -> om_scripts::ScriptStep {
    let mut step = om_scripts::ScriptStep::new(87);
    step.set_calculation(0, Some(&fixture::calc("\"File routing\"")));
    step.set_calculation(1, Some(&fixture::calc(text)));
    step.set_text(0, Some("OK"));
    step
}

/// A local file opens on the found set and record FileMaker Pro saved for
/// the layout's occurrence (file-metadata.md 11), and its OnFirstWindowOpen
/// script runs there: the window is on the record before its triggers run.
#[test]
fn a_local_files_opening_script_runs_on_the_record_it_was_closed_on() {
    use om_hbam::OwnedNode;
    let folder = Folder::new("saved-record");
    // The script then goes to Notes, which it has not shown, and to the
    // next record: Next counts from Notes' saved record.
    let opening = [fixture::script(
        1,
        "Opening",
        vec![
            show("Get ( RecordID ) & \":\" & Get ( FoundCount )"),
            fixture::go_to_layout(fixture::NOTES_LAYOUT),
            fixture::go_to_record(4),
            show("\"Notes \" & Get ( RecordID )"),
        ],
    )];
    let mut root = fixture::tree(&opening, true);
    fixture::add_local_accounts(&mut root);
    add_name_field(&mut root);
    fixture::add_tasks(&mut root, 3..=3);
    // Records 1 and 3 found, record 3 current.
    let mut found = om_hbam::RidList::new();
    found.insert(1);
    found.insert(3);
    let occurrence = om_hbam::slot::uint_segment(fixture::OCCURRENCE);
    root.put(
        &[&occurrence, &[7], &[1]],
        OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::leaf([0x03], vec![1, 3]),
                found.to_node(vec![0x05]),
            ],
        ),
    );
    // Notes records 2 and 3, every record found, record 2 current.
    let notes = om_hbam::slot::uint_segment(fixture::NOTES);
    let mut ids = om_hbam::RidList::new();
    for id in 1..=3u64 {
        ids.insert(id);
        if id > 1 {
            root.put(
                &[
                    &notes,
                    &[5],
                    &om_hbam::serial::encode(id),
                    &om_hbam::slot::uint_segment(fixture::BODY),
                ],
                om_scripts::text_node(&[], &format!("Note {id}")),
            );
        }
    }
    root.put(&[&notes, &[0x0d]], ids.to_node(Vec::new()));
    root.put(&[&notes, &[0]], OwnedNode::leaf(Vec::new(), vec![1, 3]));
    let occurrence = om_hbam::slot::uint_segment(fixture::NOTES_OCCURRENCE);
    root.put(
        &[&occurrence, &[7], &[1]],
        OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::leaf([0x03], vec![1, 2]),
                OwnedNode::list([0x05], Vec::new()),
                OwnedNode::leaf([0xff], Vec::new()),
            ],
        ),
    );
    // OnFirstWindowOpen runs script 1 (file-metadata.md 3.1; the members
    // after the default layout are optional).
    root.put(
        &[&[2], &[0x18]],
        OwnedNode::leaf(Vec::new(), vec![0, 0, 4, 3, 0, 0, 2, 1, 1, 0, 0]),
    );
    fixture::write_tree(&folder.file(), &root);
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    common::wait_for_label(&mut app, "3:2");
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "Notes 3");
    common::click(&mut app, "OK");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The walkthrough's sequence (#642) in OpenMaker alone: a window omits a
/// record, edits the next one and saves, the file closes and opens again on
/// that found set and record, which the window kept in the file
/// (file-metadata.md 11).
#[test]
fn a_local_file_reopens_on_the_found_set_and_record_it_was_closed_on() {
    use om_hbam::OwnedNode;
    let folder = Folder::new("kept-found");
    let scripts = [fixture::script(1, "Greet", Vec::new())];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_local_accounts(&mut root);
    add_name_field(&mut root);
    fixture::add_tasks(&mut root, 3..=3);
    // The table's data version, as every file has it.
    let table = om_hbam::slot::uint_segment(fixture::TABLE);
    root.put(
        &[&[3], &[0x10], &[5], &table, &[0xfb]],
        OwnedNode::leaf(Vec::new(), 1u32.to_be_bytes().to_vec()),
    );
    fixture::write_tree(&folder.file(), &root);
    let open = |app: &mut App| {
        common::open_menu(app, "File");
        common::click(app, "Open...");
        choose(app, &folder.file());
        sign_in(app, fixture::PASSWORD);
        wait_open(app);
    };
    let name = |app: &App| common::text_field(app, "Name").value();
    let mut app = app();
    open(&mut app);
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Omit Record");
    common::wait_until(&mut app, "Ship", |app| name(app).as_deref() == Some("Ship"));
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "Task 3", |app| {
        name(app).as_deref() == Some("Task 3")
    });
    replace(&mut app, "Name", "Kept task");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the save", |app| {
        app.state().is_settled() && !common::editing(app)
    });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    open(&mut app);
    common::wait_until(&mut app, "the kept record", |app| {
        name(app).as_deref() == Some("Kept task")
    });
    assert_eq!(common::record_number(&app), "2");
    app.get_by_label("Found (Unsorted)");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The walkthrough's Quick Find route (#536) in OpenMaker alone: a first
/// Quick Find, for which the host builds the field's index, then File >
/// Close. The file opens again on that found set and record, as FileMaker
/// Pro 26.0.3 reopened a fresh Tasks copy by File > Close as by the close
/// button (#642's table, 2026-11-14).
#[test]
fn a_local_file_reopens_on_a_quick_finds_found_set_after_file_close() {
    use om_hbam::OwnedNode;
    let folder = Folder::new("quick-found");
    let scripts = [fixture::script(1, "Greet", Vec::new())];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_local_accounts(&mut root);
    add_name_field(&mut root);
    fixture::add_tasks(&mut root, 3..=5);
    let table = om_hbam::slot::uint_segment(fixture::TABLE);
    root.put(
        &[&[3], &[0x10], &[5], &table, &[0xfb]],
        OwnedNode::leaf(Vec::new(), 1u32.to_be_bytes().to_vec()),
    );
    fixture::write_tree(&folder.file(), &root);
    let open = |app: &mut App| {
        common::open_menu(app, "File");
        common::click(app, "Open...");
        choose(app, &folder.file());
        sign_in(app, fixture::PASSWORD);
        wait_open(app);
    };
    let name = |app: &App| common::text_field(app, "Name").value();
    let mut app = app();
    open(&mut app);
    replace(&mut app, "Quick Find", "Task");
    app.key_press(egui::Key::Enter);
    common::wait_until(&mut app, "the found set", |app| {
        app.query_by_label("Found (Unsorted)").is_some() && name(app).as_deref() == Some("Task 3")
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "Task 4", |app| {
        name(app).as_deref() == Some("Task 4")
    });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());

    open(&mut app);
    common::wait_until(&mut app, "the kept record", |app| {
        name(app).as_deref() == Some("Task 4")
    });
    assert_eq!(common::record_number(&app), "2");
    app.get_by_label("Found (Unsorted)");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Several field placements stay in the layout draft, independently editable,
/// until explicit Save. Revert drops all additions without changing the file.
#[test]
fn consecutive_fields_are_drafts_until_save_and_survive_reopen() {
    use egui_kittest::kittest::NodeT as _;
    let folder = Folder::new("field-drafts");
    let path = folder.0.join("Drafts.fmp12");
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Create New...");
    common::click(&mut app, "Create");
    common::wait_for_label(&mut app, "Save As:");
    replace(&mut app, "Save As:", "Drafts.fmp12");
    replace(&mut app, "Where:", &folder.0.display().to_string());
    common::click(&mut app, "Save");
    common::wait_for_label(&mut app, "5 fields");
    for (name, count) in [("First", "6 fields"), ("Second", "7 fields")] {
        replace(&mut app, "Field Name", name);
        common::click(&mut app, "Create");
        common::wait_for_label(&mut app, count);
    }
    // Another base table deliberately reuses PrimaryKey's numeric field ID.
    common::click(&mut app, "Tables");
    replace(&mut app, "Table Name", "Auxiliary");
    common::click(&mut app, "Create");
    common::wait_for_label(&mut app, "2 tables defined in this file");
    common::click(&mut app, "Auxiliary");
    app.get_all_by_label("Fields").last().unwrap().click();
    app.run_steps(3);
    replace(&mut app, "Field Name", "Auxiliary Key");
    common::click(&mut app, "Create");
    common::wait_for_label(&mut app, "1 field");
    common::click(&mut app, "OK");
    // is_settled describes the loaded document, not the modal's asynchronous
    // schema commit. Until OK's reply closes Manage Database, its overlay
    // blocks Edit Layout even when the old document is already settled.
    common::wait_until(&mut app, "the schema committed and dialog closed", |app| {
        app.query_by_label("Database Table").is_none() && app.state().is_settled()
    });
    common::click(&mut app, "Edit Layout");
    common::wait_for_label(&mut app, "Layout canvas");
    common::click(&mut app, "Objects");
    common::click(&mut app, "New Layout / Report");
    common::wait_for_label(&mut app, "New Layout/Report");
    common::click(&mut app, "Show records from");
    app.get_all_by_label("Drafts").last().unwrap().click();
    app.run_steps(3);
    replace(&mut app, "Layout Name", "Entry Layout");
    common::click(&mut app, "Finish");
    // Finish closes the dialog at once and the new layout opens only after
    // the host's answer; until then the old layout is still settled on the
    // canvas, so wait for the layout bar to name the new one.
    common::wait_until(&mut app, "the new own-table layout", |app| {
        app.query_by_role_and_label(egui::Role::ComboBox, "Layout")
            .and_then(|layout| layout.value())
            .as_deref()
            == Some("Entry Layout")
            && app.state().is_settled()
            && app.query_by_label("Layout canvas").is_some()
    });
    common::click(&mut app, "Fields");
    app.get_by_label("Table: Drafts");
    assert!(app
        .query_by_role_and_label(egui::Role::Button, "Auxiliary Key")
        .is_none());
    // A later schema commit must refresh the visible field list without reopening.
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::D,
    );
    common::wait_for_label(&mut app, "Database Table");
    common::click(&mut app, "Database Table");
    app.get_all_by_label("Drafts").last().unwrap().click();
    app.run_steps(3);
    replace(&mut app, "Field Name", "Fresh");
    common::click(&mut app, "Create");
    common::wait_for_label(&mut app, "8 fields");
    common::click(&mut app, "OK");
    // Manage Database lists Fresh as a button too: until OK's reply closes
    // it, that row satisfies the sidebar check while the dialog's overlay
    // still takes the Field Tools click.
    common::wait_until(&mut app, "fresh sidebar definitions", |app| {
        app.query_by_label("Database Table").is_none()
            && app.state().is_settled()
            && app
                .query_by_role_and_label(egui::Role::Button, "Fresh")
                .is_some()
    });
    let saved = std::fs::read(&path).unwrap();
    let place = |app: &mut App, name: &str, top: f32| {
        common::click(app, "Field Tools");
        let origin = app.get_by_label("Layout canvas").rect().min + egui::vec2(22.0, 0.0);
        let from = origin + egui::vec2(115.0, top);
        let to = origin + egui::vec2(285.0, top + 24.0);
        app.event(egui::Event::PointerMoved(from));
        app.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.step();
        for step in 1..=4 {
            app.event(egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 4.0),
            ));
            app.step();
        }
        app.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(3);
        common::click(app, "Field to Place");
        app.get_all_by_label(name).last().unwrap().click();
        app.run_steps(3);
        app.get_by_label(&format!("New field {name}"));
    };
    let sidebar_place = |app: &mut App, name: &str, top: f32| {
        common::click(app, "Fields");
        let from = app
            .get_by_role_and_label(egui::Role::Button, name)
            .rect()
            .center();
        let to = app.get_by_label("Layout canvas").rect().min + egui::vec2(22.0 + 115.0, top);
        app.event(egui::Event::PointerMoved(from));
        app.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.step();
        for step in 1..=4 {
            app.event(egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 4.0),
            ));
            app.step();
        }
        app.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(3);
        app.get_by_label(&format!("New field {name}"));
    };
    for discarded in [true, false] {
        if discarded {
            place(&mut app, "First", 45.0);
            place(&mut app, "Second", 105.0);
        } else {
            sidebar_place(&mut app, "First", 45.0);
            sidebar_place(&mut app, "Second", 105.0);
            sidebar_place(&mut app, "PrimaryKey", 165.0);
        }
        app.get_all_by_label("Objects").last().unwrap().click();
        app.run_steps(3);
        app.get_by_label("New field First");
        app.get_by_label("New field Second");
        // Select and resize the earlier addition through the Inspector.
        common::click(&mut app, "Field - First (unsaved)");
        app.get_by_label("New Field Width")
            .set_value_accesskit(190.0);
        app.run_steps(3);
        assert_eq!(
            app.get_by_label("New Field Width")
                .accesskit_node()
                .numeric_value(),
            Some(190.0)
        );
        common::click(&mut app, "Field - Second (unsaved)");
        assert_eq!(
            app.get_by_label("New Field Width")
                .accesskit_node()
                .numeric_value(),
            Some(if discarded { 170.0 } else { 120.0 })
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            saved,
            "drawing and binding must not write"
        );
        if discarded {
            // Both commands must reach a selected draft, without touching its peer.
            common::open_menu(&mut app, "Edit");
            common::click(&mut app, "Clear");
            assert!(app.query_by_label("New field Second").is_none());
            app.get_by_label("New field First");
            place(&mut app, "Second", 105.0);
            common::click(&mut app, "Field - Second (unsaved)");
            app.key_press(egui::Key::Delete);
            app.run_steps(3);
            assert!(app.query_by_label("New field Second").is_none());
            app.get_by_label("New field First");
            place(&mut app, "Second", 105.0);
        }
        if discarded {
            common::open_menu(&mut app, "Layouts");
            common::click(&mut app, "Revert Layout");
            assert!(app.query_by_label("New field First").is_none());
            assert!(app.query_by_label("New field Second").is_none());
            assert_eq!(std::fs::read(&path).unwrap(), saved);
        }
    }
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    // Save Layout places the fields one at a time, rereading the layout
    // before sending the next: First and Second can show while PrimaryKey's
    // placement is still with the host, and Exit Layout would be refused.
    common::wait_until(&mut app, "all three fields saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !app.state().layout_unsaved()
            && app.state().is_settled()
            && app.query_by_label("New field First").is_none()
            && app.query_by_label("Field - First").is_some()
            && app.query_by_label("Field - Second").is_some()
    });
    common::click(&mut app, "Exit Layout");
    common::wait_until(&mut app, "Browse fields", |app| {
        common::find_text_field(app, "First").is_some()
            && common::find_text_field(app, "Second").is_some()
    });
    replace(&mut app, "First", "one");
    replace(&mut app, "Second", "two");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    common::wait_until(&mut app, "record saved", |app| app.state().is_settled());
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "closed", |app| !app.state().is_open());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    choose(&mut app, &path);
    common::wait_until(&mut app, "the file reopened", |app| {
        app.state().is_open() && app.state().is_settled()
    });
    common::click(&mut app, "Layout");
    common::click(&mut app, "Entry Layout");
    common::wait_until(&mut app, "reopened fields", |app| {
        app.state().is_settled() && common::find_text_field(app, "Second").is_some()
    });
    for (name, value) in [("First", "one"), ("Second", "two")] {
        assert_eq!(
            common::text_field(&app, name).value().as_deref(),
            Some(value)
        );
    }
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the reopened file closed", |app| {
        !app.state().is_open()
    });
    drop(app);
    let mut reader = clean_provider();
    reader
        .open_local(&om_data::LocalFile {
            path,
            credentials: None,
        })
        .unwrap();
    let layout = reader
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Entry Layout")
        .unwrap()
        .id;
    let geometry = reader.layout_geometry(layout).unwrap().unwrap();
    for (name, top, width) in [("First", 45.0, 190.0), ("Second", 105.0, 120.0)] {
        let field = geometry
            .objects
            .iter()
            .find(|object| object.field_name.as_deref() == Some(name))
            .unwrap();
        assert_eq!(
            (
                field.rect.left,
                field.rect.top,
                field.rect.width(),
                field.rect.height()
            ),
            (115.0, top, width, 22.0)
        );
    }
    let schema = reader.database_schema().unwrap();
    let own = schema.tables.iter().find(|t| t.name == "Drafts").unwrap();
    let other = schema
        .tables
        .iter()
        .find(|t| t.name == "Auxiliary")
        .unwrap();
    let own_key = own.fields.iter().find(|f| f.name == "PrimaryKey").unwrap();
    let other_key = other
        .fields
        .iter()
        .find(|f| f.name == "Auxiliary Key")
        .unwrap();
    assert_eq!(
        own_key.id, other_key.id,
        "the fixture must exercise colliding field IDs"
    );
    assert!(geometry
        .objects
        .iter()
        .any(|o| o.field_name.as_deref() == Some("PrimaryKey")));
    assert!(!geometry
        .objects
        .iter()
        .any(|o| o.field_name.as_deref() == Some("Auxiliary Key")));
    reader.close();
}

/// A local file whose script performs a script of the file beside it,
/// through the window, with no server and no license: Root's data source
/// names Peer by a plain relative name. Peer's script runs in Peer's own
/// window and record, writes and commits there, and returns its result;
/// Root continues in its own window and record. Each write is in the file
/// that owns it, and both files are released when the window goes.
#[test]
fn a_local_files_script_performs_a_script_of_the_file_beside_it() {
    let folder = Folder::new("peer-script");
    let source = om_hbam::slot::uint_segment(0x8001);
    let mut call = fixture::perform_script(2);
    call.set_data_leaf(
        32,
        Some(om_data::runtime::OwnedNode::leaf([32], [2, 0x80, 0x01])),
    );
    call.set_calculation(0, Some(&fixture::calc("\"from Root\"")));
    let root_scripts = [
        fixture::script(
            1,
            "Call Peer",
            vec![
                call,
                show("Get ( ScriptResult ) & \"|\" & Get ( FileName ) & \"|\" & Name"),
            ],
        ),
        // The same id as Peer's script: never what the call runs.
        fixture::script(2, "Write here", vec![fixture::exit_with("\"WRONG ROOT\"")]),
    ];
    let mut root = fixture::tree(&root_scripts, true);
    fixture::add_local_accounts(&mut root);
    add_name_field(&mut root);
    root.put(
        &[&[32], &[5], &source, &[16]],
        om_scripts::text_node(&[], "Peer"),
    );
    let mut reference = 1u32.to_be_bytes().to_vec();
    for text in ["file", "", "Peer"] {
        reference.extend(om_scsu::encode_counted(text));
    }
    root.put(
        &[&[32], &[5], &source, &[0x82]],
        om_hbam::OwnedNode::leaf(vec![], reference),
    );
    let root_path = folder.0.join("Root.fmp12");
    fixture::write_tree(&root_path, &root);
    let peer_scripts = [fixture::script(
        2,
        "Write here",
        vec![
            fixture::set_field(
                fixture::NAME,
                "Get ( ScriptParameter ) & \" in \" & Get ( FileName )",
            ),
            om_scripts::ScriptStep::new(75), // Commit Records/Requests.
            fixture::exit_with("Get ( FileName ) & \":\" & Name"),
        ],
    )];
    let mut peer = fixture::tree(&peer_scripts, true);
    fixture::add_local_accounts(&mut peer);
    let peer_path = folder.0.join("Peer.fmp12");
    fixture::write_tree(&peer_path, &peer);

    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &root_path);
    common::wait_for_label(&mut app, "Open “Root”");
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "OK");
    wait_open(&mut app);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Call Peer");
    common::wait_until(&mut app, "the script's report", |a| {
        a.query_by_label("File routing").is_some() || a.state().error().is_some()
    });
    let expected = "Peer:from Root in Peer|Root|Write tests";
    assert!(
        app.query_by_label(expected).is_some(),
        "expected {expected}; error {:?}; labels {:?}",
        app.state().error(),
        app.query_all(egui_kittest::kittest::By::new().role(egui::Role::Label))
            .map(|n| n.value())
            .collect::<Vec<_>>()
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the script to end", |a| {
        !a.state().script_running()
    });
    assert_eq!(
        common::text_field(&app, "Name").value().as_deref(),
        Some("Write tests"),
        "Root's window keeps its own record"
    );
    drop(app);

    // Both files are free once the window is gone, each with its own write.
    let mut other = clean_provider();
    let read = |other: &mut NativeProvider, path: &Path| {
        let file = om_data::LocalFile {
            path: path.to_path_buf(),
            credentials: Some((fixture::ACCOUNT.into(), fixture::PASSWORD.to_owned().into())),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match other.open_local(&file) {
                Ok(_) => break,
                Err(error) if error.code == Some(300) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the window kept the file"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => panic!("{error:?}"),
            }
        }
        let layout = other.layouts().unwrap()[0].id;
        let page = other.record_by_id(layout, 1).unwrap();
        let column = page.fields.iter().position(|f| f.name == "Name").unwrap();
        let name = page.records[0].values[column].clone();
        other.close();
        name
    };
    assert_eq!(
        read(&mut other, &peer_path).as_deref(),
        Some("from Root in Peer")
    );
    assert_eq!(read(&mut other, &root_path).as_deref(), Some("Write tests"));
}

#[cfg(unix)]
#[test]
fn close_current_uses_the_open_identity_after_a_cross_folder_alias() {
    let actual = Folder::new("close-current-real");
    let links = Folder::new("close-current-links");
    let mut tree = fixture::tree(
        &[fixture::script(
            1,
            "Close current",
            vec![om_scripts::ScriptStep::new(34)],
        )],
        true,
    );
    fixture::add_local_accounts(&mut tree);
    add_name_field(&mut tree);
    fixture::write_tree(&actual.file(), &tree);
    let alias = links.0.join("Launch.fmp12");
    std::os::unix::fs::symlink(actual.file(), &alias).unwrap();
    let before = std::fs::read(actual.file()).unwrap();
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    choose(&mut app, &alias);
    common::wait_until(&mut app, "the account dialog", |a| {
        common::find_text_field(a, "Account Name").is_some()
    });
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "OK");
    wait_open(&mut app);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Close current");
    common::wait_until(&mut app, "the current file to close", |a| {
        !a.state().is_open() || a.state().error().is_some()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert!(!app.state().is_open());
    drop(app);
    assert_eq!(std::fs::read(actual.file()).unwrap(), before);
}

/// One real local login supplies both the desktop script and an object hide
/// calculation. All files, credentials and the clock-derived status are synthetic.
#[test]
fn local_login_account_facts_reach_a_script_and_layout_hide_calculation() {
    use om_hbam::slot::uint_segment;
    let folder = Folder::new("account-selectors");
    let report = "Get ( AccountType ) & \"|\" & Get ( AccountPasswordDaysRemaining )";
    let mut show = om_scripts::ScriptStep::new(87);
    show.set_calculation(0, Some(&fixture::calc("\"Account facts\"")));
    show.set_calculation(1, Some(&fixture::calc(report)));
    show.set_text(0, Some("OK"));
    let mut tree = fixture::tree(&[fixture::script(1, "Account facts", vec![show])], true);
    fixture::add_local_accounts(&mut tree);
    add_name_field(&mut tree);
    let today = om_data::host::session::today(om_provider::local_utc_offset());
    let (year, month, day) = today.ymd();
    let encoded = 0x800000 | ((year as u32) << 10) | (month << 6) | day;
    tree.put(
        &[&[23], &[1], &[5], &[2], &[0x0a]],
        om_hbam::OwnedNode::leaf(
            Vec::new(),
            vec![
                5,
                (encoded >> 16) as u8,
                (encoded >> 8) as u8,
                encoded as u8,
                0x80,
                0,
            ],
        ),
    );
    tree.put(
        &[&[23], &[2], &[5], &[1], &[0x0c]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0, 10]),
    );
    let hide = fixture::calc("Get ( AccountPasswordDaysRemaining ) = 10")
        .polish
        .unwrap();
    tree.put(
        &[
            &[4],
            &[5],
            &uint_segment(fixture::LAYOUT),
            &[5],
            &[1],
            &uint_segment(141),
            &[6],
        ],
        om_hbam::OwnedNode::leaf(Vec::new(), hide),
    );
    fixture::write_tree(&folder.file(), &tree);
    let mut app = app();
    common::click(&mut app, "Browse...");
    choose(&mut app, &folder.file());
    sign_in(&mut app, fixture::PASSWORD);
    common::wait_until(&mut app, "local file and layout", |app| {
        app.state().is_open() && app.state().is_settled()
    });
    app.run_steps(8);
    assert!(
        common::find_text_field(&app, "Name").is_none(),
        "known 10-day deadline hides the field"
    );
    assert!(
        app.query_by_label("Undetermined field").is_none(),
        "the deadline was known"
    );
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Account facts");
    common::wait_for_label(&mut app, "FileMaker File|10");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "account script finished", |app| {
        !app.state().script_running()
    });
}

/// A folder of its own holding a copy of a given file as `Tasks.fmp12`,
/// removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn copy(original: &Path) -> Self {
        let dir = std::env::temp_dir().join(format!("om-gui-local-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(original, dir.join(fixture::FILE)).expect("copy the probe file");
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join(fixture::FILE)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there.
fn shot(app: &mut App, name: &str) {
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

/// Select the only step of the script named `script` in the Script
/// Workspace, whose line starts with `line`, render the workspace as
/// `screenshot` if given, and open the step's Step Options.
fn step_options(app: &mut App, script: &str, line: &str, title: &str, screenshot: Option<&str>) {
    // Cancelling Step Options restores editor focus. Click the catalog
    // pane before typing so its search, rather than New Step, gets the text.
    common::click(app, "Search Scripts");
    replace(app, "Search Scripts", script);
    common::click(app, script);
    common::wait_until(app, line, |app| app.query_by_label_contains(line).is_some());
    app.get_by_label_contains(line).click();
    app.run_steps(2);
    if let Some(name) = screenshot {
        shot(app, name);
    }
    common::click(app, "Step Options");
    common::wait_for_label(app, title);
}

/// The step catalog's maps of the steps added in FileMaker 21-26
/// (script-steps.md 12, #149) as the Script Workspace shows them, on the
/// FileMaker-written probe file those maps were measured on: a copy of the
/// Tasks starter (account Admin, empty password) into which FileMaker
/// Server's FMUpgradeTool wrote one script per step and choice. A scratch
/// copy opens through File > Open. Go to List of Records ("P149 228b")
/// shows its record ids from calculation slot 7, its layout, and its new
/// window (flag `0x20000` with the window's slots) as a row kept as stored;
/// Create PDF ("P149 243a") shows its compound PDF options kept as stored.
/// No script runs and nothing is saved: each dialog is cancelled, and the
/// copy's bytes are the same after the file closes.
///
/// With `OM_SCREENSHOTS` set to a folder, three states are rendered there.
/// They show OpenMaker's own window as it is now, not FileMaker Pro's and
/// not a comparison with an earlier build.
#[test]
#[ignore = "requires OM_STEP_PROBE_FILE: a FileMaker-written copy of the Tasks starter (account Admin, empty password) holding the probe scripts \"P149 228b\" and \"P149 243a\""]
fn the_script_workspace_shows_new_steps_of_a_filemaker_written_probe_file() {
    let original = PathBuf::from(
        std::env::var_os("OM_STEP_PROBE_FILE").expect("OM_STEP_PROBE_FILE names the probe file"),
    );
    let original_bytes = std::fs::read(&original).expect("read the probe file");
    let scratch = Scratch::copy(&original);
    let before = std::fs::read(scratch.file()).unwrap();
    let mut app = Harness::builder()
        .with_size(egui::vec2(1672.0, 900.0))
        .renderer(common::renderer())
        .build_eframe(|_cc| {
            OpenMakerApp::with_license_store(Box::new(clean_provider()), Box::new(Untouchable))
        });
    common::click(&mut app, "Browse...");
    choose(&mut app, &scratch.file());
    sign_in(&mut app, "");
    common::wait_until(&mut app, "the probe file", |app| {
        app.state().is_open() && app.state().is_settled()
    });
    // The measured probe opens on trigger-free Task Details. Other layouts
    // have enter scripts, so refuse to proceed from an unexpected layout.
    assert_eq!(
        app.get_by_label("Layout").value().as_deref(),
        Some("Task Details")
    );
    assert!(!app.state().script_running());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(&mut app, "Search Scripts");

    step_options(
        &mut app,
        "P149 228b",
        "Line 1: Go to List of Records",
        "\"Go to List of Records\" Options",
        Some("520-probe-workspace-go-to-list-of-records"),
    );
    assert_eq!(
        common::text_field(&app, "List of Record IDs")
            .value()
            .as_deref(),
        Some("\"228b_Calculation\"")
    );
    assert_eq!(
        common::text_field(&app, "Layout").value().as_deref(),
        Some("Task List")
    );
    assert!(app.query_by_label("New Window:").is_some());
    assert_eq!(app.query_all_by_label("Kept as stored").count(), 1);
    shot(&mut app, "520-probe-go-to-list-of-records-options");
    common::click(&mut app, "Cancel");

    step_options(
        &mut app,
        "P149 243a",
        "Line 1: Create PDF",
        "\"Create PDF\" Options",
        None,
    );
    assert!(app.query_by_label("PDF Options:").is_some());
    assert_eq!(app.query_all_by_label("Kept as stored").count(), 1);
    shot(&mut app, "520-probe-create-pdf-options");
    common::click(&mut app, "Cancel");

    common::click(&mut app, "Close Window");
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());
    assert_eq!(
        std::fs::read(scratch.file()).unwrap(),
        before,
        "the copy is unchanged"
    );
    assert_eq!(
        std::fs::read(&original).unwrap(),
        original_bytes,
        "the probe file is unchanged"
    );
}
