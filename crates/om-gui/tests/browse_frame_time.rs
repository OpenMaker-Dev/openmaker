//! Frame time of Browse on a large synthetic layout: 1,200 objects of
//! several kinds, themed through type and named styles, with conditional
//! formats, data formatting and library images. A manual measurement:
//!
//! ```text
//! cargo test --release -p om-gui --test browse_frame_time -- --ignored --nocapture
//! ```
//!
//! It opens the file through the window like a user, then paints the open
//! window in a context of its own, without accessibility output, and
//! times the UI pass and its tessellation.
mod common;

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;

use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
use om_data::formula::{self, Formula};
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, MemorySource, RecordValues, Recorder,
};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage, RenderLayout,
};
use om_layout::style::{ComponentStyle, TypeStyle};
use om_layout::{
    ButtonObject, CfCondition, Color, ConditionalFormat, DataFormat, DateFormat, FieldObject,
    FieldSpec, GraphicObject, Layout, LayoutDef, LayoutObject, Length, LibraryImage,
    LibraryImageFormat, NumberFormat, ObjectStore, ObjectStyle, Part, Property, Rect, StyleSet,
    TextRun, ThemeStyles, TimeFormat,
};

const TABLE: u64 = 0x8001;
const OCCURRENCE: u64 = 0xd00001;
/// `occurrence_id(KEY) == OCCURRENCE`.
const KEY: u64 = 1065089;
const COLUMNS: u64 = 6;
const ROWS: u64 = 25;
/// Objects per card, see [`card`].
const CARD: u64 = 8;
const RECORDS: i64 = 50;
const PNG: u64 = 1;
const SVG: u64 = 2;
const FRAMES: usize = 200;

fn color(red: f32, green: f32, blue: f32) -> Color {
    Color {
        red,
        green,
        blue,
        alpha: 1.0,
    }
}

fn paint(id: i32, value: Color) -> Property {
    Property {
        id,
        color: Some(value),
        ..Default::default()
    }
}

fn points(id: i32, value: f64) -> Property {
    Property {
        id,
        length: Some(Length {
            value,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn int(id: i32, value: i32) -> Property {
    Property {
        id,
        int: Some(value),
        ..Default::default()
    }
}

fn styles(components: Vec<(i32, i32, Vec<Property>)>) -> StyleSet {
    StyleSet {
        components: components
            .into_iter()
            .map(|(component, state, properties)| ComponentStyle {
                component,
                state,
                properties,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn border(width: f64, value: Color) -> Vec<Property> {
    (0..4)
        .flat_map(|side| {
            [
                points(28 + side, width),
                int(24 + side, 3),
                paint(20 + side, value),
            ]
        })
        .collect()
}

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Frame time", Locale::en_us());
    schema.add_table(TABLE, "Jobs");
    schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
    schema.add_field(TABLE, FieldDef::data(1, "Name", ValueType::Text));
    schema.add_field(TABLE, FieldDef::data(2, "Status", ValueType::Text));
    schema.add_field(TABLE, FieldDef::data(3, "Amount", ValueType::Number));
    schema.add_layout(1, "Large", Some(OCCURRENCE));
    schema
}

fn record(id: i64) -> [String; 3] {
    [
        format!("Job {id}"),
        if id % 2 == 1 { "Open" } else { "Closed" }.into(),
        (id * 1234).to_string(),
    ]
}

/// A theme with a style for each object and part type and many named
/// styles, like a file's: resolving one property scans them.
fn theme() -> ThemeStyles {
    ThemeStyles {
        type_styles: (1..=18)
            .chain([20, 30, 31, 32, 34, 38])
            .map(|object_type| TypeStyle {
                object_types: vec![object_type],
                styles: Some(styles(vec![
                    (
                        1,
                        1,
                        [
                            vec![
                                Property {
                                    id: 64,
                                    string: Some("Helvetica".into()),
                                    ..Default::default()
                                },
                                points(69, 12.0),
                                paint(70, color(0.1, 0.1, 0.1)),
                                points(101, 2.0),
                                points(104, 3.0),
                            ],
                            border(0.5, color(0.7, 0.7, 0.7)),
                        ]
                        .concat(),
                    ),
                    (1, 2, vec![paint(7, color(0.9, 0.9, 1.0))]),
                    (1, 7, vec![paint(7, color(0.8, 0.9, 0.8))]),
                    (3, 18, vec![paint(70, color(0.6, 0.6, 0.6))]),
                    (4, 1, vec![paint(514, color(0.2, 0.4, 0.8))]),
                ])),
                ..Default::default()
            })
            .collect(),
        custom_styles: (0..80)
            .map(|index| TypeStyle {
                name: format!("Named {index}"),
                styles: Some(styles(vec![(
                    1,
                    1,
                    vec![paint(7, color(index as f32 / 80.0, 0.95, 0.95))],
                )])),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn object(id: u64, kind: u32, x: f64, y: f64, w: f64, h: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
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

fn field(object: LayoutObject, field_key: u64, display_type: u32) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: KEY,
                field_key,
                repetition: 1,
                ..Default::default()
            }),
            display_type,
            ..Default::default()
        }),
        ..object
    }
}

fn named(object: LayoutObject, index: u64) -> LayoutObject {
    LayoutObject {
        style: Some(ObjectStyle {
            custom_style_names: vec![format!("Named {}", index % 80)],
            ..Default::default()
        }),
        ..object
    }
}

/// One card of [`CARD`] objects: a framed box the record's status colors,
/// a merged title, three fields, an icon button, a picture and a rule.
fn card(index: u64, open: &ConditionalFormat) -> Vec<LayoutObject> {
    let id = 1 + index * CARD;
    let x = 10.0 + (index % COLUMNS) as f64 * 180.0;
    let y = 10.0 + (index / COLUMNS) as f64 * 120.0;
    let mut frame = LayoutObject {
        style: Some(ObjectStyle {
            local_styles: Some(styles(vec![(
                1,
                1,
                [vec![points(35, 6.0)], border(1.0, color(0.3, 0.3, 0.5))].concat(),
            )])),
            custom_style_names: vec![format!("Named {}", index % 80)],
            ..Default::default()
        }),
        ..object(id, 6, x, y, 170.0, 110.0)
    };
    frame.calcs.conditions.push(open.clone());
    let currency = DataFormat {
        font_height: 12,
        number: NumberFormat {
            flags: 0x080e,
            symbol: "$".into(),
            thousands_separator: ',',
            decimal_separator: '.',
            decimal_places: 2,
            ..Default::default()
        },
        date: DateFormat::default(),
        time: TimeFormat::default(),
        raw: Vec::new(),
    };
    vec![
        frame,
        LayoutObject {
            text: Some(format!("Card {index}: <<Name>>")),
            text_runs: vec![
                TextRun::Literal(format!("Card {index}: ")),
                TextRun::MergeField("Name".into()),
            ],
            ..named(object(id + 1, 2, x + 6.0, y + 4.0, 158.0, 18.0), index + 1)
        },
        field(object(id + 2, 1, x + 6.0, y + 24.0, 100.0, 20.0), 1, 0),
        field(object(id + 3, 1, x + 110.0, y + 24.0, 54.0, 20.0), 2, 1),
        LayoutObject {
            data_format: Some(currency),
            ..field(object(id + 4, 1, x + 6.0, y + 48.0, 100.0, 20.0), 3, 0)
        },
        LayoutObject {
            text: Some("Open".into()),
            button: Some(ButtonObject {
                library_key: SVG,
                icon_size: 14,
                ..Default::default()
            }),
            ..named(object(id + 5, 10, x + 6.0, y + 74.0, 80.0, 24.0), index + 2)
        },
        LayoutObject {
            graphic: Some(GraphicObject {
                library_key: PNG,
                ..Default::default()
            }),
            ..object(id + 6, 3, x + 120.0, y + 60.0, 40.0, 40.0)
        },
        object(id + 7, 4, x + 6.0, y + 70.0, 100.0, 0.0),
    ]
}

fn document() -> RenderLayout {
    let schema = schema();
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    let open = ConditionalFormat {
        formula: Some(Formula {
            context: None,
            formatted: None,
            polish: formula::compile("Jobs::Status = \"Open\"", &context)
                .unwrap()
                .expr,
        }),
        condition: CfCondition {
            style: Some(ObjectStyle {
                local_styles: Some(styles(vec![(1, 1, vec![paint(7, color(0.85, 1.0, 0.85))])])),
                ..Default::default()
            }),
            ..Default::default()
        },
        kind: None,
    };
    let objects: Vec<_> = (0..COLUMNS * ROWS)
        .flat_map(|index| card(index, &open))
        .collect();
    let mut svg =
        String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">"#);
    for i in 0..400 {
        svg.push_str(&format!(
            r#"<path class="fm_fill" d="M{} {}h2v2h-2z"/>"#,
            i % 32 * 2,
            i / 32 * 2
        ));
    }
    svg.push_str("</svg>");
    let png = {
        let image = image::RgbaImage::from_fn(256, 256, |x, y| {
            image::Rgba([x as u8, y as u8, (x ^ y) as u8, 255])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    };
    let mut field_bindings = BTreeMap::new();
    for index in 0..COLUMNS * ROWS {
        let id = 1 + index * CARD;
        field_bindings.insert(id + 2, "Name".into());
        field_bindings.insert(id + 3, "Status".into());
        field_bindings.insert(id + 4, "Amount".into());
    }
    RenderLayout {
        layout: Layout {
            id: 1,
            name: "Large".into(),
            def: Some(LayoutDef {
                base_width: 10.0 + COLUMNS as f64 * 180.0,
                table_alias_key: KEY,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 10.0 + ROWS as f64 * 120.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: objects.iter().map(|object| object.id).collect(),
                objects,
                ..Default::default()
            },
            theme_styles: theme(),
            library_images: BTreeMap::from([
                (
                    PNG,
                    LibraryImage {
                        format: LibraryImageFormat::Png,
                        data: png,
                    },
                ),
                (
                    SVG,
                    LibraryImage {
                        format: LibraryImageFormat::Svg,
                        data: svg.into_bytes(),
                    },
                ),
            ]),
            ..Default::default()
        },
        field_bindings,
    }
}

struct Provider;
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Large.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 1,
            name: "Large".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: [
                ("Name", FieldKind::Text),
                ("Status", FieldKind::Text),
                ("Amount", FieldKind::Number),
            ]
            .map(|(name, kind)| Field {
                name: name.into(),
                kind,
            })
            .into(),
            records: (1..=RECORDS)
                .skip(offset as usize)
                .take(limit as usize)
                .map(|id| Record {
                    id,
                    values: record(id).map(Some).into(),
                })
                .collect(),
            total: RECORDS as u64,
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
        let ids: Vec<u64> = (1..=RECORDS as u64).collect();
        for id in &ids {
            let [name, status, amount] = record(*id as i64);
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text(name));
            row.set(2, 1, Value::text(status));
            row.set(3, 1, Value::number(amount.parse().unwrap()));
            source.put_record(TABLE, *id, row);
        }
        source.set_found_set(OCCURRENCE, &ids, None);
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

fn milliseconds(times: &mut [Duration]) -> String {
    times.sort();
    let mean = times.iter().sum::<Duration>().as_secs_f64() / times.len() as f64;
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize].as_secs_f64();
    format!(
        "mean {:.2} ms, median {:.2} ms, p95 {:.2} ms",
        mean * 1e3,
        at(0.5) * 1e3,
        at(0.95) * 1e3
    )
}

#[test]
#[ignore = "manual measurement; run in release with --nocapture"]
fn browse_frame_time_on_a_large_layout() {
    let mut app = common::harness(Box::new(Provider));
    common::fill(&mut app, "Host", "large.example.invalid");
    common::fill(&mut app, "File", "Large.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "Card 0: Job 1");
    // Calculations have arrived once fields show their formatted values.
    common::wait_until(&mut app, "formatted values", |app| {
        app.query_all_by_label("$1,234.00").next().is_some()
    });
    assert!(app.state().is_settled());
    let ctx = egui::Context::default();
    om_gui::install_theme(&ctx);
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        ..Default::default()
    };
    let app = app.state_mut();
    let mut frame = || {
        let started = Instant::now();
        let mut output = ctx.run_ui(input(), |ui| app.show(ui));
        let ui = started.elapsed();
        let shapes = std::mem::take(&mut output.shapes);
        let primitives = ctx.tessellate(shapes, output.pixels_per_point);
        output.drop_without_applying_deltas();
        black_box(primitives);
        (ui, started.elapsed())
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    for _ in 0..20 {
        frame();
    }
    while ctx.has_pending_images() {
        assert!(Instant::now() < deadline, "images never loaded");
        frame();
        std::thread::sleep(Duration::from_millis(5));
    }
    frame();
    let (mut ui, mut total): (Vec<_>, Vec<_>) = (0..FRAMES).map(|_| frame()).unzip();
    eprintln!(
        "{} objects, {FRAMES} frames at 1600 x 1000 pt\n  UI pass: {}\n  UI pass and tessellation: {}",
        COLUMNS * ROWS * CARD,
        milliseconds(&mut ui),
        milliseconds(&mut total),
    );
}
