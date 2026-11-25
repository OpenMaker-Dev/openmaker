//! Container fields in Browse, using only a synthetic layout and records:
//! a stored picture shows scaled by each object's graphic format, a file
//! shows its icon and name without its data being downloaded, and a
//! picture that does not decode leaves the field empty.
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::epaint::Shape;
use egui::{Rect as Area, TextureId};
use egui_kittest::kittest::{NodeT as _, Queryable};
use om_data::calc::eval::StoredContainer;
use om_data::calc::{Container, Ctx, Date, Locale, StreamType, Value, ValueType};
use om_data::containers::MemberInfo;
use om_data::host::{
    CalcData, CalcNeed, CalcSchema, FieldDef, MemorySource, RecordValues, Recorder,
};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordCapabilities, RecordPage, RenderLayout,
};
use om_layout::{
    FieldObject, FieldSpec, GraphicFormat, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect,
};

const THINGS: u64 = 0xd00001;
const T: u64 = 0x8001;
/// Fields, and the member each one's value names.
const FIELDS: [(u64, &str, u8); 6] = [
    (1, "Photo", 1),
    (2, "Cropped", 1),
    (3, "Attachment", 2),
    (4, "Broken", 3),
    (5, "Document", 4),
    (6, "Pages", 5),
];
type Reads = Arc<Mutex<Vec<CalcNeed>>>;

/// A 40 x 20 PNG: its left half red, its right half blue.
fn png() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_fn(40, 20, |x, _| {
        image::Rgba(if x < 20 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        })
    })
    .write_to(&mut bytes, image::ImageFormat::Png)
    .unwrap();
    bytes.into_inner()
}

/// A one-page PDF, 200 x 100 points, all red.
fn pdf() -> Vec<u8> {
    use pdf_writer::{Content, Pdf, Rect, Ref};
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    pdf.page(Ref::new(3))
        .parent(Ref::new(2))
        .media_box(Rect::new(0.0, 0.0, 200.0, 100.0))
        .contents(Ref::new(4));
    let mut content = Content::new();
    content
        .set_fill_rgb(1.0, 0.0, 0.0)
        .rect(0.0, 0.0, 200.0, 100.0)
        .fill_nonzero();
    pdf.stream(Ref::new(4), &content.finish());
    pdf.finish()
}

/// Member 1 a picture, 2 a text file, 3 a picture that does not decode,
/// 4 a PDF FileMaker did not render (a file path, a `PDF ` stream).
fn member(member: u8) -> (MemberInfo, Container) {
    let (path_type, name, kind, data) = match member {
        1 => ("image", "two.png", *b"PNGf", png()),
        2 => ("file", "notes.txt", *b"FILE", b"notes".to_vec()),
        4 => ("file", "page.pdf", *b"PDF ", pdf()),
        5 => ("image", "pages.pdf", *b"PDF ", pages_pdf()),
        _ => (
            "image",
            "broken.png",
            *b"PNGf",
            b"\x89PNG\r\n\x1a\n broken".to_vec(),
        ),
    };
    let info = MemberInfo {
        owner: None,
        name: name.into(),
        md5: [member; 16],
        path_type: path_type.into(),
    };
    let mut data = Container {
        streams: vec![(StreamType(kind), data)],
        ..Container::default()
    };
    // As FileMaker Pro stores a PDF it inserted: an `image` path, and a
    // preview picture beside the PDF (containers.md 6.2).
    if member == 5 {
        data.streams.insert(0, (StreamType(*b"PNGf"), png()));
    }
    data.library = info.value(T, &[member]).library;
    (info, data)
}

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Containers", Locale::en_us());
    s.add_table(T, "Things");
    s.add_occurrence(THINGS, "Things", T);
    for (id, name, _) in FIELDS {
        s.add_field(T, FieldDef::data(id, name, ValueType::Container));
    }
    s.add_layout(7, "Containers", Some(THINGS));
    s
}

struct Provider {
    reads: Reads,
    /// The layout also shows Pages in a container optimized for
    /// interactive content (#200).
    interactive: bool,
}

impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Containers.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 7,
            name: "Containers".into(),
        }])
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: FIELDS
                .map(|(_, name, _)| Field {
                    name: name.into(),
                    kind: FieldKind::Container,
                })
                .into(),
            records: [Record {
                id: 1,
                values: FIELDS
                    .map(|(.., member)| Some(om_data::containers::reference_text(&[member])))
                    .into(),
            }]
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect(),
            total: 1,
        })
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
        Ok(Some(document(self.interactive)))
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.reads.lock().unwrap().extend_from_slice(needs);
        let schema = schema();
        let mut source = MemorySource::new();
        let mut row = RecordValues::default();
        for (id, _, number) in FIELDS {
            row.set(
                id,
                1,
                Value::Container(member(number).0.value(T, &[number])),
            );
        }
        source.put_record(T, 1, row);
        source.set_found_set(THINGS, &[1], None);
        let recorder = Recorder::new(&source);
        let cx = Ctx::new(&schema.locale, Date::from_ymd(2026, 11, 13).unwrap());
        let mut library = CalcData::default();
        for need in needs {
            match need {
                CalcNeed::Member { table, member: key } => {
                    let info = member(key[0]).0;
                    library
                        .library
                        .members
                        .insert((*table, key.clone()), Some(Arc::new(info)));
                }
                CalcNeed::MemberData { table, member: key } => {
                    let stored = StoredContainer {
                        data: member(key[0]).1,
                        external: false,
                        intact: true,
                    };
                    library
                        .library
                        .data
                        .insert((*table, key.clone()), Some(Arc::new(stored)));
                }
                need => recorder
                    .fetch(&schema, need, &cx)
                    .map_err(|e| DataError::new(e.to_string()))?,
            }
        }
        let mut data = recorder.take();
        data.library = library.library;
        Ok(data)
    }
}

fn field(id: u64, field: u64, x: f64, size: f64, format: u16) -> LayoutObject {
    LayoutObject {
        id,
        object_type: 1,
        bounds: Some(Rect {
            left: x,
            top: 40.0,
            right: x + size,
            bottom: 40.0 + size,
            ..Default::default()
        }),
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: 1065089,
                field_key: field,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        graphic_format: Some(GraphicFormat(format)),
        ..Default::default()
    }
}

fn document(interactive: bool) -> RenderLayout {
    RenderLayout {
        layout: Layout {
            id: 7,
            name: "Containers".into(),
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
                root: (1..=if interactive { 6 } else { 5 }).collect(),
                objects: [
                    // Reduce or enlarge, keeping proportions, centered.
                    field(1, 1, 20.0, 100.0, 7),
                    // Crop, centered.
                    field(2, 2, 140.0, 20.0, 0),
                    field(3, 3, 180.0, 120.0, 5),
                    field(4, 4, 320.0, 60.0, 5),
                    field(5, 5, 400.0, 100.0, 7),
                    // Optimized for interactive content, as FileMaker Pro
                    // stores it (containers.md 6.6): 0x0100 over 7.
                    field(6, 6, 500.0, 100.0, 0x0107),
                ]
                .into_iter()
                .take(if interactive { 6 } else { 5 })
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: FIELDS
            .iter()
            .map(|&(id, name, _)| (id, name.to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// The areas of the pictures the last frame painted, and the areas they
/// were clipped to.
fn pictures(app: &common::App) -> Vec<(Area, Area)> {
    fn walk(shape: &Shape, clip: Area, out: &mut Vec<(Area, Area)>) {
        match shape {
            Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => {
                out.push((mesh.calc_bounds(), clip));
            }
            Shape::Vec(items) => items.iter().for_each(|s| walk(s, clip, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for shape in &app.output().shapes {
        walk(&shape.shape, shape.clip_rect, &mut out);
    }
    out
}

fn painted_text(app: &common::App, text: &str) -> bool {
    fn has(shape: &Shape, text: &str) -> bool {
        match shape {
            Shape::Text(t) => t.galley.job.text == text,
            Shape::Vec(items) => items.iter().any(|s| has(s, text)),
            _ => false,
        }
    }
    app.output().shapes.iter().any(|s| has(&s.shape, text))
}

fn near(a: Area, b: Area) -> bool {
    (a.min - b.min).length() < 0.6 && (a.max - b.max).length() < 0.6
}

#[test]
fn container_fields_show_pictures_scaled_and_files_by_name() {
    // PDF data is read by the client's worker (#566).
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    let reads = Reads::default();
    let mut app = common::harness(Box::new(Provider {
        reads: reads.clone(),
        interactive: false,
    }));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/containers.example.invalid/Containers.fmp12",
    );
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the pictures and the page", |app| {
        pictures(app).len() == 3
    });
    let frame = |app: &common::App, name: &str| app.get_by_label(name).rect();
    let (photo, cropped, broken) = (
        frame(&app, "Photo"),
        frame(&app, "Cropped"),
        frame(&app, "Broken"),
    );
    let shown = pictures(&app);
    // 40 x 20 reduced or enlarged into 100 x 100: 100 x 50, centered.
    let expected = Area::from_min_size(photo.min + egui::vec2(0.0, 25.0), egui::vec2(100.0, 50.0));
    assert!(
        shown.iter().any(|(area, _)| near(*area, expected)),
        "{shown:?} {photo:?}"
    );
    // The PDF's 200 x 100 point page reduced into 100 x 100: 100 x 50.
    let document = frame(&app, "Document");
    let page = Area::from_min_size(
        document.min + egui::vec2(0.0, 25.0),
        egui::vec2(100.0, 50.0),
    );
    assert!(
        shown.iter().any(|(area, _)| near(*area, page)),
        "{shown:?} {document:?}"
    );
    assert!(
        !painted_text(&app, "page.pdf"),
        "a drawn page's name is not shown"
    );
    // Cropped into 20 x 20: its middle 20 pixels, clipped to the frame.
    assert!(
        shown
            .iter()
            .any(|(area, clip)| near(*area, cropped) && clip.contains_rect(cropped)),
        "{shown:?} {cropped:?}"
    );
    assert!(painted_text(&app, "notes.txt"));
    assert!(
        !painted_text(&app, "two.png"),
        "a picture's name is not shown"
    );
    // The broken picture's decode finishes too; it stays empty.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        app.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pictures(&app)
        .iter()
        .all(|(area, _)| !area.intersects(broken)));
    let reads = reads.lock().unwrap();
    assert!(reads.contains(&CalcNeed::MemberData {
        table: T,
        member: vec![1]
    }));
    assert!(
        !reads.contains(&CalcNeed::MemberData {
            table: T,
            member: vec![2]
        }),
        "a file's data is not downloaded to show it"
    );
}

/// Four pages, each filled with one color: 200 x 100 red, 200 x 300
/// green, 100 x 100 blue, and 400 x 50 black, shorter than half the view.
fn pages_pdf() -> Vec<u8> {
    use pdf_writer::{Content, Pdf, Rect, Ref};
    let pages = [
        (200.0, 100.0, (1.0, 0.0, 0.0)),
        (200.0, 300.0, (0.0, 1.0, 0.0)),
        (100.0, 100.0, (0.0, 0.0, 1.0)),
        (400.0, 50.0, (0.0, 0.0, 0.0)),
    ];
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2))
        .kids((0..4).map(|i| Ref::new(3 + 2 * i)))
        .count(4);
    for (i, (w, h, (r, g, b))) in pages.into_iter().enumerate() {
        let (page, stream) = (Ref::new(3 + 2 * i as i32), Ref::new(4 + 2 * i as i32));
        pdf.page(page)
            .parent(Ref::new(2))
            .media_box(Rect::new(0.0, 0.0, w, h))
            .contents(stream);
        let mut content = Content::new();
        content
            .set_fill_rgb(r, g, b)
            .rect(0.0, 0.0, w, h)
            .fill_nonzero();
        pdf.stream(stream, &content.finish());
    }
    pdf.finish()
}

/// The PDF view of an interactive container (#200), on its own: a PDF's
/// pages one under the other at the view's width, each drawn by the PDF
/// worker as it comes into view at its own shape, and the page bar going
/// to the next page and saying which shows. (Which containers are
/// interactive is not decoded yet; the field objects of a layout show
/// none this way.)
#[test]
fn the_pdf_view_scrolls_and_pages_through_every_page() {
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    let frame = Area::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 160.0));
    let data: Arc<[u8]> = Arc::from(pages_pdf());
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(240.0, 200.0))
        .build_ui_state(
            |ui, (viewer, data): &mut (om_gui::PdfViewer, Arc<[u8]>)| viewer.show(ui, data, frame),
            (om_gui::PdfViewer::default(), data),
        );
    // The page meshes the last frame painted, by their size.
    let drawn = |harness: &egui_kittest::Harness<'_, _>| -> Vec<egui::Vec2> {
        fn walk(shape: &Shape, out: &mut Vec<egui::Vec2>) {
            match shape {
                Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => {
                    out.push(mesh.calc_bounds().size());
                }
                Shape::Vec(items) => items.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for shape in &harness.output().shapes {
            walk(&shape.shape, &mut out);
        }
        out
    };
    let page_of = |harness: &egui_kittest::Harness<'_, _>, n: usize| {
        harness.query_by_label(&format!("Page {n} of 4")).is_some()
    };
    // A page's drawing, its width the first's.
    let shaped = |sizes: &[egui::Vec2], aspect: f32| {
        sizes
            .iter()
            .any(|s| s.x > 100.0 && (s.y / s.x - aspect).abs() < 0.02)
    };
    let started = Instant::now();
    let wait = |harness: &mut egui_kittest::Harness<'_, _>,
                what: &str,
                done: &dyn Fn(&egui_kittest::Harness<'_, _>) -> bool| {
        loop {
            harness.step();
            if done(harness) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "{what}: {:?}",
                drawn(harness)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // The first page (2:1) shows, and the second's top under it.
    wait(&mut harness, "the first page", &|h| {
        page_of(h, 1) && shaped(&drawn(h), 0.5)
    });
    assert!(
        !shaped(&drawn(&harness), 1.0),
        "the third page, out of view, is not drawn"
    );
    harness.get_by_label("›").click();
    wait(&mut harness, "the second page", &|h| {
        page_of(h, 2) && shaped(&drawn(h), 1.5)
    });
    harness.get_by_label("›").click();
    wait(&mut harness, "the third page", &|h| {
        page_of(h, 3) && shaped(&drawn(h), 1.0)
    });
    // The last page, too short to reach the view's middle, still shows as
    // the page at the end of the view.
    harness.get_by_label("›").click();
    wait(&mut harness, "the fourth page", &|h| {
        page_of(h, 4) && shaped(&drawn(h), 0.125)
    });
    assert!(
        harness.get_by_label("›").accesskit_node().is_disabled(),
        "no page after the last"
    );
}

/// A container optimized for interactive content (`0x0100` of its graphic
/// format, containers.md 6.6) shows its PDF as the PDF view, every page to
/// page through, while one optimized for images shows its PDF's first page
/// as a picture (#200).
#[test]
fn an_interactive_container_shows_every_page_of_its_pdf() {
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    let mut app = common::harness(Box::new(Provider {
        reads: Reads::default(),
        interactive: true,
    }));
    common::fill(
        &mut app,
        "Path",
        "fmnet:/containers.example.invalid/Containers.fmp12",
    );
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the interactive container's pages", |app| {
        app.query_by_label("Page 1 of 4").is_some()
    });
    // Document, optimized for images, still shows its page as a picture.
    let document = app.get_by_label("Document").rect();
    let page = Area::from_min_size(
        document.min + egui::vec2(0.0, 25.0),
        egui::vec2(100.0, 50.0),
    );
    common::wait_until(&mut app, "Document's page as a picture", |app| {
        pictures(app).iter().any(|(area, _)| near(*area, page))
    });
    app.get_by_label("›").click();
    common::wait_until(&mut app, "the second page", |app| {
        app.query_by_label("Page 2 of 4").is_some()
    });
    // A click on a page selects the field, for Insert, under the view.
    app.get_by_label("Pages").click();
    common::wait_until(&mut app, "the field selected", |app| {
        app.get_by_label("Pages").accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
    });
    assert!(app.query_by_label("Page 2 of 4").is_some());
}
