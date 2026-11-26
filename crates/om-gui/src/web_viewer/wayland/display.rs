//! The page host's display: a Wayland compositor of OpenMaker's own, on a
//! thread of its own, with no screen. The host process (`host.rs`) connects
//! to it as its only client, and each of its views is a toplevel window
//! here, titled with the view's id. Every buffer a view commits goes to the
//! app as a frame, which Browse paints as a texture inside the web viewer
//! object; the app's pointer input over the object comes back as the
//! pointer's events on that window.
//!
//! It offers what GTK needs to draw (`wl_compositor`, `wl_shm`, `xdg_wm_base`,
//! a `wl_output` whose scale is the app's), a seat with a pointer and a
//! keyboard, data devices, and `zwp_text_input_v3` of its own, whose input
//! method is the app's (winit's, through egui); no DMA-BUF, so WebKitGTK
//! draws its pages in shared memory (its web process still renders them on
//! the GPU). The keyboard's keymap is the session's when the app has it
//! (`keymap.rs`), xkb's default otherwise.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::backend::input::{Axis, AxisSource, ButtonState};
use smithay::input::keyboard::{FilterResult, Keycode, XkbConfig};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{channel, EventLoop, Interest, LoopSignal, PostAction};
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_callback::WlCallback;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
use smithay::reexports::wayland_server::protocol::wl_shm_pool::WlShmPool;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, ListeningSocket, New,
    Resource,
};
use smithay::utils::{Serial, Transform, SERIAL_COUNTER};
use smithay::wayland::compositor::{
    with_states, BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_xdg_shell,
};

use super::shm::{self, Shm};
use crate::web_viewer::PageInput;

/// The title prefix the host gives each view's window.
pub(crate) const TITLE: &str = "openmaker-view-";

/// How often windows hear that their last frame was shown.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// Linux input event codes of the mouse buttons (`BTN_LEFT` ...).
const BUTTONS: [u32; 3] = [0x110, 0x111, 0x112];

/// What the app asks of the display.
pub(crate) enum Request {
    /// Pointer input over a view's page, in its window's coordinates.
    Pointer { view: u64, input: PageInput },
    /// The keyboard goes to a view's window, or to none.
    Focus(Option<u64>),
    /// The scale the app draws at (`wl_output` scales are whole numbers).
    Scale(i32),
    /// The session's keymap, in xkb's text format.
    Keymap(String),
}

/// Where each view's text caret is, in its points (x, y, width, height), as
/// its page last said: the input method's window goes beside it.
pub(crate) type Carets = Arc<std::sync::Mutex<HashMap<u64, [i32; 4]>>>;

/// Each view's newest picture, which the display replaces as views commit
/// and the app takes when it draws: a slot per view, not a queue, so a
/// window that is not drawn holds at most one frame per view.
pub(crate) type Frames = Arc<std::sync::Mutex<FrameSlots>>;

/// The frames waiting for the app, and the largest side one may have (the
/// app's largest texture): a buffer larger than that is not even read.
#[derive(Default)]
pub(crate) struct FrameSlots {
    pub frames: HashMap<u64, Frame>,
    pub max_side: usize,
}

/// The most bytes the frames waiting for the app may hold together: a
/// view's new frame that would go beyond waits for its next.
const FRAME_BUDGET: usize = 256 * 1024 * 1024;

/// A view's newest picture: premultiplied RGBA.
pub(crate) struct Frame {
    pub view: u64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// The running display. Dropping it stops the thread.
pub(crate) struct PageDisplay {
    requests: channel::Sender<Request>,
    signal: LoopSignal,
    socket: PathBuf,
}

impl PageDisplay {
    /// Start the display, putting frames in `frames` and calling `wake`
    /// after each.
    pub fn start(
        frames: Frames,
        carets: Carets,
        wake: Box<dyn Fn() + Send>,
        scale: i32,
    ) -> Result<Self, String> {
        let directory = tempfile::Builder::new()
            .prefix("openmaker-pages-")
            .tempdir_in(
                std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from),
            )
            .map_err(|e| e.to_string())?;
        let socket = directory.path().join("wayland-0");
        let listener = ListeningSocket::bind_absolute(socket.clone()).map_err(|e| e.to_string())?;
        let (requests, request_rx) = channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let socket_directory = (listener, directory);
        std::thread::Builder::new()
            .name("openmaker-page-display".into())
            .spawn(move || {
                run(
                    socket_directory,
                    request_rx,
                    frames,
                    carets,
                    wake,
                    scale,
                    ready_tx,
                )
            })
            .map_err(|e| e.to_string())?;
        let signal = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the page display did not start".to_owned())??;
        Ok(PageDisplay {
            requests,
            signal,
            socket,
        })
    }

    /// The socket the host connects to (its `WAYLAND_DISPLAY`).
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    /// Where requests go, for another thread (the keymap's).
    pub fn sender(&self) -> channel::Sender<Request> {
        self.requests.clone()
    }
}

impl Drop for PageDisplay {
    fn drop(&mut self) {
        self.signal.stop();
        self.signal.wakeup();
    }
}

#[derive(Default)]
struct ClientState {
    compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

struct State {
    start: Instant,
    compositor: CompositorState,
    xdg: XdgShellState,
    seat_state: SeatState<State>,
    data_device: DataDeviceState,
    seat: Seat<State>,
    output: Output,
    /// Every toplevel window, and the view each one's title names.
    toplevels: Vec<ToplevelSurface>,
    views: HashMap<u64, WlSurface>,
    popups: Vec<PopupSurface>,
    callbacks: Vec<WlCallback>,
    frames: Frames,
    wake: Box<dyn Fn() + Send>,
    carets: Carets,
    /// The view the pointer is over, and the one with the keyboard.
    pointer_view: Option<u64>,
    keyboard_view: Option<u64>,
    /// The buttons the pointer holds down, as the pages heard them, and
    /// the view the first of them was pressed on, which has them until the
    /// last goes up (the pointer's grab), wherever the pointer is.
    buttons: Vec<u32>,
    pressed_on: Option<u64>,
    /// The modifier keys the keyboard holds down, as the page heard them.
    held: Vec<u32>,
    text_inputs: Vec<TextInput>,
}

/// A `zwp_text_input_v3`: whether its client enabled it, how many commits
/// it made (the serial `done` answers with), and its caret.
struct TextInput {
    resource: ZwpTextInputV3,
    enabled: bool,
    pending_enable: Option<bool>,
    pending_caret: Option<[i32; 4]>,
    commits: u32,
}

/// The display's loop. The socket and its private directory go once the
/// host has connected (or with the display, should it never connect).
fn run(
    (listener, directory): (ListeningSocket, tempfile::TempDir),
    requests: channel::Channel<Request>,
    frames: Frames,
    carets: Carets,
    wake: Box<dyn Fn() + Send>,
    scale: i32,
    ready: Sender<Result<LoopSignal, String>>,
) {
    type Started = (
        EventLoop<'static, State>,
        State,
        Rc<RefCell<Display<State>>>,
    );
    let started = (|| -> Result<Started, String> {
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().map_err(|e| e.to_string())?;
        let display: Display<State> = Display::new().map_err(|e| e.to_string())?;
        let dh = display.handle();
        let compositor = CompositorState::new::<State>(&dh);
        let xdg = XdgShellState::new::<State>(&dh);
        dh.create_global::<State, WlShm, ()>(1, ());
        dh.create_global::<State, ZwpTextInputManagerV3, ()>(1, ());
        let mut seat_state = SeatState::new();
        let data_device = DataDeviceState::new::<State>(&dh);
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(XkbConfig::default(), 600, 25)
            .map_err(|e| e.to_string())?;
        seat.add_pointer();
        OutputManagerState::new_with_xdg_output::<State>(&dh);
        let output = Output::new(
            "OPENMAKER-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "OpenMaker".into(),
                model: "Web viewers".into(),
            },
        );
        output.create_global::<State>(&dh);
        set_scale(&output, scale);
        let state = State {
            start: Instant::now(),
            compositor,
            xdg,
            seat_state,
            data_device,
            seat,
            output,
            toplevels: Vec::new(),
            views: HashMap::new(),
            popups: Vec::new(),
            callbacks: Vec::new(),
            frames,
            wake,
            pointer_view: None,
            buttons: Vec::new(),
            pressed_on: None,
            carets,
            keyboard_view: None,
            held: Vec::new(),
            text_inputs: Vec::new(),
        };
        Ok((event_loop, state, Rc::new(RefCell::new(display))))
    })();
    let (mut event_loop, mut state, display) = match started {
        Ok(started) => started,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let handle = event_loop.handle();
    // The display's own file descriptor (a copy), so that the display can
    // stay outside the loop's state and be dispatched without `unsafe`.
    let inserted = (|| -> Result<(), String> {
        let poll = display
            .borrow_mut()
            .backend()
            .poll_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        let clients = Rc::clone(&display);
        handle
            .insert_source(
                Generic::new(
                    poll,
                    Interest::READ,
                    smithay::reexports::calloop::Mode::Level,
                ),
                move |_, _, state| {
                    let _ = clients.borrow_mut().dispatch_clients(state);
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| e.to_string())?;
        // Only the host connects; the socket and its directory go after it.
        let host = Rc::clone(&display);
        let mut directory = Some(directory);
        handle
            .insert_source(
                Generic::new(
                    listener,
                    Interest::READ,
                    smithay::reexports::calloop::Mode::Level,
                ),
                move |_, socket, _| {
                    if let Ok(Some(stream)) = socket.accept() {
                        let _ = host
                            .borrow_mut()
                            .handle()
                            .insert_client(stream, Arc::new(ClientState::default()));
                        drop(directory.take());
                        return Ok(PostAction::Remove);
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| e.to_string())?;
        handle
            .insert_source(requests, |event, _, state| {
                if let channel::Event::Msg(request) = event {
                    state.request(request);
                }
            })
            .map_err(|e| e.to_string())?;
        handle
            .insert_source(Timer::from_duration(FRAME_INTERVAL), |_, _, state| {
                let time = state.millis();
                for callback in state.callbacks.drain(..) {
                    callback.done(time);
                }
                TimeoutAction::ToDuration(FRAME_INTERVAL)
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = inserted {
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(event_loop.get_signal()));
    let flush = Rc::clone(&display);
    let _ = event_loop.run(None, &mut state, move |_| {
        let _ = flush.borrow_mut().flush_clients();
    });
}

fn set_scale(output: &Output, scale: i32) {
    let scale = scale.clamp(1, 8);
    let mode = Mode {
        size: (1920 * scale, 1080 * scale).into(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
}

impl State {
    fn millis(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn request(&mut self, request: Request) {
        match request {
            Request::Pointer { view, input } => match input {
                PageInput::Key {
                    code,
                    pressed,
                    held,
                } => self.key(view, code, pressed, &held),
                PageInput::Preedit(text) => self.compose(view, Some(text), None),
                PageInput::Commit(text) => self.compose(view, None, Some(text)),
                input => self.pointer(view, input),
            },
            Request::Focus(view) => self.focus(view),
            Request::Scale(scale) => set_scale(&self.output, scale),
            Request::Keymap(keymap) => {
                if let Some(keyboard) = self.seat.get_keyboard() {
                    if let Err(error) = keyboard.set_keymap_from_string(self, keymap) {
                        eprintln!("openmaker: web viewers: the session's keymap: {error:?}");
                    }
                }
            }
        }
    }

    /// Give the keyboard, and the text input, to `view`'s window or none.
    fn focus(&mut self, view: Option<u64>) {
        let surface = view.and_then(|v| self.views.get(&v).cloned());
        let before = self.keyboard_view.and_then(|v| self.views.get(&v).cloned());
        if before == surface {
            return;
        }
        // The modifier keys it held go up before the focus moves, so that
        // the keyboard's state never keeps one down.
        for modifier in std::mem::take(&mut self.held) {
            self.send_key(modifier, false);
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface.clone(), SERIAL_COUNTER.next_serial());
        }
        for input in &mut self.text_inputs {
            // Left, a text input is inactive until its client enables it
            // again for the new window.
            input.enabled = false;
            input.pending_enable = None;
            if let Some(before) = &before {
                input.resource.leave(before);
            }
            if let Some(surface) = &surface {
                input.resource.enter(surface);
            }
        }
        self.keyboard_view = surface.and(view);
    }

    /// Send a key to `view`, which gets the keyboard if it had not, the
    /// modifier keys first, as `held` has them.
    fn key(&mut self, view: u64, code: u32, pressed: bool, held: &[u32]) {
        if self.keyboard_view != Some(view) {
            self.focus(Some(view));
        }
        if self.keyboard_view != Some(view) {
            return;
        }
        let modifiers = [
            crate::web_viewer::keys::SHIFT,
            crate::web_viewer::keys::CONTROL,
            crate::web_viewer::keys::ALT,
            crate::web_viewer::keys::SUPER,
        ];
        for modifier in modifiers {
            let down = held.contains(&modifier);
            if down != self.held.contains(&modifier) {
                self.send_key(modifier, down);
                if down {
                    self.held.push(modifier);
                } else {
                    self.held.retain(|m| *m != modifier);
                }
            }
        }
        if !modifiers.contains(&code) {
            self.send_key(code, pressed);
        }
    }

    fn send_key(&mut self, code: u32, pressed: bool) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let time = self.millis();
        keyboard.input::<(), _>(
            self,
            // xkb's keycodes are the input event codes plus 8.
            Keycode::new(code + 8),
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    /// An input method's text for `view`'s enabled text inputs: what it is
    /// composing, or what it finished.
    fn compose(&mut self, view: u64, preedit: Option<String>, commit: Option<String>) {
        if self.keyboard_view != Some(view) {
            return;
        }
        for input in self.text_inputs.iter().filter(|i| i.enabled) {
            let preedit = preedit.clone().filter(|t| !t.is_empty());
            let end = preedit
                .as_ref()
                .map_or(0, |t| i32::try_from(t.len()).unwrap_or(i32::MAX));
            input.resource.preedit_string(preedit, end, end);
            if let Some(text) = &commit {
                input.resource.commit_string(Some(text.clone()));
            }
            input.resource.done(input.commits);
        }
    }

    /// Let go of every button the pointer holds, and leave its window.
    fn release_pointer(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.millis();
        self.pressed_on = None;
        for code in std::mem::take(&mut self.buttons) {
            pointer.button(
                self,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    button: code,
                    state: ButtonState::Released,
                },
            );
        }
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: (0.0, 0.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        self.pointer_view = None;
    }

    fn pointer(&mut self, view: u64, input: PageInput) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let surface = self.views.get(&view).cloned();
        let time = self.millis();
        let motion = |state: &mut Self, x: f32, y: f32| {
            pointer.motion(
                state,
                surface.clone().map(|s| (s, (0.0, 0.0).into())),
                &MotionEvent {
                    location: (f64::from(x), f64::from(y)).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            state.pointer_view = surface.as_ref().map(|_| view);
        };
        match input {
            PageInput::Move { x, y } => motion(self, x, y),
            PageInput::Leave => {
                if self.pointer_view == Some(view) {
                    pointer.motion(
                        self,
                        None,
                        &MotionEvent {
                            location: (0.0, 0.0).into(),
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                        },
                    );
                    self.pointer_view = None;
                }
            }
            PageInput::Button {
                x,
                y,
                button,
                pressed,
            } => {
                motion(self, x, y);
                if pressed {
                    // A press anywhere closes the menus a page opened
                    // (they are not shown yet).
                    for popup in self.popups.drain(..) {
                        popup.send_popup_done();
                    }
                }
                let Some(&code) = BUTTONS.get(usize::from(button)) else {
                    return;
                };
                if pressed {
                    if self.buttons.is_empty() {
                        self.pressed_on = Some(view);
                    }
                    self.buttons.push(code);
                } else {
                    self.buttons.retain(|held| *held != code);
                    if self.buttons.is_empty() {
                        self.pressed_on = None;
                    }
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                        button: code,
                        state: if pressed {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                    },
                );
            }
            PageInput::Key { .. } | PageInput::Preedit(_) | PageInput::Commit(_) => return,
            PageInput::Wheel { x, y, dx, dy } => {
                motion(self, x, y);
                let mut frame = AxisFrame::new(time).source(AxisSource::Continuous);
                if dx != 0.0 {
                    frame = frame.value(Axis::Horizontal, f64::from(-dx));
                }
                if dy != 0.0 {
                    frame = frame.value(Axis::Vertical, f64::from(-dy));
                }
                pointer.axis(self, frame);
            }
        }
        pointer.frame(self);
    }

    /// The view a toplevel's title names.
    fn view_of(surface: &ToplevelSurface) -> Option<u64> {
        with_states(surface.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>()?;
            let title = data.lock().ok()?.title.clone()?;
            title.strip_prefix(TITLE)?.parse().ok()
        })
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with its state")
            .compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        let toplevel = self
            .toplevels
            .iter()
            .find(|t| t.wl_surface() == surface)
            .cloned();
        if let Some(toplevel) = &toplevel {
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
            if let Some(view) = Self::view_of(toplevel) {
                self.views.insert(view, surface.clone());
            }
        }
        if let Some(popup) = self.popups.iter().find(|p| p.wl_surface() == surface) {
            if !popup.is_initial_configure_sent() {
                let _ = popup.send_configure();
            }
        }
        let (buffer, callbacks) = with_states(surface, |states| {
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            let current = attributes.current();
            (
                current.buffer.take(),
                std::mem::take(&mut current.frame_callbacks),
            )
        });
        self.callbacks.extend(callbacks);
        let Some(BufferAssignment::NewBuffer(buffer)) = buffer else {
            return;
        };
        let view = toplevel.as_ref().and_then(Self::view_of);
        if let (Some(view), Some(data)) = (view, buffer.data::<shm::Buffer>()) {
            let (width, height) = (data.width as usize, data.height as usize);
            // From the sizes the buffer declares, before anything is read.
            // The view's frame still waiting goes first: this one replaces
            // it, and both would otherwise be held while this one is read.
            let admitted = self.frames.lock().is_ok_and(|mut slots| {
                slots.frames.remove(&view);
                let waiting: usize = slots.frames.values().map(|f| f.rgba.len()).sum();
                shm::admits(data, slots.max_side, waiting, FRAME_BUDGET)
            });
            if let Some(rgba) = admitted.then(|| data.rgba()).flatten() {
                let frame = Frame {
                    view,
                    width,
                    height,
                    rgba,
                };
                if let Ok(mut slots) = self.frames.lock() {
                    slots.frames.insert(view, frame);
                }
                (self.wake)();
            }
        }
        buffer.release();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}
impl OutputHandler for State {}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // The host sizes its windows itself; they are always active.
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Activated);
        });
        self.output.enter(surface.wl_surface());
        self.toplevels.push(surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self
            .views
            .iter()
            .find(|(_, s)| *s == surface.wl_surface())
            .map(|(view, _)| *view);
        self.toplevels.retain(|t| t != &surface);
        self.views.retain(|_, s| s != surface.wl_surface());
        // The buttons pressed on a view that goes are let go of, so that
        // the pointer holds none when it comes to another; also when the
        // pointer already left it (a layout change sends Leave before
        // Destroy).
        if gone.is_some() && (gone == self.pressed_on || gone == self.pointer_view) {
            self.release_pointer();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|s| s.geometry = positioner.get_geometry());
        self.output.enter(surface.wl_surface());
        self.popups.push(surface);
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popups.retain(|p| p != &surface);
    }

    fn grab(&mut self, _: PopupSurface, _: WlSeat, _: Serial) {}

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| s.geometry = positioner.get_geometry());
        surface.send_repositioned(token);
    }
}

delegate_compositor!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [WlShm: ()] => Shm);
smithay::reexports::wayland_server::delegate_dispatch!(State: [WlShm: ()] => Shm);
smithay::reexports::wayland_server::delegate_dispatch!(State: [WlShmPool: Arc<shm::Pool>] => Shm);
smithay::reexports::wayland_server::delegate_dispatch!(State: [WlBuffer: shm::Buffer] => Shm);

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_text_input_manager_v3::Request::GetTextInput { id, .. } = request {
            let resource = init.init(id, ());
            // One made while a window has the keyboard hears so.
            if let Some(surface) = state.keyboard_view.and_then(|v| state.views.get(&v)) {
                if surface.id().same_client_as(&resource.id()) {
                    resource.enter(surface);
                }
            }
            state.text_inputs.push(TextInput {
                resource,
                enabled: false,
                pending_enable: None,
                pending_caret: None,
                commits: 0,
            });
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let view = state.keyboard_view;
        let Some(input) = state
            .text_inputs
            .iter_mut()
            .find(|i| &i.resource == resource)
        else {
            return;
        };
        match request {
            zwp_text_input_v3::Request::Enable => input.pending_enable = Some(true),
            zwp_text_input_v3::Request::Disable => input.pending_enable = Some(false),
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => input.pending_caret = Some([x, y, width, height]),
            zwp_text_input_v3::Request::Commit => {
                input.commits = input.commits.wrapping_add(1);
                if let Some(enabled) = input.pending_enable.take() {
                    input.enabled = enabled;
                }
                let caret = input.pending_caret.take().filter(|_| input.enabled);
                if let (Some(rect), Some(view)) = (caret, view) {
                    if let Ok(mut carets) = state.carets.lock() {
                        carets.insert(view, rect);
                    }
                    (state.wake)();
                }
            }
            zwp_text_input_v3::Request::Destroy => {
                state.text_inputs.retain(|i| &i.resource != resource);
            }
            _ => {}
        }
    }
}
