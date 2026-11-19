//! New Window's Card and Dialog styles through the GUI end to end: scripts
//! run on the provider worker against an in-process `om-server` hosting
//! the synthetic in-memory Tasks file (`om-server/tests/fixtures/tasks.rs`).
//! The behavior is Claris's ("About window styles", "New Window"); native
//! FileMaker's answers are not measured.
//!
//! - A card opens over its parent in the parent's window: the menu bar
//!   works on the card, its close box closes it and the parent is in front
//!   again. Other windows stay usable while it is open; closing its parent
//!   closes it first; a parent has one card at a time.
//! - A dialog keeps every other window from taking input until it closes,
//!   refuses document windows and selecting another window, and returns
//!   to its opener.
//! - A style OpenMaker does not support stops the script before any
//!   window opens.
//!
//! `OM_WINDOW_STYLES_RENDER=<dir>` saves what the card and dialog look like
//! as PNGs there.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use egui_kittest::kittest::{By, NodeT as _, Queryable};
use fixture::{calc, ACCOUNT, PASSWORD};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_scripts::{flag, Script, ScriptStep};

/// The Tasks starter solution's card word (Dim parent window), with a
/// close box.
const CARD: u32 = 0xC010_0010 | 0x0200;
/// FileMaker's Dialog word.
const DIALOG: u32 = 0x8005_0204;

/// New Window [ Style: `word` ; Name: name ; Height ; Width ].
fn new_window(name: &str, word: Option<u32>, size: Option<(u32, u32)>) -> ScriptStep {
    let mut step = ScriptStep::new(122);
    step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
    if let Some(word) = word {
        step.set_number(0, Some(word as i32));
    }
    if let Some((height, width)) = size {
        step.set_calculation(1, Some(&calc(&height.to_string())));
        step.set_calculation(2, Some(&calc(&width.to_string())));
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

/// View As [ Table ]: the fixture's layout has no objects, its table
/// shows the records.
fn view_as_table() -> ScriptStep {
    let mut step = ScriptStep::new(30);
    step.value = 3;
    step
}

fn note(name: &str, text: &str) -> ScriptStep {
    fixture::set_variable(name, text)
}

/// Show Custom Dialog [ "Result" ; message ].
fn show(message: &str) -> ScriptStep {
    let mut show = ScriptStep::new(87);
    show.set_calculation(0, Some(&calc("\"Result\"")));
    show.set_calculation(1, Some(&calc(message)));
    show.set_text(0, Some("OK"));
    show
}

/// The window a script runs in: `name:style:record:task:windows`.
const WHERE: &str = "Get ( WindowName ) & \":\" & Get ( WindowStyle ) & \":\" & \
                     Get ( RecordNumber ) & \":\" & Tasks::Name & \":\" & \
                     Substitute ( WindowNames ( \"\" ) ; ¶ ; \",\" )";

fn host(tag: &str, scripts: &[Script]) -> hosted::Hosted {
    host_with(tag, scripts, |_| {})
}

/// [`host`], the file changed by `change` first.
fn host_with(tag: &str, scripts: &[Script], change: impl FnOnce(&mut OwnedNode)) -> hosted::Hosted {
    let mut root = fixture::tree(scripts, true);
    fixture::add_tasks(&mut root, 3..=5);
    change(&mut root);
    let file = fixture::TempFile::from_tree(&format!("gui-window-styles-{tag}"), &root);
    hosted::host_file(
        &format!("window-styles-{tag}"),
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
    )
}

fn open(hosted: &hosted::Hosted) -> common::App {
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    app
}

/// Run `script` from the Scripts menu of the front window's viewport.
fn run(app: &mut common::App, script: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, script);
}

fn finished(app: &mut common::App) {
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The custom dialog's message, once it shows `expected`; then OK.
fn dialog_says(app: &mut common::App, expected: &str) {
    common::wait_until(app, expected, |app| {
        app.query(By::new().label_contains(expected)).is_some()
    });
    let shown = app
        .query(By::new().label_contains(expected))
        .map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value()).unwrap_or_default()
        })
        .unwrap_or_default();
    assert_eq!(shown, expected);
    common::click(app, "OK");
    finished(app);
}

fn render(app: &mut common::App, name: &str) {
    let Some(dir) = std::env::var_os("OM_WINDOW_STYLES_RENDER") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("render dir");
    let image = common::rendered(app);
    let path = dir.join(format!("{name}.png"));
    image.save(&path).expect("write png");
    eprintln!("wrote {}", path.display());
}

#[test]
fn a_card_opens_over_its_parent_and_its_close_box_returns_there() {
    let hosted = host(
        "card",
        &[
            fixture::script(
                1,
                "Open card",
                vec![
                    fixture::go_to_layout(fixture::LAYOUT),
                    view_as_table(),
                    fixture::go_to_record(1),
                    fixture::go_to_record(4),
                    new_window("Picker", Some(CARD), Some((240, 360))),
                ],
            ),
            fixture::script(2, "Report", vec![show(WHERE)]),
        ],
    );
    let mut app = open(&hosted);
    run(&mut app, "Open card");
    finished(&mut app);
    // The card is the front window, on its parent's record; the root
    // window's viewport shows it with its close box.
    assert_eq!(
        app.state_mut().window_titles(),
        ["Picker", "Tasks (127.0.0.1)"]
    );
    assert!(app.query(By::new().label("Close card")).is_some());
    render(&mut app, "card-over-parent");
    // The viewport's menu bar works on the card, not on its parent.
    run(&mut app, "Report");
    dialog_says(&mut app, "Picker:3:2:Ship:Picker,Tasks");
    // The close box closes the card; its parent is in front again.
    common::click(&mut app, "Close card");
    common::wait_until(&mut app, "the card to close", |app| {
        app.state().window_name() != "Picker"
    });
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)"]);
    run(&mut app, "Report");
    dialog_says(&mut app, "Tasks:0:2:Ship:Tasks");
}

/// Layout 1 (Tasks) shows Tasks::Name, filled yellow, at the top left of
/// a 300-point body (layout-model.md 5): what Form view paints on the
/// layout's own layer, as the starters' filled parts and panels.
fn add_name_layout(root: &mut OwnedNode) {
    use om_layout::encode::{bytes, double, field_object, uint, varint};
    // Its local style's background (property 7) in the normal state of the
    // object itself; a `ColorValue` is red, green, blue and alpha as 32-bit
    // floats (fields 1-4).
    let mut color = Vec::new();
    for (number, value) in [(1u64, 1.0f32), (2, 0.9), (3, 0.4), (4, 1.0)] {
        varint(number << 3 | 5, &mut color);
        color.extend_from_slice(&value.to_le_bytes());
    }
    let mut property = Vec::new();
    uint(1, 7, &mut property);
    bytes(5, &color, &mut property);
    let mut properties = Vec::new();
    bytes(1, &property, &mut properties);
    let mut state = Vec::new();
    uint(1, 1, &mut state);
    uint(2, 1, &mut state);
    bytes(3, &properties, &mut state);
    let mut local = Vec::new();
    bytes(1, &state, &mut local);
    let mut fill = Vec::new();
    bytes(1, &local, &mut fill);
    let alias = serial::decode(&uint_segment(fixture::OCCURRENCE)).unwrap();
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
    double(5, 300.0, &mut body);
    root.put(
        &[&[4], &[5], &layout, &[3], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), body),
    );
    let bounds = om_layout::Rect {
        top: 10.0,
        left: 10.0,
        bottom: 30.0,
        right: 210.0,
        other: Vec::new(),
    };
    let mut store = Vec::new();
    uint(1, 1, &mut store);
    let mut name = field_object(1, &bounds, alias, fixture::NAME);
    bytes(5, &fill, &mut name);
    bytes(2, &name, &mut store);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
}

/// A card over a layout in Form view is painted over its dimmed parent
/// (#865). Each layout paints on a layer of its own, and the card and the
/// dimming were painted under its parent's: the window showed the parent
/// undimmed and no card, while the accessibility tree had the card's
/// fields and buttons. This reads the rendered pixels.
#[test]
fn a_card_is_painted_over_its_dimmed_parent_in_form_view() {
    let hosted = host_with(
        "card-form",
        &[
            fixture::script(
                1,
                "Show record",
                vec![
                    fixture::go_to_layout(fixture::LAYOUT),
                    fixture::go_to_record(1),
                ],
            ),
            fixture::script(
                2,
                "Open card",
                vec![new_window("Picker", Some(CARD), Some((240, 360)))],
            ),
        ],
        add_name_layout,
    );
    let mut app = open(&hosted);
    run(&mut app, "Show record");
    finished(&mut app);
    let before = common::rendered(&mut app);
    run(&mut app, "Open card");
    finished(&mut app);
    let after = common::rendered(&mut app);
    render(&mut app, "card-over-form");
    let cards = app.state().cards();
    assert_eq!(cards.len(), 1, "{cards:?}");
    let card = &cards[0];
    let ppp = app.ctx.pixels_per_point();
    common::card_painted(&before, &after, ppp, card).unwrap();
    // The yellow Name field, which each layout paints on its own layer: the
    // parent's dimmed (it showed through the dimming), the card's as the
    // parent's was before (its layout could go under the card's backing).
    let pixel = |image: &image::RgbaImage, field: egui::Rect| {
        let (x, y) = (field.right() - 3.0, field.center().y);
        image.get_pixel((x * ppp) as u32, (y * ppp) as u32).0
    };
    let near = |a: [u8; 4], b: [u8; 4]| (0..3).all(|i| a[i].abs_diff(b[i]) <= 6);
    let fields: Vec<egui::Rect> = (app.query_all(By::new().role(egui::accesskit::Role::TextInput)))
        .map(|field| field.rect())
        .collect();
    let parent = fields
        .iter()
        .find(|f| card.parent_area.contains_rect(**f) && !card.frame.expand(24.0).intersects(**f))
        .expect("the parent's field");
    let inside = fields
        .iter()
        .find(|f| card.frame.contains_rect(**f))
        .expect("the card's field");
    let yellow = pixel(&before, *parent);
    let dimmed = yellow.map(|c| (f32::from(c) * 159.0 / 255.0).round() as u8);
    let shown = pixel(&after, *parent);
    assert!(
        near(shown, dimmed),
        "parent's field: {yellow:?} became {shown:?}, not {dimmed:?}"
    );
    let shown = pixel(&after, *inside);
    assert!(
        near(shown, yellow),
        "card's field: {shown:?}, not {yellow:?}"
    );
}

#[test]
fn closing_a_cards_parent_closes_the_card_and_other_windows_stay_usable() {
    let hosted = host(
        "parent",
        &[fixture::script(
            1,
            "Cards",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(fixture::LAYOUT),
                fixture::go_to_record(1),
                new_window("W2", None, None),
                new_window("Picker", Some(CARD), Some((200, 300))),
                note("$a", "Get ( WindowStyle ) & \":\" & Get ( WindowName )"),
                // A card blocks only its parent.
                window_step(123, Some("Tasks")),
                note("$b", "Get ( LastError ) & \":\" & Get ( WindowName )"),
                // A parent has one card at a time.
                window_step(123, Some("W2")),
                new_window("Second", Some(CARD), None),
                note("$c", "Get ( LastError ) & \":\" & Get ( WindowName )"),
                // Closing the parent closes its card first.
                window_step(121, Some("W2")),
                note(
                    "$d",
                    "Get ( LastError ) & \":\" & Get ( WindowName ) & \":\" & \
                     Substitute ( WindowNames ( \"\" ) ; ¶ ; \",\" )",
                ),
                show("$a & \" | \" & $b & \" | \" & $c & \" | \" & $d"),
            ],
        )],
    );
    let mut app = open(&hosted);
    run(&mut app, "Cards");
    dialog_says(&mut app, "3:Picker | 0:Tasks | 3:W2 | 0:Tasks:Tasks");
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)"]);
}

#[test]
fn a_dialog_blocks_other_windows_until_it_returns_to_its_opener() {
    let hosted = host(
        "dialog",
        &[
            fixture::script(
                1,
                "Dialog",
                vec![
                    fixture::on(86, true),
                    fixture::go_to_layout(fixture::LAYOUT),
                    fixture::go_to_record(1),
                    new_window("Confirm", Some(DIALOG), Some((300, 420))),
                    note("$a", "Get ( WindowStyle ) & \":\" & Get ( WindowName )"),
                    // While a dialog is open: no document window, no other
                    // window selected; a card over it opens and closes.
                    new_window("Doc", None, None),
                    note("$b", "Get ( LastError ) & \":\" & Get ( WindowName )"),
                    window_step(123, Some("Tasks")),
                    note("$c", "Get ( LastError ) & \":\" & Get ( WindowName )"),
                    new_window("Inner", Some(CARD), Some((100, 200))),
                    note("$d", "Get ( LastError ) & \":\" & Get ( WindowStyle )"),
                    window_step(121, None),
                    note("$e", "Get ( WindowName )"),
                    show("$a & \" | \" & $b & \" | \" & $c & \" | \" & $d & \" | \" & $e"),
                ],
            ),
            fixture::script(2, "Close current", vec![window_step(121, None)]),
            fixture::script(
                3,
                "Floating",
                // A style word OpenMaker does not map (not Document, Dialog
                // or Card).
                vec![new_window("Float", Some(0x0037_0608), None)],
            ),
        ],
    );
    let mut app = open(&hosted);
    run(&mut app, "Dialog");
    dialog_says(
        &mut app,
        "2:Confirm | 3:Confirm | 3:Confirm | 0:3 | Confirm",
    );
    assert_eq!(
        app.state_mut().window_titles(),
        ["Confirm", "Tasks (127.0.0.1)"]
    );
    // The window behind takes no input: its menu bar is disabled.
    let root_scripts = app
        .query_all(By::new().role(egui::Role::Button).label("Scripts"))
        .next()
        .expect("the root window's Scripts menu");
    assert!(root_scripts.accesskit_node().is_disabled());
    render(&mut app, "dialog-over-document");
    // The dialog's own Scripts menu closes it: its opener is in front and
    // takes input again.
    app.get_by_role_and_label(egui::Role::Window, "Confirm")
        .get_by_role_and_label(egui::Role::Button, "Scripts")
        .click();
    app.run_steps(2);
    common::click(&mut app, "Close current");
    finished(&mut app);
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)"]);
    let root_scripts = app
        .query_all(By::new().role(egui::Role::Button).label("Scripts"))
        .next()
        .expect("the root window's Scripts menu");
    assert!(!root_scripts.accesskit_node().is_disabled());
    // An unsupported style stops the script before a window opens.
    run(&mut app, "Floating");
    common::wait_until(&mut app, "the stop", |app| !app.state().script_running());
    let error = app.state().error().unwrap_or_default().to_owned();
    assert!(error.contains("New Window"), "{error}");
    assert!(error.contains("not supported"), "{error}");
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)"]);
}

#[test]
fn closing_a_named_background_card_returns_to_its_parent_record() {
    let hosted = host(
        "background-card",
        &[fixture::script(
            1,
            "Close background card",
            vec![
                fixture::go_to_layout(fixture::LAYOUT),
                view_as_table(),
                fixture::go_to_record(1),
                new_window("Other", None, None),
                fixture::go_to_record(4),
                window_step(123, Some("Tasks")),
                new_window("Picker", Some(CARD), Some((200, 300))),
                window_step(123, Some("Other")),
                window_step(121, Some("Picker")),
                fixture::set_field(fixture::NAME, "\"Parent chosen\""),
                ScriptStep::new(75),
                note("$after", WHERE),
                window_step(123, Some("Other")),
                note("$other", "Tasks::Name"),
                window_step(123, Some("Tasks")),
                show("$after & \" | \" & $other & \" | \" & Tasks::Name"),
            ],
        )],
    );
    let mut app = open(&hosted);
    run(&mut app, "Close background card");
    dialog_says(
        &mut app,
        "Tasks:0:1:Parent chosen:Tasks,Other | Ship | Parent chosen",
    );
}
