//! Web viewers in the real app (the `web-viewer` feature): eframe's own
//! event loop and root window, the browser's own process, and input the
//! window system delivers: WebKitGTK under an X server, with its GTK thread
//! and real X11 input (XTest), or WKWebView on macOS, with mouse events
//! posted to the window (they reach the page as a click does, without
//! moving the pointer).
//!
//! - The layout's web viewer is decoded from native wire bytes (an external
//!   object of type `WEBV` with "Allow interaction" and "Allow JavaScript to
//!   perform FileMaker scripts", named `wv`, its address the formula under
//!   attribute `00 00/5`), and its calculated `data:` page (with a `#` that
//!   automatic URL encoding must keep) opens as a child window of the app's
//!   window, the object's size.
//! - OnLayoutEnter calls the page's `show` with Perform JavaScript in Web
//!   Viewer, until the page has loaded; `show` writes its text parameters
//!   (quotes, a return, non-ASCII) into the page and calls
//!   `FileMaker.PerformScript ( "Record" ; <the element's text> )`, which
//!   sets and commits the record's field. It also asks a hidden frame to
//!   load a bridge message of its own making, which must run nothing.
//! - A real click (XTest) on the page's button calls
//!   `FileMaker.PerformScriptWithOption`; that script loads a page from a
//!   loopback HTTP server with Set Web Viewer, which calls back on load.
//! - Going to another layout destroys the view: no child window (X11) or
//!   page view (AppKit) is left in the app's window, the old page's timer
//!   never calls back, and Perform JavaScript on it fails with 116. On
//!   Linux the WebKit web process exits too; macOS runs its web processes
//!   as system services, not as the app's children, so it is not checked.
//! - On Linux it all runs with `WAYLAND_DISPLAY` set, as in a Wayland
//!   session, so `om_gui::native_options` must choose winit's X11 backend.
//!
//! `xvfb-run -a cargo test -p om-gui --features web-viewer --test
//! web_viewer_native` on Linux, skipped without a display;
//! `cargo test -p om-gui --features web-viewer --test web_viewer_native` on
//! macOS, which opens a window.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use om_data::{
    Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, HostedFile,
    LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog, ScriptStep, StoredKey,
};
use om_gui::OpenMakerApp;
use om_hbam::{KeySegment, KeyTree, Node};
use om_layout::{Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};

const OCCURRENCE: u64 = 0xd0_0002;
const TABLE: u64 = 0x8002;
/// `LayoutDef.tableAliasKey` of the Records occurrence.
const ALIAS: u64 = 0x10_4082;
const WEB: u32 = 7;
const OTHER: u32 = 8;

#[derive(Default)]
struct State {
    given: String,
    /// Every value scripts committed into Given, in order.
    history: Vec<String>,
    scripts: ScriptCatalog,
}

struct Provider(Arc<Mutex<State>>);

fn names() -> om_formula::NameTable {
    let mut names = om_formula::NameTable::default();
    names.add_occurrence(OCCURRENCE, "Records", TABLE);
    names.add_field(TABLE, 1, "Given");
    names.add_field(TABLE, 2, "Title");
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
    for (slot, text) in calcs {
        step.set_calculation(*slot, Some(&calc(text)));
    }
    step
}

fn error_capture() -> ScriptStep {
    let mut step = ScriptStep::new(86);
    step.set_flag(om_data::flag::TURN_ON, true);
    step
}

fn set_given(text: &str) -> Vec<ScriptStep> {
    vec![
        step(147, &[(1, "\"Given\""), (0, text)]),
        ScriptStep::new(75),
    ]
}

fn perform_javascript(function: &str, parameters: &[&str]) -> ScriptStep {
    let mut step = step(175, &[(0, "\"wv\""), (1, function)]);
    step.set_number(0, Some(parameters.len() as i32));
    for (n, parameter) in parameters.iter().enumerate() {
        step.set_calculation(10 + n as u16, Some(&calc(parameter)));
    }
    step
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    let mut script = Script {
        id,
        name: name.into(),
        ..Default::default()
    };
    script.insert_steps(0, steps).expect("nests");
    script
}

fn scripts(port: u16) -> ScriptCatalog {
    let mut go_to_other = ScriptStep::new(6);
    go_to_other.value = 2;
    go_to_other.set_member_key(StoredKey::from_id(u64::from(OTHER)));
    // OnLayoutEnter: call the page's function once it has loaded.
    let mut pause = step(62, &[(0, "0.1")]);
    pause.set_flag(om_data::flag::CALCULATION, true);
    let start = vec![
        error_capture(),
        step(141, &[(1, "0")]),
        ScriptStep::new(71),
        perform_javascript("\"show\"", &["\"say \\\"hi\\\"\" & ¶ & \"é\"", "2"]),
        step(72, &[(0, "Get ( LastError ) = 0 or $i > 200")]),
        step(141, &[(1, "$i + 1")]),
        pause,
        ScriptStep::new(73),
    ];
    let mut start = start;
    start[1].set_text(0, Some("$i"));
    start[5].set_text(0, Some("$i"));
    // No prior/forward item exists on the initial page. History no-ops
    // must leave its function callable, including while it first loads.
    let mut forward = step(146, &[(0, "\"wv\"")]);
    forward.set_number(0, Some(3));
    let mut back = step(146, &[(0, "\"wv\"")]);
    back.set_number(0, Some(4));
    start.splice(3..3, [forward, back]);
    let url = format!("\"http://127.0.0.1:{port}/page.html\"");
    let mut record = vec![error_capture()];
    record.extend(set_given("Get ( ScriptParameter )"));
    record.extend([
        step(
            68,
            &[(0, "Left ( Get ( ScriptParameter ) ; 8 ) = \"clicked:\"")],
        ),
        step(146, &[(0, "\"wv\""), (1, &url)]),
        ScriptStep::new(70),
        step(
            68,
            &[(0, "Left ( Get ( ScriptParameter ) ; 5 ) = \"http:\"")],
        ),
        go_to_other,
        perform_javascript("\"show\"", &["\"gone\"", "0"]),
    ]);
    record.extend(set_given("\"after:\" & Get ( LastError )"));
    record.push(ScriptStep::new(70));
    ScriptCatalog {
        scripts: vec![script(1, "Start", start), script(2, "Record", record)],
        writable: true,
        unavailable_reason: None,
    }
}

/// The page the web viewer's address calculates: its title is the record's
/// Title; the `#msg` rule must survive automatic URL encoding.
const PAGE: &str = "\"data:text/html,<!doctype html><html><head><title>\" & Records::Title & \
    \"</title><style>body{margin:0} #msg{color:green}</style><script>\
    function show(text, n) { var msg = document.getElementById('msg'); \
    var forged = document.createElement('iframe'); forged.style.display = 'none'; \
    forged.src = 'openmaker-message:openmaker-perform-script&s=Record&p=forged&o=0'; \
    document.body.appendChild(forged); \
    msg.textContent = text + '|' + n; FileMaker.PerformScript('Record', msg.textContent); } \
    function send() { FileMaker.PerformScriptWithOption('Record', 'clicked:' + \
    document.getElementById('msg').textContent, '0'); }</script></head><body>\
    <p id='msg'>waiting</p><button onclick='send()' style='position:absolute;left:0;\
    top:100px;width:300px;height:150px'>Send</button></body></html>\"";

/// The web viewer as FileMaker stores it, decoded by `om-layout`.
fn web_viewer_layout() -> Layout {
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
    fn rect(top: f64, left: f64, bottom: f64, right: f64) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, v) in [(1u64, top), (2, left), (3, bottom), (4, right)] {
            varint(n << 3 | 1, &mut out);
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
    // LayoutObject { type 13, key 2, flags (anchored left and top), bounds,
    // style { objectTypeId 86 }, externalObj { 'WEBV', 0, 0x8041 } }.
    let mut external = Vec::new();
    uint(1, u64::from(u32::from_be_bytes(*b"WEBV")), &mut external);
    uint(2, 0, &mut external);
    uint(3, 0x8041, &mut external);
    let mut object = Vec::new();
    uint(1, 13, &mut object);
    uint(2, 2, &mut object);
    uint(3, 0x3000_0000, &mut object);
    bytes(4, &rect(60.0, 20.0, 360.0, 520.0), &mut object);
    bytes(5, &[0x18, 86], &mut object);
    bytes(11, &external, &mut object);
    let mut store = Vec::new();
    uint(1, 2, &mut store);
    bytes(2, &object, &mut store);

    let name: Vec<u8> = b"wv".iter().map(|b| b ^ 0x5a).collect();
    let polish = om_formula::polish::encode(&compiled(PAGE).expr);
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
                            node(
                                &[5],
                                None,
                                vec![node(
                                    &[2],
                                    None,
                                    vec![
                                        node(
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
                                        ),
                                        node(&[0x10], Some(&name), vec![]),
                                    ],
                                )],
                            ),
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
    let viewer = layout.objects.get(2).expect("web viewer");
    assert_eq!(viewer.name.as_deref(), Some("wv"));
    let options = viewer.web_viewer().expect("a web viewer");
    assert!(options.allows_interaction() && options.allows_javascript_scripts());
    assert!(viewer.calcs.web_address.is_some());
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

fn other_layout() -> Layout {
    Layout {
        id: u64::from(OTHER),
        name: "Other".into(),
        def: Some(LayoutDef {
            base_width: 800.0,
            table_alias_key: ALIAS,
            ..Default::default()
        }),
        parts: vec![Part {
            id: 1,
            part_type: 4,
            top: 0.0,
            height: 500.0,
            ..Default::default()
        }],
        objects: ObjectStore {
            root: vec![1],
            objects: vec![LayoutObject {
                id: 1,
                object_type: 2,
                text: Some("OTHER LAYOUT".into()),
                bounds: Some(Rect {
                    top: 20.0,
                    left: 20.0,
                    bottom: 50.0,
                    right: 300.0,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
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
    schema.add_field(
        TABLE,
        FieldDef::data(2, "Title", om_data::calc::ValueType::Text),
    );
    schema
}

fn fields() -> Vec<Field> {
    ["Given", "Title"]
        .map(|name| Field {
            name: name.into(),
            kind: FieldKind::Text,
        })
        .into()
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
        Ok([(WEB, "Web"), (OTHER, "Other")]
            .map(|(id, name)| LayoutInfo {
                id,
                name: name.into(),
            })
            .into())
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.0.lock().unwrap().scripts.clone())
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
            om_data::calc::Date::from_ymd(2026, 11, 2).unwrap(),
        );
        let mut source = MemorySource::new();
        let mut values = RecordValues::default();
        values.set(
            1,
            1,
            om_data::calc::Value::text(self.0.lock().unwrap().given.clone()),
        );
        values.set(2, 1, om_data::calc::Value::text("Web viewer test"));
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
        let state = self.0.lock().unwrap();
        let records = vec![Record {
            id: 1,
            values: vec![Some(state.given.clone()), Some("Web viewer test".into())],
        }];
        Ok(RecordPage {
            versions: Default::default(),
            fields: fields(),
            records: records
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
            state.history.push(value.clone());
        }
        Ok(Record {
            id,
            values: vec![Some(state.given.clone()), Some("Web viewer test".into())],
        })
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        Ok(Some(RenderLayout {
            layout: if id == WEB {
                web_viewer_layout()
            } else {
                other_layout()
            },
            field_bindings: BTreeMap::new(),
        }))
    }
}

/// A loopback HTTP server: every request gets the page, which calls back on
/// load and, too late, from a timer.
fn serve() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            let line = String::from_utf8_lossy(&request)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            seen.lock().unwrap().push(line);
            let body = "<!doctype html><html><body><p id='m'>http page</p><script>\
                window.addEventListener('load', function () {\
                FileMaker.PerformScript('Record', 'http:' + location.host + location.pathname);\
                setTimeout(function () { FileMaker.PerformScript('Record', 'late'); }, 1500);\
                });</script></body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, requests)
}

#[derive(Debug, PartialEq)]
enum Phase {
    Loading,
    Clicking,
    Navigating,
    Late(Instant),
    Released(Instant),
}

struct Driver {
    app: OpenMakerApp,
    state: Arc<Mutex<State>>,
    requests: Arc<Mutex<Vec<String>>>,
    port: u16,
    native: native::Native,
    phase: Phase,
    deadline: Instant,
}

const SHOWN: &str = "say \"hi\"\r\u{e9}|2";

fn fail(message: String) -> ! {
    eprintln!("FAIL: {message}");
    std::process::exit(1);
}

/// A view's native window (X11) or container view (AppKit) in the app's
/// window: its id, position, size and whether it shows.
type Child = (u64, i32, i32, u32, u32, bool);

/// What the test asks of the window system.
#[cfg(target_os = "linux")]
mod native {
    use std::time::Duration;

    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{self, ConnectionExt as _};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    use super::{fail, Child};

    /// The app's X11 window.
    pub type Parent = u32;

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
            // A Wayland session's environment, with a socket that does
            // not exist: left to itself winit would choose Wayland and
            // fail, so the app's own options must put its windows on X11
            // (XWayland in a real session).
            std::env::set_var("WAYLAND_DISPLAY", "openmaker-test-no-such-wayland");
            let (x11, screen) = x11rb::connect(None).expect("X display");
            let root = x11.setup().roots[screen].root;
            Some(Native { x11, root })
        }

        pub fn parent(frame: &eframe::Frame) -> Parent {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match frame.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Xlib(h)) => h.window as u32,
                Ok(RawWindowHandle::Xcb(h)) => h.window.get(),
                other => fail(format!("not an X11 window: {other:?}")),
            }
        }

        /// The child windows of `parent`.
        pub fn children(&self, parent: Parent) -> Vec<Child> {
            let Ok(reply) = self.x11.query_tree(parent).map(|c| c.reply()) else {
                return vec![];
            };
            let Ok(tree) = reply else {
                return vec![];
            };
            tree.children
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
                    Some((
                        u64::from(child),
                        i32::from(geometry.x),
                        i32::from(geometry.y),
                        u32::from(geometry.width),
                        u32::from(geometry.height),
                        mapped,
                    ))
                })
                .collect()
        }

        /// Save the app's window, the page's child window included, as PNG
        /// in `OM_WEB_VIEWER_SHOTS` when it is set (evidence).
        pub fn shot(&self, parent: Parent, name: &str) {
            let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
                return;
            };
            let Ok(Ok(geometry)) = self.x11.get_geometry(parent).map(|c| c.reply()) else {
                return;
            };
            let (width, height) = (geometry.width, geometry.height);
            let Ok(Ok(image)) = self
                .x11
                .get_image(
                    xproto::ImageFormat::Z_PIXMAP,
                    parent,
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
            // 32 bits per pixel, blue green red unused.
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

        /// A real click (XTest) at `x`, `y` in the child window `child`.
        pub fn click(&self, _parent: Parent, child: u64, x: i16, y: i16) {
            let at = self
                .x11
                .translate_coordinates(child as u32, self.root, x, y)
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
                std::thread::sleep(Duration::from_millis(60));
            }
            println!("clicked page button at root ({}, {})", at.dst_x, at.dst_y);
        }

        /// The WebKit web processes this process started.
        pub fn web_processes() -> Option<usize> {
            let me = std::process::id().to_string();
            let count = std::fs::read_dir("/proc")
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    let path = entry.path();
                    let comm = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
                    let stat = std::fs::read_to_string(path.join("stat")).unwrap_or_default();
                    // pid (comm) state ppid ...
                    let ppid = stat
                        .rsplit(')')
                        .next()
                        .and_then(|rest| rest.split_whitespace().nth(1));
                    comm.starts_with("WebKitWebProces") && ppid == Some(me.as_str())
                })
                .count();
            Some(count)
        }
    }
}

/// What the test asks of the window system.
#[cfg(target_os = "macos")]
mod native {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
    use objc2_core_foundation::CGPoint;

    use super::{fail, Child};

    /// The app's window's number.
    pub type Parent = isize;

    pub struct Native;

    fn window(number: Parent) -> Option<Retained<NSWindow>> {
        let mtm = MainThreadMarker::new()?;
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .find(|w| w.windowNumber() == number)
    }

    impl Native {
        pub fn start() -> Option<Self> {
            Some(Native)
        }

        pub fn parent(frame: &eframe::Frame) -> Parent {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let Ok(RawWindowHandle::AppKit(handle)) = frame.window_handle().map(|h| h.as_raw())
            else {
                fail("not an AppKit window".into());
            };
            let view = handle.ns_view.as_ptr() as usize;
            let mtm = MainThreadMarker::new().expect("the main thread");
            NSApplication::sharedApplication(mtm)
                .windows()
                .iter()
                .find(|w| {
                    w.contentView()
                        .is_some_and(|c| Retained::as_ptr(&c) as usize == view)
                })
                .map_or_else(
                    || fail("the app's window is not found".into()),
                    |w| w.windowNumber(),
                )
        }

        /// The views in `parent`'s content view (the pages' containers),
        /// in points from its top left.
        pub fn children(&self, parent: Parent) -> Vec<Child> {
            let Some(content) = window(parent).and_then(|w| w.contentView()) else {
                return vec![];
            };
            let height = content.frame().size.height;
            content
                .subviews()
                .iter()
                .map(|view| {
                    let frame = view.frame();
                    let top = if content.isFlipped() {
                        frame.origin.y
                    } else {
                        height - frame.origin.y - frame.size.height
                    };
                    (
                        Retained::as_ptr(&view) as u64,
                        frame.origin.x.round() as i32,
                        top.round() as i32,
                        frame.size.width.round() as u32,
                        frame.size.height.round() as u32,
                        !view.isHidden(),
                    )
                })
                .collect()
        }

        /// Save the app's window as PNG in `OM_WEB_VIEWER_SHOTS` when it is
        /// set (evidence), with the system's `screencapture`.
        pub fn shot(&self, parent: Parent, name: &str) {
            let Some(dir) = std::env::var_os("OM_WEB_VIEWER_SHOTS") else {
                return;
            };
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            let saved = std::process::Command::new("screencapture")
                .args(["-x", "-o", &format!("-l{parent}")])
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
            if saved {
                println!("screenshot {}", path.display());
            } else {
                println!("screenshot {name} failed");
            }
        }

        /// A click at `x`, `y` points from the top left of the container
        /// `child`: a mouse down and up posted to the window, which sends
        /// them to the view under them.
        pub fn click(&self, parent: Parent, child: u64, x: i16, y: i16) {
            let Some(window) = window(parent) else {
                fail("the app's window is gone".into());
            };
            let Some(content) = window.contentView() else {
                fail("the app's window has no content view".into());
            };
            let Some(container) = content
                .subviews()
                .iter()
                .find(|view| Retained::as_ptr(view) as u64 == child)
            else {
                fail("the page's view is gone".into());
            };
            let height = container.frame().size.height;
            let point = CGPoint::new(f64::from(x), height - f64::from(y));
            let at = container.convertPoint_toView(point, None);
            // The view under the point takes the events itself: the window
            // would first make itself key, which a test must not ask of
            // the desktop it runs on.
            let Some(target) = content.hitTest(at) else {
                fail("nothing under the page's button".into());
            };
            for (number, kind) in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp]
                .into_iter()
                .enumerate()
            {
                let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    kind,
                    at,
                    NSEventModifierFlags::empty(),
                    0.0,
                    parent,
                    None,
                    number as isize,
                    1,
                    1.0,
                )
                .expect("a mouse event");
                if kind == NSEventType::LeftMouseDown {
                    target.mouseDown(&event);
                } else {
                    target.mouseUp(&event);
                }
            }
            println!("clicked page button at window ({}, {})", at.x, at.y);
        }

        /// macOS runs WebKit's web processes as system services, not as
        /// children of the app: not counted.
        pub fn web_processes() -> Option<usize> {
            None
        }
    }
}

/// Elsewhere there is no native backend to test.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod native {
    use super::Child;

    pub type Parent = u64;

    pub struct Native;

    impl Native {
        pub fn start() -> Option<Self> {
            println!("skipped: no web viewer backend on this platform");
            None
        }
        pub fn parent(_: &eframe::Frame) -> Parent {
            0
        }
        pub fn children(&self, _: Parent) -> Vec<Child> {
            Vec::new()
        }
        pub fn shot(&self, _: Parent, _: &str) {}
        pub fn click(&self, _: Parent, _: u64, _: i16, _: i16) {}
        pub fn web_processes() -> Option<usize> {
            None
        }
    }
}

impl Driver {
    fn step(&mut self, parent: native::Parent) {
        let (given, history) = {
            let state = self.state.lock().unwrap();
            (state.given.clone(), state.history.clone())
        };
        if history.iter().any(|v| v == "late") {
            fail(format!("a destroyed page called back: {history:?}"));
        }
        if history.iter().any(|v| v == "forged") {
            fail(format!(
                "a message without the bridge ran a script: {history:?}"
            ));
        }
        if Instant::now() > self.deadline {
            fail(format!(
                "timed out in {:?}; history {history:?}; error {:?}; children {:?}",
                self.phase,
                self.app.error(),
                self.native.children(parent)
            ));
        }
        match self.phase {
            Phase::Loading if given == SHOWN => {
                let children = self.native.children(parent);
                let [(child, x, y, width, height, true)] = children[..] else {
                    fail(format!("expected one shown child: {children:?}"));
                };
                // The object's 500 x 300 points (on X11 the window's scale
                // is 1; AppKit counts in points).
                println!("page at ({x}, {y}) size {width}x{height} in parent {parent:#x}");
                if (width, height) != (500, 300) {
                    fail(format!("page size {width}x{height}"));
                }
                println!("callback set Given to {given:?} after Perform JavaScript");
                if let Some(count) = native::Native::web_processes() {
                    println!("web processes: {count}");
                }
                self.native.shot(parent, "1-page-after-perform-javascript");
                self.native.click(parent, child, 150, 175);
                self.phase = Phase::Clicking;
            }
            Phase::Clicking if given.starts_with("clicked:") => {
                assert_eq!(given, format!("clicked:{SHOWN}"));
                println!("click callback set Given to {given:?}");
                self.native.shot(parent, "2-after-click");
                self.phase = Phase::Navigating;
            }
            Phase::Navigating | Phase::Clicking if given.starts_with("after:") => {
                let expected_http = format!("http:127.0.0.1:{}/page.html", self.port);
                if !history.contains(&expected_http) {
                    fail(format!("no http callback in {history:?}"));
                }
                if given != "after:116" {
                    fail(format!("Perform JavaScript on the destroyed view: {given}"));
                }
                println!(
                    "http requests {:?}; Set Web Viewer page called back; after layout change {given}",
                    self.requests.lock().unwrap()
                );
                self.phase = Phase::Late(Instant::now() + Duration::from_secs(3));
            }
            Phase::Late(until) if Instant::now() >= until => {
                let children = self.native.children(parent);
                if !children.is_empty() {
                    fail(format!(
                        "{} children left after the layout changed",
                        children.len()
                    ));
                }
                println!("nothing left in the app window after the layout changed");
                self.native.shot(parent, "3-other-layout");
                self.phase = Phase::Released(Instant::now() + Duration::from_secs(15));
            }
            Phase::Released(until) => match native::Native::web_processes() {
                None => {
                    println!("history {history:?}");
                    println!("PASS");
                    std::process::exit(0);
                }
                Some(0) => {
                    println!("web process exited; history {history:?}");
                    println!("PASS");
                    std::process::exit(0);
                }
                Some(left) if Instant::now() >= until => {
                    fail(format!("{left} WebKit web processes still running"));
                }
                Some(_) => {}
            },
            _ => {}
        }
    }
}

impl eframe::App for Driver {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        self.step(native::Native::parent(frame));
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    }
}

fn main() {
    let Some(native) = native::Native::start() else {
        return;
    };
    // As `om_gui::run` does, before any other thread starts.
    om_gui::prepare_web_viewers();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(240));
        eprintln!("FAIL: watchdog");
        std::process::exit(3);
    });
    let (port, requests) = serve();
    let state = Arc::new(Mutex::new(State {
        scripts: scripts(port),
        ..Default::default()
    }));
    let provider = Provider(Arc::clone(&state));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenMaker web viewer test")
            .with_inner_size([1000.0, 760.0])
            .with_position([0.0, 0.0]),
        ..om_gui::native_options()
    };
    let result = eframe::run_native(
        "OpenMaker web viewer test",
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
                requests,
                port,
                native,
                phase: Phase::Loading,
                deadline: Instant::now() + Duration::from_secs(180),
            }))
        }),
    );
    fail(format!("the event loop ended first: {result:?}"));
}
