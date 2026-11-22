//! Several windows of one file through the GUI end to end: scripts run on
//! the provider worker against the native provider and an in-process
//! `om-server` hosting the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs` with five tasks. As FileMaker
//! Server's engine answered (script-runtime.md 13.25, battery
//! s35-windows):
//!
//! - New Window opens a window on the current window's layout, found set
//!   and record, named as given or after the current window (" - 2");
//! - each window keeps its own found set, current record and mode: an omit
//!   and a move in the new window leave the first one as it was;
//! - Select Window and Close Window by name, 112 for a name no window has,
//!   and Close Window returns to the window current before the closed one;
//! - `Get ( WindowName )` and `WindowNames` answer for each window.
//!
//! The windows are real ones: after the script the second window is open
//! beside the first, each showing its own found set.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use egui_kittest::kittest::{By, NodeT as _, Queryable};
use fixture::{calc, ACCOUNT, PASSWORD};
use om_data::DataProvider;
use om_scripts::{flag, Script, ScriptStep};

/// New Window [ Name: name ] (empty: no name).
fn new_window(name: &str) -> ScriptStep {
    let mut step = ScriptStep::new(122);
    if !name.is_empty() {
        step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
    }
    step
}

/// Select Window (123) or Close Window (121) [ Name: name ], or the
/// current window without a name.
fn window_step(id: u16, name: Option<&str>) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    if let Some(name) = name {
        step.value = 1;
        step.flags |= flag::MENU_SELECTION;
        step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
    }
    step
}

fn note(name: &str, text: &str) -> ScriptStep {
    fixture::set_variable(name, text)
}

/// `name:found:record:task` of the current window.
const STATE: &str = "Get ( LastError ) & \":\" & Get ( WindowName ) & \":\" & \
                     Get ( FoundCount ) & \":\" & Get ( RecordNumber ) & \":\" & Tasks::Name";

fn scripts() -> Vec<Script> {
    let mut show = ScriptStep::new(87);
    show.set_calculation(0, Some(&calc("\"Windows\"")));
    show.set_calculation(
        1,
        Some(&calc(
            "$w1 & \" | \" & $w2 & \" | \" & $omit & \" | \" & $back & \" | \" & $third & \
             \" | \" & $closed & \" | \" & $missing & \" | \" & $names",
        )),
    );
    show.set_text(0, Some("OK"));
    let mut omit = ScriptStep::new(25);
    omit.flags |= flag::NO_INTERACT;
    vec![fixture::script(
        1,
        "Windows",
        vec![
            fixture::on(86, true),
            fixture::go_to_layout(fixture::LAYOUT),
            fixture::go_to_record(1),
            fixture::go_to_record(4),
            note("$w1", STATE),
            new_window("W2"),
            note("$w2", STATE),
            omit,
            fixture::go_to_record(4),
            note("$omit", STATE),
            window_step(123, Some("Tasks")),
            note("$back", STATE),
            new_window(""),
            note("$third", STATE),
            window_step(121, None),
            note("$closed", STATE),
            window_step(123, Some("nosuch")),
            note("$missing", "Get ( LastError )"),
            note(
                "$names",
                "Substitute ( WindowNames ( \"\" ) ; ¶ ; \",\" ) & \":\" & \
                 ValueCount ( WindowNames ( \"Tasks.fmp12\" ) ) & \":\" & \
                 WindowNames ( \"Other\" )",
            ),
            show,
        ],
    )]
}

fn host(tag: &str) -> hosted::Hosted {
    host_scripts(tag, &scripts())
}

fn host_scripts(tag: &str, scripts: &[Script]) -> hosted::Hosted {
    let mut root = fixture::tree(scripts, true);
    fixture::add_tasks(&mut root, 3..=5);
    let file = fixture::TempFile::from_tree(&format!("gui-client-windows-{tag}"), &root);
    hosted::host_file(
        &format!("client-windows-{tag}"),
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    )
}

/// The host administrator's Send Message shows over the window while it is
/// idle, as FileMaker Pro shows it: "Message from “<server>”", the text,
/// the countdown and Close, which closes it (#156).
#[test]
fn a_host_message_shows_over_the_idle_window_until_closed() {
    let hosted = host("host-message");
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    let server = hosted.server();
    let client = server
        .clients()
        .into_iter()
        .find(|client| !client.files.is_empty())
        .expect("the window's session")
        .id;
    server
        .send_message(client, "Back in five minutes.")
        .unwrap();
    common::wait_for_label(&mut app, "Back in five minutes.");
    app.get_by_label("Message from “openmaker-test”");
    assert!(app
        .query_by_label_contains("(This dialog will be dismissed in")
        .is_some());
    common::click(&mut app, "Close");
    app.run_steps(2);
    assert!(app.query_by_label("Back in five minutes.").is_none());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // A close with no time to leave forces the disconnect at once: the
    // alert says so with the administrator's text, and OK closes the file,
    // both its windows.
    common::open_menu(&mut app, "Window");
    common::click(&mut app, "New Window");
    common::wait_until(&mut app, "two windows", |app| !app.state().script_running());
    assert_eq!(app.state_mut().window_titles().len(), 2);
    let file = server
        .files()
        .into_iter()
        .find(|info| info.clients > 0)
        .expect("the window's file")
        .id;
    server
        .close_file(
            file,
            &om_server::Notice {
                message: "Closing for the night.".into(),
                grace: std::time::Duration::ZERO,
            },
        )
        .unwrap();
    common::wait_until(&mut app, "the forced disconnect", |app| {
        app.query_by_label_contains("File has been forcefully disconnected by the host.")
            .is_some()
    });
    assert!(app
        .query_by_label_contains("[From Administrator: “Closing for the night.”]")
        .is_some());
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());
    assert_eq!(app.state_mut().window_titles().len(), 1);
}

/// A window that goes to another table's layout and back returns to the
/// record it showed on the first table (#660): FileMaker keeps a current
/// record per table occurrence.
#[test]
fn another_tables_layout_and_back_returns_to_the_record_shown() {
    let to_notes = fixture::script(
        1,
        "To Notes",
        vec![fixture::go_to_layout(fixture::NOTES_LAYOUT)],
    );
    let hosted = host_scripts("tables", &[to_notes]);
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    for _ in 0..2 {
        common::click(&mut app, "Next");
        app.run_steps(2);
    }
    let at = |app: &mut common::App, what: &str, number: &str| {
        common::wait_until(app, what, |app| {
            app.state().is_settled() && common::record_number(app) == number
        });
    };
    at(&mut app, "task 3", "3");
    common::select_layout(&mut app, "Notes");
    at(&mut app, "the note", "1");
    common::select_layout(&mut app, "Tasks");
    at(&mut app, "task 3 again", "3");
    // A script's Go to Layout leaves Tasks on task 3 too.
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "To Notes");
    at(&mut app, "the note by script", "1");
    common::select_layout(&mut app, "Tasks");
    at(&mut app, "task 3 after the script", "3");
}

#[test]
fn each_window_has_its_own_found_set_and_record() {
    let hosted = host("state");
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Windows");
    common::wait_until(&mut app, "the dialog", |app| {
        app.query(By::new().label_contains("0:Tasks:5:2")).is_some()
    });
    let shown = app
        .query(By::new().label_contains("0:Tasks:5:2"))
        .map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value()).unwrap_or_default()
        })
        .unwrap_or_default();
    // W2 starts on W1's found set and record; its omit and move leave W1
    // on record 2 of 5; the unnamed window is named after the current one;
    // closing it returns to W1 (current before it); 112 for no such window.
    assert_eq!(
        shown,
        "0:Tasks:5:2:Ship | 0:W2:5:2:Ship | 0:W2:4:3:Task 4 | 0:Tasks:5:2:Ship | \
         0:Tasks - 2:5:2:Ship | 0:Tasks:5:2:Ship | 112 | Tasks,W2:2:"
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Both windows are open, the first one in front.
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)", "W2"]);
}

#[test]
fn the_window_menu_opens_a_window_of_the_file() {
    let hosted = host("menu");
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Window");
    common::click(&mut app, "New Window");
    common::wait_until(&mut app, "two windows", |app| !app.state().script_running());
    // The new window is in front, named after the first one.
    assert_eq!(
        app.state_mut().window_titles(),
        ["Tasks - 2", "Tasks (127.0.0.1)"]
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// W2's index zero is task 2; W1's index zero is task 1. A fresh script
/// must select W2's provider cursor, even though W1 last used the provider.
fn independent_windows(pause: bool) -> Vec<Script> {
    let mut omit = ScriptStep::new(25);
    omit.flags |= flag::NO_INTERACT;
    let mut setup = vec![
        fixture::go_to_record(1),
        new_window("W2"),
        omit,
        fixture::go_to_record(1),
        window_step(123, Some("Tasks")),
    ];
    if pause {
        setup.push(ScriptStep::new(62)); // Pause/Resume Script [Indefinitely].
    }
    vec![
        fixture::script(1, "Prepare windows", setup),
        fixture::script(
            2,
            "Mark current",
            vec![
                fixture::set_field(fixture::NAME, "\"marked\""),
                ScriptStep::new(75), // Commit Records/Requests.
            ],
        ),
    ]
}

fn open_windows(hosted: &hosted::Hosted) -> common::App {
    open_and_run(hosted, "Prepare windows")
}

/// Open the hosted file and start `script` from the Scripts menu.
fn open_and_run(hosted: &hosted::Hosted, script: &str) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, script);
    app
}

fn stored_names(hosted: &hosted::Hosted) -> Vec<(i64, Option<String>)> {
    let mut other = hosted.opened();
    let page = other.records(fixture::LAYOUT as u32, 0, 10).unwrap();
    let name = page.fields.iter().position(|f| f.name == "Name").unwrap();
    let rows = page
        .records
        .into_iter()
        .map(|r| (r.id, r.values[name].clone()))
        .collect();
    other.close();
    rows
}

fn select_w2_and_mark(app: &mut common::App) {
    app.query_all(By::new().role(egui::Role::Button).label("Window"))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.get_by_role_and_label(egui::Role::Button, "W2").click();
    app.run_steps(2);
    common::wait_until(app, "W2", |app| app.state().window_name() == "W2");
    app.get_by_role_and_label(egui::Role::Window, "W2")
        .get_by_role_and_label(egui::Role::Button, "Scripts")
        .click();
    app.run_steps(2);
    common::click(app, "Mark current");
}

#[test]
fn a_fresh_script_uses_its_windows_provider_cursor() {
    let hosted = host_scripts("fresh", &independent_windows(false));
    let before = stored_names(&hosted);
    let mut app = open_windows(&hosted);
    common::wait_until(&mut app, "prepared windows", |app| {
        !app.state().script_running()
    });
    select_w2_and_mark(&mut app);
    common::wait_until(&mut app, "the write", |app| !app.state().script_running());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let mut expected = before;
    expected.iter_mut().find(|(id, _)| *id == 2).unwrap().1 = Some("marked".into());
    assert_eq!(stored_names(&hosted), expected);
}

#[test]
fn a_script_started_during_a_pause_keeps_both_windows_and_the_right_cursor() {
    let hosted = host_scripts("paused", &independent_windows(true));
    let before = stored_names(&hosted);
    let mut app = open_windows(&hosted);
    common::wait_until(&mut app, "the pause", |app| app.state().script_paused());
    select_w2_and_mark(&mut app);
    common::wait_until(&mut app, "the script on top to finish", |app| {
        app.state().script_paused()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(app.state_mut().window_titles(), ["W2", "Tasks (127.0.0.1)"]);
    let mut expected = before;
    expected.iter_mut().find(|(id, _)| *id == 2).unwrap().1 = Some("marked".into());
    assert_eq!(stored_names(&hosted), expected);
    common::click(&mut app, "Continue");
    common::wait_until(&mut app, "the paused script to finish", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state_mut().window_titles(), ["W2", "Tasks (127.0.0.1)"]);
}

/// Like [`independent_windows`], but W1's script reads the first record of
/// its found set (task 1) before it ends or pauses, and "Mark current"
/// prefixes the current record's name with the first record's of its own
/// window's found set: W2's is task 2.
fn first_records(pause: bool) -> Vec<Script> {
    let mut omit = ScriptStep::new(25);
    omit.flags |= flag::NO_INTERACT;
    let mut setup = vec![
        fixture::go_to_record(1),
        new_window("W2"),
        omit,
        fixture::go_to_record(1),
        window_step(123, Some("Tasks")),
        note("$first", "GetNthRecord ( Tasks::Name ; 1 )"),
    ];
    if pause {
        setup.push(ScriptStep::new(62)); // Pause/Resume Script [Indefinitely].
    }
    vec![
        fixture::script(1, "Prepare windows", setup),
        fixture::script(
            2,
            "Mark current",
            vec![
                fixture::set_field(
                    fixture::NAME,
                    "\"first:\" & GetNthRecord ( Tasks::Name ; 1 )",
                ),
                ScriptStep::new(75), // Commit Records/Requests.
            ],
        ),
    ]
}

/// The stored names after W2's "Mark current" from [`first_records`]:
/// task 2 named after itself, the first record of W2's found set.
fn marked_with_own_first(before: Vec<(i64, Option<String>)>) -> Vec<(i64, Option<String>)> {
    let own = before.iter().find(|(id, _)| *id == 2).unwrap().1.clone();
    let mut expected = before;
    expected.iter_mut().find(|(id, _)| *id == 2).unwrap().1 =
        Some(format!("first:{}", own.unwrap_or_default()));
    expected
}

#[test]
fn a_fresh_script_reads_its_own_windows_found_set_not_the_last_scripts() {
    let hosted = host_scripts("first-fresh", &first_records(false));
    let before = stored_names(&hosted);
    let mut app = open_windows(&hosted);
    common::wait_until(&mut app, "prepared windows", |app| {
        !app.state().script_running()
    });
    select_w2_and_mark(&mut app);
    common::wait_until(&mut app, "the write", |app| !app.state().script_running());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored_names(&hosted), marked_with_own_first(before));
}

#[test]
fn a_script_started_during_a_pause_reads_its_own_windows_found_set() {
    let hosted = host_scripts("first-paused", &first_records(true));
    let before = stored_names(&hosted);
    let mut app = open_windows(&hosted);
    common::wait_until(&mut app, "the pause", |app| app.state().script_paused());
    select_w2_and_mark(&mut app);
    common::wait_until(&mut app, "the script on top to finish", |app| {
        app.state().script_paused()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored_names(&hosted), marked_with_own_first(before));
    common::click(&mut app, "Continue");
    common::wait_until(&mut app, "the paused script to finish", |app| {
        !app.state().script_running()
    });
}

/// W1's script renames W2 behind it, pauses and, continued, marks its own
/// record (task 1); W2 is on task 2.
fn rename_behind() -> Vec<Script> {
    let mut omit = ScriptStep::new(25);
    omit.flags |= flag::NO_INTERACT;
    let mut rename = window_step(124, Some("W2"));
    rename.set_calculation(1, Some(&calc("\"Renamed\"")));
    vec![fixture::script(
        1,
        "Rename behind",
        vec![
            fixture::go_to_record(1),
            new_window("W2"),
            omit,
            fixture::go_to_record(1),
            window_step(123, Some("Tasks")),
            rename,
            ScriptStep::new(62), // Pause/Resume Script [Indefinitely].
            fixture::set_field(fixture::NAME, "\"marked\""),
            ScriptStep::new(75), // Commit Records/Requests.
        ],
    )]
}

/// Set Window Title of a window behind the script's changes that window
/// where it is: the script's window stays in front, and the script goes on
/// there after its pause.
#[test]
fn renaming_a_window_behind_keeps_a_paused_script_in_its_own_window() {
    let hosted = host_scripts("rename-behind", &rename_behind());
    let before = stored_names(&hosted);
    let mut app = open_and_run(&hosted, "Rename behind");
    common::wait_until(&mut app, "the pause", |app| app.state().script_paused());
    assert_eq!(
        app.state_mut().window_titles(),
        ["Tasks (127.0.0.1)", "Renamed"]
    );
    // The pause bar is the front window's, under the embedded window
    // behind it: pressed through AccessKit, as in its own native window.
    app.get_by_role_and_label(egui::Role::Button, "Continue")
        .click_accesskit();
    app.run_steps(2);
    common::wait_until(&mut app, "the script to finish", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let mut expected = before;
    expected.iter_mut().find(|(id, _)| *id == 1).unwrap().1 = Some("marked".into());
    assert_eq!(stored_names(&hosted), expected);
}

/// Open the menu `menu` of the root window (the file's first window),
/// which is drawn first; it need not be in front.
fn open_root_menu(app: &mut common::App, menu: &str) {
    app.query_all(By::new().role(egui::Role::Button).label(menu))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
}

fn three_windows() -> Vec<Script> {
    vec![fixture::script(
        1,
        "Three windows",
        vec![fixture::go_to_record(1), new_window("W2"), new_window("W3")],
    )]
}

/// The Window menu of a window that is not in front brings the window it
/// names to the front, not another one.
#[test]
fn a_window_menu_behind_the_front_window_brings_its_choice_to_the_front() {
    let hosted = host_scripts("menu-behind", &three_windows());
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Three windows");
    common::wait_until(&mut app, "three windows", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state_mut().window_titles()[0], "W3");
    // The root window (behind W3) picks W2 from its Window menu.
    open_root_menu(&mut app, "Window");
    app.get_by_role_and_label(egui::Role::Button, "W2").click();
    app.run_steps(3);
    assert_eq!(app.state_mut().window_titles()[0], "W2");
    assert_eq!(app.state_mut().window_titles().len(), 3);
}

/// File > Close in a window that is not in front closes the file and all
/// its windows.
#[test]
fn closing_the_file_from_a_window_behind_the_front_one_closes_every_window() {
    let hosted = host_scripts("close-behind", &three_windows());
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Three windows");
    common::wait_until(&mut app, "three windows", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state_mut().window_titles()[0], "W3");
    open_root_menu(&mut app, "File");
    app.get_by_role_and_label(egui::Role::Button, "Close")
        .click();
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(app.state_mut().window_titles().len(), 1);
}

/// Open File keeps the caller's script context, but its visible window is a
/// presentation request. A hidden open and a later explicit selection differ.
#[test]
fn a_visible_peer_open_is_presented_even_when_the_script_returns_to_its_caller() {
    #[path = "../../om-provider/tests/common/related.rs"]
    mod related;
    use om_hbam::OwnedNode;
    let open = |hidden| {
        let mut s = ScriptStep::new(33);
        s.set_calculation(0, Some(&calc("\"Peer\"")));
        if hidden {
            s.flags |= flag::OPTION;
        }
        s
    };
    // An indefinite pause, then where the script went on.
    let pause = ScriptStep::new(62);
    let mut resumed = ScriptStep::new(87);
    resumed.set_calculation(0, Some(&calc("\"Resumed\"")));
    resumed.set_calculation(1, Some(&calc("\"resumed in \" & Get ( FileName )")));
    resumed.set_text(0, Some("OK"));
    let mut root = fixture::tree(
        &[
            fixture::script(1, "Open hidden peer", vec![open(true)]),
            fixture::script(2, "Open visible peer", vec![open(false)]),
            fixture::script(
                3,
                "Open then select caller",
                vec![open(false), window_step(123, Some("Root"))],
            ),
            fixture::script(4, "Open then pause", vec![open(false), pause, resumed]),
        ],
        true,
    );
    let mut definition = root
        .at(&[&[4], &[5], &[1], &[2]])
        .unwrap()
        .value
        .clone()
        .unwrap();
    om_layout::encode::double(8, 600.0, &mut definition);
    root.put(&[&[4], &[5], &[1], &[2]], OwnedNode::leaf([], definition));
    root.put(
        &[&[4], &[5], &[1], &[3], &[1], &[2]],
        OwnedNode::leaf([], om_layout::encode::new_body_part(400.0)),
    );
    root.put(
        &[&[4], &[5], &[1], &[3], &[1], &[7]],
        OwnedNode::leaf([], vec![0, 0, 0, 1, 2, 1, 50]),
    );
    let step = fixture::perform_script(2);
    let bounds = om_layout::Rect {
        top: 180.0,
        left: 40.0,
        bottom: 220.0,
        right: 260.0,
        ..Default::default()
    };
    let object = om_layout::encode::button_object(
        50,
        &bounds,
        step.id(),
        step.value,
        step.flags,
        &step.reserved,
    );
    let store = om_layout::encode::append_to_store(&[], &[50], &[object]);
    root.put(&[&[4], &[5], &[1], &[7]], OwnedNode::leaf([], store));
    root.put(
        &[&[4], &[5], &[1], &[5], &[50], &[1]],
        om_scripts::text_node(&[], "Open Peer"),
    );
    if let Some(data) = step.data {
        root.put(
            &[&[4], &[5], &[1], &[5], &[50], &[0x11]],
            data.encode(&[0x11]),
        );
    }
    let mut peer = fixture::tree(
        &[fixture::script(
            1,
            "Select caller",
            vec![window_step(123, Some("Root"))],
        )],
        true,
    );
    let startup = fixture::script(
        9,
        "Startup",
        vec![fixture::set_variable("$$opened", "True")],
    )
    .encode();
    peer.put(&[&[17], &[5], &startup.segment.clone()], startup);
    peer.put(
        &[&[2], &[0x18]],
        OwnedNode::leaf([], vec![0, 0, 4, 3, 0, 0, 2, 1, 9, 0, 0]),
    );
    let files: Vec<(&str, OwnedNode)> = vec![("Root.fmp12", root), ("Peer.fmp12", peer)];
    let host = related::Host::new("visible-peer-presentation", files);
    let provider = Box::new(host.provider());
    let connection = host.connection("Root.fmp12");
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .renderer(common::renderer())
        .build_eframe(move |_cc| om_gui::OpenMakerApp::new(provider).open_on_launch(connection));
    common::wait_until(&mut app, "root", |a| a.state().is_settled());
    let run = |app: &mut common::App, script: &str| {
        let peer = app.state().window_name().starts_with("Peer");
        app.query_all(By::new().role(egui::Role::Button).label("Scripts"))
            .nth(usize::from(peer))
            .unwrap()
            .click();
        app.run_steps(2);
        common::click(app, script);
        common::wait_until(app, script, |a| {
            !a.state().script_running() && a.state().is_settled()
        });
    };
    app.get_by_role_and_label(egui::Role::Button, "Open Peer")
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "visible peer button", |a| {
        !a.state().script_running()
            && a.state().is_settled()
            && a.state().window_name() == "Peer (127.0.0.1)"
    });
    assert_eq!(app.state().window_name(), "Peer (127.0.0.1)");
    run(&mut app, "Select caller");
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
    run(&mut app, "Open hidden peer");
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");

    run(&mut app, "Open then select caller");
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
    run(&mut app, "Open visible peer");
    assert_eq!(
        app.state().window_name(),
        "Peer (127.0.0.1)",
        "reuse is presented too"
    );
    // Later ordinary layout/record replies must not repeat the request.
    app.query_all(By::new().role(egui::Role::Button).label("Window"))
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Root (127.0.0.1)");
    common::wait_until(&mut app, "user-selected root", |a| {
        a.state().window_name() == "Root (127.0.0.1)"
    });
    app.run_steps(8);
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
    app.query_all(By::new().role(egui::Role::Button).label("Scripts"))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Open then pause");
    common::wait_until(&mut app, "the pause", |a| a.state().script_paused());
    // The paused script's own window shows the pause bar, so Continue goes
    // on in the caller, not in the file it opened.
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
    // Through accessibility: in the test harness the other file's window
    // is embedded over the root viewport, where a pointer would land on it.
    app.get_by_label("Continue").click_accesskit();
    common::wait_for_label(&mut app, "resumed in Root");
    app.get_by_label("OK").click_accesskit();
    common::wait_until(&mut app, "the caller to finish", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    // Answered there, the caller stays in front (as after any prompt).
    assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
}
