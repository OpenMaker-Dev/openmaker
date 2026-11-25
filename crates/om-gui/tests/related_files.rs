//! A file whose occurrence shows a table of a related file the host does
//! not have (file-metadata.md 1.1), through the GUI end to end: the native
//! provider and an in-process `om-server` hosting the in-memory Tasks file
//! of `om-server/tests/fixtures/tasks.rs`. The window opens the file and
//! says once that the related file could not be opened.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use fixture::{ACCOUNT, PASSWORD, TABLE};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;

/// Its data source has no name, so the line names the file that names it.
const WARNING: &str = "Related file “Lines” (named by “Tasks”): the host does not have it.";

/// Tasks with an occurrence "Line Tasks" of the Tasks table of "Lines", a
/// file named by a relative reference of data source 1.
fn host() -> hosted::Hosted {
    // A script, so the window finds the scripts catalog it reads.
    let mut root = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    let occurrence = 0xd0_0040;
    fixture::add_occurrence(&mut root, occurrence, "Line Tasks", TABLE);
    let key = |id: u64| {
        let segment = uint_segment(id);
        [vec![segment.len() as u8 + 1, segment.len() as u8], segment].concat()
    };
    root.put(
        &[&[3], &[17], &[5], &uint_segment(occurrence), &[2]],
        OwnedNode::leaf(Vec::new(), [key(1), key(TABLE)].concat()),
    );
    let mut reference = 1u32.to_be_bytes().to_vec();
    for text in ["file", "", "Lines"] {
        reference.extend(om_scsu::encode_counted(text));
    }
    root.put(
        &[&[32], &[5], &[1], &[0x82]],
        OwnedNode::leaf(Vec::new(), reference),
    );
    let file = fixture::TempFile::from_tree("gui-related-files", &root);
    hosted::host_file(
        "related-files",
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    )
}

#[test]
fn a_related_file_that_cannot_be_opened_is_reported_once() {
    let hosted = host();
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the warning", |app| app.state().error().is_some());
    let error = app.state().error().unwrap().to_owned();
    assert!(error.starts_with(WARNING), "{error}");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the open file", |app| app.state().is_settled());
    assert!(app.state().is_open());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Refresh Window");
    common::wait_until(&mut app, "the refreshed window", |app| {
        app.state().is_settled()
    });
    app.run_steps(3);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[path = "../../om-provider/tests/common/related.rs"]
mod related;

/// A fresh explicit reconnect retains ephemeral credentials just as an
/// ordinary open does, including for a foreign script not reached yet.
#[test]
fn a_script_opens_a_cold_related_file_after_explicit_reconnect() {
    use egui_kittest::kittest::{By, Queryable};
    let mut call = fixture::perform_script(1);
    call.set_data_leaf(0x20, Some(OwnedNode::leaf([0x20], [1, 1])));
    let mut show = om_scripts::ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&fixture::calc(
            "\"result:\" & Get ( LastError ) & \"|\" & Get ( ScriptResult )",
        )),
    );
    let mut root = fixture::tree(
        &[fixture::script(
            1,
            "Cold call",
            vec![fixture::on(86, true), call, show],
        )],
        true,
    );
    related::reference(&mut root, (0xd0_0040, "Remote Tasks"), 1, "Related");
    let child = fixture::tree(
        &[fixture::script(
            1,
            "Report file",
            vec![fixture::exit_with("Get ( FileName )")],
        )],
        true,
    );
    let host = related::Host::new(
        "reconnected-cold-script",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness(Box::new(host.provider().with_related_on_demand(true)));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "Root", |a| a.state().is_settled());
    assert!(!host
        .captures
        .opens_and_logins()
        .0
        .contains_key("Related.fmp12"));
    for client in host.handle.clients() {
        host.handle.disconnect_client(client.id);
    }
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Refresh Window");
    common::wait_until(&mut app, "lost connection", |a| {
        a.query_by_label("Reconnect…").is_some()
    });
    common::click(&mut app, "Reconnect…");
    common::wait_until(&mut app, "password", |a| {
        common::find_text_field(a, "Password").is_some()
    });
    common::fill(&mut app, "Password", PASSWORD);
    app.get_by_role_and_label(egui::Role::Button, "Reconnect")
        .click();
    common::wait_until(&mut app, "fresh Root", |a| {
        a.state().connection_lost().is_none() && a.state().is_settled()
    });
    assert!(!host
        .captures
        .opens_and_logins()
        .0
        .contains_key("Related.fmp12"));
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Cold call");
    common::wait_until(&mut app, "foreign script result", |a| {
        a.query(By::new().label_contains("result:")).is_some()
    });
    if let Some(path) = std::env::var_os("OM_RECONNECT_ARTIFACT") {
        app.render().unwrap().save(path).unwrap();
    }
    assert!(
        app.query(By::new().label("result:0|Related")).is_some(),
        "cold foreign call failed after explicit reconnect"
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "call finished", |a| !a.state().script_running());
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&1));
    assert_eq!(logins.get("Related.fmp12"), Some(&1));
    for op in [
        om_protocol::Opcode::HostCommit,
        om_protocol::Opcode::UploadListToHostTempLocation,
    ] {
        assert!(host.captures.with_opcode(op).is_empty(), "no save replay");
    }
}

#[test]
fn related_container_reads_survive_root_window_close_and_last_close_reopens_fresh_sessions() {
    use egui_kittest::kittest::{By, Queryable};
    use om_scripts::{Calculation, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    let mut names = fixture::photo_names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    let compile = |text: &str| {
        let mut cx = om_formula::Context::new(&names);
        cx.occurrence = Some(fixture::OCCURRENCE);
        let compiled = om_formula::compile(text, &cx).unwrap();
        Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )
    };
    let mut read = ScriptStep::new(87);
    read.set_calculation(1, Some(&compile("GetAsText ( Tasks::Photo ) & \"|\" & GetAsText ( Remote Tasks::Photo ) & \"|\" & Length ( Remote Tasks::Photo )")));
    read.set_text(0, Some("OK"));
    let mut new = ScriptStep::new(122);
    new.set_calculation(0, Some(&compile("\"Survivor\"")));
    let mut close_root = ScriptStep::new(121);
    close_root.value = 1;
    close_root.flags |= om_scripts::flag::MENU_SELECTION;
    close_root.set_calculation(0, Some(&compile("\"Root\"")));
    let scripts = [
        fixture::script(1, "Survive root close", vec![new, close_root, read.clone()]),
        fixture::script(2, "Read containers", vec![read]),
        fixture::script(3, "Close last", vec![ScriptStep::new(121)]),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_photo(&mut root);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        9,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (REMOTE, fixture::NAME),
    );
    let mut child = fixture::tree(&[], true);
    fixture::add_photo(&mut child);
    let value = om_data::calc::Container::file("related.txt", b"child bytes".to_vec());
    let (mut nodes, _) = om_data::containers::tree(&value).unwrap();
    nodes.push(OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec()));
    nodes.sort_by(|a, b| a.segment.cmp(&b.segment));
    child.put(
        &[&uint_segment(TABLE), &[0x1f], &[5], &[1]],
        OwnedNode::list(vec![1], nodes),
    );
    let host = related::Host::new(
        "gui-container-lifecycle",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the root file", |app| app.state().is_settled());
    for script in ["Survive root close", "Read containers"] {
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, script);
        common::wait_until(&mut app, "file-specific container values", |app| {
            app.query(By::new().label("photo2.png|related.txt|11"))
                .is_some()
        });
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "the script end", |app| {
            !app.state().script_running()
        });
        assert_eq!(app.state_mut().window_titles(), ["Survivor"]);
    }
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Root.fmp12"), Some(&1));
    assert_eq!(opens.get("Related.fmp12"), Some(&1));
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Close last");
    common::wait_until(&mut app, "last close", |app| !app.state().is_open());
    common::wait_until(&mut app, "both owning sessions to disconnect", |_| {
        host.handle
            .clients()
            .iter()
            .all(|client| client.files.is_empty())
    });
    hosted::replace(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    hosted::replace(&mut app, "Account Name", ACCOUNT);
    hosted::replace(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the reopened file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Read containers");
    common::wait_until(&mut app, "reopened container reads", |app| {
        app.query(By::new().label("photo2.png|related.txt|11"))
            .is_some()
    });
    common::click(&mut app, "OK");
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Root.fmp12"), Some(&2));
    assert_eq!(opens.get("Related.fmp12"), Some(&2));
    // Last-close sends the provider's disconnect on both connections. The
    // same lifecycle is exercised again when this harness is dropped.
}

/// A script reads a related file's unstored formula. The formula keeps
/// that file's names, number formats, Get selectors and global variables.
#[test]
fn script_calculations_keep_the_related_files_context() {
    use egui_kittest::kittest::{By, Queryable};
    use fixture::{NAME, OCCURRENCE};
    use om_scripts::{Calculation, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    const INFO: u64 = 18;
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(TABLE, INFO, "File Info");
    let formula = "Let ( $$marker = \"root\" ; Remote Tasks::File Info & \"|\" & $$marker & \"|\" & Get ( FileName ) )";
    let mut cx = om_formula::Context::new(&names);
    cx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(formula, &cx).unwrap();
    let mut show = ScriptStep::new(87);
    show.set_calculation(
        1,
        Some(&Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )),
    );
    show.set_text(0, Some("OK"));
    let mut root = fixture::tree(&[fixture::script(1, "Read related", vec![show])], true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    let mut child = fixture::tree(&[], true);
    child.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(NAME), &[16]],
        om_scripts::text_node(&[], "Child Name"),
    );
    fixture::add_field(&mut child, TABLE, INFO, "File Info", 2, 1, 0, 0x2_0020, 0);
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), INFO, 5,
        "Get ( FileName ) & \"|\" & GetField ( \"Child Name\" ) & \"|\" & Evaluate ( \"GetAsNumber ( \\\"1,5\\\" ) = 1,5\" ) & \"|\" & Let ( $$marker = \"child\" ; $$marker )");
    let mut locale = om_data::calc::Locale::en_us();
    locale.decimal_separator = ',';
    locale.thousands_separator = '.';
    child.put(
        &[&[2], &[0x0d]],
        OwnedNode::leaf(Vec::new(), locale.to_record()),
    );
    let host = related::Host::new(
        "gui-script-context",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Read related");
    common::wait_until(&mut app, "the related formula's answer", |app| {
        app.query(By::new().label("Related|Write tests|1|child|root|Root"))
            .is_some()
    });
    common::click(&mut app, "OK");
}

#[test]
fn cross_file_related_navigation_uses_native_peer_layouts_and_window_found_sets() {
    use egui_kittest::kittest::{By, Queryable};
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_scripts::{flag, ScriptStep, StoredKey};
    const REMOTE: u64 = 0xd0_0040;
    let cases = [
        ("Related only", true, false, false, 1),
        ("Keep related found set", false, false, false, 1),
        ("Related found set in new window", true, true, true, 2),
    ];
    let mut scripts = Vec::new();
    for (i, (name, only, all, new, _)) in cases.iter().enumerate() {
        let nested = 10 + i as u64;
        let mut call = fixture::with_calc(1, "\"parameter\"");
        call.set_script_key(StoredKey::from_id(nested));
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(
            1,
            Some(&fixture::calc(
                "Get ( ScriptResult ) & \"|returned:\" & Get ( FileName ) & \"|\" & Count",
            )),
        );
        scripts.push(fixture::script(
            i as u64 + 1,
            name,
            vec![
                fixture::on(86, true),
                fixture::set_variable("$$mark", "\"root\""),
                call,
                show,
            ],
        ));
        let mut related = ScriptStep::new(74);
        related.value = 5;
        related.flags |= flag::OPTION;
        related.set_flag(flag::RESTORE, *only);
        related.set_flag(flag::OPTION2, *all);
        related.set_flag(flag::TURN_ON, *new);
        related.set_member_key(StoredKey::from_id(1));
        if !only {
            related.value = 3;
            related.set_calculation(5, Some(&fixture::calc("\"Tasks\"")));
        }
        related.set_slot_child(
            0,
            0x11,
            Some(OwnedNode::leaf([0x11], om_formula::Key::from_id(REMOTE).0)),
        );
        if *new {
            related.set_calculation(0, Some(&fixture::calc("\"Related results\"")));
        }
        scripts.push(fixture::script(nested, &format!("Navigate {i}"), vec![
            related,
            fixture::exit_with("Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( FoundCount ) & \"|\" & Count & \"|\" & $$mark & \"|\" & Get ( ScriptParameter )"),
        ]));
    }
    let mut root = fixture::tree(&scripts, true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    // An owned desktop session reads the peer's script catalog as well as
    // its layouts. Reuse script id 1 to exercise the file boundary too.
    let mut child = fixture::tree(&[fixture::script(1, "Peer no-op", vec![])], true);
    for (record, value) in [(1, "37"), (2, "55")] {
        child.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &om_hbam::serial::encode(record),
                &uint_segment(COUNT),
            ],
            om_scripts::text_node(&[], value),
        );
    }
    let host = related::Host::new(
        "gui-cross-file-gtrr",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the generated root file", |app| {
        app.state().is_settled()
    });
    for (name, _, _, new, found) in cases {
        // The root viewport is drawn first; peer viewports have their own
        // Scripts menus after the first successful navigation.
        app.query_all_by_role_and_label(egui::Role::Button, "Scripts")
            .next()
            .unwrap()
            .click();
        app.run_steps(2);
        common::click(&mut app, name);
        let expected = format!("0|Related|Tasks|1|{found}|37|root|parameter|returned:Root|3");
        common::wait_until(&mut app, "peer data and restored caller", |app| {
            app.query(By::new().label_contains("|returned:")).is_some()
                || app.state().error().is_some()
                || !app.state().script_running()
        });
        let labels: Vec<_> = app
            .query_all(By::new().role(egui::Role::Label))
            .filter_map(|n| n.value())
            .collect();
        assert!(
            app.query(By::new().label(&expected)).is_some(),
            "{name}: expected {expected:?}, error {:?}, labels {labels:?}",
            app.state().error()
        );
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "the caller script end", |app| {
            !app.state().script_running() && app.state().is_settled()
        });
        assert_eq!(app.state().window_name(), "Root (127.0.0.1)");
        let titles = app.state_mut().window_titles();
        assert!(titles.contains(&"Root (127.0.0.1)".into()), "{titles:?}");
        assert!(titles.contains(&"Related (127.0.0.1)".into()), "{titles:?}");
        if new {
            assert!(titles.contains(&"Related results".into()), "{titles:?}");
        }
    }
}

/// The shapes FileMaker Pro stores most for Go to Related Record into
/// another file's layout (external layouts, only related records, a stored
/// layout key), each followed by a Perform Script of the other file's own
/// script through the data source: that script runs in the other file's
/// window, on the related found set, also after Select Window names the
/// new window the step opened. Not compared with FileMaker Pro; what the
/// calling script's own `Get ( FileName )` answers after the call is not
/// asserted.
#[test]
fn related_navigation_into_another_file_then_its_own_script() {
    use egui_kittest::kittest::{By, NodeT, Queryable};
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_scripts::{flag, ScriptStep, StoredKey};
    const REMOTE: u64 = 0xd0_0040;
    let related = |value: u16, new_window: bool| {
        let mut step = ScriptStep::new(74);
        step.value = value;
        step.flags |= flag::OPTION;
        step.set_flag(flag::RESTORE, true);
        if value == 5 {
            step.set_member_key(StoredKey::from_id(1));
        }
        step.set_slot_child(
            0,
            0x11,
            Some(OwnedNode::leaf([0x11], om_formula::Key::from_id(REMOTE).0)),
        );
        if new_window {
            step.set_flag(flag::TURN_ON, true);
            step.set_calculation(0, Some(&fixture::calc("\"Related results\"")));
        }
        step
    };
    // Perform Script ["Report" from file "Related"], through data source 1.
    let mut report = fixture::perform_script(2);
    let source = uint_segment(1);
    report.set_data_leaf(
        0x20,
        Some(OwnedNode::leaf(
            [0x20],
            [vec![source.len() as u8], source].concat(),
        )),
    );
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= flag::MENU_SELECTION;
    select.set_calculation(0, Some(&fixture::calc("\"Related results\"")));
    let mut show = ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&fixture::calc(
            "\"result:\" & Get ( LastError ) & \"|\" & Get ( ScriptResult )",
        )),
    );
    let cases = [
        ("Key then script", vec![related(5, false), report.clone()]),
        (
            "New window then select",
            vec![related(5, true), select, report],
        ),
    ];
    let scripts: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(i, (name, steps))| {
            let mut steps = steps.clone();
            steps.insert(0, fixture::on(86, true));
            steps.push(show.clone());
            fixture::script(i as u64 + 1, name, steps)
        })
        .collect();
    let mut root = fixture::tree(&scripts, true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    let mut child = fixture::tree(
        &[
            fixture::script(1, "Peer no-op", vec![]),
            fixture::script(
                2,
                "Report",
                vec![fixture::exit_with(
                    "Get ( FileName ) & \"|\" & Get ( WindowName ) & \"|\" & Get ( LayoutName ) & \"|\" & \
                     Get ( FoundCount ) & \"|\" & Get ( RecordID ) & \"|\" & Count",
                )],
            ),
        ],
        true,
    );
    for (record, value) in [(1, "37"), (2, "55")] {
        child.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &om_hbam::serial::encode(record),
                &uint_segment(COUNT),
            ],
            om_scripts::text_node(&[], value),
        );
    }
    let host = related::Host::new(
        "gui-cross-file-gtrr-script",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the generated root file", |app| {
        app.state().is_settled()
    });
    let mut seen = Vec::new();
    for (name, _) in &cases {
        app.query_all_by_role_and_label(egui::Role::Button, "Scripts")
            .next()
            .unwrap()
            .click();
        app.run_steps(2);
        common::click(&mut app, name);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let text = |app: &common::App| {
            app.query_all(By::new())
                .filter_map(|n| n.accesskit_node().label().or_else(|| n.value()))
                .find(|l| l.starts_with("result:"))
        };
        while std::time::Instant::now() < deadline
            && text(&app).is_none()
            && app.state().error().is_none()
            && app.state().script_running()
        {
            app.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.run_steps(3);
        if text(&app).is_none() {
            let labels: Vec<_> = app
                .query_all(By::new())
                .filter_map(|n| n.accesskit_node().label().or_else(|| n.value()))
                .filter(|l| !l.is_empty())
                .collect();
            eprintln!(
                "{name}: running {} paused {} prompt {:?} windows {:?} labels {labels:?}",
                app.state().script_running(),
                app.state().script_paused(),
                app.state().script_prompt(),
                app.state_mut().window_titles()
            );
        }
        let shown = text(&app);
        seen.push(format!("{name}: {shown:?} error {:?}", app.state().error()));
        if shown.is_some() {
            app.query_all(By::new().label("OK")).last().unwrap().click();
            app.run_steps(2);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline
            && !(!app.state().script_running() && app.state().is_settled())
        {
            app.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if app.state().script_running() || !app.state().is_settled() {
            let labels: Vec<_> = app
                .query_all(By::new())
                .filter_map(|n| n.accesskit_node().label().or_else(|| n.value()))
                .filter(|l| !l.is_empty())
                .collect();
            eprintln!(
                "{name} after OK: running {} paused {} settled {} prompt {:?} windows {:?} labels {labels:?}",
                app.state().script_running(),
                app.state().script_paused(),
                app.state().is_settled(),
                app.state().script_prompt(),
                app.state_mut().window_titles()
            );
            panic!("{name}: the caller script did not end");
        }
    }
    assert_eq!(
        seen,
        [
            "Key then script: Some(\"result:0|Related|Related|Tasks|1|1|37\") error None",
            "New window then select: Some(\"result:0|Related|Related results|Tasks|1|1|37\") error None",
        ]
    );
}

#[test]
fn an_imported_layout_edits_commits_corrects_and_reverts_on_the_peer() {
    use egui_kittest::kittest::{By, NodeT, Queryable};
    const REMOTE: u64 = 0xd0_0040;
    let mut root = fixture::tree(
        &[fixture::script(
            1,
            "Edit peer",
            vec![
                fixture::set_field_in(REMOTE, fixture::NAME, "Get ( FileName ) & \" script\""),
                om_scripts::ScriptStep::new(75),
            ],
        )],
        true,
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    // The Root file's Notes layout displays Related's Tasks table.
    let mut alias = om_hbam::serial::decode(&uint_segment(REMOTE)).unwrap();
    let mut definition = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        definition.push(if alias == 0 { byte } else { byte | 0x80 });
        if alias == 0 {
            break;
        }
    }
    root.put(&[&[4], &[5], &[2], &[2]], OwnedNode::leaf([], definition));
    let mut child = fixture::tree(&[], true);
    child.put(
        &[&uint_segment(TABLE), &[5], &[1], &[1]],
        om_scripts::text_node(&[], "PEER ROW"),
    );
    fixture::set_field_validation(&mut child, TABLE, fixture::NAME, 0x401);
    fixture::add_formula(
        &mut child,
        &fixture::names(),
        (TABLE, fixture::OCCURRENCE),
        fixture::NAME,
        0x19,
        "Length ( Self ) <= 20",
    );
    let host = related::Host::new(
        "gui-imported-layout",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "related layout loaded", |a| {
        a.state().is_settled()
    });
    // The synthetic catalog has blank layout canvases; Table view displays
    // the underlying fields without adding unrelated renderer fixtures.
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "peer record on root layout", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.query_all(By::new().value("PEER ROW")).next().is_some()
    });
    assert!(!app
        .get_by_label("New Record")
        .accesskit_node()
        .is_disabled());
    assert!(!app
        .get_by_label("Delete Record")
        .accesskit_node()
        .is_disabled());
    let replace = |app: &mut common::App, label: &str, text: &str| {
        app.get_all_by_role_and_label(egui::Role::TextInput, label)
            .next()
            .unwrap()
            .click();
        app.run_steps(2);
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.get_all_by_role_and_label(egui::Role::TextInput, label)
            .next()
            .unwrap()
            .type_text(text);
        app.run_steps(2);
    };
    let cell = |app: &common::App, label: &str| {
        app.query_all_by_role_and_label(egui::Role::TextInput, label)
            .next()
            .and_then(|n| n.value())
    };
    // An owning commit the host cannot save fails at its op70 with the
    // host's system exception (#1076), before any readback. It must
    // preserve both the draft and the original; Revert restores the
    // original, then a corrected save works.
    let barrier = host.block_saves(1);
    replace(&mut app, "Name", "Refused peer edit");
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "owning commit failure", |a| {
        a.state().error().is_some()
    });
    let error = app.state().error().unwrap();
    assert!(
        error.contains("Related file “Related”")
            && error.contains("commit record")
            && error.contains("SystemException"),
        "{error}"
    );
    common::click(&mut app, "OK");
    assert_eq!(common::record_number(&app), "1");
    assert_eq!(cell(&app, "Name").as_deref(), Some("Refused peer edit"));
    common::revert_record(&mut app);
    common::wait_until(&mut app, "original peer value", |a| {
        cell(a, "Name").as_deref() == Some("PEER ROW")
    });
    drop(barrier);
    replace(&mut app, "Name", "Saved peer edit");
    common::save_record(&mut app);
    common::wait_until(&mut app, "peer save", |a| {
        a.state().error().is_some() || (a.state().is_settled() && !common::editing(a))
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Owning field validation is also surfaced without discarding typing.
    replace(&mut app, "Name", "This peer draft is too long");
    assert_eq!(
        cell(&app, "Name").as_deref(),
        Some("This peer draft is too long")
    );
    assert_eq!(common::record_number(&app), "1");
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "owning validation", |a| {
        a.state().error().is_some()
    });
    assert!(app
        .state()
        .error()
        .unwrap()
        .contains("Related file “Related”"));
    common::click(&mut app, "OK");
    assert_eq!(
        cell(&app, "Name").as_deref(),
        Some("This peer draft is too long")
    );
    replace(&mut app, "Name", "Corrected peer edit");
    assert_eq!(cell(&app, "Name").as_deref(), Some("Corrected peer edit"));
    common::save_record(&mut app);
    common::wait_until(&mut app, "corrected peer save", |a| {
        a.state().error().is_some() || (a.state().is_settled() && !common::editing(a))
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Edit peer");
    common::wait_until(&mut app, "root caller script saved peer", |a| {
        cell(a, "Name").as_deref() == Some("Root script") && a.state().is_settled()
    });
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "root layout loaded", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "root record restored", |a| {
        a.query_all(By::new().value("Write tests")).next().is_some()
    });
    let commits = host.captures.with_opcode(om_protocol::Opcode::HostCommit);
    assert!(commits.len() >= 4);
    assert!(commits.iter().all(|(file, _)| file == "Related.fmp12"));

    // A new record on the peer's layout is held in the window's record
    // session (#892): its refused save keeps it and the typing, and the
    // corrected save stores it once.
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "peer layout again", |a| {
        a.state().is_settled() && common::record_number(a) == "1"
    });
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "held peer record", |a| {
        a.state().is_settled() && common::record_number(a) == "3"
    });
    let new_row = |app: &mut common::App, text: &str| {
        app.get_all_by_role_and_label(egui::Role::TextInput, "Name")
            .last()
            .unwrap()
            .click();
        app.run_steps(2);
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.get_all_by_role_and_label(egui::Role::TextInput, "Name")
            .last()
            .unwrap()
            .type_text(text);
        app.run_steps(2);
    };
    let names = |app: &common::App| -> Vec<String> {
        app.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .filter_map(|n| n.value())
            .collect()
    };
    new_row(&mut app, "This new peer draft is too long");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the held record's validation", |a| {
        a.state().error().is_some()
    });
    assert!(app
        .state()
        .error()
        .unwrap()
        .contains("Related file “Related”"));
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert_eq!(common::record_number(&app), "3");
    assert_eq!(
        names(&app).last().map(String::as_str),
        Some("This new peer draft is too long")
    );
    new_row(&mut app, "Short new");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the held record saved", |a| {
        a.state().error().is_some() || (a.state().is_settled() && !common::editing(a))
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let mut peer = host.open("Related.fmp12");
    let page = om_data::DataProvider::records(&mut peer, 1, 0, 10).unwrap();
    let stored: Vec<_> = page
        .records
        .iter()
        .map(|r| (r.id, r.values[0].clone()))
        .collect();
    assert_eq!(stored.len(), 3, "{stored:?}");
    assert_eq!(stored[2], (3, Some("Short new".to_owned())));
}

#[test]
fn imported_layout_delete_menu_and_scripts_preserve_caller_and_omitted_peers() {
    use egui_kittest::kittest::{By, NodeT, Queryable};
    use om_data::DataProvider;
    use om_scripts::{flag, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    let report = || {
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(1, Some(&fixture::calc(
            "Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( FoundCount )",
        )));
        show
    };
    let delete_script = |id, name, step| {
        let mut delete = ScriptStep::new(step);
        delete.set_flag(flag::NO_INTERACT, true);
        fixture::script(id, name, vec![delete, report()])
    };
    // Delete Record inside a script's transaction, then Revert or Commit
    // Transaction: the deletion is held in the related file's part
    // (record-writes.md, "A script's transaction across files").
    let delete_in_transaction = |id, name, end: u16| {
        let mut delete = ScriptStep::new(9);
        delete.set_flag(flag::NO_INTERACT, true);
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(1, Some(&fixture::calc(
            "$e & \"|\" & Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|in transaction\"",
        )));
        let mut steps = vec![
            fixture::on(86, true),
            fixture::on(223, false),
            ScriptStep::new(205),
            delete,
            fixture::set_variable("$e", "Get ( LastError )"),
        ];
        steps.extend((end == 207).then(|| ScriptStep::new(207)));
        steps.extend([ScriptStep::new(206), show]);
        fixture::script(id, name, steps)
    };
    let mut root = fixture::tree(
        &[
            delete_script(1, "Delete peer", 9),
            delete_script(2, "Delete found peers", 10),
            delete_in_transaction(3, "Delete peer then revert", 207),
            delete_in_transaction(4, "Delete peer then commit", 206),
        ],
        true,
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    let mut alias = om_hbam::serial::decode(&uint_segment(REMOTE)).unwrap();
    let mut definition = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        definition.push(if alias == 0 { byte } else { byte | 0x80 });
        if alias == 0 {
            break;
        }
    }
    root.put(&[&[4], &[5], &[2], &[2]], OwnedNode::leaf([], definition));
    let mut peer = fixture::tree(&[], true);
    fixture::add_tasks(&mut peer, 3..=4);
    let host = related::Host::new(
        "gui-imported-delete",
        vec![("Root.fmp12", root), ("Related.fmp12", peer)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "peer layout loaded", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    let cell = |app: &common::App| {
        app.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .next()
            .and_then(|n| n.value())
    };
    common::wait_until(&mut app, "first peer", |a| {
        cell(a).as_deref() == Some("Write tests")
    });
    app.get_all_by_role_and_label(egui::Role::TextInput, "Name")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.get_all_by_role_and_label(egui::Role::TextInput, "Name")
        .next()
        .unwrap()
        .type_text("Peer draft");
    app.run_steps(2);
    // The actual Records menu is enabled, and cancellation keeps the draft.
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Delete Record...");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Cancel");
    assert_eq!(cell(&app).as_deref(), Some("Peer draft"));
    assert_eq!(common::record_number(&app), "1");
    assert!(host
        .captures
        .with_opcode(om_protocol::Opcode::HostCommit)
        .is_empty());

    let barrier = host.block_saves(1);
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Delete Record...");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "refused peer deletion", |a| {
        a.state().error().is_some()
    });
    assert!(app
        .state()
        .error()
        .unwrap()
        .contains("Related file “Related”"));
    common::click(&mut app, "OK");
    assert_eq!(cell(&app).as_deref(), Some("Peer draft"));
    assert_eq!(common::record_number(&app), "1");
    assert_eq!(
        host.open("Related.fmp12")
            .records(1, 0, 10)
            .unwrap()
            .records
            .len(),
        4
    );
    drop(barrier);
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Delete Record...");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "next peer after menu deletion", |a| {
        a.state().is_settled() && cell(a).as_deref() == Some("Ship")
    });

    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Delete peer");
    common::wait_for_label(&mut app, "0|Root|Notes|3|2");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "delete script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "peer to omit", |a| {
        a.state().is_settled() && common::record_number(a) == "2"
    });
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Omit Record");
    common::wait_until(&mut app, "subset for bulk delete", |a| {
        a.state().is_settled() && cell(a).as_deref() == Some("Task 3")
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Delete found peers");
    common::wait_for_label(&mut app, "0|Root|Notes|0|0");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "bulk script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    common::click(&mut app, "Show All");
    common::wait_until(&mut app, "omitted peer survived", |a| {
        a.state().is_settled() && cell(a).as_deref() == Some("Task 4")
    });
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "root layout restored", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "root record restored", |a| {
        a.query_all(By::new().value("Write tests")).next().is_some()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    for (file, expected) in [("Root.fmp12", vec![1, 2]), ("Related.fmp12", vec![4])] {
        let ids: Vec<_> = host
            .open(file)
            .records(1, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, expected, "{file}");
    }
    assert!(host
        .captures
        .with_opcode(om_protocol::Opcode::HostCommit)
        .iter()
        .all(|(file, _)| file == "Related.fmp12"));

    // The last peer, deleted inside a transaction: kept when it is
    // reverted, gone once it commits, with no error either way.
    let stored = |host: &related::Host| -> Vec<i64> {
        host.open("Related.fmp12")
            .records(1, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect()
    };
    common::select_layout(&mut app, "Notes");
    for (script, kept) in [
        ("Delete peer then revert", vec![4]),
        ("Delete peer then commit", vec![]),
    ] {
        // Which records the window shows after a reverted deletion is
        // unmeasured in FileMaker; all of them are shown again first.
        common::click(&mut app, "Show All");
        common::wait_until(&mut app, "the last peer", |a| {
            a.state().is_settled() && cell(a).as_deref() == Some("Task 4")
        });
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, script);
        let by = By::new().label_contains("|in transaction");
        common::wait_until(&mut app, script, |a| {
            a.query_all(by.clone()).next().is_some()
        });
        let shown = app
            .query_all(by)
            .next()
            .unwrap()
            .accesskit_node()
            .value()
            .unwrap_or_default();
        assert!(shown.starts_with("0|0|Root|Notes|"), "{script}: {shown}");
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "script ended", |a| {
            !a.state().script_running() && a.state().is_settled()
        });
        assert!(
            app.state().error().is_none(),
            "{script}: {:?}",
            app.state().error()
        );
        assert_eq!(stored(&host), kept, "{script}");
    }
}

#[test]
fn imported_layout_new_and_duplicate_menus_and_scripts_create_on_the_peer() {
    use egui_kittest::kittest::{NodeT, Queryable};
    use om_data::DataProvider;
    use om_scripts::ScriptStep;
    const REMOTE: u64 = 0xd0_0040;
    const ENTERED: u64 = 25;
    let report = |error: &str| {
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(1, Some(&fixture::calc(&format!(
            "{error} & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( RecordNumber ) & \"|\" & Get ( FoundCount )",
        ))));
        show
    };
    let mut root = fixture::tree(
        &[
            fixture::script(
                1,
                "New peer",
                vec![
                    ScriptStep::new(7),
                    fixture::set_field_in(REMOTE, fixture::NAME, "\"Scripted\""),
                    ScriptStep::new(75),
                    report("Get ( LastError )"),
                ],
            ),
            fixture::script(
                2,
                "Duplicate peer",
                vec![
                    fixture::on(86, true),
                    ScriptStep::new(8),
                    report("Get ( LastError )"),
                ],
            ),
            fixture::script(
                3,
                "New in transaction",
                vec![
                    fixture::on(86, true),
                    fixture::on(223, false),
                    ScriptStep::new(205),
                    ScriptStep::new(7),
                    fixture::set_variable("$e", "Get ( LastError )"),
                    ScriptStep::new(207),
                    ScriptStep::new(206),
                    report("$e"),
                ],
            ),
            fixture::script(
                4,
                "Duplicate in transaction",
                vec![
                    fixture::on(86, true),
                    fixture::on(223, false),
                    fixture::go_to_record(1),
                    ScriptStep::new(205),
                    ScriptStep::new(8),
                    fixture::set_variable("$e", "Get ( LastError )"),
                    ScriptStep::new(207),
                    ScriptStep::new(206),
                    report("$e"),
                ],
            ),
        ],
        true,
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    let mut alias = om_hbam::serial::decode(&uint_segment(REMOTE)).unwrap();
    let mut definition = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        definition.push(if alias == 0 { byte } else { byte | 0x80 });
        if alias == 0 {
            break;
        }
    }
    root.put(&[&[4], &[5], &[2], &[2]], OwnedNode::leaf([], definition));
    // Root and peer share every table, field and record id. The peer's
    // record 1 holds a container, and its auto-entry reads its file name.
    let mut peer = fixture::tree(&[], true);
    peer.put(
        &[&uint_segment(TABLE), &[5], &[1], &[1]],
        om_scripts::text_node(&[], "PEER ROW"),
    );
    fixture::add_photo(&mut peer);
    fixture::add_field(&mut peer, TABLE, ENTERED, "Entered", 1, 1, 0, 0xa8, 0);
    fixture::add_formula(
        &mut peer,
        &fixture::names(),
        (TABLE, fixture::OCCURRENCE),
        ENTERED,
        5,
        "Name & \"|\" & Get ( FileName )",
    );
    let host = related::Host::new(
        "gui-imported-create",
        vec![("Root.fmp12", root), ("Related.fmp12", peer)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "peer layout loaded", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    let cells = |app: &common::App, label: &str| -> Vec<String> {
        app.query_all_by_role_and_label(egui::Role::TextInput, label)
            .filter_map(|n| n.value())
            .collect()
    };
    common::wait_until(&mut app, "peer record", |a| {
        cells(a, "Name").iter().any(|v| v == "PEER ROW")
    });
    assert!(!app
        .get_by_label("New Record")
        .accesskit_node()
        .is_disabled());
    let run = |app: &mut common::App, script: &str, expected: &str| {
        common::open_menu(app, "Scripts");
        common::click(app, script);
        common::wait_for_label(app, expected);
        common::click(app, "OK");
        common::wait_until(app, "script ended", |a| {
            !a.state().script_running() && a.state().is_settled()
        });
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
    };
    // A record holding a container is duplicated on the owner: the copy's
    // reference is counted in the owning file's library, not the root's
    // (#43).
    run(&mut app, "Duplicate peer", "0|Root|Notes|3|3|3");
    let adjusted = host
        .captures
        .with_opcode(om_protocol::Opcode::AdjustReferenceCount);
    assert_eq!(adjusted.len(), 1, "{adjusted:?}");
    assert!(adjusted.iter().all(|(file, _)| file == "Related.fmp12"));

    // New Record: the peer's id after its three records, its auto-entry
    // and file name, shown as the window's last record.
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "new peer record", |a| {
        a.state().is_settled() && common::record_number(a) == "4"
    });
    common::wait_until(&mut app, "owning auto-entry", |a| {
        cells(a, "Entered").iter().any(|v| v == "|Related")
    });
    run(&mut app, "New peer", "0|Root|Notes|5|5|5");
    // Duplicate Record copies the current record's scalar values; the
    // owner recomputes its auto-entry.
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Duplicate Record");
    common::wait_until(&mut app, "duplicated peer record", |a| {
        a.state().is_settled() && common::record_number(a) == "6"
    });
    run(&mut app, "Duplicate peer", "0|Root|Notes|7|7|7");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    // Root's own layout still creates with its own rules and ids, held
    // until saved.
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "root layout", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "root record", |a| {
        a.state().is_settled() && cells(a, "Name").iter().any(|v| v == "Write tests")
    });
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "new root record", |a| {
        a.state().is_settled() && common::record_number(a) == "3"
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Held in the window's record session until it is saved (#892).
    common::save_record(&mut app);
    common::wait_until(&mut app, "root record saved", |a| {
        a.state().error().is_some() || (a.state().is_settled() && !common::editing(a))
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let stored = |file: &str| {
        let page = host.open(file).records(1, 0, 10).unwrap();
        let column = |name: &str| page.fields.iter().position(|f| f.name == name);
        let (name, entered) = (column("Name").unwrap(), column("Entered"));
        page.records
            .iter()
            .map(|r| {
                (
                    r.id,
                    r.values[name].clone().unwrap_or_default(),
                    entered
                        .and_then(|i| r.values[i].clone())
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
    };
    let text = |id, name: &str, entered: &str| (id, name.to_owned(), entered.to_owned());
    assert_eq!(
        stored("Related.fmp12"),
        [
            text(1, "PEER ROW", ""),
            text(2, "Ship", ""),
            // Its auto-entered Entered entered again on the copy, not
            // copied (#473).
            text(3, "PEER ROW", "PEER ROW|Related"),
            text(4, "", "|Related"),
            text(5, "Scripted", "Scripted|Related"),
            text(6, "Scripted", "Scripted|Related"),
            text(7, "Scripted", "Scripted|Related"),
        ]
    );
    // The copy of record 1 holds its container, the same library member.
    let photos = {
        let page = host.open("Related.fmp12").records(1, 0, 10).unwrap();
        let photo = page.fields.iter().position(|f| f.name == "Photo").unwrap();
        page.records
            .iter()
            .map(|r| (r.id, r.values[photo].clone().unwrap_or_default()))
            .collect::<Vec<_>>()
    };
    assert!(!photos[0].1.is_empty(), "{photos:?}");
    assert_eq!((photos[2].0, &photos[2].1), (3, &photos[0].1), "{photos:?}");
    assert_eq!(
        stored("Root.fmp12"),
        [
            text(1, "Write tests", ""),
            text(2, "Ship", ""),
            text(3, "", "")
        ]
    );
    let created: Vec<_> = host
        .captures
        .with_opcode(om_protocol::Opcode::HostCreateRecord)
        .into_iter()
        .map(|(file, _)| file)
        .collect();
    // On the one connection Root and its related file share (#740), in the
    // order they were sent: the peer's five, then Root's New Record.
    assert_eq!(
        created,
        [
            "Related.fmp12",
            "Related.fmp12",
            "Related.fmp12",
            "Related.fmp12",
            "Related.fmp12",
            "Root.fmp12"
        ]
    );
    // Only the duplicate's copy of the container counted (above).
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::AdjustReferenceCount)
            .len(),
        1
    );

    // Back on the peer's layout, the menu's New Record is held by the
    // related file in the window's record session (#892): nothing is stored
    // there, and Revert releases it with no commit sent.
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "peer layout again", |a| {
        a.state().is_settled() && cells(a, "Name").iter().any(|v| v == "PEER ROW")
    });
    let peer = stored("Related.fmp12");
    let commits = host
        .captures
        .with_opcode(om_protocol::Opcode::HostCommit)
        .len();
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "held peer record", |a| {
        a.state().is_settled() && common::record_number(a) == "8"
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored("Related.fmp12"), peer);
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the held record released", |a| {
        a.state().is_settled() && common::record_number(a) == "7"
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored("Related.fmp12"), peer);
    let count = |op| host.captures.with_opcode(op).len();
    assert_eq!(count(om_protocol::Opcode::HostCommit), commits);

    // A script's New Record there inside its transaction is held in the
    // related file's part of it (#1006): its id is reserved, and Revert
    // Transaction releases the record, nothing stored and no commit sent.
    let reserved = count(om_protocol::Opcode::HostCreateRecord);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "New in transaction");
    let by = egui_kittest::kittest::By::new().label_contains("|Root|Notes|");
    common::wait_until(&mut app, "the transaction's report", |a| {
        a.query_all(by.clone()).next().is_some()
    });
    let shown = app
        .query_all(by)
        .next()
        .unwrap()
        .accesskit_node()
        .value()
        .unwrap_or_default();
    // No error; the found set as before, on a record that is there: the
    // last one takes the reverted record's place (#307).
    let parts: Vec<&str> = shown.split('|').collect();
    assert!(
        shown.starts_with("0|Root|Notes|") && shown.ends_with("|7|7") && parts[3] != "0",
        "{shown}"
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored("Related.fmp12"), peer);
    assert_eq!(count(om_protocol::Opcode::HostCommit), commits);
    assert_eq!(count(om_protocol::Opcode::HostCreateRecord), reserved + 1);

    // Inside a script's transaction a copy of the record holding the
    // container is refused (3) before its reference is counted: the
    // owner's part of the transaction would hold it, and its cleanup after
    // a refused copy is not settled.
    let adjusted = count(om_protocol::Opcode::AdjustReferenceCount);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Duplicate in transaction");
    let by = egui_kittest::kittest::By::new().label_contains("|Root|Notes|");
    common::wait_until(&mut app, "the duplicate's report", |a| {
        a.query_all(by.clone()).next().is_some()
    });
    let shown = app
        .query_all(by)
        .next()
        .unwrap()
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(shown.starts_with("3|Root|Notes|"), "{shown}");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    assert_eq!(count(om_protocol::Opcode::AdjustReferenceCount), adjusted);
    assert_eq!(stored("Related.fmp12"), peer);
}

#[test]
fn imported_layout_find_menu_and_scripts_keep_the_caller_and_peer_record_identity() {
    use egui_kittest::kittest::{By, Queryable};
    use om_scripts::specs::{self, SpecField, StoredRequest};
    use om_scripts::{flag, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    const OTHER_REMOTE: u64 = REMOTE + 1;
    const CONTEXT: u64 = 91;
    let report = || {
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(1, Some(&fixture::calc(
            "Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( FoundCount ) & \"|\" & Get ( WindowMode )",
        )));
        show
    };
    let script = |id, name, field, criterion| {
        fixture::script(
            id,
            name,
            vec![
                fixture::on(86, true),
                ScriptStep::new(22),
                fixture::set_field_in(REMOTE, field, criterion),
                ScriptStep::new(28),
                report(),
            ],
        )
    };
    let stored_find = |occurrence| {
        let mut step = ScriptStep::new(28);
        step.set_data_leaf(
            0x06,
            Some(specs::encode_requests(&[StoredRequest {
                omit: false,
                criteria: vec![(
                    SpecField {
                        occurrence: Some(occurrence),
                        field: fixture::NAME,
                        repetition: 1,
                    },
                    "==Peer row".into(),
                )],
            }])),
        );
        step.set_flag(flag::RESTORE, true);
        step
    };
    let mut root = fixture::tree(
        &[
            script(1, "Find peer", fixture::NAME, "\"==Peer row\""),
            script(2, "Refuse peer calculation", CONTEXT, "\"1\""),
            script(3, "No peer match", fixture::NAME, "\"==Absent\""),
            fixture::script(4, "Inspect menu find", vec![report()]),
            fixture::script(
                5,
                "Refuse peer relationship",
                vec![
                    fixture::on(86, true),
                    ScriptStep::new(22),
                    fixture::set_field_in(OTHER_REMOTE, fixture::NAME, "\"==Peer row\""),
                    ScriptStep::new(28),
                    report(),
                ],
            ),
            fixture::script(
                6,
                "Refuse stored peer relationship",
                vec![
                    fixture::on(86, true),
                    ScriptStep::new(22),
                    stored_find(OTHER_REMOTE),
                    report(),
                ],
            ),
            fixture::script(
                7,
                "Stored base peer",
                vec![
                    fixture::on(86, true),
                    ScriptStep::new(22),
                    stored_find(TABLE),
                    report(),
                ],
            ),
        ],
        true,
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    related::reference(
        &mut root,
        (OTHER_REMOTE, "Other Remote Tasks"),
        1,
        "Related",
    );
    fixture::add_relationship(
        &mut root,
        99,
        (REMOTE, fixture::NAME),
        1,
        (OTHER_REMOTE, fixture::NAME),
    );
    // Layout definition for the imported occurrence, using its serial alias.
    let alias = om_hbam::serial::decode(&uint_segment(REMOTE)).unwrap();
    let mut definition = vec![0x10];
    let mut value = alias;
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        definition.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            break;
        }
    }
    root.put(&[&[4], &[5], &[2], &[2]], OwnedNode::leaf([], definition));
    let mut peer = fixture::tree(&[], true);
    for (record, name) in [(1, "Peer row"), (2, "Write tests")] {
        peer.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &[record],
                &uint_segment(fixture::NAME),
            ],
            om_scripts::text_node(&[], name),
        );
    }
    fixture::add_field(
        &mut peer,
        TABLE,
        CONTEXT,
        "Found context",
        2,
        1,
        0,
        0x2_0020,
        0,
    );
    fixture::add_formula(
        &mut peer,
        &fixture::names(),
        (TABLE, fixture::OCCURRENCE),
        CONTEXT,
        5,
        "Get ( FoundCount )",
    );
    let host = related::Host::new(
        "gui-imported-find",
        vec![("Root.fmp12", root), ("Related.fmp12", peer)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "imported layout", |a| a.state().is_settled());
    common::click(&mut app, "Table View");
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::fill(&mut app, "Name", "==Write tests");
    common::open_menu(&mut app, "Requests");
    // Select the compact menu item, distinct from the tall toolbar button.
    app.get_all_by_label("Perform Find")
        .find(|n| n.rect().height() < 30.0)
        .unwrap()
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "peer menu find", |a| {
        a.state().is_settled() && a.state().mode() == om_gui::Mode::Browse
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(common::record_number(&app), "1");
    assert!(app
        .query_all(By::new().value("Write tests"))
        .next()
        .is_some());
    for (script, expected) in [
        ("Inspect menu find", "0|Root|Notes|2|1|0"),
        ("Find peer", "0|Root|Notes|1|1|0"),
        // In Find mode these selectors describe the request. Cancel below
        // must restore the previous second Browse record and both peers.
        ("Refuse peer calculation", "3|Root|Notes|0|1|1"),
        ("Refuse peer relationship", "3|Root|Notes|0|1|1"),
        ("Refuse stored peer relationship", "3|Root|Notes|0|1|1"),
        ("Stored base peer", "0|Root|Notes|1|1|0"),
        ("No peer match", "401|Root|Notes|0|0|0"),
    ] {
        let refuses = script.starts_with("Refuse ");
        if refuses {
            common::click(&mut app, "Show All");
            common::wait_until(&mut app, "all peers before refusal", |a| {
                a.state().is_settled()
            });
            if common::record_number(&app) != "2" {
                common::click(&mut app, "Next");
            }
            common::wait_until(&mut app, "second peer current", |a| {
                common::record_number(a) == "2" && a.state().is_settled()
            });
        }
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, script);
        common::wait_until(&mut app, "find context report", |a| {
            a.query(By::new().label_contains("|Root|")).is_some()
                || a.state().error().is_some()
                || !a.state().script_running()
        });
        let labels: Vec<_> = app
            .query_all(By::new().role(egui::Role::Label))
            .filter_map(|n| n.value())
            .collect();
        assert!(
            app.query(By::new().label(expected)).is_some(),
            "{script}: expected {expected:?}, error {:?}, labels {labels:?}",
            app.state().error()
        );
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "find script ended", |a| {
            !a.state().script_running() && a.state().is_settled()
        });
        if refuses {
            assert_eq!(app.state().mode(), om_gui::Mode::Find);
            common::click(&mut app, "Cancel Find");
            assert_eq!(common::record_number(&app), "2");
            common::open_menu(&mut app, "Scripts");
            common::click(&mut app, "Inspect menu find");
            common::wait_for_label(&mut app, "0|Root|Notes|2|2|0");
            common::click(&mut app, "OK");
            common::wait_until(&mut app, "refusal context restored", |a| {
                !a.state().script_running() && a.state().is_settled()
            });
        }
        assert_eq!(app.state().mode(), om_gui::Mode::Browse);
    }
    common::click(&mut app, "Show All");
    common::wait_until(&mut app, "all peer records", |a| a.state().is_settled());
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "root layout", |a| a.state().is_settled());
    assert_eq!(common::record_number(&app), "1");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn imported_layout_sort_script_and_menu_unsort_keep_root_context_and_current_record_rules() {
    use om_scripts::specs::{SortEntry, SortSpec, SpecField};
    use om_scripts::{flag, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    const OTHER_REMOTE: u64 = 0xd0_0041;
    let report = || {
        let mut show = ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        show.set_calculation(1, Some(&fixture::calc(
            "Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( LayoutName ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( RecordNumber ) & \"|\" & Get ( FoundCount ) & \"|\" & Get ( SortState )",
        )));
        show
    };
    let sort = |occurrence, field| {
        let spec = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(occurrence),
                    field,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        };
        let mut step = ScriptStep::new(39);
        step.flags |= flag::RESTORE | flag::EXTENDED_DATA | flag::NO_INTERACT;
        step.set_data_leaf(0x0a, Some(OwnedNode::leaf([0x0a], spec.encode())));
        step
    };
    let mut root = fixture::tree(
        &[
            fixture::script(1, "Sort peer", vec![sort(REMOTE, fixture::COUNT), report()]),
            fixture::script(
                2,
                "Refuse other occurrence",
                vec![
                    fixture::on(86, true),
                    sort(OTHER_REMOTE, fixture::NAME),
                    report(),
                ],
            ),
            fixture::script(3, "Inspect sort", vec![report()]),
            fixture::script(
                4,
                "Sort root",
                vec![sort(fixture::OCCURRENCE, fixture::NAME), report()],
            ),
        ],
        true,
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    related::reference(
        &mut root,
        (OTHER_REMOTE, "Other Remote Tasks"),
        1,
        "Related",
    );
    let mut alias = om_hbam::serial::decode(&uint_segment(REMOTE)).unwrap();
    let mut definition = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        definition.push(if alias == 0 { byte } else { byte | 0x80 });
        if alias == 0 {
            break;
        }
    }
    root.put(&[&[4], &[5], &[2], &[2]], OwnedNode::leaf([], definition));
    let mut peer = fixture::tree(&[], true);
    for (id, count) in [(1, "10"), (2, "2")] {
        peer.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &[id],
                &uint_segment(fixture::COUNT),
            ],
            om_scripts::text_node(&[], count),
        );
    }
    let host = related::Host::new(
        "gui-imported-sort",
        vec![("Root.fmp12", root), ("Related.fmp12", peer)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::select_layout(&mut app, "Notes");
    common::wait_until(&mut app, "imported layout", |a| a.state().is_settled());
    for (script, expected) in [
        ("Sort peer", "0|Root|Notes|2|1|2|1"),
        ("Refuse other occurrence", "3|Root|Notes|2|1|2|1"),
        ("Inspect sort", "0|Root|Notes|1|1|2|0"),
        ("Sort root", "0|Root|Tasks|2|1|2|1"),
    ] {
        if script == "Inspect sort" {
            common::open_menu(&mut app, "Records");
            common::click(&mut app, "Unsort");
            common::wait_until(&mut app, "menu unsort", |a| a.state().is_settled());
        }
        if script == "Sort root" {
            common::select_layout(&mut app, "Tasks");
            common::wait_until(&mut app, "root layout", |a| a.state().is_settled());
        }
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, script);
        common::wait_for_label(&mut app, expected);
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "sort script ended", |a| {
            !a.state().script_running() && a.state().is_settled()
        });
        assert_eq!(common::record_number(&app), "1");
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
    }
}

/// A related file that refuses the window's account (#214): the window
/// asks for another account once a layout needs the file, not at open, as
/// FileMaker Pro does (reported by a FileMaker Pro 26 run on a local pair).
/// Cancel leaves `<File Missing>` in its field and is not asked again in
/// the session; opened again, Sign in with the file's own account sends one
/// Login and the field shows the related record. With `OM_SIGNIN_SHOTS`
/// naming a directory, the dialog and both outcomes are saved there.
#[test]
fn a_related_file_that_refuses_the_account_is_asked_for_when_a_layout_needs_it() {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    // Record 1's Name, which its related record in Related shares.
    const RELATED: &str = "Write tests";
    use om_hbam::serial;
    use om_layout::encode::{self, bytes, double, uint};
    const REMOTE: u64 = 0xd0_0040;
    let shots = std::env::var_os("OM_SIGNIN_SHOTS").map(std::path::PathBuf::from);
    let shot = |app: &mut common::App, name: &str| {
        if let Some(dir) = &shots {
            app.render().unwrap().save(dir.join(name)).unwrap();
        }
    };
    // Root: Tasks related to Remote Tasks, the Tasks of Related, by Name;
    // layout 1 a Form layout whose only object is Remote Tasks::Name.
    let mut root = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        9,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (REMOTE, fixture::NAME),
    );
    let layout = uint_segment(fixture::LAYOUT);
    let alias = |occurrence: u64| serial::decode(&uint_segment(occurrence)).unwrap();
    let mut def = Vec::new();
    uint(2, alias(fixture::OCCURRENCE), &mut def);
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
    let frame = om_layout::Rect {
        top: 20.0,
        left: 110.0,
        bottom: 42.0,
        right: 230.0,
        other: Vec::new(),
    };
    let mut store = Vec::new();
    uint(1, 1, &mut store);
    bytes(
        2,
        &encode::field_object(1, &frame, alias(REMOTE), fixture::NAME),
        &mut store,
    );
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    // Related's only account is another one.
    let mut child = fixture::tree(&[], true);
    child.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    let host = related::Host::new(
        "gui-related-sign-in",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let logins = || {
        host.captures
            .opens_and_logins()
            .1
            .get("Related.fmp12")
            .copied()
            .unwrap_or(0)
    };
    // Whether some object shows `text`.
    let shows = |app: &common::App, text: &str| {
        app.query_all(egui_kittest::kittest::By::new())
            .any(|node| node.accesskit_node().value().as_deref() == Some(text))
    };
    let open = |app: &mut common::App| {
        hosted::replace(
            app,
            "Path",
            &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
        );
        hosted::replace(app, "Account Name", ACCOUNT);
        hosted::replace(app, "Password", PASSWORD);
        common::click(app, "Open");
    };
    const TEXT: &str = "Sign in to open “Related”.";

    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    open(&mut app);
    common::wait_for_label(&mut app, TEXT);
    assert_eq!(logins(), 1, "the refused Login, at open");
    shot(&mut app, "related-sign-in-dialog.png");
    common::click(&mut app, "Cancel");
    common::wait_until(&mut app, "<File Missing>", |app| {
        shows(app, "<File Missing>")
    });
    shot(&mut app, "related-sign-in-cancelled.png");
    // Not asked again in the session.
    app.run_steps(10);
    assert!(app.query_by_label(TEXT).is_none());
    assert_eq!(logins(), 1);
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the close", |app| !app.state().is_open());

    // Opened again, a new session: asked again, and Related's own account
    // opens it with one Login.
    open(&mut app);
    common::wait_for_label(&mut app, TEXT);
    assert_eq!(logins(), 2);
    hosted::replace(&mut app, "Account Name", "Other account");
    hosted::replace(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Sign in");
    common::wait_until(&mut app, "the related record", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label(TEXT).is_none() && shows(app, RELATED) && !shows(app, "<File Missing>")
    });
    assert_eq!(logins(), 3);
    shot(&mut app, "related-sign-in-opened.png");
}

/// Go to Related Record into the layouts of a related file that refused
/// the window's account (the #999 limit): the step asks for an account,
/// as Open File does. Cancel gives 1 and leaves Root's window; the file's
/// own account signs the relationship in, and the step opens the file's
/// window with that account too, holding the relationship's open of it
/// (#1011): one prompt and one Login for both.
#[test]
fn go_to_related_record_into_a_file_that_refused_the_account_asks_for_another() {
    use om_scripts::StoredKey;
    use om_scripts::{flag, ScriptStep};
    const REMOTE: u64 = 0xd0_0040;
    let mut related = ScriptStep::new(74);
    related.value = 5;
    related.flags |= flag::OPTION | flag::RESTORE;
    related.set_member_key(StoredKey::from_id(1));
    related.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf([0x11], om_formula::Key::from_id(REMOTE).0)),
    );
    let mut show = ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&fixture::calc(
            "\"Error \" & Get ( LastError ) & \"|\" & Get ( FileName )",
        )),
    );
    let scripts = [fixture::script(
        1,
        "Go Related",
        vec![fixture::on(86, true), related, show],
    )];
    let mut root = fixture::tree(&scripts, true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        9,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (REMOTE, fixture::NAME),
    );
    let mut child = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    child.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    let host = related::Host::new(
        "gui-related-sign-in",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let logins = || {
        host.captures
            .opens_and_logins()
            .1
            .get("Related.fmp12")
            .copied()
            .unwrap_or(0)
    };
    const TEXT: &str = "Sign in to open “Related”.";
    let run = |app: &mut common::App| {
        common::open_menu(app, "Scripts");
        common::click(app, "Go Related");
        common::wait_for_label(app, TEXT);
    };
    let result = |app: &mut common::App, shown: &str| {
        common::wait_for_label(app, shown);
        common::click(app, "OK");
        common::wait_until(app, "the script end", |app| !app.state().script_running());
    };

    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the root file", |app| app.state().is_settled());

    // Cancel: 1, in Root's window; Related refused the window's account,
    // its only Login.
    run(&mut app);
    common::click(&mut app, "Cancel");
    result(&mut app, "Error 1|Root");
    assert_eq!(logins(), 1);
    // Its own account: the relationship and the file's window, one Login.
    run(&mut app);
    hosted::replace(&mut app, "Account Name", "Other account");
    hosted::replace(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Sign in");
    result(&mut app, "Error 0|Related");
    assert_eq!(logins(), 2);
    let titles = app.state_mut().window_titles();
    assert!(titles.iter().any(|t| t.contains("Related")), "{titles:?}");
}

/// A script's Open File of a related file that refused the window's
/// account asks for another, as FileMaker Pro's does (#214's FileMaker
/// Pro run): Cancel gives 1; a wrong password asks again, and Cancel then
/// gives 212; the file's own account opens it (0) in a window of its own.
/// Each answer is one Login, and the sign-in also opens the file for the
/// first file's relationships (one more Login), which the script then
/// reads through.
#[test]
fn open_file_of_a_related_file_that_refuses_the_account_asks_for_another() {
    use egui_kittest::kittest::Queryable as _;
    use om_scripts::ScriptStep;
    const REMOTE: u64 = 0xd0_0040;
    // The step's error, and after a success Related's record read through
    // the first file's relationship: the sign-in reached the scripts'
    // schema too.
    let mut names = fixture::photo_names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    let mut cx = om_formula::Context::new(&names);
    cx.occurrence = Some(fixture::OCCURRENCE);
    let compiled = om_formula::compile(
        "\"Error \" & Get ( LastError ) & If ( Get ( LastError ) = 0 ; \"|\" & Remote Tasks::Name ; \"\" )",
        &cx,
    )
    .unwrap();
    let mut show = ScriptStep::new(87);
    show.set_calculation(
        1,
        Some(&om_scripts::Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )),
    );
    show.set_text(0, Some("OK"));
    let mut open = ScriptStep::new(33);
    open.set_calculation(0, Some(&fixture::calc("\"Related\"")));
    let scripts = [fixture::script(
        1,
        "Open Related",
        vec![fixture::on(86, true), open, show],
    )];
    let mut root = fixture::tree(&scripts, true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        9,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (REMOTE, fixture::NAME),
    );
    // A script, so a window of it finds the scripts catalog it reads.
    let mut child = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    child.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    let host = related::Host::new(
        "gui-open-file-sign-in",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let logins = || {
        host.captures
            .opens_and_logins()
            .1
            .get("Related.fmp12")
            .copied()
            .unwrap_or(0)
    };
    const TEXT: &str = "Sign in to open “Related”.";
    let run = |app: &mut common::App| {
        common::open_menu(app, "Scripts");
        common::click(app, "Open Related");
        common::wait_for_label(app, TEXT);
    };
    let result = |app: &mut common::App, code: &str| {
        let label = format!("Error {code}");
        common::wait_for_label(app, &label);
        common::click(app, "OK");
        common::wait_until(app, "the script end", |app| !app.state().script_running());
    };

    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the root file", |app| app.state().is_settled());
    common::wait_until(&mut app, "the related files", |_| logins() == 1);

    // Cancel: 1, and no Login.
    run(&mut app);
    common::click(&mut app, "Cancel");
    result(&mut app, "1");
    assert_eq!(logins(), 1);
    // A wrong password asks again; Cancel then gives its 212.
    run(&mut app);
    hosted::replace(&mut app, "Account Name", "Other account");
    hosted::replace(&mut app, "Password", "wrong");
    common::click(&mut app, "Sign in");
    common::wait_until(&mut app, "the refusal", |app| {
        app.query_all(egui_kittest::kittest::By::new().label_contains("did not accept"))
            .next()
            .is_some()
            || app
                .query_all(egui_kittest::kittest::By::new().label_contains("cannot be used"))
                .next()
                .is_some()
            || logins() == 2 && app.query_by_label(TEXT).is_some()
    });
    assert_eq!(logins(), 2);
    common::click(&mut app, "Cancel");
    result(&mut app, "212");
    assert_eq!(logins(), 2);
    // Its own account: 0 and a window of its own, with one Login: the
    // first file's relationships hold the window's open of the file, on the
    // same connection with the same account, with no Login of their own
    // (#1011).
    run(&mut app);
    hosted::replace(&mut app, "Account Name", "Other account");
    hosted::replace(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Sign in");
    result(&mut app, "0|Write tests");
    assert_eq!(logins(), 3);
    let titles = app.state_mut().window_titles();
    assert!(titles.iter().any(|t| t.contains("Related")), "{titles:?}");
}

/// A save of Root's record with a related record of another file that is
/// partly applied (#1006): Root's op70 is refused after Related's committed.
/// The window shows what each file did, keeps its change, and takes no
/// other: typing into its frozen fields changes nothing. Check Again reads
/// Root's record back as before its commit (not saved), and Resume Save
/// sends Root's work only, after which the bar is gone and both files hold
/// the values. Revert Record discards a second such save, and its report
/// and freeze go.
#[test]
fn a_partly_saved_record_is_checked_and_resumed_from_its_bar() {
    use egui_kittest::kittest::{NodeT, Queryable};
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_data::DataProvider;
    use om_scripts::ScriptStep;
    const REMOTE: u64 = 0xd0_0040;
    const JOB: u64 = 30;
    let script = fixture::script(
        1,
        "Write both",
        vec![
            fixture::on(86, true),
            fixture::set_field_in(REMOTE, NAME, "\"Made\""),
            fixture::set_field_in(OCCURRENCE, NAME, "\"Root edit\""),
            ScriptStep::new(75),
        ],
    );
    let again = fixture::script(
        2,
        "Write both again",
        vec![
            fixture::on(86, true),
            fixture::set_field_in(REMOTE, NAME, "\"Made again\""),
            fixture::set_field_in(OCCURRENCE, NAME, "\"Root again\""),
            ScriptStep::new(75),
        ],
    );
    let mut root = fixture::tree(&[script, again], true);
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &om_hbam::serial::encode(1),
            &uint_segment(COUNT),
        ],
        om_scripts::text_node(&[], "9"),
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (REMOTE, JOB), 0, (OCCURRENCE, COUNT));
    let unsorted = om_scripts::specs::SortSpec {
        keep_sorted: false,
        blanks_last: false,
        fields: Vec::new(),
    };
    let definition = [
        related::key(REMOTE),
        related::key(OCCURRENCE),
        unsorted.encode(),
        unsorted.encode(),
        0x20u32.to_be_bytes().to_vec(),
    ]
    .concat();
    root.put(
        &[&[3], &[0xfb], &[5], &[9], &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    let mut related_file = fixture::tree(&[], true);
    fixture::add_field(&mut related_file, TABLE, JOB, "Job", 1, 2, 0, 0, 0);
    let host = related::Host::new(
        "gui-partly-saved",
        vec![("Root.fmp12", root), ("Related.fmp12", related_file)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    // The fixture's layout places no field; Table View shows Name.
    common::click(&mut app, "Table View");

    let barrier = host.block_saves(0);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Write both");
    // Commit Records fails (Root's commit was refused): its alert is
    // answered and the script ends.
    common::wait_until(&mut app, "the commit's alert", |a| {
        a.state().error().is_some()
    });
    let answer = if app.query_by_label("Continue").is_some() {
        "Continue"
    } else {
        "OK"
    };
    common::click(&mut app, answer);
    // Frozen from the frame the script ends (the worker says what it left
    // with its end), before the provider's capabilities arrive: no field
    // takes typing that settling the save would drop.
    let name_editable = |app: &common::App| {
        app.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .next()
            .is_some()
    };
    common::wait_until(&mut app, "the script's end", |a| {
        let ended = !a.state().script_running();
        assert!(!ended || !name_editable(a), "editable as the script ends");
        ended
    });
    common::wait_for_label(
        &mut app,
        "This record was saved in some of its files but not all.",
    );
    app.get_by_label("Related: saved.");
    app.get_by_label("Root: not known whether it was saved.");
    drop(barrier);

    // The window keeps showing its change and takes no other while the
    // save is unsettled: its cells are drawn read-only, so typing into one
    // changes nothing that Resume Save would write, and clicking the
    // record does not try to save it.
    let editable = |app: &common::App| {
        app.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .next()
            .is_some()
    };
    common::wait_for_label(&mut app, "Root edit");
    assert!(!editable(&app), "frozen");
    common::click(&mut app, "Root edit");
    app.run_steps(2);
    app.event(egui::Event::Text("Later".into()));
    app.run_steps(3);
    app.get_by_label("Root edit");
    assert!(!editable(&app), "frozen");
    // A click on the record saves nothing while the save is unsettled: no
    // refused save, no alert.
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    // What the bar says of Root.
    let root = |app: &common::App| {
        [
            "saved.",
            "not saved yet.",
            "not known whether it was saved.",
            "not saved (checked).",
            "cannot be checked.",
        ]
        .into_iter()
        .find(|state| app.query_by_label(&format!("Root: {state}")).is_some())
    };
    let enabled = |app: &common::App, label: &str| {
        app.query_by_label(label)
            .is_some_and(|button| !button.accesskit_node().is_disabled())
    };
    common::wait_until(&mut app, "Check Again enabled", |a| {
        enabled(a, "Check Again")
    });
    common::click(&mut app, "Check Again");
    common::wait_until(&mut app, "Root checked", |a| {
        root(a) != Some("not known whether it was saved.")
    });
    assert_eq!(root(&app), Some("not saved (checked)."));
    common::wait_until(&mut app, "Resume Save enabled", |a| {
        enabled(a, "Resume Save")
    });
    common::click(&mut app, "Resume Save");
    common::wait_until(&mut app, "the save settled", |a| {
        a.query_by_label("Related: saved.").is_none() && a.state().is_settled()
    });
    common::wait_until(&mut app, "the saved record editable again", |a| {
        a.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .any(|field| field.value().as_deref() == Some("Root edit"))
    });
    let names = |file: &str| -> Vec<String> {
        let mut provider = host.open(file);
        let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let at = page.fields.iter().position(|f| f.name == "Name").unwrap();
        let names = page
            .records
            .iter()
            .map(|record| record.values[at].clone().unwrap_or_default())
            .collect();
        provider.close();
        names
    };
    assert!(names("Root.fmp12").contains(&"Root edit".to_owned()));
    assert!(names("Related.fmp12").contains(&"Made".to_owned()));

    // Partly saved again, then Revert Record: the provider discards the
    // save, and the window's report and freeze go with it.
    let barrier = host.block_saves(0);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Write both again");
    common::wait_until(&mut app, "the second commit's alert", |a| {
        a.state().error().is_some()
    });
    let answer = if app.query_by_label("Continue").is_some() {
        "Continue"
    } else {
        "OK"
    };
    common::click(&mut app, answer);
    common::wait_for_label(&mut app, "Root: not known whether it was saved.");
    drop(barrier);
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the reverted save's bar gone", |a| {
        a.query_by_label("Root: not known whether it was saved.")
            .is_none()
            && a.state().is_settled()
    });
    common::wait_until(&mut app, "the reverted record editable", |a| {
        a.query_all_by_role_and_label(egui::Role::TextInput, "Name")
            .any(|field| field.value().as_deref() == Some("Root edit"))
    });
}

/// Set Field into another file's related field from a layout of Root's own
/// table (#1000). The relationship is drawn from that file's occurrence and
/// creates its records from Root's (0x20), on number keys of different
/// field ids. The first commit creates the related record in that file
/// with Root's key, and the next edits it. A commit holding records of two
/// other files saves each, one after the other (#1006).
#[test]
fn set_field_creates_and_edits_another_files_related_record() {
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_data::DataProvider;
    use om_scripts::ScriptStep;
    const REMOTE: u64 = 0xd0_0040;
    const OTHER: u64 = 0xd0_0041;
    /// The related files' key, a number field.
    const JOB: u64 = 30;
    let mut show = ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&fixture::calc("\"result:\" & $a & \"|\" & $b & \"|\" & $c")),
    );
    let script = fixture::script(
        1,
        "Write related",
        vec![
            fixture::on(86, true),
            fixture::set_field_in(REMOTE, NAME, "\"Made\""),
            ScriptStep::new(75),
            fixture::set_variable("$a", "Get ( LastError )"),
            fixture::set_field_in(REMOTE, NAME, "\"Edited\""),
            ScriptStep::new(75),
            fixture::set_variable("$b", "Get ( LastError )"),
            fixture::set_field_in(REMOTE, NAME, "\"Both\""),
            fixture::set_field_in(OTHER, NAME, "\"Both\""),
            ScriptStep::new(75),
            fixture::set_variable("$c", "Get ( LastError )"),
            show,
        ],
    );
    let mut root = fixture::tree(&[script], true);
    // Root's record 1 keys no record of either file.
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &om_hbam::serial::encode(1),
            &uint_segment(COUNT),
        ],
        om_scripts::text_node(&[], "9"),
    );
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    related::reference(&mut root, (OTHER, "Other Tasks"), 2, "Other");
    let relate =
        |root: &mut OwnedNode, id: u8, left: (u64, u64), right: (u64, u64), options: u32| {
            fixture::add_relationship(root, id, left, 0, right);
            let unsorted = om_scripts::specs::SortSpec {
                keep_sorted: false,
                blanks_last: false,
                fields: Vec::new(),
            };
            let definition = [
                related::key(left.0),
                related::key(right.0),
                unsorted.encode(),
                unsorted.encode(),
                options.to_be_bytes().to_vec(),
            ]
            .concat();
            root.put(
                &[&[3], &[0xfb], &[5], &[id], &[2]],
                OwnedNode::leaf(Vec::new(), definition),
            );
        };
    relate(&mut root, 9, (REMOTE, JOB), (OCCURRENCE, COUNT), 0x20);
    relate(&mut root, 10, (OCCURRENCE, COUNT), (OTHER, JOB), 0x10);
    let keyed = || {
        let mut file = fixture::tree(&[], true);
        fixture::add_field(&mut file, TABLE, JOB, "Job", 1, 2, 0, 0, 0);
        file
    };
    let host = related::Host::new(
        "gui-related-owner-writes",
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", keyed()),
            ("Other.fmp12", keyed()),
        ],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root loaded", |a| a.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Write related");
    common::wait_for_label(&mut app, "result:0|0|0");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });

    let rows = |file: &str| -> Vec<(String, String)> {
        let mut provider = host.open(file);
        let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
        let rows = page
            .records
            .iter()
            .map(|record| {
                let text = |name| record.values[at(name)].clone().unwrap_or_default();
                (text("Name"), text("Job"))
            })
            .collect();
        provider.close();
        rows
    };
    let row = |name: &str, job: &str| (name.to_owned(), job.to_owned());
    assert_eq!(
        rows("Related.fmp12"),
        [row("Write tests", ""), row("Ship", ""), row("Both", "9")]
    );
    assert_eq!(
        rows("Other.fmp12"),
        [row("Write tests", ""), row("Ship", ""), row("Both", "9")]
    );
}

/// The three-file portal save acceptance fixture (#1006, #892): files Root,
/// Related and Other, each a synthetic Tasks file. Root names Related and
/// Other by a relative path, so all three are on one host, or with `peer`
/// (a stored `fmnet` location: `host:port` between two
/// [`om_data::file_refs::LOCATION_SEPARATOR`]s, as FileMaker stores
/// `fmnet:/host:port/`) by that `fmnet` path, so a second server hosts
/// those two.
///
/// - Root's layout "Lines" (on Tasks) shows Tasks::Name and a portal of
///   Related's Tasks (occurrence "Remote Tasks", relationship 9 on Related's
///   Job = Root's Count, creating Related's records), its rows showing
///   Remote Tasks::Name.
/// - That field's OnObjectSave runs Root's script "Log the line": Set Field
///   [Other Tasks::Name; "saved " & the active row and field contents],
///   through relationship 10 (Root's Count = Other's Job, creating Other's
///   records). The window's commit then writes the row in Related and the
///   log record in Other.
/// - Root's record 1 has Count 9; Related's records 1 "Write tests" and 2
///   "Ship" have Job 9 (the portal's two rows); Other has no record of Job
///   9.
///
/// With `refusal`, the variant whose save is refused before any record
/// data goes and then corrected:
/// - Other's Count is validated Not empty, Always (`0x408`). Other's
///   records 1 and 2 (Counts 3 and 5) meet it; the new log record, which
///   holds no Count, does not, so the window's commit is refused.
/// - Lines also shows a portal of Other Tasks (relationship 10) with Other
///   Tasks::Name and Count, where the held log record shows as a begun row
///   whose Count the user can fill in before saving again. That portal
///   makes Lines read Other when it is shown, so Other opens then, not
///   first when the log is written.
fn portal_save_log_files(
    peer: Option<&str>,
    refusal: bool,
    prepared: bool,
) -> Vec<(&'static str, OwnedNode)> {
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_hbam::serial;
    const REMOTE: u64 = 0xd0_0040;
    const OTHER: u64 = 0xd0_0041;
    const JOB: u64 = 30;
    const LINES: u64 = 3;
    const PORTAL: u64 = 1;
    const LINE_NAME: u64 = 2;
    const TASK_NAME: u64 = 3;
    const LOG_PORTAL: u64 = 4;
    const LOG_NAME: u64 = 5;
    const LOG_COUNT: u64 = 6;
    const LOG: u64 = 1;
    const DERIVED: u64 = 31;
    const LINE_DERIVED: u64 = 7;
    const NOTE: u64 = 32;
    const LINE_NOTE: u64 = 8;

    // The layout object store's protobuf (layout-model.md 5).
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
        varint(u64::from(number) << 3, out);
        varint(v, out);
    }
    fn double(number: u32, v: f64, out: &mut Vec<u8>) {
        varint(u64::from(number) << 3 | 1, out);
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
        varint(u64::from(number) << 3 | 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn rect(top: f64, left: f64, bottom: f64, right: f64) -> Vec<u8> {
        let mut out = Vec::new();
        for (number, v) in [(1, top), (2, left), (3, bottom), (4, right)] {
            double(number, v, &mut out);
        }
        out
    }
    fn alias(occurrence: u64) -> u64 {
        serial::decode(&uint_segment(occurrence)).expect("a serial key")
    }
    fn field_object(id: u64, occurrence: u64, field: u64, bounds: Vec<u8>) -> Vec<u8> {
        let mut spec = Vec::new();
        uint(1, alias(occurrence), &mut spec);
        uint(2, field, &mut spec);
        let mut field_obj = Vec::new();
        bytes(2, &spec, &mut field_obj);
        let mut obj = Vec::new();
        uint(1, 1, &mut obj);
        uint(2, id, &mut obj);
        bytes(4, &bounds, &mut obj);
        bytes(8, &field_obj, &mut obj);
        obj
    }

    let log = fixture::script(
        LOG,
        "Log the line",
        vec![fixture::set_field_in(
            OTHER,
            NAME,
            "\"saved \" & Get ( ActivePortalRowNumber ) & \":\" & Get ( ActiveFieldContents )",
        )],
    );
    let mut root = fixture::tree(&[log], true);
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(1),
            &uint_segment(COUNT),
        ],
        om_scripts::text_node(&[], "9"),
    );
    for (occurrence, source, file) in [
        ((REMOTE, "Remote Tasks"), 1, "Related"),
        ((OTHER, "Other Tasks"), 2, "Other"),
    ] {
        let path = match peer {
            Some(location) => ("fmnet", location, file),
            None => ("file", "", file),
        };
        related::reference_with(&mut root, occurrence, source, file, &[path]);
    }
    // Relationships allowing creation of the left (0x20) or right (0x10)
    // side's records (file-metadata.md 4.1).
    let relate =
        |root: &mut OwnedNode, id: u8, left: (u64, u64), right: (u64, u64), options: u32| {
            fixture::add_relationship(root, id, left, 0, right);
            let unsorted = om_scripts::specs::SortSpec {
                keep_sorted: false,
                blanks_last: false,
                fields: Vec::new(),
            };
            let definition = [
                related::key(left.0),
                related::key(right.0),
                unsorted.encode(),
                unsorted.encode(),
                options.to_be_bytes().to_vec(),
            ]
            .concat();
            root.put(
                &[&[3], &[0xfb], &[5], &[id], &[2]],
                OwnedNode::leaf(Vec::new(), definition),
            );
        };
    relate(&mut root, 9, (REMOTE, JOB), (OCCURRENCE, COUNT), 0x20);
    relate(&mut root, 10, (OCCURRENCE, COUNT), (OTHER, JOB), 0x10);

    // Layout "Lines": Tasks::Name, and the portal of Remote Tasks.
    let layout = uint_segment(LINES);
    root.put(
        &[&[4], &[5], &layout, &[16]],
        om_scripts::text_node(&[], "Lines"),
    );
    let mut def = Vec::new();
    uint(2, alias(OCCURRENCE), &mut def);
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
    let mut portal_obj = Vec::new();
    uint(2, alias(REMOTE), &mut portal_obj);
    uint(3, 0x101, &mut portal_obj);
    double(6, 3.0, &mut portal_obj);
    let mut portal = Vec::new();
    uint(1, 9, &mut portal);
    uint(2, PORTAL, &mut portal);
    bytes(
        4,
        &rect(60.0, 10.0, 130.0, if prepared { 610.0 } else { 410.0 }),
        &mut portal,
    );
    uint(6, LINE_NAME, &mut portal);
    if prepared {
        uint(6, LINE_DERIVED, &mut portal);
        uint(6, LINE_NOTE, &mut portal);
    }
    bytes(15, &portal_obj, &mut portal);
    let mut store = Vec::new();
    uint(1, PORTAL, &mut store);
    uint(1, TASK_NAME, &mut store);
    if refusal {
        uint(1, LOG_PORTAL, &mut store);
    }
    bytes(2, &portal, &mut store);
    bytes(
        2,
        &field_object(LINE_NAME, REMOTE, NAME, rect(0.0, 0.0, 20.0, 300.0)),
        &mut store,
    );
    bytes(
        2,
        &field_object(TASK_NAME, OCCURRENCE, NAME, rect(20.0, 10.0, 40.0, 210.0)),
        &mut store,
    );
    if prepared {
        bytes(
            2,
            &field_object(LINE_DERIVED, REMOTE, DERIVED, rect(0.0, 305.0, 20.0, 395.0)),
            &mut store,
        );
        bytes(
            2,
            &field_object(LINE_NOTE, REMOTE, NOTE, rect(0.0, 405.0, 20.0, 595.0)),
            &mut store,
        );
    }
    let mut names = vec![
        (PORTAL, "Lines portal"),
        (LINE_NAME, "Line name"),
        (TASK_NAME, "Task name"),
    ];
    if refusal {
        // The portal of Other Tasks, as the first one is built, below it in
        // the body: its rows show the log's Name and Count.
        let mut log_obj = Vec::new();
        uint(2, alias(OTHER), &mut log_obj);
        uint(3, 0x101, &mut log_obj);
        double(6, 3.0, &mut log_obj);
        let mut log_portal = Vec::new();
        uint(1, 9, &mut log_portal);
        uint(2, LOG_PORTAL, &mut log_portal);
        bytes(4, &rect(135.0, 10.0, 195.0, 410.0), &mut log_portal);
        uint(6, LOG_NAME, &mut log_portal);
        uint(6, LOG_COUNT, &mut log_portal);
        bytes(15, &log_obj, &mut log_portal);
        bytes(2, &log_portal, &mut store);
        bytes(
            2,
            &field_object(LOG_NAME, OTHER, NAME, rect(0.0, 0.0, 20.0, 200.0)),
            &mut store,
        );
        bytes(
            2,
            &field_object(LOG_COUNT, OTHER, COUNT, rect(0.0, 210.0, 20.0, 300.0)),
            &mut store,
        );
        names.extend([
            (LOG_PORTAL, "Log portal"),
            (LOG_NAME, "Log name"),
            (LOG_COUNT, "Log count"),
        ]);
    }
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    for (object, name) in names {
        root.put(
            &[&[4], &[5], &layout, &[5], &serial::encode(object), &[16]],
            om_scripts::text_node(&[], name),
        );
    }
    // OnObjectSave of the portal's field runs "Log the line".
    let object = serial::encode(LINE_NAME);
    let cache: [&[u8]; 6] = [&[4], &[5], &layout, &[5], &object, &[0x30]];
    let mut count = cache.to_vec();
    count.push(&[4]);
    root.put(
        &count,
        OwnedNode::leaf(Vec::new(), 1u16.to_be_bytes().to_vec()),
    );
    let event = om_data::runtime::TriggerEvent::ObjectSave.id();
    let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(LOG),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    let event = uint_segment(u64::from(event));
    let mut entry = cache.to_vec();
    entry.extend([&[5][..], &event, &[1]]);
    root.put(&entry, OwnedNode::leaf(Vec::new(), record));

    let keyed = |job: Option<&str>| {
        let mut file = fixture::tree(&[], true);
        fixture::add_field(&mut file, TABLE, JOB, "Job", 1, 2, 0, 0, 0);
        if let Some(job) = job {
            for record in [1, 2] {
                file.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(record),
                        &uint_segment(JOB),
                    ],
                    om_scripts::text_node(&[], job),
                );
            }
        }
        file
    };
    let mut child = keyed(Some("9"));
    if prepared {
        fixture::add_field(&mut child, TABLE, DERIVED, "Derived", 1, 1, 0, 0x88, 0);
        fixture::add_field(&mut child, TABLE, NOTE, "Note", 1, 1, 0, 0, 0);
        // One explicit red run, using rich_text.rs's observed style vector.
        let style = [0, 4, 0, 19, 1, 0, 0, 12, 221, 0, 0, 255, 0];
        let mut runs = Vec::new();
        for value in [1u32, 0, 0, 1, 1, style.len() as u32] {
            runs.extend(value.to_be_bytes());
        }
        runs.extend(style);
        let text = om_scsu::encode_scsu("Keep styling");
        let styled = OwnedNode::list(
            uint_segment(NOTE),
            vec![
                OwnedNode::leaf(
                    vec![0],
                    [(text.len() as u32).to_be_bytes(), 12u32.to_be_bytes()].concat(),
                ),
                OwnedNode::leaf(vec![1], text),
                OwnedNode::list(vec![0xff], vec![OwnedNode::leaf(vec![1], runs)]),
            ],
        );
        child.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &serial::encode(1),
                &uint_segment(NOTE),
            ],
            styled,
        );
        let mut names = fixture::names();
        names.add_field(TABLE, DERIVED, "Derived");
        fixture::add_formula(
            &mut child,
            &names,
            (TABLE, OCCURRENCE),
            DERIVED,
            5,
            "Name & \"!\"",
        );
        child.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &serial::encode(1),
                &uint_segment(DERIVED),
            ],
            om_scripts::text_node(&[], "Write tests!"),
        );
    }
    let mut other = keyed(None);
    if refusal {
        fixture::set_field_validation(&mut other, TABLE, COUNT, 0x408);
        // As the client's record rules read the definition: Not empty, Always.
        let definition = other
            .at(&[&uint_segment(TABLE), &[3], &[5], &uint_segment(COUNT), &[2]])
            .and_then(|node| node.value.as_deref())
            .expect("Count's definition");
        let validation = om_data::field_options::FieldOptions::decode(definition, None, None)
            .expect("a normal field's options")
            .validation;
        assert!(validation.not_empty && validation.always);
    }
    vec![
        ("Root.fmp12", root),
        ("Related.fmp12", child),
        ("Other.fmp12", other),
    ]
}

/// The normal VM obtains the accepted destination before its first op87,
/// and its session/field flags survive passive loss before explicit Save.
#[test]
fn a_scripts_first_stored_copy_reconnects_without_a_root_upload() {
    use egui_kittest::kittest::Queryable;
    use om_data::DataProvider;
    use om_protocol::Opcode;
    use om_scripts::{Calculation, FieldSpec, ScriptStep, StoredKey};
    const SOURCE: u64 = 91;
    const BACKUP: u64 = 92;
    let mut files = portal_save_log_files(None, false, false);
    let root = &mut files[0].1;
    fixture::add_field(root, TABLE, SOURCE, "StoredAttachment", 1, 6, 0, 0, 0);
    fixture::add_field(root, TABLE, BACKUP, "AttachmentBackup", 1, 6, 0, 0, 0);
    let mut names = fixture::names();
    names.add_field(TABLE, SOURCE, "StoredAttachment");
    let mut cx = om_formula::Context::new(&names);
    cx.occurrence = Some(fixture::OCCURRENCE);
    let compiled = om_formula::compile("StoredAttachment", &cx).unwrap();
    let mut copy = ScriptStep::new(76);
    copy.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(fixture::OCCURRENCE).unwrap(),
            StoredKey::from_id(BACKUP).unwrap(),
            0,
        )),
    );
    copy.set_calculation(
        0,
        Some(&Calculation::new(
            None,
            om_formula::polish::encode(&compiled.expr),
        )),
    );
    let mut held = ScriptStep::new(87);
    held.set_text(0, Some("OK"));
    held.set_calculation(
        1,
        Some(&fixture::calc(r#""copy held " & Get ( LastError )"#)),
    );
    let mut proof = ScriptStep::new(87);
    proof.set_text(0, Some("Close proof"));
    proof.set_calculation(
        1,
        Some(&fixture::calc(
            r#""copy attempts " & $$copyWitness & ":" & GetAsNumber ( $$copyAttempts )"#,
        )),
    );
    for script in [
        fixture::script(
            2,
            "Copy stored source",
            vec![
                fixture::set_variable("$$copyWitness", r#""armed""#),
                fixture::set_variable("$$copyAttempts", "0"),
                copy,
                held,
            ],
        ),
        fixture::script(3, "Check copy attempts", vec![proof]),
        fixture::script(
            4,
            "Count copy attempts",
            vec![
                fixture::set_variable("$$copyAttempts", "GetAsNumber ( $$copyAttempts ) + 1"),
                fixture::exit_with("True"),
            ],
        ),
    ] {
        let encoded = script.encode();
        root.put(&[&[17], &[5], &encoded.segment.clone()], encoded);
    }
    let event = om_data::runtime::TriggerEvent::RecordCommit.id();
    let trigger = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(4),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    let layout = uint_segment(fixture::LAYOUT);
    root.put(
        &[&[4], &[5], &layout, &[0x30], &[4]],
        OwnedNode::leaf([], 1u16.to_be_bytes()),
    );
    root.put(
        &[
            &[4],
            &[5],
            &layout,
            &[0x30],
            &[5],
            &uint_segment(u64::from(event)),
            &[1],
        ],
        OwnedNode::leaf([], trigger),
    );
    let host = related::Host::new("gui-source-first", files);
    let mut seed = host.open("Root.fmp12");
    let reference = seed
        .container_reference(
            fixture::LAYOUT as u32,
            "StoredAttachment",
            &om_data::calc::Container::file("source.bin", b"source bytes".to_vec()),
        )
        .unwrap()
        .unwrap();
    seed.update_record(
        fixture::LAYOUT as u32,
        1,
        [("StoredAttachment".into(), Some(reference.clone()))].into(),
    )
    .unwrap();
    seed.close();
    let backup = || {
        let mut fresh = host.open("Root.fmp12");
        let page = fresh.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        fresh.close();
        let at = page
            .fields
            .iter()
            .position(|f| f.name == "AttachmentBackup")
            .unwrap();
        page.records[0].values[at].clone()
    };
    let mut app = common::harness(Box::new(
        host.provider()
            .with_keepalive(Some(std::time::Duration::from_millis(100))),
    ));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "source Root", |a| a.state().is_settled());
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let before = wire();
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Copy stored source");
    common::wait_for_label(&mut app, "copy held 0");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "copy script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    let mut expected = before;
    expected[5] += 1;
    assert_eq!(wire(), expected, "one op87, no Root upload or Save");
    assert_eq!(backup(), None);
    host.handle.close_connections();
    common::wait_until(&mut app, "natural loss", |a| {
        a.state().connection_lost().is_some()
    });
    common::click(&mut app, "Reconnect…");
    common::wait_until(&mut app, "source reconnect credentials", |a| {
        common::find_text_field(a, "Password").is_some()
    });
    common::fill(&mut app, "Password", PASSWORD);
    app.get_by_role_and_label(egui::Role::Button, "Reconnect")
        .click();
    common::wait_until(&mut app, "source copy reconnected", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().connection_lost().is_none() && a.state().is_settled()
    });
    assert_eq!(wire(), expected);
    assert_eq!(backup(), None);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Check copy attempts");
    common::wait_for_label(&mut app, "copy attempts armed:0");
    common::click(&mut app, "Close proof");
    common::wait_until(&mut app, "copy proof ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    assert_eq!(wire(), expected);
    common::save_record(&mut app);
    common::wait_until(&mut app, "explicit source Save", |a| {
        a.state().is_settled() && !common::editing(a)
    });
    assert_eq!(backup(), Some(reference));
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Check copy attempts");
    common::wait_for_label(&mut app, "copy attempts armed:1");
}

/// Normal script drafts supply the accepted existing version and owner-bound
/// new preview before the log's Insert, without invoking Save to prepare them.
#[test]
fn a_new_root_and_scalar_related_edit_stage_a_new_logs_attachment_before_save() {
    mixed_attachment_caller(false);
}

#[test]
fn browse_scalar_first_existing_root_retains_mixed_attachment_rows_before_save() {
    mixed_attachment_caller(true);
}

fn mixed_attachment_caller(existing_root: bool) {
    use egui_kittest::kittest::Queryable;
    use fixture::{COUNT, NAME, OCCURRENCE};
    use om_data::DataProvider;
    use om_protocol::Opcode;
    use om_scripts::{FieldSpec, ScriptStep, StoredKey};
    #[path = "../../om-server/tests/fixtures/transfer.rs"]
    mod transfer;
    const REMOTE: u64 = 0xd0_0040;
    const OTHER: u64 = 0xd0_0041;
    const ATTACHMENT: u64 = 90;
    let filename = "mixed-before-save.txt";
    let temporary = std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id()));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join(filename), b"ordinary mixed upload").unwrap();
    let insert = |occurrence| {
        let mut step = ScriptStep::new(131);
        step.flags |= om_scripts::flag::NO_INTERACT;
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(occurrence).unwrap(),
                StoredKey::from_id(ATTACHMENT).unwrap(),
                0,
            )),
        );
        transfer::with_paths(step, &["$p"])
    };
    let mut show = ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&fixture::calc("\"mixed held \" & Get ( LastError )")),
    );
    let go_field = |occurrence| {
        let mut step = ScriptStep::new(17);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(occurrence).unwrap(),
                StoredKey::from_id(NAME).unwrap(),
                0,
            )),
        );
        step
    };
    let row = |number| {
        let mut step = ScriptStep::new(99);
        step.value = 5;
        step.flags |= om_scripts::flag::MENU_SELECTION | om_scripts::flag::NO_INTERACT;
        step.set_calculation(0, Some(&fixture::calc(number)));
        step
    };
    let mut steps = vec![
        fixture::on(86, true),
        ScriptStep::new(7),
        fixture::set_variable("$$mixedCommitAttempts", "0"),
        fixture::set_variable("$$mixedCommitWitness", "\"armed\""),
        fixture::set_field(COUNT, "9"),
        fixture::set_variable("$p", &format!("Get ( TemporaryPath ) & \"{filename}\"")),
        insert(OCCURRENCE),
        go_field(REMOTE),
        row("1"),
        fixture::set_field_in(REMOTE, NAME, "\"first\""),
        row("2"),
        fixture::set_field_in(REMOTE, NAME, "\"second\""),
        insert(REMOTE),
        go_field(OTHER),
        row("1"),
        fixture::set_field_in(OTHER, NAME, "\"logA\""),
        insert(OTHER),
        row("2"),
        fixture::set_field_in(OTHER, NAME, "\"logB\""),
        insert(OTHER),
        show,
    ];
    if existing_root {
        // The real Browse edit already armed its witness before the first key.
        // Keep its counter, and do not create a replacement Root.
        steps.drain(1..4);
    }
    let mut files = portal_save_log_files(None, false, false);
    #[path = "../../om-provider/tests/common/privileges.rs"]
    mod security;
    for (name, tree) in &mut files {
        let set = if *name == "Other.fmp12" {
            &[3][..]
        } else {
            security::CUSTOM_KEY
        };
        tree.set_child(security::tree(&[], set).child(&[23]).unwrap().clone());
        if *name != "Other.fmp12" {
            fixture::add_field(tree, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
        }
        if *name == "Related.fmp12" {
            fixture::add_field(
                tree,
                fixture::NOTES,
                ATTACHMENT,
                "Attachment",
                1,
                6,
                0,
                0,
                0,
            );
            fixture::add_field(tree, fixture::NOTES, 30, "Job", 1, 2, 0, 0, 0);
        }
    }
    if existing_root {
        // This fixture's normal value is already 9. Start at 8 so the real
        // Browse keystroke changes a scalar before Insert prepares its assets.
        files[0].1.put(
            &[
                &uint_segment(TABLE),
                &[5],
                &om_hbam::serial::encode(1),
                &uint_segment(COUNT),
            ],
            om_scripts::text_node(&[], "8"),
        );
    }
    related::reference(&mut files[0].1, (OTHER, "Log Notes"), 2, "Related");
    let mut definition = related::key(2);
    definition.extend(related::key(fixture::NOTES));
    files[0].1.put(
        &[&[3], &[17], &[5], &uint_segment(OTHER), &[2]],
        OwnedNode::leaf([], definition),
    );
    // Actual related portals select separate accepted/begun rows through normal
    // script navigation. Insert still requires displayed container occurrences.
    let layout = uint_segment(fixture::LAYOUT);
    let path = [&[4][..], &[5], &layout, &[7]];
    let mut objects = Vec::new();
    let mut keys = Vec::new();
    if existing_root {
        // A normal Root editor, separate from the related portal occurrences.
        objects.push(om_layout::encode::field_object(
            70,
            &om_layout::Rect {
                top: 55.0,
                left: 20.0,
                bottom: 80.0,
                right: 220.0,
                ..Default::default()
            },
            om_hbam::serial::id_of_raw(OCCURRENCE).unwrap(),
            COUNT,
        ));
        keys.push(70);
    }

    for (base, occurrence, top) in [(50, REMOTE, 220.0), (60, OTHER, 330.0)] {
        let alias = om_hbam::serial::id_of_raw(occurrence).unwrap();
        let bounds = om_layout::Rect {
            top,
            left: 20.0,
            bottom: top + 90.0,
            right: 500.0,
            ..Default::default()
        };
        let mut object =
            om_layout::encode::portal_object(base, &bounds, &[base + 1, base + 2], alias, 3);
        // This lifecycle navigates the normal empty creation row, which is
        // available only in a scrolling portal (the encoder defaults to fixed).
        let mut portal = Vec::new();
        om_layout::encode::uint(2, alias, &mut portal);
        om_layout::encode::uint(3, 0x11, &mut portal);
        om_layout::encode::uint(5, 1, &mut portal);
        om_layout::encode::double(6, 3.0, &mut portal);
        om_layout::encode::bytes(15, &portal, &mut object);
        objects.push(object);
        for (id, field, left) in [(base + 1, NAME, 20.0), (base + 2, ATTACHMENT, 260.0)] {
            let bounds = om_layout::Rect {
                top,
                left,
                bottom: top + 25.0,
                right: left + 200.0,
                ..Default::default()
            };
            objects.push(om_layout::encode::portal_field_object(
                id, &bounds, alias, field,
            ));
            keys.push(id);
        }
        keys.push(base);
    }
    if existing_root {
        // Tasks' generic fixture has no Form parts. Give this actual Browse
        // caller a normal body and its object index, as placed_fields does.
        files[0].1.put(
            &[&[4], &[5], &layout, &[2]],
            OwnedNode::leaf(
                [],
                om_layout::encode::new_layout_def(
                    om_hbam::serial::id_of_raw(OCCURRENCE).unwrap(),
                    600.0,
                ),
            ),
        );
        files[0].1.put(
            &[&[4], &[5], &layout, &[3], &[1], &[2]],
            OwnedNode::leaf([], om_layout::encode::new_body_part(460.0)),
        );
        let roots = [70u64, 50, 60];
        let mut index = (roots.len() as u32).to_be_bytes().to_vec();
        for root in roots {
            let key = uint_segment(root);
            index.extend([key.len() as u8 + 1, key.len() as u8]);
            index.extend(key);
        }
        files[0].1.put(
            &[&[4], &[5], &layout, &[3], &[1], &[7]],
            OwnedNode::leaf([], index),
        );
    }
    let store = files[0]
        .1
        .at(&path)
        .and_then(|n| n.value.as_deref())
        .unwrap_or_default();
    let store = om_layout::encode::append_to_store(store, &keys, &objects);
    files[0].1.put(&path, OwnedNode::leaf([], store));
    let script = fixture::script(2, "Hold mixed attachments", steps).encode();
    files[0]
        .1
        .put(&[&[17], &[5], &script.segment.clone()], script);
    // This counter catches a Save attempt even when the connection loss
    // prevents every op14/op70. The witness proves globals survive reconnect.
    let mut check = ScriptStep::new(87);
    check.set_text(0, Some("Close proof"));
    check.set_calculation(1, Some(&fixture::calc("\"mixed Save attempts \" & $$mixedCommitWitness & \":\" & GetAsNumber ( $$mixedCommitAttempts )")));
    for script in [
        fixture::script(
            5,
            "Arm scalar-first proof",
            vec![
                fixture::set_variable("$$mixedCommitAttempts", "0"),
                fixture::set_variable("$$mixedCommitWitness", r#""armed""#),
            ],
        ),
        fixture::script(3, "Check mixed Save attempts", vec![check]),
        fixture::script(
            4,
            "Count mixed Save attempts",
            vec![
                fixture::set_variable(
                    "$$mixedCommitAttempts",
                    "GetAsNumber ( $$mixedCommitAttempts ) + 1",
                ),
                fixture::exit_with("True"),
            ],
        ),
    ] {
        let script = script.encode();
        files[0]
            .1
            .put(&[&[17], &[5], &script.segment.clone()], script);
    }
    let event = om_data::runtime::TriggerEvent::RecordCommit.id();
    let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(4),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    files[0].1.put(
        &[&[4], &[5], &layout, &[0x30], &[4]],
        OwnedNode::leaf([], 1u16.to_be_bytes()),
    );
    files[0].1.put(
        &[
            &[4],
            &[5],
            &layout,
            &[0x30],
            &[5],
            &uint_segment(u64::from(event)),
            &[1],
        ],
        OwnedNode::leaf([], record),
    );
    let host = related::Host::new("gui-mixed-before-save", files);
    let stored_rows = || {
        [
            ("Root.fmp12", fixture::LAYOUT),
            ("Related.fmp12", fixture::LAYOUT),
            ("Related.fmp12", fixture::NOTES_LAYOUT),
            ("Other.fmp12", fixture::LAYOUT),
        ]
        .map(|(file, layout)| {
            let mut reader = host.open(file);
            let page = reader.records(layout as u32, 0, 100).unwrap();
            reader.close();
            page.records
                .into_iter()
                .map(|r| (r.id, r.values))
                .collect::<Vec<_>>()
        })
    };
    let original_rows = stored_rows();
    // Existing public test seam: real keepalive observes the closed socket;
    // the idle worker delivers its normal ConnectionLost notification.
    let mut app = common::harness(Box::new(
        host.provider()
            .with_related_on_demand(true)
            .with_keepalive(Some(std::time::Duration::from_millis(100))),
    ));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "Root loaded", |a| a.state().is_settled());
    if existing_root {
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Arm scalar-first proof");
        common::wait_until(&mut app, "proof armed before typing", |a| {
            !a.state().script_running() && a.state().is_settled()
        });
        // Real editor input precedes Insert and normal worker script staging.
        // The candidate text must survive its asynchronous preparation.
        common::wait_until(&mut app, "Root Count editor after the script", |a| {
            common::find_text_field(a, "Count").is_some()
        });
        hosted::replace(&mut app, "Count", "9");
        common::wait_until(&mut app, "scalar candidate prepared", |a| {
            assert!(a.state().error().is_none(), "{:?}", a.state().error());
            common::editing(a)
        });
        assert_eq!(stored_rows(), original_rows, "typing never saves");
    }
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Hold mixed attachments");
    let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        common::wait_for_label(&mut app, "mixed held 0")
    }));
    if let Err(panic) = ready {
        let labels: Vec<_> = app
            .query_all(egui_kittest::kittest::By::new().role(egui::Role::Label))
            .filter_map(|n| n.value())
            .collect();
        let ops: Vec<_> = host
            .captures
            .transactions()
            .iter()
            .map(|(_, tx)| tx[0])
            .collect();
        eprintln!(
            "mixed caller: error {:?}, script {}, labels {labels:?}, wire {ops:?}",
            app.state().error(),
            app.state().script_running()
        );
        std::panic::resume_unwind(panic);
    }
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "mixed script ended", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let before = wire();
    assert_eq!(
        before,
        [if existing_root { 2 } else { 3 }, 0, 0, 0, 4, 0],
        "three held creations/four uploads and no Save"
    );
    assert_eq!(
        stored_rows(),
        original_rows,
        "all three stored files unchanged before loss"
    );
    host.handle.close_connections();
    // No action/Refresh/Commit is sent to discover loss. Only idle frames
    // consume the keepalive -> worker -> ConnectionLost notification.
    common::wait_until(&mut app, "lost connection", |a| {
        a.query_by_label("Reconnect…").is_some()
    });
    assert!(
        app.state().connection_lost().is_some(),
        "natural lost state precedes Reconnect"
    );
    assert_eq!(
        wire(),
        before,
        "passive loss observation attempts no replacement or data/asset write"
    );
    common::click(&mut app, "Reconnect…");
    common::wait_until(&mut app, "reconnect password", |a| {
        common::find_text_field(a, "Password").is_some()
    });
    common::fill(&mut app, "Password", PASSWORD);
    app.get_by_role_and_label(egui::Role::Button, "Reconnect")
        .click();
    common::wait_until(&mut app, "mixed group reconnected", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().connection_lost().is_none() && a.state().is_settled()
    });
    let mut expected = before;
    expected[0] += if existing_root { 2 } else { 3 };
    assert_eq!(wire(), expected, "no submit/reupload/count change");
    assert_eq!(
        stored_rows(),
        original_rows,
        "reconnect submitted none of the three drafts"
    );
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Check mixed Save attempts");
    common::wait_for_label(&mut app, "mixed Save attempts armed:0");
    common::click(&mut app, "Close proof");
    common::wait_until(&mut app, "zero Save attempts checked", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    assert_eq!(wire(), expected);
    common::save_record(&mut app);
    common::wait_until(&mut app, "explicit mixed Save", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().is_settled() && !common::editing(a)
    });
    let mut reader = host.open("Related.fmp12");
    let tasks = reader.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    for name in ["first", "second"] {
        assert!(tasks
            .records
            .iter()
            .any(|r| r.values[0].as_deref() == Some(name)));
    }
    let notes = reader
        .records(fixture::NOTES_LAYOUT as u32, 0, 100)
        .unwrap();
    for name in ["logA", "logB"] {
        assert!(notes
            .records
            .iter()
            .any(|r| r.values[0].as_deref() == Some(name)));
    }
    reader.close();
    assert_eq!(
        stored_rows()[3],
        original_rows[3],
        "passive Read-Only file stays unchanged"
    );
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Check mixed Save attempts");
    common::wait_for_label(&mut app, "mixed Save attempts armed:1");
    common::click(&mut app, "Close proof");
    common::wait_until(&mut app, "explicit Save attempt checked", |a| {
        !a.state().script_running() && a.state().is_settled()
    });
    assert_eq!(
        wire()[4],
        4,
        "each actual acquisition uploaded once; reconnect did not reacquire"
    );
    if existing_root {
        // Use an ordinary fresh session after the completed recovery case;
        // it has no retained recovery result or participant session.
        drop(app);
        let mut app = common::harness(Box::new(
            host.provider()
                .with_keepalive(Some(std::time::Duration::from_millis(100))),
        ));
        common::click(&mut app, "Use saved FileMaker license");
        common::fill(
            &mut app,
            "Path",
            &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
        );
        common::fill(&mut app, "Account Name", ACCOUNT);
        common::fill(&mut app, "Password", PASSWORD);
        common::click(&mut app, "Open");
        common::wait_until(&mut app, "fresh Root caller", |a| a.state().is_settled());
        // A real stale accepted page refuses input. Its alert must dismiss,
        // and explicit discard must release the gate without replaying Save.
        let mut peer = host.open("Root.fmp12");
        let stored = peer.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let count = stored
            .fields
            .iter()
            .position(|f| f.name == "Count")
            .unwrap();
        assert_eq!(
            stored.records[0].values[count].as_deref(),
            Some("9"),
            "the original Browse scalar reached the explicit Save"
        );
        peer.update_record(
            fixture::LAYOUT as u32,
            1,
            [("Count".into(), Some("7".into()))].into_iter().collect(),
        )
        .unwrap();
        peer.close();
        let before_error = wire();
        hosted::replace(&mut app, "Count", "8");
        common::wait_until(&mut app, "stale entry refusal", |a| {
            a.state().error().is_some()
        });
        let artifact = |a: &mut common::App, stage: &str| {
            if let Some(dir) = std::env::var_os("OM_COMMIT_ARTIFACT_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                a.render()
                    .unwrap()
                    .save(dir.join(format!("entry-{stage}.png")))
                    .unwrap();
            }
        };
        artifact(&mut app, "refused");
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "entry alert dismissed", |a| {
            a.state().error().is_none()
        });
        assert_eq!(
            common::text_field(&app, "Count").value().as_deref(),
            Some("8")
        );
        common::save_record(&mut app);
        host.handle.close_connections();
        common::wait_until(&mut app, "loss after preparation refusal", |a| {
            a.state().connection_lost().is_some()
        });
        common::click(&mut app, "Reconnect…");
        assert!(
            common::find_text_field(&app, "Password").is_none(),
            "failed input waits for explicit discard"
        );
        common::click(&mut app, "Discard Input");
        common::wait_until(&mut app, "refused input discarded", |a| {
            common::text_field(a, "Count").value().as_deref() == Some("9") && a.state().is_settled()
        });
        assert_eq!(
            wire(),
            before_error,
            "discard never replays the queued Save"
        );
        artifact(&mut app, "discarded");
        assert!(
            common::find_text_field(&app, "Password").is_none(),
            "discard never replays a held Reconnect"
        );
        assert!(app.state().connection_lost().is_some());
        common::click(&mut app, "Reconnect…");
        common::wait_until(&mut app, "reconnect after discarded input", |a| {
            common::find_text_field(a, "Password").is_some()
        });
        common::fill(&mut app, "Password", PASSWORD);
        app.get_by_role_and_label(egui::Role::Button, "Reconnect")
            .click();
        common::wait_until(&mut app, "explicit reconnect after refused input", |a| {
            assert!(a.state().error().is_none(), "{:?}", a.state().error());
            a.state().connection_lost().is_none() && a.state().is_settled()
        });
        assert_eq!(
            wire(),
            before_error,
            "reconnect after discard changes no data or counts"
        );
        // The ordinary read-only refresh accepts the new page. A new entry
        // then prepares normally, and the normal explicit Revert discards it.
        app.key_press_modifiers(
            egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            egui::Key::R,
        );
        common::wait_until(&mut app, "fresh accepted page", |a| a.state().is_settled());
        hosted::replace(&mut app, "Count", "8");
        common::wait_until(&mut app, "entry after discard", |a| {
            assert!(a.state().error().is_none(), "{:?}", a.state().error());
            common::editing(a) && a.state().is_settled()
        });
        common::revert_record(&mut app);
        assert_eq!(
            wire(),
            before_error,
            "retry and Revert submit no data or asset changes"
        );
    }
}

/// Keyboard Commit while the related editor is still active: its save
/// opens Other for the first time and logs the edited row there, before
/// ObjectExit and RecordCommit resume the held commit. The same-file
/// keyboard case cannot cover this asynchronous schema/open continuation.
#[test]
fn keyboard_commit_logs_an_active_portal_edit_in_an_unopened_third_file() {
    use egui_kittest::kittest::{By, Queryable};
    use fixture::{NAME, OCCURRENCE, SETTING};
    use om_data::{runtime::TriggerEvent, DataProvider};
    use om_layout::encode;
    const OTHER: u64 = 0xd0_0041;
    const LINES: u64 = 3;
    const LINE_NAME: u64 = 2;
    const TRACE: u64 = 9;

    let mut files = portal_save_log_files(None, false, false);
    let root = &mut files[0].1;
    // Preserve the positive fixture's log operation; markers write only
    // Root's global, so they neither open Other nor change a record.
    for (id, name, marker, mut steps) in [
        (
            1,
            "Log the line",
            "S",
            vec![fixture::set_field_in(
                OTHER,
                NAME,
                "\"saved \" & Get ( ActivePortalRowNumber ) & \":\" & Get ( ActiveFieldContents )",
            )],
        ),
        (2, "Trace exit", "E", Vec::new()),
        (3, "Trace commit", "C", Vec::new()),
    ] {
        steps.push(fixture::set_field(
            SETTING,
            &format!("Setting & \"|{marker}\""),
        ));
        steps.push(fixture::exit_with("True"));
        let script = fixture::script(id, name, steps).encode();
        let key = script.segment.clone();
        root.put(&[&[17], &[5], &key], script);
    }
    let layout = uint_segment(LINES);
    let object = om_hbam::serial::encode(LINE_NAME);
    for (cache, triggers) in [
        (
            vec![&[4][..], &[5], &layout, &[5], &object, &[0x30]],
            vec![(TriggerEvent::ObjectSave, 1), (TriggerEvent::ObjectExit, 2)],
        ),
        (
            vec![&[4][..], &[5], &layout, &[0x30]],
            vec![(TriggerEvent::RecordCommit, 3)],
        ),
    ] {
        let mut count = cache.clone();
        count.push(&[4]);
        root.put(
            &count,
            OwnedNode::leaf([], (triggers.len() as u16).to_be_bytes()),
        );
        for (event, script) in triggers {
            let event = event.id();
            let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
                event,
                script: Some(script),
                file_reference: None,
                external: false,
                modes: 1,
                option: 0,
                extra: Vec::new(),
                parameter: None,
            });
            let event = uint_segment(u64::from(event));
            let mut entry = cache.clone();
            entry.extend([&[5][..], &event, &[1]]);
            root.put(&entry, OwnedNode::leaf([], record));
        }
    }
    // A visible global trace lets the first settled frame prove the
    // order, without a diagnostic click supplying a missing field exit.
    let path = [&[4][..], &[5], &layout, &[7]];
    let store = root.at(&path).unwrap().value.as_ref().unwrap();
    let trace = encode::field_object(
        TRACE,
        &om_layout::Rect {
            top: 20.0,
            left: 230.0,
            bottom: 40.0,
            right: 410.0,
            ..Default::default()
        },
        om_hbam::serial::id_of_raw(OCCURRENCE).unwrap(),
        SETTING,
    );
    let store = encode::append_to_store(store, &[TRACE], &[trace]);
    root.put(&path, OwnedNode::leaf([], store));

    let host = related::Host::new("keyboard-third-file", files);
    let mut app = common::harness_sized(
        Box::new(host.provider().with_related_on_demand(true)),
        egui::vec2(1600.0, 900.0),
    );
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "Root open", |a| a.state().is_settled());
    common::select_layout(&mut app, "Lines");
    let line = || {
        By::new()
            .role(egui::Role::TextInput)
            .label("Remote Tasks::Name")
    };
    let idle = |a: &common::App| !a.state().script_running() && a.state().is_settled();
    common::wait_until(&mut app, "Related's rows", |a| {
        a.query_all(line()).count() >= 2
    });
    app.query_all(line()).next().unwrap().click();
    common::wait_until(&mut app, "row 1's editor", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(line())
        .next()
        .unwrap()
        .type_text("first edited");
    common::wait_until(&mut app, "the typed edit", common::unsaved);
    assert!(app.query_all(line()).next().unwrap().is_focused());
    assert_eq!(
        common::text_field(&app, "Setting").value().as_deref(),
        Some("")
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert!(opens.contains_key("Related.fmp12"), "{opens:?}");
    assert!(
        !opens.contains_key("Other.fmp12"),
        "Other must still be unopened: {opens:?}"
    );
    let artifact = |a: &mut common::App, stage: &str| {
        if let Some(dir) = std::env::var_os("OM_COMMIT_ARTIFACT_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            a.render()
                .unwrap()
                .save(dir.join(format!("third-file-{stage}.png")))
                .unwrap();
        }
    };
    artifact(&mut app, "before");
    // No Tab, background click, navigation or other action between the
    // active edit and this keyboard Commit.
    common::save_record(&mut app);
    common::wait_until(&mut app, "the keyboard save and third-file log", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        idle(a) && !common::unsaved(a)
    });
    assert_eq!(
        common::text_field(&app, "Setting").value().as_deref(),
        Some("|S|E|C")
    );
    assert!(app
        .query_all(By::new().role(egui::Role::TextInput))
        .all(|n| !n.is_focused()));
    artifact(&mut app, "after");
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(
        opens.get("Other.fmp12"),
        Some(&1),
        "opened by ObjectSave: {opens:?}"
    );
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::HostCreateRecord)
            .iter()
            .filter(|(file, _)| file == "Other.fmp12")
            .count(),
        1,
        "one new log record"
    );
    let commits = host.captures.with_opcode(om_protocol::Opcode::HostCommit);
    for file in ["Related.fmp12", "Other.fmp12"] {
        assert_eq!(
            commits.iter().filter(|(name, _)| name == file).count(),
            1,
            "one saved edit in {file}"
        );
    }
    // New independent sessions read storage, not the GUI's retained drafts.
    let rows = |file: &str| {
        let mut fresh = host.open(file);
        let page = fresh.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let name = page.fields.iter().position(|f| f.name == "Name").unwrap();
        let rows: Vec<_> = page
            .records
            .iter()
            .map(|r| (r.id, r.values[name].clone()))
            .collect();
        fresh.close();
        rows
    };
    assert_eq!(
        rows("Related.fmp12"),
        vec![(1, Some("first edited".into())), (2, Some("Ship".into()))]
    );
    assert_eq!(
        rows("Other.fmp12"),
        vec![
            (1, Some("Write tests".into())),
            (2, Some("Ship".into())),
            (3, Some("saved 1:first edited".into()))
        ]
    );
    assert_eq!(
        rows("Root.fmp12"),
        vec![(1, Some("Write tests".into())), (2, Some("Ship".into()))]
    );
}

/// Write [`portal_save_log_files`] into the folder `OM_ACCEPTANCE_DIR`
/// names, for OpenMaker Server (`om-server --list-all <folder>/*.fmp12`).
/// With `OM_ACCEPTANCE_PEER` set to `host:port`, Root names Related and
/// Other on the server there, which must host them. Both parts are
/// required and must be what the client reads from the stored path: the
/// fixture never falls back to a default host or to FileMaker's port.
/// With `OM_ACCEPTANCE_VARIANT=refusal`, the files are the refusal variant
/// ([`portal_save_log_files`]); any other value is refused.
#[test]
#[ignore = "writes the three-file portal save acceptance fixture; requires OM_ACCEPTANCE_DIR"]
fn write_the_portal_save_log_acceptance_files() {
    use om_data::file_refs::{FilePath, LOCATION_SEPARATOR};
    let dir = std::env::var_os("OM_ACCEPTANCE_DIR").expect("OM_ACCEPTANCE_DIR names a folder");
    let dir = Path::new(&dir);
    let peer = std::env::var("OM_ACCEPTANCE_PEER").ok().map(|endpoint| {
        let named = endpoint
            .rsplit_once(':')
            .filter(|(host, _)| !host.is_empty())
            .and_then(|(host, port)| Some((host, port.parse::<u16>().ok().filter(|&p| p != 0)?)));
        let location = format!("{LOCATION_SEPARATOR}{endpoint}{LOCATION_SEPARATOR}");
        // The endpoint the client takes from the stored path.
        let read = FilePath {
            scheme: "fmnet".to_owned(),
            location: location.clone(),
            name: "Related".to_owned(),
        };
        assert!(
            named.is_some() && read.fmnet_endpoint() == named,
            "OM_ACCEPTANCE_PEER is host:port as an fmnet path names it, not {endpoint:?}"
        );
        location
    });
    let refusal = match std::env::var("OM_ACCEPTANCE_VARIANT").ok().as_deref() {
        None => false,
        Some("refusal") => true,
        Some(other) => panic!("OM_ACCEPTANCE_VARIANT is refusal or unset, not {other:?}"),
    };
    std::fs::create_dir_all(dir).unwrap();
    for (name, tree) in portal_save_log_files(peer.as_deref(), refusal, false) {
        fixture::write_tree(&dir.join(name), &tree);
    }
}

/// A hosted solution's refused log (#1006, #1095), on [`portal_save_log_files`]'s
/// refusal variant: the window's save is refused before any record data by
/// Other's rule (Count not empty) on the log record the row's OnObjectSave
/// made. The user fills the held log's Count in Other's portal and saves
/// once. The log keeps the record id its first attempt reserved, with
/// nothing reserved again, and each file holds its change once.
#[test]
fn a_refused_logs_record_keeps_its_reserved_id_when_corrected() {
    refused_log_correction(false, false, false);
}

#[test]
fn a_refused_related_scalar_save_keeps_both_drafts_after_reconnect() {
    refused_log_correction(true, false, false);
}

#[test]
fn a_refused_related_save_freezes_if_a_host_version_changed() {
    refused_log_correction(true, true, false);
}

#[test]
fn a_refused_new_root_and_related_scalar_save_keep_all_drafts_after_reconnect() {
    for changed in [false, true] {
        refused_log_correction(true, changed, true);
    }
}

fn refused_log_correction(reconnect: bool, changed: bool, new_root: bool) {
    use egui_kittest::kittest::{By, Queryable};
    use om_data::DataProvider;
    let mut files = portal_save_log_files(None, true, false);
    let other = &mut files
        .iter_mut()
        .find(|(name, _)| *name == "Other.fmp12")
        .unwrap()
        .1;
    let issued_fields = |other: &mut OwnedNode| {
        const SERIAL: u64 = 31;
        fixture::add_field(other, TABLE, SERIAL, "Serial", 1, 2, 0, 0x2, 0);
        fixture::set_field_serial_increment(other, TABLE, SERIAL, 1);
        other.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(SERIAL),
                &[0x0c],
            ],
            om_scripts::text_node(&[], "7"),
        );
        const STAMP: u64 = 32;
        const CREATED: u64 = 33;
        fixture::add_field(other, TABLE, STAMP, "Stamp", 1, 1, 0, 0x8, 0);
        fixture::add_field(other, TABLE, CREATED, "Created", 1, 5, 2, 1, 0);
        let mut names = fixture::names();
        names.add_field(TABLE, STAMP, "Stamp");
        fixture::add_formula(
            other,
            &names,
            (TABLE, fixture::OCCURRENCE),
            STAMP,
            5,
            "Get ( UUID )",
        );
    };
    issued_fields(other);
    let peer = reconnect.then(|| {
        related::Host::new(
            "refused-log-peer",
            files
                .iter()
                .filter(|(name, _)| *name != "Root.fmp12")
                .map(|(name, tree)| (*name, tree.clone()))
                .collect(),
        )
    });
    if let Some(peer) = &peer {
        let separator = om_data::file_refs::LOCATION_SEPARATOR;
        let endpoint = format!("{separator}127.0.0.1:{}{separator}", peer.port);
        files = portal_save_log_files(Some(&endpoint), true, false)
            .into_iter()
            .filter(|(name, _)| *name == "Root.fmp12")
            .collect();
    }
    if new_root {
        let root = &mut files
            .iter_mut()
            .find(|(name, _)| *name == "Root.fmp12")
            .unwrap()
            .1;
        issued_fields(root);
        let path = [&[4][..], &[5], &[3], &[7]];
        let store = root.at(&path).unwrap().value.as_ref().unwrap();
        let count = om_layout::encode::field_object(
            50,
            &om_layout::Rect {
                top: 20.0,
                left: 230.0,
                bottom: 40.0,
                right: 410.0,
                ..Default::default()
            },
            om_hbam::serial::id_of_raw(fixture::OCCURRENCE).unwrap(),
            fixture::COUNT,
        );
        let store = om_layout::encode::append_to_store(store, &[50], &[count]);
        root.put(&path, OwnedNode::leaf([], store));
        fixture::set_field_options(root, TABLE, fixture::NAME, 0x8);
        fixture::add_formula(
            root,
            &fixture::names(),
            (TABLE, fixture::OCCURRENCE),
            fixture::NAME,
            5,
            "\"new root\"",
        );
        // The trigger samples Root's issued creation state before the refusal.
        // Its retained log must still describe the saved Root after recovery.
        let log = fixture::script(1, "Log the line", vec![fixture::set_field_in(0xd0_0041, fixture::NAME,
            "\"saved \" & Get ( ActivePortalRowNumber ) & \":\" & Get ( ActiveFieldContents ) & \"|\" & GetField ( \"Stamp\" ) & \"|\" & GetField ( \"Created\" )")]).encode();
        root.put(&[&[17], &[5], &log.segment.clone()], log);
    } else if reconnect {
        // Root RID3 collides with Other's reserved RID3. Its replacement RID4
        // must never move this window to Root RID4 (the second root record).
        let root = &mut files
            .iter_mut()
            .find(|(name, _)| *name == "Root.fmp12")
            .unwrap()
            .1;
        let mut ids = om_hbam::RidList::new();
        for (old, new) in [(1, 3), (2, 4)] {
            let row = root
                .remove(&[&uint_segment(TABLE), &[5], &om_hbam::serial::encode(old)])
                .unwrap();
            root.put(
                &[&uint_segment(TABLE), &[5], &om_hbam::serial::encode(new)],
                row,
            );
            ids.insert(new);
        }
        root.put(&[&uint_segment(TABLE), &[0x0d]], ids.to_node(Vec::new()));
    }
    let host = related::Host::new("refused-log", files);
    let owner = peer.as_ref().unwrap_or(&host);
    let provider = peer
        .as_ref()
        .map_or_else(|| host.provider(), |peer| host.provider_trusting(peer));
    let mut app = common::harness_sized(Box::new(provider), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "Root open", |a| a.state().is_settled());
    assert!(
        app.state().is_open(),
        "Root open: {:?}; A {:?}; B {:?}",
        app.state().error(),
        host.captures.opens_and_logins(),
        peer.as_ref().map(|p| p.captures.opens_and_logins())
    );
    common::select_layout(&mut app, "Lines");
    let field = |label: &'static str| By::new().role(egui::Role::TextInput).label(label);
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    if new_root {
        // Selecting Lines temporarily clears creation capabilities. A visible
        // New Record button is not yet proof that its click can create a record.
        common::wait_until(&mut app, "Lines ready for New Record", |a| {
            use egui_kittest::kittest::NodeT;
            idle(a)
                && a.query_by_role_and_label(egui::Role::Button, "New Record")
                    .is_some_and(|button| !button.accesskit_node().is_disabled())
        });
        common::click(&mut app, "New Record");
        common::wait_until(&mut app, "the new Root", idle);
        common::fill(&mut app, "Count", "9");
        app.key_press(egui::Key::Tab);
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            common::wait_until(&mut app, "the new Root's matched rows", |a| {
                idle(a) && a.query_all(field("Remote Tasks::Name")).count() >= 2
            });
        }));
        if let Err(panic) = waited {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied());
            if message == Some("timed out waiting for the new Root's matched rows") {
                // Read the failed frame only; never step, send input, or ask the provider.
                let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    use egui_kittest::kittest::NodeT;
                    let settled = app.state().is_settled();
                    let running = app.state().script_running();
                    let rows = app.query_all(field("Remote Tasks::Name")).count();
                    let count: Vec<_> = app
                        .query_all(field("Count"))
                        .map(|node| {
                            let access = node.accesskit_node();
                            (
                                access.id(),
                                node.value(),
                                node.is_focused(),
                                access.is_disabled(),
                                access.is_read_only(),
                            )
                        })
                        .collect();
                    eprintln!(
                        "1211 deadline: reconnect={reconnect} changed={changed} \
                         new_root={new_root} settled={settled} script_running={running} \
                         idle={} matched_rows={rows} Count(id,value,focused,disabled,read_only)={count:?} \
                         edit={:?} error={:?} connection_lost={:?}",
                        settled && !running,
                        app.state().record_edit_state(),
                        app.state().error(),
                        app.state().connection_lost(),
                    );
                    eprintln!(
                        "1211 unavailable: current/held RID, Count field/RID binding and accepted \
                         buffer, internal relationship demand/result. Node id is opaque; \
                         rendered Count is not the internal match key."
                    );
                    // Preserve existing wire bytes before fixture Drop removes them.
                    // No flush/join: files can be partial and are not an atomic snapshot.
                    let saved = (|| -> std::io::Result<()> {
                        let saved = tempfile::Builder::new()
                            .prefix("om-1211-deadline-")
                            .tempdir()?;
                        let saved = saved.keep();
                        eprintln!("1211 private wire snapshot: {}", saved.display());
                        for (name, captures) in std::iter::once(("root", &host.captures))
                            .chain(peer.as_ref().map(|p| ("peer", &p.captures)))
                        {
                            for entry in std::fs::read_dir(&captures.0)? {
                                let entry = entry?;
                                if !entry.file_type()?.is_dir() {
                                    continue;
                                }
                                let dest = saved.join(name).join(entry.file_name());
                                std::fs::create_dir_all(&dest)?;
                                for stream in ["c2s.bin", "s2c.bin"] {
                                    std::fs::copy(entry.path().join(stream), dest.join(stream))?;
                                }
                            }
                        }
                        Ok(())
                    })();
                    eprintln!("1211 wire snapshot result: {saved:?}");
                }));
                if observed.is_err() {
                    eprintln!("1211 deadline observation panicked; resuming original panic");
                }
            }
            std::panic::resume_unwind(panic);
        }
    }
    common::wait_until(&mut app, "the portal's rows", |a| {
        a.query_all(field("Remote Tasks::Name")).count() >= 2
    });
    app.query_all(field("Remote Tasks::Name"))
        .next()
        .unwrap()
        .click();
    common::wait_until(&mut app, "row 1's editor", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(field("Remote Tasks::Name"))
        .next()
        .unwrap()
        .type_text("first edited");
    app.run_steps(2);
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "row 1's OnObjectSave", idle);
    // Refused before any record data: Other's log holds no Count.
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |a| a.state().error().is_some());
    let refusal = app.state().error().unwrap_or_default().to_owned();
    common::click(&mut app, "OK");
    // A second explicit save still refuses, with the same reservation.
    common::save_record(&mut app);
    common::wait_until(&mut app, "the repeated refusal", |a| {
        a.state().error().is_some()
    });
    common::click(&mut app, "OK");
    for file in ["Other.fmp12", "Related.fmp12"] {
        let mut fresh = owner.open(file);
        let page = fresh.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        assert_eq!(page.records.len(), 2, "nothing submitted in {file}");
        assert_eq!(page.records[0].values[0].as_deref(), Some("Write tests"));
        fresh.close();
    }
    if new_root {
        let mut fresh = host.open("Root.fmp12");
        assert_eq!(
            fresh
                .records(fixture::LAYOUT as u32, 0, 100)
                .unwrap()
                .records
                .len(),
            2
        );
        fresh.close();
        for op in [
            om_protocol::Opcode::UploadListToHostTempLocation,
            om_protocol::Opcode::HostCommit,
        ] {
            assert!(
                host.captures.with_opcode(op).is_empty(),
                "Root must not submit while held"
            );
        }
    }
    if let Some(peer) = &peer {
        for op in [
            om_protocol::Opcode::UploadListToHostTempLocation,
            om_protocol::Opcode::HostCommit,
        ] {
            assert!(
                peer.captures.with_opcode(op).is_empty(),
                "no record data before loss: {op:?}"
            );
        }
        for client in peer.handle.clients() {
            peer.handle.disconnect_client(client.id);
        }
        // Explicit read-only refresh makes the idle worker observe B's loss.
        common::open_menu(&mut app, "Records");
        common::click(&mut app, "Refresh Window");
        common::wait_until(&mut app, "B connection lost", |a| {
            a.query_by_label("Reconnect…").is_some()
        });
        if changed {
            let mut fresh = peer.open("Related.fmp12");
            fresh
                .update_record(
                    fixture::LAYOUT as u32,
                    1,
                    [("Name".to_owned(), Some("peer edit".to_owned()))].into(),
                )
                .unwrap();
            fresh.close();
        }
        let sent: Vec<_> = [
            om_protocol::Opcode::UploadListToHostTempLocation,
            om_protocol::Opcode::HostCommit,
        ]
        .into_iter()
        .map(|op| peer.captures.with_opcode(op).len())
        .collect();
        common::click(&mut app, "Reconnect…");
        common::wait_until(
            &mut app,
            "explicit recovery credentials (both drafts kept)",
            |a| {
                assert!(
                    a.state().error().is_none(),
                    "recovery refused: {:?}",
                    a.state().error()
                );
                common::find_text_field(a, "Password").is_some()
            },
        );
        common::fill(&mut app, "Password", PASSWORD);
        app.get_by_role_and_label(egui::Role::Button, "Reconnect")
            .click();
        app.run_steps(2);
        if changed {
            common::wait_until(&mut app, "Unresolved before reserving a fresh id", |a| {
                a.query_by_label_contains("Unresolved: the related save")
                    .is_some()
            });
            assert_eq!(
                peer.captures
                    .with_opcode(om_protocol::Opcode::HostCreateRecord)
                    .len(),
                1
            );
            // On a loaded machine both drafts' editors were once not found
            // with their values on the status's frame (#285): wait for the
            // editors to be drawn, whatever they show, then check what they
            // show, so a failure tells a late editor from a wrong value.
            common::wait_until(&mut app, "both drafts' editors", |a| {
                a.query_all(field("Remote Tasks::Name")).next().is_some()
                    && a.query_all(field("Other Tasks::Name")).next().is_some()
            });
            assert!(app
                .query_all(field("Remote Tasks::Name"))
                .any(|n| n.value().as_deref() == Some("first edited")));
            assert!(app.query_all(field("Other Tasks::Name")).any(|n| n
                .value()
                .is_some_and(|v| v.starts_with("saved 1:first edited"))));
            if new_root {
                assert_eq!(
                    host.captures
                        .with_opcode(om_protocol::Opcode::HostCreateRecord)
                        .iter()
                        .filter(|(file, _)| file == "Root.fmp12")
                        .count(),
                    1,
                    "all versions must validate before Root reserves again"
                );
                assert_eq!(
                    common::text_field(&app, "Name").value().as_deref(),
                    Some("new root")
                );
                assert_eq!(
                    common::text_field(&app, "Count").value().as_deref(),
                    Some("9")
                );
            }
            for (op, count) in [
                om_protocol::Opcode::UploadListToHostTempLocation,
                om_protocol::Opcode::HostCommit,
            ]
            .into_iter()
            .zip(sent)
            {
                assert_eq!(peer.captures.with_opcode(op).len(), count);
            }
            return;
        }
        common::wait_until(&mut app, "explicit recovery", |a| {
            idle(a) && a.query_by_label("Reconnect…").is_none()
        });
        assert_eq!(
            common::record_number(&app),
            if new_root { "3" } else { "1" },
            "the root position must follow only its own table's id remapping"
        );
        for op in [
            om_protocol::Opcode::UploadListToHostTempLocation,
            om_protocol::Opcode::HostCommit,
        ] {
            assert!(
                peer.captures.with_opcode(op).is_empty(),
                "recovery must not save: {op:?}"
            );
        }
    }

    let log_count = |app: &common::App| {
        app.query_all(field("Other Tasks::Name")).position(|name| {
            name.value()
                .is_some_and(|value| value.starts_with("saved "))
        })
    };
    common::wait_until(&mut app, "the held log in Other's portal", |a| {
        idle(a) && log_count(a).is_some()
    });
    let at = log_count(&app).unwrap();
    app.query_all(field("Other Tasks::Count"))
        .nth(at)
        .unwrap()
        .click();
    app.run_steps(2);
    app.query_all(field("Other Tasks::Count"))
        .nth(at)
        .unwrap()
        .type_text("7");
    app.run_steps(2);
    common::save_record(&mut app);
    common::wait_until(&mut app, "the corrected save", |a| {
        idle(a) && !common::unsaved(a)
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    let rows = |file: &str| {
        let mut provider = owner.open(file);
        let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let at = |name: &str| page.fields.iter().position(|f| f.name == name);
        let text = |record: &om_data::Record, name: &str| {
            at(name)
                .and_then(|at| record.values[at].clone())
                .unwrap_or_default()
        };
        let rows: Vec<(i64, String, String, String)> = page
            .records
            .iter()
            .map(|r| (r.id, text(r, "Name"), text(r, "Count"), text(r, "Job")))
            .collect();
        provider.close();
        rows
    };
    let other = rows("Other.fmp12");
    let logs: Vec<_> = other.iter().filter(|r| r.1.starts_with("saved ")).collect();
    assert_eq!(logs.len(), 1, "the log once ({refusal}): {other:?}");
    assert_eq!(
        logs[0].0,
        if reconnect { 4 } else { 3 },
        "the id its first attempt reserved, nothing reserved again: {other:?}"
    );
    assert_eq!((logs[0].2.as_str(), logs[0].3.as_str()), ("7", "9"));
    for opcode in [
        om_protocol::Opcode::HostCreateRecord,
        om_protocol::Opcode::HostGetSerial,
    ] {
        assert_eq!(
            owner
                .captures
                .with_opcode(opcode)
                .iter()
                .filter(|(file, _)| file == "Other.fmp12")
                .count(),
            if reconnect && opcode == om_protocol::Opcode::HostCreateRecord {
                2
            } else {
                1
            },
            "one serial issuance, a fresh wire id only for explicit recovery: {opcode:?}"
        );
    }
    let mut fresh = owner.open("Other.fmp12");
    let page = fresh.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let serial = page.fields.iter().position(|f| f.name == "Serial").unwrap();
    assert_eq!(
        page.records
            .iter()
            .find(|r| r.id == if reconnect { 4 } else { 3 })
            .unwrap()
            .values[serial]
            .as_deref(),
        Some("7")
    );
    let saved = page
        .records
        .iter()
        .find(|r| r.id == if reconnect { 4 } else { 3 })
        .unwrap();
    for name in ["Stamp", "Created"] {
        let field = page.fields.iter().position(|f| f.name == name).unwrap();
        assert!(
            saved.values[field]
                .as_ref()
                .is_some_and(|value| !value.is_empty()),
            "{name} was retained for the saved log"
        );
    }
    fresh.close();
    if new_root {
        let mut fresh = host.open("Root.fmp12");
        let page = fresh.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        assert_eq!(page.records.len(), 3);
        let root = page.records.iter().find(|row| row.id == 4).unwrap();
        for (name, expected) in [("Name", "new root"), ("Count", "9"), ("Serial", "7")] {
            let at = page.fields.iter().position(|f| f.name == name).unwrap();
            assert_eq!(root.values[at].as_deref(), Some(expected));
        }
        let issued: Vec<_> = logs[0].1.split('|').skip(1).collect();
        assert_eq!(issued.len(), 2);
        for (name, before) in ["Stamp", "Created"].into_iter().zip(issued) {
            assert!(
                !before.is_empty(),
                "the trigger sampled {name} before recovery"
            );
            let at = page.fields.iter().position(|f| f.name == name).unwrap();
            if name == "Created" {
                // Calculation text omits zero seconds; the stored timestamp
                // spells them out. Compare the issued value, not its display.
                let locale = om_data::calc::locale::Locale::en_us();
                let today = om_data::calc::Date::from_ymd(2026, 1, 1).unwrap();
                let parsed = |text: &str| {
                    om_data::calc::Timestamp::parse(text, &locale, today)
                        .unwrap()
                        .expect("issued timestamp")
                };
                assert_eq!(parsed(root.values[at].as_deref().unwrap()), parsed(before));
            } else {
                assert_eq!(
                    root.values[at].as_deref(),
                    Some(before),
                    "Root's issued {name} must survive recovery exactly"
                );
            }
        }
        fresh.close();
        for (op, expected) in [
            (om_protocol::Opcode::HostCreateRecord, 2),
            (om_protocol::Opcode::HostGetSerial, 1),
        ] {
            assert_eq!(
                host.captures
                    .with_opcode(op)
                    .iter()
                    .filter(|(file, _)| file == "Root.fmp12")
                    .count(),
                expected
            );
        }
    }
    let related = rows("Related.fmp12");
    assert_eq!(
        related.iter().filter(|r| r.1 == "first edited").count(),
        1,
        "{related:?}"
    );
}

/// A frozen window's portal while its partly applied save is unsettled
/// (#1151), on the three-file fixture: the user's edit of a Related row,
/// whose save is blocked on its host after the row's OnObjectSave logged it
/// in Other. Related's outcome is unknown and Other was not sent. Frozen,
/// the row shows the retained prepared replacing auto-entry as well as
/// the typed Name, not what Related's host still holds. Discard Unsaved
/// Changes gives it up, and the row shows the host's value again.
#[test]
fn a_frozen_portal_shows_what_its_partly_applied_save_would_write() {
    use egui_kittest::kittest::{By, Queryable};
    use om_data::DataProvider;
    let host = related::Host::new("frozen-portal", portal_save_log_files(None, false, true));
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1600.0, 900.0));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "Root open", |a| a.state().is_settled());
    common::select_layout(&mut app, "Lines");
    let line = || {
        By::new()
            .role(egui::Role::TextInput)
            .label("Remote Tasks::Name")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    common::wait_until(&mut app, "the portal's rows", |a| {
        a.query_all(line()).count() >= 2
    });
    // A prior typed value must not mask the replacing auto-entry that the
    // later Name edit prepares. This is still the same retained-save case.
    let derived = || {
        By::new()
            .role(egui::Role::TextInput)
            .label("Remote Tasks::Derived")
    };
    app.query_all(derived()).next().unwrap().click();
    common::wait_until(&mut app, "Derived editor", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(derived()).next().unwrap().type_text("manual");
    app.run_steps(2);
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "Derived edit", idle);

    app.query_all(line()).next().unwrap().click();
    common::wait_until(&mut app, "row 1's editor", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(line())
        .next()
        .unwrap()
        .type_text("first edited");
    app.run_steps(2);
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "row 1's OnObjectSave", idle);
    let barrier = host.block_saves(1);
    common::save_record(&mut app);
    common::wait_until(&mut app, "the bar", |a| {
        a.query_by_label("This record was saved in some of its files but not all.")
            .is_some()
    });
    drop(barrier);
    app.get_by_label("Related: not known whether it was saved.");
    app.get_by_label("Other: not saved yet.");
    // The save's failure is told in an alert.
    if app.state().error().is_some() {
        common::click(&mut app, "OK");
    }
    let related_value = |field: &str| {
        let mut provider = host.open("Related.fmp12");
        let page = provider.records(fixture::LAYOUT as u32, 0, 10).unwrap();
        let at = page.fields.iter().position(|f| f.name == field).unwrap();
        let name = page
            .records
            .iter()
            .find(|record| record.id == 1)
            .and_then(|record| record.values[at].clone());
        provider.close();
        name
    };
    assert_eq!(related_value("Derived").as_deref(), Some("Write tests!"));
    // op14 carries exactly the retained prepared cells, before the blocked
    // op70 could apply them. Read that intent independently of the GUI.
    let prepared = host
        .captures
        .with_opcode(om_protocol::Opcode::UploadListToHostTempLocation)
        .into_iter()
        .filter(|(file, _)| file == "Related.fmp12")
        .find_map(|(_, bytes)| {
            let upload = om_protocol::write::parse_upload_request(&bytes).unwrap();
            let tree =
                om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&upload.payload)).unwrap();
            let row = tree
                .root
                .children
                .iter()
                .find(|row| om_hbam::serial::decode(row.segment.0) == Some(1))?;
            om_hbam::record::Cell::from_node(row.get(31)?).text()?.ok()
        })
        .expect("Related's prepared Derived upload");
    assert_eq!(prepared, "first edited!");
    if let Some(path) = std::env::var_os("OM_PREPARED_SCREENSHOT") {
        app.set_render_cursor(false);
        app.render()
            .expect("render frozen portal")
            .save(path)
            .unwrap();
    }
    // Frozen: the row shows the window's change, which Related's host does
    // not hold.
    common::wait_for_label(&mut app, "first edited");
    common::wait_for_label(&mut app, &prepared);
    if let Some(path) = std::env::var_os("OM_PREPARED_SCREENSHOT") {
        app.set_render_cursor(false);
        app.render()
            .expect("render prepared portal")
            .save(path)
            .unwrap();
    }

    assert!(
        app.query_all(line()).next().is_none(),
        "the portal's rows are frozen"
    );
    fn styled_note(shape: &egui::epaint::Shape) -> bool {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "Keep styling" => text
                .galley
                .job
                .sections
                .iter()
                .any(|section| section.format.color == egui::Color32::from_rgb(221, 0, 0)),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().any(styled_note),
            _ => false,
        }
    }
    assert!(
        app.output()
            .shapes
            .iter()
            .any(|shape| styled_note(&shape.shape)),
        "an untouched prepared cell keeps its character formatting"
    );
    assert_eq!(related_value("Name").as_deref(), Some("Write tests"));
    // Discarded: the row shows the host's value, editable again.
    common::click(&mut app, "Discard Unsaved Changes");
    common::wait_until(&mut app, "the host's value", |a| {
        idle(a)
            && a.query_by_label("first edited").is_none()
            && a.query_all(line())
                .next()
                .and_then(|row| row.value())
                .as_deref()
                == Some("Write tests")
    });
    assert_eq!(related_value("Name").as_deref(), Some("Write tests"));
    assert_eq!(related_value("Derived").as_deref(), Some("Write tests!"));
    common::wait_until(&mut app, "Derived restored", |a| {
        a.query_by_label("first edited!").is_none()
            && a.query_all(
                By::new()
                    .role(egui::Role::TextInput)
                    .label("Remote Tasks::Derived"),
            )
            .any(|row| row.value().as_deref() == Some("Write tests!"))
    });
}

/// A script's transaction across files that fails after a file committed
/// (#1006): Related, submitted first, is saved, then Root's commit fails at
/// its host. The error alert names what became of each file, and Related's
/// record keeps what the commit stored.
#[test]
fn a_script_transaction_partly_saved_names_each_files_outcome() {
    use om_data::DataProvider;
    use om_scripts::ScriptStep;
    const REMOTE: u64 = 0xd0_0040;
    let scripts = [fixture::script(
        1,
        "Both files",
        vec![
            ScriptStep::new(205),
            fixture::set_field(fixture::NAME, "\"Root changed\""),
            fixture::set_field_in(REMOTE, fixture::NAME, "\"Related changed\""),
            ScriptStep::new(206),
        ],
    )];
    let mut root = fixture::tree(&scripts, true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        9,
        (fixture::OCCURRENCE, fixture::COUNT),
        0,
        (REMOTE, fixture::COUNT),
    );
    let child = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    let host = related::Host::new(
        "gui-transaction-partly-saved",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let names = |file: &str| {
        let page = host.open(file).records(1, 0, 10).unwrap();
        let at = page.fields.iter().position(|f| f.name == "Name").unwrap();
        page.records
            .iter()
            .map(|r| r.values[at].clone().unwrap_or_default())
            .collect::<Vec<_>>()
    };
    let root_before = names("Root.fmp12");
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the root file", |app| app.state().is_settled());

    let barrier = host.block_saves(0);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Both files");
    common::wait_until(&mut app, "the commit's error", |app| {
        app.state().error().is_some()
    });
    drop(barrier);
    let alert = app.state().error().unwrap().to_owned();
    assert!(alert.contains("Related: saved."), "{alert}");
    assert!(
        alert.contains("Root: not known whether it was saved."),
        "{alert}"
    );
    assert!(names("Related.fmp12").contains(&"Related changed".to_owned()));
    assert_eq!(names("Root.fmp12"), root_before);
}

/// A script that opens another file only for the user names it lists
/// (#1234): `Get ( UserName )` is Settings' User Name, as in FileMaker Pro
/// 26, where "Other" gives the name typed (desktop-client.md). Under the
/// system's name the script exits and nothing opens; once Settings gives
/// the listed name and saves it, the same script opens the file's window.
#[test]
fn settings_user_name_lets_a_script_guarded_on_it_open_another_file() {
    use egui_kittest::kittest::Queryable as _;
    use om_data::host::machine::{self, UserNameSetting};
    use om_scripts::ScriptStep;
    // The setting is the process's: put System back however this ends.
    struct System;
    impl Drop for System {
        fn drop(&mut self) {
            machine::set_user_name_setting(UserNameSetting::System);
        }
    }
    let _system = System;
    assert!(
        machine::Machine::current().user_name_override.is_none(),
        "run without OPENMAKER_USER_NAME"
    );
    const LISTED: &str = "Pat Doe";
    let mut open = ScriptStep::new(33);
    open.set_calculation(0, Some(&fixture::calc("\"Related\"")));
    let scripts = [fixture::script(
        1,
        "Go to Related",
        vec![
            fixture::with_calc(68, &format!("Get ( UserName ) <> \"{LISTED}\"")),
            fixture::exit_with("\"\""),
            ScriptStep::new(70),
            open,
        ],
    )];
    let root = fixture::tree(&scripts, true);
    let child = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    let host = related::Host::new(
        "gui-user-name-setting",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let provider = host.provider();
    let file = settings.clone();
    let mut app: common::App = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .renderer(common::renderer())
        .build_eframe(move |_| {
            om_gui::OpenMakerApp::new(Box::new(provider)).with_settings_file(file)
        });
    common::click(&mut app, "Use saved FileMaker license");
    common::fill(
        &mut app,
        "Path",
        &format!("fmnet:/127.0.0.1:{}/Root.fmp12", host.port),
    );
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the root file", |app| app.state().is_settled());
    let run = |app: &mut common::App| {
        common::open_menu(app, "Scripts");
        common::click(app, "Go to Related");
        common::wait_until(app, "the script end", |app| !app.state().script_running());
    };

    // The system's name is not listed: the script exits, nothing opens.
    run(&mut app);
    assert_eq!(app.state_mut().window_titles().len(), 1);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    // Settings (Cmd+,) > User Name > Other, the listed name, OK: saved and
    // in use.
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    app.run_steps(2);
    common::click(&mut app, "Other");
    common::fill(&mut app, "User Name", LISTED);
    common::click(&mut app, "OK");
    assert_eq!(machine::user_name(), LISTED);
    let saved = std::fs::read_to_string(&settings).unwrap();
    assert!(
        saved.contains("\"other\"") && saved.contains(LISTED),
        "{saved}"
    );
    run(&mut app);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !app
        .state_mut()
        .window_titles()
        .iter()
        .any(|t| t.starts_with("Related"))
    {
        assert!(std::time::Instant::now() < deadline, "no Related window");
        app.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.state().error().is_none(), "{:?}", app.state().error());

    // The app menu's Settings..., System again, saved as such.
    app.query_all_by_role_and_label(egui::Role::Button, om_gui::APP_TITLE)
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Settings...");
    let system = format!(
        "System: \u{201c}{}\u{201d}",
        machine::Machine::current().system_user_name
    );
    common::click(&mut app, &system);
    common::click(&mut app, "OK");
    assert_eq!(machine::user_name_setting(), UserNameSetting::System);
    assert!(std::fs::read_to_string(&settings)
        .unwrap()
        .contains("\"system\""));
}
