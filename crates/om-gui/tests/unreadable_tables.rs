//! Calculations over a table whose data the host refuses, through the GUI
//! end to end: the native provider and an in-process `om-server` hosting
//! the in-memory Tasks file of `om-server/tests/fixtures/tasks.rs`, told to
//! refuse downloads of the Notes records. Tasks gets two unstored
//! calculations, Label (`Name & " (" & Count & ")"`) over its own record and
//! Notes (`Count ( Notes::Body )`) over related Notes records. Table view
//! shows both for every record, so one request asks for data of both
//! tables: Label shows its values, Notes `?`, and the window says once why
//! Notes could not be read.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use egui_kittest::kittest::Queryable;
use fixture::{LABEL, NAME, NOTES, NOTES_OCCURRENCE, NOTE_COUNT, OCCURRENCE, TABLE, TASK};
use om_data::host::{CalcData, CalcNeed, CalcSchema};
use om_data::{
    Connection, DataError, DataProvider, HostedFile, LayoutGeometry, LayoutInfo,
    RecordCapabilities, RecordPage, RenderLayout,
};
use om_hbam::slot::uint_segment;
use om_provider::NativeProvider;

/// Option bit: the calculation's result is not stored (schema-path.md
/// 5.5).
const UNSTORED: u32 = 0x2_0000;

const NOTES_FAILURE: &str =
    "Calculations could not read table “Notes”: Server record download failed (lock error 0x00c8).";

/// The fixture's file with Label, Notes, Notes::Task and relationship 1
/// (Tasks::Name = Notes::Task), hosted by a server that refuses to download
/// Notes records.
fn host() -> hosted::Hosted {
    let mut root = fixture::tree(&[], true);
    let names = fixture::calculated_names();
    fixture::add_field(&mut root, NOTES, TASK, "Task", 1, 1, 0, 0, 0);
    for (field, name, data_type, text) in [
        (LABEL, "Label", 1, "Name & \" (\" & Count & \")\""),
        (NOTE_COUNT, "Notes", 2, "Count ( Notes::Body )"),
    ] {
        fixture::add_field(&mut root, TABLE, field, name, 2, data_type, 0, UNSTORED, 0);
        fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), field, 5, text);
    }
    fixture::add_relationship(
        &mut root,
        1,
        (OCCURRENCE, NAME),
        0,
        (NOTES_OCCURRENCE, TASK),
    );
    let file = fixture::TempFile::from_tree("gui-unreadable-tables", &root);
    let notes = uint_segment(NOTES);
    hosted::host_file_with(
        "unreadable-tables",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
        |server| server.refusing_downloads(&[&notes, &[0x05]]),
    )
}

/// The native provider, counting the window's requests for calculation
/// data. The file has no scripts, so the window reads none.
struct Counted {
    native: NativeProvider,
    asked: Arc<AtomicUsize>,
}

impl DataProvider for Counted {
    fn supports_client_license(&self) -> bool {
        self.native.supports_client_license()
    }
    fn set_client_license(&mut self, license: Option<&str>) -> Result<(), DataError> {
        self.native.set_client_license(license)
    }
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        self.native.list_files(host, port)
    }
    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.native.open(connection)
    }
    fn can_write_records(&self) -> bool {
        self.native.can_write_records()
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.native.layouts()
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        self.native.render_layout(id)
    }
    fn layout_geometry(&mut self, id: u32) -> Result<Option<LayoutGeometry>, DataError> {
        self.native.layout_geometry(id)
    }
    fn record_capabilities(&mut self, id: u32) -> Result<RecordCapabilities, DataError> {
        self.native.record_capabilities(id)
    }
    fn records(&mut self, id: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.native.records(id, offset, limit)
    }
    fn records_with_calculations(
        &mut self,
        id: u32,
        offset: u64,
        limit: u32,
        fields: &[String],
    ) -> Result<RecordPage, DataError> {
        self.native
            .records_with_calculations(id, offset, limit, fields)
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        self.native.calc_schema()
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        let result = self.native.calc_data(needs);
        self.asked.fetch_add(1, Ordering::SeqCst);
        result
    }
    fn close(&mut self) {
        self.native.close();
    }
}

/// Label's values and Notes' `?` on both records.
fn calculated(app: &common::App) -> bool {
    app.query_by_label("Write tests (3)").is_some()
        && app.query_by_label("Ship (5)").is_some()
        && app.query_all_by_label("?").count() == 2
}

#[test]
fn a_table_the_host_refuses_fails_only_its_calculations_and_is_reported_once() {
    let hosted = host();
    let asked = Arc::new(AtomicUsize::new(0));
    let provider = Counted {
        native: hosted.provider(),
        asked: Arc::clone(&asked),
    };
    // Wide enough for every column of Table view.
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1700.0, 760.0))
        .build_eframe(move |_| om_gui::OpenMakerApp::new(Box::new(provider)));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "the calculations", calculated);
    assert_eq!(app.state().error(), Some(NOTES_FAILURE));

    // Asked again, Notes fails again; the window does not say so again.
    common::click(&mut app, "OK");
    assert!(app.state().error().is_none());
    let before = asked.load(Ordering::SeqCst);
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Refresh Window");
    common::wait_until(&mut app, "another request", |_| {
        asked.load(Ordering::SeqCst) > before
    });
    common::wait_until(&mut app, "its answer", calculated);
    app.run_steps(3);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}
