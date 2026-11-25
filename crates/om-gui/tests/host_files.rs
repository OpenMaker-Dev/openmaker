//! Home's saved hosts end to end: the native provider's host browser
//! against in-process `om-server`s on loopback over verified TLS, hosting
//! copies of the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs` as Alpha, Beta and Hidden. A saved
//! host's page lists only what the host publishes, for that host; a listed
//! file opens with its account and no client license; and the page tells
//! an empty list, a host it cannot list and a listing no longer waited for
//! apart.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT as _, Queryable};
use egui_kittest::Harness;
use fixture::{TempFile, ACCOUNT, PASSWORD};
use om_server::Server;

/// Alpha, Beta and Hidden, each a Tasks file with two tasks and a script
/// (the window reads the script catalog once a file opens).
fn copies(tag: &str) -> Vec<TempFile> {
    let scripts = [fixture::script(1, "Start", Vec::new())];
    ["alpha", "beta", "hidden"]
        .iter()
        .map(|name| TempFile::new(&format!("gui-host-files-{tag}-{name}"), &scripts, true))
        .collect()
}

fn host(tag: &str, files: &[TempFile], configure: impl FnOnce(Server) -> Server) -> hosted::Hosted {
    let named: Vec<(&Path, &str)> = files
        .iter()
        .zip(["Alpha.fmp12", "Beta.fmp12", "Hidden.fmp12"])
        .map(|(file, name)| (Path::new(&file.0), name))
        .collect();
    hosted::host_files_with(
        &format!("host-files-{tag}"),
        &named,
        (ACCOUNT, PASSWORD),
        configure,
    )
}

/// The app at Home, its lists and saved hosts in `settings`. Tests never
/// touch the developer's credential store.
fn home(provider: om_provider::NativeProvider, settings: &Path) -> common::App {
    let settings = settings.to_path_buf();
    Harness::builder()
        .with_size(egui::vec2(1060.0, 760.0))
        .renderer(common::renderer())
        .build_eframe(move |_| {
            om_gui::OpenMakerApp::with_license_store(
                Box::new(provider),
                Box::new(common::NoLicense),
            )
            .with_settings_file(settings)
            .with_home()
        })
}

/// Home's +: save the loopback host at `port` as `name`, which shows its
/// page and lists its files.
fn add_host(app: &mut common::App, name: &str, port: u16) {
    common::click(app, "Add Host");
    fill_host(app, name, port);
}

fn fill_host(app: &mut common::App, name: &str, port: u16) {
    common::fill(app, "Address", "127.0.0.1");
    common::replace(app, "Port", &port.to_string());
    common::fill(app, "Name", name);
    common::click(app, "Add");
}

/// Turn Use saved FileMaker license off, if it shows and is on.
fn no_license(app: &mut common::App) {
    let license = "Use saved FileMaker license";
    let on = app.query_by_label(license).is_some_and(|node| {
        node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
    });
    if on {
        common::click(app, license);
    }
}

/// The sign-in for hosted file `file`: the test account, and no client
/// license, which these loopback servers do not need.
fn sign_in(app: &mut common::App, file: &str) {
    common::wait_for_label(app, &format!("Open “{file}”"));
    no_license(app);
    common::fill(app, "Account Name", ACCOUNT);
    common::fill(app, "Password", PASSWORD);
    common::click(app, "Sign in");
}

#[test]
fn a_saved_host_lists_its_files_and_a_chosen_one_opens() {
    let files = copies("listed");
    let hosted = host("listed", &files, |server| {
        server
            .listing_file("Alpha.fmp12")
            .listing_file("Beta.fmp12")
    });
    let port = hosted.connection.port;
    let folder = tempfile::tempdir().unwrap();
    let settings = folder.path().join("settings.json");
    let mut app = home(hosted.provider(), &settings);

    // With nothing saved, Show Hosts offers to add a host; adding it shows
    // its page, and its files are listed at once.
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Hosts");
    common::click(&mut app, "Show Hosts...");
    common::click(&mut app, "Add Host...");
    fill_host(&mut app, "Loopback", port);
    common::wait_for_label(&mut app, "Beta");
    app.get_by_label("Alpha");
    // What the host says it is; nothing of the file it does not publish.
    app.get_by_label(&format!(
        "127.0.0.1:{port}  \u{b7}  openmaker-test \u{b7} OpenMaker Server test"
    ));
    assert!(app.query_by_label("Hidden").is_none());
    assert_eq!(app.state().error(), None);

    // Search Files narrows the list.
    common::fill(&mut app, "Search Files", "bet");
    assert!(app.query_by_label("Alpha").is_none());

    // Double-clicking a file signs in to it.
    let at = app.get_by_label("Beta").rect().center();
    common::workspace::double_click_at(&mut app, at);
    sign_in(&mut app, "Beta");
    common::wait_until(&mut app, "Beta to open", |app| app.state().is_open());
    assert_eq!(app.state().error(), None);
    assert_eq!(app.state().window_name(), "Beta (127.0.0.1)");

    // The File menu reuses the same destination, including its nonstandard
    // port. The user signs in again before the file opens.
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Recent");
    common::click(&mut app, "Beta");
    common::wait_for_label(&mut app, "Open “Beta”");
    app.get_by_label(&format!("Sign in to “Beta” on 127.0.0.1:{port} with:"));
    for field in ["Account Name", "Password"] {
        assert_eq!(
            common::text_field(&app, field).value().unwrap_or_default(),
            ""
        );
    }
    sign_in(&mut app, "Beta");
    common::wait_until(&mut app, "Beta to reopen", |app| app.state().is_open());
    assert_eq!(app.state().window_name(), "Beta (127.0.0.1)");

    // The host is saved with Home's lists, by name, without its account.
    let saved = std::fs::read_to_string(&settings).unwrap();
    assert!(!saved.contains(PASSWORD), "{saved}");
    let again = home(hosted.provider(), &settings);
    again.get_by_label("Loopback");
}

#[test]
fn empty_unsupported_and_stale_listings_are_told_apart_for_their_host() {
    let quiet_files = copies("quiet");
    let quiet = host("quiet", &quiet_files, |server| server);
    let other_files = copies("unsupported");
    let other = host("unsupported", &other_files, |server| {
        server
            .listing_all_files()
            .without_file_list()
            .without_get_file_list()
    });
    let folder = tempfile::tempdir().unwrap();
    let mut app = home(
        quiet.provider_trusting(&[&other]),
        &folder.path().join("settings.json"),
    );

    // A host that publishes nothing: an empty list, not a failure.
    add_host(&mut app, "Quiet", quiet.connection.port);
    let empty = "This host lists no files";
    common::wait_for_label(&mut app, empty);
    assert_eq!(app.state().error(), None);

    // Another port is another host: the first one's list is not shown for
    // it. A host without a list OpenMaker can read says so.
    add_host(&mut app, "Other", other.connection.port);
    common::wait_for_label(&mut app, "OpenMaker cannot list this host’s files");
    assert!(app.query_by_label(empty).is_none());
    assert!(app.query_by_label("Alpha").is_none());
    assert_eq!(app.state().error(), None);

    // A host that accepts the connection and answers nothing for a while,
    // then hangs up: leaving its page stops its listing, and its failure,
    // when it comes, is dropped.
    let slow = TcpListener::bind("127.0.0.1:0").unwrap();
    let slow_port = slow.local_addr().unwrap().port();
    let hang_up = std::thread::spawn(move || {
        let (stream, _) = slow.accept().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        drop(stream);
    });
    add_host(&mut app, "Slow", slow_port);
    common::wait_for_label(&mut app, "Listing files…");
    common::click(&mut app, "Quiet");
    common::wait_for_label(&mut app, empty);
    hang_up.join().unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        app.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.get_by_label(empty);
    assert!(app
        .query_by_label("Could not connect to this host")
        .is_none());
    assert_eq!(app.state().error(), None);

    // A file the host does not list still opens by its name, through Open
    // Network File.
    common::fill(&mut app, "Search Files", "Hidden");
    common::click(&mut app, "Open “Hidden”...");
    common::wait_for_text_field(&mut app, "Path");
    assert_eq!(
        common::text_field(&app, "Path").value(),
        Some(format!("fmnet:/127.0.0.1:{}/Hidden", quiet.connection.port))
    );
    no_license(&mut app);
    common::fill(&mut app, "Account Name", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    // Home's own Open shows behind the dialog, disabled.
    app.query_all_by_label("Open")
        .find(|node| !node.accesskit_node().is_disabled())
        .expect("the dialog's Open")
        .click();
    common::wait_until(&mut app, "Hidden to open", |app| app.state().is_open());
    assert_eq!(app.state().error(), None);
    assert_eq!(app.state().window_name(), "Hidden (127.0.0.1)");
}
