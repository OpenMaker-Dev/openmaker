//! Checkbox and radio button sets, pop-up menus and drop-down lists in
//! Browse, using only a synthetic layout, value lists, scripts and
//! records: the user's mouse and keyboard choices become the record's
//! draft, go through the objects' own triggers and the field's exit
//! triggers, and are saved and read back from the provider; sets the user
//! may not change refuse before changing anything.
mod common;

use egui::{Key, Modifiers, Role};
use egui_kittest::kittest::{By, NodeT as _, Queryable};
use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
use om_data::host::RelationshipDef;
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, MemorySource, RecordValues, Recorder,
};
use om_data::meta::{value_list_option, JoinOp, JoinPredicate, ValueList, ValueSource};
use om_data::specs::SpecField;
use om_data::{
    Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, HostedFile, LayoutInfo,
    Record, RecordCapabilities, RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep,
};
use om_layout::{
    FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect, ScriptTrigger,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The table occurrence and its layout key (`occurrence_id(KEY) == ITEMS`).
const ITEMS: u64 = 0xd00001;
const KEY: u64 = 1065089;
const TABLE: u64 = 0x8001;
/// The second-field value list's table.
const CODES: u64 = 0xd00002;
const CODES_TABLE: u64 = 0x8002;
/// The table whose field's values Shade's drop-down list offers.
const TINTS: u64 = 0xd00003;
const TINTS_TABLE: u64 = 0x8003;
/// Tints related to every Items record (a Cartesian join): #290's
/// related-values list.
const RELATED_TINTS: u64 = 0xd00004;
const FIELDS: [&str; 8] = [
    "Colors", "Size", "Code", "Note", "Locked", "Kept", "Guarded", "Shade",
];
/// `FieldLayoutObject.flags` of Browse field entry View Only and Select
/// Only (layout-model.md 5.4).
const VIEW_ONLY: u32 = 0x4;
const SELECT_ONLY: u32 = 0x100_0000;
/// The flag of a drop-down list's arrow (`HasIcon`, layout-model.md 5.4).
const HAS_ICON: u32 = 0x400;
/// "Auto-complete using value list" (layout-model.md 5.4.1).
const AUTO_COMPLETE: u32 = 0x800;

/// What the Note field's OnObjectExit does.
#[derive(Clone, Copy, Default, PartialEq)]
enum Exit {
    #[default]
    None,
    /// Returns False: the focus stays in Note.
    Refuse,
    /// Goes to the next record and lets the focus go.
    Move,
    /// The second Colors object refuses exit.
    DuplicateVeto,
    /// Colors commits while its provider is deliberately held by the test.
    CommitModify,
}

#[derive(Default)]
struct UpdateGate {
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
}

struct ReleaseUpdate(Arc<UpdateGate>);

impl Drop for ReleaseUpdate {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.changed.notify_all();
    }
}

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    exit: Exit,
    /// Size and Shade have an OnObjectKeystroke that lets only Space
    /// through; a single checkbox Colors one that lets every key through.
    keys: bool,
    /// Shade's drop-down list is without its arrow ("Include arrow" off).
    shade_without_arrow: bool,
    /// Shade's drop-down list has "Auto-complete using value list".
    shade_completes: bool,
    /// Colors is a "Display single checkbox without value" set (field
    /// flag `0x400000`).
    single: bool,
    updates: Vec<String>,
    update_gate: Option<Arc<UpdateGate>>,
    /// The window's found set (Find mode's test).
    found: om_data::found::MemoryFoundSet,
    /// Every calculation data need the window asked for, in order.
    needs: Vec<CalcNeed>,
    /// How often the window asked the host for a field's list.
    host_lists: usize,
    /// The host cannot give the whole list: the window reads the records.
    no_host_list: bool,
    /// The custom values Edit Value List saved, by list.
    list_values: Vec<(u64, Vec<String>)>,
    /// How many Tints records there are (#290's measurement); none: the
    /// three named ones.
    tints: u64,
    /// Note has a pop-up menu of Tints::Label's values, an unstored
    /// calculation (#290's measurement).
    label_menu: bool,
    /// That menu's list is the related tints' Name values, shown by their
    /// Label, starting from Items instead (#290's related list).
    related_menu: bool,
    /// The layout's one part is a header Table view includes (#306).
    table_header: bool,
}

struct Provider(Arc<Mutex<State>>);

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Options", Locale::en_us());
    s.add_table(TABLE, "Items");
    s.add_occurrence(ITEMS, "Items", TABLE);
    for (id, name) in (1..).zip(FIELDS) {
        s.add_field(TABLE, FieldDef::data(id, name, ValueType::Text));
    }
    s.add_table(CODES_TABLE, "Codes");
    s.add_occurrence(CODES, "Codes", CODES_TABLE);
    s.add_field(CODES_TABLE, FieldDef::data(1, "Id", ValueType::Number));
    s.add_field(CODES_TABLE, FieldDef::data(2, "Name", ValueType::Text));
    s.add_table(TINTS_TABLE, "Tints");
    s.add_occurrence(TINTS, "Tints", TINTS_TABLE);
    s.add_field(TINTS_TABLE, FieldDef::data(1, "Name", ValueType::Text));
    // Tints::Label, unstored: Name & "!".
    let mut context = om_data::formula::Context::new(s.names());
    context.occurrence = Some(TINTS);
    let polish = om_data::formula::compile("Name & \"!\"", &context)
        .unwrap()
        .expr;
    s.add_field(
        TINTS_TABLE,
        FieldDef::calculation(
            2,
            "Label",
            ValueType::Text,
            om_data::formula::Formula {
                context: Some(om_data::formula::Key::from_id(TINTS)),
                formatted: None,
                polish,
            },
            false,
        ),
    );
    let custom = |id: u64, items: &str| ValueList {
        id,
        name: format!("List {id}"),
        source: ValueSource::Custom(vec![items.into()]),
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    };
    for (id, items) in [
        (1, "Red\rGreen\rBlue"),
        (2, "Small\rMedium\rLarge"),
        (4, "Teal\rOlive"),
        (5, "Sun\rRain"),
        (6, "Hot\rCold"),
        (7, "Up\rDown"),
    ] {
        s.add_value_list(custom(id, items));
    }
    // Shade's list: the values of the Tints table's Name field, in order
    // (Ash, Slate, Stone).
    s.add_value_list(ValueList {
        source: ValueSource::Field {
            primary: SpecField {
                occurrence: Some(TINTS),
                field: 1,
                repetition: 1,
            },
            secondary: None,
            start: None,
        },
        ..custom(8, "")
    });
    // Label menu's list: Tints::Label's values, shown by Tints::Name, so
    // that the closed menu reads its items to show its value's name
    // (#290's measurement).
    let tint = |field| SpecField {
        occurrence: Some(TINTS),
        field,
        repetition: 1,
    };
    s.add_value_list(ValueList {
        source: ValueSource::Field {
            primary: tint(2),
            secondary: Some(tint(1)),
            start: None,
        },
        options: value_list_option::SECOND_ONLY,
        ..custom(9, "")
    });
    // The related tints' Names, shown by their Labels, starting from Items
    // (#290's related list).
    s.add_occurrence(RELATED_TINTS, "Related tints", TINTS_TABLE);
    s.add_relationship(RelationshipDef::new(
        5,
        ITEMS,
        RELATED_TINTS,
        vec![JoinPredicate {
            op: JoinOp::Cartesian,
            left: 1,
            right: 1,
        }],
    ));
    let related = |field| SpecField {
        occurrence: Some(RELATED_TINTS),
        field,
        repetition: 1,
    };
    s.add_value_list(ValueList {
        source: ValueSource::Field {
            primary: related(1),
            secondary: Some(related(2)),
            start: Some(ITEMS),
        },
        options: value_list_option::SECOND_ONLY,
        ..custom(10, "")
    });
    let code = |field| SpecField {
        occurrence: Some(CODES),
        field,
        repetition: 1,
    };
    // Shows Codes::Name, stores Codes::Id.
    s.add_value_list(ValueList {
        source: ValueSource::Field {
            primary: code(1),
            secondary: Some(code(2)),
            start: None,
        },
        options: value_list_option::SECOND_ONLY,
        ..custom(3, "")
    });
    s.add_layout(7, "Options", Some(ITEMS));
    s
}

fn fields() -> Vec<Field> {
    FIELDS
        .map(|name| Field {
            name: name.into(),
            kind: FieldKind::Text,
        })
        .into()
}

impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Options.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Options".into(),
        }])
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(scripts())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let state = self.0.lock().unwrap();
        Ok(state.found.page(fields(), &state.rows, offset, limit))
    }
    fn change_found_set(
        &mut self,
        _: u32,
        change: om_data::found::FoundSetChange,
    ) -> Result<om_data::found::FoundSet, DataError> {
        let mut state = self.0.lock().unwrap();
        let State { found, rows, .. } = &mut *state;
        found.apply(&fields(), rows, change)
    }
    fn can_write_records(&self) -> bool {
        true
    }
    /// Guarded is view-only for this account.
    fn record_capabilities(&mut self, _: u32) -> Result<RecordCapabilities, DataError> {
        Ok(RecordCapabilities {
            create: false,
            update: true,
            delete: false,
            editable_fields: Some(
                FIELDS
                    .iter()
                    .filter(|name| **name != "Guarded")
                    .map(|name| (*name).to_owned())
                    .collect::<BTreeSet<_>>(),
            ),
            ..Default::default()
        })
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        let gate = self.0.lock().unwrap().update_gate.clone();
        if let Some(gate) = gate {
            gate.entered.store(true, Ordering::Release);
            let released = gate.released.lock().unwrap();
            drop(
                gate.changed
                    .wait_while(released, |released| !*released)
                    .unwrap(),
            );
        }
        let mut state = self.0.lock().unwrap();
        for name in values.keys() {
            if name == "Guarded" || name == "Locked" || name == "Kept" {
                return Err(DataError::new(format!("{name} must not be written")));
            }
        }
        let row = state
            .rows
            .iter_mut()
            .find(|row| row.id == id)
            .ok_or_else(|| DataError::new("Missing record"))?;
        let mut update = vec![format!("{id}")];
        for (name, value) in &values {
            let index = FIELDS.iter().position(|f| f == name).unwrap();
            row.values[index] = value.clone();
            update.push(format!("{name}={}", value.as_deref().unwrap_or("")));
        }
        let row = row.clone();
        state.updates.push(update.join(" "));
        Ok(row)
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        let state = self.0.lock().unwrap();
        let mut document = document(
            state.exit,
            state.keys,
            state.shade_without_arrow,
            state.shade_completes,
            state.single,
            state
                .label_menu
                .then_some(if state.related_menu { 10 } else { 9 }),
        );
        if state.table_header {
            let layout = &mut document.layout;
            layout.parts[0].part_type = 1;
            layout.def.as_mut().unwrap().flags = om_layout::layout::TABLE_HEADER;
        }
        Ok(Some(document))
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    /// Edit Value List opens on a custom list of two codes.
    fn custom_value_list(&mut self, _: u64) -> Result<om_data::value_lists::CustomList, DataError> {
        Ok(om_data::value_lists::CustomList {
            name: "Codes".into(),
            values: vec!["Alpha".into(), "-".into()],
            revision: [5; 32],
        })
    }
    fn set_value_list_values(
        &mut self,
        change: om_data::value_lists::ValuesChange,
    ) -> Result<om_data::layout_objects::FramedObjects, DataError> {
        assert_eq!(
            change.expected, [5; 32],
            "the revision the dialog opened on"
        );
        self.0
            .lock()
            .unwrap()
            .list_values
            .push((change.list, change.values));
        Ok(om_data::layout_objects::FramedObjects {
            changed: true,
            revision: [6; 32],
        })
    }
    /// The host lists Tints' names in index order, as one CreateValueListEx
    /// gives them, unless the test says it cannot.
    fn host_field_values(
        &mut self,
        table: u64,
        field: u64,
    ) -> Result<Option<Vec<String>>, DataError> {
        let mut state = self.0.lock().unwrap();
        state.host_lists += 1;
        Ok((table == TINTS_TABLE && field == 1 && !state.no_host_list)
            .then(|| ["Ash", "Slate", "Stone"].map(String::from).into()))
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        let schema = schema();
        let mut state = self.0.lock().unwrap();
        state.needs.extend(needs.iter().cloned());
        let mut source = MemorySource::new();
        for row in &state.rows {
            let mut values = RecordValues::default();
            for (id, value) in (1..).zip(&row.values) {
                values.set(id, 1, Value::text(value.as_deref().unwrap_or("")));
            }
            source.put_record(TABLE, row.id as u64, values);
        }
        let ids: Vec<u64> = state.rows.iter().map(|row| row.id as u64).collect();
        source.set_found_set(ITEMS, &ids, None);
        for (id, name) in [(1, "Alpha"), (2, "Beta"), (3, "Gamma")] {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::number(id));
            values.set(2, 1, Value::text(name));
            source.put_record(CODES_TABLE, id as u64, values);
        }
        if state.tints == 0 {
            for (id, name) in [(1, "Stone"), (2, "Ash"), (3, "Slate")] {
                let mut values = RecordValues::default();
                values.set(1, 1, Value::text(name));
                source.put_record(TINTS_TABLE, id, values);
            }
        }
        for id in 1..=state.tints {
            let mut values = RecordValues::default();
            values.set(1, 1, Value::text(format!("Tint {id}")));
            source.put_record(TINTS_TABLE, id, values);
        }
        let recorder = Recorder::new(&source);
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 11, 2).unwrap());
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        Ok(recorder.take())
    }
}

/// Set Field By Name [ "Note" ; `text` ].
fn note(text: &str) -> ScriptStep {
    let mut set = ScriptStep::new(147);
    set.set_calculation(1, Some(&common::workspace::calc("\"Note\"")));
    set.set_calculation(0, Some(&common::workspace::calc(&format!("\"{text}\""))));
    set
}

fn exit_script(result: &str) -> ScriptStep {
    let mut step = ScriptStep::new(103);
    step.set_calculation(0, Some(&common::workspace::calc(result)));
    step
}

/// 41 and 42 note which Colors object's OnObjectModify ran, 48 that the
/// Code pop-up menu's did, 49 that the Shade drop-down list's did; 43
/// refuses Note's exit; 44 goes to the next record and lets it go; 45 lets
/// only Space through.
fn scripts() -> ScriptCatalog {
    let mut next = ScriptStep::new(16);
    next.value = 4;
    let script = |id, name: &str, steps| Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    };
    ScriptCatalog {
        scripts: vec![
            script(41, "Original modified", vec![note("original")]),
            script(42, "Duplicate modified", vec![note("duplicate")]),
            script(43, "Refuse exit", vec![exit_script("0")]),
            script(44, "Move on", vec![next, exit_script("1")]),
            script(47, "Commit modified", vec![ScriptStep::new(75)]),
            script(48, "Pop-up modified", vec![note("popup")]),
            script(49, "Drop-down modified", vec![note("dropdown")]),
            script(
                45,
                "Only Space",
                vec![exit_script("Get ( TriggerKeystroke ) = \" \"")],
            ),
            script(46, "Every key", vec![exit_script("1")]),
        ],
        writable: false,
        unavailable_reason: None,
    }
}

fn trigger(event: u16, script: u64) -> ScriptTrigger {
    ScriptTrigger {
        event,
        script: Some(script),
        modes: 1,
        file_reference: None,
        external: false,
        option: 0,
        extra: vec![],
        parameter: None,
    }
}

fn object(id: u64, kind: u32, x: f64, y: f64, height: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        bounds: Some(Rect {
            left: x,
            top: y,
            right: x + 240.0,
            bottom: y + height,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Field `field_key` as display type `display_type` (0 edit box, 1
/// drop-down list, 2 pop-up menu, 3 checkbox set, 4 radio button set) of
/// value list `list`.
fn field(
    id: u64,
    field_key: u64,
    display_type: u32,
    list: u64,
    flags: u32,
    (x, y): (f64, f64),
) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            flags: 0xe0 | flags,
            field_spec: Some(FieldSpec {
                table_key: KEY,
                field_key,
                repetition: 1,
                ..Default::default()
            }),
            display_type,
            value_list_key: list,
            ..Default::default()
        }),
        ..object(id, 1, x, y, if display_type <= 2 { 24.0 } else { 48.0 })
    }
}

fn document(
    exit: Exit,
    keys: bool,
    shade_without_arrow: bool,
    completes: bool,
    single: bool,
    label_menu: Option<u64>,
) -> RenderLayout {
    let title = LayoutObject {
        text: Some("OPTION SETS".into()),
        ..object(1, 2, 20.0, 5.0, 24.0)
    };
    let mut colors = field(2, 1, 3, 1, if single { 0x40_0000 } else { 0 }, (20.0, 40.0));
    // A saved leading larger than the marker's font size: the options
    // must use the same line boxes their labels were shaped into.
    colors.style = Some(om_layout::ObjectStyle {
        local_styles: Some(om_layout::StyleSet {
            components: vec![om_layout::style::ComponentStyle {
                component: 1,
                state: 1,
                properties: [(69, 9.0), (72, 14.0)]
                    .into_iter()
                    .map(|(id, value)| om_layout::Property {
                        id,
                        length: Some(om_layout::Length {
                            value,
                            unit: 6,
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    });
    // Independent synthetic component geometry, in em and points, rather
    // than a copy of a FileMaker theme or a customer's saved style.
    colors
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components
        .push(om_layout::style::ComponentStyle {
            component: 3,
            state: 1,
            properties: [(120, 1.5, 7), (108, 4.0, 6)]
                .into_iter()
                .map(|(id, value, unit)| om_layout::Property {
                    id,
                    length: Some(om_layout::Length {
                        value,
                        unit,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });

    colors
        .triggers
        .push(trigger(3, if exit == Exit::CommitModify { 47 } else { 41 }));
    // The single checkbox's OnObjectKeystroke lets every key through.
    if keys && single {
        colors.triggers.push(trigger(5, 46));
    }
    let mut note = field(5, 4, 0, 0, 0, (20.0, 220.0));
    match exit {
        Exit::None | Exit::DuplicateVeto | Exit::CommitModify => {}
        Exit::Refuse => note.triggers.push(trigger(2, 43)),
        Exit::Move => note.triggers.push(trigger(2, 44)),
    }
    // A second object of Colors, with its own items and trigger.
    let mut duplicate = field(9, 1, 3, 4, 0, (300.0, 220.0));
    duplicate.triggers.push(trigger(3, 42));
    if exit == Exit::DuplicateVeto {
        duplicate.triggers.push(trigger(2, 43));
    }
    // A pop-up menu of Code, whose list shows names and stores ids, with
    // its own OnObjectModify, and Other… and Edit… after its items.
    let mut code_menu = field(10, 3, 2, 3, OTHER_AND_EDIT, (300.0, 250.0));
    code_menu.triggers.push(trigger(3, 48));
    // A drop-down list of Shade with its arrow ("Include arrow",
    // `HasIcon`) unless asked otherwise, and its own OnObjectModify.
    let arrow = if shade_without_arrow { 0 } else { HAS_ICON };
    let complete = if completes { AUTO_COMPLETE } else { 0 };
    let mut shade = field(11, 8, 1, 8, arrow | complete, (20.0, 250.0));
    shade.triggers.push(trigger(3, 49));
    let mut size = field(3, 2, 4, 2, 0, (20.0, 100.0));
    if keys {
        size.triggers.push(trigger(5, 45));
        shade.triggers.push(trigger(5, 45));
    }
    let mut objects = vec![
        title,
        colors,
        size,
        field(4, 3, 4, 3, 0, (20.0, 160.0)),
        note,
        field(6, 5, 3, 5, VIEW_ONLY, (300.0, 40.0)),
        field(7, 6, 4, 6, SELECT_ONLY, (300.0, 100.0)),
        field(8, 7, 3, 7, 0, (300.0, 160.0)),
        duplicate,
        code_menu,
        shade,
    ];
    if let Some(list) = label_menu {
        objects.push(field(12, 4, 2, list, 0, (460.0, 250.0)));
    }
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Options".into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                table_alias_key: KEY,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 280.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: objects.iter().map(|o| o.id).collect(),
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([
            (2, "Colors".into()),
            (3, "Size".into()),
            (4, "Code".into()),
            (5, "Note".into()),
            (6, "Locked".into()),
            (7, "Kept".into()),
            (8, "Guarded".into()),
            (9, "Colors".into()),
            (10, "Code".into()),
            (11, "Shade".into()),
            (12, "Note".into()),
        ]),
    }
}

fn record(id: i64, values: [&str; 8]) -> Record {
    Record {
        id,
        values: values.map(|v| (!v.is_empty()).then(|| v.to_owned())).into(),
    }
}

fn state(exit: Exit) -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State {
        rows: vec![
            // Colors holds one value outside its list.
            record(
                19,
                [
                    "Red\rBlue\rMauve",
                    "Medium",
                    "1",
                    "",
                    "Sun",
                    "Hot",
                    "Up",
                    "",
                ],
            ),
            record(20, ["", "Small", "3", "", "", "", "", ""]),
        ],
        exit,
        keys: false,
        shade_without_arrow: false,
        shade_completes: false,
        single: false,
        updates: Vec::new(),
        update_gate: None,
        found: Default::default(),
        needs: Vec::new(),
        host_lists: 0,
        no_host_list: false,
        list_values: Vec::new(),
        tints: 0,
        label_menu: false,
        related_menu: false,
        table_header: false,
    }))
}

/// A window on the file, its first record's sets drawn.
fn open(state: &Arc<Mutex<State>>) -> common::App {
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/options.example.invalid/Options.fmp12",
    );
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "OPTION SETS");
    common::wait_until(&mut app, "the value lists' items", |app| {
        app.query_by_role_and_label(Role::RadioButton, "Gamma")
            .is_some()
    });
    app
}

/// Whether item `label` of a `role` set is checked, and whether the user
/// may change it.
fn item(app: &common::App, role: Role, label: &str) -> (bool, bool) {
    let node = app.get_by_role_and_label(role, label);
    let node = node.accesskit_node();
    (
        node.toggled() == Some(egui::accesskit::Toggled::True),
        !node.is_disabled(),
    )
}

/// What the Code pop-up menu shows; `None` while it is not offered (its
/// list's items are read again after a script changes the record).
fn menu_shows(app: &common::App) -> Option<String> {
    app.query_by_role_and_label(Role::ComboBox, "Code")
        .and_then(|node| node.accesskit_node().value())
}

/// Whether radio button `label` is checked; `None` while its set is not
/// drawn.
fn radio_checked(app: &common::App, label: &str) -> Option<bool> {
    app.query_by_role_and_label(Role::RadioButton, label)
        .map(|node| node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
}

fn checked(app: &common::App, role: Role, label: &str) -> bool {
    item(app, role, label).0
}

/// Click item `label` with the mouse.
fn pick(app: &mut common::App, role: Role, label: &str) {
    app.get_by_role_and_label(role, label).click();
    app.run_steps(2);
}

fn idle(app: &mut common::App) {
    common::wait_until(app, "scripts to end", |app| !app.state().script_running());
}

fn save(app: &mut common::App, state: &Arc<Mutex<State>>) {
    common::save_record(app);
    common::wait_until(app, "the save", |_| {
        !state.lock().unwrap().updates.is_empty()
    });
    idle(app);
}

/// What the Shade drop-down list's editor holds, and whether it has the
/// focus; `None` while it is not drawn.
fn shade(app: &common::App) -> Option<(String, bool)> {
    common::find_text_field(app, "Shade").map(|node| {
        let focused = node.is_focused();
        (node.accesskit_node().value().unwrap_or_default(), focused)
    })
}

/// Whether the Shade drop-down list's list is open.
fn shade_list(app: &common::App) -> bool {
    app.query_by_role_and_label(Role::Button, "Ash").is_some()
}

/// The middle of the Shade drop-down list's arrow: right of its editor
/// while it has the focus, else inside the control's right edge.
fn shade_arrow(app: &common::App) -> egui::Pos2 {
    let node = common::text_field(app, "Shade");
    let rect = node.rect();
    let x = if node.is_focused() {
        rect.right() + 10.0
    } else {
        rect.right() - 10.0
    };
    egui::pos2(x, rect.center().y)
}

fn press_at(app: &common::App, pos: egui::Pos2, pressed: bool) {
    app.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
    });
}

/// Click the Shade drop-down list's arrow with the mouse.
fn click_shade_arrow(app: &mut common::App) {
    let pos = shade_arrow(app);
    app.hover_at(pos);
    press_at(app, pos, true);
    press_at(app, pos, false);
    app.run_steps(2);
}

/// The field object flags that end a menu with “Other…” and “Edit…”.
const OTHER_AND_EDIT: u32 = 0x1 | 0x8;

/// Open the Code pop-up menu and wait for its items.
fn open_code_menu(app: &mut common::App) {
    app.get_by_role_and_label(Role::ComboBox, "Code").click();
    app.run_steps(2);
    common::wait_until(app, "the Code menu", |app| {
        app.query_by_role_and_label(Role::Button, "Edit…").is_some()
    });
}

#[test]
fn other_enters_a_value_and_edit_saves_the_list_values() {
    let state = state(Exit::None);
    let mut app = open(&state);
    // Other… and Edit… end the menu, after the list's items, as in
    // FileMaker Pro 26.0.3 (#869).
    open_code_menu(&mut app);
    let top = |app: &common::App, label: &str| {
        app.get_by_role_and_label(Role::Button, label).rect().top()
    };
    assert!(top(&app, "Gamma") < top(&app, "Other…"));
    assert!(top(&app, "Other…") < top(&app, "Edit…"));
    app.get_by_role_and_label(Role::Button, "Other…").click();
    app.run_steps(2);
    common::wait_until(&mut app, "the Other… dialog", |app| {
        app.query_by_role_and_label(Role::MultilineTextInput, "Other…")
            .is_some_and(|node| node.is_focused())
    });
    app.event(egui::Event::Text("Dr".into()));
    app.run_steps(2);
    common::click(&mut app, "OK");
    // The menu shows the value no item stores, its OnObjectModify runs, and
    // the record's draft holds it until the save.
    common::wait_until(&mut app, "Dr entered", |app| {
        menu_shows(app).as_deref() == Some("Dr")
    });
    common::wait_for_value(&mut app, "popup");
    idle(&mut app);
    // Edit… commits the record first, as Pro does, then opens Edit Value
    // List on the list's values, an item per line.
    open_code_menu(&mut app);
    app.get_by_role_and_label(Role::Button, "Edit…").click();
    app.run_steps(2);
    common::wait_until(&mut app, "Edit Value List", |app| {
        app.query_by_role_and_label(Role::MultilineTextInput, "Edit Value List “Codes”")
            .is_some_and(|node| node.value().as_deref() == Some("Alpha\n-"))
    });
    assert_eq!(state.lock().unwrap().updates.len(), 1, "committed first");
    // A value and a divider added; OK saves the values, on the revision
    // read when the dialog opened.
    app.key_press_modifiers(Modifiers::COMMAND, egui::Key::End);
    app.event(egui::Event::Text("\nOM869\n-".into()));
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the list saved", |_| {
        !state.lock().unwrap().list_values.is_empty()
    });
    common::wait_until(&mut app, "the dialog closed", |app| {
        app.query_by_role(Role::MultilineTextInput).is_none()
    });
    assert_eq!(
        state.lock().unwrap().list_values,
        [(3, ["Alpha", "-", "OM869", "-"].map(String::from).to_vec())]
    );
    assert_eq!(menu_shows(&app).as_deref(), Some("Dr"));
    let updates = &state.lock().unwrap().updates;
    assert_eq!(updates.len(), 1);
    assert!(updates[0].contains("Code=Dr "), "{updates:?}");
}

/// Other… on a pop-up menu in a header Table view includes (#306) opens
/// its dialog there too, and enters the value.
#[test]
fn other_works_in_a_header_table_view_includes() {
    let state = state(Exit::None);
    state.lock().unwrap().table_header = true;
    let mut app = open(&state);
    common::click(&mut app, "Table View");
    common::wait_until(&mut app, "the table", |app| {
        app.query_by_role_and_label(Role::Label, "Shade").is_some()
    });
    open_code_menu(&mut app);
    app.get_by_role_and_label(Role::Button, "Other…").click();
    app.run_steps(2);
    common::wait_until(&mut app, "the Other… dialog", |app| {
        app.query_by_role_and_label(Role::MultilineTextInput, "Other…")
            .is_some_and(|node| node.is_focused())
    });
    app.event(egui::Event::Text("Dr".into()));
    app.run_steps(2);
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "Dr entered", |app| {
        menu_shows(app).as_deref() == Some("Dr")
    });
}

/// A checkbox set shown as one checkbox without value (FileMaker Pro 26.1's
/// "Display single checkbox without value", field flag `0x400000`): one
/// box and no labels, checked by the list's first value. Clearing it
/// stores the list's second value, checking it the first.
#[test]
fn a_single_checkbox_toggles_between_the_lists_first_and_second_values() {
    // Colors holds Red, Blue and Mauve: Red, the first of Red, Green and
    // Blue, checks the box, and a click is refused rather than write over
    // the other values.
    let mixed = state(Exit::None);
    mixed.lock().unwrap().single = true;
    let mut app = open(&mixed);
    assert!(checked(&app, Role::CheckBox, "Colors"));
    for item in ["Red", "Green", "Blue"] {
        assert!(app.query_by_label(item).is_none(), "{item}");
    }
    pick(&mut app, Role::CheckBox, "Colors");
    common::wait_until(&mut app, "the refusal", |app| app.state().error().is_some());
    assert!(
        app.state()
            .error()
            .is_some_and(|error| error.contains("other than its checkbox's two")),
        "{:?}",
        app.state().error()
    );
    assert!(checked(&app, Role::CheckBox, "Colors"));
    assert!(mixed.lock().unwrap().updates.is_empty());

    // Colors holds Red alone: a click stores Green, the next Red.
    let state = state(Exit::None);
    {
        let mut state = state.lock().unwrap();
        state.single = true;
        state.keys = true;
        state.rows[0].values[0] = Some("Red".into());
    }
    let mut app = open(&state);
    assert!(checked(&app, Role::CheckBox, "Colors"));
    pick(&mut app, Role::CheckBox, "Colors");
    common::wait_until(&mut app, "the box cleared", |app| {
        !checked(app, Role::CheckBox, "Colors")
    });
    idle(&mut app);
    save(&mut app, &state);
    let updates = state.lock().unwrap().updates.clone();
    assert!(
        updates.last().is_some_and(|u| u.contains("Colors=Green")),
        "{updates:?}"
    );
    pick(&mut app, Role::CheckBox, "Colors");
    common::wait_until(&mut app, "the box checked", |app| {
        checked(app, Role::CheckBox, "Colors")
    });
    idle(&mut app);
    // A letter its keystroke trigger lets through changes nothing; Space
    // toggles.
    // The first of the two sets of Colors: the single checkbox.
    app.query_all_by_role_and_label(Role::Group, "Colors")
        .next()
        .unwrap()
        .focus();
    app.run_steps(3);
    idle(&mut app);
    app.event(egui::Event::Text("a".into()));
    app.run_steps(2);
    idle(&mut app);
    app.run_steps(5);
    assert!(checked(&app, Role::CheckBox, "Colors"));
    app.key_press(Key::Space);
    app.run_steps(2);
    common::wait_until(&mut app, "Space through the trigger", |app| {
        !app.state().script_running() && !checked(app, Role::CheckBox, "Colors")
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn mouse_choices_become_the_draft_save_and_read_back_from_the_provider() {
    let state = state(Exit::None);
    let mut app = open(&state);
    // Each line of the value checks its item; the line outside the list
    // draws nothing. The second-field list shows names, checked by id.
    assert!(checked(&app, Role::CheckBox, "Red"));
    assert!(!checked(&app, Role::CheckBox, "Green"));
    assert!(checked(&app, Role::CheckBox, "Blue"));
    assert!(checked(&app, Role::RadioButton, "Medium"));
    assert!(checked(&app, Role::RadioButton, "Alpha"));
    assert!(!checked(&app, Role::RadioButton, "Beta"));
    assert!(app.query_by_label("Mauve").is_none());
    assert!(common::find_text_field(&app, "Colors").is_none());

    // A checkbox turned on adds its value; the set's own OnObjectModify
    // runs (it notes "original" in Note).
    pick(&mut app, Role::CheckBox, "Green");
    common::wait_until(&mut app, "Green checked", |app| {
        checked(app, Role::CheckBox, "Green")
    });
    common::wait_for_value(&mut app, "original");
    idle(&mut app);
    // Turned off, Red's line goes and the others stay.
    pick(&mut app, Role::CheckBox, "Red");
    common::wait_until(&mut app, "Red cleared", |app| {
        !checked(app, Role::CheckBox, "Red")
    });
    idle(&mut app);
    // A radio button replaces the value; Shift-click clears it.
    pick(&mut app, Role::RadioButton, "Large");
    common::wait_until(&mut app, "Large chosen", |app| {
        checked(app, Role::RadioButton, "Large") && !checked(app, Role::RadioButton, "Medium")
    });
    app.get_by_role_and_label(Role::RadioButton, "Large")
        .click_modifiers(Modifiers::SHIFT);
    app.run_steps(2);
    common::wait_until(&mut app, "Large cleared", |app| {
        ["Small", "Medium", "Large"]
            .iter()
            .all(|size| !checked(app, Role::RadioButton, size))
    });
    pick(&mut app, Role::RadioButton, "Small");
    // The second-field set stores the chosen name's id.
    pick(&mut app, Role::RadioButton, "Beta");
    common::wait_until(&mut app, "Beta chosen", |app| {
        checked(app, Role::RadioButton, "Beta")
            && !checked(app, Role::RadioButton, "Alpha")
            && checked(app, Role::RadioButton, "Small")
    });
    idle(&mut app);
    // The Code pop-up menu shows the chosen item's name, not the id it
    // stores, and takes no typing; clicked, it is entered and offers the
    // list's items in a menu.
    assert_eq!(menu_shows(&app).as_deref(), Some("Beta"));
    assert!(common::find_text_field(&app, "Code").is_none());
    app.get_by_role_and_label(Role::ComboBox, "Code").click();
    app.run_steps(2);
    common::wait_until(&mut app, "the menu", |app| {
        app.query_by_role_and_label(Role::Button, "Gamma").is_some()
    });
    // Choosing Gamma puts its id in this record's draft, the field the
    // radio set shows too, and runs the menu's own OnObjectModify.
    app.get_by_role_and_label(Role::Button, "Gamma").click();
    app.run_steps(2);
    common::wait_for_value(&mut app, "popup");
    idle(&mut app);
    common::wait_until(&mut app, "Gamma chosen", |app| {
        radio_checked(app, "Gamma") == Some(true)
            && radio_checked(app, "Beta") == Some(false)
            && menu_shows(app).as_deref() == Some("Gamma")
    });
    assert!(app.query_by_role_and_label(Role::Button, "Alpha").is_none());
    // Nothing was written yet: these are the record's draft.
    assert!(state.lock().unwrap().updates.is_empty());
    assert!(common::unsaved(&app));

    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Code=3 Colors=Blue\rMauve\rGreen Note=popup Size=Small"]
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    drop(app);

    // A new window reads the record back from the provider.
    let app = open(&state);
    assert!(!checked(&app, Role::CheckBox, "Red"));
    assert!(checked(&app, Role::CheckBox, "Green"));
    assert!(checked(&app, Role::CheckBox, "Blue"));
    assert!(checked(&app, Role::RadioButton, "Small"));
    assert!(checked(&app, Role::RadioButton, "Gamma"));
    assert!(!checked(&app, Role::RadioButton, "Beta"));
    assert_eq!(menu_shows(&app).as_deref(), Some("Gamma"));
}

#[test]
fn the_keyboard_picks_items_with_arrows_and_letters_and_enters_them_with_space() {
    let state = state(Exit::None);
    let mut app = open(&state);
    app.get_by_role_and_label(Role::RadioGroup, "Size").focus();
    app.run_steps(3);
    assert!(app
        .get_by_role_and_label(Role::RadioGroup, "Size")
        .is_focused());
    // From the checked item, Medium: Down picks Large, Space enters it.
    app.key_press(Key::ArrowDown);
    app.key_press(Key::Space);
    app.run_steps(3);
    common::wait_until(&mut app, "Large by keyboard", |app| {
        checked(app, Role::RadioButton, "Large")
    });
    // A first letter picks its item.
    app.event(egui::Event::Text("s".into()));
    app.key_press(Key::Space);
    app.run_steps(3);
    common::wait_until(&mut app, "Small by keyboard", |app| {
        checked(app, Role::RadioButton, "Small") && !checked(app, Role::RadioButton, "Large")
    });
    // The arrows stayed in the set: the focus did not move.
    assert!(app
        .get_by_role_and_label(Role::RadioGroup, "Size")
        .is_focused());

    // Into the checkbox set: Up from Red wraps to Blue and turns it off.
    let colors = app
        .query_all_by_role_and_label(Role::Group, "Colors")
        .next()
        .unwrap();
    colors.focus();
    app.run_steps(3);
    idle(&mut app);
    app.key_press(Key::ArrowUp);
    app.key_press(Key::Space);
    app.run_steps(3);
    common::wait_until(&mut app, "Blue off by keyboard", |app| {
        !checked(app, Role::CheckBox, "Blue") && checked(app, Role::CheckBox, "Red")
    });
    idle(&mut app);
    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Colors=Red\rMauve Note=original Size=Small"]
    );
}

#[test]
fn a_field_exit_veto_drops_the_click_that_began_it() {
    let state = state(Exit::Refuse);
    let mut app = open(&state);
    common::text_field(&app, "Note").click();
    app.run_steps(3);
    common::wait_until(&mut app, "Note focused", |app| {
        common::text_field(app, "Note").is_focused()
    });
    pick(&mut app, Role::CheckBox, "Green");
    common::wait_until(&mut app, "the refusal", |app| {
        !app.state().script_running() && common::text_field(app, "Note").is_focused()
    });
    // A click on the pop-up menu is refused the same way: no menu opens.
    app.get_by_role_and_label(Role::ComboBox, "Code").click();
    app.run_steps(2);
    common::wait_until(&mut app, "the second refusal", |app| {
        !app.state().script_running() && common::text_field(app, "Note").is_focused()
    });
    app.run_steps(10);
    assert!(app.query_by_role_and_label(Role::Button, "Gamma").is_none());
    // The clicks entered nothing and changed nothing, then or later.
    assert!(!checked(&app, Role::CheckBox, "Green"));
    assert!(common::text_field(&app, "Note").is_focused());
    assert!(!common::unsaved(&app));
    assert!(state.lock().unwrap().updates.is_empty());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn a_click_that_waited_for_an_exit_trigger_goes_when_the_record_changed() {
    let state = state(Exit::Move);
    let mut app = open(&state);
    common::text_field(&app, "Note").click();
    app.run_steps(3);
    pick(&mut app, Role::CheckBox, "Green");
    // The exit trigger let the focus go, but went to record 2 first: the
    // click, made on record 1, changes neither record.
    common::wait_until(&mut app, "the next record", |app| {
        !app.state().script_running() && common::record_number(app) == "2"
    });
    common::wait_until(&mut app, "record 2's sets", |app| {
        checked(app, Role::RadioButton, "Gamma")
    });
    app.run_steps(10);
    assert!(!checked(&app, Role::CheckBox, "Green"));
    assert!(!checked(&app, Role::CheckBox, "Red"));
    assert!(!common::unsaved(&app));
    assert!(state.lock().unwrap().updates.is_empty());
}

#[test]
fn each_object_of_a_field_runs_its_own_triggers() {
    let state = state(Exit::None);
    let mut app = open(&state);
    // The second Colors object shows its own list and runs its own
    // OnObjectModify, not the first object's.
    pick(&mut app, Role::CheckBox, "Teal");
    common::wait_until(&mut app, "Teal checked", |app| {
        checked(app, Role::CheckBox, "Teal")
    });
    common::wait_for_value(&mut app, "duplicate");
    idle(&mut app);
    // Moving to the first object is a focus change too: its trigger runs.
    pick(&mut app, Role::CheckBox, "Green");
    common::wait_until(&mut app, "Green checked", |app| {
        checked(app, Role::CheckBox, "Green")
    });
    common::wait_for_value(&mut app, "original");
    idle(&mut app);
    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Colors=Red\rBlue\rMauve\rTeal\rGreen Note=original"]
    );
}

#[test]
fn sets_the_user_may_not_change_refuse_before_changing() {
    let state = state(Exit::None);
    let mut app = open(&state);
    // View Only and Select Only field entry, and a field the account may
    // only view: shown, but not enabled.
    for (role, label, on) in [
        (Role::CheckBox, "Sun", true),
        (Role::CheckBox, "Rain", false),
        (Role::RadioButton, "Hot", true),
        (Role::RadioButton, "Cold", false),
        (Role::CheckBox, "Up", true),
        (Role::CheckBox, "Down", false),
    ] {
        assert_eq!(item(&app, role, label), (on, false), "{label}");
    }
    for label in ["Locked", "Kept", "Guarded"] {
        assert!(
            app.query_by_label(label).is_none(),
            "{label} is no focus stop"
        );
    }
    for (role, label) in [
        (Role::CheckBox, "Rain"),
        (Role::CheckBox, "Sun"),
        (Role::RadioButton, "Cold"),
        (Role::CheckBox, "Down"),
    ] {
        pick(&mut app, role, label);
    }
    app.run_steps(10);
    assert!(!checked(&app, Role::CheckBox, "Rain"));
    assert!(checked(&app, Role::CheckBox, "Sun"));
    assert!(checked(&app, Role::RadioButton, "Hot"));
    assert!(!checked(&app, Role::CheckBox, "Down"));
    assert!(!common::unsaved(&app));
    assert!(state.lock().unwrap().updates.is_empty());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn a_list_row_click_changes_that_rows_record_once_it_is_current() {
    let state = state(Exit::None);
    let mut app = open(&state);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "View as List");
    assert_eq!(app.state().view(), om_gui::ViewMode::List);
    common::wait_until(&mut app, "both rows", |app| {
        app.query_all_by_role_and_label(Role::CheckBox, "Green")
            .count()
            == 2
    });
    // Record 2's row (not current): its Green.
    app.query_all_by_role_and_label(Role::CheckBox, "Green")
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(2);
    common::wait_until(&mut app, "record 2's Green", |app| {
        let greens: Vec<_> = app
            .query_all_by_role_and_label(Role::CheckBox, "Green")
            .map(|node| node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
            .collect();
        greens == [false, true]
    });
    idle(&mut app);
    assert_eq!(common::record_number(&app), "2");
    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["20 Colors=Green Note=original"]
    );
}

#[test]
fn a_keystroke_trigger_decides_on_each_key_of_a_set() {
    let state = state(Exit::None);
    state.lock().unwrap().keys = true;
    let mut app = open(&state);
    app.get_by_role_and_label(Role::RadioGroup, "Size").focus();
    app.run_steps(3);
    idle(&mut app);
    // Down picks Large; the trigger refuses "s", so Small is not picked,
    // and lets Space through, which enters Large once.
    app.key_press(Key::ArrowDown);
    app.run_steps(2);
    app.event(egui::Event::Text("s".into()));
    app.run_steps(2);
    idle(&mut app);
    app.key_press(Key::Space);
    app.run_steps(2);
    common::wait_until(&mut app, "Large through the trigger", |app| {
        !app.state().script_running() && checked(app, Role::RadioButton, "Large")
    });
    app.run_steps(5);
    assert!(!checked(&app, Role::RadioButton, "Small"));
    assert!(!checked(&app, Role::RadioButton, "Medium"));
    save(&mut app, &state);
    assert_eq!(state.lock().unwrap().updates, ["19 Size=Large"]);

    // A drop-down list's open list keeps the keyboard with its editor:
    // the trigger still refuses "z" and lets Space in.
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "Shade's list", shade_list);
    app.event(egui::Event::Text("z".into()));
    app.run_steps(2);
    idle(&mut app);
    app.event(egui::Event::Text(" ".into()));
    app.run_steps(2);
    common::wait_until(&mut app, "Space through Shade's trigger", |app| {
        !app.state().script_running() && shade(app) == Some((" ".into(), true))
    });
    assert!(shade_list(&app));
}

#[test]
fn a_duplicate_exit_veto_returns_to_the_exact_control() {
    let state = state(Exit::DuplicateVeto);
    let mut app = open(&state);
    app.query_all_by_role_and_label(Role::Group, "Colors")
        .nth(1)
        .unwrap()
        .focus();
    app.run_steps(3);
    pick(&mut app, Role::RadioButton, "Large");
    idle(&mut app);
    common::wait_until(&mut app, "the vetoing duplicate focused", |app| {
        app.query_all_by_role_and_label(Role::Group, "Colors")
            .nth(1)
            .unwrap()
            .is_focused()
    });
    assert!(!app
        .query_all_by_role_and_label(Role::Group, "Colors")
        .next()
        .unwrap()
        .is_focused());
    assert!(!checked(&app, Role::RadioButton, "Large"));
}

#[test]
fn a_click_while_the_modify_script_owns_the_window_is_not_replayed() {
    let state = state(Exit::CommitModify);
    let mut app = open(&state);
    let gate = Arc::new(UpdateGate::default());
    // Drop after app is created, so even a failed assertion releases its
    // blocked worker before the harness shuts down.
    let release = ReleaseUpdate(gate.clone());
    state.lock().unwrap().update_gate = Some(gate.clone());
    pick(&mut app, Role::CheckBox, "Green");
    common::wait_until(&mut app, "the modify script's commit", |app| {
        gate.entered.load(Ordering::Acquire) && app.state().script_running()
    });
    pick(&mut app, Role::CheckBox, "Blue");
    assert!(checked(&app, Role::CheckBox, "Blue"));
    drop(release);
    idle(&mut app);
    app.run_steps(8);
    idle(&mut app);
    assert!(checked(&app, Role::CheckBox, "Blue"));
    assert!(checked(&app, Role::CheckBox, "Green"));
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Colors=Red\rBlue\rMauve\rGreen"]
    );
}

#[test]
fn a_drop_down_list_types_like_an_edit_box_and_offers_its_items_from_its_arrow() {
    let state = state(Exit::None);
    let mut app = open(&state);
    assert_eq!(shade(&app), Some((String::new(), false)));
    assert!(!shade_list(&app));

    // A click on the arrow enters the field and opens the list, changing
    // nothing; Escape closes it and the field keeps the focus.
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "the list", shade_list);
    assert_eq!(shade(&app), Some((String::new(), true)));
    assert!(!common::unsaved(&app));
    app.key_press(Key::Escape);
    app.run_steps(2);
    assert!(!shade_list(&app));
    assert_eq!(shade(&app), Some((String::new(), true)));

    // It takes typing, a value that is no item too; its OnObjectModify
    // runs, editing Note.
    common::text_field(&app, "Shade").type_text("Delta");
    app.run_steps(2);
    common::wait_for_value(&mut app, "dropdown");
    idle(&mut app);
    assert_eq!(shade(&app), Some(("Delta".into(), true)));

    // Opened again and closed with Escape, the draft is as it was: the
    // typed text and the other field's edit.
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "the list again", shade_list);
    app.key_press(Key::Escape);
    app.run_steps(2);
    assert!(!shade_list(&app));
    assert_eq!(shade(&app), Some(("Delta".into(), true)));
    assert!(app
        .query(By::new().role(Role::TextInput).value("dropdown"))
        .is_some());
    assert!(state.lock().unwrap().updates.is_empty());

    // A press on the arrow of the open list, held into the next frame,
    // keeps the field's focus and its Escape: Escape in that frame closes
    // the list and leaves the field entered. Moved off the arrow, the
    // release is no click.
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "the list to press on", shade_list);
    let arrow = shade_arrow(&app);
    app.hover_at(arrow);
    press_at(&app, arrow, true);
    app.step();
    app.key_press(Key::Escape);
    app.run_steps(2);
    assert!(!shade_list(&app));
    assert_eq!(shade(&app), Some(("Delta".into(), true)));
    let off = arrow + egui::vec2(0.0, 40.0);
    app.hover_at(off);
    press_at(&app, off, false);
    app.run_steps(2);
    assert!(!shade_list(&app));
    assert_eq!(shade(&app), Some(("Delta".into(), true)));

    // Choosing an item replaces the typed value in the draft; the field
    // keeps the focus.
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "the list for a choice", shade_list);
    app.get_by_role_and_label(Role::Button, "Stone").click();
    app.run_steps(2);
    idle(&mut app);
    common::wait_until(&mut app, "Stone chosen", |app| {
        shade(app) == Some(("Stone".into(), true)) && !shade_list(app)
    });
    assert!(state.lock().unwrap().updates.is_empty());

    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Note=dropdown Shade=Stone"]
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    drop(app);

    // A new window reads it back from the provider.
    let app = open(&state);
    assert_eq!(shade(&app), Some(("Stone".into(), false)));
}

/// A drop-down list without its arrow ("Include arrow" off) is an edit
/// box whose list opens as the field is entered (#178; Claris's help,
/// unmeasured on FileMaker Pro). Escape closes it and leaves the field
/// entered; entering the field again opens it again.
/// Painting a closed drop-down list asks for nothing (a production
/// solution with a 75,000-record table froze while a closed list was built
/// in paint). Opened, its field's list comes from the host in one request,
/// as FileMaker Pro asks for it (#464), and no record of the list's table
/// is read; entered again, it asks again. When the host cannot give the
/// whole list, the list's table's records give it.
#[test]
fn a_drop_down_list_asks_the_host_for_its_list_at_each_opening() {
    let state = state(Exit::None);
    let mut app = open(&state);
    let tints = |state: &Arc<Mutex<State>>| {
        state
            .lock()
            .unwrap()
            .needs
            .iter()
            .filter(|need| {
                matches!(need, CalcNeed::Records { table } | CalcNeed::Record { table, .. }
                    if *table == TINTS_TABLE)
            })
            .count()
    };
    let asked = |state: &Arc<Mutex<State>>| state.lock().unwrap().host_lists;
    app.run_steps(10);
    assert_eq!((asked(&state), tints(&state)), (0, 0));
    click_shade_arrow(&mut app);
    common::wait_until(&mut app, "Shade's list", shade_list);
    assert_eq!((asked(&state), tints(&state)), (1, 0));
    // Leaving the field and opening the list again asks again.
    let reopen = |app: &mut common::App| {
        common::text_field(app, "Note").click();
        common::wait_until(app, "Shade's list closed", |app| !shade_list(app));
        click_shade_arrow(app);
        common::wait_until(app, "Shade's list", shade_list);
    };
    reopen(&mut app);
    assert_eq!((asked(&state), tints(&state)), (2, 0));
    state.lock().unwrap().no_host_list = true;
    reopen(&mut app);
    assert_eq!(asked(&state), 3);
    assert!(tints(&state) > 0);
}

#[test]
fn a_drop_down_list_without_its_arrow_opens_its_list_when_entered() {
    let state = state(Exit::None);
    state.lock().unwrap().shade_without_arrow = true;
    let mut app = open(&state);
    assert_eq!(shade(&app), Some((String::new(), false)));
    assert!(!shade_list(&app));

    common::text_field(&app, "Shade").click();
    common::wait_until(&mut app, "the list", shade_list);
    assert_eq!(shade(&app), Some((String::new(), true)));
    app.key_press(Key::Escape);
    app.run_steps(2);
    assert!(!shade_list(&app));
    assert_eq!(shade(&app), Some((String::new(), true)));

    // Out of the field and back in: the list opens again, and choosing an
    // item replaces the value (through Shade's OnObjectModify, which sets
    // Note); the field keeps the focus.
    common::text_field(&app, "Note").click();
    app.run_steps(2);
    assert_eq!(shade(&app), Some((String::new(), false)));
    common::text_field(&app, "Shade").click();
    common::wait_until(&mut app, "the list again", shade_list);
    app.get_by_role_and_label(Role::Button, "Stone").click();
    app.run_steps(2);
    idle(&mut app);
    common::wait_until(&mut app, "Stone chosen", |app| {
        shade(app) == Some(("Stone".into(), true)) && !shade_list(app)
    });
    save(&mut app, &state);
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Note=dropdown Shade=Stone"]
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A drop-down list with "Auto-complete using value list" completes what
/// is typed from its items, as FileMaker Pro 26.0.3 does on the Contacts
/// starter's Company (#870's readings): the typed letters keep their case
/// and the rest of the first matching item follows them, selected, with
/// the list closed; Backspace removes it; leaving the field keeps the
/// item's own text. Entering the field asks the host for the list once,
/// as opening it does, and typing asks no more. Find mode's criterion
/// completes the same way.
#[test]
fn a_drop_down_list_completes_typing_from_its_items() {
    let state = state(Exit::None);
    state.lock().unwrap().shade_completes = true;
    let mut app = open(&state);
    let asked = |state: &Arc<Mutex<State>>| state.lock().unwrap().host_lists;
    // Each key, then Shade's OnObjectModify script, which writes Note.
    let typed = |app: &mut common::App, text: &str| {
        app.event(egui::Event::Text(text.into()));
        app.run_steps(3);
        idle(app);
        app.run_steps(2);
    };
    common::text_field(&app, "Shade").focus();
    common::wait_until(&mut app, "the list read", |_| asked(&state) == 1);
    app.run_steps(4);
    // The host lists Ash, Slate, Stone: "s" is Slate's beginning.
    typed(&mut app, "s");
    assert_eq!(shade(&app), Some(("slate".into(), true)));
    assert!(!shade_list(&app), "no list while completing");
    // "late" is selected: the next letter replaces it, Backspace removes it.
    typed(&mut app, "t");
    assert_eq!(shade(&app).map(|(text, _)| text).as_deref(), Some("stone"));
    app.key_press(Key::Backspace);
    app.run_steps(3);
    idle(&mut app);
    app.run_steps(2);
    assert_eq!(shade(&app).map(|(text, _)| text).as_deref(), Some("st"));
    typed(&mut app, "o");
    assert_eq!(shade(&app).map(|(text, _)| text).as_deref(), Some("stone"));
    assert_eq!(asked(&state), 1, "typing asks the host no more");
    // Leaving the field keeps the item's own text.
    common::text_field(&app, "Note").click();
    app.run_steps(4);
    assert_eq!(shade(&app).map(|(text, _)| text).as_deref(), Some("Stone"));
    save(&mut app, &state);
    // Shade's OnObjectModify wrote Note.
    assert_eq!(
        state.lock().unwrap().updates,
        ["19 Note=dropdown Shade=Stone"]
    );

    // Find mode's criterion.
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::wait_for_label(&mut app, "Perform Find");
    common::text_field(&app, "Shade").focus();
    app.run_steps(4);
    common::wait_until(&mut app, "the criterion's list read", |_| {
        asked(&state) >= 2
    });
    app.run_steps(4);
    typed(&mut app, "A");
    assert_eq!(shade(&app).map(|(text, _)| text).as_deref(), Some("Ash"));
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Table view: a cell whose field the layout shows as a drop-down list with
/// "Auto-complete using value list" completes typing as that list does,
/// as FileMaker Pro 26.0.3 does in Table view on Contacts' Company (#870's
/// readings); leaving the cell keeps the item's own text.
#[test]
fn a_table_view_cell_completes_from_its_drop_down_list() {
    let state = state(Exit::None);
    state.lock().unwrap().shade_completes = true;
    let mut app = open(&state);
    common::click(&mut app, "Table View");
    app.run_steps(4);
    let cell = |app: &common::App, field: &str| {
        app.query_all(By::new().role(Role::TextInput).label(field))
            .next()
            .and_then(|node| node.value())
    };
    app.query_all(By::new().role(Role::TextInput).label("Shade"))
        .next()
        .unwrap()
        .focus();
    common::wait_until(&mut app, "Shade's list read", |_| {
        state.lock().unwrap().host_lists == 1
    });
    app.run_steps(4);
    app.event(egui::Event::Text("s".into()));
    app.run_steps(3);
    idle(&mut app);
    app.run_steps(2);
    assert_eq!(cell(&app, "Shade").as_deref(), Some("slate"));
    // Leaving the cell keeps the item's own text.
    app.query_all(By::new().role(Role::TextInput).label("Note"))
        .next()
        .unwrap()
        .click();
    app.run_steps(4);
    common::wait_until(&mut app, "the item's text", |app| {
        cell(app, "Shade").as_deref() == Some("Slate")
    });
    assert_eq!(
        state.lock().unwrap().host_lists,
        1,
        "typing asks the host no more"
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Find mode draws the sets, pop-up menus and drop-down lists as Browse
/// does, and a pick sets the request's criterion: the Small radio button
/// finds the record whose Size is Small.
#[test]
fn find_mode_takes_criteria_from_the_value_list_controls() {
    let state = state(Exit::None);
    let mut app = open(&state);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::wait_for_label(&mut app, "Perform Find");
    common::wait_until(&mut app, "the find request's sets", |app| {
        radio_checked(app, "Small") == Some(false)
    });
    // A pick makes its set the field Insert > Operators goes into, not the
    // field typed in before.
    common::fill(&mut app, "Note", "x");
    pick(&mut app, Role::RadioButton, "Medium");
    common::click(&mut app, "Operators");
    common::click(&mut app, "==   match entire field");
    assert_eq!(
        common::text_field(&app, "Note").value().as_deref(),
        Some("x")
    );
    // A second, blank request: the first finds nothing.
    common::click(&mut app, "New Request");
    common::wait_until(&mut app, "a new blank request", |app| {
        radio_checked(app, "Medium") == Some(false)
    });
    pick(&mut app, Role::RadioButton, "Small");
    assert_eq!(radio_checked(&app, "Small"), Some(true));
    assert_eq!(radio_checked(&app, "Medium"), Some(false));
    common::click(&mut app, "Perform Find");
    common::wait_until(&mut app, "the found set", |app| {
        !app.state().script_running() && app.query_by_label("Found (Unsorted)").is_some()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::wait_until(&mut app, "record 20", |app| {
        radio_checked(app, "Small") == Some(true)
    });
    assert_eq!(common::record_number(&app), "1");
}

/// Saved leading and text-component offsets control labels and hit regions.
#[test]
fn option_rows_use_the_labels_saved_leading() {
    let state = state(Exit::None);
    let mut app = open(&state);
    if let Some(path) = std::env::var_os("OM_OPTION_GEOMETRY_PNG") {
        app.set_render_cursor(false);
        app.render()
            .expect("render neutral Browse fixture")
            .save(path)
            .expect("save neutral capture");
    }
    let red = app.get_by_role_and_label(Role::CheckBox, "Red").rect();
    let green = app.get_by_role_and_label(Role::CheckBox, "Green").rect();
    let blue = app.get_by_role_and_label(Role::CheckBox, "Blue").rect();
    assert!((green.top() - red.top() - 14.0).abs() < 0.01);
    assert!((blue.top() - green.top() - 14.0).abs() < 0.01);
    assert!(red.bottom() <= green.top() && green.bottom() <= blue.top());
    fn label_left(shape: &egui::Shape) -> Option<f32> {
        match shape {
            egui::Shape::Text(text) if text.galley.job.text == "Red" => Some(text.pos.x),
            egui::Shape::Vec(shapes) => shapes.iter().find_map(label_left),
            _ => None,
        }
    }
    let label = app
        .output()
        .shapes
        .iter()
        .find_map(|shape| label_left(&shape.shape))
        .expect("Red label painted");
    // 1.5 em of the resolved 9-point font, plus a 4-point margin.
    assert!(
        (label - red.left() - 17.5).abs() < 0.01,
        "label={label}, row={red:?}"
    );

    pick(&mut app, Role::CheckBox, "Green");
    common::wait_until(&mut app, "the spaced checkbox changed", |app| {
        checked(app, Role::CheckBox, "Green")
    });
    assert!(checked(&app, Role::CheckBox, "Red") && checked(&app, Role::CheckBox, "Blue"));
}

/// Measurement for #290: a pop-up menu whose list is an unstored
/// calculation's values (Tints::Label) over `TINTS` records, on a record
/// whose Note holds one of them, shown by Tints::Name: the closed menu
/// reads its items to show its value's name. Prints the longest frame
/// until it shows it, and the whole wait: a list built in paint shows as
/// one frame as long as the build.
#[test]
#[ignore = "measurement for #290; set OM_TINTS (default 30000), prints the longest frame"]
fn a_calculated_lists_build_and_the_frames_it_takes() {
    let tints = std::env::var("OM_TINTS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(30_000u64);
    let state = state(Exit::None);
    {
        let mut state = state.lock().unwrap();
        state.tints = tints;
        state.label_menu = true;
        state.rows[0].values[3] = Some("Tint 7!".into());
    }
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/options.example.invalid/Options.fmp12",
    );
    common::click(&mut app, "Open");
    // The closed menu of Label shows the name its value has.
    let shows = |app: &common::App| {
        app.query_by_role_and_label(Role::ComboBox, "Note")
            .and_then(|node| node.accesskit_node().value())
            .is_some_and(|value| value == "Tint 7")
    };
    let started = std::time::Instant::now();
    let mut longest = std::time::Duration::ZERO;
    let mut frames = 0;
    while !shows(&app) && started.elapsed() < std::time::Duration::from_secs(600) {
        let frame = std::time::Instant::now();
        app.run_steps(1);
        longest = longest.max(frame.elapsed());
        frames += 1;
    }
    assert!(shows(&app), "the menu never showed its value");
    println!(
        "tints {tints}: shown: longest frame {longest:?} over {frames} frames, {:?} in all",
        started.elapsed()
    );
}

/// Measurement for #290's related lists: as
/// [`a_calculated_lists_build_and_the_frames_it_takes`], the menu's list
/// the `TINTS` related tints' Names, shown by their Labels, starting from
/// Items (every tint is related): built per record, in paint today.
#[test]
#[ignore = "measurement for #290; set OM_TINTS (default 30000), prints the longest frame"]
fn a_related_lists_build_and_the_frames_it_takes() {
    let tints = std::env::var("OM_TINTS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(30_000u64);
    let state = state(Exit::None);
    {
        let mut state = state.lock().unwrap();
        state.tints = tints;
        state.label_menu = true;
        state.related_menu = true;
        state.rows[0].values[3] = Some("Tint 7".into());
    }
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/options.example.invalid/Options.fmp12",
    );
    common::click(&mut app, "Open");
    let shows = |app: &common::App| {
        app.query_by_role_and_label(Role::ComboBox, "Note")
            .and_then(|node| node.accesskit_node().value())
            .is_some_and(|value| value == "Tint 7!")
    };
    let started = std::time::Instant::now();
    let mut longest = std::time::Duration::ZERO;
    let mut frames = 0;
    while !shows(&app) && started.elapsed() < std::time::Duration::from_secs(600) {
        let frame = std::time::Instant::now();
        app.run_steps(1);
        longest = longest.max(frame.elapsed());
        frames += 1;
    }
    assert!(shows(&app), "the menu never showed its value");
    println!(
        "related tints {tints}: shown: longest frame {longest:?} over {frames} frames, {:?} in all",
        started.elapsed()
    );
}
