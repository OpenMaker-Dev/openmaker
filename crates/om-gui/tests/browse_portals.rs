//! End-to-end Browse portals, using only synthetic layouts and records.
mod common;

use egui::{epaint::Shape, Role};
use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
use om_data::formula::{self, Formula};
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, JoinOp, JoinPredicate, MemorySource, RecordValues,
    Recorder, RelationshipDef,
};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordCapabilities, RecordPage, RenderLayout, Script, ScriptCatalog,
};
use om_layout::{
    ButtonObject, FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, ObjectStore, Part,
    PortalObject, Rect, ScriptStep, TextRun,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
};

const PEOPLE: u64 = 0xd00001;
const LINES: u64 = 0xd00002;
const P: u64 = 0x8001;
const C: u64 = 0x8002;
type Reads = Arc<Mutex<Vec<CalcNeed>>>;

/// Holds back the provider's reads of related (Lines) records until opened.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn open(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
    fn close(&self) {
        *self.0 .0.lock().unwrap() = false;
    }
    fn is_open(&self) -> bool {
        *self.0 .0.lock().unwrap()
    }
    fn wait(&self) {
        let (open, wake) = &*self.0;
        let _open = wake.wait_while(open.lock().unwrap(), |open| !*open);
    }
}
/// Opens the gate when a test ends, passing or not, so the provider thread
/// never waits forever.
struct Release(Gate);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.open();
    }
}

fn formula(schema: &CalcSchema, text: &str) -> Formula {
    Formula {
        context: None,
        formatted: None,
        polish: formula::compile(text, &formula::Context::new(schema.names()))
            .unwrap()
            .expr,
    }
}
fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Portal fixture", Locale::en_us());
    for (table, occurrence, name) in [(P, PEOPLE, "People"), (C, LINES, "Lines")] {
        s.add_table(table, name);
        s.add_occurrence(occurrence, name, table);
        s.add_field(table, FieldDef::data(1, "Given", ValueType::Text));
    }
    s.add_field(P, FieldDef::data(2, "Note", ValueType::Text));
    s.add_field(C, FieldDef::data(2, "Owner", ValueType::Text));
    s.add_field(C, FieldDef::data(3, "Visible", ValueType::Number));
    let mut relation = RelationshipDef::new(
        1,
        PEOPLE,
        LINES,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 2,
        }],
    );
    relation.options = 0x10;
    s.add_relationship(relation);
    s.add_value_list(om_data::meta::ValueList {
        id: 1,
        name: "Names".into(),
        source: om_data::meta::ValueSource::Custom(vec!["Aster\rBaker\rCora".into()]),
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    });
    s.add_layout(7, "Portal fixture", Some(PEOPLE));
    s
}
fn source() -> MemorySource {
    let mut s = MemorySource::new();
    for (record, name) in [(19, "Clara"), (37, "Emery")] {
        let mut row = RecordValues::default();
        row.set(1, 1, Value::text(name));
        row.set(2, 1, Value::text(""));
        s.put_record(P, record, row);
    }
    for (record, name, owner, visible) in [
        (10, "Baker", "Clara", 1),
        (20, "Aster", "Clara", 1),
        (30, "Hidden", "Clara", 0),
        (40, "Other", "Emery", 1),
    ] {
        let mut row = RecordValues::default();
        row.set(1, 1, Value::text(name));
        row.set(2, 1, Value::text(owner));
        row.set(3, 1, Value::number(visible));
        s.put_record(C, record, row);
    }
    s.set_found_set(PEOPLE, &[19, 37], None);
    s
}
/// Which of the portal's sort and filter the fixture keeps. Without them,
/// rows paint as soon as the related set arrives, before their records do.
#[derive(Clone, Copy, Debug)]
struct Calcs {
    sort: bool,
    filter: bool,
    /// The row's Lines::Given control (layout-model.md 5.4): 0 an edit
    /// box; a pop-up menu (2) or a drop-down list with its arrow (1) of
    /// list 1 is one the user may change in a related record (#537).
    control: u32,
    /// The row's control hides when its Lines::Given is empty, as in the
    /// creation row.
    hide_empty: bool,
    /// The row's button performs script 1, which shows the row it runs in.
    row_script: bool,
    /// The portal's related records cannot be read: every read of them
    /// fails.
    fail_related: bool,
}
const SORTED_AND_FILTERED: Calcs = Calcs {
    sort: true,
    filter: true,
    control: 0,
    hide_empty: false,
    row_script: false,
    fail_related: false,
};
struct Provider {
    reads: Reads,
    calcs: Calcs,
    gate: Option<Gate>,
    /// Holds back every calc data reply while closed, counting the replies
    /// it held.
    hold: Option<(Gate, Arc<AtomicUsize>)>,
}
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Portals.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Portal fixture".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        // A page carries a related column as its first related value. A
        // portal row must never show it as its own.
        Ok(RecordPage {
            versions: Default::default(),
            fields: ["Given", "Lines::Given", "Note"]
                .map(|name| Field {
                    name: name.into(),
                    kind: FieldKind::Text,
                })
                .into(),
            records: [(19, "Clara"), (37, "Emery")]
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(|(id, name)| Record {
                    id,
                    values: vec![
                        Some(name.into()),
                        Some("Page line".into()),
                        Some(String::new()),
                    ],
                })
                .collect(),
            total: 2,
        })
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn record_capabilities(&mut self, _: u32) -> Result<RecordCapabilities, DataError> {
        Ok(RecordCapabilities {
            create: true,
            update: true,
            delete: true,
            editable_fields: None,
            related: if self.calcs.control != 0 || self.calcs.row_script {
                BTreeMap::from([(LINES, [1].into())])
            } else {
                BTreeMap::new()
            },
            ..Default::default()
        })
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        changes: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        // Only the row-script fixture saves the parent's typing.
        assert!(
            self.calcs.row_script,
            "a passive portal must not write to its parent"
        );
        let given = changes
            .get("Given")
            .cloned()
            .unwrap_or_else(|| Some("Clara".into()));
        let note = changes.get("Note").cloned().flatten();
        Ok(Record {
            id,
            values: vec![given, Some("Page line".into()), note],
        })
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(document(self.calcs)))
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        if !self.calcs.row_script {
            return Ok(ScriptCatalog::default());
        }
        // Show Custom Dialog ["row " & the active row's number and record].
        let mut show = om_data::ScriptStep::new(87);
        show.set_text(0, Some("OK"));
        let compiled = formula::compile(
            "\"row \" & Get ( ActivePortalRowNumber ) & \" \" & Lines::Given",
            &formula::Context::new(schema().names()),
        )
        .unwrap();
        show.set_calculation(
            1,
            Some(&om_data::Calculation::new(
                Some(formula::formatted::encode(&compiled.tokens)),
                formula::polish::encode(&compiled.expr),
            )),
        );
        Ok(ScriptCatalog {
            scripts: vec![Script {
                id: 1,
                name: "Row dialog".into(),
                steps: vec![show],
                ..Default::default()
            }],
            ..Default::default()
        })
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.reads.lock().unwrap().extend_from_slice(needs);
        if self.calcs.fail_related
            && needs.iter().any(|need| {
                matches!(
                    need,
                    CalcNeed::Related { to: LINES, .. } | CalcNeed::RelatedIds { to: LINES, .. }
                )
            })
        {
            return Err(DataError::new("The related records cannot be read."));
        }
        if let Some((hold, held)) = &self.hold {
            if !hold.is_open() {
                held.fetch_add(1, Ordering::SeqCst);
            }
            hold.wait();
        }
        if let Some(gate) = &self.gate {
            if needs
                .iter()
                .any(|need| matches!(need, CalcNeed::Record { table: C, .. }))
            {
                gate.wait();
            }
        }
        let schema = schema();
        let source = source();
        let recorder = Recorder::new(&source);
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 10, 30).unwrap());
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        let mut data = recorder.take();
        if self.gate.is_some() {
            // Answer related IDs without the related records read to match
            // them, as a hosted file does: rows then read their own records.
            data.records.retain(|&(table, record), _| {
                table != C || needs.contains(&CalcNeed::Record { table, record })
            });
        }
        Ok(data)
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
fn field(id: u64, parent: Option<u64>, table_key: u64, x: f64, y: f64) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key,
                field_key: 1,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(id, 1, parent, x, y, 110.0, 20.0)
    }
}
fn document(calcs: Calcs) -> RenderLayout {
    let schema = schema();
    let mut portal = LayoutObject {
        children: vec![11, 12, 13],
        portal: Some(PortalObject {
            table_alias_key: 1065090,
            num_rows: 3.0,
            initial_row: 1,
            flags: 0x8,
            ..Default::default()
        }),
        ..object(10, 9, None, 20.0, 90.0, 500.0, 120.0)
    };
    // The filter reads the parent too, so a parent draft refilters rows.
    // With the row script, it reads the parent's Note rather than its
    // match key: a draft of it reads the same related records again.
    portal.calcs.portal_filter = Some(formula(
        &schema,
        if calcs.row_script {
            "Lines::Visible > 0 and People::Note <> \"Nobody\""
        } else {
            "Lines::Visible > 0 and People::Given <> \"Nobody\""
        },
    ));
    portal.calcs.portal_sort = Some(
        om_data::specs::SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![om_data::specs::SortEntry {
                field: om_data::specs::SpecField {
                    occurrence: Some(LINES),
                    field: 1,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        }
        .encode(),
    );
    if !calcs.sort {
        portal.calcs.portal_sort = None;
    }
    if !calcs.filter {
        portal.calcs.portal_filter = None;
    }
    let mut merge = LayoutObject {
        text_runs: vec![
            TextRun::Literal("Line ".into()),
            TextRun::MergeField("Lines::Given".into()),
            TextRun::Literal(" / ".into()),
            TextRun::MergeField("Given".into()),
        ],
        ..object(12, 2, Some(10), 125.0, 0.0, 310.0, 20.0)
    };
    merge.calcs.tooltip = Some(formula(&schema, "People::Given & \"/\" & Lines::Given"));
    let action = LayoutObject {
        text: Some("PASSIVE ACTION".into()),
        button: Some(ButtonObject {
            script_step: Some(ScriptStep {
                id: 7,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(13, 10, Some(10), 0.0, 20.0, 150.0, 20.0)
    };
    // Perform Script [1], the script named by key in mReserved.
    let action = if calcs.row_script {
        let mut reference = vec![2, 1, 1, 0xbe];
        reference.resize(14, 0);
        LayoutObject {
            text: Some("ROW DIALOG".into()),
            button: Some(ButtonObject {
                script_step: Some(ScriptStep {
                    id: 1,
                    reserved: reference,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..action
        }
    } else {
        action
    };
    let mut row_field = field(11, Some(10), 1065090, 0.0, 0.0);
    if calcs.control != 0 {
        let control = row_field.field_obj.as_mut().unwrap();
        control.display_type = calcs.control;
        control.value_list_key = 1;
        // `HasIcon`: a drop-down list's arrow.
        control.flags = 0x400;
    }
    if calcs.hide_empty {
        row_field.calcs.hide = Some(formula(&schema, "IsEmpty ( Lines::Given )"));
    }
    // The parent's Note (field 2), shown with the row script only.
    let mut note = field(3, None, 1065089, 20.0, 65.0);
    if let Some(spec) = note.field_obj.as_mut().and_then(|f| f.field_spec.as_mut()) {
        spec.field_key = 2;
    }
    let title = LayoutObject {
        text: Some("RELATED LINES".into()),
        ..object(1, 2, None, 20.0, 5.0, 300.0, 25.0)
    };
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Portal fixture".into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                table_alias_key: 1065089,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 250.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: if calcs.row_script {
                    vec![1, 2, 3, 10]
                } else {
                    vec![1, 2, 10]
                },
                objects: vec![
                    title,
                    field(2, None, 1065089, 20.0, 40.0),
                    portal,
                    row_field,
                    merge,
                    action,
                    note,
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        // The collision is intentional: a related row must never show or edit the parent's Given cell.
        field_bindings: BTreeMap::from([
            (2, "Given".into()),
            (3, "Note".into()),
            (11, "Given".into()),
        ]),
    }
}
fn open() -> (common::App, Reads) {
    open_with(SORTED_AND_FILTERED, None)
}
fn open_with(calcs: Calcs, gate: Option<Gate>) -> (common::App, Reads) {
    open_holding(calcs, gate, None)
}
fn open_holding(
    calcs: Calcs,
    gate: Option<Gate>,
    hold: Option<(Gate, Arc<AtomicUsize>)>,
) -> (common::App, Reads) {
    let reads = Reads::default();
    let mut app = common::harness(Box::new(Provider {
        reads: reads.clone(),
        calcs,
        gate,
        hold,
    }));
    common::fill(&mut app, "Host", "portals.example.invalid");
    common::fill(&mut app, "File", "Portals.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "RELATED LINES");
    (app, reads)
}
fn text_y(shape: &Shape, text: &str) -> Option<f32> {
    match shape {
        Shape::Text(t) if t.galley.job.text == text => Some(t.pos.y),
        Shape::Vec(items) => items.iter().find_map(|s| text_y(s, text)),
        _ => None,
    }
}
fn painted_texts(shape: &Shape, texts: &mut Vec<String>) {
    match shape {
        Shape::Text(t) => texts.push(t.galley.job.text.clone()),
        Shape::Vec(items) => items.iter().for_each(|s| painted_texts(s, texts)),
        _ => {}
    }
}
/// Every text the last frame painted.
fn texts(app: &common::App) -> Vec<String> {
    let mut texts = Vec::new();
    for shape in &app.output().shapes {
        painted_texts(&shape.shape, &mut texts);
    }
    texts
}
#[test]
fn related_rows_sort_filter_merge_and_follow_parent_navigation() {
    let (mut app, reads) = open();
    common::wait_for_label(&mut app, "Line Aster / Clara");
    common::wait_for_label(&mut app, "Line Baker / Clara");
    assert!(app.query_by_label("Hidden").is_none());
    assert!(app.query_by_label("Other").is_none());
    let y = |text| {
        app.output()
            .shapes
            .iter()
            .find_map(|s| text_y(&s.shape, text))
            .unwrap()
    };
    assert!(
        y("Aster") < y("Baker"),
        "portal sort must override relationship order"
    );
    assert!(reads.lock().unwrap().contains(&CalcNeed::RelatedIds {
        from: PEOPLE,
        record: 19,
        to: LINES
    }));
    assert!(!reads.lock().unwrap().contains(&CalcNeed::Record {
        table: C,
        record: 40
    }));
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Line Other / Emery");
    assert!(app.query_by_label("Line Aster / Clara").is_none());
    assert!(app.state().error().is_none());
}
#[test]
fn portal_fields_cannot_target_the_parent_editor_and_buttons_belong_to_rows() {
    let (mut app, _) = open();
    common::wait_for_label(&mut app, "Line Aster / Clara");
    app.get_by_label("Aster");
    assert!(app
        .query_by_role_and_label(Role::TextInput, "Aster")
        .is_none());
    // One action button in each related row (Aster, Baker), none for the
    // parent or the empty creation row.
    assert_eq!(
        app.query_all_by_role_and_label(Role::Button, "PASSIVE ACTION")
            .count(),
        2
    );
    app.get_by_label("Aster").click();
    app.run_steps(3);
    assert_eq!(common::record_number(&app), "1");
    assert!(app.state().error().is_none());
    common::wait_for_label(&mut app, "Line Baker / Clara");
}
#[test]
fn list_view_rows_show_each_parents_set_and_a_row_click_selects_its_record() {
    let (mut app, _) = open();
    common::wait_for_label(&mut app, "Line Aster / Clara");
    common::open_menu(&mut app, "View");
    common::click(&mut app, "View as List");
    assert_eq!(app.state().view(), om_gui::ViewMode::List);
    // Both parents' portals are on screen in one frame, each with its rows.
    common::wait_until(&mut app, "both parents' rows", |app| {
        [
            "Line Aster / Clara",
            "Line Baker / Clara",
            "Line Other / Emery",
        ]
        .iter()
        .all(|label| app.query_by_label(label).is_some())
    });
    assert!(app.query_by_label("Line Other / Clara").is_none());
    assert!(app.query_by_label("Line Aster / Emery").is_none());
    assert_eq!(common::record_number(&app), "1");
    // The row lies over record 2's own click sensor; clicking it must
    // still make record 2 current.
    app.get_by_label("Other").click();
    common::wait_until(&mut app, "the clicked row's record", |app| {
        common::record_number(app) == "2"
    });
    assert!(app.state().error().is_none());
}
#[test]
fn rows_waiting_for_their_records_never_show_parent_values() {
    for sort in [false, true] {
        let gate = Gate::default();
        let _release = Release(gate.clone());
        let (mut app, reads) = open_with(
            Calcs {
                sort,
                filter: false,
                ..SORTED_AND_FILTERED
            },
            Some(gate.clone()),
        );
        common::wait_until(&mut app, "the related records' request", |_| {
            reads
                .lock()
                .unwrap()
                .iter()
                .any(|need| matches!(need, CalcNeed::Record { table: C, .. }))
        });
        for _ in 0..10 {
            app.step();
            let texts = texts(&app);
            let rows = texts.iter().filter(|t| t.starts_with("Line ")).count();
            if sort {
                // A sorted set is not known before its sort keys are...
                assert_eq!(rows, 0, "{texts:?}");
            } else {
                // ...while other rows are drawn without their records' values...
                assert!(rows > 0, "{texts:?}");
                assert!(texts.iter().all(|t| !t.starts_with("Line A")), "{texts:?}");
            }
            // ...and only the parent's own field shows the parent's value.
            assert!(
                texts.iter().filter(|t| *t == "Clara").count() <= 1,
                "{texts:?}"
            );
            assert!(texts.iter().all(|t| !t.contains("Page line")), "{texts:?}");
        }
        gate.open();
        common::wait_for_label(&mut app, "Line Aster / Clara");
        common::wait_for_label(&mut app, "Line Hidden / Clara");
        let y = |text| {
            app.output()
                .shapes
                .iter()
                .find_map(|s| text_y(&s.shape, text))
                .unwrap()
        };
        assert_eq!(y("Aster") < y("Baker"), sort);
        assert!(app.state().error().is_none());
    }
}
#[test]
fn a_parent_draft_refilters_rows_until_it_ends() {
    let (mut app, _) = open();
    common::wait_for_label(&mut app, "Line Aster / Clara");
    common::text_field(&app, "Given").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Nobody".into()));
    common::wait_until(&mut app, "rows filtered on the draft", |app| {
        app.query_by_label("Aster").is_none()
    });
    // Typing the stored value back ends the draft without a data reply.
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Clara".into()));
    common::wait_for_label(&mut app, "Line Aster / Clara");
    assert_eq!(common::record_number(&app), "1");
    assert!(app.state().error().is_none());
}

/// A click on a row's button while the portal's records are read again
/// (here for the parent's draft, as the user types) is the user's click on
/// that row: after the typing is saved it waits until the records are in,
/// then runs in the row of the record it was clicked on, rather than being
/// dropped (#781).
#[test]
fn a_row_click_while_its_portal_is_read_again_runs_in_that_row() {
    let hold = Gate::default();
    hold.open();
    let _release = Release(hold.clone());
    let held = Arc::new(AtomicUsize::new(0));
    let (mut app, _) = open_holding(
        Calcs {
            sort: false,
            row_script: true,
            ..SORTED_AND_FILTERED
        },
        None,
        Some((hold.clone(), held.clone())),
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    // From here the provider answers no calc data: the portal, read again
    // for the parent's draft, shows the rows it showed last.
    hold.close();
    common::text_field(&app, "Given").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Clarissa".into()));
    common::wait_until(&mut app, "the portal read again", |_| {
        held.load(Ordering::SeqCst) > 0
    });
    app.run_steps(3);
    // Aster's row (row 2): its button is the first one below its field.
    let aster = app
        .query_all_by_role_and_label(Role::TextInput, "Lines::Given")
        .find(|node| node.accesskit_node().value().unwrap_or_default() == "Aster")
        .unwrap()
        .rect()
        .center()
        .y;
    let button = app
        .query_all_by_role_and_label(Role::Button, "ROW DIALOG")
        .filter(|n| n.rect().center().y > aster)
        .min_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .unwrap();
    // The click leaves the field: its typing is saved first.
    button.click();
    app.run_steps(10);
    assert!(
        app.query_by_label_contains("row ").is_none(),
        "ran before the portal's records were in"
    );
    hold.open();
    common::wait_until(&mut app, "the row's dialog", |app| {
        app.query_by_label_contains("row ").is_some()
    });
    app.run_steps(3);
    let shown: Vec<String> = app
        .query_all(egui_kittest::kittest::By::new().label_contains("row "))
        .map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value()).unwrap_or_default()
        })
        .collect();
    assert_eq!(shown, ["row 2 Aster"]);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A click on a row's field while the portal's records are read again is
/// the user's click on that record's field: after the typing it left is
/// saved, the field takes the focus in that record's row once the records
/// are in, rather than the click being dropped (#1099).
#[test]
fn a_field_click_while_its_portal_is_read_again_focuses_that_records_field() {
    let hold = Gate::default();
    hold.open();
    let _release = Release(hold.clone());
    let held = Arc::new(AtomicUsize::new(0));
    let (mut app, _) = open_holding(
        Calcs {
            sort: false,
            row_script: true,
            ..SORTED_AND_FILTERED
        },
        None,
        Some((hold.clone(), held.clone())),
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    let editors = || {
        egui_kittest::kittest::By::new()
            .role(Role::TextInput)
            .label("Lines::Given")
    };
    let value = |node: &egui_kittest::Node<'_>| node.accesskit_node().value().unwrap_or_default();
    // From here the provider answers no calc data: the portal, read again
    // for the parent's draft of its match field, shows the rows it showed
    // last. The draft relates the same lines (a text key matches whatever
    // its case); a draft only the portal's filter reads would be filtered
    // at once from the rows already read, with nothing read again (#1308).
    hold.close();
    common::text_field(&app, "Given").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("clara".into()));
    common::wait_until(&mut app, "the portal read again", |_| {
        held.load(Ordering::SeqCst) > 0
    });
    app.run_steps(3);
    app.query_all(editors())
        .find(|node| value(node) == "Aster")
        .unwrap()
        .click();
    app.run_steps(10);
    assert!(
        app.query_all(editors()).all(|node| !node.is_focused()),
        "a row field took the focus before the portal's records were in"
    );
    hold.open();
    common::wait_until(&mut app, "the clicked field's focus", |app| {
        app.query_all(editors()).any(|node| node.is_focused())
    });
    let focused: Vec<String> = app
        .query_all(editors())
        .filter(|node| node.is_focused())
        .map(|node| value(&node))
        .collect();
    assert_eq!(focused, ["Aster"]);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A click on a portal row's pop-up menu while the portal's records are
/// read again is not dropped, as a click on a row's edit box is not
/// (#1099): once the records are in, that record's pop-up opens its list,
/// with no second click (#288).
#[test]
fn a_pop_up_click_while_its_portal_is_read_again_opens_its_list_once_read() {
    let hold = Gate::default();
    hold.open();
    let _release = Release(hold.clone());
    let held = Arc::new(AtomicUsize::new(0));
    let (mut app, _) = open_holding(
        Calcs {
            control: 2,
            sort: false,
            row_script: true,
            ..SORTED_AND_FILTERED
        },
        None,
        Some((hold.clone(), held.clone())),
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    let menus = || {
        egui_kittest::kittest::By::new()
            .role(Role::ComboBox)
            .label("Lines::Given")
    };
    let value = |node: &egui_kittest::Node<'_>| node.accesskit_node().value().unwrap_or_default();
    // The portal is read again for the parent's draft of its match field,
    // and shows the rows it showed last until the provider answers.
    hold.close();
    common::text_field(&app, "Given").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("clara".into()));
    common::wait_until(&mut app, "the portal read again", |_| {
        held.load(Ordering::SeqCst) > 0
    });
    app.run_steps(3);
    app.query_all(menus())
        .find(|node| value(node) == "Aster")
        .unwrap()
        .click();
    app.run_steps(10);
    assert!(
        app.query_by_role_and_label(Role::Button, "Cora").is_none(),
        "the list opened before the portal's records were in"
    );
    hold.open();
    common::wait_for_label(&mut app, "Cora");
    app.get_by_role_and_label(Role::Button, "Cora").click();
    common::wait_until(&mut app, "the chosen item", |app| {
        app.query_all(menus()).any(|node| value(&node) == "Cora")
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A pop-up menu in a portal row (#537) offers its list in that row, and
/// the item chosen is the row's related record's edit, kept with the
/// record the layout shows: the row shows it and the record has unsaved
/// changes. The parent's own field is not touched. In the creation row
/// the item chosen begins a related record, as typing does (#585).
#[test]
fn a_pop_up_menu_in_a_portal_row_edits_its_related_record() {
    let (mut app, _) = open_with(
        Calcs {
            control: 2,
            ..SORTED_AND_FILTERED
        },
        None,
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    let menus = |app: &common::App| {
        app.query_all(
            egui_kittest::kittest::By::new()
                .role(Role::ComboBox)
                .label("Lines::Given"),
        )
        .map(|node| node.accesskit_node().value().unwrap_or_default().to_owned())
        .collect::<Vec<_>>()
    };
    // Two rows and the creation row.
    assert_eq!(menus(&app), ["Aster", "Baker", ""]);
    app.query_all(
        egui_kittest::kittest::By::new()
            .role(Role::ComboBox)
            .label("Lines::Given"),
    )
    .next()
    .unwrap()
    .click();
    common::wait_for_label(&mut app, "Cora");
    app.get_by_role_and_label(Role::Button, "Cora").click();
    common::wait_until(&mut app, "the chosen item", |app| {
        menus(app).contains(&"Cora".to_owned()) && !menus(app).contains(&"Aster".to_owned())
    });
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    assert_eq!(
        common::text_field(&app, "Given")
            .accesskit_node()
            .value()
            .unwrap_or_default(),
        "Clara"
    );
    app.query_all(
        egui_kittest::kittest::By::new()
            .role(Role::ComboBox)
            .label("Lines::Given"),
    )
    .nth(2)
    .unwrap()
    .click();
    common::wait_for_label(&mut app, "Aster");
    app.get_by_role_and_label(Role::Button, "Aster").click();
    // The sorted portal shows the edited row by its edit; the begun
    // record's row follows the saved ones.
    common::wait_until(&mut app, "the begun record's row", |app| {
        menus(app) == ["Baker", "Cora", "Aster"]
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A related control's hide condition holds in the creation row as in a
/// saved row: one hidden there offers nothing to choose (#591 review).
#[test]
fn a_hidden_pop_up_menu_offers_nothing_in_the_creation_row() {
    let (mut app, _) = open_with(
        Calcs {
            control: 2,
            hide_empty: true,
            ..SORTED_AND_FILTERED
        },
        None,
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    app.run_steps(3);
    let menus: Vec<String> = app
        .query_all(
            egui_kittest::kittest::By::new()
                .role(Role::ComboBox)
                .label("Lines::Given"),
        )
        .map(|node| node.accesskit_node().value().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(menus, ["Aster", "Baker"]);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A drop-down list in a portal row (#537): a click on its arrow enters
/// the row's field and opens the list there; the item chosen replaces the
/// related record's edit, kept with the record.
#[test]
fn a_drop_down_list_in_a_portal_row_edits_its_related_record() {
    let (mut app, _) = open_with(
        Calcs {
            control: 1,
            ..SORTED_AND_FILTERED
        },
        None,
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    let editors = |app: &common::App| {
        app.query_all(
            egui_kittest::kittest::By::new()
                .role(Role::TextInput)
                .label("Lines::Given"),
        )
        .map(|node| node.accesskit_node().value().unwrap_or_default().to_owned())
        .collect::<Vec<_>>()
    };
    // Two rows and the creation row.
    assert_eq!(editors(&app), ["Aster", "Baker", ""]);
    assert!(app.query_by_label("Cora").is_none());
    // The first row's arrow, at the field's right edge.
    let field = app
        .query_all(
            egui_kittest::kittest::By::new()
                .role(Role::TextInput)
                .label("Lines::Given"),
        )
        .next()
        .unwrap()
        .rect();
    let arrow = egui::pos2(field.right() - 8.0, field.center().y);
    app.hover_at(arrow);
    app.run_steps(2);
    // The own-table refusal of portals is not the related list's help
    // (the walkthrough's macOS accessibility read it, #536).
    let help = app
        .query_all(
            egui_kittest::kittest::By::new()
                .role(Role::TextInput)
                .label("Lines::Given"),
        )
        .next()
        .unwrap()
        .accesskit_node()
        .description();
    assert!(
        help.as_deref().is_none_or(|help| !help.contains("portals")),
        "{help:?}"
    );
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos: arrow,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    app.run_steps(2);
    common::wait_for_label(&mut app, "Cora");
    app.get_by_role_and_label(Role::Button, "Cora").click();
    common::wait_until(&mut app, "the chosen item", |app| {
        editors(app).contains(&"Cora".to_owned())
    });
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    assert!(
        !editors(&app).contains(&"Aster".to_owned()),
        "{:?}",
        editors(&app)
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A window whose portal reads its records again has not settled: until
/// they are in, its rows, their editors and its creation row are the ones
/// it showed last, which the records as read may not show (#1113).
#[test]
fn a_window_whose_portal_reads_its_records_again_has_not_settled() {
    let hold = Gate::default();
    hold.open();
    let _release = Release(hold.clone());
    let held = Arc::new(AtomicUsize::new(0));
    let (mut app, _) = open_holding(
        SORTED_AND_FILTERED,
        None,
        Some((hold.clone(), held.clone())),
    );
    common::wait_for_label(&mut app, "Line Aster / Clara");
    common::wait_until(&mut app, "the window settled", |app| {
        app.state().is_settled()
    });
    // From here the provider answers no calc data; a parent draft that
    // the portal's filter refuses makes it read its records again.
    hold.close();
    common::text_field(&app, "Given").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Nobody".into()));
    common::wait_until(&mut app, "the portal read again", |_| {
        held.load(Ordering::SeqCst) > 0
    });
    for _ in 0..10 {
        app.step();
        assert!(
            !app.state().is_settled(),
            "settled while its portal reads its records"
        );
        // Meanwhile the portal shows the rows it showed last.
        assert!(app.query_by_label("Aster").is_some());
    }
    hold.open();
    common::wait_until(&mut app, "the window settled", |app| {
        app.state().is_settled()
    });
    // Settled, it shows the records as read for the draft: none.
    assert!(app.query_by_label("Aster").is_none());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Readiness waits for reads on their way, never for one that failed: a
/// window whose portal's related records cannot be read still settles.
#[test]
fn a_window_whose_portals_records_cannot_be_read_settles() {
    let (mut app, _) = open_with(
        Calcs {
            fail_related: true,
            ..SORTED_AND_FILTERED
        },
        None,
    );
    common::wait_until(&mut app, "the window settled", |app| {
        app.state().is_settled()
    });
}
