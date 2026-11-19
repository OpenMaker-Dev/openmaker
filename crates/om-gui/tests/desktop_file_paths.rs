//! Independent synthetic TLS native-provider -> GUI worker -> runtime chain.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
#[path = "../../om-provider/tests/common/related.rs"]
mod related;
use egui_kittest::kittest::{By, Queryable};
use om_scripts::{Calculation, ScriptStep};
fn calc(text: &str) -> Calculation {
    let names = fixture::photo_names();
    let mut cx = om_formula::Context::new(&names);
    cx.occurrence = Some(fixture::OCCURRENCE);
    let compiled = om_formula::compile(text, &cx).unwrap();
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}
fn call(id: u64, external: bool, param: &str) -> ScriptStep {
    let mut s = fixture::perform_script(id);
    if external {
        s.set_data_leaf(
            32,
            Some(om_data::runtime::OwnedNode::leaf([32], [2, 0x80, 0x01])),
        );
    }
    s.set_calculation(0, Some(&calc(param)));
    s
}
fn exit(expression: &str) -> ScriptStep {
    let mut s = ScriptStep::new(103);
    s.set_calculation(0, Some(&calc(expression)));
    s
}
fn show(expression: &str) -> ScriptStep {
    let mut s = ScriptStep::new(87);
    s.set_calculation(0, Some(&calc("\"File routing\"")));
    s.set_calculation(1, Some(&calc(expression)));
    s.set_text(0, Some("OK"));
    s
}
fn source(root: &mut om_hbam::OwnedNode, name: &str, file: &str) {
    let raw = om_hbam::slot::uint_segment(0x8001);
    root.put(
        &[&[32], &[5], &raw, &[16]],
        om_scripts::text_node(&[], name),
    );
    let mut bytes = 1u32.to_be_bytes().to_vec();
    for text in ["file", "", file] {
        bytes.extend(om_scsu::encode_counted(text));
    }
    root.put(
        &[&[32], &[5], &raw, &[0x82]],
        om_hbam::OwnedNode::leaf(vec![], bytes),
    );
}

#[test]
fn external_script_uses_the_first_available_source_path() {
    let root_scripts = [
        fixture::script(
            1,
            "Resolve alternatives",
            vec![
                fixture::on(86, true),
                call(2, true, "\"parameter\""),
                show("Get ( ScriptResult )"),
            ],
        ),
        fixture::script(2, "Target", vec![exit("\"WRONG ROOT FALLBACK\"")]),
    ];
    let peer_scripts = [fixture::script(
        2,
        "Target",
        vec![exit("\"PREFERRED PEER\"")],
    )];
    let mut root = fixture::tree(&root_scripts, true);
    source(&mut root, "Alternatives", "Peer");
    let raw = om_hbam::slot::uint_segment(0x8001);
    let mut bytes = 2u32.to_be_bytes().to_vec();
    for file in ["Peer", "Root"] {
        for text in ["file", "", file] {
            bytes.extend(om_scsu::encode_counted(text));
        }
    }
    root.put(
        &[&[32], &[5], &raw, &[0x82]],
        om_hbam::OwnedNode::leaf(vec![], bytes),
    );
    let host = related::Host::new(
        "desktop-script-source-alternatives",
        vec![
            ("Root.fmp12", root),
            ("Peer.fmp12", fixture::tree(&peer_scripts, true)),
        ],
    );
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved license");
    common::fill(&mut app, "Host", "127.0.0.1");
    hosted::replace(&mut app, "Port", &host.port.to_string());
    common::fill(&mut app, "File", "Root.fmp12");
    common::fill(&mut app, "Account", fixture::ACCOUNT);
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "root ready", |a| a.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Resolve alternatives");
    common::wait_until(&mut app, "resolved target report", |a| {
        a.query(By::new().label("PREFERRED PEER")).is_some()
            || a.query(By::new().label("WRONG ROOT FALLBACK")).is_some()
    });
    assert!(
        app.query(By::new().label("PREFERRED PEER")).is_some(),
        "native external call selected the already-open fallback file"
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Peer.fmp12"), Some(&1));
}

/// Both files reuse script 2; a trigger's stored reference owns that id.
fn external_layout_trigger_fixture(event: om_data::runtime::TriggerEvent) -> related::Host {
    let root_scripts = [
        fixture::script(
            1,
            "Leave root layout",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(2),
                show("\"ROOT CONTINUED\" & Get ( LayoutName )"),
            ],
        ),
        fixture::script(2, "Trigger", vec![show("\"WRONG LOCAL TRIGGER\"")]),
    ];
    let peer_scripts = [fixture::script(
        2,
        "Trigger",
        vec![
            show("Get ( FileName ) & \"|\" & Get ( ScriptParameter )"),
            exit("1"),
        ],
    )];
    let mut root = fixture::tree(&root_scripts, true);
    source(&mut root, "Trigger source", "Peer");
    let trigger = om_layout::ScriptTrigger {
        event: event.id(),
        script: Some(2),
        file_reference: Some(129),
        external: true,
        modes: 1,
        option: 0,
        extra: vec![],
        parameter: None,
    };
    let event_key = om_hbam::slot::uint_segment(u64::from(event.id()));
    root.put(
        &[&[4], &[5], &[1], &[0x30], &[4]],
        om_hbam::OwnedNode::leaf([], 1u16.to_be_bytes()),
    );
    root.put(
        &[&[4], &[5], &[1], &[0x30], &[5], &event_key, &[1]],
        om_hbam::OwnedNode::leaf([], om_layout::triggers::encode_record(&trigger)),
    );
    let parameter = calc("Get ( FileName ) & \"|\" & Name");
    root.put(
        &[&[4], &[5], &[1], &[0x30], &[5], &event_key, &[0x31], &[6]],
        om_hbam::OwnedNode::leaf([], parameter.polish.unwrap()),
    );
    related::Host::new(
        "desktop-external-layout-trigger",
        vec![
            ("Root.fmp12", root),
            ("Peer.fmp12", fixture::tree(&peer_scripts, true)),
        ],
    )
}

fn open_trigger_fixture(host: &related::Host) -> common::App {
    let mut app = common::harness(Box::new(host.provider()));
    common::click(&mut app, "Use saved license");
    common::fill(&mut app, "Host", "127.0.0.1");
    hosted::replace(&mut app, "Port", &host.port.to_string());
    common::fill(&mut app, "File", "Root.fmp12");
    common::fill(&mut app, "Account", fixture::ACCOUNT);
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "Open");
    app
}

#[test]
fn user_layout_entry_runs_the_external_trigger_with_the_origin_parameter() {
    let host = external_layout_trigger_fixture(om_data::runtime::TriggerEvent::LayoutEnter);
    let mut app = open_trigger_fixture(&host);
    trigger_report(&mut app, "Peer|Root|Write tests");
    assert!(app.query(By::new().label("WRONG LOCAL TRIGGER")).is_none());
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "external trigger finished", |a| {
        !a.state().script_running()
    });
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Peer.fmp12"), Some(&1));
}

#[test]
fn scripted_layout_exit_resumes_the_origin_action_after_the_external_trigger() {
    let host = external_layout_trigger_fixture(om_data::runtime::TriggerEvent::LayoutExit);
    let mut app = open_trigger_fixture(&host);
    common::wait_until(&mut app, "root ready", |a| a.state().is_settled());
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Leave root layout");
    trigger_report(&mut app, "Peer|Root|Write tests");
    common::click(&mut app, "OK");
    common::wait_for_label(&mut app, "ROOT CONTINUEDNotes");
    assert!(app.query(By::new().label("WRONG LOCAL TRIGGER")).is_none());
}

fn trigger_report(app: &mut common::App, expected: &str) {
    common::wait_until(app, "trigger report", |a| {
        a.query_by_label("File routing").is_some() || a.state().error().is_some()
    });
    assert!(
        app.query_by_label(expected).is_some(),
        "expected {expected}; error {:?}; values {:?}",
        app.state().error(),
        app.query_all(By::new().role(egui::Role::Label))
            .map(|n| n.value())
            .collect::<Vec<_>>()
    );
}

/// Select Window [ Name ], or [ Current Window ; Current file ] without a
/// name.
fn select_window(name: Option<&str>) -> ScriptStep {
    let mut s = ScriptStep::new(123);
    match name {
        Some(name) => {
            s.value = 1;
            s.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
        }
        None => s.set_flag(om_scripts::flag::LIMIT_TO_CURRENT_FILE, true),
    }
    s
}

/// A script performed in another hosted file opens that file in a hidden
/// state. Select Window from the calling file does not find its window
/// (Claris, Select Window: it "does not open a window of a related file
/// when the related file is opened in a hidden state"), but run from
/// within the hidden file it shows it ("Select Window can make visible a
/// hidden related file if the script step is executed from within the
/// hidden file"), and the caller can then select it by name (#1313). Also
/// when the file's script froze its window first: the window then shows,
/// in front, only as the script ends.
#[test]
fn a_file_opened_hidden_for_its_script_shows_when_that_script_selects_its_window() {
    for frozen in [false, true] {
        let root_scripts = [
            fixture::script(
                1,
                "Select peer",
                vec![
                    fixture::on(86, true),
                    call(2, true, "\"\""),
                    select_window(Some("Peer")),
                    show("Get ( LastError ) & \"|\" & WindowNames"),
                ],
            ),
            fixture::script(
                3,
                "Go to peer",
                vec![call(3, true, "\"\""), select_window(Some("Peer"))],
            ),
        ];
        let bring_forward = if frozen {
            vec![ScriptStep::new(79), select_window(None)]
        } else {
            vec![select_window(None)]
        };
        let peer_scripts = [
            fixture::script(2, "Stay hidden", vec![exit("1")]),
            fixture::script(3, "Bring forward", bring_forward),
        ];
        let mut root = fixture::tree(&root_scripts, true);
        source(&mut root, "Peer", "Peer");
        let host = related::Host::new(
            "desktop-external-script-select-window",
            vec![
                ("Root.fmp12", root),
                ("Peer.fmp12", fixture::tree(&peer_scripts, true)),
            ],
        );
        let mut app = open_trigger_fixture(&host);
        common::wait_until(&mut app, "root ready", |a| a.state().is_settled());
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Select peer");
        trigger_report(&mut app, "112|Root");
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "the first script's end", |a| {
            !a.state().script_running()
        });
        assert_eq!(
            app.state_mut().window_titles().len(),
            1,
            "the peer stays hidden"
        );
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Go to peer");
        common::wait_until(&mut app, "the second script's end or alert", |a| {
            !a.state().script_running() || a.state().error().is_some()
        });
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        // Front to back: the selected window in front.
        assert_eq!(
            app.state_mut().window_titles(),
            ["Peer (127.0.0.1)", "Root (127.0.0.1)"],
            "frozen {frozen}"
        );
        let (opens, _) = host.captures.opens_and_logins();
        assert_eq!(opens.get("Peer.fmp12"), Some(&1));
    }
}
