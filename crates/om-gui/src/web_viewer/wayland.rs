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
mod keymap;
mod shm;
mod transfer;

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) use display::TITLE;
use display::{Carets, Copied, FrameSlots, Frames, Outbox, PageDisplay, Request};

use super::{
    Backend, Event, Geometry, MessageBudget, NativeParent, Navigation, PageFrame, PageInput,
    PagePopup, Unavailable, ViewSpec, MAX_MESSAGE,
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
/// The longest command line the host takes, its newline included: a page's
/// address or a function's parameters longer than that fail instead.
pub(crate) const MAX_COMMAND: usize = 16 * 1024 * 1024;

/// `command` as its line, or `None` when that would be longer than
/// [`MAX_COMMAND`]: it is written into a buffer that refuses to grow past
/// the cap, so an oversized command is never held whole.
fn encode(command: &HostCommand) -> Option<Vec<u8>> {
    struct Capped(Vec<u8>);
    impl std::io::Write for Capped {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + data.len() >= MAX_COMMAND {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            self.0.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut line = Capped(Vec::new());
    serde_json::to_writer(&mut line, command).ok()?;
    line.0.push(b'\n');
    Some(line.0)
}

/// The most bytes of commands that may wait for the host (a page's address
/// or a function's parameters can be long): a host this far behind is
/// stopped too.
const COMMAND_BUDGET: usize = 64 * 1024 * 1024;
/// How long a host whose stdin closed gets to end before it is killed.
const END_TIMEOUT: Duration = Duration::from_secs(2);

/// A live view as the host has it.
struct Hosted {
    /// The size its object asks for, in points, and the size the host has
    /// (no more than a texture holds at the display's scale), and its zoom.
    wanted: [i32; 2],
    size: [i32; 2],
    zoom: f64,
    /// Its newest navigation's generation (a host that ends fails it with
    /// that one), and the scale its window draws at.
    generation: u64,
    scale: i32,
    /// Whether the host was sent its Create: one whose first address was
    /// too long to send is made, empty, by its next load.
    created: bool,
    interactive: bool,
    scripts: bool,
}

/// Why a command was not sent.
enum Unsent {
    /// Its line would pass [`MAX_COMMAND`]; the host is fine.
    TooLong,
    /// The host ended, or was stopped; its views fail (`poll`).
    HostGone,
}

pub(crate) struct WaylandBackend {
    display: PageDisplay,
    /// Lines for the host's stdin, written by a thread of their own, so
    /// that a host that stops reading never holds up the app.
    commands: Option<SyncSender<Vec<u8>>>,
    /// The bytes of the commands not yet written.
    in_flight: Arc<AtomicUsize>,
    host: Option<Child>,
    events: Receiver<Option<Event>>,
    /// Each view's newest frame, which the display replaces, and its
    /// text caret.
    frames: Frames,
    carets: Carets,
    /// What a page last copied, for the app's clipboard.
    copied: Copied,
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
            text.shrink_to_fit();
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
        let frames = Arc::new(Mutex::new(FrameSlots {
            frames: HashMap::new(),
            popups: HashMap::new(),
            max_side: usize::try_from(max_side).unwrap_or(usize::MAX),
        }));
        let carets = Carets::default();
        let copied = Copied::default();
        let wake = ctx.clone();
        let outbox = Outbox {
            frames: Arc::clone(&frames),
            carets: Arc::clone(&carets),
            copied: Arc::clone(&copied),
            wake: Arc::new(move || wake.request_repaint()),
        };
        let display = PageDisplay::start(outbox, scale).map_err(|error| {
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
        // Keys mean in a page what they mean in the app.
        keymap::follow(display.sender());
        let (Some(mut stdin), Some(stdout)) = (host.stdin.take(), host.stdout.take()) else {
            reap(host);
            return Err(Unavailable::Platform);
        };
        let (commands, lines) = sync_channel::<Vec<u8>>(QUEUE);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let written = Arc::clone(&in_flight);
        let writer = std::thread::Builder::new()
            .name("openmaker-page-commands".into())
            .spawn(move || {
                for line in lines {
                    if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
                        break;
                    }
                    written.fetch_sub(line.len(), Ordering::SeqCst);
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
            in_flight,
            host: Some(host),
            events,
            frames,
            carets,
            copied,
            views: HashMap::new(),
            pending: Vec::new(),
            scale,
            max_side,
            gone: false,
            budget,
        })
    }

    /// Send `command` to the host.
    fn send(&mut self, command: &HostCommand) -> Result<(), Unsent> {
        let line = encode(command).ok_or(Unsent::TooLong)?;
        let Some(commands) = self.commands.as_ref() else {
            return Err(Unsent::HostGone);
        };
        // A host this far behind is not taking its commands.
        let length = line.len();
        if self.in_flight.fetch_add(length, Ordering::SeqCst) + length > COMMAND_BUDGET {
            self.stop();
            return Err(Unsent::HostGone);
        }
        match commands.try_send(line) {
            Ok(()) => Ok(()),
            // A host this far behind is not taking its commands, and one
            // whose writer stopped is not reading them: it goes, and its
            // views fail (`poll`).
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.stop();
                Err(Unsent::HostGone)
            }
        }
    }

    /// Whether the host still runs: one that ended is gone, and its views
    /// fail, before anything is asked of it.
    fn alive(&mut self) -> bool {
        if !self.gone && !matches!(self.host.as_mut().map(Child::try_wait), Some(Ok(None))) {
            self.stop();
        }
        !self.gone
    }

    /// End the host (its stdin closes; it is killed if it does not end).
    fn stop(&mut self) {
        self.commands = None;
        self.gone = true;
        if let Some(host) = self.host.take() {
            reap(host);
        }
    }

    /// The size the host gives a view whose object asks for `wanted`
    /// points: no more than a texture can hold at the display's scale.
    fn fitted(&self, wanted: [i32; 2]) -> [i32; 2] {
        let limit = (self.max_side / self.scale.max(1)).max(1);
        wanted.map(|side| side.clamp(1, limit))
    }

    /// Draw at the largest scale any view's window needs: one window at 2
    /// and another at 1 both get sharp pages. Views the new scale fits
    /// otherwise are resized.
    fn follow_scale(&mut self) {
        let scale = self
            .views
            .values()
            .map(|v| v.scale)
            .max()
            .unwrap_or(1)
            .max(1);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.display.send(Request::Scale(scale));
        let mut resized = Vec::new();
        for (view, hosted) in &self.views {
            let size = self.fitted(hosted.wanted);
            if size != hosted.size {
                resized.push((*view, size, hosted.zoom));
            }
        }
        for (view, size, zoom) in resized {
            if let Some(hosted) = self.views.get_mut(&view) {
                hosted.size = size;
            }
            let _ = self.send(&HostCommand::Resize {
                view,
                width: size[0],
                height: size[1],
                zoom,
            });
        }
    }
}

impl WaylandBackend {
    /// A load too long to send fails its view's navigation.
    fn refused(&mut self, view: u64, generation: u64) {
        self.pending.push(Event::Failed {
            view,
            generation,
            reason: "the page's address is too long".into(),
        });
    }

    /// A navigation not sent fails: a refused one here, one to a host that
    /// went with its views (`poll`).
    fn unsent(&mut self, view: u64, generation: u64, why: Unsent) -> bool {
        if let Unsent::TooLong = why {
            self.refused(view, generation);
        }
        false
    }

    /// Make the host's view for `view` if its Create was never sent, empty
    /// (its first address was too long), before a load: without it the
    /// host would ignore the view's commands.
    fn make(&mut self, view: u64) -> Result<(), Unsent> {
        let Some(hosted) = self.views.get(&view) else {
            return Err(Unsent::HostGone);
        };
        if hosted.created {
            return Ok(());
        }
        let create = HostCommand::Create {
            view,
            url: "about:blank".into(),
            width: hosted.size[0],
            height: hosted.size[1],
            zoom: hosted.zoom,
            interactive: hosted.interactive,
            scripts: hosted.scripts,
        };
        self.send(&create)?;
        if let Some(hosted) = self.views.get_mut(&view) {
            hosted.created = true;
        }
        Ok(())
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
        if !self.alive() {
            self.pending.push(Event::Failed {
                view,
                generation: 0,
                reason: "the page host ended".into(),
            });
            return;
        }
        let scale = spec.geometry.pixels_per_point.ceil() as i32;
        let wanted = [
            points(spec.geometry.content[2], &spec.geometry),
            points(spec.geometry.content[3], &spec.geometry),
        ];
        self.views.insert(
            view,
            Hosted {
                wanted,
                size: [1, 1],
                zoom: spec.geometry.zoom,
                generation: 0,
                scale,
                created: false,
                interactive: spec.interactive,
                scripts: spec.scripts,
            },
        );
        self.follow_scale();
        let size = self.fitted(wanted);
        if let Some(hosted) = self.views.get_mut(&view) {
            hosted.size = size;
        }
        let sent = self.send(&HostCommand::Create {
            view,
            url: spec.url,
            width: size[0],
            height: size[1],
            zoom: spec.geometry.zoom,
            interactive: spec.interactive,
            scripts: spec.scripts,
        });
        match sent {
            Ok(()) => {
                if let Some(hosted) = self.views.get_mut(&view) {
                    hosted.created = true;
                }
            }
            Err(why) => {
                self.unsent(view, 0, why);
            }
        }
    }

    fn place(&mut self, view: u64, geometry: Geometry, _visible: bool) {
        let scale = geometry.pixels_per_point.ceil() as i32;
        let Some(hosted) = self.views.get_mut(&view) else {
            return;
        };
        hosted.scale = scale;
        hosted.wanted = [
            points(geometry.content[2], &geometry),
            points(geometry.content[3], &geometry),
        ];
        let wanted = hosted.wanted;
        self.follow_scale();
        let size = self.fitted(wanted);
        let Some(hosted) = self.views.get_mut(&view) else {
            return;
        };
        if hosted.size != size || (hosted.zoom - geometry.zoom).abs() > f64::EPSILON {
            hosted.size = size;
            hosted.zoom = geometry.zoom;
            let _ = self.send(&HostCommand::Resize {
                view,
                width: size[0],
                height: size[1],
                zoom: geometry.zoom,
            });
        }
    }

    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation) -> bool {
        if !self.alive() {
            self.pending.push(Event::Failed {
                view,
                generation,
                reason: "the page host ended".into(),
            });
            return false;
        }
        if let Some(hosted) = self.views.get_mut(&view) {
            hosted.generation = generation;
        }
        let made = match &navigation {
            Navigation::Load(_) => self.make(view),
            // Reloading, or going back or forward in, a view the host never
            // had: its address is still the one too long to send.
            _ if self.views.get(&view).is_some_and(|h| !h.created) => Err(Unsent::TooLong),
            _ => Ok(()),
        };
        let sent = made.and_then(|()| {
            self.send(&HostCommand::Navigate {
                view,
                generation,
                navigation,
            })
        });
        match sent {
            Ok(()) => true,
            Err(why) => self.unsent(view, generation, why),
        }
    }

    fn evaluate(&mut self, view: u64, call: u64, script: String) -> bool {
        self.alive()
            && self
                .send(&HostCommand::Evaluate { view, call, script })
                .is_ok()
    }

    fn destroy(&mut self, view: u64) {
        self.views.remove(&view);
        if let Ok(mut slots) = self.frames.lock() {
            slots.frames.remove(&view);
        }
        if let Ok(mut carets) = self.carets.lock() {
            carets.remove(&view);
        }
        self.follow_scale();
        let _ = self.send(&HostCommand::Destroy { view });
    }

    fn release_focus(&mut self, _parent: NativeParent) {
        self.display.send(Request::Focus(None));
    }

    fn copied(&mut self) -> Option<String> {
        self.copied.lock().ok()?.take()
    }

    fn popups(&mut self) -> Vec<PagePopup> {
        let Ok(mut slots) = self.frames.lock() else {
            return Vec::new();
        };
        slots
            .popups
            .iter_mut()
            .map(|(id, slot)| PagePopup {
                id: *id,
                view: slot.view,
                at: slot.at,
                extent: slot.extent,
                frame: slot.frame.take().map(|f| PageFrame {
                    view: f.view,
                    size: [f.width, f.height],
                    rgba: f.rgba,
                }),
            })
            .collect()
    }

    fn popup_input(&mut self, popup: u32, input: PageInput) {
        self.display.send(Request::PopupPointer { popup, input });
    }

    fn input(&mut self, view: u64, input: PageInput) {
        if let PageInput::Paste(text) = input {
            self.display.send(Request::Paste { view, text });
            return;
        }
        if matches!(input, PageInput::Button { pressed: true, .. }) {
            self.display.send(Request::Focus(Some(view)));
        }
        self.display.send(Request::Pointer { view, input });
    }

    fn frames(&mut self) -> Vec<PageFrame> {
        let Ok(mut slots) = self.frames.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut slots.frames)
            .into_values()
            .map(|f| PageFrame {
                view: f.view,
                size: [f.width, f.height],
                rgba: f.rgba,
            })
            .collect()
    }

    fn available(&mut self) -> bool {
        self.alive()
    }

    fn caret(&self, view: u64) -> Option<egui::Rect> {
        let [x, y, width, height] = *self.carets.lock().ok()?.get(&view)?;
        Some(egui::Rect::from_min_size(
            egui::pos2(x as f32, y as f32),
            egui::vec2(width.max(1) as f32, height.max(1) as f32),
        ))
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

#[cfg(test)]
mod tests {
    use super::{encode, HostCommand, MAX_COMMAND};

    /// A command whose line would pass the cap is refused while it is
    /// written, never held whole, also when only its escaping passes it (a
    /// script of quotes half the cap long); one within it is a line.
    #[test]
    fn a_command_longer_than_the_cap_is_refused_as_it_is_written() {
        let quotes = HostCommand::Evaluate {
            view: 1,
            call: 1,
            script: "\"".repeat(MAX_COMMAND / 2),
        };
        assert!(encode(&quotes).is_none());
        let line = encode(&HostCommand::Destroy { view: 1 }).expect("a line");
        assert!(line.ends_with(b"\n") && line.len() < MAX_COMMAND);
    }
}
