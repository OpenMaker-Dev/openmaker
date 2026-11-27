//! A web viewer's WebKitGTK view, as both Linux backends make it: the X11
//! one (`gtk.rs`) in a child window of the app's window, the Wayland one in
//! the page host's window (`host.rs`). Wherever the view lives, it is
//! incognito (no cookies or storage written to disk) and has no developer
//! tools; it opens no other windows, downloads nothing, goes only to http,
//! https, data, blob, file and about URLs, and reports its loads, function
//! results and bridge messages as [`Event`]s.
//!
//! The page bridge has two halves, both injected into the page's main
//! frame only: the `FileMaker` object in the page's own script world
//! ([`BRIDGE`]), and in a script world of OpenMaker's own ([`WORLD`]) the
//! poster ([`BRIDGE_POSTER`]), which posts what `FileMaker` hands it to the
//! view's message handler. The handler is registered in that world only,
//! where no script of the page or of its frames runs, so another site's
//! frame in the page cannot post to it (WebKitGTK does not say which frame
//! posted a message). Messages are bounded as they arrive ([`bridge_text`],
//! and a [`MessageBudget`] where one is given).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use javascriptcore::{ContextExt as _, ValueExt as _};
use webkit2gtk::{
    UserContentInjectedFrames, UserContentManagerExt as _, UserScript, UserScriptInjectionTime,
    WebViewExt as _,
};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

use super::{allowed, Event, MessageBudget, Navigation, BRIDGE, BRIDGE_POSTER, MAX_MESSAGE};

/// The bridge's message handler (`window.webkit.messageHandlers.openmaker`).
const HANDLER: &str = "openmaker";
/// The script world the handler and the bridge's poster are in.
const WORLD: &str = "openmaker-bridge";

/// Where a view's events go (from GTK's thread, and from wry's callbacks).
pub(crate) type Events = Arc<dyn Fn(Event) + Send + Sync>;

/// A view and the navigation generations its loads report.
pub(crate) struct Page {
    pub webview: WebView,
    /// The generation the app's newest navigation carries, and the one of
    /// the load in progress.
    generation: Rc<Cell<u64>>,
    active_generation: Rc<Cell<u64>>,
    zoom: f64,
}

/// What a view is made with.
pub(crate) struct Spec {
    pub view: u64,
    pub url: String,
    /// Where the page lies in `container`, in its pixels.
    pub bounds: [i32; 4],
    pub interactive: bool,
    pub scripts: bool,
    /// Where bridge messages are counted until the app takes them; without
    /// one, the receiver counts them.
    pub budget: Option<MessageBudget>,
}

impl Page {
    /// Make the view of `spec` in `container`.
    pub fn new(container: &gtk::Fixed, spec: Spec, events: &Events) -> Result<Self, String> {
        let Spec {
            view,
            url,
            bounds: [x, y, width, height],
            interactive,
            scripts,
            budget,
        } = spec;
        // The page loads once the bridge is in place (below): its first
        // document gets both halves.
        let mut builder = WebViewBuilder::new()
            .with_incognito(true)
            .with_devtools(false)
            .with_bounds(wry::Rect {
                position: PhysicalPosition::new(x, y).into(),
                size: PhysicalSize::new(width.max(1) as u32, height.max(1) as u32).into(),
            })
            .with_navigation_handler(|url| allowed(&url))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false);
        if scripts {
            builder = builder.with_initialization_script(BRIDGE);
        }
        let webview = builder.build_gtk(container).map_err(|e| e.to_string())?;
        let page = webview.webview();
        let generation = Rc::new(Cell::new(0));
        let requested = Rc::clone(&generation);
        let active_generation = Rc::new(Cell::new(0));
        let active = Rc::clone(&active_generation);
        let loads = Arc::clone(events);
        // wry's Started maps to Committed on GTK. Use WebKit's real
        // provisional-load signal, and tag completions with the generation
        // that started that load rather than a later queued navigation.
        page.connect_load_changed(move |page, event| match event {
            webkit2gtk::LoadEvent::Started => {
                active.set(requested.get());
                loads(Event::LoadStarted {
                    view,
                    generation: active.get(),
                });
            }
            webkit2gtk::LoadEvent::Finished if !page.is_loading() => {
                loads(Event::Loaded {
                    view,
                    generation: active.get(),
                });
            }
            _ => {}
        });
        if scripts {
            let manager = page
                .user_content_manager()
                .ok_or_else(|| "the page has no content manager".to_owned())?;
            manager.add_script(&UserScript::for_world(
                BRIDGE_POSTER,
                UserContentInjectedFrames::TopFrame,
                UserScriptInjectionTime::Start,
                WORLD,
                &[],
                &[],
            ));
            // WebKit has already copied the message into this process; only
            // text the bridge could have posted, within the budget, goes
            // further.
            let messages = Arc::clone(events);
            manager.connect_script_message_received(Some(HANDLER), move |_, message| {
                let Some(body) = message.js_value().as_ref().and_then(bridge_text) else {
                    return;
                };
                if budget.as_ref().is_none_or(|budget| budget.admit(&body)) {
                    messages(Event::Message { view, body });
                }
            });
            if !manager.register_script_message_handler_in_world(HANDLER, WORLD) {
                return Err("the page's message handler could not be registered".into());
            }
        }
        webview.load_url(&url).map_err(|e| e.to_string())?;
        // "Allow interaction with web viewer content" off: the page takes
        // no pointer or keyboard input.
        page.set_sensitive(interactive);
        page.set_can_focus(interactive);
        Ok(Page {
            webview,
            generation,
            active_generation,
            zoom: 1.0,
        })
    }

    /// Go where `navigation` says, as the app's navigation `generation`.
    pub fn navigate(&self, view: u64, generation: u64, navigation: Navigation, events: &Events) {
        self.generation.set(generation);
        let webview = &self.webview;
        let page = webview.webview();
        let unchanged = matches!(&navigation, Navigation::Back) && !page.can_go_back()
            || matches!(&navigation, Navigation::Forward) && !page.can_go_forward();
        if unchanged {
            // WebKit emits no load signal for a history no-op. Adopt an
            // in-flight load too, so its completion still belongs to the
            // registry's latest generation.
            self.active_generation.set(generation);
            events(if page.is_loading() {
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
            events(Event::Failed {
                view,
                generation,
                reason: error.to_string(),
            });
        }
    }

    /// Run `script`, reporting its completion text as call `call`.
    pub fn evaluate(&self, view: u64, call: u64, script: &str, events: &Events) {
        let done = Arc::clone(events);
        let result = self
            .webview
            .evaluate_script_with_callback(script, move |result| {
                done(Event::Evaluated { view, call, result });
            });
        if result.is_err() {
            events(Event::Evaluated {
                view,
                call,
                result: String::new(),
            });
        }
    }

    /// Follow the document's zoom.
    pub fn zoom(&mut self, zoom: f64) {
        if (zoom - self.zoom).abs() > f64::EPSILON {
            let _ = self.webview.zoom(zoom);
            self.zoom = zoom;
        }
    }
}

/// The text of a message a page posted to the bridge's handler, if it can
/// be a bridge message: a string of ASCII without a NUL, which is all the
/// bridge posts (`encodeURIComponent`'s output), no longer than
/// [`MAX_MESSAGE`]. Its length is read before it is copied out, so one too
/// long is not copied; one JavaScriptCore cannot convert (an unpaired
/// surrogate) is `None`.
fn bridge_text(value: &javascriptcore::Value) -> Option<String> {
    if !value.is_string() {
        return None;
    }
    let units = utf16_length(value).filter(|&units| units <= MAX_MESSAGE)?;
    // As bytes, the string's whole length, where a C string would stop at
    // a NUL. Their number first, so that no more than the limit is read;
    // ASCII has as many bytes as units. A raw NUL is never the bridge's,
    // which encodes one as `%00`.
    let bytes = value.to_string_as_bytes()?;
    if bytes.len() != units || !bytes.is_ascii() || bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(&bytes).ok().map(str::to_owned)
}

/// A JavaScript string's length in UTF-16 code units, which is at most its
/// length in UTF-8 bytes, read in JavaScript without copying the string.
fn utf16_length(value: &javascriptcore::Value) -> Option<usize> {
    let length = value
        .context()?
        .evaluate("(function (text) { return text.length; })")?;
    let units = length
        .function_callv(std::slice::from_ref(value))?
        .to_double();
    (units.is_finite() && units >= 0.0).then_some(units as usize)
}

#[cfg(test)]
mod tests {
    use javascriptcore::ContextExt as _;

    /// What a page posts reaches the app only as text the bridge could have
    /// posted, and nothing it posts makes the handler panic (a panic in a
    /// GTK callback aborts the app).
    #[test]
    fn only_text_the_bridge_could_post_is_taken_from_a_page() {
        let context = javascriptcore::Context::new();
        let text = |code: &str| super::bridge_text(&context.evaluate(code).expect("evaluates"));
        let message = "openmaker-perform-script&s=a&p=%C3%A9%00&o=0";
        assert_eq!(text(&format!("'{message}'")).as_deref(), Some(message));
        // An unpaired surrogate, which JavaScriptCore may fail to convert,
        // other text that is not ASCII, a raw NUL, and what is not text.
        assert_eq!(text("'openmaker-perform-script&s=a\\uD800&p=&o=0'"), None);
        assert_eq!(text("'openmaker-perform-script&s=\\u00e9&p=&o=0'"), None);
        assert_eq!(text("'openmaker-perform-script&s=a&p=b\\u0000c&o=0'"), None);
        assert_eq!(text("42"), None);
        // Longer than a message may be, by its length alone.
        let long = format!("'x'.repeat({})", super::MAX_MESSAGE + 1);
        assert_eq!(text(&long), None);
    }
}
