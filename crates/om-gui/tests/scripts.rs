//! Script execution through actual GUI buttons, menu items and workspace.
//! Synthetic native-shaped definitions keep these checks independent of a server.
mod common;
use egui_kittest::kittest::{By, Queryable};
use om_data::{
    Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, HostedFile,
    LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep, StoredKey,
};
use om_layout::{
    ButtonObject, FieldObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    events: Vec<String>,
    scripts: ScriptCatalog,
    parameter_buttons: bool,
    field_exit_triggers: bool,
    /// OnObjectKeystroke on the Given field and OnLayoutKeystroke on Entry.
    keystroke_triggers: bool,
    second_keystroke_field: bool,
    found: om_data::found::MemoryFoundSet,
    peer: Option<Arc<Mutex<State>>>,
    /// How long a record update takes to answer, as on a loaded machine.
    update_delay: Duration,
}
struct Provider(Arc<Mutex<State>>);
impl DataProvider for Provider {
    fn open_peer(&mut self, file: &str) -> Result<Box<dyn DataProvider>, DataError> {
        assert_eq!(file, "Peer.fmp12");
        let mut state = self.0.lock().unwrap();
        state.events.push("open:Peer".into());
        let peer = state
            .peer
            .get_or_insert_with(|| {
                Arc::new(Mutex::new(State {
                    scripts: scripts(),
                    rows: vec![Record {
                        id: 1,
                        values: vec![Some("Peer original".into())],
                    }],
                    ..Default::default()
                }))
            })
            .clone();
        Ok(Box::new(Provider(peer)))
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<om_data::host::CalcSchema>>, DataError> {
        let state = self.0.lock().unwrap();
        Ok(
            (state.parameter_buttons || state.field_exit_triggers || state.keystroke_triggers)
                .then(|| Arc::new(button_schema())),
        )
    }
    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        use om_data::host::{MemorySource, RecordValues, Recorder};
        let state = self.0.lock().unwrap();
        if !state.parameter_buttons && !state.field_exit_triggers && !state.keystroke_triggers {
            return Ok(Default::default());
        }
        let schema = button_schema();
        let cx = om_data::calc::Ctx::new(
            &schema.locale,
            om_data::calc::Date::from_ymd(2026, 1, 2).unwrap(),
        );
        let mut source = MemorySource::new();
        for row in &state.rows {
            let mut values = RecordValues::default();
            values.set(
                1,
                1,
                om_data::calc::Value::text(row.values[0].as_deref().unwrap_or("")),
            );
            source.put_record(0x8002, row.id as u64, values);
        }
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        Ok(recorder.take())
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
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
                name: "Details".into(),
            },
        ])
    }
    fn layout_menu(&mut self) -> Result<Vec<om_data::LayoutMenuEntry>, DataError> {
        Ok([
            (20, "Folder", 9),
            (21, "Nested", 1),
            (7, "Entry", 0),
            (22, "--", 2),
            (23, "-", 0x10),
            (8, "Details", 8),
            (24, "--", 2),
        ]
        .into_iter()
        .map(|(id, name, flags)| om_data::LayoutMenuEntry {
            id,
            name: name.into(),
            flags,
        })
        .collect())
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.0.lock().unwrap().scripts.clone())
    }
    fn save_script(&mut self, script: Script) -> Result<Script, DataError> {
        let mut state = self.0.lock().unwrap();
        let entry = state
            .scripts
            .scripts
            .iter_mut()
            .find(|s| s.id == script.id)
            .ok_or_else(|| DataError::new("Missing script"))?;
        *entry = script.clone();
        state.events.push("script-save".into());
        Ok(script)
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let state = self.0.lock().unwrap();
        Ok(state.found.page(
            vec![Field {
                name: "Given".into(),
                kind: FieldKind::Text,
            }],
            &state.rows,
            offset,
            limit,
        ))
    }
    fn change_found_set(
        &mut self,
        _: u32,
        change: om_data::found::FoundSetChange,
    ) -> Result<om_data::found::FoundSet, DataError> {
        let mut state = self.0.lock().unwrap();
        let State { rows, found, .. } = &mut *state;
        found.apply(
            &[Field {
                name: "Given".into(),
                kind: FieldKind::Text,
            }],
            rows,
            change,
        )
    }
    fn create_record(&mut self, _: u32, values: FieldChanges) -> Result<Record, DataError> {
        let mut state = self.0.lock().unwrap();
        let row = Record {
            id: state.rows.iter().map(|r| r.id).max().unwrap_or(0) + 1,
            values: vec![values.get("Given").cloned().flatten()],
        };
        state.rows.push(row.clone());
        state.events.push("create".into());
        Ok(row)
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        let delay = self.0.lock().unwrap().update_delay;
        std::thread::sleep(delay);
        let mut state = self.0.lock().unwrap();
        let row = state
            .rows
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| DataError::new("Missing record"))?;
        if let Some(value) = values.get("Given") {
            row.values[0] = value.clone();
        }
        let row = row.clone();
        state.events.push("update".into());
        Ok(row)
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        let state = self.0.lock().unwrap();
        let mut layout = if state.parameter_buttons {
            parameterized_render(id)
        } else {
            render(id)
        };
        let triggers = state.field_exit_triggers || state.keystroke_triggers;
        if triggers {
            layout.layout.def.as_mut().unwrap().table_alias_key = 0x104082;
        }
        let trigger = |event, script| om_layout::ScriptTrigger {
            event,
            script: Some(script),
            modes: 1,
            file_reference: None,
            external: false,
            option: 0,
            extra: vec![],
            parameter: None,
        };
        if triggers && id == 7 {
            let field = layout
                .layout
                .objects
                .objects
                .iter_mut()
                .find(|o| o.id == 20)
                .unwrap();
            field.field_obj.as_mut().unwrap().field_spec = Some(om_layout::FieldSpec {
                table_key: 0x104082,
                field_key: 1,
                ..Default::default()
            });
            if state.field_exit_triggers {
                for (event, script) in [(6, 40), (4, 41), (2, 42)] {
                    field.triggers.push(trigger(event, script));
                }
            }
            if state.keystroke_triggers {
                field.triggers.push(trigger(5, 50));
                if state.second_keystroke_field {
                    layout.layout.parts[1].height = 450.0;
                    layout.layout.objects.root.push(21);
                    layout.layout.objects.objects.push(LayoutObject {
                        field_obj: Some(FieldObject::default()),
                        placeholder: Some("Given".into()),
                        ..object(21, 1, "", 340.0)
                    });
                    layout.field_bindings.insert(21, "Given".into());
                }
                layout.layout.triggers.push(trigger(102, 51));
                layout.layout.objects.root.push(18);
                layout
                    .layout
                    .objects
                    .objects
                    .push(button(18, "Show keys", 52, 300.0));
            }
        }
        Ok(Some(layout))
    }
    fn close(&mut self) {}
}
fn native_step(opcode: i16, key: u8) -> ScriptStep {
    ScriptStep {
        data_key: StoredKey([2, 1, key, 0, 0xbe, 0xbe]),
        opcode,
        ..Default::default()
    }
}
fn literal(text: &str) -> Calculation {
    // ASCII masked SCSU literal, matching the native Contacts expressions.
    let mut value = vec![0x13, text.len() as u8];
    value.extend(text.bytes().map(|b| b ^ 0x5a));
    value.push(0);
    Calculation {
        polish: Some(value),
        ..Default::default()
    }
}
fn scripts() -> ScriptCatalog {
    let mut set = native_step(147, 2);
    set.set_calculation(1, Some(&literal("Given")));
    set.set_calculation(0, Some(&literal("Created by script")));
    // Go to Layout as FileMaker stores it: the layout key in mReserved
    // with kMemberKey, step value 5 (script-steps.md 3.2).
    let mut navigate = native_step(6, 1);
    navigate.reserved[..4].copy_from_slice(&[2, 1, 8, 0xbe]);
    navigate.value = 5;
    navigate.flags = 0x8;
    ScriptCatalog {
        scripts: vec![
            Script {
                id: 1,
                name: "Create scripted contact".into(),
                steps: vec![native_step(7, 1), set, native_step(75, 3)],
                ..Default::default()
            },
            Script {
                id: 2,
                name: "Open detail card".into(),
                steps: vec![navigate],
                ..Default::default()
            },
            Script {
                id: 3,
                name: "Unsupported action".into(),
                steps: vec![native_step(999, 1), native_step(7, 2)],
                ..Default::default()
            },
        ],
        writable: true,
        unavailable_reason: None,
    }
}
fn object(id: u64, kind: u32, text: &str, y: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        text: Some(text.into()),
        bounds: Some(Rect {
            top: y,
            left: 20.0,
            bottom: y + 30.0,
            right: 280.0,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn button(id: u64, text: &str, target: u8, y: f64) -> LayoutObject {
    let mut reference = vec![2, 1, target, 0xbe];
    reference.resize(14, 0);
    LayoutObject {
        button: Some(ButtonObject {
            script_step: Some(om_layout::ScriptStep {
                id: 1,
                reserved: reference,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(id, 10, text, y)
    }
}
fn render(id: u32) -> RenderLayout {
    let objects = vec![
        object(
            1,
            2,
            if id == 7 {
                "SCRIPTED ENTRY"
            } else {
                "SCRIPTED DETAILS"
            },
            5.0,
        ),
        button(10, "Create Contact", 1, 55.0),
        button(11, "Open Details", 2, 95.0),
        button(12, "Unsupported Action", 3, 135.0),
        LayoutObject {
            button: Some(ButtonObject {
                script_step: Some(om_layout::ScriptStep {
                    id: 7,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..object(13, 10, "New Blank Contact", 230.0)
        },
        LayoutObject {
            script_parameter_unsupported: true,
            ..button(14, "Calculated Parameter", 1, 270.0)
        },
        LayoutObject {
            field_obj: Some(FieldObject::default()),
            placeholder: Some("Given".into()),
            ..object(20, 1, "", 190.0)
        },
    ];
    RenderLayout {
        layout: Layout {
            id: u64::from(id),
            name: if id == 7 { "Entry" } else { "Details" }.into(),
            def: Some(LayoutDef {
                base_width: 800.0,
                ..Default::default()
            }),
            parts: vec![
                Part {
                    id: 1,
                    part_type: 1,
                    top: 0.0,
                    height: 40.0,
                    ..Default::default()
                },
                Part {
                    id: 2,
                    part_type: 4,
                    top: 40.0,
                    height: 300.0,
                    ..Default::default()
                },
            ],
            objects: ObjectStore {
                root: vec![1, 10, 11, 12, 13, 14, 20],
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([(20, "Given".into())]),
    }
}
fn open() -> (common::App, Arc<Mutex<State>>) {
    open_state(State {
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        scripts: scripts(),
        ..Default::default()
    })
}
fn open_state(state: State) -> (common::App, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(state));
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(&mut app, "Host", "scripts.example.invalid");
    common::fill(&mut app, "File", "Scripts.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "SCRIPTED ENTRY");
    // Menus become populated after the asynchronous script catalog read.
    common::open_menu(&mut app, "Scripts");
    common::wait_for_label(&mut app, "Create scripted contact");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    (app, state)
}

fn button_schema() -> om_data::host::CalcSchema {
    use om_data::host::{CalcSchema, FieldDef};
    let mut schema = CalcSchema::new("Scripts", om_data::calc::Locale::en_us());
    schema.add_table(0x8002, "Records");
    schema.add_occurrence(0xd00002, "Records", 0x8002);
    schema.add_field(
        0x8002,
        FieldDef::data(1, "Given", om_data::calc::ValueType::Text),
    );
    schema.add_layout(7, "Entry", Some(0xd00002));
    schema.add_layout(8, "Details", Some(0xd00002));
    schema
}

fn parameterized_render(id: u32) -> RenderLayout {
    let mut layout = render(id);
    layout.layout.def.as_mut().unwrap().table_alias_key = 0x104082;
    layout.layout.parts[1].height = 450.0;
    let schema = button_schema();
    let mut context = om_data::formula::Context::new(schema.names());
    context.occurrence = Some(0xd00002);
    let formula = om_data::formula::Formula {
        context: Some(om_data::formula::Key::from_id(0xd00002)),
        formatted: None,
        polish: om_data::formula::compile("Given", &context).unwrap().expr,
    };
    let mut number = ScriptStep::new(6);
    number.value = 4;
    number.set_calculation(
        5,
        Some(&Calculation {
            polish: Some(formula.encode_polish()),
            ..Default::default()
        }),
    );
    let mut name = ScriptStep::new(6);
    name.value = 3;
    name.set_calculation(5, Some(&literal("Details")));
    let mut set = ScriptStep::new(147);
    set.set_calculation(0, Some(&literal("Changed by button")));
    set.set_calculation(1, Some(&literal("Given")));
    for (object_id, label, step, group, y) in [
        (15, "Calculated number", number, true, 310.0),
        (16, "Calculated name", name, false, 350.0),
        (17, "Single step assignment", set, false, 390.0),
    ] {
        let native = om_layout::ScriptStep {
            id: i32::from(step.opcode),
            value: u32::from(step.value),
            flags: step.flags as i32,
            reserved: step.reserved.to_vec(),
            data: step.data.as_ref().map(|data| data.encode(&[17])),
            ..Default::default()
        };
        let mut object = object(object_id, if group { 8 } else { 10 }, label, y);
        if group {
            object.group = Some(om_layout::GroupObject {
                script_step: Some(native),
                ..Default::default()
            });
        } else {
            object.button = Some(ButtonObject {
                script_step: Some(native),
                ..Default::default()
            });
        }
        layout.layout.objects.root.push(object_id);
        layout.layout.objects.objects.push(object);
    }
    layout
}

#[test]
fn parameterized_buttons_use_current_record_calculations_and_shared_assignment_steps() {
    let (mut app, state) = open_state(State {
        rows: vec![
            Record {
                id: 21,
                values: vec![Some("2".into())],
            },
            Record {
                id: 42,
                values: vec![Some("1".into())],
            },
        ],
        scripts: scripts(),
        parameter_buttons: true,
        ..Default::default()
    });
    click_canvas_button(&mut app, "Calculated number");
    common::wait_until(&mut app, "calculated layout number", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("SCRIPTED DETAILS").is_some()
    });
    // Next is disabled until the new layout's page arrives.
    assert_eq!(common::loaded_record_number(&mut app), "1");
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "second record", |app| {
        common::record_number(app) == "2"
    });
    click_canvas_button(&mut app, "Calculated number");
    common::wait_for_label(&mut app, "SCRIPTED ENTRY");
    assert_eq!(common::loaded_record_number(&mut app), "2");
    click_canvas_button(&mut app, "Calculated name");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    assert_eq!(common::loaded_record_number(&mut app), "2");
    click_canvas_button(&mut app, "Single step assignment");
    common::wait_for_value(&mut app, "Changed by button");
    assert_eq!(
        state.lock().unwrap().rows[1].values[0].as_deref(),
        Some("1")
    );
    common::save_record(&mut app);
    common::wait_until(&mut app, "committed assignment", |_| {
        state.lock().unwrap().rows[1].values[0].as_deref() == Some("Changed by button")
    });
}
fn click_canvas_button(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(egui::Role::Button, label).click();
    app.run_steps(2);
}

fn trigger_dialog(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&common::workspace::calc("\"Field exit\"")));
    step.set_calculation(1, Some(&common::workspace::calc(message)));
    step.set_text(0, Some("OK"));
    step
}

fn expect_exit_dialog(app: &mut common::App, expected: &str) {
    // Not between the exit triggers and the button: the button first saves
    // the typed value, which is no script, and runs once it is saved.
    common::wait_until(app, "field exit result", |app| {
        app.query_by_label("Field exit").is_some()
            || app.state().error().is_some()
            || !(app.state().script_running() || common::saving(app))
    });
    assert!(
        app.query_by_label(expected).is_some(),
        "expected {expected}; error {:?}; labels {:?}",
        app.state().error(),
        app.query_all(By::new().role(egui::Role::Label))
            .map(|n| n.value())
            .collect::<Vec<_>>()
    );
}

fn with_exit_triggers(veto: Option<u16>, change_layout: bool) -> State {
    let mut scripts = scripts();
    let navigate = scripts.scripts[1].steps[0].clone();
    scripts.scripts[1]
        .steps
        .push(trigger_dialog("\"button:\" & $$trace"));
    for (event, id, mark) in [(6, 40, "V"), (4, 41, "S"), (2, 42, "E")] {
        let mut trace = ScriptStep::new(141);
        trace.set_text(0, Some("$$trace"));
        trace.set_calculation(
            1,
            Some(&common::workspace::calc(&format!("$$trace & \"{mark}\""))),
        );
        let mut steps = vec![trace];
        if veto == Some(event) {
            steps.push(trigger_dialog("\"veto:\" & $$trace"));
        }
        if change_layout && event == 2 {
            steps.push(navigate.clone());
        }
        let mut exit = ScriptStep::new(103);
        exit.set_calculation(
            0,
            Some(&common::workspace::calc(if veto == Some(event) {
                "0"
            } else {
                "1"
            })),
        );
        steps.push(exit);
        scripts.scripts.push(Script {
            id,
            name: format!("Field {mark}"),
            steps,
            ..Default::default()
        });
    }
    State {
        field_exit_triggers: true,
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        scripts,
        ..Default::default()
    }
}

#[test]
fn the_first_button_click_resumes_after_field_exit_triggers() {
    let mut state = with_exit_triggers(None, false);
    // The save of the typed value answers late: between the exit triggers
    // and the button no script runs, only that save (#314).
    state.update_delay = Duration::from_millis(200);
    let (mut app, state) = open_state(state);
    common::loaded_record_number(&mut app);
    common::fill(&mut app, "Given", " typed before one click");
    let typed = common::text_field(&app, "Given").value().unwrap();
    click_canvas_button(&mut app, "Open Details");
    expect_exit_dialog(&mut app, "button:VSE");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    assert_eq!(
        state.lock().unwrap().rows[0].values[0].as_deref(),
        Some(typed.as_str())
    );
    assert_eq!(state.lock().unwrap().events, ["update"]);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "button script finished", |a| {
        !a.state().script_running()
    });
    app.run_steps(8);
    assert!(
        app.query_by_label("button:VSE").is_none(),
        "one click must run the button only once"
    );
}

#[test]
fn the_held_button_saves_the_values_changed_by_its_exit_trigger() {
    let mut initial = with_exit_triggers(None, false);
    let exit = initial
        .scripts
        .scripts
        .iter_mut()
        .find(|s| s.id == 42)
        .unwrap();
    let mut set = ScriptStep::new(147);
    set.set_calculation(1, Some(&literal("Given")));
    set.set_calculation(0, Some(&literal("Revised by exit")));
    exit.steps.insert(1, set);
    let (mut app, state) = open_state(initial);
    common::loaded_record_number(&mut app);
    common::fill(&mut app, "Given", " user value");
    click_canvas_button(&mut app, "Open Details");
    expect_exit_dialog(&mut app, "button:VSE");
    assert_eq!(
        state.lock().unwrap().rows[0].values[0].as_deref(),
        Some("Revised by exit")
    );
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "button completed after exit write", |a| {
        !a.state().script_running()
    });
}

#[test]
fn field_exit_vetoes_cancel_the_first_button_click_and_keep_typing() {
    for (event, trace) in [(6, "V"), (4, "VS"), (2, "VSE")] {
        let (mut app, state) = open_state(with_exit_triggers(Some(event), false));
        common::loaded_record_number(&mut app);
        common::fill(&mut app, "Given", " retained after veto");
        let typed = common::text_field(&app, "Given").value().unwrap();
        click_canvas_button(&mut app, "Open Details");
        expect_exit_dialog(&mut app, &format!("veto:{trace}"));
        common::click(&mut app, "OK");
        common::wait_until(&mut app, "veto returned focus", |a| {
            !a.state().script_running() && common::text_field(a, "Given").is_focused()
        });
        app.run_steps(8);
        assert!(app.query_by_label("SCRIPTED ENTRY").is_some());
        assert!(app.query_by_label("Field exit").is_none());
        common::wait_for_value(&mut app, &typed);
        assert!(state.lock().unwrap().events.is_empty());
    }
}

#[test]
fn an_exit_trigger_layout_change_discards_the_original_button_click() {
    let (mut app, _) = open_state(with_exit_triggers(None, true));
    common::loaded_record_number(&mut app);
    common::fill(&mut app, "Given", " layout changes during exit");
    click_canvas_button(&mut app, "Open Details");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    common::wait_until(&mut app, "layout-changing exit trigger ended", |a| {
        !a.state().script_running()
            || a.query_by_label("Field exit").is_some()
            || a.state().error().is_some()
    });
    app.run_steps(8);
    assert!(
        app.query_by_label("Field exit").is_none(),
        "the old layout's button must not run in the new context"
    );
}

#[test]
fn layout_button_runs_create_set_field_and_commit_in_order() {
    let (mut app, state) = open();
    click_canvas_button(&mut app, "Create Contact");
    common::wait_until(&mut app, "scripted record committed", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        let state = state.lock().unwrap();
        state.rows.len() == 2 && state.rows[1].values[0].as_deref() == Some("Created by script")
    });
    assert_eq!(state.lock().unwrap().events, ["create", "update"]);
    // The window follows the script's record once its update and the new
    // page arrive; until then the field is not drawn.
    common::wait_until(&mut app, "scripted value rendered", |app| {
        common::record_number(app) == "2"
            && common::find_text_field(app, "Given")
                .and_then(|f| f.value())
                .as_deref()
                == Some("Created by script")
    });
    click_canvas_button(&mut app, "Open Details");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    // The other layout keeps the record once its page has arrived.
    common::wait_until(&mut app, "the record on the details layout", |app| {
        common::record_number(app) == "2"
    });
}
#[test]
fn scripts_menu_and_workspace_run_actual_catalog_scripts_and_save_drafts() {
    let (mut app, state) = open();
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Create scripted contact");
    common::wait_until(&mut app, "menu script committed", |_| {
        state.lock().unwrap().events.len() == 2
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(&mut app, "Open detail card");
    assert!(app
        .query_all_by_label("Script Workspace (Scripts (scripts.example.invalid))")
        .next()
        .is_some());
    common::click(&mut app, "Open detail card");
    common::click(&mut app, "Run Script");
    common::wait_until(&mut app, "workspace script navigation", |app| {
        app.query_by_role_and_label(egui::Role::ComboBox, "Layout")
            .and_then(|node| node.value())
            .as_deref()
            == Some("Details")
    });
    // Close to observe the real layout, then reopen and save an edited
    // script.
    common::click(&mut app, "Close Window");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Script Workspace...");
    common::wait_for_label(&mut app, "Line 1: New Record/Request");
    common::click(&mut app, "Line 1: New Record/Request");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D);
    app.run_steps(2);
    common::wait_for_label(&mut app, "Line 2: New Record/Request");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    common::wait_until(&mut app, "workspace save acknowledged", |_| {
        state.lock().unwrap().scripts.scripts[0].steps.len() == 4
    });
    common::click(&mut app, "Close Window");
    assert_eq!(
        state.lock().unwrap().events.last().map(String::as_str),
        Some("script-save")
    );
}
/// Unsort Records commits the record first, as the other found-set steps
/// do: a field the script set is saved, not dropped with the record.
#[test]
fn unsort_records_commits_what_the_script_set() {
    let mut set = native_step(147, 1);
    set.set_calculation(1, Some(&literal("Given")));
    set.set_calculation(0, Some(&literal("Kept by unsort")));
    let mut catalog = scripts();
    catalog.scripts.push(Script {
        id: 4,
        name: "Set then unsort".into(),
        steps: vec![set, native_step(21, 2)],
        ..Default::default()
    });
    let (mut app, state) = open_state(State {
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        scripts: catalog,
        ..Default::default()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Set then unsort");
    common::wait_until(&mut app, "the set value committed", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        state.lock().unwrap().rows[0].values[0].as_deref() == Some("Kept by unsort")
    });
}

#[test]
fn an_unsupported_button_script_stops_before_later_record_creation() {
    let (mut app, state) = open();
    click_canvas_button(&mut app, "Unsupported Action");
    common::wait_until(&mut app, "unsupported step error", |app| {
        app.state().error().is_some()
    });
    assert!(app.state().error().unwrap().contains("999"));
    assert_eq!(state.lock().unwrap().rows.len(), 1);
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn direct_new_record_button_and_unsupported_parameters_use_the_same_runtime_safely() {
    let (mut app, state) = open();
    click_canvas_button(&mut app, "Calculated Parameter");
    common::wait_until(&mut app, "unsupported parameter error", |app| {
        app.state().error().is_some()
    });
    assert!(app
        .state()
        .error()
        .unwrap()
        .contains("parameter calculation"));
    assert!(state.lock().unwrap().events.is_empty());
    common::click(&mut app, "OK");
    click_canvas_button(&mut app, "New Blank Contact");
    common::wait_until(&mut app, "direct new record", |app| {
        assert!(app.state().error().is_none());
        state.lock().unwrap().rows.len() == 2
    });
    assert_eq!(state.lock().unwrap().events, ["create"]);
    assert_eq!(state.lock().unwrap().rows[1].values[0], None);
    assert_eq!(common::record_number(&app), "2");
}

#[test]
fn layout_menus_filter_hidden_targets_but_scripts_and_current_name_keep_them() {
    let (mut app, _) = open();
    app.get_by_role_and_label(egui::Role::ComboBox, "Layout")
        .click();
    app.run_steps(3);
    assert!(app.query_by_label("Details").is_none());
    assert!(app.query_by_label("--").is_none());
    assert!(app.query_by_label("-").is_none());
    common::click(&mut app, "Folder");
    common::click(&mut app, "Nested");
    app.get_by_label("Entry");
    app.key_press(egui::Key::Escape);
    app.key_press(egui::Key::Escape);
    app.key_press(egui::Key::Escape);
    app.run_steps(3);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Go to Layout");
    app.get_by_label("Folder");
    assert!(app.query_by_label("Details").is_none());
    assert!(app.query_by_label("--").is_none());
    app.key_press(egui::Key::Escape);
    app.key_press(egui::Key::Escape);
    app.run_steps(3);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Open detail card");
    common::wait_for_label(&mut app, "SCRIPTED DETAILS");
    assert_eq!(
        app.get_by_role_and_label(egui::Role::ComboBox, "Layout")
            .value()
            .as_deref(),
        Some("Details")
    );
    app.get_by_role_and_label(egui::Role::ComboBox, "Layout")
        .click();
    app.run_steps(3);
    assert!(app.query_by_label("Details").is_none());
    app.get_by_label("Folder");
    app.key_press(egui::Key::Escape);
    app.run_steps(3);
    common::click(&mut app, "Edit Layout");
    app.get_by_role_and_label(egui::Role::ComboBox, "Layout")
        .click();
    app.run_steps(3);
    app.get_by_label("Details");
    app.get_by_label("Entry");
}

#[test]
fn hidden_layout_script_name_and_number_use_the_complete_layout_list() {
    for (value, calculation) in [(3, literal("Details")), (4, literal("2"))] {
        let mut step = native_step(6, 1);
        step.value = value;
        step.set_calculation(5, Some(&calculation));
        let mut catalog = scripts();
        catalog.scripts[1].steps = vec![step];
        let (mut app, _) = open_state(State {
            rows: vec![Record {
                id: 1,
                values: vec![Some("Original".into())],
            }],
            scripts: catalog,
            ..Default::default()
        });
        common::open_menu(&mut app, "Scripts");
        common::click(&mut app, "Open detail card");
        common::wait_for_label(&mut app, "SCRIPTED DETAILS");
        assert_eq!(
            app.get_by_role_and_label(egui::Role::ComboBox, "Layout")
                .value()
                .as_deref(),
            Some("Details")
        );
    }
}

#[test]
fn open_file_continues_the_initiating_script() {
    let mut catalog = scripts();
    let mut open = native_step(33, 1);
    open.set_calculation(0, Some(&literal("Peer")));
    catalog.scripts.push(Script {
        id: 40,
        name: "Open peer and continue".into(),
        steps: vec![open, native_step(7, 2), native_step(75, 3)],
        ..Default::default()
    });
    let (mut app, state) = open_state(State {
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        scripts: catalog,
        ..Default::default()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Open peer and continue");
    common::wait_until(&mut app, "continuation after Open File", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        state.lock().unwrap().rows.len() == 2
    });
}

#[test]
fn open_file_retains_both_desktop_files_and_closing_the_caller_keeps_the_peer() {
    let mut catalog = scripts();
    let mut open = native_step(33, 1);
    open.set_calculation(0, Some(&literal("Peer")));
    catalog.scripts.push(Script {
        id: 40,
        name: "Open peer window".into(),
        steps: vec![open],
        ..Default::default()
    });
    let (mut app, state) = open_state(State {
        scripts: catalog,
        rows: vec![Record {
            id: 1,
            values: vec![Some("Caller original".into())],
        }],
        ..Default::default()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Open peer window");
    common::wait_until(&mut app, "both file windows", |app| {
        !app.state().script_running()
    });
    assert_eq!(
        app.state_mut().window_titles(),
        [
            "Peer (scripts.example.invalid)",
            "Scripts (scripts.example.invalid)"
        ]
    );
    // Visible Open File presents Peer; select the caller before closing it.
    app.query_all_by_role_and_label(egui::Role::Button, "Window")
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Scripts (scripts.example.invalid)");
    common::wait_until(&mut app, "caller selected for close", |a| {
        a.state().window_name() == "Scripts (scripts.example.invalid)"
    });
    common::wait_for_value(&mut app, "Caller original");
    app.query_all_by_role_and_label(egui::Role::Button, "File")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "surviving peer window", |app| {
        app.state().window_name() == "Peer (scripts.example.invalid)"
    });
    common::wait_for_value(&mut app, "Peer original");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(
        app.state_mut().window_titles(),
        ["Peer (scripts.example.invalid)"]
    );
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Create scripted contact");
    let peer = state.lock().unwrap().peer.clone().unwrap();
    // The script makes the record before it sets the field: wait for the
    // value, not only for the row.
    common::wait_until(&mut app, "peer write after closing caller", |_| {
        let peer = peer.lock().unwrap();
        peer.rows.len() == 2
            && peer.rows[1].values.first().and_then(|v| v.as_deref()) == Some("Created by script")
    });
    assert_eq!(state.lock().unwrap().rows.len(), 1);
    assert_eq!(
        peer.lock().unwrap().rows[1].values[0].as_deref(),
        Some("Created by script")
    );
}

#[test]
fn hidden_open_file_has_no_visible_peer_until_an_explicit_visible_open() {
    let mut catalog = scripts();
    for (id, hidden, name) in [(40, true, "Open peer hidden"), (41, false, "Show peer")] {
        let mut open = native_step(33, 1);
        open.set_calculation(0, Some(&literal("Peer")));
        if hidden {
            open.flags |= om_data::flag::OPTION;
        }
        catalog.scripts.push(Script {
            id,
            name: name.into(),
            steps: vec![open],
            ..Default::default()
        });
    }
    let (mut app, state) = open_state(State {
        scripts: catalog,
        rows: vec![Record {
            id: 1,
            values: vec![Some("Caller original".into())],
        }],
        ..Default::default()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Open peer hidden");
    common::wait_until(&mut app, "hidden open completed", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state_mut().window_titles().len(), 1);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Show peer");
    common::wait_until(&mut app, "visible reopen completed", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state_mut().window_titles().len(), 2);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| *e == "open:Peer")
            .count(),
        1
    );
}

#[test]
fn closing_an_idle_peer_keeps_the_callers_paused_script_and_continuation() {
    let mut catalog = scripts();
    let mut open = native_step(33, 1);
    open.set_calculation(0, Some(&literal("Peer")));
    catalog.scripts.push(Script {
        id: 40,
        name: "Pause after opening peer".into(),
        steps: vec![
            open,
            native_step(62, 2),
            native_step(7, 3),
            native_step(75, 4),
        ],
        ..Default::default()
    });
    let (mut app, state) = open_state(State {
        scripts: catalog,
        rows: vec![Record {
            id: 1,
            values: vec![Some("Caller original".into())],
        }],
        ..Default::default()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Pause after opening peer");
    common::wait_until(&mut app, "caller paused", |app| app.state().script_paused());
    app.query_all_by_role_and_label(egui::Role::Button, "Window")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.get_by_role_and_label(egui::Role::Button, "Peer (scripts.example.invalid)")
        .click();
    app.run_steps(2);
    app.query_all_by_role_and_label(egui::Role::Button, "File")
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "caller surviving peer close", |app| {
        app.state().window_name() == "Scripts (scripts.example.invalid)"
    });
    assert!(
        app.state().script_paused(),
        "closing an idle peer discarded the caller's pause"
    );
    common::click(&mut app, "Continue");
    common::wait_until(&mut app, "caller continuation", |app| {
        let rows = state.lock().unwrap().rows.len();
        assert!(
            app.state().script_running() || rows == 2,
            "the caller stopped before its continuation"
        );
        rows == 2
    });
    assert_eq!(
        state
            .lock()
            .unwrap()
            .peer
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .rows
            .len(),
        1
    );
}

fn exit_mode_change(opcode: u16, mode: om_gui::Mode) {
    let mut initial = with_exit_triggers(None, false);
    initial
        .scripts
        .scripts
        .iter_mut()
        .find(|s| s.id == 42)
        .unwrap()
        .steps
        .insert(1, ScriptStep::new(opcode));
    let (mut app, _) = open_state(initial);
    common::loaded_record_number(&mut app);
    common::fill(&mut app, "Given", " before mode change");
    click_canvas_button(&mut app, "Open Details");
    common::wait_until(&mut app, "mode-changing exit finished", |a| {
        !a.state().script_running() || a.query_by_label("Field exit").is_some()
    });
    app.run_steps(8);
    assert_eq!(app.state().mode(), mode);
    assert!(
        app.query_by_label("Field exit").is_none(),
        "the Browse click must not survive a mode change"
    );
}

#[test]
fn entering_find_in_a_field_exit_trigger_discards_the_browse_click() {
    exit_mode_change(22, om_gui::Mode::Find);
}

#[test]
fn entering_preview_in_a_field_exit_trigger_discards_the_browse_click() {
    exit_mode_change(41, om_gui::Mode::Preview);
}

fn sorted_exit_click(sort: bool, write: bool) -> (common::App, Arc<Mutex<State>>) {
    use om_data::specs::{SortEntry, SortSpec, SpecField};
    let mut initial = with_exit_triggers(None, false);
    initial.rows = vec![
        Record {
            id: 1,
            values: vec![Some("A".into())],
        },
        Record {
            id: 22,
            values: vec![Some("C".into())],
        },
        Record {
            id: 33,
            values: vec![Some("B".into())],
        },
    ];
    initial.scripts.scripts[1].steps = vec![trigger_dialog(
        "\"button:\" & $$trace & \":\" & Get ( RecordID )",
    )];
    if write {
        let mut set = ScriptStep::new(147);
        set.set_calculation(1, Some(&literal("Given")));
        set.set_calculation(0, Some(&literal("Button wrote")));
        initial.scripts.scripts[1]
            .steps
            .insert(0, ScriptStep::new(75));
        initial.scripts.scripts[1].steps.insert(0, set);
    }
    if sort {
        let spec = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00002),
                    field: 1,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        };
        let mut step = ScriptStep::new(39);
        step.flags |=
            om_data::flag::RESTORE | om_data::flag::EXTENDED_DATA | om_data::flag::NO_INTERACT;
        step.set_data_leaf(
            0x0a,
            Some(om_data::runtime::OwnedNode::leaf([0x0a], spec.encode())),
        );
        initial
            .scripts
            .scripts
            .iter_mut()
            .find(|s| s.id == 42)
            .unwrap()
            .steps
            .insert(1, step);
    }
    let (mut app, state) = open_state(initial);
    common::loaded_record_number(&mut app);
    common::click(&mut app, "List View");
    common::wait_until(&mut app, "second row button", |a| {
        a.query_all_by_role_and_label(egui::Role::Button, "Open Details")
            .count()
            >= 2
    });
    app.query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.step();
    app.query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .next()
        .unwrap()
        .type_text(" first-row edit");
    app.run_steps(2);
    app.query_all_by_role_and_label(egui::Role::Button, "Open Details")
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "row click result", |a| {
        !a.state().script_running()
            || a.query_by_label("Field exit").is_some()
            || a.state().error().is_some()
    });
    app.run_steps(8);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    (app, state)
}

#[test]
fn a_sorting_exit_trigger_cancels_the_obsolete_list_row_button() {
    let (app, _) = sorted_exit_click(true, false);
    assert!(
        app.query_by_label("Field exit").is_none(),
        "a changed record order invalidates the old row click"
    );
}

#[test]
fn a_sorting_exit_trigger_cannot_redirect_button_writes_to_another_record() {
    let (app, state) = sorted_exit_click(true, true);
    let state = state.lock().unwrap();
    assert_eq!(
        state.rows.iter().find(|r| r.id == 33).unwrap().values[0].as_deref(),
        Some("B")
    );
    assert_eq!(
        state.rows.iter().find(|r| r.id == 22).unwrap().values[0].as_deref(),
        Some("C")
    );
    assert!(app.query_by_label("Field exit").is_none());
}

#[test]
fn a_stable_list_row_button_still_writes_the_clicked_record() {
    let (mut app, state) = sorted_exit_click(false, true);
    expect_exit_dialog(&mut app, "button:VSE:22");
    {
        let state = state.lock().unwrap();
        assert_eq!(
            state.rows.iter().find(|r| r.id == 22).unwrap().values[0].as_deref(),
            Some("Button wrote")
        );
        assert_eq!(
            state.rows.iter().find(|r| r.id == 33).unwrap().values[0].as_deref(),
            Some("B")
        );
    }
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "stable button completed", |a| {
        !a.state().script_running()
    });
}

/// The Given field's OnObjectKeystroke and the Entry layout's
/// OnLayoutKeystroke each add "o" or "l", `Code ( Get ( TriggerKeystroke )
/// )`, "/" and `Get ( TriggerModifierKeys )` to `$$keys`. The field's
/// returns False for keys with modifiers, the layout's for the up arrow
/// (29). The Show keys button shows `$$keys`.
fn with_keystroke_triggers() -> State {
    let mut scripts = scripts();
    let calc = common::workspace::calc;
    let trace = |mark: &str| {
        let mut step = ScriptStep::new(141);
        step.set_text(0, Some("$$keys"));
        step.set_calculation(
            1,
            Some(&calc(&format!(
                "$$keys & \"{mark}\" & Code ( Get ( TriggerKeystroke ) ) & \"/\" & \
                 Get ( TriggerModifierKeys ) & \";\""
            ))),
        );
        step
    };
    let exit = |result: &str| {
        let mut step = ScriptStep::new(103);
        step.set_calculation(0, Some(&calc(result)));
        step
    };
    let mut show = ScriptStep::new(87);
    show.set_calculation(0, Some(&calc("\"Keys\"")));
    show.set_calculation(1, Some(&calc("$$keys")));
    show.set_text(0, Some("OK"));
    for (id, name, steps) in [
        (
            50,
            "Field keys",
            vec![trace("o"), exit("Get ( TriggerModifierKeys ) = 0")],
        ),
        (
            51,
            "Layout keys",
            vec![trace("l"), exit("Code ( Get ( TriggerKeystroke ) ) <> 29")],
        ),
        (52, "Show keys", vec![show]),
    ] {
        scripts.scripts.push(Script {
            id,
            name: name.into(),
            steps,
            ..Default::default()
        });
    }
    State {
        keystroke_triggers: true,
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        scripts,
        ..Default::default()
    }
}

/// Wait for the keystroke triggers of the keys pressed to end, and for the
/// keys they let through to reach the editor.
fn after_keys(app: &mut common::App) {
    common::wait_until(app, "keystroke triggers ended", |a| {
        !a.state().script_running()
    });
    app.run_steps(8);
}

fn expect_keys(app: &mut common::App, expected: &str) {
    click_canvas_button(app, "Show keys");
    common::wait_until(app, "Show keys dialog", |a| {
        a.query_by_label("Keys").is_some()
    });
    assert!(
        app.query_by_label(expected).is_some(),
        "expected {expected}; labels {:?}",
        app.query_all(By::new().role(egui::Role::Label))
            .map(|n| n.value())
            .collect::<Vec<_>>()
    );
}

#[test]
fn keystroke_triggers_see_control_keys_and_modifiers_and_veto_them() {
    let (mut app, state) = open_state(with_keystroke_triggers());
    common::loaded_record_number(&mut app);
    common::text_field(&app, "Given").focus();
    app.run_steps(3);
    // End has no code in the Claris reference: no trigger sees it.
    app.key_press(egui::Key::End);
    app.run_steps(2);
    // Both triggers let Backspace through: the editor deletes.
    app.key_press(egui::Key::Backspace);
    common::wait_for_value(&mut app, "Origina");
    // Shift-Backspace: the field's trigger returns False; the layout's
    // does not run.
    app.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Backspace);
    after_keys(&mut app);
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Origina")
    );
    // The left arrow moves the cursor; text typed while it waits goes in
    // after it, at the cursor, in order.
    app.key_press(egui::Key::ArrowLeft);
    common::text_field(&app, "Given").type_text("x");
    common::text_field(&app, "Given").type_text("y");
    common::wait_for_value(&mut app, "Originxya");
    // The layout's trigger stops the up arrow, which would have moved the
    // cursor to the start.
    app.key_press(egui::Key::ArrowUp);
    after_keys(&mut app);
    assert!(common::text_field(&app, "Given").is_focused());
    common::text_field(&app, "Given").type_text("z");
    common::wait_for_value(&mut app, "Originxyza");
    expect_keys(
        &mut app,
        "o8/0;l8/0;o8/1;o28/0;l28/0;o120/0;l120/0;o121/0;l121/0;o29/0;l29/0;o122/0;l122/0;",
    );
    // Leaving the field for the button saved what the keys typed.
    common::wait_until(&mut app, "the typed value saved", |_| {
        state.lock().unwrap().rows[0].values[0].as_deref() == Some("Originxyza")
    });
}

#[test]
fn the_layout_keystroke_trigger_runs_when_no_field_is_active() {
    let (mut app, _) = open_state(with_keystroke_triggers());
    common::loaded_record_number(&mut app);
    assert!(!common::text_field(&app, "Given").is_focused());
    app.key_press(egui::Key::Enter);
    after_keys(&mut app);
    // Text typed with no active field: only the layout's trigger sees it,
    // and no field takes it.
    app.event(egui::Event::Text("q".into()));
    after_keys(&mut app);
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Original")
    );
    expect_keys(&mut app, "l13/0;l113/0;");
}

/// The pause bar's Continue, through accessibility rather than the
/// pointer: a pointer click would take the keyboard focus from the field
/// the test put it in.
fn continue_pause(app: &mut common::App) {
    app.get_by_label("Continue").click_accesskit();
    app.run_steps(2);
}

// Typeahead keeps its original editor while a prior trigger is paused.
#[test]
fn queued_keystrokes_cannot_move_to_an_unrelated_editor() {
    let mut state = with_keystroke_triggers();
    state.second_keystroke_field = true;
    let field_script = state
        .scripts
        .scripts
        .iter_mut()
        .find(|s| s.id == 50)
        .unwrap();
    // Pause/Resume Script [Indefinitely], resumed by the test once it has
    // checked the paused state: a timed pause could end before a loaded
    // machine's frames reach the checks (#850).
    field_script.steps.insert(0, ScriptStep::new(62));
    let (mut app, state) = open_state(state);
    common::loaded_record_number(&mut app);
    app.query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.run_steps(3);
    app.event(egui::Event::Text("x".into()));
    common::wait_until(&mut app, "field keystroke trigger paused", |a| {
        a.state().script_paused()
    });
    // This shifted text was typed into the FIRST object. Its trigger returns
    // False when modifiers != 0. It must not become an edit on another object.
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    app.event(egui::Event::Text("Y".into()));
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    app.run_steps(1);
    app.query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .focus();
    app.run_steps(3);
    assert!(
        app.state().script_paused(),
        "focus must move before the trigger resumes"
    );
    assert!(app
        .query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .is_focused());
    continue_pause(&mut app);
    // A trigger the queued keystroke starts runs the same script, and so
    // pauses too: it is continued as well. Text that is rightly dropped
    // starts none.
    common::wait_until(&mut app, "the trigger resumed", |a| {
        a.state().script_paused() || !a.state().script_running()
    });
    if app.state().script_paused() {
        continue_pause(&mut app);
    }
    common::wait_until(&mut app, "keystroke script finished", |a| {
        !a.state().script_running()
    });
    // Resuming did not move the focus: the queued text met the editor the
    // user moved to.
    assert!(app
        .query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .is_focused());
    app.run_steps(30);
    let values: Vec<_> = app
        .query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .map(|n| n.value())
        .collect();
    eprintln!("editor values after focus change: {values:?}");
    eprintln!("stored row: {:?}", state.lock().unwrap().rows[0]);
    click_canvas_button(&mut app, "Show keys");
    common::wait_until(&mut app, "trace dialog after saving", |a| {
        a.query_by_label("Keys").is_some()
    });
    let labels: Vec<_> = app
        .query_all(By::new().role(egui::Role::Label))
        .map(|n| n.value())
        .collect();
    eprintln!("trace dialog labels: {labels:?}");
    eprintln!(
        "persisted row after leaving editor: {:?}",
        state.lock().unwrap().rows[0]
    );
    assert_eq!(
        state.lock().unwrap().rows[0].values[0].as_deref(),
        Some("Original")
    );
    assert_eq!(
        values,
        vec![Some("Original".into()), Some("Original".into())],
        "text queued in object 20 must not be approved/applied in object 21"
    );
}

/// Typeahead can follow a focus change caused by its own approved Tab;
/// moving the keyboard independently does not get that permission.
#[test]
fn an_approved_tab_transfers_its_queued_text_to_the_next_editor() {
    let mut initial = with_keystroke_triggers();
    initial.second_keystroke_field = true;
    let field_script = initial
        .scripts
        .scripts
        .iter_mut()
        .find(|s| s.id == 50)
        .unwrap();
    let mut pause = ScriptStep::new(62);
    pause.value = 1;
    pause.set_calculation(0, Some(&common::workspace::calc("0.5")));
    field_script.steps.insert(0, pause);
    let (mut app, state) = open_state(initial);
    common::loaded_record_number(&mut app);
    app.query_all_by_role_and_label(egui::Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.run_steps(3);
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "Tab trigger paused", |a| {
        a.state().script_paused()
    });
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    app.event(egui::Event::Text("Y".into()));
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    app.run_steps(1);
    common::wait_until(&mut app, "Tab and its typeahead applied", |a| {
        a.query_all_by_role_and_label(egui::Role::TextInput, "Given")
            .nth(1)
            .is_some_and(|n| n.is_focused() && n.value().as_deref() == Some("OriginalY"))
            && !a.state().script_running()
    });
    click_canvas_button(&mut app, "Show keys");
    common::wait_until(&mut app, "the edited value committed", |_| {
        state.lock().unwrap().rows[0].values[0].as_deref() == Some("OriginalY")
    });
}
