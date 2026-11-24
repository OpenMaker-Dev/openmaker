//! A document's window on its own thread: input in, egui's pass for the
//! window, a frame out when anything the app shows changed.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::accesskit::{Action, ActionRequest, NodeId, TreeId};
use egui::{Event, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};
use om_data::{Connection, LocalFile};
use om_gui::licensing::{LicenseKey, LicenseStore};
use om_gui::OpenMakerApp;
use zeroize::Zeroizing;

use crate::frame::{self, Frame, FrameTimings, Geometry, TextureUpdate};
use crate::input::{self, Input};

/// How many passes a deletion around the selection waits at most for a Tab
/// or Return to give the focus to the next field.
const DELETE_WAITS: u8 = 30;
use crate::state::{self, DocumentState};
use crate::{
    accessibility, provider, Element, ElementKind, GoError, Menu, SignIn, Source, Viewport,
};

/// How long an open may take before the document gives up.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a glide moves the content: about a display refresh.
const GLIDE_STEP: Duration = Duration::from_millis(16);
/// A glide's speed falls by this factor each millisecond, as iOS's scroll
/// views slow by default (`UIScrollView.DecelerationRate.normal`).
const GLIDE_DECAY_PER_MS: f32 = 0.998;
/// A glide slower than this, in points a second, stops.
const GLIDE_STOP: f32 = 20.0;
/// How often the window is drawn while it waits on a script, a save or a
/// page. A desktop window is drawn on every pointer move while the user
/// works, so a step that takes effect on the frame after a reply never
/// waits there; here it would wait for the next touch.
const BUSY_STEP: Duration = Duration::from_millis(50);

/// The client license a user typed: it lasts until the file closes.
struct SessionLicense(Option<LicenseKey>);

impl LicenseStore for SessionLicense {
    fn load(&mut self) -> Result<Option<LicenseKey>, om_data::DataError> {
        Ok(self
            .0
            .as_ref()
            .map(|key| LicenseKey::new(key.expose().to_owned())))
    }
    fn save(&mut self, key: &LicenseKey) -> Result<(), om_data::DataError> {
        self.0 = Some(LicenseKey::new(key.expose().to_owned()));
        Ok(())
    }
    fn remove(&mut self) -> Result<(), om_data::DataError> {
        self.0 = None;
        Ok(())
    }
}

/// The sign-in a hosted file was opened with, kept while it is open: a
/// lost connection signs in again with it, at the host it was typed for.
struct KeptSignIn {
    host: String,
    port: u16,
    account: String,
    password: Zeroizing<String>,
}

/// Content gliding on after a pan, slowing to a stop.
struct Glide {
    velocity: Vec2,
    last: Instant,
}

/// What the app asked for that waits until the window lets it run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// Save the record's changes, as Done does (the app left the
    /// foreground).
    Save,
    /// Save them, then close the file ([`crate::Document::close`], or the
    /// app let go of the document).
    Close,
}

/// What a step made.
pub(crate) struct Step {
    pub frame: Option<Frame>,
    /// When the window asks to be drawn again; `None` for not until input.
    pub repaint_after: Option<Duration>,
    pub closed: bool,
    /// A window, shown or hidden, holds changes not saved to the host
    /// ([`OpenMakerApp::unsaved_anywhere`]).
    pub unsaved: bool,
    /// A save or close the app asked for is under way.
    pub saving: bool,
    /// How a close asked for ended since the last step: its file closed,
    /// or the close was refused.
    pub close_result: Option<bool>,
}

pub(crate) struct Surface {
    app: OpenMakerApp,
    ctx: egui::Context,
    viewport: Viewport,
    started: Instant,
    /// The app is in the foreground.
    active: bool,
    /// The pointer leaves after the next pass: a tap's or a pan's ended.
    pointer_leaves: bool,
    /// Where the pan in progress began.
    pan: Option<Pan>,
    glide: Option<Glide>,
    /// An input method's composition shows in the field: its text so far.
    preedit: Option<String>,
    /// Passes a deletion around the selection has waited for the focus to
    /// settle ([`DELETE_WAITS`] at most).
    delete_waits: u8,
    /// Taps that left a field taking typing ([`DocumentState::field_taps`]).
    field_taps: u32,
    /// What the last Copy or Cut copied ([`Input::PasteCopied`]).
    last_copy: Option<String>,
    /// The insertion point of the field that takes typing.
    caret: Option<crate::state::Caret>,
    /// What the app asked to happen to the record or file once the window
    /// lets it.
    intent: Option<Intent>,
    /// The windows whose save (Done) the intent asked for, each once.
    asked: Vec<u32>,
    /// The file in front when the close was asked for, by its id in the
    /// app: the close is for it, not for whatever file a script brings
    /// forward meanwhile.
    close_target: Option<u64>,
    /// The intent asked for Close File: once its file is gone the close is
    /// done, whatever file comes to the front after it.
    closing: bool,
    /// How the last close ended, for [`crate::Document::close`]: closed, or
    /// refused.
    close_result: Option<bool>,
    /// Passes left before what the intent asked for last is read: a Done
    /// or Close File runs in the next pass, and starts its save, triggers
    /// or close there.
    settle: u8,
    /// The app's last close ended with the file open: its save kept the
    /// changes (a trigger refused it, or it failed), they are held, or the
    /// close itself was refused (an OnWindowClose trigger).
    close_refused: bool,
    /// Text the file copied, and URLs it opened, not yet in a frame.
    copied: Option<String>,
    open_urls: Vec<String>,
    accessibility: bool,
    /// The visible elements as of the last pass, for [`Input::Activate`].
    elements: Vec<Element>,
    /// Texture changes not yet in a frame.
    textures: Vec<TextureUpdate>,
    freed: Vec<u64>,
    /// What the last frame made carried, to tell what changed.
    geometry: Geometry,
    state: Option<DocumentState>,
    menus: Vec<Menu>,
    sent_elements: Vec<Element>,
    /// A hosted file's sign-in, for as long as the file is open.
    sign_in: Option<KeptSignIn>,
    /// The file reconnected by itself since its connection was last lost.
    reconnected: bool,
}

impl Surface {
    /// Open `source` and run the window until the file is open, it fails,
    /// or [`OPEN_TIMEOUT`] passes.
    pub(crate) fn open(
        source: Source,
        sign_in: SignIn,
        viewport: Viewport,
        repaint: impl Fn(egui::RequestRepaintInfo) + Send + Sync + 'static,
    ) -> Result<Self, GoError> {
        let host = match &source {
            Source::Hosted { host, .. } => Some(host),
            Source::Local { .. } => None,
        };
        let SignIn {
            account,
            password,
            license,
        } = sign_in;
        let license = SessionLicense((!license.is_empty()).then(|| LicenseKey::new(license)));
        let mut app =
            OpenMakerApp::with_license_store(Box::new(provider(host)?), Box::new(license))
                .mobile_document();
        let ctx = egui::Context::default();
        ctx.set_request_repaint_callback(repaint);
        let mut kept = None;
        match source {
            Source::Hosted { host, file } => {
                let address = host.address.trim().to_owned();
                kept = Some(KeptSignIn {
                    host: address.clone(),
                    port: host.port,
                    account: account.clone(),
                    password: Zeroizing::new(password.clone()),
                });
                app = app.open_on_launch(Connection {
                    host: address,
                    port: host.port,
                    file,
                    account,
                    password,
                });
            }
            Source::Local { path } => app.open_device_file(
                &ctx,
                LocalFile {
                    path: path.into(),
                    credentials: (!(account.is_empty() && password.is_empty()))
                        .then(|| (account, password.into())),
                },
            ),
        }
        let mut surface = Surface {
            app,
            ctx,
            viewport,
            started: Instant::now(),
            active: true,
            pointer_leaves: false,
            pan: None,
            glide: None,
            preedit: None,
            delete_waits: 0,
            field_taps: 0,
            last_copy: None,
            caret: None,
            intent: None,
            asked: Vec::new(),
            close_target: None,
            closing: false,
            close_result: None,
            settle: 0,
            close_refused: false,
            copied: None,
            open_urls: Vec::new(),
            accessibility: false,
            elements: Vec::new(),
            textures: Vec::new(),
            freed: Vec::new(),
            geometry: Geometry::default(),
            state: None,
            menus: Vec::new(),
            sent_elements: Vec::new(),
            sign_in: kept,
            reconnected: false,
        };
        let deadline = Instant::now() + OPEN_TIMEOUT;
        while !surface.app.is_open() {
            // The window's own passes, unseen: their texture changes wait
            // for the first frame.
            surface.pass(Vec::new());
            if let Some((code, message)) = surface.app.failure() {
                return Err(GoError::from_failure(code, message));
            }
            if surface.app.needs_account() {
                return Err(GoError::SignInRequired {
                    reason: "This file requires an account and password.".into(),
                });
            }
            if Instant::now() >= deadline {
                return Err(GoError::failed("Opening the file timed out."));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(surface)
    }

    pub(crate) fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// A file is open, in a window shown or hidden.
    pub(crate) fn is_open(&self) -> bool {
        self.app.any_file_open()
    }

    /// Run one pass with as much of `inputs` as one frame takes. A command
    /// or menu choice, a tap and an element's action get a pass of their
    /// own, after the field has taken everything typed before them: they may
    /// leave the field, and what the field had not read would be lost or go
    /// to the next field. What is left stays queued.
    pub(crate) fn step(&mut self, inputs: &mut VecDeque<Input>) -> Step {
        let mut events = Vec::new();
        if std::mem::take(&mut self.pointer_leaves) {
            events.push(Event::PointerGone);
        }
        let mut took = false;
        let mut tapped = false;
        // A pan found to drag a handle presses it where it began, in a pass
        // of its own: egui finds the handle at the pointer's last position
        // in a pass, so the finger's first move must not come with it.
        let mut press_only = false;
        if let Some(pan) = &mut self.pan {
            if pan.drags == Some(true) && !pan.pressed {
                pan.pressed = true;
                events.push(pointer_button(pan.at, true));
                took = true;
                press_only = true;
            }
        }
        while let Some(input) = inputs.front().filter(|_| !press_only) {
            // A tap or an element's action can move the focus to another
            // field: it gets a pass of its own, so that the field reads what
            // was typed or committed before it, and what comes after reaches
            // the field it focuses.
            // A pan's start gets one too: the pointer shows there first, and
            // what it shows decides whether the pan drags or scrolls.
            let moves_focus = matches!(
                input,
                Input::Tap { .. } | Input::Activate { .. } | Input::PanBegan { .. }
            );
            // A deletion around the selection reads the field's selection
            // and text as the last pass left them: it waits for a pass of
            // its own, and while a Tab or Return still moves the focus to
            // another field (om-gui gives it in a later pass), so that it
            // counts against the field it will delete in.
            // A paste of the last copy waits for the pass that copied it. A
            // pan's end waits for a pass after its last move: egui drops the
            // scroll a pass has gathered when the same pass ends it.
            if took && matches!(input, Input::PasteCopied { .. } | Input::PanEnded { .. }) {
                break;
            }
            let deletes = matches!(input, Input::DeleteAround { .. });
            let moving = deletes && self.app.focus_moving();
            // The wait is bounded: a trigger's script can wait for the
            // user, whose taps are queued behind the deletion. A deletion
            // whose field is still unknown then is dropped rather than made
            // in the field the focus is leaving.
            if moving && self.delete_waits >= DELETE_WAITS {
                inputs.pop_front();
                self.delete_waits = 0;
                continue;
            }
            if moving {
                self.delete_waits += 1;
            }
            if (took && (moves_focus || deletes)) || moving {
                break;
            }
            if deletes {
                self.delete_waits = 0;
            }
            if matches!(input, Input::Run { .. } | Input::Menu { .. }) {
                if !took {
                    if let Some(text) = self.preedit.take() {
                        // An input method's composition is kept as it shows
                        // before a command can save the field.
                        events.push(Event::Ime(egui::ImeEvent::Commit(text)));
                    } else {
                        let input = inputs.pop_front().expect("peeked");
                        self.take(input, &mut events);
                    }
                    took = true;
                }
                break;
            }
            let input = inputs.pop_front().expect("peeked");
            tapped |= matches!(input, Input::Tap { .. } | Input::Activate { .. });
            self.take(input, &mut events);
            took = true;
            if moves_focus {
                break;
            }
        }
        self.glide(&mut events);
        if !took {
            self.advance_intent();
        }
        let started = Instant::now();
        let output = self.pass(events);
        let pass = started.elapsed();
        self.reconnect();
        let egui::FullOutput {
            shapes,
            pixels_per_point,
            viewport_output,
            ..
        } = output;
        let started = Instant::now();
        let primitives = self.ctx.tessellate(shapes, pixels_per_point);
        let tessellate = started.elapsed();
        let started = Instant::now();
        let geometry = frame::pack(&primitives, self.viewport);
        let pack = started.elapsed();
        let mut state = DocumentState::of(&self.app, self.caret);
        // A tap or an element's action that leaves a field taking typing
        // asks for the keyboard again: the user may have put it away
        // (Android's Back).
        if tapped && state.keyboard {
            self.field_taps = self.field_taps.wrapping_add(1);
        }
        state.field_taps = self.field_taps;
        // The soonest any viewport asked for: with no native windows, all
        // of them are drawn in the root's pass.
        let mut repaint_after = viewport_output
            .values()
            .map(|viewport| viewport.repaint_delay)
            .filter(|delay| *delay != Duration::MAX)
            .min();
        let mut soon = |step: Duration| {
            repaint_after = Some(repaint_after.map_or(step, |delay| delay.min(step)));
        };
        if self.pointer_leaves || !inputs.is_empty() {
            soon(Duration::ZERO);
        }
        // A glide moves on, and a close the app asked for waits for the
        // window to let it run, once a display refresh.
        if self.glide.is_some() {
            soon(GLIDE_STEP);
        }
        // Until the window lets what the app asked for run, and while it
        // waits on a script, a save or a page.
        if (state.busy || self.intent.is_some()) && !state.closed {
            soon(BUSY_STEP);
        }
        let menus = state::menus(self.app.chrome_menus());
        let copied = self.copied.take();
        let open_urls = std::mem::take(&mut self.open_urls);
        let menus_changed = menus != self.menus;
        let elements_changed = self.accessibility && self.elements != self.sent_elements;
        let changed = geometry != self.geometry
            || !self.textures.is_empty()
            || !self.freed.is_empty()
            || self.state.as_ref() != Some(&state)
            || menus_changed
            || elements_changed
            || copied.is_some()
            || !open_urls.is_empty();
        let closed = state.closed;
        // Every window's, not only the front one's that the state shows.
        let unsaved = !closed && self.app.unsaved_anywhere();
        // A closed file has nothing left to save.
        let saving = self.intent.is_some() && !closed;
        if !changed {
            return Step {
                frame: None,
                repaint_after,
                closed,
                unsaved,
                saving,
                close_result: self.close_result.take(),
            };
        }
        let micros = |d: Duration| d.as_micros().min(u128::from(u32::MAX)) as u32;
        let frame = Frame {
            viewport: self.viewport,
            textures: std::mem::take(&mut self.textures),
            freed: std::mem::take(&mut self.freed),
            vertices: geometry.vertices.clone(),
            indices: geometry.indices.clone(),
            batches: geometry.batches.clone(),
            state: state.clone(),
            menus: menus_changed.then(|| menus.clone()),
            elements: elements_changed.then(|| self.elements.clone()),
            copied,
            open_urls,
            timings: FrameTimings {
                pass: micros(pass),
                tessellate: micros(tessellate),
                pack: micros(pack),
            },
        };
        self.geometry = geometry;
        self.state = Some(state);
        self.menus = menus;
        if elements_changed {
            self.sent_elements = self.elements.clone();
        }
        Step {
            frame: Some(frame),
            repaint_after,
            closed,
            unsaved,
            saving,
            close_result: self.close_result.take(),
        }
    }

    /// Delete `before` and `after` units around the focused field's
    /// selection, keeping the selection, as Android's input connection
    /// does: one edit (egui's `DeleteSurrounding`), not keys a keystroke
    /// trigger could take one by one. While an input method composes, the
    /// composition is the selection and stays. UTF-16 counts are turned
    /// into characters against the field's text; with no layout field's
    /// text known (a dialog's field) a unit is taken as a character.
    fn delete_around(&mut self, before: u32, after: u32, utf16: bool, events: &mut Vec<Event>) {
        const MOST: usize = 64;
        let Some(range) = self
            .ctx
            .memory(|memory| memory.focused())
            .and_then(|id| egui::text_edit::TextEditState::load(&self.ctx, id))
            .and_then(|state| state.cursor.char_range())
        else {
            // No field takes typing: there is nothing to delete.
            return;
        };
        let [start, end] = range.sorted_cursors().map(|cursor| cursor.index.0);
        let units = |count: u32| (count as usize).min(MOST);
        let (before, after) = match self.app.focused_field_text(&self.ctx) {
            Some(text) => {
                let chars: Vec<char> = text.chars().collect();
                let start = start.min(chars.len());
                let end = end.clamp(start, chars.len());
                (
                    chars_for(chars[..start].iter().rev(), units(before), utf16),
                    chars_for(chars[end..].iter(), units(after), utf16),
                )
            }
            None => (units(before).min(start), units(after)),
        };
        if before > 0 || after > 0 {
            events.push(Event::Ime(egui::ImeEvent::DeleteSurrounding {
                before_chars: before,
                after_chars: after,
            }));
        }
    }

    fn take(&mut self, input: Input, events: &mut Vec<Event>) {
        match input {
            Input::Resize { viewport } => match viewport.check() {
                Ok(viewport) => self.viewport = viewport,
                Err(_) => log::warn!("ignored an unsupported canvas size {viewport:?}"),
            },
            Input::Tap { x, y } => {
                self.stop_glide(events);
                let pos = egui::pos2(x, y);
                events.push(Event::PointerMoved(pos));
                for pressed in [true, false] {
                    events.push(Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                // Not in this pass: the tap must land on what it hovers.
                self.pointer_leaves = true;
            }
            Input::PanBegan { x, y } => {
                self.stop_glide(events);
                let pos = egui::pos2(x, y);
                // The pointer goes there first: what it shows over that
                // spot (`Surface::pass`) says whether the pan drags or
                // scrolls.
                self.pan = Some(Pan {
                    at: pos,
                    drags: None,
                    pressed: false,
                    scrolling: false,
                });
                events.push(Event::PointerMoved(pos));
            }
            Input::PanMoved { dx, dy } => {
                if let Some(pan) = &mut self.pan {
                    if pan.drags == Some(true) {
                        // A handle, pressed where the pan began (`step`),
                        // dragged with the finger.
                        pan.at += egui::vec2(dx, dy);
                        events.push(Event::PointerMoved(pan.at));
                    } else {
                        if !pan.scrolling {
                            pan.scrolling = true;
                            events.push(wheel(Vec2::ZERO, TouchPhase::Start));
                        }
                        events.push(wheel(egui::vec2(dx, dy), TouchPhase::Move));
                    }
                }
            }
            Input::PanEnded { vx, vy } => {
                if let Some(pan) = self.pan.take() {
                    if pan.drags == Some(true) {
                        if pan.pressed {
                            events.push(pointer_button(pan.at, false));
                        }
                        self.pointer_leaves = true;
                        return;
                    }
                    let velocity = egui::vec2(vx, vy);
                    if velocity.is_finite() && velocity.length() > GLIDE_STOP {
                        self.glide = Some(Glide {
                            velocity,
                            last: Instant::now(),
                        });
                    } else {
                        events.push(wheel(Vec2::ZERO, TouchPhase::End));
                        self.pointer_leaves = true;
                    }
                }
            }
            Input::Text { text } => {
                if self.preedit.take().is_some() {
                    events.push(Event::Ime(egui::ImeEvent::Commit(text)));
                } else {
                    events.push(Event::Text(text));
                }
            }
            Input::Compose { text } => {
                self.preedit = (!text.is_empty()).then(|| text.clone());
                events.push(Event::Ime(egui::ImeEvent::Preedit {
                    text,
                    active_range_chars: None,
                }));
            }
            Input::Press {
                key,
                shift,
                command,
            } => {
                let modifiers = input::modifiers(shift, command);
                // Copy and Cut are egui's own events, as a desktop event
                // loop gives them for Command-C and Command-X.
                if command && matches!(key, input::Key::C | input::Key::X) {
                    // What it copies, if anything, is this copy's.
                    self.last_copy = None;
                    events.push(if key == input::Key::C {
                        Event::Copy
                    } else {
                        Event::Cut
                    });
                    return;
                }
                for pressed in [true, false] {
                    // Space types a space and is the key option controls
                    // take, as a desktop keyboard's Space gives both.
                    if key == input::Key::Space && !pressed {
                        events.push(Event::Text(" ".to_owned()));
                    }
                    events.push(if key == input::Key::KeypadEnter {
                        // Marked as om-gui's event loop marks it on the
                        // desktop, so Browse tells it from Return.
                        om_gui::keypad::enter_event(pressed, modifiers)
                    } else {
                        Event::Key {
                            key: key.egui(),
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
                        }
                    });
                }
            }
            Input::Paste { text } => events.push(Event::Paste(text)),
            Input::PasteCopied { clipboard } => {
                // The document's copy until it is on the clipboard; then, or
                // after a copy of nothing, the clipboard's text as it was
                // when the paste was asked for.
                let text = self.last_copy.clone().unwrap_or(clipboard);
                if !text.is_empty() {
                    events.push(Event::Paste(text));
                }
            }
            Input::ClipboardSet { text } => {
                // A later copy's text stays until it, too, is there.
                if self.last_copy.as_deref() == Some(text.as_str()) {
                    self.last_copy = None;
                }
            }
            Input::Modifiers { shift, command } => {
                events.push(Event::ModifiersChanged(input::modifiers(shift, command)));
            }
            Input::DeleteAround {
                before,
                after,
                utf16,
            } => self.delete_around(before, after, utf16, events),
            Input::Activate { id } => {
                let Some(element) = self.elements.iter().find(|e| e.id == id && e.enabled) else {
                    return;
                };
                let action = if element.kind == ElementKind::Field {
                    Action::Focus
                } else {
                    Action::Click
                };
                events.push(Event::AccessKitActionRequest(ActionRequest {
                    action,
                    target_tree: TreeId::ROOT,
                    target_node: NodeId(id),
                    data: None,
                }));
            }
            Input::Menu { id } => self.app.choose_menu_row(id),
            // The chrome's Close is the app's close: the record's save, then
            // Close File, followed to its outcome. It is for the file the
            // app showed when it was tapped: one a script brought forward
            // since is left open, as a control that changed does nothing.
            Input::Run {
                command: input::Command::Close { file },
            } => {
                if file == self.app.front_file()
                    && self
                        .app
                        .chrome_command_available(&om_gui::ChromeCommand::Close)
                {
                    self.request_close();
                }
            }
            Input::Run { command } => {
                // A control's state can change before its tap arrives: a
                // command no longer available does nothing, as a disabled
                // control does.
                self.app.run_chrome_command(command.chrome());
            }
            Input::Active { active } => {
                self.active = active;
                if !active {
                    self.stop_glide(events);
                    // FileMaker Go keeps an open file's state when it leaves
                    // the foreground; what becomes of a record's changes is
                    // not documented (go-client.md). OpenMaker saves them, as
                    // Done does, so that iOS or Android ending the app loses
                    // nothing: as soon as the window lets it, also when an
                    // edit still waits for its record.
                    if self.intent.is_none() {
                        self.intent = Some(Intent::Save);
                    }
                }
            }
            Input::Accessibility { enabled } => {
                self.accessibility = enabled;
                if enabled {
                    self.ctx.enable_accesskit();
                } else {
                    self.ctx.disable_accesskit();
                    self.elements.clear();
                    self.sent_elements.clear();
                }
            }
        }
    }

    /// Android closes a backgrounded app's connections within seconds, and a
    /// network change drops them too: in the foreground, a hosted file whose
    /// connection was lost signs in again by itself, once per loss, as the
    /// lost banner's Reconnect does once its password is typed. A refusal
    /// stays in the window's Reconnect dialog for the user.
    fn reconnect(&mut self) {
        if self.app.connection_lost().is_none() {
            self.reconnected = false;
            return;
        }
        let Some(kept) = &self.sign_in else {
            return;
        };
        if self.active
            && !self.reconnected
            && self.app.reconnect_lost(
                &self.ctx,
                &kept.host,
                kept.port,
                &kept.account,
                &kept.password,
            )
        {
            self.reconnected = true;
            self.ctx.request_repaint();
        }
    }

    fn stop_glide(&mut self, events: &mut Vec<Event>) {
        if self.glide.take().is_some() {
            events.push(wheel(Vec2::ZERO, TouchPhase::End));
        }
    }

    /// Move a gliding pan on by the time since its last move.
    fn glide(&mut self, events: &mut Vec<Event>) {
        let Some(glide) = &mut self.glide else {
            return;
        };
        let now = Instant::now();
        let ms = now.duration_since(glide.last).as_secs_f32() * 1000.0;
        glide.last = now;
        let start = glide.velocity;
        glide.velocity *= GLIDE_DECAY_PER_MS.powf(ms);
        // The distance covered as the speed fell from `start`.
        let distance = (start - glide.velocity) / -GLIDE_DECAY_PER_MS.ln() / 1000.0;
        events.push(wheel(distance, TouchPhase::Move));
        if glide.velocity.length() < GLIDE_STOP {
            self.glide = None;
            events.push(wheel(Vec2::ZERO, TouchPhase::End));
            self.pointer_leaves = true;
        }
    }

    /// One egui pass of the window.
    fn pass(&mut self, events: Vec<Event>) -> egui::FullOutput {
        let viewport = self.viewport;
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(viewport.width, viewport.height),
            )),
            max_texture_side: Some(viewport.max_texture_side as usize),
            time: Some(self.started.elapsed().as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            events,
            focused: self.active,
            ..Default::default()
        };
        let root = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
        root.native_pixels_per_point = Some(viewport.scale);
        root.focused = Some(self.active);
        let app = &mut self.app;
        let mut output = self.ctx.run_ui(raw, |ui| app.show(ui));
        let (updates, freed) = frame::texture_updates(std::mem::take(&mut output.textures_delta));
        self.textures.extend(updates);
        self.freed.extend(freed);
        let platform = std::mem::take(&mut output.platform_output);
        // A pan that began on a handle (a Table view column's divider, any
        // widget that shows a resize cursor) drags it; elsewhere it scrolls.
        if let Some(pan) = &mut self.pan {
            if pan.drags.is_none() {
                pan.drags = Some(resizes(platform.cursor_icon));
            }
        }
        self.caret = platform.ime.map(|ime| crate::state::Caret {
            x: ime.cursor_rect.min.x,
            y: ime.cursor_rect.min.y,
            width: ime.cursor_rect.width(),
            height: ime.cursor_rect.height(),
            password: ime.purpose == egui::IMEPurpose::Password,
        });
        for command in platform.commands {
            match command {
                egui::OutputCommand::CopyText(text) => {
                    self.last_copy = Some(text.clone());
                    self.copied = Some(text);
                }
                egui::OutputCommand::OpenUrl(url) => self.open_urls.push(url.url),
                _ => {}
            }
        }
        if let Some(update) = &platform.accesskit_update {
            self.elements = accessibility::visible(
                update,
                egui::accesskit::Rect {
                    x0: 0.0,
                    y0: 0.0,
                    x1: f64::from(viewport.width),
                    y1: f64::from(viewport.height),
                },
            );
        }
        output
    }

    /// Close the file as Close File does (its triggers, the record's
    /// commit) as soon as the window lets it.
    /// A close already under way goes on, and one that follows a save
    /// under way (leaving the foreground) goes on from it: asking again
    /// would run the records' or the window's triggers again.
    pub(crate) fn request_close(&mut self) {
        if self.intent == Some(Intent::Close) {
            return;
        }
        self.intent = Some(Intent::Close);
        self.close_target = Some(self.app.front_file());
        self.close_refused = false;
    }

    /// Take what the app asked for one step further. Each window whose
    /// record has changes, the front one first, is asked to save them once,
    /// as Done does (one behind the front may hold a record a script set a
    /// field in): so that what was typed never depends on the close's own
    /// commit, and a trigger that keeps the record (OnRecordCommit) is not
    /// run again. The next window is asked while saves are on their way,
    /// and while the alert of one the host refused (a validation failure)
    /// waits for the user, so that it keeps no other window's from going.
    /// Nothing is
    /// read while the window is busy (a save on its way, a script, a
    /// dialog, an edit waiting for its record, a close or a reconnect):
    /// once it is not, what is left was kept. The app's save then ends; a
    /// close goes on only when nothing was kept, and ends when the file
    /// closed or, the window free again with the file open, the close was
    /// refused (an OnWindowClose trigger, or a lost connection).
    fn advance_intent(&mut self) {
        let Some(intent) = self.intent else {
            return;
        };
        if self.settle > 0 {
            self.settle -= 1;
            return;
        }
        if self.app.lifecycle_blocked() {
            if !self.closing && self.app.lifecycle_blocked_by_alert() {
                self.ask_next_save();
            }
            return;
        }
        if self.app.lifecycle_busy() {
            // Only saves are on their way.
            if !self.closing {
                self.ask_next_save();
            }
            return;
        }
        if self.closing {
            if self
                .close_target
                .is_some_and(|file| self.app.file_open(file))
            {
                self.refuse_intent(intent);
            } else {
                self.close_result = Some(true);
                self.end_intent();
            }
            return;
        }
        if self.ask_next_save() {
            return;
        }
        let drafts = self.app.windows_with_drafts();
        match intent {
            // What is left was kept. Changes a window kept when its
            // connection was lost are saved only after a reconnect, those a
            // reconnect found changed on the host wait for Keep or Discard,
            // and text the host refused waits for Discard Input: Close File
            // would discard them (the lost banner's own Close File does, when
            // the user chooses it), so the file stays open with them. So it
            // does when another file came to the front since the close was
            // asked for: the close was not for it.
            Intent::Close
                if !drafts.is_empty()
                    || self.app.changes_held()
                    || self.close_target != Some(self.app.front_file()) =>
            {
                self.refuse_intent(intent)
            }
            Intent::Close => {
                if self.app.close_front_file(&self.ctx) {
                    self.closing = true;
                    self.settle = 2;
                } else {
                    self.refuse_intent(intent);
                }
            }
            Intent::Save => self.end_intent(),
        }
    }

    /// Ask the next window with changes not yet asked to save them. False
    /// when every one has been asked.
    fn ask_next_save(&mut self) -> bool {
        let drafts = self.app.windows_with_drafts();
        let Some(&id) = drafts.iter().find(|id| !self.asked.contains(id)) else {
            return false;
        };
        self.asked.push(id);
        if self.app.save_window(&self.ctx, id) {
            self.settle = 2;
        }
        true
    }

    /// The intent ends with the changes still in the window.
    fn refuse_intent(&mut self, intent: Intent) {
        if intent == Intent::Close {
            self.close_refused = true;
            self.close_result = Some(false);
        }
        self.end_intent();
    }

    fn end_intent(&mut self) {
        self.intent = None;
        self.asked.clear();
        self.closing = false;
        self.close_target = None;
        self.settle = 0;
    }

    /// Run what the app sent before it let go, then close the file and run
    /// the window until it closes or `budget` passes. A dialog the save
    /// raises, a save that keeps the changes or changes held keep the file
    /// open, and what it holds is lost with the document: the app let go of
    /// it. When the app's own close just ended that way, letting go does not
    /// ask again, which would run the record's commit triggers again.
    pub(crate) fn close(&mut self, mut inputs: VecDeque<Input>, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !inputs.is_empty() && Instant::now() < deadline {
            self.step(&mut inputs);
        }
        // After a refused close, letting go does not ask to close again, but
        // a save asked for since (the app left the foreground) still runs.
        if !self.close_refused && self.app.bring_open_file_forward() {
            self.request_close();
        }
        loop {
            while self.is_open() && self.intent.is_some() && Instant::now() < deadline {
                self.advance_intent();
                self.pass(Vec::new());
                std::thread::sleep(Duration::from_millis(10));
            }
            // A file a script opened, shown or hidden, is left: letting go
            // closes it too, as Close File would.
            if self.is_open()
                && self.intent.is_none()
                && !self.close_refused
                && Instant::now() < deadline
                && self.app.bring_open_file_forward()
            {
                self.request_close();
                continue;
            }
            return;
        }
    }
}

fn wheel(delta: Vec2, phase: TouchPhase) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta,
        phase,
        // A finger on the glass moves the content with it: egui does not
        // smooth a trackpad's scrolling.
        source: MouseWheelSource::Trackpad,
        modifiers: egui::Modifiers::NONE,
    }
}

/// A finger panning over the canvas.
struct Pan {
    /// Where the finger is.
    at: Pos2,
    /// Whether it drags a handle rather than scrolling, once the pass that
    /// showed the pointer where it began has said.
    drags: Option<bool>,
    /// The handle has been pressed (dragging), or the scroll has begun.
    pressed: bool,
    scrolling: bool,
}

/// The primary button pressed or released at `pos`, as a touch's is.
fn pointer_button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

/// Whether a widget showing `cursor` under the pointer is a handle to drag.
fn resizes(cursor: egui::CursorIcon) -> bool {
    use egui::CursorIcon as C;
    matches!(
        cursor,
        C::ResizeHorizontal
            | C::ResizeNeSw
            | C::ResizeNwSe
            | C::ResizeVertical
            | C::ResizeEast
            | C::ResizeSouthEast
            | C::ResizeSouth
            | C::ResizeSouthWest
            | C::ResizeWest
            | C::ResizeNorthWest
            | C::ResizeNorth
            | C::ResizeNorthEast
            | C::ResizeColumn
            | C::ResizeRow
    )
}

/// How many of `side`'s characters, nearest first, make up `units` units
/// (UTF-16 code units when `utf16`, characters otherwise): a character is
/// taken whole, as a deletion never splits one.
fn chars_for<'a>(side: impl Iterator<Item = &'a char>, units: usize, utf16: bool) -> usize {
    let mut taken = 0;
    let mut chars = 0;
    for c in side {
        if taken >= units {
            break;
        }
        taken += if utf16 { c.len_utf16() } else { 1 };
        chars += 1;
    }
    chars
}

#[cfg(test)]
mod delete_around_tests {
    use super::chars_for;

    #[test]
    fn utf16_units_are_counted_as_whole_characters() {
        let before: Vec<char> = "A\u{1F600}".chars().collect();
        // The emoji is two UTF-16 units and one character: A stays.
        assert_eq!(chars_for(before.iter().rev(), 2, true), 1);
        assert_eq!(chars_for(before.iter().rev(), 3, true), 2);
        assert_eq!(chars_for(before.iter().rev(), 2, false), 2);
        // No more than the side holds.
        assert_eq!(chars_for(before.iter().rev(), 64, true), 2);
    }
}
