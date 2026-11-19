//! Actual application paints and keyboard input across a queued scroll batch.

use super::*;
use crate::scripting::{ScrollRequest, WindowState};
struct NoLicense;
impl crate::licensing::LicenseStore for NoLicense {
    fn load(&mut self) -> Result<Option<crate::licensing::LicenseKey>, om_data::DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &crate::licensing::LicenseKey) -> Result<(), om_data::DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), om_data::DataError> {
        Ok(())
    }
}
use crate::{
    app::ViewMode,
    calc::Calculations,
    document::DocumentState,
    session::{Geometry, RenderState, Session, Target},
};
use egui::{pos2, vec2, RawInput, Rect};
use om_data::calc::{Locale, Value, ValueType};
use om_data::formula::{self, Formula};
use om_data::host::{CalcData, CalcNeed, CalcSchema, FieldDef, RecordValues};
use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage};
use std::sync::Arc;
#[test]
fn scroll_window_keyboard_draft_change_remeasures_calculated_content() {
    let mut schema = CalcSchema::new("Synthetic", Locale::en_us());
    schema.add_table(10, "Data");
    schema.add_occurrence(20, "Data", 10);
    schema.add_layout(7, "Form", Some(20));
    for i in 1..=2 {
        schema.add_field(10, FieldDef::data(i, format!("Field {i}"), ValueType::Text));
    }
    let cx = formula::Context::new(schema.names());
    let code = Formula {
        context: None,
        formatted: None,
        polish: formula::compile("Data::Field 1", &cx).unwrap().expr,
    };
    schema.add_field(
        10,
        FieldDef::calculation(3, "Field 3", ValueType::Text, code, false),
    );
    let mut calcs = Calculations::default();
    calcs.set_schema(Some(Arc::new(schema)));
    let mut values = RecordValues::default();
    values.set(1, 1, Value::text("First"));
    values.set(2, 1, Value::text("Second"));
    let mut data = CalcData::default();
    data.records.insert((10, 1), Some(Arc::new(values)));
    assert!(calcs
        .accept_data(
            &[CalcNeed::Record {
                table: 10,
                record: 1
            }],
            Ok(data)
        )
        .is_none());
    let mut session = Session::new(
        Target {
            host: "draft.invalid".into(),
            file: "Synthetic".into(),
            local: None,
        },
        25,
    );
    let k = session
        .set_layouts(vec![LayoutInfo {
            id: 7,
            name: "Form".into(),
        }])
        .unwrap();
    session
        .accept_page(
            k,
            Ok(RecordPage {
                versions: Default::default(),
                fields: (1..=3)
                    .map(|i| Field {
                        name: format!("Field {i}"),
                        kind: FieldKind::Text,
                    })
                    .collect(),
                records: vec![Record {
                    id: 1,
                    values: vec![Some("First".into()), Some("Second".into()), None],
                }],
                total: 1,
            }),
        )
        .unwrap();
    session.geometry = Geometry::Unavailable;
    session.render = RenderState::Unavailable;
    let w = WindowState {
        layout: Some(7),
        ..Default::default()
    };
    let mut doc = DocumentState::default();
    doc.capabilities.update = true;
    doc.put_draft(7, 1, "Field 1", "First".into(), "short".into());
    doc.scroll.context((&w).into(), 100);
    let ctx = egui::Context::default();
    let mut app = OpenMakerApp::with_license_store(
        Box::new(om_data::mock::MockProvider::new()),
        Box::new(NoLicense),
    );
    app.w.session = Some(session);
    app.w.calcs = calcs;
    app.w.document = doc;
    app.w.view = ViewMode::Form;
    app.w.record_capabilities.update = true;
    app.w.show_toolbar = false;
    app.w.menubar = false;
    let paint = |app: &mut OpenMakerApp, text: Option<String>| {
        assert!(crate::calc::draft_overlay(
            app.w.calcs.schema().unwrap(),
            &app.w.calcs.cache.borrow(),
            app.w.session.as_ref().unwrap(),
            &app.w.document.changes(app.w.session.as_ref().unwrap()),
            20,
            om_data::host::session::today(0)
        )
        .is_some());
        ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 360.0))),
                events: text.map(|t| vec![egui::Event::Text(t)]).unwrap_or_default(),
                ..Default::default()
            },
            |ui| {
                app.draw_window_with(
                    ui,
                    true,
                    false,
                    Err(crate::web_viewer::Unavailable::Window),
                    crate::app::window_styles::Sharing::default(),
                )
            },
        )
        .drop_without_applying_deltas();
        let (y, h, v, _) = app.w.document.scroll.measured_motion(&ctx);
        (y, h, v)
    };
    paint(&mut app, None);
    let short = paint(&mut app, None);
    let mut selected = None;
    for repeats in 20..150 {
        let long = "late keyboard draft value ".repeat(repeats);
        app.w
            .document
            .put_draft(7, 1, "Field 1", "First".into(), long.clone());
        let x = paint(&mut app, None);
        if x.1 > 2.0 * x.2 + 45.0 && x.1 < 2.0 * x.2 + 75.0 {
            selected = Some((long, x));
            break;
        }
    }
    let (long, tall) =
        selected.expect("fixture must grow a visible calculated field to suitable height");
    app.w
        .document
        .put_draft(7, 1, "Field 1", "First".into(), "short".into());
    app.w.document.focus = Some("Field 1".into());
    app.w.document.select_all = true;
    paint(&mut app, None);
    paint(&mut app, None);
    assert_eq!(app.w.document.active.as_deref(), Some("Field 1"));
    let state = app.window_state();
    let scroll = [2, 3]
        .map(|choice| ScrollRequest {
            choice,
            context: (&state).into(),
            field: None,
            field_focus: false,
        })
        .into();
    app.apply_window(
        &ctx,
        crate::scripting::WindowUpdate {
            file: app.w.file_id,
            state,
            data_changed: false,
            refreshed: false,
            objects: Vec::new(),
            scroll,
            fronted: Vec::new(),
            originals: Default::default(),
            found_set: None,
            background: false,
        },
    );
    let measuring = paint(&mut app, Some(long.clone()));
    assert_eq!(app.w.document.scroll.measured_motion(&ctx).3, 2);
    assert_eq!(
        app.w
            .document
            .changes(app.w.session.as_ref().unwrap())
            .get("Field 1"),
        Some(&Some(long))
    );
    let after = paint(&mut app, None);
    let settled = paint(&mut app, None);
    let expected = (after.1 - 2.0 * after.2).max(0.0);
    println!("keyboard draft short={short:?} tall={tall:?} measuring={measuring:?} after={after:?} settled={settled:?} expected={expected}");
    assert_eq!(
        app.w.session.as_ref().unwrap().page.as_ref().unwrap().key,
        k
    );
    assert!((after.1 - tall.1).abs() < 1.0);
    assert!(expected > 15.0);
    assert!(
        (settled.0 - expected).abs() < 1.0,
        "changed real keyboard draft consumed stale calculation geometry"
    );
}
