//! The web viewer registry with a recording backend: lifecycle, placement,
//! script commands and page callbacks, without a browser. The browser's own
//! behavior is exercised under a real X server by `tests/web_viewer_native.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use egui::{pos2, Rect};

use super::*;

#[derive(Default)]
struct Recorded {
    commands: Vec<String>,
    events: Vec<Event>,
    frames: Vec<PageFrame>,
    unavailable: bool,
    /// Commands the backend cannot send (too long for its page host).
    refused: bool,
    started: usize,
}

struct Recording(Rc<RefCell<Recorded>>);

/// The parent window's id or number, as the recording shows it.
fn number(parent: NativeParent) -> u64 {
    match parent {
        NativeParent::X11(window) => window,
        #[cfg(all(feature = "web-viewer", target_os = "macos"))]
        NativeParent::AppKit(number) => number.unsigned_abs() as u64,
        #[cfg(all(feature = "web-viewer", target_os = "linux"))]
        NativeParent::Composited => 0,
    }
}

impl Backend for Recording {
    fn create(&mut self, view: u64, parent: NativeParent, spec: ViewSpec) {
        let parent = number(parent);
        self.0.borrow_mut().commands.push(format!(
            "create {view} in {parent} {} {:?} {:?} visible={} interactive={} scripts={}",
            spec.url,
            spec.geometry.frame,
            spec.geometry.content,
            spec.visible,
            spec.interactive,
            spec.scripts
        ));
    }
    fn place(&mut self, view: u64, geometry: Geometry, visible: bool) {
        self.0.borrow_mut().commands.push(format!(
            "place {view} {:?} {:?} visible={visible}",
            geometry.frame, geometry.content
        ));
    }
    fn navigate(&mut self, view: u64, _generation: u64, navigation: Navigation) -> bool {
        let mut recorded = self.0.borrow_mut();
        recorded
            .commands
            .push(format!("navigate {view} {navigation:?}"));
        !recorded.refused
    }
    fn evaluate(&mut self, view: u64, call: u64, script: String) -> bool {
        let mut recorded = self.0.borrow_mut();
        recorded
            .commands
            .push(format!("evaluate {view} {call} {script}"));
        !recorded.refused
    }
    fn destroy(&mut self, view: u64) {
        self.0.borrow_mut().commands.push(format!("destroy {view}"));
    }
    fn release_focus(&mut self, parent: NativeParent) {
        let parent = number(parent);
        self.0
            .borrow_mut()
            .commands
            .push(format!("release focus to {parent}"));
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.0.borrow_mut().events)
    }
    fn frames(&mut self) -> Vec<PageFrame> {
        std::mem::take(&mut self.0.borrow_mut().frames)
    }
    fn available(&mut self) -> bool {
        !self.0.borrow().unavailable
    }
    fn input(&mut self, view: u64, input: PageInput) {
        self.0
            .borrow_mut()
            .commands
            .push(format!("input {view} {input:?}"));
    }
}

fn viewers() -> (WebViewers, Rc<RefCell<Recorded>>) {
    let recorded = Rc::new(RefCell::new(Recorded::default()));
    let shared = Rc::clone(&recorded);
    let viewers = WebViewers::with_factory(Box::new(move |_| {
        shared.borrow_mut().started += 1;
        Ok(Box::new(Recording(Rc::clone(&shared))) as Box<dyn Backend>)
    }));
    (viewers, recorded)
}

fn commands(recorded: &Rc<RefCell<Recorded>>) -> Vec<String> {
    std::mem::take(&mut recorded.borrow_mut().commands)
}

/// "Allow interaction" and "Allow JavaScript to perform FileMaker scripts".
const SCRIPTS: WebViewerOptions = WebViewerOptions(0x8041);

fn scrolled_placement() -> Placement {
    let mut scrolled = placement(Some("data:text/html,<p id=a>#1</p>"));
    scrolled.visible = Rect::from_min_max(pos2(0.0, 200.0), pos2(1000.0, 800.0));
    scrolled
}

fn placement(address: Option<&str>) -> Placement {
    Placement {
        object: 7,
        options: SCRIPTS,
        rect: Rect::from_min_max(pos2(10.0, 100.0), pos2(210.0, 300.0)),
        visible: Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 800.0)),
        address: address.map(str::to_owned),
        record: None,
        current: true,
        zoom: 1.0,
        input: Vec::new(),
        hovered: false,
    }
}

const UUID: &str = "B0A97EC3-E710-45FD-B0A3-0EF1D7257532";

fn frame<'a>(placements: &'a [Placement], covered: &'a [Rect]) -> WindowFrame<'a> {
    WindowFrame {
        window: 1,
        uuid: UUID,
        file: 3,
        epoch: 5,
        layout: Some(9),
        parent: Some(NativeParent::X11(0x40_0001)),
        pixels_per_point: 1.0,
        covered,
        placements,
        pressed: false,
    }
}

fn window() -> ViewKey {
    ViewKey {
        window: 1,
        uuid: UUID.into(),
        file: 3,
        epoch: 5,
        layout: 9,
        object: 0,
        record: None,
    }
}

fn set_url(url: &str) -> Command {
    Command::Set {
        layout: 9,
        object: 7,
        action: 5,
        url: Some(url.into()),
    }
}

fn perform(function: &str, parameters: &[&str]) -> Command {
    Command::Perform {
        layout: 9,
        object: 7,
        function: function.into(),
        parameters: parameters.iter().map(|p| p.to_string()).collect(),
    }
}

/// A file's object bounds can be anything finite: the view's geometry
/// stays bounded instead of overflowing, and no native view is made for
/// it (#953's review).
#[test]
fn a_view_of_hostile_bounds_gets_bounded_geometry() {
    let window = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
    let huge = Rect::from_min_max(pos2(-3e38, -3e38), pos2(3e38, 3e38));
    let geometry = Geometry::of(huge, window, 2.0, 1.0);
    assert_eq!(geometry.frame, [0, 0, 1600, 1200]);
    assert_eq!(
        geometry.content,
        [-16_777_216, -16_777_216, 33_554_432, 33_554_432]
    );
    assert!(!geometry.fits(), "no native view is made that large");
}

#[test]
fn a_view_follows_its_object_until_the_object_is_no_longer_painted() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    // Nothing starts before an address is known.
    viewers.reconcile(&ctx, &frame(&[placement(None)], &[]));
    assert_eq!(recorded.borrow().started, 0);
    assert_eq!(viewers.len(), 0);

    viewers.reconcile(
        &ctx,
        &frame(&[placement(Some("data:text/html,<p id=a>#1</p>"))], &[]),
    );
    assert_eq!(
        commands(&recorded),
        [
            "create 1 in 4194305 data:text/html,%3Cp%20id=a%3E%231%3C/p%3E [10, 100, 200, 200] \
             [0, 0, 200, 200] visible=true interactive=true scripts=true"
        ]
    );
    // An unchanged frame sends nothing.
    viewers.reconcile(
        &ctx,
        &frame(&[placement(Some("data:text/html,<p id=a>#1</p>"))], &[]),
    );
    assert!(commands(&recorded).is_empty());

    // Scrolled half out of the document: the frame is the visible part, the
    // page keeps its size at an offset.
    let mut scrolled = placement(Some("data:text/html,<p id=a>#1</p>"));
    scrolled.visible = Rect::from_min_max(pos2(0.0, 200.0), pos2(1000.0, 800.0));
    viewers.reconcile(&ctx, &frame(&[scrolled.clone()], &[]));
    assert_eq!(
        commands(&recorded),
        ["place 1 [10, 200, 200, 100] [0, -100, 200, 200] visible=true"]
    );
    // A menu over it hides it; it shows again once the menu is gone.
    let menu = [Rect::from_min_max(pos2(50.0, 250.0), pos2(120.0, 400.0))];
    viewers.reconcile(&ctx, &frame(&[scrolled.clone()], &menu));
    assert_eq!(
        commands(&recorded),
        ["place 1 [10, 200, 200, 100] [0, -100, 200, 200] visible=false"]
    );
    viewers.reconcile(&ctx, &frame(&[scrolled], &[]));
    assert_eq!(commands(&recorded).len(), 1);

    // A click in the app's window takes the keyboard back from the page.
    let placements = [scrolled_placement()];
    let mut clicked = frame(&placements, &[]);
    clicked.pressed = true;
    viewers.reconcile(&ctx, &clicked);
    assert_eq!(commands(&recorded), ["release focus to 4194305"]);

    // Another record's address loads in the same view.
    let mut next = placement(Some("https://example.com/a b"));
    next.visible = Rect::from_min_max(pos2(0.0, 200.0), pos2(1000.0, 800.0));
    viewers.reconcile(&ctx, &frame(&[next], &[]));
    assert_eq!(
        commands(&recorded),
        ["navigate 1 Load(\"https://example.com/a%20b\")"]
    );

    // Another layout (or mode) paints no web viewer: the view goes.
    viewers.reconcile(&ctx, &frame(&[], &[]));
    assert_eq!(commands(&recorded), ["destroy 1"]);
    assert_eq!(viewers.len(), 0);

    // The window's viewport gets another native window (eframe recreated
    // it, or the window moved to the root viewport): the view is made again
    // in that one. Without one it goes.
    let placements = [placement(Some("data:text/html,x"))];
    viewers.reconcile(&ctx, &frame(&placements, &[]));
    let mut moved = frame(&placements, &[]);
    moved.parent = Some(NativeParent::X11(0x60_0001));
    viewers.reconcile(&ctx, &moved);
    let made = "data:text/html,x [10, 100, 200, 200] [0, 0, 200, 200] visible=true \
                interactive=true scripts=true";
    assert_eq!(
        commands(&recorded),
        [
            format!("create 2 in 4194305 {made}"),
            "destroy 2".to_owned(),
            format!("create 3 in 6291457 {made}"),
        ]
    );
    moved.parent = None;
    viewers.reconcile(&ctx, &moved);
    assert_eq!(commands(&recorded), ["destroy 3"]);
    assert_eq!(viewers.len(), 0);
}

/// A painted page (`composited`) gets the pointer's input Browse noted over
/// it and hears the pointer leave; a press on it keeps the keyboard there,
/// and a press elsewhere in the window takes it back.
#[test]
fn a_painted_page_gets_the_pointer_input_over_it() {
    let (mut viewers, recorded) = viewers();
    let ctx = egui::Context::default();
    let page = placement(Some("about:blank"));
    viewers.reconcile(&ctx, &frame(std::slice::from_ref(&page), &[]));
    commands(&recorded);
    let press = PageInput::Button {
        x: 5.0,
        y: 6.0,
        button: 0,
        pressed: true,
    };
    let over = Placement {
        hovered: true,
        input: vec![press],
        ..page.clone()
    };
    let mut pressed = frame(std::slice::from_ref(&over), &[]);
    pressed.pressed = true;
    viewers.reconcile(&ctx, &pressed);
    assert_eq!(commands(&recorded), [format!("input 1 {press:?}")]);
    let mut elsewhere = frame(std::slice::from_ref(&page), &[]);
    elsewhere.pressed = true;
    viewers.reconcile(&ctx, &elsewhere);
    assert_eq!(
        commands(&recorded),
        ["input 1 Leave", "release focus to 4194305"]
    );
}

/// Once the backend can no longer draw pages (the Wayland backend's page
/// host ended), Set Web Viewer and Perform JavaScript fail as unavailable
/// commands, error 3, rather than seem to work; so does a Set Web Viewer
/// the backend could not send (an address too long for the page host).
#[test]
fn steps_on_a_backend_that_ended_fail_as_unavailable() {
    let (mut viewers, recorded) = viewers();
    let ctx = egui::Context::default();
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    recorded.borrow_mut().unavailable = true;
    viewers.command(1, &window(), set_url("https://example.com/"));
    viewers.command(2, &window(), perform("show", &[]));
    let answers: Vec<Outcome> = viewers.poll();
    assert_eq!(
        answers,
        [1, 2].map(|prompt| Outcome::Answer {
            prompt,
            result: Err(UNAVAILABLE),
        })
    );
    {
        let mut recorded = recorded.borrow_mut();
        recorded.unavailable = false;
        recorded.refused = true;
    }
    viewers.command(3, &window(), set_url("https://example.com/"));
    let answers: Vec<Outcome> = viewers.poll();
    assert_eq!(
        answers,
        [Outcome::Answer {
            prompt: 3,
            result: Err(UNAVAILABLE),
        }]
    );
}

/// A painted page's newest frame is what Browse paints, until its view
/// fails (its browser went away): then nothing of it is painted.
#[test]
fn a_painted_page_that_fails_is_no_longer_painted() {
    let (mut viewers, recorded) = viewers();
    let ctx = egui::Context::default();
    let page = placement(Some("about:blank"));
    viewers.reconcile(&ctx, &frame(std::slice::from_ref(&page), &[]));
    recorded.borrow_mut().frames.push(PageFrame {
        view: 1,
        size: [2, 2],
        rgba: vec![255; 16],
    });
    viewers.frames(&ctx);
    let painted = |viewers: &WebViewers| viewers.pages(1, UUID, 3, 5, Some(9)).len();
    assert_eq!(painted(&viewers), 1);
    recorded.borrow_mut().events.push(Event::Failed {
        view: 1,
        generation: 0,
        reason: "the page host ended".into(),
    });
    viewers.poll();
    assert_eq!(painted(&viewers), 0);
}

#[test]
fn closed_windows_and_earlier_sessions_lose_their_views() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    commands(&recorded);
    // The window is still open in this session: nothing happens.
    viewers.retain_windows(5, &[(1, UUID.into())]);
    assert!(commands(&recorded).is_empty());
    // Window 1 reopened after the file closed: a new UUID and epoch.
    viewers.retain_windows(6, &[(1, "C2".into())]);
    assert_eq!(commands(&recorded), ["destroy 1"]);
    assert!(viewers.poll().is_empty());
}

#[test]
fn set_web_viewer_navigates_only_a_live_view_of_the_window_it_names() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    viewers.reconcile(&ctx, &frame(&[placement(Some("https://home.test/"))], &[]));
    commands(&recorded);

    viewers.command(1, &window(), set_url("https://example.com/x?a=b c"));
    viewers.command(
        2,
        &window(),
        Command::Set {
            layout: 9,
            object: 7,
            action: 1,
            url: None,
        },
    );
    viewers.command(
        3,
        &window(),
        Command::Set {
            layout: 9,
            object: 7,
            action: 2,
            url: None,
        },
    );
    assert_eq!(
        commands(&recorded),
        [
            "navigate 1 Load(\"https://example.com/x?a=b%20c\")",
            "navigate 1 Load(\"https://home.test/\")",
            "navigate 1 Reload",
        ]
    );
    assert_eq!(
        viewers.poll(),
        [1, 2, 3].map(|prompt| Outcome::Answer {
            prompt,
            result: Ok(())
        })
    );

    // Another window instance with the same id (a reopened file) does not
    // reach this view: its command waits for its own view, then fails.
    let other = ViewKey {
        uuid: "C2".into(),
        ..window()
    };
    viewers.command(4, &other, set_url("https://example.com/"));
    assert!(viewers.poll().is_empty());
    let mut frame_other = frame(&[], &[]);
    frame_other.uuid = "C2";
    viewers.reconcile(&ctx, &frame_other);
    viewers.reconcile(&ctx, &frame_other);
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 4,
            result: Err(OBJECT_MISSING)
        }]
    );
    assert!(commands(&recorded)
        .iter()
        .all(|c| !c.starts_with("navigate")));

    // In List view each row's viewer is a view of its own, and a command
    // goes to the current record's, even one sent before the row was in
    // sight (its page still loading, say): it waits for the view.
    viewers.reconcile(&ctx, &frame(&[], &[]));
    commands(&recorded);
    viewers.command(5, &window(), set_url("https://example.com/22"));
    let rows = [(21, false), (22, true)].map(|(record, current)| Placement {
        record: Some(record),
        current,
        ..placement(Some(&format!("https://row.test/{record}")))
    });
    viewers.reconcile(&ctx, &frame(&rows, &[]));
    assert_eq!(viewers.len(), 2);
    let row_22 = viewers
        .views
        .iter()
        .find(|v| v.key.record == Some(22))
        .unwrap()
        .id;
    let sent = commands(&recorded);
    assert!(
        sent[..2].iter().all(|c| c.starts_with("create")),
        "{sent:?}"
    );
    assert_eq!(
        sent[2..],
        [format!(
            "navigate {row_22} Load(\"https://example.com/22\")"
        )]
    );
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 5,
            result: Ok(())
        }]
    );
}

#[test]
fn a_command_for_a_view_the_window_is_about_to_make_waits_for_its_next_frame() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    // The script went to the layout and set the web viewer at once.
    viewers.command(1, &window(), set_url("https://example.com/"));
    assert!(viewers.poll().is_empty());
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    assert_eq!(
        commands(&recorded),
        [
            "create 1 in 4194305 about:blank [10, 100, 200, 200] [0, 0, 200, 200] \
             visible=true interactive=true scripts=true",
            "navigate 1 Load(\"https://example.com/\")",
        ]
    );
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 1,
            result: Ok(())
        }]
    );
}

#[test]
fn perform_javascript_runs_only_in_a_loaded_page_and_reports_its_outcome() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    commands(&recorded);

    // Still loading: the function does not run.
    viewers.command(1, &window(), perform("show", &["a"]));
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 1,
            result: Err(INVALID)
        }]
    );
    assert!(commands(&recorded).is_empty());

    recorded.borrow_mut().events.extend([
        Event::LoadStarted {
            view: 1,
            generation: 0,
        },
        Event::Loaded {
            view: 1,
            generation: 0,
        },
    ]);
    assert!(viewers.poll().is_empty());
    viewers.command(2, &window(), perform("app.show", &["a \"b\"\n", "2"]));
    let sent = commands(&recorded);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].starts_with("evaluate 1 2 (function () {"),
        "{}",
        sent[0]
    );
    assert!(sent[0].contains("var path = [\"app\", \"show\"]"));
    assert!(sent[0].contains("target.apply(owner, [\"a \\\"b\\\"\\n\", \"2\"])"));
    // Waiting for the function.
    assert!(viewers.poll().is_empty());
    assert!(viewers.is_waiting());
    recorded.borrow_mut().events.push(Event::Evaluated {
        view: 1,
        call: 2,
        result: "\"ok\"".into(),
    });
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 2,
            result: Ok(())
        }]
    );

    // A function the page lacks fails the step.
    viewers.command(3, &window(), perform("nothing", &[]));
    commands(&recorded);
    recorded.borrow_mut().events.push(Event::Evaluated {
        view: 1,
        call: 3,
        result: "\"missing\"".into(),
    });
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 3,
            result: Err(INVALID)
        }]
    );

    // A call still running when its view goes (the user went to another
    // layout) fails, and its late completion is ignored.
    viewers.command(4, &window(), perform("slow", &[]));
    commands(&recorded);
    viewers.reconcile(&ctx, &frame(&[], &[]));
    assert_eq!(commands(&recorded), ["destroy 1"]);
    recorded.borrow_mut().events.push(Event::Evaluated {
        view: 1,
        call: 4,
        result: "\"ok\"".into(),
    });
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 4,
            result: Err(OBJECT_MISSING)
        }]
    );
    assert!(!viewers.is_waiting());
}

#[test]
fn page_callbacks_reach_only_the_live_view_that_allows_them() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    let mut plain = placement(Some("about:blank"));
    plain.object = 8;
    plain.options = WebViewerOptions(0x8001);
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank")), plain], &[]));
    let created = commands(&recorded);
    assert!(created[0].ends_with("scripts=true") && created[1].ends_with("scripts=false"));

    let body = "openmaker-perform-script&s=Store%20%C3%A9&p=a%26b%0A2&o=3";
    recorded.borrow_mut().events.extend([
        Event::Message {
            view: 1,
            body: body.into(),
        },
        // The view without the option never gets the bridge; a message is
        // ignored all the same.
        Event::Message {
            view: 2,
            body: body.into(),
        },
        Event::Message {
            view: 1,
            body: "something else".into(),
        },
    ]);
    assert_eq!(
        viewers.poll(),
        [Outcome::PerformScript {
            key: ViewKey {
                object: 7,
                ..window()
            },
            call: ScriptCall {
                script: "Store é".into(),
                parameter: "a&b\n2".into(),
                option: 3,
            },
        }]
    );

    // Once the session ends its pages are gone: a message still in the
    // channel from the destroyed view does not start anything.
    viewers.retain_windows(6, &[]);
    recorded.borrow_mut().events.push(Event::Message {
        view: 1,
        body: body.into(),
    });
    assert!(viewers.poll().is_empty());
}

#[test]
fn a_view_whose_browser_failed_fails_its_commands() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    recorded.borrow_mut().events.extend([
        Event::Loaded {
            view: 1,
            generation: 0,
        },
        Event::Failed {
            view: 1,
            generation: 0,
            reason: "the web process went away".into(),
        },
    ]);
    assert!(viewers.poll().is_empty());
    viewers.command(1, &window(), perform("show", &[]));
    viewers.command(2, &window(), set_url("https://example.com/"));
    assert_eq!(
        viewers.poll(),
        [1, 2].map(|prompt| Outcome::Answer {
            prompt,
            result: Err(OBJECT_MISSING)
        })
    );
}

#[test]
fn without_a_backend_web_viewers_say_why_and_commands_fail() {
    let ctx = egui::Context::default();
    let mut viewers = WebViewers::with_factory(Box::new(|_| Err(Unavailable::Platform)));
    assert_eq!(viewers.unavailable(Ok(NativeParent::X11(1))), None);
    assert_eq!(
        viewers.unavailable(Err(Unavailable::Window)),
        Some(Unavailable::Window)
    );
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    assert_eq!(
        viewers.unavailable(Ok(NativeParent::X11(1))),
        Some(Unavailable::Platform)
    );
    viewers.command(1, &window(), set_url("https://example.com/"));
    viewers.reconcile(&ctx, &frame(&[], &[]));
    viewers.reconcile(&ctx, &frame(&[], &[]));
    assert_eq!(
        viewers.poll(),
        [Outcome::Answer {
            prompt: 1,
            result: Err(OBJECT_MISSING)
        }]
    );
    let native = WebViewers::default();
    let expected = (!super::NATIVE)
        .then_some(Unavailable::Build)
        .or(Some(Unavailable::Window));
    assert_eq!(native.unavailable(Err(Unavailable::Window)), expected);
}

#[test]
fn messages_and_calls_encode_text_safely() {
    assert_eq!(
        parse_message("openmaker-perform-script&s=a&p=&o=9"),
        Some(ScriptCall {
            script: "a".into(),
            parameter: String::new(),
            option: 0,
        })
    );
    assert_eq!(parse_message("openmaker-perform-script&s=%ZZ&p=&o=0"), None);
    assert_eq!(parse_message("openmaker-perform-script&p=x&o=0"), None);
    assert_eq!(parse_message("openmaker-perform-script&s=a&x=1"), None);
    assert_eq!(call_script("", &[]), None);
    assert_eq!(call_script("a..b", &[]), None);
    assert_eq!(
        js_string("a\u{2028}</script>\u{1}"),
        "\"a\\u2028</script>\\u0001\""
    );
    assert!(call_succeeded("\"ok\"") && !call_succeeded("") && !call_succeeded("\"error\""));
    // "Automatically encode URL" off leaves the address as it is.
    assert_eq!(
        url_for("data:text/html,<b>#</b>", WebViewerOptions(0x21)),
        "data:text/html,<b>#</b>"
    );
    assert_eq!(url_for("  ", SCRIPTS), "about:blank");
    assert_eq!(
        url_for("https://example.com/#top", SCRIPTS),
        "https://example.com/#top"
    );
}

#[test]
fn bridge_messages_wait_for_the_app_within_a_bound() {
    let budget = MessageBudget::default();
    let prefix = "openmaker-perform-script&s=a&p=";
    let small = format!("{prefix}x");
    // Too long or malformed: dropped, with nothing left counted.
    assert!(!budget.admit(&format!("{prefix}{}", "x".repeat(MAX_MESSAGE))));
    assert!(!budget.admit("openmaker-perform-script&x=1"));
    // So many messages wait, and one the app polls makes room for one more.
    for _ in 0..MESSAGE_ENTRIES {
        assert!(budget.admit(&small));
    }
    assert!(!budget.admit(&small));
    budget.release(&small);
    assert!(budget.admit(&small));
    for _ in 0..MESSAGE_ENTRIES {
        budget.release(&small);
    }
    // So many bytes: two of the longest messages fill the budget.
    let longest = format!("{prefix}{}", "x".repeat(MAX_MESSAGE - prefix.len()));
    assert!(budget.admit(&longest) && budget.admit(&longest));
    assert!(!budget.admit(&small));
    budget.release(&longest);
    assert!(budget.admit(&small));
}

#[test]
fn navigation_blocks_javascript_and_ignores_old_page_completions() {
    let ctx = egui::Context::default();
    let (mut viewers, recorded) = viewers();
    viewers.reconcile(&ctx, &frame(&[placement(Some("about:blank"))], &[]));
    recorded.borrow_mut().events.push(Event::Loaded {
        view: 1,
        generation: 0,
    });
    assert!(viewers.poll().is_empty());
    commands(&recorded);
    viewers.command(1, &window(), perform("old", &[]));
    assert_eq!(commands(&recorded).len(), 1);
    viewers.command(2, &window(), set_url("https://example.com/next"));
    viewers.command(3, &window(), perform("too_early", &[]));
    assert_eq!(
        viewers.poll(),
        [
            Outcome::Answer {
                prompt: 1,
                result: Err(INVALID)
            },
            Outcome::Answer {
                prompt: 2,
                result: Ok(())
            },
            Outcome::Answer {
                prompt: 3,
                result: Err(INVALID)
            },
        ]
    );
    assert!(commands(&recorded)
        .iter()
        .all(|c| !c.starts_with("evaluate")));
    // Events already queued for the old page cannot finish or fail this load.
    recorded.borrow_mut().events.extend([
        Event::Loaded {
            view: 1,
            generation: 0,
        },
        Event::Failed {
            view: 1,
            generation: 0,
            reason: "old navigation".into(),
        },
    ]);
    assert!(viewers.poll().is_empty());
    assert_eq!(viewers.views[0].state, LoadState::Loading);
    recorded.borrow_mut().events.extend([
        Event::LoadStarted {
            view: 1,
            generation: 1,
        },
        Event::Loaded {
            view: 1,
            generation: 1,
        },
    ]);
    assert!(viewers.poll().is_empty());
    viewers.command(4, &window(), perform("ready", &[]));
    assert_eq!(commands(&recorded).len(), 1);
    // A changed calculated address has the same immediate loading boundary.
    viewers.reconcile(
        &ctx,
        &frame(&[placement(Some("https://example.com/calculated"))], &[]),
    );
    recorded.borrow_mut().events.extend([
        Event::LoadStarted {
            view: 1,
            generation: 1,
        },
        Event::Loaded {
            view: 1,
            generation: 1,
        },
    ]);
    viewers.command(5, &window(), perform("still_early", &[]));
    assert_eq!(
        viewers.poll(),
        [
            Outcome::Answer {
                prompt: 4,
                result: Err(INVALID)
            },
            Outcome::Answer {
                prompt: 5,
                result: Err(INVALID)
            },
        ]
    );
    assert_eq!(viewers.views[0].state, LoadState::Loading);
    assert!(commands(&recorded)
        .iter()
        .all(|c| !c.starts_with("evaluate")));
    recorded.borrow_mut().events.push(Event::Loaded {
        view: 1,
        generation: 2,
    });
    assert!(viewers.poll().is_empty());
    viewers.command(6, &window(), perform("new_page", &[]));
    assert_eq!(commands(&recorded).len(), 1);
}
