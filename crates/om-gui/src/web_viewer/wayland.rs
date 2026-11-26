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
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) use display::TITLE;
use display::{Frames, PageDisplay, Request};

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

/// How many commands may wait for the host, and events for the app: a host
/// that falls this far behind its commands is stopped, and one this far
/// ahead of the app waits.
const QUEUE: usize = 1024;
/// The longest text kept of an event other than a bridge message.
const MAX_TEXT: usize = 4096;
/// How long a host whose stdin closed gets to end before it is killed.
const END_TIMEOUT: Duration = Duration::from_secs(2);

/// A live view as the host has it.
struct Hosted {
    /// Its size in points and its zoom.
    size: [i32; 2],
    zoom: f64,
    /// Its newest navigation's generation (a host that ends fails it with
    /// that one), and the scale its window draws at.
    generation: u64,
    scale: i32,
}

pub(crate) struct WaylandBackend {
    display: PageDisplay,
    /// Lines for the host's stdin, written by a thread of their own, so
    /// that a host that stops reading never holds up the app.
    commands: Option<SyncSender<Vec<u8>>>,
    host: Option<Child>,
    events: Receiver<Option<Event>>,
    /// Each view's newest frame, which the display replaces.
    frames: Frames,
    views: HashMap<u64, Hosted>,
    /// Events the backend makes itself, for commands a host that ended
    /// cannot carry out.
    pending: Vec<Event>,
    /// The scale the display's output has, and the largest texture side.
    scale: i32,
    max_side: i32,
    /// The host ended, or was stopped: its views failed, and new ones fail.
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

/// An event with its texts cut to what the app keeps: a function's result
/// and a failure's reason are short when the host is honest (bridge
/// messages are bounded by the budget instead).
fn bounded(event: Event) -> Event {
    let cut = |mut text: String| {
        if text.len() > MAX_TEXT {
            let mut end = MAX_TEXT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        text
    };
    match event {
        Event::Evaluated { view, call, result } => Event::Evaluated {
            view,
            call,
            result: cut(result),
        },
        Event::Failed {
            view,
            generation,
            reason,
        } => Event::Failed {
            view,
            generation,
            reason: cut(reason),
        },
        event => event,
    }
}

impl WaylandBackend {
    /// Start the display and the host; `Err` when either cannot.
    pub fn start(ctx: &egui::Context) -> Result<Self, Unavailable> {
        let program = host_program().ok_or(Unavailable::Platform)?;
        let scale = ctx.pixels_per_point().ceil() as i32;
        let max_side = i32::try_from(ctx.input(|i| i.max_texture_side)).unwrap_or(i32::MAX);
        let frames = Frames::default();
        let wake = ctx.clone();
        let display = PageDisplay::start(
            Arc::clone(&frames),
            Box::new(move || wake.request_repaint()),
            scale,
        )
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
        let (Some(mut stdin), Some(stdout)) = (host.stdin.take(), host.stdout.take()) else {
            reap(host);
            return Err(Unavailable::Platform);
        };
        let (commands, lines) = sync_channel::<Vec<u8>>(QUEUE);
        let writer = std::thread::Builder::new()
            .name("openmaker-page-commands".into())
            .spawn(move || {
                for line in lines {
                    if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
                        break;
                    }
                }
                // Dropping stdin ends the host.
            });
        let (event_tx, events) = sync_channel(QUEUE);
        let wake = ctx.clone();
        let budget = MessageBudget::default();
        let admitted = budget.clone();
        let reader = std::thread::Builder::new()
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
                            let Ok(event) = serde_json::from_slice::<Event>(&line) else {
                                continue;
                            };
                            // As the X11 backend bounds its pages'.
                            if let Event::Message { body, .. } = &event {
                                if !admitted.admit(body) {
                                    continue;
                                }
                            }
                            if event_tx.send(Some(bounded(event))).is_err() {
                                return;
                            }
                            wake.request_repaint();
                        }
                    }
                }
                let _ = event_tx.send(None);
                wake.request_repaint();
            });
        if writer.is_err() || reader.is_err() {
            reap(host);
            return Err(Unavailable::Platform);
        }
        Ok(WaylandBackend {
            display,
            commands: Some(commands),
            host: Some(host),
            events,
            frames,
            views: HashMap::new(),
            pending: Vec::new(),
            scale,
            max_side,
            gone: false,
            budget,
        })
    }

    fn send(&mut self, command: &HostCommand) {
        let Some(commands) = self.commands.as_ref() else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(command) else {
            return;
        };
        line.push(b'\n');
        match commands.try_send(line) {
            Ok(()) => {}
            // A host this far behind is not taking its commands: it goes,
            // and its views fail (`poll`).
            Err(TrySendError::Full(_)) => self.stop(),
            Err(TrySendError::Disconnected(_)) => self.commands = None,
        }
    }

    /// End the host (its stdin closes; it is killed if it does not end).
    fn stop(&mut self) {
        self.commands = None;
        self.gone = true;
        if let Some(host) = self.host.take() {
            reap(host);
        }
    }

    /// The size the host makes a view `geometry` places: its content in
    /// points, no more than a texture can hold at the display's scale.
    fn size(&self, geometry: &Geometry) -> [i32; 2] {
        let limit = (self.max_side / self.scale.max(1)).max(1);
        [
            points(geometry.content[2], geometry).clamp(1, limit),
            points(geometry.content[3], geometry).clamp(1, limit),
        ]
    }

    /// Draw at the largest scale any view's window needs: one window at 2
    /// and another at 1 both get sharp pages.
    fn follow_scale(&mut self) {
        let scale = self
            .views
            .values()
            .map(|v| v.scale)
            .max()
            .unwrap_or(1)
            .max(1);
        if scale != self.scale {
            self.scale = scale;
            self.display.send(Request::Scale(scale));
        }
    }
}

/// Wait for `host` to end, on a thread of its own, killing it after
/// [`END_TIMEOUT`]: no zombie is left, and the app never waits.
fn reap(mut host: Child) {
    let _ = std::thread::Builder::new()
        .name("openmaker-page-host-end".into())
        .spawn(move || {
            let until = Instant::now() + END_TIMEOUT;
            while Instant::now() < until {
                if !matches!(host.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = host.kill();
            let _ = host.wait();
        });
}

impl Drop for WaylandBackend {
    fn drop(&mut self) {
        // Closing the commands closes the host's stdin, which ends it.
        self.stop();
    }
}

impl Backend for WaylandBackend {
    fn create(&mut self, view: u64, _parent: NativeParent, spec: ViewSpec) {
        if self.gone {
            self.pending.push(Event::Failed {
                view,
                generation: 0,
                reason: "the page host ended".into(),
            });
            return;
        }
        let scale = spec.geometry.pixels_per_point.ceil() as i32;
        self.views.insert(
            view,
            Hosted {
                size: [1, 1],
                zoom: spec.geometry.zoom,
                generation: 0,
                scale,
            },
        );
        self.follow_scale();
        let size = self.size(&spec.geometry);
        if let Some(hosted) = self.views.get_mut(&view) {
            hosted.size = size;
        }
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
        let scale = geometry.pixels_per_point.ceil() as i32;
        let Some(hosted) = self.views.get_mut(&view) else {
            return;
        };
        hosted.scale = scale;
        self.follow_scale();
        let size = self.size(&geometry);
        let Some(hosted) = self.views.get_mut(&view) else {
            return;
        };
        if hosted.size != size || (hosted.zoom - geometry.zoom).abs() > f64::EPSILON {
            hosted.size = size;
            hosted.zoom = geometry.zoom;
            self.send(&HostCommand::Resize {
                view,
                width: size[0],
                height: size[1],
                zoom: geometry.zoom,
            });
        }
    }

    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation) {
        if self.gone {
            self.pending.push(Event::Failed {
                view,
                generation,
                reason: "the page host ended".into(),
            });
            return;
        }
        if let Some(hosted) = self.views.get_mut(&view) {
            hosted.generation = generation;
        }
        self.send(&HostCommand::Navigate {
            view,
            generation,
            navigation,
        });
    }

    fn evaluate(&mut self, view: u64, call: u64, script: String) {
        if self.gone {
            // No result: the call fails.
            self.pending.push(Event::Evaluated {
                view,
                call,
                result: String::new(),
            });
            return;
        }
        self.send(&HostCommand::Evaluate { view, call, script });
    }

    fn destroy(&mut self, view: u64) {
        self.views.remove(&view);
        if let Ok(mut frames) = self.frames.lock() {
            frames.remove(&view);
        }
        self.follow_scale();
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
        let Ok(mut frames) = self.frames.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *frames)
            .into_values()
            .map(|f| PageFrame {
                view: f.view,
                size: [f.width, f.height],
                rgba: f.rgba,
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.pending);
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
            for (view, hosted) in std::mem::take(&mut self.views) {
                events.push(Event::Failed {
                    view,
                    generation: hosted.generation,
                    reason: "the page host ended".into(),
                });
            }
        }
        events
    }
}
