//! Web viewers painted as textures (`OPENMAKER_WEB_VIEWER_BACKEND=wayland`,
//! the `web-viewer` feature on Linux), in the real app: eframe's event loop
//! and window on an X server, the page drawn by the page host
//! (`om-gui-webview`) on the app's own display, and real X11 input (XTest).
//!
//! - The layout's web viewer shows a `data:` page whose body is one color;
//!   the page calls `FileMaker.PerformScript` once loaded. The app's window
//!   shows that color over the object's 500 x 300 points, which egui painted:
//!   the window has no child window.
//! - A text object stacked above the viewer draws over the page.
//! - A real click on the page's button reaches the page, whose bridge
//!   performs a script that commits the record's field.
//!
//! `dbus-run-session -- xvfb-run -a cargo test -p om-gui --features web-viewer
//! --test web_viewer_composited` on Linux; skipped without an X display.

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use om_data::{
        Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind,
        HostedFile, LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog,
        ScriptStep,
    };
    use om_gui::OpenMakerApp;
    use om_hbam::{KeySegment, KeyTree, Node};
    use om_layout::{Layout, LayoutDef, LayoutObject, Part, Rect};
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{self, ConnectionExt as _};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    const OCCURRENCE: u64 = 0xd0_0002;
    const TABLE: u64 = 0x8002;
    /// `LayoutDef.tableAliasKey` of the Records occurrence.
    const ALIAS: u64 = 0x10_4082;
    const WEB: u32 = 7;
    /// The page's color, and how far a pixel may be from it.
    const PAGE_COLOR: [u8; 3] = [32, 192, 64];
    const TOLERANCE: i32 = 12;

    /// The page: one color, a button that calls back, and a call back once
    /// loaded. Its colour is written without `#`, which encoding would keep.
    const PAGE: &str = "\"data:text/html,<!doctype html><html><body style='margin:0;\
        background:rgb(32,192,64)'><button onclick=\\\"FileMaker.PerformScript('Record', \
        'clicked')\\\" style='position:absolute;left:100px;top:100px;width:200px;\
        height:80px'>Go</button><script>window.addEventListener('load', function () \
        { FileMaker.PerformScript('Record', 'loaded'); });</script></body></html>\"";

    #[derive(Default)]
    struct State {
        given: String,
    }

    struct Provider(Arc<Mutex<State>>);

    fn names() -> om_formula::NameTable {
        let mut names = om_formula::NameTable::default();
        names.add_occurrence(OCCURRENCE, "Records", TABLE);
        names.add_field(TABLE, 1, "Given");
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

    fn scripts() -> ScriptCatalog {
        // Record: Set Field [ Given ; Get ( ScriptParameter ) ], Commit.
        let mut set = ScriptStep::new(147);
        set.set_calculation(1, Some(&calc("\"Given\"")));
        set.set_calculation(0, Some(&calc("Get ( ScriptParameter )")));
        let mut record = Script {
            id: 2,
            name: "Record".into(),
            ..Default::default()
        };
        record
            .insert_steps(0, vec![set, ScriptStep::new(75)])
            .expect("nests");
        ScriptCatalog {
            scripts: vec![record],
            writable: true,
            unavailable_reason: None,
        }
    }

    /// A text object, laid out in points.
    fn text(id: u64, text: &str, top: f64, left: f64, bottom: f64, right: f64) -> LayoutObject {
        LayoutObject {
            id,
            object_type: 2,
            text: Some(text.into()),
            bounds: Some(Rect {
                top,
                left,
                bottom,
                right,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// The web viewer (object 2) as FileMaker stores it, decoded by
    /// `om-layout` (as `tests/web_viewer_native.rs` makes it), with a text
    /// object (3) stacked above its lower right corner.
    fn layout() -> Layout {
        fn node<'a>(key: &'a [u8], value: Option<&'a [u8]>, children: Vec<Node<'a>>) -> Node<'a> {
            Node {
                segment: KeySegment(key),
                value,
                push: !children.is_empty(),
                children,
            }
        }
        fn varint(mut v: u64, out: &mut Vec<u8>) {
            while v >= 0x80 {
                out.push(v as u8 | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
        }
        fn uint(number: u64, v: u64, out: &mut Vec<u8>) {
            varint(number << 3, out);
            varint(v, out);
        }
        fn bytes(number: u64, b: &[u8], out: &mut Vec<u8>) {
            varint(number << 3 | 2, out);
            varint(b.len() as u64, out);
            out.extend_from_slice(b);
        }
        let mut rect = Vec::new();
        for (n, v) in [(1u64, 60.0f64), (2, 20.0), (3, 360.0), (4, 520.0)] {
            varint(n << 3 | 1, &mut rect);
            rect.extend_from_slice(&v.to_le_bytes());
        }
        // LayoutObject { type 13, key 2, flags (anchored left and top),
        // bounds, style { objectTypeId 86 }, externalObj { 'WEBV', 0,
        // 0x8041: interaction, JavaScript scripts } }.
        let mut external = Vec::new();
        uint(1, u64::from(u32::from_be_bytes(*b"WEBV")), &mut external);
        uint(2, 0, &mut external);
        uint(3, 0x8041, &mut external);
        let mut object = Vec::new();
        uint(1, 13, &mut object);
        uint(2, 2, &mut object);
        uint(3, 0x3000_0000, &mut object);
        bytes(4, &rect, &mut object);
        bytes(5, &[0x18, 86], &mut object);
        bytes(11, &external, &mut object);
        let mut store = Vec::new();
        uint(1, 2, &mut store);
        bytes(2, &object, &mut store);
        let name: Vec<u8> = b"wv".iter().map(|b| b ^ 0x5a).collect();
        let polish = om_formula::polish::encode(&compiled(PAGE).expr);
        let address = node(
            &[0, 0],
            None,
            vec![node(
                &[5],
                None,
                vec![
                    node(&[4], Some(&[1, 0]), vec![]),
                    node(&[6], Some(&polish), vec![]),
                ],
            )],
        );
        let attributes = node(
            &[2],
            None,
            vec![address, node(&[0x10], Some(&name), vec![])],
        );
        let tree = KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[4],
                    None,
                    vec![node(
                        &[5],
                        None,
                        vec![node(
                            &[7],
                            None,
                            vec![
                                node(&[5], None, vec![attributes]),
                                node(&[7], Some(&store), vec![]),
                            ],
                        )],
                    )],
                )],
            ),
        };
        let mut layout = Layout::decode_from_tree(&tree, u64::from(WEB))
            .expect("decodes")
            .expect("layout");
        assert!(layout.objects.get(2).and_then(|o| o.web_viewer()).is_some());
        layout
            .objects
            .objects
            .push(text(3, "COVER", 320.0, 380.0, 344.0, 500.0));
        layout.objects.root.push(3);
        layout.name = "Web".into();
        layout.def = Some(LayoutDef {
            base_width: 800.0,
            table_alias_key: ALIAS,
            ..Default::default()
        });
        layout.parts = vec![Part {
            id: 1,
            part_type: 4,
            top: 0.0,
            height: 500.0,
            ..Default::default()
        }];
        layout
    }

    fn schema() -> om_data::host::CalcSchema {
        use om_data::host::{CalcSchema, FieldDef};
        let mut schema = CalcSchema::new("Web", om_data::calc::Locale::en_us());
        schema.add_table(TABLE, "Records");
        schema.add_occurrence(OCCURRENCE, "Records", TABLE);
        schema.add_field(
            TABLE,
            FieldDef::data(1, "Given", om_data::calc::ValueType::Text),
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
        fn can_write_records(&self) -> bool {
            true
        }
        fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
            Ok(vec![LayoutInfo {
                id: WEB,
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
                om_data::calc::Date::from_ymd(2026, 11, 26).unwrap(),
            );
            let mut source = MemorySource::new();
            let mut values = RecordValues::default();
            values.set(
                1,
                1,
                om_data::calc::Value::text(self.0.lock().unwrap().given.clone()),
            );
            source.put_record(TABLE, 1, values);
            let recorder = Recorder::new(&source);
            for need in needs {
                recorder
                    .fetch(&schema, need, &cx)
                    .map_err(|e| DataError::new(e.to_string()))?;
            }
            Ok(recorder.take())
        }
        fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
            let given = self.0.lock().unwrap().given.clone();
            Ok(RecordPage {
                versions: Default::default(),
                fields: vec![Field {
                    name: "Given".into(),
                    kind: FieldKind::Text,
                }],
                records: vec![Record {
                    id: 1,
                    values: vec![Some(given)],
                }]
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect(),
                total: 1,
            })
        }
        fn update_record(
            &mut self,
            _: u32,
            id: i64,
            values: FieldChanges,
        ) -> Result<Record, DataError> {
            let mut state = self.0.lock().unwrap();
            if let Some(Some(value)) = values.get("Given") {
                state.given = value.clone();
            }
            Ok(Record {
                id,
                values: vec![Some(state.given.clone())],
            })
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

    /// The app's window's pixels, as RGB rows.
    struct Shot {
        width: usize,
        height: usize,
        rgb: Vec<[u8; 3]>,
    }

    impl Shot {
        fn take(x11: &RustConnection, window: u32) -> Option<Self> {
            let geometry = x11.get_geometry(window).ok()?.reply().ok()?;
            let image = x11
                .get_image(
                    xproto::ImageFormat::Z_PIXMAP,
                    window,
                    0,
                    0,
                    geometry.width,
                    geometry.height,
                    !0,
                )
                .ok()?
                .reply()
                .ok()?;
            // 32 bits per pixel: blue, green, red, unused.
            let rgb = image
                .data
                .chunks_exact(4)
                .map(|p| [p[2], p[1], p[0]])
                .collect();
            Some(Shot {
                width: usize::from(geometry.width),
                height: usize::from(geometry.height),
                rgb,
            })
        }

        fn at(&self, x: usize, y: usize) -> [u8; 3] {
            self.rgb[y * self.width + x]
        }

        /// The bounding box of the page's color: left, top, right, bottom.
        fn page(&self) -> Option<[usize; 4]> {
            let near = |p: [u8; 3]| {
                p.iter()
                    .zip(PAGE_COLOR)
                    .all(|(a, b)| (i32::from(*a) - i32::from(b)).abs() <= TOLERANCE)
            };
            let mut found: Option<[usize; 4]> = None;
            for y in 0..self.height {
                for x in 0..self.width {
                    if near(self.at(x, y)) {
                        let b = found.get_or_insert([x, y, x, y]);
                        b[0] = b[0].min(x);
                        b[1] = b[1].min(y);
                        b[2] = b[2].max(x);
                        b[3] = b[3].max(y);
                    }
                }
            }
            found
        }

        /// Save it as PNG in `OM_WEB_VIEWER_SHOTS` when that is set.
        fn save(&self, name: &str) {
            let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
                return;
            };
            let mut image = image::RgbImage::new(self.width as u32, self.height as u32);
            for (pixel, rgb) in image.pixels_mut().zip(&self.rgb) {
                *pixel = image::Rgb(*rgb);
            }
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            match image.save(&path) {
                Ok(()) => println!("screenshot {}", path.display()),
                Err(error) => println!("screenshot {name} failed: {error}"),
            }
        }
    }

    #[derive(Debug, PartialEq)]
    enum Phase {
        Loading,
        Clicking,
    }

    struct Driver {
        app: OpenMakerApp,
        state: Arc<Mutex<State>>,
        x11: RustConnection,
        root: u32,
        phase: Phase,
        deadline: Instant,
    }

    impl Driver {
        fn window(frame: &eframe::Frame) -> u32 {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match frame.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Xlib(h)) => h.window as u32,
                Ok(RawWindowHandle::Xcb(h)) => h.window.get(),
                other => fail(format!("not an X11 window: {other:?}")),
            }
        }

        fn click(&self, window: u32, x: i16, y: i16) {
            let at = self
                .x11
                .translate_coordinates(window, self.root, x, y)
                .unwrap()
                .reply()
                .unwrap();
            for (kind, detail) in [
                (xproto::MOTION_NOTIFY_EVENT, 0),
                (xproto::BUTTON_PRESS_EVENT, 1),
                (xproto::BUTTON_RELEASE_EVENT, 1),
            ] {
                self.x11
                    .xtest_fake_input(
                        kind,
                        detail,
                        x11rb::CURRENT_TIME,
                        self.root,
                        at.dst_x,
                        at.dst_y,
                        0,
                    )
                    .unwrap();
                self.x11.flush().unwrap();
                std::thread::sleep(Duration::from_millis(80));
            }
        }

        fn step(&mut self, window: u32) {
            let given = self.state.lock().unwrap().given.clone();
            if Instant::now() > self.deadline {
                fail(format!(
                    "timed out in {:?}; Given {given:?}; error {:?}",
                    self.phase,
                    self.app.error()
                ));
            }
            match self.phase {
                Phase::Loading if given == "loaded" => {
                    let Some(shot) = Shot::take(&self.x11, window) else {
                        return;
                    };
                    let Some([left, top, right, bottom]) = shot.page() else {
                        // Loaded, not painted yet.
                        return;
                    };
                    let (width, height) = (right - left + 1, bottom - top + 1);
                    if (width, height) != (500, 300) {
                        return;
                    }
                    shot.save("1-composited-page");
                    println!("page painted at ({left}, {top}), {width}x{height}");
                    let children = self
                        .x11
                        .query_tree(window)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .children;
                    if !children.is_empty() {
                        fail(format!("the window has child windows: {children:?}"));
                    }
                    // The text object above the page's lower right corner
                    // (at 360, 260 points in the page) draws over it.
                    let dark = (left + 360..left + 480)
                        .flat_map(|x| (top + 260..top + 284).map(move |y| (x, y)))
                        .filter(|&(x, y)| shot.at(x, y).iter().all(|c| *c < 96))
                        .count();
                    if dark == 0 {
                        fail("the text stacked above the web viewer is not drawn over it".into());
                    }
                    println!("{dark} dark pixels of the text above the page");
                    // The page's button: 100..300 x 100..180 in the page.
                    self.click(window, (left + 200) as i16, (top + 140) as i16);
                    self.phase = Phase::Clicking;
                }
                Phase::Clicking if given == "clicked" => {
                    if let Some(shot) = Shot::take(&self.x11, window) {
                        shot.save("2-composited-after-click");
                    }
                    println!("the click reached the page, whose bridge set Given");
                    println!("PASS");
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }

    impl eframe::App for Driver {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            eframe::App::ui(&mut self.app, ui, frame);
            self.step(Self::window(frame));
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }

    pub fn main() {
        if std::env::var_os("DISPLAY").is_none() {
            println!("skipped: no X display (run under xvfb-run)");
            return;
        }
        // Before anything reads them: pages painted by the page host this
        // test's build made, and the app's window on the X server.
        std::env::set_var("OPENMAKER_WEB_VIEWER_BACKEND", "wayland");
        std::env::set_var(
            "OPENMAKER_WEB_VIEWER_HOST",
            env!("CARGO_BIN_EXE_om-gui-webview"),
        );
        std::env::remove_var("WAYLAND_DISPLAY");
        om_gui::prepare_web_viewers();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(180));
            eprintln!("FAIL: watchdog");
            std::process::exit(3);
        });
        let (x11, screen) = x11rb::connect(None).expect("X display");
        let root = x11.setup().roots[screen].root;
        let state = Arc::new(Mutex::new(State::default()));
        let provider = Provider(Arc::clone(&state));
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("OpenMaker composited web viewer test")
                .with_inner_size([1000.0, 760.0])
                .with_position([0.0, 0.0]),
            ..om_gui::native_options()
        };
        let result = eframe::run_native(
            "OpenMaker composited web viewer test",
            options,
            Box::new(move |cc| {
                om_gui::install_theme(&cc.egui_ctx);
                let app = OpenMakerApp::new(Box::new(provider)).open_on_launch(Connection {
                    host: "web-viewer.test.invalid".into(),
                    port: 5003,
                    file: "Web.fmp12".into(),
                    account: "Admin".into(),
                    password: String::new(),
                });
                Ok(Box::new(Driver {
                    app,
                    state,
                    x11,
                    root,
                    phase: Phase::Loading,
                    deadline: Instant::now() + Duration::from_secs(120),
                }))
            }),
        );
        fail(format!("the event loop ended first: {result:?}"));
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
    #[cfg(not(target_os = "linux"))]
    println!("skipped: the Wayland backend is Linux's");
}
