//! The alpha scorecard's measure of criterion 1's local half (ROADMAP.md,
//! "Alpha gate": open a local file with the file's own accounts, with
//! privilege sets enforced), on two copies of the Contacts starter
//! solution FileMaker Pro 26 gave accounts to (#156, #137). They stay out of
//! git: name them with `OM_ACCOUNTS_FILE` (accounts `entry` with [Data
//! Entry Only] and `reader` with [Read-Only Access]) and
//! `OM_CUSTOM_SETS_FILE` (accounts `limited`, `nolayout`, `viewonly` and
//! `editonly`, each with a custom set of its name, and two records,
//! Allowed and Denied; passwords are the account names).
//!
//! Each check runs on a fresh copy:
//!
//! - through the provider calls the window makes (`open_local`,
//!   `record_capabilities`, `create_record`, `update_record`,
//!   `delete_record`, `records`, `layouts`, `layout_menu`): a wrong
//!   password is refused; `reader` cannot type, create or delete; `entry`
//!   edits a record; `limited` sees one of the two records; `nolayout`
//!   does not get Contact List; `viewonly` cannot change Last Name;
//! - through the window (File > Open and its account dialog): `entry`
//!   cannot reach Layout mode or Manage Database; `editonly` gets the
//!   "Editing only" commands FileMaker Pro gives it: what Pro disables is
//!   disabled, and a Last Name edit commits.
//!
//! It counts what holds and writes `privileges.md` to `OM_SCORECARD_OUT`
//! (or prints it); a check that does not hold is reported, not a test
//! failure. Nothing is compared with FileMaker Pro here: what each set
//! allows is FileMaker Pro's own definition, read back in its Manage
//! Security by the fixtures' author.
mod common;

use std::path::{Path, PathBuf};

use common::App;
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use egui_kittest::Harness;
use om_data::{DataProvider, FieldChanges, LocalFile, LocalOpen};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::OpenMakerApp;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};

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

/// One check: what it is, whether it holds (`None`: not yet possible), and
/// what was seen.
struct Check {
    name: &'static str,
    holds: Option<bool>,
    detail: String,
}

/// A fresh copy of `original` in `dir`, named after it.
fn copy(original: &Path, dir: &Path, tag: &str) -> PathBuf {
    let path = dir.join(format!(
        "{tag}-{}",
        original.file_name().unwrap().to_string_lossy()
    ));
    std::fs::copy(original, &path).expect("copy the fixture");
    path
}

/// `path` opened with `account` and `password` through the provider, or
/// why not.
fn open_as(path: &Path, account: &str, password: &str) -> Result<NativeProvider, String> {
    let mut p = provider();
    match p.open_local(&LocalFile {
        path: path.to_owned(),
        credentials: Some((account.into(), password.to_owned().into())),
    }) {
        Ok(LocalOpen::Opened { .. }) => Ok(p),
        Ok(LocalOpen::NeedsLogin { account }) => Err(format!("asked again (for “{account}”)")),
        Err(e) => Err(e.message),
    }
}

fn layout_id(p: &mut NativeProvider, name: &str) -> Result<u32, String> {
    p.layouts()
        .map_err(|e| e.message)?
        .into_iter()
        .find(|l| l.name == name)
        .map(|l| l.id)
        .ok_or_else(|| format!("no layout “{name}”"))
}

/// Field `field` of every record layout `layout` shows.
fn values(p: &mut NativeProvider, layout: u32, field: &str) -> Result<Vec<(i64, String)>, String> {
    let page = p.records(layout, 0, 1000).map_err(|e| e.message)?;
    let column = page
        .fields
        .iter()
        .position(|f| f.name == field)
        .ok_or_else(|| format!("the layout does not show {field}"))?;
    Ok(page
        .records
        .iter()
        .map(|r| {
            (
                r.id,
                r.values.get(column).cloned().flatten().unwrap_or_default(),
            )
        })
        .collect())
}

fn change(field: &str, value: &str) -> FieldChanges {
    FieldChanges::from([(field.to_owned(), Some(value.to_owned()))])
}

fn wrong_password(file: &Path, dir: &Path) -> Check {
    let path = copy(file, dir, "wrong");
    let detail = match open_as(&path, "reader", "not the password") {
        Ok(mut p) => {
            p.close();
            Err("opened".to_owned())
        }
        Err(e) => Ok(format!("refused: {e}")),
    };
    Check {
        name: "A wrong password is refused",
        holds: Some(detail.is_ok()),
        detail: detail.unwrap_or_else(|e| e),
    }
}

/// A copy of the accounts file with one record, made as Admin.
fn seeded(file: &Path, dir: &Path, tag: &str) -> Result<(PathBuf, i64), String> {
    let path = copy(file, dir, tag);
    let mut p = open_as(&path, "Admin", "")?;
    let layout = layout_id(&mut p, "Contact Details")?;
    let record = p
        .create_record(layout, change("First Name", "Seeded"))
        .map_err(|e| format!("seeding as Admin: {}", e.message))?;
    p.close();
    Ok((path, record.id))
}

fn reader_cannot_change(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let (path, record) = seeded(file, dir, "reader")?;
        let mut p = open_as(&path, "reader", "reader")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        let caps = p.record_capabilities(layout).map_err(|e| e.message)?;
        let typed = p.update_record(layout, record, change("First Name", "Typed"));
        let created = p.create_record(layout, FieldChanges::new());
        let deleted = p.delete_record(layout, record);
        p.close();
        let refused = typed.is_err() && created.is_err() && deleted.is_err();
        let offered = caps.update || caps.create || caps.delete;
        Ok((
            refused && !offered,
            format!(
                "capabilities update {} create {} delete {}; typing {}, New Record {}, Delete Record {}",
                caps.update,
                caps.create,
                caps.delete,
                if typed.is_err() { "refused" } else { "accepted" },
                if created.is_err() { "refused" } else { "accepted" },
                if deleted.is_err() { "refused" } else { "accepted" },
            ),
        ))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "reader cannot type, create or delete",
        holds: Some(holds),
        detail,
    }
}

fn entry_edits(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let (path, record) = seeded(file, dir, "entry")?;
        let mut p = open_as(&path, "entry", "entry")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        p.update_record(layout, record, change("First Name", "Edited"))
            .map_err(|e| format!("the edit was refused: {}", e.message))?;
        p.close();
        // Read back as Admin, on a reopened file.
        let mut p = open_as(&path, "Admin", "")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        let read = values(&mut p, layout, "First Name")?;
        p.close();
        let holds = read.iter().any(|(id, v)| *id == record && v == "Edited");
        Ok((holds, format!("First Name reads {read:?} after reopening")))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "entry edits records",
        holds: Some(holds),
        detail,
    }
}

fn limited_sees_one(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let path = copy(file, dir, "limited");
        let mut p = open_as(&path, "limited", "limited")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        let read = values(&mut p, layout, "First Name")?;
        p.close();
        // A record the view formula rejects stays listed with its fields
        // withheld, as FileMaker lists one it shows as <No Access> (#639).
        let shown: Vec<&str> = read
            .iter()
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
            .collect();
        Ok((
            shown == ["Allowed"],
            format!(
                "{} records listed; First Name shown: {shown:?}, withheld on the rest",
                read.len()
            ),
        ))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "limited sees one of the two records",
        holds: Some(holds),
        detail,
    }
}

fn nolayout_lacks_contact_list(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let path = copy(file, dir, "nolayout");
        let mut p = open_as(&path, "nolayout", "nolayout")?;
        let listed = p
            .layouts()
            .map_err(|e| e.message)?
            .iter()
            .any(|l| l.name == "Contact List");
        let in_menu = p
            .layout_menu()
            .map_err(|e| e.message)?
            .iter()
            .any(|l| l.name == "Contact List");
        // Contact Details' own button goes to Contact List: whether the
        // layout still loads there.
        let loads = match layout_id(&mut p, "Contact List") {
            Ok(id) => match p.render_layout(id) {
                Ok(Some(_)) => "renders".to_owned(),
                Ok(None) => "does not render".to_owned(),
                Err(e) => format!("refused: {}", e.message),
            },
            Err(e) => e,
        };
        p.close();
        Ok((
            !in_menu && !loads.starts_with("renders"),
            format!(
                "Contact List in the Layouts menu: {in_menu}; in the layout list scripts use: {listed}; the layout itself {loads}"
            ),
        ))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "nolayout does not get Contact List",
        holds: Some(holds),
        detail,
    }
}

fn viewonly_keeps_last_name(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let path = copy(file, dir, "viewonly");
        let mut p = open_as(&path, "viewonly", "viewonly")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        let page = p.records(layout, 0, 1).map_err(|e| e.message)?;
        let record = page.records.first().ok_or("no record")?.id;
        let caps = p.record_capabilities(layout).map_err(|e| e.message)?;
        let field = |name: &str| page.fields.iter().find(|f| f.name == name).cloned();
        let last = field("Last Name").ok_or("the layout does not show Last Name")?;
        let first = field("First Name").ok_or("the layout does not show First Name")?;
        let last_edit = p.update_record(layout, record, change("Last Name", "Changed"));
        let first_edit = p.update_record(layout, record, change("First Name", "Allowed"));
        p.close();
        let holds = !caps.can_edit(&last) && last_edit.is_err() && first_edit.is_ok();
        Ok((
            holds,
            format!(
                "Last Name offered for typing: {}, its edit {}; First Name offered: {}, its edit {}",
                caps.can_edit(&last),
                if last_edit.is_err() { "refused" } else { "accepted" },
                caps.can_edit(&first),
                if first_edit.is_ok() { "accepted" } else { "refused" },
            ),
        ))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "viewonly cannot change Last Name",
        holds: Some(holds),
        detail,
    }
}

fn app() -> App {
    Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(|_cc| {
            OpenMakerApp::with_license_store(Box::new(provider()), Box::new(NoLicense))
        })
}

fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// Step until `done` or an error alert, for up to 30 s.
fn wait(app: &mut App, mut done: impl FnMut(&App) -> bool) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.step();
        if let Some(error) = app.state().error() {
            return Err(error.to_owned());
        }
        if done(app) {
            app.run_steps(3);
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err("timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// File > Open `path` in the window, answering its account dialog with
/// `account` and `password`. Shift is held as the file is chosen: the
/// copies' File Options sign in as Admin without asking, unless Shift or
/// Option is held (local-files.md 3.1).
fn open_window(path: &Path, account: &str, password: &str) -> Result<App, String> {
    let mut app = app();
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Open...");
    common::wait_for_label(&mut app, "Open File");
    replace(&mut app, "Path", &path.display().to_string());
    app.get_by_label("Open")
        .click_modifiers(egui::Modifiers::SHIFT);
    app.run_steps(2);
    wait(&mut app, |app| app.query_by_label("Account Name").is_some())?;
    replace(&mut app, "Account Name", account);
    common::fill(&mut app, "Password", password);
    common::click(&mut app, "OK");
    // Open and settled, or refused: an alert, or the dialog still asking
    // after 20 s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        app.step();
        if let Some(error) = app.state().error() {
            return Err(error.to_owned());
        }
        if app.state().is_open() && app.state().is_settled() {
            app.run_steps(3);
            break;
        }
        if std::time::Instant::now() > deadline {
            return Err(if app.query_by_label("Account Name").is_some() {
                "the account dialog is still asking after 20 s".into()
            } else {
                "the file did not open within 20 s".into()
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(app)
}

/// Whether menu `menu`'s item `item` (in `submenu`, if given) is enabled.
fn enabled(app: &mut App, menu: &str, submenu: Option<&str>, item: &str) -> Option<bool> {
    common::open_menu(app, menu);
    if let Some(submenu) = submenu {
        app.query_by_label(submenu)?.hover();
        app.run_steps(3);
    }
    let node = app
        .query_all(By::new().label(item))
        .find(|n| n.accesskit_node().role() == egui::Role::Button)?;
    let enabled = !node.accesskit_node().is_disabled();
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    Some(enabled)
}

fn entry_no_design(file: &Path, dir: &Path) -> Check {
    let result = (|| -> Result<(bool, String), String> {
        let path = copy(file, dir, "entry-window");
        let mut app = open_window(&path, "entry", "entry")?;
        let layout = enabled(&mut app, "View", None, "Layout Mode");
        let database = enabled(&mut app, "File", Some("Manage"), "Database...");
        Ok((
            layout == Some(false) && database == Some(false),
            format!(
                "View > Layout Mode enabled: {}; File > Manage > Database... enabled: {}",
                layout.map_or("not shown".into(), |e| e.to_string()),
                database.map_or("not shown".into(), |e| e.to_string())
            ),
        ))
    })();
    let (holds, detail) = result.unwrap_or_else(|e| (false, e));
    Check {
        name: "entry cannot reach Layout mode or Manage Database",
        holds: Some(holds),
        detail,
    }
}

/// Whether the window shows control `label` (of `role`) disabled; `None`
/// when it is not shown. The tallest one: a toolbar button, not a menu's
/// item of the same name.
fn control_disabled(app: &App, role: egui::Role, label: &str) -> Option<bool> {
    app.query_all(By::new().role(role).label(label))
        .max_by(|a, b| a.rect().height().total_cmp(&b.rect().height()))
        .map(|n| n.accesskit_node().is_disabled())
}

/// Of menu `menu`'s `items` (each in its `submenu`, if any), those the
/// window offers enabled; those it does not show, and a menu disabled
/// whole (which offers none), are noted in `seen`.
fn enabled_items(
    app: &mut App,
    menu: &str,
    items: &[(Option<&str>, &str)],
    seen: &mut Vec<String>,
) -> Vec<String> {
    let menu_disabled = app
        .query_all(By::new().role(egui::Role::Button).label(menu))
        .next()
        .is_some_and(|n| n.accesskit_node().is_disabled());
    if menu_disabled {
        seen.push(format!("the {menu} menu is disabled"));
        return Vec::new();
    }
    let mut enabled = Vec::new();
    for (submenu, item) in items {
        common::open_menu(app, menu);
        if let Some(submenu) = submenu {
            match app.query_by_label(submenu) {
                // A disabled submenu offers none of its items.
                Some(node) if node.accesskit_node().is_disabled() => {
                    let note = format!("{menu} > {submenu} is disabled");
                    if !seen.contains(&note) {
                        seen.push(note);
                    }
                    app.key_press(egui::Key::Escape);
                    app.run_steps(2);
                    continue;
                }
                Some(node) => {
                    node.hover();
                    app.run_steps(3);
                }
                None => {}
            }
        }
        // The menu's item: shorter than a toolbar button of the same name.
        let found: Vec<bool> = app
            .query_all(By::new().role(egui::Role::Button).label(item))
            .filter(|n| n.rect().height() < 30.0)
            .map(|n| !n.accesskit_node().is_disabled())
            .collect();
        if found.is_empty() {
            seen.push(format!("{menu} > {item} not shown"));
        }
        if found.iter().any(|on| *on) {
            enabled.push(format!(
                "{menu} > {}{item}",
                submenu.map_or(String::new(), |s| format!("{s} > "))
            ));
        }
        app.key_press(egui::Key::Escape);
        app.run_steps(2);
    }
    enabled
}

/// What FileMaker Pro 26.0.3 disables for `editonly`, whose set's
/// available menu commands are "Editing only" (#137, the 16:30Z reading):
/// the toolbar's New, Delete, Find, Sort, Quick Find, Preview and view
/// controls; the Records menu's record, omit, find, sort and field-wide
/// commands; the File menu's print, import, export, save-as and copy
/// commands. OpenMaker disabling more (the layout bar, record navigation)
/// is not counted against it here; a Last Name edit must still commit.
fn editonly_editing_only(file: &Path, dir: &Path) -> Check {
    const NAME: &str = "editonly has the Editing only commands FileMaker Pro gives it";
    let edit = (|| -> Result<(), String> {
        let path = copy(file, dir, "editonly");
        let mut p = open_as(&path, "editonly", "editonly")?;
        let layout = layout_id(&mut p, "Contact Details")?;
        let record = p
            .records(layout, 0, 1)
            .map_err(|e| e.message)?
            .records
            .first()
            .ok_or("no record")?
            .id;
        let edited = p.update_record(layout, record, change("Last Name", "Edited"));
        p.close();
        edited
            .map(|_| ())
            .map_err(|e| format!("the Last Name edit was refused: {}", e.message))
    })();
    let path = copy(file, dir, "editonly-window");
    let mut app = match open_window(&path, "editonly", "editonly") {
        Ok(app) => app,
        Err(e) => {
            return Check {
                name: NAME,
                holds: Some(false),
                detail: format!("the window's open is refused ({e})"),
            }
        }
    };
    let mut enabled = Vec::new();
    let mut seen = Vec::new();
    for (role, label) in [
        (egui::Role::Button, "New Record"),
        (egui::Role::Button, "Delete Record"),
        (egui::Role::Button, "Find"),
        (egui::Role::Button, "Sort"),
        (egui::Role::TextInput, "Quick Find"),
        (egui::Role::Button, "Form View"),
        (egui::Role::Button, "List View"),
        (egui::Role::Button, "Table View"),
        (egui::Role::Button, "Preview"),
    ] {
        match control_disabled(&app, role, label) {
            Some(false) => enabled.push(format!("toolbar {label}")),
            Some(true) => {}
            None => seen.push(format!("toolbar {label} not shown")),
        }
    }
    enabled.extend(enabled_items(
        &mut app,
        "Records",
        &[
            (None, "New Record"),
            (None, "Duplicate Record"),
            (None, "Delete Record..."),
            (None, "Omit Record"),
            (None, "Omit Multiple..."),
            (None, "Modify Last Find"),
            (None, "Sort Records..."),
            (None, "Unsort"),
            (None, "Replace Field Contents..."),
            (None, "Relookup Field Contents"),
        ],
        &mut seen,
    ));
    enabled.extend(enabled_items(
        &mut app,
        "File",
        &[
            (None, "Page Setup..."),
            (None, "Print..."),
            (Some("Import Records"), "File..."),
            (None, "Export Records..."),
            (Some("Save/Send Records As"), "Excel..."),
            (Some("Save/Send Records As"), "PDF..."),
            (Some("Save/Send Records As"), "Snapshot Link..."),
            (None, "Save a Copy As..."),
        ],
        &mut seen,
    ));
    let holds = enabled.is_empty() && edit.is_ok();
    Check {
        name: NAME,
        holds: Some(holds),
        detail: format!(
            "opens; {}; {}{}",
            match &edit {
                Ok(()) => "a Last Name edit commits".to_owned(),
                Err(e) => e.clone(),
            },
            if enabled.is_empty() {
                "every control FileMaker Pro disables is disabled".to_owned()
            } else {
                format!(
                    "enabled where FileMaker Pro disables: {}",
                    enabled.join(", ")
                )
            },
            if seen.is_empty() {
                String::new()
            } else {
                format!(" ({})", seen.join("; "))
            }
        ),
    }
}

#[test]
#[ignore = "requires OM_ACCOUNTS_FILE (#156's Contacts copy with entry and reader) and OM_CUSTOM_SETS_FILE (#137's Contacts-custom copy)"]
fn privilege_measure() {
    let accounts = PathBuf::from(std::env::var_os("OM_ACCOUNTS_FILE").expect("OM_ACCOUNTS_FILE"));
    let custom =
        PathBuf::from(std::env::var_os("OM_CUSTOM_SETS_FILE").expect("OM_CUSTOM_SETS_FILE"));
    let dir = tempfile::tempdir().unwrap();
    let checks = [
        wrong_password(&accounts, dir.path()),
        reader_cannot_change(&accounts, dir.path()),
        entry_edits(&accounts, dir.path()),
        entry_no_design(&accounts, dir.path()),
        limited_sees_one(&custom, dir.path()),
        nolayout_lacks_contact_list(&custom, dir.path()),
        viewonly_keeps_last_name(&custom, dir.path()),
        editonly_editing_only(&custom, dir.path()),
    ];
    let held = checks.iter().filter(|c| c.holds == Some(true)).count();
    let pending = checks.iter().filter(|c| c.holds.is_none()).count();
    let mut md = format!(
        "Criterion 1, local: {held} of {} checks hold ({pending} not yet possible).\n\n| Check | Holds | Seen |\n|---|---|---|\n",
        checks.len()
    );
    for c in &checks {
        md.push_str(&format!(
            "| {} | {} | {} |\n",
            c.name,
            match c.holds {
                Some(true) => "yes",
                Some(false) => "**no**",
                None => "not yet",
            },
            c.detail.replace('|', "\\|").replace('\n', " ")
        ));
    }
    match std::env::var_os("OM_SCORECARD_OUT") {
        Some(out) => {
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(Path::new(&out).join("privileges.md"), &md).unwrap();
        }
        None => println!("{md}"),
    }
}
