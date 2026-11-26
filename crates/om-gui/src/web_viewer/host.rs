//! The page host: the process (`om-gui-webview`, beside the program) that
//! runs WebKitGTK for the Wayland backend (`wayland.rs`). It is a GTK client
//! of the app's page display only (`WAYLAND_DISPLAY` names its socket),
//! shows each view in an undecorated window of its own titled with the
//! view's id, takes the app's commands as JSON lines on stdin and answers
//! with [`Event`]s as JSON lines on stdout. It ends when stdin closes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead as _, Read as _, Write as _};
use std::process::ExitCode;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};

use gtk::glib;
use gtk::prelude::*;
use wry::WebViewExtUnix as _;

use super::wayland::{HostCommand, TITLE};
use super::webkit::{Events, Page, Spec};
use super::Event;

/// How many commands wait for GTK at most, and the longest command line.
const COMMANDS: usize = 16;
const MAX_COMMAND: u64 = 64 * 1024 * 1024;

struct Hosted {
    window: gtk::Window,
    page: Page,
}

struct Host {
    commands: Receiver<HostCommand>,
    events: Events,
    views: HashMap<u64, Hosted>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Run the page host until the app closes its stdin.
pub fn serve() -> ExitCode {
    gtk::gdk::set_allowed_backends("wayland");
    if gtk::init().is_err() {
        eprintln!("om-gui-webview: no display to show pages on");
        return ExitCode::FAILURE;
    }
    let out = Mutex::new(std::io::stdout());
    let events: Events = Arc::new(move |event: Event| {
        let Ok(mut line) = serde_json::to_vec(&event) else {
            return;
        };
        line.push(b'\n');
        let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.write_all(&line).and_then(|()| out.flush());
    });
    // A few commands wait for GTK at most: while it is busy, the app's
    // writes wait instead, and the app bounds those.
    let (commands, received) = sync_channel(COMMANDS);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            match (&mut stdin).take(MAX_COMMAND).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                // Longer than any command the app sends.
                Ok(_) if line.last() != Some(&b'\n') => break,
                Ok(_) => {}
            }
            if let Ok(command) = serde_json::from_slice::<HostCommand>(&line) {
                if commands.send(command).is_err() {
                    break;
                }
                glib::idle_add_once(drain);
            }
        }
        glib::idle_add_once(gtk::main_quit);
    });
    HOST.with(|host| {
        *host.borrow_mut() = Some(Host {
            commands: received,
            events,
            views: HashMap::new(),
        });
    });
    gtk::main();
    ExitCode::SUCCESS
}

/// Run the commands that came in (an idle callback on GTK's thread).
fn drain() {
    HOST.with(|host| {
        let mut host = host.borrow_mut();
        let Some(host) = host.as_mut() else {
            return;
        };
        while let Ok(command) = host.commands.try_recv() {
            host.apply(command);
        }
    });
}

impl Host {
    fn apply(&mut self, command: HostCommand) {
        match command {
            HostCommand::Create {
                view,
                url,
                width,
                height,
                zoom,
                interactive,
                scripts,
            } => {
                if let Err(reason) =
                    self.create(view, url, [width, height], zoom, interactive, scripts)
                {
                    (self.events)(Event::Failed {
                        view,
                        generation: 0,
                        reason,
                    });
                }
            }
            HostCommand::Resize {
                view,
                width,
                height,
                zoom,
            } => {
                if let Some(hosted) = self.views.get_mut(&view) {
                    resize(hosted, [width, height]);
                    hosted.page.zoom(zoom);
                }
            }
            HostCommand::Navigate {
                view,
                generation,
                navigation,
            } => {
                if let Some(hosted) = self.views.get(&view) {
                    hosted
                        .page
                        .navigate(view, generation, navigation, &self.events);
                }
            }
            HostCommand::Evaluate { view, call, script } => {
                if let Some(hosted) = self.views.get(&view) {
                    hosted.page.evaluate(view, call, &script, &self.events);
                }
            }
            HostCommand::Destroy { view } => {
                if let Some(hosted) = self.views.remove(&view) {
                    drop(hosted.page);
                    hosted.window.close();
                }
            }
        }
    }

    fn create(
        &mut self,
        view: u64,
        url: String,
        size: [i32; 2],
        zoom: f64,
        interactive: bool,
        scripts: bool,
    ) -> Result<(), String> {
        let [width, height] = size.map(|v| v.clamp(1, i32::from(i16::MAX)));
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(&format!("{TITLE}{view}"));
        window.set_decorated(false);
        window.set_default_size(width, height);
        let fixed = gtk::Fixed::new();
        window.add(&fixed);
        let mut page = Page::new(
            &fixed,
            Spec {
                view,
                url,
                bounds: [0, 0, width, height],
                interactive,
                scripts,
                // The app counts what it reads from this process.
                budget: None,
            },
            &self.events,
        )?;
        page.zoom(zoom);
        let mut hosted = Hosted { window, page };
        resize(&mut hosted, [width, height]);
        hosted.window.show_all();
        self.views.insert(view, hosted);
        Ok(())
    }
}

/// Size the window and its page to the object, in points.
fn resize(hosted: &mut Hosted, size: [i32; 2]) {
    let [width, height] = size.map(|v| v.clamp(1, i32::from(i16::MAX)));
    hosted.window.resize(width, height);
    hosted
        .page
        .webview
        .webview()
        .set_size_request(width, height);
}
