//! A GIOP relay between one client and a server on loopback, like the
//! capture relay (`tools/capture/fm_tls_proxy.py`) in-process: it ends the
//! client's TLS, dials the server with a TLS client of its own, and passes
//! every GIOP message on, keeping a log of them in order. It notes a client
//! request sent while an earlier one still waits for its reply, can hold
//! replies back to widen that window, and can cut the connection the way a
//! failed network does (no TLS close, no GIOP CloseConnection). Playing a
//! host that misbehaves, it can also swallow requests of an operation
//! (they get no reply), keep calling the client with `Notify`, and send
//! the client a GIOP CloseConnection without closing the connection.
#![allow(dead_code)]

use std::collections::HashSet;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use om_giop::{parse_header, MessageReader, MsgType, Reply, Request, Writer};
use om_server::tls::{server_config, Identity};
use om_session::Transport;
use rustls::{ServerConnection, StreamOwned};

/// How long each side's read waits before the relay looks at the other.
const POLL: Duration = Duration::from_millis(2);

/// One message the relay passed on.
#[derive(Clone, Debug)]
pub struct Event {
    pub at: Instant,
    pub from_client: bool,
    pub message: Vec<u8>,
}

impl Event {
    /// The message's request, if it is one.
    pub fn request(&self) -> Option<Request<'_>> {
        Request::parse(&self.message).ok()
    }

    /// The operation of a client request, e.g. `b"Tickle"`.
    pub fn client_operation(&self) -> Option<&[u8]> {
        self.from_client
            .then(|| self.request().map(|r| r.operation))
            .flatten()
    }
}

#[derive(Default)]
struct State {
    events: Vec<Event>,
    /// Client requests sent while another awaited its reply: (waiting,
    /// interloper) request ids.
    overlaps: Vec<(u32, u32)>,
    /// Operations whose client requests are logged but not passed on.
    swallowed: Vec<Vec<u8>>,
    /// Call the client with `Notify` this often.
    chatter: Option<Duration>,
    /// Drop the response after this Perform opcode has run on the server.
    cut_after_opcode: Option<u8>,
    cut_reply: Option<u32>,
    corrupt_reply: bool,
    refuse_commits: bool,
}

/// A relay serving one connection.
pub struct Relay {
    pub port: u16,
    pub ca_pem: Vec<u8>,
    state: Arc<Mutex<State>>,
    cut: Arc<AtomicBool>,
    hang_up: Arc<AtomicBool>,
}

impl Relay {
    /// Acknowledge op70 without forwarding it, modeling a statusless refusal.
    pub fn refuse_commits(&self) {
        self.state.lock().unwrap().refuse_commits = true;
    }

    /// Listen on an ephemeral loopback port and relay the first client to
    /// the server on `server_port`. Each reply a client request waits for is
    /// held for `reply_delay` before the client gets it.
    pub fn start(server_port: u16, reply_delay: Duration) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let cut = Arc::new(AtomicBool::new(false));
        let hang_up = Arc::new(AtomicBool::new(false));
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let ca_pem = cert.pem().into_bytes();
        let identity = Identity::from_pem(&ca_pem, key.serialize_pem().as_bytes()).unwrap();
        let (log, stop, close) = (state.clone(), cut.clone(), hang_up.clone());
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            relay(tcp, server_port, reply_delay, &log, &stop, &close, identity);
        });
        Relay {
            port,
            ca_pem,
            state,
            cut,
            hang_up,
        }
    }

    /// Lose the reply after the server executes a particular Perform opcode.
    pub fn lose_reply_after(&self, opcode: om_protocol::Opcode) {
        self.state.lock().unwrap().cut_after_opcode = Some(opcode as u8);
    }

    /// Replace the executed opcode's response with an invalid empty
    /// Perform result, leaving the transport usable for subsequent requests.
    pub fn corrupt_reply_after(&self, opcode: om_protocol::Opcode) {
        let mut state = self.state.lock().unwrap();
        state.cut_after_opcode = Some(opcode as u8);
        state.corrupt_reply = true;
    }

    /// Fail the connection: both sockets are shut down without a word.
    pub fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
    }

    /// Send the client a GIOP CloseConnection, as a host closing the
    /// connection does, but keep the connection open: from then on nothing
    /// is passed on either way, and only the client can end it.
    pub fn close_connection(&self) {
        self.hang_up.store(true, Ordering::SeqCst);
    }

    /// From now on, log the client's requests of `operation` without
    /// passing them on: they get no reply.
    pub fn swallow(&self, operation: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .swallowed
            .push(operation.to_vec());
    }

    /// From now on, call the client with an empty `Notify` every `every`
    /// (its answers are not passed on).
    pub fn chatter(&self, every: Duration) {
        self.state.lock().unwrap().chatter = Some(every);
    }

    /// The `Notify` calls the relay made of its own.
    pub fn chatters(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| !e.from_client && e.request().is_some_and(|r| r.operation == b"Notify"))
            .count()
    }

    pub fn events(&self) -> Vec<Event> {
        self.state.lock().unwrap().events.clone()
    }

    /// Client requests that went out while an earlier one waited for its
    /// reply.
    pub fn overlaps(&self) -> Vec<(u32, u32)> {
        self.state.lock().unwrap().overlaps.clone()
    }

    /// The client's requests of `operation` so far.
    pub fn count(&self, operation: &[u8]) -> usize {
        self.events()
            .iter()
            .filter(|e| e.client_operation() == Some(operation))
            .count()
    }
}

/// The client side: the relay's end of the client's TLS.
struct ClientSide {
    stream: StreamOwned<ServerConnection, TcpStream>,
    reader: MessageReader,
}

impl ClientSide {
    /// The client's next message; `Ok(None)` when it sent nothing in time,
    /// `Err` once it is gone.
    fn recv(&mut self) -> Result<Option<Vec<u8>>, ()> {
        match self.reader.read_logical(&mut self.stream) {
            Ok(msg) => Ok(Some(msg)),
            Err(om_giop::ReadError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(_) => Err(()),
        }
    }

    fn send(&mut self, msg: &[u8]) {
        use std::io::Write;
        let _ = self
            .stream
            .write_all(msg)
            .and_then(|()| self.stream.flush());
    }
}

/// An empty `Notify` call (write-path.md 6.4) with request id `id`.
fn notify(id: u32) -> Vec<u8> {
    let mut body = Writer::with_origin(0);
    body.u32(0);
    body.string(b"");
    body.octet_sequence(&[]);
    om_giop::build_request(
        id,
        true,
        om_protocol::OBJECT_KEY,
        b"Notify",
        &[],
        &body.into_bytes(),
    )
}

fn relay(
    tcp: TcpStream,
    server_port: u16,
    reply_delay: Duration,
    log: &Mutex<State>,
    cut: &AtomicBool,
    hang_up: &AtomicBool,
    identity: Identity,
) {
    let mut conn = ServerConnection::new(server_config(identity).unwrap()).unwrap();
    let mut tcp = tcp;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).unwrap();
    }
    tcp.set_read_timeout(Some(POLL)).unwrap();
    let raw = tcp.try_clone().unwrap();
    let mut client = ClientSide {
        stream: StreamOwned::new(conn, tcp),
        reader: MessageReader::new(om_session::MAX_MESSAGE_LEN),
    };
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["relay".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let mut server = Transport::connect(
        "127.0.0.1",
        server_port,
        cert.pem().as_bytes(),
        key.serialize_pem().as_bytes(),
    )
    .unwrap();
    server.set_read_timeout(Some(POLL)).unwrap();

    // The client request awaiting its reply.
    let mut waiting: Option<u32> = None;
    // The relay's own calls of the client (odd ids, as the server's are).
    let mut calls = HashSet::new();
    let mut next_call = 1_000_001u32;
    let mut next_chatter = Instant::now();
    let mut hung = false;
    let from_client =
        |msg: Vec<u8>, waiting: &mut Option<u32>, calls: &HashSet<u32>, server: &mut Transport| {
            let mut state = log.lock().unwrap();
            let mut pass = true;
            if let Ok(request) = Request::parse(&msg) {
                if request.response_expected {
                    if let Some(earlier) = *waiting {
                        state.overlaps.push((earlier, request.request_id));
                    }
                    *waiting = Some(request.request_id);
                }
                pass = !state.swallowed.iter().any(|op| *op == request.operation);
                if request.operation == b"Perform"
                    && state.cut_after_opcode.is_some_and(|opcode| {
                        om_protocol::parse_perform_request(request.body)
                            .is_some_and(|perform| perform.transaction.first() == Some(&opcode))
                    })
                {
                    state.cut_reply = Some(request.request_id);
                }
            } else if parse_header(&msg).is_ok_and(|(h, _)| h.msg_type == MsgType::Reply) {
                pass = !Reply::parse(&msg).is_ok_and(|r| calls.contains(&r.request_id));
            }
            state.events.push(Event {
                at: Instant::now(),
                from_client: true,
                message: msg.clone(),
            });
            drop(state);
            if pass {
                let _ = server.send(&msg);
            }
        };
    let to_client = |msg: Vec<u8>, client: &mut ClientSide| {
        log.lock().unwrap().events.push(Event {
            at: Instant::now(),
            from_client: false,
            message: msg.clone(),
        });
        client.send(&msg);
    };
    loop {
        if cut.load(Ordering::SeqCst) {
            let _ = raw.shutdown(Shutdown::Both);
            return;
        }
        if hung {
            // Read what the client still sends, pass nothing on, and keep
            // the connection until the client leaves.
            match client.recv() {
                Ok(Some(msg)) => log.lock().unwrap().events.push(Event {
                    at: Instant::now(),
                    from_client: true,
                    message: msg,
                }),
                Ok(None) => {}
                Err(()) => return,
            }
            continue;
        }
        if hang_up.load(Ordering::SeqCst) {
            hung = true;
            to_client(om_giop::build_close_connection(), &mut client);
            continue;
        }
        let chatter = log.lock().unwrap().chatter;
        if let Some(every) = chatter.filter(|_| Instant::now() >= next_chatter) {
            next_chatter = Instant::now() + every;
            calls.insert(next_call);
            to_client(notify(next_call), &mut client);
            next_call += 2;
        }
        match client.recv() {
            Ok(Some(msg)) => {
                if let Ok(request) = Request::parse(&msg) {
                    if log.lock().unwrap().refuse_commits
                        && request.operation == b"Perform"
                        && om_protocol::parse_perform_request(request.body).is_some_and(|p| {
                            p.transaction.first() == Some(&(om_protocol::Opcode::HostCommit as u8))
                        })
                    {
                        let done = om_protocol::write::upload_reply(
                            &om_protocol::Resume::no_stream(),
                            0x100000,
                        );
                        to_client(
                            om_protocol::build_perform_reply(request.request_id, &done, &[]),
                            &mut client,
                        );
                        continue;
                    }
                }
                from_client(msg, &mut waiting, &calls, &mut server);
                continue;
            }
            Ok(None) => {}
            // The client left: so does the relay.
            Err(()) => return,
        }
        let msg = match server.recv_logical() {
            Ok(msg) => msg,
            Err(e) if e.is_timeout() => continue,
            // The server closed: so does the relay.
            Err(_) => {
                let _ = raw.shutdown(Shutdown::Both);
                return;
            }
        };
        let answers = parse_header(&msg).is_ok_and(|(h, _)| h.msg_type == MsgType::Reply)
            && Reply::parse(&msg).is_ok_and(|r| Some(r.request_id) == waiting);
        let corrupt = {
            let state = log.lock().unwrap();
            (answers && state.cut_reply.is_some_and(|id| Some(id) == waiting))
                .then_some(state.corrupt_reply)
        };
        if let Some(corrupt) = corrupt {
            if corrupt {
                to_client(
                    om_protocol::build_perform_reply(waiting.expect("reply id"), &[], &[]),
                    &mut client,
                );
                waiting = None;
                continue;
            }
            let _ = raw.shutdown(Shutdown::Both);
            return;
        }
        if answers {
            // Hold the reply, then take whatever the client sent before
            // it could have seen it.
            std::thread::sleep(reply_delay);
            while let Ok(Some(early)) = client.recv() {
                from_client(early, &mut waiting, &calls, &mut server);
            }
            waiting = None;
        }
        to_client(msg, &mut client);
    }
}
