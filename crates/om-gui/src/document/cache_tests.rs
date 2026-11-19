//! Browse paints the same shapes with the layout cache as without it, for
//! a synthetic layout of every kind of object it draws, frame after frame,
//! while the record, its conditional formats, the window and zoom change.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::epaint::{ClippedShape, Shape};
use egui::{vec2, Color32, Vec2};
use om_data::calc::eval::Variables;
use om_data::calc::{Locale, Value, ValueType};
use om_data::formula::{self, Formula};
use om_data::host::{CalcSchema, FieldDef, RecordValues, RelationshipDef, SessionInfo};
use om_data::meta::{ValueList, ValueSource};
use om_data::{Field, FieldKind, LayoutInfo, Record, RecordPage, RenderLayout};
use om_layout::style::{ComponentStyle, TypeStyle};
use om_layout::{
    ButtonObject, CfCondition, CharacterStyle, Color, ConditionalFormat, DataFormat, DateFormat,
    FieldObject, FieldSpec, GraphicObject, GroupObject, Layout, LayoutDef, LayoutObject, Length,
    LibraryImage, LibraryImageFormat, NumberFormat, ObjectStore, ObjectStyle, Part,
    PopoverButtonObject, PopoverObject, PortalObject, Property, ScriptStep, StyleSet, StyledRun,
    StyledText, TabObject, TabPanelObject, TextRun, ThemeStyles, TimeFormat,
};

use super::{layout_cache::LayoutCache, show, DocumentState};
use crate::app::ViewMode;
use crate::calc::{Calculations, Evaluator};
use crate::session::{RenderState, Session, Target};

const TABLE: u64 = 0x8001;
const OCCURRENCE: u64 = 0xd00001;
/// `occurrence_id(LAYOUT_KEY) == OCCURRENCE`.
const LAYOUT_KEY: u64 = 1065089;
const ITEMS: u64 = 0x8002;
const ITEMS_OCCURRENCE: u64 = 0xd00002;
const ITEMS_KEY: u64 = 1065090;
const PNG: u64 = 100;
const SVG: u64 = 101;
const POPOVER: u64 = 16;
/// The record fields, by field id, and both records' values.
const FIELDS: [(u64, &str, FieldKind); 5] = [
    (1, "Name", FieldKind::Text),
    (2, "Status", FieldKind::Text),
    (3, "Amount", FieldKind::Number),
    (4, "Colors", FieldKind::Text),
    (5, "Note", FieldKind::Text),
];
const RECORDS: [(i64, [&str; 5]); 2] = [
    (1, ["Ada", "Open", "1234", "Red\rBlue", ""]),
    (2, ["Grace", "Closed", "99", "Green", ""]),
];

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

fn local(components: Vec<(i32, i32, Vec<Property>)>) -> ObjectStyle {
    ObjectStyle {
        local_styles: Some(styles(components)),
        ..Default::default()
    }
}

/// A frame of every side: width, style, color.
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

fn formula(schema: &CalcSchema, source: &str) -> Formula {
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    Formula {
        context: None,
        formatted: None,
        polish: formula::compile(source, &context).unwrap().expr,
    }
}

/// Holds on open records only: it toggles from one record to the next.
fn open_format(schema: &CalcSchema, properties: Vec<Property>) -> ConditionalFormat {
    ConditionalFormat {
        formula: Some(formula(schema, "Jobs::Status = \"Open\"")),
        condition: CfCondition {
            style: Some(local(vec![(1, 1, properties)])),
            ..Default::default()
        },
        kind: None,
    }
}

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Cache fixture", Locale::en_us());
    schema.add_table(TABLE, "Jobs");
    schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
    for (id, name, kind) in FIELDS {
        let kind = match kind {
            FieldKind::Number => ValueType::Number,
            _ => ValueType::Text,
        };
        schema.add_field(TABLE, FieldDef::data(id, name, kind));
    }
    schema.add_table(ITEMS, "Items");
    schema.add_occurrence(ITEMS_OCCURRENCE, "Items", ITEMS);
    schema.add_field(ITEMS, FieldDef::data(1, "Label", ValueType::Text));
    // Allows creation: the portal also paints its empty template row.
    let mut relationship = RelationshipDef::new(1, OCCURRENCE, ITEMS_OCCURRENCE, vec![]);
    relationship.options = 0x10;
    schema.add_relationship(relationship);
    schema.add_value_list(ValueList {
        id: 1,
        name: "Colors".into(),
        source: ValueSource::Custom(vec!["Red\rGreen\rBlue".into()]),
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    });
    schema.add_layout(1, "Cache fixture", Some(OCCURRENCE));
    schema
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 90, 160, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

const ICON: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path class="fm_fill" d="M0 0h16v16H0z"/></svg>"#;

fn object(id: u64, kind: u32, parent: Option<u64>, x: f64, y: f64, w: f64, h: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        parent,
        bounds: Some(om_layout::Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn field(object: LayoutObject, table_key: u64, field_key: u64, display_type: u32) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key,
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

fn text(object: LayoutObject, text: &str) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        ..object
    }
}

/// The theme's type and named styles, for a cascade with work to save.
fn theme() -> ThemeStyles {
    let types = (1..=18).chain([20, 30, 31, 32, 34, 38, 100, 200]);
    ThemeStyles {
        type_styles: types
            .map(|object_type| TypeStyle {
                object_types: vec![object_type],
                styles: Some(styles(vec![
                    (
                        1,
                        1,
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
                    ),
                    (1, 2, vec![paint(7, color(0.9, 0.9, 1.0))]),
                    (1, 7, vec![paint(7, color(0.8, 0.9, 0.8))]),
                    (3, 18, vec![paint(70, color(0.6, 0.6, 0.6))]),
                    (4, 1, vec![paint(514, color(0.2, 0.4, 0.8))]),
                ])),
                ..Default::default()
            })
            .collect(),
        custom_styles: (0..24)
            .map(|index| TypeStyle {
                name: format!("Named {index}"),
                styles: Some(styles(vec![(
                    1,
                    1,
                    vec![paint(7, color(index as f32 / 24.0, 0.95, 0.95))],
                )])),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// Every kind of object Browse draws, with conditional formats, a hide
/// condition, anchors, library images, panels, a popover, a portal and an
/// object beyond the layout's width.
fn document(schema: &CalcSchema) -> RenderLayout {
    let mut objects = vec![
        text(object(39, 2, None, 10.0, 10.0, 200.0, 20.0), "HEADER"),
        LayoutObject {
            style: Some(local(vec![(
                1,
                1,
                [
                    vec![paint(7, color(0.9, 0.8, 0.7))],
                    border(1.0, color(0.0, 0.0, 0.5)),
                ]
                .concat(),
            )])),
            ..object(1, 5, None, 10.0, 50.0, 120.0, 40.0)
        },
        LayoutObject {
            style: Some(local(vec![(
                1,
                1,
                vec![paint(7, color(0.7, 0.8, 0.9)), points(35, 8.0)],
            )])),
            ..object(2, 6, None, 140.0, 50.0, 120.0, 40.0)
        },
        LayoutObject {
            style: Some(ObjectStyle {
                custom_style_names: vec!["Named 5".into()],
                ..Default::default()
            }),
            ..object(3, 7, None, 270.0, 50.0, 80.0, 40.0)
        },
        LayoutObject {
            style: Some(local(vec![(1, 1, border(2.0, color(0.5, 0.0, 0.0)))])),
            ..object(4, 4, None, 360.0, 70.0, 100.0, 0.0)
        },
        LayoutObject {
            text_runs: vec![
                TextRun::Literal("Job ".into()),
                TextRun::MergeField("Name".into()),
            ],
            ..text(object(5, 2, None, 10.0, 100.0, 300.0, 24.0), "Job <<Name>>")
        },
        LayoutObject {
            rich_text: Some(StyledText {
                text: "Styled label".into(),
                character_runs: vec![StyledRun {
                    range: 0..6,
                    style: CharacterStyle {
                        font_size: Some(15.0),
                        color: Some(color(0.0, 0.0, 1.0)),
                        ..Default::default()
                    },
                }],
                ..Default::default()
            }),
            ..text(
                object(41, 2, None, 320.0, 100.0, 200.0, 24.0),
                "Styled label",
            )
        },
        field(
            object(6, 1, None, 10.0, 130.0, 200.0, 24.0),
            LAYOUT_KEY,
            1,
            0,
        ),
        field(
            object(7, 1, None, 220.0, 130.0, 150.0, 24.0),
            LAYOUT_KEY,
            2,
            1,
        ),
        field(
            object(8, 1, None, 10.0, 160.0, 200.0, 50.0),
            LAYOUT_KEY,
            4,
            3,
        ),
        field(
            object(9, 1, None, 220.0, 160.0, 150.0, 50.0),
            LAYOUT_KEY,
            4,
            4,
        ),
        field(
            object(10, 1, None, 380.0, 130.0, 120.0, 24.0),
            LAYOUT_KEY,
            1,
            7,
        ),
        LayoutObject {
            data_format: Some(DataFormat {
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
            }),
            ..field(
                object(11, 1, None, 380.0, 160.0, 120.0, 24.0),
                LAYOUT_KEY,
                3,
                0,
            )
        },
        LayoutObject {
            placeholder: Some("Type a note".into()),
            ..field(
                object(12, 1, None, 510.0, 130.0, 120.0, 24.0),
                LAYOUT_KEY,
                5,
                0,
            )
        },
        LayoutObject {
            graphic: Some(GraphicObject {
                library_key: PNG,
                ..Default::default()
            }),
            ..object(13, 3, None, 10.0, 220.0, 32.0, 32.0)
        },
        LayoutObject {
            button: Some(ButtonObject {
                script_step: Some(ScriptStep {
                    id: 1,
                    ..Default::default()
                }),
                library_key: SVG,
                icon_size: 16,
                ..Default::default()
            }),
            ..text(object(14, 10, None, 50.0, 220.0, 120.0, 28.0), "Run")
        },
        LayoutObject {
            popover_button: Some(PopoverButtonObject {
                popover_key: POPOVER,
                library_key: SVG,
                icon_size: 12,
                ..Default::default()
            }),
            ..text(object(15, 14, None, 180.0, 220.0, 120.0, 28.0), "More")
        },
        LayoutObject {
            popover: Some(PopoverObject {
                button_key: 15,
                ..Default::default()
            }),
            style: Some(local(vec![(1, 1, vec![paint(7, color(1.0, 1.0, 0.9))])])),
            ..object(POPOVER, 15, None, 180.0, 250.0, 200.0, 80.0)
        },
        text(
            object(17, 2, Some(POPOVER), 10.0, 10.0, 150.0, 20.0),
            "Inside",
        ),
        LayoutObject {
            tab: Some(TabObject {
                tab_height: 24,
                ..Default::default()
            }),
            ..object(18, 11, None, 10.0, 260.0, 300.0, 100.0)
        },
        LayoutObject {
            tab_panel: Some(TabPanelObject {
                tab_left_edge: 0,
                tab_width: 80,
                ..Default::default()
            }),
            ..text(object(19, 12, Some(18), 0.0, 24.0, 300.0, 76.0), "First")
        },
        LayoutObject {
            tab_panel: Some(TabPanelObject {
                tab_left_edge: 80,
                tab_width: 80,
                ..Default::default()
            }),
            ..text(object(20, 12, Some(18), 0.0, 24.0, 300.0, 76.0), "Second")
        },
        field(
            object(21, 1, Some(19), 10.0, 10.0, 150.0, 24.0),
            LAYOUT_KEY,
            1,
            0,
        ),
        text(
            object(22, 2, Some(20), 10.0, 10.0, 150.0, 24.0),
            "Hidden panel",
        ),
        object(23, 16, None, 320.0, 260.0, 150.0, 100.0),
        object(24, 17, Some(23), 0.0, 0.0, 150.0, 100.0),
        object(25, 17, Some(23), 0.0, 0.0, 150.0, 100.0),
        LayoutObject {
            style: Some(local(vec![(1, 1, vec![paint(7, color(0.3, 0.7, 0.3))])])),
            ..object(26, 7, Some(24), 20.0, 20.0, 60.0, 40.0)
        },
        LayoutObject {
            portal: Some(PortalObject {
                table_alias_key: ITEMS_KEY,
                num_rows: 3.0,
                initial_row: 1,
                ..Default::default()
            }),
            style: Some(local(vec![
                (
                    1,
                    1,
                    [
                        border(1.0, color(0.2, 0.2, 0.2)),
                        vec![int(610, 1), int(620, 1)],
                    ]
                    .concat(),
                ),
                (12, 1, vec![paint(7, color(1.0, 1.0, 1.0))]),
                (13, 1, vec![paint(7, color(0.95, 0.95, 0.95))]),
                (28, 1, vec![paint(7, color(0.8, 0.9, 1.0))]),
            ])),
            ..object(27, 9, None, 10.0, 370.0, 400.0, 60.0)
        },
        LayoutObject {
            style: Some(local(vec![(1, 1, border(0.5, color(0.4, 0.4, 0.4)))])),
            ..field(
                object(28, 1, Some(27), 0.0, 0.0, 200.0, 20.0),
                ITEMS_KEY,
                1,
                0,
            )
        },
        LayoutObject {
            style: Some(local(vec![(1, 1, vec![paint(7, color(0.9, 0.7, 0.9))])])),
            ..object(29, 5, Some(27), 210.0, 0.0, 100.0, 20.0)
        },
        LayoutObject {
            group: Some(GroupObject::default()),
            ..object(30, 8, None, 420.0, 220.0, 100.0, 60.0)
        },
        LayoutObject {
            // Anchored on all four sides: its bounds follow the window.
            flags: 0xf000_0000,
            style: Some(local(vec![(1, 1, vec![paint(7, color(0.6, 0.6, 0.9))])])),
            ..object(31, 5, Some(30), 0.0, 0.0, 50.0, 30.0)
        },
        object(32, 13, None, 530.0, 220.0, 100.0, 60.0),
        object(33, 18, None, 420.0, 300.0, 200.0, 30.0),
        text(object(34, 10, Some(33), 0.0, 0.0, 100.0, 30.0), "Left"),
        text(object(35, 10, Some(33), 100.0, 0.0, 100.0, 30.0), "Right"),
        LayoutObject {
            // Anchored right and bottom: it moves as the window grows.
            flags: 0xc000_0000,
            ..text(object(36, 2, None, 500.0, 400.0, 120.0, 20.0), "Anchored")
        },
        text(object(37, 2, None, 700.0, 50.0, 50.0, 20.0), "Outside"),
        LayoutObject {
            style: Some(local(vec![(1, 1, vec![paint(7, color(0.0, 0.0, 0.0))])])),
            ..object(38, 5, None, 470.0, 50.0, 40.0, 40.0)
        },
        object(42, 99, None, 520.0, 50.0, 40.0, 40.0),
        text(object(40, 2, None, 10.0, 450.0, 200.0, 20.0), "FOOTER"),
    ];
    for object in &mut objects {
        match object.id {
            1 => object
                .calcs
                .conditions
                .push(open_format(schema, vec![paint(7, color(0.0, 0.8, 0.0))])),
            5 | 6 | 41 => object.calcs.conditions.push(open_format(
                schema,
                vec![paint(70, color(0.8, 0.0, 0.0)), int(65, 2)],
            )),
            8 | 9 => {
                object.field_obj.as_mut().unwrap().value_list_key = 1;
                object
                    .calcs
                    .conditions
                    .push(open_format(schema, vec![paint(7, color(1.0, 0.9, 0.6))]));
            }
            38 => object.calcs.hide = Some(formula(schema, "Jobs::Status <> \"Open\"")),
            _ => {}
        }
    }
    let children: Vec<(u64, u64)> = objects
        .iter()
        .filter_map(|object| Some((object.parent?, object.id)))
        .collect();
    for (parent, child) in children {
        let parent = objects.iter_mut().find(|o| o.id == parent).unwrap();
        parent.children.push(child);
    }
    let root = objects
        .iter()
        .filter(|object| object.parent.is_none())
        .map(|object| object.id)
        .collect();
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
            name: "Cache fixture".into(),
            def: Some(LayoutDef {
                base_width: 640.0,
                table_alias_key: LAYOUT_KEY,
                background_style: Some(local(vec![(
                    1,
                    1,
                    vec![paint(7, color(0.97, 0.97, 0.97))],
                )])),
                ..Default::default()
            }),
            parts: vec![
                part(1, 1, 0.0, 40.0),
                part(2, 4, 40.0, 400.0),
                part(3, 7, 440.0, 40.0),
            ],
            objects: ObjectStore {
                root,
                objects,
                ..Default::default()
            },
            theme_styles: theme(),
            library_images: BTreeMap::from([
                (
                    PNG,
                    LibraryImage {
                        format: LibraryImageFormat::Png,
                        data: png(4, 4),
                    },
                ),
                (
                    SVG,
                    LibraryImage {
                        format: LibraryImageFormat::Svg,
                        data: ICON.to_vec(),
                    },
                ),
            ]),
            ..Default::default()
        },
        field_bindings: BTreeMap::from([
            (6, "Name".into()),
            (7, "Status".into()),
            (8, "Colors".into()),
            (9, "Colors".into()),
            (10, "Name".into()),
            (11, "Amount".into()),
            (12, "Note".into()),
            (21, "Name".into()),
        ]),
    }
}

fn session_and_calculations(document: RenderLayout) -> (Session, Calculations) {
    let schema = schema();
    let mut calcs = Calculations::default();
    calcs.set_schema(Some(Arc::new(schema)));
    {
        let data = calcs.cache.get_mut();
        for (record, values) in RECORDS {
            let mut row = RecordValues::default();
            for ((id, _, kind), value) in FIELDS.into_iter().zip(values) {
                row.set(
                    id,
                    1,
                    match kind {
                        FieldKind::Number => Value::number(value.parse().unwrap()),
                        _ => Value::text(value),
                    },
                );
            }
            data.records
                .insert((TABLE, record as u64), Some(Arc::new(row)));
        }
        for (record, label) in [(10, "Alder"), (20, "Birch"), (30, "Cedar")] {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::text(label));
            data.records.insert((ITEMS, record), Some(Arc::new(row)));
        }
        data.related
            .insert((OCCURRENCE, 1, ITEMS_OCCURRENCE), Arc::from([10, 20]));
        data.related
            .insert((OCCURRENCE, 2, ITEMS_OCCURRENCE), Arc::from([30]));
    }
    let mut session = Session::new(
        Target {
            host: "localhost".into(),
            file: "Cache fixture".into(),
            local: None,
        },
        25,
    );
    let key = session
        .set_layouts(vec![LayoutInfo {
            id: 1,
            name: "Cache fixture".into(),
        }])
        .unwrap();
    session
        .accept_page(
            key,
            Ok(RecordPage {
                versions: Default::default(),
                fields: FIELDS
                    .map(|(_, name, kind)| Field {
                        name: name.into(),
                        kind,
                    })
                    .into(),
                records: RECORDS
                    .map(|(id, values)| Record {
                        id,
                        values: values.map(|value| Some(value.into())).into(),
                    })
                    .into(),
                total: 2,
            }),
        )
        .unwrap();
    session.render = RenderState::Loaded(Box::new(document));
    (session, calcs)
}

fn frame(
    ctx: &egui::Context,
    size: Vec2,
    session: &Session,
    calcs: &Calculations,
    state: &mut DocumentState,
    view: ViewMode,
) -> Vec<ClippedShape> {
    let RenderState::Loaded(document) = &session.render else {
        panic!("no layout");
    };
    let evaluator = Evaluator::new(
        calcs,
        session,
        state,
        SessionInfo::default(),
        &Variables::new(),
    )
    .unwrap();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            show(
                ui,
                session,
                document,
                state,
                view,
                Some(&evaluator),
                &mut Vec::new(),
            );
        });
    });
    let shapes = std::mem::take(&mut output.shapes);
    output.drop_without_applying_deltas();
    shapes
}

/// A context like the window's, in which nothing animates: frames compare
/// as soon as they are painted.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    ctx.all_styles_mut(|style| style.animation_time = 0.0);
    ctx
}

/// Paint in each context until egui has decoded the library images, which
/// happens off the UI thread, so that later frames differ by nothing else.
fn load_images(contexts: [&egui::Context; 2], session: &Session, calcs: &Calculations) {
    let mut states = [DocumentState::default(), DocumentState::default()];
    let deadline = Instant::now() + Duration::from_secs(30);
    for _ in 0..2 {
        while contexts
            .iter()
            .zip(&mut states)
            .fold(false, |pending, (ctx, state)| {
                frame(
                    ctx,
                    vec2(1000.0, 700.0),
                    session,
                    calcs,
                    state,
                    ViewMode::Form,
                );
                pending | ctx.has_pending_images()
            })
        {
            assert!(Instant::now() < deadline, "library images never loaded");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Textures by name: two contexts number the same images in the order
/// their decoding finished.
fn canonical(ctx: &egui::Context, shapes: &mut [ClippedShape]) {
    use std::hash::{Hash, Hasher};
    fn visit(shape: &mut Shape, name: &dyn Fn(egui::TextureId) -> egui::TextureId) {
        match shape {
            Shape::Rect(rect) => {
                if let Some(brush) = &mut rect.brush {
                    Arc::make_mut(brush).fill_texture_id = name(brush.fill_texture_id);
                }
            }
            Shape::Vec(shapes) => shapes.iter_mut().for_each(|shape| visit(shape, name)),
            _ => {}
        }
    }
    let textures = ctx.tex_manager();
    let textures = textures.read();
    let name = |id| {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        textures.meta(id).map(|meta| &meta.name).hash(&mut hash);
        egui::TextureId::User(hash.finish())
    };
    for shape in shapes {
        visit(&mut shape.shape, &name);
    }
}

fn texts(shape: &Shape, out: &mut Vec<String>) {
    match shape {
        Shape::Text(text) => out.push(text.galley.job.text.clone()),
        Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
        _ => {}
    }
}

fn fills(shape: &Shape, out: &mut Vec<Color32>) {
    match shape {
        Shape::Rect(rect) => out.push(rect.fill),
        Shape::Vec(shapes) => shapes.iter().for_each(|shape| fills(shape, out)),
        _ => {}
    }
}

fn strokes(shape: &Shape, out: &mut Vec<Color32>) {
    match shape {
        Shape::LineSegment { stroke, .. } => out.push(stroke.color),
        Shape::Rect(rect) => out.push(rect.stroke.color),
        Shape::Path(path) => {
            if let egui::epaint::ColorMode::Solid(color) = path.stroke.color {
                out.push(color);
            }
        }
        Shape::Vec(shapes) => shapes.iter().for_each(|shape| strokes(shape, out)),
        _ => {}
    }
}

fn images(shape: &Shape) -> usize {
    match shape {
        Shape::Rect(rect) => usize::from(rect.brush.is_some()),
        Shape::Vec(shapes) => shapes.iter().map(images).sum(),
        _ => 0,
    }
}

fn assert_same(expected: &[ClippedShape], actual: &[ClippedShape], what: &str) {
    assert_eq!(expected.len(), actual.len(), "{what}: shape count");
    if let Some(index) = (0..expected.len()).find(|&i| expected[i] != actual[i]) {
        panic!(
            "{what}: shape {index} differs\nuncached: {:?}\ncached: {:?}",
            expected[index], actual[index]
        );
    }
}

#[test]
fn cached_browse_paints_what_uncached_browse_paints() {
    let schema = schema();
    let (mut session, calcs) = session_and_calculations(document(&schema));
    // One context each, fed the same frames: egui keeps scroll and other
    // widget state per context.
    let (without, with) = (context(), context());
    load_images([&without, &with], &session, &calcs);
    let mut uncached = DocumentState {
        layout_cache: LayoutCache::uncached(),
        ..Default::default()
    };
    let mut cached = DocumentState::default();
    for state in [&mut uncached, &mut cached] {
        state.popovers.insert(1, POPOVER);
    }
    let green = Color32::from_rgb(0, 204, 0);
    let small = vec2(1000.0, 700.0);
    let large = vec2(1400.0, 900.0);
    // No clear between these: records toggle conditional formats and hide
    // conditions, the window grows and zooms.
    for (view, current, size, zoom) in [
        (ViewMode::Form, 0, small, 100),
        (ViewMode::Form, 1, small, 100),
        (ViewMode::Form, 0, small, 100),
        (ViewMode::List, 0, small, 100),
        (ViewMode::Form, 0, large, 100),
        (ViewMode::Form, 0, large, 150),
        (ViewMode::Form, 1, large, 50),
        (ViewMode::List, 1, large, 100),
    ] {
        let what = format!("{view:?} record {current} {size:?} at {zoom}%");
        session.current = current;
        for state in [&mut uncached, &mut cached] {
            state.zoom.set_percent(zoom);
        }
        let mut expected = Vec::new();
        for pass in ["first", "second"] {
            expected = frame(&without, size, &session, &calcs, &mut uncached, view);
            let mut shapes = frame(&with, size, &session, &calcs, &mut cached, view);
            canonical(&without, &mut expected);
            canonical(&with, &mut shapes);
            assert_same(&expected, &shapes, &format!("{what}, {pass} frame"));
        }
        let mut painted = Vec::new();
        let mut filled = Vec::new();
        for shape in &expected {
            texts(&shape.shape, &mut painted);
            fills(&shape.shape, &mut filled);
        }
        // The fixture paints what it is meant to cover.
        let has = |label: &str| painted.iter().any(|text| text == label);
        for label in [
            "HEADER",
            "FOOTER",
            "Styled label",
            "First",
            "Second",
            "Inside",
        ] {
            assert!(has(label), "{what}: {label:?} absent from {painted:?}");
        }
        let images: usize = expected.iter().map(|shape| images(&shape.shape)).sum();
        assert!(images >= 3, "{what}: {images} images");
        if view == ViewMode::Form {
            let index = current as usize;
            for label in [
                ["Job Ada", "Job Grace"][index],
                ["$1,234.00", "$99.00"][index],
                ["Alder", "Cedar"][index],
            ] {
                assert!(has(label), "{what}: {label:?} absent from {painted:?}");
            }
            assert_eq!(filled.contains(&green), current == 0, "{what}: format");
        } else {
            // Both rows in one frame: with and without the format. With
            // record 1 current the list shows it at the top, and record 0's
            // row, whose format it is, is above the view: its objects paint,
            // clipped, without evaluating their formats (#1120).
            assert!(has("Job Ada") && has("Job Grace"), "{what}: {painted:?}");
            assert_eq!(filled.contains(&green), current == 0, "{what}: format");
        }
        for absent in ["Outside", "Hidden panel"] {
            assert!(!painted.iter().any(|t| t == absent), "{what}: {absent}");
        }
    }
}

/// A field with the keyboard focus draws in its theme's In Focus state
/// (#753): its border takes the color its style gives state 4 over the
/// normal one. Its frame is painted before its control takes the focus
/// (Go to Field, New Record), so the next frame shows it, which egui asks
/// for when the focus changes. An edit box, and a checkbox set, which has
/// no blinking cursor to ask for frames of its own.
#[test]
fn a_focused_field_draws_its_in_focus_state() {
    let (gray, blue) = (color(0.78, 0.78, 0.8), color(0.0, 0.44, 0.81));
    let (orange, green) = (color(0.9, 0.5, 0.1), color(0.1, 0.6, 0.2));
    let solid = |value| {
        (0..4)
            .flat_map(|side| {
                [
                    points(28 + side, 1.0),
                    int(24 + side, 5),
                    paint(20 + side, value),
                ]
            })
            .collect::<Vec<_>>()
    };
    let painted = |shapes: Vec<ClippedShape>| {
        let mut out = Vec::new();
        for shape in shapes {
            strokes(&shape.shape, &mut out);
        }
        out
    };
    let (gray32, blue32) = (super::color(gray), super::color(blue));
    let (orange32, green32) = (super::color(orange), super::color(green));
    for (id, name) in [(6, "Name"), (8, "Colors")] {
        let mut doc = document(&schema());
        doc.layout.objects.objects.retain(|object| object.id == id);
        let field = &mut doc.layout.objects.objects[0];
        field.calcs = Default::default();
        let mut components = vec![
            (1, 1, solid(gray)),
            (1, 4, (0..4).map(|side| paint(20 + side, blue)).collect()),
        ];
        // The set's checked marks (record 1 checks Red and Blue): their
        // ink is the icon color, checked (7) and checked+focus (16).
        if id == 8 {
            components.push((4, 7, vec![paint(514, orange)]));
            components.push((4, 16, vec![paint(514, green)]));
        }
        field.style = Some(local(components));
        doc.layout.objects.root = vec![id];
        doc.field_bindings.retain(|object, _| *object == id);
        let (session, calcs) = session_and_calculations(doc);
        let ctx = context();
        let mut state = DocumentState::default();
        state.capabilities.update = true;
        let size = vec2(800.0, 600.0);
        let unfocused = painted(frame(
            &ctx,
            size,
            &session,
            &calcs,
            &mut state,
            ViewMode::Form,
        ));
        assert!(
            unfocused.contains(&gray32) && !unfocused.contains(&blue32),
            "{name}: {unfocused:?}"
        );
        if id == 8 {
            assert!(unfocused.contains(&orange32) && !unfocused.contains(&green32));
        }
        // Go to Field: the control takes the focus as it paints, after
        // its frame.
        state.focus = Some(name.into());
        frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
        assert!(
            ctx.has_requested_repaint(),
            "{name}: one more frame shows the focus"
        );
        let focused = painted(frame(
            &ctx,
            size,
            &session,
            &calcs,
            &mut state,
            ViewMode::Form,
        ));
        assert!(
            focused.contains(&blue32) && !focused.contains(&gray32),
            "{name}: {focused:?}"
        );
        if id == 8 {
            assert!(
                focused.contains(&green32) && !focused.contains(&orange32),
                "checked marks take checked+focus: {focused:?}"
            );
        }
    }
}

#[test]
fn targeted_stored_refresh_enters_editor_with_fresh_value_and_keeps_drafts() {
    use om_data::host::CalcNeed;
    let mut doc = document(&schema());
    doc.layout.objects.objects.retain(|object| object.id == 6);
    doc.layout.objects.objects[0].calcs = Default::default();
    doc.layout.objects.root = vec![6];
    doc.field_bindings.retain(|id, _| *id == 6);
    let (mut session, mut calcs) = session_and_calculations(doc);
    let ctx = context();
    let mut state = DocumentState::default();
    state.capabilities.update = true;
    let size = vec2(800.0, 600.0);
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let mut fresh = calcs.cache.borrow().clone();
    fresh.generation = Some(2);
    Arc::make_mut(
        fresh
            .records
            .get_mut(&(TABLE, 1))
            .unwrap()
            .as_mut()
            .unwrap(),
    )
    .set(1, 1, Value::text("Fresh stored value"));
    calcs.refresh_object(
        &crate::scripting::ObjectRefresh {
            layout: 1,
            objects: vec![6],
            repetition: 0,
            join: None,
            generation: Some(2),
        },
        &session,
    );
    calcs.accept_data(
        &[CalcNeed::Record {
            table: TABLE,
            record: 1,
        }],
        Ok(fresh),
    );
    let shapes = frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let mut shown = Vec::new();
    for shape in shapes {
        texts(&shape.shape, &mut shown);
    }
    assert!(
        shown.iter().any(|text| text == "Fresh stored value"),
        "refreshed preview: {shown:?}"
    );
    state.focus = Some("Name".into());
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    assert_eq!(
        state.buffers[&(1, 1, "Name".into())].value,
        "Fresh stored value",
        "entering a clean refreshed stored field must use the value just displayed"
    );
    // The retained page can be empty even though the refreshed baseline is
    // populated. Clearing that fresh value is still an actual edit.
    session.page.as_mut().unwrap().data.records[0].values[0] = None;
    state
        .buffers
        .get_mut(&(1, 1, "Name".into()))
        .unwrap()
        .value
        .clear();
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let buffer = &state.buffers[&(1, 1, "Name".into())];
    assert_eq!(buffer.original, "Fresh stored value");
    assert_eq!(buffer.value, "");
    assert_eq!(
        state.changes(&session).get("Name"),
        Some(&None),
        "the old page must not make a cleared refreshed field look unchanged"
    );
}

#[test]
fn targeted_editor_baseline_survives_duplicate_object() {
    use om_data::host::CalcNeed;
    let mut doc = document(&schema());
    doc.layout
        .objects
        .objects
        .retain(|object| matches!(object.id, 6 | 10));
    for object in &mut doc.layout.objects.objects {
        object.calcs = Default::default();
        object.field_obj.as_mut().unwrap().display_type = 0;
    }
    doc.layout.objects.root = vec![6, 10];
    doc.field_bindings.retain(|id, _| matches!(*id, 6 | 10));
    let (mut session, mut calcs) = session_and_calculations(doc);
    // Install the empty older page through Session so its record_at cache,
    // rather than only the retained display page, has the empty field.
    let mut old_page = session.page.as_ref().unwrap().data.clone();
    old_page.records[0].values[0] = None;
    let old_key = session.refresh().unwrap();
    session.accept_page(old_key, Ok(old_page)).unwrap();
    assert_eq!(session.current_record().unwrap().1.values[0], None);
    let ctx = context();
    let mut state = DocumentState::default();
    state.capabilities.update = true;
    let size = vec2(800.0, 600.0);
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let mut fresh = calcs.cache.borrow().clone();
    fresh.generation = Some(2);
    Arc::make_mut(
        fresh
            .records
            .get_mut(&(TABLE, 1))
            .unwrap()
            .as_mut()
            .unwrap(),
    )
    .set(1, 1, Value::text("Fresh stored value"));
    calcs.refresh_object(
        &crate::scripting::ObjectRefresh {
            layout: 1,
            objects: vec![6],
            repetition: 0,
            join: None,
            generation: Some(2),
        },
        &session,
    );
    calcs.accept_data(
        &[CalcNeed::Record {
            table: TABLE,
            record: 1,
        }],
        Ok(fresh),
    );
    let shapes = frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let mut shown = Vec::new();
    for shape in shapes {
        texts(&shape.shape, &mut shown);
    }
    assert!(
        shown.iter().any(|text| text == "Fresh stored value"),
        "refreshed preview: {shown:?}"
    );
    state.focus = Some("Name".into());
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);

    // Exercise the retained probe's empty older-page case through an actual
    // Backspace event in the focused refreshed editor, with a second object
    // still bound to the same stored field.
    assert_eq!(session.current_record().unwrap().1.values[0], None);
    let edit_id = ctx.memory(|memory| memory.focused()).unwrap();
    let mut edit = egui::TextEdit::load_state(&ctx, edit_id).unwrap();
    edit.cursor
        .set_char_range(Some(egui::text::CCursorRange::select_all_str(
            "Fresh stored value",
        )));
    edit.store(&ctx, edit_id);
    state.changed.clear();
    let RenderState::Loaded(doc) = &session.render else {
        panic!("no layout")
    };
    let evaluator = Evaluator::new(
        &calcs,
        &session,
        &state,
        SessionInfo::default(),
        &Variables::new(),
    )
    .unwrap();
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events: vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show(
                    ui,
                    &session,
                    doc,
                    &mut state,
                    ViewMode::Form,
                    Some(&evaluator),
                    &mut Vec::new(),
                );
            });
        },
    )
    .drop_without_applying_deltas();

    assert!(
        state.changed.iter().any(|(name, _)| name == "Name"),
        "the key must edit the focused field"
    );
    assert_eq!(state.buffers[&(1, 1, "Name".into())].value, "");
    assert_eq!(
        state.changes(&session).get("Name"),
        Some(&None),
        "a duplicate field object must not erase the cleared targeted draft"
    );
    // Once that edit is undone, an accepted replacement page becomes the
    // baseline; merely finishing a request must not have released it sooner.
    let buffer = state.buffers.get_mut(&(1, 1, "Name".into())).unwrap();
    buffer.value.clone_from(&buffer.original);
    let mut page = session.page.as_ref().unwrap().data.clone();
    page.records[0].values[0] = Some("After reload".into());
    let key = session.refresh().unwrap();
    session.accept_page(key, Ok(page)).unwrap();
    calcs.data_changed();
    frame(&ctx, size, &session, &calcs, &mut state, ViewMode::Form);
    let buffer = &state.buffers[&(1, 1, "Name".into())];
    assert_eq!(buffer.original, "After reload");
    assert_eq!(buffer.value, "After reload");
    assert!(!buffer.refreshed);
}
