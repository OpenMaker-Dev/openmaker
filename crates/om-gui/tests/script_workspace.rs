//! The Script Workspace through its window (captures 35, 35b): the panes,
//! each editing operation, the scripts pane's catalog changes, saving,
//! the unsaved-changes prompts, the step options and Specify Calculation
//! (42). The provider stores what the workspace saves, so each test checks
//! the saved steps. The last test saves to an OpenMaker Server on loopback
//! hosting the synthetic Tasks file of `om-server/tests/fixtures/tasks.rs`
//! through the native provider, and reads it back over a new connection.

mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::sync::{Arc, Mutex};

use common::workspace::{contacts, double_click_at, step_text_position, Provider, State};
use egui::Modifiers;
use egui_kittest::kittest::{By, NodeT, Queryable};
use om_data::scripts::display::{calculation_text, NoStepNames};
use om_data::scripts::specs::SortSpec;
use om_data::scripts::StepData;

fn open_with(read_only: bool) -> (common::App, Arc<Mutex<State>>) {
    open_catalog(read_only, contacts())
}

fn open_catalog(
    read_only: bool,
    scripts: om_data::ScriptCatalog,
) -> (common::App, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        scripts,
        read_only,
        ..State::default()
    }));
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/fms.example.invalid/Contacts.fmp12",
    );
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file to open", |app| {
        app.query_by_label("Scripts").is_some()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(
        &mut app,
        "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
    );
    (app, state)
}

fn open() -> (common::App, Arc<Mutex<State>>) {
    open_with(false)
}

/// Let the provider's answers arrive (the workspace is disabled while a
/// save is in flight).
fn settle(app: &mut common::App) {
    for _ in 0..20 {
        app.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A double click on the node labelled `label`.
fn double_click(app: &mut common::App, label: &str) {
    let pos = app.get_by_label(label).rect().center();
    double_click_at(app, pos);
}

fn press(app: &mut common::App, modifiers: Modifiers, key: egui::Key) {
    app.key_press_modifiers(modifiers, key);
    app.run_steps(2);
}

/// Save the active script (⌘S) and wait for the provider.
fn save(app: &mut common::App, state: &Arc<Mutex<State>>) -> Vec<u16> {
    let before = state.lock().unwrap().events.len();
    press(app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(app, "the script to be saved", |_| {
        state.lock().unwrap().events.len() > before
    });
    let state = state.lock().unwrap();
    state.scripts.scripts[1]
        .steps
        .iter()
        .map(|s| s.opcode.unsigned_abs())
        .collect()
}

#[test]
fn the_workspace_shows_the_catalog_and_filemakers_step_text() {
    let (app, _) = open();
    // The window and its title bar both carry the title.
    assert!(app
        .query_all_by_label("Script Workspace (Contacts (fms.example.invalid))")
        .next()
        .is_some());
    for label in [
        "Contacts",
        "Go to Entry Field",
        "Sort by Name",
        "Line 2: Select All",
    ] {
        assert!(app.query_by_label(label).is_some(), "{label}");
    }
    for category in ["Favorites", "Control", "Open Menu Item", "Miscellaneous"] {
        assert!(app.query_by_label(category).is_some(), "{category}");
    }
    // The debugger is later work: its button is there, disabled.
    assert!(app
        .get_by_label("Debug Script")
        .accesskit_node()
        .is_disabled());
}

#[test]
fn steps_pane_inserts_after_the_selection_with_its_block() {
    let (mut app, state) = open();
    common::click(
        &mut app,
        "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
    );
    common::click(&mut app, "Control");
    double_click(&mut app, "If");
    common::wait_for_label(&mut app, "Line 3: End If");
    assert!(app.query_by_label("Line 4: Select All").is_some());
    assert_eq!(save(&mut app, &state), [145, 68, 70, 50]);
}

#[test]
fn selection_moves_duplicates_deletes_and_undoes() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::COMMAND, egui::Key::ArrowUp);
    common::wait_for_label(&mut app, "Line 1: Select All");
    press(&mut app, Modifiers::COMMAND, egui::Key::D);
    common::wait_for_label(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::NONE, egui::Key::Delete);
    press(&mut app, Modifiers::COMMAND, egui::Key::Z);
    common::wait_for_label(
        &mut app,
        "Line 3: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
    );
    press(
        &mut app,
        Modifiers::COMMAND.plus(Modifiers::SHIFT),
        egui::Key::Z,
    );
    assert_eq!(save(&mut app, &state), [50, 145]);
    // Shift-click selects a range; Delete removes it.
    common::click(&mut app, "Line 1: Select All");
    app.get_by_label("Line 2: Go to Object [ Object Name: Get ( ScriptParameter ) ]")
        .click_modifiers(Modifiers::SHIFT);
    app.run_steps(2);
    press(&mut app, Modifiers::NONE, egui::Key::Backspace);
    assert_eq!(save(&mut app, &state), Vec::<u16>::new());
}

#[test]
fn unbalancing_edits_are_refused_and_explained() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    common::click(&mut app, "Control");
    double_click(&mut app, "Loop");
    common::wait_for_label(&mut app, "Line 4: End Loop");
    // End Loop cannot move above its Loop.
    common::click(&mut app, "Line 4: End Loop");
    press(&mut app, Modifiers::COMMAND, egui::Key::ArrowUp);
    press(&mut app, Modifiers::COMMAND, egui::Key::ArrowUp);
    common::wait_until(&mut app, "the refusal", |app| {
        app.query_by_label_contains("has no matching").is_some()
    });
    assert!(
        app.query_by_label("Line 3: Loop [ Flush: Always ]")
            .is_some()
            || app.query_by_label("Line 4: End Loop").is_some()
    );
    let saved = save(&mut app, &state);
    assert_eq!(
        saved.iter().filter(|id| **id == 71 || **id == 73).count(),
        2
    );
}

#[test]
fn disabling_and_comments_edit_in_place() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::COMMAND, egui::Key::Slash);
    common::wait_for_label(&mut app, "Line 2: // Select All");
    app.get_by_label("Line 2: // Select All").click_secondary();
    app.run_steps(2);
    common::click(&mut app, "Add Comment");
    common::wait_until(&mut app, "the comment editor", |app| {
        common::find_text_field(app, "Edit Option").is_some()
    });
    common::text_field(&app, "Edit Option").type_text("Select the field's text");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_for_label(&mut app, "Line 3: # Select the field's text");
    let saved = save(&mut app, &state);
    assert_eq!(saved, [145, 50, 89]);
    let state = state.lock().unwrap();
    assert!(!state.scripts.scripts[1].steps[1].is_enabled());
}

#[test]
fn typing_a_step_name_inserts_the_match() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the new step line", |app| {
        common::find_text_field(app, "New Step").is_some()
    });
    common::text_field(&app, "New Step").type_text("beep");
    app.run_steps(2);
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_for_label(&mut app, "Line 3: Beep");
    assert_eq!(save(&mut app, &state), [145, 50, 93]);
}

/// Send the system's copy event and return the text the workspace put on
/// the clipboard.
fn copy(app: &mut common::App) -> String {
    app.event(egui::Event::Copy);
    app.step();
    let text = app
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .expect("copied text");
    app.run_steps(2);
    text
}

fn paste(app: &mut common::App, text: &str) {
    app.event(egui::Event::Paste(text.into()));
    app.run_steps(3);
}

#[test]
fn steps_copy_as_text_and_paste_back() {
    let (mut app, state) = open();
    common::click(
        &mut app,
        "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
    );
    let text = copy(&mut app);
    assert_eq!(
        text,
        "Go to Object [ Object Name: Get ( ScriptParameter ) ]\n"
    );
    common::click(&mut app, "Line 2: Select All");
    // Other text on the system clipboard is read as text, never taken as
    // the stale copy.
    paste(&mut app, "unrelated clipboard text");
    common::wait_for_label(
        &mut app,
        "Line 3: // # unrelated clipboard text (could not be read)",
    );
    assert!(app
        .query_by_label("Line 3: Go to Object [ Object Name: Get ( ScriptParameter ) ]")
        .is_none());
    press(&mut app, Modifiers::COMMAND, egui::Key::Z);
    common::wait_until(&mut app, "the paste undone", |app| {
        app.query_by_label("Line 3: // # unrelated clipboard text (could not be read)")
            .is_none()
    });
    common::click(&mut app, "Line 2: Select All");
    paste(&mut app, &text);
    common::wait_for_label(
        &mut app,
        "Line 3: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
    );
    assert_eq!(save(&mut app, &state), [145, 50, 145]);
    // The copy is a new step: its own data key.
    let steps = state.lock().unwrap().scripts.scripts[1].steps.clone();
    assert_ne!(steps[0].data_key, steps[2].data_key);
    assert_eq!(steps[0].calculation(0), steps[2].calculation(0));
}

#[test]
fn pasted_blocks_read_with_their_indentation_and_one_undo() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    paste(
        &mut app,
        "If [ Get ( FoundCount ) = 0 ]\n\tShow Custom Dialog [ Title: \"Nothing\" ]\n\tGo to Field [ Contacts::Company ]\nEnd If\n",
    );
    common::wait_for_label(&mut app, "Line 6: End If");
    assert!(app
        .query_by_label("Line 5: Go to Field [ Contacts::Company ]")
        .is_some());
    press(&mut app, Modifiers::COMMAND, egui::Key::Z);
    common::wait_until(&mut app, "the paste undone", |app| {
        app.query_by_label("Line 3: If [ Get ( FoundCount ) = 0 ]")
            .is_none()
    });
    press(
        &mut app,
        Modifiers::COMMAND.plus(Modifiers::SHIFT),
        egui::Key::Z,
    );
    assert_eq!(save(&mut app, &state), [145, 50, 68, 87, 17, 70]);
}

/// One `<Step>` of FileMaker's XML from another file.
fn xml_step(id: u16, options: u32, parameter: &str) -> String {
    format!(
        r#"<Step enable="True" id="{id}"><Options>{options}</Options><ParameterValues membercount="1"><Parameter type="{}">{parameter}</Parameter></ParameterValues></Step>"#,
        &parameter[1..parameter.find([' ', '>']).unwrap()]
    )
}

#[test]
fn pasted_xml_from_another_file_resolves_by_name_never_by_colliding_ids() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    // File A's ids differ from this file's for the same names (Company is
    // 5 here, Contact List 4, Sort by Name 3), and A's ids for the names
    // this file lacks are this file's ids of other objects.
    let field = |id: u32, name: &str| {
        xml_step(
            17,
            1,
            &format!(
                r#"<FieldReference id="{id}" name="{name}"><TableOccurrenceReference id="1065089" name="Contacts"></TableOccurrenceReference></FieldReference>"#
            ),
        )
    };
    let layout = |id: u32, name: &str| {
        xml_step(
            6,
            8,
            &format!(
                r#"<LayoutReferenceContainer value="5"><LayoutReference id="{id}" name="{name}"></LayoutReference></LayoutReferenceContainer>"#
            ),
        )
    };
    let script = |id: u32, name: &str| {
        let list = format!(
            r#"<List name="From list" value="1"><ScriptReference id="{id}" name="{name}"></ScriptReference></List>"#
        );
        format!(
            r#"<Step enable="True" id="1"><Options>64</Options><ParameterValues membercount="1"><Parameter type="List">{list}</Parameter></ParameterValues></Step>"#
        )
    };
    let snippet = format!(
        "<fmxmlsnippet type=\"FMObjectList\">{}{}{}{}{}{}</fmxmlsnippet>",
        field(77, "Company"),
        field(5, "Nickname"),
        layout(9, "Contact List"),
        layout(4, "Archive View"),
        script(12, "Sort by Name"),
        script(3, "Cleanup"),
    );
    paste(&mut app, &snippet);
    common::wait_for_label(
        &mut app,
        "3 pasted lines could not be read and were added as disabled comments.",
    );
    assert!(app
        .query_by_label("Line 3: Go to Field [ Contacts::Company ]")
        .is_some());
    assert!(app
        .query_by_label("Line 5: Go to Layout [ “Contact List” (Contacts) ]")
        .is_some());
    for line in [4, 6, 8] {
        assert!(
            app.query_all_by_role(egui::Role::ListItem)
                .filter_map(|n| n.accesskit_node().label())
                .any(|l| l.starts_with(&format!("Line {line}: // # <Step"))
                    && l.ends_with("(could not be read)")),
            "line {line}"
        );
    }
    let saved = save(&mut app, &state);
    assert_eq!(saved, [145, 50, 17, 89, 6, 89, 1, 89]);
    let steps = state.lock().unwrap().scripts.scripts[1].steps.clone();
    assert_eq!(steps[2].field_spec().unwrap().field().id(), Some(5));
    assert_eq!(steps[4].member_key().and_then(|k| k.id()), Some(4));
    assert_eq!(steps[6].script_id(), Some(3));
    for i in [3, 5, 7] {
        assert!(!steps[i].is_enabled(), "{i}");
    }
}

#[test]
fn a_paste_past_the_limits_is_refused_whole() {
    let (mut app, _state) = open();
    common::click(&mut app, "Line 2: Select All");
    let steps = om_data::scripts::display::MAX_PASTE_STEPS;
    paste(&mut app, &"Beep\n".repeat(steps + 1));
    common::wait_for_label(
        &mut app,
        &format!(
            "Nothing was pasted. The clipboard holds {} steps; a paste inserts at most {steps}.",
            steps + 1
        ),
    );
    assert!(app.query_by_label("Line 3: Beep").is_none());
}

#[test]
fn unreadable_pasted_lines_become_marked_disabled_comments() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    paste(
        &mut app,
        "Beep\nThis is not a step\nSet Field [ Contacts::Nickname ; 1 ]\n",
    );
    common::wait_for_label(
        &mut app,
        "2 pasted lines could not be read and were added as disabled comments.",
    );
    assert!(app.query_by_label("Line 3: Beep").is_some());
    assert!(app
        .query_by_label("Line 4: // # This is not a step (could not be read)")
        .is_some());
    assert!(app
        .query_by_label("Line 5: // # Set Field [ Contacts::Nickname ; 1 ] (could not be read)")
        .is_some());
    let saved = save(&mut app, &state);
    assert_eq!(saved, [145, 50, 93, 89, 89]);
    let steps = state.lock().unwrap().scripts.scripts[1].steps.clone();
    assert!(!steps[3].is_enabled());
    assert_eq!(
        steps[4].text(0).as_deref(),
        Some("Set Field [ Contacts::Nickname ; 1 ]")
    );
}

#[test]
fn a_whole_script_copies_from_the_scripts_pane() {
    let (mut app, _) = open();
    common::click(&mut app, "Go to Entry Field");
    let text = copy(&mut app);
    assert_eq!(
        text,
        "Go to Object [ Object Name: Get ( ScriptParameter ) ]\nSelect All\n"
    );
    // And from its context menu.
    app.get_by_label("Sort by Name").click_secondary();
    common::wait_until(&mut app, "the context menu", |app| {
        app.query_by_label("Copy as Text").is_some()
    });
    app.get_by_label("Copy as Text").click();
    app.step();
    let copied =
        app.output().platform_output.commands.iter().any(
            |c| matches!(c, egui::OutputCommand::CopyText(t) if t.starts_with("Sort Records")),
        );
    assert!(copied);
}

#[test]
fn the_scripts_pane_creates_renames_and_deletes_at_once() {
    let (mut app, state) = open();
    common::click(&mut app, "Sort by Name");
    // The toolbar's + and the scripts pane's header both make a script.
    app.query_all_by_label("New Script").next().unwrap().click();
    app.run_steps(2);
    common::wait_until(&mut app, "the new script's name field", |app| {
        common::find_text_field(app, "Script Name").is_some()
    });
    common::text_field(&app, "Script Name").focus();
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Script Name").type_text("Find Duplicates");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the renamed script", |_| {
        state
            .lock()
            .unwrap()
            .scripts
            .scripts
            .iter()
            .any(|s| s.name == "Find Duplicates")
    });
    let names: Vec<String> = state
        .lock()
        .unwrap()
        .scripts
        .scripts
        .iter()
        .map(|s| s.name.clone())
        .collect();
    // In the folder, after Sort by Name.
    assert_eq!(
        names,
        [
            "Contacts",
            "Go to Entry Field",
            "Sort by Name",
            "Find Duplicates",
            "--"
        ]
    );
    common::wait_for_label(&mut app, "Find Duplicates");
    settle(&mut app);
    app.get_by_label("Find Duplicates").click_secondary();
    common::wait_until(&mut app, "the context menu", |app| {
        app.query_by_label("Delete").is_some()
    });
    common::click(&mut app, "Delete");
    common::wait_for_label(
        &mut app,
        "Are you sure you want to permanently delete “Find Duplicates”?",
    );
    app.get_by_role_and_label(egui::Role::Button, "Delete")
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "the deleted script", |_| {
        state.lock().unwrap().scripts.scripts.len() == 4
    });
}

#[test]
fn closing_with_unsaved_changes_asks_save_dont_save_cancel() {
    let (mut app, state) = open();
    common::click(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::COMMAND, egui::Key::D);
    common::wait_for_label(&mut app, "Tab Go to Entry Field *");
    common::click(&mut app, "Close Window");
    common::wait_for_label(&mut app, "Save changes to script “Go to Entry Field”?");
    common::click(&mut app, "Cancel");
    assert!(app.query_by_label("Line 3: Select All").is_some());
    common::click(&mut app, "Close Window");
    common::wait_for_label(&mut app, "Save changes to script “Go to Entry Field”?");
    common::click(&mut app, "Don't Save");
    common::wait_until(&mut app, "the workspace to close", |app| {
        app.query_by_label("Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]")
            .is_none()
    });
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn saving_where_the_host_cannot_store_scripts_alerts_and_keeps_the_draft() {
    let (mut app, state) = open_with(true);
    common::click(&mut app, "Line 2: Select All");
    press(&mut app, Modifiers::COMMAND, egui::Key::D);
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "the save alert", |app| {
        app.query_by_label_contains("could not be saved").is_some()
    });
    assert!(app.query_by_label_contains("not available yet").is_some());
    common::click(&mut app, "OK");
    assert!(app.query_by_label("Tab Go to Entry Field *").is_some());
    assert_eq!(state.lock().unwrap().scripts.scripts[1].steps.len(), 2);
}

/// Open the options of the step on `line` with its gear (35b).
fn open_options(app: &mut common::App, line: &str, title: &str) {
    common::click(app, line);
    common::click(app, "Step Options");
    common::wait_for_label(app, title);
}

/// Insert a step after line 2 by typing its name.
fn type_step(app: &mut common::App, name: &str) {
    common::click(app, "Line 2: Select All");
    press(app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(app, "the new step line", |app| {
        common::find_text_field(app, "New Step").is_some()
    });
    common::text_field(app, "New Step").type_text(name);
    app.run_steps(2);
    press(app, Modifiers::NONE, egui::Key::Enter);
}

#[test]
fn step_options_edit_a_calculation_that_is_saved() {
    let (mut app, state) = open();
    open_options(
        &mut app,
        "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
        "\"Go to Object\" Options",
    );
    assert!(app
        .query_by_label_contains("Object names are assigned using the Inspector")
        .is_some());
    assert_eq!(
        common::text_field(&app, "Object Name").value().as_deref(),
        Some("Get ( ScriptParameter )")
    );
    assert_eq!(
        common::text_field(&app, "Repetition").value().as_deref(),
        Some("1")
    );
    common::text_field(&app, "Object Name").focus();
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Object Name").type_text("\"First Name\"");
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        "Line 1: Go to Object [ Object Name: \"First Name\" ]",
    );
    assert_eq!(save(&mut app, &state), [145, 50]);
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[1].steps[0];
    let calc = step.calculation(0).unwrap();
    assert_eq!(calculation_text(&calc, &NoStepNames), "\"First Name\"");
    // The repetition stays as it was: nothing stored.
    assert!(step.calculation(1).is_none());
}

#[test]
fn a_calculation_that_does_not_compile_alerts_and_keeps_the_options_open() {
    for submit_with_return in [false, true] {
        let (mut app, _) = open();
        open_options(
            &mut app,
            "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]",
            "\"Go to Object\" Options",
        );
        common::text_field(&app, "Object Name").focus();
        app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
        common::text_field(&app, "Object Name").type_text("1 +");
        app.run_steps(2);
        if submit_with_return {
            press(&mut app, Modifiers::NONE, egui::Key::Enter);
        } else {
            common::click(&mut app, "OK");
        }
        common::wait_until(&mut app, "the calculation alert", |app| {
            app.query_all_by_label("OK").count() == 2
        });
        // The alert's OK is on top: answering it leaves the options open.
        app.key_press(egui::Key::Enter);
        app.run_steps(3);
        assert!(app.query_by_label("\"Go to Object\" Options").is_some());
        assert_eq!(
            common::text_field(&app, "Object Name").value().as_deref(),
            Some("1 +")
        );
    }
}

#[test]
fn specify_field_picks_set_fields_target() {
    let (mut app, state) = open();
    type_step(&mut app, "Set Field");
    common::wait_for_label(&mut app, "Line 3: Set Field [ ]");
    open_options(&mut app, "Line 3: Set Field [ ]", "\"Set Field\" Options");
    common::click(&mut app, "Specify Field…");
    common::wait_for_label(&mut app, "Specify Field");
    assert_eq!(
        app.get_by_role_and_label(egui::Role::ComboBox, "Table")
            .value()
            .as_deref(),
        Some("Contacts")
    );
    // A double click on a field answers the picker.
    double_click(&mut app, "Last Name");
    common::wait_until(&mut app, "the picker to close", |app| {
        app.query_by_label("Specify Field").is_none()
    });
    assert_eq!(
        common::find_text_field(&app, "Field")
            .and_then(|f| f.value())
            .as_deref(),
        Some("Contacts::Last Name")
    );
    common::text_field(&app, "Value").focus();
    common::text_field(&app, "Value").type_text("\"Lovelace\"");
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        "Line 3: Set Field [ Contacts::Last Name ; \"Lovelace\" ]",
    );
    assert_eq!(save(&mut app, &state), [145, 50, 76]);
    let state = state.lock().unwrap();
    let spec = state.scripts.scripts[1].steps[2].field_spec().unwrap();
    assert_eq!(
        (spec.table().id(), spec.field().id(), spec.repetition()),
        (Some(0xd00002), Some(2), 1)
    );
}

#[test]
fn the_sort_dialog_stores_sort_records_order() {
    let (mut app, state) = open();
    common::click(&mut app, "Sort by Name");
    let line = "Line 1: Sort Records [ With dialog: On ]";
    common::wait_for_label(&mut app, line);
    open_options(&mut app, line, "\"Sort Records\" Options");
    common::click(&mut app, "Specify Sort Order…");
    common::wait_for_label(&mut app, "Sort Records");
    common::click(&mut app, "Last Name");
    common::click(&mut app, "Move");
    common::wait_for_label(&mut app, "Contacts::Last Name");
    common::click(&mut app, "Descending order");
    common::click(&mut app, "First Name");
    common::click(&mut app, "Move");
    common::wait_for_label(&mut app, "Contacts::First Name");
    // Return answers the dialog on top: the sort order's OK.
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the sort dialog to close", |app| {
        app.query_by_label("Sort Order").is_none()
    });
    assert_eq!(
        app.get_by_role_and_label(egui::Role::CheckBox, "Specify sort order")
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::True)
    );
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        "Line 1: Sort Records [ Restore ; With dialog: On ]",
    );
    let before = state.lock().unwrap().events.len();
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "the script to be saved", |_| {
        state.lock().unwrap().events.len() > before
    });
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[2].steps[0];
    let node = step.data_leaf(StepData::SORT_ORDER).unwrap();
    let spec = SortSpec::from_node(node).unwrap();
    let fields: Vec<(u64, bool)> = spec
        .fields
        .iter()
        .map(|e| (e.field.field, e.descending))
        .collect();
    assert_eq!(fields, [(2, true), (1, false)]);
    assert!(spec.keep_sorted);
}

#[test]
fn export_records_order_is_specified_and_stored() {
    let (mut app, state) = open();
    type_step(&mut app, "Export Records");
    let line = app
        .query_by_label_contains("Line 3: Export Records")
        .and_then(|n| n.accesskit_node().label())
        .expect("the new step's line");
    open_options(&mut app, &line, "\"Export Records\" Options");
    common::click(&mut app, "Specify Export Order…");
    common::wait_for_label(&mut app, "Specify Field Order for Export");
    common::click(&mut app, "Last Name");
    common::click(&mut app, "Move");
    common::wait_for_label(&mut app, "Contacts::Last Name");
    common::click(&mut app, "First Name");
    common::click(&mut app, "Move");
    common::wait_for_label(&mut app, "Contacts::First Name");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the export order dialog to close", |app| {
        app.query_by_label("Field export order").is_none()
    });
    assert_eq!(
        app.get_by_role_and_label(egui::Role::CheckBox, "Specify export order")
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::True)
    );
    // The output file's type, a row of the step's options.
    app.get_by_role_and_label(egui::Role::ComboBox, "Type")
        .click();
    common::wait_for_label(&mut app, "Comma-Separated Values");
    common::click(&mut app, "Comma-Separated Values");
    // A type without an output file is not stored.
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the refusal", |app| {
        app.query(By::new().label_contains("stored with the output file"))
            .is_some()
    });
    app.query_all_by_label("OK").last().unwrap().click();
    app.run_steps(2);
    app.get_by_role_and_label(egui::Role::Button, "Specify File Paths…")
        .click();
    common::wait_for_label(&mut app, "Specify Output File");
    app.get_by_label("File Path List").focus();
    app.get_by_label("File Path List")
        .type_text("file:Contacts.csv");
    app.run_steps(2);
    app.query_all_by_label("OK").last().unwrap().click();
    common::wait_until(&mut app, "the file dialog to close", |app| {
        app.query_by_label("Specify Output File").is_none()
    });
    common::click(&mut app, "OK");
    let before = state.lock().unwrap().events.len();
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "the script to be saved", |_| {
        state.lock().unwrap().events.len() > before
    });
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[1].steps[2];
    assert!(step.has_flag(om_data::scripts::flag::RESTORE));
    let profile = om_data::scripts::node_bytes(step.data_leaf(StepData::PROFILE).unwrap())
        .unwrap()
        .into_owned();
    assert_eq!(
        om_transfer::Profile::decode(&profile).unwrap().file_type,
        om_transfer::FileType::CommaSeparated
    );
    assert!(step.has_flag(om_data::scripts::flag::FILE_PATH));
    assert!(step.has_flag(om_data::scripts::flag::EXTENDED_DATA));
    let bytes = om_data::scripts::node_bytes(step.data_leaf(StepData::EXPORT_ORDER).unwrap())
        .unwrap()
        .into_owned();
    let order = om_transfer::ExportOptions::decode(&bytes).unwrap();
    let fields: Vec<Option<(u64, u64)>> = order.fields.iter().map(|f| f.field.ids()).collect();
    assert_eq!(fields, [Some((0xd00002, 2)), Some((0xd00002, 1))]);
}

#[test]
fn import_records_source_and_order_are_specified_and_stored() {
    let dir = std::env::temp_dir().join(format!("om-import-step-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("people.tab");
    std::fs::write(&source, "Given\tFamily\rAda\tLovelace\r").unwrap();
    let (mut app, state) = open();
    type_step(&mut app, "Import Records");
    let line = app
        .query_by_label_contains("Line 3: Import Records")
        .and_then(|n| n.accesskit_node().label())
        .expect("the new step's line");
    open_options(&mut app, &line, "\"Import Records\" Options");
    // The data source first: the import order maps its fields.
    app.get_by_role_and_label(egui::Role::Button, "Specify Data Source…")
        .click();
    common::wait_for_label(&mut app, "Specify File");
    app.get_by_label("File Path List").focus();
    app.get_by_label("File Path List")
        .type_text(&common::file_line(&source));
    app.run_steps(2);
    // The file dialog's OK, above the options'.
    app.query_all_by_label("OK").last().unwrap().click();
    app.run_steps(2);
    common::wait_until(&mut app, "the file dialog to close", |app| {
        app.query_by_label("Specify File").is_none()
    });
    common::click(&mut app, "Specify Import Order…");
    common::wait_for_label(&mut app, "Import Field Mapping");
    common::wait_for_label(&mut app, "Family");
    for (column, field) in [(1, "First Name"), (2, "Last Name")] {
        app.get_by_role_and_label(
            egui::Role::ComboBox,
            &format!("Target field for source column {column}"),
        )
        .click();
        common::wait_for_label(&mut app, field);
        common::click(&mut app, field);
    }
    common::click(&mut app, "Don't import first record (contains field names)");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the mapping dialog to close", |app| {
        app.query_by_label("Import Field Mapping").is_none()
    });
    common::click(&mut app, "OK");
    let before = state.lock().unwrap().events.len();
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "the script to be saved", |_| {
        state.lock().unwrap().events.len() > before
    });
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[1].steps[2];
    // A file source: value 1 with kMenuSelection, its path list.
    assert_eq!(step.value, 1);
    assert!(step.has_flag(om_data::scripts::flag::MENU_SELECTION));
    assert!(step.has_flag(om_data::scripts::flag::RESTORE));
    assert_eq!(
        step.path_list().as_deref(),
        Some(common::file_line(&source).as_str())
    );
    let leaf = |key: u8| {
        om_data::scripts::node_bytes(step.data_leaf(key).unwrap())
            .unwrap()
            .into_owned()
    };
    let order = om_transfer::ImportOptions::decode(&leaf(StepData::IMPORT_ORDER)).unwrap();
    assert_eq!(
        order
            .map
            .iter()
            .map(|e| (e.field.id(), e.mapping))
            .collect::<Vec<_>>(),
        [
            (Some(1), om_transfer::Mapping::Import),
            (Some(2), om_transfer::Mapping::Import)
        ]
    );
    assert_eq!(order.table.id(), Some(0xd00002));
    let ex = om_transfer::ProfileEx::decode(&leaf(0x87)).unwrap();
    assert_eq!(ex.field_names_row, 0);
    let profile = om_transfer::Profile::decode(&leaf(StepData::PROFILE)).unwrap();
    assert_eq!(profile.file_type, om_transfer::FileType::TabSeparated);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn find_requests_are_specified_and_stored() {
    let (mut app, state) = open();
    type_step(&mut app, "Perform Find");
    let line = "Line 3: Perform Find [ ]";
    common::wait_for_label(&mut app, line);
    open_options(&mut app, line, "\"Perform Find\" Options");
    common::click(&mut app, "Specify Find Requests…");
    common::wait_for_label(&mut app, "Specify Find Requests");
    common::click(&mut app, "New");
    common::wait_for_label(&mut app, "Edit Find Request");
    common::click(&mut app, "Last Name");
    common::text_field(&app, "Criteria").focus();
    common::text_field(&app, "Criteria").type_text("Lovelace");
    app.run_steps(2);
    common::click(&mut app, "Add");
    common::wait_for_label(&mut app, "Contacts::Last Name Lovelace");
    app.get_by_role_and_label(egui::Role::ComboBox, "Action")
        .click();
    common::wait_for_label(&mut app, "Omit Records");
    common::click(&mut app, "Omit Records");
    // Return answers the dialog on top each time.
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_for_label(&mut app, "Omit Records Contacts::Last Name: [Lovelace]");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the requests dialog to close", |app| {
        app.query_by_label("Specify Find Requests").is_none()
    });
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "Line 3: Perform Find [ Restore ]");
    assert_eq!(save(&mut app, &state), [145, 50, 28]);
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[1].steps[2];
    let requests =
        om_data::scripts::specs::requests(step.data_leaf(StepData::REQUESTS).unwrap()).unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].omit);
    assert_eq!(requests[0].criteria[0].0.field, 2);
    assert_eq!(requests[0].criteria[0].1, "Lovelace");
}

const FIRST: &str = "Line 1: Go to Object [ Object Name: Get ( ScriptParameter ) ]";

/// Specify Calculation for line 1's Object Name: a double click on the
/// calculation in the step's brackets.
fn specify_calculation(app: &mut common::App) {
    let at = step_text_position(app, FIRST, "Go to Object [ Object Name: ");
    double_click_at(app, at);
    common::wait_for_label(app, "Specify Calculation");
}

/// The formula editor (a multiline text field).
fn formula<'a>(app: &'a common::App) -> egui_kittest::Node<'a> {
    app.get_by_role_and_label(egui::Role::MultilineTextInput, "Calculation")
}

fn editor_text(app: &common::App) -> String {
    formula(app).value().unwrap_or_default()
}

#[test]
fn specify_calculation_builds_a_formula_from_its_lists_and_typing() {
    let (mut app, state) = open();
    specify_calculation(&mut app);
    assert_eq!(editor_text(&app), "Get ( ScriptParameter )");
    assert!(app.query_by_label("L: 1 C: 1").is_some());
    // Replace the formula: a function from the list with its parameter
    // selected, a field from the fields pane over it, then typing.
    formula(&app).focus();
    app.run_steps(2);
    press(&mut app, Modifiers::COMMAND, egui::Key::A);
    double_click(&mut app, "Abs ( number )");
    assert_eq!(editor_text(&app), "Abs ( number )");
    assert!(app.query_by_label("L: 1 C: 7").is_some());
    double_click(&mut app, "First Name");
    assert_eq!(editor_text(&app), "Abs ( Contacts::First Name )");
    formula(&app).type_text(" + 1");
    app.run_steps(2);
    common::click(&mut app, "-");
    formula(&app).type_text("2");
    app.run_steps(2);
    let formula = "Abs ( Contacts::First Name + 1 - 2 )";
    assert_eq!(editor_text(&app), formula);
    common::click(&mut app, "OK");
    common::wait_for_label(
        &mut app,
        &format!("Line 1: Go to Object [ Object Name: {formula} ]"),
    );
    assert_eq!(save(&mut app, &state), [145, 50]);
    let schema = common::workspace::schema();
    let compiled =
        om_data::formula::compile(formula, &om_data::formula::Context::new(schema.names()))
            .unwrap();
    let expected = om_data::Calculation::new(
        Some(om_data::formula::formatted::encode(&compiled.tokens)),
        om_data::formula::polish::encode(&compiled.expr),
    );
    let state = state.lock().unwrap();
    assert_eq!(
        state.scripts.scripts[1].steps[0].calculation(0),
        Some(expected)
    );
}

#[test]
fn an_invalid_formula_shows_filemakers_error_and_keeps_the_dialog() {
    let (mut app, state) = open();
    specify_calculation(&mut app);
    formula(&app).focus();
    app.run_steps(2);
    press(&mut app, Modifiers::COMMAND, egui::Key::A);
    formula(&app).type_text("Abs ( 1 ; 2 )");
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "Too many parameters in the function");
    // The alert's OK (Return); the dialog stays with the error's `;`
    // selected.
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    common::wait_until(&mut app, "the alert to close", |app| {
        app.query_by_label("Too many parameters in the function")
            .is_none()
    });
    assert!(app.query_by_label("Specify Calculation").is_some());
    assert_eq!(editor_text(&app), "Abs ( 1 ; 2 )");
    assert!(app.query_by_label("L: 1 C: 9").is_some());
    // Typing replaces the selected `;`.
    formula(&app).type_text("+");
    app.run_steps(2);
    assert_eq!(editor_text(&app), "Abs ( 1 + 2 )");
    // Cancel keeps the step as it was.
    common::click(&mut app, "Cancel");
    common::wait_until(&mut app, "the dialog to close", |app| {
        app.query_by_label("Specify Calculation").is_none()
    });
    assert!(app.query_by_label(FIRST).is_some());
    assert!(app.query_by_label("Tab Go to Entry Field *").is_none());
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn specify_calculation_lists_functions_by_category_and_search_with_descriptions() {
    let (mut app, _) = open();
    specify_calculation(&mut app);
    // The view menu narrows the list to the Get functions (the selectors).
    common::click(&mut app, "Function View");
    common::wait_for_label(&mut app, "All Functions by Type");
    common::click(&mut app, "Get");
    common::wait_for_label(&mut app, "Get ( AccountName )");
    assert!(app.query_by_label("Abs ( number )").is_none());
    common::fill(&mut app, "Search Functions", "scriptparam");
    common::click(&mut app, "Get ( ScriptParameter )");
    assert!(app
        .query_by_label("Returns the script parameter passed into the current script.")
        .is_some());
    // Searching the whole catalog.
    common::click(&mut app, "Function View");
    common::wait_for_label(&mut app, "All Functions by Name");
    common::click(&mut app, "All Functions by Name");
    common::text_field(&app, "Search Functions").focus();
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Search Functions").type_text("upper");
    app.run_steps(2);
    common::wait_for_label(&mut app, "Upper ( text )");
    assert!(app.query_by_label("Abs ( number )").is_none());
    double_click(&mut app, "Upper ( text )");
    // Inserted at the cursor (the formula's start), `text` selected.
    assert_eq!(editor_text(&app), "Upper ( text )Get ( ScriptParameter )");
    assert!(app.query_by_label("L: 1 C: 9").is_some());
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there (for
/// review).
fn calculation_shot(app: &mut common::App, name: &str) {
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(std::path::Path::new(&dir).join(format!("{name}.png")))
        .expect("write png");
}

#[test]
fn specify_calculation_completes_a_typed_name_over_its_prefix() {
    let (mut app, state) = open();
    specify_calculation(&mut app);
    calculation_shot(&mut app, "42b-completion-before");
    formula(&app).focus();
    app.run_steps(2);
    press(&mut app, Modifiers::COMMAND, egui::Key::A);
    // Typing a name lists the names completing it: the context's field
    // as the fields pane writes it.
    formula(&app).type_text("\"é😀\" & Ti");
    app.run_steps(2);
    common::wait_for_label(&mut app, "Contacts::Title");
    calculation_shot(&mut app, "42c-completion-list");
    // Escape closes the list only.
    press(&mut app, Modifiers::NONE, egui::Key::Escape);
    assert!(app.query_by_label("Contacts::Title").is_none());
    assert!(app.query_by_label("Specify Calculation").is_some());
    assert_eq!(editor_text(&app), "\"é😀\" & Ti");
    // Typing reopens and refines it; Return writes the name over the
    // typed prefix, the cursor after it (characters, not bytes).
    formula(&app).type_text("t");
    app.run_steps(2);
    common::wait_for_label(&mut app, "Contacts::Title");
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    assert_eq!(editor_text(&app), "\"é😀\" & Contacts::Title");
    assert!(app.query_by_label("L: 1 C: 23").is_some());
    calculation_shot(&mut app, "42d-completion-inserted");
    // A second Escape cancels the dialog; the step keeps its formula.
    press(&mut app, Modifiers::NONE, egui::Key::Escape);
    common::wait_until(&mut app, "the dialog to close", |app| {
        app.query_by_label("Specify Calculation").is_none()
    });
    assert!(app.query_by_label(FIRST).is_some());
    assert!(state.lock().unwrap().events.is_empty());
}

/// Drag with the primary button from `from` by `by`, over a few frames.
fn drag(app: &mut common::App, from: egui::Pos2, by: egui::Vec2) {
    app.hover_at(from);
    app.step();
    app.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    app.step();
    for n in 1..=4 {
        app.hover_at(from + by * (n as f32 / 4.0));
        app.step();
    }
    app.event(egui::Event::PointerButton {
        pos: from + by,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    app.run_steps(3);
}

#[test]
fn specify_calculation_moves_resizes_and_cancels_with_escape() {
    let (mut app, _) = open();
    specify_calculation(&mut app);
    // The window fills the harness but for a margin; its corner resizes
    // it, and the editor takes the change.
    let editor = formula(&app).rect();
    let ok = app.get_by_role_and_label(egui::Role::Button, "OK").rect();
    let corner = ok.right_bottom() + egui::vec2(16.0, 14.0);
    drag(&mut app, corner, egui::vec2(-120.0, -60.0));
    let resized = formula(&app).rect();
    assert_eq!(resized.min, editor.min);
    assert_eq!(resized.size(), editor.size() - egui::vec2(120.0, 60.0));
    // The title bar moves it.
    let title = app.get_by_label("Specify Calculation").rect();
    drag(
        &mut app,
        title.left_center() - egui::vec2(20.0, 0.0),
        egui::vec2(30.0, 20.0),
    );
    assert_eq!(
        formula(&app).rect().min,
        resized.min + egui::vec2(30.0, 20.0)
    );
    press(&mut app, Modifiers::NONE, egui::Key::Escape);
    common::wait_until(&mut app, "the dialog to close", |app| {
        app.query_by_label("Specify Calculation").is_none()
    });
    assert!(app.query_by_label(FIRST).is_some());
}

fn click_position(app: &mut common::App, pos: egui::Pos2) {
    let input = app.input_mut();
    input.events.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    app.run_steps(2);
}

fn edit_inline_option(app: &mut common::App, label: &str, prefix: &str) {
    let row = app.get_by_label(label).rect();
    click_position(app, egui::pos2(row.left() + 5.0, row.center().y));
    let at = step_text_position(app, label, prefix);
    click_position(app, at);
    common::wait_until(app, "the inline calculation editor", |app| {
        common::find_text_field(app, "Edit Option").is_some()
    });
}

#[test]
fn editing_window_width_inline_preserves_height_top_and_left() {
    use common::workspace::calc;
    use om_data::scripts::display::step_line;
    let mut catalog = contacts();
    let mut step = om_data::ScriptStep::new(119);
    for (slot, source) in [(1, "400"), (2, "600"), (3, "20"), (4, "30")] {
        step.set_calculation(slot, Some(&calc(source)));
    }
    let text = step_line(&step, &NoStepNames).text;
    let prefix = &text[..text.find("600").unwrap()];
    let label = format!("Line 3: {text}");
    catalog.scripts[1].steps.push(step);
    let (mut app, state) = open_catalog(false, catalog);
    edit_inline_option(&mut app, &label, prefix);
    assert_eq!(
        common::text_field(&app, "Edit Option").value().as_deref(),
        Some("600")
    );
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Edit Option").type_text("900");
    app.run_steps(2);
    press(&mut app, Modifiers::NONE, egui::Key::Enter);
    assert_eq!(save(&mut app, &state), [145, 50, 119]);
    let state = state.lock().unwrap();
    let step = &state.scripts.scripts[1].steps[2];
    for (slot, source) in [(1, "400"), (2, "900"), (3, "20"), (4, "30")] {
        assert_eq!(
            calculation_text(&step.calculation(slot).unwrap(), &NoStepNames),
            source
        );
    }
}

#[test]
fn an_untouched_bare_inline_calculation_keeps_its_comment_and_stays_saved() {
    use common::workspace::calc;
    use om_data::scripts::display::step_line;
    let mut catalog = contacts();
    let mut step = om_data::ScriptStep::new(68);
    let source = "1 // see: 42";
    step.set_calculation(0, Some(&calc(source)));
    let text = step_line(&step, &NoStepNames).text;
    let label = format!("Line 3: {text}");
    catalog.scripts[1]
        .steps
        .extend([step, om_data::ScriptStep::new(70)]);
    let (mut app, _) = open_catalog(false, catalog);
    edit_inline_option(&mut app, &label, "If [ ");
    assert_eq!(
        common::text_field(&app, "Edit Option").value().as_deref(),
        Some(source)
    );
    let row = app.get_by_label("Line 2: Select All").rect();
    click_position(&mut app, egui::pos2(row.left() + 5.0, row.center().y));
    common::wait_until(&mut app, "the unchanged inline edit to finish", |app| {
        common::find_text_field(app, "Edit Option").is_none()
    });
    assert!(app.query_by_label("Tab Go to Entry Field *").is_none());
    app.get_by_label("Tab Go to Entry Field");
}

#[test]
fn a_read_only_catalog_explains_saving_and_still_allows_drafting() {
    let mut catalog = contacts();
    catalog.writable = false;
    catalog.unavailable_reason = Some("This test provider allows local drafts only.".into());
    let (mut app, state) = open_catalog(true, catalog);
    assert!(app
        .query_by_label("This test provider allows local drafts only.")
        .is_some());
    assert!(app
        .query_all_by_label("New Script")
        .all(|n| n.accesskit_node().is_disabled()));
    assert!(app
        .query_all_by_label("New Folder")
        .all(|n| n.accesskit_node().is_disabled()));
    type_step(&mut app, "Beep");
    common::wait_for_label(&mut app, "Line 3: Beep");
    let events = state.lock().unwrap().events.clone();
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    settle(&mut app);
    assert_eq!(state.lock().unwrap().events, events);
    assert!(app.query_by_label("Line 3: Beep").is_some());
    assert!(state.lock().unwrap().scripts.scripts[1]
        .steps
        .iter()
        .all(|step| step.opcode != 93));
}

/// The script "Greeting" read over a new connection to `hosted`.
fn greeting_on(hosted: &hosted::Hosted) -> om_data::Script {
    use om_data::DataProvider;
    let catalog = hosted.opened().scripts().expect("read the scripts");
    catalog.script(GREETING).expect("Greeting").clone()
}

/// Whether a step line of the editor starts with `prefix`.
fn has_line(app: &common::App, prefix: &str) -> bool {
    app.query_all(By::new().label_contains(prefix)).any(|node| {
        node.accesskit_node()
            .label()
            .is_some_and(|l| l.starts_with(prefix))
    })
}

const GREETING: u64 = 5;

/// With `OM_SCREENSHOTS` set to a folder, render the window there as
/// `name`.png and add `name: line` to the folder's `readback.txt`.
fn shot(app: &mut common::App, name: &str, line: &str) {
    use std::io::Write as _;
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    let dir = std::path::Path::new(&dir);
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

/// Whether each step of `script` is enabled, as `readback.txt` notes it.
fn steps_enabled(script: &om_data::Script) -> Vec<bool> {
    script.steps.iter().map(|step| step.is_enabled()).collect()
}

#[test]
fn a_step_edit_saves_to_openmaker_server_and_never_over_a_changed_script() {
    use om_data::DataProvider;
    use om_hbam::{serial, OwnedNode};
    use std::path::Path;
    // Set Variable [ $who ; "world" ], Exit Script [ "hello" ], with an
    // item UUID, a key OpenMaker does not know and step data no step uses,
    // which a step save keeps as the host has them.
    let mut greeting = fixture::script(
        GREETING,
        "Greeting",
        vec![
            fixture::set_variable("$who", "\"world\""),
            fixture::exit_with("\"hello\""),
        ],
    );
    greeting.steps[0].set_unique_ids([5; 16], None);
    greeting
        .metadata
        .push(OwnedNode::leaf(vec![0x77], vec![1, 2, 3]));
    greeting.unreferenced_data.push(OwnedNode::list(
        serial::encode(40),
        vec![OwnedNode::leaf(vec![0xd8], vec![9; 16])],
    ));
    let file = fixture::TempFile::from_tree("gui-script-save", &fixture::tree(&[greeting], true));
    let hosted = hosted::host_file(
        "script-save",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let before = greeting_on(&hosted);
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_until(&mut app, "Greeting's steps", |app| {
        has_line(app, "Line 2: Exit Script")
    });
    shot(
        &mut app,
        "01-before",
        &format!("host steps enabled {:?}", steps_enabled(&before)),
    );

    // Disable Exit Script and save: a new connection reads it back, the
    // rest as it was.
    app.get(By::new().label_contains("Line 2: Exit Script"))
        .click();
    app.run_steps(2);
    press(&mut app, Modifiers::COMMAND, egui::Key::Slash);
    common::wait_until(&mut app, "the disabled step", |app| {
        has_line(app, "Line 2: // Exit Script")
    });
    common::wait_for_label(&mut app, "Tab Greeting *");
    // A draft: nothing reaches the host before Save.
    let drafted = greeting_on(&hosted);
    assert_eq!(drafted.steps, before.steps);
    shot(
        &mut app,
        "02-draft",
        &format!(
            "tab \"Greeting *\", host steps enabled {:?}",
            steps_enabled(&drafted)
        ),
    );
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_for_label(&mut app, "Tab Greeting");
    assert!(app
        .query(By::new().label_contains("could not be saved"))
        .is_none());
    let saved = greeting_on(&hosted);
    let mut disabled = before.steps[1].clone();
    disabled.set_enabled(false);
    assert_eq!(saved.steps, [before.steps[0].clone(), disabled]);
    let unstamped = |script: &om_data::Script| -> Vec<OwnedNode> {
        script
            .metadata
            .iter()
            .filter(|node| !om_protocol::authoring::is_stamp(&node.segment))
            .cloned()
            .collect()
    };
    assert_eq!(unstamped(&saved), unstamped(&before));
    assert_eq!(saved.unreferenced_data, before.unreferenced_data);
    shot(
        &mut app,
        "03-saved",
        &format!(
            "tab \"Greeting\", new connection steps enabled {:?}, other keys kept",
            steps_enabled(&saved)
        ),
    );

    // Another connection enables it again. The window's next save is
    // refused as a conflict: its draft stays, the host keeps the other
    // change.
    let mut other = hosted.opened();
    let mut enabled = other.scripts().unwrap().script(GREETING).unwrap().clone();
    enabled.steps[1].set_enabled(true);
    other
        .save_script(enabled)
        .expect("the other connection's save");
    app.get(By::new().label_contains("Line 1: Set Variable"))
        .click();
    app.run_steps(2);
    press(&mut app, Modifiers::COMMAND, egui::Key::Slash);
    common::wait_for_label(&mut app, "Tab Greeting *");
    press(&mut app, Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "the conflict", |app| {
        app.query(By::new().label_contains("was changed on the host since it was read"))
            .is_some()
    });
    let kept = greeting_on(&hosted);
    shot(
        &mut app,
        "04-conflict",
        &format!(
            "save refused as a conflict, host steps enabled {:?}",
            steps_enabled(&kept)
        ),
    );
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "Tab Greeting *");
    assert!(kept.steps[0].is_enabled() && kept.steps[1].is_enabled());
}

/// New Script on a file hosted by OpenMaker Server: named in the scripts
/// pane, given a step and saved, it is stored under the next id the
/// file's index issues (not the one the workspace numbered it with), read
/// back by a new connection and from the file on disk, listed in the
/// Scripts menu, and runs. A catalog another connection changed since the
/// workspace read it is not saved over.
#[test]
fn a_new_script_saves_to_openmaker_server_and_runs() {
    use common::workspace::{add_script_index, create_script, scripts_on_disk};
    use om_data::DataProvider;
    use std::path::Path;
    let greeting = fixture::script(GREETING, "Greeting", vec![fixture::exit_with("\"hello\"")]);
    let mut root = fixture::tree(std::slice::from_ref(&greeting), true);
    // Ids up to 7 were issued (6 and 7 deleted since): the workspace
    // numbers its new script 6, the host stores it as 8.
    add_script_index(&mut root, std::slice::from_ref(&greeting), 7);
    let file = fixture::TempFile::from_tree("gui-new-script", &root);
    let hosted = hosted::host_file(
        "new-script",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    shot(&mut app, "new-01-file", "the hosted file, one script");
    create_script(
        &mut app,
        "Say Built",
        "Show Custom Dialog [ Title: \"Built\" ; Message: \"Made in OpenMaker\" ]\n",
        "Show Custom Dialog",
    );
    shot(&mut app, "new-02-saved", "Say Built saved with its step");

    // A new connection reads it back under the host's id, after Greeting.
    let read = hosted.opened().scripts().expect("read the scripts");
    let names: Vec<(u64, &str)> = read
        .scripts
        .iter()
        .map(|s| (s.id, s.name.as_str()))
        .collect();
    assert_eq!(names, [(GREETING, "Greeting"), (8, "Say Built")]);
    let built = read.script(8).unwrap();
    assert_eq!(
        built.steps.iter().map(|s| s.opcode).collect::<Vec<_>>(),
        [87]
    );
    assert!(built.in_scripts_menu());
    // So does the file on disk.
    let on_disk = scripts_on_disk(hosted.file());
    assert_eq!(
        on_disk.script(8).map(|s| s.name.as_str()),
        Some("Say Built")
    );

    // The Scripts menu lists it, and it runs.
    common::click(&mut app, "Close Window");
    common::wait_until(&mut app, "the workspace to close", |app| {
        app.query_by_label("Tab Say Built").is_none()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Say Built");
    common::wait_for_label(&mut app, "Made in OpenMaker");
    shot(
        &mut app,
        "new-03-runs",
        "Say Built runs from the Scripts menu",
    );
    common::click(&mut app, "OK");

    // Another connection renames Greeting; the window's next New Script
    // is refused, and the host keeps the other change.
    let mut other = hosted.opened();
    let mut renamed = other.scripts_for_editing().unwrap();
    renamed.scripts[0].name = "Hello".into();
    other
        .save_script_catalog(renamed)
        .expect("the other rename");
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_until(&mut app, "the workspace's scripts", |app| {
        app.query_by_label("Say Built").is_some()
    });
    // The workspace read the scripts after the rename: one more rename
    // from the other connection makes its catalog old.
    let mut again = other.scripts_for_editing().unwrap();
    again.scripts[0].name = "Hello Again".into();
    other.save_script_catalog(again).expect("the second rename");
    app.query_all_by_label("New Script").next().unwrap().click();
    common::wait_until(&mut app, "the conflict", |app| {
        app.query(By::new().label_contains("Another user changed the scripts"))
            .is_some()
    });
    shot(
        &mut app,
        "new-04-conflict",
        "New Script refused: changed since read",
    );
    common::click(&mut app, "OK");
    let kept = hosted.opened().scripts().unwrap();
    let names: Vec<&str> = kept.scripts.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Hello Again", "Say Built"]);

    // A save the file on disk does not take is not reported as saved: the
    // server's new copy of the file cannot be made where a folder stands,
    // so the host answers the first upload with an error, which cannot say
    // what it stored, and keeps nothing of it (docs/server-storage.md 5).
    let mut blocked = hosted.file().as_os_str().to_owned();
    blocked.push(".omsave");
    std::fs::create_dir(&blocked).unwrap();
    let mut third = hosted.opened();
    let mut unsaved = third.scripts_for_editing().unwrap();
    unsaved.scripts[0].name = "Unsaved".into();
    let error = third.save_script_catalog(unsaved).unwrap_err();
    std::fs::remove_dir(&blocked).unwrap();
    assert!(
        error.message.contains("saved only in part"),
        "{}",
        error.message
    );
    let kept = hosted.opened().scripts().unwrap();
    let names: Vec<&str> = kept.scripts.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Hello Again", "Say Built"], "the host kept nothing");
    assert_eq!(
        scripts_on_disk(hosted.file()).scripts[0].name,
        "Hello Again"
    );
}

/// Duplicate and Delete in the scripts pane, on a file hosted by
/// OpenMaker Server: the copy is stored under the host's next id with the
/// original's step, the deleted script is gone from the host and from the
/// file on disk, and the copy runs from the Scripts menu.
#[test]
fn duplicate_and_delete_save_to_openmaker_server() {
    use common::workspace::{add_script_index, scripts_on_disk};
    use om_data::DataProvider;
    use std::path::Path;
    let mut show = om_data::ScriptStep::new(87);
    show.set_calculation(0, Some(&fixture::calc("\"Copied\"")));
    show.set_calculation(1, Some(&fixture::calc("\"from Greeting\"")));
    let greeting = fixture::script(GREETING, "Greeting", vec![show]);
    let farewell = fixture::script(6, "Farewell", vec![fixture::exit_with("\"bye\"")]);
    let scripts = [greeting, farewell];
    let mut root = fixture::tree(&scripts, true);
    add_script_index(&mut root, &scripts, 6);
    let file = fixture::TempFile::from_tree("gui-duplicate-delete", &root);
    let hosted = hosted::host_file(
        "duplicate-delete",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(&mut app, "Farewell");
    settle(&mut app);

    app.get_by_label("Greeting").click_secondary();
    common::wait_until(&mut app, "the context menu", |app| {
        app.query_by_label("Duplicate").is_some()
    });
    common::click(&mut app, "Duplicate");
    common::wait_until(&mut app, "the copy on the host", |_| {
        greeting_copy(&hosted).is_some()
    });
    settle(&mut app);
    let copy = greeting_copy(&hosted).unwrap();
    assert_eq!(copy.id, 7);
    let original = greeting_on(&hosted);
    assert_eq!(copy.steps.len(), 1);
    assert_eq!(
        copy.steps[0].calculation(1),
        original.steps[0].calculation(1)
    );

    app.get_by_label("Farewell").click_secondary();
    common::wait_until(&mut app, "the context menu", |app| {
        app.query_by_label("Delete").is_some()
    });
    common::click(&mut app, "Delete");
    common::wait_for_label(
        &mut app,
        "Are you sure you want to permanently delete “Farewell”?",
    );
    app.get_by_role_and_label(egui::Role::Button, "Delete")
        .click();
    common::wait_until(&mut app, "the deletion on the host", |_| {
        hosted
            .opened()
            .scripts()
            .is_ok_and(|c| c.scripts.iter().all(|s| s.name != "Farewell"))
    });
    settle(&mut app);
    shot(
        &mut app,
        "dup-01-saved",
        "Greeting Copy saved, Farewell deleted",
    );
    assert!(app
        .query(By::new().label_contains("could not be saved"))
        .is_none());
    let names = |catalog: &om_data::ScriptCatalog| -> Vec<(u64, String)> {
        catalog
            .scripts
            .iter()
            .map(|s| (s.id, s.name.clone()))
            .collect()
    };
    let expected = vec![
        (GREETING, "Greeting".to_owned()),
        (7, "Greeting Copy".to_owned()),
    ];
    assert_eq!(names(&hosted.opened().scripts().unwrap()), expected);
    assert_eq!(names(&scripts_on_disk(hosted.file())), expected);

    common::click(&mut app, "Close Window");
    common::wait_until(&mut app, "the workspace to close", |app| {
        app.query_by_label("Tab Greeting").is_none()
    });
    common::open_menu(&mut app, "Scripts");
    assert!(app.query_by_label("Farewell").is_none());
    common::click(&mut app, "Greeting Copy");
    common::wait_for_label(&mut app, "from Greeting");
    common::click(&mut app, "OK");
}

/// The script "Greeting Copy" read over a new connection to `hosted`.
fn greeting_copy(hosted: &hosted::Hosted) -> Option<om_data::Script> {
    use om_data::DataProvider;
    let catalog = hosted.opened().scripts().ok()?;
    catalog
        .scripts
        .into_iter()
        .find(|s| s.name == "Greeting Copy")
}

/// A new Save Records as PDF step names its file in Specify Output File:
/// a path without its kind is refused there, a path with one is stored as
/// the step's path list with `kFilePath` (script-steps.md 11.0). A step
/// that reads a file (Insert File) has the same row, in Specify File.
#[test]
fn specify_output_file_stores_a_steps_path_list() {
    let (mut app, state) = open();
    type_step(&mut app, "Save Records as PDF");
    common::wait_until(&mut app, "the new step", |app| {
        has_line(app, "Line 3: Save Records as PDF")
    });
    let line = app
        .get(By::new().label_contains("Line 3: Save Records as PDF"))
        .accesskit_node()
        .label()
        .unwrap()
        .to_string();
    // The gear is past the end of the long line, outside the editor: a
    // double click on the step's name opens its options too.
    let row = app.get_by_label(&line).rect();
    double_click_at(&mut app, egui::pos2(row.left() + 60.0, row.center().y));
    common::wait_for_label(&mut app, "\"Save Records as PDF\" Options");
    common::click(&mut app, "Specify File Paths…");
    common::wait_for_label(&mut app, "Specify Output File");
    app.get_by_label("File Path List").type_text("Invoice.pdf");
    app.run_steps(2);
    app.query_all_by_label("OK").last().unwrap().click();
    common::wait_until(&mut app, "the refusal", |app| {
        app.query(By::new().label_contains("has no kind")).is_some()
    });
    shot(
        &mut app,
        "paths-01-refused",
        "a path without its kind refused",
    );
    app.get_by_label("File Path List").focus();
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    app.get_by_label("File Path List")
        .type_text("file:Invoices/Invoice.pdf");
    app.run_steps(2);
    app.query_all_by_label("OK").last().unwrap().click();
    common::wait_until(&mut app, "the path dialog to close", |app| {
        app.query_by_label("Specify Output File").is_none()
    });
    shot(&mut app, "paths-02-row", "the options row shows the path");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the step's path", |app| {
        has_line(app, "Line 3: Save Records as PDF")
            && app
                .query(By::new().label_contains("file:Invoices/Invoice.pdf"))
                .is_some()
    });
    assert_eq!(save(&mut app, &state), [145, 50, 144]);
    {
        let state = state.lock().unwrap();
        let step = &state.scripts.scripts[1].steps[2];
        assert_eq!(
            step.path_list().as_deref(),
            Some("file:Invoices/Invoice.pdf")
        );
        assert!(step.has_flag(om_data::scripts::flag::FILE_PATH));
    }

    // A step that reads a file names it in Specify File.
    type_step(&mut app, "Insert File");
    common::wait_until(&mut app, "the Insert File step", |app| {
        has_line(app, "Line 3: Insert File")
    });
    let line = app
        .get(By::new().label_contains("Line 3: Insert File"))
        .accesskit_node()
        .label()
        .unwrap()
        .to_string();
    let row = app.get_by_label(&line).rect();
    double_click_at(&mut app, egui::pos2(row.left() + 40.0, row.center().y));
    common::wait_for_label(&mut app, "\"Insert File\" Options");
    common::click(&mut app, "Specify File Paths…");
    common::wait_for_label(&mut app, "Specify File");
    app.query_all_by_label("Cancel").last().unwrap().click();
    common::wait_until(&mut app, "the path dialog to close", |app| {
        app.query_by_label("Specify File").is_none()
    });
    common::click(&mut app, "Cancel");
}

/// A new Close Window step closes a window by name: Window Name in its
/// options needs a name, and stores step value 1 and the name calculation
/// in slot 0, without `kMenuSelection`; back to Current Window stores 0
/// and drops the name.
#[test]
fn a_window_step_names_its_window() {
    let (mut app, state) = open();
    type_step(&mut app, "Close Window");
    common::wait_until(&mut app, "the new step", |app| {
        has_line(app, "Line 3: Close Window")
    });
    let line = app
        .get(By::new().label_contains("Line 3: Close Window"))
        .accesskit_node()
        .label()
        .unwrap()
        .to_string();
    let row = app.get_by_label(&line).rect();
    double_click_at(&mut app, egui::pos2(row.left() + 40.0, row.center().y));
    common::wait_for_label(&mut app, "\"Close Window\" Options");
    assert!(common::find_text_field(&app, "Window Name").is_none());
    app.get_by_role_and_label(egui::Role::ComboBox, "Window")
        .click();
    common::wait_for_label(&mut app, "Window Name");
    common::click(&mut app, "Window Name");
    common::wait_until(&mut app, "the name field", |app| {
        common::find_text_field(app, "Window Name").is_some()
    });
    // By name without a name is refused: the runtime would act on the
    // current window.
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the missing name's alert", |app| {
        app.query(By::new().label_contains("needs the window's name"))
            .is_some()
    });
    app.key_press(egui::Key::Enter);
    app.run_steps(3);
    assert!(app.query_by_label("\"Close Window\" Options").is_some());
    common::text_field(&app, "Window Name").focus();
    common::text_field(&app, "Window Name").type_text("\"Details\"");
    app.run_steps(2);
    shot(&mut app, "window-01-by-name", "Close Window by name");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the step's window name", |app| {
        app.query(By::new().label_contains("\"Details\"")).is_some()
    });
    assert_eq!(save(&mut app, &state), [145, 50, 121]);
    let step = state.lock().unwrap().scripts.scripts[1].steps[2].clone();
    assert_eq!(step.value, 1);
    assert!(!step.has_flag(om_data::scripts::flag::MENU_SELECTION));
    assert_eq!(
        calculation_text(&step.calculation(0).unwrap(), &NoStepNames),
        "\"Details\""
    );

    // Back to the current window: no name is kept.
    let line = app
        .get(By::new().label_contains("Line 3: Close Window"))
        .accesskit_node()
        .label()
        .unwrap()
        .to_string();
    let row = app.get_by_label(&line).rect();
    double_click_at(&mut app, egui::pos2(row.left() + 40.0, row.center().y));
    common::wait_for_label(&mut app, "\"Close Window\" Options");
    app.get_by_role_and_label(egui::Role::ComboBox, "Window")
        .click();
    common::wait_for_label(&mut app, "Current Window");
    common::click(&mut app, "Current Window");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the name gone from the step", |app| {
        app.query(By::new().label_contains("\"Details\"")).is_none()
    });
    assert_eq!(save(&mut app, &state), [145, 50, 121]);
    let step = state.lock().unwrap().scripts.scripts[1].steps[2].clone();
    assert_eq!(step.value, 0);
    assert!(step.calculation(0).is_none());
}
