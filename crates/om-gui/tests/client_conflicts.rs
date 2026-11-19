//! Another client changes or holds the record a window is editing, or
//! deletes records the window shows, through the GUI end to end: the
//! native provider and an in-process `om-server`
//! hosting the in-memory Tasks file of `om-server/tests/fixtures/tasks.rs`,
//! with the other client on a connection of its own. The window's edit
//! answers to the record as the window showed it when the edit began
//! (recovery.rs): a save over the other client's change is held for the
//! user's choice, a save of a record the other client holds waits for the
//! user to try again, and nothing is sent again by itself. A script acts on
//! the record the window shows, found by its id.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use egui::Role;
use egui_kittest::kittest::Queryable;
use fixture::{ACCOUNT, PASSWORD};
use om_data::runtime::TransactionOptions;
use om_data::{DataProvider, FieldChanges};
use om_scripts::ScriptStep;

const LAYOUT: u32 = fixture::LAYOUT as u32;
const ON_HOLD: &str = "Your unsaved changes are on hold.";

fn host(tag: &str) -> hosted::Hosted {
    // Delete Record/Request [With dialog: Off].
    let mut delete = ScriptStep::new(9);
    delete.set_flag(om_scripts::flag::NO_INTERACT, true);
    let root = fixture::tree(&[fixture::script(1, "Delete", vec![delete])], true);
    let file = fixture::TempFile::from_tree(&format!("gui-conflicts-{tag}"), &root);
    hosted::host_file(
        &format!("conflicts-{tag}"),
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    )
}

/// The window open on the hosted file, in Table view.
fn open(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "the records", |app| {
        cell(app).as_deref() == Some("Write tests")
    });
    app
}

/// The Name cell of the first row (task 1).
fn cell(app: &common::App) -> Option<String> {
    app.query_all_by_role_and_label(Role::TextInput, "Name")
        .next()
        .and_then(|cell| cell.value())
}

/// The first row's Name cell is on screen and empty.
fn emptied(app: &common::App) -> bool {
    app.query_all_by_role_and_label(Role::TextInput, "Name")
        .next()
        .is_some_and(|cell| cell.value().unwrap_or_default().is_empty())
}

/// Type `text` over the first row's Name (empty text empties it).
fn type_name(app: &mut common::App, text: &str) {
    app.get_all_by_role_and_label(Role::TextInput, "Name")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    if text.is_empty() {
        app.key_press(egui::Key::Backspace);
    } else {
        app.event(egui::Event::Text(text.into()));
    }
    app.run_steps(2);
}

fn name(text: &str) -> FieldChanges {
    [(
        "Name".to_owned(),
        (!text.is_empty()).then(|| text.to_owned()),
    )]
    .into_iter()
    .collect()
}

/// Task 1's Name as the host has it, read by a client of its own.
fn stored(hosted: &hosted::Hosted) -> Option<String> {
    let mut reader = hosted.opened();
    let page = reader.record_by_id(LAYOUT, 1).unwrap();
    reader.close();
    let index = page.fields.iter().position(|f| f.name == "Name").unwrap();
    page.records[0].values[index].clone()
}

/// The other client changes task 1's Name.
fn change(other: &mut om_provider::NativeProvider, text: &str) {
    other.records(LAYOUT, 0, 10).unwrap();
    other.update_record(LAYOUT, 1, name(text)).unwrap();
}

/// The other client changes task 1's Name in a transaction it keeps open:
/// the host locks the record for it until the transaction ends.
fn hold(other: &mut om_provider::NativeProvider, text: &str) {
    other.records(LAYOUT, 0, 10).unwrap();
    other
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    other.update_record(LAYOUT, 1, name(text)).unwrap();
}

/// The window's save is held for the user's choice, the bar saying why.
fn held(app: &mut common::App) {
    common::wait_until(app, "the edit held", |app| {
        app.query_by_label("Keep My Changes").is_some() && app.state().is_settled()
    });
    app.get_by_label(ON_HOLD);
    app.get_by_label_contains("was changed on the host");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The window's save went through.
fn saved(app: &mut common::App) {
    common::wait_until(app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !common::editing(app) && app.state().is_settled()
    });
}

/// An emptied Name is refused over the other client's change, held, and
/// kept by the user; a change the other client makes before that save is
/// refused too; a later edit held the same way goes when discarded.
#[test]
fn a_save_over_another_clients_change_waits_for_the_users_choice() {
    let hosted = host("changed");
    let mut app = open(&hosted);
    let mut other = hosted.opened();
    type_name(&mut app, "");
    change(&mut other, "Theirs");

    common::save_record(&mut app);
    held(&mut app);
    assert!(emptied(&app), "the emptied draft stays: {:?}", cell(&app));
    assert_eq!(stored(&hosted).as_deref(), Some("Theirs"));

    // Kept, then changed again on the host before the save: held again.
    common::click(&mut app, "Keep My Changes");
    change(&mut other, "Theirs again");
    common::save_record(&mut app);
    held(&mut app);
    assert!(emptied(&app), "{:?}", cell(&app));
    assert_eq!(stored(&hosted).as_deref(), Some("Theirs again"));
    common::click(&mut app, "Keep My Changes");
    common::save_record(&mut app);
    saved(&mut app);
    assert_eq!(stored(&hosted), None, "the user's choice, saved once");

    type_name(&mut app, "Mine");
    change(&mut other, "Theirs last");
    common::save_record(&mut app);
    held(&mut app);
    common::click(&mut app, "Discard My Changes");
    common::wait_until(&mut app, "the host's value", |app| {
        cell(app).as_deref() == Some("Theirs last") && app.state().is_settled()
    });
    assert!(!common::editing(&app), "nothing unsaved");
    assert_eq!(stored(&hosted).as_deref(), Some("Theirs last"));
    other.close();
}

/// The other client holds the record: the window's save is refused and its
/// edit stays, saved only when the user tries again, after the other
/// client let go. When the other client changed the record before letting
/// go, the retry is held instead.
#[test]
fn a_record_another_client_holds_is_saved_when_the_user_tries_again() {
    let hosted = host("locked");
    let mut app = open(&hosted);
    let mut other = hosted.opened();
    type_name(&mut app, "Mine");
    hold(&mut other, "Abandoned");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| {
        app.query_by_label_contains("Save Record tries again")
            .is_some()
    });
    assert!(
        app.state().error().is_none(),
        "the bar says so, not an alert"
    );
    assert_eq!(cell(&app).as_deref(), Some("Mine"));
    other.revert_transaction().unwrap();
    app.run_steps(20);
    assert_eq!(
        stored(&hosted).as_deref(),
        Some("Write tests"),
        "nothing sent again by itself"
    );
    common::save_record(&mut app);
    saved(&mut app);
    assert_eq!(stored(&hosted).as_deref(), Some("Mine"));

    type_name(&mut app, "Mine again");
    hold(&mut other, "Theirs");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| {
        app.query_by_label_contains("Save Record tries again")
            .is_some()
    });
    other.commit_transaction(false).unwrap();
    common::save_record(&mut app);
    held(&mut app);
    assert_eq!(cell(&app).as_deref(), Some("Mine again"));
    assert_eq!(stored(&hosted).as_deref(), Some("Theirs"));
    common::click(&mut app, "Discard My Changes");
    common::wait_until(&mut app, "the host's value", |app| {
        cell(app).as_deref() == Some("Theirs") && app.state().is_settled()
    });
    other.close();
}

/// The Names of the tasks the host has, read by a client of its own.
fn stored_names(hosted: &hosted::Hosted) -> Vec<Option<String>> {
    let mut reader = hosted.opened();
    let page = reader.records(LAYOUT, 0, 10).unwrap();
    reader.close();
    let index = page.fields.iter().position(|f| f.name == "Name").unwrap();
    page.records
        .into_iter()
        .map(|record| record.values[index].clone())
        .collect()
}

/// The script Delete (Delete Record/Request [With dialog: Off]), run
/// from the Scripts menu.
fn delete_record(app: &mut common::App) {
    common::open_menu(app, "Scripts");
    common::click(app, "Delete");
}

/// The window shows task 2 when another client deletes task 1, the record
/// before it: a script's Delete Record deletes task 2, the record shown,
/// not the one now at its position. Once another client has deleted the
/// record shown, it deletes nothing.
#[test]
fn a_script_deletes_the_record_shown_after_another_client_deletes_one() {
    let hosted = host("deleted");
    let mut other = hosted.opened();
    other.create_record(LAYOUT, name("Third")).unwrap();
    let mut app = open(&hosted);
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "task 2", |app| {
        app.state().is_settled() && common::record_number(app) == "2"
    });
    other.records(LAYOUT, 0, 10).unwrap();
    other.delete_record(LAYOUT, 1).unwrap();

    delete_record(&mut app);
    common::wait_until(&mut app, "the deletion", |app| {
        app.state().is_settled() && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored_names(&hosted), [Some("Third".to_owned())]);

    // The window now shows Third, which the other client deletes.
    other.create_record(LAYOUT, name("Fourth")).unwrap();
    common::wait_until(&mut app, "Third", |app| {
        app.state().is_settled() && cell(app).as_deref() == Some("Third")
    });
    let third = other.records(LAYOUT, 0, 10).unwrap().records[0].id;
    other.delete_record(LAYOUT, third).unwrap();
    delete_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| app.state().error().is_some());
    let error = app.state().error().unwrap();
    assert!(error.contains("Record is missing (error 101)"), "{error}");
    assert_eq!(stored_names(&hosted), [Some("Fourth".to_owned())]);
    other.close();
}

// Delay the delivery of one real native page after the backend has read it.
// The native read, version bookkeeping, retain and write paths are unchanged.
mod delayed_page_tests {
    use super::*;
    use om_data::host::{CalcData, CalcNeed, CalcSchema};
    use om_data::{
        Connection, DataError, HostedFile, LayoutGeometry, LayoutInfo, Record, RecordCapabilities,
        RecordPage, RenderLayout, ScriptCatalog,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    struct Gate {
        armed: AtomicBool,
        fetched: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
        trace: Mutex<Vec<String>>,
    }
    struct PausedRead {
        native: om_provider::NativeProvider,
        gate: Arc<Gate>,
    }
    impl DataProvider for PausedRead {
        fn list_files(&mut self, h: &str, p: u16) -> Result<Vec<HostedFile>, DataError> {
            self.native.list_files(h, p)
        }
        fn open(&mut self, c: &Connection) -> Result<(), DataError> {
            self.native.open(c)
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            self.native.layouts()
        }
        fn records(&mut self, l: u32, o: u64, n: u32) -> Result<RecordPage, DataError> {
            self.records_with_calculations(l, o, n, &[])
        }
        fn records_with_calculations(
            &mut self,
            l: u32,
            o: u64,
            n: u32,
            f: &[String],
        ) -> Result<RecordPage, DataError> {
            let page = self.native.records_with_calculations(l, o, n, f)?;
            if self.gate.armed.swap(false, Ordering::SeqCst) {
                self.gate
                    .trace
                    .lock()
                    .unwrap()
                    .push("native page read finished; reply withheld".into());
                self.gate.fetched.send(()).unwrap();
                self.gate
                    .resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap();
                self.gate
                    .trace
                    .lock()
                    .unwrap()
                    .push("page reply released".into());
            }
            Ok(page)
        }
        fn can_write_records(&self) -> bool {
            self.native.can_write_records()
        }
        fn record_capabilities(&mut self, l: u32) -> Result<RecordCapabilities, DataError> {
            self.native.record_capabilities(l)
        }
        fn layout_geometry(&mut self, l: u32) -> Result<Option<LayoutGeometry>, DataError> {
            self.native.layout_geometry(l)
        }
        fn render_layout(&mut self, l: u32) -> Result<Option<RenderLayout>, DataError> {
            self.native.render_layout(l)
        }
        fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
            self.native.scripts()
        }
        fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
            self.native.calc_schema()
        }
        fn calc_data(&mut self, n: &[CalcNeed]) -> Result<CalcData, DataError> {
            self.native.calc_data(n)
        }
        fn use_window(&mut self, w: u32) {
            self.native.use_window(w);
        }
        fn retain_record(&mut self, l: u32, r: i64, version: Option<u64>) {
            self.gate
                .trace
                .lock()
                .unwrap()
                .push("retain_record runs after backend page read".into());
            self.native.retain_record(l, r, version);
        }
        fn release_record(&mut self, l: u32, r: i64) {
            self.native.release_record(l, r);
        }
        fn record_by_id(&mut self, l: u32, r: i64) -> Result<RecordPage, DataError> {
            self.native.record_by_id(l, r)
        }
        fn update_record(&mut self, l: u32, r: i64, v: FieldChanges) -> Result<Record, DataError> {
            let result = self.native.update_record(l, r, v);
            self.gate.trace.lock().unwrap().push(format!(
                "native update result: {:?}",
                result.as_ref().map(|_| "success").map_err(|e| e.code)
            ));
            result
        }
        fn close(&mut self) {
            self.native.close();
        }
    }

    fn delayed_read(save_before_reply: bool) {
        let hosted = host(if save_before_reply {
            "review-delayed-read"
        } else {
            "review-accepted-read"
        });
        let (fetched_tx, fetched_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let gate = Arc::new(Gate {
            armed: AtomicBool::new(false),
            fetched: fetched_tx,
            resume: Mutex::new(resume_rx),
            trace: Mutex::new(Vec::new()),
        });
        let mut app = common::harness(Box::new(PausedRead {
            native: hosted.provider(),
            gate: Arc::clone(&gate),
        }));
        hosted.fill_form(&mut app);
        common::click(&mut app, "Open");
        common::wait_until(&mut app, "file", |app| {
            assert!(app.state().error().is_none(), "{:?}", app.state().error());
            app.state().is_settled()
        });
        common::click(&mut app, "Table View");
        common::wait_until(&mut app, "old row", |app| {
            cell(app).as_deref() == Some("Write tests")
        });
        let mut other = hosted.opened();
        change(&mut other, "Theirs");
        gate.armed.store(true, Ordering::SeqCst);
        app.key_press_modifiers(
            egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            egui::Key::R,
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while fetched_rx.try_recv().is_err() {
            assert!(Instant::now() < deadline, "reload did not reach provider");
            app.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            cell(&app).as_deref(),
            Some("Write tests"),
            "UI has not accepted backend read"
        );
        type_name(&mut app, "Mine");
        assert_eq!(cell(&app).as_deref(), Some("Mine"));
        if save_before_reply {
            common::save_record(&mut app);
        }
        eprintln!(
            "UI showed Write tests; host Theirs; typed Mine; save_before_reply={save_before_reply}"
        );
        resume_tx.send(()).unwrap();
        if !save_before_reply {
            common::wait_until(&mut app, "page accepted", |app| app.state().is_settled());
            assert_eq!(cell(&app).as_deref(), Some("Mine"));
            common::save_record(&mut app);
        }
        common::wait_until(&mut app, "save outcome", |app| {
            app.state().is_settled()
                && !common::saving(app)
                && (app.query_by_label("Keep My Changes").is_some() || !common::editing(app))
        });
        let persisted = stored(&hosted);
        eprintln!("trace: {:?}", gate.trace.lock().unwrap());
        eprintln!(
            "persisted={persisted:?}, error={:?}, keep={}, save={}",
            app.state().error(),
            app.query_by_label("Keep My Changes").is_some(),
            common::editing(&app)
        );
        other.close();
        assert_eq!(
            persisted.as_deref(),
            Some("Theirs"),
            "must refuse stale edit instead of overwriting unseen host value"
        );
        assert!(
            app.query_by_label("Keep My Changes").is_some(),
            "must hold the stale draft"
        );
    }

    #[test]
    fn an_inflight_page_read_cannot_rebase_a_draft() {
        delayed_read(true);
    }

    #[test]
    fn accepting_a_delayed_page_cannot_rebase_a_draft() {
        delayed_read(false);
    }
}

/// The menus' New Record and Duplicate Record hold the record in the
/// window's record session, as FileMaker Pro holds it under op71 (#892):
/// nothing is stored, and another client cannot write its id. Revert
/// Record is offered on the untouched new record and releases it, deleting
/// nothing, as Pro 26.0.3 removes an untouched new record. A duplicate is
/// stored when it is saved.
#[test]
fn the_menus_new_record_is_held_until_saved_and_its_revert_deletes_nothing() {
    let hosted = host("menu-new");
    let mut app = open(&hosted);
    let before = stored_names(&hosted);
    let held = i64::try_from(before.len()).unwrap() + 1;

    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "the new record", |app| {
        app.state().is_settled() && common::record_number(app) == held.to_string()
    });
    assert_eq!(stored_names(&hosted), before);
    let mut other = hosted.opened();
    other.records(LAYOUT, 0, 10).unwrap();
    assert!(other.update_record(LAYOUT, held, name("other")).is_err());
    other.close();
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the record released", |app| {
        app.state().is_settled() && common::record_number(app) == before.len().to_string()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored_names(&hosted), before);

    // Duplicate Record of the record shown, held until the save.
    let shown = before[before.len() - 1].clone();
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Duplicate Record");
    common::wait_until(&mut app, "the duplicate", |app| {
        app.state().is_settled() && common::record_number(app) == (before.len() + 1).to_string()
    });
    assert_eq!(stored_names(&hosted), before);
    common::save_record(&mut app);
    saved(&mut app);
    let after = stored_names(&hosted);
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(after[before.len()..], [shown]);
}
