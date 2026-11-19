//! The window when the open file's connection to the host is lost
//! (network-protocol.md 6.2, 6.3): it says so, keeps the records it shows
//! and the unsaved edits, and asks the host for nothing more. Scripts stop
//! wherever they are, and nothing starts new ones.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{click, connect_to_mock, harness, record_number, select_layout, wait_until};
use egui::Role;
use egui_kittest::kittest::{By, Queryable};
use om_data::formula::{compile, formatted, polish, Context, NameTable};
use om_data::host::{CalcData, CalcNeed, CalcSchema};
use om_data::meta::FileOptions;
use om_data::mock::MockProvider;
use om_data::runtime::TransactionOptions;
use om_data::{
    Calculation, Connection, DataError, DataProvider, FieldChanges, HostedFile, LayoutGeometry,
    LayoutInfo, Record, RecordCapabilities, RecordPage, RenderLayout, Script, ScriptCatalog,
    ScriptStep, StoredKey,
};

const BANNER: &str = "The connection to the host was lost.";

/// The mock, until `lost` is set; from then on every call fails as on a
/// broken connection, and is counted. It has [`scripts`], and the file
/// the document options (file triggers) given. Its transactions hold
/// their changes back, and committing one loses the connection.
struct Dropping {
    mock: MockProvider,
    lost: Arc<AtomicBool>,
    refused: Arc<AtomicUsize>,
    options: Option<FileOptions>,
    /// The changes of the open transaction.
    transaction: Option<Vec<FieldChanges>>,
    /// While set, a call waits (the worker is stuck in it).
    hold: Arc<AtomicBool>,
    host: Arc<Host>,
    /// Record versions as the native provider keeps them: the host's, the
    /// one a save is checked against (as last read), and the records whose
    /// version a read by id or a retained edit kept, which page reads
    /// leave.
    versions: std::collections::BTreeMap<i64, u64>,
    seen: std::collections::BTreeMap<i64, u64>,
    pinned: std::collections::BTreeSet<i64>,
    /// A record was read by id since the last page read.
    read_back: bool,
}

impl Dropping {
    fn version(&self, record: i64) -> u64 {
        self.versions.get(&record).copied().unwrap_or_default()
    }
    fn changed(&mut self, record: i64) {
        *self.versions.entry(record).or_default() += 1;
    }
}

/// What happens on the host while the client is away, and what reaches it.
#[derive(Default)]
struct Host {
    /// Another client's changes, made while the connection is lost (by
    /// record; `None` deletes it): the host has them by the time the
    /// file opens again.
    meanwhile: Mutex<Vec<(i64, Option<FieldChanges>)>>,
    /// The next save reaches the host, and then the connection goes
    /// before its answer comes back.
    landing: AtomicBool,
    /// Every save that reached the host, by record.
    saved: Mutex<Vec<(i64, FieldChanges)>>,
    /// Another client's changes, made right after the reconnect read the
    /// records back (before the window's next page read).
    after_check: Mutex<Vec<(i64, FieldChanges)>>,
    /// Every attempt to open the file.
    opens: AtomicUsize,
}

impl Dropping {
    fn reach(&self) -> Result<(), DataError> {
        while self.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.lost.load(Ordering::SeqCst) {
            self.refused.fetch_add(1, Ordering::SeqCst);
            return Err(DataError::new("io: Broken pipe (os error 32)"));
        }
        Ok(())
    }
}

impl DataProvider for Dropping {
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        self.mock.list_files(host, port)
    }
    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.host.opens.fetch_add(1, Ordering::SeqCst);
        if connection.password != "secret" {
            return Err(DataError::with_code(
                212,
                "authenticate to hosted file: The server rejected login.",
            ));
        }
        self.mock.open(connection)?;
        self.lost.store(false, Ordering::SeqCst);
        // A fresh session has read nothing yet.
        (self.seen, self.pinned, self.read_back) = Default::default();
        let meanwhile = std::mem::take(&mut *self.host.meanwhile.lock().unwrap());
        for (record, change) in meanwhile {
            match change {
                Some(values) => self.mock.update_record(1, record, values).map(drop)?,
                None => self.mock.delete_record(1, record)?,
            }
            self.changed(record);
        }
        Ok(())
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn record_by_id(&mut self, layout: u32, record: i64) -> Result<RecordPage, DataError> {
        self.reach()?;
        let page = self.mock.record_by_id(layout, record)?;
        if !page.records.is_empty() {
            self.seen.insert(record, self.version(record));
            self.pinned.insert(record);
        }
        self.read_back = true;
        Ok(page)
    }
    fn retain_record(&mut self, _: u32, record: i64, _: Option<u64>) {
        // The window's edit answers to the version its page showed.
        if self.seen.contains_key(&record) {
            self.pinned.insert(record);
        }
    }
    fn release_record(&mut self, _: u32, record: i64) {
        self.pinned.remove(&record);
    }
    fn record_position(&mut self, layout: u32, record: i64) -> Result<Option<u64>, DataError> {
        self.reach()?;
        self.mock.record_position(layout, record)
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.reach()?;
        self.mock.layouts()
    }
    fn records(&mut self, layout: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.reach()?;
        if std::mem::take(&mut self.read_back) {
            let after_check = std::mem::take(&mut *self.host.after_check.lock().unwrap());
            for (record, values) in after_check {
                self.mock.update_record(layout, record, values)?;
                self.changed(record);
            }
        }
        let page = self.mock.records(layout, offset, limit)?;
        for record in &page.records {
            if !self.pinned.contains(&record.id) {
                self.seen.insert(record.id, self.version(record.id));
            }
        }
        Ok(page)
    }
    fn record_capabilities(&mut self, layout: u32) -> Result<RecordCapabilities, DataError> {
        self.reach()?;
        self.mock.record_capabilities(layout)
    }
    fn layout_geometry(&mut self, layout: u32) -> Result<Option<LayoutGeometry>, DataError> {
        self.reach()?;
        self.mock.layout_geometry(layout)
    }
    fn render_layout(&mut self, layout: u32) -> Result<Option<RenderLayout>, DataError> {
        self.reach()?;
        self.mock.render_layout(layout)
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        self.reach()?;
        Ok(scripts())
    }
    fn update_record(
        &mut self,
        layout: u32,
        record: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        self.reach()?;
        if let Some(changes) = &mut self.transaction {
            changes.push(values);
            let page = self.mock.records(layout, 0, 100)?;
            return Ok(page.records.into_iter().find(|r| r.id == record).unwrap());
        }
        if self
            .seen
            .get(&record)
            .is_some_and(|seen| *seen != self.version(record))
        {
            return Err(DataError::with_code(
                306,
                "Another client changed this record. Revert or refresh to load its changes \
                 before saving.",
            ));
        }
        let saved = self.mock.update_record(layout, record, values.clone())?;
        self.changed(record);
        self.seen.insert(record, self.version(record));
        self.pinned.remove(&record);
        self.host.saved.lock().unwrap().push((record, values));
        if self.host.landing.swap(false, Ordering::SeqCst) {
            self.lost.store(true, Ordering::SeqCst);
            return Err(DataError::new("io: Broken pipe (os error 32)"));
        }
        Ok(saved)
    }
    fn begin_transaction(&mut self, _: TransactionOptions) -> Result<(), DataError> {
        self.reach()?;
        self.transaction = Some(Vec::new());
        Ok(())
    }
    fn commit_transaction(&mut self, _: bool) -> Result<(), DataError> {
        self.reach()?;
        // The connection goes while the commit is on its way.
        self.transaction = None;
        self.lost.store(true, Ordering::SeqCst);
        Err(DataError::new("io: Broken pipe (os error 32)"))
    }
    fn revert_transaction(&mut self) -> Result<(), DataError> {
        self.reach()?;
        self.transaction = None;
        Ok(())
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        self.reach()?;
        let schema = self.mock.calc_schema()?;
        Ok(match (&self.options, schema) {
            (Some(options), Some(schema)) => {
                let mut schema = (*schema).clone();
                schema.file_options = Some(options.clone());
                Some(Arc::new(schema))
            }
            (_, schema) => schema,
        })
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.reach()?;
        self.mock.calc_data(needs)
    }
    fn connection_lost(&self) -> Option<DataError> {
        self.lost.load(Ordering::SeqCst).then(|| {
            DataError::connection_lost(
                "The connection to the host was lost (the host closed the connection).",
            )
        })
    }
    fn close(&mut self) {
        self.lost.store(false, Ordering::SeqCst);
        self.mock.close();
    }
}

/// A window open on the mock's file, and its provider's switches.
fn open() -> (common::App, Arc<AtomicBool>, Arc<AtomicUsize>) {
    open_with(None)
}

/// [`open`], the file with document `options`.
fn open_with(options: Option<FileOptions>) -> (common::App, Arc<AtomicBool>, Arc<AtomicUsize>) {
    open_holding(options, Arc::default())
}

/// [`open_with`], the provider's calls waiting while `hold` is set.
fn open_holding(
    options: Option<FileOptions>,
    hold: Arc<AtomicBool>,
) -> (common::App, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let (app, lost, refused, _) = open_hosted(options, hold);
    (app, lost, refused)
}

/// [`open_holding`], and what happens on the host.
fn open_hosted(
    options: Option<FileOptions>,
    hold: Arc<AtomicBool>,
) -> (common::App, Arc<AtomicBool>, Arc<AtomicUsize>, Arc<Host>) {
    let (lost, refused, host) = (Arc::default(), Arc::default(), Arc::<Host>::default());
    let mut app = harness(Box::new(Dropping {
        mock: MockProvider::new(),
        lost: Arc::clone(&lost),
        refused: Arc::clone(&refused),
        options,
        transaction: None,
        hold,
        host: Arc::clone(&host),
        versions: Default::default(),
        seen: Default::default(),
        pinned: Default::default(),
        read_back: false,
    }));
    connect_to_mock(&mut app);
    // The Scripts menu has read the catalog.
    common::open_menu(&mut app, "Scripts");
    common::wait_for_label(&mut app, "Pause");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    (app, lost, refused, host)
}

// ----- scripts ----------------------------------------------------------------

fn calc(source: &str) -> Calculation {
    let names = NameTable::default();
    let compiled = compile(source, &Context::new(&names)).expect("formula compiles");
    Calculation::new(
        Some(formatted::encode(&compiled.tokens)),
        polish::encode(&compiled.expr),
    )
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    }
}

const TICK: u64 = 5;
const LOSE_CLOSING: u64 = 7;

/// Pause (indefinitely); Spin (Allow User Abort Off, then an endless
/// loop); Ask (Show Custom Dialog); Install timer (OnTimer running Tick
/// every 0.2 s); Tick (does nothing); Lose commit (Set Error Capture On,
/// Open Transaction, Pause, Commit Transaction); Lose closing (Lose
/// commit with Allow User Abort Off, for OnWindowClose).
fn scripts() -> ScriptCatalog {
    let mut endless = vec![ScriptStep::new(71), ScriptStep::new(73)];
    endless[0].value = 2;
    endless[1].value = 0;
    let mut spin = vec![ScriptStep::new(85)];
    spin.extend(endless);
    let mut dialog = ScriptStep::new(87);
    dialog.set_calculation(0, Some(&calc("\"Hello\"")));
    dialog.set_calculation(1, Some(&calc("\"Still there?\"")));
    dialog.set_text(0, Some("OK"));
    let mut capture = ScriptStep::new(86);
    capture.flags |= om_data::flag::TURN_ON;
    let (mut begin, mut commit) = (ScriptStep::new(205), ScriptStep::new(206));
    // Both name the Commit Transaction step.
    (begin.value, commit.value) = (3, 3);
    let lose_commit = vec![capture.clone(), begin, ScriptStep::new(62), commit];
    let (mut begin, mut commit) = (ScriptStep::new(205), ScriptStep::new(206));
    (begin.value, commit.value) = (4, 4);
    let lose_closing = vec![
        capture,
        ScriptStep::new(85),
        begin,
        ScriptStep::new(62),
        commit,
    ];
    let mut timer = ScriptStep::new(148);
    timer.set_script_key(StoredKey::new(&[1, TICK as u8]));
    timer.set_calculation(1, Some(&calc("0.2")));
    ScriptCatalog {
        scripts: vec![
            script(1, "Pause", vec![ScriptStep::new(62)]),
            script(2, "Spin", spin),
            script(3, "Ask", vec![dialog]),
            script(4, "Install timer", vec![timer]),
            script(TICK, "Tick", Vec::new()),
            script(6, "Lose commit", lose_commit),
            script(LOSE_CLOSING, "Lose closing", lose_closing),
        ],
        ..Default::default()
    }
}

fn run(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    click(app, name);
}

/// The banner is up and no script runs.
fn stopped_and_told(app: &common::App) -> bool {
    app.query_by_label(BANNER).is_some() && !app.state().script_running()
}

fn layout_shown(app: &common::App) -> Option<String> {
    app.get_by_role_and_label(Role::ComboBox, "Layout").value()
}

/// The value of the First Name cell of the current row in Table view.
fn first_name(app: &common::App) -> Option<String> {
    app.query_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .and_then(|cell| cell.value())
}

#[test]
fn an_idle_window_learns_the_connection_was_lost_and_keeps_its_edits() {
    let (mut app, lost, refused) = open();
    click(&mut app, "Table View");
    app.get_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Unsaved draft".into()));
    app.run_steps(2);
    assert!(common::unsaved(&app), "unsaved record changes");

    // Nothing is asked of the host: the worker notices on its own.
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    assert!(app.state().connection_lost().is_some());
    assert!(app.state().error().is_none(), "the banner, not an alert");
    app.get_by_label_contains("1 unsaved field change is kept");
    assert_eq!(record_number(&app), "1");
    assert_eq!(first_name(&app).as_deref(), Some("Unsaved draft"));

    // Saving, moving and switching layouts do nothing now.
    let before = refused.load(Ordering::SeqCst);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    click(&mut app, "Next");
    select_layout(&mut app, "Contacts List");
    app.run_steps(5);
    assert_eq!(refused.load(Ordering::SeqCst), before, "a request went out");
    assert_eq!(record_number(&app), "1");
    assert_eq!(layout_shown(&app).as_deref(), Some("Contact Details"));
    assert_eq!(first_name(&app).as_deref(), Some("Unsaved draft"));

    // Closing the file is still possible, and starts over.
    click(&mut app, "Close File");
    wait_until(&mut app, "the Hosts dialog", |app| {
        app.query_by_label("Hosts").is_some()
    });
    assert!(!app.state().is_open());
    assert!(app.state().connection_lost().is_none());
}

#[test]
fn a_layout_switch_that_finds_the_connection_lost_keeps_the_last_layout() {
    let (mut app, lost, refused) = open();
    // The first request of the switch fails; the window must not go blank.
    lost.store(true, Ordering::SeqCst);
    select_layout(&mut app, "Contacts List");
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    app.run_steps(5);
    let refused_then = refused.load(Ordering::SeqCst);
    assert_eq!(layout_shown(&app).as_deref(), Some("Contact Details"));
    assert_eq!(record_number(&app), "1");
    app.get_by_label("Body");
    // The records are still there to read, in another view too.
    click(&mut app, "Table View");
    assert!(app
        .query(By::new().role(Role::TextInput).value("First0"))
        .is_some());
    assert_eq!(refused.load(Ordering::SeqCst), refused_then);
}

#[test]
fn a_save_that_finds_the_connection_lost_keeps_the_edit() {
    let (mut app, lost, _) = open();
    click(&mut app, "Table View");
    app.get_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Not saved".into()));
    app.run_steps(2);
    lost.store(true, Ordering::SeqCst);
    // Moving on commits first; the commit fails and the move is dropped.
    click(&mut app, "Next");
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    app.run_steps(5);
    assert!(app.state().error().is_none(), "the banner, not an alert");
    assert_eq!(record_number(&app), "1");
    assert_eq!(first_name(&app).as_deref(), Some("Not saved"));
    app.get_by_label_contains("1 unsaved field change is kept");
}

#[test]
fn an_idle_loss_wakes_the_window_by_itself() {
    let (mut app, lost, _) = open();
    // No frame runs from here on: only the worker asks for a repaint. The
    // observer sees every request, even one made while a repaint is due.
    let woken = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&woken);
    app.ctx.set_repaint_observer(move |info| {
        if info.delay.is_zero() {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    // Replies to the last frame's requests have come by now.
    std::thread::sleep(Duration::from_millis(500));
    woken.store(0, Ordering::SeqCst);
    lost.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(30);
    while woken.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the worker never woke the window"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The frame it asked for shows the banner.
    app.run_steps(2);
    app.get_by_label(BANNER);
}

#[test]
fn a_paused_script_stops_when_the_connection_is_lost() {
    let (mut app, lost, _) = open();
    run(&mut app, "Pause");
    wait_until(&mut app, "the pause", |app| app.state().script_paused());
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    assert!(!app.state().script_paused());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(record_number(&app), "1");
}

#[test]
fn a_script_that_does_not_allow_abort_stops_when_the_connection_is_lost() {
    let (mut app, lost, _) = open();
    run(&mut app, "Spin");
    wait_until(&mut app, "the loop", |app| app.state().script_running());
    // Esc does not stop it.
    app.key_press(egui::Key::Escape);
    app.run_steps(5);
    assert!(app.state().script_running());
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn a_script_waiting_on_a_dialog_stops_when_the_connection_is_lost() {
    let (mut app, lost, _) = open();
    run(&mut app, "Ask");
    common::wait_for_label(&mut app, "Still there?");
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    assert!(
        app.query_by_label("Still there?").is_none(),
        "the dialog stayed"
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn close_file_skips_its_trigger_once_the_connection_is_lost() {
    // OnWindowClose runs Pause.
    let (mut app, lost, _) = open_with(Some(FileOptions {
        window_close: Some(1),
        ..FileOptions::default()
    }));
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    // OnWindowClose's script cannot run: the first click closes the file.
    click(&mut app, "Close File");
    assert!(!app.state().script_running(), "a script that never runs");
    wait_until(&mut app, "the Hosts dialog", |app| {
        app.query_by_label("Hosts").is_some()
    });
    assert!(!app.state().is_open());
}

#[test]
fn ontimer_starts_nothing_once_the_connection_is_lost() {
    let (mut app, lost, _) = open();
    run(&mut app, "Install timer");
    wait_until(&mut app, "the script to end", |app| {
        !app.state().script_running()
    });
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    // Several intervals pass with frames running: no Tick is started.
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        app.step();
        assert!(!app.state().script_running(), "OnTimer started a script");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_scripted_commit_the_connection_is_lost_in_keeps_the_edit() {
    let (mut app, _, _) = open();
    click(&mut app, "Table View");
    run(&mut app, "Lose commit");
    wait_until(&mut app, "the pause in the transaction", |app| {
        app.state().script_paused()
    });
    // Edited in the transaction, then committed with it.
    app.get_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Kept in the window".into()));
    app.run_steps(2);
    click(&mut app, "Continue");
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    app.run_steps(5);
    // Whether the host has the edit is unknown: it stays on screen.
    assert_eq!(first_name(&app).as_deref(), Some("Kept in the window"));
    app.get_by_label_contains("1 unsaved field change is kept");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn replies_queued_behind_the_loss_leave_the_layout_put_back() {
    let hold = Arc::new(AtomicBool::new(false));
    let (mut app, lost, _) = open_holding(None, Arc::clone(&hold));
    app.get_by_label("Body");
    // The worker is stuck in a call while the user goes to another layout
    // and back: both switches' requests queue up behind it.
    hold.store(true, Ordering::SeqCst);
    select_layout(&mut app, "Contacts List");
    select_layout(&mut app, "Contact Details");
    lost.store(true, Ordering::SeqCst);
    hold.store(false, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    // The queued failures for Contact Details come after the loss put it
    // back; it keeps its parts and its record.
    app.run_steps(8);
    assert_eq!(layout_shown(&app).as_deref(), Some("Contact Details"));
    app.get_by_label("Body");
    assert_eq!(record_number(&app), "1");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The second window New Window opens, named after the file's.
const SECOND: &str = "Contacts - 2";

/// The value of the First Name cell of row `row` of `window` in Table
/// view.
fn first_name_in(window: &egui_kittest::Node<'_>, row: usize) -> Option<String> {
    window
        .query_all_by_role_and_label(Role::TextInput, "First Name")
        .nth(row)
        .and_then(|cell| cell.value())
}

/// Type `text` over what the focused field holds.
fn type_over(app: &mut common::App, text: &str) {
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// Both windows of the file lose the connection: each says so, keeps its
/// layout, record and unsaved edit, and neither asks the host for
/// anything more; Close File still closes them both.
#[test]
fn every_window_keeps_its_records_and_edits_when_the_connection_is_lost() {
    let (mut app, lost, refused) = open();
    common::open_menu(&mut app, "Window");
    click(&mut app, "New Window");
    wait_until(&mut app, "the second window", |app| {
        !app.state().script_running() && app.query_by_role_and_label(Role::Window, SECOND).is_some()
    });
    // The second window (in front): Contacts List as a table, on record
    // 2, its first name edited.
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_role_and_label(Role::ComboBox, "Layout")
        .click();
    app.run_steps(2);
    click(&mut app, "Contacts List");
    wait_until(&mut app, "the second window's layout", |app| {
        app.get_by_role_and_label(Role::Window, SECOND)
            .get_by_role_and_label(Role::ComboBox, "Layout")
            .value()
            .as_deref()
            == Some("Contacts List")
    });
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_label("Table View")
        .click();
    app.run_steps(2);
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_label("Next")
        .click();
    wait_until(&mut app, "the second window's rows", |app| {
        first_name_in(&app.get_by_role_and_label(Role::Window, SECOND), 1).is_some()
    });
    app.get_by_role_and_label(Role::Window, SECOND)
        .query_all_by_role_and_label(Role::TextInput, "First Name")
        .nth(1)
        .unwrap()
        .focus();
    app.run_steps(2);
    type_over(&mut app, "Second window draft");
    // The first window (behind): Contact Details as a table on record 1,
    // edited too. Its controls come first and are pressed through
    // AccessKit, as in its own native window.
    app.query_all(By::new().role(Role::Button).label("Table View"))
        .next()
        .unwrap()
        .click_accesskit();
    wait_until(&mut app, "the first window's rows", |app| {
        app.query_all_by_role_and_label(Role::TextInput, "First Name")
            .count()
            > 2
    });
    app.query_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .unwrap()
        .focus();
    app.run_steps(2);
    type_over(&mut app, "First window draft");
    assert_eq!(
        app.state_mut().record_edit_states(),
        [Some(false), Some(false)],
        "each window has its edit"
    );

    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner in both windows", |app| {
        app.query_all_by_label(BANNER).count() == 2
    });
    assert!(app.state().connection_lost().is_some());
    assert!(app.state().error().is_none(), "the banners, not an alert");
    assert_eq!(
        app.query_all_by_label_contains("1 unsaved field change is kept")
            .count(),
        2
    );
    let check = |app: &common::App| {
        let second = app.get_by_role_and_label(Role::Window, SECOND);
        assert_eq!(
            second
                .get_by_role_and_label(Role::ComboBox, "Layout")
                .value()
                .as_deref(),
            Some("Contacts List")
        );
        assert_eq!(
            second
                .get_by_role_and_label(Role::TextInput, "Record Number")
                .value()
                .as_deref(),
            Some("2")
        );
        assert_eq!(
            first_name_in(&second, 1).as_deref(),
            Some("Second window draft")
        );
        assert_eq!(
            app.query_all(By::new().role(Role::ComboBox).label("Layout"))
                .next()
                .unwrap()
                .value()
                .as_deref(),
            Some("Contact Details")
        );
        assert_eq!(
            app.query_all(By::new().role(Role::TextInput).label("Record Number"))
                .next()
                .unwrap()
                .value()
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            app.query_all_by_role_and_label(Role::TextInput, "First Name")
                .next()
                .unwrap()
                .value()
                .as_deref(),
            Some("First window draft")
        );
    };
    check(&app);

    // Neither window asks the host for anything now.
    let before = refused.load(Ordering::SeqCst);
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_label("Next")
        .click();
    app.query_all(By::new().role(Role::Button).label("Next"))
        .next()
        .unwrap()
        .click_accesskit();
    app.run_steps(5);
    assert_eq!(refused.load(Ordering::SeqCst), before, "a request went out");
    check(&app);

    // Close File, from either banner, closes both.
    app.query_all(By::new().role(Role::Button).label("Close File"))
        .next()
        .unwrap()
        .click_accesskit();
    wait_until(&mut app, "the Hosts dialog", |app| {
        app.query_by_label("Hosts").is_some()
    });
    assert!(!app.state().is_open());
    assert!(app.state().connection_lost().is_none());
    assert_eq!(app.state_mut().window_titles().len(), 1);
}

/// Both windows were switching layouts when the connection went, their
/// requests queued behind a call the worker was stuck in: each puts back
/// the layout it showed, with its record, whichever window's reply told
/// the loss.
#[test]
fn every_window_puts_back_the_layout_it_was_loading_when_the_connection_is_lost() {
    let hold = Arc::new(AtomicBool::new(false));
    let (mut app, lost, _) = open_holding(None, Arc::clone(&hold));
    common::open_menu(&mut app, "Window");
    click(&mut app, "New Window");
    wait_until(&mut app, "the second window", |app| {
        !app.state().script_running()
            && app.query_by_role_and_label(Role::Window, SECOND).is_some()
            && app.query_all_by_label("Body").count() == 2
    });
    hold.store(true, Ordering::SeqCst);
    // The second window (in front), then the first (behind, through
    // AccessKit), go to Contacts List.
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_role_and_label(Role::ComboBox, "Layout")
        .click();
    app.run_steps(2);
    click(&mut app, "Contacts List");
    app.query_all(By::new().role(Role::ComboBox).label("Layout"))
        .next()
        .unwrap()
        .click_accesskit();
    app.run_steps(2);
    app.get_by_role_and_label(Role::Button, "Contacts List")
        .click_accesskit();
    app.run_steps(2);
    let layouts = |app: &common::App| -> Vec<Option<String>> {
        app.query_all(By::new().role(Role::ComboBox).label("Layout"))
            .map(|combo| combo.value())
            .collect()
    };
    let switching = layouts(&app);
    lost.store(true, Ordering::SeqCst);
    hold.store(false, Ordering::SeqCst);
    wait_until(&mut app, "the banner in both windows", |app| {
        app.query_all_by_label(BANNER).count() == 2
    });
    app.run_steps(8);
    let contacts_list = Some("Contacts List".to_owned());
    assert_eq!(switching, [contacts_list.clone(), contacts_list]);
    let details = Some("Contact Details".to_owned());
    assert_eq!(layouts(&app), [details.clone(), details]);
    assert_eq!(
        app.query_all_by_label("Body").count(),
        2,
        "a window is blank"
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// File > Close in the second window runs OnWindowClose, which pauses in
/// a transaction; the user edits the record and continues, and the
/// commit loses the connection. The trigger let the close go on, but the
/// window's save cannot happen now: both windows stay open, the edit
/// kept.
#[test]
fn a_close_its_trigger_let_go_on_stops_when_the_connection_is_lost() {
    let (mut app, _, _) = open_with(Some(FileOptions {
        window_close: Some(LOSE_CLOSING),
        ..FileOptions::default()
    }));
    common::open_menu(&mut app, "Window");
    click(&mut app, "New Window");
    wait_until(&mut app, "the second window", |app| {
        !app.state().script_running() && app.query_by_role_and_label(Role::Window, SECOND).is_some()
    });
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_label("Table View")
        .click();
    app.run_steps(2);
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_role_and_label(Role::Button, "File")
        .click();
    app.run_steps(2);
    click(&mut app, "Close");
    wait_until(&mut app, "the close trigger's pause", |app| {
        app.state().script_paused()
    });
    app.get_by_role_and_label(Role::Window, SECOND)
        .query_all_by_role_and_label(Role::TextInput, "First Name")
        .next()
        .unwrap()
        .focus();
    app.run_steps(2);
    type_over(&mut app, "Kept after the loss");
    assert!(common::unsaved(&app), "unsaved record changes");
    app.get_by_role_and_label(Role::Window, SECOND)
        .get_by_label("Continue")
        .click();
    wait_until(&mut app, "the loss, the trigger stopped", |app| {
        !app.state().is_open()
            || (!app.state().script_running() && app.state().connection_lost().is_some())
    });
    app.run_steps(12);
    assert!(app.state().connection_lost().is_some(), "the loss was told");
    assert_eq!(
        app.state_mut().window_titles().len(),
        2,
        "the close went on and discarded the edited window"
    );
    assert_eq!(
        first_name_in(&app.get_by_role_and_label(Role::Window, SECOND), 0).as_deref(),
        Some("Kept after the loss")
    );
    app.get_by_label_contains("1 unsaved field change is kept");
}

// ----- reconnecting ------------------------------------------------------------

const RECONNECTED: &str = "Reconnected to the host.";

/// The value of the `field` cell of Table view's row `row`.
fn cell(app: &common::App, row: usize, field: &str) -> Option<String> {
    app.query_all_by_role_and_label(Role::TextInput, field)
        .nth(row)
        .and_then(|cell| cell.value())
}

/// Type `text` over the `field` cell of Table view's row `row` (empty text
/// empties it), the row's record becoming the current one.
fn type_into(app: &mut common::App, row: usize, field: &str, text: &str) {
    app.get_all_by_role_and_label(Role::TextInput, field)
        .nth(row)
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

/// The lost banner's Reconnect, with `password`. A new dialog shows from
/// its second frame.
fn reconnect(app: &mut common::App, password: &str) {
    if common::find_text_field(app, "Password").is_none() {
        app.get_by_role_and_label(Role::Button, "Reconnect…")
            .click();
        wait_until(app, "the reconnect dialog", |app| {
            common::find_text_field(app, "Password").is_some()
        });
    }
    common::fill(app, "Password", password);
    app.get_by_role_and_label(Role::Button, "Reconnect").click();
    app.run_steps(2);
}

/// The window's edits, empty and not, outlive a reconnect that fails; one
/// that succeeds finds the same record (another client deleted the one
/// before it meanwhile), checks the edits against the host's copy, and
/// leaves them to be saved as usual.
#[test]
fn a_failed_reconnect_keeps_typed_edits_and_a_fresh_session_recovers_them() {
    let (mut app, lost, _, host) = open_hosted(None, Arc::default());
    click(&mut app, "Table View");
    type_into(&mut app, 1, "First Name", "");
    type_into(&mut app, 1, "Last Name", "Draft last");
    assert_eq!(record_number(&app), "2");
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    app.get_by_label_contains("2 unsaved field changes are kept");
    host.meanwhile.lock().unwrap().push((1, None));

    reconnect(&mut app, "not the password");
    wait_until(&mut app, "the refusal", |app| {
        app.query_by_label_contains("was not reconnected").is_some()
    });
    assert!(app.state().connection_lost().is_some());
    app.get_by_label_contains("2 unsaved field changes are kept");
    assert_eq!(cell(&app, 1, "Last Name").as_deref(), Some("Draft last"));
    assert_ne!(cell(&app, 1, "First Name").as_deref(), Some("First1"));

    reconnect(&mut app, "secret");
    wait_until(&mut app, "the reconnect", |app| {
        app.state().connection_lost().is_none() && app.state().is_settled()
    });
    app.get_by_label(RECONNECTED);
    app.get_by_label_contains("which has not changed");
    // Record 2 is the found set's first now.
    assert_eq!(record_number(&app), "1");
    assert_eq!(cell(&app, 0, "Last Name").as_deref(), Some("Draft last"));
    assert!(host.saved.lock().unwrap().is_empty(), "nothing saved yet");

    common::save_record(&mut app);
    wait_until(&mut app, "the save", |app| {
        !common::editing(app) && app.state().is_settled()
    });
    let expected: FieldChanges = [
        ("First Name".to_owned(), None),
        ("Last Name".to_owned(), Some("Draft last".to_owned())),
    ]
    .into();
    assert_eq!(*host.saved.lock().unwrap(), [(2, expected)]);
}

/// Another client changed the record while the connection was lost: the
/// edit is held, nothing saves it until the user keeps it, and the save
/// that follows the choice is the only write.
#[test]
fn an_edit_of_a_record_changed_on_the_host_waits_for_the_users_choice() {
    let (mut app, lost, _, host) = open_hosted(None, Arc::default());
    click(&mut app, "Table View");
    type_into(&mut app, 0, "First Name", "Mine");
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    let theirs = [("First Name".to_owned(), Some("Theirs".to_owned()))];
    host.meanwhile
        .lock()
        .unwrap()
        .push((1, Some(theirs.into_iter().collect())));

    reconnect(&mut app, "secret");
    wait_until(&mut app, "the reconnect", |app| {
        app.state().connection_lost().is_none() && app.state().is_settled()
    });
    app.get_by_label_contains("was changed on the host");
    assert_eq!(cell(&app, 0, "First Name").as_deref(), Some("Mine"));

    // Saving or moving on is refused, with the reason.
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    app.run_steps(3);
    assert!(app
        .state()
        .error()
        .is_some_and(|e| e.contains("Keep or discard")));
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    click(&mut app, "Next");
    app.run_steps(3);
    assert!(app.state().error().is_some());
    app.key_press(egui::Key::Escape);
    app.run_steps(5);
    assert_eq!(record_number(&app), "1");
    assert!(
        host.saved.lock().unwrap().is_empty(),
        "the host's change stands"
    );

    click(&mut app, "Keep My Changes");
    common::save_record(&mut app);
    wait_until(&mut app, "the save", |app| {
        !common::editing(app) && app.state().is_settled()
    });
    let mine: FieldChanges = [("First Name".to_owned(), Some("Mine".to_owned()))].into();
    assert_eq!(*host.saved.lock().unwrap(), [(1, mine)]);
}

/// A save whose answer the loss took away reached the host: the record
/// read back holds the edit, which is then no longer pending, and nothing
/// is sent again.
#[test]
fn a_save_the_connection_was_lost_in_is_checked_and_never_sent_again() {
    let (mut app, _, _, host) = open_hosted(None, Arc::default());
    click(&mut app, "Table View");
    type_into(&mut app, 0, "First Name", "Landed");
    host.landing.store(true, Ordering::SeqCst);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    app.get_by_label_contains("1 unsaved field change is kept");

    reconnect(&mut app, "secret");
    wait_until(&mut app, "the reconnect", |app| {
        app.state().connection_lost().is_none() && app.state().is_settled()
    });
    app.get_by_label_contains("already holds the unsaved change");
    assert!(!common::editing(&app), "nothing pending");
    app.run_steps(10);
    assert_eq!(cell(&app, 0, "First Name").as_deref(), Some("Landed"));
    assert_eq!(host.saved.lock().unwrap().len(), 1, "sent once");
}

/// Another client changes the record after the reconnect read it back,
/// and the window's reload shows the change: the edit, checked against
/// the earlier copy, is not saved over it. The save is refused, the record
/// read again and the edit held until the user keeps it.
#[test]
fn a_change_after_the_check_is_not_saved_over() {
    let (mut app, lost, _, host) = open_hosted(None, Arc::default());
    click(&mut app, "Table View");
    type_into(&mut app, 0, "First Name", "Mine");
    lost.store(true, Ordering::SeqCst);
    wait_until(&mut app, "the banner", |app| {
        app.query_by_label(BANNER).is_some()
    });
    let theirs = [("Email".to_owned(), Some("theirs@example.com".to_owned()))];
    host.after_check
        .lock()
        .unwrap()
        .push((1, theirs.into_iter().collect()));

    reconnect(&mut app, "secret");
    wait_until(&mut app, "the reconnect", |app| {
        app.state().connection_lost().is_none() && app.state().is_settled()
    });
    app.get_by_label_contains("which has not changed");
    assert_eq!(
        cell(&app, 0, "Email").as_deref(),
        Some("theirs@example.com")
    );

    common::save_record(&mut app);
    wait_until(&mut app, "the edit held again", |app| {
        app.query_by_label("Keep My Changes").is_some() && app.state().is_settled()
    });
    app.get_by_label_contains("was changed on the host");
    assert!(
        app.state().error().is_none(),
        "the bar says so, not an alert"
    );
    assert!(host.saved.lock().unwrap().is_empty(), "the change stands");

    click(&mut app, "Keep My Changes");
    common::save_record(&mut app);
    wait_until(&mut app, "the save", |app| {
        !common::editing(app) && app.state().is_settled()
    });
    let mine: FieldChanges = [("First Name".to_owned(), Some("Mine".to_owned()))].into();
    assert_eq!(*host.saved.lock().unwrap(), [(1, mine)]);
}

/// A Commit Transaction the connection was lost in: whether the host has
/// its writes is unknown, so the worker refuses the reconnect before it
/// opens anything, and the window keeps its edit and its banner.
#[test]
fn a_transaction_lost_in_its_commit_is_not_reconnected() {
    let (mut app, _, _, host) = open_hosted(None, Arc::default());
    click(&mut app, "Table View");
    run(&mut app, "Lose commit");
    wait_until(&mut app, "the pause in the transaction", |app| {
        app.state().script_paused()
    });
    type_into(&mut app, 0, "First Name", "Kept in the window");
    click(&mut app, "Continue");
    wait_until(&mut app, "the banner, the script stopped", stopped_and_told);
    let opens = host.opens.load(Ordering::SeqCst);

    reconnect(&mut app, "secret");
    wait_until(&mut app, "the refusal", |app| {
        app.query_by_label_contains("transaction a script opened")
            .is_some()
    });
    assert_eq!(host.opens.load(Ordering::SeqCst), opens, "nothing reopened");
    assert!(app.state().connection_lost().is_some());
    app.get_by_label_contains("1 unsaved field change is kept");
    assert_eq!(
        cell(&app, 0, "First Name").as_deref(),
        Some("Kept in the window")
    );
}
