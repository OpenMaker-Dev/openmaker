//! Web viewers in Browse (layout-model.md 9.6). A layout's web viewer
//! objects show pages in native browser views that their window owns:
//! Browse notes where each one is ([`Placement`]), and after the window is
//! drawn the app reconciles those notes with the live views
//! ([`WebViewers::reconcile`]). A view is created once its address has been
//! calculated; it follows the object's rectangle and the document's scroll
//! clip, hides while anything egui draws above the document covers it, and
//! is destroyed when its object is no longer painted (another layout, mode
//! or view, a panel that is not in front, a hide condition, the window or
//! the file closing).
//!
//! Every view belongs to one window instance (its id and `Get ( WindowUUID
//! )`), the app's session epoch, the file, the layout and the object
//! ([`ViewKey`]). Set Web Viewer, Perform JavaScript in Web Viewer and the
//! page's `FileMaker.PerformScript` callbacks are routed only through that
//! identity: a page of a closed window or an earlier session never reaches
//! a new one, and a callback runs a script of the view's own file only.
//!
//! Native views come from a [`Backend`]: WebKitGTK on Linux/X11 with the
//! `web-viewer` feature. Without one, web viewers show a note and the script
//! steps fail instead of reporting success.

#[cfg(all(feature = "web-viewer", target_os = "linux"))]
mod gtk;
mod viewports;

pub(crate) use viewports::ViewportWindows;

use om_layout::WebViewerOptions;

/// Error codes the script steps report (error-codes.csv): no object name,
/// a page still loading, a function that failed; no live web viewer of that
/// name on the window's layout.
pub(crate) const INVALID: i32 = 5;
pub(crate) const OBJECT_MISSING: i32 = 116;

/// How long Perform JavaScript in Web Viewer waits for its function before
/// the step fails (FileMaker waits without a limit; a page that never
/// answers must not hold the script forever).
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The most bytes of one `FileMaker.PerformScript` message a page may send.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// A web viewer Browse painted this frame, in the window's points.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placement {
    pub object: u64,
    pub options: WebViewerOptions,
    /// The object's rectangle, and the part of it the document shows (its
    /// scroll area clips the rest).
    pub rect: egui::Rect,
    pub visible: egui::Rect,
    /// The calculated web address; `None` while its data is fetched.
    pub address: Option<String>,
    /// The record whose List row the viewer is painted in: each row's is a
    /// view of its own. `None` elsewhere (Form view, a List's header and
    /// footer), where one view follows the current record.
    pub record: Option<i64>,
    /// The viewer is on the current record (always, outside List rows):
    /// Set Web Viewer and Perform JavaScript reach this one.
    pub current: bool,
    /// The document's zoom factor, which the page's zoom follows.
    pub zoom: f32,
}

/// The native window a view is a child of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeParent {
    /// An X11 window id.
    X11(u64),
}

impl NativeParent {
    /// The native window eframe created for the root viewport: an X11
    /// window, or why there is none to host views in.
    #[allow(
        clippy::useless_conversion,
        reason = "an Xlib window id is a C unsigned long: 32 bits on some targets"
    )]
    pub fn of(handle: &impl raw_window_handle::HasWindowHandle) -> Result<Self, Unavailable> {
        use raw_window_handle::RawWindowHandle;
        match handle.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Xlib(h)) => Ok(Self::X11(u64::from(h.window))),
            Ok(RawWindowHandle::Xcb(h)) => Ok(Self::X11(u64::from(h.window.get()))),
            Ok(_) => Err(Unavailable::Platform),
            Err(_) => Err(Unavailable::Window),
        }
    }
}

/// A view's place in its parent, in physical pixels: the visible part of
/// the object (`frame`, relative to the parent window), and where the whole
/// page lies relative to that frame (`content`: x and y offsets, at most
/// zero, and the object's full size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub frame: [i32; 4],
    pub content: [i32; 4],
    pub zoom: f64,
}

impl Geometry {
    fn of(rect: egui::Rect, visible: egui::Rect, pixels_per_point: f32, zoom: f32) -> Self {
        let px = |v: f32| (v * pixels_per_point).round() as i32;
        let shown = rect.intersect(visible);
        let (left, top) = (px(shown.min.x), px(shown.min.y));
        let (width, height) = (
            (px(shown.max.x) - left).max(0),
            (px(shown.max.y) - top).max(0),
        );
        Geometry {
            frame: [left, top, width, height],
            content: [
                px(rect.min.x) - left,
                px(rect.min.y) - top,
                (px(rect.max.x) - px(rect.min.x)).max(1),
                (px(rect.max.y) - px(rect.min.y)).max(1),
            ],
            zoom: f64::from(zoom),
        }
    }

    fn is_empty(&self) -> bool {
        self.frame[2] <= 0 || self.frame[3] <= 0
    }
}

/// What a native view is created with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewSpec {
    pub url: String,
    pub geometry: Geometry,
    pub visible: bool,
    /// "Allow interaction with web viewer content".
    pub interactive: bool,
    /// "Allow JavaScript to perform FileMaker scripts": the page gets the
    /// `FileMaker` object ([`BRIDGE`]).
    pub scripts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Navigation {
    Load(String),
    Reload,
    Back,
    Forward,
}

/// What a backend reports about its views, in the order it happened.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    not(all(feature = "web-viewer", target_os = "linux")),
    allow(dead_code, reason = "only a native backend reports events")
)]
pub(crate) enum Event {
    /// The view could not be created, or its browser went away.
    Failed {
        view: u64,
        generation: u64,
        reason: String,
    },
    LoadStarted {
        view: u64,
        generation: u64,
    },
    Loaded {
        view: u64,
        generation: u64,
    },
    /// A function call ([`Backend::evaluate`]) returned its completion
    /// text: [`call_succeeded`] reads it.
    Evaluated {
        view: u64,
        call: u64,
        result: String,
    },
    /// The page posted a message through its bridge.
    Message {
        view: u64,
        body: String,
    },
}

/// Native browser views. Commands apply in order; a command for a view the
/// backend no longer has is ignored.
pub(crate) trait Backend {
    fn create(&mut self, view: u64, parent: NativeParent, spec: ViewSpec);
    fn place(&mut self, view: u64, geometry: Geometry, visible: bool);
    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation);
    fn evaluate(&mut self, view: u64, call: u64, script: String);
    fn destroy(&mut self, view: u64);
    /// The app's window took a click: keyboard focus a view has goes back
    /// to `parent`.
    fn release_focus(&mut self, parent: NativeParent);
    fn poll(&mut self) -> Vec<Event>;
}

/// Why web viewers show no page in a window.
pub(crate) fn unavailable_note(reason: Unavailable) -> &'static str {
    match reason {
        Unavailable::Build => "Web viewer: this OpenMaker build has no web viewer support.",
        Unavailable::Window => "Web viewer: this window has no native window to show it in.",
        Unavailable::Platform => "Web viewer: unavailable on this display.",
    }
}

/// Why interactive containers' audio and video do not play in a window
/// (#200): they play in the web viewer engine.
pub(crate) fn media_note(reason: Unavailable) -> &'static str {
    match reason {
        Unavailable::Build => "No media player in this build.",
        Unavailable::Window => "Plays only in the file's first window.",
        Unavailable::Platform => "No media player on this display.",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// Built without the `web-viewer` feature, or for a platform without a
    /// backend.
    Build,
    /// The window's viewport has no native window of its own that is known
    /// (an embedded viewport, or one whose X11 window was not found:
    /// `viewports.rs`).
    Window,
    /// The backend could not start (no X11 display, a Wayland session).
    Platform,
}

/// Who a view belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewKey {
    pub window: u32,
    pub uuid: String,
    pub file: u64,
    pub epoch: u64,
    pub layout: u32,
    pub object: u64,
    /// The List row's record (`Placement::record`); `None` for a command.
    pub record: Option<i64>,
}

/// The window a frame drew, as reconciling sees it.
pub(crate) struct WindowFrame<'a> {
    pub window: u32,
    pub uuid: &'a str,
    pub file: u64,
    pub epoch: u64,
    /// The layout shown in Browse, `None` in other modes and views.
    pub layout: Option<u32>,
    pub parent: Option<NativeParent>,
    pub pixels_per_point: f32,
    /// Whatever egui drew above the document this frame, in window points.
    pub covered: &'a [egui::Rect],
    pub placements: &'a [Placement],
    /// egui took a pointer press in the window this frame (a press in a
    /// page goes to the page's own window, not egui).
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadState {
    Created,
    Loading,
    Loaded,
    Failed,
}

struct View {
    id: u64,
    key: ViewKey,
    options: WebViewerOptions,
    /// The calculated address the view last loaded: Reset goes back to it,
    /// and a different calculated address loads that one.
    address: String,
    /// Programmatic navigations invalidate queued events from older pages.
    generation: u64,
    state: LoadState,
    placed: (Geometry, bool),
    /// Painted on the current record last frame (`Placement::current`).
    current: bool,
    /// The native window the view is a child of.
    parent: NativeParent,
    seen: bool,
}

/// A command for a view its window has not created yet (the script went to
/// the layout a moment ago): tried again after the window's next frames.
struct Deferred {
    prompt: u64,
    window: ViewKey,
    command: Command,
    frames: u8,
}

/// How many of its window's frames a deferred command waits for its view.
const DEFER_FRAMES: u8 = 2;

/// A Perform JavaScript in Web Viewer waiting for its function.
struct Call {
    id: u64,
    prompt: u64,
    view: u64,
    deadline: std::time::Instant,
}

/// What the worker asks of a web viewer of its window's layout.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    /// Set Web Viewer (146): 1 Reset, 2 Reload, 3 Go Forward, 4 Go Back,
    /// otherwise Go to URL.
    Set {
        layout: u32,
        object: u64,
        action: u16,
        url: Option<String>,
    },
    /// Perform JavaScript in Web Viewer (175): the function and its text
    /// parameters.
    Perform {
        layout: u32,
        object: u64,
        function: String,
        parameters: Vec<String>,
    },
}

/// What came of reconciling and of the backend's events, for the app.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Answer the worker's prompt: `Err` carries the error code.
    Answer {
        prompt: u64,
        result: Result<(), i32>,
    },
    /// A page asked to perform a script of its view's file.
    PerformScript { key: ViewKey, call: ScriptCall },
}

/// `FileMaker.PerformScriptWithOption ( script ; parameter ; option )`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScriptCall {
    pub script: String,
    pub parameter: String,
    /// 0 Continue ... 5 Suspend and Resume (options-for-starting-scripts).
    pub option: u16,
}

type Factory = Box<dyn FnMut(&egui::Context) -> Result<Box<dyn Backend>, Unavailable>>;

/// The app's web viewers.
pub(crate) struct WebViewers {
    /// Whether this build has a backend for the platform.
    supported: bool,
    factory: Option<Factory>,
    backend: Option<Result<Box<dyn Backend>, Unavailable>>,
    views: Vec<View>,
    calls: Vec<Call>,
    deferred: Vec<Deferred>,
    next: u64,
    outcomes: Vec<Outcome>,
}

impl Default for WebViewers {
    fn default() -> Self {
        Self::native()
    }
}

/// Prepare the process for web viewers before any other thread starts
/// (`om_gui::prepare_web_viewers`).
pub(crate) fn prepare_environment() {
    #[cfg(all(feature = "web-viewer", target_os = "linux"))]
    gtk::prepare_environment();
}

/// How the app's event loop must be built for this build's web viewers, if
/// differently from eframe's default (`om_gui::native_options`).
pub(crate) fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    #[cfg(all(feature = "web-viewer", target_os = "linux"))]
    {
        gtk::event_loop_hook()
    }
    #[cfg(not(all(feature = "web-viewer", target_os = "linux")))]
    {
        None
    }
}

/// The platform's backend.
#[allow(unused_variables)]
fn native_backend(ctx: &egui::Context) -> Result<Box<dyn Backend>, Unavailable> {
    #[cfg(all(feature = "web-viewer", target_os = "linux"))]
    {
        gtk::GtkBackend::start(ctx).map(|b| Box::new(b) as Box<dyn Backend>)
    }
    #[cfg(not(all(feature = "web-viewer", target_os = "linux")))]
    {
        Err(Unavailable::Build)
    }
}

impl WebViewers {
    /// Web viewers with the platform's backend, when the build has one.
    fn native() -> Self {
        let mut viewers = Self::with_factory(Box::new(native_backend));
        viewers.supported = cfg!(all(feature = "web-viewer", target_os = "linux"));
        viewers
    }

    /// Web viewers whose native views `factory` makes (tests).
    pub fn with_factory(factory: Factory) -> Self {
        WebViewers {
            supported: true,
            factory: Some(factory),
            backend: None,
            views: Vec::new(),
            calls: Vec::new(),
            deferred: Vec::new(),
            next: 0,
            outcomes: Vec::new(),
        }
    }

    /// Whether pages can show at all: the build has a backend, and it did
    /// not fail to start.
    pub fn hosts_pages(&self) -> bool {
        self.supported && !matches!(self.backend, Some(Err(_)))
    }

    /// Why a window with this native parent shows no pages, if it does not.
    pub fn unavailable(&self, parent: Result<NativeParent, Unavailable>) -> Option<Unavailable> {
        if let Some(Err(reason)) = &self.backend {
            return Some(*reason);
        }
        if !self.supported {
            return Some(Unavailable::Build);
        }
        parent.err()
    }

    fn backend(&mut self, ctx: &egui::Context) -> Option<&mut Box<dyn Backend>> {
        if self.backend.is_none() {
            let factory = self.factory.as_mut()?;
            self.backend = Some(factory(ctx));
        }
        self.backend.as_mut()?.as_mut().ok()
    }

    /// Whether a script waits for a view (the app polls for timeouts).
    pub fn is_waiting(&self) -> bool {
        !self.calls.is_empty() || !self.deferred.is_empty()
    }

    /// The number of live views (diagnostics and tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Match the views of `frame.window` to what Browse painted: create,
    /// place, load and destroy them.
    pub fn reconcile(&mut self, ctx: &egui::Context, frame: &WindowFrame<'_>) {
        for view in &mut self.views {
            view.seen = false;
        }
        let parent = frame.parent;
        for placement in frame.placements {
            let Some(layout) = frame.layout else {
                break;
            };
            let key = ViewKey {
                window: frame.window,
                uuid: frame.uuid.to_owned(),
                file: frame.file,
                epoch: frame.epoch,
                layout,
                object: placement.object,
                record: placement.record,
            };
            let geometry = Geometry::of(
                placement.rect,
                placement.visible,
                frame.pixels_per_point,
                placement.zoom,
            );
            let shown = placement.rect.intersect(placement.visible);
            let covered = frame.covered.iter().any(|c| c.intersects(shown));
            let visible = !geometry.is_empty() && !covered;
            // A view whose window has another native window now (the window
            // moved to the root viewport, or eframe recreated its viewport's
            // window), or none, goes; it is made again in the new one.
            let moved = self
                .views
                .iter()
                .find(|v| v.key == key && Some(v.parent) != parent)
                .map(|v| v.id);
            if let Some(moved) = moved {
                self.destroy(moved);
            }
            if let Some(view) = self.views.iter_mut().find(|v| v.key == key) {
                view.seen = true;
                view.current = placement.current;
                let (id, placed) = (view.id, view.placed);
                let reload = placement
                    .address
                    .as_ref()
                    .filter(|address| **address != view.address)
                    .cloned();
                if let Some(address) = &reload {
                    view.address.clone_from(address);
                    view.generation = view.generation.wrapping_add(1);
                    view.state = LoadState::Loading;
                }
                let (options, generation) = (view.options, view.generation);
                if reload.is_some() {
                    self.fail_calls(id, INVALID);
                }
                let Some(backend) = self.backend.as_mut().and_then(|b| b.as_mut().ok()) else {
                    continue;
                };
                if let Some(address) = reload {
                    backend.navigate(id, generation, Navigation::Load(url_for(&address, options)));
                }
                if placed != (geometry, visible) {
                    backend.place(id, geometry, visible);
                    if let Some(view) = self.views.iter_mut().find(|v| v.id == id) {
                        view.placed = (geometry, visible);
                    }
                }
                continue;
            }
            let (Some(parent), Some(address)) = (parent, placement.address.clone()) else {
                continue;
            };
            let id = self.next + 1;
            let Some(backend) = self.backend(ctx) else {
                continue;
            };
            backend.create(
                id,
                parent,
                ViewSpec {
                    url: url_for(&address, placement.options),
                    geometry,
                    visible,
                    interactive: placement.options.allows_interaction(),
                    scripts: placement.options.allows_javascript_scripts(),
                },
            );
            self.next = id;
            self.views.push(View {
                id,
                key,
                options: placement.options,
                address,
                generation: 0,
                state: LoadState::Created,
                placed: (geometry, visible),
                current: placement.current,
                parent,
                seen: true,
            });
        }
        if let (true, Some(parent)) = (frame.pressed, parent) {
            let hosts = self
                .views
                .iter()
                .any(|v| v.key.window == frame.window && v.key.uuid == frame.uuid);
            if let (true, Some(Ok(backend))) = (hosts, self.backend.as_mut()) {
                backend.release_focus(parent);
            }
        }
        let gone: Vec<u64> = self
            .views
            .iter()
            .filter(|v| v.key.window == frame.window && v.key.uuid == frame.uuid && !v.seen)
            .map(|v| v.id)
            .collect();
        for id in gone {
            self.destroy(id);
        }
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|d| d.window.window == frame.window && d.window.uuid == frame.uuid);
        self.deferred = others;
        for mut deferred in mine {
            deferred.frames += 1;
            if let Err(command) =
                self.try_command(deferred.prompt, &deferred.window, deferred.command)
            {
                if deferred.frames >= DEFER_FRAMES {
                    self.outcomes.push(Outcome::Answer {
                        prompt: deferred.prompt,
                        result: Err(OBJECT_MISSING),
                    });
                } else {
                    self.deferred.push(Deferred {
                        command,
                        ..deferred
                    });
                }
            }
        }
    }

    /// Destroy the views of windows that are gone (closed, or of an earlier
    /// session): `live` are the open windows' ids and UUIDs.
    pub fn retain_windows(&mut self, epoch: u64, live: &[(u32, String)]) {
        let gone: Vec<u64> = self
            .views
            .iter()
            .filter(|v| {
                v.key.epoch != epoch
                    || !live
                        .iter()
                        .any(|(id, uuid)| *id == v.key.window && *uuid == v.key.uuid)
            })
            .map(|v| v.id)
            .collect();
        for id in gone {
            self.destroy(id);
        }
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|d| {
                d.window.epoch == epoch
                    && live
                        .iter()
                        .any(|(id, uuid)| *id == d.window.window && *uuid == d.window.uuid)
            });
        self.deferred = kept;
        for deferred in gone {
            self.outcomes.push(Outcome::Answer {
                prompt: deferred.prompt,
                result: Err(OBJECT_MISSING),
            });
        }
    }

    fn destroy(&mut self, id: u64) {
        self.views.retain(|v| v.id != id);
        if let Some(Ok(backend)) = self.backend.as_mut() {
            backend.destroy(id);
        }
        self.fail_calls(id, OBJECT_MISSING);
    }

    fn fail_calls(&mut self, view: u64, code: i32) {
        let failed: Vec<u64> = self
            .calls
            .iter()
            .filter(|c| c.view == view)
            .map(|c| c.prompt)
            .collect();
        self.calls.retain(|c| c.view != view);
        for prompt in failed {
            self.outcomes.push(Outcome::Answer {
                prompt,
                result: Err(code),
            });
        }
    }

    /// Carry out the worker's request for window `window`'s web viewer
    /// `command` names, answering prompt `prompt` now, when its function
    /// returns, or (when the window has not made the view yet) after the
    /// window's next frames.
    pub fn command(&mut self, prompt: u64, window: &ViewKey, command: Command) {
        if let Err(command) = self.try_command(prompt, window, command) {
            self.deferred.push(Deferred {
                prompt,
                window: window.clone(),
                command,
                frames: 0,
            });
        }
    }

    /// [`Self::command`], or the command back when there is no such view.
    fn try_command(
        &mut self,
        prompt: u64,
        window: &ViewKey,
        command: Command,
    ) -> Result<(), Command> {
        let (layout, object) = match &command {
            Command::Set { layout, object, .. } | Command::Perform { layout, object, .. } => {
                (*layout, *object)
            }
        };
        let view = self.views.iter().find(|v| {
            v.key.window == window.window
                && v.key.uuid == window.uuid
                && v.key.file == window.file
                && v.key.epoch == window.epoch
                && v.key.layout == layout
                && v.key.object == object
                && v.current
        });
        let answer = |result| Outcome::Answer { prompt, result };
        let Some(view) = view else {
            return Err(command);
        };
        if view.state == LoadState::Failed {
            self.outcomes.push(answer(Err(OBJECT_MISSING)));
            return Ok(());
        }
        let (id, options, state, home) = (view.id, view.options, view.state, view.address.clone());
        if !matches!(self.backend.as_ref(), Some(Ok(_))) {
            self.outcomes.push(answer(Err(OBJECT_MISSING)));
            return Ok(());
        }
        match command {
            Command::Set { action, url, .. } => {
                let navigation = match action {
                    1 => Navigation::Load(url_for(&home, options)),
                    2 => Navigation::Reload,
                    3 => Navigation::Forward,
                    4 => Navigation::Back,
                    _ => match url.filter(|u| !u.trim().is_empty()) {
                        Some(url) => Navigation::Load(url_for(&url, options)),
                        None => {
                            self.outcomes.push(answer(Err(INVALID)));
                            return Ok(());
                        }
                    },
                };
                let view = self.views.iter_mut().find(|v| v.id == id).unwrap();
                view.generation = view.generation.wrapping_add(1);
                view.state = LoadState::Loading;
                let generation = view.generation;
                self.fail_calls(id, INVALID);
                self.backend
                    .as_mut()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .navigate(id, generation, navigation);
                self.outcomes.push(answer(Ok(())));
            }
            Command::Perform {
                function,
                parameters,
                ..
            } => {
                // The page must have finished loading (Claris: the function
                // does not run otherwise).
                if state != LoadState::Loaded {
                    self.outcomes.push(answer(Err(INVALID)));
                    return Ok(());
                }
                let Some(script) = call_script(&function, &parameters) else {
                    self.outcomes.push(answer(Err(INVALID)));
                    return Ok(());
                };
                self.next += 1;
                let call = self.next;
                self.backend
                    .as_mut()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .evaluate(id, call, script);
                self.calls.push(Call {
                    id: call,
                    prompt,
                    view: id,
                    deadline: std::time::Instant::now() + CALL_TIMEOUT,
                });
            }
        }
        Ok(())
    }

    /// Fold in what the backend reported, and time out calls; the outcomes
    /// since the last time.
    pub fn poll(&mut self) -> Vec<Outcome> {
        let events = match self.backend.as_mut() {
            Some(Ok(backend)) => backend.poll(),
            _ => Vec::new(),
        };
        for event in events {
            match event {
                Event::Failed {
                    view, generation, ..
                } => {
                    if let Some(v) = self
                        .views
                        .iter_mut()
                        .find(|v| v.id == view && v.generation == generation)
                    {
                        v.state = LoadState::Failed;
                        self.fail_calls(view, OBJECT_MISSING);
                    }
                }
                Event::LoadStarted { view, generation } => {
                    if let Some(v) = self.views.iter_mut().find(|v| {
                        v.id == view && v.generation == generation && v.state != LoadState::Failed
                    }) {
                        v.state = LoadState::Loading;
                        self.fail_calls(view, INVALID);
                    }
                }
                Event::Loaded { view, generation } => {
                    if let Some(v) = self.views.iter_mut().find(|v| {
                        v.id == view && v.generation == generation && v.state != LoadState::Failed
                    }) {
                        v.state = LoadState::Loaded;
                    }
                }
                Event::Evaluated { view, call, result } => {
                    let Some(at) = self
                        .calls
                        .iter()
                        .position(|c| c.id == call && c.view == view)
                    else {
                        continue;
                    };
                    let call = self.calls.remove(at);
                    self.outcomes.push(Outcome::Answer {
                        prompt: call.prompt,
                        result: if call_succeeded(&result) {
                            Ok(())
                        } else {
                            Err(INVALID)
                        },
                    });
                }
                Event::Message { view, body } => {
                    // Only a live view whose object allows it performs
                    // scripts; anything else (a destroyed view's last
                    // message, a page without the option) is dropped.
                    let Some(v) = self.views.iter().find(|v| v.id == view) else {
                        continue;
                    };
                    if !v.options.allows_javascript_scripts() {
                        continue;
                    }
                    if let Some(call) = parse_message(&body) {
                        self.outcomes.push(Outcome::PerformScript {
                            key: v.key.clone(),
                            call,
                        });
                    }
                }
            }
        }
        let now = std::time::Instant::now();
        let late: Vec<u64> = self
            .calls
            .iter()
            .filter(|c| c.deadline <= now)
            .map(|c| c.prompt)
            .collect();
        self.calls.retain(|c| c.deadline > now);
        for prompt in late {
            self.outcomes.push(Outcome::Answer {
                prompt,
                result: Err(INVALID),
            });
        }
        std::mem::take(&mut self.outcomes)
    }
}

/// The URL a view loads for `address`: "Automatically encode URL" encodes
/// what a URL cannot hold, as Insert from URL does; in a `data:` URL also
/// `#`, which would otherwise cut its content off as a fragment. An empty
/// address shows a blank page.
pub(crate) fn url_for(address: &str, options: WebViewerOptions) -> String {
    let address = address.trim();
    if address.is_empty() {
        return "about:blank".into();
    }
    if !options.encodes_url() {
        return address.to_owned();
    }
    let encoded = om_url::encode_url(address);
    if address
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
    {
        encoded.replace('#', "%23")
    } else {
        encoded
    }
}

/// The `FileMaker` object a page gets when its web viewer allows
/// JavaScript to perform FileMaker scripts. Its functions post the script
/// name, parameter and option (each `encodeURIComponent`d, `&`-separated)
/// to the view's `openmaker` message handler; nothing comes back to the
/// page.
#[cfg_attr(
    not(all(feature = "web-viewer", target_os = "linux")),
    allow(dead_code, reason = "only a native backend injects it")
)]
pub(crate) const BRIDGE: &str = r#"(function () {
  if (window.FileMaker) { return; }
  function text(value, fallback) {
    return value === undefined || value === null ? fallback : String(value);
  }
  function post(script, parameter, option) {
    var body = "openmaker-perform-script&s=" + encodeURIComponent(text(script, "")) +
      "&p=" + encodeURIComponent(text(parameter, "")) +
      "&o=" + encodeURIComponent(text(option, "0"));
    window.webkit.messageHandlers.openmaker.postMessage(body);
  }
  var fileMaker = Object.freeze({
    PerformScript: function (script, parameter) { post(script, parameter, "0"); },
    PerformScriptWithOption: function (script, parameter, option) {
      post(script, parameter, option);
    }
  });
  Object.defineProperty(window, "FileMaker", {
    value: fileMaker, writable: false, configurable: false, enumerable: true
  });
})();"#;

/// A bridge message: the script, parameter and option a page passed. The
/// option is the first character's digit (`"3"` is Resume); anything else
/// is 0, Continue.
pub(crate) fn parse_message(body: &str) -> Option<ScriptCall> {
    if body.len() > MAX_MESSAGE {
        return None;
    }
    let rest = body.strip_prefix("openmaker-perform-script&")?;
    let (mut script, mut parameter, mut option) = (None, None, None);
    for part in rest.split('&') {
        let (name, value) = part.split_once('=')?;
        let value = percent_decode(value)?;
        match name {
            "s" => script = Some(value),
            "p" => parameter = Some(value),
            "o" => option = Some(value),
            _ => return None,
        }
    }
    let option = option
        .as_deref()
        .and_then(|o| o.trim().parse::<u16>().ok())
        .filter(|o| *o <= 5)
        .unwrap_or(0);
    Some(ScriptCall {
        script: script?,
        parameter: parameter.unwrap_or_default(),
        option,
    })
}

/// `%XX` escapes of UTF-8, as `encodeURIComponent` writes them; `None` for
/// malformed escapes or bytes that are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The script that calls `function` (a global function, or a dotted path to
/// a method: `app.update`) with text parameters and completes with `"ok"`,
/// `"missing"` (no such function) or `"error"` (it threw). `None` for an
/// empty or malformed function name.
pub(crate) fn call_script(function: &str, parameters: &[String]) -> Option<String> {
    let function = function.trim();
    if function.is_empty() || function.split('.').any(str::is_empty) {
        return None;
    }
    let path: Vec<String> = function.split('.').map(js_string).collect();
    let arguments: Vec<String> = parameters.iter().map(|p| js_string(p)).collect();
    Some(format!(
        "(function () {{ try {{ var path = [{}], owner = window, target = window; \
         for (var i = 0; i < path.length; i++) {{ owner = target; \
         target = target == null ? undefined : target[path[i]]; }} \
         if (typeof target !== \"function\") {{ return \"missing\"; }} \
         target.apply(owner, [{}]); return \"ok\"; }} catch (e) {{ return \"error\"; }} }})()",
        path.join(", "),
        arguments.join(", ")
    ))
}

/// Whether a call's completion text (the JSON of [`call_script`]'s value)
/// says the function ran.
pub(crate) fn call_succeeded(result: &str) -> bool {
    result.trim() == "\"ok\""
}

/// `text` as a JavaScript string literal.
pub(crate) fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
pub(crate) mod tests;
