//! The mouse wheel over a Browse Form layout, at actual size and zoomed:
//! a scrolling portal under the pointer scrolls, in a tab panel or an open
//! popover too, and otherwise the document does. Synthetic layouts and
//! records only.
mod common;

use egui::{vec2, Pos2, Role};
use egui_kittest::kittest::Queryable;
use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, JoinOp, JoinPredicate, MemorySource, RecordValues,
    Recorder, RelationshipDef,
};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage, RenderLayout,
};
use om_layout::{
    FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore, Part,
    PopoverButtonObject, PopoverObject, PortalObject, Rect, TabObject, TabPanelObject,
};
use std::{collections::BTreeMap, sync::Arc};

const PEOPLE: u64 = 0xd00001;
const LINES: u64 = 0xd00002;
const P: u64 = 0x8001;
const C: u64 = 0x8002;
const LINE_COUNT: usize = 60;
// Layout coordinates. Each portal shows the same related rows, through its
// own field: "Line 01" in the layout's portal, "Note 01" in the popover's,
// "Code 01" in the tab panel's.
const MARKER: (f32, f32) = (380.0, 150.0);
/// Over the layout's portal, clear of the popover.
const PORTAL_POINT: (f32, f32) = (30.0, 251.0);
/// Over the layout, clear of every object.
const BACKGROUND_POINT: (f32, f32) = (520.0, 251.0);
/// Over the open popover and the portal it covers, clear of the popover's
/// own portal.
const COVERED_POINT: (f32, f32) = (200.0, 250.0);
/// Over the open popover's portal, and the layout's portal under it.
const POPOVER_PORTAL_POINT: (f32, f32) = (90.0, 290.0);
/// Over the first tab panel's portal.
const TAB_PORTAL_POINT: (f32, f32) = (40.0, 511.0);

fn row(field: &str, index: usize) -> String {
    format!("{field} {index:02}")
}
fn line(index: usize) -> String {
    row("Line", index)
}

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Wheel fixture", Locale::en_us());
    for (table, occurrence, name) in [(P, PEOPLE, "People"), (C, LINES, "Lines")] {
        s.add_table(table, name);
        s.add_occurrence(occurrence, name, table);
        s.add_field(table, FieldDef::data(1, "Given", ValueType::Text));
    }
    s.add_field(C, FieldDef::data(2, "Owner", ValueType::Text));
    s.add_field(C, FieldDef::data(3, "Code", ValueType::Text));
    s.add_field(C, FieldDef::data(4, "Note", ValueType::Text));
    s.add_relationship(RelationshipDef::new(
        1,
        PEOPLE,
        LINES,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 2,
        }],
    ));
    s.add_layout(7, "Wheel fixture", Some(PEOPLE));
    s
}
fn source() -> MemorySource {
    let mut s = MemorySource::new();
    let mut clara = RecordValues::default();
    clara.set(1, 1, Value::text("Clara"));
    s.put_record(P, 19, clara);
    for index in 1..=LINE_COUNT {
        let mut values = RecordValues::default();
        values.set(1, 1, Value::text(line(index)));
        values.set(2, 1, Value::text("Clara"));
        values.set(3, 1, Value::text(row("Code", index)));
        values.set(4, 1, Value::text(row("Note", index)));
        s.put_record(C, index as u64, values);
    }
    s.set_found_set(PEOPLE, &[19], None);
    s
}

struct Provider;
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Wheel.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Wheel fixture".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: vec![Field {
                name: "Given".into(),
                kind: FieldKind::Text,
            }],
            records: [(19, "Clara")]
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(|(id, name)| Record {
                    id,
                    values: vec![Some(name.into())],
                })
                .collect(),
            total: 1,
        })
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(document()))
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        let schema = schema();
        let source = source();
        let recorder = Recorder::new(&source);
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 10, 31).unwrap());
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        Ok(recorder.take())
    }
}

fn object(id: u64, kind: u32, parent: Option<u64>, x: f32, y: f32, w: f32, h: f32) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        parent,
        bounds: Some(Rect {
            left: x.into(),
            top: y.into(),
            right: (x + w).into(),
            bottom: (y + h).into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn text(id: u64, parent: Option<u64>, text: &str, x: f32, y: f32) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        ..object(id, 2, parent, x, y, 200.0, 24.0)
    }
}
/// `base` as a scrolling portal of `rows` visible rows, with the row field
/// `base.id + 1`, which shows the Lines field `key`.
fn portal(base: LayoutObject, rows: f64, key: u64) -> [LayoutObject; 2] {
    let field = LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: 1065090,
                field_key: key,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(base.id + 1, 1, Some(base.id), 0.0, 0.0, 200.0, 20.0)
    };
    let portal = LayoutObject {
        children: vec![base.id + 1],
        portal: Some(PortalObject {
            table_alias_key: 1065090,
            num_rows: rows,
            initial_row: 1,
            flags: 0x1,
            ..Default::default()
        }),
        ..base
    };
    [portal, field]
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
        ..object(id, 12, Some(parent), 0.0, 30.0, 400.0, 180.0)
    }
}
/// A layout taller than the window: a scrolling portal of many related
/// rows, a popover with its own portal that opens over part of it, and a
/// tab control with a portal in its first panel.
fn document() -> RenderLayout {
    let button = LayoutObject {
        text: Some("Details".into()),
        popover_button: Some(PopoverButtonObject {
            popover_key: 3,
            ..Default::default()
        }),
        ..object(2, 14, None, 20.0, 40.0, 110.0, 24.0)
    };
    let popover = LayoutObject {
        children: vec![4, 20],
        popover: Some(PopoverObject {
            button_key: 2,
            ..Default::default()
        }),
        ..object(3, 15, None, 60.0, 230.0, 300.0, 140.0)
    };
    let tabs = LayoutObject {
        children: vec![31, 34],
        tab: Some(TabObject {
            default_panel_key: 31,
            tab_height: 30,
            ..Default::default()
        }),
        ..object(30, 11, None, 20.0, 420.0, 400.0, 210.0)
    };
    let [layout_portal, layout_row] =
        portal(object(10, 9, None, 20.0, 200.0, 400.0, 102.0), 5.0, 1);
    let [popover_portal, popover_row] =
        portal(object(20, 9, Some(3), 20.0, 40.0, 260.0, 62.0), 3.0, 4);
    let [panel_portal, panel_row] =
        portal(object(32, 9, Some(31), 10.0, 10.0, 300.0, 102.0), 5.0, 3);
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Wheel fixture".into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                table_alias_key: 1065089,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 2000.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![1, 5, 10, 30, 6, 2, 3],
                objects: vec![
                    text(1, None, "WHEEL FIXTURE", 20.0, 5.0),
                    text(5, None, "MARKER", MARKER.0, MARKER.1),
                    layout_portal,
                    layout_row,
                    tabs,
                    panel(31, 30, "First", 0, vec![32]),
                    panel_portal,
                    panel_row,
                    panel(34, 30, "Second", 110, vec![35]),
                    text(35, Some(34), "Second panel content", 10.0, 10.0),
                    text(6, None, "END OF LAYOUT", 20.0, 1950.0),
                    button,
                    popover,
                    text(4, Some(3), "Inside the popover", 10.0, 10.0),
                    popover_portal,
                    popover_row,
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([
            (11, "Given".into()),
            (21, "Note".into()),
            (33, "Code".into()),
        ]),
    }
}

fn open() -> common::App {
    let mut app = common::harness(Box::new(Provider));
    common::fill(&mut app, "Path", "fmnet:/wheel.example.invalid/Wheel.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "WHEEL FIXTURE");
    common::wait_for_label(&mut app, &line(1));
    app
}

/// Where the layout point `(x, y)` is on screen, from the marker text's
/// painted bounds, which follow the document's zoom and scroll.
fn screen(app: &common::App, (x, y): (f32, f32)) -> Pos2 {
    let marker = app.get_by_label("MARKER").rect();
    let factor = marker.width() / 200.0;
    marker.min + (vec2(x, y) - vec2(MARKER.0, MARKER.1)) * factor
}
/// The marker's top on screen: it moves only when the document scrolls.
fn document_top(app: &common::App) -> f32 {
    app.get_by_label("MARKER").rect().top()
}
/// The first row painted of the portal showing `field`.
fn first_row(app: &common::App, field: &str) -> usize {
    (1..=LINE_COUNT)
        .find(|&index| app.query_by_label(&row(field, index)).is_some())
        .expect("a painted portal row")
}
fn zoom(app: &mut common::App, command: &str, factor: f32) {
    let before = app.get_by_label("MARKER").rect();
    common::click(app, "View");
    common::click(app, command);
    app.run_steps(2);
    let after = app.get_by_label("MARKER").rect();
    assert!((after.width() - before.width() * factor).abs() < 0.1);
}
fn open_popover(app: &mut common::App) {
    app.get_by_role_and_label(Role::Button, "Details").click();
    app.run_steps(3);
    common::wait_for_label(app, "Inside the popover");
    common::wait_for_label(app, &row("Note", 1));
}
fn wheel(app: &mut common::App, at: Pos2, delta_y: f32) {
    app.hover_at(at);
    app.run_steps(2);
    app.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    app.run_steps(12);
}

/// The wheel over the portal scrolls it and not the document; over the
/// layout's background it scrolls the document and not the portal; back over
/// the portal at its first row, it scrolls the document again.
fn check_portal_then_document(app: &mut common::App) {
    assert_eq!(first_row(app, "Line"), 1);
    let top = document_top(app);
    let portal = screen(app, PORTAL_POINT);
    wheel(app, portal, -120.0);
    let scrolled = first_row(app, "Line");
    assert!(scrolled > 1, "the portal under the pointer must scroll");
    assert!(
        (document_top(app) - top).abs() < 0.5,
        "the document must stay while the portal scrolls"
    );

    let background = screen(app, BACKGROUND_POINT);
    wheel(app, background, -100.0);
    assert!(
        document_top(app) < top - 50.0,
        "the wheel over the background must scroll the document"
    );
    assert_eq!(first_row(app, "Line"), scrolled, "the portal must stay");

    // Up past the portal's first row: the rest scrolls the document back.
    let scrolled_top = document_top(app);
    let portal = screen(app, PORTAL_POINT);
    wheel(app, portal, 1000.0);
    assert_eq!(first_row(app, "Line"), 1);
    assert!(
        document_top(app) > scrolled_top + 50.0,
        "a portal at its end must pass the wheel to the document"
    );
    assert!(app.state().error().is_none());
}

#[test]
fn the_wheel_scrolls_the_portal_under_the_pointer_and_otherwise_the_document() {
    let mut app = open();
    check_portal_then_document(&mut app);
}

#[test]
fn zoomed_the_wheel_scrolls_the_portal_under_the_pointer_and_otherwise_the_document() {
    let mut app = open();
    zoom(&mut app, "Zoom In", 1.5);
    check_portal_then_document(&mut app);
}

/// As in #64's unit test, with the whole Form view: the wheel over an open
/// popover leaves the portal it covers alone and scrolls the document, while
/// the same portal still scrolls under the pointer beside the popover.
#[test]
fn the_wheel_over_an_open_popover_scrolls_the_document_and_not_the_portal_it_covers() {
    let mut app = open();
    open_popover(&mut app);
    let top = document_top(&app);
    let covered = screen(&app, COVERED_POINT);
    wheel(&mut app, covered, -100.0);
    assert_eq!(
        first_row(&app, "Line"),
        1,
        "the covered portal must not scroll"
    );
    assert!(
        document_top(&app) < top - 50.0,
        "the wheel over the popover must scroll the document"
    );

    let top = document_top(&app);
    let beside = screen(&app, PORTAL_POINT);
    assert!(
        beside.x < screen(&app, (60.0, 0.0)).x,
        "clear of the popover"
    );
    wheel(&mut app, beside, -120.0);
    assert!(
        first_row(&app, "Line") > 1,
        "the portal beside the popover scrolls"
    );
    assert!((document_top(&app) - top).abs() < 0.5);
    assert!(app.state().error().is_none());
}

/// A portal inside the open popover takes the wheel under the pointer, at
/// 150 %, while the layout's portal it covers stays; past its last row, the
/// rest scrolls the document.
#[test]
fn zoomed_a_portal_in_an_open_popover_scrolls_under_the_pointer() {
    let mut app = open();
    zoom(&mut app, "Zoom In", 1.5);
    open_popover(&mut app);
    let top = document_top(&app);
    let point = screen(&app, POPOVER_PORTAL_POINT);
    wheel(&mut app, point, -120.0);
    assert!(first_row(&app, "Note") > 1, "the popover's portal scrolls");
    assert_eq!(first_row(&app, "Line"), 1, "the portal it covers stays");
    assert!((document_top(&app) - top).abs() < 0.5, "the document stays");

    let point = screen(&app, POPOVER_PORTAL_POINT);
    wheel(&mut app, point, -1200.0);
    app.get_by_label(&row("Note", LINE_COUNT));
    assert_eq!(first_row(&app, "Line"), 1);
    assert!(
        document_top(&app) < top - 50.0,
        "a portal at its end must pass the wheel to the document"
    );
    assert!(app.state().error().is_none());
}

/// At 75 %, the portal in the selected tab panel takes the wheel under the
/// pointer. Once another panel is selected, the wheel there scrolls the
/// document.
#[test]
fn zoomed_out_a_portal_in_the_selected_tab_panel_scrolls_under_the_pointer() {
    let mut app = open();
    zoom(&mut app, "Zoom Out", 0.75);
    common::wait_for_label(&mut app, &row("Code", 1));
    let top = document_top(&app);
    let point = screen(&app, TAB_PORTAL_POINT);
    wheel(&mut app, point, -120.0);
    assert!(first_row(&app, "Code") > 1, "the panel's portal scrolls");
    assert_eq!(first_row(&app, "Line"), 1);
    assert!((document_top(&app) - top).abs() < 0.5, "the document stays");

    app.get_by_role_and_label(Role::Tab, "Second").click();
    app.run_steps(3);
    app.get_by_label("Second panel content");
    assert!((1..=LINE_COUNT).all(|index| app.query_by_label(&row("Code", index)).is_none()));
    wheel(&mut app, point, -100.0);
    assert!(
        document_top(&app) < top - 50.0,
        "with the portal's panel hidden, the wheel scrolls the document"
    );
    assert!(app.state().error().is_none());
}
