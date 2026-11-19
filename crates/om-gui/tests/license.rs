//! License management through the actual Hosts UI, with isolated storage.
mod common;

use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::{
    mock::MockProvider, Connection, DataError, DataProvider, HostedFile, LayoutInfo, RecordPage,
};
use om_gui::licensing::{LicenseKey, LicenseStore};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Storage {
    key: Option<String>,
    fail: bool,
}

struct TestStore(Arc<Mutex<Storage>>);
impl LicenseStore for TestStore {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        let storage = self.0.lock().unwrap();
        if storage.fail {
            return Err(DataError::new("Test credential store is locked."));
        }
        Ok(storage.key.clone().map(LicenseKey::new))
    }
    fn save(&mut self, key: &LicenseKey) -> Result<(), DataError> {
        let mut storage = self.0.lock().unwrap();
        if storage.fail {
            return Err(DataError::new("Test credential store is locked."));
        }
        storage.key = Some(key.expose().to_owned());
        Ok(())
    }
    fn remove(&mut self) -> Result<(), DataError> {
        let mut storage = self.0.lock().unwrap();
        if storage.fail {
            return Err(DataError::new("Test credential store is locked."));
        }
        storage.key = None;
        Ok(())
    }
}

struct LicensedProvider {
    mock: MockProvider,
    license: Option<String>,
    opened: Arc<Mutex<Vec<Option<String>>>>,
}

impl DataProvider for LicensedProvider {
    fn supports_client_license(&self) -> bool {
        true
    }
    fn set_client_license(&mut self, key: Option<&str>) -> Result<(), DataError> {
        self.license = key.map(str::to_owned);
        Ok(())
    }
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        self.mock.list_files(host, port)
    }
    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.opened.lock().unwrap().push(self.license.clone());
        self.mock.open(connection)
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.mock.layouts()
    }
    fn records(&mut self, layout: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.mock.records(layout, offset, limit)
    }
    fn close(&mut self) {
        self.mock.close();
    }
}

fn harness(storage: &Arc<Mutex<Storage>>, opened: &Arc<Mutex<Vec<Option<String>>>>) -> common::App {
    let provider = LicensedProvider {
        mock: MockProvider::new(),
        license: Some("stale-key".into()),
        opened: opened.clone(),
    };
    let storage = storage.clone();
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .build_eframe(move |_| {
            om_gui::OpenMakerApp::with_license_store(
                Box::new(provider),
                Box::new(TestStore(storage)),
            )
        })
}

fn fill_connection(app: &mut common::App) {
    common::fill(app, "Host", "example.test");
    common::fill(app, "File", "Contacts.fmp12");
    common::fill(app, "Account", "Admin");
    common::fill(app, "Password", "test-password");
}

fn open(app: &mut common::App) {
    fill_connection(app);
    common::click(app, "Open");
    common::wait_until(app, "licensed open", |app| app.state().is_open());
}

#[test]
fn saved_license_survives_restart_and_removal_clears_future_connections() {
    let storage = Arc::new(Mutex::new(Storage::default()));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let mut app = harness(&storage, &opened);
    common::click(&mut app, "FileMaker License...");
    common::wait_for_label(&mut app, "No license is saved.");
    assert!(app
        .get_by_label("Save License")
        .accesskit_node()
        .is_disabled());
    app.get_by_role_and_label(egui::Role::PasswordInput, "License key");
    common::fill(&mut app, "License key", "synthetic-client-key");
    common::click(&mut app, "Save License");
    common::wait_for_label(
        &mut app,
        "A license is saved in your system credential store.",
    );
    assert_eq!(
        common::text_field(&app, "License key").value().as_deref(),
        Some("")
    );
    common::click(&mut app, "Done");
    drop(app);

    let mut app = harness(&storage, &opened);
    open(&mut app);
    assert_eq!(
        opened.lock().unwrap().last().unwrap().as_deref(),
        Some("synthetic-client-key")
    );
    common::open_menu(&mut app, "OpenMaker");
    common::click(&mut app, "FileMaker License...");
    common::wait_for_label(
        &mut app,
        "A license is saved in your system credential store.",
    );
    // The saved secret is never loaded into the form.
    assert_eq!(
        common::text_field(&app, "License key").value().as_deref(),
        Some("")
    );
    common::click(&mut app, "Remove License");
    common::wait_for_label(&mut app, "No license is saved.");
    common::click(&mut app, "Done");
    drop(app);

    let mut app = harness(&storage, &opened);
    open(&mut app);
    assert_eq!(opened.lock().unwrap().last(), Some(&None));
}

#[test]
fn unavailable_storage_never_blocks_a_connection_and_clears_any_previous_key() {
    // The license is optional (OpenMaker Server needs none): a credential
    // store that cannot be read sends no license rather than refusing to
    // connect, with Use saved license on or off.
    let storage = Arc::new(Mutex::new(Storage {
        key: Some("unused-key".into()),
        fail: true,
    }));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let mut app = harness(&storage, &opened);
    open(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(opened.lock().unwrap().as_slice(), &[None]);
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "closed", |app| !app.state().is_open());
    common::click(&mut app, "Use saved license");
    open(&mut app);
    assert_eq!(opened.lock().unwrap().as_slice(), &[None, None]);
    // The dialog can read the store again once it answers.
    common::open_menu(&mut app, "OpenMaker");
    common::click(&mut app, "FileMaker License...");
    common::wait_for_label(&mut app, "Test credential store is locked.");
    storage.lock().unwrap().fail = false;
    common::click(&mut app, "Check Again");
    common::wait_for_label(
        &mut app,
        "A license is saved in your system credential store.",
    );
    assert!(app.query_by_label("Check Again").is_none());
}

#[test]
fn failed_saves_and_removals_preserve_the_old_key_and_allow_retry() {
    let storage = Arc::new(Mutex::new(Storage {
        key: Some("old-key".into()),
        fail: false,
    }));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let mut app = harness(&storage, &opened);
    common::click(&mut app, "FileMaker License...");
    common::wait_for_label(
        &mut app,
        "A license is saved in your system credential store.",
    );
    common::fill(&mut app, "License key", "replacement-key");
    storage.lock().unwrap().fail = true;
    common::click(&mut app, "Save License");
    common::wait_for_label(&mut app, "Test credential store is locked.");
    assert_eq!(storage.lock().unwrap().key.as_deref(), Some("old-key"));
    assert!(!app
        .get_by_label("Save License")
        .accesskit_node()
        .is_disabled());
    storage.lock().unwrap().fail = false;
    common::click(&mut app, "Save License");
    common::wait_until(&mut app, "license saved", |app| {
        app.query_by_label("Test credential store is locked.")
            .is_none()
    });
    assert_eq!(
        storage.lock().unwrap().key.as_deref(),
        Some("replacement-key")
    );
    storage.lock().unwrap().fail = true;
    common::click(&mut app, "Remove License");
    common::wait_for_label(&mut app, "Test credential store is locked.");
    assert_eq!(
        storage.lock().unwrap().key.as_deref(),
        Some("replacement-key")
    );
    storage.lock().unwrap().fail = false;
    common::click(&mut app, "Remove License");
    common::wait_for_label(&mut app, "No license is saved.");
    assert!(storage.lock().unwrap().key.is_none());
}

#[test]
#[ignore = "renders the license dialog for visual inspection in the system temporary directory"]
fn render_license_dialog() {
    let storage = Arc::new(Mutex::new(Storage::default()));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let mut app = harness(&storage, &opened);
    common::click(&mut app, "FileMaker License...");
    common::wait_for_label(&mut app, "No license is saved.");
    common::fill(&mut app, "License key", "synthetic-key-for-mask-check");
    app.set_render_cursor(false);
    app.render()
        .unwrap()
        .save(std::env::temp_dir().join("openmaker-filemaker-license.png"))
        .unwrap();
}
