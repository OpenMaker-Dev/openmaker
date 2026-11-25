//! Scripted window scrolling uses generated rows and never writes records.
mod common;

use std::sync::{Arc, Mutex};

use egui_kittest::kittest::{By, Queryable};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage, Script, ScriptCatalog, ScriptStep,
};

type Requests = Arc<Mutex<Vec<(u64, u32)>>>;

struct Provider {
    requests: Requests,
    columns: usize,
    rich: bool,
    editable: bool,
    header: bool,
    duplicate: bool,
}
fn field_name(index: usize) -> String {
    if index == 0 {
        "Code".into()
    } else {
        format!("Field {:02}", index + 1)
    }
}
fn scroll(choice: u16) -> ScriptStep {
    let mut step = ScriptStep::new(81);
    step.value = choice;
    step
}
fn view(choice: u16) -> ScriptStep {
    let mut step = ScriptStep::new(30);
    step.value = choice;
    step
}
fn field(number: u8) -> ScriptStep {
    let mut field = ScriptStep::new(17);
    field.set_field_spec(
        0,
        Some(om_data::FieldSpec::new(
            om_data::StoredKey::new(&[1, 5]).unwrap(),
            om_data::StoredKey::new(&[1, number]).unwrap(),
            1,
        )),
    );
    field
}
fn active_field() -> Vec<ScriptStep> {
    vec![field(1), scroll(2), ScriptStep::new(62), scroll(5)]
}
fn next_record() -> ScriptStep {
    let mut step = ScriptStep::new(16);
    step.value = 4;
    step
}
fn background(frozen: bool) -> Vec<ScriptStep> {
    let mut new = ScriptStep::new(122);
    new.set_calculation(0, Some(&common::workspace::calc("\"Scroll peer\"")));
    let mut select = ScriptStep::new(123);
    select.value = 1;
    select.flags |= om_data::flag::MENU_SELECTION;
    select.set_calculation(0, Some(&common::workspace::calc("\"Scroll\"")));
    let mut steps = vec![new];
    if frozen {
        steps.push(ScriptStep::new(79));
    }
    steps.extend([scroll(2), select]);
    steps
}
fn report() -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&common::workspace::calc("\"Scroll result\"")));
    step.set_calculation(
        1,
        Some(&common::workspace::calc("\"scroll:\" & Get ( LastError )")),
    );
    step.set_text(0, Some("OK"));
    step
}
fn script(id: u64, name: &str, mut steps: Vec<ScriptStep>) -> Script {
    let mut capture = ScriptStep::new(86);
    capture.flags |= om_data::flag::TURN_ON;
    steps.insert(0, capture);
    steps.push(report());
    Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    }
}
impl DataProvider for Provider {
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Long rows".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.requests.lock().unwrap().push((offset, limit));
        Ok(RecordPage {
            versions: Default::default(),
            fields: (0..self.columns)
                .map(|i| Field {
                    name: field_name(i),
                    kind: FieldKind::Text,
                })
                .collect(),
            records: (1..=200_i64)
                .skip(offset as usize)
                .take(limit as usize)
                .map(|id| Record {
                    id,
                    values: (0..self.columns)
                        .map(|i| {
                            Some(if i == 0 {
                                format!("Row {id:04}")
                            } else {
                                format!("Value {:02}", i + 1)
                            })
                        })
                        .collect(),
                })
                .collect(),
            total: 200,
        })
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(ScriptCatalog {
            scripts: vec![
                script(1, "Scroll Home", vec![scroll(1)]),
                script(2, "Scroll End", vec![scroll(2)]),
                script(3, "Scroll Up", vec![scroll(3)]),
                script(4, "Scroll Down", vec![scroll(4)]),
                script(5, "Scroll Selection", vec![scroll(5)]),
                script(6, "Scroll Invalid", vec![scroll(0)]),
                script(
                    7,
                    "Frozen Pages",
                    vec![ScriptStep::new(79), scroll(1), scroll(3), scroll(4)],
                ),
                script(8, "Frozen End", vec![ScriptStep::new(79), scroll(2)]),
                script(
                    9,
                    "Frozen View Change",
                    vec![ScriptStep::new(79), scroll(2), view(1)],
                ),
                script(12, "Scroll Active Field", active_field()),
                script(13, "Zoom Then End", {
                    let mut zoom = ScriptStep::new(97);
                    zoom.value = 5;
                    vec![ScriptStep::new(79), zoom, scroll(2)]
                }),
                script(
                    14,
                    "Header Selection",
                    vec![ScriptStep::new(79), field(1), scroll(2), scroll(5)],
                ),
                script(
                    15,
                    "End Then Next",
                    vec![ScriptStep::new(79), scroll(2), next_record()],
                ),
                script(
                    16,
                    "Header End",
                    vec![ScriptStep::new(79), field(1), scroll(2)],
                ),
                script(
                    17,
                    "End Before Pause",
                    vec![scroll(2), report(), ScriptStep::new(62)],
                ),
                script(18, "Pause Selection", vec![ScriptStep::new(62), scroll(5)]),
                script(
                    19,
                    "Pause Fixed Selection",
                    vec![field(1), scroll(2), ScriptStep::new(62), scroll(5)],
                ),
                script(
                    20,
                    "Header Selection Then Other Field",
                    vec![
                        ScriptStep::new(79),
                        field(1),
                        scroll(2),
                        scroll(5),
                        field(2),
                    ],
                ),
                script(
                    21,
                    "Captured Selection Then Field",
                    vec![
                        ScriptStep::new(62),
                        ScriptStep::new(79),
                        scroll(2),
                        scroll(5),
                        field(1),
                    ],
                ),
                script(10, "Background Scroll", background(false)),
                script(11, "Frozen Background Scroll", background(true)),
            ],
            ..Default::default()
        })
    }
    fn can_write_records(&self) -> bool {
        self.editable
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<om_data::RenderLayout>, DataError> {
        if !self.rich {
            return Ok(None);
        }
        use om_layout::{FieldObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};
        let mut fields: Vec<LayoutObject> = (0..self.columns)
            .map(|i| LayoutObject {
                id: i as u64 + 1,
                object_type: 1,
                field_obj: Some(FieldObject {
                    field_spec: Some(om_layout::FieldSpec {
                        table_key: 5,
                        field_key: if self.duplicate && i + 1 == self.columns {
                            1
                        } else {
                            i as u64 + 1
                        },
                        repetition: 1,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                bounds: Some(Rect {
                    left: 20.0,
                    right: 300.0,
                    top: 30.0 + i as f64 * 45.0,
                    bottom: 60.0 + i as f64 * 45.0,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect();
        for (id, label, choice, y) in [
            (1000, "Jump Down", 2, 30.0),
            (
                1001,
                "Jump Up",
                1,
                (self.columns as f64 * 45.0 - 30.0).max(30.0),
            ),
        ] {
            fields.push(LayoutObject {
                id,
                object_type: 10,
                text: Some(label.into()),
                bounds: Some(Rect {
                    left: 400.0,
                    right: 550.0,
                    top: y,
                    bottom: y + 30.0,
                    ..Default::default()
                }),
                button: Some(om_layout::ButtonObject {
                    script_step: Some(om_layout::ScriptStep {
                        id: 81,
                        value: choice,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        if self.header {
            // The selected Code is fixed; only Field 02 repeats with the body.
            fields[0].bounds.as_mut().unwrap().top = 15.0;
            fields[0].bounds.as_mut().unwrap().bottom = 45.0;
            fields[1].bounds.as_mut().unwrap().top = 100.0;
            fields[1].bounds.as_mut().unwrap().bottom = 130.0;
        }
        Ok(Some(om_data::RenderLayout {
            layout: Layout {
                id: 7,
                name: "Long rows".into(),
                def: Some(LayoutDef {
                    base_width: 1500.0,
                    table_alias_key: 5,
                    ..Default::default()
                }),
                parts: if self.header {
                    vec![
                        Part {
                            id: 2,
                            part_type: 1,
                            top: 0.0,
                            height: 80.0,
                            ..Default::default()
                        },
                        Part {
                            id: 1,
                            part_type: 4,
                            top: 80.0,
                            height: 70.0,
                            ..Default::default()
                        },
                    ]
                } else {
                    vec![Part {
                        id: 1,
                        part_type: 4,
                        top: 0.0,
                        height: 30.0 + self.columns as f64 * 45.0,
                        ..Default::default()
                    }]
                },
                objects: ObjectStore {
                    root: (1..=self.columns as u64).chain([1000, 1001]).collect(),
                    objects: fields,
                    ..Default::default()
                },
                ..Default::default()
            },
            field_bindings: (0..self.columns)
                .map(|i| {
                    (
                        i as u64 + 1,
                        if self.duplicate && i + 1 == self.columns {
                            "Code".into()
                        } else {
                            field_name(i)
                        },
                    )
                })
                .collect(),
        }))
    }
}

fn open() -> (common::App, Requests) {
    open_config("List View", 1, false, false)
}

fn open_config(view: &str, columns: usize, rich: bool, editable: bool) -> (common::App, Requests) {
    open_parts(view, columns, rich, editable, false)
}
fn open_parts(
    view: &str,
    columns: usize,
    rich: bool,
    editable: bool,
    header: bool,
) -> (common::App, Requests) {
    let requests = Requests::default();
    let mut app = common::harness(Box::new(Provider {
        requests: requests.clone(),
        columns,
        rich,
        editable,
        header,
        duplicate: false,
    }));
    common::fill(&mut app, "Path", "fmnet:/scroll.invalid/Scroll.fmp12");
    common::click(&mut app, "Open");
    assert_eq!(common::loaded_record_number(&mut app), "1");
    common::click(&mut app, view);
    if header {
        common::wait_for_value(&mut app, "Row 0001");
        common::wait_until(&mut app, "repeated body rows", |a| {
            a.query(By::new().role(egui::Role::ListItem).label("Record 2"))
                .is_some()
        });
    } else if view == "List View" || view == "Table View" {
        if editable {
            common::wait_for_value(&mut app, "Row 0002");
        } else {
            common::wait_for_label(&mut app, "Row 0002");
        }
    } else {
        common::wait_for_label(&mut app, if rich { "Row 0001" } else { "Code" });
    }
    common::open_menu(&mut app, "Scripts");
    common::wait_for_label(&mut app, "Scroll End");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    (app, requests)
}

fn run(app: &mut common::App, name: &str) {
    run_error(app, name, 0)
}
fn run_error(app: &mut common::App, name: &str, error: i32) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
    common::wait_for_label(app, "Scroll result");
    assert!(app.query_by_label(&format!("scroll:{error}")).is_some());
    common::click(app, "OK");
    common::wait_until(app, "scroll script finished", |a| {
        !a.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

fn visible(app: &common::App, name: &str) -> bool {
    app.query_by_label(name).is_some_and(|node| {
        let rect = node.rect();
        let viewport = app.ctx.viewport_rect();
        rect.top() >= 0.0 && rect.bottom() <= viewport.bottom() && rect.intersects(viewport)
    })
}

#[test]
fn scroll_window_end_and_home_move_the_list_without_changing_its_record() {
    let (mut app, requests) = open();
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last page requested by scrolling", |_| {
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|(offset, _)| *offset >= 175)
    });
    common::wait_until(&mut app, "last row visible", |a| visible(a, "Row 0200"));
    assert_eq!(common::record_number(&app), "1");
    run(&mut app, "Scroll Home");
    common::wait_until(&mut app, "first row visible again", |a| {
        visible(a, "Row 0001")
    });
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_pages_are_ordered_even_while_frozen() {
    let (mut app, _) = open();
    run(&mut app, "Frozen Pages");
    common::wait_until(&mut app, "a later row visible after one page", |a| {
        visible(a, "Row 0020")
    });
    assert!(!visible(&app, "Row 0001"));
    run(&mut app, "Scroll Up");
    common::wait_until(&mut app, "home after page up", |a| visible(a, "Row 0001"));
    run(&mut app, "Frozen End");
    common::wait_until(&mut app, "end after script unfreezes", |a| {
        visible(a, "Row 0200")
    });
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_table_end_and_home_keep_the_selected_record() {
    let (mut app, _) = open_config("Table View", 1, false, false);
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last table row", |a| visible(a, "Row 0200"));
    run(&mut app, "Scroll Home");
    common::wait_until(&mut app, "first table row", |a| visible(a, "Row 0001"));
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_form_and_find_use_their_actual_content_height() {
    let (mut app, _) = open_config("Form View", 60, false, false);
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last form field", |a| visible(a, "Field 60"));
    run(&mut app, "Scroll Home");
    common::wait_until(&mut app, "first form field", |a| visible(a, "Code"));
    common::click(&mut app, "Find");
    common::wait_until(&mut app, "find mode", |a| {
        a.state().mode() == om_gui::Mode::Find
    });
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last find field", |a| visible(a, "Field 60"));
    run(&mut app, "Scroll Home");
    common::wait_until(&mut app, "first find field", |a| visible(a, "Code"));
    assert_eq!(app.state().mode(), om_gui::Mode::Find);
}

#[test]
fn scroll_window_styled_form_end_home_and_zoom_keep_geometry() {
    let (mut app, _) = open_config("Form View", 40, true, false);
    run(&mut app, "Zoom Then End");
    common::wait_until(&mut app, "last styled field", |a| visible(a, "Value 40"));
    run(&mut app, "Scroll Home");
    common::wait_until(&mut app, "first styled field", |a| visible(a, "Row 0001"));
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_selection_returns_to_the_current_field_in_virtualized_views() {
    for view in ["List View", "Table View"] {
        let (mut app, _) = open_config(view, 1, true, true);
        // An editor is active, but scrolling must not commit its draft.
        let editor = app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .find(|n| n.rect().intersects(app.ctx.viewport_rect()))
            .unwrap();
        editor.click();
        app.run();
        app.key_press(egui::Key::End);
        app.query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .find(|n| n.rect().intersects(app.ctx.viewport_rect()))
            .unwrap()
            .type_text(" draft");
        app.run();
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Scroll Active Field");
        common::wait_for_label(&mut app, "Continue");
        common::wait_until(&mut app, "last row with editor retained", |a| {
            a.query_all(By::new().value("Row 0200")).any(|n| {
                n.rect().top() >= 0.0 && n.rect().bottom() <= a.ctx.viewport_rect().bottom()
            }) || visible(a, "Row 0200")
        });
        assert!(app.query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .any(|n| n.is_focused() && n.value().is_some_and(|value| value.ends_with(" draft"))),
            "the current draft editor keeps its focus while offscreen");
        common::click(&mut app, "Continue");
        common::wait_for_label(&mut app, "Scroll result");
        app.get_by_label("scroll:0");
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "active field script finished", |a| {
            !a.state().script_running()
        });
        common::wait_until(&mut app, "draft editor visible again", |a| {
            a.query_all(By::new().role(egui::Role::TextInput).label("Code"))
                .any(|n| {
                    n.rect().top() >= 0.0
                        && n.rect().bottom() < a.ctx.viewport_rect().bottom()
                        && n.value().is_some_and(|s| s.ends_with(" draft"))
                })
        });
        assert_eq!(common::record_number(&app), "1");
    }
}

#[test]
fn scroll_window_invalid_options_report_five_and_navigation_discards_stale_scroll() {
    let (mut app, _) = open_config("List View", 60, false, false);
    run_error(&mut app, "Scroll Invalid", 5);
    assert!(visible(&app, "Row 0001"));
    run(&mut app, "Frozen View Change");
    common::wait_until(&mut app, "form view after frozen navigation", |a| {
        a.state().view() == om_gui::ViewMode::Form
    });
    common::wait_until(&mut app, "form stays at top", |a| visible(a, "Code"));
}

#[test]
fn scroll_window_background_and_frozen_background_effects_reach_only_their_owner() {
    for name in ["Background Scroll", "Frozen Background Scroll"] {
        let (mut app, _) = open();
        run(&mut app, name);
        assert_eq!(app.state().window_name(), "Scroll (scroll.invalid)");
        common::wait_until(&mut app, "peer end row", |a| {
            a.query(By::new().role(egui::Role::Window).label("Scroll peer"))
                .is_some_and(|w| w.query_by_label("Row 0200").is_some())
        });
        assert!(visible(&app, "Row 0001"));
        assert!(app
            .query_all(By::new().role(egui::Role::TextInput).label("Record Number"))
            .all(|n| n.value().as_deref() == Some("1")));
    }
}

#[test]
fn scroll_window_preview_without_printable_layout_is_explicit_and_keeps_browse_context() {
    let (mut app, _) = open_config("Form View", 40, false, false);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Preview Mode");
    common::wait_for_label(
        &mut app,
        "Preview unavailable: This provider has no printable layout. Exact Preview is unavailable.",
    );
    run(&mut app, "Scroll End");
    assert_eq!(app.state().mode(), om_gui::Mode::Preview);
    assert!(app.query_by_label("Row 0001").is_none());
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Browse Mode");
    common::wait_until(&mut app, "browse after preview", |a| {
        a.state().mode() == om_gui::Mode::Browse
    });
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_runs_as_a_layout_buttons_single_step() {
    let (mut app, _) = open_config("Form View", 40, true, false);
    app.get_by_role_and_label(egui::Role::Button, "Jump Down")
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "button moved to bottom", |a| {
        !a.state().script_running()
            && visible_button(a, "Jump Up")
            && !visible_button(a, "Jump Down")
    });
    app.get_by_role_and_label(egui::Role::Button, "Jump Up")
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "button moved to top", |a| {
        !a.state().script_running()
            && visible_button(a, "Jump Down")
            && !visible_button(a, "Jump Up")
    });
    assert_eq!(common::record_number(&app), "1");
    assert!(app.state().error().is_none());
}

fn visible_button(app: &common::App, name: &str) -> bool {
    app.query_by_role_and_label(egui::Role::Button, name)
        .is_some_and(|n| {
            let rect = n.rect();
            rect.top() >= 0.0 && rect.bottom() <= app.ctx.viewport_rect().bottom()
        })
}

#[test]
fn scroll_window_review_frozen_next_record_preserves_form_scroll() {
    let (mut app, _) = open_config("Form View", 40, true, false);
    run(&mut app, "End Then Next");
    assert_eq!(common::record_number(&app), "2");
    assert!(
        visible(&app, "Value 40"),
        "record navigation on the same Form must retain End while frozen"
    );
}

#[test]
fn scroll_window_review_fixed_header_selection_preserves_body_scroll() {
    let (mut app, _) = open_parts("List View", 2, true, true, true);
    let at_end = |a: &common::App| {
        a.query_all(By::new().role(egui::Role::ListItem).label("Record 200"))
            .any(|n| n.rect().intersects(a.ctx.viewport_rect()))
    };
    run(&mut app, "Header Selection");
    assert!(
        at_end(&app),
        "an already-visible Header field must not move the body from End"
    );
    assert!(app
        .query_by_role_and_label(egui::Role::TextInput, "Code")
        .is_some_and(|n| n.rect().top() >= 0.0 && n.rect().bottom() < 200.0));
    run(&mut app, "Scroll Home");
    run(&mut app, "Header End");
    assert!(at_end(&app), "paired End control reaches the last row");
}

#[test]
fn scroll_window_end_stays_visible_when_pause_controls_appear_later() {
    for view in ["List View", "Table View"] {
        let (mut app, _) = open_config(view, 1, false, false);
        let at_end = |a: &common::App| {
            a.query_all(By::new().value("Row 0200")).any(|n| {
                n.rect().top() >= 0.0 && n.rect().bottom() <= a.ctx.viewport_rect().bottom()
            })
        };
        // Page fetches are separate worker jobs. Cache both ends before a
        // blocking dialog, so this tests viewport changes rather than loading.
        run(&mut app, "Scroll End");
        common::wait_until(&mut app, "last page cached", at_end);
        run(&mut app, "Scroll Home");
        common::wait_until(&mut app, "home before delayed pause", |a| {
            visible(a, "Row 0001")
        });
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "End Before Pause");
        common::wait_for_label(&mut app, "Scroll result");
        // The blocking dialog makes End paint before Pause is executed.
        common::wait_until(&mut app, "end painted before pause", at_end);
        common::click(&mut app, "OK");
        common::wait_for_label(&mut app, "Continue");
        common::wait_until(&mut app, "end remains visible after pause bar", at_end);
        common::click(&mut app, "Continue");
        common::wait_for_label(&mut app, "Scroll result");
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "late pause script ends", |a| {
            !a.state().script_running()
        });
        assert_eq!(common::record_number(&app), "1");
    }
}

#[test]
fn scroll_window_selection_targets_the_focused_duplicate_object() {
    // Both ends matter: neither first-wins nor last-wins identifies focus.
    for last in [false, true] {
        let mut app = common::harness(Box::new(Provider {
            requests: Requests::default(),
            columns: 40,
            rich: true,
            editable: true,
            header: false,
            duplicate: true,
        }));
        common::fill(&mut app, "Path", "fmnet:/scroll.invalid/Scroll.fmp12");
        common::click(&mut app, "Open");
        assert_eq!(common::loaded_record_number(&mut app), "1");
        common::click(&mut app, "Form View");
        common::wait_until(&mut app, "duplicate field objects", |a| {
            a.query_all(By::new().role(egui::Role::TextInput).value("Row 0001"))
                .count()
                == 2
        });
        if last {
            run(&mut app, "Scroll End");
        }
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Pause Selection");
        common::wait_for_label(&mut app, "Continue");
        let visible = app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .find(|n| {
                n.rect().top() > 100.0 && n.rect().bottom() < app.ctx.viewport_rect().bottom()
            })
            .expect("the chosen duplicate is visible before selection");
        visible.click();
        app.run_steps(3);
        assert!(app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .any(|n| n.is_focused()));
        app.hover_at(egui::pos2(800.0, 350.0));
        app.run_steps(2);
        app.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, if last { 800.0 } else { -800.0 }),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(30);
        let mut before = app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .map(|node| node.rect())
            .collect::<Vec<_>>();
        before.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let target = before[usize::from(last)];
        assert!(
            target.bottom() < 100.0 || target.top() > app.ctx.viewport_rect().bottom(),
            "wheel must hide the focused duplicate before selection (last={last}): {before:?}"
        );
        common::click(&mut app, "Continue");
        common::wait_for_label(&mut app, "Scroll result");
        let mut editors = app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .map(|n| n.rect())
            .collect::<Vec<_>>();
        editors.sort_by(|a, b| a.top().total_cmp(&b.top()));
        assert_eq!(editors.len(), 2);
        let target = editors[usize::from(last)];
        assert!(
            target.top() > 100.0 && target.bottom() <= app.ctx.viewport_rect().bottom() + 0.5,
            "selection hid the focused duplicate (last={last}): {editors:?}"
        );
    }
}

#[test]
fn scroll_window_fixed_selection_preserves_manual_wheel_position() {
    let (mut app, _) = open_parts("List View", 2, true, true, true);
    let last = |a: &common::App| {
        a.query_all(By::new().role(egui::Role::ListItem).label("Record 200"))
            .any(|n| n.rect().intersects(a.ctx.viewport_rect()))
    };
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last page cached before pause", last);
    run(&mut app, "Scroll Home");
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Pause Fixed Selection");
    common::wait_for_label(&mut app, "Continue");
    common::wait_until(&mut app, "End before manual wheel", last);
    app.hover_at(egui::pos2(800.0, 350.0));
    app.run_steps(2);
    app.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 800.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    app.run_steps(30);
    assert!(!last(&app), "wheel must leave End before selection");
    common::click(&mut app, "Continue");
    common::wait_for_label(&mut app, "Scroll result");
    assert!(
        !last(&app),
        "fixed Header selection reactivated the released End anchor"
    );
}

fn open_header_duplicates(columns: usize) -> common::App {
    let mut app = common::harness(Box::new(Provider {
        requests: Requests::default(),
        columns,
        rich: true,
        editable: true,
        header: true,
        duplicate: true,
    }));
    common::fill(&mut app, "Path", "fmnet:/scroll.invalid/Scroll.fmp12");
    common::click(&mut app, "Open");
    assert_eq!(common::loaded_record_number(&mut app), "1");
    common::click(&mut app, "List View");
    common::wait_until(&mut app, "Header Code", |a| {
        a.query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .any(|n| n.rect().top() > 100.0 && n.rect().bottom() < 220.0)
    });
    common::open_menu(&mut app, "Scripts");
    common::wait_for_label(&mut app, "Header Selection");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    app
}

fn go_to_header_duplicate(previous_body: bool) {
    let mut app = open_header_duplicates(2);
    let last = |a: &common::App| {
        a.query_all(By::new().role(egui::Role::ListItem).label("Record 200"))
            .any(|n| n.rect().intersects(a.ctx.viewport_rect()))
    };
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "end cached", last);
    run(&mut app, "Scroll Home");
    if previous_body {
        app.query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .find(|n| n.rect().top() > 180.0 && n.rect().bottom() < 250.0)
            .unwrap()
            .click();
        app.run_steps(3);
        assert!(app
            .query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .any(|n| n.is_focused() && n.rect().top() > 180.0));
    }
    run(&mut app, "Header Selection");
    app.run_steps(3);
    assert!(
        app.query_all(By::new().role(egui::Role::TextInput).label("Code"))
            .any(|n| n.is_focused() && n.rect().bottom() < 180.0),
        "Go to Field must activate Header Code"
    );
    assert!(last(&app),"To Selection moved the Body while Header Code is active (previous Body focus={previous_body})");
}

#[test]
fn scroll_window_go_to_field_resolves_the_header_duplicate() {
    go_to_header_duplicate(false);
}
#[test]
fn scroll_window_go_to_field_does_not_inherit_a_previous_body_duplicate() {
    go_to_header_duplicate(true);
}

#[test]
fn scroll_window_programmatic_selection_keeps_the_field_of_its_own_step() {
    let mut app = open_header_duplicates(3);
    let last = |a: &common::App| {
        a.query_all(By::new().role(egui::Role::ListItem).label("Record 200"))
            .any(|n| n.rect().intersects(a.ctx.viewport_rect()))
    };
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last page cached", last);
    run(&mut app, "Scroll Home");
    run(&mut app, "Header Selection Then Other Field");
    app.run_steps(3);
    assert!(
        last(&app),
        "a later Go to Field replaced the queued Header selection target"
    );
    assert_eq!(common::record_number(&app), "1");
}

#[test]
fn scroll_window_captured_selection_survives_a_later_programmatic_focus() {
    let mut app = open_header_duplicates(2);
    let last = |a: &common::App| {
        a.query_all(By::new().role(egui::Role::ListItem).label("Record 200"))
            .any(|n| n.rect().intersects(a.ctx.viewport_rect()))
    };
    run(&mut app, "Scroll End");
    common::wait_until(&mut app, "last page cached", last);
    run(&mut app, "Scroll Home");
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Captured Selection Then Field");
    common::wait_for_label(&mut app, "Continue");
    let body = app
        .query_all(
            By::new()
                .role(egui::Role::TextInput)
                .label("Code")
                .value("Row 0001"),
        )
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("current Body duplicate is painted alongside Header");
    assert!(
        body.rect().intersects(app.ctx.viewport_rect()),
        "current Body must be visible before clicking"
    );
    body.click();
    app.run_steps(3);
    assert!(app
        .query_all(By::new().role(egui::Role::TextInput).label("Code"))
        .any(|n| n.is_focused() && n.value().as_deref() == Some("Row 0001")));
    common::click(&mut app, "Continue");
    common::wait_for_label(&mut app, "Scroll result");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "script done", |a| !a.state().script_running());
    let first = app
        .query_all(By::new().role(egui::Role::ListItem).label("Record 1"))
        .any(|n| n.rect().intersects(app.ctx.viewport_rect()));
    assert!(
        first && !last(&app),
        "the final Header focus replaced an earlier captured Body selection"
    );
}
