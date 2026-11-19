//! A few desktop integration checks over the existing synthetic report provider.
use super::*;
use crate::page_setup::{tests::A4, Settings};
use crate::scripting::Prompt;

fn setup(dialog: bool, landscape: bool) -> ScriptStep {
    let mut bytes = A4.to_vec();
    bytes[8] = if landscape { 2 } else { 0 };
    let mut step = ScriptStep::new(42);
    step.flags = flag::RESTORE | if dialog { 0 } else { flag::NO_INTERACT };
    step.set_data_leaf(0x87, Some(om_data::scripts::bytes_node(&[0x87], &bytes)));
    step
}
fn pdf(path: &std::path::Path) -> ScriptStep {
    let mut step = ScriptStep::new(144);
    step.value = 1;
    step.flags = flag::NO_INTERACT | flag::FILE_PATH;
    step.set_slot_child(
        0,
        0x82,
        Some(om_data::text_node(
            &[0x82],
            &format!("file:{}", path.display()),
        )),
    );
    step
}
#[test]
fn page_setup_script_restore_drives_pdf_and_external_file_ownership() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("page-setup-a4-landscape.pdf");
    let mut last = ScriptStep::new(16);
    last.value = 2;
    let mut c = Client::new(
        vec![
            setup(false, true),
            ScriptStep::new(122),
            open("Peer", false),
            call("Peer source::Run"),
            pdf(&path),
            ScriptStep::new(41),
            last,
            exit("Get ( LastError ) & \"|\" & Get ( PageNumber )"),
        ],
        vec![setup(false, false), exit("Get ( LastError )")],
    );
    c.server.lock().unwrap().pdf_rows = vec![7, 2, 9, 4, 8, 1, 6, 3, 5];
    c.run();
    assert_eq!(c.result(), "0|5");
    assert_eq!(c.session.s.file.window.state.preview_page, 4);
    let root = c.session.s.file.page_setup.as_ref().unwrap();
    assert_eq!(
        root.paper.orientation,
        om_layout::paper::Orientation::Landscape
    );
    assert_eq!(
        c.files
            .peers
            .values()
            .next()
            .unwrap()
            .state
            .page_setup
            .as_ref()
            .unwrap()
            .paper
            .orientation,
        om_layout::paper::Orientation::Portrait
    );
    assert_eq!(c.session.s.file.others.len(), 1);
    let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert!(
        text.contains("/Count 5"),
        "A4 landscape fits two complete bodies per page"
    );
    assert!(
        text.contains("/MediaBox [0 0 841.86 595.27]"),
        "{text:.400}"
    );
    assert_eq!(
        c.events()
            .iter()
            .filter(|e| matches!(e, ScriptEvent::PaperSettings { .. }))
            .count(),
        2
    );
}

#[test]
fn page_setup_dialog_cancel_invalid_restore_and_current_noop_preserve_dirty_record() {
    let mut invalid = setup(false, true);
    let mut bad = A4.to_vec();
    bad[9..13].copy_from_slice(&0.5f32.to_be_bytes());
    invalid.set_data_leaf(0x87, Some(om_data::scripts::bytes_node(&[0x87], &bad)));
    let mut noop = invalid.clone();
    noop.flags &= !flag::RESTORE;
    let mut c = Client::new(
        vec![
            setup(true, true),
            var("$cancel", "Get ( LastError )"),
            invalid,
            var("$invalid", "Get ( LastError )"),
            noop,
            exit("$cancel & \"|\" & $invalid & \"|\" & Get ( LastError )"),
        ],
        vec![],
    );
    let previous = Settings::restored(A4).unwrap();
    c.session.s.file.page_setup = Some(previous.clone());
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    let cancel = std::thread::spawn(move || {
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            if let worker::Reply::Script(ScriptEvent::Prompt {
                id,
                prompt: Prompt::PageSetup(settings),
            }) = response.reply
            {
                assert_eq!(
                    settings.paper.orientation,
                    om_layout::paper::Orientation::Landscape
                );
                jobs.send(Job {
                    epoch: 1,
                    window: 1,
                    ctx: Default::default(),
                    request: Request::Answer {
                        prompt: id,
                        answer: Answer::PageSetup(None),
                    },
                })
                .unwrap();
                return;
            }
        }
        panic!("Page Setup did not show a dialog");
    });
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(1),
        index: 0,
        drafts: [("Text".into(), Some("unsaved".into()))].into(),
        focus: Some("Text".into()),
        ..Default::default()
    });
    cancel.join().unwrap();
    assert_eq!(c.result(), "1|3|0");
    assert_eq!(c.session.s.file.page_setup, Some(previous));
    assert_eq!(
        c.session.s.file.window.state.drafts["Text"].as_deref(),
        Some("unsaved")
    );
    assert_eq!(c.session.s.file.window.state.focus.as_deref(), Some("Text"));
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write")));
}
