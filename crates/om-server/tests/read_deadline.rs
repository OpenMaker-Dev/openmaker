//! `Transport::recv_logical_by`, the read a Tickle waits for its reply
//! with (network-protocol.md 6.1): it gives up at its deadline however
//! slowly the host sends, whether one TLS record a byte at a time, a large
//! message in pieces, or GIOP fragments. A TLS server on loopback plays the
//! slow host. And the other way round: OpenMaker Server's deadline for a
//! client's whole TLS handshake, which a client sending it a byte at a time
//! cannot stretch either.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fixture::TempFile;
use om_server::tls::{server_config, Identity};
use om_server::ConnectionLimits;
use om_session::{Transport, TransportError};
use rustls::{ServerConnection, StreamOwned};

/// The deadline each read is given.
const DEADLINE: Duration = Duration::from_millis(250);

/// A slow host: once the client is connected, `send` writes to the TLS
/// connection's socket until it returns or `stop` is set.
fn host(
    send: impl FnOnce(StreamOwned<ServerConnection, std::net::TcpStream>, &AtomicBool) + Send + 'static,
) -> (u16, Arc<AtomicBool>, mpsc::Sender<()>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let (go, start) = mpsc::channel();
    let stopped = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
        let mut conn = ServerConnection::new(server_config(identity).unwrap()).unwrap();
        while conn.is_handshaking() {
            conn.complete_io(&mut tcp).unwrap();
        }
        tcp.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
        start.recv().unwrap();
        send(StreamOwned::new(conn, tcp), &stopped);
    });
    (port, stop, go, thread)
}

fn connect(port: u16) -> Transport {
    let key = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
    Transport::connect(
        "127.0.0.1",
        port,
        key.cert.pem().as_bytes(),
        key.signing_key.serialize_pem().as_bytes(),
    )
    .unwrap()
}

/// Read with the deadline while the host sends: how long it took, and
/// what it read.
fn read_by_deadline(
    port: u16,
    stop: &AtomicBool,
    go: &mpsc::Sender<()>,
    thread: JoinHandle<()>,
) -> (Duration, Result<Vec<u8>, TransportError>) {
    let mut transport = connect(port);
    go.send(()).unwrap();
    let started = Instant::now();
    let read = transport.recv_logical_by(started + DEADLINE, Some(Duration::from_secs(30)));
    let elapsed = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    drop(transport);
    thread.join().unwrap();
    (elapsed, read)
}

/// A `Notify` call with a 128 KiB body.
fn large_notify() -> Vec<u8> {
    let mut body = om_giop::Writer::with_origin(0);
    body.u32(0);
    body.string(b"");
    body.octet_sequence(&vec![0; 128 * 1024]);
    om_giop::build_request(
        1,
        true,
        om_protocol::OBJECT_KEY,
        b"Notify",
        &[],
        &body.into_bytes(),
    )
}

/// `message` written 256 bytes at a time, 40 ms apart: whole TLS records,
/// none of which completes the message in time.
fn in_pieces(message: Vec<u8>) -> (Duration, Result<Vec<u8>, TransportError>) {
    let (port, stop, go, thread) = host(move |mut stream, stop| {
        for piece in message.chunks(256) {
            if stop.load(Ordering::SeqCst)
                || stream
                    .write_all(piece)
                    .and_then(|()| stream.flush())
                    .is_err()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    read_by_deadline(port, &stop, &go, thread)
}

#[test]
fn one_tls_record_sent_a_byte_at_a_time_does_not_stretch_the_deadline() {
    // One TLS record carrying a 12-byte CloseConnection, its bytes 50 ms
    // apart: about 1.7 s in all. TLS reads the socket many times for it.
    let (port, stop, go, thread) = host(|stream, stop| {
        let (mut conn, mut tcp) = (stream.conn, stream.sock);
        conn.writer()
            .write_all(&om_giop::build_close_connection())
            .unwrap();
        let mut record = Vec::new();
        while conn.wants_write() {
            conn.write_tls(&mut record).unwrap();
        }
        for byte in record {
            if stop.load(Ordering::SeqCst) || tcp.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let (elapsed, read) = read_by_deadline(port, &stop, &go, thread);
    assert!(
        read.as_ref().is_err_and(TransportError::is_timeout),
        "the record that came after the deadline was read: {read:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

#[test]
fn a_large_message_sent_in_pieces_does_not_stretch_the_deadline() {
    let (elapsed, read) = in_pieces(large_notify());
    assert!(
        read.as_ref().is_err_and(TransportError::is_timeout),
        "{read:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

#[test]
fn giop_fragments_sent_slowly_do_not_stretch_the_deadline() {
    // The same Notify as a first message of 24 body bytes and 128-byte
    // GIOP 1.2 Fragments.
    let message = large_notify();
    let first_body = 24;
    let mut fragmented = message[..12 + first_body].to_vec();
    fragmented[6] |= 2;
    fragmented[8..12].copy_from_slice(&u32::try_from(first_body).unwrap().to_le_bytes());
    let pieces: Vec<&[u8]> = message[12 + first_body..].chunks(128).collect();
    for (i, piece) in pieces.iter().enumerate() {
        let more = if i + 1 == pieces.len() { 1 } else { 3 };
        fragmented.extend_from_slice(b"GIOP");
        fragmented.extend_from_slice(&[1, 2, more, 7]);
        fragmented.extend_from_slice(&u32::try_from(4 + piece.len()).unwrap().to_le_bytes());
        fragmented.extend_from_slice(&1u32.to_le_bytes());
        fragmented.extend_from_slice(piece);
    }
    let (elapsed, read) = in_pieces(fragmented);
    assert!(
        read.as_ref().is_err_and(TransportError::is_timeout),
        "{read:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

#[test]
fn a_tls_handshake_sent_a_byte_at_a_time_ends_at_the_servers_deadline() {
    let file = TempFile::new("handshake-deadline", &[], true);
    let handshake = Duration::from_secs(1);
    let limits = ConnectionLimits {
        handshake,
        ..ConnectionLimits::DEFAULT
    };
    let port = server::start_configured(&file, |server| server.with_connection_limits(limits));
    // A real ClientHello, so the server waits for the rest of it rather
    // than refusing what it has.
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    let mut client =
        rustls::ClientConnection::new(Arc::new(config), "localhost".try_into().unwrap()).unwrap();
    let mut hello = Vec::new();
    while client.wants_write() {
        client.write_tls(&mut hello).unwrap();
    }

    // A byte every 50 ms or more: each read of the handshake gets one long
    // before any per-read timeout, and all of them take many seconds.
    let started = Instant::now();
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut sent = 0;
    let closed = loop {
        if sent == hello.len() {
            break false;
        }
        if tcp.write_all(&hello[sent..=sent]).is_ok() {
            sent += 1;
        }
        match tcp.read(&mut [0; 64]) {
            Ok(0) => break true,
            Err(e)
                if !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break true
            }
            _ => {}
        }
    };
    assert!(closed, "the whole ClientHello went out a byte at a time");
    assert!(started.elapsed() >= handshake, "{:?}", started.elapsed());
    assert!(sent < hello.len(), "{sent} of {} bytes", hello.len());
}
