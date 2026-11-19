//! Field data formatting in Browse, using only a synthetic layout and record:
//! field objects and a text object's merge field.
mod common;

use egui::epaint::Shape;
use om_data::calc::{Ctx, Date, Locale, Number, Value, ValueType};
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, MemorySource, RecordValues, Recorder,
};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordCapabilities, RecordPage, RenderLayout,
};
use om_layout::{
    DataFormat, DateFormat, FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject, NumberFormat,
    ObjectStore, Part, Rect, TextRun, TimeFormat,
};
use std::{collections::BTreeMap, sync::Arc};

/// The table occurrence and its layout key (`occurrence_id(KEY) == ITEMS`).
const ITEMS: u64 = 0xd00001;
const KEY: u64 = 1065089;
const TABLE: u64 = 0x8001;
const FIELDS: [(&str, FieldKind, ValueType, &str); 2] = [
    ("Amount", FieldKind::Number, ValueType::Number, "12345.5"),
    ("Due", FieldKind::Date, ValueType::Date, "4/1/2017"),
];

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Formats", Locale::en_us());
    s.add_table(TABLE, "Items");
    s.add_occurrence(ITEMS, "Items", TABLE);
    for (id, (name, _, kind, _)) in (1..).zip(FIELDS) {
        s.add_field(TABLE, FieldDef::data(id, name, kind));
    }
    s.add_layout(7, "Formats", Some(ITEMS));
    s
}

struct Provider;
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Formats.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Formats".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, _: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: FIELDS
                .map(|(name, kind, _, _)| Field {
                    name: name.into(),
                    kind,
                })
                .into(),
            records: (offset == 0)
                .then(|| Record {
                    id: 19,
                    values: FIELDS.map(|(.., value)| Some(value.into())).into(),
                })
                .into_iter()
                .collect(),
            total: 1,
        })
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn record_capabilities(&mut self, _: u32) -> Result<RecordCapabilities, DataError> {
        Ok(RecordCapabilities {
            create: false,
            update: true,
            delete: false,
            editable_fields: None,
            ..Default::default()
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
        let mut source = MemorySource::new();
        let mut row = RecordValues::default();
        row.set(1, 1, Value::Number(Number::parse_text("12345.5", '.', ',')));
        row.set(2, 1, Value::Date(Date::from_ymd(2017, 4, 1).unwrap()));
        source.put_record(TABLE, 19, row);
        source.set_found_set(ITEMS, &[19], None);
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

fn object(id: u64, kind: u32, x: f64, y: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        bounds: Some(Rect {
            left: x,
            top: y,
            right: x + 200.0,
            bottom: y + 24.0,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn field(id: u64, field_key: u64, y: f64, format: DataFormat) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: KEY,
                field_key,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        data_format: Some(format),
        ..object(id, 1, 20.0, y)
    }
}
fn format(number: NumberFormat, date: DateFormat) -> DataFormat {
    DataFormat {
        font_height: 12,
        number,
        date,
        time: TimeFormat::default(),
        raw: Vec::new(),
    }
}
fn document() -> RenderLayout {
    // Currency: a leading symbol, thousands separators and two decimals.
    let currency = NumberFormat {
        flags: 0x080e,
        symbol: "$".into(),
        thousands_separator: ',',
        decimal_separator: '.',
        decimal_places: 2,
        ..Default::default()
    };
    // A numeric date with a full year, the month and day zero-padded.
    let padded = DateFormat {
        flags: 7 << 24 | 1 << 8 | 1,
        separator: '/',
        ..Default::default()
    };
    let title = LayoutObject {
        text: Some("FORMATTED FIELDS".into()),
        ..object(1, 2, 20.0, 5.0)
    };
    // A text with a merge field and its own currency format, as the Assets
    // starter's Asset Details header shows <<Price>>: FileMaker formats
    // the merged value with the object's format.
    let merged = LayoutObject {
        text: Some("Total <<Amount>>".into()),
        text_runs: vec![
            TextRun::Literal("Total ".into()),
            TextRun::MergeField("Amount".into()),
        ],
        data_format: Some(format(currency.clone(), DateFormat::default())),
        ..object(4, 2, 300.0, 40.0)
    };
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Formats".into(),
            def: Some(LayoutDef {
                base_width: 600.0,
                table_alias_key: KEY,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 200.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![1, 2, 3, 4],
                objects: vec![
                    title,
                    field(2, 1, 40.0, format(currency, DateFormat::default())),
                    field(3, 2, 80.0, format(NumberFormat::default(), padded)),
                    merged,
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([(2, "Amount".into()), (3, "Due".into())]),
    }
}

/// Where the painted `text` shows the caret position `index`.
fn painted_at(shape: &Shape, text: &str, index: usize) -> Option<egui::Pos2> {
    match shape {
        Shape::Text(t) if t.galley.job.text == text => Some(
            t.pos
                + t.galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .center()
                    .to_vec2(),
        ),
        Shape::Vec(items) => items.iter().find_map(|s| painted_at(s, text, index)),
        _ => None,
    }
}
fn position(app: &common::App, text: &str, index: usize) -> Option<egui::Pos2> {
    app.output()
        .shapes
        .iter()
        .find_map(|s| painted_at(&s.shape, text, index))
}
fn paints(app: &common::App, text: &str) -> bool {
    position(app, text, 0).is_some()
}

#[test]
fn fields_show_their_format_and_edit_their_value() {
    let mut app = common::harness(Box::new(Provider));
    common::fill(&mut app, "Host", "formats.example.invalid");
    common::fill(&mut app, "File", "Formats.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "FORMATTED FIELDS");
    common::wait_until(&mut app, "the formatted amount", |app| {
        paints(app, "$12,345.50")
    });
    assert!(paints(&app, "04/01/2017"));
    assert!(!paints(&app, "12345.5"));
    assert!(paints(&app, "Total $12,345.50"));
    assert_eq!(
        common::text_field(&app, "Amount").value().as_deref(),
        Some("$12,345.50")
    );
    assert_eq!(
        common::text_field(&app, "Due").value().as_deref(),
        Some("04/01/2017")
    );
    // A real click between "$12" and ",345.50": the editor opens with the
    // value, the caret at its end, since no offset in the formatted text
    // addresses the value.
    let pos = position(&app, "$12,345.50", 3).unwrap();
    app.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    common::wait_until(&mut app, "the amount's editor", |app| {
        common::text_field(app, "Amount").is_focused()
    });
    assert_eq!(
        common::text_field(&app, "Amount").value().as_deref(),
        Some("12345.5")
    );
    app.event(egui::Event::Text("9".into()));
    app.run_steps(2);
    assert_eq!(
        common::text_field(&app, "Amount").value().as_deref(),
        Some("12345.59")
    );
    // The other field keeps its format while the amount is edited.
    assert!(paints(&app, "04/01/2017"));
    assert!(app.state().error().is_none());
}
