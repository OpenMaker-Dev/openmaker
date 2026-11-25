//! Button bars in Browse, on a synthetic file: segments run their actions
//! through the layout button runtime (parameters, the user's typing saved
//! first, field exit vetoes), and the active segment shows as the bar
//! stores it: a chosen segment, or the segment its calculation names.
mod common;

use egui::accesskit::Toggled;
use egui::Role;
use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::host::{CalcSchema, FieldDef};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, HostedFile, LayoutInfo,
    Record, RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep,
};
use om_layout::{
    ButtonObject, FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect,
    SegmentedBarObject,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const TABLE: u64 = 0x8002;
const OCCURRENCE: u64 = 0xd00002;
/// `occurrence_id(LAYOUT_KEY) == OCCURRENCE`.
const LAYOUT_KEY: u64 = 0x104082;
const ENTRY: u32 = 7;
const DETAILS: u32 = 8;
/// The note an undetermined bar gives.
const UNDETERMINED: &str = "OpenMaker cannot tell which segment of this button bar is active: \
                            it stores an active segment calculation it is not set to use.";

#[derive(Default)]
enum EntrySegment {
    #[default]
    Scripted,
    Actionless,
    Popover,
}

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    events: Vec<String>,
    /// The Given field vetoes its exit (OnObjectExit returns False).
    veto: bool,
    entry_segment: EntrySegment,
}
struct Provider(Arc<Mutex<State>>);

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Bars", om_data::calc::Locale::en_us());
    schema.add_table(TABLE, "Records");
    schema.add_occurrence(OCCURRENCE, "Records", TABLE);
    schema.add_field(
        TABLE,
        FieldDef::data(1, "Given", om_data::calc::ValueType::Text),
    );
    schema.add_layout(ENTRY, "Entry", Some(OCCURRENCE));
    schema.add_layout(DETAILS, "Details", Some(OCCURRENCE));
    schema
}

fn formula(source: &str) -> om_data::formula::Formula {
    let schema = schema();
    let mut context = om_data::formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    om_data::formula::Formula {
        context: None,
        formatted: None,
        polish: om_data::formula::compile(source, &context).unwrap().expr,
    }
}

fn given() -> Vec<Field> {
    vec![Field {
        name: "Given".into(),
        kind: FieldKind::Text,
    }]
}

impl DataProvider for Provider {
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        use om_data::host::{MemorySource, RecordValues, Recorder};
        let state = self.0.lock().unwrap();
        let schema = schema();
        let cx = om_data::calc::Ctx::new(
            &schema.locale,
            om_data::calc::Date::from_ymd(2026, 1, 2).unwrap(),
        );
        let mut source = MemorySource::new();
        for row in &state.rows {
            let mut values = RecordValues::default();
            let given = row.values[0].as_deref().unwrap_or("");
            values.set(1, 1, om_data::calc::Value::text(given));
            source.put_record(TABLE, row.id as u64, values);
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
    fn close(&mut self) {}
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok([(ENTRY, "Entry"), (DETAILS, "Details")]
            .map(|(id, name)| LayoutInfo {
                id,
                name: name.into(),
            })
            .into())
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(scripts())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let state = self.0.lock().unwrap();
        Ok(RecordPage {
            versions: Default::default(),
            fields: given(),
            records: state
                .rows
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect(),
            total: state.rows.len() as u64,
        })
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
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
        state.events.push(format!(
            "update:{}",
            row.values[0].as_deref().unwrap_or_default()
        ));
        Ok(row)
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        let state = self.0.lock().unwrap();
        let mut layout = render(id, state.veto);
        let objects = &mut layout.layout.objects.objects;
        match state.entry_segment {
            EntrySegment::Scripted => {}
            EntrySegment::Actionless => {
                objects
                    .iter_mut()
                    .find(|object| object.id == 31)
                    .unwrap()
                    .button
                    .as_mut()
                    .unwrap()
                    .script_step = None;
            }
            EntrySegment::Popover => {
                let segment = objects.iter_mut().find(|object| object.id == 31).unwrap();
                segment.object_type = 14;
                segment.button = None;
                segment.popover_button = Some(om_layout::PopoverButtonObject {
                    popover_key: 90,
                    ..Default::default()
                });
                segment.children.push(90);
                objects.push(LayoutObject {
                    id: 90,
                    object_type: 15,
                    parent: Some(31),
                    bounds: rect(0.0, 30.0, 240.0, 120.0),
                    popover: Some(om_layout::PopoverObject {
                        button_key: 31,
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
        }
        Ok(Some(layout))
    }
}

fn dialog(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&common::workspace::calc("\"Field exit\"")));
    step.set_calculation(1, Some(&common::workspace::calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// Script 1 sets Given to its parameter; script 42, the field's
/// OnObjectExit, says so and vetoes.
fn scripts() -> ScriptCatalog {
    let mut set = ScriptStep::new(147);
    set.set_calculation(1, Some(&common::workspace::calc("\"Given\"")));
    set.set_calculation(0, Some(&common::workspace::calc("Get ( ScriptParameter )")));
    let mut veto = ScriptStep::new(103);
    veto.set_calculation(0, Some(&common::workspace::calc("0")));
    ScriptCatalog {
        scripts: vec![
            Script {
                id: 1,
                name: "Stamp".into(),
                steps: vec![set],
                ..Default::default()
            },
            Script {
                id: 42,
                name: "Keep editing".into(),
                steps: vec![dialog("\"vetoed\""), veto],
                ..Default::default()
            },
        ],
        writable: false,
        unavailable_reason: None,
    }
}

fn rect(left: f64, top: f64, width: f64, height: f64) -> Option<Rect> {
    Some(Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
        ..Default::default()
    })
}

/// Segment `id` of bar `bar` at `column`: named `name`, labelled `label`.
fn segment(id: u64, bar: u64, column: f64, name: &str, label: &str) -> LayoutObject {
    LayoutObject {
        id,
        object_type: 10,
        flags: 0x10000, // Stored Show hand cursor option.
        parent: Some(bar),
        name: Some(name.into()),
        text: Some(label.into()),
        bounds: rect(column * 120.0, 0.0, 120.0, 24.0),
        ..Default::default()
    }
}

/// A Go to Layout segment as the Tasks template's navigation bar stores
/// it: step 6, value 5, the layout's key in `reserved`.
fn go_to_layout(object: LayoutObject, layout: u32) -> LayoutObject {
    let mut reserved = vec![2, 1, layout as u8, 0x6f];
    reserved.resize(14, 0);
    LayoutObject {
        button: Some(ButtonObject {
            script_step: Some(om_layout::ScriptStep {
                id: 6,
                value: 5,
                flags: 8,
                reserved,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object
    }
}

/// Both layouts: a title, the Given field and navigation bar 30, whose
/// segments are named after the layouts. Entry chooses its own segment;
/// Details calculates it from `Get ( LayoutName )`. The Stamp segment
/// performs script 1 with a calculated parameter and hides on Details.
/// Details also has bar 40, which stores a calculation it is not set to
/// use.
fn render(id: u32, veto: bool) -> RenderLayout {
    let title = if id == ENTRY {
        "BAR ENTRY"
    } else {
        "BAR DETAILS"
    };
    let mut bar = LayoutObject {
        id: 30,
        object_type: 18,
        name: Some("Navigation".into()),
        children: vec![31, 32, 33],
        bounds: rect(20.0, 50.0, 360.0, 24.0),
        segmented_bar: Some(SegmentedBarObject {
            active_segment_key: 31,
            ..Default::default()
        }),
        ..Default::default()
    };
    if id == DETAILS {
        bar.segmented_bar = Some(SegmentedBarObject {
            flags: 2,
            ..Default::default()
        });
        bar.calcs.active_segment = Some(formula("Get ( LayoutName )"));
    }
    let mut stamp = segment(33, 30, 2.0, "Stamp", "STAMP");
    let mut reference = vec![2, 1, 1, 0xbe];
    reference.resize(14, 0);
    stamp.button = Some(ButtonObject {
        script_step: Some(om_layout::ScriptStep {
            id: 1,
            reserved: reference,
            ..Default::default()
        }),
        ..Default::default()
    });
    stamp.flags = 0; // Its stored Show hand cursor option is off.
    stamp.calcs.script_parameter = Some(formula("Get ( LayoutName ) & \" stamp\""));
    stamp.calcs.hide = Some(formula("Get ( LayoutName ) = \"Details\""));
    let mut field = LayoutObject {
        id: 20,
        object_type: 1,
        bounds: rect(20.0, 100.0, 260.0, 24.0),
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: LAYOUT_KEY,
                field_key: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    if veto {
        field.triggers.push(om_layout::ScriptTrigger {
            event: 2,
            script: Some(42),
            modes: 1,
            file_reference: None,
            external: false,
            option: 0,
            extra: vec![],
            parameter: None,
        });
    }
    let mut objects = vec![
        LayoutObject {
            id: 1,
            object_type: 2,
            text: Some(title.into()),
            bounds: rect(20.0, 10.0, 260.0, 24.0),
            ..Default::default()
        },
        field,
        bar,
        go_to_layout(segment(31, 30, 0.0, "Entry", "ENTRY TAB"), ENTRY),
        go_to_layout(segment(32, 30, 1.0, "Details", "DETAILS TAB"), DETAILS),
        stamp,
    ];
    let mut root = vec![1, 20, 30];
    if id == DETAILS {
        let mut other = LayoutObject {
            id: 40,
            object_type: 18,
            children: vec![41],
            bounds: rect(20.0, 150.0, 120.0, 24.0),
            ..Default::default()
        };
        other.calcs.active_segment = Some(formula("\"Other\""));
        objects.push(other);
        objects.push(segment(41, 40, 0.0, "Other", "OTHER"));
        root.push(40);
    }
    RenderLayout {
        layout: Layout {
            id: u64::from(id),
            name: if id == ENTRY { "Entry" } else { "Details" }.into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                table_alias_key: LAYOUT_KEY,
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
                root,
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([(20, "Given".into())]),
    }
}

fn open(veto: bool) -> (common::App, Arc<Mutex<State>>) {
    open_with_segment(veto, EntrySegment::Scripted)
}

fn open_with_segment(veto: bool, entry_segment: EntrySegment) -> (common::App, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        rows: vec![Record {
            id: 1,
            values: vec![Some("Original".into())],
        }],
        veto,
        entry_segment,
        ..Default::default()
    }));
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(&mut app, "Path", "fmnet:/bars.example.invalid/Bars.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "BAR ENTRY");
    common::loaded_record_number(&mut app);
    (app, state)
}

/// Whether segment `label` shows as the active one; `None` when it is not
/// on screen.
fn active(app: &common::App, label: &str) -> Option<bool> {
    let node = app.query_by_role_and_label(Role::Button, label)?;
    Some(node.accesskit_node().toggled() == Some(Toggled::True))
}

fn press(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(2);
}

fn hover_cursor(app: &mut common::App, at: egui::Pos2) -> egui::CursorIcon {
    app.event(egui::Event::PointerMoved(at));
    app.run_steps(3);
    app.output().platform_output.cursor_icon
}

#[test]
fn available_layout_buttons_request_the_hand_and_reset_on_leave() {
    let (mut app, _) = open(false);
    let at = app
        .get_by_role_and_label(Role::Button, "DETAILS TAB")
        .rect()
        .center();
    let cursor = hover_cursor(&mut app, at);
    if let Some(path) = std::env::var_os("OM_BUTTON_CURSOR_PNG") {
        app.set_render_cursor(true);
        app.render()
            .expect("render neutral Browse fixture")
            .save(path)
            .expect("save neutral cursor capture");
    }
    assert_eq!(cursor, egui::CursorIcon::PointingHand);
    let unflagged = app
        .get_by_role_and_label(Role::Button, "STAMP")
        .rect()
        .center();
    assert_eq!(hover_cursor(&mut app, unflagged), egui::CursorIcon::Default);
    let text = common::text_field(&app, "Given").rect().center();
    assert_ne!(hover_cursor(&mut app, text), egui::CursorIcon::PointingHand);
    assert_eq!(
        hover_cursor(&mut app, egui::pos2(900.0, 650.0)),
        egui::CursorIcon::Default
    );
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Zoom In");
    let zoomed = app
        .get_by_role_and_label(Role::Button, "DETAILS TAB")
        .rect()
        .center();
    assert_eq!(
        hover_cursor(&mut app, zoomed),
        egui::CursorIcon::PointingHand
    );
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    let finding = app
        .get_by_role_and_label(Role::Button, "DETAILS TAB")
        .rect()
        .center();
    assert_ne!(
        hover_cursor(&mut app, finding),
        egui::CursorIcon::PointingHand
    );
}

#[test]
fn segments_run_their_actions_and_show_the_active_segment_of_each_layout() {
    let (mut app, state) = open(false);
    // Entry's bar chose its own segment.
    common::wait_until(&mut app, "the chosen segment", |app| {
        active(app, "STAMP").is_some()
    });
    assert_eq!(active(&app, "ENTRY TAB"), Some(true));
    assert_eq!(active(&app, "DETAILS TAB"), Some(false));
    assert_eq!(active(&app, "STAMP"), Some(false));
    assert!(app.query_by_label(UNDETERMINED).is_none());

    // A segment performs its script with its parameter, calculated in the
    // window's layout and record.
    press(&mut app, "STAMP");
    common::wait_for_value(&mut app, "Entry stamp");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the stamp saved", |_| {
        state.lock().unwrap().events == ["update:Entry stamp"]
    });

    // The user's typing is saved before a segment goes to another layout.
    common::fill(&mut app, "Given", " typed");
    let typed = common::text_field(&app, "Given").value().unwrap();
    assert!(
        typed.contains("typed") && typed.contains("Entry stamp"),
        "{typed}"
    );
    press(&mut app, "DETAILS TAB");
    common::wait_for_label(&mut app, "BAR DETAILS");
    assert_eq!(
        state.lock().unwrap().rows[0].values[0].as_deref(),
        Some(typed.as_str())
    );
    assert_eq!(
        state.lock().unwrap().events,
        ["update:Entry stamp".to_owned(), format!("update:{typed}")]
    );

    // Details' bar names its segment by calculation: the object name, not
    // the label. The Stamp segment's hide condition holds there. The other
    // bar's choice cannot be told: no segment of it shows active and it
    // says why.
    common::wait_until(&mut app, "the calculated segment", |app| {
        active(app, "DETAILS TAB") == Some(true)
    });
    assert_eq!(active(&app, "ENTRY TAB"), Some(false));
    assert_eq!(active(&app, "STAMP"), None);
    assert!(app.query_by_label(UNDETERMINED).is_some());

    press(&mut app, "ENTRY TAB");
    common::wait_for_label(&mut app, "BAR ENTRY");
    common::wait_until(&mut app, "the chosen segment again", |app| {
        active(app, "ENTRY TAB") == Some(true) && active(app, "DETAILS TAB") == Some(false)
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn a_field_exit_veto_keeps_the_typing_and_the_layout() {
    let (mut app, state) = open(true);
    common::fill(&mut app, "Given", " kept");
    let typed = common::text_field(&app, "Given").value().unwrap();
    press(&mut app, "DETAILS TAB");
    common::wait_until(&mut app, "the exit trigger", |app| {
        app.query_by_label("vetoed").is_some() || app.state().error().is_some()
    });
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "focus back in the field", |app| {
        !app.state().script_running() && common::text_field(app, "Given").is_focused()
    });
    app.run_steps(8);
    assert!(app.query_by_label("BAR ENTRY").is_some());
    assert!(app.query_by_label("vetoed").is_none());
    common::wait_for_value(&mut app, &typed);
    assert_eq!(active(&app, "ENTRY TAB"), Some(true));
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn an_actionless_segment_exposes_selection_without_a_click_action() {
    let (mut app, state) = open_with_segment(false, EntrySegment::Actionless);
    common::wait_until(&mut app, "the actionless selected segment", |app| {
        active(app, "ENTRY TAB") == Some(true)
    });
    let at = app
        .get_by_role_and_label(Role::Button, "ENTRY TAB")
        .rect()
        .center();
    assert_eq!(hover_cursor(&mut app, at), egui::CursorIcon::Default);
    let segment = app.get_by_role_and_label(Role::Button, "ENTRY TAB");
    assert!(!segment
        .accesskit_node()
        .data()
        .supports_action(egui::accesskit::Action::Click));
    assert!(!segment
        .accesskit_node()
        .data()
        .supports_action(egui::accesskit::Action::Focus));
    assert_eq!(active(&app, "DETAILS TAB"), Some(false));
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn a_popover_segment_keeps_selection_separate_from_expansion() {
    let (mut app, _) = open_with_segment(false, EntrySegment::Popover);
    let at = app
        .get_by_role_and_label(Role::Button, "ENTRY TAB")
        .rect()
        .center();
    assert_eq!(hover_cursor(&mut app, at), egui::CursorIcon::PointingHand);
    common::wait_until(&mut app, "the closed selected popover segment", |app| {
        active(app, "ENTRY TAB") == Some(true)
    });
    let expanded = |app: &common::App| {
        app.get_by_role_and_label(Role::Button, "ENTRY TAB")
            .accesskit_node()
            .data()
            .is_expanded()
    };
    assert_eq!(expanded(&app), Some(false));
    press(&mut app, "ENTRY TAB");
    common::wait_until(&mut app, "the expanded popover", |app| {
        expanded(app) == Some(true)
    });
    assert_eq!(active(&app, "ENTRY TAB"), Some(true));
    press(&mut app, "ENTRY TAB");
    common::wait_until(&mut app, "the collapsed popover", |app| {
        expanded(app) == Some(false)
    });
    assert_eq!(active(&app, "ENTRY TAB"), Some(true));
}
