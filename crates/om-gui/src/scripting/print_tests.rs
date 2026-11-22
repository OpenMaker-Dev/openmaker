//! Focused end-to-end Print acceptance. Every dispatch is a recording fake.
use super::*;
use crate::{
    page_setup::{tests::A4, Settings},
    printing::{
        tests::{Fake, COMMON},
        Options, PageRange,
    },
    scripting::Prompt,
};

fn print(current: bool, restore: bool, dialog: bool) -> ScriptStep {
    let mut step = ScriptStep::new(43);
    step.value = if current { 2 } else { 1 };
    step.flags = if dialog { 0 } else { flag::NO_INTERACT };
    if restore {
        step.flags |= flag::RESTORE;
        step.set_data_leaf(0x88, Some(om_data::scripts::bytes_node(&[0x88], COMMON)));
    }
    step
}
fn client(steps: Vec<ScriptStep>, peer: Vec<ScriptStep>) -> (Client, Arc<Fake>) {
    let mut c = Client::new(steps, peer);
    c.server.lock().unwrap().pdf_rows = vec![7, 2, 9, 4, 8, 1, 6, 3, 5];
    let fake = Arc::new(Fake::default());
    c.session.set_print_dispatcher(fake.clone());
    (c, fake)
}
fn dirty() -> WindowState {
    WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        drafts: [("Text".into(), Some("unsaved".into()))].into(),
        focus: Some("Text".into()),
        ..Default::default()
    }
}
fn answer(c: &mut Client, accepted: Option<Options>) -> std::thread::JoinHandle<()> {
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    std::thread::spawn(move || {
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            if let worker::Reply::Script(ScriptEvent::Prompt {
                id,
                prompt: Prompt::Print(dialog),
            }) = response.reply
            {
                assert_eq!(dialog.printers, ["Synthetic;$(literal)"]);
                jobs.send(Job {
                    epoch: 1,
                    window: 1,
                    ctx: Default::default(),
                    request: Request::Answer {
                        prompt: id,
                        answer: Answer::Print(accepted),
                    },
                })
                .unwrap();
                return;
            }
        }
        panic!("Print dialog was not shown");
    })
}
#[test]
fn desktop_print_shares_vector_pdf_and_owned_file_window_paper() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("comparison.pdf");
    let mut pdf = ScriptStep::new(144);
    pdf.value = 1;
    pdf.flags = flag::NO_INTERACT | flag::FILE_PATH;
    pdf.set_slot_child(
        0,
        0x82,
        Some(om_data::text_node(
            &[0x82],
            &crate::scripting::files::file_line(&path),
        )),
    );
    let mut peer_print = print(true, true, false);
    // A job-local common format; no opaque chunks or native printer names.
    let mut letter = A4[..37].to_vec();
    for (offset, value) in [(21, 603.5f32), (25, 783.5), (29, 595.0), (33, 775.0)] {
        letter[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    peer_print.set_data_leaf(0x87, Some(om_data::scripts::bytes_node(&[0x87], &letter)));
    let (mut c, fake) = client(
        vec![
            ScriptStep::new(122),
            print(false, false, false),
            pdf,
            open("Peer", false),
            call("Peer source::Run"),
            print(true, true, false),
            exit("Get ( ScriptResult ) & \"|\" & Get ( LastError ) & \"|\" & Get ( FileName ) & \"|\" & Get ( RecordID )"),
        ],
        vec![peer_print, exit("Get ( LastError )")],
    );
    let mut settings = Settings::restored(A4).unwrap();
    settings.paper.orientation = om_layout::paper::Orientation::Landscape;
    c.session.s.file.page_setup = Some(settings.clone());
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert_eq!(c.result(), "0|0|Root|9");
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].bytes, std::fs::read(path).unwrap());
    for (job, count, media) in [
        (&jobs[0], 5, "/MediaBox [0 0 841.86 595.27]"),
        (&jobs[1], 1, "/MediaBox [0 0 612 792]"),
        (&jobs[2], 1, "/MediaBox [0 0 841.86 595.27]"),
    ] {
        assert!(
            !job.path.exists(),
            "temporary PDF must be removed after submission"
        );
        assert!(job.bytes.starts_with(b"%PDF-1.7"));
        let text = String::from_utf8_lossy(&job.bytes);
        assert!(text.contains(&format!("/Count {count}")));
        assert!(text.contains(media));
        assert!(text.contains("/FontFile2"), "embedded vector font");
        assert!(!text.contains("/Subtype /Image"));
    }
    assert!(jobs[0].title.ends_with("Root.fmp12"));
    assert!(jobs[1].title.ends_with("Peer.fmp12"));
    assert_eq!(jobs[1].options.copies, 2);
    assert_eq!(jobs[2].options.copies, 2);
    assert_eq!(c.session.s.file.page_setup, Some(settings));
    assert!(c.files.peers.values().all(|p| p.state.page_setup.is_none()));
    assert_eq!(c.session.s.file.others.len(), 1);
    if let Some(dir) = std::env::var_os("OM_PRINT_ARTIFACT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        for (i, job) in jobs.iter().enumerate() {
            std::fs::write(
                std::path::Path::new(&dir).join(format!("print-synthetic-{i}.pdf")),
                &job.bytes,
            )
            .unwrap();
        }
    }
}

#[test]
fn desktop_print_cancel_restore_refusal_and_spool_error_cleanup() {
    let (mut canceled, fake) = client(
        vec![print(false, false, true), exit("Get ( LastError )")],
        vec![],
    );
    let previous = Settings::restored(A4).unwrap();
    canceled.session.s.file.page_setup = Some(previous.clone());
    let respond = answer(&mut canceled, None);
    canceled.run_in(dirty());
    respond.join().unwrap();
    assert_eq!(canceled.result(), "1");
    assert!(fake.jobs.lock().unwrap().is_empty());
    assert_eq!(
        canceled.session.s.file.window.state.drafts["Text"].as_deref(),
        Some("unsaved")
    );
    assert_eq!(canceled.session.s.file.page_setup, Some(previous));
    assert!(!canceled
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write")));

    let (mut missing, fake) = client(
        vec![print(false, false, true), exit("Get ( LastError )")],
        vec![],
    );
    let respond = answer(
        &mut missing,
        Some(Options {
            printer: Some("Unavailable".into()),
            ..Default::default()
        }),
    );
    missing.run_in(dirty());
    respond.join().unwrap();
    assert_eq!(missing.result(), "600");
    assert!(
        fake.jobs.lock().unwrap().is_empty(),
        "no default-printer fallback"
    );
    assert_eq!(
        missing.session.s.file.window.state.drafts["Text"].as_deref(),
        Some("unsaved")
    );

    let mut opaque = print(false, true, false);
    let mut bytes = COMMON.to_vec();
    bytes.extend_from_slice(b"M_PD\0\0\0\x03raw");
    opaque.set_data_leaf(0x88, Some(om_data::scripts::bytes_node(&[0x88], &bytes)));
    let (mut refused, fake) = client(vec![opaque.clone(), exit("Get ( LastError )")], vec![]);
    refused.run_in(dirty());
    assert_eq!(refused.result(), "3");
    assert!(fake.jobs.lock().unwrap().is_empty());
    assert_eq!(
        refused.session.s.file.window.state.drafts["Text"].as_deref(),
        Some("unsaved")
    );
    // Restore off ignores stale bytes and uses defaults; an actual spool error
    // returns 600 and removes the temporary PDF without touching paper settings.
    opaque.flags &= !flag::RESTORE;
    let (mut failed, fake) = client(vec![opaque, exit("Get ( LastError )")], vec![]);
    *fake.fail.lock().unwrap() = true;
    failed.run();
    assert_eq!(failed.result(), "600");
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].options.copies, 1);
    assert!(!jobs[0].path.exists());
    assert!(failed.session.s.file.page_setup.is_none());
}

#[test]
fn desktop_print_ranges_validate_complete_report_before_submission_and_stay_job_local() {
    let (mut c, fake) = client(
        vec![
            print(false, false, true),
            print(false, false, false),
            exit("Get ( LastError )"),
        ],
        vec![],
    );
    let respond = answer(
        &mut c,
        Some(Options {
            pages: PageRange::Range { from: 2, to: 3 },
            ..Default::default()
        }),
    );
    c.run();
    respond.join().unwrap();
    assert_eq!(c.result(), "0");
    {
        let jobs = fake.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].options.pages, PageRange::Range { from: 2, to: 3 });
        assert_eq!(jobs[1].options.pages, PageRange::All);
        assert_eq!(jobs[0].bytes, jobs[1].bytes);
        assert!(String::from_utf8_lossy(&jobs[0].bytes).contains("/Count 3"));
        assert!(jobs.iter().all(|job| !job.path.exists()));
    }
    // Reject malformed bounds before commit; document bounds use the actual
    // generated plan, including the current-record choice (one physical page).
    for (from, to, current) in [(0, 1, false), (3, 2, false), (1, 4, false), (1, 2, true)] {
        let (mut c, fake) = client(
            vec![print(false, false, true), exit("Get ( LastError )")],
            vec![],
        );
        let respond = answer(
            &mut c,
            Some(Options {
                current_record: current,
                pages: PageRange::Range { from, to },
                ..Default::default()
            }),
        );
        c.run_in(dirty());
        respond.join().unwrap();
        assert_eq!(c.result(), "3");
        assert!(fake.jobs.lock().unwrap().is_empty());
        let writes = c
            .server
            .lock()
            .unwrap()
            .events
            .iter()
            .any(|e| e.ends_with(":write"));
        assert_eq!(writes, from > 0 && from <= to);
        if from > 0 && from <= to {
            let pages = if current { 1 } else { 3 };
            assert_eq!(
                c.session.s.failure.as_deref(),
                Some(
                    format!(
                        "The report has {pages} physical pages; choose a range within 1-{pages}"
                    )
                    .as_str()
                )
            );
        } else {
            assert_eq!(
                c.session.s.file.window.state.drafts["Text"].as_deref(),
                Some("unsaved")
            );
        }
    }
}

/// A stored current record prints only with "Specify print options"
/// (Restore): without them Print takes the records being browsed, as
/// FileMaker Pro 26 opens its Print dialog on them (#843; that it prints
/// them is inferred from that dialog).
#[test]
fn desktop_print_takes_a_stored_current_record_only_with_its_options() {
    let (mut c, fake) = client(
        vec![
            print(true, false, false),
            print(true, true, false),
            exit("Get ( LastError )"),
        ],
        vec![],
    );
    c.run();
    assert_eq!(c.result(), "0");
    let jobs = fake.jobs.lock().unwrap();
    let chosen: Vec<_> = jobs.iter().map(|job| job.options.current_record).collect();
    assert_eq!(chosen, [false, true]);
    assert!(String::from_utf8_lossy(&jobs[0].bytes).contains("/Count 3"));
    assert!(String::from_utf8_lossy(&jobs[1].bytes).contains("/Count 1"));
}

#[test]
fn desktop_print_commit_veto_and_external_trigger_resume_owning_window() {
    for (proceed, replace_trigger, expected, jobs) in [
        (false, false, "20", 0),
        (true, false, "0", 1),
        (true, true, "0", 1),
    ] {
        let trigger = if replace_trigger {
            vec![ScriptStep::new(62)]
        } else {
            vec![exit(if proceed { "1" } else { "0" })]
        };
        let (mut c, fake) = client(
            vec![
                var("$$signer", "\"Ada\""),
                var("$who", "\"Lovelace\""),
                print(true, false, false),
                exit("Get ( LastError )"),
            ],
            trigger,
        );
        // Print, held for the trigger, prints with the script's variables
        // when the trigger lets it (#596).
        let mut document = pdf_tests::signed_document();
        document.layout.triggers = vec![external_trigger(TriggerEvent::RecordCommit)];
        {
            let mut server = c.server.lock().unwrap();
            server.report_variables = true;
            server.refresh_render = Some(document);
            if replace_trigger {
                let mut replacement = script(vec![exit("1")]);
                replacement.id = 2;
                replacement.name = "Replacement".into();
                server
                    .scripts
                    .get_mut("Root.fmp12")
                    .unwrap()
                    .push(replacement);
            }
        }
        if replace_trigger {
            // The button exits the paused trigger and starts a new script
            // before the held Print runs. Print still belongs to its caller.
            let mut button = ScriptStep::new(1);
            button.reserved[..4].copy_from_slice(&[2, 1, 2, 0xbe]);
            button.value = 2; // Exit current script.
            let window = dirty();
            c._keep_jobs
                .send(Job {
                    epoch: 1,
                    window: window.id,
                    ctx: Default::default(),
                    request: Request::Script(Box::new(ScriptJob {
                        start: Start::Button {
                            step: Box::new(button),
                            formula: None,
                            literal: String::new(),
                            portal: None,
                        },
                        window,
                        others: Vec::new(),
                        context: SessionContext {
                            host: "host.invalid".into(),
                            file: "Root.fmp12".into(),
                            account: "root-account".into(),
                            local: None,
                        },
                    })),
                })
                .unwrap();
        }
        c.run_in(dirty());
        assert_eq!(c.result(), expected);
        assert_eq!(fake.jobs.lock().unwrap().len(), jobs);
        if !proceed {
            assert_eq!(
                c.session.s.file.window.state.drafts["Text"].as_deref(),
                Some("unsaved")
            );
            assert!(!c
                .server
                .lock()
                .unwrap()
                .events
                .iter()
                .any(|e| e.ends_with(":write")));
        } else {
            let job = &fake.jobs.lock().unwrap()[0];
            assert!(job.title.ends_with("Root.fmp12"));
            let pdf = String::from_utf8_lossy(&job.bytes);
            assert!(!pdf_tests::text_origins(&pdf, 3, "Ada / Lovelace").is_empty());
        }
    }
}

/// A custom set that may do everything but print and export.
fn no_print_no_export() -> om_data::host::security::SetAccess {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, ItemAccess, MenuCommands, Quad, TableRules,
    };
    let all = ItemAccess {
        default: Some(Quad(0xff)),
        items: BTreeMap::new(),
    };
    om_data::host::security::SetAccess::Custom(Box::new(CustomRules {
        default: TableRules {
            actions: Actions {
                create: Allow::Yes,
                view: Allow::Yes,
                edit: Allow::Yes,
                delete: Allow::Yes,
            },
            fields: AccessLevel::Modifiable,
            field_levels: BTreeMap::new(),
        },
        tables: BTreeMap::new(),
        layouts: all.clone(),
        scripts: all.clone(),
        value_lists: all,
        prints: false,
        exports: false,
        menus: MenuCommands::All,
    }))
}

/// Answers every alert the script shows with Continue, and returns each
/// alert's message and whether it offered Cancel once the script ends.
fn collect_alerts(c: &mut Client) -> std::thread::JoinHandle<Vec<(String, bool)>> {
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    std::thread::spawn(move || {
        let mut alerts = Vec::new();
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            match response.reply {
                worker::Reply::Script(ScriptEvent::Prompt {
                    id,
                    prompt:
                        Prompt::Alert {
                            message,
                            can_cancel,
                            ..
                        },
                }) => {
                    alerts.push((message, can_cancel));
                    jobs.send(Job {
                        epoch: 1,
                        window: 1,
                        ctx: Default::default(),
                        request: Request::Answer {
                            prompt: id,
                            answer: crate::scripting::Answer::Alert(
                                om_data::runtime::AlertChoice::Continue,
                            ),
                        },
                    })
                    .unwrap();
                }
                worker::Reply::Script(ScriptEvent::Finished { .. }) => break,
                _ => {}
            }
        }
        alerts
    })
}

fn without_dialog(id: u16) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.flags = flag::NO_INTERACT;
    step
}

/// FileMaker Pro 26.0.3 under a custom set with printing and exporting off,
/// errors not captured (#651's reading): Print, Save Records as PDF, Export
/// Records and Save Records as Excel return 3 with no alert; Copy All
/// Records/Requests shows the privilege alert, OK only, and returns 200;
/// Print Setup runs (here without its dialog, which does nothing). The
/// steps run without their dialogs, so a step that is not refused fails
/// some other way rather than waiting for one.
#[test]
fn steps_a_set_denies_return_filemakers_errors_and_alerts() {
    // Set Error Capture [Off], after the harness's On.
    let mut c = Client::new(
        vec![
            ScriptStep::new(86),
            without_dialog(43),
            var("$print", "Get ( LastError )"),
            without_dialog(144),
            var("$pdf", "Get ( LastError )"),
            without_dialog(36),
            var("$export", "Get ( LastError )"),
            without_dialog(143),
            var("$excel", "Get ( LastError )"),
            ScriptStep::new(98),
            var("$copy", "Get ( LastError )"),
            without_dialog(42),
            exit(
                "$print & \"|\" & $pdf & \"|\" & $export & \"|\" & $excel & \"|\" & $copy & \"|\" & Get ( LastError )",
            ),
        ],
        vec![],
    );
    c.server.lock().unwrap().set_access = Some(no_print_no_export());
    let alerts = collect_alerts(&mut c);
    c.run();
    assert_eq!(c.result(), "3|3|3|3|200|0");
    assert_eq!(
        alerts.join().unwrap(),
        [(om_data::PRIVILEGE_DENIED.to_owned(), false)]
    );
}

/// A script set to run with full access may save records as PDF under the
/// same set; when the step then fails for another reason (appending, which
/// OpenMaker does not do), that error keeps its alert, though the
/// script's elevation may have ended by then.
#[test]
fn an_elevated_steps_own_error_keeps_its_alert() {
    let temp = tempfile::tempdir().unwrap();
    let mut append = ScriptStep::new(144);
    append.value = 1;
    append.flags = flag::NO_INTERACT | flag::FILE_PATH | flag::OPTION;
    append.set_slot_child(
        0,
        0x82,
        Some(om_data::text_node(
            &[0x82],
            &crate::scripting::files::file_line(&temp.path().join("out.pdf")),
        )),
    );
    let mut c = Client::new(vec![ScriptStep::new(86), append], vec![]);
    {
        let mut server = c.server.lock().unwrap();
        server.set_access = Some(no_print_no_export());
        server.scripts.get_mut("Root.fmp12").unwrap()[0].set_runs_with_full_access(true);
    }
    let alerts = collect_alerts(&mut c);
    c.run();
    let alerts = alerts.join().unwrap();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert!(alerts[0].0.ends_with("(error 3)"), "{alerts:?}");
}
