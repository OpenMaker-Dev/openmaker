//! Web viewers in native Wayland windows (`OPENMAKER_WEB_VIEWER_BACKEND=wayland`):
//! WebKitGTK draws the pages in the page host, a process of its own
//! (`host.rs`) whose only display is one of the app's (`display.rs`), and
//! Browse paints each view's newest frame as a texture inside its object,
//! in the object's place among the layout's objects, so that whatever is
//! drawn later (objects above it, menus, popovers, dialogs) covers it. The
//! pointer's input over the object goes back to the page through that
//! display. The views need no native window of their own, in any window.
//!
//! The host is found beside the program (`om-gui-webview`), or where
//! `OPENMAKER_WEB_VIEWER_HOST` says. Its stdout is read as it would be from
//! any other process: lines are bounded, events are checked as any page's
//! are (`WebViewers::poll`), and a host that ends fails its views.

mod display;
mod shm;

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};

use serde::{Deserialize, Serialize};

pub(crate) use display::TITLE;
use display::{PageDisplay, Request};

use super::{
    Backend, Event, Geometry, MessageBudget, NativeParent, Navigation, PageFrame, PageInput,
    Unavailable, ViewSpec, MAX_MESSAGE,
};

/// The variable naming another host program.
const HOST_VARIABLE: &str = "OPENMAKER_WEB_VIEWER_HOST";
/// The host program's name beside the app's.
const HOST_PROGRAM: &str = "om-gui-webview";

/// The longest line read from the host: a bridge message at its limit,
/// every character escaped.
const MAX_LINE: u64 = 6 * MAX_MESSAGE as u64 + 4096;

/// What the app asks of the host, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub(crate) enum HostCommand {
    /// Show `url` in a new view, `width` × `height` points.
    Create {
        view: u64,
        url: String,
        width: i32,
        height: i32,
        zoom: f64,
        interactive: bool,
        scripts: bool,
    },
    Resize {
        view: u64,
        width: i32,
        height: i32,
        zoom: f64,
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
}

pub(crate) struct WaylandBackend {
    display: PageDisplay,
    host: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Option<Event>>,
    frames: Receiver<display::Frame>,
    /// Each live view's size and zoom as the host has them, and its newest
    /// navigation's generation (a host that ends fails it with that one).
    views: HashMap<u64, ([i32; 2], f64, u64)>,
    scale: i32,
    /// The host ended: its views failed, and new ones fail too.
    gone: bool,
    /// Bridge messages read from the host that the app has not polled.
    budget: MessageBudget,
}

/// The host program to run, if there is one.
fn host_program() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(HOST_VARIABLE) {
        return Some(PathBuf::from(path));
    }
    let program = std::env::current_exe().ok()?.with_file_name(HOST_PROGRAM);
    program.is_file().then_some(program)
}

/// A size in points from one in pixels.
fn points(pixels: i32, geometry: &Geometry) -> i32 {
    (pixels as f32 / geometry.pixels_per_point.max(0.01)).round() as i32
}

impl WaylandBackend {
    /// Start the display and the host; `Err` when either cannot.
    pub fn start(ctx: &egui::Context) -> Result<Self, Unavailable> {
        let program = host_program().ok_or(Unavailable::Platform)?;
        let scale = ctx.pixels_per_point().ceil() as i32;
        let (frame_tx, frames) = channel();
        let wake = ctx.clone();
        let display = PageDisplay::start(frame_tx, Box::new(move || wake.request_repaint()), scale)
            .map_err(|error| {
                eprintln!("openmaker: web viewers: {error}");
                Unavailable::Platform
            })?;
        let mut host = Command::new(&program)
            .env("WAYLAND_DISPLAY", display.socket())
            .env("GDK_BACKEND", "wayland")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            // The display's scale is the app's; a desktop's own GDK scale
            // would draw the pages at another (#1086).
            .env_remove("GDK_SCALE")
            .env_remove("GDK_DPI_SCALE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| {
                eprintln!("openmaker: web viewers: {}: {error}", program.display());
                Unavailable::Platform
            })?;
        let stdin = host.stdin.take();
        let stdout = host.stdout.take().ok_or(Unavailable::Platform)?;
        let (event_tx, events) = channel();
        let wake = ctx.clone();
        let budget = MessageBudget::default();
        let admitted = budget.clone();
        std::thread::Builder::new()
            .name("openmaker-page-host".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    let read = (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line);
                    match read {
                        Ok(0) | Err(_) => break,
                        // A line too long to be an event: the host is not
                        // to be believed any more.
                        Ok(_) if line.last() != Some(&b'\n') => break,
                        Ok(_) => {
                            if let Ok(event) = serde_json::from_slice::<Event>(&line) {
                                // As the X11 backend bounds its pages'.
                                if let Event::Message { body, .. } = &event {
                                    if !admitted.admit(body) {
                                        continue;
                                    }
                                }
                                if event_tx.send(Some(event)).is_err() {
                                    return;
                                }
                                wake.request_repaint();
                            }
                        }
                    }
                }
                let _ = event_tx.send(None);
                wake.request_repaint();
            })
            .map_err(|_| Unavailable::Platform)?;
        Ok(WaylandBackend {
            display,
            host,
            stdin,
            events,
            frames,
            views: HashMap::new(),
            scale,
            gone: false,
            budget,
        })
    }

    fn send(&mut self, command: &HostCommand) {
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(command) else {
            return;
        };
        line.push(b'\n');
        if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
            self.stdin = None;
        }
    }

    /// Follow the scale the app draws at.
    fn follow_scale(&mut self, geometry: &Geometry) {
        let scale = geometry.pixels_per_point.ceil() as i32;
        if scale != self.scale && scale > 0 {
            self.scale = scale;
            self.display.send(Request::Scale(scale));
        }
    }
}

impl Drop for WaylandBackend {
    fn drop(&mut self) {
        // Closing stdin ends the host; it is waited for so as to leave no
        // zombie behind.
        self.stdin = None;
        let _ = self.host.wait();
    }
}

impl Backend for WaylandBackend {
    fn create(&mut self, view: u64, _parent: NativeParent, spec: ViewSpec) {
        if self.gone {
            self.events_failed(view, 0);
            return;
        }
        self.follow_scale(&spec.geometry);
        let size = [
            points(spec.geometry.content[2], &spec.geometry),
            points(spec.geometry.content[3], &spec.geometry),
        ];
        self.views.insert(view, (size, spec.geometry.zoom, 0));
        self.send(&HostCommand::Create {
            view,
            url: spec.url,
            width: size[0],
            height: size[1],
            zoom: spec.geometry.zoom,
            interactive: spec.interactive,
            scripts: spec.scripts,
        });
    }

    fn place(&mut self, view: u64, geometry: Geometry, _visible: bool) {
        self.follow_scale(&geometry);
        let size = [
            points(geometry.content[2], &geometry),
            points(geometry.content[3], &geometry),
        ];
        let Some(placed) = self.views.get_mut(&view) else {
            return;
        };
        if placed.0 != size || (placed.1 - geometry.zoom).abs() > f64::EPSILON {
            placed.0 = size;
            placed.1 = geometry.zoom;
            self.send(&HostCommand::Resize {
                view,
                width: size[0],
                height: size[1],
                zoom: geometry.zoom,
            });
        }
    }

    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation) {
        if let Some(placed) = self.views.get_mut(&view) {
            placed.2 = generation;
        }
        self.send(&HostCommand::Navigate {
            view,
            generation,
            navigation,
        });
    }

    fn evaluate(&mut self, view: u64, call: u64, script: String) {
        self.send(&HostCommand::Evaluate { view, call, script });
    }

    fn destroy(&mut self, view: u64) {
        self.views.remove(&view);
        self.send(&HostCommand::Destroy { view });
    }

    fn release_focus(&mut self, _parent: NativeParent) {
        self.display.send(Request::Focus(None));
    }

    fn input(&mut self, view: u64, input: PageInput) {
        if matches!(input, PageInput::Button { pressed: true, .. }) {
            self.display.send(Request::Focus(Some(view)));
        }
        self.display.send(Request::Pointer { view, input });
    }

    fn frames(&mut self) -> Vec<PageFrame> {
        self.frames
            .try_iter()
            .map(|f| PageFrame {
                view: f.view,
                size: [f.width, f.height],
                rgba: f.rgba,
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for event in self.events.try_iter() {
            match event {
                Some(event) => {
                    if let Event::Message { body, .. } = &event {
                        self.budget.release(body);
                    }
                    events.push(event);
                }
                None => self.gone = true,
            }
        }
        if self.gone && !self.views.is_empty() {
            for (view, (_, _, generation)) in std::mem::take(&mut self.views) {
                events.push(Event::Failed {
                    view,
                    generation,
                    reason: "the page host ended".into(),
                });
            }
        }
        events
    }
}

impl WaylandBackend {
    fn events_failed(&mut self, view: u64, generation: u64) {
        self.views.insert(view, ([0, 0], 1.0, generation));
    }
}
