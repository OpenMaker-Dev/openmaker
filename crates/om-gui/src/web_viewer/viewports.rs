//! The native window of each window's own viewport, for its web viewers.
//!
//! eframe 0.37 gives out the root viewport's window handle only
//! (`eframe::Frame`); the winit windows of the immediate viewports that
//! show the app's other windows (`app.rs`) stay private. On X11 their
//! window ids come from the server instead, without guessing from titles:
//!
//! - eframe creates an immediate viewport's window inside
//!   `show_viewport_immediate`, before it runs the viewport's code, and
//!   winit waits until the server has created it;
//! - an X11 window id carries the resource-id base of the connection that
//!   created it, and the root viewport's window shows the base of eframe's
//!   connection;
//! - a connection of our own selects SubstructureNotify on the screen's root
//!   window, so the server reports every new top-level window to it, in
//!   order, before it answers a later round trip.
//!
//! So a top-level window that eframe's connection created between the round
//! trip just before a viewport's `show_viewport_immediate` and the one at
//! the start of its code is that viewport's window. eframe creates one; a
//! title that matches breaks a tie should there be several. When that does
//! not settle it (a backlog of events too long to read at once leaves the
//! moment unknown), the one window eframe made that no viewport has, with
//! the viewport's title, is taken; anything else leaves the window without
//! one (its web viewers say so). A window
//! eframe destroys (it recreates a viewport's window for some changes of
//! its builder) is forgotten when the server reports it, and the next one
//! created for the viewport takes its place.
//!
//! On macOS (with the `web-viewer` feature) the same moment is read from
//! AppKit: winit makes a window on the main thread as eframe asks, so the
//! window in the app's list (`NSApplication.windows`) at the start of a
//! viewport's code that was not there just before it was shown is that
//! viewport's. A window that closes leaves the list and is forgotten; the
//! one made after it takes its place. When the moment shows no one window
//! (none, or several), the one window no viewport has, other than the root's,
//! with the viewport's title is taken; anything else leaves the window
//! without one.

use std::collections::BTreeMap;

use super::{NativeParent, Unavailable};

/// The app windows' (by id) own native windows, found as the module doc
/// says. The root viewport's comes from eframe.
#[derive(Default)]
pub(crate) struct ViewportWindows {
    #[cfg(target_os = "linux")]
    watch: Watching,
    windows: BTreeMap<u32, u32>,
    /// The windows' AppKit window numbers, the root's, and the app's
    /// windows just before a viewport was shown.
    #[cfg(all(feature = "web-viewer", target_os = "macos"))]
    numbers: BTreeMap<u32, isize>,
    #[cfg(all(feature = "web-viewer", target_os = "macos"))]
    root: Option<isize>,
    #[cfg(all(feature = "web-viewer", target_os = "macos"))]
    before: Vec<isize>,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
enum Watching {
    #[default]
    NotYet,
    On(Box<x11::Watch>),
    /// The display could not be watched, or failed while it was: no other
    /// window gets a native window this session.
    Failed,
}

impl ViewportWindows {
    /// Start a frame whose root viewport's window is `root`. The server is
    /// watched from the first frame on which web viewers can use the
    /// windows (`hosts_pages`), which comes before any other window's.
    pub fn begin(&mut self, root: Result<NativeParent, Unavailable>, hosts_pages: bool) {
        #[cfg(target_os = "linux")]
        {
            if let (Watching::NotYet, true, Ok(NativeParent::X11(root))) =
                (&self.watch, hosts_pages, root)
            {
                self.watch = match u32::try_from(root).ok().map(x11::Watch::start) {
                    Some(Ok(watch)) => Watching::On(Box::new(watch)),
                    _ => Watching::Failed,
                };
            }
            // Read what the server reported, so that nothing piles up
            // while no other window is shown.
            self.read(false);
        }
        #[cfg(all(feature = "web-viewer", target_os = "macos"))]
        {
            let _ = hosts_pages;
            self.root = match root {
                Ok(NativeParent::AppKit(number)) => Some(number),
                _ => None,
            };
        }
        #[cfg(not(any(
            target_os = "linux",
            all(feature = "web-viewer", target_os = "macos")
        )))]
        let _ = (root, hosts_pages);
    }

    /// Just before a window's viewport is shown: windows created from here
    /// on, until its code starts, are its own.
    pub fn before_show(&mut self) {
        #[cfg(target_os = "linux")]
        if self.read(true) {
            if let Watching::On(watch) = &mut self.watch {
                // Events left unread make the moment unknown.
                watch.created.clear();
                watch.overflow = watch.backlog;
            }
        }
        #[cfg(all(feature = "web-viewer", target_os = "macos"))]
        {
            self.before = super::appkit::windows().into_iter().map(|w| w.0).collect();
        }
    }

    /// At the start of window `id`'s viewport code: its native window, or
    /// why it has none. `title` is the title its viewport was shown with.
    pub fn parent(&mut self, id: u32, title: &str) -> Result<NativeParent, Unavailable> {
        #[cfg(target_os = "linux")]
        {
            if !self.read(true) {
                return Err(Unavailable::Window);
            }
            let Watching::On(watch) = &mut self.watch else {
                return Err(Unavailable::Window);
            };
            let created = std::mem::take(&mut watch.created);
            let overflow = std::mem::take(&mut watch.overflow);
            if let Some(window) = self.windows.get(&id) {
                return Ok(NativeParent::X11(u64::from(*window)));
            }
            let mut candidates = if overflow { Vec::new() } else { created };
            if candidates.len() > 1 {
                candidates.retain(|w| watch.title(*w).as_deref() == Some(title));
            }
            if candidates.len() != 1 {
                let owned: Vec<u32> = self.windows.values().copied().collect();
                candidates = watch
                    .alive
                    .iter()
                    .copied()
                    .filter(|w| !owned.contains(w))
                    .filter(|w| watch.title(*w).as_deref() == Some(title))
                    .collect();
            }
            let [window] = candidates[..] else {
                return Err(Unavailable::Window);
            };
            if watch.follow(window).is_err() {
                self.watch = Watching::Failed;
                self.windows.clear();
                return Err(Unavailable::Window);
            }
            self.windows.insert(id, window);
            Ok(NativeParent::X11(u64::from(window)))
        }
        #[cfg(all(feature = "web-viewer", target_os = "macos"))]
        {
            let now = super::appkit::windows();
            let open = |number: isize| now.iter().any(|w| w.0 == number);
            self.numbers.retain(|_, number| open(*number));
            if let Some(number) = self.numbers.get(&id) {
                return Ok(NativeParent::AppKit(*number));
            }
            let owned: Vec<isize> = self.numbers.values().copied().chain(self.root).collect();
            let mine = |number: &isize| !owned.contains(number);
            let created: Vec<isize> = now
                .iter()
                .map(|w| w.0)
                .filter(|number| !self.before.contains(number))
                .filter(mine)
                .collect();
            let window = match created[..] {
                [window] => window,
                _ => {
                    let titled: Vec<isize> = now
                        .iter()
                        .filter(|w| w.1 == title)
                        .map(|w| w.0)
                        .filter(mine)
                        .collect();
                    let [window] = titled[..] else {
                        return Err(Unavailable::Window);
                    };
                    window
                }
            };
            self.numbers.insert(id, window);
            Ok(NativeParent::AppKit(window))
        }
        #[cfg(not(any(
            target_os = "linux",
            all(feature = "web-viewer", target_os = "macos")
        )))]
        {
            let _ = (id, title);
            Err(Unavailable::Window)
        }
    }

    /// Forget the windows that are no longer open.
    pub fn retain(&mut self, open: impl Fn(u32) -> bool) {
        self.windows.retain(|id, _| open(*id));
        #[cfg(all(feature = "web-viewer", target_os = "macos"))]
        self.numbers.retain(|id, _| open(*id));
    }

    /// Take in what the server reported (after a round trip with `sync`):
    /// windows eframe created, and windows that went. `false` when the
    /// display is not watched.
    #[cfg(target_os = "linux")]
    fn read(&mut self, sync: bool) -> bool {
        let Watching::On(watch) = &mut self.watch else {
            return false;
        };
        let windows = &mut self.windows;
        let read = watch.read(sync, |gone| windows.retain(|_, w| *w != gone));
        if read.is_err() {
            self.watch = Watching::Failed;
            self.windows.clear();
            return false;
        }
        true
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask,
    };
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    /// Top-level windows one viewport may create before its code runs and
    /// still be told apart (eframe creates one).
    const MAX_CREATED: usize = 16;
    /// The most events one read takes in: the rest wait for the next.
    const MAX_EVENTS: usize = 4096;
    /// The most of eframe's live windows remembered for the fallback.
    const MAX_ALIVE: usize = 256;
    /// The longest title read to break a tie, in bytes.
    const MAX_TITLE: u32 = 4096;

    type Failed = Box<dyn std::error::Error>;

    pub(super) struct Watch {
        conn: RustConnection,
        /// The screen's root window, and the root viewport's window.
        screen: u32,
        app: u32,
        /// The resource-id base of eframe's connection, and the server's
        /// mask of the bits a connection allocates.
        base: u32,
        mask: u32,
        net_wm_name: u32,
        utf8_string: u32,
        /// Top-level windows eframe created since the last mark, in order.
        pub created: Vec<u32>,
        /// More were created than `created` keeps, or events were left
        /// unread: which window is whose is not known from the order.
        pub overflow: bool,
        /// The last read stopped at [`MAX_EVENTS`] with events left.
        pub backlog: bool,
        /// Top-level windows eframe created that still exist (the most
        /// [`MAX_ALIVE`]).
        pub alive: Vec<u32>,
    }

    impl Watch {
        /// Watch the screen of `app`, the root viewport's window.
        pub fn start(app: u32) -> Result<Self, Failed> {
            let (conn, _) = x11rb::connect(None)?;
            let mask = conn.setup().resource_id_mask;
            let screen = conn.query_tree(app)?.reply()?.root;
            conn.change_window_attributes(
                screen,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
            )?
            .check()?;
            let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
            let utf8_string = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
            Ok(Watch {
                conn,
                screen,
                app,
                base: app & !mask,
                mask,
                net_wm_name,
                utf8_string,
                created: Vec::new(),
                overflow: false,
                backlog: false,
                alive: Vec::new(),
            })
        }

        /// Take in the reported events; with `sync`, after a round trip, so
        /// everything the server did before it is among them. `gone` hears
        /// of each window destroyed.
        pub fn read(&mut self, sync: bool, mut gone: impl FnMut(u32)) -> Result<(), Failed> {
            if sync {
                self.conn.sync()?;
            }
            self.backlog = true;
            for _ in 0..MAX_EVENTS {
                let Some(event) = self.conn.poll_for_event()? else {
                    self.backlog = false;
                    break;
                };
                match event {
                    Event::CreateNotify(e)
                        if e.parent == self.screen
                            && e.window & !self.mask == self.base
                            && e.window != self.app =>
                    {
                        if self.created.len() < MAX_CREATED {
                            self.created.push(e.window);
                        } else {
                            self.overflow = true;
                        }
                        if self.alive.len() < MAX_ALIVE {
                            self.alive.push(e.window);
                        }
                    }
                    Event::DestroyNotify(e) => {
                        self.created.retain(|w| *w != e.window);
                        self.alive.retain(|w| *w != e.window);
                        gone(e.window);
                    }
                    _ => {}
                }
            }
            if self.backlog {
                self.overflow = true;
            }
            Ok(())
        }

        /// Hear of `window`'s destruction once a window manager has
        /// reparented it (the screen's root no longer reports it then).
        pub fn follow(&self, window: u32) -> Result<(), Failed> {
            self.conn
                .change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
                )?
                .check()?;
            Ok(())
        }

        /// `window`'s title (`_NET_WM_NAME`, else `WM_NAME`).
        pub fn title(&self, window: u32) -> Option<String> {
            let read = |property: u32, kind: u32| {
                let reply = self
                    .conn
                    .get_property(false, window, property, kind, 0, MAX_TITLE / 4)
                    .ok()?
                    .reply()
                    .ok()?;
                (reply.format == 8 && reply.type_ == kind).then_some(reply.value)
            };
            let bytes = read(self.net_wm_name, self.utf8_string)
                .or_else(|| read(AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))?;
            String::from_utf8(bytes).ok()
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
