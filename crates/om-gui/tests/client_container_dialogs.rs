//! Inserting a file into a container field and exporting one through the
//! desktop's menus and the script steps without a path, end to end: the
//! in-process `om-server` hosts the Tasks file of
//! `om-server/tests/fixtures/tasks.rs` (record 1 "Write tests" holds
//! library member 1, `photo2.png`, in Tasks::Photo; record 2 "Ship" has no
//! photo), whose layout shows Tasks::Name and Tasks::Photo. A click selects
//! the container field; Insert > File... and Edit > Export Field
//! Contents... run the steps on the provider worker, which ask for the file
//! in an in-app dialog. The data goes through the provider's library path
//! (op88/op87) and the record's commit; the reference counts and values
//! are read from what the server saved. A file on this computer opened
//! read-only exports but never inserts.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::{Path, PathBuf};

use egui::accesskit::Toggled;
use egui::Role;
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use egui_kittest::Harness;
use fixture::{OCCURRENCE, PHOTO, TABLE};
use om_data::{DataError, DataProvider};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::OpenMakerApp;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, HbamFile, OwnedNode};
use om_scripts::{Calculation, FieldSpec, Script, ScriptStep, StoredKey};

/// The layout's objects: Tasks::Name and Tasks::Photo.
const NAME_OBJECT: u64 = 1;
const PHOTO_OBJECT: u64 = 2;

/// A minimal protobuf writer for the layout's object store
/// (layout-model.md 5; `docs/re/protobuf/LayoutObject.proto`).
fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
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

fn field_object(id: u64, field: u64, [top, left, bottom, right]: [f64; 4]) -> Vec<u8> {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let mut bounds = Vec::new();
    for (number, value) in [(1, top), (2, left), (3, bottom), (4, right)] {
        double(number, value, &mut bounds);
    }
    let mut spec = Vec::new();
    uint(1, alias, &mut spec);
    uint(2, field, &mut spec);
    let mut field_obj = Vec::new();
    bytes(2, &spec, &mut field_obj);
    let mut object = Vec::new();
    uint(1, 1, &mut object);
    uint(2, id, &mut object);
    bytes(4, &bounds, &mut object);
    bytes(8, &field_obj, &mut object);
    object
}

/// Layout 1 (Tasks) shows Tasks::Name above Tasks::Photo.
fn add_photo_layout(root: &mut OwnedNode) {
    let alias = serial::decode(&uint_segment(OCCURRENCE)).unwrap();
    let layout = uint_segment(fixture::LAYOUT);
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
    double(5, 220.0, &mut body);
    root.put(
        &[&[4], &[5], &layout, &[3], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), body),
    );
    let mut store = Vec::new();
    uint(1, NAME_OBJECT, &mut store);
    uint(1, PHOTO_OBJECT, &mut store);
    bytes(
        2,
        &field_object(NAME_OBJECT, fixture::NAME, [20.0, 10.0, 40.0, 210.0]),
        &mut store,
    );
    bytes(
        2,
        &field_object(PHOTO_OBJECT, PHOTO, [60.0, 10.0, 160.0, 210.0]),
        &mut store,
    );
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
}

/// OnObjectExit of the Name field runs `script`.
fn add_name_exit_trigger(root: &mut OwnedNode, script: u64) {
    let event = om_data::runtime::TriggerEvent::ObjectExit.id();
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
    let (layout, object) = (uint_segment(fixture::LAYOUT), serial::encode(NAME_OBJECT));
    let cache: [&[u8]; 6] = [&[4], &[5], &layout, &[5], &object, &[0x30]];
    let mut count = cache.to_vec();
    count.push(&[4]);
    root.put(
        &count,
        OwnedNode::leaf(Vec::new(), 1u16.to_be_bytes().to_vec()),
    );
    let event = uint_segment(u64::from(event));
    let mut entry = cache.to_vec();
    entry.extend([&[5][..], &event, &[1]]);
    root.put(&entry, OwnedNode::leaf(Vec::new(), record));
}

/// The Tasks file with Photo, the layout, `scripts` and `extra`.
fn tree(scripts: &[Script], extra: impl FnOnce(&mut OwnedNode)) -> OwnedNode {
    let mut root = fixture::tree(scripts, true);
    fixture::add_photo(&mut root);
    add_photo_layout(&mut root);
    extra(&mut root);
    root
}

fn host(tag: &str, root: &OwnedNode) -> hosted::Hosted {
    let file = fixture::TempFile::from_tree(tag, root);
    hosted::host_file(
        tag,
        &file.0,
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    )
}

fn open(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        // The field object appears before its library member's metadata
        // arrives. Wait for the initial filename, not just the control.
        app.state().is_settled() && photo_shows(app) == "photo2.png"
    });
    app
}

/// The Photo container field object.
fn photo(app: &common::App) -> Option<egui_kittest::Node<'_>> {
    app.query_by_role_and_label(Role::Button, "Photo")
}

/// What the Photo field shows: its file name.
fn photo_shows(app: &common::App) -> String {
    photo(app).and_then(|p| p.value()).unwrap_or_default()
}

fn photo_selected(app: &common::App) -> bool {
    photo(app).is_some_and(|p| p.accesskit_node().toggled() == Some(Toggled::True))
}

fn select_photo(app: &mut common::App) {
    photo(app).expect("the Photo field").click();
    app.run_steps(2);
    common::wait_until(app, "the Photo field selected", |app| {
        photo_selected(app) && !app.state().script_running()
    });
}

/// Whether menu `menu`'s item `item` can be chosen; the menu closes again.
fn menu_enabled(app: &mut common::App, menu: &str, item: &str) -> bool {
    settle(app);
    common::open_menu(app, menu);
    common::wait_for_label(app, item);
    let enabled = !app.get_by_label(item).accesskit_node().is_disabled();
    // Opening the menu can start the active field's asynchronous exit
    // trigger. Let its veto finish before Escape or another menu click.
    settle(app);
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    settle(app);
    enabled
}

fn choose(app: &mut common::App, menu: &str, item: &str) {
    common::open_menu(app, menu);
    common::click(app, item);
}

/// A button of the dialog on screen; the menu bar has an "Insert" too.
fn press(app: &mut common::App, label: &str) {
    app.get_all_by_role_and_label(Role::Button, label)
        .last()
        .expect("the dialog's button")
        .click();
    app.run_steps(2);
}

fn settle(app: &mut common::App) {
    common::wait_until(app, "the step to end", |app| {
        app.state().is_settled() && !app.state().script_running()
    });
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

/// Each record's stored Name and Photo, through another connection.
fn stored(hosted: &hosted::Hosted) -> Vec<(Option<String>, Option<String>)> {
    let mut other = hosted.opened();
    let page = other.records(fixture::LAYOUT as u32, 0, 10).unwrap();
    let column = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let (name, photo) = (column("Name"), column("Photo"));
    let values = page
        .records
        .iter()
        .map(|r| (r.values[name].clone(), r.values[photo].clone()))
        .collect();
    other.close();
    values
}

fn reference(member: u8) -> Option<String> {
    Some(om_data::containers::reference_text(&[member]))
}

/// 512 bytes, every byte value twice: exact binary, not text.
fn blob() -> Vec<u8> {
    (0..=255u8).rev().chain(0..=255).collect()
}

fn photo_spec() -> FieldSpec {
    FieldSpec::new(
        StoredKey::from_id(OCCURRENCE).unwrap(),
        StoredKey::from_id(PHOTO).unwrap(),
        0,
    )
}

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

/// Show Custom Dialog [ "Containers" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Containers\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// The Name field's value as the window shows it.
fn name(app: &common::App) -> Option<String> {
    common::text_field(app, "Name").value()
}

/// The menus insert into and export the selected container field of the
/// current record only. Cancel changes nothing, a typed value of the
/// record is kept and saved with the file, and an existing output file is
/// replaced only when the user says so; the data reads back exactly after
/// the file is opened again.
#[test]
fn the_menus_insert_and_export_the_selected_container_field() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("blob.bin");
    std::fs::write(&source, blob()).unwrap();
    let output = temporary.path().join("exported.bin");
    let hosted = host(
        "container-menus",
        &tree(&[fixture::script(1, "Start", Vec::new())], |_| {}),
    );
    let mut app = open(&hosted);
    assert_eq!(photo_shows(&app), "photo2.png");
    // Nothing selected: the commands are off.
    assert!(!menu_enabled(&mut app, "Insert", "File..."));
    assert!(!menu_enabled(&mut app, "Edit", "Export Field Contents..."));
    assert!(!menu_enabled(&mut app, "Insert", "PDF..."));

    common::click(&mut app, "Next");
    common::wait_until(&mut app, "record 2", |app| {
        name(app).as_deref() == Some("Ship") && photo_shows(app).is_empty()
    });
    hosted::replace(&mut app, "Name", "Shipped");
    select_photo(&mut app);
    assert_eq!(name(&app).as_deref(), Some("Shipped"));
    for item in ["Picture...", "File..."] {
        assert!(menu_enabled(&mut app, "Insert", item), "{item}");
    }
    for item in ["Audio/Video...", "PDF..."] {
        assert!(!menu_enabled(&mut app, "Insert", item), "{item}");
    }

    // Cancel: nothing uploaded, nothing stored, the typing kept.
    choose(&mut app, "Insert", "File...");
    common::wait_for_label(&mut app, "Insert File");
    assert!(app.query_by_label("Into field Tasks::Photo").is_some());
    app.key_press(egui::Key::Escape);
    settle(&mut app);
    assert!(app.query_by_label("Insert File").is_none());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(count(&hosted, 2), None);
    assert_eq!(photo_shows(&app), "");
    assert_eq!(name(&app).as_deref(), Some("Shipped"));

    // Insert: the file is uploaded for the draft, stored at the commit.
    choose(&mut app, "Insert", "File...");
    common::wait_for_label(&mut app, "Insert File");
    common::fill(&mut app, "File:", source.to_str().unwrap());
    press(&mut app, "Insert");
    settle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::wait_until(&mut app, "the file name", |app| {
        photo_shows(app) == "blob.bin"
    });
    assert_eq!(count(&hosted, 2), Some(1));
    assert_eq!(stored(&hosted)[1], (Some("Ship".into()), None));
    common::save_record(&mut app);
    common::wait_until(&mut app, "the save", |app| {
        app.state().is_settled() && !common::editing(app)
    });
    assert_eq!(
        stored(&hosted),
        [
            (Some("Write tests".into()), reference(1)),
            (Some("Shipped".into()), reference(2)),
        ]
    );
    assert_eq!((count(&hosted, 1), count(&hosted, 2)), (Some(1), Some(1)));

    // Export: the container's own name is offered; an existing file is
    // replaced only after the user agrees.
    std::fs::write(&output, b"sentinel").unwrap();
    for replace in [false, true] {
        select_photo(&mut app);
        choose(&mut app, "Edit", "Export Field Contents...");
        common::wait_for_label(&mut app, "Export Field Contents");
        assert_eq!(
            common::text_field(&app, "Save as:").value().as_deref(),
            Some("file:blob.bin")
        );
        hosted::replace(&mut app, "Save as:", output.to_str().unwrap());
        press(&mut app, "Save");
        common::wait_for_label(&mut app, "Replace");
        press(&mut app, if replace { "Replace" } else { "Cancel" });
        settle(&mut app);
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        let expected = if replace {
            blob()
        } else {
            b"sentinel".to_vec()
        };
        assert_eq!(std::fs::read(&output).unwrap(), expected);
    }
    drop(app);

    // Opened again: the stored data exports exactly.
    let mut app = open(&hosted);
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "record 2", |app| photo_shows(app) == "blob.bin");
    select_photo(&mut app);
    let again = temporary.path().join("again.bin");
    choose(&mut app, "Edit", "Export Field Contents...");
    common::wait_for_label(&mut app, "Export Field Contents");
    hosted::replace(&mut app, "Save as:", again.to_str().unwrap());
    press(&mut app, "Save");
    settle(&mut app);
    assert_eq!(std::fs::read(&again).unwrap(), blob());
    assert_eq!(count(&hosted, 1), Some(1));
}

/// The Insert commands for interactive containers reject the wrong file
/// type before starting an edit, while Cancel leaves the attachment intact.
#[test]
fn interactive_container_menus_preserve_the_attachment_on_rejection() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("note.txt");
    std::fs::write(&source, b"not a PDF or media file").unwrap();
    // Named as a PDF, but text.
    let named = temporary.path().join("report.pdf");
    std::fs::write(&named, b"not a PDF either").unwrap();
    let hosted = host(
        "interactive-container-menus",
        &tree(&[], |root| {
            let layout = uint_segment(fixture::LAYOUT);
            let object = serial::encode(PHOTO_OBJECT);
            let mut format = vec![0; 213];
            format[210..212].copy_from_slice(
                &(om_layout::GraphicFormat::DEFAULT.0 | om_layout::GraphicFormat::INTERACTIVE)
                    .to_be_bytes(),
            );
            root.put(
                &[&[4], &[5], &layout, &[5], &object, &[13]],
                OwnedNode::leaf(Vec::new(), format),
            );
        }),
    );
    let mut app = open(&hosted);
    for (item, title, wrong) in [
        ("PDF...", "Insert PDF", &named),
        ("Audio/Video...", "Insert Audio/Video", &source),
    ] {
        for cancel in [true, false] {
            select_photo(&mut app);
            assert!(menu_enabled(&mut app, "Insert", item), "{item}");
            choose(&mut app, "Insert", item);
            common::wait_for_label(&mut app, title);
            if cancel {
                app.key_press(egui::Key::Escape);
            } else {
                common::fill(&mut app, "File:", wrong.to_str().unwrap());
                press(&mut app, "Insert");
                common::wait_until(&mut app, "the wrong-file-type alert", |app| {
                    app.query(By::new().label_contains("(error 700)")).is_some()
                });
                app.key_press(egui::Key::Enter);
            }
            settle(&mut app);
            assert!(app.query_by_label(title).is_none());
            assert_eq!(photo_shows(&app), "photo2.png");
            assert!(!common::editing(&app));
            assert_eq!(stored(&hosted)[0].1, reference(1));
            assert_eq!((count(&hosted, 1), count(&hosted, 2)), (Some(1), None));
        }
    }
}

/// Insert File, Insert Picture and Export Field Contents without a path
/// ask for the file. Cancel is error 1, a file Insert Picture cannot show
/// is 734, and neither changes anything.
#[test]
fn scripts_without_a_path_ask_for_the_file() {
    let temporary = tempfile::tempdir().unwrap();
    let note = temporary.path().join("note.txt");
    std::fs::write(&note, b"hello").unwrap();
    let output = temporary.path().join("note-copy.txt");
    let report = "\"error \" & $e & \": \" & GetAsText ( Tasks::Photo ) & \" \" & \
                  Length ( Tasks::Photo )";
    let mut insert = ScriptStep::new(131);
    insert.set_field_spec(0, Some(photo_spec()));
    let mut go_to_photo = ScriptStep::new(17);
    go_to_photo.set_field_spec(0, Some(photo_spec()));
    let mut export = ScriptStep::new(132);
    export.set_field_spec(0, Some(photo_spec()));
    let scripts = [
        fixture::script(
            1,
            "Insert a file",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(2),
                insert,
                fixture::set_variable("$e", "Get ( LastError )"),
                ScriptStep::new(75),
                show(report),
            ],
        ),
        fixture::script(
            2,
            "Insert a picture",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(2),
                go_to_photo,
                ScriptStep::new(56),
                fixture::set_variable("$e", "Get ( LastError )"),
                show(report),
            ],
        ),
        fixture::script(
            3,
            "Export the photo",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(2),
                export,
                fixture::set_variable("$e", "Get ( LastError )"),
                show("\"exported \" & $e"),
            ],
        ),
    ];
    let hosted = host("container-script-dialogs", &tree(&scripts, |_| {}));
    let mut app = open(&hosted);
    let run = |app: &mut common::App, script: &str| {
        common::open_menu(app, "Scripts");
        common::click(app, script);
    };
    let ok = |app: &mut common::App, expected: &str| {
        common::wait_for_label(app, expected);
        common::click(app, "OK");
        settle(app);
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
    };

    run(&mut app, "Insert a file");
    common::wait_for_label(&mut app, "Insert File");
    press(&mut app, "Cancel");
    ok(&mut app, "error 1:  0");
    assert_eq!(count(&hosted, 2), None);

    run(&mut app, "Insert a picture");
    common::wait_for_label(&mut app, "Insert Picture");
    common::fill(&mut app, "File:", note.to_str().unwrap());
    press(&mut app, "Insert");
    ok(&mut app, "error 734:  0");
    assert_eq!(count(&hosted, 2), None);

    run(&mut app, "Insert a file");
    common::wait_for_label(&mut app, "Insert File");
    common::fill(&mut app, "File:", note.to_str().unwrap());
    press(&mut app, "Insert");
    ok(&mut app, "error 0: note.txt 5");
    assert_eq!(stored(&hosted)[1].1, reference(2));
    assert_eq!((count(&hosted, 1), count(&hosted, 2)), (Some(1), Some(1)));

    run(&mut app, "Export the photo");
    common::wait_for_label(&mut app, "Export Field Contents");
    assert_eq!(
        common::text_field(&app, "Save as:").value().as_deref(),
        Some("file:note.txt")
    );
    hosted::replace(&mut app, "Save as:", output.to_str().unwrap());
    press(&mut app, "Save");
    ok(&mut app, "exported 0");
    assert_eq!(std::fs::read(&output).unwrap(), b"hello");
}

/// The Name field's exit trigger refuses to let the focus go: the click on
/// Photo selects nothing, and the commands stay off.
#[test]
fn a_refused_field_exit_leaves_the_container_unselected() {
    const KEEP: u64 = 2;
    let scripts = [
        fixture::script(1, "Start", Vec::new()),
        fixture::script(KEEP, "Keep", vec![fixture::exit_with("False")]),
    ];
    let hosted = host(
        "container-exit-veto",
        &tree(&scripts, |root| add_name_exit_trigger(root, KEEP)),
    );
    let mut app = open(&hosted);
    hosted::replace(&mut app, "Name", "Kept");
    photo(&app).expect("the Photo field").click();
    app.run_steps(2);
    settle(&mut app);
    app.run_steps(3);
    assert!(!photo_selected(&app));
    assert!(common::text_field(&app, "Name").is_focused());
    assert_eq!(name(&app).as_deref(), Some("Kept"));
    assert!(!menu_enabled(&mut app, "Insert", "File..."));
    assert!(!menu_enabled(&mut app, "Edit", "Export Field Contents..."));
    assert_eq!(count(&hosted, 1), Some(1));
}

/// A credential store no local file may touch.
struct Untouchable;

impl LicenseStore for Untouchable {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        panic!("a local file read the license store");
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
        panic!("a local file wrote the license store");
    }
    fn remove(&mut self) -> Result<(), DataError> {
        panic!("a local file changed the license store");
    }
}

fn local_app() -> common::App {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let provider = om_provider::NativeProvider::new(om_provider::NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: om_protocol::MachineInfo::default(),
    });
    Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .build_eframe(|_cc| {
            OpenMakerApp::with_license_store(Box::new(provider), Box::new(Untouchable))
        })
}

fn open_local(app: &mut common::App, path: &Path) {
    common::open_menu(app, "File");
    common::click(app, "Open...");
    common::wait_for_label(app, "Open File");
    hosted::replace(app, "Path", &path.display().to_string());
    common::click(app, "Open");
    common::wait_for_label(app, "Open “Tasks”");
    common::fill(app, "Password", fixture::PASSWORD);
    common::click(app, "OK");
    common::wait_until(app, "the file", |app| {
        app.state().is_settled() && photo(app).is_some()
    });
}

fn digest(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

/// A file on this computer with another name (a hard link) opens
/// read-only: its container exports, but neither the menu nor a script's
/// dialog inserts, and the file is not written.
#[test]
fn a_read_only_file_exports_its_container_but_never_inserts() {
    let temporary = tempfile::tempdir().unwrap();
    let file: PathBuf = temporary.path().join(fixture::FILE);
    let mut insert = ScriptStep::new(131);
    insert.set_field_spec(0, Some(photo_spec()));
    let scripts = [fixture::script(
        1,
        "Insert a file",
        vec![
            fixture::on(86, true),
            insert,
            fixture::set_variable("$e", "Get ( LastError )"),
            show("\"insert \" & $e"),
        ],
    )];
    let mut root = tree(&scripts, |_| {});
    fixture::add_local_accounts(&mut root);
    fixture::write_tree(&file, &root);
    std::fs::hard_link(&file, temporary.path().join("other name.fmp12")).unwrap();
    let before = digest(&file);
    let output = temporary.path().join("photo.png");

    let mut app = local_app();
    open_local(&mut app, &file);
    select_photo(&mut app);
    assert!(!menu_enabled(&mut app, "Insert", "File..."));
    assert!(!menu_enabled(&mut app, "Insert", "Picture..."));
    choose(&mut app, "Edit", "Export Field Contents...");
    common::wait_for_label(&mut app, "Export Field Contents");
    assert_eq!(
        common::text_field(&app, "Save as:").value().as_deref(),
        Some("file:photo2.png")
    );
    hosted::replace(&mut app, "Save as:", output.to_str().unwrap());
    press(&mut app, "Save");
    settle(&mut app);
    assert_eq!(std::fs::read(&output).unwrap(), fixture::png());

    // The step refuses before asking for a file.
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Insert a file");
    common::wait_for_label(&mut app, "insert 201");
    assert!(app.query_by_label("Insert File").is_none());
    common::click(&mut app, "OK");
    settle(&mut app);
    assert_eq!(photo_shows(&app), "photo2.png");
    drop(app);
    assert_eq!(digest(&file), before);
}
