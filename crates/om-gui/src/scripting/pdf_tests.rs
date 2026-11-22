//! Focused end-to-end acceptance through the real desktop script host.
use super::*;
use crate::scripting::Prompt;
use om_layout::{
    Color, DataFormat, FieldObject, Layout, LayoutDef, LayoutObject, NumberFormat, ObjectStore,
    ObjectStyle, Part, Property, Rect, StyleSet,
};

fn style(size: f64, bold: bool, background: Option<Color>) -> ObjectStyle {
    let mut props = vec![
        Property {
            id: 69,
            length: Some(om_layout::Length {
                value: size,
                ..Default::default()
            }),
            ..Default::default()
        },
        Property {
            id: 65,
            int: Some(if bold { 700 } else { 400 }),
            ..Default::default()
        },
    ];
    if let Some(color) = background {
        props.push(Property {
            id: 7,
            color: Some(color),
            ..Default::default()
        });
    }
    ObjectStyle {
        local_styles: Some(StyleSet {
            components: vec![om_layout::style::ComponentStyle {
                component: 1,
                state: 1,
                properties: props,
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}
/// `style` with every corner rounded to `radius` in `unit` (6 pt, 1 %) and
/// a solid border of `width` points in `color` on every side (style
/// properties 32-35, 28-31, 20-23 and 24-27; layout-model.md 6.5).
fn rounded(style: &mut ObjectStyle, radius: f64, unit: i32, width: f64, color: Color) {
    let properties = &mut style.local_styles.as_mut().unwrap().components[0].properties;
    for side in 0..4 {
        let length = |value, unit| {
            Some(om_layout::Length {
                value,
                unit,
                ..Default::default()
            })
        };
        properties.extend([
            Property {
                id: 32 + side,
                length: length(radius, unit),
                ..Default::default()
            },
            Property {
                id: 28 + side,
                length: length(width, 6),
                ..Default::default()
            },
            Property {
                id: 20 + side,
                color: Some(color),
                ..Default::default()
            },
            Property {
                id: 24 + side,
                int: Some(5),
                ..Default::default()
            },
        ]);
    }
}
fn rect(left: f64, top: f64, width: f64, height: f64) -> Rect {
    Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
        ..Default::default()
    }
}
fn label(id: u64, value: &str, bounds: Rect, size: f64, bold: bool) -> LayoutObject {
    LayoutObject {
        id,
        object_type: 2,
        text: Some(value.into()),
        bounds: Some(bounds),
        style: Some(style(size, bold, None)),
        rich_text: Some(om_layout::StyledText {
            text: value.into(),
            character_runs: vec![om_layout::StyledRun {
                range: 0..value.len(),
                style: om_layout::CharacterStyle {
                    font_size: Some(size),
                    faces: Some(if bold { 256 } else { 0 }),
                    face_mask: Some(256),
                    ..Default::default()
                },
            }],
            paragraph_runs: vec![om_layout::StyledRun {
                range: 0..value.len(),
                style: om_layout::ParagraphStyle {
                    line_height: Some(1.0),
                    line_height_unit: Some(0),
                    ..Default::default()
                },
            }],
        }),
        ..Default::default()
    }
}
fn field(id: u64, key: u64, bounds: Rect) -> LayoutObject {
    LayoutObject {
        id,
        object_type: 1,
        bounds: Some(bounds),
        style: Some(style(12.0, false, None)),
        field_obj: Some(FieldObject {
            num_of_reps: 1,
            field_spec: Some(om_layout::FieldSpec {
                table_key: 5,
                field_key: key,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}
pub(super) fn document() -> RenderLayout {
    let mut amount = field(5, 3, rect(360.0, 138.0, 150.0, 28.0));
    amount.data_format = Some(DataFormat {
        number: NumberFormat {
            flags: 0x080e,
            decimal_places: 2,
            symbol: "$".into(),
            decimal_separator: '.',
            thousands_separator: ',',
            ..Default::default()
        },
        font_height: 12,
        date: Default::default(),
        time: Default::default(),
        raw: Vec::new(),
    });
    let mut objects = vec![
        label(
            1,
            "Synthetic records report",
            rect(12.0, 6.0, 500.0, 32.0),
            18.0,
            true,
        ),
        label(
            2,
            "Label / calculated row context",
            rect(12.0, 44.0, 500.0, 22.0),
            10.0,
            false,
        ),
        field(3, 1, rect(12.0, 86.0, 330.0, 26.0)),
        field(4, 4, rect(12.0, 112.0, 500.0, 26.0)),
        amount,
        label(
            6,
            "Wrapped text stays within the body part and continues onto its next line.",
            rect(12.0, 148.0, 290.0, 52.0),
            11.0,
            false,
        ),
        LayoutObject {
            id: 7,
            object_type: 5,
            bounds: Some(rect(320.0, 148.0, 24.0, 24.0)),
            style: Some(style(
                12.0,
                false,
                Some(Color {
                    red: 0.2,
                    green: 0.4,
                    blue: 0.7,
                    alpha: 1.0,
                }),
            )),
            ..Default::default()
        },
        LayoutObject {
            id: 8,
            object_type: 4,
            bounds: Some(rect(12.0, 212.0, 500.0, 0.0)),
            ..Default::default()
        },
        label(
            9,
            "Repeated footer",
            rect(12.0, 226.0, 300.0, 24.0),
            11.0,
            false,
        ),
    ];
    let mut page_number = label(10, "Page", rect(400.0, 226.0, 110.0, 24.0), 11.0, false);
    page_number.text_runs = vec![
        om_layout::TextRun::Literal("Page ".into()),
        om_layout::TextRun::MergeField("Page".into()),
    ];
    objects.push(page_number);
    RenderLayout {
        layout: Layout {
            id: 1,
            name: "Synthetic report".into(),
            def: Some(LayoutDef {
                base_width: 540.0,
                table_alias_key: 5,
                ..Default::default()
            }),
            parts: vec![
                Part {
                    part_type: 1,
                    top: 0.0,
                    height: 80.0,
                    ..Default::default()
                },
                Part {
                    part_type: 4,
                    top: 80.0,
                    height: 140.0,
                    ..Default::default()
                },
                Part {
                    part_type: 7,
                    top: 220.0,
                    height: 40.0,
                    ..Default::default()
                },
            ],
            objects: ObjectStore {
                root: objects.iter().map(|o| o.id).collect(),
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: Default::default(),
    }
}

pub(super) fn title_grand_document() -> RenderLayout {
    let mut document = document();
    let layout = &mut document.layout;
    let mut top = 0.0;
    layout.parts = [
        (0, 80.0),
        (1, 40.0),
        (2, 100.0),
        (4, 140.0),
        (6, 500.0),
        (7, 40.0),
        (8, 60.0),
    ]
    .into_iter()
    .map(|(part_type, height)| {
        let part = Part {
            part_type,
            top,
            height,
            ..Default::default()
        };
        top += height;
        part
    })
    .collect();
    layout.objects.objects.retain(|o| (3..=8).contains(&o.id));
    for object in &mut layout.objects.objects {
        let bounds = object.bounds.as_mut().unwrap();
        bounds.top += 140.0;
        bounds.bottom += 140.0;
    }
    for (id, prefix, field, top) in [
        (20, "Title header / ", "Caption", 8.0),
        (21, "Header / ", "Caption", 88.0),
        (22, "Leading total ", "TotalAmount", 128.0),
        (23, "Leading page ", "Page", 156.0),
        (24, "Trailing total ", "TotalAmount", 368.0),
        (25, "Grand record / ", "Caption", 396.0),
        (26, "Grand page ", "Page", 424.0),
        (27, "Page total ", "TotalPage", 452.0),
        (28, "SQL page total ", "SqlTotalPage", 480.0),
        (29, "Footer / ", "Caption", 868.0),
        (30, "Title footer / ", "Caption", 908.0),
    ] {
        let mut object = label(id, prefix, rect(12.0, top, 510.0, 26.0), 12.0, false);
        object.text_runs = vec![
            om_layout::TextRun::Literal(prefix.into()),
            om_layout::TextRun::MergeField(field.into()),
        ];
        layout.objects.objects.push(object);
    }
    layout.objects.root = layout.objects.objects.iter().map(|o| o.id).collect();
    document
}

/// The ordinary fixture with static PNG/JPEG pictures in every part.
pub(super) fn picture_document() -> RenderLayout {
    let mut document = document();
    crate::report::tests::add_pictures(&mut document.layout);
    document
}

/// A field beside an ordinary button (no icon) and a group of a filled
/// rectangle and a caption, at the group's origin: buttons and groups as a
/// report prints them (#263).
pub(super) fn button_group_document() -> RenderLayout {
    let mut document = document();
    let fill = Color {
        red: 0.85,
        green: 0.92,
        blue: 0.85,
        alpha: 1.0,
    };
    let backdrop = LayoutObject {
        id: 21,
        object_type: 5,
        bounds: Some(rect(0.0, 0.0, 260.0, 56.0)),
        style: Some(style(12.0, false, Some(fill))),
        parent: Some(20),
        ..Default::default()
    };
    let mut caption = label(
        22,
        "Grouped caption",
        rect(10.0, 16.0, 220.0, 24.0),
        12.0,
        false,
    );
    caption.parent = Some(20);
    let group = LayoutObject {
        id: 20,
        object_type: 8,
        bounds: Some(rect(12.0, 120.0, 260.0, 56.0)),
        group: Some(om_layout::GroupObject::default()),
        children: vec![21, 22],
        ..Default::default()
    };
    let mut button = label(
        30,
        "Print button",
        rect(300.0, 120.0, 160.0, 32.0),
        12.0,
        false,
    );
    button.object_type = 10;
    button.button = Some(om_layout::ButtonObject::default());
    button.style = Some(style(
        12.0,
        false,
        Some(Color {
            red: 0.8,
            green: 0.85,
            blue: 0.95,
            alpha: 1.0,
        }),
    ));
    // A theme button's rounded corners and its border.
    rounded(
        button.style.as_mut().unwrap(),
        8.0,
        6,
        1.0,
        Color {
            red: 0.2,
            green: 0.3,
            blue: 0.6,
            alpha: 1.0,
        },
    );
    let layout = &mut document.layout;
    layout.name = "Buttons and groups".into();
    // The ordinary report's header and footer labels and its name field.
    layout
        .objects
        .objects
        .retain(|o| [1, 2, 3, 9].contains(&o.id));
    layout
        .objects
        .objects
        .extend([group, backdrop, caption, button]);
    layout.objects.root = vec![1, 2, 3, 20, 30, 9];
    document
}

/// One PDF page drawn on white at a pixel per point, independently of the
/// writer (as om-calc renders PDF containers).
fn raster(bytes: &[u8], index: usize) -> image::RgbaImage {
    use hayro::hayro_interpret::{hayro_syntax::Pdf, InterpreterSettings};
    let pdf = Pdf::new(Arc::new(bytes.to_vec())).unwrap();
    let pixmap = hayro::render(
        &pdf.pages()[index],
        &hayro::RenderCache::new(),
        &InterpreterSettings::default(),
        &hayro::RenderSettings::default(),
        &hayro::PixmapSettings {
            x_scale: 1.0,
            y_scale: 1.0,
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        },
    );
    image::RgbaImage::from_raw(
        u32::from(pixmap.width()),
        u32::from(pixmap.height()),
        pixmap.data_as_u8_slice().to_vec(),
    )
    .unwrap()
}

#[test]
fn static_pictures_embed_once_in_pdf_and_fake_print_and_bad_images_refuse_atomically() {
    use crate::report::tests::{add_pictures, assert_pictures, png, JPEG_KEY, PNG_KEY};
    use om_layout::{LibraryImage, LibraryImageFormat};
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("pictures.pdf");
    let mut print = ScriptStep::new(43);
    print.value = 1;
    print.flags = flag::NO_INTERACT;
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        var("$pdfError", "Get ( LastError )"),
        print,
        exit("$pdfError & \"|\" & Get ( LastError ) & \"|\" & Get ( RecordID )"),
    ]);
    c.server.lock().unwrap().refresh_render = Some(picture_document());
    let fake = Arc::new(crate::printing::tests::Fake::default());
    c.session.set_print_dispatcher(fake.clone());
    let window = || WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    };
    c.run_in(window());
    assert_eq!(c.result(), "0|0|9");
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(fake.jobs.lock().unwrap()[0].bytes, saved);
    let text = String::from_utf8_lossy(&saved);
    assert!(text.contains("/Count 3"));
    // Seven static formats, including a PNG alpha mask, are embedded once
    // for the whole report, however many parts and pages repeat them.
    assert_eq!(text.matches("/Subtype /Image").count(), 8);
    assert_eq!(text.matches("/SMask").count(), 1);
    // The header PNG's unit square, top row first, covers its whole frame
    // (layout 320, 16 to 480, 64, inside the printer's margins).
    let margins = om_layout::paper::printer_margins();
    assert!(text.contains(&format!(
        "160 0 0 48 {} {} cm",
        margins.left + 320.0,
        792.0 - margins.top - 64.0
    )));

    let layout = &picture_document().layout;
    let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap();
    let plan = om_layout::pagination::paginate(layout, 9, paper.clone(), &[]).unwrap();
    for (page, parts) in [(0, 6), (2, 3)] {
        let raster = raster(&saved, page);
        raster
            .save(output.join(format!("pictures-hayro-page-{}.png", page + 1)))
            .unwrap();
        let sampled = assert_pictures(layout, &plan, page, "PDF", |x, y| {
            let pixel = raster.get_pixel(x as u32, y as u32);
            [pixel[0], pixel[1], pixel[2]]
        });
        assert_eq!(sampled, parts, "header, every body and footer");
    }

    // Damaged and missing pictures refuse both outputs: the saved PDF stays
    // and nothing is spooled.
    for (key, image, reason) in [
        (
            PNG_KEY,
            Some(png()[..40].to_vec()),
            "Library image 7 could not be decoded",
        ),
        (
            JPEG_KEY,
            None,
            "The picture of report object 44 (library image 8) is missing",
        ),
    ] {
        let mut broken = picture_document();
        broken.layout.library_images.remove(&key);
        if let Some(data) = image {
            broken.layout.library_images.insert(
                key,
                LibraryImage {
                    format: LibraryImageFormat::Png,
                    data,
                },
            );
        }
        c.server.lock().unwrap().refresh_render = Some(broken);
        c.session.s.file.rendered.clear();
        c.run_in(window());
        assert_eq!(c.result(), "3|3|9");
        assert!(
            c.session
                .s
                .failure
                .as_deref()
                .is_some_and(|f| f.starts_with(reason)),
            "{:?}",
            c.session.s.failure
        );
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    }
    // Mismatched format bytes, tints and grouped pictures stay refused.
    let mut layout = document().layout;
    add_pictures(&mut layout);
    layout.library_images.get_mut(&PNG_KEY).unwrap().format = LibraryImageFormat::Svg;
    let refuse = |layout: &Layout| {
        crate::report::render_pdf(layout, 1, paper.clone(), &[], |o, _| {
            Ok(o.text.clone().unwrap_or_default())
        })
        .unwrap_err()
        .message
    };
    assert!(refuse(&layout).contains("could not be decoded"));
    layout = picture_document().layout;
    layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == 45)
        .unwrap()
        .graphic
        .as_mut()
        .unwrap()
        .mix_color = 1;
    assert_eq!(refuse(&layout), "Unsupported report picture options: 45");
    layout = picture_document().layout;
    layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == 45)
        .unwrap()
        .parent = Some(40);
    assert_eq!(refuse(&layout), "Nested report objects are unsupported");
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write")));
}

/// The synthetic Region of a report row: the found order 7 2 9 | 4 8 1 6 |
/// 3 5 is sorted by it.
pub(super) fn region(id: i64) -> &'static str {
    match id {
        7 | 2 | 9 => "East",
        4 | 8 | 1 | 6 => "North",
        _ => "West",
    }
}

/// Header, leading and trailing sub-summaries by Region (field 10 of
/// occurrence 5), body, trailing grand summary and footer.
pub(super) fn grouped_document() -> RenderLayout {
    let mut document = document();
    let layout = &mut document.layout;
    let region = vec![0x06, 0x05, 0x00, 0x01, 0x05, 0x01, 10];
    let mut top = 0.0;
    layout.parts = [
        (1, 40.0),
        (3, 60.0),
        (4, 140.0),
        (5, 60.0),
        (6, 40.0),
        (7, 40.0),
    ]
    .into_iter()
    .map(|(part_type, height)| {
        let part = Part {
            part_type,
            top,
            height,
            break_field_key: if matches!(part_type, 3 | 5) {
                region.clone()
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        top += height;
        part
    })
    .collect();
    layout.objects.objects.retain(|o| (3..=8).contains(&o.id));
    for object in &mut layout.objects.objects {
        let bounds = object.bounds.as_mut().unwrap();
        bounds.top += 20.0;
        bounds.bottom += 20.0;
    }
    use om_layout::TextRun::{Literal, MergeField};
    let merge = |name: &str| MergeField(name.into());
    let text = |s: &str| Literal(s.into());
    for (id, top, runs) in [
        (40, 8.0, vec![text("Grouped / "), merge("Caption")]),
        (
            41,
            44.0,
            vec![
                text("Leading "),
                merge("Region"),
                text(" "),
                merge("TotalAmount"),
                text(" from "),
                merge("Caption"),
            ],
        ),
        (42, 72.0, vec![text("Leading page "), merge("Page")]),
        (
            43,
            244.0,
            vec![
                text("Trailing "),
                merge("Region"),
                text(" "),
                merge("TotalAmount"),
                text(" to "),
                merge("Caption"),
            ],
        ),
        (
            44,
            272.0,
            vec![
                text("Trailing page "),
                merge("Page"),
                text(" group "),
                merge("TotalPage"),
                text(" sql "),
                merge("SqlTotalPage"),
            ],
        ),
        (
            45,
            306.0,
            vec![
                text("Grand total "),
                merge("TotalAmount"),
                text(" page "),
                merge("Page"),
            ],
        ),
        (46, 346.0, vec![text("Footer page "), merge("Page")]),
    ] {
        let mut object = label(id, "", rect(12.0, top, 510.0, 26.0), 12.0, false);
        object.rich_text = None;
        object.text_runs = runs;
        layout.objects.objects.push(object);
    }
    // A merge field with its object's own currency format, as the Assets
    // starter's Asset Details header shows <<Price>> (#648).
    let mut formatted = label(47, "", rect(300.0, 306.0, 210.0, 26.0), 12.0, false);
    formatted.rich_text = None;
    formatted.text_runs = vec![text("Formatted "), merge("TotalAmount")];
    formatted.data_format = Some(DataFormat {
        number: NumberFormat {
            flags: 0x080e,
            decimal_places: 2,
            symbol: "$".into(),
            decimal_separator: '.',
            thousands_separator: ',',
            ..Default::default()
        },
        font_height: 12,
        date: Default::default(),
        time: Default::default(),
        raw: Vec::new(),
    });
    layout.objects.objects.push(formatted);
    layout.objects.root = layout.objects.objects.iter().map(|o| o.id).collect();
    document
}

pub(super) fn add_grand_fields(schema: &mut om_data::host::CalcSchema) {
    use om_data::host::{FieldClass, SummaryDef, SummaryKind};
    for (id, name, source) in [(7, "TotalAmount", 3), (8, "TotalPage", 6)] {
        schema.add_field(
            5,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef::new(SummaryKind::Total, source)),
                ..FieldDef::data(id, name, ValueType::Number)
            },
        );
    }
    let cx = om_data::formula::Context::new(schema.names());
    let compiled = om_data::formula::compile(
        r#"GetValue ( ExecuteSQL ( "SELECT \"TotalPage\" FROM \"Rows\"" ; "" ; "" ) ; 1 )"#,
        &cx,
    )
    .unwrap();
    schema.add_field(
        5,
        FieldDef::calculation(
            9,
            "SqlTotalPage",
            ValueType::Number,
            om_data::formula::Formula {
                context: None,
                formatted: Some(compiled.tokens),
                polish: compiled.expr,
            },
            false,
        ),
    );
}

#[test]
fn title_grand_pdf_and_fake_print_share_complete_report_and_physical_page_context() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("title-grand.pdf");
    let mut print = ScriptStep::new(43);
    print.value = 1;
    print.flags = flag::NO_INTERACT;
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        var("$pdfError", "Get ( LastError )"),
        print,
        exit("$pdfError & \"|\" & Get ( LastError ) & \"|\" & Get ( RecordID ) & \"|\" & Get ( RecordNumber ) & \"|\" & Get ( PageNumber )"),
    ]);
    {
        let mut server = c.server.lock().unwrap();
        server.report_grands = true;
        server.refresh_render = Some(title_grand_document());
    }
    let fake = Arc::new(crate::printing::tests::Fake::default());
    c.session.set_print_dispatcher(fake.clone());
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert_eq!(c.result(), "0|0|9|3|");
    let saved = std::fs::read(&path).unwrap();
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].bytes, saved);
    assert!(!jobs[0].path.exists());
    let text = String::from_utf8_lossy(&saved);
    assert!(text.contains("/Count 4"));
    assert!(text.contains("/MediaBox [0 0 612 792]"));
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    for expected in [
        "Title header / Row 07 - café / record 1",
        "Title footer / Row 05 - café / record 9",
        "Leading total 5625",
        "Trailing total 5625",
        "Grand record / Row 05 - café / record 9",
        "Grand page 4",
        "Page total 36",
        "SQL page total 36",
    ] {
        let glyphs: String = expected
            .chars()
            .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
            .collect();
        assert!(
            text.contains(&format!("<{glyphs}> Tj")),
            "Missing report text: {expected}"
        );
    }
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write")));
    drop(jobs);
    // Unsupported part options preserve atomic PDF output and never spool.
    let existing = saved;
    let mut invalid = title_grand_document();
    invalid.layout.parts[4].flags = 1;
    c.server.lock().unwrap().refresh_render = Some(invalid);
    c.session.s.file.rendered.clear();
    c.run();
    assert!(c.result().starts_with("3|3|"));
    assert_eq!(std::fs::read(path).unwrap(), existing);
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
}
#[test]
fn grouped_pdf_print_and_preview_pages_share_sorted_sub_summaries() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("grouped-sub-summaries.pdf");
    let mut print = ScriptStep::new(43);
    print.value = 1;
    print.flags = flag::NO_INTERACT;
    let mut last_page = ScriptStep::new(16);
    last_page.value = 2;
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        var("$pdfError", "Get ( LastError )"),
        print,
        var("$printError", "Get ( LastError )"),
        last_page,
        exit("$pdfError & \"|\" & $printError & \"|\" & Get ( LastError ) & \"|\" & Get ( PageNumber ) & \"|\" & Get ( RecordID )"),
    ]);
    {
        let mut server = c.server.lock().unwrap();
        server.report_grands = true;
        server.report_groups = true;
        server.report_sort = vec![10];
        server.refresh_render = Some(grouped_document());
    }
    let fake = Arc::new(crate::printing::tests::Fake::default());
    c.session.set_print_dispatcher(fake.clone());
    let state = WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        mode: om_data::runtime::Mode::Preview,
        placement: Some(crate::desktop::placement(
            &egui::Context::default(),
            None,
            true,
            0.0,
            false,
            0.0,
        )),
        ..Default::default()
    };
    c.run_in(state.clone());
    // Preview's page count comes from the same grouped plan as the output.
    assert_eq!(c.result(), "0|0|0|3|9");
    assert_eq!(c.session.s.file.window.state.record, Some(9));
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(output.join("grouped-sorted.pdf"), &saved).unwrap();
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].bytes, saved);
    drop(jobs);
    let text = String::from_utf8_lossy(&saved).into_owned();
    assert!(text.contains("/Count 3"));
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let shown = |pdf: &str, expected: &str| {
        let glyphs: String = expected
            .chars()
            .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
            .collect();
        pdf.contains(&format!("<{glyphs}> Tj"))
    };
    let shows = |expected: &str| shown(&text, expected);
    // Group totals (East 2250, North 2375, West 1000) differ from the grand
    // total 5625. North's heading would fit at the foot of page 1 but its
    // first record would not, so it opens page 2 with that record, as
    // FileMaker Pro 26 prints a group heading (#263); North closes on page 3. The group's
    // first/last record anchors other fields; summaries in calculations
    // and nested SQL keep the found set and the physical page.
    for expected in [
        "Leading East 2250 from Row 07 - café / record 1",
        "Trailing East 2250 to Row 09 - café / record 3",
        "Trailing page 1 group 3 sql 9",
        "Leading North 2375 from Row 04 - café / record 4",
        "Trailing North 2375 to Row 06 - café / record 7",
        "Trailing page 3 group 12 sql 27",
        "Leading West 1000 from Row 03 - café / record 8",
        "Trailing West 1000 to Row 05 - café / record 9",
        "Trailing page 3 group 6 sql 27",
        "Grand total 5625 page 3",
        "Formatted $5,625.00",
    ] {
        assert!(shows(expected), "Missing report text: {expected}");
    }
    assert!(shows("Leading page 2"), "North opens on page 2");

    // Unsorted by the break field, sub-summaries do not print.
    let reset = |c: &mut Client, sort: Vec<u64>| {
        c.server.lock().unwrap().report_sort = sort;
        *c.session.s.file.cache.borrow_mut() = Default::default();
    };
    reset(&mut c, Vec::new());
    c.run_in(state.clone());
    assert_eq!(c.result(), "0|0|0|3|9");
    let unsorted = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert!(unsorted.contains("/Count 3") && shown(&unsorted, "Grand total 5625 page 3"));
    assert!(
        !shown(&unsorted, "Leading page 1") && !shown(&unsorted, "Trailing page 3 group 6 sql 27")
    );
    // A break after a related sort key is refused: the previous PDF stays
    // and nothing is spooled.
    let existing = std::fs::read(&path).unwrap();
    reset(&mut c, vec![om_data::host::UNRESOLVED_SORT_FIELD, 10]);
    c.run_in(state);
    assert!(c.result().starts_with("3|3|3|"), "{}", c.result());
    assert_eq!(std::fs::read(&path).unwrap(), existing);
    assert_eq!(fake.jobs.lock().unwrap().len(), 2);
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

/// A report row's synthetic Street, Suite (empty for even ids) and Notes
/// (`id % 4` lines, none for multiples of 4).
pub(super) fn address(id: i64) -> (String, String, String) {
    let notes: Vec<String> = (1..=id % 4)
        .map(|line| format!("Note {line} of {id}"))
        .collect();
    (
        format!("{id} Synthetic Way"),
        if id % 2 == 0 {
            String::new()
        } else {
            format!("Suite {id}")
        },
        notes.join("\r"),
    )
}

/// An address block that closes up, grouped by Region (field 10). Header
/// (0-40) with the page number, leading sub-summary (40-70), body (70-210)
/// and footer (210-250). In the body, Name (1), Street (11), Suite (12) and
/// Notes (13) slide up on the objects directly above them and resize the
/// part; a fixed label to their right holds the blue patch below it, which
/// slides on the objects directly above it; a separator line slides on all
/// objects above it and resizes the body.
pub(super) fn sliding_document() -> RenderLayout {
    use om_layout::sliding::{DIRECTLY_ABOVE, RESIZE_PART, UP};
    let mut document = document();
    let layout = &mut document.layout;
    let region = vec![0x06, 0x05, 0x00, 0x01, 0x05, 0x01, 10];
    layout.parts = [
        (1, 0.0, 40.0),
        (3, 40.0, 30.0),
        (4, 70.0, 140.0),
        (7, 210.0, 40.0),
    ]
    .into_iter()
    .map(|(part_type, top, height)| Part {
        part_type,
        top,
        height,
        break_field_key: if part_type == 3 {
            region.clone()
        } else {
            Vec::new()
        },
        ..Default::default()
    })
    .collect();
    use om_layout::TextRun::{Literal, MergeField};
    let merged = |id, top, runs: Vec<om_layout::TextRun>| {
        let mut object = label(id, "", rect(12.0, top, 400.0, 24.0), 12.0, false);
        object.text_runs = runs;
        object
    };
    let column = UP | RESIZE_PART | DIRECTLY_ABOVE;
    let slide = |mut object: LayoutObject, flags| {
        object.flags |= flags;
        object
    };
    let mut objects = vec![
        merged(
            50,
            8.0,
            vec![Literal("Sliding / page ".into()), MergeField("Page".into())],
        ),
        merged(
            51,
            44.0,
            vec![Literal("Region ".into()), MergeField("Region".into())],
        ),
        slide(field(52, 1, rect(12.0, 76.0, 300.0, 18.0)), column),
        slide(field(53, 11, rect(12.0, 96.0, 300.0, 18.0)), column),
        slide(field(54, 12, rect(12.0, 116.0, 300.0, 18.0)), column),
        slide(field(55, 13, rect(12.0, 136.0, 300.0, 48.0)), column),
        label(56, "Fixed", rect(330.0, 76.0, 150.0, 18.0), 12.0, false),
        slide(
            LayoutObject {
                id: 57,
                object_type: 5,
                bounds: Some(rect(330.0, 98.0, 24.0, 16.0)),
                style: Some(style(12.0, false, Some(BLUE))),
                ..Default::default()
            },
            UP | DIRECTLY_ABOVE,
        ),
        slide(
            LayoutObject {
                id: 58,
                object_type: 4,
                bounds: Some(rect(12.0, 194.0, 500.0, 0.0)),
                ..Default::default()
            },
            UP | RESIZE_PART,
        ),
        label(
            59,
            "Sliding footer",
            rect(12.0, 216.0, 300.0, 24.0),
            12.0,
            false,
        ),
    ];
    for object in &mut objects {
        object.flags |= 0x3000_0000;
    }
    layout.objects = ObjectStore {
        root: objects.iter().map(|o| o.id).collect(),
        objects,
        ..Default::default()
    };
    document
}
const BLUE: Color = Color {
    red: 0.2,
    green: 0.4,
    blue: 0.8,
    alpha: 1.0,
};

/// How far below its text box's top a 12-point Inter line's first
/// baseline sits (the ascent, rounded to a whole point), and its line
/// height: the sliding fixture's fields are 18 high (Notes 48), with
/// 2-point gaps between them.
pub(super) fn sliding_metrics() -> (f64, f64) {
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let unit = 12.0 / f64::from(font.units_per_em());
    let ascent = f64::from(font.ascender()) * unit;
    let line = ascent - f64::from(font.descender()) * unit;
    assert!(line > 14.4 && line < 18.0);
    (ascent.round(), line)
}

/// How far a sliding fixture row's Notes rose plus shrank: its separator
/// rises and its body shrinks by that.
pub(super) fn sliding_lift(id: i64) -> f64 {
    let line = sliding_metrics().1;
    let shrink = 18.0 - line;
    let suite = if id % 2 == 0 {
        2.0 * shrink + 18.0
    } else {
        3.0 * shrink
    };
    suite + 48.0 - (id % 4) as f64 * line
}

/// Where each line showing `expected` starts, (x, y) in PDF user space, by
/// page: every page's content stream precedes the fonts and images.
fn text_origins(pdf: &str, pages: usize, expected: &str) -> Vec<(usize, f64, f64)> {
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let glyphs: String = expected
        .chars()
        .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
        .collect();
    let needle = format!("<{glyphs}> Tj");
    let mut origins = Vec::new();
    for (page, content) in pdf.split("endstream").take(pages).enumerate() {
        for (at, _) in content.match_indices(&needle) {
            let matrix = content[..at].trim_end().lines().last().unwrap();
            let numbers: Vec<f64> = matrix
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            assert!(matrix.ends_with("Tm") && numbers.len() == 6, "{matrix}");
            origins.push((page, numbers[4], numbers[5]));
        }
    }
    origins
}

#[test]
fn sliding_address_blocks_close_up_and_resize_parts_in_pdf_print_and_preview_pages() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("sliding.pdf");
    let mut print = ScriptStep::new(43);
    print.value = 1;
    print.flags = flag::NO_INTERACT;
    let mut last_page = ScriptStep::new(16);
    last_page.value = 2;
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        var("$pdfError", "Get ( LastError )"),
        print,
        var("$printError", "Get ( LastError )"),
        last_page,
        exit("$pdfError & \"|\" & $printError & \"|\" & Get ( LastError ) & \"|\" & Get ( PageNumber ) & \"|\" & Get ( RecordID )"),
    ]);
    {
        let mut server = c.server.lock().unwrap();
        server.report_groups = true;
        server.report_slides = true;
        server.report_sort = vec![10];
        server.refresh_render = Some(sliding_document());
    }
    let fake = Arc::new(crate::printing::tests::Fake::default());
    c.session.set_print_dispatcher(fake.clone());
    let state = WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        mode: om_data::runtime::Mode::Preview,
        placement: Some(crate::desktop::placement(
            &egui::Context::default(),
            None,
            true,
            0.0,
            false,
            0.0,
        )),
        ..Default::default()
    };
    c.run_in(state.clone());
    // Preview's page movement counts the same two measured pages.
    assert_eq!(c.result(), "0|0|0|2|9");
    assert_eq!(c.session.s.file.window.state.record, Some(9));
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(fake.jobs.lock().unwrap()[0].bytes, saved);
    let text = String::from_utf8_lossy(&saved).into_owned();
    assert!(text.contains("/Count 2"));

    let (ascent, line) = sliding_metrics();
    let lift = sliding_lift;
    let body = |id: i64| 140.0 - lift(id);
    // Fixed bodies would need three pages; measured ones fit two, and
    // record 6 (North continues) opens page 2.
    let fixed = om_layout::pagination::paginate(
        &sliding_document().layout,
        9,
        om_layout::pagination::PageSize::default(),
        &[om_layout::pagination::BreakGroups {
            field: om_layout::BreakField {
                occurrence: 5,
                field: 10,
            },
            depth: 0,
            groups: vec![0, 0, 0, 1, 1, 1, 1, 2, 2],
        }],
    )
    .unwrap();
    assert_eq!(fixed.pages.len(), 3);
    // The printer's margins (`printer_margins`), a 40-point header and footer.
    let margins = om_layout::paper::printer_margins();
    let (first, footer) = (margins.top + 40.0, 792.0 - margins.bottom - 40.0);
    let mut tops = Vec::new();
    let mut top = first;
    for (id, opens) in [
        (7, true),
        (2, false),
        (9, false),
        (4, true),
        (8, false),
        (1, false),
    ] {
        top += if opens { 30.0 } else { 0.0 };
        tops.push((0, id, top));
        top += body(id);
    }
    assert!(top <= footer && top + body(6) > footer, "{top}");
    tops.push((1, 6, first));
    tops.push((1, 3, first + body(6) + 30.0));
    tops.push((1, 5, first + body(6) + 30.0 + body(3)));
    let y = |top: f64| 792.0 - (top + ascent);
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    for &(page, id, top) in &tops {
        let name = format!("Row {id:02} - café");
        let origins = text_origins(&text, 2, &name);
        assert_eq!(origins.len(), 1, "{name}");
        let (on, x, at) = origins[0];
        assert!(
            on == page && near(x, margins.left + 12.0) && near(at, y(top + 6.0)),
            "{name}: {origins:?}, top {top}"
        );
        // Street follows Name's shrink; an empty Suite collapses, keeping
        // the 2-point gaps around it; Notes follows.
        let street = text_origins(&text, 2, &format!("{id} Synthetic Way"))[0];
        assert!(
            near(street.2, y(top + 6.0 + line + 2.0)),
            "{id}: {street:?}"
        );
        let notes_top = if id % 2 == 0 {
            assert!(text_origins(&text, 2, &format!("Suite {id}")).is_empty());
            top + 6.0 + 2.0 * line + 6.0
        } else {
            let suite = text_origins(&text, 2, &format!("Suite {id}"))[0];
            assert!(
                near(suite.2, y(top + 6.0 + 2.0 * line + 4.0)),
                "{id}: {suite:?}"
            );
            top + 6.0 + 3.0 * line + 6.0
        };
        for n in 1..=id % 4 {
            let note = text_origins(&text, 2, &format!("Note {n} of {id}"))[0];
            assert!(
                near(note.2, y(notes_top + (n - 1) as f64 * line)),
                "{id} note {n}: {note:?}"
            );
        }
    }
    // Headers keep the physical page; a group opens only where it starts.
    for (page, expected) in [(0, "Sliding / page 1"), (1, "Sliding / page 2")] {
        assert_eq!(text_origins(&text, 2, expected)[0].0, page);
    }
    let regions: Vec<_> = ["Region East", "Region North", "Region West"]
        .iter()
        .map(|r| text_origins(&text, 2, r)[0].0)
        .collect();
    assert_eq!(regions, [0, 0, 1]);
    // The footer label is a text object: its text is 1 point inside it.
    assert!(text_origins(&text, 2, "Sliding footer")
        .iter()
        .all(|&(_, _, at)| near(at, y(footer + 6.0 + 1.0))));

    // Pixels: the blue patch stays below the fixed label in every body; the
    // separator line rises with Notes, keeping its 10-point gap.
    for page in 0..2 {
        let raster = raster(&saved, page);
        raster
            .save(output.join(format!("sliding-hayro-page-{}.png", page + 1)))
            .unwrap();
        for &(_, id, top) in tops.iter().filter(|t| t.0 == page) {
            let blue = raster.get_pixel((margins.left + 342.0) as u32, (top + 36.0) as u32);
            assert!(
                blue[2] > 180 && blue[0] < 80,
                "page {} patch of {id}: {blue:?}",
                page + 1
            );
            let rule = top + 124.0 - lift(id);
            let dark =
                (rule as u32 - 1..=rule as u32 + 1).any(|row| raster.get_pixel(356, row)[0] < 200);
            assert!(dark, "page {} rule of {id} at {rule}", page + 1);
            let clear = raster.get_pixel(356, (rule - 6.0) as u32);
            assert!(clear[0] > 200, "page {} above rule of {id}", page + 1);
        }
    }

    // Unsupported sliding refuses atomically: the PDF stays, nothing spools.
    let mut left = sliding_document();
    left.layout.objects.objects[2].flags |= om_layout::sliding::LEFT;
    c.server.lock().unwrap().refresh_render = Some(left);
    c.session.s.file.rendered.clear();
    c.run_in(state);
    assert!(c.result().starts_with("3|3|3|"), "{}", c.result());
    assert!(c
        .session
        .s
        .failure
        .as_deref()
        .is_some_and(|f| f.contains("slide left and up in one part")));
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

/// Sliding left closes up a line, as Claris describes it: a field set to
/// slide left narrows to the width its text needs, and an object set to
/// slide left moves by the least its nearest objects on its left moved and
/// narrowed, keeping the gaps between them; an object that does not slide
/// holds. What is unmeasured refuses: sliding left in a header, a part
/// where objects slide both left and up, and a field of several lines.
#[test]
fn sliding_left_closes_up_a_line_of_fields() {
    use om_layout::sliding::{LEFT, UP};
    let paper = om_layout::pagination::PageSize::default();
    let slide = |mut object: LayoutObject, flags| {
        object.flags |= 0x3000_0000 | flags;
        object
    };
    let layout = |objects: Vec<LayoutObject>, part_type| Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type,
            height: 60.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: objects.iter().map(|o| o.id).collect(),
            objects,
            ..Default::default()
        },
        ..Default::default()
    };
    let line = |first_flags| {
        vec![
            slide(field(1, 1, rect(10.0, 10.0, 100.0, 18.0)), first_flags),
            slide(field(2, 11, rect(115.0, 10.0, 100.0, 18.0)), LEFT),
            slide(
                label(3, "!", rect(220.0, 10.0, 20.0, 18.0), 12.0, false),
                LEFT,
            ),
        ]
    };
    let render = |layout: &Layout, first: &str| {
        let first = first.to_owned();
        crate::report::render_pdf(layout, 1, paper.clone(), &[], move |o, _| {
            Ok(match o.id {
                1 => first.clone(),
                2 => "Lovelace".into(),
                _ => o.text.clone().unwrap_or_default(),
            })
        })
        .map(|report| String::from_utf8_lossy(&report.bytes).into_owned())
        .map_err(|e| e.message)
    };
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let width = |text: &str| {
        text.chars()
            .map(|ch| {
                let glyph = font.glyph_index(ch).unwrap();
                f64::from(font.glyph_hor_advance(glyph).unwrap()) * 12.0
                    / f64::from(font.units_per_em())
            })
            .sum::<f64>()
    };
    let pdf = render(&layout(line(LEFT), 4), "Ada").unwrap();
    let x = |text: &str| text_origins(&pdf, 1, text)[0].1;
    // First narrows to "Ada"; Last keeps its 5-point gap after it and
    // narrows to "Lovelace"; the label keeps its 5-point gap after Last
    // (its text 1 point into its frame).
    assert!((x("Lovelace") - x("Ada") - (width("Ada") + 5.0)).abs() < 0.05);
    assert!(
        (x("!") - x("Lovelace") - (width("Lovelace") + 5.0 + 1.0)).abs() < 0.05,
        "{} {}",
        x("!"),
        x("Lovelace")
    );
    // A first field that does not slide holds the line where it is placed.
    let held = render(&layout(line(0), 4), "Ada").unwrap();
    let held_x = |text: &str| text_origins(&held, 1, text)[0].1;
    assert!((held_x("Lovelace") - held_x("Ada") - 105.0).abs() < 0.05);
    // Refused: a header (Claris says only that it never slides up), a part
    // that slides both ways, and a field of more than one line.
    let mut headed = layout(line(LEFT), 1);
    headed.parts.push(Part {
        part_type: 4,
        top: 60.0,
        height: 20.0,
        ..Default::default()
    });
    let header = render(&headed, "Ada").unwrap_err();
    assert!(header.contains("header, footer or title part"), "{header}");
    assert!(render(&layout(line(LEFT | UP), 4), "Ada")
        .unwrap_err()
        .contains("slide left and up in one part"));
    assert!(render(&layout(line(LEFT), 4), "Ada\rKing")
        .unwrap_err()
        .contains("more than one line"));
    // So does one whose one line is wider than the field (it would wrap),
    // whatever its text's colour.
    assert!(render(&layout(line(LEFT), 4), &"Ada King ".repeat(6))
        .unwrap_err()
        .contains("more than one line"));
}

#[test]
fn sliding_refuses_unsupported_options_overlaps_and_cut_off_objects() {
    let paper = om_layout::pagination::PageSize::default();
    let refuse = |layout: &Layout| {
        crate::report::render_pdf(layout, 2, paper.clone(), &[], |o, _| {
            Ok(o.text.clone().unwrap_or_default())
        })
        .unwrap_err()
        .message
    };
    let mut ungrouped = sliding_document().layout;
    ungrouped.parts[1].part_type = 12; // A navigation part does not print.
    let with = |change: &dyn Fn(&mut Layout)| {
        let mut layout = ungrouped.clone();
        change(&mut layout);
        layout
    };
    fn find(layout: &mut Layout, id: u64) -> &mut LayoutObject {
        layout
            .objects
            .objects
            .iter_mut()
            .find(|o| o.id == id)
            .unwrap()
    }
    // Empty fields collapse and the body shrinks; the rule would rise 102
    // points with Notes but stops at the fixed patch above it (part-relative
    // 44), so the body is 140 - (124 - 44) high.
    let plan = crate::report::render_pdf(&ungrouped, 2, paper.clone(), &[], |o, _| {
        Ok(o.text.clone().unwrap_or_default())
    })
    .unwrap()
    .plan;
    assert_eq!(plan.pages[0][1].height, 60.0);
    assert_eq!(plan.pages[0][2].top, plan.pages[0][1].top + 60.0);
    for (change, reason) in [
        (
            &(|l: &mut Layout| find(l, 53).flags |= om_layout::sliding::LEFT)
                as &dyn Fn(&mut Layout),
            "slide left and up in one part",
        ),
        (
            &|l: &mut Layout| find(l, 56).flags |= om_layout::sliding::NON_PRINTING,
            "hidden when printing",
        ),
        // The patch moved into Notes' frame.
        (
            &|l: &mut Layout| find(l, 57).bounds = Some(rect(300.0, 140.0, 24.0, 16.0)),
            "overlap",
        ),
        // A fixed label at the body's foot is cut off when the body shrinks.
        (
            &|l: &mut Layout| {
                l.objects.root.push(60);
                l.objects.objects.push(LayoutObject {
                    id: 60,
                    object_type: 5,
                    flags: 0x3000_0000,
                    bounds: Some(rect(515.0, 190.0, 20.0, 18.0)),
                    ..Default::default()
                });
            },
            "would print below its resized part",
        ),
        (
            &|l: &mut Layout| find(l, 55).rotation = 90,
            "Unsupported report object",
        ),
    ] {
        let message = refuse(&with(change));
        assert!(message.contains(reason), "{reason}: {message}");
    }
    // Headers, footers and title parts never shrink or slide up (Claris):
    // objects there set to slide up (and resize their part) print exactly
    // as the same objects without the options. Among them: a rectangle over
    // the header's merge text (an overlap a slider may not have), an empty
    // field that would shrink and a label below it that would rise if they
    // slid, and a field whose text does not fit, cut at its box.
    use om_layout::sliding::{RESIZE_PART, UP};
    let header = |slides: u64| {
        with(&|l| {
            find(l, 50).flags |= slides;
            find(l, 59).flags |= slides & UP;
            let mut extra = vec![
                LayoutObject {
                    id: 61,
                    object_type: 5,
                    bounds: Some(rect(12.0, 10.0, 20.0, 10.0)),
                    ..Default::default()
                },
                field(62, 1, rect(330.0, 8.0, 60.0, 18.0)),
                field(63, 11, rect(400.0, 8.0, 60.0, 14.0)),
                label(64, "Below", rect(400.0, 24.0, 60.0, 12.0), 12.0, false),
            ];
            for object in &mut extra {
                object.flags |= 0x3000_0000 | slides;
                l.objects.root.push(object.id);
            }
            l.objects.objects.extend(extra);
        })
    };
    let render = |layout: &Layout| {
        crate::report::render_pdf(layout, 2, paper.clone(), &[], |o, _| {
            Ok(match o.id {
                62 => "A name far too long for its header field".repeat(4),
                63 => String::new(),
                _ => o.text.clone().unwrap_or_default(),
            })
        })
        .unwrap()
        .bytes
    };
    let placed = render(&header(0));
    assert_eq!(render(&header(UP | RESIZE_PART)), placed);
}

fn pdf(path: &str, current: bool) -> ScriptStep {
    let mut step = ScriptStep::new(144);
    step.value = if current { 2 } else { 1 };
    step.flags = flag::NO_INTERACT | flag::FILE_PATH;
    step.set_slot_child(0, 0x82, Some(om_data::text_node(&[0x82], path)));
    step
}
fn client(steps: Vec<ScriptStep>) -> Client {
    let c = Client::new(steps, vec![]);
    // Found-set order deliberately differs from record-id order.
    c.server.lock().unwrap().pdf_rows = vec![7, 2, 9, 4, 8, 1, 6, 3, 5];
    c
}
#[test]
fn desktop_pdf_outputs_found_set_and_current_record_and_preserves_existing_on_refusal_or_cancel() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let all = output.join("found-set.pdf");
    let current = output.join("current-record.pdf");
    let path = |p: &std::path::Path| crate::scripting::files::file_line(p);
    let mut c = client(vec![
        var("$pdfPath", &format!("{:?}", path(&all))),
        pdf("$pdfPath", false),
        var("$saved", "Get ( LastError )"),
        pdf(&path(&current), true),
        exit("$saved & \"|\" & Get ( LastError )"),
    ]);
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert_eq!(c.result(), "0|0");
    assert_eq!(c.session.s.file.window.state.index, 2);
    assert_eq!(c.session.s.file.window.state.record, Some(9));
    for (p, pages) in [(&all, 3), (&current, 1)] {
        let bytes = std::fs::read(p).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(String::from_utf8_lossy(&bytes).contains(&format!("/Count {pages}")));
    }
    let existing = std::fs::read(&all).unwrap();
    let mut append = pdf(&path(&all), false);
    append.flags |= flag::OPTION;
    let mut c = client(vec![append, exit("Get ( LastError )")]);
    c.run();
    assert_eq!(c.result(), "3");
    assert_eq!(std::fs::read(&all).unwrap(), existing);
    let mut security = pdf(&path(&all), false);
    security.flags |= flag::RESTORE;
    security.set_number(2, Some(1));
    let mut c = client(vec![security, exit("Get ( LastError )")]);
    c.run();
    assert_eq!(c.result(), "3");
    assert_eq!(std::fs::read(&all).unwrap(), existing);
    let mut dialog = pdf(&path(&all), false);
    dialog.flags &= !flag::NO_INTERACT;
    let mut c = client(vec![dialog, exit("Get ( LastError )")]);
    let sender = c._keep_jobs.clone();
    let responses = std::mem::replace(&mut c.events, mpsc::channel().1);
    let cancel = std::thread::spawn(move || {
        while let Ok(response) = responses.recv_timeout(std::time::Duration::from_secs(10)) {
            if let worker::Reply::Script(ScriptEvent::Prompt {
                id,
                prompt: Prompt::Pdf(_),
            }) = response.reply
            {
                sender
                    .send(Job {
                        epoch: 1,
                        window: 1,
                        ctx: Default::default(),
                        request: Request::Answer {
                            prompt: id,
                            answer: Answer::Pdf(None),
                        },
                    })
                    .unwrap();
                return;
            }
        }
        panic!("The PDF output dialog was not shown");
    });
    c.run();
    cancel.join().unwrap();
    assert_eq!(c.result(), "1");
    assert_eq!(std::fs::read(&all).unwrap(), existing);
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|s| s.ends_with(":write")));
}

/// A restored step as a production solution stores it (script-steps.md
/// 11.7): all pages, no passwords, print permission 2 with the page-from
/// calculation beside it in slot 4, edit permission 4, copying and the
/// screen reader on, the pages panel (view 2), and the default page layout
/// and magnification. The PDF holds the found set and opens on its pages
/// panel. The page-from calculation is not evaluated: it would set
/// `$$probe` (the stored one is `1`). A real page range, a page layout
/// other than Default, and a slot-4 member that is not a calculation are
/// still refused, leaving the file as it was.
#[test]
fn a_restored_pdf_opens_on_its_pages_panel_with_an_unused_page_from() {
    use hayro::hayro_interpret::hayro_syntax::object::{Dict, Name};
    use hayro::hayro_interpret::hayro_syntax::Pdf;
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("restored.pdf");
    let path = crate::scripting::files::file_line(&output);
    let restored = |change: &dyn Fn(&mut ScriptStep)| {
        let mut step = pdf(&path, false);
        step.flags |= flag::RESTORE;
        step.set_number_bool(1, Some(true));
        step.set_number_bool(2, Some(false));
        step.set_number_bool(3, Some(false));
        step.set_number_u16(4, Some(2));
        step.set_calculation(4, Some(&calc("Let ( $$probe = 1 ; 1 )")));
        step.set_number_u16(5, Some(4));
        step.set_number_bool(6, Some(true));
        step.set_number_bool(7, Some(true));
        step.set_number_u16(8, Some(2));
        step.set_number_u16(9, Some(0));
        step.set_number_u16(10, Some(0));
        change(&mut step);
        step
    };
    let mut c = client(vec![
        restored(&|_| {}),
        exit("Get ( LastError ) & \"|\" & $$probe"),
    ]);
    c.run();
    assert_eq!(c.result(), "0|");
    let written = std::fs::read(&output).unwrap();
    let document = Pdf::new(written.clone()).unwrap();
    let catalog: Dict = document.xref().get(document.xref().root_id()).unwrap();
    let mode: Name = catalog.get(b"PageMode").unwrap();
    assert_eq!(&*mode, b"UseThumbs");
    assert!(!catalog.contains_key(b"PageLayout"));
    assert!(!catalog.contains_key(b"OpenAction"));
    assert_eq!(document.pages().len(), 3);
    for (what, change) in [
        (
            "a page range",
            &(|s: &mut ScriptStep| s.set_number_u16(1, Some(2))) as &dyn Fn(&mut ScriptStep),
        ),
        ("a single-page layout", &|s: &mut ScriptStep| {
            s.set_number_u16(9, Some(1))
        }),
        (
            "a slot-4 member that is no calculation",
            &|s: &mut ScriptStep| {
                let junk = om_data::runtime::OwnedNode::leaf([0x05], b"junk".to_vec());
                s.set_slot_child(4, 0x05, Some(junk))
            },
        ),
    ] {
        let mut c = client(vec![restored(change), exit("Get ( LastError )")]);
        c.run();
        assert_eq!(c.result(), "3", "{what}");
        assert_eq!(std::fs::read(&output).unwrap(), written, "{what}");
    }
}

/// The ordinary fixture whose Text field (object 3) has one conditional
/// format: on the record whose Text is "Row 09 - café" it fills yellow and
/// its text turns red. The Caption field (object 4) fills yellow in Browse
/// only, as the starter solutions' `Get ( WindowMode ) = 0` formats do.
pub(super) fn conditional_document() -> RenderLayout {
    let mut document = document();
    let schema = CalcSchema::new("Root", Locale::en_us());
    let mut cx = om_data::formula::Context::new(schema.names());
    cx.support_self = true;
    let formula = |source: &str| {
        let compiled = om_data::formula::compile(source, &cx).unwrap();
        om_data::formula::Formula {
            context: None,
            formatted: Some(compiled.tokens),
            polish: compiled.expr,
        }
    };
    for (id, source, fill) in [
        (3, "Self = \"Row 09 - café\"", CONDITION_FILL),
        (4, "Get ( WindowMode ) = 0", CONDITION_FILL),
        (
            5,
            "LayoutObjectUUID = \"01010101-0101-0101-0101-010101010101\"",
            UUID_FILL,
        ),
    ] {
        let mut format = style(12.0, false, Some(fill));
        format.local_styles.as_mut().unwrap().components[0]
            .properties
            .push(Property {
                id: 70,
                color: Some(CONDITION_TEXT),
                ..Default::default()
            });
        let field = (document.layout.objects.objects.iter_mut())
            .find(|o| o.id == id)
            .unwrap();
        if id == 5 {
            field.uuid = Some([1; 16]);
        }
        field.calcs.conditions.push(om_layout::ConditionalFormat {
            formula: Some(formula(source)),
            condition: om_layout::CfCondition {
                style: Some(format),
                ..Default::default()
            },
            kind: None,
        });
    }
    document
}
const CONDITION_FILL: Color = Color {
    red: 1.0,
    green: 0.9,
    blue: 0.2,
    alpha: 1.0,
};
/// The Amount field's fill, on its own `LayoutObjectUUID`.
const UUID_FILL: Color = Color {
    red: 0.6,
    green: 0.9,
    blue: 0.6,
    alpha: 1.0,
};
const CONDITION_TEXT: Color = Color {
    red: 0.9,
    green: 0.1,
    blue: 0.1,
    alpha: 1.0,
};

/// Where `conditional_document`'s Text field prints for each body on each
/// page, and whether its record (found order 7 2 9 4 8 1 6 3 5) is the one
/// whose condition holds: points (x, y) inside its frame, right of its text.
pub(super) fn conditional_samples(
    plan: &om_layout::pagination::ReportPlan,
) -> Vec<(usize, f64, f64, bool)> {
    let found = [7, 2, 9, 4, 8, 1, 6, 3, 5];
    let mut samples = Vec::new();
    for (page, placements) in plan.pages.iter().enumerate() {
        for placement in placements.iter().filter(|p| p.part == 1) {
            // The field is at (12, 86) to (342, 112); the body starts at 80.
            let (x, y) = (plan.paper.margins.left + 320.0, placement.top + 6.0 + 13.0);
            samples.push((page, x, y, found[placement.record] == 9));
        }
    }
    samples
}

/// Conditional formatting prints per record, as Browse shows it (#263):
/// the condition is evaluated on each printed record with `Self` the field's
/// value, and only where it holds does its fill and text color apply.
#[test]
fn conditional_formatting_prints_on_the_records_where_it_holds() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join("conditional.pdf");
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        exit("Get ( LastError )"),
    ]);
    c.server.lock().unwrap().refresh_render = Some(conditional_document());
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert_eq!(c.result(), "0", "{:?}", c.session.s.failure);
    let saved = std::fs::read(&path).unwrap();
    let layout = &conditional_document().layout;
    let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap();
    let plan = om_layout::pagination::paginate(layout, 9, paper, &[]).unwrap();
    let samples = conditional_samples(&plan);
    assert_eq!(samples.len(), 9);
    let fill = [255, 230, 51];
    for (page, x, y, holds) in samples {
        let raster = raster(&saved, page);
        if holds {
            raster
                .save(output.join(format!("conditional-page-{}.png", page + 1)))
                .unwrap();
        }
        let pixel = raster.get_pixel(x as u32, y as u32);
        let filled = (0..3).all(|i| pixel[i].abs_diff(fill[i]) <= 3);
        assert_eq!(filled, holds, "page {page} at ({x}, {y}): {pixel:?}");
        // The Caption field below it is formatted in Browse only: printing
        // answers Get ( WindowMode ) 3.
        let caption = raster.get_pixel(x as u32 + 160, (y + 26.0) as u32);
        assert_eq!(caption, &image::Rgba([255; 4]), "page {page}");
        // The Amount field's format reads its own LayoutObjectUUID, so it
        // fills on every record: its frame is (360, 138) to (510, 166), and
        // this point is 2 points above its bottom, clear of its text.
        let amount = raster.get_pixel(
            (plan.paper.margins.left + 500.0) as u32,
            (y - 19.0 + 84.0) as u32,
        );
        let green = [153, 230, 153];
        let filled = (0..3).all(|i| amount[i].abs_diff(green[i]) <= 3);
        assert!(filled, "page {page}: {amount:?}");
        // The field's text is red only where the condition holds.
        let red = (12..200).any(|dx| {
            (0..26).any(|dy| {
                let p = raster.get_pixel(
                    (plan.paper.margins.left + f64::from(dx)) as u32,
                    (y - 13.0 + f64::from(dy)) as u32,
                );
                p[0] > 180 && p[1] < 100 && p[2] < 100
            })
        });
        assert_eq!(red, holds, "page {page} at y {y}");
    }

    // A source that does not evaluate conditions refuses such an object,
    // and other objects still refuse conditional formatting.
    let refuse = |layout: &Layout| {
        crate::report::render_pdf(
            layout,
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |o, _| Ok(o.text.clone().unwrap_or_else(|| "Row 09 - café".into())),
        )
        .unwrap_err()
        .message
    };
    assert_eq!(
        refuse(layout),
        "Report object 3's conditional formatting was not evaluated"
    );
    let mut rectangle = layout.clone();
    let conditions = std::mem::take(&mut rectangle.objects.objects[2].calcs.conditions);
    rectangle.objects.objects[6].calcs.conditions = conditions;
    assert_eq!(
        refuse(&rectangle),
        "Unsupported report object or calculation: 7 (Rectangle), conditional formatting"
    );
}

#[test]
fn desktop_pdf_refuses_layout_overflow_before_replacing_a_file() {
    let mut layout = document().layout;
    let long = "A long title that cannot fit inside the deliberately tiny text box";
    layout.objects.objects[0].text = Some(long.into());
    layout.objects.objects[0].bounds = Some(rect(12.0, 6.0, 30.0, 20.0));
    let render = |layout: &Layout| {
        crate::report::render_pdf(
            layout,
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |o, _| Ok(o.text.clone().unwrap_or_else(|| long.into())),
        )
    };
    // The title label is cut at its text box, as Pro clips a label; a field
    // that slides and cannot hold its text still refuses.
    assert!(render(&layout).is_ok());
    let name = layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == 3)
        .unwrap();
    name.flags |= om_layout::sliding::UP;
    name.bounds = Some(rect(12.0, 86.0, 30.0, 20.0));
    assert!(render(&layout).unwrap_err().message.contains("overflows"));
    layout.parts[1].part_type = 3;
    assert!(crate::report::render_pdf(
        &layout,
        1,
        om_layout::pagination::PageSize::default(),
        &[],
        |_, _| Ok(String::new())
    )
    .is_err());
}

/// An ordinary button and a group print as Browse draws them: the group's
/// members at its origin, in `childkey` order over its own frame, and a
/// button's icon left of its label. Other icon placements, sliding grouped
/// objects, other containers and objects inside anything but a group still
/// refuse the report (#263).
#[test]
fn buttons_and_groups_print_and_other_containers_stay_refused() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let render = |layout: &Layout| {
        crate::report::render_pdf(
            layout,
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |o, _| Ok(o.text.clone().unwrap_or_else(|| "Ada".into())),
        )
    };
    let layout = button_group_document().layout;
    let report = render(&layout).unwrap();
    std::fs::write(output.join("buttons-groups.pdf"), &report.bytes).unwrap();
    let page = raster(&report.bytes, 0);
    page.save(output.join("buttons-groups-page-1.png")).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    let [(_, button_x, button_y)] = text_origins(&pdf, 1, "Print button")[..] else {
        panic!("the button's label prints once");
    };
    let [(_, caption_x, caption_y)] = text_origins(&pdf, 1, "Grouped caption")[..] else {
        panic!("the grouped caption prints once");
    };
    // The caption is at the group's origin (12, 120) plus its own (10, 16);
    // the button at (300, 120). Both texts sit 1 point in from the left; a
    // button's label is 2 points below its top and a text object's 1
    // (layout-model.md 6.5). PDF y grows upwards.
    let margins = &report.plan.paper.margins;
    assert!(
        (caption_x - (margins.left + 22.0 + 1.0)).abs() < 0.01,
        "{caption_x}"
    );
    assert!(
        (button_x - (margins.left + 300.0 + 1.0)).abs() < 0.01,
        "{button_x}"
    );
    // Inter's 12-point ascent, 11.6 points, rounds to a 12-point baseline.
    let baseline = |top: f64| 792.0 - (margins.top + top + 12.0);
    assert!(
        (caption_y - baseline(136.0 + 1.0)).abs() < 0.01,
        "{caption_y}"
    );
    assert!(
        (button_y - baseline(120.0 + 2.0)).abs() < 0.01,
        "{button_y}"
    );
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    let near = |p: image::Rgba<u8>, rgb: [f64; 3]| {
        (0..3).all(|i| (f64::from(p[i]) - rgb[i] * 255.0).abs() <= 3.0)
    };
    // Inside the group's backdrop, below its caption; inside the button.
    assert!(
        near(pixel(17.0, 170.0), [0.85, 0.92, 0.85]),
        "{:?}",
        pixel(17.0, 170.0)
    );
    assert!(
        near(pixel(450.0, 148.0), [0.8, 0.85, 0.95]),
        "{:?}",
        pixel(450.0, 148.0)
    );
    // The button's corners are rounded (8 pt): its bottom-left corner pixel is
    // the page's white, a pixel inside that arc, clear of the label, is its fill, and
    // its top edge is its border, drawn inside the bounds.
    assert!(
        near(pixel(300.0, 151.0), [1.0, 1.0, 1.0]),
        "{:?}",
        pixel(300.0, 151.0)
    );
    assert!(
        near(pixel(303.0, 148.0), [0.8, 0.85, 0.95]),
        "{:?}",
        pixel(303.0, 148.0)
    );
    let border = pixel(380.0, 120.0);
    assert!(border[0] < 120 && border[2] > 120, "{border:?}");
    // The caption (group origin plus 10, 16) is drawn over its backdrop:
    // some pixel in its box is ink, not the backdrop's green.
    let ink = (22..242).any(|x| {
        (136..160).any(|y| {
            let p = pixel(f64::from(x), f64::from(y));
            p[0] < 100 && p[1] < 100 && p[2] < 100
        })
    });
    assert!(ink, "the grouped caption is hidden under its backdrop");

    let refuse = |layout: &Layout| render(layout).unwrap_err().message;
    let edited = |id: u64, change: &dyn Fn(&mut LayoutObject)| {
        let mut layout = button_group_document().layout;
        change(
            layout
                .objects
                .objects
                .iter_mut()
                .find(|o| o.id == id)
                .unwrap(),
        );
        layout
    };
    // An icon with display type 4, Button Setup's "Icon at left, text at
    // right" (Add Assignee's button in the Tasks starter), prints left of the
    // label as Browse paints it: 16 points square, centered 6 + 8 points from
    // the frame's left edge, tinted with the frame's border color, the label
    // 16 + 10 points further right.
    let icon = |key: u64, display: i32| {
        move |o: &mut LayoutObject| {
            let button = o.button.as_mut().unwrap();
            (button.library_key, button.icon_size, button.display_type) = (key, 16, display);
        }
    };
    let mut iconic = edited(30, &icon(7, 4));
    iconic.library_images.insert(
        7,
        om_layout::LibraryImage {
            format: om_layout::LibraryImageFormat::Svg,
            data: br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect class="fm_fill" width="16" height="16"/></svg>"#.to_vec(),
        },
    );
    let report = render(&iconic).unwrap();
    std::fs::write(output.join("button-icon.pdf"), &report.bytes).unwrap();
    let page = raster(&report.bytes, 0);
    page.save(output.join("button-icon-page-1.png")).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    let [(_, label_x, label_y)] = text_origins(&pdf, 1, "Print button")[..] else {
        panic!("the button's label prints once");
    };
    assert!((label_x - button_x - 26.0).abs() < 0.01 && (label_y - button_y).abs() < 0.01);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    // The square is (306, 128) to (322, 144); left of it, the button's fill.
    for (x, y) in [(308.0, 130.0), (314.0, 136.0), (320.0, 142.0)] {
        assert!(near(pixel(x, y), [0.2, 0.3, 0.6]), "{:?}", pixel(x, y));
    }
    assert!(
        near(pixel(303.0, 136.0), [0.8, 0.85, 0.95]),
        "{:?}",
        pixel(303.0, 136.0)
    );
    assert!(
        near(pixel(324.0, 136.0), [0.8, 0.85, 0.95]),
        "{:?}",
        pixel(324.0, 136.0)
    );
    // Display type 1, Button Setup's "Image only" (Task Details' Add Assignee
    // in the Tasks starter), prints its icon alone, centered in the frame,
    // without the label the object stores, as FileMaker Pro 26 prints it
    // (#536).
    let mut alone = edited(30, &icon(7, 1));
    alone.library_images = iconic.library_images.clone();
    let report = render(&alone).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    assert!(text_origins(&pdf, 1, "Print button").is_empty());
    let page = raster(&report.bytes, 0);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    // The frame is (300, 120) to (460, 152): the square is (372, 128) to
    // (388, 144), and the fill shows where the left icon was.
    assert!(
        near(pixel(380.0, 136.0), [0.2, 0.3, 0.6]),
        "{:?}",
        pixel(380.0, 136.0)
    );
    assert!(
        near(pixel(314.0, 136.0), [0.8, 0.85, 0.95]),
        "{:?}",
        pixel(314.0, 136.0)
    );
    // Other icon placements, and a missing icon, refuse.
    let mut placed = edited(30, &icon(7, 2));
    placed.library_images = iconic.library_images.clone();
    assert_eq!(
        refuse(&placed),
        "Report button 30 places its icon by display type 2; only display types 1 (icon alone) \
         and 4 (icon left of the label) print"
    );
    assert_eq!(
        refuse(&edited(30, &icon(8, 1))),
        "The icon of report button 30 (library image 8) is missing"
    );
    // Rounded corners print only when all four share an absolute radius.
    let corners = |o: &mut LayoutObject, change: &dyn Fn(&mut om_layout::Length, i32)| {
        let style = o.style.as_mut().unwrap().local_styles.as_mut().unwrap();
        for p in &mut style.components[0].properties {
            if (32..=35).contains(&p.id) {
                change(p.length.as_mut().unwrap(), p.id);
            }
        }
    };
    assert_eq!(
        refuse(&edited(30, &|o| corners(o, &|length, id| if id == 33 {
            length.value = 0.0;
        }))),
        "Report object 30 has corners of different radii; only equal corners print"
    );
    assert_eq!(
        refuse(&edited(30, &|o| corners(o, &|length, _| length.unit = 1))),
        "Report object 30 has an elliptical corner or a corner radius in a relative unit or an \
         automatic value; only circular absolute radii print"
    );
    // A stroke whose inset collapses or reverses cannot stay inside the frame.
    assert_eq!(
        refuse(&edited(30, &|o| {
            rounded(o.style.as_mut().unwrap(), 8.0, 6, 40.0, BLUE);
        })),
        "A rounded report frame's border is too wide for its bounds"
    );
    assert_eq!(
        refuse(&edited(22, &|o| o.flags |= om_layout::sliding::UP)),
        "Report object 22 slides up in a group; sliding groups is unsupported"
    );
    assert_eq!(
        refuse(&edited(20, &|o| {
            o.object_type = 14;
            o.group = None;
            o.popover_button = Some(Default::default());
        })),
        "Unsupported report object or calculation: 20 (PopoverButton)"
    );
    // The same members under a rectangle are not a group's: refused, not
    // dropped from the page.
    assert_eq!(
        refuse(&edited(20, &|o| {
            o.object_type = 5;
            o.group = None;
        })),
        "Nested report objects are unsupported"
    );
}

/// A tab control prints its front panel, the default one, as Browse first
/// shows it (#263): the panel and what it holds, then every tab over it,
/// the front one in its active state. Another panel's objects neither print
/// nor refuse the report.
#[test]
fn tab_controls_print_their_front_panel_and_tabs() {
    let gray = Color {
        red: 0.8,
        green: 0.8,
        blue: 0.8,
        alpha: 1.0,
    };
    let blue = Color {
        red: 0.8,
        green: 0.9,
        blue: 1.0,
        alpha: 1.0,
    };
    let panel_style = || ObjectStyle {
        local_styles: Some(StyleSet {
            components: [(1, gray), (7, blue)]
                .into_iter()
                .map(|(state, color)| om_layout::style::ComponentStyle {
                    component: 1,
                    state,
                    properties: vec![Property {
                        id: 7,
                        color: Some(color),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let panel = |id: u64, name: &str, left: i32, children: Vec<u64>| LayoutObject {
        id,
        object_type: 12,
        parent: Some(50),
        bounds: Some(rect(0.0, 0.0, 400.0, 150.0)),
        text: Some(name.into()),
        tab_panel: Some(om_layout::TabPanelObject {
            tab_left_edge: left,
            tab_width: 100,
            ..Default::default()
        }),
        style: Some(panel_style()),
        children,
        ..Default::default()
    };
    let control = LayoutObject {
        id: 50,
        object_type: 11,
        bounds: Some(rect(20.0, 20.0, 400.0, 150.0)),
        tab: Some(om_layout::TabObject {
            tab_height: 30,
            default_panel_key: 52,
            ..Default::default()
        }),
        children: vec![51, 52],
        ..Default::default()
    };
    let mut alpha = label(
        53,
        "Alpha content",
        rect(10.0, 40.0, 200.0, 20.0),
        12.0,
        false,
    );
    alpha.parent = Some(51);
    let mut beta = label(
        54,
        "Beta content",
        rect(10.0, 40.0, 200.0, 20.0),
        12.0,
        false,
    );
    beta.parent = Some(52);
    // Hidden on the other panel: a portal reports refuse.
    let hidden = LayoutObject {
        id: 55,
        object_type: 9,
        parent: Some(51),
        bounds: Some(rect(10.0, 70.0, 200.0, 60.0)),
        portal: Some(om_layout::PortalObject {
            table_alias_key: 6,
            num_rows: 2.0,
            flags: 0x8,
            ..Default::default()
        }),
        calcs: om_layout::ObjectCalcs {
            portal_sort: Some(vec![0]),
            ..Default::default()
        },
        ..Default::default()
    };
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            table_alias_key: 5,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 200.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![50],
            objects: vec![
                control,
                panel(51, "Alpha", 0, vec![53, 55]),
                panel(52, "Beta", 100, vec![54]),
                alpha,
                beta,
                hidden,
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let paper = om_layout::pagination::PageSize::default();
    let render = |layout: &Layout| {
        crate::report::render_pdf(layout, 1, paper.clone(), &[], |o, _| {
            assert_ne!(o.id, 53, "another panel's object is never read");
            Ok(o.text.clone().unwrap_or_default())
        })
    };
    let report = render(&layout).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::write(output.join("tab-control.pdf"), &report.bytes).unwrap();
    let page = raster(&report.bytes, 0);
    page.save(output.join("tab-control-page-1.png")).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    assert_eq!(text_origins(&pdf, 1, "Beta content").len(), 1);
    assert!(text_origins(&pdf, 1, "Alpha content").is_empty());
    // Both tabs' labels print, centered in their tabs.
    let [(_, alpha_x, _)] = text_origins(&pdf, 1, "Alpha")[..] else {
        panic!("the Alpha tab prints once");
    };
    let [(_, beta_x, _)] = text_origins(&pdf, 1, "Beta")[..] else {
        panic!("the Beta tab prints once");
    };
    let margins = &report.plan.paper.margins;
    assert!(alpha_x > margins.left + 20.0 && alpha_x < margins.left + 120.0);
    assert!(beta_x > margins.left + 120.0 && beta_x < margins.left + 220.0);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    // The Alpha tab in its normal state, the front Beta tab and its panel
    // in their active state.
    assert_eq!(pixel(25.0, 25.0), image::Rgba([204, 204, 204, 255]));
    assert_eq!(pixel(125.0, 25.0), image::Rgba([204, 230, 255, 255]));
    assert_eq!(pixel(300.0, 120.0), image::Rgba([204, 230, 255, 255]));
    // The front panel's own unsupported objects still refuse.
    let mut front = layout.clone();
    front.objects.objects[5].parent = Some(52);
    front.objects.objects[1].children = vec![53];
    front.objects.objects[2].children = vec![54, 55];
    assert_eq!(
        render(&front).unwrap_err().message,
        "Report portal 55 sorts or filters its rows; sorted and filtered portals are \
         unsupported"
    );

    // Tabs keep their panel's equal corner radii, as Browse draws them:
    // the Alpha tab's top-left corner is the page's white.
    let mut rounded = layout.clone();
    for panel in &mut rounded.objects.objects[1..=2] {
        for component in &mut panel
            .style
            .as_mut()
            .unwrap()
            .local_styles
            .as_mut()
            .unwrap()
            .components
        {
            component.properties.extend((32..=35).map(|id| Property {
                id,
                length: Some(om_layout::Length {
                    value: 10.0,
                    unit: 6,
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }
    let report = render(&rounded).unwrap();
    let page = raster(&report.bytes, 0);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    assert_eq!(pixel(20.5, 20.5), image::Rgba([255; 4]));
    assert_eq!(pixel(70.0, 30.0), image::Rgba([204, 204, 204, 255]));

    // Save Records as PDF prints the panel the window shows: Alpha, picked
    // in Browse, rather than the default Beta.
    let path = output.join("tab-control-window.pdf");
    let mut shown = layout.clone();
    shown.id = 1;
    shown.objects.objects.truncate(5);
    shown.objects.objects[1].children = vec![53];
    let mut c = client(vec![
        self::pdf(&crate::scripting::files::file_line(&path), false),
        exit("Get ( LastError )"),
    ]);
    c.server.lock().unwrap().refresh_render = Some(RenderLayout {
        layout: shown,
        field_bindings: Default::default(),
    });
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        panels: [((1, 50), 51)].into(),
        ..Default::default()
    });
    assert_eq!(c.result(), "0", "{:?}", c.session.s.failure);
    let saved = std::fs::read(&path).unwrap();
    let pdf = String::from_utf8_lossy(&saved).into_owned();
    // Nine records, one body a page: the first page's body shows Alpha's.
    assert!(!text_origins(&pdf, 1, "Alpha content").is_empty());
    assert!(text_origins(&pdf, 1, "Beta content").is_empty());
}

/// A portal prints the rows it shows without scrolling (#263), as Browse
/// lays them out: its first-row objects in each row with a related record,
/// reading that record; each row's background in the row style, alternate
/// rows in theirs; rows without a record print their background only.
#[test]
fn portals_print_their_visible_rows_with_each_rows_record() {
    let gray = Color {
        red: 0.8,
        green: 0.8,
        blue: 0.8,
        alpha: 1.0,
    };
    let white = Color {
        red: 1.0,
        green: 1.0,
        blue: 1.0,
        alpha: 1.0,
    };
    let component = |component: i32, properties: Vec<Property>| om_layout::style::ComponentStyle {
        component,
        state: 1,
        properties,
        ..Default::default()
    };
    let fill = |color: Color| Property {
        id: 7,
        color: Some(color),
        ..Default::default()
    };
    // A frame with 1 pt top and bottom borders, white rows, gray
    // alternate rows (property 610 on).
    let mut frame = vec![Property {
        id: 610,
        int: Some(1),
        ..Default::default()
    }];
    for side in [0, 2] {
        frame.extend([
            Property {
                id: 28 + side,
                length: Some(om_layout::Length {
                    value: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
            Property {
                id: 24 + side,
                int: Some(5),
                ..Default::default()
            },
            Property {
                id: 20 + side,
                color: Some(Color {
                    alpha: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
    }
    let portal = LayoutObject {
        id: 40,
        object_type: 9,
        bounds: Some(rect(20.0, 20.0, 300.0, 122.0)),
        portal: Some(om_layout::PortalObject {
            table_alias_key: 6,
            num_rows: 3.0,
            initial_row: 1,
            flags: 0x101,
            ..Default::default()
        }),
        style: Some(ObjectStyle {
            local_styles: Some(StyleSet {
                components: vec![
                    component(1, frame),
                    component(12, vec![fill(white)]),
                    component(13, vec![fill(gray)]),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }),
        children: vec![41],
        ..Default::default()
    };
    // In the first row (rows are 40 points), 2 points below its top.
    let mut name = field(41, 7, rect(10.0, 2.0, 200.0, 20.0));
    name.parent = Some(40);
    name.field_obj
        .as_mut()
        .unwrap()
        .field_spec
        .as_mut()
        .unwrap()
        .table_key = 6;
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            table_alias_key: 5,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 300.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![40],
            objects: vec![portal, name],
            ..Default::default()
        },
        ..Default::default()
    };
    struct Rows(Vec<u64>);
    impl crate::report::ReportContent for Rows {
        fn text(
            &mut self,
            _: &LayoutObject,
            context: crate::report::ContentContext,
        ) -> Result<String, crate::report::ReportError> {
            let row = context.portal.expect("a portal row's object reads its row");
            assert_eq!(
                (row.occurrence, context.record),
                (6, 0),
                "the row's record, the report's record"
            );
            Ok(format!("Row {} #{}", row.record, row.number))
        }
        fn portal_records(
            &mut self,
            portal: &LayoutObject,
            _: crate::report::ContentContext,
        ) -> Result<Option<Vec<u64>>, crate::report::ReportError> {
            assert_eq!(portal.id, 40);
            Ok(Some(self.0.clone()))
        }
    }
    let paper = om_layout::pagination::PageSize::default();
    let render = |layout: &Layout, rows: Vec<u64>| {
        crate::report::render_pdf_with(layout, 1, paper.clone(), &[], &mut Rows(rows))
    };
    let report = render(&layout, vec![101, 102]).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::write(output.join("portal.pdf"), &report.bytes).unwrap();
    let page = raster(&report.bytes, 0);
    page.save(output.join("portal-page-1.png")).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    let [(_, x1, y1)] = text_origins(&pdf, 1, "Row 101 #1")[..] else {
        panic!("the first row prints once");
    };
    let [(_, x2, y2)] = text_origins(&pdf, 1, "Row 102 #2")[..] else {
        panic!("the second row prints once");
    };
    // Rows are (122 - 2) / 3 = 40 points; PDF y grows upwards.
    assert!((x1 - x2).abs() < 0.01 && (y1 - y2 - 40.0).abs() < 0.01);
    let margins = &report.plan.paper.margins;
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    // Inside the frame (top border at y 20): row 1 white, row 2 gray, the
    // empty row 3 white, each right of the field.
    assert_eq!(pixel(300.0, 40.0), image::Rgba([255; 4]));
    assert_eq!(pixel(300.0, 80.0), image::Rgba([204, 204, 204, 255]));
    assert_eq!(pixel(300.0, 120.0), image::Rgba([255; 4]));
    let border = pixel(300.0, 20.5);
    assert!(border[0] < 100, "{border:?}");

    // A sorted or filtered portal, a first-row object outside the first
    // row, and a source that reads no related records refuse.
    let refuse = |layout: &Layout| render(layout, vec![101]).unwrap_err().message;
    let mut sorted = layout.clone();
    sorted.objects.objects[0].portal.as_mut().unwrap().flags |= 0x8;
    sorted.objects.objects[0].calcs.portal_sort = Some(vec![0]);
    assert_eq!(
        refuse(&sorted),
        "Report portal 40 sorts or filters its rows; sorted and filtered portals are \
         unsupported"
    );
    let mut low = layout.clone();
    low.objects.objects[1].bounds = Some(rect(10.0, 30.0, 200.0, 20.0));
    assert_eq!(
        refuse(&low),
        "Report object 41 is outside its portal's first row"
    );
    let unread =
        crate::report::render_pdf(&layout, 1, paper.clone(), &[], |_, _| Ok(String::new()));
    assert_eq!(
        unread.unwrap_err().message,
        "Report portal 40's related records were not read"
    );

    // Seven rows of (232 - 2) / 7 points show seven records, not an eighth
    // below the frame, however the division rounds.
    let mut seven = layout.clone();
    seven.objects.objects[0].bounds = Some(rect(20.0, 20.0, 300.0, 232.0));
    seven.objects.objects[0].portal.as_mut().unwrap().num_rows = 7.0;
    let report = render(&seven, (101..=108).collect()).unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    assert_eq!(text_origins(&pdf, 1, "Row 107 #7").len(), 1);
    assert!(text_origins(&pdf, 1, "Row 108 #8").is_empty());
    // A row style's border prints after the rows, as Browse draws it: a
    // 2 point black bottom rule inside each row.
    let mut ruled = layout.clone();
    let rows = &mut ruled.objects.objects[0]
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[1]
        .properties;
    rows.extend([
        Property {
            id: 30,
            length: Some(om_layout::Length {
                value: 2.0,
                ..Default::default()
            }),
            ..Default::default()
        },
        Property {
            id: 26,
            int: Some(5),
            ..Default::default()
        },
        Property {
            id: 22,
            color: Some(Color {
                alpha: 1.0,
                ..Default::default()
            }),
            ..Default::default()
        },
    ]);
    let report = render(&ruled, vec![101, 102]).unwrap();
    let page = raster(&report.bytes, 0);
    // Row 1 runs from 21 (below the frame's border) to 61.
    let rule = *page.get_pixel((margins.left + 300.0) as u32, (margins.top + 60.0) as u32);
    assert!(rule[0] < 80, "{rule:?}");
}

/// A web viewer prints its frame and, instead of a page, a caption saying
/// OpenMaker does not print web pages, on a plate as Browse labels a viewer
/// without a page: the layout prints and the gap shows. Charts, the other
/// external objects, still refuse.
#[test]
fn web_viewers_print_their_frame_and_a_caption() {
    let viewer = |type_code: u32| {
        let mut style = style(12.0, false, None);
        style.local_styles.as_mut().unwrap().components[0]
            .properties
            .extend([
                Property {
                    id: 30,
                    length: Some(om_layout::Length {
                        value: 2.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Property {
                    id: 26,
                    int: Some(5),
                    ..Default::default()
                },
                Property {
                    id: 22,
                    color: Some(Color {
                        alpha: 1.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ]);
        LayoutObject {
            id: 1,
            object_type: 13,
            bounds: Some(rect(20.0, 20.0, 400.0, 200.0)),
            style: Some(style),
            external: Some(om_layout::ExternalObject {
                type_code,
                ..Default::default()
            }),
            ..Default::default()
        }
    };
    let render = |object: LayoutObject| {
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: vec![Part {
                part_type: 4,
                height: 300.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![1],
                objects: vec![object],
                ..Default::default()
            },
            ..Default::default()
        };
        crate::report::render_pdf(
            &layout,
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |_, _| Ok(String::new()),
        )
    };
    let report = render(viewer(om_layout::ExternalObject::WEB_VIEWER)).unwrap();
    let margins = report.plan.paper.margins.clone();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    let caption = crate::report::WEB_VIEWER_CAPTION;
    let [(_, x, baseline)] = text_origins(&pdf, 1, caption)[..] else {
        panic!("the caption prints once");
    };
    // Centered on the viewer, its plate 4 points above the viewer's bottom.
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let width: f64 = caption
        .chars()
        .map(|ch| {
            f64::from(
                font.glyph_hor_advance(font.glyph_index(ch).unwrap())
                    .unwrap(),
            )
        })
        .sum::<f64>()
        * 12.0
        / f64::from(font.units_per_em());
    let center = margins.left + 20.0 + 200.0;
    assert!((x + width / 2.0 - center).abs() < 0.01, "{x} {width}");
    let bottom = margins.top + 220.0 - 4.0;
    let unit = 12.0 / f64::from(font.units_per_em());
    let (ascent, descent) = (
        f64::from(font.ascender()) * unit,
        -f64::from(font.descender()) * unit,
    );
    // The plate fits the line exactly; its baseline is the rounded ascent
    // below the plate's text top.
    let baseline_at = bottom - 3.0 - descent - ascent + ascent.round();
    assert!((792.0 - baseline - baseline_at).abs() < 0.01);
    let page = raster(&report.bytes, 0);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    // The frame's bottom border, the empty viewer, and the plate's padding.
    assert!(pixel(50.0, 219.0)[0] < 80, "{:?}", pixel(50.0, 219.0));
    assert_eq!(pixel(220.0, 120.0), image::Rgba([255; 4]));
    assert_eq!(pixel(220.0, 214.5), image::Rgba([0xF5, 0xF5, 0xF5, 255]));
    let chart = viewer(u32::from_be_bytes(*b"CHRT"));
    assert_eq!(
        render(chart).unwrap_err().message,
        "Unsupported report object or calculation: 1 (External)"
    );
}

/// The starter solutions' Details layouts set body flag `0x400` (#266). It
/// no longer refuses the report, and the body prints in its own style on
/// every record: FileMaker Pro 26 shows Task Details' two pages with the
/// same fill, and that body's "Use alternate row state" is off.
#[test]
fn details_body_flag_prints_the_body_style_on_every_record() {
    let fill = |color: Color| {
        Some(ObjectStyle {
            local_styles: Some(StyleSet {
                components: vec![om_layout::style::ComponentStyle {
                    component: 1,
                    state: 1,
                    properties: vec![Property {
                        id: 7,
                        color: Some(color),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        })
    };
    let gray = Color {
        red: 0.8,
        green: 0.8,
        blue: 0.8,
        alpha: 1.0,
    };
    let blue = Color {
        red: 0.8,
        green: 0.9,
        blue: 1.0,
        alpha: 1.0,
    };
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 400.0,
            flags: om_layout::pagination::DETAILS_BODY_FLAG,
            style: fill(gray),
            alt_style: fill(blue),
            ..Default::default()
        }],
        ..Default::default()
    };
    let report = crate::report::render_pdf(
        &layout,
        2,
        om_layout::pagination::PageSize::default(),
        &[],
        |_, _| Ok(String::new()),
    )
    .unwrap();
    // One 400 point body a page.
    assert_eq!(report.plan.pages.len(), 2);
    let margins = report.plan.paper.margins.clone();
    let body = |page: usize| {
        *raster(&report.bytes, page)
            .get_pixel((margins.left + 100.0) as u32, (margins.top + 100.0) as u32)
    };
    // The second record's body is gray too, not the alternate blue.
    for page in 0..2 {
        assert_eq!(body(page), image::Rgba([204, 204, 204, 255]), "page {page}");
    }
}

/// A page is painted as FileMaker Pro 26's PDFs paint it (#536): the layout
/// background under everything, the parts, and below the last part, down to
/// the footer, the body's fill. A part without a fill shows the layout
/// background.
#[test]
fn pages_show_the_layout_background_and_the_body_fill_below_the_last_part() {
    let color = |red: f32, green: f32, blue: f32| Color {
        red,
        green,
        blue,
        alpha: 1.0,
    };
    let part = |part_type: i32, top: f64, height: f64, fill: Option<Color>| Part {
        part_type,
        top,
        height,
        style: fill.map(|fill| style(12.0, false, Some(fill))),
        ..Default::default()
    };
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            background_style: Some(style(12.0, false, Some(color(1.0, 0.0, 0.0)))),
            ..Default::default()
        }),
        parts: vec![
            part(1, 0.0, 30.0, None),
            part(4, 30.0, 100.0, Some(color(0.0, 0.0, 1.0))),
            part(7, 130.0, 40.0, Some(color(0.0, 1.0, 0.0))),
        ],
        ..Default::default()
    };
    let report = crate::report::render_pdf(
        &layout,
        1,
        om_layout::pagination::PageSize::default(),
        &[],
        |_, _| Ok(String::new()),
    )
    .unwrap();
    let page = raster(&report.bytes, 0);
    let margins = report.plan.paper.margins.clone();
    let pixel = |y: f64| {
        let p = *page.get_pixel((margins.left + 100.0) as u32, y as u32);
        [p[0], p[1], p[2]]
    };
    let top = margins.top;
    // The unfilled header shows the layout background; then the body.
    assert_eq!(pixel(top + 15.0), [255, 0, 0]);
    assert_eq!(pixel(top + 80.0), [0, 0, 255]);
    // Below the body, down to the footer at the page bottom, the body's fill.
    assert_eq!(pixel(top + 300.0), [0, 0, 255]);
    let bottom = 792.0 - margins.bottom;
    assert_eq!(pixel(bottom - 45.0), [0, 0, 255]);
    assert_eq!(pixel(bottom - 20.0), [0, 255, 0]);
    // Outside the printable area the page stays white.
    assert_eq!(pixel(bottom + 10.0), [255, 255, 255]);
}

/// A field's borders and fill print unless the layout delineates fields on
/// the current record only (layout option bit 0): Claris documents that they
/// then show on the current record only, never in print, and FileMaker Pro
/// prints neither the starter lists' field underlines (#263) nor the Details
/// layouts' pop-up and photo fills (#536).
#[test]
fn field_borders_and_fill_print_unless_delineated_on_the_current_record_only() {
    let mut name = field(1, 1, rect(10.0, 10.0, 200.0, 30.0));
    name.style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[0]
        .properties
        .extend([
            Property {
                id: 30,
                length: Some(om_layout::Length {
                    value: 2.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
            Property {
                id: 26,
                int: Some(5),
                ..Default::default()
            },
            Property {
                id: 22,
                color: Some(Color {
                    alpha: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            },
            // A light blue fill.
            Property {
                id: 7,
                color: Some(Color {
                    red: 0.6,
                    green: 0.8,
                    blue: 1.0,
                    alpha: 1.0,
                }),
                ..Default::default()
            },
        ]);
    let layout = |flags: u64| Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            flags,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 100.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1],
            objects: vec![name.clone()],
            ..Default::default()
        },
        ..Default::default()
    };
    let pixel = |flags: u64, y: f64| {
        let report = crate::report::render_pdf(
            &layout(flags),
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |_, _| Ok("Ada".into()),
        )
        .unwrap();
        let page = raster(&report.bytes, 0);
        let margins = &report.plan.paper.margins;
        // Right of the text: inside the bottom border, or above it.
        *page.get_pixel((margins.left + 150.0) as u32, (margins.top + y) as u32)
    };
    assert!(pixel(0, 39.0)[0] < 80, "{:?}", pixel(0, 39.0));
    assert_eq!(pixel(0x1, 39.0), image::Rgba([255; 4]));
    assert_eq!(pixel(0, 14.0), image::Rgba([153, 204, 255, 255]));
    assert_eq!(pixel(0x1, 14.0), image::Rgba([255; 4]));
}

/// Text whose font is Arial or Helvetica prints in the bundled Liberation
/// Sans, which is metric-compatible with Arial, so it wraps where FileMaker
/// Pro's does (#265); any other font, or none, stays on Inter. A face is
/// embedded only when the document uses it. Italic Arial or Helvetica, from
/// the style's font-style or a character run's italic face, prints in
/// Liberation Sans Italic or Bold Italic; italic in another family refuses
/// with 3 (no italic Inter is bundled).
#[test]
fn arial_text_prints_in_liberation_sans_and_other_text_in_inter() {
    let text = |family: Option<&str>, bold: bool, value: &str| {
        let mut text = label(1, value, rect(10.0, 10.0, 300.0, 40.0), 12.0, bold);
        if let Some(family) = family {
            text.style
                .as_mut()
                .unwrap()
                .local_styles
                .as_mut()
                .unwrap()
                .components[0]
                .properties
                .push(Property {
                    id: 64,
                    string: Some(family.to_owned()),
                    ..Default::default()
                });
        }
        text
    };
    let render = |text: LayoutObject| {
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: vec![Part {
                part_type: 4,
                height: 60.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: vec![1],
                objects: vec![text],
                ..Default::default()
            },
            ..Default::default()
        };
        crate::report::render_pdf(
            &layout,
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |o, _| Ok(o.text.clone().unwrap_or_default()),
        )
        .map(|report| String::from_utf8_lossy(&report.bytes).into_owned())
    };
    let labelled =
        |family: Option<&str>, bold: bool| render(text(family, bold, "Ada Lovelace")).unwrap();
    let labelled_value =
        |family: Option<&str>, value: &str| render(text(family, false, value)).unwrap();
    for family in ["arial,sans-serif", "Arial", "Helvetica"] {
        let pdf = labelled(Some(family), false);
        assert!(pdf.contains("/BaseFont /LiberationSans"), "{family}");
        assert!(pdf.contains("/F3 "), "{family}");
    }
    let bold = labelled(Some("arial,sans-serif"), true);
    assert!(bold.contains("/BaseFont /LiberationSans-Bold"));
    // A blank line sets no font, so a page whose Arial text is only blank
    // lines references no face it does not embed.
    let blank = labelled_value(Some("arial,sans-serif"), "\n");
    assert!(!blank.contains("/F3"));
    for family in [None, Some("Verdana")] {
        let pdf = labelled(family, false);
        assert!(!pdf.contains("LiberationSans"), "{family:?}");
        assert_eq!(text_origins(&pdf, 1, "Ada Lovelace").len(), 1, "{family:?}");
    }
    // Italic from the style (font-style 2, italic) and from a character
    // run's italic face (512 under mask 512), which a run can also turn off.
    let italic_style = |mut text: LayoutObject| {
        let local = text.style.as_mut().unwrap().local_styles.as_mut().unwrap();
        local.components[0].properties.push(Property {
            id: 67,
            int: Some(2),
            ..Default::default()
        });
        text
    };
    let italic_run = |mut text: LayoutObject, italic: bool| {
        let rich = text.rich_text.as_mut().unwrap();
        let run = &mut rich.character_runs[0].style;
        run.faces = Some(run.faces.unwrap_or(0) | if italic { 512 } else { 0 });
        run.face_mask = Some(256 | 512);
        text
    };
    let pdf = render(italic_style(text(Some("Arial"), false, "Ada Lovelace"))).unwrap();
    assert!(pdf.contains("/BaseFont /LiberationSans-Italic"), "{pdf}");
    assert!(pdf.contains("/F5 ") && !pdf.contains("/F3 "));
    assert!(pdf.contains("/ItalicAngle -12"));
    let pdf = render(italic_run(
        text(Some("Helvetica"), true, "Ada Lovelace"),
        true,
    ))
    .unwrap();
    assert!(pdf.contains("/BaseFont /LiberationSans-BoldItalic"));
    let upright = italic_run(italic_style(text(Some("Arial"), false, "Ada")), false);
    let pdf = render(upright).unwrap();
    assert!(pdf.contains("/F3 ") && !pdf.contains("LiberationSans-Italic"));
    // The run's own font chooses the face, as Browse draws it: an Arial
    // italic run on a Verdana label prints, a Verdana one on an Arial
    // label refuses.
    let run_family = |mut text: LayoutObject, family: &str| {
        let rich = text.rich_text.as_mut().unwrap();
        rich.character_runs[0].style.font_family = Some(family.to_owned());
        text
    };
    let pdf = render(run_family(
        italic_run(text(Some("Verdana"), false, "Ada"), true),
        "Arial",
    ))
    .unwrap();
    assert!(pdf.contains("/BaseFont /LiberationSans-Italic"));
    for italic in [
        italic_style(text(Some("Verdana"), false, "Ada")),
        italic_run(text(None, false, "Ada"), true),
        run_family(
            italic_run(text(Some("Arial"), false, "Ada"), true),
            "Verdana",
        ),
    ] {
        let error = render(italic).unwrap_err();
        assert_eq!(error.code, 3, "{}", error.message);
    }
    // An italic glyph whose outline reaches past its text box, though its
    // advance fits (a right-aligned 36-point `f` in a 13-point label, or an
    // 11-point one in a 7-point label, into the 1-point inset), is cut at
    // the box's sides and the frame's top and bottom; one well inside it is
    // not.
    let spilling = |align: i32, width: f64, size: f64| {
        let mut text = italic_style(label(1, "f", rect(10.0, 10.0, width, 44.0), size, false));
        let props = &mut text
            .style
            .as_mut()
            .unwrap()
            .local_styles
            .as_mut()
            .unwrap()
            .components[0]
            .properties;
        props.push(Property {
            id: 64,
            string: Some("Arial".into()),
            ..Default::default()
        });
        props.push(Property {
            id: 74,
            int: Some(align),
            ..Default::default()
        });
        render(text).unwrap()
    };
    // The label's clip: its text box's width (the frame less the 1-point
    // inset at each side) and its frame's height, so nothing above or below
    // the box is cut for a glyph's side.
    let label_clip = |pdf: &str, width: f64, height: f64| {
        pdf.matches(&format!(" {width} {height} re\nW\nn")).count()
    };
    assert_eq!(label_clip(&spilling(3, 13.0, 36.0), 11.0, 44.0), 1);
    assert_eq!(label_clip(&spilling(3, 7.0, 11.0), 5.0, 44.0), 1);
    assert_eq!(label_clip(&spilling(1, 200.0, 36.0), 198.0, 44.0), 0);
}

/// A field's text sits inside its text component's margins (properties
/// 105-108, here in em as the starter lists store them), and text too long
/// for the field prints cut at its text box instead of refusing the report
/// (#263, #536). Text objects and sliding fields still refuse.
#[test]
fn field_text_keeps_its_margins_and_is_cut_at_its_text_box() {
    let mut name = field(1, 1, rect(10.0, 10.0, 300.0, 44.0));
    name.style = Some(style(16.0, false, None));
    let margins = name.style.as_mut().unwrap().local_styles.as_mut().unwrap();
    margins.components.push(om_layout::style::ComponentStyle {
        component: 3,
        state: 1,
        properties: [(105, 0.875), (106, 0.25), (107, 0.625), (108, 0.25)]
            .into_iter()
            .map(|(id, value)| Property {
                id,
                length: Some(om_layout::Length {
                    value,
                    unit: 7,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    let layout = |object: LayoutObject| Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 100.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1],
            objects: vec![object],
            ..Default::default()
        },
        ..Default::default()
    };
    let render = |object: LayoutObject, value: &'static str| {
        crate::report::render_pdf(
            &layout(object),
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            move |_, _| Ok(value.to_owned()),
        )
    };
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let unit = 16.0 / f64::from(font.units_per_em());
    let (ascent, descent) = (
        f64::from(font.ascender()) * unit,
        -f64::from(font.descender()) * unit,
    );
    let pitch = (ascent + descent).max(16.0 * 1.2);
    // The first baseline sits a whole number of points below the text box's
    // top: the ascent, rounded, as FileMaker Pro 26 sets it.
    let first = ascent.round();
    // 14 points below the field's top and 4 in from its left edge.
    let report = render(name.clone(), "Ada").unwrap();
    let paper = report.plan.paper.margins.clone();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    let [(_, x, baseline)] = text_origins(&pdf, 1, "Ada")[..] else {
        panic!("the value prints once");
    };
    assert!((x - (paper.left + 10.0 + 4.0)).abs() < 0.01, "{x}");
    let top = paper.top + 10.0 + 14.0;
    assert!(
        (792.0 - baseline - (top + first)).abs() < 0.01,
        "{baseline}"
    );
    // With the starters' 1-point border widths on every side (a style on
    // the bottom only), the text is inset by the border too: 15 points down
    // and 5 in, as FileMaker Pro 26 clips a Task Details edit box (#536).
    let mut bordered = name.clone();
    let properties = &mut bordered
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[0]
        .properties;
    for side in 0..4 {
        properties.push(Property {
            id: 28 + side,
            length: Some(om_layout::Length {
                value: 1.0,
                unit: 6,
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    properties.push(Property {
        id: 26,
        int: Some(5),
        ..Default::default()
    });
    let pdf = String::from_utf8_lossy(&render(bordered.clone(), "Ada").unwrap().bytes).into_owned();
    let [(_, x, baseline)] = text_origins(&pdf, 1, "Ada")[..] else {
        panic!("the value prints once");
    };
    assert!((x - (paper.left + 10.0 + 5.0)).abs() < 0.01, "{x}");
    assert!(
        (792.0 - baseline - (top + 1.0 + first)).abs() < 0.01,
        "{baseline}"
    );
    // In Arial, as the starters set it, Pro's Task List puts a 16-point row
    // value's baseline 29 points below the field's top (15 of inset, then
    // 14 rather than the exact 14.48 ascent) and 5 in (#263's 60 records).
    let mut arial = bordered.clone();
    arial
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[0]
        .properties
        .push(Property {
            id: 64,
            string: Some("arial,sans-serif".into()),
            ..Default::default()
        });
    let pdf = String::from_utf8_lossy(&render(arial, "Ada").unwrap().bytes).into_owned();
    assert_eq!(
        pdf.matches("/F3 16 Tf").count(),
        1,
        "one Liberation Sans line"
    );
    let matrix = pdf
        .split("/F3 16 Tf")
        .nth(1)
        .unwrap()
        .lines()
        .find(|l| l.ends_with(" Tm"));
    let numbers: Vec<f64> = matrix
        .unwrap()
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    let (x, baseline) = (numbers[4], numbers[5]);
    assert!((x - (paper.left + 10.0 + 5.0)).abs() < 0.01, "{x}");
    assert!(
        (792.0 - baseline - (paper.top + 10.0 + 29.0)).abs() < 0.01,
        "{baseline}"
    );
    // Its overflowing text is cut 1 point higher too: the clip runs from
    // 15 points below the frame's top to 11 above its bottom (46 + 15 to
    // 90 - 11 on the page; PDF y 713, 18 points tall).
    let cut = String::from_utf8_lossy(
        &render(bordered.clone(), "First\nSecond\nThird")
            .unwrap()
            .bytes,
    )
    .into_owned();
    assert!(
        cut.contains("46 713 300 18 re"),
        "the bordered text box clip"
    );
    let plain =
        String::from_utf8_lossy(&render(name.clone(), "First\nSecond\nThird").unwrap().bytes)
            .into_owned();
    assert!(
        plain.contains("46 712 300 20 re"),
        "the borderless text box clip"
    );
    // And it is inset on the right: right-aligned text ends 1 point further in.
    let right = |mut field: LayoutObject| {
        field
            .style
            .as_mut()
            .unwrap()
            .local_styles
            .as_mut()
            .unwrap()
            .components[0]
            .properties
            .push(Property {
                id: 74,
                int: Some(3),
                ..Default::default()
            });
        let pdf = String::from_utf8_lossy(&render(field, "Ada").unwrap().bytes).into_owned();
        text_origins(&pdf, 1, "Ada")[0].1
    };
    // A pop-up menu with its arrow, styled as the starters' are (a solid
    // bottom border, none on the other sides, a 30-point icon component).
    // Pro 26 insets its text by the bottom border only and keeps the icon's
    // width clear at the right: Task List's Status text box is the frame
    // less 14 / 34 / 11 / 4 points (#263).
    let mut pop_up = bordered.clone();
    pop_up.field_obj.as_mut().unwrap().display_type = 2;
    let styles = pop_up
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap();
    styles.components[0]
        .properties
        .extend([24, 25, 27].map(|id| Property {
            id,
            int: Some(1),
            ..Default::default()
        }));
    styles.components.push(om_layout::style::ComponentStyle {
        component: 4,
        state: 1,
        properties: vec![Property {
            id: 109,
            length: Some(om_layout::Length {
                value: 30.0,
                unit: 6,
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    });
    let pdf = String::from_utf8_lossy(&render(pop_up.clone(), "Ada").unwrap().bytes).into_owned();
    let [(_, x, baseline)] = text_origins(&pdf, 1, "Ada")[..] else {
        panic!("the pop-up's value prints once");
    };
    assert!((x - (paper.left + 10.0 + 4.0)).abs() < 0.01, "{x}");
    assert!(
        (792.0 - baseline - (top + first)).abs() < 0.01,
        "{baseline}"
    );
    assert!((right(name.clone()) - right(pop_up) - 30.0).abs() < 0.01);
    assert!((right(name.clone()) - right(bordered) - 1.0).abs() < 0.01);
    // Three lines in a box with room for one. The first prints whole. The
    // second stays in the PDF's text but is cut at the text box (10 points
    // above the field's bottom), as FileMaker Pro 26 cuts it (#536), so none
    // of its ink shows. The third starts below the field and does not print.
    let report = render(name.clone(), "First\nSecond\nThird").unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    assert_eq!(text_origins(&pdf, 1, "First").len(), 1);
    assert_eq!(text_origins(&pdf, 1, "Second").len(), 1);
    assert!(text_origins(&pdf, 1, "Third").is_empty());
    assert!(top + pitch < paper.top + 54.0 && top + 2.0 * pitch >= paper.top + 54.0);
    let page = raster(&report.bytes, 0);
    let ink = |y: f64| {
        (paper.left as u32 + 14..paper.left as u32 + 120)
            .any(|x| page.get_pixel(x, y as u32)[0] < 160)
    };
    assert!(ink(top + pitch - 6.0), "the first line shows");
    for y in 0..30 {
        let y = paper.top + 44.0 + f64::from(y);
        assert!(!ink(y), "ink below the text box at {y}");
    }
    // The same text in a text object is cut at its text box too, as Pro
    // clips every label's text (layout-model.md 6.5): the first line
    // prints, and the third, which starts below the object, does not.
    let mut label = name.clone();
    label.object_type = 2;
    label.field_obj = None;
    label.text = Some("First\nSecond\nThird".into());
    let pdf =
        String::from_utf8_lossy(&render(label, "First\nSecond\nThird").unwrap().bytes).into_owned();
    assert_eq!(text_origins(&pdf, 1, "First").len(), 1);
    assert!(text_origins(&pdf, 1, "Third").is_empty());
    // A label that fit its frame's width before its 1-point inset now wraps
    // inside it, as Pro's narrower text box would, and is cut rather than
    // refused: 13-point Arial "WW" (24.54 points) in a 25 by 16 label. So
    // is a glyph wider than its label.
    for (value, width, clip_left, clip_width) in [("WW", 25.0, 10.0, 25.0), ("W", 8.0, 11.0, 6.0)] {
        let mut tight = self::label(2, value, rect(10.0, 10.0, width, 16.0), 13.0, false);
        tight
            .style
            .as_mut()
            .unwrap()
            .local_styles
            .as_mut()
            .unwrap()
            .components[0]
            .properties
            .push(Property {
                id: 64,
                string: Some("Arial".into()),
                ..Default::default()
            });
        let pdf = String::from_utf8_lossy(&render(tight, value).unwrap().bytes).into_owned();
        // Every line is cut at the text box's top and bottom, and at the
        // frame's sides, or the box's (the frame less its 1-point inset)
        // where its glyph's ink reaches past them, as the wide "W" does;
        // "WW"'s second "W" starts inside the frame, so it stays in the
        // PDF's text, as Pro keeps a field's.
        let lines = pdf.matches("/F3 13 Tf").count();
        assert_eq!(lines, value.len(), "{value}");
        let clip = format!("{} 731 {clip_width} 14 re", paper.left + clip_left);
        assert_eq!(pdf.matches(&clip).count(), lines, "{value}: {clip}");
    }
    // A rounded baseline can carry a descender below a field whose exact
    // metrics just fit it: 13-point Arial "g" in a 14.55-point field with no
    // padding (ascent plus descent 14.52, baseline 12 rather than 11.77).
    // Its text is cut at the field then, so the page has a second clip.
    let mut snug = field(3, 1, rect(10.0, 10.0, 200.0, 14.55));
    snug.style = Some(style(13.0, false, None));
    snug.style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[0]
        .properties
        .push(Property {
            id: 64,
            string: Some("Arial".into()),
            ..Default::default()
        });
    let pdf = String::from_utf8_lossy(&render(snug, "g").unwrap().bytes).into_owned();
    assert_eq!(
        pdf.matches(" re\nW\nn").count(),
        2,
        "the part's and the text's"
    );
    // A margin given as an automatic keyword is no concrete inset.
    let mut automatic = name.clone();
    let text = &mut automatic
        .style
        .as_mut()
        .unwrap()
        .local_styles
        .as_mut()
        .unwrap()
        .components[1];
    text.properties[0].length.as_mut().unwrap().auto_value = 2;
    assert_eq!(
        render(automatic, "Ada").unwrap_err().message,
        "Report object 1 has a text margin in a relative unit or an automatic value"
    );
    let mut sliding = name;
    sliding.flags |= om_layout::sliding::UP;
    assert!(render(sliding, "First\nSecond\nThird")
        .unwrap_err()
        .message
        .starts_with("Report text overflows its object"));
}

/// A translucent color prints blended over what is beneath it, as Browse
/// draws it: through an opacity graphics state in the PDF (#622; the
/// Meetings starter's Meeting Details fills a button at 50% while its
/// Duration is empty). A color outside 0-1 still refuses.
#[test]
fn translucent_colors_print_blended_over_what_is_beneath() {
    let rectangle = |id: u64, bounds: Rect, fill: Color| LayoutObject {
        id,
        object_type: 5,
        bounds: Some(bounds),
        style: Some(style(12.0, false, Some(fill))),
        ..Default::default()
    };
    let blue = Color {
        blue: 1.0,
        alpha: 1.0,
        ..Default::default()
    };
    let green = Color {
        green: 1.0,
        alpha: 1.0,
        ..Default::default()
    };
    let layout = |red: Color| Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 200.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1, 2, 3],
            objects: vec![
                rectangle(1, rect(20.0, 20.0, 200.0, 100.0), blue),
                rectangle(2, rect(120.0, 20.0, 200.0, 100.0), red),
                // Opaque, drawn after the translucent rectangle and over it.
                rectangle(3, rect(250.0, 60.0, 100.0, 100.0), green),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let render = |red: Color| {
        crate::report::render_pdf(
            &layout(red),
            1,
            om_layout::pagination::PageSize::default(),
            &[],
            |_, _| Ok(String::new()),
        )
    };
    let report = render(Color {
        red: 1.0,
        alpha: 0.5,
        ..Default::default()
    })
    .unwrap();
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    // 0.5 is kept in 255ths, as Browse's colors are: 128 / 255.
    assert!(pdf.contains("/ca 0.5019608") && pdf.contains("/CA 0.5019608"));
    let margins = report.plan.paper.margins.clone();
    let page = raster(&report.bytes, 0);
    let pixel =
        |x: f64, y: f64| *page.get_pixel((margins.left + x) as u32, (margins.top + y) as u32);
    let near = |p: image::Rgba<u8>, rgb: [u8; 3]| (0..3).all(|i| p[i].abs_diff(rgb[i]) <= 3);
    // Blue alone, red over blue, red over the white page.
    assert!(
        near(pixel(70.0, 70.0), [0, 0, 255]),
        "{:?}",
        pixel(70.0, 70.0)
    );
    assert!(
        near(pixel(170.0, 70.0), [128, 0, 127]),
        "{:?}",
        pixel(170.0, 70.0)
    );
    assert!(
        near(pixel(300.0, 40.0), [255, 127, 127]),
        "{:?}",
        pixel(300.0, 40.0)
    );
    // Later drawing is opaque again: the green rectangle drawn over the
    // translucent one shows no red or blue.
    assert!(
        near(pixel(280.0, 100.0), [0, 255, 0]),
        "{:?}",
        pixel(280.0, 100.0)
    );
    assert!(
        near(pixel(330.0, 140.0), [0, 255, 0]),
        "{:?}",
        pixel(330.0, 140.0)
    );
    assert_eq!(
        render(Color {
            red: 1.0,
            alpha: 1.5,
            ..Default::default()
        })
        .unwrap_err()
        .message,
        "Invalid report colors are unsupported"
    );
}

/// The starter solutions' layouts are 1024 pt wide, store orientation 1 and
/// a column count of 2 with no column width (#263). They print portrait, in
/// one column, as much as fits: an object wholly beyond the printable width
/// is left out with its contents, unchecked, and one crossing it is cut.
#[test]
fn wide_layouts_print_what_fits_the_page() {
    let blue = Color {
        red: 0.2,
        green: 0.3,
        blue: 0.9,
        alpha: 1.0,
    };
    let mut caption = label(4, "Off page", rect(10.0, 10.0, 100.0, 20.0), 12.0, false);
    caption.parent = Some(3);
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 1024.0,
            orientation: 1,
            print_column_count: 2,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 100.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1, 2, 5, 3],
            objects: vec![
                LayoutObject {
                    id: 1,
                    object_type: 5,
                    bounds: Some(rect(400.0, 10.0, 400.0, 40.0)),
                    style: Some(style(12.0, false, Some(blue))),
                    ..Default::default()
                },
                label(2, "Printed", rect(10.0, 60.0, 200.0, 20.0), 12.0, false),
                // An 8 pt vertical line on the printable edge (612 points
                // less the printer's 18-point side margins): half its
                // stroke is inside.
                LayoutObject {
                    id: 5,
                    object_type: 4,
                    bounds: Some(rect(576.0, 60.0, 0.0, 30.0)),
                    style: Some({
                        let mut line = style(12.0, false, None);
                        line.local_styles.as_mut().unwrap().components[0]
                            .properties
                            .push(Property {
                                id: 28,
                                length: Some(om_layout::Length {
                                    value: 8.0,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            });
                        line
                    }),
                    ..Default::default()
                },
                // A portal, which reports refuse, wholly beyond the page.
                LayoutObject {
                    id: 3,
                    object_type: 9,
                    bounds: Some(rect(700.0, 10.0, 200.0, 60.0)),
                    children: vec![4],
                    ..Default::default()
                },
                caption,
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let render = |layout: &Layout| {
        let paper = om_layout::pagination::PageSize::for_layout(layout)?;
        crate::report::render_pdf(layout, 1, paper, &[], |o, _| {
            assert_ne!(o.id, 4, "an object beyond the page is never read");
            Ok(o.text.clone().unwrap_or_default())
        })
        .map_err(|e| e.message)
    };
    let report = render(&layout).unwrap();
    assert_eq!(
        (report.plan.paper.width, report.plan.paper.height),
        (612.0, 792.0)
    );
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    assert_eq!(text_origins(&pdf, 1, "Printed").len(), 1);
    assert!(text_origins(&pdf, 1, "Off page").is_empty());
    // The rectangle fills up to the printable edge (18 + 576) and no further.
    let page = raster(&report.bytes, 0);
    let fill = *page.get_pixel(18 + 571, 18 + 30);
    assert!(fill[2] > 200 && fill[0] < 80, "{fill:?}");
    assert_eq!(*page.get_pixel(18 + 581, 18 + 30), image::Rgba([255; 4]));
    // The edge line's inner half prints; its outer half is cut.
    let stroke = *page.get_pixel(18 + 574, 18 + 75);
    assert!(stroke[0] < 80 && stroke[2] < 80, "{stroke:?}");
    assert_eq!(*page.get_pixel(18 + 578, 18 + 75), image::Rgba([255; 4]));

    // "Use fixed page margins" (flag 0x40, #609): within stored 36 pt
    // margins the text prints 18 pt further right and lower than within the
    // printer's 18 pt, as FileMaker Pro 26 moved Task List's. Stored
    // margins without the flag change nothing.
    let printed = |layout: &Layout| {
        let report = render(layout).unwrap();
        let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
        let [(_, x, y)] = text_origins(&pdf, 1, "Printed")[..] else {
            panic!("one Printed");
        };
        (x, y)
    };
    let (x, y) = printed(&layout);
    let mut stored = layout.clone();
    stored.def.as_mut().unwrap().custom_margins = Some(om_layout::Rect {
        top: 36.0,
        left: 36.0,
        bottom: 36.0,
        right: 36.0,
        ..Default::default()
    });
    assert_eq!(printed(&stored), (x, y));
    let mut fixed = stored.clone();
    fixed.def.as_mut().unwrap().flags |= om_layout::paper::FIXED_PAGE_MARGINS;
    let (fx, fy) = printed(&fixed);
    assert!(
        (fx - x - 18.0).abs() < 0.01 && (y - fy - 18.0).abs() < 0.01,
        "{x},{y} -> {fx},{fy}"
    );

    // Hide when printing (0x200) leaves an object out with its
    // contents, unchecked, as beyond the page: the label does not print,
    // nor does the portal (which reports refuse) moved onto the page.
    let mut hidden = layout.clone();
    hidden.objects.objects[1].flags |= om_layout::sliding::NON_PRINTING;
    hidden.objects.objects[3].bounds = Some(rect(300.0, 10.0, 200.0, 60.0));
    hidden.objects.objects[3].flags |= om_layout::sliding::NON_PRINTING;
    let pdf = String::from_utf8_lossy(&render(&hidden).unwrap().bytes).into_owned();
    assert!(text_origins(&pdf, 1, "Printed").is_empty());
    assert!(text_origins(&pdf, 1, "Off page").is_empty());

    let refused = |change: &dyn Fn(&mut Layout)| {
        let mut layout = layout.clone();
        change(&mut layout);
        render(&layout).unwrap_err()
    };
    // "Print in 2 columns" is flag 0x400 (#925): the column count and width
    // FileMaker Pro saves with it unchecked print in one column.
    let mut saved = layout.clone();
    saved.def.as_mut().unwrap().print_column_width = 270.0;
    assert!(render(&saved).is_ok());
    assert_eq!(
        refused(&|l| l.def.as_mut().unwrap().flags |= om_layout::pagination::PRINT_IN_COLUMNS),
        "Multi-column and responsive reports are unsupported"
    );
    assert_eq!(
        refused(&|l| l.objects.objects[1].flags |= om_layout::sliding::UP),
        "Report object 2 slides up in a part with objects beyond the printable width; how \
         those slide is unmeasured"
    );
}

#[test]
fn desktop_pdf_preserves_root_stacking_with_storage_fallbacks() {
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PDF_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let title = label(
        1,
        "VISIBLE LABEL",
        rect(24.0, 24.0, 220.0, 40.0),
        18.0,
        false,
    );
    let background = LayoutObject {
        id: 2,
        object_type: 5,
        bounds: Some(rect(12.0, 12.0, 260.0, 70.0)),
        style: Some(style(
            12.0,
            false,
            Some(Color {
                red: 0.7,
                green: 0.8,
                blue: 0.9,
                alpha: 1.0,
            }),
        )),
        ..Default::default()
    };
    let mut layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 100.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![2, 1],
            objects: vec![title, background],
            ..Default::default()
        },
        ..Default::default()
    };
    let first = crate::report::render_pdf(
        &layout,
        1,
        om_layout::pagination::PageSize::default(),
        &[],
        |o, _| Ok(o.text.clone().unwrap_or_default()),
    )
    .unwrap();
    std::fs::write(
        output.join("root-order-stored-text-first.pdf"),
        &first.bytes,
    )
    .unwrap();
    layout.objects.objects.swap(0, 1);
    let second = crate::report::render_pdf(
        &layout,
        1,
        om_layout::pagination::PageSize::default(),
        &[],
        |o, _| Ok(o.text.clone().unwrap_or_default()),
    )
    .unwrap();
    std::fs::write(
        output.join("root-order-stored-rectangle-first.pdf"),
        &second.bytes,
    )
    .unwrap();
    assert_eq!(
        first.bytes, second.bytes,
        "Storage order must not change the layout's stacking order"
    );
    // Repeated/missing root references do not duplicate objects, and an
    // unattached top-level label still follows the listed background.
    layout.objects.root = vec![2, 404, 2];
    let fallback = crate::report::render_pdf(
        &layout,
        1,
        om_layout::pagination::PageSize::default(),
        &[],
        |o, _| Ok(o.text.clone().unwrap_or_default()),
    )
    .unwrap();
    assert_eq!(fallback.bytes, first.bytes);
}

#[test]
fn preview_script_moves_physical_pages_and_keeps_browse_record_and_loop_exit() {
    let movement = |value| {
        let mut step = ScriptStep::new(16);
        step.value = value;
        step
    };
    let mut by_calc = movement(5);
    by_calc.set_calculation(0, Some(&calc("2")));
    let mut next_exit = movement(4);
    next_exit.flags |= flag::EXIT;
    let mut c = client(vec![
        movement(2), var("$last", "Get ( PageNumber )"),
        movement(4), var("$end", "Get ( LastError )"),
        movement(3), var("$previous", "Get ( PageNumber )"),
        movement(1), var("$first", "Get ( PageNumber )"),
        by_calc, var("$calculated", "Get ( PageNumber )"),
        ScriptStep::new(71), // Loop
        next_exit,
        ScriptStep::new(73), // End Loop
        exit("$last & \"|\" & $end & \"|\" & $previous & \"|\" & $first & \"|\" & $calculated & \"|\" & Get ( PageNumber ) & \"|\" & Get ( RecordNumber ) & \"|\" & Get ( RecordID )"),
    ]);
    c.server
        .lock()
        .unwrap()
        .scripts
        .get_mut("Root.fmp12")
        .unwrap()[0]
        .update_partner_indexes()
        .unwrap();
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        mode: om_data::runtime::Mode::Preview,
        placement: Some(crate::desktop::placement(
            &egui::Context::default(),
            None,
            true,
            0.0,
            false,
            0.0,
        )),
        ..Default::default()
    });
    assert_eq!(c.result(), "3|101|2|1|2|3|3|9");
    assert_eq!(c.session.s.file.window.state.index, 2);
    assert_eq!(c.session.s.file.window.state.record, Some(9));
    assert_eq!(c.session.s.file.window.state.preview_page, 2);
    assert!(!c
        .server
        .lock()
        .unwrap()
        .events
        .iter()
        .any(|e| e.ends_with(":write")));
}

#[test]
fn summary_page_context_matches_preview_and_pdf() {
    for merge_field in ["TotalPage", "SqlTotalPage"] {
        assert_summary_page_context(merge_field);
    }
}

fn assert_summary_page_context(merge_field: &str) {
    use om_data::host::{CalcNeed, FieldClass, SummaryDef, SummaryKind, WindowMode};
    let temporary = tempfile::tempdir().unwrap();
    let output = std::env::var_os("OM_PREVIEW_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join(format!("summary-page-context-{merge_field}.pdf"));
    let mut c = client(vec![
        pdf(&crate::scripting::files::file_line(&path), false),
        exit("Get ( LastError ) & \"|\" & Get ( PageNumber ) & \"|\" & Get ( WindowMode )"),
    ]);
    let mut schema = c.provider.calc_schema().unwrap().unwrap().as_ref().clone();
    schema.add_field(
        5,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::Total, 6)),
            ..FieldDef::data(7, "TotalPage", ValueType::Number)
        },
    );
    let cx = om_data::formula::Context::new(schema.names());
    let compiled = om_data::formula::compile(
        r#"GetValue ( ExecuteSQL ( "SELECT \"TotalPage\" FROM \"Rows\"" ; "" ; "" ) ; 1 )"#,
        &cx,
    )
    .unwrap();
    schema.add_field(
        5,
        FieldDef::calculation(
            8,
            "SqlTotalPage",
            ValueType::Number,
            om_data::formula::Formula {
                context: None,
                formatted: Some(compiled.tokens),
                polish: compiled.expr,
            },
            false,
        ),
    );
    let schema = Arc::new(schema);
    let mut document = document();
    document
        .layout
        .objects
        .objects
        .iter_mut()
        .find(|o| o.id == 10)
        .unwrap()
        .text_runs = vec![
        om_layout::TextRun::Literal("TotalPage ".into()),
        om_layout::TextRun::MergeField(merge_field.into()),
    ];
    c.session.s.file.schema = Some(Some(schema.clone()));
    c.session
        .s
        .file
        .rendered
        .insert(1, Some(Arc::new(document.clone())));
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        mode: om_data::runtime::Mode::Preview,
        preview_page: 2,
        placement: Some(crate::desktop::placement(
            &egui::Context::default(),
            None,
            true,
            0.0,
            false,
            0.0,
        )),
        ..Default::default()
    });
    assert_eq!(
        c.result(),
        "0|3|2",
        "report context must not leak into the script"
    );

    let mut session = crate::session::Session::new(
        crate::session::Target {
            host: "synthetic.invalid".into(),
            file: "Root.fmp12".into(),
            local: None,
        },
        25,
    );
    let key = session.select_layout(1);
    session
        .accept_render_layout(1, Ok(Some(document.clone())))
        .unwrap();
    session
        .accept_page(key, Ok(c.provider.records(1, 0, 25).unwrap()))
        .unwrap();
    session.go_to_preview(2);
    let mut calcs = crate::calc::Calculations::default();
    calcs.set_schema(Some(schema));
    let mut needs = vec![
        CalcNeed::Records { table: 5 },
        CalcNeed::FoundSet {
            occurrence: 5,
            table: 5,
        },
    ];
    needs.extend((1..=9).map(|record| CalcNeed::Record { table: 5, record }));
    *calcs.cache.get_mut() = c.provider.calc_data(&needs).unwrap();
    let mut info = crate::calc::session_info(
        &session,
        calcs.schema().map(AsRef::as_ref),
        "synthetic",
        WindowMode::Preview,
    );
    info.window = Some(Default::default());
    let state = crate::document::DocumentState::default();
    let globals = Default::default();
    let eval = crate::calc::Evaluator::new(&calcs, &session, &state, info, &globals).unwrap();
    let object = document.layout.objects.get(10).unwrap();
    let preview = eval.report_text(object, 5, 2, None).unwrap();
    assert_eq!(preview, "TotalPage 27");
    assert_eq!(eval.report_text(object, 5, 0, None).unwrap(), "TotalPage 9");
    assert_eq!(
        eval.report_text(object, 5, 1, None).unwrap(),
        "TotalPage 18"
    );
    // The portable PDF embeds this font and emits hex glyph strings. Check
    // the actual saved content without an external PDF tool dependency.
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let saved = std::fs::read(&path).unwrap();
    let saved = String::from_utf8_lossy(&saved);
    for expected in ["TotalPage 9", "TotalPage 18", &preview] {
        let glyphs: String = expected
            .chars()
            .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
            .collect();
        assert!(
            saved.contains(&format!("<{glyphs}> Tj")),
            "the saved PDF must contain its physical page value: {expected}"
        );
    }

    // Exporting from Browse without a desktop placement has the same report
    // page context while ordinary script selectors retain their existing
    // answers (PageNumber is unavailable without desktop geometry).
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert_eq!(c.result(), "0||0");
    let saved = std::fs::read(&path).unwrap();
    let saved = String::from_utf8_lossy(&saved);
    let glyphs: String = preview
        .chars()
        .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
        .collect();
    assert!(saved.contains(&format!("<{glyphs}> Tj")));
}

/// Container fields print as Browse shows them (#264): a picture placed by
/// its object's graphic format inside the field's content box, a file as
/// its icon above its name, and data that does not decode as an empty
/// field. Preview prepares the same page.
#[test]
fn container_fields_print_pictures_by_graphic_format_and_files_by_name() {
    use crate::report::{ContentContext, Pictures, ReportContent, ReportError};
    use om_data::calc::eval::StoredContainer;
    use om_data::calc::{Container, StreamType, Text};
    struct Containers;
    impl ReportContent for Containers {
        fn text(
            &mut self,
            object: &LayoutObject,
            _: ContentContext,
        ) -> Result<String, ReportError> {
            Ok(object.text.clone().unwrap_or_default())
        }
        fn container(
            &mut self,
            object: &LayoutObject,
            _: ContentContext,
        ) -> Result<Option<Arc<StoredContainer>>, ReportError> {
            let (name, kind, data) = match object.id {
                // 40 x 20: its left half red, its right half blue.
                40 => {
                    let mut png = std::io::Cursor::new(Vec::new());
                    image::RgbaImage::from_fn(40, 20, |x, _| {
                        image::Rgba(if x < 20 {
                            [255, 0, 0, 255]
                        } else {
                            [0, 0, 255, 255]
                        })
                    })
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                    ("two.png", *b"PNGf", png.into_inner())
                }
                41 => ("notes.txt", *b"FILE", b"notes".to_vec()),
                42 => ("broken.png", *b"PNGf", b"\x89PNG\r\n\x1a\n broken".to_vec()),
                _ => return Ok(None),
            };
            Ok(Some(Arc::new(StoredContainer {
                data: Container {
                    text: Text::plain(name),
                    streams: vec![(StreamType(kind), data)],
                    ..Container::default()
                },
                intact: true,
                external: false,
            })))
        }
    }
    let mut document = document();
    let layout = &mut document.layout;
    layout.objects.objects.retain(|o| !(3..=8).contains(&o.id));
    let mut photo = field(40, 40, rect(12.0, 90.0, 100.0, 100.0));
    // Reduce or enlarge, keeping proportions, centered.
    photo.graphic_format = Some(om_layout::GraphicFormat(7));
    layout.objects.objects.extend([
        photo,
        field(41, 41, rect(130.0, 90.0, 120.0, 80.0)),
        field(42, 42, rect(270.0, 90.0, 60.0, 60.0)),
    ]);
    layout.objects.root = layout.objects.objects.iter().map(|o| o.id).collect();
    let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap();
    let report = crate::report::render_pdf_with(layout, 1, paper, &[], &mut Containers).unwrap();
    let page = crate::report::prepare_page(
        layout,
        &report.plan,
        0,
        &Pictures::default(),
        &mut Containers,
    )
    .unwrap();
    let objects: Vec<_> = page.parts.iter().flat_map(|part| &part.objects).collect();
    let [clip, image] = objects.iter().find_map(|o| o.placed.clone()).unwrap();
    // As wide as the content box, half as high, centered in it.
    assert!(
        (image.width() - clip.width()).abs() < 0.01,
        "{clip:?} {image:?}"
    );
    assert!((image.height() - clip.width() / 2.0).abs() < 0.01);
    assert!(((image.top - clip.top) - (clip.bottom - image.bottom)).abs() < 0.01);
    let file = objects.iter().find(|o| o.file_icon.is_some()).unwrap();
    let name: String = file
        .text
        .iter()
        .flat_map(|l| l.glyphs.iter().map(|g| g.2))
        .collect();
    assert_eq!(name, "notes.txt");
    assert_eq!(objects.iter().filter(|o| o.picture.is_some()).count(), 1);
    let raster = raster(&report.bytes, 0);
    let at = |x: f64, y: f64| raster.get_pixel(x as u32, y as u32).0;
    let middle = (image.top + image.bottom) / 2.0;
    assert_eq!(
        at(image.left + image.width() / 4.0, middle),
        [255, 0, 0, 255]
    );
    assert_eq!(
        at(image.right - image.width() / 4.0, middle),
        [0, 0, 255, 255]
    );
    // The rest of the frame stays the field's own.
    assert_eq!(at(image.left + 5.0, clip.top + 2.0), [255, 255, 255, 255]);
}

/// Character runs that differ only in color print each in its own, as
/// Browse colors them: a line keeps its baseline and wrapping and changes
/// color where its runs do, each part from where the previous one ends
/// (#106). Stored styles compare every byte but the color's, so an explicit
/// size 0 differs from an unset size. Runs that are not their text's
/// partition, runs in several sizes, and a value other than the text the
/// runs are stored for refuse with 3.
#[test]
fn character_runs_print_in_their_own_colors() {
    use om_layout::{CharacterStyle, StyledRun};
    let font =
        ttf_parser::Face::parse(include_bytes!("../../assets/fonts/Inter-Regular.ttf"), 0).unwrap();
    let advance = |text: &str| {
        text.chars()
            .map(|ch| {
                f64::from(
                    font.glyph_hor_advance(font.glyph_index(ch).unwrap())
                        .unwrap(),
                )
            })
            .sum::<f64>()
            * 12.0
            / f64::from(font.units_per_em())
    };
    // A stored color-only style (mask 0x0004, layout-model.md 8.1), and the
    // same with an explicit size of 0 (mask 0x0006).
    let stored = |mask: u16, rgb: [u8; 3]| {
        let mut raw = mask.to_be_bytes().to_vec();
        raw.extend([0; 6]);
        raw.extend([rgb[0], rgb[1], rgb[2], 255]);
        CharacterStyle {
            color: Some(Color {
                red: f32::from(rgb[0]) / 255.0,
                green: f32::from(rgb[1]) / 255.0,
                blue: f32::from(rgb[2]) / 255.0,
                alpha: 1.0,
            }),
            raw,
            ..Default::default()
        }
    };
    // "Grüße", a CRLF, then a line that wraps after "overdue ": black up to
    // "overdue", red from there on, across the wrap.
    let text = "Grüße\r\nTotal overdue item";
    let red_from = text.find("overdue").unwrap();
    let width = advance("Total overdue ") + 3.0;
    let mut colored = label(1, text, rect(12.0, 10.0, width, 60.0), 12.0, false);
    colored.rich_text.as_mut().unwrap().character_runs = vec![
        StyledRun {
            range: 0..red_from,
            style: stored(4, [0, 0, 0]),
        },
        StyledRun {
            range: red_from..text.len(),
            style: stored(4, [255, 0, 0]),
        },
    ];
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![Part {
            part_type: 4,
            height: 100.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1],
            objects: vec![colored],
            ..Default::default()
        },
        ..Default::default()
    };
    let render = |layout: &Layout, value: &'static str| {
        let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap();
        crate::report::render_pdf(layout, 1, paper, &[], move |_, _| Ok(value.to_owned()))
            .map(|report| String::from_utf8_lossy(&report.bytes).into_owned())
            .map_err(|e| (e.code, e.message))
    };
    let pdf = render(&layout, text).unwrap();
    let origin = |word: &str| {
        let found = text_origins(&pdf, 1, word);
        assert_eq!(found.len(), 1, "{word}: {found:?}");
        found[0]
    };
    let (first, total, overdue, item) = (
        origin("Grüße"),
        origin("Total "),
        origin("overdue "),
        origin("item"),
    );
    // "overdue " continues "Total "'s line, at its advance, on its baseline;
    // "item" wraps to the next line, at the line's start.
    assert!(
        (overdue.1 - total.1 - advance("Total ")).abs() < 0.01,
        "{total:?} {overdue:?}"
    );
    assert_eq!(overdue.2, total.2);
    assert_eq!(item.1, total.1);
    assert!(
        first.2 > total.2 && total.2 > item.2,
        "{first:?} {total:?} {item:?}"
    );
    // Each part is filled in its color, set just before it.
    let fill_before = |word: &str| {
        let glyphs: String = word
            .chars()
            .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
            .collect();
        let at = pdf.find(&format!("<{glyphs}> Tj")).unwrap();
        let fill = pdf[..at]
            .lines()
            .rev()
            .find(|l| l.ends_with(" rg"))
            .unwrap();
        fill.to_owned()
    };
    for (word, fill) in [
        ("Grüße", "0 0 0 rg"),
        ("Total ", "0 0 0 rg"),
        ("overdue ", "1 0 0 rg"),
        ("item", "1 0 0 rg"),
    ] {
        assert_eq!(fill_before(word), fill, "{word}");
    }

    fn runs(layout: &mut Layout) -> &mut Vec<om_layout::StyledRun<CharacterStyle>> {
        &mut layout.objects.objects[0]
            .rich_text
            .as_mut()
            .unwrap()
            .character_runs
    }
    // Merged or calculated text is not the text the runs are stored for.
    let several = "Report object 1's text is in several colors and is merged, calculated or \
                   transformed; only its text as written can print in them";
    assert_eq!(
        render(&layout, "Grüße\r\nTotal 3 overdue").unwrap_err(),
        (3, several.to_owned())
    );
    // An explicit size of 0 is not an unset size, nor are two sizes one.
    let mixed = (3, "Mixed character styles are unsupported".to_owned());
    let mut sized = layout.clone();
    runs(&mut sized)[1].style = stored(6, [255, 0, 0]);
    assert_eq!(render(&sized, text).unwrap_err(), mixed);
    let mut sized = layout.clone();
    runs(&mut sized)[1].style.raw[7] = 18;
    runs(&mut sized)[1].style.font_size = Some(18.0);
    assert_eq!(render(&sized, text).unwrap_err(), mixed);
    // A size given only decoded, with the stored bytes unchanged, too.
    let mut sized = layout.clone();
    runs(&mut sized)[1].style.font_size = Some(18.0);
    assert_eq!(render(&sized, text).unwrap_err(), mixed);
    // One style stored in a length the decoder does not read still prints,
    // as before, in one color, and two copies of it as one.
    let mut odd = layout.clone();
    let style = CharacterStyle {
        raw: vec![0; 14],
        ..Default::default()
    };
    for run in runs(&mut odd) {
        run.style = style.clone();
    }
    assert!(render(&odd, text).is_ok());
    // A field's layout runs in two colors refuse, its filename included.
    let mut field = layout.clone();
    field.objects.objects[0].object_type = 1;
    assert_eq!(render(&field, "notes.txt").unwrap_err(), mixed);
    // Runs that leave a gap, or overlap, are not the text's partition.
    let mut gap = layout.clone();
    runs(&mut gap)[1].range.start += 1;
    assert_eq!(
        render(&gap, text).unwrap_err(),
        (
            3,
            "Report object 1's character runs do not cover its text in order".to_owned()
        )
    );
}

/// `{{PageNumber}}` prints each page's number, in a static label and in a
/// merge text's literal (any case), as FileMaker Pro 26.0.3's Save Records
/// as PDF wrote it on page *n* (calc-functions.md, O4), without touching
/// the merged field's value. An unclosed `{{` prints as it is, and a
/// symbol is read within one literal run, never across a merged field.
/// `{{PageCount}}` and symbols Browse does not show refuse, as do merge
/// variables and other symbols in an object whose data format presents
/// values.
#[test]
fn page_number_symbols_print_each_pages_number() {
    use om_layout::TextRun::{Literal, MergeField};
    let mut merged = label(3, "No.", rect(320.0, 610.0, 200.0, 24.0), 11.0, false);
    merged.text_runs = vec![
        Literal("No. {{pagenumber}} ".into()),
        MergeField("Name".into()),
    ];
    let mut noted = label(
        5,
        "Note {{ <<Name>> / Page {{PageNumber}}",
        rect(12.0, 60.0, 400.0, 20.0),
        12.0,
        false,
    );
    noted.text_runs = vec![
        Literal("Note {{ ".into()),
        MergeField("Name".into()),
        Literal(" / Page {{PageNumber}}".into()),
    ];
    // A merged field whose name holds the symbol keeps its name.
    let mut named = label(
        6,
        "<<Name {{PageNumber}}>> / Page {{PageNumber}}",
        rect(12.0, 85.0, 400.0, 20.0),
        12.0,
        false,
    );
    named.text_runs = vec![
        MergeField("Name {{PageNumber}}".into()),
        Literal(" / Page {{PageNumber}}".into()),
    ];
    // Literal text that looks like a placeholder is numbered as text.
    let mut angled = label(
        7,
        "<<Page {{PageNumber}}>> / <<Name>>",
        rect(12.0, 110.0, 400.0, 20.0),
        12.0,
        false,
    );
    angled.text_runs = vec![
        Literal("<<Page {{PageNumber}}>> / ".into()),
        MergeField("Name".into()),
    ];
    // A label the runs do not spell is numbered by whole symbols outside
    // placeholders, by number too.
    let mut stored = label(
        8,
        "Page {{1015}} of <<Name>> (draft)",
        rect(12.0, 135.0, 400.0, 20.0),
        12.0,
        false,
    );
    stored.text_runs = vec![
        Literal("Page {{1015}} of ".into()),
        MergeField("Name".into()),
    ];
    let layout = Layout {
        def: Some(LayoutDef {
            base_width: 540.0,
            ..Default::default()
        }),
        parts: vec![
            Part {
                part_type: 4,
                height: 600.0,
                ..Default::default()
            },
            Part {
                part_type: 7,
                top: 600.0,
                height: 40.0,
                ..Default::default()
            },
        ],
        objects: ObjectStore {
            root: vec![1, 2, 3, 4, 5, 6, 7, 8],
            objects: vec![
                label(1, "Body", rect(12.0, 10.0, 200.0, 20.0), 12.0, false),
                label(
                    2,
                    "Page {{PageNumber}}",
                    rect(12.0, 610.0, 200.0, 24.0),
                    11.0,
                    false,
                ),
                merged,
                label(4, "Draft {{", rect(12.0, 35.0, 200.0, 20.0), 12.0, false),
                noted,
                named,
                angled,
                stored,
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let render = |layout: &Layout| {
        let paper = om_layout::pagination::PageSize::for_layout(layout).unwrap();
        crate::report::render_pdf(layout, 2, paper, &[], |o, _| {
            Ok(match o.text_runs.as_slice() {
                [] => o.text.clone().unwrap_or_default(),
                runs => runs
                    .iter()
                    .map(|run| match run {
                        Literal(text) => text.clone(),
                        // A value that holds the symbol stays as it is.
                        _ => "{{PageNumber}}".to_owned(),
                    })
                    .collect(),
            })
        })
        .map_err(|e| e.message)
    };
    let report = render(&layout).unwrap();
    assert_eq!(report.plan.pages.len(), 2);
    let pdf = String::from_utf8_lossy(&report.bytes).into_owned();
    for (page, number) in [(0, "1"), (1, "2")] {
        let labels = text_origins(&pdf, 2, &format!("Page {number}"));
        assert_eq!(labels.iter().map(|o| o.0).collect::<Vec<_>>(), [page]);
        let merge = format!("No. {number} {{{{PageNumber}}}}");
        let merged = text_origins(&pdf, 2, &merge);
        assert_eq!(
            merged.iter().map(|o| o.0).collect::<Vec<_>>(),
            [page],
            "{merge}"
        );
        let note = format!("Note {{{{ {{{{PageNumber}}}} / Page {number}");
        let notes = text_origins(&pdf, 2, &note);
        assert_eq!(
            notes.iter().map(|o| o.0).collect::<Vec<_>>(),
            [page],
            "{note}"
        );
    }
    let drafts = text_origins(&pdf, 2, "Draft {{");
    assert_eq!(drafts.iter().map(|o| o.0).collect::<Vec<_>>(), [0, 1]);
    // Content that reads a merge text's label gets it numbered too, only
    // where a whole symbol is.
    let paper = om_layout::pagination::PageSize::for_layout(&layout).unwrap();
    let labels = crate::report::render_pdf(&layout, 2, paper, &[], |o, _| {
        Ok(o.text.clone().unwrap_or_default())
    })
    .unwrap();
    let labels = String::from_utf8_lossy(&labels.bytes).into_owned();
    for (page, number) in [(0, "1"), (1, "2")] {
        for label in [
            format!("Note {{{{ <<Name>> / Page {number}"),
            format!("<<Name {{{{PageNumber}}}}>> / Page {number}"),
            format!("<<Page {number}>> / <<Name>>"),
            format!("Page {number} of <<Name>> (draft)"),
        ] {
            let found = text_origins(&labels, 2, &label);
            assert_eq!(
                found.iter().map(|o| o.0).collect::<Vec<_>>(),
                [page],
                "{label}"
            );
        }
    }
    for symbol in ["{{PageCount}}", "{{1125}}", "{{AccountName}}"] {
        let mut counted = layout.clone();
        counted.objects.objects[1].text = Some(format!("Page {{{{PageNumber}}}} of {symbol}"));
        assert_eq!(
            render(&counted).unwrap_err(),
            "Layout merge symbols other than {{PageNumber}}, {{RecordNumber}}, \
             {{CurrentDate}}, {{CurrentTime}} and {{UserName}} are unsupported",
            "{symbol}"
        );
    }
    // A variable name with a space, which Set Variable accepts and Browse
    // shows as written.
    let mut spaced = layout.clone();
    spaced.objects.objects[1].text = Some("Printed by <<$$printed by>>".into());
    assert_eq!(
        render(&spaced).unwrap_err(),
        "Report object 2 holds a <<$...>> OpenMaker does not read as a merge variable; \
         how FileMaker reads it is unmeasured"
    );
    // A data format that presents dates (format 7, numeric with the full
    // year): FileMaker's for a variable or symbol is unmeasured.
    let mut formatted = layout.clone();
    formatted.objects.objects[1].data_format = Some(DataFormat {
        date: om_layout::DateFormat {
            flags: 7 << 24,
            ..Default::default()
        },
        ..DataFormat::new_field()
    });
    for (text, refused) in [
        ("Page {{PageNumber}}", false),
        ("Printed {{CurrentDate}}", true),
        ("Total <<$$total>>", true),
    ] {
        formatted.objects.objects[1].text = Some(text.into());
        assert_eq!(
            render(&formatted).err(),
            refused.then(|| {
                "Report object 2 formats numbers, dates or times and holds a merge variable \
                 or layout symbol; whether FileMaker formats them is unmeasured"
                    .to_owned()
            }),
            "{text}"
        );
    }
}

/// [`document`] with layout symbols and a merge variable: the header shows
/// the user name and the date, each body the record's number by name and
/// by its selector's number (`{{1020}}`), then `$$suffix`'s second
/// repetition.
pub(super) fn symbols_document() -> RenderLayout {
    let mut document = document();
    let objects = &mut document.layout.objects;
    objects.objects[1] = label(
        2,
        "By {{UserName}} on {{CurrentDate}}",
        rect(12.0, 44.0, 500.0, 22.0),
        10.0,
        false,
    );
    objects.objects.push(label(
        11,
        "No. {{RecordNumber}} / {{1020}}<<$$suffix[2]>>",
        rect(360.0, 86.0, 170.0, 24.0),
        11.0,
        false,
    ));
    objects.root.push(11);
    document
}

/// Layout symbols print as Browse shows them
/// (`om_data::host::layout::merge_text`), on each record: the record
/// number its found-set position, the header's user name and date the
/// session's. In a portal row the record number is the row's, as in Browse
/// and Preview. A merge variable refuses (#596): this host does not read
/// the running script's variables.
#[test]
fn layout_symbols_print_as_browse_shows_them() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("symbols.pdf");
    let steps = |path: &std::path::Path| {
        vec![
            var("$$suffix", "\" of 9\""),
            var("$before", "Get ( CurrentDate )"),
            pdf(&crate::scripting::files::file_line(path), false),
            exit("Get ( LastError ) & \"|\" & Get ( UserName ) & \"|\" & $before & \"|\" & Get ( CurrentDate )"),
        ]
    };
    let mut c = client(steps(&path));
    let mut document = symbols_document();
    let objects = &mut document.layout.objects.objects;
    let body = objects.iter_mut().find(|o| o.id == 11).unwrap();
    *body = label(
        11,
        "No. {{RecordNumber}} / {{1020}}",
        rect(360.0, 86.0, 170.0, 24.0),
        11.0,
        false,
    );
    c.server.lock().unwrap().refresh_render = Some(document);
    c.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    let result = c.result();
    let [error, user, before, after] = result.split('|').collect::<Vec<_>>()[..] else {
        panic!("{result}");
    };
    assert_eq!(error, "0", "{:?}", c.session.s.failure);
    let pdf = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    // Each record's once, in found-set order over the three pages.
    let pages: Vec<usize> = (1..=9)
        .map(|n| {
            let numbered = text_origins(&pdf, 3, &format!("No. {n} / {n}"));
            assert_eq!(numbered.len(), 1, "{n}");
            numbered[0].0
        })
        .collect();
    assert!(
        pages.is_sorted() && pages[0] == 0 && pages[8] == 2,
        "{pages:?}"
    );
    // The header on each page; the date is the day the script ran on.
    let mut dates = vec![before];
    if after != before {
        dates.push(after);
    }
    let mut headers: Vec<usize> = (dates.iter())
        .flat_map(|date| text_origins(&pdf, 3, &format!("By {user} on {date}")))
        .map(|o| o.0)
        .collect();
    headers.sort();
    assert_eq!(headers, [0, 1, 2], "{dates:?}");

    // `$$suffix[2]` would print empty whatever the script set: refused, and
    // nothing is written.
    let refused = temporary.path().join("variables.pdf");
    let mut v = client(steps(&refused));
    v.server.lock().unwrap().refresh_render = Some(symbols_document());
    v.run_in(WindowState {
        layout: Some(1),
        record: Some(9),
        index: 2,
        ..Default::default()
    });
    assert!(v.result().starts_with("3|"), "{}", v.result());
    assert!(!refused.exists());

    // The report host answers a portal row's number there.
    let mut store = Store;
    let mut io = Io {
        provider: &mut c.provider,
        files: &mut c.files,
        store: &mut store,
        jobs: &c.jobs,
        responses: &c.responses,
    };
    let mut host = WorkerHost::new(&mut io, &mut c.session.s);
    host.report_record = Some((9, 2, 0));
    let row = crate::report::PortalRowContext {
        occurrence: 6,
        record: 101,
        number: 2,
    };
    for (row, shown) in [(None, "3"), (Some(row), "2")] {
        let context = crate::report::in_row(om_data::calc::eval::Context::record(5, 9), row);
        assert_eq!(
            om_data::host::layout::merge_text(&mut host, "{{RecordNumber}}", &context),
            shown
        );
    }
}
