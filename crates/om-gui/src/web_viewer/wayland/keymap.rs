//! The session's keymap, for the page host's keyboard: on a Wayland session
//! the compositor sends every client its keyboard's keymap (`wl_keyboard`'s
//! `keymap` event, the text of an xkb keymap in a file), and a connection
//! of the app's own takes it, then each new one, for as long as the app
//! runs. A key the app hears then means in a page what it means in the
//! app. Without a Wayland session (the app on X11) the display keeps xkb's
//! default keymap.

use std::os::unix::fs::FileExt as _;

use smithay::reexports::calloop::channel::Sender;
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use super::display::Request;

/// The largest keymap taken, in bytes (one is some tens of KiB).
const MAX_KEYMAP: u64 = 1024 * 1024;

struct Follow {
    display: Sender<Request>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
}

/// Follow the session's keymap on a thread of its own, sending each to
/// `display`; nothing when there is no Wayland session to ask.
pub(crate) fn follow(display: Sender<Request>) {
    let Ok(connection) = Connection::connect_to_env() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("openmaker-page-keymap".into())
        .spawn(move || {
            let mut queue = connection.new_event_queue();
            let handle = queue.handle();
            connection.display().get_registry(&handle, ());
            let mut state = Follow {
                display,
                seat: None,
                keyboard: None,
            };
            // Until the connection fails or the display goes.
            while queue.blocking_dispatch(&mut state).is_ok() {}
        });
}

impl Dispatch<wl_registry::WlRegistry, ()> for Follow {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_seat" && state.seat.is_none() {
                state.seat = Some(registry.bind(name, version.min(5), handle, ()));
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Follow {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            let has = capabilities.contains(wl_seat::Capability::Keyboard);
            if has && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(handle, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Follow {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        else {
            return;
        };
        // Read from its start without moving its offset, which other
        // clients of the compositor may share.
        let Ok(length) = usize::try_from(u64::from(size).min(MAX_KEYMAP)) else {
            return;
        };
        let mut text = vec![0; length];
        if std::fs::File::from(fd).read_exact_at(&mut text, 0).is_err() {
            return;
        }
        // The text ends with a NUL.
        while text.last() == Some(&0) {
            text.pop();
        }
        if let Ok(text) = String::from_utf8(text) {
            let _ = state.display.send(Request::Keymap(text));
        }
    }
}
