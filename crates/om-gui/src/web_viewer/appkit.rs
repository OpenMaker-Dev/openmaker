//! Web viewers on macOS: WKWebView through wry. AppKit views live on the
//! main thread, where the app's frames run, so commands apply as they come,
//! except that a view is made at the end of the frame ([`Backend::flush`]),
//! where the root window's handle is to hand; until then, commands wait
//! behind it in order. WebKit loads and paints pages in its own processes
//! and reports back through wry's handlers, which send events over a
//! channel with a repaint request.
//!
//! wry makes a view as a child of a window it is given a handle of, and
//! eframe gives out the root viewport's only. So each page is made in the
//! root window, then moved into a clipping view of its own (`container`) in
//! the content view of the window it belongs to: the container shows the
//! object's visible part, and the page lies in it at its offset, as the
//! X11 child window does on Linux (`gtk.rs`). With "Allow interaction with
//! web viewer content" off, a plain view (`shield`) lies over the page:
//! clicks and scrolling go past it to the window, not the page.
//!
//! Views are incognito (no cookies or storage written to disk) and have no
//! developer tools; they open no other windows, download nothing, and go
//! only to http, https, data, blob, file and about URLs. The bridge
//! ([`BRIDGE`]) reaches the app by asking a hidden frame to load an
//! `openmaker-message:` URL, which the navigation handler takes and
//! refuses: wry's message handler, like WebKitGTK's through wry, drops
//! messages from pages whose URL has no host, such as `data:` URLs.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNumber, NSObject, NSObjectNSKeyValueCoding as _, NSString};
use raw_window_handle::HasWindowHandle;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{PageLoadEvent, WebView, WebViewBuilder, WebViewExtMacOS};

use super::{allowed, Backend, Event, Geometry, NativeParent, Navigation, Unavailable, ViewSpec};
use super::{BRIDGE, MESSAGE_URL};

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

pub(crate) struct AppKitBackend {
    mtm: MainThreadMarker,
    queued: Vec<Command>,
    views: HashMap<u64, Hosted>,
    events: Events,
    received: Receiver<Event>,
}

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
    /// The number of the window the view is in.
    window: isize,
    container: Retained<NSView>,
    shield: Option<Retained<NSView>>,
    webview: WebView,
    generation: Rc<Cell<u64>>,
    active_generation: Rc<Cell<u64>>,
    shown: bool,
    zoom: f64,
}

impl AppKitBackend {
    /// The backend, on the main thread, where AppKit's views live.
    pub fn start(ctx: &egui::Context) -> Result<Self, Unavailable> {
        let mtm = MainThreadMarker::new().ok_or(Unavailable::Platform)?;
        let (sender, received) = channel();
        Ok(AppKitBackend {
            mtm,
            queued: Vec::new(),
            views: HashMap::new(),
            events: Events {
                sender,
                ctx: ctx.clone(),
            },
            received,
        })
    }

    /// Apply `command` now, or after the views still to be made.
    fn command(&mut self, command: Command) {
        if self.queued.is_empty() {
            self.apply(command);
        } else {
            self.queued.push(command);
        }
    }

    /// Apply a command other than [`Command::Create`].
    fn apply(&mut self, command: Command) {
        match command {
            Command::Create { view, .. } => self.events.send(Event::Failed {
                view,
                generation: 0,
                reason: "made without a window".into(),
            }),
            Command::Place {
                view,
                geometry,
                visible,
            } => {
                let mtm = self.mtm;
                if let Some(hosted) = self.views.get_mut(&view) {
                    place(mtm, hosted, geometry, visible);
                }
            }
            Command::Navigate {
                view,
                generation,
                navigation,
            } => self.go(view, generation, navigation),
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
            Command::Destroy { view } => {
                if let Some(hosted) = self.views.remove(&view) {
                    hosted.container.removeFromSuperview();
                }
            }
            Command::ReleaseFocus { parent } => self.unfocus(parent),
        }
    }

    fn go(&mut self, view: u64, generation: u64, navigation: Navigation) {
        let Some(hosted) = self.views.get(&view) else {
            return;
        };
        hosted.generation.set(generation);
        let webview = &hosted.webview;
        let page = webview.webview();
        let unchanged = matches!(&navigation, Navigation::Back) && !flag(&page, "canGoBack")
            || matches!(&navigation, Navigation::Forward) && !flag(&page, "canGoForward");
        if unchanged {
            // WebKit reports no load for a history no-op. Adopt a load in
            // flight too, so that its end belongs to the latest generation.
            hosted.active_generation.set(generation);
            self.events.send(if flag(&page, "loading") {
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

    fn make(
        &mut self,
        view: u64,
        parent: NativeParent,
        spec: ViewSpec,
        root: &dyn HasWindowHandle,
    ) -> Result<(), String> {
        let NativeParent::AppKit(number) = parent else {
            return Err("not an AppKit window".into());
        };
        let window = window_numbered(self.mtm, number).ok_or("the window is gone")?;
        let content = window
            .contentView()
            .ok_or("the window has no content view")?;

        let generation = Rc::new(Cell::new(0));
        let requested = Rc::clone(&generation);
        let active_generation = Rc::new(Cell::new(0));
        let active = Rc::clone(&active_generation);
        let loads = self.events.clone();
        let messages = self.events.clone();
        let scripts = spec.scripts;
        let mut builder = WebViewBuilder::new()
            .with_url(spec.url)
            .with_incognito(true)
            .with_devtools(false)
            .with_bounds(wry::Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(1, 1).into(),
            })
            .with_navigation_handler(move |url| match url.strip_prefix(MESSAGE_URL) {
                Some(body) => {
                    if scripts {
                        messages.send(Event::Message {
                            view,
                            body: body.to_owned(),
                        });
                    }
                    false
                }
                None => allowed(&url),
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_on_page_load_handler(move |event, _| match event {
                PageLoadEvent::Started => {
                    active.set(requested.get());
                    loads.send(Event::LoadStarted {
                        view,
                        generation: active.get(),
                    });
                }
                PageLoadEvent::Finished => loads.send(Event::Loaded {
                    view,
                    generation: active.get(),
                }),
            });
        if spec.scripts {
            builder = builder.with_initialization_script(BRIDGE);
        }
        let webview = builder.build_as_child(&root).map_err(|e| e.to_string())?;
        let page = webview.webview();
        let container = NSView::initWithFrame(self.mtm.alloc(), CGRect::ZERO);
        // Views clip what lies outside them up to macOS 13; from macOS 14
        // on, for programs built with its SDK, only when asked to.
        if objc2::available!(macos = 14.0) {
            container.setClipsToBounds(true);
        }
        content.addSubview(&container);
        // Out of the root window's view, into its own.
        container.addSubview(&page);
        let shield = (!spec.interactive).then(|| {
            let shield = NSView::initWithFrame(self.mtm.alloc(), CGRect::ZERO);
            container.addSubview(&shield);
            shield
        });
        let mut hosted = Hosted {
            window: number,
            container,
            shield,
            webview,
            generation,
            active_generation,
            shown: false,
            zoom: 1.0,
        };
        place(self.mtm, &mut hosted, spec.geometry, spec.visible);
        self.views.insert(view, hosted);
        Ok(())
    }

    /// Give the keyboard back to `parent`'s own view when one of its pages
    /// has it.
    fn unfocus(&self, parent: NativeParent) {
        let NativeParent::AppKit(number) = parent else {
            return;
        };
        let Some(window) = window_numbered(self.mtm, number) else {
            return;
        };
        let Some(view) = window
            .firstResponder()
            .and_then(|responder| responder.downcast::<NSView>().ok())
        else {
            return;
        };
        let in_page = self
            .views
            .values()
            .any(|hosted| hosted.window == number && view.isDescendantOf(&hosted.container));
        if in_page {
            if let Some(content) = window.contentView() {
                window.makeFirstResponder(Some(&content));
            }
        }
    }
}

impl Backend for AppKitBackend {
    fn create(&mut self, view: u64, parent: NativeParent, spec: ViewSpec) {
        self.queued.push(Command::Create { view, parent, spec });
    }

    fn place(&mut self, view: u64, geometry: Geometry, visible: bool) {
        self.command(Command::Place {
            view,
            geometry,
            visible,
        });
    }

    fn navigate(&mut self, view: u64, generation: u64, navigation: Navigation) {
        self.command(Command::Navigate {
            view,
            generation,
            navigation,
        });
    }

    fn evaluate(&mut self, view: u64, call: u64, script: String) {
        self.command(Command::Evaluate { view, call, script });
    }

    fn destroy(&mut self, view: u64) {
        self.command(Command::Destroy { view });
    }

    fn release_focus(&mut self, parent: NativeParent) {
        self.command(Command::ReleaseFocus { parent });
    }

    fn poll(&mut self) -> Vec<Event> {
        self.received.try_iter().collect()
    }

    fn flush(&mut self, root: &dyn HasWindowHandle) {
        for command in std::mem::take(&mut self.queued) {
            match command {
                Command::Create { view, parent, spec } => {
                    if let Err(reason) = self.make(view, parent, spec, root) {
                        self.events.send(Event::Failed {
                            view,
                            generation: 0,
                            reason,
                        });
                    }
                }
                command => self.apply(command),
            }
        }
    }
}

/// A boolean property of a page's WKWebView, through key-value coding: its
/// accessors are unsafe in objc2-web-kit, and these properties are key-value
/// compliant (Apple documents them as observable).
fn flag(page: &NSObject, key: &str) -> bool {
    page.valueForKey(&NSString::from_str(key))
        .and_then(|value| value.downcast::<NSNumber>().ok())
        .is_some_and(|number| number.as_bool())
}

/// The app's window numbered `number`, if it is still open.
fn window_numbered(mtm: MainThreadMarker, number: isize) -> Option<Retained<NSWindow>> {
    NSApplication::sharedApplication(mtm)
        .windows()
        .iter()
        .find(|window| window.windowNumber() == number)
}

/// The number of the window whose content view is `view` (the address
/// eframe's window handle gives), if it is one of the app's.
pub(crate) fn window_of_view(view: usize) -> Option<isize> {
    let mtm = MainThreadMarker::new()?;
    NSApplication::sharedApplication(mtm)
        .windows()
        .iter()
        .find(|window| {
            window
                .contentView()
                .is_some_and(|content| Retained::as_ptr(&content) as usize == view)
        })
        .map(|window| window.windowNumber())
}

/// Move and size the container to the visible part and the page within it
/// to its offset, and show or hide them. Geometry comes in physical pixels
/// from the window's top left; AppKit wants points, and a view whose
/// coordinates are not flipped counts from the bottom.
fn place(mtm: MainThreadMarker, hosted: &mut Hosted, geometry: Geometry, visible: bool) {
    let [x, y, width, height] = geometry.frame;
    let shown = visible && width > 0 && height > 0;
    let window = window_numbered(mtm, hosted.window);
    let content = window.as_ref().and_then(|w| w.contentView());
    if let (true, Some(window), Some(content)) = (shown, &window, &content) {
        let scale = window.backingScaleFactor().max(1.0);
        let points = |v: i32| f64::from(v) / scale;
        let (width, height) = (points(width), points(height));
        let top = if content.isFlipped() {
            points(y)
        } else {
            content.frame().size.height - points(y) - height
        };
        hosted.container.setFrame(CGRect::new(
            CGPoint::new(points(x), top),
            CGSize::new(width, height),
        ));
        let [cx, cy, cw, ch] = geometry.content;
        let (page_width, page_height) = (points(cw.max(1)), points(ch.max(1)));
        hosted.webview.webview().setFrame(CGRect::new(
            CGPoint::new(points(cx), height - points(cy) - page_height),
            CGSize::new(page_width, page_height),
        ));
        if let Some(shield) = &hosted.shield {
            shield.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(width, height)));
        }
        if (geometry.zoom - hosted.zoom).abs() > f64::EPSILON {
            let _ = hosted.webview.zoom(geometry.zoom);
            hosted.zoom = geometry.zoom;
        }
    }
    if shown != hosted.shown {
        hosted.container.setHidden(!shown);
        hosted.shown = shown;
    }
}
