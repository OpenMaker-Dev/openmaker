//! Drive the app through its UI with the mock provider, the way a user would.

mod common;

use common::{
    click, connect_to_mock, fill, harness, mock, open_menu, record_number, select_layout,
    text_field, wait_for_label, wait_for_value, wait_until,
};
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use om_data::mock::MockProvider;
use om_gui::{Mode, ViewMode};

fn is_disabled(app: &common::App, label: &str) -> bool {
    app.get_by_label(label).accesskit_node().is_disabled()
}

/// Whether the toggle-style control `label` is on.
fn is_on(app: &common::App, label: &str) -> bool {
    app.get_by_label(label).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
}

/// Without Home, the window opens on Open Network File, which reads the
/// path as it is typed and opens only one naming a host and a file.
#[test]
fn launch_shows_open_network_file() {
    let mut app = mock();
    app.get_by_label("Open Network File");
    app.get_by_label("A file on a host: fmnet:/host/File");
    assert!(is_disabled(&app, "Open"), "Open needs a host and file");
    // No document is open, so there is no status toolbar.
    assert!(app.query_by_label("Edit Layout").is_none());
    fill(&mut app, "Path", "fmnet:/fms.example.com:5004");
    app.get_by_label("Add the file’s name: fmnet:/host/File");
    assert!(is_disabled(&app, "Open"));
    fill(&mut app, "Path", "/Contacts");
    app.get_by_label("File “Contacts” on fms.example.com, port 5004");
    assert!(!is_disabled(&app, "Open"));
}

#[test]
fn open_a_file_then_browse_records() {
    let mut app = mock();
    connect_to_mock(&mut app);
    assert!(app.state().is_open());
    assert_eq!(app.state().mode(), Mode::Browse);
    assert_eq!(app.state().view(), ViewMode::Form);

    // The Browse chrome: found-set count, layout bar and the first record.
    app.get_by_label("42");
    app.get_by_label("Total (Unsorted)");
    let layout = app.get_by_role_and_label(egui::Role::ComboBox, "Layout");
    assert_eq!(layout.value().as_deref(), Some("Contact Details"));
    assert_eq!(record_number(&app), "1");
    assert!(is_disabled(&app, "Previous"));
    assert!(
        is_disabled(&app, "Show All"),
        "every record is already shown"
    );

    click(&mut app, "Next");
    assert_eq!(record_number(&app), "2");
    // Table view shows record values; the mock's Form view draws only
    // layout geometry.
    click(&mut app, "Table View");
    app.get(By::new().role(egui::Role::TextInput).value("First1"));

    // Records > Go to Record > Last crosses into the next fetch window.
    open_menu(&mut app, "Records");
    click(&mut app, "Go to Record");
    click(&mut app, "Last");
    wait_for_value(&mut app, "First41");
    assert_eq!(record_number(&app), "42");
    assert!(is_disabled(&app, "Next"));

    // List view shows the window of records around the current one.
    click(&mut app, "List View");
    assert_eq!(app.state().view(), ViewMode::List);
    app.get(By::new().role(egui::Role::TextInput).value("First40"));
    app.get(By::new().role(egui::Role::TextInput).value("First26"));
}

#[test]
fn form_view_draws_the_layout_geometry() {
    let mut app = mock();
    connect_to_mock(&mut app);
    // Each part is labelled, and each object is a box labelled with its kind.
    for part in ["Header", "Body", "Footer"] {
        app.get_by_label(part);
    }
    assert_eq!(app.get_all_by_label("Field").count(), 4);
    assert_eq!(app.get_all_by_label("Text").count(), 6);
    app.get_by_label("Button");
    app.get_by_label("Rectangle");
    // No generic field list.
    assert!(app.query_by_label("First Name").is_none());

    // One point is one pixel: the mock's field rows are 36 pt apart, and
    // the Body part starts 60 pt below the Header.
    let mut fields: Vec<f32> = app
        .get_all_by_label("Field")
        .map(|node| node.rect().top())
        .collect();
    fields.sort_by(f32::total_cmp);
    for pair in fields.windows(2) {
        assert!((pair[1] - pair[0] - 36.0).abs() < 0.01, "{fields:?}");
    }
    let header = app.get_by_label("Header").rect().top();
    let body = app.get_by_label("Body").rect().top();
    assert!((body - header - 60.0).abs() < 0.01, "{header} {body}");

    // Layouts without geometry keep the generic Form view.
    select_layout(&mut app, "Contacts List");
    wait_for_value(&mut app, "First0");
    app.get_by_label("First Name");
    assert!(app.query_by_label("Body").is_none());
    select_layout(&mut app, "Contact Details");
    wait_for_label(&mut app, "Body");
    assert!(app.query_by_label("First Name").is_none());
}

#[test]
fn a_layout_larger_than_the_window_scrolls() {
    let mut app = harness(Box::new(common::LargeLayoutProvider(MockProvider::new())));
    connect_to_mock(&mut app);
    let window = app.ctx.content_rect();
    let corner = app.get_by_label("Corner");
    assert!(!window.contains_rect(corner.rect()), "starts out of view");
    corner.scroll_to_me();
    app.run_steps(3);
    let corner = app.get_by_label("Corner").rect();
    assert!(window.contains_rect(corner), "{corner:?} in {window:?}");
}

#[test]
fn the_record_number_box_jumps_to_a_record() {
    let mut app = mock();
    connect_to_mock(&mut app);
    let field = text_field(&app, "Record Number");
    field.focus();
    app.step();
    // Replace the shown "1".
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    text_field(&app, "Record Number").type_text("30");
    app.key_press(egui::Key::Enter);
    app.run_steps(2);
    assert_eq!(record_number(&app), "30");
    click(&mut app, "List View");
    wait_for_value(&mut app, "First29");
}

#[test]
fn the_layout_popup_switches_layouts() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Next");
    select_layout(&mut app, "Contacts List");
    // These layouts share the contacts table. Form view falls back to
    // fields here and must keep the selected contact.
    wait_for_value(&mut app, "First1");
    assert_eq!(record_number(&app), "2");
    let layout = app.get_by_role_and_label(egui::Role::ComboBox, "Layout");
    assert_eq!(layout.value().as_deref(), Some("Contacts List"));

    // Crossing a fetch window must preserve the record's values and
    // calculation context as well as its number in the toolbar.
    open_menu(&mut app, "Records");
    click(&mut app, "Go to Record");
    click(&mut app, "Last");
    wait_for_value(&mut app, "First41");
    select_layout(&mut app, "Contact Card");
    wait_for_label(&mut app, "First41 Last41");
    assert_eq!(record_number(&app), "42");
}

#[test]
fn clicking_a_table_row_makes_it_current() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Table View");
    app.get(
        By::new()
            .role(egui::Role::TextInput)
            .value("user3@example.com"),
    )
    .click();
    app.run_steps(2);
    assert_eq!(record_number(&app), "4");
    click(&mut app, "Form View");
    assert_eq!(record_number(&app), "4");
}

#[test]
fn the_view_menu_switches_views_and_modes() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_menu(&mut app, "View");
    click(&mut app, "View as List");
    assert_eq!(app.state().view(), ViewMode::List);

    open_menu(&mut app, "View");
    click(&mut app, "Find Mode");
    assert_eq!(app.state().mode(), Mode::Find);
    // Find mode swaps the Records menu for Requests.
    app.get_by_label("Requests");
    assert!(app.query_by_label("Records").is_none());

    open_menu(&mut app, "View");
    click(&mut app, "Browse Mode");
    assert_eq!(app.state().mode(), Mode::Browse);
}

#[test]
fn mode_shortcuts_switch_modes() {
    let mut app = mock();
    connect_to_mock(&mut app);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::L);
    app.run_steps(2);
    assert_eq!(app.state().mode(), Mode::Layout);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::U);
    app.run_steps(2);
    assert_eq!(app.state().mode(), Mode::Preview);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::B);
    app.run_steps(2);
    assert_eq!(app.state().mode(), Mode::Browse);
}

#[test]
fn find_mode_edits_requests() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Find");
    assert_eq!(app.state().mode(), Mode::Find);
    app.get_by_label("Find Requests");
    assert!(
        is_disabled(&app, "Delete Request"),
        "one request always stays"
    );

    // Criteria go into the layout's fields; operators into the last one used.
    fill(&mut app, "Last Name", "Smith");
    click(&mut app, "Operators");
    click(&mut app, "==   match entire field");
    let field = text_field(&app, "Last Name");
    assert_eq!(field.value().as_deref(), Some("Smith=="));

    // Requests > Revert Request... (#871): Pro's alert, then the request's
    // criteria go, and with nothing typed the item is disabled.
    common::open_menu(&mut app, "Requests");
    click(&mut app, "Revert Request...");
    wait_for_label(&mut app, om_data::scripts::revert_record::REQUEST_MESSAGE);
    click(&mut app, "Revert");
    wait_until(&mut app, "the request reverted", |app| {
        !app.state().script_running()
    });
    assert_eq!(
        text_field(&app, "Last Name").value().unwrap_or_default(),
        ""
    );
    common::open_menu(&mut app, "Requests");
    assert!(is_disabled(&app, "Revert Request..."), "nothing typed");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);

    click(&mut app, "New Request");
    app.get_by_label("2");
    assert!(!is_disabled(&app, "Delete Request"));
    click(&mut app, "Omit");
    assert!(is_on(&app, "Omit"));
    click(&mut app, "Delete Request");
    assert!(is_disabled(&app, "Delete Request"));

    click(&mut app, "Cancel Find");
    assert_eq!(app.state().mode(), Mode::Browse);
}

#[test]
fn perform_find_shows_the_found_set_and_show_all_restores_it() {
    let mut app = mock();
    connect_to_mock(&mut app);
    // No record's last name has a word starting with "Smith": FileMaker's
    // dialog. Modify Find keeps Find mode and the request; Cancel goes back
    // to Browse with the found set as it was.
    click(&mut app, "Find");
    fill(&mut app, "Last Name", "Smith");
    click(&mut app, "Perform Find");
    wait_for_label(&mut app, "No records match this find criteria.");
    click(&mut app, "Modify Find");
    wait_until(&mut app, "script ended", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state().mode(), Mode::Find);
    assert_eq!(
        text_field(&app, "Last Name").value().as_deref(),
        Some("Smith")
    );
    click(&mut app, "Perform Find");
    wait_for_label(&mut app, "No records match this find criteria.");
    click(&mut app, "Cancel");
    wait_until(&mut app, "script ended", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state().mode(), Mode::Browse);
    app.get_by_label("Total (Unsorted)");
    assert!(app.state().error().is_none());

    // "Last1" starts Last1 and Last10-Last19: eleven of the 42 records.
    click(&mut app, "Find");
    fill(&mut app, "Last Name", "Last1");
    click(&mut app, "Perform Find");
    wait_until(&mut app, "found set", |app| {
        !app.state().script_running() && app.query_by_label("11").is_some()
    });
    app.get_by_label("Found (Unsorted)");
    assert_eq!(app.state().mode(), Mode::Browse);
    assert!(!is_disabled(&app, "Show All"));
    // The Find button's menu: Modify Last Find shows the last find's request
    // again, as Records > Modify Last Find does.
    click(&mut app, "Find options");
    click(&mut app, "Modify Last Find");
    wait_until(&mut app, "the last find's request", |app| {
        app.state().mode() == Mode::Find
            && text_field(app, "Last Name").value().as_deref() == Some("Last1")
    });
    click(&mut app, "Perform Find");
    wait_until(&mut app, "the same found set", |app| {
        !app.state().script_running() && app.query_by_label("11").is_some()
    });
    click(&mut app, "Show All");
    wait_until(&mut app, "every record", |app| {
        !app.state().script_running() && app.query_by_label("42").is_some()
    });
    app.get_by_label("Total (Unsorted)");
    assert!(is_disabled(&app, "Show All"));
}

#[test]
fn edit_layout_and_preview_enter_and_exit_their_modes() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Edit Layout");
    assert_eq!(app.state().mode(), Mode::Layout);
    app.get_by_label("Selection");
    app.get_by_label("Layouts");
    app.get_by_label("Arrange");
    click(&mut app, "Field Tools");
    assert!(is_on(&app, "Field Tools"));
    assert!(!is_on(&app, "Selection"));
    click(&mut app, "Exit Layout");
    assert_eq!(app.state().mode(), Mode::Browse);

    click(&mut app, "Preview");
    assert_eq!(app.state().mode(), Mode::Preview);
    app.get_by_label("Pages");
    app.get_by_label("Save as PDF");
    click(&mut app, "Exit Preview");
    assert_eq!(app.state().mode(), Mode::Browse);
}

#[test]
fn the_status_toolbar_can_be_hidden() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_menu(&mut app, "View");
    click(&mut app, "Status Toolbar");
    assert!(app.query_by_label("Edit Layout").is_none());
    assert!(app.query_by_label("Total (Unsorted)").is_none());
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::ALT),
        egui::Key::S,
    );
    app.run_steps(2);
    app.get_by_label("Edit Layout");
}

#[test]
fn pending_menu_commands_are_disabled() {
    let mut app = mock();
    connect_to_mock(&mut app);
    // Unimplemented menu commands are present but disabled.
    open_menu(&mut app, "Records");
    assert!(is_disabled(&app, "Saved Finds"));
}

#[test]
fn sort_opens_the_sort_dialog_and_sorts_the_found_set() {
    let mut app = mock();
    connect_to_mock(&mut app);
    // The sample's Contact Card has a design: the fields it shows are the
    // dialog's "Current Layout".
    common::select_layout(&mut app, "Contact Card");
    wait_for_label(&mut app, "First0 Last0");
    let first_name = |app: &common::App| text_field(app, "First Name").value();
    click(&mut app, "Sort");
    wait_for_label(&mut app, "Sort Records");
    // Cancel leaves the records as they were.
    click(&mut app, "Cancel");
    wait_until(&mut app, "sort canceled", |app| {
        !app.state().script_running()
    });
    app.get_by_label("Total (Unsorted)");
    // Descending by first name puts First9 first.
    click(&mut app, "Sort");
    wait_for_label(&mut app, "Sort Records");
    click(&mut app, "Descending order");
    // The dialog's row, drawn over the layout's own "First Name".
    app.query_all_by_label("First Name").last().unwrap().click();
    app.run_steps(2);
    click(&mut app, "Move");
    wait_for_label(&mut app, "Contacts::First Name");
    // The dialog's Sort, drawn after the toolbar's.
    app.query_all_by_label("Sort").last().unwrap().click();
    wait_until(&mut app, "sorted", |app| {
        !app.state().script_running()
            && app.query_by_label("Total (Sorted)").is_some()
            && first_name(app).as_deref() == Some("First9")
    });
    // The dialog shows that order again, and its Unsort restores creation
    // order.
    click(&mut app, "Sort");
    wait_for_label(&mut app, "Contacts::First Name");
    click(&mut app, "Unsort");
    wait_until(&mut app, "unsorted", |app| {
        !app.state().script_running()
            && app.query_by_label("Total (Unsorted)").is_some()
            && first_name(app).as_deref() == Some("First0")
    });
    open_menu(&mut app, "Records");
    assert!(!is_disabled(&app, "Sort Records..."));
}

/// Table view: Tab and Shift-Tab move across the current record's cells
/// and wrap within its row, never to another record, as FileMaker Pro
/// 26.0.3 does on the Tasks starter (#848's readings; #858). The keys
/// typed after a Tab reach the cell entered, after its text.
#[test]
fn tab_moves_across_the_row_in_table_view() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Table View");
    app.run_steps(4);
    app.query_all(By::new().role(egui::Role::TextInput).label("First Name"))
        .next()
        .unwrap()
        .focus();
    app.run_steps(3);
    let tab = |app: &mut common::App, shift: bool, typed: &str| {
        let modifiers = if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        let key = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        app.event_modifiers(key, modifiers);
        if !typed.is_empty() {
            app.event(egui::Event::Text(typed.to_owned()));
        }
        app.run_steps(4);
        app.query_all(By::new().role(egui::Role::TextInput))
            .find(|field| field.is_focused())
            .and_then(|field| Some((field.accesskit_node().label()?, field.value()?)))
    };
    let at = |label: &str, value: &str| Some((label.to_owned(), value.to_owned()));
    assert_eq!(tab(&mut app, false, "x"), at("Last Name", "Last0x"));
    assert_eq!(tab(&mut app, false, ""), at("Email", "user0@example.com"));
    let created = tab(&mut app, false, "").map(|(label, _)| label);
    assert_eq!(created.as_deref(), Some("Created"));
    // From the last cell, back to the first of the same row.
    assert_eq!(tab(&mut app, false, ""), at("First Name", "First0"));
    // Shift-Tab from the first cell wraps to the row's last.
    let back = tab(&mut app, true, "").map(|(label, _)| label);
    assert_eq!(back.as_deref(), Some("Created"));
    assert_eq!(tab(&mut app, true, ""), at("Email", "user0@example.com"));
    assert_eq!(common::record_number(&app), "1");
    // Find mode's criteria in Table view: the same order, wrapping.
    app.query_all(By::new().role(egui::Role::Button).label("Find"))
        .next()
        .unwrap()
        .click();
    wait_for_label(&mut app, "Perform Find");
    app.query_all(By::new().role(egui::Role::TextInput).label("Created"))
        .next()
        .unwrap()
        .focus();
    app.run_steps(3);
    assert_eq!(tab(&mut app, false, "F"), at("First Name", "F"));
    assert_eq!(tab(&mut app, true, ""), at("Created", ""));
}

#[test]
fn file_close_returns_to_open_network_file() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_menu(&mut app, "File");
    click(&mut app, "Close");
    assert!(!app.state().is_open());
    // The file and account stay; the password is not kept once a file
    // has opened.
    wait_for_label(&mut app, "Open Network File");
    assert_eq!(
        text_field(&app, "Path").value().as_deref(),
        Some("fmnet:/fms.example.com/Contacts.fmp12")
    );
    assert_eq!(
        text_field(&app, "Account Name").value().as_deref(),
        Some("admin")
    );
    let password = app.get_by_role_and_label(egui::Role::PasswordInput, "Password");
    assert_eq!(password.value().as_deref().unwrap_or_default(), "");
}

#[test]
fn open_errors_show_an_alert() {
    let mut app = harness(Box::new(common::FailingProvider));
    fill(&mut app, "Path", "fmnet:/fms.example.com/Contacts.fmp12");
    click(&mut app, "Open");
    let message = "The account name and password you entered could not be authenticated.";
    wait_for_label(&mut app, message);
    assert!(!app.state().is_open());
    assert_eq!(app.state().error(), Some(message));
    // The dialog is usable again once the alert is dismissed.
    click(&mut app, "OK");
    assert!(app.query_by_label(message).is_none());
    assert!(!is_disabled(&app, "Open"));

    // A message taller than the window scrolls, and a click on OK still
    // closes it (#981): it had grown past the window's bottom edge.
    let long = ["A line of a long message, wrapped in the alert."; 60].join("\n");
    let mut app = harness(Box::new(common::FailingWith(long)));
    fill(&mut app, "Path", "fmnet:/fms.example.com/Contacts.fmp12");
    click(&mut app, "Open");
    wait_until(&mut app, "the alert", |app| app.state().error().is_some());
    // The modal takes a few frames to size its scrolled message.
    app.run_steps(6);
    click(&mut app, "OK");
    assert_eq!(app.state().error(), None);
}

/// A host's listing that fails shows on its page, which offers to open a
/// file by its name instead.
#[test]
fn list_files_errors_show_on_their_hosts_page() {
    let settings = tempfile::tempdir().unwrap();
    let path = settings.path().join("settings.json");
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .renderer(common::renderer())
        .build_eframe(move |_| {
            om_gui::OpenMakerApp::new(Box::new(common::FailingProvider))
                .with_settings_file(path)
                .with_home()
        });
    click(&mut app, "Add Host");
    fill(&mut app, "Address", "fms.example.com");
    click(&mut app, "Add");
    wait_for_label(&mut app, "The files could not be listed");
    app.get_by_label("Connection refused (fms.example.com:5003)");
    // A failed listing is the host's: no alert, and it can be asked again.
    assert_eq!(app.state().error(), None);
    assert!(!is_disabled(&app, "List Files"));
    click(&mut app, "Open Network File...");
    common::wait_for_text_field(&mut app, "Path");
    assert_eq!(
        text_field(&app, "Path").value().as_deref(),
        Some("fmnet:/fms.example.com/")
    );
}

#[test]
fn layout_selection_inspects_real_geometry_and_objects_search() {
    let mut app = mock();
    connect_to_mock(&mut app);
    click(&mut app, "Edit Layout");
    // The actual mock layout includes header/footer and 36 pt field spacing;
    // Layout mode must use that geometry, not synthesize a new field list.
    for part in ["Header", "Body", "Footer"] {
        app.get_by_label(part);
    }
    let first = app.get_by_label("Field: First Name (object 5)").rect();
    let second = app.get_by_label("Field: Last Name (object 7)").rect();
    assert!((second.top() - first.top() - 36.0).abs() < 0.01);
    click(&mut app, "Field: First Name (object 5)");
    assert_eq!(
        text_field(&app, "Object Left").value().as_deref(),
        Some("132.00")
    );
    assert_eq!(
        text_field(&app, "Object Top").value().as_deref(),
        Some("92.00")
    );
    assert_eq!(
        text_field(&app, "Object Width").value().as_deref(),
        Some("288.00")
    );
    click(&mut app, "Measurement Units");
    click(&mut app, "in");
    assert_eq!(
        text_field(&app, "Object Width").value().as_deref(),
        Some("4.000")
    );
    click(&mut app, "Data");
    app.get_by_label("First Name");
    app.get_by_label("Type: Text");
    click(&mut app, "Objects");
    app.get_by_label("Layout Objects");
    fill(&mut app, "Search Objects", "email");
    app.get_by_label("1 of 12 objects");
    assert!(app.query_by_label("Field - First Name").is_none());
    click(&mut app, "Field - Email");
    app.get_by_label("Email");
    click(&mut app, "Position");
    assert_eq!(
        text_field(&app, "Object Top").value().as_deref(),
        Some("2.278")
    );
    select_layout(&mut app, "Contacts List");
    wait_for_label(
        &mut app,
        "Object information is unavailable for this layout.",
    );
    app.get_by_label("No object selected");
    assert_eq!(
        text_field(&app, "Object Top")
            .value()
            .as_deref()
            .unwrap_or_default(),
        ""
    );
}

#[test]
fn manage_layouts_search_opens_a_layout_and_blocks_document_shortcuts() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_menu(&mut app, "File");
    click(&mut app, "Manage");
    click(&mut app, "Layouts...");
    wait_for_label(&mut app, "Manage Layouts");
    app.get_by_label("Associated Table");
    // Delete acts on the selected layout, the window's.
    assert!(!is_disabled(&app, "Delete"));
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::L);
    app.run_steps(2);
    assert_eq!(app.state().mode(), Mode::Browse);
    fill(&mut app, "Search Layouts", "missing");
    app.get_by_label("No layouts match the search.");
    assert!(is_disabled(&app, "Open"));
    app.key_press(egui::Key::Enter);
    app.run_steps(2);
    app.get_by_label("Manage Layouts");
    text_field(&app, "Search Layouts").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    text_field(&app, "Search Layouts").type_text("LIST");
    app.run_steps(2);
    // The sample has three layouts; one name contains "list".
    app.get_by_label("1 of 3 layouts");
    click(&mut app, "Open");
    wait_for_value(&mut app, "First0");
    assert!(app.query_by_label("Manage Layouts").is_none());
    let layout = app.get_by_role_and_label(egui::Role::ComboBox, "Layout");
    assert_eq!(layout.value().as_deref(), Some("Contacts List"));
    click(&mut app, "Edit Layout");
    open_menu(&mut app, "Layouts");
    click(&mut app, "Manage Layouts...");
    wait_for_label(&mut app, "Manage Layouts");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    assert!(app.query_by_label("Manage Layouts").is_none());
    assert_eq!(app.state().mode(), Mode::Layout);
}

#[test]
fn objects_pane_reveals_an_offscreen_layout_object() {
    let mut app = harness(Box::new(common::LargeLayoutProvider(MockProvider::new())));
    connect_to_mock(&mut app);
    click(&mut app, "Edit Layout");
    click(&mut app, "Objects");
    for _ in 0..2 {
        open_menu(&mut app, "View");
        click(&mut app, "Zoom In");
    }
    click(&mut app, "Corner 1");
    app.run_steps(4);
    let rect = app.get_by_label("Corner (object 1)").rect();
    assert_eq!(rect.size(), egui::vec2(180.0, 60.0));
    assert!(
        app.ctx.content_rect().contains_rect(rect),
        "selected object is revealed: {rect:?}"
    );
    assert!(
        rect.left() >= 220.0 && rect.right() <= 808.0,
        "selection stays between panels: {rect:?}"
    );
    assert_eq!(
        text_field(&app, "Object Left").value().as_deref(),
        Some("1900.00")
    );
    assert_eq!(
        text_field(&app, "Object Height").value().as_deref(),
        Some("30.00")
    );
}

/// Records > Omit Multiple asks how many records to omit, from the current
/// one: three of the 42 leave the found set; Cancel omits none.
#[test]
fn omit_multiple_asks_how_many_records_to_omit() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_menu(&mut app, "Records");
    click(&mut app, "Omit Multiple...");
    wait_for_label(
        &mut app,
        "Omit how many records, starting with the current one?",
    );
    click(&mut app, "Cancel");
    wait_until(&mut app, "the command to end", |app| {
        !app.state().script_running()
    });
    app.get_by_label("Total (Unsorted)");
    open_menu(&mut app, "Records");
    click(&mut app, "Omit Multiple...");
    wait_for_label(
        &mut app,
        "Omit how many records, starting with the current one?",
    );
    let field = text_field(&app, "Records to omit");
    assert_eq!(field.value().as_deref(), Some("1"));
    field.focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("3".into()));
    app.run_steps(2);
    click(&mut app, "Omit");
    wait_until(&mut app, "39 found", |app| {
        !app.state().script_running() && app.query_by_label("39").is_some()
    });
    app.get_by_label("Found (Unsorted)");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}
