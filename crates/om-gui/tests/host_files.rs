//! The Hosts dialog's List files end to end: the native provider's host
//! browser against in-process `om-server`s on loopback over verified TLS,
//! hosting copies of the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs` as Alpha, Beta and Hidden. The
//! dialog lists only what a host publishes, for the host it names, opens
//! a chosen file with its account and no client license, and tells an
//! empty list, a host it cannot list, an unreachable host and a listing
//! no longer waited for apart.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
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

/// Fill in the Hosts dialog's host, port and account, not the file. These
/// loopback servers need no client license, and tests never touch the
/// developer's credential store.
fn fill_server(app: &mut common::App, port: u16) {
    if app.query_by_label("Use saved license").is_some() {
        common::click(app, "Use saved license");
    }
    common::fill(app, "Host", "127.0.0.1");
    set_port(app, port);
    common::fill(app, "Account", ACCOUNT);
    common::fill(app, "Password", PASSWORD);
}

fn set_port(app: &mut common::App, port: u16) {
    hosted::replace(app, "Port", &port.to_string());
}

#[test]
fn published_files_are_listed_searched_and_the_chosen_one_opens() {
    let files = copies("listed");
    let hosted = host("listed", &files, |server| {
        server
            .listing_file("Alpha.fmp12")
            .listing_file("Beta.fmp12")
    });
    let mut app = common::harness(Box::new(hosted.provider()));
    fill_server(&mut app, hosted.connection.port);

    common::click(&mut app, "List files");
    common::wait_for_label(&mut app, "Beta.fmp12");
    app.get_by_label("Alpha.fmp12");
    // What the host says it is; nothing of the file it does not publish.
    app.get_by_label("openmaker-test \u{b7} OpenMaker Server test");
    assert!(app.query_by_label("Hidden.fmp12").is_none());
    assert_eq!(app.state().error(), None);

    // Search Files narrows the list.
    common::fill(&mut app, "Search Files", "bet");
    assert!(app.query_by_label("Alpha.fmp12").is_none());

    // Choosing a file names it in File; double-clicking it opens it with
    // the account typed, without a license.
    common::click(&mut app, "Beta.fmp12");
    assert_eq!(
        common::text_field(&app, "File").value().as_deref(),
        Some("Beta.fmp12")
    );
    let at = app.get_by_label("Beta.fmp12").rect().center();
    common::workspace::double_click_at(&mut app, at);
    common::wait_until(&mut app, "Beta to open", |app| app.state().is_open());
    assert_eq!(app.state().error(), None);
    assert_eq!(app.state().window_name(), "Beta (127.0.0.1)");
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
    let mut app = common::harness(Box::new(quiet.provider_trusting(&[&other])));
    fill_server(&mut app, quiet.connection.port);
    common::fill(&mut app, "File", "Typed.fmp12");

    // A host that publishes nothing: an empty list, not a failure.
    let empty = "This host lists no files. Enter a file name below to open one.";
    common::click(&mut app, "List files");
    common::wait_for_label(&mut app, empty);
    assert_eq!(app.state().error(), None);

    // Another port is another host: the first one's list is not shown for
    // it. A host without a list OpenMaker can read says so.
    set_port(&mut app, other.connection.port);
    assert!(app.query_by_label(empty).is_none());
    common::click(&mut app, "List files");
    common::wait_for_label(&mut app, "OpenMaker cannot list this host's files.");
    assert!(app.query_by_label("Alpha.fmp12").is_none());
    assert_eq!(app.state().error(), None);

    // A host that accepts the connection and answers nothing for a while,
    // then hangs up: its listing is canceled while it waits, and its
    // failure, when it comes, is dropped.
    let slow = TcpListener::bind("127.0.0.1:0").unwrap();
    let slow_port = slow.local_addr().unwrap().port();
    let hang_up = std::thread::spawn(move || {
        let (stream, _) = slow.accept().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        drop(stream);
    });
    set_port(&mut app, slow_port);
    common::click(&mut app, "List files");
    app.get_by_label("Listing files...");
    common::click(&mut app, "Cancel");
    assert!(app.query_by_label("Listing files...").is_none());
    app.get_by_label("Listing files was canceled.");
    assert_eq!(app.state().error(), None);

    // The quiet host again; the slow one's late failure changes nothing.
    set_port(&mut app, quiet.connection.port);
    common::click(&mut app, "List files");
    common::wait_for_label(&mut app, empty);
    hang_up.join().unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        app.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.get_by_label(empty);
    assert!(app
        .query_by_label("Could not connect to this host.")
        .is_none());
    assert_eq!(app.state().error(), None);
    // The form is as typed.
    assert_eq!(
        common::text_field(&app, "File").value().as_deref(),
        Some("Typed.fmp12")
    );
    assert!(!app.state().is_open());
}
