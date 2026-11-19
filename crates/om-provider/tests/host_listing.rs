//! Hosted-file discovery (the Hosts dialog's List files) against an
//! `om-server` on loopback over verified TLS, hosting three copies of the
//! in-memory Tasks file of `om-server/tests/fixtures/tasks.rs`: Alpha and
//! Beta published, Hidden not. The client holds no license; the discovery
//! connection is its own and closed after each listing.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fixture::{TempFile, ACCOUNT, LAYOUT, PASSWORD};
use om_data::{Connection, DataProvider, HostListing, ListingError, ListingFailure};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerHandle, ServerIdentity};

struct Host {
    port: u16,
    handle: ServerHandle,
    ca_pem: Vec<u8>,
    _files: Vec<TempFile>,
}

/// A server hosting Alpha, Beta and Hidden, changed by `configure`.
fn host(tag: &str, configure: impl FnOnce(Server) -> Server) -> Host {
    let files: Vec<TempFile> = ["alpha", "beta", "hidden"]
        .iter()
        .map(|name| TempFile::new(&format!("listing-{tag}-{name}"), &[], true))
        .collect();
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: ["Alpha.fmp12", "Beta.fmp12", "Hidden.fmp12"]
            .iter()
            .zip(&files)
            .map(|(name, file)| HostedFile {
                name: (*name).into(),
                path: file.0.clone(),
            })
            .collect(),
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    let server = configure(server);
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    Host {
        port,
        handle,
        ca_pem,
        _files: files,
    }
}

/// A provider trusting the host, with no client license.
fn provider(ca_pem: &[u8]) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let machine = MachineInfo::default();
    assert!(machine.install_code.is_empty());
    NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem.to_vec(),
        callback_host: "127.0.0.1".into(),
        machine,
    })
}

fn browse(provider: &NativeProvider, port: u16) -> Result<HostListing, ListingError> {
    let browser = provider.host_browser().expect("a native browser");
    browser.browse("127.0.0.1", port, &AtomicBool::new(false))
}

fn names(listing: &HostListing) -> Vec<&str> {
    listing.files.iter().map(|f| f.name.as_str()).collect()
}

fn connection(port: u16, file: &str, password: &str) -> Connection {
    Connection {
        host: "127.0.0.1".into(),
        port,
        file: file.into(),
        account: ACCOUNT.into(),
        password: password.into(),
    }
}

/// The server's connected clients settle to `count` (a closed
/// connection's session ends once the server reads its Disconnect).
fn settles_to(handle: &ServerHandle, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.clients().len() != count {
        assert!(
            Instant::now() < deadline,
            "{} clients, not {count}",
            handle.clients().len()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn two_published_files_are_listed_and_the_chosen_one_opens_with_its_account() {
    // Names are matched case-insensitively, as FileMaker matches them.
    let host = host("listed", |s| {
        s.listing_file("beta.fmp12").listing_file("Alpha.fmp12")
    });
    let mut provider = provider(&host.ca_pem);
    let listing = browse(&provider, host.port).unwrap();
    // Only the published files, by name, alphabetically; what the host
    // says it is when the client connects.
    assert_eq!(names(&listing), ["Alpha.fmp12", "Beta.fmp12"]);
    assert_eq!(listing.host_name, "openmaker-test");
    assert_eq!(listing.host_version, "OpenMaker Server test");
    // The discovery connection is gone.
    settles_to(&host.handle, 0);

    // Listing is not authorization: the chosen file still needs its
    // account, and a wrong password opens nothing.
    assert!(provider
        .open(&connection(host.port, "Beta.fmp12", "wrong"))
        .is_err());
    provider
        .open(&connection(host.port, "Beta.fmp12", PASSWORD))
        .unwrap();
    let layout = u32::try_from(LAYOUT).unwrap();
    assert_eq!(provider.records(layout, 0, 10).unwrap().total, 2);
    settles_to(&host.handle, 1);

    // Listing again while the file is open uses a connection of its own:
    // the open file's session is untouched and still reads.
    assert_eq!(
        provider.list_files("127.0.0.1", host.port).unwrap().len(),
        2
    );
    settles_to(&host.handle, 1);
    assert!(provider.connection_lost().is_none());
    assert_eq!(provider.records(layout, 0, 10).unwrap().total, 2);
    let open: Vec<_> = host.handle.clients()[0]
        .files
        .iter()
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(open, ["Beta.fmp12"]);

    // An unlisted file is not listed, but still opens by name.
    let mut hidden = self::provider(&host.ca_pem);
    hidden
        .open(&connection(host.port, "Hidden.fmp12", PASSWORD))
        .unwrap();
    hidden.close();
    provider.close();
}

#[test]
fn empty_unsupported_malformed_unreachable_and_canceled_are_told_apart() {
    // A host that publishes nothing lists nothing: not a failure.
    let quiet = host("quiet", |s| s);
    let provider = provider(&quiet.ca_pem);
    assert!(browse(&provider, quiet.port).unwrap().files.is_empty());

    // A host without OpenMaker's list is asked with FileMaker's GetFileList
    // (op77), as FileMaker Pro's Hosts window asks: it lists the same files.
    let both = host("both-lists", |s| s.listing_all_files());
    let provider = self::provider(&both.ca_pem);
    let by_extension = browse(&provider, both.port).unwrap();
    let native = host("op77", |s| s.listing_all_files().without_file_list());
    let provider = self::provider(&native.ca_pem);
    let by_op77 = browse(&provider, native.port).unwrap();
    assert!(!by_op77.files.is_empty());
    assert_eq!(by_op77.files, by_extension.files);

    // A host offering neither list (op77 answered with a bare error 3) is
    // unsupported, not empty.
    let other = host("unsupported", |s| {
        s.listing_all_files()
            .without_file_list()
            .without_get_file_list()
    });
    let provider = self::provider(&other.ca_pem);
    let error = browse(&provider, other.port).unwrap_err();
    assert_eq!(error.failure, ListingFailure::Unsupported, "{error}");
    assert!(error.message.contains("OpenMaker Server test"), "{error}");

    // A list cut short is malformed, never a shorter list.
    let mut body = Vec::new();
    for word in [1u32, 2, 12] {
        body.extend_from_slice(&word.to_le_bytes());
    }
    body.extend_from_slice(b"Alpha.fmp12\0");
    let cut = host("malformed", |s| {
        s.listing_all_files().overriding_file_list(body)
    });
    let provider = self::provider(&cut.ca_pem);
    let error = browse(&provider, cut.port).unwrap_err();
    assert_eq!(error.failure, ListingFailure::Malformed, "{error}");

    // Each failed listing closed its connection.
    settles_to(&other.handle, 0);
    settles_to(&native.handle, 0);
    settles_to(&cut.handle, 0);

    // Nothing listening.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let error = browse(&provider, port).unwrap_err();
    assert_eq!(error.failure, ListingFailure::Unreachable, "{error}");

    // Canceled before it connects.
    let browser = provider.host_browser().unwrap();
    let error = browser
        .browse("127.0.0.1", quiet.port, &AtomicBool::new(true))
        .unwrap_err();
    assert_eq!(error.failure, ListingFailure::Canceled);
    settles_to(&quiet.handle, 0);

    // Cancel after TCP connects but before the TLS step reports its I/O
    // failure. The late connection failure must remain a typed cancellation.
    let stalled = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = stalled.local_addr().unwrap().port();
    let canceled = Arc::new(AtomicBool::new(false));
    let cancel_on_accept = canceled.clone();
    let peer = std::thread::spawn(move || {
        let (stream, _) = stalled.accept().unwrap();
        cancel_on_accept.store(true, Ordering::SeqCst);
        drop(stream);
    });
    let error = browser.browse("127.0.0.1", port, &canceled).unwrap_err();
    peer.join().unwrap();
    assert_eq!(error.failure, ListingFailure::Canceled, "{error}");
}
