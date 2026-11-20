//! Web viewers in a second window and in List rows, with WebKitGTK (the
//! `web-viewer` feature) under a real X server: eframe's own event loop and
//! windows, the browser's own processes.
//!
//! - Two records' web viewer pages are solid red and solid blue (`data:`
//!   pages calculated from each record's Color field). The app's window
//!   shows the layout in Form view: one page, the current record's, red, a
//!   child window the object's size.
//! - OnLayoutEnter opens one New Window beside it and shows it as a List.
//!   That window is another egui viewport, whose X11 window eframe does not
//!   give out (`web_viewer/viewports.rs` finds it): it gets one page per
//!   row, each its own record's color.
//!
//! `xvfb-run -a -s "-screen 0 1920x1200x24" cargo test -p om-gui --features
//! web-viewer --test web_viewer_windows_native` (the windows lie side by
//! side); skipped without a display. With
//! `OM_WEB_VIEWER_SHOTS` set to a directory, each window is saved there as
//! PNG. Linux only: on macOS the other windows' views are not placed yet
//! ([#304](https://github.com/OpenMaker-Dev/openmaker/issues/304)).
#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, unused_imports, reason = "the test runs on Linux only")
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use om_data::{
    Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, HostedFile,
    LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep,
};
use om_gui::OpenMakerApp;
use om_layout::{Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};
#[cfg(target_os = "linux")]
use x11rb::connection::Connection as _;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{self, ConnectionExt as _};
#[cfg(target_os = "linux")]
use x11rb::rust_connection::RustConnection;

const OCCURRENCE: u64 = 0xd0_0002;
const TABLE: u64 = 0x8002;
/// `LayoutDef.tableAliasKey` of the Rows occurrence.
const ALIAS: u64 = 0x10_4082;
const LAYOUT: u32 = 7;
/// The records' ids and their pages' colors.
const ROWS: [(i64, &str, [u8; 3]); 2] = [(1, "red", [255, 0, 0]), (2, "blue", [0, 0, 255])];
/// The web viewer's size in points (the display's scale is 1).
const SIZE: (u16, u16) = (200, 60);

struct Provider;

fn names() -> om_formula::NameTable {
    let mut names = om_formula::NameTable::default();
    names.add_occurrence(OCCURRENCE, "Rows", TABLE);
    names.add_field(TABLE, 1, "Color");
    names
}

fn compiled(text: &str) -> om_formula::Compiled {
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"))
}

fn calc(text: &str) -> Calculation {
    let compiled = compiled(text);
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn step(id: u16, calcs: &[(u16, &str)]) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    for (index, text) in calcs {
        step.set_calculation(*index, Some(&calc(text)));
    }
    step
}

/// OnLayoutEnter: the first window to enter the layout opens one more
/// beside itself, as a List.
fn scripts() -> ScriptCatalog {
    let mut mark = step(141, &[(1, "1")]);
    mark.set_text(0, Some("$$second"));
    // Height, width, top and left.
    let new_window = step(122, &[(1, "600"), (2, "900"), (3, "0"), (4, "920")]);
    let mut list = ScriptStep::new(30);
    list.value = 2;
    let steps = vec![
        step(68, &[(0, "IsEmpty ( $$second )")]),
        mark,
        new_window,
        list,
        ScriptStep::new(70),
    ];
    let mut script = Script {
        id: 1,
        name: "Open second window".into(),
        ..Default::default()
    };
    script.insert_steps(0, steps).expect("nests");
    ScriptCatalog {
        scripts: vec![script],
        writable: true,
        unavailable_reason: None,
    }
}

fn layout() -> Layout {
    let page = "\"data:text/html,<!doctype html><body style='margin:0;background:\" & Rows::Color \
                & \"'></body>\"";
    let compiled = compiled(page);
    let viewer = LayoutObject {
        id: 2,
        object_type: 13,
        name: Some("wv".into()),
        bounds: Some(Rect {
            top: 10.0,
            left: 20.0,
            bottom: 10.0 + f64::from(SIZE.1),
            right: 20.0 + f64::from(SIZE.0),
            ..Default::default()
        }),
        external: Some(om_layout::ExternalObject {
            type_code: om_layout::ExternalObject::WEB_VIEWER,
            // Interaction allowed, the URL encoded (FileMaker's defaults).
            flags: 0x8001,
            ..Default::default()
        }),
        calcs: om_layout::calcs::ObjectCalcs {
            web_address: Some(om_formula::Formula {
                context: None,
                formatted: Some(compiled.tokens),
                polish: compiled.expr,
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut layout = Layout {
        id: u64::from(LAYOUT),
        name: "Web".into(),
        def: Some(LayoutDef {
            base_width: 400.0,
            table_alias_key: ALIAS,
            ..Default::default()
        }),
        parts: vec![Part {
            id: 1,
            part_type: 4,
            top: 0.0,
            height: 80.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![2],
            objects: vec![viewer],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(layout.objects.get(2).and_then(|o| o.web_viewer()).is_some());
    layout.triggers.push(om_layout::ScriptTrigger {
        event: 101,
        script: Some(1),
        modes: 1,
        file_reference: None,
        external: false,
        option: 0,
        extra: vec![],
        parameter: None,
    });
    layout
}

fn schema() -> om_data::host::CalcSchema {
    use om_data::host::{CalcSchema, FieldDef};
    let mut schema = CalcSchema::new("Web", om_data::calc::Locale::en_us());
    schema.add_table(TABLE, "Rows");
    schema.add_occurrence(OCCURRENCE, "Rows", TABLE);
    schema.add_field(
        TABLE,
        FieldDef::data(1, "Color", om_data::calc::ValueType::Text),
    );
    schema
}

impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: LAYOUT,
            name: "Web".into(),
        }])
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(scripts())
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<om_data::host::CalcSchema>>, DataError> {
        Ok(Some(Arc::new(schema())))
    }
    fn calc_data(
        &mut self,
        needs: &[om_data::host::CalcNeed],
    ) -> Result<om_data::host::CalcData, DataError> {
        use om_data::host::{MemorySource, RecordValues, Recorder};
        let schema = schema();
        let cx = om_data::calc::Ctx::new(
            &schema.locale,
            om_data::calc::Date::from_ymd(2026, 11, 19).unwrap(),
        );
        let mut source = MemorySource::new();
        for (id, color, _) in ROWS {
            let mut values = RecordValues::default();
            values.set(1, 1, om_data::calc::Value::text(color));
            source.put_record(TABLE, id as u64, values);
        }
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(e.to_string()))?;
        }
        Ok(recorder.take())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: vec![Field {
                name: "Color".into(),
                kind: FieldKind::Text,
            }],
            records: ROWS
                .iter()
                .map(|(id, color, _)| Record {
                    id: *id,
                    values: vec![Some((*color).into())],
                })
                .skip(offset as usize)
                .take(limit as usize)
                .collect(),
            total: ROWS.len() as u64,
        })
    }
    fn update_record(&mut self, _: u32, _: i64, _: FieldChanges) -> Result<Record, DataError> {
        Err(DataError::new("read-only test file"))
    }
    fn render_layout(&mut self, _: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(RenderLayout {
            layout: layout(),
            field_bindings: BTreeMap::new(),
        }))
    }
}

fn fail(message: String) -> ! {
    eprintln!("FAIL: {message}");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
struct Driver {
    app: OpenMakerApp,
    x11: RustConnection,
    root: u32,
    deadline: Instant,
}

#[cfg(target_os = "linux")]
impl Driver {
    /// The mapped child windows of `parent` the web viewer's size, top to
    /// bottom: one per List row. `None` when they do not share one column.
    fn pages(&self, parent: u32) -> Option<Vec<u32>> {
        let tree = self.x11.query_tree(parent).ok()?.reply().ok()?;
        let mut pages: Vec<(i16, i16, u32)> = tree
            .children
            .into_iter()
            .filter_map(|child| {
                let geometry = self.x11.get_geometry(child).ok()?.reply().ok()?;
                let mapped = self
                    .x11
                    .get_window_attributes(child)
                    .ok()?
                    .reply()
                    .ok()?
                    .map_state
                    == xproto::MapState::VIEWABLE;
                (mapped && (geometry.width, geometry.height) == SIZE)
                    .then_some((geometry.y, geometry.x, child))
            })
            .collect();
        pages.sort_unstable();
        pages
            .windows(2)
            .all(|pair| pair[0].1 == pair[1].1 && pair[0].0 < pair[1].0)
            .then(|| pages.into_iter().map(|(_, _, window)| window).collect())
    }

    /// The color at the middle of window `window`, once its page has
    /// painted.
    fn color(&self, window: u32) -> Option<[u8; 3]> {
        let image = self
            .x11
            .get_image(
                xproto::ImageFormat::Z_PIXMAP,
                window,
                (SIZE.0 / 2) as i16,
                (SIZE.1 / 2) as i16,
                1,
                1,
                !0,
            )
            .ok()?
            .reply()
            .ok()?;
        // 32 bits per pixel: blue, green, red, unused.
        let [blue, green, red, _] = image.data.get(..4)?.try_into().ok()?;
        Some([red, green, blue])
    }

    /// The colors of `parent`'s pages, top row first, once each shows red
    /// or blue.
    fn page_colors(&self, parent: u32) -> Option<Vec<[u8; 3]>> {
        let pages = self.pages(parent)?;
        let colors: Vec<[u8; 3]> = pages.iter().filter_map(|&p| self.color(p)).collect();
        let painted = colors.len() == pages.len()
            && colors
                .iter()
                .all(|c| ROWS.iter().any(|(_, _, color)| color == c));
        painted.then_some(colors)
    }

    /// The New Window's X11 window: the other top-level window eframe's own
    /// connection made (its id carries that connection's resource-id base,
    /// which the first window's shows), wherever a window manager put it.
    fn second_window(&self, first: u32) -> Option<u32> {
        let mask = self.x11.setup().resource_id_mask;
        let ours = |window: u32| window & !mask == first & !mask;
        let mut pending = vec![(self.root, 0)];
        let mut found = Vec::new();
        while let Some((window, depth)) = pending.pop() {
            let Ok(Ok(tree)) = self.x11.query_tree(window).map(|c| c.reply()) else {
                continue;
            };
            for child in tree.children {
                if ours(child) {
                    if child != first {
                        found.push(child);
                    }
                } else if depth < 2 {
                    pending.push((child, depth + 1));
                }
            }
        }
        // eframe makes no other top-level window here.
        match found[..] {
            [window] => Some(window),
            [] => None,
            _ => fail(format!("more than one other eframe window: {found:x?}")),
        }
    }

    /// Print `window`'s child windows: geometry, mapped, middle color.
    fn describe(&self, window: u32) {
        let Ok(Ok(tree)) = self.x11.query_tree(window).map(|c| c.reply()) else {
            return;
        };
        for child in tree.children {
            let geometry = self
                .x11
                .get_geometry(child)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|g| (g.x, g.y, g.width, g.height));
            let mapped = self
                .x11
                .get_window_attributes(child)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|a| a.map_state);
            println!(
                "child {child:#x} of {window:#x}: {geometry:?} {mapped:?} middle {:?}",
                self.color(child)
            );
        }
    }

    /// Save window `window` as PNG in `OM_WEB_VIEWER_SHOTS` when it is set.
    fn shot(&self, window: u32, name: &str) {
        let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
            return;
        };
        let Ok(Ok(geometry)) = self.x11.get_geometry(window).map(|c| c.reply()) else {
            return;
        };
        let (width, height) = (geometry.width, geometry.height);
        let Ok(Ok(image)) = self
            .x11
            .get_image(
                xproto::ImageFormat::Z_PIXMAP,
                window,
                0,
                0,
                width,
                height,
                !0,
            )
            .map(|c| c.reply())
        else {
            return;
        };
        let mut rgb = image::RgbImage::new(u32::from(width), u32::from(height));
        for (pixel, [blue, green, red, _]) in rgb.pixels_mut().zip(image.data.as_chunks::<4>().0) {
            *pixel = image::Rgb([*red, *green, *blue]);
        }
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        match rgb.save(&path) {
            Ok(()) => println!("screenshot {}", path.display()),
            Err(error) => println!("screenshot {name} failed: {error}"),
        }
    }

    fn step(&mut self, first: u32) {
        // The List's rows in record order, and Form view's current record.
        let rows = ROWS.map(|(_, _, color)| color).to_vec();
        let current = vec![ROWS[0].2];
        let first_colors = self.page_colors(first);
        let second = self.second_window(first);
        let second_colors = second.and_then(|w| self.page_colors(w));
        if Instant::now() > self.deadline {
            for (name, window) in [("timeout-first", Some(first)), ("timeout-second", second)] {
                let Some(window) = window else { continue };
                self.shot(window, name);
                self.describe(window);
            }
            fail(format!(
                "timed out: first window {first:#x} pages {:?} colors {first_colors:?}; \
                 second window {second:x?} colors {second_colors:?}; error {:?}",
                self.pages(first),
                self.app.error(),
            ));
        }
        let (Some(first_colors), Some(second), Some(second_colors)) =
            (first_colors, second, second_colors)
        else {
            return;
        };
        if first_colors != current || second_colors.len() < rows.len() {
            return;
        }
        println!(
            "first window {first:#x}: pages {:?}, colors {first_colors:?}",
            self.pages(first)
        );
        println!(
            "second window {second:#x}: pages {:?}, colors {second_colors:?}",
            self.pages(second)
        );
        if second_colors != rows {
            fail(format!(
                "the second window's rows show {second_colors:?}, not each its own record's \
                 page ({rows:?})"
            ));
        }
        self.shot(first, "1-first-window-form");
        self.shot(second, "2-second-window-list");
        println!("PASS");
        std::process::exit(0);
    }
}

#[cfg(target_os = "linux")]
impl eframe::App for Driver {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let first = match frame.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Xlib(h)) => h.window as u32,
            Ok(RawWindowHandle::Xcb(h)) => h.window.get(),
            other => fail(format!("not an X11 window: {other:?}")),
        };
        self.step(first);
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("skipped: Linux only");
}

#[cfg(target_os = "linux")]
fn main() {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no X display (run under xvfb-run)");
        return;
    }
    // As `om_gui::run` does, before any other thread starts.
    om_gui::prepare_web_viewers();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(180));
        eprintln!("FAIL: watchdog");
        std::process::exit(3);
    });
    let (x11, screen) = x11rb::connect(None).expect("X display");
    let root = x11.setup().roots[screen].root;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenMaker web viewer windows test")
            .with_inner_size([900.0, 600.0])
            .with_position([0.0, 0.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "OpenMaker web viewer windows test",
        options,
        Box::new(move |cc| {
            om_gui::install_theme(&cc.egui_ctx);
            let app = OpenMakerApp::new(Box::new(Provider)).open_on_launch(Connection {
                host: "web-viewer.test.invalid".into(),
                port: 5003,
                file: "Rows.fmp12".into(),
                account: "Admin".into(),
                password: String::new(),
            });
            Ok(Box::new(Driver {
                app,
                x11,
                root,
                deadline: Instant::now() + Duration::from_secs(120),
            }))
        }),
    );
    fail(format!("the event loop ended first: {result:?}"));
}
