//! Close Popover through the real GUI script worker, with synthetic data only.
mod common;

use std::collections::BTreeMap;

use egui::Role;
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep,
};
use om_layout::{
    ButtonObject, FieldObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part,
    PopoverButtonObject, PopoverObject, Rect,
};

#[derive(Default)]
struct Provider {
    field_triggers: bool,
    cancel: Option<u16>,
    recursive_exit: bool,
    trace_report: bool,
    saved: BTreeMap<(i64, String), Option<String>>,
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
        Ok(vec![
            LayoutInfo {
                id: 7,
                name: "Entry".into(),
            },
            LayoutInfo {
                id: 8,
                name: "Other".into(),
            },
        ])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: vec![
                Field {
                    name: "Note".into(),
                    kind: FieldKind::Text,
                },
                Field {
                    name: "Outside".into(),
                    kind: FieldKind::Text,
                },
            ],
            records: [(19, "Stored note"), (37, "Second note")]
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(|(id, note)| Record {
                    id,
                    values: vec![
                        self.saved
                            .get(&(id, "Note".into()))
                            .cloned()
                            .unwrap_or_else(|| Some(note.into())),
                        self.saved
                            .get(&(id, "Outside".into()))
                            .cloned()
                            .unwrap_or_else(|| Some("Outside value".into())),
                    ],
                })
                .collect(),
            total: 2,
        })
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        for (name, value) in values {
            self.saved.insert((id, name), value);
        }
        Ok(Record {
            id,
            values: vec![
                self.saved
                    .get(&(id, "Note".into()))
                    .cloned()
                    .unwrap_or_else(|| {
                        Some(
                            if id == 19 {
                                "Stored note"
                            } else {
                                "Second note"
                            }
                            .into(),
                        )
                    }),
                self.saved
                    .get(&(id, "Outside".into()))
                    .cloned()
                    .unwrap_or_else(|| Some("Outside value".into())),
            ],
        })
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        let mut doc = document(id);
        if self.field_triggers {
            for (object, event, script) in [(4, 6, 5), (4, 4, 6), (4, 2, 7), (3, 2, 8), (2, 1, 9)] {
                doc.layout
                    .objects
                    .objects
                    .iter_mut()
                    .find(|o| o.id == object)
                    .unwrap()
                    .triggers
                    .push(om_layout::ScriptTrigger {
                        event,
                        script: Some(script),
                        modes: 1,
                        file_reference: None,
                        external: false,
                        option: 0,
                        extra: vec![],
                        parameter: None,
                    });
            }
        }
        Ok(Some(doc))
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        let mut focus = ScriptStep::new(145);
        focus.set_calculation(0, Some(&calc("\"note\"")));
        let mut set = ScriptStep::new(147);
        set.set_calculation(1, Some(&calc("\"Note\"")));
        set.set_calculation(0, Some(&calc("\"Unsaved draft\"")));
        let mut scripts = vec![
            script(
                1,
                "Close and continue",
                vec![
                    ScriptStep::new(169),
                    show(if self.recursive_exit || self.trace_report {
                        "\"continued:\" & Get ( LastError ) & \":\" & $$trace"
                    } else {
                        "\"continued:\" & Get ( LastError )"
                    }),
                ],
            ),
            script(
                2,
                "Nested close",
                vec![
                    perform(3),
                    show("\"parent:\" & Get ( ScriptResult ) & \":\" & Get ( LastError )"),
                ],
            ),
            script(
                3,
                "Close child",
                vec![ScriptStep::new(169), exit("\"child:\" & Get ( LastError )")],
            ),
            script(
                4,
                "Close with draft",
                vec![
                    note("$$trace", "\"\""),
                    focus,
                    set,
                    ScriptStep::new(169),
                    note("$error", "Get ( LastError )"),
                    show("$$trace & \":\" & $error"),
                ],
            ),
        ];
        for (id, event, mark) in [
            (5, 6, "V"),
            (6, 4, "S"),
            (7, 2, "F"),
            (8, 2, "P"),
            (9, 1, "E"),
        ] {
            let mut steps = vec![note("$$trace", &format!("$$trace & \"{mark}\""))];
            if id == 8 && self.recursive_exit {
                steps.push(ScriptStep::new(169));
            }
            steps.push(exit(
                if self.cancel == Some(if id == 8 { 1002 } else { event }) {
                    "0"
                } else {
                    "1"
                },
            ));
            scripts.push(script(id, &format!("Trigger {mark}"), steps));
        }
        let mut outside = ScriptStep::new(145);
        outside.set_calculation(0, Some(&calc("\"outside\"")));
        scripts.push(script(
            12,
            "Close then focus",
            vec![ScriptStep::new(169), outside],
        ));
        let mut other_draft = ScriptStep::new(147);
        other_draft.set_calculation(1, Some(&calc("\"Outside\"")));
        other_draft.set_calculation(0, Some(&calc("\"Other draft\"")));
        let mut clean_focus = ScriptStep::new(145);
        clean_focus.set_calculation(0, Some(&calc("\"note\"")));
        scripts.push(script(
            14,
            "Close clean field",
            vec![
                note("$$trace", "\"\""),
                other_draft,
                clean_focus,
                ScriptStep::new(169),
                note("$error", "Get ( LastError )"),
                show("$$trace & \":\" & $error"),
            ],
        ));
        let mut list = ScriptStep::new(30);
        list.value = 2;
        scripts.push(script(13, "List view", vec![list]));
        scripts.push(script(10, "Close quietly", vec![ScriptStep::new(169)]));
        let mut new = ScriptStep::new(122);
        new.set_calculation(0, Some(&calc("\"Second\"")));
        let mut layout = ScriptStep::new(6);
        layout.reserved[..4].copy_from_slice(&[2, 1, 8, 0xbe]);
        layout.value = 5;
        layout.flags = 0x8;
        scripts.push(script(11, "Open second", vec![new, layout]));
        Ok(ScriptCatalog {
            scripts,
            ..Default::default()
        })
    }
}

fn calc(source: &str) -> om_data::Calculation {
    common::workspace::calc(source)
}
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Popover script\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
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
fn button(id: u64, parent: Option<u64>, label: &str, script: u8, x: f64, y: f64) -> LayoutObject {
    let mut reserved = vec![2, 1, script, 0xbe];
    reserved.resize(14, 0);
    LayoutObject {
        text: Some(label.into()),
        button: Some(ButtonObject {
            script_step: Some(om_layout::ScriptStep {
                id: 1,
                reserved,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(id, 10, parent, x, y, 170.0, 28.0)
    }
}
fn document(id: u32) -> RenderLayout {
    RenderLayout {
        layout: Layout {
            id: u64::from(id),
            name: if id == 7 { "Entry" } else { "Other" }.into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                height: 330.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![7, 1, 2, 3, 6],
                objects: vec![
                    LayoutObject {
                        text: Some("SCRIPT POPOVER FIXTURE".into()),
                        ..object(1, 2, None, 20.0, 5.0, 300.0, 24.0)
                    },
                    LayoutObject {
                        text: Some(if id == 7 { "Details" } else { "Other details" }.into()),
                        popover_button: Some(PopoverButtonObject {
                            popover_key: 3,
                            ..Default::default()
                        }),
                        ..object(2, 14, None, 20.0, 40.0, 160.0, 28.0)
                    },
                    LayoutObject {
                        children: vec![4, 5],
                        popover: Some(PopoverObject {
                            button_key: 2,
                            ..Default::default()
                        }),
                        ..object(3, 15, None, 20.0, 80.0, 300.0, 150.0)
                    },
                    LayoutObject {
                        name: Some("note".into()),
                        field_obj: Some(FieldObject {
                            field_spec: Some(om_layout::FieldSpec {
                                field_key: 1,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..object(4, 1, Some(3), 10.0, 10.0, 250.0, 28.0)
                    },
                    button(
                        5,
                        Some(3),
                        if id == 7 {
                            "Close by script"
                        } else {
                            "Other close"
                        },
                        1,
                        10.0,
                        60.0,
                    ),
                    button(6, None, "Run close", 1, 350.0, 40.0),
                    LayoutObject {
                        name: Some("outside".into()),
                        field_obj: Some(FieldObject {
                            field_spec: Some(om_layout::FieldSpec {
                                field_key: 2,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..object(7, 1, None, 350.0, 100.0, 200.0, 28.0)
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([(4, "Note".into()), (7, "Outside".into())]),
    }
}

fn open() -> common::App {
    open_with(Provider::default())
}
fn open_with(provider: Provider) -> common::App {
    connect(common::harness(Box::new(provider)))
}
fn connect(mut app: common::App) -> common::App {
    common::fill(&mut app, "Host", "popovers.example.invalid");
    common::fill(&mut app, "File", "Popovers.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "SCRIPT POPOVER FIXTURE");
    common::loaded_record_number(&mut app);
    app
}
fn press(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(3);
}
fn assert_closed(app: &common::App) {
    assert!(
        app.query_by_role_and_label(Role::Button, "Close by script")
            .is_none(),
        "Close Popover must dismiss its visible contents"
    );
    assert_eq!(
        app.get_by_role_and_label(Role::Button, "Details")
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::False)
    );
}

#[test]
fn contained_close_popover_button_dismisses_before_the_next_step() {
    let mut app = open();
    press(&mut app, "Details");
    common::wait_until(&mut app, "the popover contents", |a| {
        a.query_by_role_and_label(Role::Button, "Close by script")
            .is_some()
    });
    press(&mut app, "Close by script");
    common::wait_for_label(&mut app, "continued:0");
    assert_closed(&app);
    assert!(app.state().error().is_none());
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    }
}
fn perform(id: u8) -> ScriptStep {
    let mut step = ScriptStep::new(1);
    step.set_script_key(om_data::StoredKey::from_id(u64::from(id)));
    step
}
fn exit(result: &str) -> ScriptStep {
    let mut step = ScriptStep::new(103);
    step.set_calculation(0, Some(&calc(result)));
    step
}
fn note(name: &str, value: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(value)));
    step
}
fn run(app: &mut common::App, script: &str) {
    app.query_all(By::new().role(Role::Button).label("Scripts"))
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(app, script);
}
fn finish(app: &mut common::App) {
    common::click(app, "OK");
    common::wait_until(app, "script finished", |a| !a.state().script_running());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn no_popover_is_a_successful_noop() {
    let mut app = open_with(Provider {
        field_triggers: true,
        ..Default::default()
    });
    run(&mut app, "Close and continue");
    common::wait_for_label(&mut app, "continued:0");
    assert_closed(&app);
    finish(&mut app);
}

#[test]
fn nested_script_closes_and_returns_its_result_to_the_caller() {
    let mut app = open();
    press(&mut app, "Details");
    run(&mut app, "Nested close");
    report(&mut app, "parent:child:0:0");
    assert_closed(&app);
    finish(&mut app);
}

#[test]
fn close_orders_field_and_popover_triggers_and_retains_the_draft() {
    let mut app = open_with(Provider {
        field_triggers: true,
        ..Default::default()
    });
    press(&mut app, "Details");
    run(&mut app, "Close with draft");
    report(&mut app, "VSFPE:0");
    assert_closed(&app);
    finish(&mut app);

    press(&mut app, "Details");
    common::wait_for_value(&mut app, "Unsaved draft");
}

#[test]
fn field_validation_or_exit_can_cancel_without_losing_the_draft() {
    for (event, trace) in [(6, "V:20"), (4, "VS:20"), (2, "VSF:20")] {
        let mut app = open_with(Provider {
            field_triggers: true,
            cancel: Some(event),
            ..Default::default()
        });
        press(&mut app, "Details");
        run(&mut app, "Close with draft");
        report(&mut app, trace);
        assert!(app
            .query_by_role_and_label(Role::Button, "Close by script")
            .is_some());
        common::wait_for_value(&mut app, "Unsaved draft");
        finish(&mut app);
        assert!(common::text_field(&app, "Note").is_focused());
    }
}

#[test]
fn popover_exit_can_veto_the_close_after_field_exit() {
    let mut app = open_with(Provider {
        field_triggers: true,
        cancel: Some(1002),
        ..Default::default()
    });
    press(&mut app, "Details");
    run(&mut app, "Close with draft");
    report(&mut app, "VSFP:20");
    assert!(app
        .query_by_role_and_label(Role::Button, "Close by script")
        .is_some());
    common::wait_for_value(&mut app, "Unsaved draft");
    finish(&mut app);
    assert!(common::text_field(&app, "Note").is_focused());
}

#[test]
fn a_close_inside_its_own_exit_trigger_does_not_recurse_or_enter_twice() {
    let mut app = open_with(Provider {
        field_triggers: true,
        recursive_exit: true,
        ..Default::default()
    });
    press(&mut app, "Details");
    run(&mut app, "Close and continue");
    common::wait_for_label(&mut app, "continued:0:PE");
    assert_closed(&app);
    finish(&mut app);
}

#[test]
fn close_returns_focus_to_its_button_without_committing_the_record() {
    let mut app = open();
    press(&mut app, "Details");
    run(&mut app, "Close quietly");
    common::wait_until(&mut app, "quiet script finished", |a| {
        !a.state().script_running()
    });
    assert_closed(&app);
    assert!(app
        .get_by_role_and_label(Role::Button, "Details")
        .is_focused());
}

#[test]
fn two_windows_keep_independent_popovers_and_manual_controls() {
    let mut app = open();
    press(&mut app, "Details");
    run(&mut app, "Open second");
    common::wait_until(&mut app, "second window", |a| {
        !a.state().script_running() && a.state().window_name() == "Second"
    });
    assert_eq!(app.state_mut().window_titles().len(), 2);
    press(&mut app, "Other details");
    assert!(app
        .query_by_role_and_label(Role::Button, "Other close")
        .is_some());
    assert!(app
        .query_by_role_and_label(Role::Button, "Close by script")
        .is_some());
    run(&mut app, "Close and continue");
    common::wait_for_label(&mut app, "continued:0");
    assert!(app
        .query_by_role_and_label(Role::Button, "Other close")
        .is_none());
    assert!(app
        .query_by_role_and_label(Role::Button, "Close by script")
        .is_some());
    finish(&mut app);
    // Its manual toggle still works after the script removed the surface.
    press(&mut app, "Other details");
    assert!(app
        .query_by_role_and_label(Role::Button, "Other close")
        .is_some());
    press(&mut app, "Other details");
    assert!(app
        .query_by_role_and_label(Role::Button, "Other close")
        .is_none());
    assert!(app
        .query_by_role_and_label(Role::Button, "Close by script")
        .is_some());
}

fn report(app: &mut common::App, expected: &str) {
    common::wait_until(app, "script report", |a| {
        a.query_by_label("Popover script").is_some() || a.state().error().is_some()
    });
    assert!(
        app.query_by_label(expected).is_some(),
        "expected {expected}, error {:?}, labels {:?}",
        app.state().error(),
        app.query_all(By::new().role(Role::Label))
            .map(|n| n.value())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_later_script_focus_step_wins_over_returning_to_the_popover_button() {
    let mut app = open();
    press(&mut app, "Details");
    run(&mut app, "Close then focus");
    common::wait_until(&mut app, "later field focus", |a| {
        !a.state().script_running()
    });
    assert!(common::text_field(&app, "Outside").is_focused());
    assert_closed(&app);
    assert!(!app
        .get_by_role_and_label(Role::Button, "Details")
        .is_focused());
}

#[test]
fn list_view_returns_focus_to_the_opener_on_the_current_record() {
    let mut app = connect(common::harness_sized(
        Box::new(Provider::default()),
        egui::vec2(1060.0, 1100.0),
    ));
    run(&mut app, "List view");
    common::wait_until(&mut app, "two list rows", |a| {
        !a.state().script_running()
            && a.query_all(By::new().role(Role::Button).label("Details"))
                .count()
                == 2
    });
    app.query_all(By::new().role(Role::Button).label("Details"))
        .last()
        .unwrap()
        .click();
    app.run_steps(3);
    common::wait_until(&mut app, "second record popover", |a| {
        common::record_number(a) == "2"
            && a.query_by_role_and_label(Role::Button, "Close by script")
                .is_some()
    });
    run(&mut app, "Close quietly");
    common::wait_until(&mut app, "list close finished", |a| {
        !a.state().script_running()
    });
    let openers: Vec<_> = app
        .query_all(By::new().role(Role::Button).label("Details"))
        .collect();
    assert_eq!(openers.len(), 2);
    assert!(
        !openers[0].is_focused(),
        "the first record must not take the current record's focus"
    );
    assert!(
        openers[1].is_focused(),
        "the current record's opener must receive focus"
    );
    assert!(app
        .query_by_role_and_label(Role::Button, "Close by script")
        .is_none());
    assert_eq!(common::record_number(&app), "2");
}

#[test]
fn another_fields_draft_does_not_validate_or_save_the_clean_active_field() {
    let mut app = open_with(Provider {
        field_triggers: true,
        ..Default::default()
    });
    press(&mut app, "Details");
    run(&mut app, "Close clean field");
    report(&mut app, "FPE:0");
    assert_closed(&app);
    finish(&mut app);
    common::wait_for_value(&mut app, "Other draft");
    press(&mut app, "Details");
    common::wait_for_value(&mut app, "Stored note");
}

#[test]
fn the_first_contained_button_click_closes_after_exit_triggers_and_preserves_typing() {
    let mut app = open_with(Provider {
        field_triggers: true,
        trace_report: true,
        ..Default::default()
    });
    press(&mut app, "Details");
    common::fill(&mut app, "Note", " typed before one click");
    let typed = common::text_field(&app, "Note").value().unwrap();
    assert!(typed.contains("typed before one click"));
    press(&mut app, "Close by script");
    report(&mut app, "continued:0:VSFPE");
    assert_closed(&app);
    finish(&mut app);
    press(&mut app, "Details");
    common::wait_for_value(&mut app, &typed);
}
