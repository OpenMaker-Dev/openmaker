//! `Get` selectors in the desktop window, as FileMaker Pro 26.0.3 answers
//! them for a local file (calc-functions.md 6.3): scripts run on the
//! provider worker read the window on the desktop and its toolbar, focus
//! and selection, Find mode's requests, the record's open state and
//! changed fields, Preview, layout objects in screen points, and fields in
//! Find mode and without a record. Each probe script sets the Note field
//! of the record it ends on to its answers joined with `|`.
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use om_data::calc::{Ctx, Date, Locale, ValueType};
use om_data::formula::{compile, formatted, polish, Context, Formula, Key, NameTable};
use om_data::found::{FoundSet, FoundSetChange, MemoryFoundSet};
use om_data::host::{
    stored_value, CalcData, CalcNeed, CalcSchema, FieldDef, LayoutEntry, MemorySource,
    RecordValues, Recorder,
};
use om_data::specs::{SortEntry, SortSpec, SpecField};
use om_data::{
    flag, Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind,
    FieldSpec, HostedFile, LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog,
    ScriptStep, StoredKey,
};
use om_layout::{FieldObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};

/// The layout's table occurrence: alias 5 is occurrence 5.
const OCCURRENCE: u64 = 5;
/// The fields records pages show; `Created` is auto-entered.
const FIELDS: [&str; 4] = ["Name", "Age", "Note", "Created"];
/// A stored calculation on Name, and a global field.
const INITIAL: u64 = 5;
const GLOBAL: u64 = 6;
const ENTRY: u32 = 1;
/// The UUID `[0xaa; 16]` as `GetLayoutObjectAttribute` writes it.
const UUID_A: &str = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA";

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    found: MemoryFoundSet,
    scripts: ScriptCatalog,
    global: Option<String>,
    account_info: om_data::host::AccountInfo,
    /// A layout in place of `render()`'s.
    layout: Option<RenderLayout>,
}

struct Provider(Arc<Mutex<State>>);

fn fields() -> Vec<Field> {
    FIELDS
        .iter()
        .map(|name| Field {
            name: (*name).into(),
            kind: if *name == "Age" {
                FieldKind::Number
            } else {
                FieldKind::Text
            },
        })
        .collect()
}

impl DataProvider for Provider {
    fn account_info(&self) -> om_data::host::AccountInfo {
        self.0.lock().unwrap().account_info.clone()
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
        Ok(vec![LayoutInfo {
            id: ENTRY,
            name: "Entry".into(),
        }])
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.0.lock().unwrap().scripts.clone())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let state = self.0.lock().unwrap();
        Ok(state.found.page(fields(), &state.rows, offset, limit))
    }
    fn change_found_set(&mut self, _: u32, change: FoundSetChange) -> Result<FoundSet, DataError> {
        let mut state = self.0.lock().unwrap();
        let State { rows, found, .. } = &mut *state;
        found.apply(&fields(), rows, change)
    }
    fn set_global(&mut self, _: u64, _: u64, _: u32, text: &str) -> Result<(), DataError> {
        self.0.lock().unwrap().global = Some(text.to_owned());
        Ok(())
    }
    /// A new record auto-enters `Created`.
    fn create_record(&mut self, _: u32, values: FieldChanges) -> Result<Record, DataError> {
        let mut state = self.0.lock().unwrap();
        let id = state.rows.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let row = Record {
            id,
            values: FIELDS
                .iter()
                .map(|name| match *name {
                    "Created" => Some("auto".into()),
                    _ => values.get(*name).cloned().flatten(),
                })
                .collect(),
        };
        state.rows.push(row.clone());
        state.found.add(id);
        Ok(row)
    }
    fn delete_record(&mut self, _: u32, id: i64) -> Result<(), DataError> {
        self.0.lock().unwrap().rows.retain(|r| r.id != id);
        Ok(())
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
        for (name, value) in values {
            let i = FIELDS.iter().position(|n| *n == name).unwrap();
            row.values[i] = value;
        }
        Ok(row.clone())
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        let mut schema = CalcSchema::new("Contacts", Locale::en_us());
        schema.authenticated_account = self.account_info();
        schema.add_table(OCCURRENCE, "Contacts");
        schema.add_occurrence(OCCURRENCE, "Contacts", OCCURRENCE);
        for (i, name) in FIELDS.iter().enumerate() {
            let ty = if *name == "Age" {
                ValueType::Number
            } else {
                ValueType::Text
            };
            let mut def = FieldDef::data(i as u64 + 1, *name, ty);
            if *name == "Created" {
                // An auto-entered creation value.
                def.options = 0x1;
            }
            schema.add_field(OCCURRENCE, def);
        }
        schema.add_field(
            OCCURRENCE,
            FieldDef::calculation(
                INITIAL,
                "Initial",
                ValueType::Text,
                formula("Left ( Name ; 1 )"),
                true,
            ),
        );
        schema.add_field(
            OCCURRENCE,
            FieldDef::data(GLOBAL, "gText", ValueType::Text).with_global(true),
        );
        schema.add_layout_entry(LayoutEntry {
            layout: Some(Arc::new(render().layout)),
            ..LayoutEntry::new(ENTRY, "Entry", Some(OCCURRENCE))
        });
        Ok(Some(Arc::new(schema)))
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        let schema = self.calc_schema()?.expect("a calculation schema");
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let mut source = MemorySource::new();
        let state = self.0.lock().unwrap();
        for row in &state.rows {
            let mut values = RecordValues::default();
            for (i, value) in row.values.iter().enumerate() {
                if let Some(text) = value {
                    let ty = if i == 1 {
                        ValueType::Number
                    } else {
                        ValueType::Text
                    };
                    values.set(i as u64 + 1, 1, stored_value(text, ty, &cx));
                }
            }
            if let Some(name) = &row.values[0] {
                let initial = name.chars().take(1).collect::<String>();
                values.set(INITIAL, 1, stored_value(&initial, ValueType::Text, &cx));
            }
            source.put_record(OCCURRENCE, row.id as u64, values);
        }
        let mut globals = RecordValues::default();
        if let Some(text) = &state.global {
            globals.set(GLOBAL, 1, stored_value(text, ValueType::Text, &cx));
        }
        source.set_globals(OCCURRENCE, globals);
        let found: Vec<u64> = state
            .found
            .page(fields(), &state.rows, 0, 1000)
            .records
            .iter()
            .map(|r| r.id as u64)
            .collect();
        source.set_found_set(OCCURRENCE, &found, None);
        drop(state);
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(format!("{need:?}: {e}")))?;
        }
        Ok(recorder.take())
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(
            self.0.lock().unwrap().layout.clone().unwrap_or_else(render),
        ))
    }
    /// A file hosted by FileMaker Server 26.0.3.
    fn host_application_version(&self) -> Option<String> {
        Some("Server 26.0.3".into())
    }
    fn close(&mut self) {}
}

/// The Entry layout: a 300-point body, 800 wide, the fields top to bottom
/// from 60 points, anchored left and top; Name's object is named. The tab
/// order starts at Age, then Name and Note; Created is not in it. Note is a
/// drop-down calendar with its arrow, the others edit boxes.
fn render() -> RenderLayout {
    let label = |id: u64, text: &str, left: f64| LayoutObject {
        id,
        object_type: 2,
        text: Some(text.into()),
        bounds: Some(Rect {
            top: 5.0,
            left,
            bottom: 35.0,
            right: left + 200.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    // Two labels hidden when `LayoutObjectUUID` is UUID_A: only the one
    // whose own UUID it is.
    let hidden_when_a = |mut object: LayoutObject, uuid: [u8; 16]| {
        object.uuid = Some(uuid);
        object.calcs.hide = Some(formula(&format!("LayoutObjectUUID = \"{UUID_A}\"")));
        object
    };
    let mut objects = vec![
        label(1, "ENTRY LAYOUT", 20.0),
        hidden_when_a(label(2, "OWN UUID", 300.0), [0xaa; 16]),
        hidden_when_a(label(3, "OTHER UUID", 520.0), [0xbb; 16]),
        // Hidden on a FileMaker Server 26.0.3 host.
        {
            let mut object = label(4, "HOST VERSION", 740.0);
            object.calcs.hide = Some(formula(
                "Get ( HostApplicationVersion ) = \"Server 26.0.3\"",
            ));
            object
        },
    ];
    let mut expiry_label = label(5, "EXPIRY WARNING", 20.0);
    expiry_label.name = Some("Expiry warning".into());
    expiry_label.calcs.hide = Some(formula("Get ( AccountPasswordDaysRemaining ) = 0"));
    objects.push(expiry_label);
    let mut bindings = BTreeMap::new();
    for (i, name) in FIELDS.iter().enumerate() {
        let object = 10 + i as u64;
        let top = 60.0 + 40.0 * i as f64;
        objects.push(LayoutObject {
            id: object,
            object_type: 1,
            flags: 0x3000_0000,
            name: (i == 0).then(|| "Name field".to_owned()),
            tab_order: [2, 1, 3, 0][i],
            field_obj: Some(FieldObject {
                field_spec: Some(om_layout::FieldSpec {
                    table_key: OCCURRENCE,
                    field_key: i as u64 + 1,
                    repetition: 1,
                    ..Default::default()
                }),
                num_of_reps: 1,
                display_type: if *name == "Note" { 6 } else { 0 },
                flags: if *name == "Note" { 0x400 } else { 0 },
                ..Default::default()
            }),
            bounds: Some(Rect {
                top,
                left: 20.0,
                bottom: top + 30.0,
                right: 280.0,
                ..Default::default()
            }),
            ..Default::default()
        });
        bindings.insert(object, (*name).to_owned());
    }
    RenderLayout {
        layout: Layout {
            id: u64::from(ENTRY),
            name: "Entry".into(),
            def: Some(LayoutDef {
                base_width: 800.0,
                table_alias_key: OCCURRENCE,
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
                root: objects.iter().map(|o| o.id).collect(),
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: bindings,
    }
}

// ----- building scripts ---------------------------------------------------------

fn names() -> NameTable {
    let mut names = NameTable::default();
    names.add_occurrence(OCCURRENCE, "Contacts", OCCURRENCE);
    for (i, name) in FIELDS.iter().enumerate() {
        names.add_field(OCCURRENCE, i as u64 + 1, name);
    }
    names.add_field(OCCURRENCE, INITIAL, "Initial");
    names.add_field(OCCURRENCE, GLOBAL, "gText");
    names
}

fn formula(source: &str) -> Formula {
    let names = names();
    let mut context = Context::new(&names);
    context.occurrence = Some(OCCURRENCE);
    Formula {
        context: Some(Key::from_id(OCCURRENCE)),
        formatted: None,
        polish: compile(source, &context).expect("formula compiles").expr,
    }
}

fn calc(source: &str) -> Calculation {
    let names = names();
    let compiled = compile(source, &Context::new(&names)).expect("formula compiles");
    Calculation::new(
        Some(formatted::encode(&compiled.tokens)),
        polish::encode(&compiled.expr),
    )
}

fn field(name: &str) -> FieldSpec {
    let id = FIELDS.iter().position(|n| *n == name).unwrap() as u8 + 1;
    FieldSpec::new(
        StoredKey::new(&[1, OCCURRENCE as u8]).unwrap(),
        StoredKey::new(&[1, id]).unwrap(),
        1,
    )
}

fn global() -> FieldSpec {
    FieldSpec::new(
        StoredKey::new(&[1, OCCURRENCE as u8]).unwrap(),
        StoredKey::new(&[1, GLOBAL as u8]).unwrap(),
        1,
    )
}

fn step(id: u16) -> ScriptStep {
    ScriptStep::new(id)
}

fn with_value(mut s: ScriptStep, value: u16) -> ScriptStep {
    s.value = value;
    s.flags |= flag::MENU_SELECTION;
    s
}

fn set_field(target: FieldSpec, value: &str) -> ScriptStep {
    let mut s = step(76);
    s.set_field_spec(0, Some(target));
    s.set_calculation(0, Some(&calc(value)));
    s
}

fn set_variable(name: &str, value: &str) -> ScriptStep {
    let mut s = step(141);
    s.set_text(0, Some(name));
    s.set_calculation(1, Some(&calc(value)));
    s
}

/// Collect `expressions` into `$$r`, joined with `|`.
fn probe(expressions: &[&str]) -> ScriptStep {
    let joined: Vec<String> = expressions.iter().map(|e| format!("( {e} )")).collect();
    set_variable(
        "$$r",
        &format!("$$r & \"#\" & {}", joined.join(" & \"|\" & ")),
    )
}

/// Write `$$r` into the first record's Note: to the first record, clear
/// the found set and leave every mode first.
fn report() -> Vec<ScriptStep> {
    vec![
        step(55),
        step(23),
        with_value(step(16), 1),
        set_field(field("Note"), "$$r"),
        step(75),
    ]
}

fn go_to_field(name: &str, select: bool) -> ScriptStep {
    let mut s = step(17);
    s.set_field_spec(0, Some(field(name)));
    if select {
        s.flags |= flag::SELECT_ALL;
    }
    s
}

fn insert_text(name: &str, text: &str) -> ScriptStep {
    let mut s = step(61);
    s.set_field_spec(0, Some(field(name)));
    s.set_text(1, Some(text));
    s
}

fn set_selection(name: &str, start: i64, end: i64) -> ScriptStep {
    let mut s = step(130);
    s.set_field_spec(0, Some(field(name)));
    s.set_calculation(0, Some(&calc(&start.to_string())));
    s.set_calculation(1, Some(&calc(&end.to_string())));
    s
}

fn move_resize(height: f64, width: f64, top: f64, left: f64) -> ScriptStep {
    let mut s = step(119);
    for (slot, value) in [(1, height), (2, width), (3, top), (4, left)] {
        s.set_calculation(slot, Some(&calc(&value.to_string())));
    }
    s
}

fn no_dialog(mut s: ScriptStep) -> ScriptStep {
    s.flags |= flag::NO_INTERACT;
    s
}

/// Sort Records by Name, ascending, without the dialog.
fn sort_by_name() -> ScriptStep {
    let spec = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(OCCURRENCE),
                field: 1,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        }],
    };
    let mut s = step(39);
    s.flags |= flag::RESTORE | flag::EXTENDED_DATA | flag::NO_INTERACT;
    s.set_data_leaf(
        0x0a,
        Some(om_data::runtime::OwnedNode::leaf([0x0a], spec.encode())),
    );
    s
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    }
}

fn rows() -> Vec<Record> {
    [("Ada", "36"), ("Grace", "85"), ("Alan", "41")]
        .iter()
        .enumerate()
        .map(|(i, (name, age))| Record {
            id: i as i64 + 1,
            values: vec![Some((*name).into()), Some((*age).into()), None, None],
        })
        .collect()
}

/// Open the file with `scripts`, the window placed like FileMaker Pro's
/// probe window: its client area 28 points below the frame's top at 166,
/// 150 on a 1920 x 1080 screen (the desktop area is the screen here: no
/// X11 display gives a work area).
fn open(scripts: Vec<Script>) -> (common::App, Arc<Mutex<State>>) {
    open_with_account(scripts, Default::default())
}

/// [`open`] with no scripts, drawing `layout`.
fn open_layout(layout: RenderLayout) -> common::App {
    open_with(Vec::new(), Default::default(), Some(layout)).0
}

fn open_with_account(
    scripts: Vec<Script>,
    account_info: om_data::host::AccountInfo,
) -> (common::App, Arc<Mutex<State>>) {
    open_with(scripts, account_info, None)
}

fn open_with(
    scripts: Vec<Script>,
    account_info: om_data::host::AccountInfo,
    layout: Option<RenderLayout>,
) -> (common::App, Arc<Mutex<State>>) {
    std::env::remove_var("DISPLAY");
    let state = Arc::new(Mutex::new(State {
        rows: rows(),
        account_info,
        scripts: ScriptCatalog {
            scripts,
            ..ScriptCatalog::default()
        },
        layout,
        ..State::default()
    }));
    let mut app = common::harness(Box::new(Provider(state.clone())));
    {
        let viewport = app
            .input_mut()
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap();
        viewport.monitor_size = Some(egui::vec2(1920.0, 1080.0));
        viewport.outer_rect = Some(egui::Rect::from_min_size(
            egui::pos2(166.0, 150.0),
            egui::vec2(1060.0, 788.0),
        ));
        viewport.inner_rect = Some(egui::Rect::from_min_size(
            egui::pos2(166.0, 178.0),
            egui::vec2(1060.0, 760.0),
        ));
        viewport.focused = Some(true);
    }
    common::fill(
        &mut app,
        "Path",
        "fmnet:/scripts.example.invalid/Contacts.fmp12",
    );
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "ENTRY LAYOUT");
    common::wait_until(&mut app, "first record", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Ada")
    });
    (app, state)
}

/// Run script `name` and return its answers, one list per probe step.
fn answers(app: &mut common::App, state: &Arc<Mutex<State>>, name: &str) -> Vec<Vec<String>> {
    common::open_menu(app, "Scripts");
    common::click(app, name);
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let note = state.lock().unwrap().rows[0].values[2]
        .clone()
        .unwrap_or_default();
    let answers: Vec<Vec<String>> = note
        .split('#')
        .skip(1)
        .map(|p| p.split('|').map(str::to_owned).collect())
        .collect();
    assert!(!answers.is_empty(), "{name} wrote no answers: {note:?}");
    answers
}

fn with_report(mut steps: Vec<ScriptStep>) -> Vec<ScriptStep> {
    steps.extend(report());
    steps
}

#[test]
fn the_window_answers_from_the_desktop_area_and_its_toolbar() {
    let geometry = [
        "Get ( WindowLeft )",
        "Get ( WindowTop )",
        "Get ( WindowWidth )",
        "Get ( WindowHeight )",
        "Get ( WindowContentWidth )",
        "Get ( WindowContentHeight )",
        "Get ( WindowDesktopWidth )",
        "Get ( WindowDesktopHeight )",
        "Get ( ScreenWidth )",
        "Get ( ScreenHeight )",
        "Get ( StatusAreaState )",
        "Get ( WindowZoomLevel )",
        // The field object: from the content area's top left, which is the
        // window's left and the content's top.
        "GetLayoutObjectAttribute ( \"Name field\" ; \"left\" ) - Get ( WindowLeft )",
        "GetLayoutObjectAttribute ( \"Name field\" ; \"top\" ) - ( Get ( WindowTop ) + Get ( WindowHeight ) - Get ( WindowContentHeight ) )",
        "GetLayoutObjectAttribute ( \"Name field\" ; \"width\" )",
    ];
    let mut hide = with_value(step(29), 2);
    hide.flags |= flag::MENU_SELECTION;
    let mut show = with_value(step(29), 1);
    show.flags |= flag::MENU_SELECTION;
    let mut zoom = with_value(step(97), 5);
    zoom.flags |= flag::MENU_SELECTION;
    let scripts = vec![script(
        200,
        "Window probe",
        with_report(vec![
            set_variable("$$r", "\"\""),
            probe(&geometry),
            // Move/Resize Window measures from the desktop area.
            move_resize(900.0, 1100.0, 120.0, 100.0),
            probe(&geometry),
            hide,
            probe(&geometry),
            zoom,
            probe(&geometry),
            move_resize(900.0, 1100.0, 120.0, 100.0),
            probe(&geometry),
            show,
            probe(&geometry),
            probe(&[
                "Get ( ScreenDepth )",
                "Get ( ScreenScaleFactor )",
                "Get ( WindowVisible )",
                "Get ( WindowStyle )",
                "Get ( WindowOrientation )",
                "Get ( MenubarState )",
                "Get ( TextRulerVisible )",
                "Length ( Get ( WindowUUID ) )",
                "Get ( WindowName )",
            ]),
        ]),
    )];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Window probe");
    let first = &all[0];
    // The frame where the window system has it; the content area below the
    // status toolbar and layout bar.
    assert_eq!(first[..4], ["166", "150", "1060", "788"]);
    assert_eq!(first[4], "1060");
    let content: i64 = first[5].parse().unwrap();
    assert!(content < 760 && content > 500, "{first:?}");
    assert_eq!(first[6..12], ["1920", "1080", "1920", "1080", "1", "100"]);
    assert_eq!(first[12..], ["20", "60", "260"]);
    // Moved and resized: the frame FileMaker Pro's script asked for, the
    // content area keeping its chrome.
    let moved = &all[1];
    assert_eq!(moved[..4], ["100", "120", "1100", "900"]);
    assert_eq!(moved[4], "1100");
    assert_eq!(moved[5], (content + 112).to_string());
    assert_eq!(moved[12..], ["20", "60", "260"]);
    // The toolbar hidden: the content area gains the bars' height.
    let hidden = &all[2];
    assert_eq!(hidden[..3], ["100", "120", "1100"]);
    let grown: i64 = hidden[5].parse().unwrap();
    let moved_content = content + 112;
    let frame_delta = if cfg!(target_os = "macos") {
        assert_eq!(hidden[3], "845");
        assert_eq!(grown, moved_content + 25);
        55
    } else {
        assert_eq!(hidden[3], "900");
        assert!(grown > moved_content, "{hidden:?}");
        0
    };
    assert_eq!(hidden[10], "0");
    assert_eq!(hidden[12..], ["20", "60", "260"]);
    // Zoom 150 %: the layout scaled from the content's top left.
    let zoomed = &all[3];
    assert_eq!(zoomed[11], "150");
    assert_eq!(zoomed[12..], ["30", "90", "390"]);
    let fixed = &all[4];
    assert_eq!(fixed[..4], ["100", "120", "1100", "900"]);
    assert_eq!(fixed[5], (grown + frame_delta).to_string());
    let shown = &all[5];
    assert_eq!(shown[..3], ["100", "120", "1100"]);
    assert_eq!(shown[3], (900 + frame_delta).to_string());
    assert_eq!(shown[5], (moved_content + frame_delta).to_string());
    assert_eq!(shown[10], "1");
    assert_eq!(
        all[6],
        ["32", "1", "1", "0", "-1", "1", "0", "32", "Contacts"]
    );
}

#[test]
fn toolbar_lock_controls_the_menu_shortcut_and_status_area_state() {
    use egui_kittest::kittest::{NodeT as _, Queryable};

    let script_with_toolbar = |id, name, visibility, lock| {
        let mut bar = with_value(step(29), visibility);
        if lock {
            bar.flags |= flag::LOCK;
        }
        script(
            id,
            name,
            with_report(vec![
                set_variable("$$r", "\"\""),
                bar,
                probe(&["Get ( LastError )", "Get ( StatusAreaState )"]),
            ]),
        )
    };
    let (mut app, state) = open(vec![
        script_with_toolbar(250, "Lock shown", 1, true),
        script_with_toolbar(251, "Lock hidden", 2, true),
        script_with_toolbar(252, "Unlock shown", 1, false),
        script_with_toolbar(253, "Hide unlocked", 2, false),
    ]);
    let shortcut = |app: &mut common::App| {
        app.key_press_modifiers(
            egui::Modifiers::COMMAND.plus(egui::Modifiers::ALT),
            egui::Key::S,
        );
        app.run_steps(2);
    };
    let shots = std::env::var_os("OM_TOOLBAR_LOCK_SHOTS");
    for (script, value, shown, locked) in [
        ("Hide unlocked", "0", false, false),
        ("Lock hidden", "3", false, true),
        ("Unlock shown", "1", true, false),
        ("Lock shown", "2", true, true),
    ] {
        assert_eq!(answers(&mut app, &state, script), [["0", value]]);
        common::open_menu(&mut app, "View");
        assert_eq!(
            app.get_by_label("Status Toolbar")
                .accesskit_node()
                .is_disabled(),
            locked
        );
        if let Some(dir) = &shots {
            app.set_render_cursor(false);
            app.render()
                .unwrap()
                .save(std::path::Path::new(dir).join(format!("state-{value}.png")))
                .unwrap();
        }
        app.key_press(egui::Key::Escape);
        app.run_steps(2);
        if locked {
            shortcut(&mut app);
        }
        assert_eq!(app.query_by_label("Edit Layout").is_some(), shown);
        assert_eq!(app.query_by_label("Total (Unsorted)").is_some(), shown);
    }
    assert_eq!(answers(&mut app, &state, "Unlock shown"), [["0", "1"]]);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Status Toolbar");
    assert!(app.query_by_label("Edit Layout").is_none());
    shortcut(&mut app);
    app.get_by_label("Edit Layout");
}

/// Adjust Window [Resize to Fit] gives the content area the layout's width
/// and the bottom of its lowest part at the window's zoom, and keeps the
/// window's top left (Claris, Adjust Window; script-steps.md: inferred).
#[test]
fn resize_to_fit_gives_the_content_area_the_layout_at_its_zoom() {
    let geometry = [
        "Get ( WindowLeft )",
        "Get ( WindowTop )",
        "Get ( WindowContentWidth )",
        "Get ( WindowContentHeight )",
    ];
    let fit = with_value(step(31), 1);
    let scripts = vec![script(
        202,
        "Fit probe",
        with_report(vec![
            set_variable("$$r", "\"\""),
            fit.clone(),
            probe(&geometry),
            with_value(step(97), 5),
            fit,
            probe(&geometry),
        ]),
    )];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Fit probe");
    // The layout: 800 points wide, its one part 300 points down.
    assert_eq!(all[0], ["166", "150", "800", "300"]);
    assert_eq!(all[1], ["166", "150", "1200", "450"]);
}

#[test]
fn focus_selection_and_the_records_open_state() {
    let focus = [
        "Get ( ActiveFieldName )",
        "Get ( ActiveFieldTableName )",
        "Get ( ActiveFieldContents )",
        "Get ( ActiveRepetitionNumber )",
        "Get ( ActiveSelectionStart )",
        "Get ( ActiveSelectionSize )",
        "Get ( ActiveLayoutObjectName )",
        "Get ( ActivePortalRowNumber )",
        "Get ( RecordOpenState )",
        "Get ( RecordOpenCount )",
        "Get ( ModifiedFields )",
        "GetLayoutObjectAttribute ( \"Name field\" ; \"hasFocus\" )",
        "GetLayoutObjectAttribute ( \"Name field\" ; \"content\" )",
        "Get ( ActiveModifierKeys )",
    ];
    let scripts = vec![script(
        201,
        "Focus probe",
        with_report(vec![
            set_variable("$$r", "\"\""),
            probe(&focus),
            // Ann typed into Name, nn selected (O2).
            go_to_field("Name", true),
            insert_text("Name", "Ann"),
            set_selection("Name", 2, 3),
            probe(&focus),
            // Go to Field [ ]: the field is left, which commits (O2).
            step(17),
            probe(&[
                "Get ( RecordOpenState )",
                "Get ( ActiveFieldName )",
                "Get ( ModifiedFields )",
                "Contacts::Name",
            ]),
            // Set Field: the field and the stored calculation on it (O3).
            set_field(field("Name"), "\"Ada\""),
            set_field(field("Age"), "37"),
            probe(&[
                "Get ( RecordOpenState )",
                "Get ( RecordOpenCount )",
                "Get ( ModifiedFields )",
                "Get ( ActiveFieldName )",
            ]),
            step(75),
            // A new record: its auto-entered field (O3).
            step(7),
            probe(&[
                "Get ( RecordOpenState )",
                "Get ( RecordOpenCount )",
                "Get ( ModifiedFields )",
                "Get ( RecordModificationCount )",
                "Get ( FoundCount )",
                "Get ( ActiveFieldName )",
            ]),
            no_dialog(step(51)),
            probe(&[
                "Get ( RecordOpenState )",
                "Get ( ModifiedFields )",
                "Get ( FoundCount )",
                "Length ( Get ( TriggerCurrentPanel ) )",
                "Code ( Get ( TriggerTargetPanel ) )",
                "Get ( TriggerKeystroke )",
            ]),
        ]),
    )];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Focus probe");
    assert_eq!(
        all[0],
        ["", "", "", "0", "", "", "", "0", "0", "0", "", "0", "Ada", "0"]
    );
    assert_eq!(
        all[1],
        [
            "Name",
            "Contacts",
            "Ann",
            "1",
            "2",
            "2",
            "Name field",
            "0",
            "2",
            "1",
            "",
            "1",
            "Ann",
            "0"
        ]
    );
    assert_eq!(all[2], ["0", "", "", "Ann"]);
    assert_eq!(
        state.lock().unwrap().rows[0].values[0].as_deref(),
        Some("Ada")
    );
    assert_eq!(all[3], ["2", "1", "Name\rAge\rInitial", ""]);
    // New Record focuses the first field of the tab order.
    assert_eq!(all[4], ["1", "1", "Created", "0", "4", "Age"]);
    // This provider has no record sessions, so New Record saved the record
    // at once, and Revert keeps a stored record rather than delete it
    // (#892): the record is closed, still found. A held record is removed
    // (client_conflicts).
    assert_eq!(all[5], ["0", "", "4", "2", "1300048", ""]);
}

/// The layout's calculations read the window's focus, the text typed so
/// far, the record's changed fields and the sort as a script in the window
/// does: hide conditions on them follow the user's focus and typing, the
/// field being left and Sort Records.
#[test]
fn layout_calculations_read_the_focus_changes_and_sort() {
    use egui_kittest::kittest::Queryable;
    let mut layout = render();
    for (id, text, hide) in [
        (
            30,
            "OUT OF NAME",
            "Get ( ActiveFieldName ) = \"Name\" and Get ( ActiveFieldTableName ) = \"Contacts\"",
        ),
        (
            31,
            "NOTHING TYPED",
            "PatternCount ( Get ( ActiveFieldContents ) ; \"x\" ) > 0",
        ),
        (
            32,
            "NAME UNCHANGED",
            "PatternCount ( Get ( ModifiedFields ) ; \"Name\" ) > 0",
        ),
        (33, "UNSORTED", "Get ( SortState ) = 1"),
    ] {
        let top = 230.0 + 15.0 * (id - 30) as f64;
        let mut marker = LayoutObject {
            id,
            object_type: 2,
            text: Some(text.into()),
            bounds: Some(Rect {
                top,
                left: 320.0,
                bottom: top + 14.0,
                right: 620.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        marker.calcs.hide = Some(formula(hide));
        layout.layout.objects.root.push(id);
        layout.layout.objects.objects.push(marker);
    }
    let markers = ["OUT OF NAME", "NOTHING TYPED", "NAME UNCHANGED", "UNSORTED"];
    let shown = |app: &common::App| markers.map(|label| app.query_by_label(label).is_some());
    let (mut app, _state) = open_with(
        vec![script(208, "Sort by name", vec![sort_by_name()])],
        Default::default(),
        Some(layout),
    );
    common::wait_until(&mut app, "every marker", |app| shown(app) == [true; 4]);
    // Typing into Name: Name is active with what was typed, and listed as
    // modified only once it is left.
    common::fill(&mut app, "Name", "x");
    common::wait_until(&mut app, "the focus and typing", |app| {
        shown(app) == [false, false, true, true]
    });
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "Name left", |app| {
        shown(app) == [true, true, false, true]
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Sort by name");
    common::wait_until(&mut app, "the sort", |app| {
        !app.state().script_running() && !shown(app)[3]
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The text field that has the focus: its label and its top.
fn focused(app: &common::App) -> Option<(String, f32)> {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    app.query_all_by_role(egui::Role::TextInput)
        .find(|field| field.is_focused())
        .and_then(|field| Some((field.accesskit_node().label()?, field.rect().min.y)))
}

/// New Record from the toolbar, until a field has the focus.
fn new_record(app: &mut common::App) -> (String, f32) {
    common::click(app, "New Record");
    common::wait_until(app, "the new record's focused field", |app| {
        app.state().is_settled() && focused(app).is_some()
    });
    focused(app).unwrap()
}

/// New Record from the toolbar focuses the first field of the stored tab
/// order, as a script's New Record does (s33: Title on the Contacts
/// starter's Contact Details), not the first field drawn: Age, not Name.
/// A first field hidden by its condition is passed over (Name, the next);
/// of a field placed twice, the object in the tab order takes the focus,
/// not a copy drawn first outside it.
#[test]
fn new_record_from_the_toolbar_focuses_the_first_field_of_the_tab_order() {
    let (mut app, _state) = open(Vec::new());
    assert_eq!(new_record(&mut app).0, "Age");

    let mut hidden = render();
    let age = hidden
        .layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == 11)
        .unwrap();
    age.calcs.hide = Some(formula("1"));
    let mut app = open_layout(hidden);
    assert_eq!(new_record(&mut app).0, "Name");

    // A second Age object above Name, outside the tab order.
    let mut twice = render();
    let mut copy = twice
        .layout
        .objects
        .objects
        .iter()
        .find(|o| o.id == 11)
        .unwrap()
        .clone();
    copy.id = 20;
    copy.tab_order = 0;
    if let Some(bounds) = copy.bounds.as_mut() {
        bounds.top = 20.0;
        bounds.bottom = 50.0;
    }
    twice.layout.objects.root.insert(0, 20);
    twice.layout.objects.objects.insert(0, copy);
    twice.field_bindings.insert(20, "Age".into());
    let mut app = open_layout(twice);
    let (name, top) = new_record(&mut app);
    assert_eq!(name, "Age");
    use egui_kittest::kittest::Queryable as _;
    let tops: Vec<f32> = app
        .query_all_by_role_and_label(egui::Role::TextInput, "Age")
        .map(|field| field.rect().min.y)
        .collect();
    assert_eq!(tops.len(), 2, "both Age objects are drawn");
    assert_eq!(
        top,
        tops.iter().copied().fold(f32::MIN, f32::max),
        "the lower one, in the tab order"
    );
}

/// Tab and Shift-Tab move the focus through the layout's stored tab order
/// (#848), not egui's widget order: from Age (place 1), where New Record
/// puts it, to Name (2), on to Note (3), and round to Age past Created,
/// which is outside the order. Shift-Tab goes back. The keys typed in the
/// frame of a Tab, after it, reach the field entered and no other; an edit
/// box's insertion point is after its text, nothing selected (FileMaker
/// Pro 26.0.3, #848), while the calendar's text is selected, so typing
/// replaces it (#964). With no widget focused, Tab goes to the first field.
#[test]
fn tab_moves_the_focus_through_the_stored_tab_order() {
    let (mut app, _state) = open(Vec::new());
    assert_eq!(new_record(&mut app).0, "Age");
    let tab = |app: &mut common::App, modifiers: egui::Modifiers, typed: &str| {
        let key = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        app.event_modifiers(key, modifiers);
        if !typed.is_empty() {
            app.event(egui::Event::Text(typed.to_owned()));
        }
        app.run_steps(4);
        focused(app).map(|(label, _)| label)
    };
    let value =
        |app: &common::App, field: &str| common::text_field(app, field).value().unwrap_or_default();
    let (next, back) = (egui::Modifiers::NONE, egui::Modifiers::SHIFT);
    assert_eq!(tab(&mut app, next, "Zed").as_deref(), Some("Name"));
    assert_eq!(
        (value(&app, "Age"), value(&app, "Name")),
        ("".into(), "Zed".into())
    );
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Note"));
    assert_eq!(tab(&mut app, back, "Q").as_deref(), Some("Name"));
    assert_eq!(
        (value(&app, "Name"), value(&app, "Note")),
        ("ZedQ".into(), "".into()),
        "typed after Name's text, which stays in its draft"
    );
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Note"));
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Age"));
    // A click on blank canvas leaves no widget focused.
    let blank = egui::pos2(500.0, 300.0);
    app.event(egui::Event::PointerMoved(blank));
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos: blank,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    common::wait_until(&mut app, "the click's commit", |app| {
        app.state().is_settled() && app.ctx.memory(|memory| memory.focused()).is_none()
    });
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Age"));
    // Two Tabs and a key in one frame: the key reaches the field the second
    // Tab enters.
    for event in [
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: next,
        },
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: next,
        },
        egui::Event::Text("X".into()),
    ] {
        app.event(event);
    }
    app.run_steps(6);
    assert_eq!(
        focused(&app).map(|(label, _)| label).as_deref(),
        Some("Note")
    );
    assert_eq!(
        (value(&app, "Name"), value(&app, "Note")),
        ("ZedQ".into(), "X".into())
    );
    // Back round to the calendar, which holds text now: typing replaces it.
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Age"));
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Name"));
    assert_eq!(tab(&mut app, next, "Y").as_deref(), Some("Note"));
    assert_eq!(
        (value(&app, "Name"), value(&app, "Note")),
        ("ZedQ".into(), "Y".into()),
        "the calendar's text was selected"
    );
}

/// In Find mode Tab goes through the request's criteria in the stored tab
/// order, as in Browse, and wraps (FileMaker Pro 26.0.3 on the Tasks
/// starter, #848; #858): Age, Name, Note, past Created. The keys typed
/// after a Tab reach the criterion entered, after its text.
#[test]
fn tab_moves_through_the_criteria_in_find_mode() {
    let (mut app, _state) = open(Vec::new());
    common::click(&mut app, "Find");
    common::wait_until(&mut app, "the first criterion focused", |app| {
        app.state().is_settled() && focused(app).is_some()
    });
    assert_eq!(
        focused(&app).map(|(label, _)| label).as_deref(),
        Some("Age")
    );
    let tab = |app: &mut common::App, modifiers: egui::Modifiers, typed: &str| {
        app.event_modifiers(
            egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            modifiers,
        );
        if !typed.is_empty() {
            app.event(egui::Event::Text(typed.to_owned()));
        }
        app.run_steps(4);
        focused(app).map(|(label, _)| label)
    };
    let value =
        |app: &common::App, field: &str| common::text_field(app, field).value().unwrap_or_default();
    let (next, back) = (egui::Modifiers::NONE, egui::Modifiers::SHIFT);
    app.event(egui::Event::Text(">3".into()));
    assert_eq!(tab(&mut app, next, "Zed").as_deref(), Some("Name"));
    assert_eq!(
        (value(&app, "Age"), value(&app, "Name")),
        (">3".into(), "Zed".into())
    );
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Note"));
    assert_eq!(tab(&mut app, back, "Q").as_deref(), Some("Name"));
    assert_eq!(value(&app, "Name"), "ZedQ", "typed after the criterion");
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Note"));
    assert_eq!(tab(&mut app, next, "").as_deref(), Some("Age"));
    assert_eq!(tab(&mut app, back, "").as_deref(), Some("Note"));
}

#[test]
fn find_mode_requests_omit_state_and_field_reads() {
    let requests = [
        "Get ( RequestCount )",
        "Get ( RequestOmitState )",
        "Get ( WindowMode )",
        "Get ( RecordNumber )",
        "Get ( ActiveRecordNumber )",
        "Get ( FoundCount )",
        "Get ( RecordID )",
        "Contacts::Name",
        "EvaluationError ( Contacts::Name )",
        "Contacts::gText",
    ];
    let scripts = vec![
        script(
            202,
            "Find probe",
            with_report(vec![
                set_variable("$$r", "\"\""),
                probe(&["Get ( RequestCount )"]),
                set_field(global(), "\"kept\""),
                step(22),
                probe(&["Get ( ActiveFieldName )", "Get ( RequestCount )"]),
                set_field(field("Name"), "\"a\""),
                step(7),
                step(25),
                // Request 2 of 2, which omits.
                probe(&requests),
                with_value(step(16), 1),
                probe(&requests),
                step(55),
                // Back in Browse mode without a find: the count stays.
                probe(&requests[..6]),
            ]),
        ),
        script(
            203,
            "Nothing found",
            with_report(vec![
                set_variable("$$r", "\"\""),
                {
                    let mut s = step(86);
                    s.flags |= flag::TURN_ON;
                    s
                },
                step(22),
                set_field(field("Name"), "\"Nobody\""),
                step(28),
                probe(&[
                    "Get ( LastError )",
                    "Get ( FoundCount )",
                    "Get ( RecordID )",
                    "Get ( RecordNumber )",
                    "Get ( ActiveRecordNumber )",
                    "Get ( RecordModificationCount )",
                    "Get ( TotalRecordCount )",
                    "Contacts::Name",
                    "EvaluationError ( Contacts::Name )",
                    "Contacts::gText",
                    "GetLayoutObjectAttribute ( \"Name field\" ; \"content\" )",
                    "Get ( WindowMode )",
                    "Get ( SortState )",
                ]),
            ]),
        ),
    ];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Find probe");
    assert_eq!(all[0], ["0"]);
    // Find mode focuses the first field of the tab order.
    assert_eq!(all[1], ["Age", "1"]);
    assert_eq!(all[2], ["2", "1", "1", "2", "2", "2", "0", "", "0", "kept"]);
    assert_eq!(
        all[3],
        ["2", "0", "1", "1", "1", "2", "0", "a", "0", "kept"]
    );
    assert_eq!(all[4], ["2", "0", "0", "1", "1", "3"]);

    let all = answers(&mut app, &state, "Nothing found");
    assert_eq!(
        all[0],
        ["401", "0", "0", "0", "0", "0", "3", "", "0", "kept", "", "0", "0"]
    );
}

#[test]
fn sorting_preview_printing_and_layout_object_functions() {
    let scripts = vec![script(
        204,
        "Sort and preview",
        with_report(vec![
            set_variable("$$r", "\"\""),
            probe(&[
                "Get ( SortState )",
                "Get ( PageNumber )",
                "Get ( PageCount )",
                "Get ( PrinterName )",
                "Get ( QuickFindText )",
                "GetRecordIDsFromFoundSet ( JSONStringRanges )",
                "GetRecordIDsFromFoundSet ( 2 )",
            ]),
            sort_by_name(),
            probe(&[
                "Get ( SortState )",
                "GetRecordIDsFromFoundSet ( JSONStringRanges )",
                "GetRecordIDsFromFoundSet ( 2 )",
                "EvaluationError ( GetRecordIDsFromFoundSet ( 5 ) )",
            ]),
            // Still sorted with a new record open (O3).
            step(7),
            probe(&["Get ( SortState )", "Get ( RecordOpenState )"]),
            no_dialog(step(51)),
            with_value(step(41), 0),
            probe(&[
                "Get ( WindowMode )",
                "Get ( PageNumber )",
                "Get ( PageCount )",
                "GetLayoutObjectAttribute ( \"Name field\" ; \"bounds\" )",
            ]),
            probe(&[
                "EvaluationError ( LayoutObjectUUID )",
                "JSONGetElement ( GetLayoutObjectOwnerInfo ( \"x\" ) ; \"Object.Index\" )",
                "Get ( UseSystemFormatsState )",
                "Get ( CurrentPrivilegeSetName )",
                "Length ( Get ( PersistentID ) )",
            ]),
        ]),
    )];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Sort and preview");
    // The found set's ids in its order, each run of ascending ids a range
    // (calc-functions.md, "GetRecordIDsFromFoundSet on FileMaker
    // Server"): Ada 1, Grace 2, Alan 3, then by name.
    assert_eq!(all[0], ["0", "0", "", " ", "", r#"["1-3"]"#, "[1,2,3]"]);
    assert_eq!(all[1], ["1", r#"["1","3","2"]"#, "[1,3,2]", "1708"]);
    assert_eq!(all[2], ["1", "1"]);
    // Preview has pages but no page count, and no object bounds.
    assert_eq!(all[3], ["2", "1", "", ""]);
    // A script is no layout object's calculation; owner information
    // finds nothing; the privilege set is not known to the desktop.
    assert_eq!(all[4], ["10", "4294967295", "1", "", "32"]);
}

#[test]
fn layout_object_uuid_inside_an_objects_calculation() {
    let (mut app, _state) = open(Vec::new());
    common::wait_for_label(&mut app, "OTHER UUID");
    assert!(
        egui_kittest::kittest::Queryable::query_by_label(&app, "OWN UUID").is_none(),
        "the object's own UUID hid it"
    );
}

/// `Get ( HostApplicationVersion )` names the host in a script and in the
/// window's layout calculations.
#[test]
fn the_host_application_version_reaches_scripts_and_layouts() {
    let scripts = vec![script(
        206,
        "Host probe",
        with_report(vec![
            set_variable("$$r", "\"\""),
            probe(&["Get ( HostApplicationVersion )"]),
        ]),
    )];
    let (mut app, state) = open(scripts);
    common::wait_for_label(&mut app, "OTHER UUID");
    assert!(
        egui_kittest::kittest::Queryable::query_by_label(&app, "HOST VERSION").is_none(),
        "the layout calculation read the host's version"
    );
    let all = answers(&mut app, &state, "Host probe");
    assert_eq!(all[0], ["Server 26.0.3"]);
}

/// The script and the layout use the same authenticated file facts. Missing
/// facts keep a hide condition unresolved, rather than hiding as if zero were real.
#[test]
fn account_facts_reach_scripts_and_layout_visibility() {
    use egui_kittest::kittest::Queryable;
    let make_scripts = || {
        vec![script(
            207,
            "Account probe",
            with_report(vec![
                set_variable("$$r", "\"\""),
                probe(&[
                    "Get ( AccountType )",
                    "Get ( AccountPasswordDaysRemaining )",
                ]),
            ]),
        )]
    };
    let (mut app, state) = open_with_account(
        make_scripts(),
        om_data::host::AccountInfo {
            account_type: Some("FileMaker File".into()),
            password_expiry: Some(1),
            set_access: None,
        },
    );
    assert!(app.query_by_label("EXPIRY WARNING").is_none());
    assert!(app
        .query_by_label("Undetermined text “Expiry warning”")
        .is_none());
    assert_eq!(
        answers(&mut app, &state, "Account probe")[0],
        ["FileMaker File", "0"]
    );
    drop(app);
    let (mut app, state) = open_with_account(make_scripts(), Default::default());
    common::wait_for_label(&mut app, "Undetermined text “Expiry warning”");
    assert_eq!(answers(&mut app, &state, "Account probe")[0], ["", ""]);
}

/// The built-in [Data Entry Only] and [Read-Only Access] sets may not
/// change the design (session-identity.md 2.2, 2.3): Edit Layout is
/// disabled and Layout mode's shortcut does nothing. [Full Access], and a
/// set the host did not verify as built in, keep both.
#[test]
fn limited_built_in_sets_keep_layout_mode_closed() {
    use egui_kittest::kittest::{NodeT as _, Queryable};
    use om_data::host::security::BuiltInSet;
    for (set, design) in [
        (Some(BuiltInSet::DataEntryOnly), false),
        (Some(BuiltInSet::ReadOnlyAccess), false),
        (Some(BuiltInSet::FullAccess), true),
        (None, true),
    ] {
        let (mut app, _state) = open_with_account(
            Vec::new(),
            om_data::host::AccountInfo {
                set_access: set.map(om_data::host::security::SetAccess::BuiltIn),
                ..Default::default()
            },
        );
        common::wait_for_label(&mut app, "Edit Layout");
        let disabled = app
            .get_by_label("Edit Layout")
            .accesskit_node()
            .is_disabled();
        assert_eq!(disabled, !design, "{set:?}");
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::L);
        app.run();
        assert_eq!(
            app.query_by_label("Exit Layout").is_some(),
            design,
            "{set:?}"
        );
    }
}

#[test]
fn window_names_and_uuids_follow_new_window() {
    let mut new_window = step(122);
    new_window.set_calculation(0, Some(&calc("\"Second\"")));
    // Select Window by name finds a name without case, else a window's
    // UUID (Claris, select-window.html; #320).
    let select = |name: &str| {
        let mut select = with_value(step(123), 1);
        select.set_calculation(0, Some(&calc(name)));
        select
    };
    let scripts = vec![script(
        205,
        "Windows probe",
        with_report(vec![
            set_variable("$$r", "\"\""),
            set_variable("$$u", "Get ( WindowUUID )"),
            probe(&["WindowNames", "WindowNames ( \"Contacts\" )"]),
            new_window,
            probe(&[
                "WindowNames",
                "Get ( WindowName )",
                "Get ( WindowUUID ) <> $$u",
                "Length ( Get ( WindowUUID ) )",
            ]),
            select("$$u"),
            probe(&["Get ( LastError )", "Get ( WindowName )"]),
            select("\"sECOND\""),
            probe(&["Get ( LastError )", "Get ( WindowName )"]),
            // A name as given wins over one that differs only in case.
            {
                let mut upper = step(122);
                upper.set_calculation(0, Some(&calc("\"SECOND\"")));
                upper
            },
            select("\"Second\""),
            probe(&["Get ( LastError )", "Get ( WindowName )"]),
            // Close Second, then SECOND, the window current before it.
            step(121),
            step(121),
            probe(&["WindowNames", "Get ( WindowUUID ) = $$u"]),
        ]),
    )];
    let (mut app, state) = open(scripts);
    let all = answers(&mut app, &state, "Windows probe");
    assert_eq!(all[0], ["Contacts", "Contacts"]);
    assert_eq!(all[1], ["Second\rContacts", "Second", "1", "32"]);
    assert_eq!(all[2], ["0", "Contacts"]);
    assert_eq!(all[3], ["0", "Second"]);
    assert_eq!(all[4], ["0", "Second"]);
    assert_eq!(all[5], ["Contacts", "1"]);
}
