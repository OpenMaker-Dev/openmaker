//! Container writes through the GUI end to end: scripts run on the
//! provider worker against the native provider and an in-process
//! `om-server` hosting the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs`, whose record 1 holds library member
//! 1 (a 3 x 2 PNG named `photo2.png`) in Tasks::Photo. Insert File puts a
//! file into record 2 (op88), the commit stores the reference and the
//! window reloads the record, and a container function reads the data back
//! from the library; Insert File again replaces it, Set Field clears it and
//! copies record 1's value (op87 +1). The reference counts are read from
//! the file the server saved (containers.md 5).
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
#[path = "../../om-server/tests/fixtures/transfer.rs"]
mod transfer;

use std::path::PathBuf;

use fixture::{OCCURRENCE, PHOTO, TABLE};
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, HbamFile};
use om_scripts::{Calculation, FieldSpec, Script, ScriptStep, StoredKey};

/// `text` compiled in the Tasks context, Photo among the names.
fn calc(text: &str) -> Calculation {
    let names = fixture::photo_names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn photo_spec() -> FieldSpec {
    FieldSpec::new(
        StoredKey::from_id(OCCURRENCE).unwrap(),
        StoredKey::from_id(PHOTO).unwrap(),
        0,
    )
}

fn set_photo(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(0, Some(photo_spec()));
    step
}

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Insert File [Tasks::Photo; `name` in the session's temporary folder].
fn insert_file(name: &str) -> [ScriptStep; 2] {
    let mut step = ScriptStep::new(131);
    step.set_field_spec(0, Some(photo_spec()));
    step.flags |= om_scripts::flag::NO_INTERACT;
    [
        set_variable("$p", &format!("Get ( TemporaryPath ) & \"{name}\"")),
        transfer::with_paths(step, &["$p"]),
    ]
}

/// Show Custom Dialog [ "Containers" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Containers\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// Go to record 2 (the last), the steps, commit, then show `message` with
/// the commit's error.
fn on_record_2(id: u64, name: &str, steps: Vec<ScriptStep>, message: &str) -> Script {
    let mut all = vec![fixture::on(86, true), fixture::go_to_record(2)];
    all.extend(steps);
    all.push(ScriptStep::new(75));
    all.push(set_variable("$e", "Get ( LastError )"));
    all.push(show(&format!("\"error \" & $e & \": \" & {message}")));
    fixture::script(id, name, all)
}

const READ: &str = "GetAsText ( Tasks::Photo ) & \" \" & Length ( Tasks::Photo ) & \" \" & \
                    Base64Encode ( Tasks::Photo ) & \" \" & GetWidth ( Tasks::Photo )";

fn scripts() -> Vec<Script> {
    vec![
        on_record_2(1, "Insert note", insert_file("note.txt").to_vec(), READ),
        on_record_2(2, "Insert picture", insert_file("red.png").to_vec(), READ),
        on_record_2(
            3,
            "Clear photo",
            vec![set_photo("\"\"")],
            "IsEmpty ( Tasks::Photo )",
        ),
        on_record_2(
            4,
            "Copy photo",
            vec![
                fixture::go_to_record(1),
                set_variable("$c", "Tasks::Photo"),
                fixture::go_to_record(2),
                set_photo("$c"),
            ],
            READ,
        ),
    ]
}

/// The process's temporary folder, as `Get ( TemporaryPath )` names it.
fn temporary() -> PathBuf {
    std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id()))
}

/// Run `script`, wait for its dialog to show `expected`, and close it.
fn expect(app: &mut common::App, script: &str, expected: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, script);
    common::wait_for_label(app, expected);
    common::click(app, "OK");
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The reference count of member `member` of Tasks' library in the file
/// the server saved; `None` when the library no longer has it.
fn count(hosted: &hosted::Hosted, member: u64) -> Option<u32> {
    let file = HbamFile::open(hosted.file()).unwrap();
    let root = file.key_tree().unwrap().root.to_owned_node();
    let (t, m) = (uint_segment(TABLE), serial::encode(member));
    let leaf = root.at(&[&t, &[0x1f], &[5], &m, &[0x12]])?;
    Some(u32::from_be_bytes(
        leaf.value.as_deref()?.try_into().unwrap(),
    ))
}

/// Real Insert File prepares an existing row before the first acquisition/Save.
#[test]
fn an_inserted_attachment_on_an_existing_record_survives_explicit_reconnect() {
    use egui_kittest::kittest::{NodeT, Queryable};
    use om_protocol::Opcode;
    #[path = "../../om-provider/tests/common/related.rs"]
    mod related;
    let filename = "existing-before-save.txt";
    std::fs::create_dir_all(temporary()).unwrap();
    std::fs::write(
        temporary().join(filename),
        b"existing row unsaved attachment",
    )
    .unwrap();
    let mut steps = vec![fixture::on(86, true), fixture::go_to_record(2)];
    steps.extend(insert_file(filename));
    steps.push(show("\"attachment held \" & Get ( LastError )"));
    let mut tree = fixture::tree(&[fixture::script(1, "Insert without Save", steps)], true);
    // The actual upload creates native library/member identities; the older
    // hand-built photo library intentionally has no native identity metadata.
    fixture::add_field(&mut tree, TABLE, PHOTO, "Photo", 1, 6, 0, 0, 0);
    let host = related::Host::new("gui-existing-before-save", vec![(fixture::FILE, tree)]);
    // Observe the closed socket through the normal idle worker, without a
    // Refresh that would first try to save this unsaved attachment.
    let mut app = common::harness(Box::new(
        host.provider()
            .with_keepalive(Some(std::time::Duration::from_millis(100))),
    ));
    common::click(&mut app, "Use saved license");
    common::fill(&mut app, "Host", "127.0.0.1");
    hosted::replace(&mut app, "Port", &host.port.to_string());
    common::fill(&mut app, "File", fixture::FILE);
    common::fill(&mut app, "Account", fixture::ACCOUNT);
    common::fill(&mut app, "Password", fixture::PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "existing file", |a| a.state().is_settled());
    expect(&mut app, "Insert without Save", "attachment held 0");
    let read = || {
        let mut reader = host.open(fixture::FILE);
        let page = reader.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let index = page.fields.iter().position(|f| f.name == "Photo").unwrap();
        let value = page.records.iter().find(|r| r.id == 2).unwrap().values[index].clone();
        reader.close();
        value
    };
    assert!(
        read().is_none(),
        "Insert has not submitted the existing row"
    );
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let before = wire();
    assert_eq!(before[0], 0);
    assert_eq!(before[5], 1, "actual upload");
    host.handle.close_connections();
    common::wait_until(&mut app, "lost connection ready for reconnect", |a| {
        a.query_by_role_and_label(egui::Role::Button, "Reconnect…")
            .is_some_and(|button| !button.accesskit_node().is_disabled())
    });
    assert_eq!(wire(), before, "passive loss observation submits nothing");
    common::click(&mut app, "Reconnect…");
    common::wait_until(&mut app, "reconnect password", |a| {
        common::find_text_field(a, "Password").is_some()
    });
    common::fill(&mut app, "Password", fixture::PASSWORD);
    app.get_by_role_and_label(egui::Role::Button, "Reconnect")
        .click();
    let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        common::wait_until(&mut app, "reconnected existing row", |a| {
            assert!(a.state().error().is_none(), "{:?}", a.state().error());
            a.state().connection_lost().is_none() && a.state().is_settled()
        });
    }));
    if let Err(panic) = recovered {
        let labels: Vec<_> = app
            .query_all(egui_kittest::kittest::By::new().role(egui::Role::Label))
            .filter_map(|n| n.value())
            .collect();
        eprintln!(
            "reconnect state {:?}, labels {labels:?}, wire {:?}",
            app.state().connection_lost(),
            wire()
        );
        std::panic::resume_unwind(panic);
    }
    assert_eq!(
        wire(),
        before,
        "recovery neither reserves nor submits/reuploads/releases"
    );
    assert!(read().is_none());
    // Automatic authoring capability reads are the only op240 requests here;
    // their mutating siblings must never become a no-submission exemption.
    for (_, tx) in host.captures.transactions() {
        if tx[0] == Opcode::OpenMakerAuthoring as u8 {
            assert!(matches!(
                om_protocol::authoring::parse_request(&tx),
                Ok(om_protocol::authoring::Request::Describe)
            ));
        }
    }
    let opcodes = || {
        let mut counts = std::collections::BTreeMap::new();
        for (_, tx) in host.captures.transactions() {
            *counts.entry(tx[0]).or_insert(0usize) += 1;
        }
        counts
    };
    common::save_record(&mut app);
    common::wait_until(&mut app, "explicit attachment Save", |a| {
        assert!(
            a.state().error().is_none(),
            "{:?}; wire {:?}; opcodes {:?}",
            a.state().error(),
            wire(),
            opcodes()
        );
        a.state().is_settled() && !common::editing(a)
    });
    assert!(read().is_some());
    assert_eq!(wire()[0], 0, "existing RID retained");
    assert_eq!(wire()[5], 1, "uploaded once");
}

#[test]
fn scripts_insert_replace_clear_and_copy_container_data() {
    std::fs::create_dir_all(temporary()).unwrap();
    std::fs::write(temporary().join("note.txt"), b"hello").unwrap();
    std::fs::write(temporary().join("red.png"), fixture::png()).unwrap();
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_photo(&mut root);
    let file = fixture::TempFile::from_tree("gui-client-containers", &root);
    let hosted = hosted::host_file(
        "client-containers",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    assert_eq!(count(&hosted, 1), Some(1));

    // Insert File stores the file as member 2; after the commit the
    // record reloads and its data reads back from the library.
    expect(
        &mut app,
        "Insert note",
        "error 0: note.txt 5 aGVsbG8=\r\n 0",
    );
    assert_eq!(count(&hosted, 2), Some(1));
    // A picture replaces it: member 3, and member 2 is released (gone).
    let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAEElEQVQI12P8zwAFTDAGAwATKQEDO7FpHgAAAABJRU5ErkJggg==";
    expect(
        &mut app,
        "Insert picture",
        &format!(
            "error 0: red.png 73 {}\r\n{}\r\n 3",
            &encoded[..76],
            &encoded[76..]
        ),
    );
    assert_eq!((count(&hosted, 2), count(&hosted, 3)), (None, Some(1)));
    // Clearing releases member 3.
    expect(&mut app, "Clear photo", "error 0: 1");
    assert_eq!(count(&hosted, 3), None);
    // Record 1's value copied: one more reference to member 1.
    expect(
        &mut app,
        "Copy photo",
        &format!(
            "error 0: photo2.png 73 {}\r\n{}\r\n 3",
            &encoded[..76],
            &encoded[76..]
        ),
    );
    assert_eq!(count(&hosted, 1), Some(2));
    let _ = std::fs::remove_file(temporary().join("note.txt"));
    let _ = std::fs::remove_file(temporary().join("red.png"));
}

fn guarded_command(id: u16, calculated: bool) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_field_spec(0, Some(photo_spec()));
    step.flags |= om_scripts::flag::NO_INTERACT;
    if id == 61 {
        step.set_text(0, Some("text"));
    }
    if id == 91 {
        step.set_number(1, Some(if calculated { 3 } else { 1 }));
        if calculated {
            step.set_calculation(0, Some(&calc("Tasks::Photo")));
        }
    }
    step
}

fn guard_script(id: u64, name: &str, command: ScriptStep) -> Script {
    fixture::script(
        id,
        name,
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            command,
            set_variable("$error", "Get ( LastError )"),
            ScriptStep::new(75),
            show("\"guard \" & $error"),
        ],
    )
}

fn container_host(tag: &str, scripts: &[Script]) -> hosted::Hosted {
    let mut root = fixture::tree(scripts, true);
    fixture::add_photo(&mut root);
    let file = fixture::TempFile::from_tree(tag, &root);
    hosted::host_file(
        tag,
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    )
}

fn open_container_host(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| app.state().is_settled());
    app
}

/// Native compressed storage is read through the provider and the desktop
/// script host, then exported as the original file, not its gzip stream.
#[test]
fn exporting_a_stored_compressed_container_writes_the_original_file() {
    use om_data::calc::{Container, StreamType, Text};
    use om_hbam::OwnedNode;

    std::fs::create_dir_all(temporary()).unwrap();
    let output = temporary().join("stored-compressed-export.txt");
    let mut export = ScriptStep::new(132);
    export.flags |= om_scripts::flag::NO_INTERACT;
    export.set_field_spec(0, Some(photo_spec()));
    let script = fixture::script(
        1,
        "Export compressed",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            set_variable(
                "$p",
                "Get(TemporaryPath) & \"stored-compressed-export.txt\"",
            ),
            transfer::with_paths(export, &["$p"]),
            set_variable("$error", "Get(LastError)"),
            show("\"export error \" & $error"),
        ],
    );
    let mut root = fixture::tree(&[script], true);
    fixture::add_photo(&mut root);
    let compressed = Container {
        text: Text::plain("openmaker-oracle.txt"),
        streams: vec![(
            StreamType(*b"ZLIB"),
            include_bytes!("../../om-data/tests/fixtures/native-compressed-file.gz").to_vec(),
        )],
        path_type: Some("file"),
        ..Container::default()
    };
    let (mut nodes, md5) = om_data::containers::tree(&compressed).unwrap();
    nodes.push(OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes()));
    nodes.sort_by(|a, b| a.segment.cmp(&b.segment));
    let table = uint_segment(TABLE);
    root.put(
        &[&table, &[0x1f], &[5], &[1]],
        OwnedNode::list(vec![1], nodes),
    );
    root.put(
        &[&table, &[0x1f], &[1], &[8]],
        OwnedNode::list(
            vec![8],
            vec![OwnedNode::list(
                md5.to_vec(),
                vec![OwnedNode::leaf(vec![1], vec![])],
            )],
        ),
    );
    let file = fixture::TempFile::from_tree("compressed-container-export", &root);
    let hosted = hosted::host_file(
        "compressed-container-export",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Export compressed", "export error 0");
    let exported = std::fs::read(&output).unwrap();
    let _ = std::fs::remove_file(output);
    assert_eq!(
        exported,
        "OpenMaker native container oracle\r\nhé\n".as_bytes()
    );
    assert_eq!(count(&hosted, 1), Some(1));
}

fn photos(hosted: &hosted::Hosted) -> Vec<Option<String>> {
    let mut other = hosted.opened();
    let page = other.records(fixture::LAYOUT as u32, 0, 10).unwrap();
    let photo = page.fields.iter().position(|f| f.name == "Photo").unwrap();
    let values = page
        .records
        .into_iter()
        .map(|r| r.values[photo].clone())
        .collect();
    other.close();
    values
}

#[test]
fn raw_text_container_transfers_are_refused_without_mutating_data_or_counts() {
    let commands = [
        (46, false),
        (47, false),
        (48, false),
        (61, false),
        (12, false),
        (91, false),
        (91, true),
    ];
    let scripts: Vec<Script> = commands
        .iter()
        .enumerate()
        .map(|(n, (id, calc))| {
            guard_script(
                n as u64 + 1,
                &format!("Guard {n}"),
                guarded_command(*id, *calc),
            )
        })
        .chain([guard_script(20, "Clear safely", guarded_command(49, false))])
        .collect();
    let hosted = container_host("container-raw-transfers", &scripts);
    let before = photos(&hosted);
    let mut app = open_container_host(&hosted);
    for n in 0..commands.len() {
        expect(&mut app, &format!("Guard {n}"), "guard 3");
        assert_eq!(photos(&hosted), before);
        assert_eq!(count(&hosted, 1), Some(1));
    }
    expect(&mut app, "Clear safely", "guard 0");
    assert!(photos(&hosted).iter().all(Option::is_none));
    assert_eq!(count(&hosted, 1), None);
}

#[test]
fn set_field_by_name_copies_a_typed_container_and_takes_its_reference() {
    let mut set = ScriptStep::new(147);
    set.set_calculation(0, Some(&calc("$photo")));
    set.set_calculation(1, Some(&calc("\"Tasks::Photo\"")));
    set.flags |= 0x0800_0000; // kFieldSpecByCalc.
    let script = on_record_2(
        1,
        "Copy by name",
        vec![
            fixture::go_to_record(1),
            set_variable("$photo", "Tasks::Photo"),
            fixture::go_to_record(2),
            set,
        ],
        "GetAsText ( Tasks::Photo ) & \" \" & Length ( Tasks::Photo )",
    );
    let hosted = container_host("container-set-by-name", &[script]);
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Copy by name", "error 0: photo2.png 73");
    assert_eq!(count(&hosted, 1), Some(2));
    assert!(photos(&hosted).iter().all(Option::is_some));
}

#[test]
fn exporting_unavailable_external_data_refuses_before_truncating_the_file() {
    std::fs::create_dir_all(temporary()).unwrap();
    let path = temporary().join("unavailable-external.bin");
    std::fs::write(&path, b"keep the existing file").unwrap();
    let mut export = ScriptStep::new(132);
    export.flags |= om_scripts::flag::NO_INTERACT;
    export.set_field_spec(0, Some(photo_spec()));
    let script = fixture::script(
        1,
        "Export unavailable",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            set_variable("$p", "Get ( TemporaryPath ) & \"unavailable-external.bin\""),
            transfer::with_paths(export, &["$p"]),
            set_variable("$error", "Get ( LastError )"),
            show("\"export error \" & $error"),
        ],
    );
    let mut root = fixture::tree(&[script], true);
    fixture::add_unavailable_external_photo(&mut root);
    let file = fixture::TempFile::from_tree("container-external-export", &root);
    let hosted = hosted::host_file(
        "container-external-export",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Export unavailable", "export error 1225");
    assert_eq!(std::fs::read(&path).unwrap(), b"keep the existing file");
    let _ = std::fs::remove_file(path);
}

#[test]
fn duplicate_copies_only_stored_container_data_and_takes_one_reference() {
    let script = guard_script(1, "Duplicate containers", ScriptStep::new(8));
    let mut root = fixture::tree(&[script], true);
    fixture::add_nonwritable_photo_fields(&mut root);
    // Count a serial number: the copy takes the next one, not the
    // original's 3 (a field without a next value starts at 1).
    fixture::set_field_options(&mut root, TABLE, fixture::COUNT, 0x2);
    fixture::set_field_serial_increment(&mut root, TABLE, fixture::COUNT, 1);
    // Populate the raw page's global cell too: its classification, rather
    // than the empty-value filter, must keep it out of a duplicate.
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(1),
            &uint_segment(30),
        ],
        om_scripts::text_node(&[], "global-value"),
    );
    let file = fixture::TempFile::from_tree("container-duplicate-writable", &root);
    let hosted = hosted::host_file(
        "container-duplicate-writable",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Duplicate containers", "guard 0");
    let values = photos(&hosted);
    assert_eq!(values.len(), 3);
    assert_eq!(values.iter().filter(|v| v.is_some()).count(), 2);
    assert_eq!(count(&hosted, 1), Some(2));
    let file = HbamFile::open(hosted.file()).unwrap();
    let tree = file.key_tree().unwrap();
    for field in [30, 31] {
        assert!(
            tree.at(&[TABLE, 5, 3, field]).is_none(),
            "field {field} was stored"
        );
    }
    let count = tree
        .at(&[TABLE, 5, 3, fixture::COUNT])
        .and_then(|n| n.text());
    assert_eq!(count.and_then(Result::ok).as_deref(), Some("1"));
}

/// A duplicate is held in the window's record session (#892): the
/// container reference it took is the session's, kept while its commit is
/// refused and released when Revert Record releases the record.
#[test]
fn a_refused_duplicate_releases_the_references_it_took() {
    let mut revert = ScriptStep::new(51);
    revert.flags |= om_scripts::flag::NO_INTERACT;
    let script = fixture::script(
        1,
        "Refuse duplicate",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            ScriptStep::new(8),
            set_variable("$error", "Get ( LastError )"),
            ScriptStep::new(75),
            set_variable("$commit", "Get ( LastError )"),
            revert,
            show("\"guard \" & $error & \":\" & $commit"),
        ],
    );
    let mut root = fixture::tree(&[script], true);
    fixture::add_photo(&mut root);
    // Explicitly Always: Duplicate Record now uses main's script write
    // path, which correctly skips data-entry-only validation.
    fixture::add_field(&mut root, TABLE, fixture::CODE, "Code", 1, 1, 0, 0, 0x401);
    // An existing invalid value triggers the create's Always validation.
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(1),
            &uint_segment(fixture::CODE),
        ],
        om_scripts::text_node(&[], "TOOLONG"),
    );
    let file = fixture::TempFile::from_tree("container-duplicate-refused", &root);
    let hosted = hosted::host_file(
        "container-duplicate-refused",
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Refuse duplicate", "guard 0:507");
    assert_eq!(photos(&hosted).len(), 2);
    assert_eq!(count(&hosted, 1), Some(1));
}

#[test]
fn setting_a_container_to_its_stored_value_leaves_no_extra_reference() {
    let script = guard_script(1, "Keep photo", set_photo("Tasks::Photo"));
    let hosted = container_host("container-self-assignment", &[script]);
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Keep photo", "guard 0");
    assert_eq!(count(&hosted, 1), Some(1));
    assert_eq!(photos(&hosted).iter().filter(|v| v.is_some()).count(), 1);
}

#[test]
fn container_drafts_returning_to_the_stored_value_keep_balanced_ownership() {
    let replacement = || set_photo("Base64Decode ( \"eA==\" ; \"temporary.txt\" )");
    let mut revert = ScriptStep::new(51);
    revert.flags |= om_scripts::flag::NO_INTERACT;
    let mut rename = ScriptStep::new(76);
    rename.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(fixture::NAME).unwrap(),
            0,
        )),
    );
    rename.set_calculation(0, Some(&calc("\"a changed name\"")));
    let cases = [
        ("clear-return", set_photo("\"\""), vec![], Some(1), None),
        ("replace-return", replacement(), vec![], Some(1), None),
        (
            "other-change",
            set_photo("\"\""),
            vec![rename],
            Some(1),
            None,
        ),
        (
            "revert-return",
            set_photo("\"\""),
            vec![revert],
            Some(1),
            None,
        ),
        (
            "clear-again",
            set_photo("\"\""),
            vec![set_photo("\"\"")],
            None,
            None,
        ),
        (
            "replace-again",
            set_photo("\"\""),
            vec![replacement()],
            None,
            Some(1),
        ),
    ];
    for (tag, initial, later, original_count, replacement_count) in cases {
        let mut steps = vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            set_variable("$original", "Tasks::Photo"),
            initial,
            set_photo("$original"),
        ];
        steps.extend(later);
        steps.extend([
            ScriptStep::new(75),
            set_variable("$error", "Get ( LastError )"),
            show("\"restored \" & $error"),
        ]);
        let hosted = container_host(
            &format!("container-draft-{tag}"),
            &[fixture::script(1, "Restore photo", steps)],
        );
        let mut app = open_container_host(&hosted);
        expect(&mut app, "Restore photo", "restored 0");
        assert_eq!(count(&hosted, 1), original_count, "{tag}");
        assert_eq!(count(&hosted, 2), replacement_count, "{tag}");
        let present = photos(&hosted).iter().filter(|v| v.is_some()).count();
        assert_eq!(
            present,
            usize::from(original_count.is_some() || replacement_count.is_some()),
            "{tag}"
        );
    }
}

#[test]
fn raw_reference_strings_cannot_claim_another_records_container() {
    let script = fixture::script(
        1,
        "Refuse raw reference",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(2),
            set_photo("\"01\" & Char ( 57344 )"),
            set_variable("$error", "Get ( LastError )"),
            ScriptStep::new(75),
            show("\"raw reference \" & $error"),
        ],
    );
    let hosted = container_host("container-raw-reference", &[script]);
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Refuse raw reference", "raw reference 3");
    assert_eq!(count(&hosted, 1), Some(1));
    assert_eq!(photos(&hosted).iter().filter(|v| v.is_some()).count(), 1);
}

#[test]
fn a_failed_commit_keeps_the_restored_container_reference_until_revert() {
    let mut invalid = ScriptStep::new(76);
    invalid.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(fixture::CODE).unwrap(),
            0,
        )),
    );
    invalid.set_calculation(0, Some(&calc("\"TOOLONG\"")));
    let mut revert = ScriptStep::new(51);
    revert.flags |= om_scripts::flag::NO_INTERACT;
    let script = fixture::script(
        1,
        "Refuse restored commit",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            set_variable("$original", "Tasks::Photo"),
            set_photo("\"\""),
            set_photo("$original"),
            invalid,
            ScriptStep::new(75),
            set_variable("$error", "Get ( LastError )"),
            revert,
            show("\"failed commit \" & $error"),
        ],
    );
    let hosted = container_host("container-restored-commit-refused", &[script]);
    let mut app = open_container_host(&hosted);
    expect(&mut app, "Refuse restored commit", "failed commit 507");
    assert_eq!(count(&hosted, 1), Some(1));
    assert_eq!(photos(&hosted).iter().filter(|v| v.is_some()).count(), 1);
}

#[test]
fn reverting_a_transaction_releases_both_active_and_held_container_uploads() {
    for held in [false, true] {
        let mut steps = vec![
            fixture::go_to_record(2),
            ScriptStep::new(205),
            set_photo("Base64Decode ( \"aGVsbG8=\" ; \"temporary.txt\" )"),
        ];
        if held {
            steps.push(ScriptStep::new(75));
        }
        steps.extend([
            ScriptStep::new(207),
            ScriptStep::new(206),
            show("\"rolled back \" & Get ( LastError )"),
        ]);
        let hosted = container_host(
            if held {
                "container-revert-held"
            } else {
                "container-revert-active"
            },
            &[fixture::script(1, "Rollback upload", steps)],
        );
        let mut app = open_container_host(&hosted);
        expect(&mut app, "Rollback upload", "rolled back 0");
        assert_eq!(count(&hosted, 1), Some(1));
        assert_eq!(count(&hosted, 2), None);
        assert_eq!(photos(&hosted).iter().filter(|v| v.is_some()).count(), 1);
    }
}
