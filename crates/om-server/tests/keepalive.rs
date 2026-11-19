//! End to end: the client's keepalive (network-protocol.md 6.1), what
//! happens when its connection goes (6.2, 6.3), and the server's limits on
//! connections: how many it serves from one address, and how long it keeps
//! a silent one. `om-session` clients log in to an `om-server` on loopback,
//! directly or through a relay that logs every message, holds replies back
//! and can fail the connection or play a misbehaving host. The intervals
//! are cut from FileMaker's 60 s to fractions of a second. Timings are
//! asserted as bounds with room for a loaded machine; the alternative each
//! one rules out takes many times longer (a 30 s read timeout) or never
//! happens.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/relay.rs"]
mod relay;
#[path = "fixtures/server.rs"]
mod server;

use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use fixture::{TempFile, FILE};
use om_protocol::{parse_tickle_parms, parse_tickle_request, MachineInfo, TickleSession};
use om_server::ConnectionLimits;
use om_session::{Session, SessionError, Transport, TransportError};
use relay::Relay;

/// Far less than the 30 s read timeout a request on a dead connection
/// would wait, and far more than failing at once takes on a loaded machine.
const AT_ONCE: Duration = Duration::from_secs(5);

/// Wait up to ten seconds for `done`.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A request on the open file.
fn read_status(session: &mut Session) -> Result<(), SessionError> {
    session.download(&[&[2]]).map(|_| ())
}

/// A TLS connection to the server on `port`, with nothing sent on it.
fn tls(port: u16) -> Result<Transport, TransportError> {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    Transport::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
    )
}

/// The records each Tickle the relay passed on reported, in order.
fn tickles(relay: &Relay) -> Vec<Vec<TickleSession>> {
    relay
        .events()
        .iter()
        .filter(|e| e.client_operation() == Some(&b"Tickle"[..]))
        .map(|e| {
            let request = e.request().unwrap();
            let tickle = parse_tickle_request(request.body).unwrap();
            parse_tickle_parms(tickle.parms).unwrap()
        })
        .collect()
}

#[test]
fn an_idle_session_outlives_the_servers_idle_timeout() {
    let file = TempFile::new("keepalive-idle", &[], true);
    let (port, handle) = server::start_with_idle_timeout(&file, Duration::from_millis(600));
    let (mut session, _) = server::logged_in(port, "idle");
    session.set_keepalive(Some(Duration::from_millis(150)));

    // Silent for more than three of the server's timeouts but for Tickles.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(session.connection_lost(), None);
    read_status(&mut session).unwrap();
    assert_eq!(handle.clients().len(), 1);
    session.disconnect().unwrap();
}

#[test]
fn a_silent_session_is_closed_by_the_server_and_then_refuses_requests() {
    let file = TempFile::new("keepalive-silent", &[], true);
    let (port, handle) = server::start_with_idle_timeout(&file, Duration::from_millis(300));
    let (mut session, _) = server::logged_in(port, "silent");
    session.set_keepalive(None);
    wait_until("the server dropped the client", || {
        handle.clients().is_empty()
    });

    // The host said CloseConnection and closed: the next request learns so.
    let error = read_status(&mut session).unwrap_err();
    assert!(error.is_connection_lost(), "{error:?}");
    let reason = session.connection_lost().unwrap();
    // Every later request fails at once, without the network: with the
    // reason first found, not a new failure of the socket.
    let started = Instant::now();
    match read_status(&mut session) {
        Err(SessionError::ConnectionLost(again)) => assert_eq!(again, reason),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < AT_ONCE);
    assert!(matches!(
        session.disconnect(),
        Err(SessionError::ConnectionLost(_))
    ));
}

#[test]
fn the_keepalive_notices_a_closed_connection_while_idle() {
    let file = TempFile::new("keepalive-notice", &[], true);
    let (port, _handle) = server::start_with_idle_timeout(&file, Duration::from_millis(300));
    let (mut session, _) = server::logged_in(port, "late");
    // Slower than the server's timeout: the first Tickle finds the
    // connection closed, with nobody asking.
    session.set_keepalive(Some(Duration::from_millis(800)));
    wait_until("the keepalive noticed", || {
        session.connection_lost().is_some()
    });
    let error = read_status(&mut session).unwrap_err();
    assert!(error.is_connection_lost(), "{error:?}");
}

#[test]
fn tickles_never_go_out_during_another_exchange() {
    let file = TempFile::new("keepalive-busy", &[], true);
    let port = server::start(&file);
    // Every awaited reply is held 20 ms: room for a Tickle to slip in.
    let relay = Relay::start(port, Duration::from_millis(20));
    let (mut session, _) = server::logged_in(relay.port, "busy");
    let connection = session.identity().connection_id;
    // Due at once whenever the connection is free.
    session.set_keepalive(Some(Duration::from_millis(1)));
    for _ in 0..40 {
        read_status(&mut session).unwrap();
        std::thread::sleep(Duration::from_millis(3));
    }
    session.disconnect().unwrap();
    wait_until("the relay passed the Disconnect on", || {
        relay.count(b"Disconnect") == 1
    });

    assert_eq!(relay.overlaps(), []);
    let events = relay.events();
    let requests: Vec<_> = events
        .iter()
        .filter(|e| e.from_client)
        .filter_map(|e| e.request())
        .collect();
    let tickles: Vec<_> = requests
        .iter()
        .filter(|r| r.operation == b"Tickle")
        .collect();
    assert!(tickles.len() >= 5, "only {} Tickles", tickles.len());
    // One sequence of request ids for every request: even, step 2.
    let ids: Vec<u32> = requests.iter().map(|r| r.request_id).collect();
    assert_eq!(ids[0], 2);
    assert!(ids.windows(2).all(|w| w[1] == w[0] + 2), "{ids:?}");
    // Each names the connection and reports the file's session.
    for tickle in tickles {
        assert!(tickle.response_expected);
        let tickle = parse_tickle_request(tickle.body).unwrap();
        assert_eq!(tickle.connection_id, connection);
        let sessions = parse_tickle_parms(tickle.parms).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, 1);
    }
}

#[test]
fn tickles_run_only_while_a_file_is_open() {
    let file = TempFile::new("keepalive-open", &[], true);
    let port = server::start(&file);
    let relay = Relay::start(port, Duration::ZERO);
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut session = Session::connect(
        "127.0.0.1",
        relay.port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.set_keepalive(Some(Duration::from_millis(20)));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(relay.count(b"Tickle"), 0, "no file is open yet");

    session.open_file("127.0.0.1", FILE).unwrap();
    wait_until("Tickles started", || relay.count(b"Tickle") >= 3);

    session.disconnect().unwrap();
    wait_until("the relay passed the Disconnect on", || {
        relay.count(b"Disconnect") == 1
    });
    let after = relay.count(b"Tickle");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(relay.count(b"Tickle"), after, "a Tickle after Disconnect");
    let last = relay.events().into_iter().rfind(|e| e.from_client).unwrap();
    assert_eq!(last.client_operation(), Some(&b"Disconnect"[..]));
}

#[test]
fn tickles_report_the_seconds_since_the_last_request() {
    let file = TempFile::new("keepalive-seconds", &[], true);
    let port = server::start(&file);
    let relay = Relay::start(port, Duration::ZERO);
    let (mut session, _) = server::logged_in(relay.port, "seconds");
    // The session's last request ended before this.
    let logged_in = Instant::now();
    session.set_keepalive(Some(Duration::from_millis(300)));
    let idle = || -> Vec<u32> {
        tickles(&relay)
            .iter()
            .map(|sessions| sessions[0].idle_seconds)
            .collect()
    };
    wait_until("a Tickle reported 2 s idle", || {
        idle().last().is_some_and(|last| *last >= 2)
    });
    let idle_then = idle();
    // Counting up, and never more than the time that passed.
    assert!(idle_then.windows(2).all(|w| w[0] <= w[1]), "{idle_then:?}");
    let bound = logged_in.elapsed().as_secs();
    assert!(
        idle_then.iter().all(|s| u64::from(*s) <= bound),
        "{idle_then:?} > {bound}"
    );

    // A request of the session's own starts the count again: the next
    // Tickle reports no more than the time since that request.
    read_status(&mut session).unwrap();
    let requested = Instant::now();
    let before = tickles(&relay).len();
    wait_until("the next Tickle", || tickles(&relay).len() > before);
    let since = requested.elapsed().as_secs();
    let after = tickles(&relay)[before][0].idle_seconds;
    assert!(
        u64::from(after) <= since,
        "{after} s idle {since} s after a request"
    );
    session.disconnect().unwrap();
}

#[test]
fn a_dropped_transport_is_detected_and_ends_the_session() {
    let file = TempFile::new("keepalive-drop", &[], true);
    let port = server::start(&file);

    // While idle: the keepalive's Tickle finds the connection gone.
    let relay = Relay::start(port, Duration::ZERO);
    let (mut session, _) = server::logged_in(relay.port, "dropped");
    session.set_keepalive(Some(Duration::from_millis(100)));
    relay.cut();
    wait_until("the keepalive noticed the drop", || {
        session.connection_lost().is_some()
    });
    let started = Instant::now();
    let error = read_status(&mut session).unwrap_err();
    assert!(error.is_connection_lost(), "{error:?}");
    assert!(started.elapsed() < AT_ONCE);

    // In a request: it fails as lost, not after a read timeout.
    let relay = Relay::start(port, Duration::ZERO);
    let (mut session, _) = server::logged_in(relay.port, "cut");
    session.set_keepalive(None);
    relay.cut();
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let error = read_status(&mut session).unwrap_err();
    assert!(error.is_connection_lost(), "{error:?}");
    assert!(started.elapsed() < AT_ONCE);
    assert!(session.connection_lost().is_some());
}

#[test]
fn a_closeconnection_alone_loses_the_connection() {
    let file = TempFile::new("keepalive-close", &[], true);
    let port = server::start(&file);
    let relay = Relay::start(port, Duration::ZERO);
    let (mut session, _) = server::logged_in(relay.port, "closed");
    session.set_keepalive(None);
    // The host says CloseConnection but the TLS connection stays open: no
    // end of stream follows to give the loss away.
    relay.close_connection();
    wait_until("the relay sent CloseConnection", || {
        relay.events().iter().any(|e| {
            !e.from_client
                && om_giop::parse_header(&e.message)
                    .is_ok_and(|(h, _)| h.msg_type == om_giop::MsgType::CloseConnection)
        })
    });
    let started = Instant::now();
    match read_status(&mut session) {
        Err(SessionError::ConnectionLost(reason)) => {
            assert!(reason.contains("closed the connection"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < AT_ONCE);
    assert!(session.connection_lost().is_some());
}

#[test]
fn a_long_request_does_not_use_up_the_idle_allowance() {
    let file = TempFile::new("keepalive-long", &[fixture::slow(1.5)], true);
    let (port, handle) = server::start_with_idle_timeout(&file, Duration::from_millis(700));
    let (mut session, _) = server::logged_in(port, "long");
    session.set_keepalive(Some(Duration::from_millis(300)));
    // Twice the server's idle timeout answering one request, during which
    // the client may not tickle (one exchange at a time).
    let reply = session.perform_script_on_server(3, "", true).unwrap();
    assert_eq!(reply.result, "slow done", "{reply:?}");
    // The allowance counts from the reply: the Tickle 300 ms after it is in
    // time. Counted from the request, the server would close the
    // connection at its next look, 100 ms after replying.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(session.connection_lost(), None);
    read_status(&mut session).unwrap();
    assert_eq!(handle.clients().len(), 1);
    session.disconnect().unwrap();
}

#[test]
fn an_unanswered_tickle_loses_the_connection_however_chatty_the_host() {
    let file = TempFile::new("keepalive-chatty", &[], true);
    let port = server::start(&file);
    let relay = Relay::start(port, Duration::ZERO);
    let (mut session, _) = server::logged_in(relay.port, "chatty");
    let timeout = Duration::from_millis(1500);
    session.set_tickle_timeout(timeout);
    // The host never answers a Tickle, but calls the client every 50 ms:
    // each read finds a message long before any read timeout.
    relay.swallow(b"Tickle");
    relay.chatter(Duration::from_millis(50));
    session.set_keepalive(Some(Duration::from_millis(100)));
    wait_until("a Tickle went out", || relay.count(b"Tickle") == 1);
    let sent = Instant::now();

    // Asking whether the connection is lost does not wait for the Tickle.
    let asked = Instant::now();
    assert_eq!(session.connection_lost(), None);
    assert!(asked.elapsed() < timeout / 3, "{:?}", asked.elapsed());

    let deadline = Instant::now() + Duration::from_secs(10);
    while session.connection_lost().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if session.connection_lost().is_none() {
        // Free the stuck exchange, or dropping the session waits for it.
        relay.cut();
        panic!("the Tickle never timed out");
    }
    assert!(sent.elapsed() >= timeout * 9 / 10, "{:?}", sent.elapsed());
    let reason = session.connection_lost().unwrap();
    assert!(reason.contains("stopped answering"), "{reason}");
    assert!(relay.chatters() >= 10, "{} calls", relay.chatters());
    // The connection is free again: requests fail at once, and closing
    // the session does not wait.
    let started = Instant::now();
    assert!(read_status(&mut session).unwrap_err().is_connection_lost());
    drop(session);
    assert!(started.elapsed() < AT_ONCE);
    assert_eq!(relay.count(b"Tickle"), 1);
}

#[test]
fn a_silent_connection_without_a_session_is_closed_by_default() {
    let file = TempFile::new("keepalive-sessionless", &[], true);
    // The default limits, but for a short idle limit without a session.
    let limits = ConnectionLimits {
        idle_without_session: Duration::from_millis(300),
        ..ConnectionLimits::DEFAULT
    };
    let port = server::start_configured(&file, |server| server.with_connection_limits(limits));
    let (mut session, _) = server::logged_in(port, "silent with a session");
    session.set_keepalive(None);

    // Through TLS, then nothing: no Connect, so no session.
    let mut silent = tls(port).unwrap();
    let close = silent.recv_logical().unwrap();
    let (header, _) = om_giop::parse_header(&close).unwrap();
    assert_eq!(header.msg_type, om_giop::MsgType::CloseConnection);
    // The connection with a session, as silent for as long, is kept: its
    // limit is the default hour.
    assert_eq!(session.connection_lost(), None);
    read_status(&mut session).unwrap();
    session.disconnect().unwrap();
}

#[test]
fn connections_over_the_per_address_cap_are_closed_until_one_ends() {
    let file = TempFile::new("keepalive-cap", &[], true);
    let limits = ConnectionLimits {
        per_address: 2,
        ..ConnectionLimits::DEFAULT
    };
    let port = server::start_configured(&file, |server| server.with_connection_limits(limits));
    let (mut first, _) = server::logged_in(port, "first");
    let second = tls(port).unwrap();

    // A third from the same address is closed at its accept, before TLS:
    // at once, not after a handshake or idle limit.
    let started = Instant::now();
    assert!(tls(port).is_err());
    assert!(started.elapsed() < AT_ONCE);
    // The two admitted are served all the same.
    read_status(&mut first).unwrap();

    // Once one ends, its place is free again.
    first.disconnect().unwrap();
    wait_until("a third connection is let in", || tls(port).is_ok());
    drop(second);
}

#[test]
#[cfg(target_os = "linux")]
fn a_connection_without_a_thread_is_closed_and_the_server_goes_on() {
    let file = TempFile::new("keepalive-no-thread", &[], true);
    // No thread starts with a stack larger than the address space: every
    // connection's thread fails, as when the system has no threads left.
    let port = server::start_configured(&file, |server| server.with_connection_stack_size(1 << 50));
    for _ in 0..3 {
        // Refused here, not accepted, if the accept loop had gone with
        // the first failure.
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tcp.set_read_timeout(Some(AT_ONCE)).unwrap();
        match tcp.read(&mut [0; 16]) {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("the connection was not closed at once: {other:?}"),
        }
    }
}
