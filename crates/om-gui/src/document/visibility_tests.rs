//! Hide conditions decide what Browse paints and what takes a click
//! (calc.rs, `Visibility`): a button FileMaker would show runs its action, a
//! hidden one is not there, one OpenMaker cannot decide is a disabled
//! stand-in, and one whose calculations are still on their way is withheld.
//! A result that reads the window (`Get`) follows the current record.

use std::sync::Arc;

use egui::epaint::Shape;
use egui::{pos2, Pos2};
use om_data::calc::eval::Variables;
use om_data::calc::{Locale, Value, ValueType};
use om_data::formula::{self, Formula};
use om_data::host::{CalcSchema, FieldDef, Privileges, RecordValues, SessionInfo, WindowMode};
use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage, RenderLayout};
use om_layout::{ButtonObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part, ScriptStep};

use super::{show, DocumentState};
use crate::app::{Action, ViewMode};
use crate::calc::{Calculations, Evaluator};
use crate::session::{RenderState, Session, Target};

const TABLE: u64 = 0x8001;
const OCCURRENCE: u64 = 0xd00001;
/// `occurrence_id(LAYOUT_KEY) == OCCURRENCE`.
const LAYOUT_KEY: u64 = 1065089;
/// Button ids: shown, hidden, undecidable, shown on the current record.
const ALWAYS: u64 = 1;
const HIDDEN: u64 = 2;
const ADMIN: u64 = 3;
const CURRENT: u64 = 4;
/// Header and body heights; every button sits in the body.
const HEADER: f64 = 20.0;
const BODY: f64 = 60.0;

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Visibility fixture", Locale::en_us());
    schema.add_table(TABLE, "Jobs");
    schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
    schema.add_field(TABLE, FieldDef::data(1, "Row", ValueType::Number));
    schema.add_layout(1, "Visibility fixture", Some(OCCURRENCE));
    schema
}

fn formula(schema: &CalcSchema, source: &str) -> Formula {
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    Formula {
        context: None,
        formatted: None,
        polish: formula::compile(source, &context).unwrap().expr,
    }
}

/// A button with an action at column `column` of the body.
fn button(
    schema: &CalcSchema,
    id: u64,
    column: f64,
    label: &str,
    hide: Option<&str>,
) -> LayoutObject {
    let left = 10.0 + column * 110.0;
    let top = HEADER + 10.0;
    let mut object = LayoutObject {
        id,
        object_type: 10,
        name: Some(label.into()),
        text: Some(label.into()),
        bounds: Some(om_layout::Rect {
            left,
            top,
            right: left + 100.0,
            bottom: top + 30.0,
            ..Default::default()
        }),
        button: Some(ButtonObject {
            script_step: Some(ScriptStep {
                id: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    object.calcs.hide = hide.map(|source| formula(schema, source));
    object
}

fn document(schema: &CalcSchema) -> RenderLayout {
    let objects = vec![
        button(schema, ALWAYS, 0.0, "Always", None),
        button(schema, HIDDEN, 1.0, "Hidden", Some("Jobs::Row > 0")),
        button(
            schema,
            ADMIN,
            2.0,
            "Admin",
            Some("Get ( AccountPrivilegeSetName ) ≠ \"[Full Access]\""),
        ),
        button(
            schema,
            CURRENT,
            3.0,
            "Current",
            Some("Get ( ActiveRecordNumber ) ≠ Jobs::Row"),
        ),
    ];
    let part = |id, part_type, top, height| Part {
        id,
        part_type,
        top,
        height,
        ..Default::default()
    };
    RenderLayout {
        layout: Layout {
            id: 1,
            name: "Visibility fixture".into(),
            def: Some(LayoutDef {
                base_width: 480.0,
                table_alias_key: LAYOUT_KEY,
                ..Default::default()
            }),
            parts: vec![part(1, 1, 0.0, HEADER), part(2, 4, HEADER, BODY)],
            objects: ObjectStore {
                root: objects.iter().map(|object| object.id).collect(),
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: Default::default(),
    }
}

/// Two records, Row 1 and Row 2, with their stored values cached.
fn session_and_calculations(schema: CalcSchema) -> (Session, Calculations) {
    let document = document(&schema);
    let mut calcs = Calculations::default();
    calcs.set_schema(Some(Arc::new(schema)));
    for record in [1, 2] {
        let mut row = RecordValues::default();
        row.set(1, 1, Value::number(record as i64));
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, record), Some(Arc::new(row)));
    }
    let mut session = Session::new(
        Target {
            local: None,
            host: "localhost".into(),
            file: "Visibility fixture".into(),
        },
        25,
    );
    let key = session
        .set_layouts(vec![LayoutInfo {
            id: 1,
            name: "Visibility fixture".into(),
        }])
        .unwrap();
    session
        .accept_page(
            key,
            Ok(RecordPage {
                versions: Default::default(),
                fields: vec![Field {
                    name: "Row".into(),
                    kind: FieldKind::Number,
                }],
                records: [1, 2]
                    .map(|id| Record {
                        id,
                        values: vec![Some(id.to_string())],
                    })
                    .into(),
                total: 2,
            }),
        )
        .unwrap();
    session.render = RenderState::Loaded(Box::new(document));
    (session, calcs)
}

/// The window's `Get` answers, with the account's set when it is known.
fn info(session: &Session, calcs: &Calculations, set: Option<&str>) -> SessionInfo {
    let mut info = crate::calc::session_info(
        session,
        calcs.schema().map(|schema| schema.as_ref()),
        "Admin",
        WindowMode::Browse,
    );
    info.privileges = set.map(|name| Privileges {
        account_set_name: name.into(),
        ..Default::default()
    });
    info.privilege_set_name = set.unwrap_or_default().into();
    info
}

struct Frame {
    actions: Vec<Action>,
    /// The text painted, and where.
    texts: Vec<(String, egui::Rect)>,
}

impl Frame {
    fn shows(&self, text: &str) -> Vec<egui::Rect> {
        self.texts
            .iter()
            .filter(|(painted, _)| painted == text)
            .map(|(_, rect)| *rect)
            .collect()
    }
}

fn texts(shape: &Shape, out: &mut Vec<(String, egui::Rect)>) {
    match shape {
        Shape::Text(text) => out.push((text.galley.job.text.clone(), text.visual_bounding_rect())),
        Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
        _ => {}
    }
}

fn frame(
    ctx: &egui::Context,
    session: &Session,
    calcs: &Calculations,
    state: &mut DocumentState,
    view: ViewMode,
    info: &SessionInfo,
    events: Vec<egui::Event>,
) -> Frame {
    let RenderState::Loaded(document) = &session.render else {
        panic!("no layout");
    };
    let evaluator = Evaluator::new(calcs, session, state, info.clone(), &Variables::new());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(640.0, 400.0),
        )),
        events,
        ..Default::default()
    };
    let mut actions = Vec::new();
    let mut output = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                show(
                    ui,
                    session,
                    document,
                    state,
                    view,
                    evaluator.as_ref(),
                    &mut actions,
                );
            });
    });
    let mut painted = Vec::new();
    for shape in std::mem::take(&mut output.shapes) {
        texts(&shape.shape, &mut painted);
    }
    output.drop_without_applying_deltas();
    Frame {
        actions,
        texts: painted,
    }
}

/// The actions of a click at `at`: move there, press, release.
#[allow(clippy::too_many_arguments)]
fn click(
    ctx: &egui::Context,
    session: &Session,
    calcs: &Calculations,
    state: &mut DocumentState,
    view: ViewMode,
    info: &SessionInfo,
    at: Pos2,
) -> Vec<Action> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut actions = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(at)],
        vec![button(true)],
        vec![button(false)],
        vec![],
    ] {
        actions.extend(frame(ctx, session, calcs, state, view, info, events).actions);
    }
    actions
}

/// The middle of button `column` of row `row` (0 in Form view).
fn at(column: f64, row: f64) -> Pos2 {
    pos2(
        (10.0 + column * 110.0 + 50.0) as f32,
        (HEADER + row * BODY + 10.0 + 15.0) as f32,
    )
}

fn run(object_id: u64, record_index: u64) -> Action {
    Action::RunButton {
        object_id,
        record_index,
        portal: None,
    }
}

#[test]
fn hidden_and_undetermined_buttons_take_no_click_and_shown_ones_run() {
    let (session, calcs) = session_and_calculations(schema());
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    // The account's privilege set was not read: whether FileMaker shows
    // Admin is unknown here.
    let unknown = info(&session, &calcs, None);
    let painted = frame(
        &ctx,
        &session,
        &calcs,
        &mut state,
        ViewMode::Form,
        &unknown,
        vec![],
    );
    assert_eq!(painted.shows("Always").len(), 1);
    assert_eq!(painted.shows("Current").len(), 1);
    for absent in ["Hidden", "Admin"] {
        assert!(painted.shows(absent).is_empty(), "{absent}");
    }
    let clicked = |state: &mut DocumentState, info: &SessionInfo, column| {
        click(
            &ctx,
            &session,
            &calcs,
            state,
            ViewMode::Form,
            info,
            at(column, 0.0),
        )
    };
    assert_eq!(clicked(&mut state, &unknown, 0.0), [run(ALWAYS, 0)]);
    assert_eq!(clicked(&mut state, &unknown, 1.0), []);
    // The stand-in keeps the click from the canvas, and runs nothing.
    assert_eq!(clicked(&mut state, &unknown, 2.0), []);
    assert_eq!(clicked(&mut state, &unknown, 3.0), [run(CURRENT, 0)]);

    // FileMaker's answers for a known account decide it either way.
    let admin = info(&session, &calcs, Some("[Full Access]"));
    assert_eq!(clicked(&mut state, &admin, 2.0), [run(ADMIN, 0)]);
    let viewer = info(&session, &calcs, Some("[Read-Only Access]"));
    assert_eq!(clicked(&mut state, &viewer, 2.0), []);
}

#[test]
fn a_button_waiting_for_its_calculations_is_withheld_until_they_run() {
    let (session, mut calcs) = session_and_calculations(schema());
    let schema = calcs.schema().cloned();
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    let admin = info(&session, &calcs, Some("[Full Access]"));
    let form = |calcs: &Calculations, state: &mut DocumentState, column| {
        click(
            &ctx,
            &session,
            calcs,
            state,
            ViewMode::Form,
            &admin,
            at(column, 0.0),
        )
    };
    // The schema is still being read: no evaluator yet.
    calcs.set_schema(None);
    assert_eq!(form(&calcs, &mut state, 1.0), []);
    assert_eq!(form(&calcs, &mut state, 0.0), [run(ALWAYS, 0)]);
    // It arrives, without the records' values: a condition on the account
    // decides, one on the record is withheld and its data asked for.
    calcs.set_schema(schema);
    assert_eq!(form(&calcs, &mut state, 2.0), [run(ADMIN, 0)]);
    assert_eq!(form(&calcs, &mut state, 3.0), []);
    assert!(!calcs.take_wanted().is_empty());
    // Without any calculations, a stand-in shows where each is.
    calcs.schema_unavailable();
    state.calculations_unavailable = true;
    let painted = frame(
        &ctx,
        &session,
        &calcs,
        &mut state,
        ViewMode::Form,
        &admin,
        vec![],
    );
    let names: Vec<_> = painted
        .texts
        .iter()
        .map(|(text, _)| text.as_str())
        .collect();
    assert_eq!(names, ["Always"]);
    assert_eq!(form(&calcs, &mut state, 1.0), []);
}

#[test]
fn a_button_shown_on_the_current_record_follows_it_from_row_to_row() {
    let (mut session, calcs) = session_and_calculations(schema());
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    // The List view's button on each row whose record is the current one,
    // where it is painted after the view settles on that record.
    let current_button = |session: &Session, state: &mut DocumentState| {
        let info = info(session, &calcs, Some("[Full Access]"));
        let mut shown = Vec::new();
        for _ in 0..3 {
            shown =
                frame(&ctx, session, &calcs, state, ViewMode::List, &info, vec![]).shows("Current");
        }
        shown
            .iter()
            .map(|rect| {
                click(
                    &ctx,
                    session,
                    &calcs,
                    state,
                    ViewMode::List,
                    &info,
                    rect.center(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        current_button(&session, &mut state),
        [vec![run(CURRENT, 0)]]
    );
    // The second record becomes current within the same page: no data
    // changes, but Get ( ActiveRecordNumber ) does.
    session.go_to(1);
    assert_eq!(
        current_button(&session, &mut state),
        [vec![run(CURRENT, 1)]]
    );
}

#[test]
fn account_deadline_visibility_refreshes_the_same_cached_object_after_midnight() {
    use om_data::calc::{Date, Time, TimeStyle, Timestamp};
    let mut schema = schema();
    let day = Date::from_ymd(2026, 11, 2).unwrap();
    schema.authenticated_account = om_data::host::AccountInfo {
        account_type: Some("FileMaker File".into()),
        password_expiry: Some(day.day_number() + 1),
        set_access: None,
    };
    let (mut session, calcs) = session_and_calculations(schema);
    let RenderState::Loaded(ref mut layout) = session.render else {
        panic!()
    };
    let object = layout
        .layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == ADMIN)
        .unwrap();
    object.calcs.hide = Some(formula(
        calcs.schema().unwrap(),
        "Get ( AccountPasswordDaysRemaining ) = 0",
    ));
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    let mut first = info(&session, &calcs, Some("[Full Access]"));
    let midnight = Time::from_micros(0, TimeStyle::Elapsed);
    first.now = Timestamp::from_date_time(day, midnight);
    assert_eq!(
        frame(
            &ctx,
            &session,
            &calcs,
            &mut state,
            ViewMode::Form,
            &first,
            vec![]
        )
        .shows("Admin")
        .len(),
        1
    );
    let mut next = first.clone();
    next.now = Timestamp::from_date_time(Date::from_ymd(2026, 11, 3).unwrap(), midnight);
    assert_eq!(
        frame(
            &ctx,
            &session,
            &calcs,
            &mut state,
            ViewMode::Form,
            &next,
            vec![]
        )
        .shows("Admin")
        .len(),
        0,
        "deadline day must refresh cached hide"
    );
}

/// Each List row in sight shows its web viewer as a view of its own, keyed by
/// its record and calculated on it, whichever row is current; a row out of
/// sight has none.
#[test]
fn each_list_row_has_its_own_web_viewer_on_its_records_address() {
    let schema = schema();
    let address = formula(&schema, "\"data:text/plain,Row\" & Jobs::Row");
    let (mut session, calcs) = session_and_calculations(schema);
    let RenderState::Loaded(document) = &mut session.render else {
        panic!("no layout")
    };
    document.layout.objects.root.push(5);
    document.layout.objects.objects.push(LayoutObject {
        id: 5,
        object_type: 13,
        name: Some("Website".into()),
        bounds: Some(om_layout::Rect {
            left: 10.0,
            top: HEADER + 40.0,
            right: 300.0,
            bottom: HEADER + 58.0,
            ..Default::default()
        }),
        external: Some(om_layout::ExternalObject {
            type_code: om_layout::ExternalObject::WEB_VIEWER,
            ..Default::default()
        }),
        calcs: om_layout::calcs::ObjectCalcs {
            web_address: Some(address),
            ..Default::default()
        },
        ..Default::default()
    });
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    for current in [0, 1, 0] {
        session.current = current;
        state.begin_frame();
        let info = info(&session, &calcs, Some("[Full Access]"));
        let painted = frame(
            &ctx,
            &session,
            &calcs,
            &mut state,
            ViewMode::List,
            &info,
            vec![],
        );
        // Every row in sight has its own placement, on its own record (ids
        // 1 and 2 hold Row 1 and Row 2), and only the current row's takes
        // commands. The List scrolls the current row to the top, which
        // takes the first row out of sight when the second is current.
        let mut rows: Vec<_> = state
            .web_viewers
            .iter()
            .map(|p| (p.record, p.address.clone(), p.current))
            .collect();
        rows.sort();
        let row =
            |id: i64, current: bool| (Some(id), Some(format!("data:text/plain,Row{id}")), current);
        let expected = match current {
            0 => vec![row(1, true), row(2, false)],
            _ => vec![row(2, true)],
        };
        assert_eq!(rows, expected, "current {current}");
        assert!(state
            .web_viewers
            .iter()
            .all(|p| p.object == 5 && !p.rect.intersect(p.visible).is_negative()));
        assert!(
            painted
                .texts
                .iter()
                .all(|(t, _)| !t.starts_with("Web viewer:")),
            "no note for any row, current {current}"
        );
    }
    // A build/platform refusal shows on every row instead.
    state.web_note = Some("Web viewer: this OpenMaker build has no web viewer support.");
    state.begin_frame();
    let info = info(&session, &calcs, Some("[Full Access]"));
    let painted = frame(
        &ctx,
        &session,
        &calcs,
        &mut state,
        ViewMode::List,
        &info,
        vec![],
    );
    assert!(state.web_viewers.is_empty());
    assert_eq!(
        painted
            .shows("Web viewer: this OpenMaker build has no web viewer support.")
            .len(),
        2,
        "every row in sight says so"
    );
}

/// A web viewer stored inside a portal shows once, over the portal at its
/// own place, on the layout's record, not in each of the portal's rows:
/// Claris, "Working with web viewers on layouts": "You can't place a web
/// viewer in a portal. If you place a web viewer in a portal, the web viewer
/// appears as an object on the layout that overlaps the portal."
#[test]
fn a_web_viewer_in_a_portal_shows_once_on_the_layouts_record() {
    let schema = schema();
    let address = formula(&schema, "\"data:text/plain,Row\" & Jobs::Row");
    let (mut session, calcs) = session_and_calculations(schema);
    let RenderState::Loaded(document) = &mut session.render else {
        panic!("no layout")
    };
    let bounds = |left, top, right, bottom| {
        Some(om_layout::Rect {
            left,
            top,
            right,
            bottom,
            ..Default::default()
        })
    };
    document.layout.objects.root.push(6);
    document.layout.objects.objects.push(LayoutObject {
        id: 6,
        object_type: 9,
        bounds: bounds(5.0, HEADER + 2.0, 400.0, HEADER + 56.0),
        portal: Some(om_layout::PortalObject {
            num_rows: 3.0,
            initial_row: 1,
            ..Default::default()
        }),
        children: vec![5, 7, 9],
        ..Default::default()
    });
    // A closed popover's viewer, and the inactive panel's, are not shown;
    // the active panel's is (the first panel without a stored default).
    let container = |id, object_type, parent, top: f64, children: Vec<u64>| LayoutObject {
        id,
        object_type,
        parent: Some(parent),
        bounds: bounds(0.0, top, 200.0, top + 30.0),
        children,
        ..Default::default()
    };
    document.layout.objects.objects.extend([
        container(7, 15, 6, 0.0, vec![8]),
        container(9, 11, 6, 20.0, vec![10, 11]),
        container(10, 12, 9, 0.0, vec![13]),
        container(11, 12, 9, 0.0, vec![12]),
    ]);
    let viewer = LayoutObject {
        id: 5,
        object_type: 13,
        parent: Some(6),
        name: Some("Website".into()),
        // Relative to the portal, as children's bounds are: (10, 24) to
        // (300, 36) on the layout, its first row's place.
        bounds: bounds(5.0, 2.0, 295.0, 14.0),
        external: Some(om_layout::ExternalObject {
            type_code: om_layout::ExternalObject::WEB_VIEWER,
            ..Default::default()
        }),
        calcs: om_layout::calcs::ObjectCalcs {
            web_address: Some(address),
            ..Default::default()
        },
        ..Default::default()
    };
    document.layout.objects.objects.push(viewer.clone());
    for (id, parent) in [(8, 7), (12, 11), (13, 10)] {
        document.layout.objects.objects.push(LayoutObject {
            id,
            parent: Some(parent),
            bounds: bounds(5.0, 2.0, 105.0, 12.0),
            ..viewer.clone()
        });
    }
    let ctx = egui::Context::default();
    let mut state = DocumentState::default();
    state.begin_frame();
    let info = info(&session, &calcs, Some("[Full Access]"));
    let painted = frame(
        &ctx,
        &session,
        &calcs,
        &mut state,
        ViewMode::Form,
        &info,
        vec![],
    );
    let placements: Vec<_> = state
        .web_viewers
        .iter()
        .map(|p| (p.object, p.record, p.current, p.address.clone(), p.rect))
        .collect();
    assert_eq!(
        placements,
        [
            (
                5,
                None,
                true,
                Some("data:text/plain,Row1".to_owned()),
                egui::Rect::from_min_max(pos2(10.0, 24.0), pos2(300.0, 36.0)),
            ),
            // Portal (5, 22), tab control 20 below it, viewer (5, 2) in.
            (
                13,
                None,
                true,
                Some("data:text/plain,Row1".to_owned()),
                egui::Rect::from_min_max(pos2(10.0, 44.0), pos2(110.0, 54.0)),
            ),
        ]
    );
    assert!(
        painted
            .texts
            .iter()
            .all(|(t, _)| !t.starts_with("Web viewer:")),
        "no note in any row"
    );
}
