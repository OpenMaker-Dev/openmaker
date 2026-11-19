//! Popover buttons and their popovers in Browse, using only synthetic
//! layouts and records.
mod common;

use egui::epaint::Shape;
use egui::Role;
use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage, RenderLayout,
};
use om_layout::{
    Layout, LayoutDef, LayoutObject, ObjectStore, Part, PopoverButtonObject, PopoverObject, Rect,
    TabObject, TabPanelObject,
};

const INSIDE: &str = "Inside the popover";
const INSIDE_SHIPPING: &str = "Inside the shipping popover";
const INSIDE_PANEL: &str = "Inside the panel popover";

struct Provider;
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Popovers.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok([(7, "Popovers"), (8, "Other Popovers")]
            .map(|(id, name)| LayoutInfo {
                id,
                name: name.into(),
            })
            .into())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: vec![
                Field {
                    name: "Given".into(),
                    kind: FieldKind::Text,
                },
                Field {
                    name: "Photo".into(),
                    kind: FieldKind::Container,
                },
            ],
            records: [(19, "Clara"), (37, "Emery")]
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(|(id, name)| Record {
                    id,
                    values: vec![Some(name.into()), None],
                })
                .collect(),
            total: 2,
        })
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(document(id)))
    }
}

fn object(id: u64, kind: u32, parent: Option<u64>, x: f64, y: f64, w: f64, h: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        parent,
        bounds: Some(Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn text(id: u64, parent: Option<u64>, text: &str, x: f64, y: f64) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        ..object(id, 2, parent, x, y, 200.0, 24.0)
    }
}
fn popover_button(
    id: u64,
    parent: Option<u64>,
    text: &str,
    popover_key: u64,
    x: f64,
    y: f64,
) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        flags: 0x10000,
        popover_button: Some(PopoverButtonObject {
            popover_key,
            ..Default::default()
        }),
        ..object(id, 14, parent, x, y, 110.0, 24.0)
    }
}
/// `base` placed as the popover of `button_key`.
fn popover(button_key: u64, children: Vec<u64>, base: LayoutObject) -> LayoutObject {
    LayoutObject {
        children,
        popover: Some(PopoverObject {
            button_key,
            ..Default::default()
        }),
        ..base
    }
}
fn panel(id: u64, parent: u64, text: &str, left: i32, children: Vec<u64>) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        children,
        tab_panel: Some(TabPanelObject {
            tab_left_edge: left,
            tab_width: 110,
            ..Default::default()
        }),
        ..object(id, 12, Some(parent), 0.0, 30.0, 260.0, 110.0)
    }
}
/// Both layouts use the same object keys: an open popover on one must not
/// open the other's.
fn document(id: u32) -> RenderLayout {
    let title = if id == 7 {
        "POPOVER FIXTURE"
    } else {
        "OTHER POPOVERS"
    };
    let tabs = LayoutObject {
        children: vec![11, 12],
        tab: Some(TabObject {
            default_panel_key: 11,
            tab_height: 30,
            ..Default::default()
        }),
        ..object(10, 11, None, 320.0, 150.0, 260.0, 140.0)
    };
    RenderLayout {
        layout: Layout {
            id: u64::from(id),
            name: title.into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 300.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                // Objects listed after the popover lie under it once it
                // opens: a text and another popover button.
                root: vec![1, 2, 3, 5, 6, 7, 10],
                objects: vec![
                    text(1, None, title, 20.0, 5.0),
                    popover_button(2, None, "Details", 3, 20.0, 40.0),
                    popover(
                        2,
                        vec![4, 9, 17],
                        object(3, 15, None, 20.0, 80.0, 260.0, 100.0),
                    ),
                    text(4, Some(3), INSIDE, 10.0, 10.0),
                    text(5, None, "Listed after the popover", 30.0, 95.0),
                    popover_button(6, None, "Shipping", 7, 150.0, 145.0),
                    popover(6, vec![8], object(7, 15, None, 320.0, 40.0, 220.0, 80.0)),
                    text(8, Some(7), INSIDE_SHIPPING, 10.0, 10.0),
                    // A control inside the popover, over its surface.
                    popover_button(9, Some(3), "Close details", 3, 10.0, 40.0),
                    object(17, 1, Some(3), 10.0, 70.0, 200.0, 24.0),
                    tabs,
                    panel(11, 10, "First", 0, vec![13, 14]),
                    panel(12, 10, "Second", 110, vec![16]),
                    popover_button(13, Some(11), "Panel details", 14, 10.0, 5.0),
                    popover(
                        13,
                        vec![15],
                        object(14, 15, Some(11), 10.0, 40.0, 230.0, 60.0),
                    ),
                    text(15, Some(14), INSIDE_PANEL, 5.0, 5.0),
                    text(16, Some(12), "Second panel content", 10.0, 10.0),
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: [(17, "Photo".into())].into(),
    }
}

fn painted(shape: &Shape, texts: &mut Vec<String>) {
    match shape {
        Shape::Text(t) => texts.push(t.galley.job.text.clone()),
        Shape::Vec(items) => items.iter().for_each(|s| painted(s, texts)),
        _ => {}
    }
}
/// Every text the last frame painted, in painting order.
fn texts(app: &common::App) -> Vec<String> {
    let mut texts = Vec::new();
    for shape in &app.output().shapes {
        painted(&shape.shape, &mut texts);
    }
    texts
}
fn button_open(app: &common::App) -> bool {
    app.get_by_role_and_label(Role::Button, "Details")
        .accesskit_node()
        .toggled()
        == Some(egui::accesskit::Toggled::True)
}
fn toggle(app: &mut common::App) {
    press(app, "Details");
}
fn press(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(3);
}
fn open_fixture() -> common::App {
    let mut app = common::harness(Box::new(Provider));
    common::fill(&mut app, "Host", "popovers.example.invalid");
    common::fill(&mut app, "File", "Popovers.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "POPOVER FIXTURE");
    common::wait_until(&mut app, "the first record", |app| {
        app.query_by_label("2").is_some()
    });
    app
}

#[test]
fn a_popover_button_toggles_its_popover_per_layout_across_records() {
    let mut app = open_fixture();
    // Hidden until its button is clicked.
    assert!(app.query_by_label(INSIDE).is_none());
    assert!(!button_open(&app));
    toggle(&mut app);
    common::wait_for_label(&mut app, INSIDE);
    assert!(button_open(&app));
    // It floats above objects listed after it.
    let painted = texts(&app);
    let index = |text: &str| painted.iter().position(|t| t == text).unwrap();
    assert!(
        index(INSIDE) > index("Listed after the popover"),
        "{painted:?}"
    );
    // Changing records keeps it open.
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "record 2", |app| {
        common::record_number(app) == "2"
    });
    assert!(app.query_by_label(INSIDE).is_some());
    // Another layout has its own popovers, closed; coming back finds this
    // layout's popover still open.
    common::select_layout(&mut app, "Other Popovers");
    common::wait_for_label(&mut app, "OTHER POPOVERS");
    assert!(app.query_by_label(INSIDE).is_none());
    assert!(!button_open(&app));
    common::select_layout(&mut app, "Popovers");
    common::wait_for_label(&mut app, "POPOVER FIXTURE");
    common::wait_for_label(&mut app, INSIDE);
    // A second click closes it.
    toggle(&mut app);
    assert!(app.query_by_label(INSIDE).is_none());
    assert!(!button_open(&app));
    assert!(app.state().error().is_none());
}

#[test]
fn an_open_popover_takes_clicks_from_the_controls_it_covers() {
    let mut app = open_fixture();
    let covered = app
        .get_by_role_and_label(Role::Button, "Shipping")
        .rect()
        .center();
    app.event(egui::Event::PointerMoved(covered));
    app.run_steps(3);
    assert_eq!(
        app.output().platform_output.cursor_icon,
        egui::CursorIcon::PointingHand
    );
    toggle(&mut app);
    common::wait_for_label(&mut app, INSIDE);
    app.event(egui::Event::PointerMoved(covered));
    app.run_steps(3);
    assert_ne!(
        app.output().platform_output.cursor_icon,
        egui::CursorIcon::PointingHand
    );
    // The Shipping button lies under the open popover: clicking where it is
    // reaches the popover, not the button.
    press(&mut app, "Shipping");
    assert!(app.query_by_label(INSIDE_SHIPPING).is_none());
    assert!(app.query_by_label(INSIDE).is_some());
    assert!(button_open(&app));
    // The popover's own controls take clicks above its surface.
    press(&mut app, "Close details");
    assert!(app.query_by_label(INSIDE).is_none());
    assert!(!button_open(&app));
    assert!(app
        .get_by_role_and_label(Role::Button, "Details")
        .is_focused());
    // Uncovered, the Shipping button opens its popover.
    press(&mut app, "Shipping");
    common::wait_for_label(&mut app, INSIDE_SHIPPING);
    assert!(app.state().error().is_none());
}

#[test]
fn a_popover_shows_only_while_its_containers_do() {
    let mut app = open_fixture();
    press(&mut app, "Panel details");
    common::wait_for_label(&mut app, INSIDE_PANEL);
    // Selecting another panel hides the button's panel, popover included.
    app.get_by_role_and_label(Role::Tab, "Second").click();
    app.run_steps(3);
    app.get_by_label("Second panel content");
    assert!(app.query_by_label("Panel details").is_none());
    assert!(app.query_by_label(INSIDE_PANEL).is_none());
    // The popover is still open: it shows again with its panel.
    app.get_by_role_and_label(Role::Tab, "First").click();
    app.run_steps(3);
    common::wait_for_label(&mut app, INSIDE_PANEL);
    assert!(app.state().error().is_none());
}

#[test]
fn closing_a_popover_releases_its_selected_container() {
    let mut app = open_fixture();
    toggle(&mut app);
    common::wait_for_label(&mut app, "Inside the popover");
    press(&mut app, "Photo");
    let selected = app
        .get_by_role_and_label(Role::Button, "Photo")
        .accesskit_node()
        .toggled()
        == Some(egui::accesskit::Toggled::True);
    assert!(selected, "probe setup must select the visible container");
    common::open_menu(&mut app, "Edit");
    assert!(
        !app.get_by_label("Export Field Contents...")
            .accesskit_node()
            .is_disabled(),
        "probe setup: menu must act on selected container"
    );
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    toggle(&mut app);
    assert!(
        app.query_by_role_and_label(Role::Button, "Photo").is_none(),
        "the popover container must now be hidden"
    );
    assert!(!button_open(&app));
    common::open_menu(&mut app, "Edit");
    let enabled = !app
        .get_by_label("Export Field Contents...")
        .accesskit_node()
        .is_disabled();
    eprintln!("visible container selected={selected}; after closing popover: Photo absent=true, Export enabled={enabled}");
    assert!(!enabled, "closing the popover clicked elsewhere on the layout, so its hidden container must not stay selected");
}
