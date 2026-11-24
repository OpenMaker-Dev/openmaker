mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};
use openmaker_go::{
    hosted_files, Command, Document, DocumentMode, DocumentObserver, DocumentState, Element,
    ElementKind, Host, Input, Key, Menu, SignIn, Source, Viewport,
};

struct Headless;

impl DocumentObserver for Headless {
    fn frame_ready(&self) {}
}

/// A document and what its frames showed last, as the app's chrome sees it.
struct Shown {
    doc: Arc<Document>,
    state: Option<DocumentState>,
    elements: Vec<Element>,
    menus: Vec<Menu>,
}

impl Shown {
    fn open(source: Source) -> Self {
        let doc = Document::open(
            source,
            SignIn {
                account: "Admin".into(),
                password: String::new(),
                license: String::new(),
            },
            Viewport {
                width: 400.0,
                height: 420.0,
                scale: 2.0,
                max_texture_side: 4096,
            },
            Arc::new(Headless),
        )
        .unwrap();
        doc.send(Input::Accessibility { enabled: true });
        Shown {
            doc,
            state: None,
            elements: Vec::new(),
            menus: Vec::new(),
        }
    }

    fn until(&mut self, what: &str, done: impl Fn(&DocumentState, &[Element]) -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            while let Some(frame) = self.doc.take_frame() {
                assert!(!frame.batches.is_empty(), "a frame with nothing drawn");
                self.state = Some(frame.state);
                if let Some(elements) = frame.elements {
                    self.elements = elements;
                }
                if let Some(menus) = frame.menus {
                    self.menus = menus;
                }
            }
            if let Some(state) = &self.state {
                if done(state, &self.elements) {
                    return;
                }
            }
            assert!(
                Instant::now() < end,
                "{what}: {:?}\n{:#?}",
                self.state,
                self.elements
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn command(&self, command: Command) {
        self.doc.send(Input::Run { command });
    }
}

#[test]
fn a_composition_is_committed_in_its_field_before_a_tap_moves_on() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Field Notes.fmp12");
    support::fixture(&file);
    let mut local = Shown::open(Source::Local {
        path: file.to_str().unwrap().into(),
    });
    local.until("the file's button", |state, elements| {
        state.count == 2 && elements.iter().any(|e| e.label == "Next task")
    });
    local.doc.send(Input::Tap { x: 80., y: 42. });
    local.until("the keyboard", |state, _| state.keyboard);
    // Android commits what the input method composes, and the tap on the
    // file's Next button follows at once: the field reads the commit before
    // the tap moves on, and the record keeps it.
    local.doc.send(Input::Compose {
        text: "Draf".into(),
    });
    local.doc.send(Input::Text {
        text: "Drafted".into(),
    });
    local.doc.send(Input::Tap { x: 80., y: 110. });
    local.until("the next record", |state, _| state.position == 1);
    local.command(Command::Previous);
    local.until("the first record's text", |state, elements| {
        state.position == 0
            && elements
                .iter()
                .any(|e| e.kind == ElementKind::Field && e.value.contains("Drafted"))
    });
    assert!(local.doc.close(10_000), "the file did not close");
}

#[test]
fn text_typed_on_device_is_saved_by_close_and_found_on_a_host() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Field Notes.fmp12");
    support::fixture(&file);
    let mut local = Shown::open(Source::Local {
        path: file.to_str().unwrap().into(),
    });
    local.until("the file's button", |state, elements| {
        state.count == 2 && elements.iter().any(|e| e.label == "Next task")
    });
    // Go's Scripts menu has the file's scripts but no Script Workspace.
    let scripts = local.menus.iter().find(|menu| menu.title == "Scripts");
    assert!(
        scripts.is_some_and(|menu| menu
            .rows
            .iter()
            .all(|row| !row.label.starts_with("Script Workspace"))),
        "{scripts:?}"
    );
    // The file's own Next button, then its Name field.
    local.doc.send(Input::Tap { x: 80., y: 110. });
    local.until("the next record", |state, _| state.position == 1);
    local.doc.send(Input::Tap { x: 80., y: 42. });
    local.until("the keyboard", |state, _| state.keyboard);
    local.doc.send(Input::Text {
        text: "VeriXfied".into(),
    });
    // An input method's delete around the selection (Android's
    // deleteSurroundingText) takes the X before the selected "f" and keeps
    // the selection, which the "f" typed next replaces: a lost selection
    // would leave "Veriffied", a Backspace would have taken the "f".
    let press = |key, shift| Input::Press {
        key,
        shift,
        command: false,
    };
    for _ in 0..4 {
        local.doc.send(press(Key::ArrowLeft, false));
    }
    local.doc.send(press(Key::ArrowRight, true));
    local.doc.send(Input::DeleteAround {
        before: 1,
        after: 0,
        utf16: true,
    });
    local.doc.send(Input::Text { text: "f".into() });
    // No Done, and no wait for the text to show: closing the file saves
    // what was typed, the keys the field had not read yet included.
    assert!(local.doc.close(10_000), "the file did not close");
    drop(local);

    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let pem = cert.cert.pem();
    let identity =
        Identity::from_pem(pem.as_bytes(), cert.key_pair.serialize_pem().as_bytes()).unwrap();
    let name = "Field Notes.fmp12";
    let server = Server::bind(
        "127.0.0.1:0",
        ServerConfig {
            identity: ServerIdentity {
                name: "Go test".into(),
                version_string: "OpenMaker Server".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: name.into(),
                path: file,
            }],
            insecure_files: true,
            record_dir: None,
        },
        identity,
    )
    .unwrap()
    .listing_file(name);
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    let host = || Host {
        address: "127.0.0.1".into(),
        port,
        certificate: pem.clone(),
    };
    assert!(hosted_files(host()).unwrap().iter().any(|f| f == name));
    let mut hosted = Shown::open(Source::Hosted {
        host: host(),
        file: name.into(),
    });
    let field = |elements: &[Element]| {
        elements
            .iter()
            .find(|e| e.kind == ElementKind::Field)
            .map(|e| e.id)
    };
    hosted.until("the hosted records", |state, elements| {
        state.count == 2 && state.can_find && field(elements).is_some()
    });
    let browse_field = field(&hosted.elements);
    hosted.command(Command::Find);
    // Find mode's editor for the field, not Browse's, before the tap.
    hosted.until("Find mode's field", |state, elements| {
        state.mode == DocumentMode::Find
            && field(elements).is_some_and(|id| Some(id) != browse_field)
    });
    hosted.doc.send(Input::Tap { x: 80., y: 42. });
    hosted.until("the keyboard", |state, _| state.keyboard);
    hosted.doc.send(Input::Text {
        text: "*Verified*".into(),
    });
    hosted.until("the criteria", |state, _| state.can_perform_find);
    hosted.command(Command::PerformFind);
    hosted.until("the found record", |state, elements| {
        state.mode == DocumentMode::Browse
            && state.count == 1
            && elements.iter().any(|e| e.value.contains("Verified"))
    });

    // An edit the connection was lost with: closing must neither discard
    // it nor say the file closed. The file stays open with it until a
    // reconnect saves it, or the user discards it.
    hosted.doc.send(Input::Tap { x: 80., y: 42. });
    hosted.until("the field", |state, _| state.keyboard);
    hosted.doc.send(Input::Text {
        text: " offline".into(),
    });
    hosted.until("the record's lock", |state, _| {
        state.unsaved && state.can_close
    });
    handle.shutdown(Duration::from_secs(1));
    handle.close_connections();
    hosted.until("the lost connection", |state, _| {
        state.unsaved && !state.can_close
    });
    assert!(!hosted.doc.close(2_000), "a close discarded the edit");
    assert!(hosted.doc.has_unsaved_changes());
}
