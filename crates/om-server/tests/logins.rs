//! End to end: how often logins may fail (`om-server` `login_limits`).
//! Repeated wrong passwords from one address get error 214 without their
//! password being checked, whatever account they name, until the address
//! has waited; an account failing from many addresses is refused from new
//! ones, but not from an address it logged in from or for other names.
//! The server's login clock is stopped, so nothing here waits on time.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::time::Duration;

use fixture::{TempFile, ACCOUNT, CLERK, CLERK_PASSWORD, FILE, INTERNAL_CREATION, PASSWORD};
use om_protocol::{LoginEnvironment, MachineInfo};
use om_server::{ScriptLimits, ServerHandle, LOGIN_TOO_MANY_ATTEMPTS};
use om_session::Session;

const REJECTED: u16 = 212;

/// A server for a fresh copy of the file, its login clock stopped.
fn start(tag: &str) -> (TempFile, u16, ServerHandle) {
    let file = TempFile::new(tag, &[], true);
    let (port, handle) = server::start_with_handle(&file, ScriptLimits::DEFAULT);
    handle.stop_login_clock();
    (file, port, handle)
}

/// A session with the file open, connected to `port` on loopback.
fn opened(port: u16) -> Session {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut session = Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.open_file("127.0.0.1", FILE).unwrap();
    assert_eq!(
        session.fetch_internal_creation().unwrap(),
        INTERNAL_CREATION
    );
    session
}

/// The Login reply's error code.
fn login(session: &mut Session, account: &str, password: &str) -> u16 {
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    session
        .login(account, password, INTERNAL_CREATION, &environment)
        .unwrap()
        .error_code
}

/// [`opened`], the server seeing the connection come from `source`
/// (another loopback address, as from another computer): a pipe on
/// 127.0.0.1 dials the server from `source`.
#[cfg(target_os = "linux")]
fn opened_from(port: u16, source: [u8; 4]) -> Session {
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pipe_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (client, _) = listener.accept().unwrap();
        let socket =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        socket.bind(&SocketAddr::from((source, 0)).into()).unwrap();
        socket
            .connect(&SocketAddr::from(([127, 0, 0, 1], port)).into())
            .unwrap();
        let server = TcpStream::from(socket);
        let (to_server, from_client) = (server.try_clone().unwrap(), client.try_clone().unwrap());
        let up = std::thread::spawn(move || {
            let _ = std::io::copy(&mut &from_client, &mut &to_server);
            let _ = to_server.shutdown(Shutdown::Write);
        });
        let _ = std::io::copy(&mut &server, &mut &client);
        let _ = client.shutdown(Shutdown::Write);
        let _ = up.join();
    });
    opened(pipe_port)
}

/// FileMaker Pro's first Login on each file it opens is the file's "Log in
/// using" account with an empty password, refused, before the user's own
/// on the same connection (network-protocol.md 5.4). A user opening files
/// again and again is not locked out (#440): an accepted login takes back
/// the connection's empty-password failures, also for another account.
/// Wrong passwords stay counted, a login between them notwithstanding, also
/// when the empty-password failure taken back had worn off before them
/// (#1294's review).
#[test]
fn filemaker_pros_automatic_logins_do_not_lock_out_a_user_opening_files() {
    let (_file, port, _handle) = start("logins-automatic");
    let mut user = opened(port);
    for _ in 0..7 {
        assert_eq!(login(&mut user, ACCOUNT, ""), REJECTED);
        assert_eq!(login(&mut user, CLERK, CLERK_PASSWORD), 0);
    }
    for i in 0..4 {
        assert_eq!(login(&mut user, ACCOUNT, &format!("guess {i}")), REJECTED);
    }
    assert_eq!(login(&mut user, CLERK, CLERK_PASSWORD), 0);
    assert_eq!(login(&mut user, ACCOUNT, "guess 4"), REJECTED);
    assert_eq!(
        login(&mut user, CLERK, CLERK_PASSWORD),
        LOGIN_TOO_MANY_ATTEMPTS
    );

    let (_file, port, handle) = start("logins-automatic-late");
    let mut user = opened(port);
    assert_eq!(login(&mut user, ACCOUNT, ""), REJECTED);
    handle.advance_login_clock(Duration::from_secs(61));
    for i in 0..4 {
        assert_eq!(login(&mut user, ACCOUNT, &format!("guess {i}")), REJECTED);
    }
    assert_eq!(login(&mut user, CLERK, CLERK_PASSWORD), 0);
    assert_eq!(login(&mut user, ACCOUNT, "guess 4"), REJECTED);
    assert_eq!(
        login(&mut user, ACCOUNT, "guess 5"),
        LOGIN_TOO_MANY_ATTEMPTS
    );
}

#[test]
fn an_address_that_keeps_failing_waits_whatever_it_tries() {
    let (_file, port, handle) = start("logins-address");
    let mut guesser = opened(port);
    for i in 0..5 {
        assert_eq!(
            login(&mut guesser, ACCOUNT, &format!("guess {i}")),
            REJECTED
        );
    }
    // The right password is refused unchecked, as is any other account,
    // on this connection or a new one from the same address.
    assert_eq!(
        login(&mut guesser, ACCOUNT, PASSWORD),
        LOGIN_TOO_MANY_ATTEMPTS
    );
    assert_eq!(
        login(&mut opened(port), "Nobody", "guess"),
        LOGIN_TOO_MANY_ATTEMPTS
    );
    // Another computer is not held back.
    #[cfg(target_os = "linux")]
    assert_eq!(
        login(&mut opened_from(port, [127, 0, 0, 2]), ACCOUNT, PASSWORD),
        0
    );
    // A minute later the address may try once more: the right password
    // is accepted; a wrong one would have held it back another minute.
    handle.advance_login_clock(Duration::from_secs(59));
    assert_eq!(
        login(&mut guesser, ACCOUNT, PASSWORD),
        LOGIN_TOO_MANY_ATTEMPTS
    );
    handle.advance_login_clock(Duration::from_secs(1));
    assert_eq!(login(&mut guesser, ACCOUNT, PASSWORD), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn an_account_failing_from_many_addresses_is_refused_only_from_new_ones() {
    let (_file, port, handle) = start("logins-account");
    // 127.0.0.1 logs in: an address the account is known from.
    let mut known = opened(port);
    assert_eq!(login(&mut known, ACCOUNT, PASSWORD), 0);
    // Six other addresses fail five times each, the name in any case.
    for host in 2..=7 {
        let mut guesser = opened_from(port, [127, 0, 0, host]);
        for i in 0..5 {
            let code = login(&mut guesser, &ACCOUNT.to_uppercase(), &format!("guess {i}"));
            assert_eq!(code, REJECTED, "127.0.0.{host}");
        }
    }
    // A new address is refused for the account, right password or not,
    // and not for other names.
    let mut new = opened_from(port, [127, 0, 0, 8]);
    assert_eq!(login(&mut new, ACCOUNT, PASSWORD), LOGIN_TOO_MANY_ATTEMPTS);
    assert_eq!(login(&mut new, "Nobody", "guess"), REJECTED);
    // The address the account logged in from still logs in.
    assert_eq!(login(&mut known, ACCOUNT, PASSWORD), 0);
    // Thirty seconds later the account may be tried once more.
    handle.advance_login_clock(Duration::from_secs(30));
    assert_eq!(login(&mut new, ACCOUNT, PASSWORD), 0);
}

/// A login to a file whose accounts could not be read (the test view
/// failing reads, as a damaged page would, docs/server-storage.md 5) is
/// answered 802, "unable to open file", whatever its password: neither
/// 212, which a client counts toward locking the account out, nor a
/// success. It is not counted toward the login limits either: seven from
/// one address, which may fail five at once, are all answered so, none
/// 214. So for the account records `[23, 1, 5]` failing from the start;
/// for them failing only after a read computed the test view's derived
/// state, which it keeps; and for the whole security catalog `[23]`
/// failing, which fails the catalog's computation first.
#[cfg(debug_assertions)]
#[test]
fn a_login_whose_accounts_cannot_be_read_is_answered_unable_to_open() {
    let cases: [(&str, &[&[u8]], bool); 3] = [
        ("logins-unreadable", &[&[23], &[1], &[5]], false),
        ("logins-unreadable-later", &[&[23], &[1], &[5]], true),
        ("logins-unreadable-security", &[&[23]], false),
    ];
    for (tag, path, later) in cases {
        let (_file, port, handle) = start(tag);
        if !later {
            handle.failing_reads(FILE, path);
        }
        // Its download of the status area computes the derived state.
        let mut session = opened(port);
        if later {
            handle.failing_reads(FILE, path);
        }
        for password in [PASSWORD, "wrong", PASSWORD, "", "wrong", PASSWORD, "wrong"] {
            assert_eq!(
                login(&mut session, ACCOUNT, password),
                om_server::LOGIN_UNREADABLE_FILE,
                "{tag}: {password:?}"
            );
        }
        assert_eq!(
            login(&mut session, "Nobody", PASSWORD),
            om_server::LOGIN_UNREADABLE_FILE,
            "{tag}"
        );
        session.disconnect().unwrap();
    }
}
