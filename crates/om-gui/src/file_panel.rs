//! The platform's own file panels: the open panel of File > Open... and
//! Home's Open, limited to FileMaker files; the save panel of File > Create
//! New... and Home's Create, which names the new file and chooses its
//! folder; and the open panel of the Insert commands
//! ([#50](https://github.com/OpenMaker-Dev/openmaker/issues/50)), which
//! shows every file. FileMaker Pro shows the system's panels for each
//! (Claris Help: Opening files, Creating a FileMaker Pro file, Insert
//! Picture, Insert File).
//!
//! - **macOS:** an `NSOpenPanel` or `NSSavePanel`, modeless, beside the
//!   window; a filter is the extension's content type.
//! - **Linux:** the desktop's own file chooser, through the
//!   xdg-desktop-portal FileChooser interface on the session bus (its
//!   `OpenFile` and `SaveFile`), asked from a thread of its own; a filter is
//!   a glob of the extension. The chooser is the portal backend's: GTK's,
//!   GNOME's or KDE's.
//! - **Windows** and the phone and tablet shells show none yet
//!   ([#826](https://github.com/OpenMaker-Dev/openmaker/issues/826)).
//!
//! Each answers the path the user chose, Cancel, or
//! [`Answer::Unavailable`] when no panel could be shown (no portal on the
//! session bus, or one with no file chooser). There and where the app has
//! no native window (the GUI tests, which drive OpenMaker's own dialogs),
//! the caller asks in its own dialog instead: `open_file::OpenFileDialog`,
//! `create_new`'s save chooser, or the Insert prompt's typed path. The
//! Insert panel offers no "Store only a reference to the file": how
//! FileMaker stores a reference is unmeasured (docs/re/containers.md,
//! "External storage: still open"), and such a step still stops as
//! unsupported.

// Where no panel is shown, requests are made but not read.
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]

use std::path::PathBuf;
use std::sync::mpsc;

/// What a panel asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub kind: Kind,
    /// The command's name ("Open File", "Insert File"): the panel's title.
    pub title: String,
    /// What the panel says above its files (macOS), as "Into field
    /// Tasks::Photo"; empty for none.
    pub message: String,
    /// Only files with this extension can be chosen (open), or are
    /// listed and named (save); `None` for every file.
    pub extension: Option<&'static str>,
    /// The folder shown first; `None` for the platform's choice.
    pub folder: Option<PathBuf>,
    /// The save panel's proposed name ("Untitled.fmp12").
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Choose one existing file.
    Open,
    /// Name a file to write, and its folder.
    Save,
}

impl Request {
    /// An open panel for one existing file.
    pub fn open(title: &str, message: &str) -> Self {
        Self {
            kind: Kind::Open,
            title: title.to_owned(),
            message: message.to_owned(),
            extension: None,
            folder: None,
            name: String::new(),
        }
    }

    /// A save panel proposing `name` in `folder`.
    pub fn save(title: &str, folder: PathBuf, name: String) -> Self {
        Self {
            kind: Kind::Save,
            title: title.to_owned(),
            message: String::new(),
            extension: None,
            folder: Some(folder),
            name,
        }
    }

    /// Starting in `folder`.
    pub fn in_folder(mut self, folder: PathBuf) -> Self {
        self.folder = Some(folder);
        self
    }

    /// Only files with `extension`.
    pub fn of_kind(mut self, extension: &'static str) -> Self {
        self.extension = Some(extension);
        self
    }
}

/// The native window a panel shows beside, as the app's frame reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parent {
    AppKit,
    /// An X11 window, by its id.
    X11(std::ffi::c_ulong),
    /// A Wayland surface: the portal is told no parent, since naming one
    /// needs the xdg-foreign protocol.
    Wayland,
}

impl Parent {
    /// The frame's native window; `None` without one (the GUI tests) or on
    /// a platform with no panel.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub(crate) fn of(frame: &eframe::Frame) -> Option<Parent> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match frame.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(_) => Some(Parent::AppKit),
            RawWindowHandle::Xlib(handle) => Some(Parent::X11(handle.window)),
            RawWindowHandle::Xcb(handle) => Some(Parent::X11(handle.window.get().into())),
            RawWindowHandle::Wayland(_) => Some(Parent::Wayland),
            _ => None,
        }
    }
}

/// A panel's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Chosen {
        path: PathBuf,
        /// Shift or Option (Alt) was held as the panel answered:
        /// FileMaker's gesture for choosing the account a file opens with
        /// (docs/re/local-files.md 3.1). The panel has the keyboard, so the
        /// window's own input cannot see it.
        asking: bool,
    },
    Canceled,
    /// No panel could be shown: the caller asks in its own dialog.
    Unavailable,
}

/// A panel the user is answering. Dropping it closes the panel (what it
/// answers went away: a script was halted, or the window closed).
pub(crate) struct FilePanel {
    answer: mpsc::Receiver<Answer>,
    /// The panel on screen, kept for its drop, which closes it; `None` for
    /// a test's panel, which only answers.
    _shown: Option<Shown>,
}

impl FilePanel {
    /// Show `request`'s panel beside `parent`. `wake` is called once the
    /// user has answered (or no panel could be shown). `None` where this
    /// platform has no panel to show.
    pub(crate) fn show(
        request: &Request,
        parent: Parent,
        wake: impl Fn() + Send + 'static,
    ) -> Option<FilePanel> {
        let (send, answer) = mpsc::channel();
        let shown = platform_show(request, parent, send, wake)?;
        Some(FilePanel {
            answer,
            _shown: Some(shown),
        })
    }

    /// A panel that has answered `answer`, showing nothing.
    #[cfg(test)]
    pub(crate) fn answered(answer: Answer) -> FilePanel {
        let (send, receive) = mpsc::channel();
        let _ = send.send(answer);
        FilePanel {
            answer: receive,
            _shown: None,
        }
    }

    /// The user's answer once given; `None` while the panel is open.
    pub(crate) fn answer(&self) -> Option<Answer> {
        match self.answer.try_recv() {
            Ok(answer) => Some(answer),
            Err(mpsc::TryRecvError::Empty) => None,
            // The panel went without answering.
            Err(mpsc::TryRecvError::Disconnected) => Some(Answer::Unavailable),
        }
    }
}

/// The panel on screen, to close it with.
struct Shown {
    #[cfg(target_os = "macos")]
    panel: objc2::rc::Retained<objc2_app_kit::NSSavePanel>,
    #[cfg(target_os = "linux")]
    request: std::sync::Arc<std::sync::Mutex<portal::Shown>>,
}

#[cfg(target_os = "macos")]
fn platform_show(
    request: &Request,
    parent: Parent,
    send: mpsc::Sender<Answer>,
    wake: impl Fn() + Send + 'static,
) -> Option<Shown> {
    use objc2::rc::{Retained, Weak};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSEvent, NSEventModifierFlags, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSSavePanel,
    };
    use objc2_foundation::{NSArray, NSString, NSURL};
    use objc2_uniform_type_identifiers::UTType;
    if parent != Parent::AppKit {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let panel: Retained<NSSavePanel> = match request.kind {
        Kind::Open => {
            let open = NSOpenPanel::openPanel(mtm);
            open.setCanChooseFiles(true);
            open.setCanChooseDirectories(false);
            open.setAllowsMultipleSelection(false);
            open.setResolvesAliases(true);
            Retained::into_super(open)
        }
        Kind::Save => {
            let save = NSSavePanel::savePanel(mtm);
            save.setCanCreateDirectories(true);
            save.setNameFieldStringValue(&NSString::from_str(&request.name));
            save
        }
    };
    panel.setTitle(Some(&NSString::from_str(&request.title)));
    if !request.message.is_empty() {
        panel.setMessage(Some(&NSString::from_str(&request.message)));
    }
    if let Some(folder) = request.folder.as_deref().and_then(|f| f.to_str()) {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(folder), true);
        panel.setDirectoryURL(Some(&url));
    }
    if let Some(extension) = request.extension {
        // A dynamic type when no app declares the extension: it still
        // limits the panel to files named with it.
        if let Some(kind) = UTType::typeWithFilenameExtension(&NSString::from_str(extension)) {
            panel.setAllowedContentTypes(&NSArray::from_retained_slice(&[kind]));
        }
    }
    let kind = request.kind;
    // The panel keeps the block: the block holds the panel weakly.
    let weak = Weak::from_retained(&panel);
    let done = block2::RcBlock::new(move |response: NSModalResponse| {
        let chosen = weak
            .load()
            .filter(|_| response == NSModalResponseOK)
            .and_then(|panel| match kind {
                Kind::Open => panel
                    .downcast::<NSOpenPanel>()
                    .ok()
                    .and_then(|open| open.URLs().firstObject()),
                Kind::Save => panel.URL(),
            })
            .and_then(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()));
        let answer = match chosen {
            Some(path) => Answer::Chosen {
                path,
                asking: NSEvent::modifierFlags_class()
                    .intersects(NSEventModifierFlags::Shift | NSEventModifierFlags::Option),
            },
            None => Answer::Canceled,
        };
        let _ = send.send(answer);
        wake();
    });
    panel.beginWithCompletionHandler(&done);
    Some(Shown { panel })
}

#[cfg(target_os = "linux")]
fn platform_show(
    request: &Request,
    parent: Parent,
    send: mpsc::Sender<Answer>,
    wake: impl Fn() + Send + 'static,
) -> Option<Shown> {
    let parent = match parent {
        Parent::X11(window) => format!("x11:{window:x}"),
        Parent::Wayland => String::new(),
        Parent::AppKit => return None,
    };
    let shown = std::sync::Arc::new(std::sync::Mutex::new(portal::Shown::Asking));
    let request_of_thread = request.clone();
    let shown_of_thread = shown.clone();
    std::thread::Builder::new()
        .name("file panel".into())
        .spawn(move || {
            let answer = portal::ask(&request_of_thread, &parent, &shown_of_thread);
            let _ = send.send(answer);
            wake();
        })
        .ok()?;
    Some(Shown { request: shown })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_show(
    _request: &Request,
    _parent: Parent,
    _send: mpsc::Sender<Answer>,
    _wake: impl Fn() + Send + 'static,
) -> Option<Shown> {
    None
}

impl Drop for Shown {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.panel.isVisible() {
            self.panel.close();
        }
        #[cfg(target_os = "linux")]
        portal::close(&self.request);
    }
}

/// The xdg-desktop-portal FileChooser
/// (<https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html>).
#[cfg(target_os = "linux")]
mod portal {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use zbus::blocking::{Connection, MessageIterator};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    use super::{Answer, Kind, Request};

    const DESKTOP: &str = "org.freedesktop.portal.Desktop";
    const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
    const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
    const REQUEST: &str = "org.freedesktop.portal.Request";

    /// Where a portal request is: closing it closes the chooser.
    pub(super) enum Shown {
        /// Not yet asked, or asking.
        Asking,
        /// On screen.
        Open(Connection, OwnedObjectPath),
        /// The panel was dropped: a request made after is closed at once.
        Dropped,
    }

    /// Close the chooser of a dropped panel, from a thread of its own (a
    /// D-Bus call the window never waits for).
    pub(super) fn close(shown: &Arc<Mutex<Shown>>) {
        let Ok(mut shown) = shown.lock() else {
            return;
        };
        if let Shown::Open(connection, handle) = std::mem::replace(&mut *shown, Shown::Dropped) {
            let _ = std::thread::Builder::new()
                .name("file panel close".into())
                .spawn(move || close_request(connection, &handle));
        }
    }

    /// Close the request, then the connection: a closed request sends no
    /// Response, so the asking thread's wait ends with the connection.
    fn close_request(connection: Connection, handle: &OwnedObjectPath) {
        let _ = connection.call_method(Some(DESKTOP), handle.as_str(), Some(REQUEST), "Close", &());
        let _ = connection.close();
    }

    /// Closes the connection when dropped, ending every wait on it.
    struct Closing(Connection);

    impl Drop for Closing {
        fn drop(&mut self) {
            let _ = self.0.clone().close();
        }
    }

    /// Ask the portal for `request`'s file, and wait for the answer.
    pub(super) fn ask(request: &Request, parent: &str, shown: &Arc<Mutex<Shown>>) -> Answer {
        match try_ask(request, parent, shown) {
            Ok(answer) => answer,
            Err(err) => {
                log::info!("no file chooser portal: {err}");
                Answer::Unavailable
            }
        }
    }

    fn try_ask(request: &Request, parent: &str, shown: &Arc<Mutex<Shown>>) -> zbus::Result<Answer> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TOKENS: AtomicU64 = AtomicU64::new(0);
        // A portal that never answers the call must not keep Open and
        // Create waiting: after this the panel is unavailable and
        // OpenMaker's chooser asks. Starting the portal on first use takes
        // a second or two. The user's own time in the chooser is not
        // bounded: its answer is a signal, not the call's reply.
        let connection = zbus::blocking::connection::Builder::session()?
            .method_timeout(std::time::Duration::from_secs(30))
            .build()?;
        // Listen before asking, so the answer cannot come first. The
        // connection is this request's own.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(REQUEST)?
            .member("Response")?
            .build();
        let mut responses = MessageIterator::for_match_rule(rule, &connection, Some(16))?;
        // And for the portal leaving the bus: a request whose portal went
        // never answers.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, DESKTOP)?
            .build();
        let owners = MessageIterator::for_match_rule(rule, &connection, Some(16))?;
        let token = format!(
            "openmaker{}_{}",
            std::process::id(),
            TOKENS.fetch_add(1, Ordering::Relaxed)
        );
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("modal", Value::from(true));
        if let Some(extension) = request.extension {
            let filter = (
                format!("FileMaker Pro files (.{extension})"),
                vec![(0u32, any_case_glob(extension))],
            );
            options.insert("filters", Value::new(vec![filter.clone()]));
            options.insert("current_filter", Value::new(filter));
        }
        if let Some(folder) = &request.folder {
            options.insert("current_folder", Value::new(nul_terminated(folder)));
        }
        let method = match request.kind {
            Kind::Open => {
                options.insert("accept_label", Value::from("_Open"));
                options.insert("multiple", Value::from(false));
                options.insert("directory", Value::from(false));
                "OpenFile"
            }
            Kind::Save => {
                options.insert("accept_label", Value::from("_Save"));
                options.insert("current_name", Value::from(request.name.as_str()));
                "SaveFile"
            }
        };
        let reply = connection.call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some(FILE_CHOOSER),
            method,
            &(parent, request.title.as_str(), options),
        )?;
        let handle: OwnedObjectPath = reply.body().deserialize()?;
        // The portal's own name: only its Response is the user's answer.
        let Some(portal) = reply.header().sender().map(|name| name.to_owned()) else {
            return Ok(Answer::Unavailable);
        };
        {
            let Ok(mut shown) = shown.lock() else {
                return Ok(Answer::Canceled);
            };
            if matches!(*shown, Shown::Dropped) {
                close_request(connection, &handle);
                return Ok(Answer::Canceled);
            }
            *shown = Shown::Open(connection.clone(), handle.clone());
        }
        // Whichever way this ends, the connection closes, which ends the
        // watch below; the portal leaving closes it too, which ends the wait
        // for its answer.
        let _closing = Closing(connection.clone());
        let watched = connection.clone();
        let left = portal.clone();
        std::thread::Builder::new()
            .name("file panel watch".into())
            .spawn(move || {
                for message in owners {
                    let Ok(message) = message else {
                        return;
                    };
                    let gone = message
                        .body()
                        .deserialize::<(String, String, String)>()
                        .is_ok_and(|(_, old, _)| old == left.as_str());
                    if gone {
                        let _ = watched.close();
                        return;
                    }
                }
            })
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        for message in responses.by_ref() {
            let message = message?;
            let header = message.header();
            if header.path().map(|p| p.as_str()) != Some(handle.as_str())
                || header.sender() != Some(&portal)
            {
                continue;
            }
            let (response, results): (u32, HashMap<String, OwnedValue>) =
                message.body().deserialize()?;
            return Ok(match response {
                0 => results
                    .get("uris")
                    .and_then(|uris| Vec::<String>::try_from(uris.clone()).ok())
                    .and_then(|uris| uris.into_iter().next())
                    .and_then(|uri| file_uri_path(&uri))
                    .map_or(Answer::Unavailable, |path| Answer::Chosen {
                        path,
                        asking: parent.starts_with("x11:") && x11_asking(),
                    }),
                1 => Answer::Canceled,
                // Ended some other way: the backend could not show one.
                _ => Answer::Unavailable,
            });
        }
        // The portal left, or the bus closed.
        Ok(Answer::Unavailable)
    }

    /// Shift or Alt is held now, as the X server sees it: the window's own
    /// input saw nothing while the chooser had the keyboard.
    fn x11_asking() -> bool {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};
        let Ok((conn, screen)) = x11rb::connect(None) else {
            return false;
        };
        let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else {
            return false;
        };
        let held = u16::from(KeyButMask::SHIFT) | u16::from(KeyButMask::MOD1);
        conn.query_pointer(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| u16::from(reply.mask) & held != 0)
    }

    /// A glob of `extension` in any case: `*.[fF][mM][pP]12`, as
    /// OpenMaker's own chooser matches it.
    fn any_case_glob(extension: &str) -> String {
        let mut glob = String::from("*.");
        for c in extension.chars() {
            if c.is_ascii_alphabetic() {
                glob.push('[');
                glob.push(c.to_ascii_lowercase());
                glob.push(c.to_ascii_uppercase());
                glob.push(']');
            } else {
                glob.push(c);
            }
        }
        glob
    }

    /// A path as the portal's `ay` options take it: its bytes and a NUL.
    fn nul_terminated(path: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        bytes
    }

    /// The local path a `file://` URI names, its percent escapes decoded
    /// to the name's exact bytes (a name need not be UTF-8); `None` for
    /// another scheme or a remote host.
    pub(super) fn file_uri_path(uri: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let rest = uri.strip_prefix("file://")?;
        let (host, path) = rest.split_at(rest.find('/')?);
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        let bytes = path.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = |b: u8| char::from(b).to_digit(16);
            match (
                bytes[i],
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) {
                (b'%', Some(high), Some(low)) => {
                    out.push((high * 16 + low) as u8);
                    i += 3;
                }
                (byte, _, _) => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::ffi::OsStrExt;

        use super::file_uri_path;

        /// The portal answers URIs; a name's escapes come back as its
        /// exact bytes, whatever they are.
        #[test]
        fn file_uris_become_the_names_bytes() {
            for (uri, bytes) in [
                (
                    "file:///home/a/My%20Tasks.fmp12",
                    &b"/home/a/My Tasks.fmp12"[..],
                ),
                (
                    "file:///home/a/Caf%C3%A9%20%231%3F.fmp12",
                    "/home/a/Café #1?.fmp12".as_bytes(),
                ),
                ("file://localhost/tmp/%FFx.fmp12", &b"/tmp/\xffx.fmp12"[..]),
                ("file:///tmp/100%.fmp12", &b"/tmp/100%.fmp12"[..]),
            ] {
                let path = file_uri_path(uri).unwrap();
                assert_eq!(path.as_os_str().as_bytes(), bytes, "{uri}");
            }
            assert_eq!(file_uri_path("file://server/share/a.fmp12"), None);
            assert_eq!(file_uri_path("sftp://host/a.fmp12"), None);
        }
    }
}
