//! Sorts and finds on FileMaker Pro 26's English starter solutions
//! (`DynamicTemplates/*/en.fmp12`), end to end through the desktop window
//! on disposable copies holding records with distinct values (ROADMAP.md,
//! alpha criterion 3):
//!
//! - entering a list layout runs its layout trigger, the solution's own
//!   stored sort (Sort Records [Restore], script-steps.md 11.1): Tasks'
//!   and Content Management's "Sort by Status" (Status in the custom order
//!   of its value list), Contacts' "Sort by Name";
//! - the Sort dialog shows that stored order, and sorts by Task ascending,
//!   then descending;
//! - Find, Extend Found Set and Constrain Found Set from Find mode;
//! - on each of the six (Assets, Contacts, Content Management, Inventory,
//!   Meetings, Tasks), with five records named in the list's first name
//!   field: Show All, the Sort dialog by that field both ways, Find,
//!   Extend, Constrain, Omit Record of record 1, Quick Find from the
//!   toolbar and Show All again (`finds_on`).
//!
//! The records are added through the native provider before the window
//! opens the copy. Ignored by default: the templates are git-ignored inputs
//! (`re/extracted`, docs/development.md). Run with
//!   OM_TEMPLATES=<DynamicTemplates folder> \
//!     tools/xvfb-run.sh cargo test -p om-gui --test starter_finds -- --ignored
//!
//! With `OM_STARTER_FINDS_HOSTED` set, each copy is hosted instead by
//! OpenMaker Server, a separate `om-server` process on loopback
//! (`scorecard/server.rs`; build it with `cargo build -p om-server`), and
//! the window opens it through Open Network File as the file's own Admin.
mod common;
#[path = "../src/scorecard/server.rs"]
mod server;

use std::path::{Path, PathBuf};

use common::App;
use egui::Role;
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use egui_kittest::Harness;
use om_data::{DataProvider, FieldChanges, LocalFile, LocalOpen};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::{Mode, OpenMakerApp};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};

const TEMPLATES: [&str; 2] = [
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates",
];

/// A store the window must not need: local files take no license.
struct NoLicense;

impl LicenseStore for NoLicense {
    fn load(&mut self) -> Result<Option<LicenseKey>, om_data::DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), om_data::DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), om_data::DataError> {
        Ok(())
    }
}

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

/// The template `solution` (its English file): in `OM_TEMPLATES` (a
/// `DynamicTemplates` folder) or this checkout's `re/extracted`.
fn template(solution: &str) -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::var_os("OM_TEMPLATES")
        .map(PathBuf::from)
        .into_iter()
        .chain(TEMPLATES.iter().map(|dir| manifest.join(dir)))
        .map(|dir| dir.join(solution).join("en.fmp12"))
        .find(|path| path.is_file())
}

/// A disposable copy of template `solution`, as `<solution>.fmp12`, with a
/// record per row of `rows` (field and value pairs) added on layout
/// `layout`.
fn seeded(dir: &Path, solution: &str, layout: &str, rows: &[[(&str, &str); 2]]) -> PathBuf {
    let template = template(solution).expect("the template under OM_TEMPLATES or re/extracted");
    let path = dir.join(format!("{solution}.fmp12"));
    std::fs::copy(template, &path).unwrap();
    let mut provider = provider();
    let opened = provider
        .open_local(&LocalFile {
            path: path.clone(),
            credentials: Some(("Admin".into(), String::new().into())),
        })
        .unwrap();
    assert!(matches!(opened, LocalOpen::Opened { read_only: None }));
    let layout = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == layout)
        .unwrap()
        .id;
    for row in rows {
        let values: FieldChanges = row
            .iter()
            .map(|(field, value)| ((*field).to_owned(), Some((*value).to_owned())))
            .collect();
        provider.create_record(layout, values).unwrap();
    }
    provider.close();
    path
}

fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// The window, and the server hosting its file when it is hosted.
struct Opened {
    app: App,
    _server: Option<server::HostedServer>,
}

/// File > Open `path` as Admin, whose password is empty (or, hosted, the
/// Open Network File's Open as the server's Admin), then its list layout
/// through the details layout's `back` button (Go to Layout).
fn open(path: &Path, back: &str) -> Opened {
    let hosted = std::env::var_os("OM_STARTER_FINDS_HOSTED").is_some();
    let host = hosted.then(|| {
        let dir = path.parent().unwrap().join("server");
        std::fs::create_dir_all(&dir).unwrap();
        server::HostedServer::start(&[path.to_owned()], &dir).expect("OpenMaker Server")
    });
    let window_provider = match &host {
        Some(server) => {
            let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
            NativeProvider::new(NativeConfig {
                client_cert_pem: client.cert.pem().into_bytes(),
                client_key_pem: client.key_pair.serialize_pem().into_bytes(),
                additional_ca_pem: server.ca_pem.clone(),
                callback_host: "127.0.0.1".into(),
                machine: MachineInfo::default(),
            })
        }
        None => provider(),
    };
    let mut app = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(move |_cc| {
            OpenMakerApp::with_license_store(Box::new(window_provider), Box::new(NoLicense))
        });
    match &host {
        Some(server) => {
            common::fill(
                &mut app,
                "Path",
                &format!(
                    "fmnet:/127.0.0.1:{}/{}",
                    server.port,
                    path.file_name().unwrap().to_string_lossy()
                ),
            );
            common::fill(&mut app, "Account Name", server::ACCOUNT);
            common::fill(&mut app, "Password", server::PASSWORD);
            common::click(&mut app, "Open");
        }
        None => {
            common::open_menu(&mut app, "File");
            common::click(&mut app, "Open...");
            common::wait_for_label(&mut app, "Open File");
            replace(&mut app, "Path", &path.display().to_string());
            common::click(&mut app, "Open");
            // A starter's File Options log in as Admin without asking
            // (local-files.md 3.1); a copy that asks gets its dialog's OK.
            let stem = path.file_stem().unwrap().to_string_lossy();
            let asking = format!("Open “{stem}”");
            common::wait_until(&mut app, "the file open or its sign-in", |app| {
                app.state().is_open() || app.query_by_label(&asking).is_some()
            });
            if app.query_by_label(&asking).is_some() {
                common::click(&mut app, "OK");
            }
        }
    }
    settle(&mut app);
    // The top-left one: the details layout's own back button.
    app.query_all(By::new().role(Role::Button).label(back))
        .min_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x))
        .unwrap()
        .click();
    app.run_steps(2);
    settle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // A hosted file's window names its host.
    if host.is_some() {
        let title = app.state().window_name();
        assert!(title.contains("127.0.0.1"), "not hosted: {title}");
    }
    Opened { app, _server: host }
}

/// No script running and nothing loading.
fn settle(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !(app.state().is_settled() && !app.state().script_running()) {
        app.step();
        if std::time::Instant::now() > deadline {
            panic!(
                "the window did not settle: settled {} script {} paused {} error {:?}\n{:#?}",
                app.state().is_settled(),
                app.state().script_running(),
                app.state().script_paused(),
                app.state().error(),
                app.root()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.run_steps(3);
}

/// Field `field` of each row List view shows, top to bottom.
fn rows(app: &App, field: &str) -> Vec<String> {
    let mut rows: Vec<(f32, String)> = app
        .query_all(By::new().role(Role::TextInput).label(field))
        .filter_map(|node| Some((node.rect().min.y, node.value()?)))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, task)| task).collect()
}

/// Wait until List view shows `expected` in `field`, in order.
fn expect_rows(app: &mut App, field: &str, expected: &[&str]) {
    common::wait_until(app, &format!("{field} rows {expected:?}"), |app| {
        rows(app, field) == expected
    });
}

/// Find mode, `task` as the Task criterion, then `command` from the
/// Requests menu (Perform Find, Constrain or Extend Found Set).
fn find(app: &mut App, task: &str, command: &str) {
    common::click(app, "Find");
    common::wait_for_label(app, "Perform Find");
    common::fill(app, "Task", task);
    common::open_menu(app, "Requests");
    // The menu item, distinct from the tall toolbar button.
    app.get_all_by_label(command)
        .find(|node| node.rect().height() < 30.0)
        .unwrap()
        .click();
    app.run_steps(2);
    common::wait_until(app, "Browse mode", |app| app.state().mode() == Mode::Browse);
    settle(app);
}

#[test]
#[ignore = "requires the English Tasks template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn tasks_sorts_by_its_value_list_and_by_field_and_finds_constrains_and_extends() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        [("Task", "Echo"), ("Status", "Postponed")],
        [("Task", "Alpha"), ("Status", "Completed")],
        [("Task", "Charlie"), ("Status", "Pending")],
        [("Task", "Delta"), ("Status", "Canceled")],
        [("Task", "Bravo"), ("Status", "In Progress")],
    ];
    let path = seeded(dir.path(), "Tasks", "Task Details", &rows);
    let Opened {
        mut app,
        _server: _host,
    } = open(&path, "Tasks");

    // Sort by Status: In Progress, Pending, Postponed, Canceled, Completed
    // (the Status value list's order).
    app.get_by_label("Total (Sorted)");
    expect_rows(
        &mut app,
        "Task",
        &["Bravo", "Charlie", "Echo", "Delta", "Alpha"],
    );
    // The Sort dialog shows the stored order the layout's trigger sorted by,
    // and Current Layout's fields alphabetically, as FileMaker Pro lists
    // them (#840's reading).
    common::click(&mut app, "Sort");
    common::wait_for_label(&mut app, "Tasks::Status");
    let mut items: Vec<(f32, String)> = app
        .query_all(By::new().role(Role::ListItem))
        .filter_map(|item| Some((item.rect().min.y, item.accesskit_node().label()?)))
        .filter(|(_, label)| ["Category", "Due Date", "Status", "Task"].contains(&label.as_str()))
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let items: Vec<String> = items.into_iter().map(|(_, label)| label).collect();
    assert_eq!(items, ["Category", "Due Date", "Status", "Task"]);
    app.query_all_by_label("Cancel").last().unwrap().click();
    app.run_steps(2);
    settle(&mut app);
    app.get_by_label("Total (Sorted)");

    sort_by(&mut app, "Task", false);
    expect_rows(
        &mut app,
        "Task",
        &["Alpha", "Bravo", "Charlie", "Delta", "Echo"],
    );
    sort_by(&mut app, "Task", true);
    expect_rows(
        &mut app,
        "Task",
        &["Echo", "Delta", "Charlie", "Bravo", "Alpha"],
    );

    find(&mut app, "Alpha", "Perform Find");
    expect_rows(&mut app, "Task", &["Alpha"]);
    app.get_by_label("Found (Unsorted)");
    find(&mut app, "Bravo", "Extend Found Set");
    expect_rows(&mut app, "Task", &["Alpha", "Bravo"]);
    // `>=Bravo` would also find Charlie, Delta and Echo in the whole table:
    // only constraining Alpha and Bravo leaves Bravo alone.
    find(&mut app, ">=Bravo", "Constrain Found Set");
    expect_rows(&mut app, "Task", &["Bravo"]);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A window keeps each table's last sort order, as FileMaker Pro does
/// (#840's reading on this starter): Tasks sorted by Task descending and
/// Assignees by First Name descending each come back in the Sort Records
/// dialog after switching between their layouts with the layout pop-up.
#[test]
#[ignore = "requires the English Tasks template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn tasks_and_assignees_keep_their_own_last_sort() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        [("Task", "Echo"), ("Status", "Postponed")],
        [("Task", "Alpha"), ("Status", "Completed")],
    ];
    let path = seeded(dir.path(), "Tasks", "Task Details", &rows);
    let Opened {
        mut app,
        _server: _host,
    } = open(&path, "Tasks");
    choose_layout(&mut app, "Task Details");
    sort_by(&mut app, "Task", true);
    choose_layout(&mut app, "Assignee Details");
    sort_by(&mut app, "First Name", true);
    assert_eq!(sort_order(&mut app), ["Assignees::First Name"]);
    choose_layout(&mut app, "Task Details");
    assert_eq!(sort_order(&mut app), ["Tasks::Task"]);
    choose_layout(&mut app, "Assignee Details");
    assert_eq!(sort_order(&mut app), ["Assignees::First Name"]);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Content List's "Sort by Status" follows the Status list's order (In
/// Progress, then Archived, Completed, Filed, Sent), not the alphabet.
#[test]
#[ignore = "requires the English Content Management template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn content_management_sorts_by_its_status_list() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        [("Name", "Echo"), ("Status", "Sent")],
        [("Name", "Alpha"), ("Status", "Filed")],
        [("Name", "Charlie"), ("Status", "Archived")],
        [("Name", "Delta"), ("Status", "In Progress")],
        [("Name", "Bravo"), ("Status", "Completed")],
    ];
    let path = seeded(dir.path(), "Content_Management", "Content Details", &rows);
    let Opened {
        mut app,
        _server: _host,
    } = open(&path, "Content");
    app.get_by_label("Total (Sorted)");
    expect_rows(
        &mut app,
        "Name",
        &["Delta", "Charlie", "Bravo", "Alpha", "Echo"],
    );
}

/// Contact List's "Sort by Name" sorts by the Initial calculation. First
/// and last names start with the same letter here, so the order is the
/// same whichever of them Initial reads.
#[test]
#[ignore = "requires the English Contacts template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn contacts_sorts_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let rows = [
        [("First Name", "Charlie"), ("Last Name", "Clark")],
        [("First Name", "Alpha"), ("Last Name", "Adams")],
        [("First Name", "Bravo"), ("Last Name", "Baker")],
    ];
    let path = seeded(dir.path(), "Contacts", "Contact Details", &rows);
    let Opened {
        mut app,
        _server: _host,
    } = open(&path, "Contacts");
    app.get_by_label("Total (Sorted)");
    expect_rows(&mut app, "First Name", &["Alpha", "Bravo", "Charlie"]);
}

/// Field `field` of each row List view shows, as a sorted set.
fn row_set(app: &App, field: &str) -> Vec<String> {
    let mut rows = rows(app, field);
    rows.sort();
    rows
}

/// Wait until List view shows the records of `expected` in `field`, in
/// any order.
fn expect_set(app: &mut App, field: &str, expected: &[&str]) {
    let mut expected: Vec<String> = expected.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    common::wait_until(app, &format!("{field} rows {expected:?}"), |app| {
        row_set(app, field) == expected
    });
}

/// A pointer click at `pos`, as the window gets one.
fn pointer_click(app: &mut App, pos: egui::Pos2) {
    app.event(egui::Event::PointerMoved(pos));
    app.run_steps(1);
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(1);
    }
    app.run_steps(3);
}

/// Go to `layout` through the layout bar's pop-up, as the user does: the
/// UI changes the layout, not a script. The starters keep their layouts in
/// folders, so a folder's submenu is hovered first.
fn choose_layout(app: &mut App, layout: &str) {
    let center = |b: egui::Rect| b.center();
    let combo = app
        .query_all(By::new().role(Role::ComboBox).label("Layout"))
        .next()
        .unwrap()
        .rect();
    pointer_click(app, center(combo));
    let target = |app: &App| {
        app.query_all(By::new().role(Role::Button).label(layout))
            .map(|node| node.rect())
            .find(|rect| rect.min.y > combo.max.y)
    };
    if target(app).is_none() {
        let folders: Vec<egui::Rect> = app
            .query_all(By::new().role(Role::Button))
            .map(|node| node.rect())
            .filter(|rect| {
                rect.min.y > combo.max.y
                    && rect.min.y < combo.max.y + 400.0
                    && rect.min.x >= combo.min.x - 10.0
                    && rect.min.x < combo.max.x
            })
            .collect();
        for folder in folders {
            app.event(egui::Event::PointerMoved(folder.center()));
            app.run_steps(6);
            if target(app).is_some() {
                break;
            }
        }
    }
    let item = target(app).unwrap_or_else(|| panic!("{layout} in the layout pop-up"));
    pointer_click(app, center(item));
    common::wait_until(app, &format!("layout {layout}"), |app| {
        app.query_all(By::new().role(Role::ComboBox).label("Layout"))
            .any(|node| node.value().as_deref() == Some(layout))
    });
    settle(app);
}

/// The fields of the Sort Records dialog's sort order, opened and
/// cancelled.
fn sort_order(app: &mut App) -> Vec<String> {
    common::click(app, "Sort");
    common::wait_for_label(app, "Sort Records");
    app.run_steps(2);
    let order: Vec<String> = app
        .query_all(By::new().role(Role::ListItem))
        .filter_map(|item| item.accesskit_node().label())
        .filter(|label| label.contains("::"))
        .collect();
    app.query_all_by_label("Cancel").last().unwrap().click();
    app.run_steps(2);
    settle(app);
    order
}

/// Records > Sort Records..., the order cleared and the layout's `field`
/// moved into it, ascending or descending.
fn sort_by(app: &mut App, field: &str, descending: bool) {
    common::click(app, "Sort");
    common::wait_for_label(app, "Sort Records");
    common::click(app, "Clear All");
    app.run_steps(2);
    common::click(
        app,
        if descending {
            "Descending order"
        } else {
            "Ascending order"
        },
    );
    // The dialog's row, drawn over the layout's own field.
    app.query_all_by_label(field).last().unwrap().click();
    app.run_steps(2);
    common::click(app, "Move");
    app.run_steps(2);
    app.query_all_by_label("Sort").last().unwrap().click();
    app.run_steps(2);
    settle(app);
}

/// Find mode, `value` as `field`'s criterion, then `command` from the
/// Requests menu.
fn find_in(app: &mut App, field: &str, value: &str, command: &str) {
    common::click(app, "Find");
    common::wait_for_label(app, "Perform Find");
    common::fill(app, field, value);
    common::open_menu(app, "Requests");
    app.get_all_by_label(command)
        .find(|node| node.rect().height() < 30.0)
        .unwrap()
        .click();
    app.run_steps(2);
    common::wait_until(app, "Browse mode", |app| app.state().mode() == Mode::Browse);
    settle(app);
}

fn show_all(app: &mut App) {
    common::click(app, "Show All");
    app.run_steps(2);
    settle(app);
}

/// Go to record 1 of the found set, then Records > Omit Record; the record
/// omitted, read from the top row.
fn omit_first(app: &mut App, field: &str) -> String {
    replace(app, "Record Number", "1");
    app.key_press(egui::Key::Enter);
    app.run_steps(2);
    settle(app);
    let first = rows(app, field).first().cloned().expect("a row");
    common::open_menu(app, "Records");
    common::click(app, "Omit Record");
    app.run_steps(2);
    settle(app);
    first
}

/// Quick Find of `value` from the toolbar.
fn quick_find(app: &mut App, value: &str) {
    replace(app, "Quick Find", value);
    app.key_press(egui::Key::Enter);
    app.run_steps(2);
    settle(app);
}

/// A copy of template `solution` with five records whose `field` is
/// Echo, Alpha, Charlie, Delta and Bravo (added in that order on layout
/// `details`), opened at its list layout through `back`: Show All, the Sort
/// dialog by `field` both ways, Find, Extend and Constrain, Omit Record,
/// Quick Find and Show All again (ROADMAP.md, alpha criterion 3).
fn finds_on(solution: &str, details: &str, back: &str, field: &str) {
    let dir = tempfile::tempdir().unwrap();
    let names = ["Echo", "Alpha", "Charlie", "Delta", "Bravo"];
    let rows: Vec<[(&str, &str); 1]> = names.iter().map(|name| [(field, *name)]).collect();
    let template = template(solution).expect("the template under OM_TEMPLATES or re/extracted");
    let path = dir.path().join(format!("{solution}.fmp12"));
    std::fs::copy(template, &path).unwrap();
    {
        let mut provider = provider();
        provider
            .open_local(&LocalFile {
                path: path.clone(),
                credentials: Some(("Admin".into(), String::new().into())),
            })
            .unwrap();
        let layout = provider
            .layouts()
            .unwrap()
            .into_iter()
            .find(|l| l.name == details)
            .unwrap()
            .id;
        for row in &rows {
            let values: FieldChanges = row
                .iter()
                .map(|(f, v)| ((*f).to_owned(), Some((*v).to_owned())))
                .collect();
            provider.create_record(layout, values).unwrap();
        }
        provider.close();
    }
    let Opened {
        mut app,
        _server: _host,
    } = open(&path, back);

    show_all(&mut app);
    expect_set(&mut app, field, &names);

    sort_by(&mut app, field, false);
    expect_rows(
        &mut app,
        field,
        &["Alpha", "Bravo", "Charlie", "Delta", "Echo"],
    );
    sort_by(&mut app, field, true);
    expect_rows(
        &mut app,
        field,
        &["Echo", "Delta", "Charlie", "Bravo", "Alpha"],
    );

    find_in(&mut app, field, "Alpha", "Perform Find");
    expect_set(&mut app, field, &["Alpha"]);
    find_in(&mut app, field, "Bravo", "Extend Found Set");
    expect_set(&mut app, field, &["Alpha", "Bravo"]);
    find_in(&mut app, field, ">=Bravo", "Constrain Found Set");
    expect_set(&mut app, field, &["Bravo"]);

    show_all(&mut app);
    expect_set(&mut app, field, &names);
    let omitted = omit_first(&mut app, field);
    let left: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| *name != omitted)
        .collect();
    expect_set(&mut app, field, &left);

    quick_find(&mut app, "Charlie");
    expect_set(&mut app, field, &["Charlie"]);

    show_all(&mut app);
    expect_set(&mut app, field, &names);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
#[ignore = "requires the English Assets template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn assets_finds_omits_quick_finds_and_shows_all() {
    finds_on("Assets", "Asset Details", "Assets", "Name");
}

#[test]
#[ignore = "requires the English Contacts template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn contacts_finds_omits_quick_finds_and_shows_all() {
    finds_on("Contacts", "Contact Details", "Contacts", "First Name");
}

#[test]
#[ignore = "requires the English Content Management template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn content_management_finds_omits_quick_finds_and_shows_all() {
    finds_on("Content_Management", "Content Details", "Content", "Name");
}

#[test]
#[ignore = "requires the English Inventory template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn inventory_finds_omits_quick_finds_and_shows_all() {
    finds_on("Inventory", "Product Details", "Inventory", "Name");
}

#[test]
#[ignore = "requires the English Meetings template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn meetings_finds_omits_quick_finds_and_shows_all() {
    finds_on("Meetings", "Meeting Details", "Meetings", "Title");
}

#[test]
#[ignore = "requires the English Tasks template: OM_TEMPLATES or re/extracted/.../DynamicTemplates"]
fn tasks_finds_omits_quick_finds_and_shows_all() {
    finds_on("Tasks", "Task Details", "Tasks", "Task");
}
