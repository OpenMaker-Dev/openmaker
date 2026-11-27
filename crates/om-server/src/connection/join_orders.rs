//! Every order of the steps by which a session's owner, the administrator
//! and the server take a session's authority away, against the steps of
//! the transports that joined it and of addresses guessing at it (#968),
//! run by `joins::model` with its three invariants checked after every
//! step.
//!
//! Driven as they run: `Connection::end_session`, `clear_file_session`,
//! `logged_in`, `Session::publish`, `Shared::disconnect_session`,
//! `Shared::close_connections`, `Connection::join` (with `Joins::admit`, the
//! lookup and the rule) and `Seat::table`; the engine releases the
//! session's locks for real, a step of its own (`Section::Release`).
//! Stubbed: a Login's password check (a step of its own,
//! `Section::Login`), the engine's work in a joined read (each read pages
//! a fixed stream into the transport's real table, as `download` does),
//! and the parsing of requests. A scenario's points are the sections more
//! than one of its actors takes, which the model checks.

use std::any::Any;

use om_protocol::Resume;

use super::*;
use crate::config::JoinRefusals;
use crate::joins::model::{self, Event, Explored, Kept, Removal, Run};
use crate::joins::Section;
use crate::scripts::fixture;

const CERTIFICATE: joins::CertificateHash = [1; 32];
const PAGE: usize = 512;

/// A hosted file and the server state around it, kept for every order of
/// a scenario.
struct Lab {
    _file: fixture::TempFile,
    shared: Arc<Shared>,
    hosted: Arc<Hosted>,
}

impl Lab {
    fn new(name: &str, refusals: JoinRefusals) -> Lab {
        let file = fixture::TempFile::new(name, &[], true);
        let shared = crate::scripts::tests::shared(&file);
        let hosted = crate::scripts::tests::hosted(&shared);
        shared.joins.set_limits(refusals);
        Lab {
            _file: file,
            shared,
            hosted,
        }
    }

    /// A session's own transport from `address`, logged in to the file.
    fn owner(&self, address: IpAddr, port: u16) -> (Connection, u32) {
        let control = Arc::new(Control::default());
        let socket = SocketAddr::new(address, port);
        let mut owner = Connection::new(Arc::clone(&self.shared), socket, Arc::clone(&control))
            .with_client_certificate(Some(CERTIFICATE));
        let id = self.shared.new_connection_id();
        self.shared.clients.connected(
            id,
            socket,
            &om_protocol::MachineInfo::default(),
            Arc::clone(&control),
        );
        let grant = self
            .shared
            .joins
            .open(id, owner.transport, &owner.peer, control);
        let mut session = Session {
            connection_id: id,
            key: [0; 16],
            name: "owner".into(),
            open: HashMap::new(),
            transfers: Transfers::new(
                &self.shared.upload_memory,
                &self.shared.download_memory,
                address,
            ),
            temp: HashMap::new(),
            client: address,
            version: String::new(),
            address,
            access: None,
            callback: Callback::Unlocated,
            file_list: None,
            automatic_logins: Vec::new(),
            grant,
        };
        let mut file = OpenFile {
            hosted: Arc::clone(&self.hosted),
            nonce: 0,
            account: None,
            close: CloseStep::None,
            withheld: Default::default(),
            login: 0,
        };
        logged_in(&mut file, self.account());
        session.open.insert(self.hosted.id, file);
        session.publish();
        owner.session = Some(session);
        (owner, id)
    }

    fn account(&self) -> Arc<FileAccount> {
        self.hosted
            .account(fixture::ACCOUNT)
            .expect("the file's account")
    }

    /// A transport from `address` naming session `session` for the file,
    /// with the session's client certificate or none.
    fn joiner(&self, address: IpAddr, port: u16, certified: bool, session: u32) -> Joiner {
        let socket = SocketAddr::new(address, port);
        let connection = Connection::new(
            Arc::clone(&self.shared),
            socket,
            Arc::new(Control::default()),
        )
        .with_client_certificate(certified.then_some(CERTIFICATE));
        Joiner {
            connection,
            session,
            file: self.hosted.id,
            admitted: None,
            resume: None,
            handles: 0,
        }
    }
}

/// A transport that is not a session's own, reading through it.
struct Joiner {
    connection: Connection,
    session: u32,
    file: u32,
    /// The file as its last admission granted it.
    admitted: Option<FileGrant>,
    /// The download it pages.
    resume: Option<Resume>,
    handles: u64,
}

impl Joiner {
    fn header(&self) -> Header {
        let mut header = Header::new(Opcode::DownloadWithLock as u8);
        header.file_id = self.file;
        header
    }

    /// A request's admission (`Connection::join`).
    fn admit(&mut self) -> bool {
        match self.connection.join(self.session, &self.header()) {
            Ok(file) => {
                model::record(Event::Admitted {
                    session: self.session,
                    file: self.file,
                    login: file.login,
                    facts: model::facts_step(),
                });
                self.admitted = Some(file);
                true
            }
            Err(_) => false,
        }
    }

    /// An admitted read's turn at its table (`Seat::table`): the first page
    /// of a stream of several, or the next page of the one it pages.
    fn read(&mut self) -> bool {
        let Some(login) = self.admitted.as_ref().map(|file| file.login) else {
            return false;
        };
        let owner = Owner::of(&self.header());
        let Some(joined) = self.connection.joined.get_mut(&self.session) else {
            return false;
        };
        let table = Arc::downgrade(joined.seat.transfers());
        let Ok(mut transfers) = joined.seat.table(self.file, login) else {
            return false;
        };
        let handles = &mut self.handles;
        let resume = match self.resume.take() {
            None => Some(
                transfers
                    .start(owner, stream(), PAGE, || {
                        *handles += 1;
                        *handles
                    })
                    .1,
            ),
            Some(resume) => transfers
                .next(owner, &resume, PAGE)
                .ok()
                .flatten()
                .map(|(_, resume)| resume),
        };
        drop(transfers);
        let Some(resume) = resume.filter(Resume::has_more) else {
            return false;
        };
        model::record(Event::Kept {
            session: self.session,
            file: self.file,
            login,
            table,
            cursor: resume.cursor,
        });
        self.resume = Some(resume);
        true
    }
}

/// A slot stream of several pages.
fn stream() -> Vec<u8> {
    OwnedNode::list(
        [1],
        (0..40u8)
            .map(|n| OwnedNode::leaf([n], vec![7; 50]))
            .collect(),
    )
    .to_wire()
    .unwrap()
}

/// Somewhere for an actor to leave what it is done with: dropped once
/// every actor is done, outside the orders, so that a connection's own end
/// adds no steps.
fn parking() -> (Arc<Mutex<Vec<Kept>>>, Kept) {
    let parked: Arc<Mutex<Vec<Kept>>> = Arc::default();
    let kept = Box::new(Arc::clone(&parked));
    (parked, kept)
}

fn park(parked: &Mutex<Vec<Kept>>, what: impl Any + Send) {
    parked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Box::new(what));
}

fn generous() -> JoinRefusals {
    JoinRefusals {
        per_address: u32::MAX,
        refill: Duration::from_secs(1),
        per_connection: u32::MAX,
    }
}

fn address(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn report(scenario: &str, explored: &Explored) {
    eprintln!(
        "{scenario}: {} orders, the longest of {} steps",
        explored.orders, explored.steps
    );
}

/// A session ends (Disconnect, Connect on its transport, its transport
/// closing: `end_session`) while another transport joins it and starts
/// a paged read.
#[test]
fn every_order_of_a_sessions_end_and_a_joined_read() {
    let lab = Lab::new("orders-end", generous());
    let here = address("127.0.0.1");
    let explored = model::every_order(
        &[
            Section::Grants,
            Section::Grant,
            Section::Table,
            Section::Release,
        ],
        None,
        10_000,
        || {
            let (mut owner, id) = lab.owner(here, 5000);
            let mut joiner = lab.joiner(here, 5001, true, id);
            let (parked, kept) = parking();
            let parked_too = Arc::clone(&parked);
            Run {
                actors: vec![
                    (
                        "owner",
                        Box::new(move || {
                            model::record(Event::Began(Removal::Session(id)));
                            owner.end_session();
                            model::record(Event::Finished(Removal::Session(id)));
                            park(&parked, owner);
                        }),
                    ),
                    (
                        "joined",
                        Box::new(move || {
                            if joiner.admit() {
                                joiner.read();
                            }
                            park(&parked_too, joiner);
                        }),
                    ),
                ],
                keep: vec![kept],
            }
        },
    );
    report("end and a joined read", &explored);
}

/// The session logs out of the file and in again (`clear_file_session`,
/// the password check, the new Login published) while another transport
/// pages a download of the file through it.
#[test]
fn every_order_of_a_login_again_and_a_paged_joined_read() {
    let lab = Lab::new("orders-login", generous());
    let here = address("127.0.0.1");
    let explored = model::every_order(
        &[
            Section::Grant,
            Section::Table,
            Section::Release,
            Section::Login,
        ],
        None,
        20_000,
        || {
            let (mut owner, id) = lab.owner(here, 5000);
            let mut joiner = lab.joiner(here, 5001, true, id);
            let (shared, file, account) = (Arc::clone(&lab.shared), lab.hosted.id, lab.account());
            let (parked, kept) = parking();
            let parked_too = Arc::clone(&parked);
            Run {
                actors: vec![
                    (
                        "owner",
                        Box::new(move || {
                            let session = owner.session.as_mut().unwrap();
                            let login = session.open[&file].login;
                            let old = Removal::File {
                                session: id,
                                file,
                                login,
                            };
                            model::record(Event::Began(old));
                            clear_file_session(&shared, session, file);
                            model::record(Event::Finished(old));
                            model::point(Section::Login);
                            logged_in(session.open.get_mut(&file).unwrap(), account);
                            session.publish();
                            park(&parked, owner);
                        }),
                    ),
                    (
                        "joined",
                        Box::new(move || {
                            let _ =
                                joiner.admit() && joiner.read() && joiner.admit() && joiner.read();
                            park(&parked_too, joiner);
                        }),
                    ),
                ],
                keep: vec![kept],
            }
        },
    );
    report("login again and a paged joined read", &explored);
}

/// The administrator disconnects a session (`Shared::disconnect_session`)
/// while its own transport is busy elsewhere and another transport joins
/// it and starts a paged read.
#[test]
fn every_order_of_an_administrators_disconnect_and_a_joined_read() {
    let lab = Lab::new("orders-admin", generous());
    let here = address("127.0.0.1");
    let explored = model::every_order(
        &[Section::Grants, Section::Grant, Section::Table],
        None,
        10_000,
        || {
            let (owner, id) = lab.owner(here, 5000);
            let mut joiner = lab.joiner(here, 5001, true, id);
            let shared = Arc::clone(&lab.shared);
            let (parked, kept) = parking();
            Run {
                actors: vec![
                    (
                        "administrator",
                        Box::new(move || {
                            model::record(Event::Began(Removal::Session(id)));
                            shared.disconnect_session(id);
                            model::record(Event::Finished(Removal::Session(id)));
                        }),
                    ),
                    (
                        "joined",
                        Box::new(move || {
                            if joiner.admit() {
                                joiner.read();
                            }
                            park(&parked, joiner);
                        }),
                    ),
                ],
                keep: vec![kept, Box::new(owner)],
            }
        },
    );
    report("an administrator's disconnect and a joined read", &explored);
}

/// The server closes every connection (`Shared::close_connections`) while
/// a transport that joined session A pages a read of it, and another is
/// being admitted to session B.
#[test]
fn every_order_of_a_bulk_close_and_the_reads_of_two_sessions() {
    let lab = Lab::new("orders-close", generous());
    let here = address("127.0.0.1");
    let explored = model::every_order(
        &[Section::Grants, Section::Grant, Section::Table],
        None,
        40_000,
        || {
            let (owner_a, a) = lab.owner(here, 5000);
            let (owner_b, b) = lab.owner(here, 5002);
            let mut reader = lab.joiner(here, 5001, true, a);
            assert!(reader.admit(), "admitted before the order begins");
            let mut joiner = lab.joiner(here, 5003, true, b);
            let shared = Arc::clone(&lab.shared);
            let (parked, kept) = parking();
            let parked_too = Arc::clone(&parked);
            Run {
                actors: vec![
                    (
                        "server",
                        Box::new(move || {
                            model::record(Event::Began(Removal::All));
                            shared.close_connections();
                            model::record(Event::Finished(Removal::All));
                        }),
                    ),
                    (
                        "reader of A",
                        Box::new(move || {
                            reader.read();
                            park(&parked, reader);
                        }),
                    ),
                    (
                        "joining B",
                        Box::new(move || {
                            joiner.admit();
                            park(&parked_too, joiner);
                        }),
                    ),
                ],
                keep: vec![kept, Box::new(owner_a), Box::new(owner_b)],
            }
        },
    );
    report("a bulk close and the reads of two sessions", &explored);
}

/// Limits of one refused look per address, refilled each second of the
/// model's clock.
fn one_a_second() -> JoinRefusals {
    JoinRefusals {
        per_address: 1,
        refill: Duration::from_secs(1),
        per_connection: u32::MAX,
    }
}

/// Two connections from one address guess at a session at once
/// (`Connection::join`, refused without a certificate).
#[test]
fn every_order_of_two_guesses_from_one_address() {
    let lab = Lab::new("orders-guesses", one_a_second());
    let here = address("192.0.2.7");
    let explored = model::every_order(
        &[Section::Addresses, Section::Address],
        Some(1),
        1_000,
        || {
            lab.shared.joins.reset_allowances(0);
            let mut first = lab.joiner(here, 5001, false, 7);
            let mut second = lab.joiner(here, 5002, false, 7);
            Run {
                actors: vec![
                    (
                        "guess",
                        Box::new(move || {
                            first.admit();
                        }),
                    ),
                    (
                        "guess",
                        Box::new(move || {
                            second.admit();
                        }),
                    ),
                ],
                keep: Vec::new(),
            }
        },
    );
    report("two guesses from one address", &explored);
}

/// The session's client is admitted while a connection from its address
/// guesses twice, across a second of the model's clock.
#[test]
fn every_order_of_an_admitted_join_and_guesses_across_a_refill() {
    let lab = Lab::new("orders-refill", one_a_second());
    let here = address("192.0.2.7");
    let explored = model::every_order(
        &[Section::Addresses, Section::Address],
        Some(1),
        5_000,
        || {
            lab.shared.joins.reset_allowances(0);
            let (owner, id) = lab.owner(here, 5000);
            let mut client = lab.joiner(here, 5001, true, id);
            let mut guesser = lab.joiner(here, 5002, false, id);
            let (parked, kept) = parking();
            Run {
                actors: vec![
                    (
                        "client",
                        Box::new(move || {
                            client.admit();
                            park(&parked, client);
                        }),
                    ),
                    (
                        "guesses",
                        Box::new(move || {
                            guesser.admit();
                            guesser.admit();
                        }),
                    ),
                    ("clock", Box::new(model::tick)),
                ],
                keep: vec![kept, Box::new(owner)],
            }
        },
    );
    report("an admitted join and guesses across a refill", &explored);
}

/// Connections from two addresses guess at a table with room for one
/// address, the first address twice.
#[test]
fn every_order_of_guesses_at_a_full_table() {
    let lab = Lab::new("orders-table", one_a_second());
    let (a, b) = (address("192.0.2.7"), address("192.0.2.8"));
    let explored = model::every_order(
        &[Section::Addresses, Section::Address],
        Some(1),
        5_000,
        || {
            lab.shared.joins.reset_allowances(1);
            let mut first = lab.joiner(a, 5001, false, 7);
            let mut other = lab.joiner(b, 5002, false, 7);
            let mut again = lab.joiner(a, 5003, false, 7);
            Run {
                actors: vec![
                    (
                        "guess from A",
                        Box::new(move || {
                            first.admit();
                        }),
                    ),
                    (
                        "guess from B",
                        Box::new(move || {
                            other.admit();
                        }),
                    ),
                    (
                        "guess from A",
                        Box::new(move || {
                            again.admit();
                        }),
                    ),
                ],
                keep: Vec::new(),
            }
        },
    );
    report("guesses at a full table", &explored);
}
