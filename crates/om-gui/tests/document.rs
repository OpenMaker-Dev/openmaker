//! Browse rendering from a provider's decoded file layout. These tests have
//! no server, environment, vendor fixture, or graphics-device requirement,
//! except the ignored manual capture, which renders offscreen through wgpu.
mod common;

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
};

use egui::{epaint::Shape, Color32, Role};
use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordCapabilities, RecordPage, RenderLayout,
};
use om_layout::{
    style::{ComponentStyle, TypeStyle},
    Color, ExternalObject, FieldObject, GraphicObject, Layout, LayoutDef, LayoutObject,
    LibraryImage, LibraryImageFormat, ObjectStore, Part, Property, Rect, StyleSet, TabObject,
    TabPanelObject, TextRun, ThemeStyles,
};

struct DocumentProvider {
    rows: Vec<Record>,
    writable: bool,
    styled: bool,
    conditional_color: bool,
    duplicate_field: bool,
    /// Given's "Go to next object using" includes Return and the Enter
    /// key, as 123 of the starter solutions' 179 field objects do.
    given_next_keys: bool,
    replacement: Option<Arc<AtomicBool>>,
    list: bool,
    /// The card is [`web_viewer_card`].
    web_viewer: bool,
    update_entered: Option<mpsc::Sender<()>>,
    update_gate: Option<mpsc::Receiver<()>>,
    /// Holds the page read that follows a save, like a slow server.
    reload_gate: Option<mpsc::Receiver<()>>,
    read_gate: Option<mpsc::Receiver<()>>,
    reject_updates: bool,
    external_update: Option<String>,
    reads: usize,
    capabilities: Option<RecordCapabilities>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl DocumentProvider {
    fn new(empty: bool) -> Self {
        Self {
            writable: false,
            styled: false,
            conditional_color: false,
            duplicate_field: false,
            given_next_keys: false,
            replacement: None,
            list: false,
            web_viewer: false,
            update_entered: None,
            update_gate: None,
            reload_gate: None,
            read_gate: None,
            reject_updates: false,
            external_update: None,
            reads: 0,
            capabilities: None,
            events: Arc::default(),
            rows: if empty {
                vec![]
            } else {
                vec![
                    Record {
                        id: 19,
                        values: vec![Some("Clara".into()), Some("Mason".into())],
                    },
                    Record {
                        id: 37,
                        values: vec![Some("Emery".into()), Some("Chen".into())],
                    },
                ]
            },
        }
    }
}

impl DataProvider for DocumentProvider {
    fn calc_schema(&mut self) -> Result<Option<Arc<om_data::host::CalcSchema>>, DataError> {
        Ok(self.styled.then(|| Arc::new(styled_schema())))
    }

    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        use om_data::calc::{CharStyle, Ctx, Date, Text, Value};
        use om_data::host::{MemorySource, RecordValues, Recorder};
        let schema = styled_schema();
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 1, 2).unwrap());
        let mut source = MemorySource::new();
        for row in &self.rows {
            let mut values = RecordValues::default();
            values.set(
                1,
                1,
                Value::Text(Text::styled(
                    row.values[0].as_deref().unwrap_or(""),
                    CharStyle {
                        font: Some("Arial".into()),
                        size: Some(13),
                        color: Some(0xdd0000),
                        flags: 256,
                    },
                )),
            );
            values.set(2, 1, Value::text(row.values[1].as_deref().unwrap_or("")));
            source.put_record(0x8002, row.id as u64, values);
        }
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|error| DataError::new(error.to_string()))?;
        }
        Ok(recorder.take())
    }
    fn can_write_records(&self) -> bool {
        self.writable
    }
    fn record_capabilities(&mut self, _: u32) -> Result<RecordCapabilities, DataError> {
        Ok(self.capabilities.clone().unwrap_or(RecordCapabilities {
            create: self.writable,
            update: self.writable,
            delete: self.writable,
            editable_fields: None,
            ..Default::default()
        }))
    }
    fn create_record(
        &mut self,
        _: u32,
        values: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        let record = Record {
            id: self.rows.iter().map(|r| r.id).max().unwrap_or(0) + 1,
            values: ["Given", "Family"]
                .iter()
                .map(|name| values.get(*name).cloned().flatten())
                .collect(),
        };
        self.rows.push(record.clone());
        Ok(record)
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: om_data::FieldChanges,
    ) -> Result<Record, DataError> {
        self.events.lock().unwrap().push("update");
        if let Some(entered) = self.update_entered.take() {
            entered.send(()).unwrap();
        }
        if let Some(gate) = self.update_gate.take() {
            gate.recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| DataError::new(error.to_string()))?;
        }
        if self.reject_updates {
            return Err(DataError::new("The server rejected this edit."));
        }
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.id == id)
            .ok_or_else(|| DataError::new("Missing record"))?;
        for (index, name) in ["Given", "Family"].iter().enumerate() {
            if let Some(value) = values.get(*name) {
                row.values[index] = value.clone();
            }
        }
        self.read_gate = self.reload_gate.take();
        Ok(row.clone())
    }
    fn delete_record(&mut self, _: u32, id: i64) -> Result<(), DataError> {
        self.rows.retain(|row| row.id != id);
        Ok(())
    }
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Cards.fmp12".into(),
        }])
    }

    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }

    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        let mut layouts = vec![LayoutInfo {
            id: 7,
            name: "Client Card".into(),
        }];
        if self.replacement.is_some() {
            layouts.push(LayoutInfo {
                id: 8,
                name: "Other Card".into(),
            });
        }
        Ok(layouts)
    }

    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.events.lock().unwrap().push("read");
        self.reads += 1;
        if let Some(gate) = self.read_gate.take() {
            gate.recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| DataError::new(error.to_string()))?;
        }
        if self.reads > 1 {
            if let Some(value) = self.external_update.take() {
                self.rows[0].values[0] = Some(value);
            }
        }
        Ok(RecordPage {
            versions: Default::default(),
            fields: ["Given", "Family"]
                .into_iter()
                .map(|name| Field {
                    name: name.into(),
                    kind: FieldKind::Text,
                })
                .collect(),
            records: self
                .rows
                .iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(limit as usize)
                .cloned()
                .collect(),
            total: self.rows.len() as u64,
        })
    }

    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        if id == 8 && self.replacement.is_some() {
            return Ok(None);
        }
        let mut document = document();
        if self.given_next_keys {
            let given = document
                .layout
                .objects
                .objects
                .iter_mut()
                .find(|o| o.id == 3);
            given.unwrap().field_obj.as_mut().unwrap().flags |= 0xc0;
        }
        if self.styled {
            document.layout.def.as_mut().unwrap().table_alias_key = 0x104082;
            for (object_id, field_id) in [(3, 1), (5, 2)] {
                document
                    .layout
                    .objects
                    .objects
                    .iter_mut()
                    .find(|o| o.id == object_id)
                    .unwrap()
                    .field_obj
                    .as_mut()
                    .unwrap()
                    .field_spec = Some(om_layout::FieldSpec {
                    table_key: 0x104082,
                    field_key: field_id,
                    repetition: 1,
                    ..Default::default()
                });
            }
            let label = document
                .layout
                .objects
                .objects
                .iter_mut()
                .find(|o| o.id == 1)
                .unwrap();
            label.rich_text = Some(om_layout::StyledText {
                text: label.text.clone().unwrap(),
                character_runs: vec![om_layout::StyledRun {
                    range: 0..6,
                    style: om_layout::CharacterStyle {
                        font_size: Some(8.0),
                        color: Some(Color {
                            blue: 1.0,
                            alpha: 1.0,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                }],
                ..Default::default()
            });
            document.layout.objects.objects.push(LayoutObject {
                field_obj: Some(FieldObject {
                    field_spec: Some(om_layout::FieldSpec {
                        table_key: 0x104082,
                        field_key: 3,
                        repetition: 1,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..object(6, 1, None, 390.0, 90.0, 180.0, 28.0)
            });
            document.layout.objects.root.push(6);
            if self.conditional_color {
                for object in document
                    .layout
                    .objects
                    .objects
                    .iter_mut()
                    .filter(|object| matches!(object.id, 1 | 3))
                {
                    object.calcs.conditions.push(green_condition());
                }
            }
        }
        if self.list {
            document.layout.def.as_mut().unwrap().flags = 0x200;
            document
                .layout
                .objects
                .objects
                .retain(|object| matches!(object.id, 1 | 3 | 5));
            document.layout.objects.root = vec![1, 3, 5];
            for object in &mut document.layout.objects.objects {
                if object.id == 5 {
                    object.bounds.as_mut().unwrap().left = 220.0;
                    object.bounds.as_mut().unwrap().right = 360.0;
                }
                if object.id != 1 {
                    object.bounds.as_mut().unwrap().top = 45.0;
                    object.bounds.as_mut().unwrap().bottom = 70.0;
                }
            }
            document.layout.parts[1].height = 40.0;
        }
        if self.web_viewer {
            web_viewer_card(&mut document.layout);
        }
        if self.duplicate_field {
            let index = document
                .layout
                .objects
                .objects
                .iter()
                .position(|o| o.id == 3)
                .unwrap();
            let mut duplicate = document.layout.objects.objects[index].clone();
            duplicate.id = 7;
            duplicate.bounds.as_mut().unwrap().left += 200.0;
            duplicate.bounds.as_mut().unwrap().right += 200.0;
            document.layout.objects.objects.insert(index + 1, duplicate);
            let index = document
                .layout
                .objects
                .root
                .iter()
                .position(|id| *id == 3)
                .unwrap();
            document.layout.objects.root.insert(index + 1, 7);
            document.field_bindings.insert(7, "Given".into());
        }
        if self
            .replacement
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            // A new immutable reply may reuse every id with a new vector order.
            document.layout.objects.objects.reverse();
            document
                .layout
                .objects
                .objects
                .iter_mut()
                .find(|o| o.id == 1)
                .unwrap()
                .text = Some("REPLACED CLIENT CARD".into());
            document.layout.def.as_mut().unwrap().base_width = 900.0;
        }
        Ok(Some(document))
    }

    fn close(&mut self) {}
}

fn styled_schema() -> om_data::host::CalcSchema {
    use om_data::calc::{Locale, ValueType};
    use om_data::host::{CalcSchema, FieldDef};
    let mut schema = CalcSchema::new("Cards", Locale::en_us());
    schema.add_table(0x8002, "Contacts");
    schema.add_occurrence(0xd00002, "Contacts", 0x8002);
    schema.add_layout(7, "Client Card", Some(0xd00002));
    schema.add_field(0x8002, FieldDef::data(1, "Given", ValueType::Text));
    schema.add_field(0x8002, FieldDef::data(2, "Family", ValueType::Text));
    let mut context = om_data::formula::Context::new(schema.names());
    context.occurrence = Some(0xd00002);
    let polish = om_data::formula::compile("Given & \"!\"", &context)
        .unwrap()
        .expr;
    schema.add_field(
        0x8002,
        FieldDef::calculation(
            3,
            "Preview",
            ValueType::Text,
            om_data::formula::Formula {
                context: Some(om_data::formula::Key::from_id(0xd00002)),
                formatted: None,
                polish,
            },
            false,
        ),
    );
    schema
}

fn green_condition() -> om_layout::calcs::ConditionalFormat {
    let schema = styled_schema();
    let context = om_data::formula::Context::new(schema.names());
    om_layout::calcs::ConditionalFormat {
        formula: Some(om_data::formula::Formula {
            context: None,
            formatted: None,
            polish: om_data::formula::compile("1", &context).unwrap().expr,
        }),
        condition: om_layout::calcs::CfCondition {
            style: Some(om_layout::ObjectStyle {
                local_styles: Some(StyleSet {
                    components: vec![ComponentStyle {
                        component: 1,
                        state: 1,
                        properties: vec![Property {
                            id: 70,
                            color: Some(Color {
                                green: 1.0,
                                alpha: 1.0,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        kind: None,
    }
}

fn object(
    id: u64,
    kind: u32,
    parent: Option<u64>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> LayoutObject {
    LayoutObject {
        id,
        object_type: kind,
        parent,
        bounds: Some(Rect {
            top: y,
            left: x,
            bottom: y + height,
            right: x + width,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn label(id: u64, parent: Option<u64>, text: &str, x: f64, y: f64) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        ..object(id, 2, parent, x, y, 300.0, 25.0)
    }
}

fn panel(id: u64, parent: u64, text: &str, left: i32, children: Vec<u64>) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        children,
        tab_panel: Some(TabPanelObject {
            tab_left_edge: left,
            tab_width: 110,
            ..Default::default()
        }),
        ..object(id, 12, Some(parent), 0.0, 30.0, 450.0, 260.0)
    }
}

fn document() -> RenderLayout {
    let mut merge = label(2, None, "Contact: <<Given>> <<Family>>", 20.0, 48.0);
    merge.text_runs = vec![
        TextRun::Literal("Contact: ".into()),
        TextRun::MergeField("Given".into()),
        TextRun::Literal(" ".into()),
        TextRun::MergeField("Family".into()),
    ];
    let field = LayoutObject {
        field_obj: Some(FieldObject::default()),
        placeholder: Some("Given name".into()),
        ..object(3, 1, None, 20.0, 85.0, 140.0, 25.0)
    };
    let family = LayoutObject {
        field_obj: Some(FieldObject::default()),
        placeholder: Some("Family name".into()),
        ..object(5, 1, None, 20.0, 120.0, 140.0, 25.0)
    };
    let group = LayoutObject {
        children: vec![30],
        ..object(20, 8, None, 180.0, 120.0, 500.0, 350.0)
    };
    let outer_tabs = LayoutObject {
        children: vec![31, 32],
        tab: Some(TabObject {
            default_panel_key: 31,
            tab_height: 30,
            ..Default::default()
        }),
        ..object(30, 11, Some(20), 20.0, 10.0, 450.0, 320.0)
    };
    let nested_tabs = LayoutObject {
        children: vec![41, 42],
        // Default selection is deliberately the second panel, and must take
        // precedence over the stored current-panel key.
        tab: Some(TabObject {
            default_panel_key: 42,
            current_panel_key: 41,
            tab_height: 30,
            ..Default::default()
        }),
        ..object(40, 11, Some(31), 15.0, 65.0, 400.0, 180.0)
    };
    let objects = vec![
        label(1, None, "CLIENT CARD", 20.0, 15.0),
        merge,
        field,
        family,
        object(4, 5, None, 500.0, 45.0, 90.0, 35.0),
        group,
        outer_tabs,
        panel(31, 30, "Summary", 0, vec![35, 40]),
        panel(32, 30, "Notes", 110, vec![36]),
        label(35, Some(31), "Summary content", 15.0, 15.0),
        label(36, Some(32), "Notes content", 15.0, 15.0),
        nested_tabs,
        panel(41, 40, "Work", 0, vec![45]),
        panel(42, 40, "Home", 110, vec![46]),
        label(45, Some(41), "Work details", 10.0, 10.0),
        label(46, Some(42), "Home details", 10.0, 10.0),
        label(900, None, "Outside the layout width", 820.0, 50.0),
        label(901, None, "Below the last layout part", 20.0, 550.0),
    ];
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Client Card".into(),
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
                    height: 480.0,
                    ..Default::default()
                },
            ],
            objects: ObjectStore {
                root: vec![1, 2, 3, 4, 5, 20],
                objects,
                ..Default::default()
            },
            theme_key: Some(9),
            theme_styles: ThemeStyles {
                type_styles: vec![TypeStyle {
                    object_types: vec![51],
                    styles: Some(StyleSet {
                        components: vec![ComponentStyle {
                            component: 1,
                            state: 1,
                            properties: vec![Property {
                                id: 7,
                                color: Some(Color {
                                    red: 0.25,
                                    green: 0.5,
                                    blue: 0.75,
                                    alpha: 1.0,
                                }),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([(3, "Given".into()), (5, "Family".into())]),
    }
}

/// The card with only its title and name fields, and on its right a web
/// viewer over a picture (a globe drawn for this test). The viewer is
/// anchored left, top and bottom (layout-model.md 5.7): the window's spare
/// height stretches it, and the record-edits bar takes some of that height
/// back. The picture keeps its place. Without a live view the viewer shows
/// OpenMaker's own note over the picture.
fn web_viewer_card(layout: &mut Layout) {
    const GLOBE: u64 = 0x51;
    let svg = concat!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" "##,
        r##"viewBox="0 0 120 120"><g fill="none" stroke="#2f6aa8" stroke-width="3">"##,
        r##"<circle cx="60" cy="60" r="56" fill="#d8e8f8"/>"##,
        r##"<ellipse cx="60" cy="60" rx="24" ry="56"/>"##,
        r##"<path d="M4 60h112M60 4v112M12 32h96M12 88h96"/></g></svg>"##,
    );
    layout
        .objects
        .objects
        .retain(|object| matches!(object.id, 1 | 3 | 5));
    layout.objects.objects.push(LayoutObject {
        graphic: Some(GraphicObject {
            library_key: GLOBE,
            ..Default::default()
        }),
        ..object(50, 3, None, 340.0, 150.0, 120.0, 120.0)
    });
    layout.objects.objects.push(LayoutObject {
        flags: 0xb000_0000,
        external: Some(ExternalObject {
            type_code: ExternalObject::WEB_VIEWER,
            ..Default::default()
        }),
        ..object(51, 13, None, 200.0, 60.0, 400.0, 300.0)
    });
    layout.objects.root = vec![1, 3, 5, 50, 51];
    layout.library_images.insert(
        GLOBE,
        LibraryImage {
            format: LibraryImageFormat::Svg,
            data: svg.into(),
        },
    );
}

/// Render the window to `name`.png in the `OM_SCREENSHOTS` folder and add
/// `name: line` to the folder's `readback.txt`.
fn shot(app: &mut common::App, name: &str, line: &str) {
    use std::io::Write as _;
    let dir = std::env::var_os("OM_SCREENSHOTS").expect("OM_SCREENSHOTS names a folder");
    let dir = std::path::Path::new(&dir);
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(dir.join(format!("{name}.png")))
        .expect("write png");
    let mut notes = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("readback.txt"))
        .expect("open readback.txt");
    writeln!(notes, "{name}: {line}").expect("write readback.txt");
}

fn open(empty: bool) -> common::App {
    open_provider(DocumentProvider::new(empty))
}

#[test]
fn replacing_a_layout_snapshot_with_the_same_ids_rebuilds_browse_lookups() {
    let replacement = Arc::new(AtomicBool::new(false));
    let mut provider = DocumentProvider::new(false);
    provider.replacement = Some(replacement.clone());
    let mut app = open_provider(provider);
    assert!(app.query_by_label("Outside the layout width").is_none());
    replacement.store(true, Ordering::SeqCst);
    // A provider can decline a stored layout and use generic Form instead.
    // It must not leave the previous snapshot's cached vector indices alive.
    common::select_layout(&mut app, "Other Card");
    common::wait_until(&mut app, "fallback form", |app| {
        app.query_by_label("CLIENT CARD").is_none() && app.query_by_label("Given").is_some()
    });
    common::select_layout(&mut app, "Client Card");
    common::wait_for_label(&mut app, "REPLACED CLIENT CARD");
    common::wait_for_label(&mut app, "Outside the layout width");
    assert!(app.query_by_label("CLIENT CARD").is_none());
    app.get_by_label("Contact: Clara Mason");
}

#[test]
fn browse_does_not_show_objects_in_the_layout_workspace() {
    let mut app = open(false);
    app.run_steps(3);
    app.get_by_label("CLIENT CARD");
    app.get_by_label("Contact: Clara Mason");
    assert!(app.query_by_label("Outside the layout width").is_none());
    assert!(app.query_by_label("Below the last layout part").is_none());
}

fn open_provider(provider: DocumentProvider) -> common::App {
    let total = provider.rows.len().to_string();
    let mut app = common::harness(Box::new(provider));
    common::fill(&mut app, "Host", "cards.example.invalid");
    common::fill(&mut app, "File", "Cards.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "CLIENT CARD");
    common::wait_until(&mut app, "record count", |app| {
        assert!(app.state().error().is_none());
        app.query_by_label(&total).is_some()
    });
    app
}

fn replace(app: &mut common::App, label: &str, value: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(app, label).type_text(value);
    app.run_steps(2);
}

fn click_blank_canvas(app: &mut common::App) {
    let pos = egui::pos2(1000.0, 700.0);
    app.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    app.run_steps(2);
}

#[test]
fn blank_canvas_commits_but_changing_fields_keeps_the_record_draft() {
    for reject in [false, true] {
        let mut provider = DocumentProvider::new(false);
        provider.writable = true;
        provider.reject_updates = reject;
        let mut app = open_provider(provider);
        replace(&mut app, "Given", "Canvas edit");
        common::text_field(&app, "Family").click();
        app.run_steps(2);
        assert!(common::unsaved(&app), "unsaved record changes");
        assert!(app.state().error().is_none());
        click_blank_canvas(&mut app);
        if reject {
            common::wait_for_label(&mut app, "The server rejected this edit.");
            common::click(&mut app, "OK");
            assert_eq!(
                common::text_field(&app, "Given").value().as_deref(),
                Some("Canvas edit")
            );
            assert_eq!(common::record_number(&app), "1");
            assert!(common::unsaved(&app), "unsaved record changes");
            common::revert_record(&mut app);
            assert_eq!(
                common::text_field(&app, "Given").value().as_deref(),
                Some("Clara")
            );
        } else {
            common::wait_for_label(&mut app, "Contact: Canvas edit Mason");
            assert!(!common::unsaved(&app));
        }
    }
    // In List view too, where the rows draw in the zoom layer over the
    // window's canvas.
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.list = true;
    let mut app = open_provider(provider);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("List edit".into()));
    app.run_steps(2);
    assert!(common::unsaved(&app), "unsaved record changes");
    click_blank_canvas(&mut app);
    common::wait_until(&mut app, "the List view commit", |app| {
        !common::unsaved(app) && !common::saving(app)
    });
    // And in Table view, zoomed: a cell takes the click meant for it, and
    // the canvas below the rows commits.
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    common::click(&mut app, "Table View");
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Zoom In");
    app.run_steps(2);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .expect("second record's Given editor")
        .click();
    app.run_steps(2);
    assert_eq!(common::record_number(&app), "2");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Table edit".into()));
    app.run_steps(2);
    assert!(common::unsaved(&app), "unsaved record changes");
    click_blank_canvas(&mut app);
    common::wait_until(&mut app, "the Table view commit", |app| {
        !common::unsaved(app) && !common::saving(app)
    });
}

/// Insert > Current User Name puts the user name at the insertion point of
/// the field being typed in, which a click on the menus leaves entered, and
/// the insertion point goes after it.
#[test]
fn the_insert_menu_puts_its_text_at_the_insertion_point() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    let family = |app: &common::App| common::text_field(app, "Family").value();
    common::text_field(&app, "Family").focus();
    app.run_steps(2);
    // After "Ma" of "Mason".
    for key in [
        egui::Key::Home,
        egui::Key::ArrowRight,
        egui::Key::ArrowRight,
    ] {
        app.key_press(key);
    }
    app.run_steps(2);
    common::open_menu(&mut app, "Insert");
    assert!(common::text_field(&app, "Family").is_focused());
    common::click(&mut app, "Current User Name");
    let name = om_data::host::machine::user_name();
    common::wait_until(&mut app, "the user name", |app| {
        family(app).as_deref() == Some(format!("Ma{name}son").as_str())
    });
    assert!(common::text_field(&app, "Family").is_focused());
    app.event(egui::Event::Text("!".into()));
    app.run_steps(2);
    assert_eq!(family(&app), Some(format!("Ma{name}!son")));
    assert!(common::unsaved(&app));
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Edit > Select All, Clear and Undo act on the field being typed in,
/// which a click on the menus leaves entered.
#[test]
fn the_edit_menu_selects_clears_and_undoes_in_the_field() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    let family = |app: &common::App| common::text_field(app, "Family").value();
    let edit = |app: &mut common::App, command: &str| {
        common::open_menu(app, "Edit");
        common::click(app, command);
        app.run_steps(2);
        assert!(common::text_field(app, "Family").is_focused(), "{command}");
    };
    common::text_field(&app, "Family").focus();
    app.run_steps(2);
    app.key_press(egui::Key::End);
    app.event(egui::Event::Text(" Jr".into()));
    app.run_steps(2);
    assert_eq!(family(&app).as_deref(), Some("Mason Jr"));
    edit(&mut app, "Select All");
    edit(&mut app, "Clear");
    assert_eq!(family(&app).as_deref(), Some(""));
    assert!(common::unsaved(&app));
    edit(&mut app, "Undo");
    assert_eq!(family(&app).as_deref(), Some("Mason Jr"));
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Return types a line break into an edit box, stored as FileMaker stores
/// it, a carriage return; the keypad's Enter commits the record. From a
/// field whose "Go to next object using" includes them, both go to the next
/// field, as Tab does.
#[test]
fn return_types_a_line_break_and_the_keypad_enter_commits_or_both_go_on() {
    let keypad_enter = |app: &mut common::App| {
        for pressed in [true, false] {
            app.event(om_gui::keypad::enter_event(pressed, egui::Modifiers::NONE));
        }
        app.run_steps(3);
    };
    let family = |app: &common::App| common::text_field(app, "Family").value();
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.given_next_keys = true;
    let mut app = open_provider(provider);
    replace(&mut app, "Family", "Keypad");
    app.key_press(egui::Key::Enter);
    app.event(egui::Event::Text("Two".into()));
    app.run_steps(3);
    assert!(common::text_field(&app, "Family").is_focused());
    assert_eq!(family(&app).as_deref(), Some("Keypad\rTwo"));
    assert!(common::unsaved(&app), "a line break keeps the draft");

    replace(&mut app, "Given", "Return");
    app.key_press(egui::Key::Enter);
    app.run_steps(3);
    assert!(common::text_field(&app, "Family").is_focused());
    assert!(common::unsaved(&app), "going on keeps the draft");

    replace(&mut app, "Given", "Enter");
    keypad_enter(&mut app);
    assert!(common::text_field(&app, "Family").is_focused());
    assert!(common::unsaved(&app), "going on keeps the draft");

    // A Return after a Tab in the same frame goes by the field the Tab
    // went to: from Given, which goes on with Return, to Family, which
    // takes the line break after its text.
    common::text_field(&app, "Given").focus();
    app.step();
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    app.input_mut().events.extend([
        key(egui::Key::Tab),
        key(egui::Key::Enter),
        egui::Event::Text("Q".into()),
    ]);
    app.run_steps(3);
    assert!(common::text_field(&app, "Family").is_focused());
    assert_eq!(family(&app).as_deref(), Some("Keypad\rTwo\rQ"));

    // Typing and the keypad's Enter in one frame (the harness's queued
    // events take a frame each): the editor applies the typing before the
    // commit takes its focus.
    app.input_mut().events.extend([
        egui::Event::Text(" Z".into()),
        om_gui::keypad::enter_event(true, egui::Modifiers::NONE),
        om_gui::keypad::enter_event(false, egui::Modifiers::NONE),
    ]);
    app.run_steps(3);
    common::wait_until(&mut app, "the commit", |app| !common::editing(app));
    assert!(!common::text_field(&app, "Family").is_focused());
    assert_eq!(family(&app).as_deref(), Some("Keypad\rTwo\rQ Z"));
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // The saved record read back, so that it does not land between the
    // keys below and the field they go to.
    common::wait_until(&mut app, "the saved record", |app| {
        app.state().is_settled() && !common::saving(app)
    });

    // Return, then Tab and typing, in one frame: Return goes on from Given
    // to Family, the Tab from Family on to Given, which takes the typing.
    common::text_field(&app, "Given").focus();
    app.step();
    app.input_mut().events.extend([
        key(egui::Key::Enter),
        key(egui::Key::Tab),
        egui::Event::Text("X".into()),
    ]);
    app.run_steps(4);
    assert!(common::text_field(&app, "Given").is_focused());
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("EnterX")
    );
    assert_eq!(family(&app).as_deref(), Some("Keypad\rTwo\rQ Z"));
}

/// Undo and Redo put the text back with its line breaks as the field
/// stored them: an `\n` or `\r\n` the user did not type is saved as it
/// was, not as the carriage return Return types.
#[test]
fn undo_and_redo_keep_the_line_breaks_the_field_stored() {
    let stored = "Ma\nson\r\nX";
    let family = |app: &common::App| common::text_field(app, "Family").value();
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.rows[0].values[1] = Some(stored.into());
    let mut app = open_provider(provider);
    common::text_field(&app, "Family").focus();
    app.run_steps(2);
    app.event(egui::Event::Text("Y".into()));
    app.run_steps(2);
    let typed = family(&app).expect("Family's value");
    assert_eq!(typed.replace('Y', ""), stored, "{typed:?}");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    app.run_steps(2);
    assert_eq!(family(&app).as_deref(), Some(stored));
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::Z,
    );
    app.run_steps(2);
    assert_eq!(family(&app), Some(typed.clone()));
    for pressed in [true, false] {
        app.event(om_gui::keypad::enter_event(pressed, egui::Modifiers::NONE));
    }
    app.run_steps(3);
    common::wait_until(&mut app, "the commit", |app| !common::editing(app));
    assert_eq!(family(&app), Some(typed));
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn clicking_an_existing_field_edits_text_with_create_and_delete_independent() {
    let mut provider = DocumentProvider::new(false);
    // Hosted providers can expose supported operations without claiming all
    // CRUD operations through the old global boolean.
    provider.capabilities = Some(RecordCapabilities {
        create: true,
        update: true,
        delete: false,
        editable_fields: Some(["Given".into()].into()),
        ..Default::default()
    });
    let mut app = open_provider(provider);
    assert!(!app
        .get_by_label("New Record")
        .accesskit_node()
        .is_disabled());
    assert!(app
        .get_by_label("Delete Record")
        .accesskit_node()
        .is_disabled());
    assert!(app
        .query_by_role_and_label(Role::TextInput, "Family")
        .is_none());
    app.get_by_label("Mason");
    // The editable field can arrive a frame after the record's text, while
    // the worker answers; wait for it rather than for a fixed number of
    // steps.
    common::wait_until(&mut app, "editable Given field", |app| {
        app.query_by_role_and_label(Role::TextInput, "Given")
            .is_some()
    });
    common::text_field(&app, "Given").click();
    app.step();
    assert!(common::text_field(&app, "Given").is_focused());
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Changed by clicking".into()));
    app.run_steps(2);
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Contact: Emery Chen");
    common::click(&mut app, "Previous");
    common::wait_for_label(&mut app, "Contact: Changed by clicking Mason");
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "new record field focus", |app| {
        common::record_number(app) == "3"
            && common::find_text_field(app, "Given").is_some_and(|f| f.is_focused())
    });
    app.event(egui::Event::Text("Newly created".into()));
    app.run_steps(2);
    common::save_record(&mut app);
    common::wait_for_label(&mut app, "Contact: Newly created ");
}

/// #690: a field the account's set gives view-only access is entered, and
/// what is typed into it is refused with FileMaker Pro's alert, the value
/// and the record unchanged. After OK the field is still entered.
#[test]
fn typing_into_a_field_the_set_refuses_alerts_and_keeps_its_value() {
    let mut provider = DocumentProvider::new(false);
    provider.capabilities = Some(RecordCapabilities {
        update: true,
        editable_fields: Some(["Given".into()].into()),
        refused: [("Family".into(), om_data::EditRefusal::ViewOnlyField)].into(),
        ..Default::default()
    });
    let mut app = open_provider(provider);
    common::wait_until(&mut app, "refused Family field", |app| {
        app.query_by_role_and_label(Role::TextInput, "Family")
            .is_some()
    });
    common::text_field(&app, "Family").click();
    app.run_steps(2);
    assert!(common::text_field(&app, "Family").is_focused());
    app.event(egui::Event::Text("x".into()));
    app.run_steps(2);
    common::wait_for_label(&mut app, om_data::FIELD_NOT_MODIFIABLE);
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert!(app.query_by_label(om_data::FIELD_NOT_MODIFIABLE).is_none());
    assert!(common::text_field(&app, "Family").is_focused());
    assert!(!common::unsaved(&app));
    app.get_by_label("Contact: Clara Mason");
    // Edit > Select All selects in it; Edit > Clear is refused as typing is.
    for command in ["Select All", "Clear"] {
        common::open_menu(&mut app, "Edit");
        common::click(&mut app, command);
        app.run_steps(2);
    }
    common::wait_for_label(&mut app, om_data::FIELD_NOT_MODIFIABLE);
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert_eq!(
        common::text_field(&app, "Family").value().as_deref(),
        Some("Mason")
    );
    assert!(!common::unsaved(&app));
}

fn click_at(app: &mut common::App, pos: egui::Pos2) {
    app.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    app.run_steps(3);
}

#[test]
fn the_first_click_places_the_caret_in_the_clicked_text() {
    for styled in [false, true] {
        for (source, index, expected) in [("Clara😀", 0, "XClara😀"), ("a😀éZ", 2, "a😀XéZ")]
        {
            let mut provider = DocumentProvider::new(false);
            provider.writable = true;
            provider.styled = styled;
            provider.rows[0].values[0] = Some(source.into());
            let mut app = open_provider(provider);
            common::wait_until(&mut app, "preview text for caret", |app| {
                rendered_text(app, source).is_some_and(|galley| {
                    !styled || galley.job.sections[0].format.font_id.size == 13.0
                })
            });
            assert!(app.query_by_role_and_label(Role::Label, source).is_none());
            let rect = common::text_field(&app, "Given").rect();
            let galley = rendered_text(&app, source).unwrap();
            let offset = galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .center()
                .x;
            // Real glyph geometry, including the emoji's width; no guessed
            // character widths or byte offsets in the synthetic click.
            click_at(
                &mut app,
                egui::pos2(rect.left() + offset + 0.05, rect.center().y),
            );
            app.event(egui::Event::Text("X".into()));
            app.run_steps(2);
            assert_eq!(
                common::text_field(&app, "Given").value().as_deref(),
                Some(expected)
            );
        }
    }
}

#[test]
fn a_list_click_keeps_its_caret_across_a_delayed_record_save() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.list = true;
    let (release, gate) = mpsc::channel();
    let (entered, saving) = mpsc::channel();
    provider.update_gate = Some(gate);
    provider.update_entered = Some(entered);
    let mut app = open_provider(provider);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("First draft".into()));
    app.run_steps(2);
    let rect = app
        .get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .rect();
    click_at(&mut app, egui::pos2(rect.left() + 1.0, rect.center().y));
    let saving_started = saving.recv_timeout(std::time::Duration::from_secs(2));
    let held_record = common::record_number(&app);
    // Always release the provider before an assertion can unwind the harness.
    release.send(()).unwrap();
    saving_started.unwrap();
    assert_eq!(held_record, "1");
    common::wait_until(&mut app, "second row caret after save", |app| {
        common::record_number(app) == "2"
            && app
                .get_all_by_role_and_label(Role::TextInput, "Given")
                .any(|f| f.is_focused() && f.value().as_deref() == Some("Emery"))
    });
    app.event(egui::Event::Text("X".into()));
    app.run_steps(2);
    assert!(app
        .get_all_by_role_and_label(Role::TextInput, "Given")
        .any(|f| f.value().as_deref() == Some("XEmery")));
}

#[test]
fn a_failed_list_navigation_does_not_keep_its_clicked_target() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.list = true;
    provider.reject_updates = true;
    let mut app = open_provider(provider);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Rejected draft".into()));
    app.run_steps(2);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .click();
    common::wait_for_label(&mut app, "The server rejected this edit.");
    assert_eq!(common::record_number(&app), "1");
    common::click(&mut app, "OK");
    common::revert_record(&mut app);
    common::wait_until(&mut app, "reverted first row", |app| {
        app.get_all_by_role_and_label(Role::TextInput, "Given")
            .next()
            .is_some_and(|f| f.value().as_deref() == Some("Clara"))
    });
    click_blank_canvas(&mut app);
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "second row without stale focus", |app| {
        common::record_number(app) == "2"
    });
    app.run_steps(3);
    assert!(!app
        .get_all_by_role_and_label(Role::TextInput, "Given")
        .any(|f| f.is_focused()));
}

#[test]
fn clicking_and_tabbing_between_duplicate_field_objects_keeps_the_specific_editor() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.duplicate_field = true;
    let mut app = open_provider(provider);
    common::wait_until(&mut app, "duplicate field controls", |app| {
        app.get_all_by_role_and_label(Role::TextInput, "Given")
            .count()
            == 2
    });
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(3);
    assert!(app
        .get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .is_focused());
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.run_steps(2);
    app.key_press(egui::Key::Tab);
    app.run_steps(3);
    assert!(app
        .get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .is_focused());
    app.key_press(egui::Key::Tab);
    app.run_steps(3);
    assert!(common::text_field(&app, "Family").is_focused());
}

#[test]
fn clicking_another_list_record_keeps_its_editor_after_a_delayed_save() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.list = true;
    let (release, gate) = mpsc::channel();
    let (entered, saving) = mpsc::channel();
    provider.update_gate = Some(gate);
    provider.update_entered = Some(entered);
    let mut app = open_provider(provider);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .next()
        .unwrap()
        .focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("First draft".into()));
    app.run_steps(2);
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .unwrap()
        .click();
    app.run_steps(3);
    let saving_started = saving.recv_timeout(std::time::Duration::from_secs(2));
    let held_record = common::record_number(&app);
    app.event(egui::Event::Text("while saving".into()));
    app.run_steps(3);
    // Release the worker before any assertion can unwind the harness.
    release.send(()).unwrap();
    saving_started.unwrap();
    assert_eq!(held_record, "1");
    common::wait_until(&mut app, "second row editor after save", |app| {
        common::record_number(app) == "2"
            && app
                .get_all_by_role_and_label(Role::TextInput, "Given")
                .any(|field| field.is_focused() && field.value().as_deref() == Some("Emery"))
    });
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Second draft".into()));
    app.run_steps(2);
    common::click(&mut app, "Previous");
    common::wait_until(&mut app, "first row retained", |app| {
        common::record_number(app) == "1"
            && app
                .get_all_by_role_and_label(Role::TextInput, "Given")
                .next()
                .is_some_and(|field| field.value().as_deref() == Some("First draft"))
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "second row retained", |app| {
        common::record_number(app) == "2"
            && app
                .get_all_by_role_and_label(Role::TextInput, "Given")
                .any(|field| field.value().as_deref() == Some("Second draft"))
    });
}

#[test]
fn table_cells_edit_the_clicked_record_and_commit_before_navigation() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    common::click(&mut app, "Table View");
    app.get_all_by_role_and_label(Role::TextInput, "Given")
        .nth(1)
        .expect("second record's Given editor")
        .click();
    app.run_steps(2);
    assert_eq!(common::record_number(&app), "2");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("Table edit".into()));
    app.run_steps(2);
    common::click(&mut app, "Previous");
    common::wait_until(&mut app, "first record", |app| {
        common::record_number(app) == "1"
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "retained table edit", |app| {
        app.get_all_by_role_and_label(Role::TextInput, "Given")
            .nth(1)
            .and_then(|node| node.value())
            .as_deref()
            == Some("Table edit")
    });
}

#[test]
fn moving_within_the_page_before_its_post_save_reload_still_shows_the_save() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let (release, gate) = mpsc::channel();
    provider.reload_gate = Some(gate);
    let mut app = open_provider(provider);
    replace(&mut app, "Given", "Saved edit");
    // Next saves the edit and moves; the page reload after the save is slow.
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "saved and moved", |app| {
        common::record_number(app) == "2"
    });
    // Back to the first record, still on the page from before the save.
    common::click(&mut app, "Previous");
    release.send(()).unwrap();
    common::wait_until(&mut app, "reload", |app| app.state().is_settled());
    app.get_by_label("Contact: Saved edit Mason");
}

#[test]
fn browse_creates_updates_reverts_and_deletes_records() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    common::click(&mut app, "New Record");
    // The record number changes before the page with the new record's
    // fields arrives.
    common::wait_until(&mut app, "created record", |app| {
        common::record_number(app) == "3" && common::find_text_field(app, "Given").is_some()
    });
    replace(&mut app, "Given", "New client");
    replace(&mut app, "Family", "Original");
    common::save_record(&mut app);
    common::wait_for_label(&mut app, "Contact: New client Original");
    replace(&mut app, "Family", "Discarded");
    common::revert_record(&mut app);
    assert_eq!(
        common::text_field(&app, "Family").value().as_deref(),
        Some("Original")
    );
    replace(&mut app, "Family", "Retained");
    // Navigation commits the edit before moving to the other record.
    common::click(&mut app, "Previous");
    common::wait_until(&mut app, "previous record", |app| {
        common::record_number(app) == "2"
    });
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Contact: New client Retained");
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Cancel");
    assert_eq!(common::record_number(&app), "3");
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "deleted record", |app| {
        common::record_number(app) == "2"
    });
    common::wait_for_label(&mut app, "Contact: Emery Chen");
}

/// Records > Revert Record... (#871; FileMaker Pro 26, observed): disabled
/// with nothing to revert. Its alert has no default button, so Return
/// leaves it open, and Escape keeps the draft. Revert restores the record
/// and leaves no field focused.
#[test]
fn revert_record_asks_with_no_default_button() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    let message = om_data::scripts::revert_record::MESSAGE;
    let revert_enabled = |app: &common::App| {
        !app.get_by_label("Revert Record...")
            .accesskit_node()
            .is_disabled()
    };
    common::open_menu(&mut app, "Records");
    assert!(!revert_enabled(&app), "nothing to revert");
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    replace(&mut app, "Given", "Draft");
    common::open_menu(&mut app, "Records");
    assert!(revert_enabled(&app), "an edit to revert");
    common::click(&mut app, "Revert Record...");
    common::wait_for_label(&mut app, message);
    app.key_press(egui::Key::Enter);
    app.run_steps(4);
    app.get_by_label(message);
    app.key_press(egui::Key::Escape);
    common::wait_until(&mut app, "the alert closed", |app| {
        app.query_by_label(message).is_none() && !app.state().script_running()
    });
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Draft")
    );
    assert!(common::unsaved(&app), "Escape keeps the draft");
    common::revert_record(&mut app);
    common::wait_until(&mut app, "reverted", |app| {
        common::text_field(app, "Given").value().as_deref() == Some("Clara")
            && !app.state().script_running()
    });
    assert!(!common::unsaved(&app));
    assert!(!common::text_field(&app, "Given").is_focused());
    assert!(!common::text_field(&app, "Family").is_focused());
}

#[test]
fn a_rejected_save_keeps_the_draft_and_stops_navigation() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.reject_updates = true;
    let mut app = open_provider(provider);
    replace(&mut app, "Given", "Unsaved draft");
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "The server rejected this edit.");
    common::click(&mut app, "OK");
    assert_eq!(common::record_number(&app), "1");
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Unsaved draft")
    );
    common::revert_record(&mut app);
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Clara")
    );
}

#[test]
fn refreshing_untouched_fields_displays_external_edits_without_creating_a_draft() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.external_update = Some("Changed in another client".into());
    let mut app = open_provider(provider);
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Clara")
    );
    app.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::R,
    );
    common::wait_until(&mut app, "external edit", |app| {
        common::find_text_field(app, "Given")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Changed in another client")
    });
    assert!(!common::unsaved(&app));
}

#[test]
fn refreshing_a_dirty_record_saves_before_fetching_and_blocks_fetch_after_rejection() {
    for reject in [false, true] {
        let mut provider = DocumentProvider::new(false);
        provider.writable = true;
        provider.reject_updates = reject;
        if reject {
            provider.external_update = Some("Changed by another client".into());
        }
        let events = provider.events.clone();
        let mut app = open_provider(provider);
        events.lock().unwrap().clear();
        replace(&mut app, "Given", "Draft before refresh");
        app.key_press_modifiers(
            egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            egui::Key::R,
        );
        if reject {
            common::wait_for_label(&mut app, "The server rejected this edit.");
            assert_eq!(
                *events.lock().unwrap(),
                vec!["update"],
                "a rejected save must not fetch a new record version"
            );
            common::click(&mut app, "OK");
            assert_eq!(
                common::text_field(&app, "Given").value().as_deref(),
                Some("Draft before refresh")
            );
            assert_eq!(common::record_number(&app), "1");
            // Revert discards the draft and fetches the current server value,
            // rather than retaining the old record version after the error.
            common::revert_record(&mut app);
            common::wait_until(&mut app, "fresh value after reverting", |app| {
                common::find_text_field(app, "Given")
                    .and_then(|f| f.value())
                    .as_deref()
                    == Some("Changed by another client")
            });
            // Revert writes nothing. It runs Revert Record/Request (51),
            // whose host reads the record before the window reads it again.
            let events = events.lock().unwrap();
            assert_eq!(events[0], "update");
            assert!(events[1..].iter().all(|e| *e == "read"), "{events:?}");
        } else {
            common::wait_for_label(&mut app, "Contact: Draft before refresh Mason");
            assert_eq!(
                *events.lock().unwrap(),
                vec!["update", "read"],
                "refresh must commit first and fetch once"
            );
            assert!(!common::unsaved(&app));
        }
    }
}

#[test]
fn a_list_layout_repeats_the_body_and_fetches_as_it_scrolls() {
    let mut provider = DocumentProvider::new(false);
    provider.list = true;
    provider.rows = (0..90)
        .map(|index| Record {
            id: index + 1,
            values: vec![
                Some(format!("Client {index}")),
                Some(format!("Family {index}")),
            ],
        })
        .collect();
    let mut app = open_provider(provider);
    assert_eq!(app.state().view(), om_gui::ViewMode::List);
    app.get_by_label("Client 0");
    app.get_by_label("Client 1");
    app.get_by_label("Family 2");
    assert_eq!(
        app.get_all_by_label("CLIENT CARD").count(),
        1,
        "header is not repeated"
    );
    app.get_by_label("Client 1").hover();
    app.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -1600.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    common::wait_for_label(&mut app, "Client 40");
    assert_eq!(
        common::record_number(&app),
        "1",
        "scrolling loads pages without changing the current record"
    );
}

fn select_tab(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Tab, label).click();
    app.run_steps(3);
}

fn assert_browse_has_no_authoring_captions(app: &common::App) {
    for caption in ["Header", "Body", "Footer", "Field", "Text", "Rectangle"] {
        assert!(
            app.query_by_label(caption).is_none(),
            "Browse painted authoring caption {caption}"
        );
    }
}

#[test]
fn browse_renders_file_text_theme_and_current_record_values() {
    let mut app = open(false);
    app.get_by_label("Clara");
    app.get_by_label("Mason");
    app.get_by_label("Contact: Clara Mason");
    assert_eq!(common::record_number(&app), "1");
    assert_browse_has_no_authoring_captions(&app);
    assert!(
        app.output()
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape,
        Shape::Rect(rect) if rect.fill == Color32::from_rgb(64, 128, 191)
            && (rect.rect.width() - 90.0).abs() < 0.1
            && (rect.rect.height() - 35.0).abs() < 0.1)),
        "the rectangle must use the file's theme fill"
    );

    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Contact: Emery Chen");
    assert_eq!(common::record_number(&app), "2");
    app.get_by_label("Emery");
    app.get_by_label("Chen");
    app.get_by_label("CLIENT CARD");
    assert!(app.query_by_label("Clara").is_none());
    assert!(app.query_by_label("Mason").is_none());
    assert!(app.query_by_label("Contact: Clara Mason").is_none());
}

fn rendered_text(app: &common::App, text: &str) -> Option<Arc<egui::Galley>> {
    fn find(shape: &Shape, text: &str) -> Option<Arc<egui::Galley>> {
        match shape {
            Shape::Text(shape) if shape.galley.job.text == text => Some(shape.galley.clone()),
            Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, text)),
            _ => None,
        }
    }
    app.output()
        .shapes
        .iter()
        .find_map(|shape| find(&shape.shape, text))
}

#[test]
fn stored_and_calculated_styles_render_while_unfocused_and_drafts_remain_editable() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.styled = true;
    let events = provider.events.clone();
    let mut app = open_provider(provider);
    let red = Color32::from_rgb(221, 0, 0);
    common::wait_for_label(&mut app, "Clara!");
    assert_eq!(
        rendered_text(&app, "Clara").unwrap().job.sections[0]
            .format
            .color,
        red
    );
    assert_eq!(
        rendered_text(&app, "Clara!").unwrap().job.sections[0]
            .format
            .color,
        red
    );
    let label = rendered_text(&app, "CLIENT CARD").unwrap();
    assert_eq!(label.job.sections.len(), 2);
    assert_eq!(label.job.sections[0].format.color, Color32::BLUE);
    assert_eq!(label.job.sections[0].format.font_id.size, 8.0);
    assert_eq!(label.job.sections[1].format.font_id.size, 14.0);
    let field = rendered_text(&app, "Clara").unwrap();
    assert_eq!(field.job.sections[0].format.font_id.size, 13.0);
    assert_eq!(
        field.job.sections[0].format.font_id.family,
        egui::FontFamily::Name("document-arial-1-0".into())
    );

    // Enter the regular editor, then leave a draft on the canvas. Its
    // changed text must not reuse the stored value's character ranges.
    replace(&mut app, "Given", "Edited");
    common::text_field(&app, "Family").focus();
    common::wait_until(&mut app, "unfocused draft", |app| {
        rendered_text(app, "Edited").is_some() && rendered_text(app, "Edited!").is_some()
    });
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("Edited")
    );
    assert!(rendered_text(&app, "Edited")
        .unwrap()
        .job
        .sections
        .iter()
        .all(|section| section.format.color != red));
    assert!(
        !events.lock().unwrap().contains(&"update"),
        "changing fields keeps the record draft"
    );

    common::revert_record(&mut app);
    common::wait_until(&mut app, "restored styled value", |app| {
        rendered_text(app, "Clara").is_some_and(|g| g.job.sections[0].format.color == red)
    });
}

#[test]
fn conditional_color_overrides_character_colors_but_not_placeholders() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.styled = true;
    provider.conditional_color = true;
    let mut app = open_provider(provider);
    common::wait_for_label(&mut app, "Clara!");
    assert_eq!(
        rendered_text(&app, "Clara").unwrap().job.sections[0]
            .format
            .color,
        Color32::GREEN
    );
    assert!(rendered_text(&app, "CLIENT CARD")
        .unwrap()
        .job
        .sections
        .iter()
        .all(|section| section.format.color == Color32::GREEN));
    assert_eq!(
        rendered_text(&app, "Clara").unwrap().job.sections[0]
            .format
            .font_id
            .size,
        13.0
    );
    common::text_field(&app, "Given").focus();
    app.run_steps(4);
    assert!(rendered_text(&app, "Clara")
        .unwrap()
        .job
        .sections
        .iter()
        .all(|section| section.format.color != Color32::GREEN));

    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.styled = true;
    provider.conditional_color = true;
    provider.rows[0].values[0] = None;
    let mut empty = open_provider(provider);
    common::wait_until(&mut empty, "placeholder", |app| {
        rendered_text(app, "Given name").is_some()
    });
    assert!(rendered_text(&empty, "Given name")
        .unwrap()
        .job
        .sections
        .iter()
        .all(|section| section.format.color != Color32::GREEN));
}

#[test]
fn nested_tabs_paint_only_active_panels_and_keep_selection_when_records_change() {
    let mut app = open(false);
    app.get_by_label("Summary content");
    app.get_by_label("Home details");
    assert!(app.query_by_label("Work details").is_none());
    assert!(app.query_by_label("Notes content").is_none());

    select_tab(&mut app, "Work");
    app.get_by_label("Work details");
    assert!(app.query_by_label("Home details").is_none());
    select_tab(&mut app, "Notes");
    app.get_by_label("Notes content");
    for hidden in ["Summary content", "Work details", "Home details"] {
        assert!(
            app.query_by_label(hidden).is_none(),
            "inactive content: {hidden}"
        );
    }
    assert!(app.query_by_role_and_label(Role::Tab, "Work").is_none());

    select_tab(&mut app, "Summary");
    app.get_by_label("Work details");
    assert!(app.query_by_label("Home details").is_none());
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Emery");
    app.get_by_label("Work details");
    assert!(app.query_by_label("Home details").is_none());
}

#[test]
fn empty_found_set_keeps_the_static_layout_and_tab_controls() {
    let mut app = open(true);
    app.get_by_label("CLIENT CARD");
    app.get_by_label("Given name");
    app.get_by_label("Family name");
    app.get_by_label("Home details");
    assert!(
        app.query_by_label("No records").is_none(),
        "the static layout stays visible"
    );
    assert!(app.query_by_label("Clara").is_none());
    assert!(app.get_by_label("Next").accesskit_node().is_disabled());
    assert_browse_has_no_authoring_captions(&app);
    select_tab(&mut app, "Notes");
    app.get_by_label("Notes content");
    assert!(app.query_by_label("Home details").is_none());
}

#[test]
fn zoomed_form_keeps_chrome_size_and_edits_the_clicked_field() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    let mut app = open_provider(provider);
    let next = app.get_by_label("Next").rect();
    let field = common::text_field(&app, "Given").rect();
    common::click(&mut app, "View");
    common::click(&mut app, "Zoom In");
    app.run_steps(2);
    assert_eq!(app.get_by_label("Next").rect(), next);
    let enlarged = common::text_field(&app, "Given").rect();
    assert!((enlarged.width() - field.width() * 1.5).abs() < 0.1);
    let galley = rendered_text(&app, "Clara").unwrap();
    let offset = galley
        .pos_from_cursor(egui::text::CCursor::new(2))
        .center()
        .x;
    click_at(
        &mut app,
        egui::pos2(enlarged.left() + offset * 1.5 + 0.05, enlarged.center().y),
    );
    app.event(egui::Event::Text("X".into()));
    app.run_steps(2);
    assert_eq!(
        common::text_field(&app, "Given").value().as_deref(),
        Some("ClXara")
    );
    replace(&mut app, "Given", "Zoom edit");
    common::text_field(&app, "Family").click();
    app.run_steps(2);
    assert!(common::unsaved(&app), "unsaved record changes");
    click_blank_canvas(&mut app);
    common::wait_for_label(&mut app, "Contact: Zoom edit Mason");
    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "Contact: Emery Chen");
    assert_eq!(app.get_by_label("Next").rect(), next);
}

/// A manual capture of the web viewer's own note (#391): one window, at
/// rest and with its record being edited, which takes no bar (#871), so
/// the anchored viewer keeps its size over the picture.
/// `OM_SCREENSHOTS=<folder> cargo test -p om-gui --test document
/// web_viewer_note -- --ignored`
#[test]
#[ignore = "manual capture: requires OM_SCREENSHOTS, a folder for its PNGs"]
fn web_viewer_note_over_a_picture_at_rest_and_while_editing() {
    let mut provider = DocumentProvider::new(false);
    provider.writable = true;
    provider.web_viewer = true;
    let mut app = open_provider(provider);
    // The picture decodes on a background thread.
    common::wait_until(&mut app, "the picture", |app| !app.ctx.has_pending_images());
    shot(
        &mut app,
        "web-viewer-note-at-rest",
        "the web viewer over the picture, no record edits",
    );
    replace(&mut app, "Given", "Draft");
    common::text_field(&app, "Family").click();
    app.run_steps(2);
    assert!(common::unsaved(&app), "unsaved record changes");
    shot(
        &mut app,
        "web-viewer-note-record-edits",
        "the same window with its record being edited",
    );
}
