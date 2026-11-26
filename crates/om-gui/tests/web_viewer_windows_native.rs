//! Web viewers in a second window and in List rows (the `web-viewer`
//! feature): eframe's own event loop and windows, the browser's own
//! processes; WebKitGTK under a real X server, or WKWebView on macOS.
//!
//! - Two records' web viewer pages are solid red and solid blue (`data:`
//!   pages calculated from each record's Color field). The app's window
//!   shows the layout in Form view: one page, the current record's, red, a
//!   child window the object's size.
//! - OnLayoutEnter opens one New Window beside it and shows it as a List.
//!   That window is another egui viewport, whose native window eframe does
//!   not give out (`web_viewer/viewports.rs` finds it): it gets one page per
//!   row, each its own record's color.
//! - First, in a process of its own (`OM_WEB_VIEWER_WINDOWS_EMBEDDED`), the
//!   same with egui's embedded viewports, as on a backend without native
//!   windows: the New Window is an egui window inside the app's window, and
//!   its rows' pages are views of the app's window where that egui window
//!   shows them. It lies over the app's own page, which hides.
//!
//! `xvfb-run -a -s "-screen 0 1920x1200x24" cargo test -p om-gui --features
//! web-viewer --test web_viewer_windows_native` on Linux (the windows lie
//! side by side), skipped without a display; the same without `xvfb-run` on
//! macOS, which opens two windows and reads the pages' colors from the
//! system's `screencapture` of each. With `OM_WEB_VIEWER_SHOTS` set to a
//! directory, each window is saved there as PNG.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(dead_code, unused_imports, reason = "no web viewer backend to test")
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

const OCCURRENCE: u64 = 0xd0_0002;
const TABLE: u64 = 0x8002;
/// `LayoutDef.tableAliasKey` of the Rows occurrence.
const ALIAS: u64 = 0x10_4082;
const LAYOUT: u32 = 7;
/// The records' ids and their pages' colors.
const ROWS: [(i64, &str, [u8; 3]); 2] = [(1, "red", [255, 0, 0]), (2, "blue", [0, 0, 255])];
/// The web viewer's size in points (on X11 the display's scale is 1).
const SIZE: (u16, u16) = (200, 60);
/// Set in the process that runs the test with embedded viewports.
const EMBEDDED: &str = "OM_WEB_VIEWER_WINDOWS_EMBEDDED";

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

/// What the test asks of the window system: a window and a page are ids.
#[cfg(target_os = "linux")]
mod native {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{self, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    use super::{fail, SIZE};

    pub struct Native {
        x11: RustConnection,
        root: u32,
    }

    impl Native {
        /// Connect to the X server; `None` without a display.
        pub fn start() -> Option<Self> {
            if std::env::var_os("DISPLAY").is_none() {
                println!("skipped: no X display (run under xvfb-run)");
                return None;
            }
            let (x11, screen) = x11rb::connect(None).expect("X display");
            let root = x11.setup().roots[screen].root;
            Some(Native { x11, root })
        }

        pub fn first(frame: &eframe::Frame) -> u64 {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match frame.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Xlib(h)) => h.window,
                Ok(RawWindowHandle::Xcb(h)) => u64::from(h.window.get()),
                other => fail(format!("not an X11 window: {other:?}")),
            }
        }

        /// The mapped child windows of `parent` the web viewer's size, top
        /// to bottom: one per List row. `None` when they do not share one
        /// column.
        pub fn pages(&self, parent: u64) -> Option<Vec<u64>> {
            let tree = self.x11.query_tree(parent as u32).ok()?.reply().ok()?;
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
                .then(|| pages.into_iter().map(|(_, _, w)| u64::from(w)).collect())
        }

        /// The color at the middle of each of `parent`'s `pages`.
        pub fn colors(&mut self, _parent: u64, pages: &[u64]) -> Vec<Option<[u8; 3]>> {
            pages.iter().map(|&page| self.color(page as u32)).collect()
        }

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

        /// The New Window's X11 window: the other top-level window eframe's
        /// own connection made (its id carries that connection's resource-id
        /// base, which the first window's shows), wherever a window manager
        /// put it.
        pub fn second_window(&self, first: u64) -> Option<u64> {
            let first = first as u32;
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
                [window] => Some(u64::from(window)),
                [] => None,
                _ => fail(format!("more than one other eframe window: {found:x?}")),
            }
        }

        /// Print `window`'s child windows: geometry, mapped, middle color.
        pub fn describe(&mut self, window: u64) {
            let Ok(Ok(tree)) = self.x11.query_tree(window as u32).map(|c| c.reply()) else {
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

        /// Save window `window` as PNG in `OM_WEB_VIEWER_SHOTS` when it is
        /// set.
        pub fn shot(&mut self, window: u64, name: &str) {
            let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
                return;
            };
            let window = window as u32;
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
            for (pixel, [blue, green, red, _]) in
                rgb.pixels_mut().zip(image.data.as_chunks::<4>().0)
            {
                *pixel = image::Rgb([*red, *green, *blue]);
            }
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            match rgb.save(&path) {
                Ok(()) => println!("screenshot {}", path.display()),
                Err(error) => println!("screenshot {name} failed: {error}"),
            }
        }
    }
}

/// What the test asks of the window system: a window is its AppKit window
/// number, a page its container view.
#[cfg(target_os = "macos")]
mod native {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView, NSWindow};

    use super::{fail, ROWS, SIZE};

    /// A window's capture: the window, when, of which pages, and their
    /// colors.
    type Sample = (u64, Instant, Vec<u64>, Vec<Option<[u8; 3]>>);

    pub struct Native {
        /// Each window's last capture. The screen is read at most twice a
        /// second per window, and a window's last sample stands in
        /// meanwhile.
        captured: Vec<Sample>,
    }

    fn windows() -> Vec<Retained<NSWindow>> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Vec::new();
        };
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .collect()
    }

    fn window(number: u64) -> Option<Retained<NSWindow>> {
        windows()
            .into_iter()
            .find(|w| w.windowNumber() as u64 == number)
    }

    /// The containers of `window`'s pages that show, with their frames in
    /// points from the content view's top left.
    fn containers(window: &NSWindow) -> Vec<(Retained<NSView>, f64, f64, f64, f64)> {
        let Some(content) = window.contentView() else {
            return Vec::new();
        };
        let height = content.frame().size.height;
        content
            .subviews()
            .iter()
            .filter(|view| !view.isHidden())
            .map(|view| {
                let frame = view.frame();
                let top = if content.isFlipped() {
                    frame.origin.y
                } else {
                    height - frame.origin.y - frame.size.height
                };
                (
                    view,
                    frame.origin.x,
                    top,
                    frame.size.width,
                    frame.size.height,
                )
            })
            .collect()
    }

    /// The window as the system's `screencapture` saves it, without its
    /// shadow.
    fn capture(number: u64, path: &std::path::Path) -> bool {
        std::process::Command::new("screencapture")
            .args(["-x", "-o", &format!("-l{number}")])
            .arg(path)
            .status()
            .is_ok_and(|s| s.success())
    }

    impl Native {
        pub fn start() -> Option<Self> {
            Some(Native {
                captured: Vec::new(),
            })
        }

        pub fn first(frame: &eframe::Frame) -> u64 {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let Ok(RawWindowHandle::AppKit(handle)) = frame.window_handle().map(|h| h.as_raw())
            else {
                fail("not an AppKit window".into());
            };
            let view = handle.ns_view.as_ptr() as usize;
            windows()
                .into_iter()
                .find(|w| {
                    w.contentView()
                        .is_some_and(|c| Retained::as_ptr(&c) as usize == view)
                })
                .map_or_else(
                    || fail("the app's window is not found".into()),
                    |w| w.windowNumber() as u64,
                )
        }

        /// The containers in `window` the web viewer's size, top to bottom:
        /// one per List row. `None` when they do not share one column.
        pub fn pages(&self, window: u64) -> Option<Vec<u64>> {
            let window = self::window(window)?;
            let size = (f64::from(SIZE.0), f64::from(SIZE.1));
            let mut pages: Vec<(i64, i64, u64)> = containers(&window)
                .into_iter()
                .filter(|(_, _, _, w, h)| (w.round(), h.round()) == size)
                .map(|(view, x, y, _, _)| {
                    (
                        y.round() as i64,
                        x.round() as i64,
                        Retained::as_ptr(&view) as u64,
                    )
                })
                .collect();
            pages.sort_unstable();
            pages
                .windows(2)
                .all(|pair| pair[0].1 == pair[1].1 && pair[0].0 < pair[1].0)
                .then(|| pages.into_iter().map(|(_, _, page)| page).collect())
        }

        /// The color at the middle of each of `window`'s `pages`, from a
        /// capture of the window: one of the rows' colors when it is close
        /// to it (the capture is in the display's color space).
        pub fn colors(&mut self, number: u64, pages: &[u64]) -> Vec<Option<[u8; 3]>> {
            let recent = self.captured.iter().find(|(w, at, p, _)| {
                *w == number && p == pages && at.elapsed() < Duration::from_millis(500)
            });
            if let Some((_, _, _, colors)) = recent {
                return colors.clone();
            }
            let colors = self.capture(number, pages);
            self.captured.retain(|(w, ..)| *w != number);
            self.captured
                .push((number, Instant::now(), pages.to_vec(), colors.clone()));
            colors
        }

        fn capture(&self, number: u64, pages: &[u64]) -> Vec<Option<[u8; 3]>> {
            let none = vec![None; pages.len()];
            let Some(window) = window(number) else {
                return none;
            };
            let path = std::env::temp_dir().join(format!(
                "openmaker-web-viewer-windows-{}-{number}.png",
                std::process::id()
            ));
            if !capture(number, &path) {
                return none;
            }
            let image = image::open(&path).ok().map(|i| i.to_rgb8());
            let _ = std::fs::remove_file(&path);
            let Some(image) = image else {
                return none;
            };
            let frame = window.frame().size;
            let scale = f64::from(image.width()) / frame.width;
            let title_bar = frame.height
                - window
                    .contentView()
                    .map_or(frame.height, |content| content.frame().size.height);
            let found = containers(&window);
            pages
                .iter()
                .map(|page| {
                    let (_, x, y, w, h) = found
                        .iter()
                        .find(|c| Retained::as_ptr(&c.0) as u64 == *page)?;
                    let px = ((x + w / 2.0) * scale) as u32;
                    let py = ((title_bar + y + h / 2.0) * scale) as u32;
                    let pixel = image.get_pixel_checked(px, py)?.0;
                    let close = |c: &[u8; 3]| {
                        c.iter()
                            .zip(pixel)
                            .all(|(a, b)| (i16::from(*a) - i16::from(b)).abs() < 64)
                    };
                    Some(
                        ROWS.iter()
                            .map(|(_, _, color)| *color)
                            .find(|c| close(c))
                            .unwrap_or(pixel),
                    )
                })
                .collect()
        }

        /// The New Window's AppKit window: the app's other visible window.
        pub fn second_window(&self, first: u64) -> Option<u64> {
            let others: Vec<u64> = windows()
                .into_iter()
                .filter(|w| w.isVisible())
                .map(|w| w.windowNumber() as u64)
                .filter(|number| *number != first)
                .collect();
            match others[..] {
                [window] => Some(window),
                [] => None,
                _ => fail(format!("more than one other window: {others:?}")),
            }
        }

        /// Print `window`'s containers: frame and whether they show.
        pub fn describe(&mut self, number: u64) {
            let Some(window) = window(number) else {
                return;
            };
            for (view, x, y, w, h) in containers(&window) {
                println!(
                    "container {:#x} of {number}: ({x}, {y}) {w}x{h}",
                    Retained::as_ptr(&view) as usize
                );
            }
        }

        /// Save window `window` as PNG in `OM_WEB_VIEWER_SHOTS` when it is
        /// set.
        pub fn shot(&mut self, window: u64, name: &str) {
            let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
                return;
            };
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            if capture(window, &path) {
                println!("screenshot {}", path.display());
            } else {
                println!("screenshot {name} failed");
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Driver {
    app: OpenMakerApp,
    native: native::Native,
    deadline: Instant,
    embedded: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Driver {
    /// The colors of `window`'s pages, top row first, once each shows red
    /// or blue.
    fn page_colors(&mut self, window: u64) -> Option<Vec<[u8; 3]>> {
        let pages = self.native.pages(window)?;
        let colors: Vec<[u8; 3]> = self
            .native
            .colors(window, &pages)
            .into_iter()
            .flatten()
            .collect();
        let painted = colors.len() == pages.len()
            && colors
                .iter()
                .all(|c| ROWS.iter().any(|(_, _, color)| color == c));
        painted.then_some(colors)
    }

    fn step(&mut self, first: u64) {
        if self.embedded {
            return self.step_embedded(first);
        }
        // The List's rows in record order, and Form view's current record.
        let rows = ROWS.map(|(_, _, color)| color).to_vec();
        let current = vec![ROWS[0].2];
        let first_colors = self.page_colors(first);
        let second = self.native.second_window(first);
        let second_colors = second.and_then(|w| self.page_colors(w));
        if Instant::now() > self.deadline {
            for (name, window) in [("timeout-first", Some(first)), ("timeout-second", second)] {
                let Some(window) = window else { continue };
                self.native.shot(window, name);
                self.native.describe(window);
            }
            fail(format!(
                "timed out: first window {first:#x} pages {:?} colors {first_colors:?}; \
                 second window {second:x?} colors {second_colors:?}; error {:?}",
                self.native.pages(first),
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
            self.native.pages(first)
        );
        println!(
            "second window {second:#x}: pages {:?}, colors {second_colors:?}",
            self.native.pages(second)
        );
        if second_colors != rows {
            fail(format!(
                "the second window's rows show {second_colors:?}, not each its own record's \
                 page ({rows:?})"
            ));
        }
        self.native.shot(first, "1-first-window-form");
        self.native.shot(second, "2-second-window-list");
        println!("PASS");
        std::process::exit(0);
    }

    /// With embedded viewports: the List's rows' pages in the app's own
    /// window, once the New Window covers its Form page.
    fn step_embedded(&mut self, first: u64) {
        let rows = ROWS.map(|(_, _, color)| color).to_vec();
        let colors = self.page_colors(first);
        if Instant::now() > self.deadline {
            self.native.shot(first, "timeout-embedded");
            self.native.describe(first);
            fail(format!(
                "timed out with embedded viewports: window {first:#x} pages {:?} colors \
                 {colors:?}; error {:?}",
                self.native.pages(first),
                self.app.error(),
            ));
        }
        let Some(colors) = colors.filter(|c| c.len() >= rows.len()) else {
            return;
        };
        println!(
            "embedded: window {first:#x}: pages {:?}, colors {colors:?}",
            self.native.pages(first)
        );
        if let Some(other) = self.native.second_window(first) {
            fail(format!("embedded viewports, but another window {other:#x}"));
        }
        if colors != rows {
            fail(format!(
                "the embedded window's rows show {colors:?}, not each its own record's page \
                 ({rows:?})"
            ));
        }
        self.native.shot(first, "3-embedded-window-list");
        println!("PASS (embedded viewports)");
        std::process::exit(0);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl eframe::App for Driver {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        self.step(native::Native::first(frame));
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    println!("skipped: no web viewer backend on this platform");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    let Some(native) = native::Native::start() else {
        return;
    };
    let embedded = std::env::var_os(EMBEDDED).is_some();
    if !embedded {
        // A process runs one event loop: the embedded run has its own.
        let exe = std::env::current_exe().unwrap_or_else(|e| fail(format!("no executable: {e}")));
        // Its watchdog starts only once it has its display: this bounds the
        // wait before that too.
        let mut child = std::process::Command::new(exe)
            .env(EMBEDDED, "1")
            .spawn()
            .unwrap_or_else(|e| fail(format!("the embedded run did not start: {e}")));
        let deadline = Instant::now() + Duration::from_secs(240);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                other => {
                    let _ = child.kill();
                    let _ = child.wait();
                    fail(format!("the embedded run did not end: {other:?}"));
                }
            }
        };
        if !status.success() {
            fail(format!("with embedded viewports: {status}"));
        }
    }
    // As `om_gui::run` does, before any other thread starts.
    om_gui::prepare_web_viewers();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(180));
        eprintln!("FAIL: watchdog");
        std::process::exit(3);
    });
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
            if embedded {
                cc.egui_ctx.set_embed_viewports(true);
            }
            let app = OpenMakerApp::new(Box::new(Provider)).open_on_launch(Connection {
                host: "web-viewer.test.invalid".into(),
                port: 5003,
                file: "Rows.fmp12".into(),
                account: "Admin".into(),
                password: String::new(),
            });
            Ok(Box::new(Driver {
                app,
                native,
                deadline: Instant::now() + Duration::from_secs(120),
                embedded,
            }))
        }),
    );
    fail(format!("the event loop ended first: {result:?}"));
}
