//! Web viewers on Linux/X11: WebKitGTK through wry, on a thread of its own
//! that runs GTK's main loop, so pages load, paint and take input without
//! waiting for the app's frames and a slow page never holds up its windows.
//! Commands come over a channel (each one wakes the loop with an idle
//! callback); events go back over another, with a repaint request.
//!
//! A view is a GTK window whose GDK window is an X11 child window of the
//! app's window, made on an x11rb connection (wry's child-window technique,
//! without unsafe code). The page lies in a `gtk::Fixed` at its offset from
//! that child window, so the window clips it to the document's visible part.
//! Views are incognito (no cookies or storage written to disk) and have no
//! developer tools; they open no other windows, download nothing, and go
//! only to http, https, data, blob, file and about URLs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use javascriptcore::ValueExt as _;
use webkit2gtk::{UserContentManagerExt as _, WebViewExt as _};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, InputFocus, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Backend, Event, Geometry, NativeParent, Navigation, Unavailable, ViewSpec, BRIDGE};

enum Command {
    Create {
        view: u64,
        parent: NativeParent,
        spec: ViewSpec,
    },
    Place {
        view: u64,
        geometry: Geometry,
        visible: bool,
    },
    Navigate {
        view: u64,
        generation: u64,
        navigation: Navigation,
    },
    Evaluate {
        view: u64,
        call: u64,
        script: String,
    },
    Destroy {
        view: u64,
    },
    ReleaseFocus {
        parent: NativeParent,
    },
}

pub(crate) struct GtkBackend {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

/// WebKitGTK's DMA-BUF renderer shows blank pages on X servers without
/// DRI3 (Xvfb, VNC, some proprietary drivers). There, unless the
/// environment says otherwise, turn it off for this process's views. The
/// variable must be set before any other thread starts.
pub(crate) fn prepare_environment() {
    const VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    if std::env::var_os(VARIABLE).is_some() {
        return;
    }
    let Ok((x11, _)) = x11rb::connect(None) else {
        return;
    };
    let dri3 = x11
        .query_extension(b"DRI3")
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.present);
    if !dri3 {
        std::env::set_var(VARIABLE, "1");
    }
}

/// The views are X11 child windows, so the app's windows must be X11
/// windows too. On a Wayland session winit would open Wayland windows
/// (it prefers `WAYLAND_DISPLAY` to `DISPLAY`), so while an X server
/// answers, which on a Wayland session is XWayland, the event loop is built
/// on winit's X11 backend. Without one, or without the libraries that
/// backend opens, the windows stay on Wayland and their web viewers show
/// the note.
pub(crate) fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    if !x11_libraries() {
        return None;
    }
    x11rb::connect(None).ok()?;
    Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;
        builder.with_x11();
    }))
}

/// Whether the libraries winit's X11 backend opens at run time are there,
/// looked for with the loaders and names it uses. Without libxkbcommon-x11
/// it panics, and without the others its event loop fails to start, on a
/// Wayland session where the Wayland backend would have worked. The package
/// depends on all of them (`tools/package/build.sh`); a build from source
/// may lack them.
fn x11_libraries() -> bool {
    use x11_dl::{xcursor::Xcursor, xinput2::XInput2, xlib::Xlib, xlib_xcb::Xlib_xcb};
    Xlib::open().is_ok()
        && Xcursor::open().is_ok()
        && Xlib_xcb::open().is_ok()
        && XInput2::open().is_ok()
        && xkbcommon_dl::x11::xkbcommon_x11_option().is_some()
}

/// The bridge's message handler (`window.webkit.messageHandlers.openmaker`).
const HANDLER: &str = "openmaker";

/// How long the app waits, once, for GTK to start.
const START_TIMEOUT: Duration = Duration::from_secs(10);

impl GtkBackend {
    /// Start GTK on its thread; `Err` when there is no X11 display for it.
    pub fn start(ctx: &egui::Context) -> Result<Self, Unavailable> {
        let (commands, command_rx) = channel();
        let (event_tx, events) = channel();
        let (ready_tx, ready_rx) = channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("openmaker-web-viewers".into())
            .spawn(move || run(ctx, command_rx, event_tx, ready_tx))
            .map_err(|_| Unavailable::Platform)?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(true) => Ok(GtkBackend { commands, events }),
            _ => Err(Unavailable::Platform),
        }
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            glib::idle_add_once(drain);
        }
    }
}

impl Backend for GtkBackend {
    fn create(&mut self, view: u64, parent: NativeParent, spec: ViewSpec) {
        self.send(Command::Create { view, parent, spec });
    }

    fn place(&mut self, view: u64, geometry: Geometry, visible: bool) {
        self.send(Command::Place {
            view,
            geometry,
            visible,
        });
    }

    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation) {
        self.send(Command::Navigate {
            view,
            generation,
            navigation,
        });
    }

    fn evaluate(&mut self, view: u64, call: u64, script: String) {
        self.send(Command::Evaluate { view, call, script });
    }

    fn destroy(&mut self, view: u64) {
        self.send(Command::Destroy { view });
    }

    fn release_focus(&mut self, parent: NativeParent) {
        self.send(Command::ReleaseFocus { parent });
    }

    fn poll(&mut self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

/// The GTK thread's state.
struct State {
    commands: Receiver<Command>,
    events: Events,
    display: gdkx11::X11Display,
    x11: Rc<RustConnection>,
    views: HashMap<u64, Hosted>,
    /// Child windows of destroyed views, removed once GTK has let go of
    /// them (`Retired`).
    retired: Vec<Retired>,
}

/// A destroyed view's X11 child window. GTK closes its window in a later
/// main loop iteration, and GDK still unmaps and releases the window then
/// (an X error on its connection would abort the app), so the window is
/// destroyed only after the GTK window's `destroy` and a GDK round trip.
struct Retired {
    window: u32,
    released: Rc<Cell<bool>>,
    since: std::time::Instant,
}

/// How often retired windows are looked at.
const RETIRE_INTERVAL: Duration = Duration::from_millis(100);
/// How long a retired window waits for GTK before it goes all the same
/// (GDK then sees it destroyed and leaves it alone).
const RETIRE_LIMIT: Duration = Duration::from_secs(2);

/// Where events go: the app's channel, with a repaint so it reads them.
#[derive(Clone)]
struct Events {
    sender: Sender<Event>,
    ctx: egui::Context,
}

impl Events {
    fn send(&self, event: Event) {
        if self.sender.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

struct Hosted {
    /// The X11 child window of the app's window.
    window: u32,
    gtk_window: gtk::Window,
    fixed: gtk::Fixed,
    webview: WebView,
    generation: Rc<Cell<u64>>,
    active_generation: Rc<Cell<u64>>,
    shown: bool,
    zoom: f64,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn run(
    ctx: egui::Context,
    commands: Receiver<Command>,
    sender: Sender<Event>,
    ready: Sender<bool>,
) {
    // The views are children of X11 windows: GDK must use X11 too, also in
    // a Wayland session where the app's windows run through XWayland.
    gtk::gdk::set_allowed_backends("x11");
    if gtk::init().is_err() {
        let _ = ready.send(false);
        return;
    }
    let display =
        gtk::gdk::Display::default().and_then(|d| d.downcast::<gdkx11::X11Display>().ok());
    let (Some(display), Ok((x11, _))) = (display, x11rb::connect(None)) else {
        let _ = ready.send(false);
        return;
    };
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            commands,
            events: Events { sender, ctx },
            display,
            x11: Rc::new(x11),
            views: HashMap::new(),
            retired: Vec::new(),
        });
    });
    let _ = ready.send(true);
    gtk::main();
}

/// Run the commands that came in (an idle callback on the GTK thread).
fn drain() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        while let Ok(command) = state.commands.try_recv() {
            state.apply(command);
        }
        let _ = state.x11.flush();
    });
}

/// Destroy the child windows GTK has let go of; look again later for the
/// others.
fn retire() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        let due = |r: &Retired| r.released.get() || r.since.elapsed() >= RETIRE_LIMIT;
        if !state.retired.iter().any(due) {
            glib::timeout_add_local_once(RETIRE_INTERVAL, retire);
            return;
        }
        // Whatever GDK sent about these windows reaches the server first.
        state.display.upcast_ref::<gtk::gdk::Display>().sync();
        let (done, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.retired)
            .into_iter()
            .partition(due);
        for retired in done {
            let _ = state.x11.destroy_window(retired.window);
        }
        let _ = state.x11.flush();
        state.retired = waiting;
        if !state.retired.is_empty() {
            glib::timeout_add_local_once(RETIRE_INTERVAL, retire);
        }
    });
}

/// The URLs a page may go to.
fn allowed(url: &str) -> bool {
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        scheme.as_str(),
        "http" | "https" | "data" | "blob" | "file" | "about"
    )
}

impl State {
    fn apply(&mut self, command: Command) {
        match command {
            Command::Create { view, parent, spec } => {
                if let Err(reason) = self.create(view, parent, spec) {
                    self.events.send(Event::Failed {
                        view,
                        generation: 0,
                        reason,
                    });
                }
            }
            Command::Place {
                view,
                geometry,
                visible,
            } => {
                if let Some(hosted) = self.views.get_mut(&view) {
                    place(&self.x11, hosted, geometry, visible);
                }
            }
            Command::Navigate {
                view,
                generation,
                navigation,
            } => {
                if let Some(hosted) = self.views.get(&view) {
                    hosted.generation.set(generation);
                    let webview = &hosted.webview;
                    let page = webview.webview();
                    let unchanged = matches!(&navigation, Navigation::Back) && !page.can_go_back()
                        || matches!(&navigation, Navigation::Forward) && !page.can_go_forward();
                    if unchanged {
                        // WebKit emits no load signal for a history no-op.
                        // Adopt an in-flight load too, so its completion still
                        // belongs to the registry's latest generation.
                        hosted.active_generation.set(generation);
                        self.events.send(if page.is_loading() {
                            Event::LoadStarted { view, generation }
                        } else {
                            Event::Loaded { view, generation }
                        });
                        return;
                    }
                    let result = match navigation {
                        Navigation::Load(url) => webview.load_url(&url),
                        Navigation::Reload => webview.reload(),
                        Navigation::Back => webview.go_back(),
                        Navigation::Forward => webview.go_forward(),
                    };
                    if let Err(error) = result {
                        self.events.send(Event::Failed {
                            view,
                            generation,
                            reason: error.to_string(),
                        });
                    }
                }
            }
            Command::Evaluate { view, call, script } => {
                let Some(hosted) = self.views.get(&view) else {
                    return;
                };
                let events = self.events.clone();
                let result = hosted
                    .webview
                    .evaluate_script_with_callback(&script, move |result| {
                        events.send(Event::Evaluated { view, call, result });
                    });
                if result.is_err() {
                    self.events.send(Event::Evaluated {
                        view,
                        call,
                        result: String::new(),
                    });
                }
            }
            Command::ReleaseFocus { parent } => self.release_focus(parent),
            Command::Destroy { view } => {
                if let Some(hosted) = self.views.remove(&view) {
                    let Hosted {
                        window,
                        gtk_window,
                        webview,
                        ..
                    } = hosted;
                    drop(webview);
                    let released = Rc::new(Cell::new(false));
                    let flag = Rc::clone(&released);
                    gtk_window.connect_destroy(move |_| flag.set(true));
                    gtk_window.hide();
                    gtk_window.close();
                    if self.retired.is_empty() {
                        glib::timeout_add_local_once(RETIRE_INTERVAL, retire);
                    }
                    self.retired.push(Retired {
                        window,
                        released,
                        since: std::time::Instant::now(),
                    });
                }
            }
        }
    }

    /// Give the X11 input focus back to `parent` when one of the views (or
    /// a window inside one) has it.
    fn release_focus(&self, parent: NativeParent) {
        let NativeParent::X11(parent) = parent;
        let Ok(parent) = u32::try_from(parent) else {
            return;
        };
        let Some(mut window) = self
            .x11
            .get_input_focus()
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.focus)
        else {
            return;
        };
        // Up from the focus window to the root, at most a few levels.
        for _ in 0..8 {
            if self.views.values().any(|v| v.window == window) {
                let _ = self
                    .x11
                    .set_input_focus(InputFocus::PARENT, parent, x11rb::CURRENT_TIME);
                return;
            }
            let Some(tree) = self
                .x11
                .query_tree(window)
                .ok()
                .and_then(|c| c.reply().ok())
            else {
                return;
            };
            if tree.parent == x11rb::NONE || tree.parent == tree.root {
                return;
            }
            window = tree.parent;
        }
    }

    fn create(&mut self, view: u64, parent: NativeParent, spec: ViewSpec) -> Result<(), String> {
        let NativeParent::X11(parent) = parent;
        let parent = u32::try_from(parent).map_err(|_| "not an X11 window id".to_owned())?;
        let x11 = &self.x11;
        let [x, y, width, height] = spec.geometry.frame;
        let window = x11.generate_id().map_err(|e| e.to_string())?;
        x11.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            clamp_i16(x),
            clamp_i16(y),
            clamp_u16(width),
            clamp_u16(height),
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0x00ff_ffff),
        )
        .map_err(|e| e.to_string())?;
        // GDK must find the window on the server before it adopts it.
        x11.sync().map_err(|e| e.to_string())?;

        let foreign: gtk::gdk::Window =
            gdkx11::X11Window::foreign_new_for_display(&self.display, window.into()).upcast();
        let gtk_window = gtk::Window::new(gtk::WindowType::Toplevel);
        // Registered, the window's events reach the GTK window, and closing
        // it destroys the GTK window, which lets go of the X11 window.
        gtk_window.connect_realize(move |w| {
            w.set_window(foreign.clone());
            w.register_window(&foreign);
        });
        gtk_window.set_has_window(true);
        gtk_window.realize();
        let fixed = gtk::Fixed::new();
        gtk_window.add(&fixed);

        let [cx, cy, cw, ch] = spec.geometry.content;
        let loads = self.events.clone();
        let messages = self.events.clone();
        let mut builder = WebViewBuilder::new()
            .with_url(spec.url)
            .with_incognito(true)
            .with_devtools(false)
            .with_bounds(wry::Rect {
                position: PhysicalPosition::new(cx, cy).into(),
                size: PhysicalSize::new(cw.max(1) as u32, ch.max(1) as u32).into(),
            })
            .with_navigation_handler(|url| allowed(&url))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false);
        if spec.scripts {
            builder = builder.with_initialization_script(BRIDGE);
        }
        let webview = builder.build_gtk(&fixed).map_err(|e| e.to_string())?;
        let page = webview.webview();
        let generation = Rc::new(Cell::new(0));
        let requested = Rc::clone(&generation);
        let active_generation = Rc::new(Cell::new(0));
        let active = Rc::clone(&active_generation);
        // wry's Started maps to Committed on GTK. Use WebKit's real
        // provisional-load signal, and tag completions with the generation
        // that started that load rather than a later queued navigation.
        page.connect_load_changed(move |page, event| match event {
            webkit2gtk::LoadEvent::Started => {
                active.set(requested.get());
                loads.send(Event::LoadStarted {
                    view,
                    generation: active.get(),
                });
            }
            webkit2gtk::LoadEvent::Finished if !page.is_loading() => {
                loads.send(Event::Loaded {
                    view,
                    generation: active.get(),
                });
            }
            _ => {}
        });
        if spec.scripts {
            let manager = page
                .user_content_manager()
                .ok_or_else(|| "the page has no content manager".to_owned())?;
            manager.connect_script_message_received(Some(HANDLER), move |_, message| {
                if let Some(value) = message.js_value() {
                    messages.send(Event::Message {
                        view,
                        body: value.to_str().into(),
                    });
                }
            });
            if !manager.register_script_message_handler(HANDLER) {
                return Err("the page's message handler could not be registered".into());
            }
        }
        // "Allow interaction with web viewer content" off: the page takes
        // no pointer or keyboard input.
        page.set_sensitive(spec.interactive);
        page.set_can_focus(spec.interactive);
        if spec.interactive {
            // Without a window manager to give it the focus, a click in the
            // page puts the X11 input focus in its window, so keys reach it.
            let x11 = Rc::clone(&self.x11);
            page.connect_button_press_event(move |_, _| {
                let _ = x11.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME);
                let _ = x11.flush();
                glib::Propagation::Proceed
            });
        }
        let mut hosted = Hosted {
            window,
            gtk_window,
            fixed,
            webview,
            generation,
            active_generation,
            shown: false,
            zoom: 1.0,
        };
        place(&self.x11, &mut hosted, spec.geometry, spec.visible);
        self.views.insert(view, hosted);
        Ok(())
    }
}

/// Move and size the child window to the visible part, the page within it
/// to its offset, and show or hide them.
fn place(x11: &RustConnection, hosted: &mut Hosted, geometry: Geometry, visible: bool) {
    let [x, y, width, height] = geometry.frame;
    let shown = visible && width > 0 && height > 0;
    if shown {
        let _ = x11.configure_window(
            hosted.window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(width.max(1) as u32)
                .height(height.max(1) as u32),
        );
        let scale = hosted.gtk_window.scale_factor().max(1);
        let [cx, cy, cw, ch] = geometry.content;
        hosted
            .gtk_window
            .size_allocate(&gtk::Allocation::new(0, 0, width / scale, height / scale));
        let page = hosted.webview.webview();
        page.set_size_request((cw / scale).max(1), (ch / scale).max(1));
        hosted.fixed.move_(&page, cx / scale, cy / scale);
        if (geometry.zoom - hosted.zoom).abs() > f64::EPSILON {
            let _ = hosted.webview.zoom(geometry.zoom);
            hosted.zoom = geometry.zoom;
        }
    }
    if shown != hosted.shown {
        if shown {
            let _ = x11.map_window(hosted.window);
            hosted.gtk_window.show_all();
        } else {
            hosted.gtk_window.hide();
            let _ = x11.unmap_window(hosted.window);
        }
        hosted.shown = shown;
    }
}

fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn clamp_u16(v: i32) -> u16 {
    v.clamp(1, i32::from(u16::MAX)) as u16
}
