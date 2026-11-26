//! A field placed on a layout through OpenMaker Server's authoring
//! extension (`om_provider::NativeProvider::place_field`, the backend
//! `om-author`'s `place_field` uses) shows in the desktop client as any
//! field object does: the saved file, hosted again by a new server, opens
//! on the Form layout with the field's editor showing the record's value
//! and the label beside it, and the user's edit in it is saved to the
//! record.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use egui_kittest::kittest::{NodeT as _, Queryable};
use fixture::{ACCOUNT, COUNT, NAME, OCCURRENCE, PASSWORD};
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_layout::encode::{self, bytes, double, uint};
use om_protocol::authoring::{catalog_revision, PlaceField};

fn rect(left: f64, top: f64, width: f64, height: f64) -> om_layout::Rect {
    om_layout::Rect {
        top,
        left,
        bottom: top + height,
        right: left + width,
        other: Vec::new(),
    }
}

/// The Tasks file with layout 1 a Form layout 480 points wide with a body
/// (0 to 200) holding only a Count field, which the body's list of its
/// objects names (layout-model.md 10.5).
fn form_file() -> fixture::TempFile {
    form_file_with(COUNT)
}

/// [`form_file`] with `field` of Tasks in Count's place.
fn form_file_with(field: u64) -> fixture::TempFile {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    form_file_object(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        field,
    ))
}

/// [`encode::field_object`] of `field` made a drop-down calendar showing
/// its icon (displayType 6, `HasIcon` 0x400, layout-model.md 5.4): flags
/// `0xe0` become `0x4e0` and displayType 0 becomes 6, each in a varint of
/// the same length.
fn calendar_object(field: u64) -> Vec<u8> {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut object = encode::field_object(1, &rect(110.0, 20.0, 120.0, 22.0), alias, field);
    let mut patch = |from: &[u8], to: &[u8]| {
        let at = object
            .windows(from.len())
            .position(|bytes| bytes == from)
            .expect("the edit box's bytes");
        object[at..at + to.len()].copy_from_slice(to);
    };
    patch(&[0x08, 0xe0, 0x01, 0x12], &[0x08, 0xe0, 0x09, 0x12]);
    patch(&[0x28, 0x00, 0x38, 0x01], &[0x28, 0x06, 0x38, 0x01]);
    object
}

/// [`form_file`] with `object` (a field object, key 1) in Count's place.
fn form_file_object(object: Vec<u8>) -> fixture::TempFile {
    fixture::TempFile::from_tree("gui-placed-fields", &form_tree(object))
}

/// [`form_file_object`] with `scripts` in the file's script catalog.
fn form_file_scripts(object: Vec<u8>, scripts: &[om_scripts::Script]) -> fixture::TempFile {
    fixture::TempFile::from_tree("gui-placed-fields", &form_tree_scripts(object, scripts))
}

/// The tree of [`form_file_object`].
fn form_tree(object: Vec<u8>) -> OwnedNode {
    form_tree_scripts(object, &[fixture::script(1, "Start", Vec::new())])
}

/// The tree of [`form_file_scripts`].
fn form_tree_scripts(object: Vec<u8>, scripts: &[om_scripts::Script]) -> OwnedNode {
    let mut root = fixture::tree(scripts, true);
    let layout = uint_segment(fixture::LAYOUT);
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
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
    root.put(
        &[&[4], &[5], &layout, &[3], &[1], &[7]],
        OwnedNode::leaf(Vec::new(), vec![0, 0, 0, 1, 2, 1, 1]),
    );
    let mut store = Vec::new();
    uint(1, 1, &mut store);
    bytes(2, &object, &mut store);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    root
}

#[test]
fn a_placed_field_is_shown_and_edited_after_the_file_is_hosted_again() {
    let file = form_file();
    let first = hosted::host_file(
        "placed-first",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut author = first.opened();
    let member = author.layout_member(fixture::LAYOUT).unwrap().unwrap();
    let placed = author
        .place_field(PlaceField {
            layout: fixture::LAYOUT,
            expected: catalog_revision(&member),
            table: fixture::TABLE,
            occurrence: OCCURRENCE,
            field: NAME,
            frame: [60.0, 110.0, 82.0, 330.0],
            label: Some(("Task".into(), [60.0, 20.0, 82.0, 100.0])),
        })
        .unwrap();
    assert!(placed.changed);
    assert_eq!((placed.field_object, placed.label_object), (2, 3));
    author.close();

    // The file as the first server saved it, hosted by a new one.
    let hosted = hosted::host_file(
        "placed-again",
        first.file(),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the Form layout", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Task").is_some()
    });
    // The editor shows record 1's Name, right of its label and level with
    // it, as the frames place them.
    let editor = common::text_field(&app, "Name");
    assert_eq!(
        editor.accesskit_node().value().unwrap_or_default(),
        "Write tests"
    );
    let label = app.get_by_label("Task").rect();
    let field = editor.rect();
    assert!(label.right() <= field.left(), "{label:?} {field:?}");
    assert!(
        label.top() < field.bottom() && field.top() < label.bottom(),
        "{label:?} {field:?}"
    );

    common::text_field(&app, "Name").click();
    app.step();
    assert!(common::text_field(&app, "Name").is_focused());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Write more tests".into()));
    app.run_steps(2);
    common::save_record(&mut app);
    common::wait_until(&mut app, "the record saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !common::unsaved(app) && !common::saving(app)
    });

    let mut reader = hosted.opened();
    let page = reader
        .record_by_id(u32::try_from(fixture::LAYOUT).unwrap(), 1)
        .unwrap();
    let column = page.fields.iter().position(|f| f.name == "Name").unwrap();
    assert_eq!(
        page.records[0].values[column].as_deref(),
        Some("Write more tests")
    );
    reader.close();
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there as
/// `name`.png and add `name: line` to the folder's `readback.txt`.
fn shot(app: &mut common::App, name: &str, line: &str) {
    use std::io::Write as _;
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    let dir = Path::new(&dir);
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(dir.join(format!("{name}.png")))
        .expect("write png");
    let mut notes = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("readback.txt"))
        .expect("open readback.txt");
    writeln!(notes, "{name}: {line}").expect("write readback.txt");
}

/// What the editor of the field named `name` holds, if it has one.
fn editor(app: &common::App, name: &str) -> Option<String> {
    common::find_text_field(app, name)
        .map(|editor| editor.accesskit_node().value().unwrap_or_default())
}

/// The hosted file opened in a new window, on its Form layout.
fn open_window(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the Form layout", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app
}

#[test]
fn a_placed_number_field_keeps_the_text_typed_into_it() {
    let file = form_file();
    let hosted = hosted::host_file(
        "placed-number",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    let shown = editor(&app, "Count");
    shot(&mut app, "01-opened", &format!("Count editor {shown:?}"));
    // Browse edits a stored number field of one repetition (#185).
    assert_eq!(shown.as_deref(), Some("3"), "Count has no editor");

    common::text_field(&app, "Count").click();
    app.step();
    assert!(common::text_field(&app, "Count").is_focused());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("007".into()));
    app.run_steps(2);
    let typed = editor(&app, "Count");
    shot(&mut app, "02-typed", &format!("Count editor {typed:?}"));
    assert_eq!(typed.as_deref(), Some("007"));
    common::save_record(&mut app);
    // Until the saved record is read back, the editor shows the page's
    // earlier value (#608): wait for the saved one.
    common::wait_until(&mut app, "the saved Count", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !common::unsaved(app)
            && !common::saving(app)
            && editor(app, "Count").as_deref() == Some("007")
    });
    let saved = editor(&app, "Count");
    shot(&mut app, "03-saved", &format!("Count editor {saved:?}"));
    drop(app);

    // Stored as typed, as Set Field stores it: another session reads 007.
    let mut reader = hosted.opened();
    let page = reader
        .record_by_id(u32::try_from(fixture::LAYOUT).unwrap(), 1)
        .unwrap();
    let column = page.fields.iter().position(|f| f.name == "Count").unwrap();
    let stored = page.records[0].values[column].clone();
    reader.close();

    // A new window's editor takes the stored text back unchanged.
    let mut app = open_window(&hosted);
    let reopened = editor(&app, "Count");
    let line = format!("stored {stored:?}; Count editor {reopened:?}");
    shot(&mut app, "04-reopened", &line);
    assert_eq!(stored.as_deref(), Some("007"));
    common::text_field(&app, "Count").click();
    app.step();
    let editing = editor(&app, "Count");
    shot(
        &mut app,
        "05-reopened-editing",
        &format!("Count editor {editing:?}"),
    );
    assert_eq!(editing.as_deref(), Some("007"));
}

/// Replace the text of the editor of the field named `name` with `text`.
fn retype(app: &mut common::App, name: &str, text: &str) {
    common::text_field(app, name).click();
    app.step();
    assert!(common::text_field(app, name).is_focused());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// Browse edits a stored date field (#185): a two-digit year typed into
/// it is completed and the date stored in the file's date format, as data
/// entry reads it (`om_data::rules`); text that is not a date is refused
/// with nothing written and the draft kept.
#[test]
fn a_placed_date_field_stores_a_typed_date_and_refuses_other_text() {
    let file = form_file_with(fixture::DUE);
    let hosted = hosted::host_file(
        "placed-date",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    let shown = editor(&app, "Due");
    shot(&mut app, "date-01-opened", &format!("Due editor {shown:?}"));
    assert_eq!(shown.as_deref(), Some("10/1/2026"), "Due has no editor");

    retype(&mut app, "Due", "1/2/26");
    let typed = editor(&app, "Due");
    shot(&mut app, "date-02-typed", &format!("Due editor {typed:?}"));
    assert_eq!(typed.as_deref(), Some("1/2/26"));
    common::save_record(&mut app);
    // Until the saved record is read back, the editor shows the page's
    // earlier value (#608): wait for the saved one.
    common::wait_until(&mut app, "the saved Due", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !common::unsaved(app)
            && !common::saving(app)
            && editor(app, "Due").as_deref() == Some("1/2/2026")
    });
    let saved = editor(&app, "Due");
    shot(&mut app, "date-03-saved", &format!("Due editor {saved:?}"));
    assert_eq!(saved.as_deref(), Some("1/2/2026"));

    retype(&mut app, "Due", "abc");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| app.state().error().is_some());
    let refusal = app.state().error().map(str::to_owned);
    shot(&mut app, "date-04-refused", &format!("alert {refusal:?}"));
    assert_eq!(refusal.as_deref(), Some("Field Due requires a valid date."));
    drop(app);

    // Another session reads the completed date: the refused text was not
    // written.
    let mut reader = hosted.opened();
    let page = reader
        .record_by_id(u32::try_from(fixture::LAYOUT).unwrap(), 1)
        .unwrap();
    let column = page.fields.iter().position(|f| f.name == "Due").unwrap();
    let stored = page.records[0].values[column].clone();
    reader.close();
    assert_eq!(stored.as_deref(), Some("1/2/2026"));
}

/// A drop-down calendar (#178): a click on its icon enters the date field
/// and opens a calendar on the field's month; its arrow goes to the next
/// month, and a day picked there is the field's draft, in the file's date
/// format, saved as a typed date is.
#[test]
fn a_drop_down_calendar_picks_a_day_into_its_date_field() {
    let file = form_file_object(calendar_object(fixture::DUE));
    let hosted = hosted::host_file(
        "placed-calendar",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    assert_eq!(editor(&app, "Due").as_deref(), Some("10/1/2026"));
    assert!(app.query_by_label("10/15/2026").is_none());

    // The icon at the right edge of the field, not yet entered.
    let field = common::text_field(&app, "Due").rect();
    click_at(&mut app, egui::pos2(field.right() - 10.0, field.center().y));
    common::wait_until(&mut app, "the calendar", |app| {
        app.query_by_label("10/15/2026").is_some()
    });
    shot(
        &mut app,
        "calendar-01-open",
        "October 2026, 10/1/2026 marked",
    );
    assert!(common::text_field(&app, "Due").is_focused());

    common::click(&mut app, "Next Month");
    common::wait_until(&mut app, "November", |app| {
        app.query_by_label("11/20/2026").is_some()
    });
    // The year stepper goes a year forward and back.
    common::click(&mut app, "Next Year");
    common::wait_until(&mut app, "November 2027", |app| {
        app.query_by_label("11/20/2027").is_some()
    });
    common::click(&mut app, "Previous Year");
    common::wait_until(&mut app, "November 2026 again", |app| {
        app.query_by_label("11/20/2026").is_some()
    });
    common::click(&mut app, "11/20/2026");
    common::wait_until(&mut app, "the picked day", |app| {
        editor(app, "Due").as_deref() == Some("11/20/2026")
            && app.query_by_label("11/20/2026").is_none()
    });
    shot(
        &mut app,
        "calendar-02-picked",
        "Due 11/20/2026, calendar closed",
    );
    assert!(common::text_field(&app, "Due").is_focused());
    common::save_record(&mut app);
    common::wait_until(&mut app, "the record saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        !common::unsaved(app) && !common::saving(app)
    });
    drop(app);

    let mut reader = hosted.opened();
    let page = reader
        .record_by_id(u32::try_from(fixture::LAYOUT).unwrap(), 1)
        .unwrap();
    let column = page.fields.iter().position(|f| f.name == "Due").unwrap();
    let stored = page.records[0].values[column].clone();
    reader.close();
    assert_eq!(stored.as_deref(), Some("11/20/2026"));
}

/// A button placed through the host's authoring extension, as Layout
/// mode's Save Layout places one (#226): the saved file, hosted again by a
/// new server, shows it with its label on the Form layout, and in Browse
/// it performs its script with its parameter.
/// Layout mode's Button tool draws a button; Button Setup, which opens as
/// the drawing ends, gives it its label and a Perform Script step (the
/// script chosen in Specify Script, a parameter typed in "Perform Script"
/// Options); Save Layout places it (`DataProvider::place_layout_button`).
/// Browse runs the script with the parameter, and so does the saved file
/// hosted again by a new server.
#[test]
fn a_button_placed_in_layout_mode_runs_its_script_with_its_parameter() {
    let start = fixture::script(
        1,
        "Start",
        vec![fixture::set_field(NAME, "Get ( ScriptParameter )")],
    );
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let name = encode::field_object(1, &rect(110.0, 20.0, 200.0, 22.0), alias, NAME);
    let file = form_file_scripts(name, &[start]);
    let first = hosted::host_file(
        "button-first",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    // A parameter whose text does not decode (a truncated text token) is
    // refused by the host, with nothing written.
    let mut author = first.opened();
    let member = author.layout_member(fixture::LAYOUT).unwrap().unwrap();
    let polish = fixture::calc("\"From button\"").polish.unwrap();
    let refused = author.place_button(om_protocol::authoring::PlaceButton {
        layout: fixture::LAYOUT,
        expected: catalog_revision(&member),
        script: 1,
        frame: [60.0, 110.0, 84.0, 200.0],
        label: "Run".into(),
        parameter: Some((vec![0x13], polish)),
    });
    assert!(
        matches!(
            refused,
            Err(om_provider::AuthoringError::Refused { status: 5, .. })
        ),
        "{refused:?}"
    );
    let unchanged = author.layout_member(fixture::LAYOUT).unwrap().unwrap();
    assert_eq!(catalog_revision(&unchanged), catalog_revision(&member));
    author.close();

    let mut app = open_window(&first);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Name (object 1)").is_some()
    });
    common::click(&mut app, "Button Tools");
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    drag(
        &mut app,
        origin + egui::vec2(110.0, 60.0),
        origin + egui::vec2(200.0, 84.0),
    );
    common::wait_until(&mut app, "Button Setup", |app| {
        app.query_by_label("Specify Perform Script…").is_some()
    });
    common::fill(&mut app, "Label", "Run");
    common::click(&mut app, "Specify Perform Script…");
    common::click(&mut app, "Specify Script…");
    app.get_by_role_and_label(egui::Role::ListItem, "Start")
        .click();
    app.run_steps(2);
    ok(&mut app);
    // The options dialog is the top modal again from the next frame.
    app.run_steps(2);
    common::fill(&mut app, "Parameter", "\"From button\"");
    shot(&mut app, "button-01-options", "Perform Script options");
    ok(&mut app);
    let action = app
        .get_by_label("Button Action")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert_eq!(
        action,
        "Perform Script [ Specified: From list ; “Start” ; Parameter: \"From button\" ]"
    );
    shot(&mut app, "button-02-setup", &action);
    ok(&mut app);
    app.get_by_label("New button Run");
    shot(
        &mut app,
        "button-03-drawn",
        "the new button before Save Layout",
    );
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Button (object 2)").is_some()
    });
    assert!(app.query_by_label("New button Run").is_none());
    shot(&mut app, "button-04-saved", "the button saved");

    common::click(&mut app, "Exit Layout");
    common::wait_until(&mut app, "Browse", |app| {
        app.state().is_settled() && editor(app, "Name").as_deref() == Some("Write tests")
    });
    app.get_by_role_and_label(egui::Role::Button, "Run").click();
    app.run_steps(2);
    common::wait_until(&mut app, "Start set Name", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        editor(app, "Name").as_deref() == Some("From button")
    });
    shot(
        &mut app,
        "button-05-ran",
        "Name set by Start from the parameter",
    );
    drop(app);

    // The saved file, hosted again: the button is there and runs. The
    // script left its record open, so the file still holds the first value.
    let hosted = hosted::host_file(
        "button-again",
        first.file(),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    assert_eq!(editor(&app, "Name").as_deref(), Some("Write tests"));
    app.get_by_role_and_label(egui::Role::Button, "Run").click();
    app.run_steps(2);
    common::wait_until(&mut app, "Start set Name again", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        editor(app, "Name").as_deref() == Some("From button")
    });
}

/// Button Setup's Single Step: Specify Step lists the steps FileMaker Pro
/// allows on a button (no If); Set Field is chosen there and its options
/// (the field, the value) in its own options dialog. Save Layout places it
/// (operation 8) with its options in the button's step data, and Browse
/// runs the step.
#[test]
fn a_single_step_button_placed_in_layout_mode_runs_its_step() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let name = encode::field_object(1, &rect(110.0, 20.0, 200.0, 22.0), alias, NAME);
    let file = form_file_scripts(name, &[fixture::script(1, "Start", Vec::new())]);
    let hosted = hosted::host_file(
        "single-step",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    // A step FileMaker Pro keeps off buttons (If), sent to the host
    // directly, is refused with nothing written.
    let mut author = hosted.opened();
    let member = author.layout_member(fixture::LAYOUT).unwrap().unwrap();
    let refused = author.place_button_step(om_protocol::authoring::PlaceButtonStep {
        layout: fixture::LAYOUT,
        expected: catalog_revision(&member),
        frame: [60.0, 110.0, 84.0, 200.0],
        label: "If".into(),
        id: 68,
        value: 0,
        flags: 0,
        reserved: [0; 14],
        data: None,
    });
    assert!(
        matches!(
            refused,
            Err(om_provider::AuthoringError::Refused { status: 5, .. })
        ),
        "{refused:?}"
    );
    let unchanged = author.layout_member(fixture::LAYOUT).unwrap().unwrap();
    assert_eq!(catalog_revision(&unchanged), catalog_revision(&member));
    author.close();

    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Name (object 1)").is_some()
    });
    common::click(&mut app, "Button Tools");
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    drag(
        &mut app,
        origin + egui::vec2(110.0, 60.0),
        origin + egui::vec2(200.0, 84.0),
    );
    common::wait_until(&mut app, "Button Setup", |app| {
        app.query_by_label("Specify Perform Script…").is_some()
    });
    common::fill(&mut app, "Label", "Finish");
    common::click(&mut app, "Action");
    common::click(&mut app, "Single Step");
    common::wait_for_label(&mut app, "Search Steps");
    app.run_steps(2);
    common::fill(&mut app, "Search Steps", "If");
    assert!(
        app.query_by_role_and_label(egui::Role::ListItem, "If")
            .is_none(),
        "If is not a button's step"
    );
    retype(&mut app, "Search Steps", "Set Field");
    app.get_by_role_and_label(egui::Role::ListItem, "Set Field")
        .click();
    app.run_steps(2);
    shot(
        &mut app,
        "step-01-specify-step",
        "Specify Step with Set Field",
    );
    ok(&mut app);
    app.run_steps(2);
    common::click(&mut app, "Specify Set Field…");
    common::click(&mut app, "Specify Field…");
    app.get_by_role_and_label(egui::Role::ListItem, "Name")
        .click();
    app.run_steps(2);
    ok(&mut app);
    app.run_steps(2);
    common::fill(&mut app, "Value", "\"From step\"");
    shot(&mut app, "step-02-options", "Set Field options");
    ok(&mut app);
    let action = app
        .get_by_label("Button Action")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(
        action.starts_with("Set Field [") && action.contains("\"From step\""),
        "{action}"
    );
    shot(&mut app, "step-03-setup", &action);
    ok(&mut app);
    app.get_by_label("New button Finish");
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Button (object 2)").is_some()
    });

    // The host stored the step's options in the button's step data.
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    let layout = om_data::layout_placement::decode(&member).unwrap();
    let stored = layout
        .objects
        .get(2)
        .unwrap()
        .script_step()
        .unwrap()
        .clone();
    assert_eq!(stored.id, 76);
    let step = om_data::ScriptStep::from_layout(&stored).unwrap();
    assert!(step.calculation(0).is_some(), "{step:?}");

    common::click(&mut app, "Exit Layout");
    common::wait_until(&mut app, "Browse", |app| {
        app.state().is_settled() && editor(app, "Name").as_deref() == Some("Write tests")
    });
    app.get_by_role_and_label(egui::Role::Button, "Finish")
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "Set Field ran", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        editor(app, "Name").as_deref() == Some("From step")
    });
    shot(
        &mut app,
        "step-04-ran",
        "Name set by the button's Set Field",
    );
}

/// OK in the dialog on top: Button Setup, "Perform Script" Options and
/// Specify Script are stacked, each with its OK.
fn ok(app: &mut common::App) {
    app.get_all_by_label("OK").last().unwrap().click();
    app.run_steps(2);
}

/// A primary-button click at `pos`.
fn click_at(app: &mut common::App, pos: egui::Pos2) {
    app.hover_at(pos);
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    app.run_steps(2);
}

/// A primary-button drag from `from` to `to`, in steps.
fn drag(app: &mut common::App, from: egui::Pos2, to: egui::Pos2) {
    drag_with_modifiers(app, from, to, egui::Modifiers::NONE);
}

fn drag_with_modifiers(
    app: &mut common::App,
    from: egui::Pos2,
    to: egui::Pos2,
    modifiers: egui::Modifiers,
) {
    app.event(egui::Event::ModifiersChanged(modifiers));
    app.event(egui::Event::PointerMoved(from));
    app.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers,
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
        modifiers,
    });
    app.run_steps(3);
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    app.step();
}

/// Draw a new field with the Field tool from layout point `from` to `to`
/// and choose Name for it in the Inspector; its frame as the Inspector
/// shows it (left, top, width, height).
fn draw_name_field(app: &mut common::App, from: egui::Vec2, to: egui::Vec2) -> [f64; 4] {
    common::click(app, "Field Tools");
    let canvas = app.get_by_label("Layout canvas").rect();
    // The layout's origin: right of the part strip (22 points).
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    drag(app, origin + from, origin + to);
    common::click(app, "Field to Place");
    app.get_all_by_label("Name").last().unwrap().click();
    app.run_steps(3);
    assert!(app.get_all_by_label("New field Name").next().is_some());
    ["Left", "Top", "Width", "Height"].map(|side| {
        app.get_by_label(&format!("New Field {side}"))
            .accesskit_node()
            .numeric_value()
            .unwrap()
    })
}

fn save_layout(app: &mut common::App) {
    common::open_menu(app, "Layouts");
    common::click(app, "Save Layout");
}

/// A sidebar drop into the zoomed canvas stores its position in layout
/// points and keeps the full edit-box height (#315).
#[test]
fn a_sidebar_field_drop_saves_a_44_point_edit_box() {
    let file = form_file();
    let hosted = hosted::host_file(
        "sidebar-field-drop",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_for_label(&mut app, "Field: Count (object 1)");
    common::click(&mut app, "Objects");
    common::click(&mut app, "Fields");
    common::wait_for_label(&mut app, "Name");
    for _ in 0..2 {
        common::open_menu(&mut app, "View");
        common::click(&mut app, "Zoom In");
    }
    let source = app.get_by_label("Name").rect().center();
    let canvas = app.get_by_label("Layout canvas").rect();
    drag(&mut app, source, canvas.min + 2.0 * egui::vec2(132.0, 72.0));
    common::wait_for_label(&mut app, "New field Name");
    shot(&mut app, "sidebar-field-drop", "Name field draft");
    save_layout(&mut app);
    common::wait_until(&mut app, "the dropped field saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("New field Name").is_none()
    });
    let geometry = hosted
        .opened()
        .layout_geometry(fixture::LAYOUT.try_into().unwrap())
        .unwrap()
        .unwrap();
    let field = geometry
        .objects
        .iter()
        .find(|object| object.field_name.as_deref() == Some("Name"))
        .unwrap();
    assert_eq!((field.rect.left, field.rect.top), (110.0, 72.0));
    assert_eq!(field.rect.height(), 44.0);
}

/// Layout mode's Portal tool draws a portal; Portal Setup, which opens as
/// the drawing ends, offers the occurrences related to the layout's and the
/// number of rows, and Add Fields to Portal the occurrence's fields; Save
/// Layout places it with authoring operation 21
/// (`DataProvider::place_layout_portal`), and a new connection reads it.
#[test]
fn a_portal_drawn_in_layout_mode_is_set_up_and_saved() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut tree = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        NAME,
    ));
    fixture::add_relationship(
        &mut tree,
        1,
        (OCCURRENCE, NAME),
        0,
        (fixture::NOTES_OCCURRENCE, fixture::BODY),
    );
    let file = fixture::TempFile::from_tree("gui-place-portal", &tree);
    let hosted = hosted::host_file(
        "place-portal",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Name (object 1)").is_some()
    });
    common::click(&mut app, "Portal");
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    drag(
        &mut app,
        origin + egui::vec2(20.0, 60.0),
        origin + egui::vec2(320.0, 170.0),
    );
    common::wait_until(&mut app, "Portal Setup", |app| {
        app.query_by_label("Number of rows").is_some()
    });
    // The related Notes is chosen at first; two rows of 39 points fit.
    assert_eq!(
        app.get_by_label("Number of rows")
            .accesskit_node()
            .value()
            .as_deref(),
        Some("2")
    );
    // Seven rows of under 16 points leave no room for a field: refused
    // here, where the rows can still be changed.
    hosted::replace(&mut app, "Number of rows", "7");
    assert!(app
        .get_all_by_label("OK")
        .last()
        .unwrap()
        .accesskit_node()
        .is_disabled());
    hosted::replace(&mut app, "Number of rows", "3");
    shot(
        &mut app,
        "portal-01-setup",
        "Portal Setup for the drawn portal",
    );
    ok(&mut app);
    common::wait_until(&mut app, "Add Fields to Portal", |app| {
        app.query_by_role_and_label(egui::Role::List, "Included fields")
            .is_some()
    });
    app.get_by_role_and_label(egui::Role::Button, "Body")
        .click();
    app.run_steps(2);
    common::click(&mut app, "Move");
    shot(&mut app, "portal-02-fields", "Notes::Body included");
    ok(&mut app);
    app.get_by_label("New portal Notes");
    shot(
        &mut app,
        "portal-03-drawn",
        "the new portal before Save Layout",
    );
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("New portal Notes").is_none()
    });
    shot(&mut app, "portal-04-saved", "the portal saved");

    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    // Layout mode names the portal's field by its occurrence and draws
    // `::Body` in it.
    let geometry = reader
        .layout_geometry(fixture::LAYOUT.try_into().unwrap())
        .unwrap()
        .unwrap();
    assert!(
        geometry
            .objects
            .iter()
            .any(|o| o.field_name.as_deref() == Some("Notes::Body")),
        "{:?}",
        geometry.objects
    );
    reader.close();
    let layout = om_data::layout_placement::decode(&member).unwrap();
    let portal = layout
        .objects
        .get(*layout.objects.root.last().unwrap())
        .unwrap();
    assert_eq!(portal.kind(), om_layout::ObjectKind::Portal);
    let setup = portal.portal.as_ref().unwrap();
    assert_eq!(setup.num_rows, 3.0);
    let bounds = portal.bounds.as_ref().unwrap();
    assert_eq!(
        (bounds.left, bounds.top, bounds.right, bounds.bottom),
        (20.0, 60.0, 320.0, 170.0)
    );
    let field = layout.objects.get(portal.children[0]).unwrap();
    assert_eq!(
        field.field_spec().map(|s| s.field_key),
        om_data::layout_placement::serial_value(fixture::BODY)
    );
}

/// Layout mode's New Layout / Report adds a Form layout on the occurrence
/// chosen, named as typed, with the operation `om-author`'s
/// `create_layout` uses (`DataProvider::create_layout`): the window opens
/// it in Layout mode, the Field tool places a field on it (a new layout is
/// refused while the field is not saved), and Browse shows the field. From
/// Browse with a record being edited, Manage Layouts' New saves the record
/// and opens its new layout in Layout mode. Both layouts are listed over a
/// new connection.
#[test]
fn new_layout_report_adds_a_form_layout_that_takes_a_field() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut root = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        COUNT,
    ));
    fixture::index_layouts(&mut root);
    let file = fixture::TempFile::from_tree("gui-new-layout", &root);
    let hosted = hosted::host_file(
        "new-layout",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::click(&mut app, "New Layout / Report");
    common::wait_for_label(&mut app, "New Layout/Report");
    // The name starts as the layout's occurrence's.
    assert_eq!(
        common::text_field(&app, "Layout Name")
            .accesskit_node()
            .value()
            .as_deref(),
        Some("Tasks")
    );
    shot(&mut app, "21-new-layout", "the dialog");
    hosted::replace(&mut app, "Layout Name", "Task Entry");
    common::click(&mut app, "Finish");
    common::wait_until(&mut app, "the new layout in Layout mode", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && app.query_by_label("Layout canvas").is_some()
            && app.query_by_label("Field: Count (object 1)").is_none()
            && layout_shown(app) == "Task Entry"
    });
    shot(&mut app, "22-created", "the new layout in Layout mode");
    common::click(&mut app, "Field Tools");
    draw_name_field(&mut app, egui::vec2(110.0, 60.0), egui::vec2(330.0, 82.0));
    common::click(&mut app, "New Layout / Report");
    let refused = app.state().error().unwrap_or_default().to_owned();
    assert!(refused.contains("not saved"), "{refused}");
    assert!(app.query_by_label("New Layout/Report").is_none());
    common::wait_for_label(&mut app, "OK");
    common::click(&mut app, "OK");
    save_layout(&mut app);
    common::wait_until(&mut app, "the field saved on the new layout", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Field: Name (object 1)").is_some()
    });
    common::click(&mut app, "Exit Layout");
    common::wait_until(&mut app, "the new layout in Browse", |app| {
        app.state().is_settled() && editor(app, "Name").is_some()
    });
    shot(&mut app, "23-browse", "the new layout in Browse");

    // A record being edited is saved before the next new layout opens.
    hosted::replace(&mut app, "Name", "Typed");
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Manage");
    common::click(&mut app, "Layouts...");
    common::wait_for_label(&mut app, "Manage Layouts");
    common::click(&mut app, "New");
    common::wait_for_label(&mut app, "New Layout/Report");
    hosted::replace(&mut app, "Layout Name", "Task Review");
    common::click(&mut app, "Finish");
    common::wait_until(&mut app, "the second new layout in Layout mode", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && app.query_by_label("Layout canvas").is_some()
            && layout_shown(app) == "Task Review"
    });
    drop(app);

    // Over a new connection, both layouts are listed after the stored one,
    // and the typed name was saved.
    let mut reader = hosted.opened();
    let names: Vec<String> = reader
        .layouts()
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    let first = reader.records(1, 0, 1).unwrap();
    reader.close();
    assert_eq!(
        &names[names.len() - 2..],
        ["Task Entry".to_owned(), "Task Review".to_owned()],
        "{names:?}"
    );
    let name = first.fields.iter().position(|f| f.name == "Name").unwrap();
    assert_eq!(first.records[0].values[name].as_deref(), Some("Typed"));
}

/// The canvas rect of the object with accessible label `label`, in layout
/// points from the layout's origin (right of the part strip).
fn canvas_frame(app: &common::App, label: &str) -> [f32; 4] {
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    let rect = app.get_by_label(label).rect();
    [
        rect.left() - origin.x,
        rect.top() - origin.y,
        rect.right() - origin.x,
        rect.bottom() - origin.y,
    ]
}

/// The Selection tool moves a stored field by dragging it and resizes it by
/// a handle and in the Inspector; Save Layout stores its new frame with the
/// operation `om-author`'s `set_object_frames` uses
/// (`DataProvider::set_layout_object_frames`), and the field is there when
/// the file is opened again. Revert Layout drops an unsaved move.
#[test]
fn a_stored_field_is_moved_and_resized_and_kept() {
    let file = form_file();
    let hosted = hosted::host_file(
        "moved-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    // Count is at left 110, top 20, 120 by 22: dragged 40 right, 30 down.
    drag(
        &mut app,
        origin + egui::vec2(170.0, 31.0),
        origin + egui::vec2(210.0, 61.0),
    );
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [150.0, 50.0, 270.0, 72.0]
    );
    let left = app
        .get_by_label("Object Left")
        .accesskit_node()
        .numeric_value();
    assert_eq!(left, Some(150.0));
    // Another layout, or Browse, would drop the move: both wait for Save
    // Layout or Revert Layout.
    common::click(&mut app, "Layout");
    app.query_all(egui_kittest::kittest::By::new().label("Notes"))
        .last()
        .unwrap()
        .click();
    app.run_steps(3);
    let refused = app.state().error().unwrap_or_default().to_owned();
    assert!(refused.contains("not saved"), "{refused}");
    common::wait_for_label(&mut app, "OK");
    common::click(&mut app, "OK");
    common::click(&mut app, "Exit Layout");
    let refused = app.state().error().unwrap_or_default().to_owned();
    assert!(refused.contains("not saved"), "{refused}");
    common::wait_for_label(&mut app, "OK");
    common::click(&mut app, "OK");
    assert_eq!(layout_shown(&app), "Tasks");
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [150.0, 50.0, 270.0, 72.0]
    );
    // Revert Layout puts it back.
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Revert Layout");
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [110.0, 20.0, 230.0, 42.0]
    );
    // Again at 200%, then 30 layout points wider by its right handle.
    shot(&mut app, "layout-zoom-100", "the canvas at Actual Size");
    for _ in 0..2 {
        common::open_menu(&mut app, "View");
        common::click(&mut app, "Zoom In");
    }
    shot(
        &mut app,
        "layout-zoom-200",
        "the same canvas at 200 percent",
    );
    let from = app.get_by_label("Field: Count (object 1)").rect().center();
    drag(&mut app, from, from + egui::vec2(80.0, 60.0));
    let handle = app.get_by_label("Resize right").rect().center();
    drag(&mut app, handle, handle + egui::vec2(60.0, 0.0));
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Actual Size");
    // The Inspector still measures in layout points.
    // A click on the Inspector's Height starts typing a value into it.
    app.get_by_label("Object Height").click();
    app.run_steps(3);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("30".into()));
    app.key_press(egui::Key::Enter);
    app.run_steps(3);
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [150.0, 50.0, 300.0, 80.0]
    );
    shot(&mut app, "31-moved", "Count moved and resized, not saved");
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && app.query_by_label("Field: Count (object 1)").is_some()
            && app.query_by_label("Resize right").is_none()
    });
    drop(app);

    // Over a new connection, and in a new window, it is where it was put.
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    let layout = om_data::layout_placement::decode(&member).unwrap();
    let count = layout.objects.get(1).unwrap();
    assert_eq!(
        count.bounds.as_ref(),
        Some(&om_layout::Rect {
            top: 50.0,
            left: 150.0,
            bottom: 80.0,
            right: 300.0,
            other: Vec::new(),
        })
    );
    let mut again = open_window(&hosted);
    common::click(&mut again, "Edit Layout");
    common::wait_until(&mut again, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    assert_eq!(
        canvas_frame(&again, "Field: Count (object 1)"),
        [150.0, 50.0, 300.0, 80.0]
    );
    shot(&mut again, "32-reopened", "Count where it was put");
}

/// Arrange > Align > Right Edges on two stored objects: the canvas shows
/// them aligned at once, as a change that is not saved yet; Save Layout
/// stores the new frame, read back over a new connection. The frames come
/// from `om_data::layout_arrange`, the operation `om-author`'s
/// arrange_objects runs.
#[test]
fn arrange_aligns_the_selected_objects_and_save_layout_stores_them() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::By;

    let file = form_file();
    let hosted = hosted::host_file(
        "arranged-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_for_label(&mut app, "Field: Count (object 1)");
    // A second stored object: Name, placed and saved below Count, whose
    // frame is 110, 20 to 230, 42.
    draw_name_field(&mut app, egui::vec2(150.0, 80.0), egui::vec2(250.0, 102.0));
    save_layout(&mut app);
    common::wait_until(&mut app, "Name saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Field: Name (object 2)").is_some()
    });
    let origin = app.get_by_label("Layout canvas").rect().min + egui::vec2(22.0, 0.0);
    let click_at = |app: &mut common::App, point: egui::Vec2, modifiers: egui::Modifiers| {
        let pos = origin + point;
        app.event(egui::Event::ModifiersChanged(modifiers));
        app.event(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            app.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            });
            app.step();
        }
        app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        app.run_steps(3);
    };
    click_at(&mut app, egui::vec2(170.0, 31.0), egui::Modifiers::NONE);
    click_at(&mut app, egui::vec2(200.0, 91.0), egui::Modifiers::SHIFT);
    app.get_by_label("2 objects selected");

    common::open_menu(&mut app, "Arrange");
    let align = app
        .query_all(By::new().role(Role::Button).label("Align"))
        .next()
        .expect("the Align submenu")
        .rect();
    app.event(egui::Event::PointerMoved(align.center()));
    app.run_steps(6);
    shot(
        &mut app,
        "41-arrange-align",
        "Arrange > Align with two objects",
    );
    common::click(&mut app, "Right Edges");
    // Count's right edge moves to Name's, the rightmost; Name stays.
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [130.0, 20.0, 250.0, 42.0]
    );
    assert_eq!(
        canvas_frame(&app, "Field: Name (object 2)"),
        [150.0, 80.0, 250.0, 102.0]
    );
    shot(
        &mut app,
        "42-aligned",
        "Count's right edge at Name's, not saved",
    );
    let stored = |hosted: &hosted::Hosted| {
        let mut reader = hosted.opened();
        let geometry = reader
            .layout_geometry(fixture::LAYOUT.try_into().unwrap())
            .unwrap()
            .unwrap();
        reader.close();
        geometry
            .objects
            .iter()
            .find(|o| o.field_name.as_deref() == Some("Count"))
            .map(|o| (o.rect.left, o.rect.right))
    };
    assert_eq!(stored(&hosted), Some((110.0, 230.0)), "not saved yet");
    save_layout(&mut app);
    common::wait_until(&mut app, "the arrangement saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("2 objects selected").is_none()
    });
    assert_eq!(stored(&hosted), Some((130.0, 250.0)));
}

/// Shift-click selects saved objects and field drafts together. Moving and
/// deleting the selection must never mutate only an arbitrary single member.
#[test]
fn shift_selection_moves_and_deletes_saved_objects_and_field_drafts_together() {
    let file = form_file();
    let hosted = hosted::host_file(
        "layout-selection",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_for_label(&mut app, "Field: Count (object 1)");
    let origin = app.get_by_label("Layout canvas").rect().min + egui::vec2(22.0, 0.0);
    let click_at = |app: &mut common::App, point: egui::Vec2, shift: bool| {
        let pos = origin + point;
        let modifiers = if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        app.event(egui::Event::ModifiersChanged(modifiers));
        app.event(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            app.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            });
            app.step();
        }
        app.run_steps(3);
    };
    let original = hosted
        .opened()
        .layout_member(fixture::LAYOUT)
        .unwrap()
        .unwrap();
    draw_name_field(&mut app, egui::vec2(110.0, 80.0), egui::vec2(210.0, 102.0));
    draw_name_field(&mut app, egui::vec2(280.0, 80.0), egui::vec2(380.0, 102.0));
    click_at(&mut app, egui::vec2(170.0, 31.0), false);
    click_at(&mut app, egui::vec2(160.0, 91.0), true);
    click_at(&mut app, egui::vec2(330.0, 91.0), true);
    app.get_by_label("3 objects selected");
    assert!(app.query_by_label("Object Left").is_none());
    assert!(app.query_by_label("Field to Place").is_none());
    click_at(&mut app, egui::vec2(160.0, 91.0), true);
    app.get_by_label("2 objects selected");
    click_at(&mut app, egui::vec2(330.0, 91.0), false);
    assert!(app.query_by_label("2 objects selected").is_none());
    click_at(&mut app, egui::vec2(440.0, 160.0), false);
    app.get_by_label("No object selected");
    click_at(&mut app, egui::vec2(170.0, 31.0), false);
    click_at(&mut app, egui::vec2(160.0, 91.0), true);
    // A Shift-drag of the unselected third member adds it, even when
    // egui classifies a held/slightly moved press as a drag rather than click.
    drag_with_modifiers(
        &mut app,
        origin + egui::vec2(330.0, 91.0),
        origin + egui::vec2(350.0, 111.0),
        egui::Modifiers::SHIFT,
    );
    app.get_by_label("3 objects selected");
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [130.0, 40.0, 250.0, 62.0]
    );
    let drafts: Vec<_> = app
        .get_all_by_label("New field Name")
        .map(|n| n.rect().min - origin)
        .collect();
    assert_eq!(drafts, [egui::vec2(130.0, 100.0), egui::vec2(300.0, 100.0)]);
    app.key_press(egui::Key::Delete);
    app.run_steps(3);
    assert!(app.query_by_label("Field: Count (object 1)").is_none());
    assert!(app.query_by_label("New field Name").is_none());
    let unchanged = hosted
        .opened()
        .layout_member(fixture::LAYOUT)
        .unwrap()
        .unwrap();
    assert_eq!(
        unchanged, original,
        "selection edits must stay in the draft"
    );
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Revert Layout");
    assert_eq!(
        canvas_frame(&app, "Field: Count (object 1)"),
        [110.0, 20.0, 230.0, 42.0]
    );
    assert!(app.query_by_label("New field Name").is_none());
    // Save a mixed move/addition and check it through a fresh connection.
    draw_name_field(&mut app, egui::vec2(110.0, 80.0), egui::vec2(210.0, 102.0));
    click_at(&mut app, egui::vec2(170.0, 31.0), true);
    app.get_by_label("2 objects selected");
    drag(
        &mut app,
        origin + egui::vec2(160.0, 91.0),
        origin + egui::vec2(190.0, 101.0),
    );
    save_layout(&mut app);
    common::wait_until(&mut app, "the mixed selection saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("New field Name").is_none()
    });
    drop(app);
    let mut reader = hosted.opened();
    let geometry = reader
        .layout_geometry(fixture::LAYOUT.try_into().unwrap())
        .unwrap()
        .unwrap();
    let count = geometry
        .objects
        .iter()
        .find(|o| o.field_name.as_deref() == Some("Count"))
        .unwrap();
    let name = geometry
        .objects
        .iter()
        .find(|o| o.field_name.as_deref() == Some("Name"))
        .unwrap();
    assert_eq!((count.rect.left, count.rect.top), (140.0, 30.0));
    assert_eq!((name.rect.left, name.rect.top), (140.0, 90.0));
    reader.close();
}

/// Layout mode's Delete key and Edit > Clear delete a selected object as
/// an unsaved change: the canvas and the Objects pane stop showing it, and
/// Revert Layout brings it back. Save Layout deletes it with the operation
/// `om-author`'s `delete_objects` uses
/// (`DataProvider::delete_layout_objects`), and it stays deleted when the
/// file is opened again.
#[test]
fn a_stored_field_is_deleted_and_stays_deleted() {
    let file = form_file();
    let hosted = hosted::host_file(
        "deleted-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    // Count is object 1, as the move test reads it.
    let count = 1;
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::click(&mut app, "Field: Count (object 1)");
    shot(&mut app, "40-selected", "Count selected, before Delete");
    app.key_press(egui::Key::Delete);
    app.run_steps(3);
    assert!(app.query_by_label("Field: Count (object 1)").is_none());
    // Revert Layout brings it back.
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Revert Layout");
    assert!(app.query_by_label("Field: Count (object 1)").is_some());
    // Edit > Clear deletes it too.
    common::click(&mut app, "Field: Count (object 1)");
    common::open_menu(&mut app, "Edit");
    common::click(&mut app, "Clear");
    assert!(app.query_by_label("Field: Count (object 1)").is_none());
    shot(&mut app, "41-deleted", "Count deleted, not saved");
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && !app.state().layout_unsaved()
            && app.query_by_label("Field: Count (object 1)").is_none()
    });
    drop(app);

    // Over a new connection, and in a new window, it is gone.
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert!(om_data::layout_objects::already_deleted(&member, &[count]));
    let mut again = open_window(&hosted);
    common::click(&mut again, "Edit Layout");
    common::wait_until(&mut again, "the layout canvas", |app| {
        app.query_by_label("Layout canvas").is_some() && app.state().is_settled()
    });
    assert!(again.query_all(egui_kittest::kittest::By::new()).all(|n| !n
        .accesskit_node()
        .label()
        .unwrap_or_default()
        .contains("Count")));
    shot(&mut again, "42-reopened", "Count gone after reopening");
}

/// The Inspector's Control style and Values from give a stored field and a
/// new one another control, saved by one Save Layout with the operation
/// `om-author`'s `set_field_control` uses (`DataProvider::set_field_control`),
/// the new field's after it is placed: Name shows as a pop-up menu of the
/// file's value list and the new field as a checkbox set of it, over a new
/// connection too.
#[test]
fn the_inspector_sets_a_stored_and_a_new_fields_control() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut tree = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        NAME,
    ));
    tree.put(
        &[&[0x21], &[5], &[1]],
        OwnedNode::list(vec![1], vec![om_scripts::text_node(&[0x10], "Statuses")]),
    );
    let file = fixture::TempFile::from_tree("gui-field-controls", &tree);
    let hosted = hosted::host_file(
        "controls-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let list = serial::id_of_raw(1).unwrap();
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Name (object 1)").is_some()
    });
    common::click(&mut app, "Field: Name (object 1)");
    common::click(&mut app, "Data");
    combo(&mut app, "Control style");
    common::click(&mut app, "Pop-up Menu");
    combo(&mut app, "Values from");
    common::click(&mut app, "Statuses");
    shot(
        &mut app,
        "60-stored",
        "Name as a pop-up menu of Statuses, not saved",
    );

    common::click(&mut app, "Field Tools");
    draw_name_field(&mut app, egui::vec2(110.0, 80.0), egui::vec2(330.0, 102.0));
    combo(&mut app, "Control style");
    common::click(&mut app, "Checkbox Set");
    combo(&mut app, "Values from");
    common::click(&mut app, "Statuses");
    shot(
        &mut app,
        "61-new",
        "a new Name field as a checkbox set, not saved",
    );
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && !app.state().layout_unsaved()
    });
    drop(app);

    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert!(om_data::layout_objects::already_control(
        &member,
        1,
        2,
        Some(list)
    ));
    // The new field and its label take the next keys.
    assert!((2..=3).any(|key| om_data::layout_objects::already_control(
        &member,
        key,
        3,
        Some(list)
    )));
}

/// Open the pop-up menu labelled `label`.
fn combo(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(egui::accesskit::Role::ComboBox, label)
        .click();
    app.run_steps(2);
}

/// Layout mode's Text tool places a text with the operation `om-author`'s
/// `place_text` uses (`DataProvider::place_layout_text`): a drag draws its
/// frame, the text is typed on the canvas, and Save Layout stores it on
/// top of the layout's objects, where a new connection and Browse find it.
#[test]
fn a_text_placed_with_the_text_tool_is_saved_and_shown() {
    let file = form_file();
    let hosted = hosted::host_file(
        "text-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    // Count selected, then a text drawn: Count is no longer what Edit >
    // Clear deletes.
    common::click(&mut app, "Field: Count (object 1)");
    common::click(&mut app, "Text Tools");
    let canvas = app.get_by_label("Layout canvas").rect();
    let origin = canvas.min + egui::vec2(22.0, 0.0);
    drag(
        &mut app,
        origin + egui::vec2(20.0, 100.0),
        origin + egui::vec2(140.0, 120.0),
    );
    // The new text takes the keyboard.
    app.event(egui::Event::Text("Notes".into()));
    app.run_steps(3);
    assert_eq!(
        app.get_by_label("New text")
            .accesskit_node()
            .value()
            .as_deref(),
        Some("Notes")
    );
    common::open_menu(&mut app, "Edit");
    common::click(&mut app, "Clear");
    app.key_press(egui::Key::Escape);
    app.run_steps(3);
    assert!(app.query_by_label("Field: Count (object 1)").is_some());
    shot(&mut app, "51-text", "a new text typed, not saved");
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && !app.state().layout_unsaved()
            && app.query_by_label("New text").is_none()
    });
    drop(app);

    // Over a new connection: the text on top, at the frame drawn.
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    let layout = om_data::layout_placement::decode(&member).unwrap();
    let text = layout
        .objects
        .get(*layout.objects.root.last().unwrap())
        .unwrap();
    assert!(layout.objects.get(1).is_some(), "Count was kept");
    assert_eq!(text.text.as_deref(), Some("Notes"));
    let at = text.bounds.as_ref().unwrap();
    assert_eq!(
        [at.left, at.top, at.width(), at.height()],
        [20.0, 100.0, 120.0, 20.0]
    );
    // A new window shows it in Browse.
    let mut again = open_window(&hosted);
    common::wait_until(&mut again, "Browse", |app| {
        app.state().is_settled() && app.query_by_label("Notes").is_some()
    });
    shot(&mut again, "52-browse", "the text in Browse");
}

/// Layouts > Duplicate Layout copies the layout the window shows with the
/// operation `om-author`'s `duplicate_layout` uses
/// (`DataProvider::duplicate_layout`), and the window shows the copy,
/// "Tasks Copy", in Layout mode with the source's objects.
#[test]
fn duplicate_layout_shows_the_copy_in_layout_mode() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut root = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        COUNT,
    ));
    fixture::index_layouts(&mut root);
    let file = fixture::TempFile::from_tree("gui-duplicate-layout", &root);
    let hosted = hosted::host_file(
        "duplicate-layout",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Duplicate Layout");
    common::wait_until(&mut app, "the copy in Layout mode", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && layout_shown(app) == "Tasks Copy"
            && app.query_by_label("Field: Count (object 1)").is_some()
    });
    shot(&mut app, "61-copy", "the copy in Layout mode");
    let mut reader = hosted.opened();
    let names: Vec<String> = reader
        .layouts()
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    reader.close();
    assert_eq!(names, ["Tasks", "Notes", "Tasks Copy"]);
}

/// Layouts > Delete Layout... asks "Permanently delete this layout?" as
/// FileMaker Pro asks; Cancel keeps the layout, Delete deletes it with the
/// operation `om-author`'s `delete_layout` uses
/// (`DataProvider::delete_layout`), and the window shows the next layout in
/// Layout mode.
#[test]
fn delete_layout_asks_then_shows_the_next_layout() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut root = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        COUNT,
    ));
    fixture::index_layouts(&mut root);
    let file = fixture::TempFile::from_tree("gui-delete-layout", &root);
    let hosted = hosted::host_file(
        "delete-layout",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Delete Layout...");
    common::wait_for_label(&mut app, "Permanently delete this layout?");
    shot(&mut app, "71-ask", "Delete Layout's alert");
    common::click(&mut app, "Cancel");
    app.run_steps(3);
    assert_eq!(layout_shown(&app), "Tasks");
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Delete Layout...");
    common::wait_for_label(&mut app, "Permanently delete this layout?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "the next layout in Layout mode", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
            && layout_shown(app) == "Notes"
            && app.query_by_label("Layout canvas").is_some()
    });
    shot(&mut app, "72-deleted", "the next layout after the delete");
    let mut reader = hosted.opened();
    let names: Vec<String> = reader
        .layouts()
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    reader.close();
    assert_eq!(names, ["Notes"]);
}

/// Layouts > Layout Setup... renames the layout, turns two General options
/// over, makes List view available and prints within fixed page margins
/// with the operations `om-author`'s `layout_setup` uses
/// (`DataProvider::set_layout_setup`); the layout menu shows the new name.
#[test]
fn layout_setup_renames_the_layout_and_sets_its_views_and_options() {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut root = form_tree(encode::field_object(
        1,
        &rect(110.0, 20.0, 120.0, 22.0),
        alias,
        COUNT,
    ));
    fixture::index_layouts(&mut root);
    // Tasks Form only, in its member and its entry, as FileMaker stores
    // every layout's view flags.
    let member: [&[u8]; 4] = [&[4], &[5], &[1], &[0xf1]];
    let entry: [&[u8]; 5] = [&[4], &[1], &[7], &[1], &[0xf1]];
    for path in [&member[..], &entry[..]] {
        let flags = 0x10f3_080d_u64.to_be_bytes().to_vec();
        root.put(path, OwnedNode::leaf(Vec::new(), flags));
    }
    let file = fixture::TempFile::from_tree("gui-layout-setup", &root);
    let hosted = hosted::host_file(
        "layout-setup",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Layout Setup...");
    common::wait_until(&mut app, "Layout Setup", |app| {
        app.query_by_role_and_label(egui::accesskit::Role::TextInput, "Layout Name")
            .is_some()
    });
    // It opens with the name selected, as FileMaker Pro's does: typing
    // replaces it.
    app.run_steps(2);
    assert!(common::text_field(&app, "Layout Name").is_focused());
    shot(&mut app, "80-opened", "Layout Setup as it opens");
    app.input_mut()
        .events
        .push(egui::Event::Text("Task Board".into()));
    app.run_steps(2);
    // Quick Find off sets its bit; the record indicator on clears its
    // (re/layout-model.md 10.11).
    common::click(&mut app, "Enable Quick Find");
    common::click(&mut app, "Show current record indicator in List View");
    shot(&mut app, "81-general", "Layout Setup's General tab");
    common::click(&mut app, "Views");
    common::click(&mut app, "List View");
    shot(&mut app, "82-views", "Layout Setup's Views tab");
    // Printing: the box on, and the left margin typed; the others stay as
    // shown (re/layout-model.md 4.2).
    common::click(&mut app, "Printing");
    common::click(&mut app, "Use fixed page margins");
    app.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Left")
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.input_mut().events.push(egui::Event::Text("70".into()));
    app.run_steps(3);
    shot(&mut app, "83-printing", "Layout Setup's Printing tab");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the renamed layout", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && layout_shown(app) == "Task Board"
    });
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert_eq!(
        om_data::layout_catalog::member_name(&member).as_deref(),
        Some("Task Board")
    );
    let flags = member.child(&[0xf1]).and_then(|n| n.value.clone());
    assert_eq!(flags, Some(0x00f3_8849_u64.to_be_bytes().to_vec()));
    let def = om_data::layout_placement::definition(&member).unwrap();
    let margins = def.custom_margins.unwrap();
    assert_eq!(
        (margins.top, margins.left, margins.bottom, margins.right),
        (36.0, 70.0, 36.0, 36.0)
    );
}

/// A double click on a stored text in Layout mode types into it; the words
/// are an unsaved change, which Save Layout (Ctrl+S, still typing) stores
/// with the operation `om-author`'s `set_text` uses
/// (`DataProvider::set_layout_text`); a new connection reads the new
/// words.
#[test]
fn a_stored_text_is_retyped_in_layout_mode_and_saved() {
    let mut tree = form_tree(encode::text_object(1, &rect(110.0, 20.0, 120.0, 20.0)));
    let layout = uint_segment(fixture::LAYOUT);
    tree.put(
        &[&[4], &[5], &layout, &[5], &[1], &[1]],
        om_scripts::text_node(&[], "Due"),
    );
    let file = fixture::TempFile::from_tree("gui-retype-text", &tree);
    let hosted = hosted::host_file(
        "retype-text",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Text (object 1)").is_some()
    });
    let at = app.get_by_label("Text (object 1)").rect().center();
    common::workspace::double_click_at(&mut app, at);
    common::wait_until(&mut app, "the text being typed", |app| {
        app.query_by_label("Text being typed").is_some()
    });
    app.input_mut()
        .events
        .push(egui::Event::Text(" date".into()));
    app.run_steps(2);
    shot(&mut app, "70-retyping", "the stored text being typed");
    // Saved with Ctrl+S while still typing: the words go as Return would
    // keep them.
    assert!(app.state().layout_unsaved());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    app.run_steps(2);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && !app.state().layout_unsaved()
    });
    shot(&mut app, "71-retyped", "the text saved with its new words");
    drop(app);

    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert!(om_data::layout_placement::already_words(
        &member, 1, "Due date"
    ));
}

/// Layouts > Part Setup... lists the layout's parts; Change opens Part
/// Definition on the body, which offers what FileMaker Pro offers a body,
/// and OK turns on a page break before each occurrence with the operation
/// `om-author`'s `set_part_options` uses (`DataProvider::set_part_options`);
/// the body keeps it over a new connection.
#[test]
fn part_setup_turns_on_the_bodys_page_break() {
    use egui::accesskit::Role;
    use om_data::layout_placement::part_options::PAGE_BREAK_BEFORE;
    let file = form_file();
    let hosted = hosted::host_file(
        "part-setup",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Part Setup...");
    common::wait_until(&mut app, "Part Setup", |app| {
        app.query_by_role_and_label(Role::Button, "Body").is_some()
    });
    app.get_by_role_and_label(Role::Button, "Body").click();
    app.run_steps(2);
    shot(
        &mut app,
        "90-part-setup",
        "Part Setup with the body selected",
    );
    common::click(&mut app, "Change");
    common::wait_until(&mut app, "Part Definition", |app| {
        app.query_by_label("Page break before each occurrence")
            .is_some()
    });
    // A body offers neither a sort field nor its row states here.
    assert!(app
        .get_by_label("Use alternate row state")
        .accesskit_node()
        .is_disabled());
    common::click(&mut app, "Page break before each occurrence");
    shot(
        &mut app,
        "91-part-definition",
        "Part Definition on the body",
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the part saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Done")
            .is_some_and(|done| !done.accesskit_node().is_disabled())
            && app.state().is_settled()
    });
    common::click(&mut app, "Done");
    drop(app);

    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    let parts = om_data::layout_placement::parts(&member).unwrap();
    assert_eq!(parts[0].flags & PAGE_BREAK_BEFORE, PAGE_BREAK_BEFORE);
}

/// Part Setup's Create adds a Header above the body, which moves the body's
/// field down by its 110 points, and Delete removes it again, each with the
/// operation `om-author`'s `change_parts` uses
/// (`DataProvider::set_layout_parts`). A Create made on a layout another
/// client has changed since is refused and closes Part Setup. Then the
/// Selection tool drags the body's lower boundary down, the Footer below
/// moving with it. A new connection reads each change.
#[test]
fn part_setup_creates_and_deletes_a_header_and_the_body_boundary_is_dragged() {
    use egui::accesskit::Role;
    let file = form_file();
    let hosted = hosted::host_file(
        "part-create",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let read = |hosted: &hosted::Hosted| {
        let mut reader = hosted.opened();
        let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
        reader.close();
        let parts: Vec<(i32, f64, f64)> = om_data::layout_placement::parts(&member)
            .unwrap()
            .iter()
            .map(|p| (p.part_type, p.top, p.height))
            .collect();
        let layout = om_data::layout_placement::decode(&member).unwrap();
        let top = layout
            .objects
            .get(1)
            .and_then(|o| o.bounds.clone())
            .unwrap()
            .top;
        (parts, top)
    };
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Part Setup...");
    common::wait_until(&mut app, "Part Setup", |app| {
        app.query_by_role_and_label(Role::Button, "Body").is_some()
    });
    let saved = |app: &mut common::App| {
        common::wait_until(app, "the parts saved", |app| {
            assert!(app.state().error().is_none(), "{:?}", app.state().error());
            app.query_by_label("Done")
                .is_some_and(|done| !done.accesskit_node().is_disabled())
                && app.state().is_settled()
        });
    };
    common::click(&mut app, "Create");
    common::wait_until(&mut app, "Part Definition", |app| {
        app.query_by_role_and_label(Role::RadioButton, "Header")
            .is_some()
    });
    app.get_by_role_and_label(Role::RadioButton, "Header")
        .click();
    app.run_steps(2);
    shot(
        &mut app,
        "92-part-create",
        "Create's Part Definition, Header chosen",
    );
    common::click(&mut app, "OK");
    saved(&mut app);
    assert_eq!(
        read(&hosted),
        (vec![(1, 0.0, 110.0), (4, 110.0, 200.0)], 130.0)
    );
    app.get_by_role_and_label(Role::Button, "Header").click();
    app.run_steps(2);
    shot(&mut app, "93-part-setup", "Part Setup with the new Header");
    common::click(&mut app, "Delete");
    saved(&mut app);
    assert_eq!(read(&hosted), (vec![(4, 0.0, 200.0)], 20.0));

    // Another client adds a Footer: the revision Part Setup holds is no
    // longer the layout's, so its Create is refused and the dialog closes
    // rather than keep stale parts.
    let mut other = hosted.opened();
    let shown = other.layout_member(fixture::LAYOUT).unwrap().unwrap();
    other
        .set_layout_parts(om_data::layout_placement::PartsChange {
            layout: u32::try_from(fixture::LAYOUT).unwrap(),
            expected: catalog_revision(&shown),
            change: om_data::layout_placement::PartChange::Add {
                part_type: 7,
                size: 22.0,
            },
        })
        .unwrap();
    other.close();
    common::click(&mut app, "Create");
    common::wait_until(&mut app, "Part Definition", |app| {
        app.query_by_role_and_label(Role::RadioButton, "Header")
            .is_some()
    });
    app.get_by_role_and_label(Role::RadioButton, "Header")
        .click();
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the conflict", |app| {
        app.state().error().is_some()
    });
    let error = app.state().error().unwrap().to_owned();
    assert!(error.contains("was changed since the revision"), "{error}");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the layout read again", |app| {
        app.state().is_settled() && app.query_by_label("Footer boundary").is_some()
    });
    assert!(
        app.query_by_role_and_label(Role::Button, "Create")
            .is_none(),
        "Part Setup closed"
    );
    assert_eq!(
        read(&hosted),
        (vec![(4, 0.0, 200.0), (7, 200.0, 22.0)], 20.0)
    );

    let edge = app.get_by_label("Body boundary").rect().center();
    drag(&mut app, edge, edge + egui::vec2(0.0, 40.0));
    common::wait_until(&mut app, "the body resized", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    shot(
        &mut app,
        "95-part-boundary",
        "the Body dragged 40 points taller",
    );
    drop(app);
    assert_eq!(
        read(&hosted),
        (vec![(4, 0.0, 240.0), (7, 240.0, 22.0)], 20.0)
    );
}

/// Layout mode's Selection tool drags the layout's right edge to set its
/// width, and Save Layout saves it with the operation `om-author`'s
/// `set_layout_width` uses (`DataProvider::set_layout_width`).
#[test]
fn dragging_the_layouts_right_edge_sets_its_width() {
    let file = form_file();
    let hosted = hosted::host_file(
        "layout-width",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let width = |hosted: &hosted::Hosted| {
        let mut reader = hosted.opened();
        let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
        reader.close();
        om_data::layout_placement::definition(&member)
            .unwrap()
            .base_width
    };
    let stored = width(&hosted);
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    let edge = app.get_by_label("Layout width").rect().center();
    drag(&mut app, edge, edge - egui::vec2(100.0, 0.0));
    shot(&mut app, "91-width", "the layout's right edge dragged in");
    save_layout(&mut app);
    common::wait_until(&mut app, "the saved width", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && width(&hosted) != stored
    });
    assert_eq!(width(&hosted), stored - 100.0);
}

/// The layout the window's layout pop-up shows.
fn layout_shown(app: &common::App) -> String {
    app.query_all(egui_kittest::kittest::By::new().label("Layout"))
        .find_map(|n| n.accesskit_node().value())
        .unwrap_or_default()
}

/// Layout mode's Field tool and Save Layout place a field with the
/// operation `om-author`'s `place_field` uses
/// (`DataProvider::place_layout_field`): the frame dragged on the canvas
/// and the field chosen in the Inspector, saved by the host only at the
/// layout revision the window showed. A layout another client changed
/// meanwhile is refused with nothing written; placed again on what is
/// there now, the field is saved, read back over a new connection as
/// `get_layout` reads it, and shown in Browse.
#[test]
fn a_field_placed_in_layout_mode_is_saved_at_the_shown_revision_and_shown() {
    let file = form_file();
    let hosted = hosted::host_file(
        "placed-in-layout-mode",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    );
    let mut app = open_window(&hosted);
    common::click(&mut app, "Edit Layout");
    common::wait_until(&mut app, "the layout canvas", |app| {
        app.query_by_label("Field: Count (object 1)").is_some()
    });
    common::click(&mut app, "Field Tools");
    let first = draw_name_field(&mut app, egui::vec2(110.0, 60.0), egui::vec2(330.0, 82.0));
    shot(&mut app, "11-drawn", &format!("new field frame {first:?}"));

    // Another client places Name lower down first: the shown revision is
    // no longer the layout's.
    let mut other = hosted.opened();
    let shown = other.layout_member(fixture::LAYOUT).unwrap().unwrap();
    other
        .place_field(PlaceField {
            layout: fixture::LAYOUT,
            expected: catalog_revision(&shown),
            table: fixture::TABLE,
            occurrence: OCCURRENCE,
            field: NAME,
            frame: [120.0, 110.0, 142.0, 330.0],
            label: None,
        })
        .unwrap();
    let changed = other.layout_member(fixture::LAYOUT).unwrap().unwrap();
    other.close();
    save_layout(&mut app);
    common::wait_until(&mut app, "the conflict", |app| {
        app.state().error().is_some()
    });
    let error = app.state().error().unwrap().to_owned();
    assert!(error.contains("changed since it was shown"), "{error}");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the layout read again", |app| {
        app.state().is_settled() && app.query_by_label("Field: Name (object 2)").is_some()
    });
    assert!(app.query_by_label("New field Name").is_some());
    shot(&mut app, "12-conflict", &error);
    let mut reader = hosted.opened();
    let unchanged = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert_eq!(catalog_revision(&unchanged), catalog_revision(&changed));

    // The refused draft is kept until the user explicitly reverts it.
    app.get_by_label("New field Name");
    common::open_menu(&mut app, "Layouts");
    common::click(&mut app, "Revert Layout");
    // Placed again on the layout as it is now.
    let frame = draw_name_field(&mut app, egui::vec2(110.0, 60.0), egui::vec2(330.0, 82.0));
    assert_eq!(frame, first);
    save_layout(&mut app);
    common::wait_until(&mut app, "the layout saved", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled() && app.query_by_label("Field: Name (object 3)").is_some()
    });
    assert!(app.query_by_label("New field Name").is_none());
    shot(&mut app, "13-saved", &format!("saved frame {frame:?}"));

    // Read back over a new connection, as `get_layout` reads it: the edit
    // box on top of the stack at the frame the Inspector showed, every
    // stored byte of the object store kept.
    let mut reader = hosted.opened();
    let member = reader.layout_member(fixture::LAYOUT).unwrap().unwrap();
    reader.close();
    assert_ne!(catalog_revision(&member), catalog_revision(&changed));
    let layout = om_data::layout_placement::decode(&member).unwrap();
    // The edit box, then its label (Create label is on at first): the
    // field's name, 90 points wide and 4 points left of it.
    assert_eq!(layout.objects.root[layout.objects.root.len() - 2..], [3, 4]);
    let label = layout.objects.get(4).unwrap();
    assert_eq!(label.text.as_deref(), Some("Name"));
    let at = label.bounds.as_ref().unwrap();
    assert_eq!(
        [at.left, at.top, at.width(), at.height()],
        [frame[0] - 94.0, frame[1], 90.0, frame[3]]
    );
    let placed = layout.objects.get(3).unwrap();
    let spec = placed.field_spec().unwrap();
    assert_eq!(
        (om_layout::occurrence_id(spec.table_key), spec.field_id()),
        (Some(OCCURRENCE), Some(NAME))
    );
    let bounds = placed.bounds.as_ref().unwrap();
    assert_eq!(
        [bounds.left, bounds.top, bounds.width(), bounds.height()],
        frame
    );
    let store = |m: &OwnedNode| om_data::layout_placement::stored_objects(m).unwrap().0;
    assert!(store(&member).starts_with(&store(&changed)));

    // Browse shows the record's Name in both placed edit boxes (and the
    // new label, a text named Name too).
    common::click(&mut app, "Exit Layout");
    let editor = |node: &egui_kittest::Node<'_>| {
        node.accesskit_node().role() == egui::accesskit::Role::TextInput
    };
    common::wait_until(&mut app, "Browse", |app| {
        app.state().is_settled() && app.query_all_by_label("Name").filter(editor).count() == 2
    });
    let names: Vec<String> = app
        .query_all_by_label("Name")
        .filter(editor)
        .filter_map(|node| node.accesskit_node().value().map(|v| v.to_string()))
        .collect();
    shot(&mut app, "14-browse", &format!("Name editors {names:?}"));
    assert_eq!(names, ["Write tests", "Write tests"]);
}
