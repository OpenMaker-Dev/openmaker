//! The GUI on a hosted file, read only: test project on an in-process
//! `om-server` (see `hosted`). Screenshots are local ignored artifacts.
//! `cargo test -p om-gui --test live -- --ignored`
mod common;
mod hosted;

use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::{
    Connection, DataError, DataProvider, HostedFile, LayoutGeometry, LayoutInfo, RecordPage,
    RenderLayout,
};
use om_provider::NativeProvider;
use std::sync::{Arc, Mutex};

struct Observed {
    native: NativeProvider,
    page: Arc<Mutex<Option<RecordPage>>>,
}
impl DataProvider for Observed {
    fn database_schema(&mut self) -> Result<om_data::DatabaseSchema, DataError> {
        self.native.database_schema()
    }
    fn scripts(&mut self) -> Result<om_data::ScriptCatalog, DataError> {
        self.native.scripts()
    }
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        self.native.list_files(host, port)
    }
    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.native.open(connection)
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.native.layouts()
    }
    fn layout_geometry(&mut self, id: u32) -> Result<Option<LayoutGeometry>, DataError> {
        self.native.layout_geometry(id)
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        self.native.render_layout(id)
    }
    fn records(&mut self, id: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let page = self.native.records(id, offset, limit)?;
        // The window's pages, not the single records scripts read.
        if limit > 1 {
            *self.page.lock().unwrap() = Some(page.clone());
        }
        Ok(page)
    }
    fn calc_schema(
        &mut self,
    ) -> Result<Option<std::sync::Arc<om_data::host::CalcSchema>>, DataError> {
        self.native.calc_schema()
    }
    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        self.native.calc_data(needs)
    }
    fn change_found_set(
        &mut self,
        id: u32,
        change: om_data::found::FoundSetChange,
    ) -> Result<om_data::found::FoundSet, DataError> {
        self.native.change_found_set(id, change)
    }
    fn close(&mut self) {
        self.native.close();
    }
}

fn screenshot(app: &mut common::App, name: &str) {
    app.run_steps(3);
    app.set_render_cursor(false);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    app.render()
        .expect("render live layout")
        .save(dir.join(name))
        .unwrap();
}

#[test]
#[ignore = "requires the git-ignored copy of test project"]
fn live_contacts_records_and_layout() {
    let hosted = hosted::host("live");
    let page = Arc::new(Mutex::new(None));
    let provider = Observed {
        native: hosted.provider(),
        page: page.clone(),
    };
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 982.0))
        .build_eframe(move |_| om_gui::OpenMakerApp::new(Box::new(provider)));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    // Contact List, the first layout, sorts its records on entry.
    common::wait_until(&mut app, "native records", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled() && page.lock().unwrap().is_some()
    });
    common::select_layout(&mut app, "Contact Details");
    common::wait_until(&mut app, "Company tab", |app| {
        app.query_all_by_role_and_label(egui::Role::Tab, "Company")
            .next()
            .is_some()
    });
    screenshot(&mut app, "19-live-contact-details.png");
    common::wait_until(&mut app, "real field value", |app| {
        let guard = page.lock().unwrap();
        let data = guard.as_ref().unwrap();
        let index = data
            .fields
            .iter()
            .position(|f| f.name == "First Name")
            .unwrap();
        let value = data.records[0].values[index].as_deref().unwrap();
        let displayed = app.query_all_by_label(value).next().is_some();
        displayed
    });
    assert!(
        page.lock().unwrap().as_ref().unwrap().total >= 2,
        "test file needs at least two records"
    );
    screenshot(&mut app, "19-live-contact-details.png");
    common::click(&mut app, "Next");
    assert_eq!(common::record_number(&app), "2");
    let data = page.lock().unwrap().as_ref().unwrap().clone();
    let first = data
        .fields
        .iter()
        .position(|f| f.name == "First Name")
        .unwrap();
    let value = data.records[1].values[first].as_deref().unwrap();
    assert!(
        app.query_all_by_label(value).next().is_some(),
        "second server record must be rendered"
    );
    screenshot(&mut app, "20-live-second-record.png");
    app.get_by_role_and_label(egui::Role::Tab, "Email").click();
    app.run_steps(3);
    common::wait_for_label(&mut app, "Email Address");
    assert!(
        app.query_all_by_label("COMPANY").next().is_none(),
        "inactive panel is hidden"
    );
    screenshot(&mut app, "21-live-email-tab.png");
    common::select_layout(&mut app, "Contact List");
    common::wait_until(&mut app, "native list view", |app| {
        app.state().view() == om_gui::ViewMode::List
    });
    let rows = page.lock().unwrap().as_ref().unwrap().clone();
    let first = rows
        .fields
        .iter()
        .position(|field| field.name == "First Name")
        .unwrap();
    for record in rows.records.iter().take(3) {
        if let Some(value) = record.values[first]
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            common::wait_for_label(&mut app, value);
        }
    }
    screenshot(&mut app, "22-live-contact-list.png");
    app.get_all_by_role_and_label(egui::Role::Button, "Button 7")
        .next()
        .expect("native row navigation button")
        .click();
    app.run_steps(3);
    // The button's script runs on the worker; the layout draws when its
    // render arrives.
    common::wait_until(&mut app, "detail button navigation", |app| {
        app.state().view() == om_gui::ViewMode::Form
            && app
                .query_all_by_role_and_label(egui::Role::Tab, "Company")
                .next()
                .is_some()
    });
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::S,
    );
    common::wait_for_label(&mut app, "Script Name");
    assert_eq!(
        common::text_field(&app, "Script Name").value().as_deref(),
        Some("Go to Entry Field")
    );
    assert!(app.get_by_label("Save").accesskit_node().is_disabled());
    screenshot(&mut app, "23-live-script-workspace.png");
    common::click(&mut app, "Close");
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::D,
    );
    common::wait_for_label(&mut app, "13 fields");
    app.get_by_label("PrimaryKey");
    assert!(app.get_by_label("Create").accesskit_node().is_disabled());
    screenshot(&mut app, "24-live-manage-database.png");
    common::click(&mut app, "Relationships");
    common::wait_until(&mut app, "relationship occurrences", |app| {
        app.query_all_by_label("Addresses").next().is_some()
    });
    screenshot(&mut app, "25-live-relationships.png");
    common::click(&mut app, "Cancel");
}
