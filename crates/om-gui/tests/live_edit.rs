//! Native record writes through the GUI on a hosted file: New Record, editing,
//! navigation and Delete Record, against test project on an in-process
//! `om-server` (see `hosted`). Every save is checked from a separate session.
//! `cargo test -p om-gui --test live_edit -- --ignored`
mod common;
mod hosted;

use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::{DataProvider, RecordPage};

const FIELD: &str = "First Name";

/// Every Contacts record as a fresh session reads it.
fn contacts(hosted: &hosted::Hosted) -> RecordPage {
    let mut provider = hosted.opened();
    let layout = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|layout| layout.name == "Contact Details")
        .expect("Contact Details layout")
        .id;
    let page = provider.records(layout, 0, 100).unwrap();
    provider.close();
    page
}

fn first_name(page: &RecordPage, id: i64) -> Option<&str> {
    let field = page.fields.iter().position(|f| f.name == FIELD).unwrap();
    page.records.iter().find(|r| r.id == id)?.values[field].as_deref()
}

fn replace_first_name(app: &mut common::App, value: &str) {
    common::text_field(app, FIELD).click();
    app.step();
    assert!(
        common::text_field(app, FIELD).is_focused(),
        "click must focus the native field editor"
    );
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(value.into()));
    app.run_steps(2);
}

fn save(app: &mut common::App) {
    common::save_record(app);
    common::wait_until(app, "hosted save", |app| {
        assert!(app.state().error().is_none(), "hosted save failed");
        !common::saving(app) && !common::unsaved(app)
    });
}

fn screenshot(app: &mut common::App, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("screenshots");
    std::fs::create_dir_all(&dir).expect("create ignored screenshot directory");
    app.set_render_cursor(false);
    app.render()
        .expect("render hosted record")
        .save(dir.join(name))
        .expect("save ignored screenshot");
}

#[test]
#[ignore = "requires the git-ignored copy of test project"]
fn new_record_edit_navigation_and_delete() {
    let hosted = hosted::host("edit");
    let baseline = contacts(&hosted);
    assert_eq!(baseline.total, 3);
    let provider = hosted.provider();
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 982.0))
        .build_eframe(move |_| om_gui::OpenMakerApp::new(Box::new(provider)));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    // Contact List, the first layout, sorts its records on entry.
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    common::select_layout(&mut app, "Contact Details");
    common::wait_until(&mut app, "hosted field editor", |app| {
        app.query_by_role_and_label(egui::Role::TextInput, FIELD)
            .is_some()
    });
    for command in ["New Record", "Delete Record"] {
        assert!(!app.get_by_label(command).accesskit_node().is_disabled());
    }

    // New Record shows an empty record 4 with a field ready for typing;
    // saving creates it on the server.
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "new record field focus", |app| {
        common::record_number(app) == "4"
            && app.query_all_by_role(egui::Role::TextInput).any(|field| {
                field.is_focused()
                    && field
                        .accesskit_node()
                        .label()
                        .is_some_and(|l| l != "Search")
            })
    });
    replace_first_name(&mut app, "OpenMaker GUI");
    save(&mut app);
    let created = contacts(&hosted);
    assert_eq!(created.total, 4);
    assert_eq!(first_name(&created, 4), Some("OpenMaker GUI"));

    // Edit it, leave it and come back: the saved value is shown.
    replace_first_name(&mut app, "OpenMaker GUI edit");
    screenshot(&mut app, "26-live-editable-fields.png");
    save(&mut app);
    assert_eq!(
        first_name(&contacts(&hosted), 4),
        Some("OpenMaker GUI edit")
    );
    common::click(&mut app, "Previous");
    common::wait_until(&mut app, "previous record", |app| {
        common::record_number(app) == "3"
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "edited record", |app| {
        common::text_field(app, FIELD).value().as_deref() == Some("OpenMaker GUI edit")
    });

    // Delete Record asks first, then removes only this record.
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "deleted record", |app| {
        assert!(app.state().error().is_none(), "hosted delete failed");
        common::record_number(app) == "3"
    });
    assert_eq!(contacts(&hosted), baseline);
}
