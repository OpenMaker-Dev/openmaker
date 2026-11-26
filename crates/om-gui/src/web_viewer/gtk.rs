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
//!
//! The view itself, with the page bridge, is `webkit.rs`'s, which the
//! Wayland backend's page host makes too.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use wry::WebViewExtUnix as _;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, InputFocus, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::webkit::{self, Page};
use super::{
    Backend, Event, Geometry, MessageBudget, NativeParent, Navigation, Unavailable, ViewSpec,
};

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
    /// Bridge messages the GTK thread sent and the app has not polled.
    budget: MessageBudget,
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

/// How long the app waits, once, for GTK to start.
const START_TIMEOUT: Duration = Duration::from_secs(10);

impl GtkBackend {
    /// Start GTK on its thread; `Err` when there is no X11 display for it.
    pub fn start(ctx: &egui::Context) -> Result<Self, Unavailable> {
        let (commands, command_rx) = channel();
        let (event_tx, events) = channel();
        let (ready_tx, ready_rx) = channel();
        let ctx = ctx.clone();
        let budget = MessageBudget::default();
        let taken = budget.clone();
        std::thread::Builder::new()
            .name("openmaker-web-viewers".into())
            .spawn(move || run(ctx, command_rx, event_tx, taken, ready_tx))
            .map_err(|_| Unavailable::Platform)?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(true) => Ok(GtkBackend {
                commands,
                events,
                budget,
            }),
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
        let events: Vec<Event> = self.events.try_iter().collect();
        for event in &events {
            if let Event::Message { body, .. } = event {
                self.budget.release(body);
            }
        }
        events
    }
}

/// The GTK thread's state.
struct State {
    commands: Receiver<Command>,
    events: Events,
    budget: MessageBudget,
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

    /// These events as a view's sink.
    fn sink(&self) -> webkit::Events {
        let events = self.clone();
        std::sync::Arc::new(move |event| events.send(event))
    }
}

struct Hosted {
    /// The X11 child window of the app's window.
    window: u32,
    gtk_window: gtk::Window,
    fixed: gtk::Fixed,
    page: Page,
    shown: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn run(
    ctx: egui::Context,
    commands: Receiver<Command>,
    sender: Sender<Event>,
    budget: MessageBudget,
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
            budget,
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
                    hosted
                        .page
                        .navigate(view, generation, navigation, &self.events.sink());
                }
            }
            Command::Evaluate { view, call, script } => {
                if let Some(hosted) = self.views.get(&view) {
                    hosted
                        .page
                        .evaluate(view, call, &script, &self.events.sink());
                }
            }
            Command::ReleaseFocus { parent } => self.release_focus(parent),
            Command::Destroy { view } => {
                if let Some(hosted) = self.views.remove(&view) {
                    let Hosted {
                        window,
                        gtk_window,
                        page,
                        ..
                    } = hosted;
                    drop(page);
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
        let NativeParent::X11(parent) = parent else {
            return;
        };
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
        let NativeParent::X11(parent) = parent else {
            return Err("not an X11 window".into());
        };
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

        let page = Page::new(
            &fixed,
            webkit::Spec {
                view,
                url: spec.url,
                bounds: spec.geometry.content,
                interactive: spec.interactive,
                scripts: spec.scripts,
                budget: Some(self.budget.clone()),
            },
            &self.events.sink(),
        )?;
        if spec.interactive {
            // Without a window manager to give it the focus, a click in the
            // page puts the X11 input focus in its window, so keys reach it.
            let x11 = Rc::clone(&self.x11);
            page.webview
                .webview()
                .connect_button_press_event(move |_, _| {
                    let _ = x11.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME);
                    let _ = x11.flush();
                    glib::Propagation::Proceed
                });
        }
        let mut hosted = Hosted {
            window,
            gtk_window,
            fixed,
            page,
            shown: false,
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
        let page = hosted.page.webview.webview();
        page.set_size_request((cw / scale).max(1), (ch / scale).max(1));
        hosted.fixed.move_(&page, cx / scale, cy / scale);
        hosted.page.zoom(geometry.zoom);
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
