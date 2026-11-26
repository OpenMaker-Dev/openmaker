//! Reads a session's client sends on a transport that is not the session's
//! own ([#968](https://github.com/OpenMaker-Dev/openmaker/issues/968)).
//!
//! FileMaker Pro was seen opening a second TLS connection during the start
//! of a file hosted read only, and sending Download (op90) and Download
//! List (op92) there with no Connect of its own; FileMaker Server answered
//! them (docs/re/network-protocol.md 3). By inference its network library
//! opens another connection when its others are busy with a call, and
//! FileMaker Server finds a session by its id whatever the connection.
//!
//! OpenMaker Server serves such a transport those two reads and nothing
//! else, and only when the transport proves it is the session's client
//! ([`decide`]). The transport that sent Connect owns the session and keeps
//! it as before; it publishes a [`Grant`]: what its files are and who is
//! logged in to them, which a joined transport reads at each request and
//! never changes. A request the rule refuses is answered `BAD_PARAM`, as a
//! Perform naming an unknown connection always was.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use log::info;
use om_protocol::Opcode;

use crate::hosting::{FileAccount, Hosted};
use crate::views::Withheld;

/// The operations a joined transport may send: those FileMaker Pro was
/// seen sending on its second connection. Everything else is refused as
/// before.
pub(crate) const READS: [u8; 2] = [Opcode::DownloadWithLock as u8, Opcode::DownloadList as u8];

/// The transports one session may have joined at once: FileMaker Pro opens
/// at most 10 connections to one server (`maxGIOPConnectionPerServer`,
/// network-protocol.md 2), one of them the session's own.
pub(crate) const MAX_JOINED: usize = 9;

/// Refused joins are logged one line each, up to [`REFUSAL_LOG_BURST`] at
/// once and then one every [`REFUSAL_LOG_INTERVAL`]; those left out are
/// counted into the next line, so that a guesser cannot fill the disk.
const REFUSAL_LOG_BURST: u32 = 64;
const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// A SHA-256 of the DER of a client certificate.
pub(crate) type CertificateHash = [u8; 32];

/// Who is at the other end of a transport: its client's address and the
/// SHA-256 of the client certificate it presented in its TLS handshake,
/// whose private key the handshake proved it holds (`tls::AnyClientCert`
/// checks the handshake signature against the certificate's key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Peer {
    /// The address, an IPv4-mapped IPv6 one as the IPv4 address it is.
    pub address: IpAddr,
    /// `None` for a transport whose certificate the server did not see.
    pub certificate: Option<CertificateHash>,
}

impl Peer {
    pub fn new(address: IpAddr, certificate: Option<CertificateHash>) -> Self {
        Self {
            address: address.to_canonical(),
            certificate,
        }
    }
}

/// A file a session has open, as a joined transport reads it: the same
/// file, the account the session is logged in to it with now, and the
/// session's own record of what reads withheld from that account
/// (`views::Withheld`), which a joined read adds to as one of the session's
/// own does.
#[derive(Clone)]
pub(crate) struct FileGrant {
    pub hosted: Arc<Hosted>,
    pub account: Option<Arc<FileAccount>>,
    pub withheld: Arc<Mutex<Withheld>>,
    /// Which Login the account is from ([`next_login`]): a joined
    /// transport drops what it pages of the file under an earlier one, as
    /// the session drops its own transfers at Logout.
    pub login: u64,
}

/// A number for one accepted Login, never 0 and never given twice.
pub(crate) fn next_login() -> u64 {
    static LOGINS: AtomicU64 = AtomicU64::new(1);
    LOGINS.fetch_add(1, Ordering::Relaxed)
}

struct State {
    ended: bool,
    files: HashMap<u32, FileGrant>,
}

/// What a session lets joined transports read, kept current by the
/// transport that owns it.
pub(crate) struct Grant {
    id: u32,
    owner: Peer,
    /// The owning transport's address and port, for the log.
    owner_name: String,
    state: RwLock<State>,
    seats: AtomicUsize,
}

impl Grant {
    /// The state, also after a thread panicked holding it: each change
    /// replaces it whole, and a session's end must still end it.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The files the session has open now, replacing what was published.
    pub fn publish(&self, files: impl IntoIterator<Item = (u32, FileGrant)>) {
        let files = files.into_iter().collect();
        let mut state = self.write();
        if !state.ended {
            state.files = files;
        }
    }

    pub fn owner_name(&self) -> &str {
        &self.owner_name
    }

    /// What [`decide`] needs of the session, and the file `file_id` as it
    /// has it open. The lock is let go before this returns.
    fn facts(&self, file_id: u32) -> (Session, Option<FileGrant>) {
        let state = self.read();
        let session = Session {
            owner: self.owner,
            ended: state.ended,
        };
        (session, state.files.get(&file_id).cloned())
    }

    /// A place among the session's [`MAX_JOINED`] joined transports, kept
    /// until the [`Seat`] is dropped; `None` when they are all taken.
    pub fn take_seat(self: &Arc<Self>) -> Option<Seat> {
        self.seats
            .try_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
                (taken < MAX_JOINED).then_some(taken + 1)
            })
            .ok()
            .map(|_| Seat(Arc::clone(self)))
    }
}

/// A joined transport's place in a session ([`Grant::take_seat`]).
pub(crate) struct Seat(Arc<Grant>);

impl Seat {
    pub fn grant(&self) -> &Arc<Grant> {
        &self.0
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.0.seats.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The sessions transports may join, by connection id: every session a
/// client's Connect started, from that Connect to the session's end.
#[derive(Default)]
pub(crate) struct Joins {
    grants: Mutex<HashMap<u32, Arc<Grant>>>,
    refusals: Mutex<RefusalLog>,
}

/// What the log of refused joins has left ([`Joins::refused`]).
#[derive(Default)]
struct RefusalLog {
    /// Lines it may write now, and when it last earned one.
    lines: u32,
    earned: Option<Instant>,
    /// The refusals of each kind not logged since the last line.
    left_out: BTreeMap<&'static str, u64>,
}

impl Joins {
    fn grants(&self) -> MutexGuard<'_, HashMap<u32, Arc<Grant>>> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the grant of the session `id` whose own transport is `owner`.
    pub fn open(&self, id: u32, owner: Peer, owner_name: &str) -> Arc<Grant> {
        let grant = Arc::new(Grant {
            id,
            owner,
            owner_name: owner_name.to_owned(),
            state: RwLock::new(State {
                ended: false,
                files: HashMap::new(),
            }),
            seats: AtomicUsize::new(0),
        });
        self.grants().insert(id, Arc::clone(&grant));
        grant
    }

    /// End `grant`: from now on no transport may read through it, and a
    /// read already under way finishes. Never panics (a session's end runs
    /// while a panic unwinds).
    pub fn close(&self, grant: &Grant) {
        {
            let mut state = grant.write();
            state.ended = true;
            state.files.clear();
        }
        let mut grants = self.grants();
        if grants
            .get(&grant.id)
            .is_some_and(|live| std::ptr::eq(Arc::as_ptr(live), grant))
        {
            grants.remove(&grant.id);
        }
    }

    /// What the rule needs to decide on a request naming session `id`
    /// for file `file_id`: the session's grant, its facts and the file as
    /// it has it open, each `None` when there is none. Each lock is held
    /// only while it is read.
    pub fn lookup(&self, id: u32, file_id: u32) -> Lookup {
        let grant = self.grants().get(&id).cloned();
        let (session, file) = match &grant {
            Some(grant) => {
                let (session, file) = grant.facts(file_id);
                (Some(session), file)
            }
            None => (None, None),
        };
        Lookup {
            grant,
            session,
            file,
        }
    }

    /// Log one refused join, with the transport, the operation, the id it
    /// named and the condition that failed: each one while the server has
    /// lines left ([`REFUSAL_LOG_BURST`], one more earned every
    /// [`REFUSAL_LOG_INTERVAL`]), the others counted by condition into the
    /// next line. One allowance for every kind, so that the time a refusal
    /// takes, logged or not, tells nothing of which condition failed, and
    /// so of whether a session has the id.
    pub fn refused(&self, peer: &str, id: u32, opcode: u8, refusal: Refusal) {
        let mut log = self.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        match log.earned {
            None => {
                log.lines = REFUSAL_LOG_BURST;
                log.earned = Some(now);
            }
            Some(earned) => {
                let intervals =
                    now.duration_since(earned).as_secs() / REFUSAL_LOG_INTERVAL.as_secs();
                if intervals > 0 {
                    let earned_now = u32::try_from(intervals).unwrap_or(u32::MAX);
                    log.lines = log.lines.saturating_add(earned_now).min(REFUSAL_LOG_BURST);
                    log.earned = Some(now);
                }
            }
        }
        if log.lines == 0 {
            *log.left_out.entry(refusal.kind()).or_default() += 1;
            return;
        }
        log.lines -= 1;
        let left_out = std::mem::take(&mut log.left_out);
        drop(log);
        let more = if left_out.is_empty() {
            String::new()
        } else {
            let counts: Vec<String> = left_out
                .iter()
                .map(|(kind, n)| format!("{kind} {n}"))
                .collect();
            format!(" (refused since the last such line: {})", counts.join(", "))
        };
        info!("{peer}: refused op{opcode} naming session 0x{id:08x}: {refusal}{more}");
    }
}

/// [`Joins::lookup`]'s answer.
pub(crate) struct Lookup {
    pub grant: Option<Arc<Grant>>,
    pub session: Option<Session>,
    pub file: Option<FileGrant>,
}

impl Lookup {
    /// The rule's inputs for a request of `opcode` on `transport`.
    pub fn inputs(&self, opcode: u8, transport: Peer) -> Inputs {
        Inputs {
            session: self.session,
            file: self.file.as_ref().map(|file| File {
                logged_in: file.account.is_some(),
            }),
            opcode,
            transport,
        }
    }
}

/// What the rule looks at of the session a request names.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Session {
    /// The transport that sent its Connect.
    pub owner: Peer,
    /// It ended (Disconnect, its transport closed, the administrator).
    pub ended: bool,
}

/// What the rule looks at of the file a request names, as the session has
/// it open.
#[derive(Clone, Copy, Debug)]
pub(crate) struct File {
    /// The session is logged in to it.
    pub logged_in: bool,
}

/// Everything the join rule looks at for one request on a transport that is
/// not the session's own.
pub(crate) struct Inputs {
    /// The session the request names, `None` when no session has that id.
    pub session: Option<Session>,
    /// The file the request names, as that session has it open (`None`:
    /// not open in it).
    pub file: Option<File>,
    /// The request's operation.
    pub opcode: u8,
    /// The transport the request came on.
    pub transport: Peer,
}

/// Why a request on a transport that is not the session's own is refused.
/// The client is answered the same for each (`BAD_PARAM`); only the log
/// says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No session has the id.
    NoSession,
    /// The session ended.
    Ended,
    /// The transport, or the session's own, presented no client certificate.
    NoCertificate,
    /// The transport presented another client certificate than the
    /// session's own transport did.
    Certificate,
    /// The transport comes from another address than the session's own.
    Address,
    /// The operation is not one a joined transport may send ([`READS`]).
    Operation,
    /// The session does not have the file open.
    FileNotOpen,
    /// The session has the file open and is not logged in to it.
    NotLoggedIn,
    /// The session's joined transports are at [`MAX_JOINED`].
    Full,
}

impl Refusal {
    fn kind(self) -> &'static str {
        match self {
            Refusal::NoSession => "no session",
            Refusal::Ended => "ended",
            Refusal::NoCertificate => "no certificate",
            Refusal::Certificate => "another certificate",
            Refusal::Address => "another address",
            Refusal::Operation => "operation",
            Refusal::FileNotOpen => "file not open",
            Refusal::NotLoggedIn => "not logged in",
            Refusal::Full => "full",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NoSession => "no live session has that id",
            Refusal::Ended => "the session ended",
            Refusal::NoCertificate => "no client certificate to compare",
            Refusal::Certificate => {
                "the transport presented another client certificate than the session's"
            }
            Refusal::Address => "the transport comes from another address than the session's",
            Refusal::Operation => "the operation is not a read a joined transport may send",
            Refusal::FileNotOpen => "the session does not have the file open",
            Refusal::NotLoggedIn => "the session is not logged in to the file",
            Refusal::Full => "the session has its limit of joined transports",
        })
    }
}

/// The join rule: whether a transport that is not a session's own may have
/// a request answered from that session. It must show what only the
/// session's client has, the private key of the client certificate the
/// session's own transport presented (the same certificate, byte for byte,
/// which its handshake proved it holds the key of), come from the same
/// address, name a session that is live, and ask for one of the [`READS`]
/// of a file that session is logged in to. The connection id alone proves
/// nothing: it is 32 bits.
///
/// Whether FileMaker Pro presents the same certificate on its second
/// connection, and whether that certificate is its own or one every copy
/// carries, is not measured yet (#968).
pub(crate) fn decide(inputs: &Inputs) -> Result<(), Refusal> {
    let session = inputs.session.ok_or(Refusal::NoSession)?;
    if session.ended {
        return Err(Refusal::Ended);
    }
    let (Some(theirs), Some(ours)) = (session.owner.certificate, inputs.transport.certificate)
    else {
        return Err(Refusal::NoCertificate);
    };
    if theirs != ours {
        return Err(Refusal::Certificate);
    }
    if session.owner.address != inputs.transport.address {
        return Err(Refusal::Address);
    }
    if !READS.contains(&inputs.opcode) {
        return Err(Refusal::Operation);
    }
    let file = inputs.file.ok_or(Refusal::FileNotOpen)?;
    if !file.logged_in {
        return Err(Refusal::NotLoggedIn);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(address: &str, certificate: Option<u8>) -> Peer {
        Peer::new(address.parse().unwrap(), certificate.map(|c| [c; 32]))
    }

    /// Each condition of the rule, against the inputs of a request that
    /// passes it.
    #[test]
    fn the_rule_admits_only_the_sessions_client_reading_a_file_it_logged_in_to() {
        let owner = peer("192.0.2.7", Some(1));
        let live = Session {
            owner,
            ended: false,
        };
        let logged_in = File { logged_in: true };
        let opened = File { logged_in: false };
        let read = |session, file, opcode, transport| {
            decide(&Inputs {
                session,
                file,
                opcode,
                transport,
            })
        };
        let op90 = Opcode::DownloadWithLock as u8;
        let op92 = Opcode::DownloadList as u8;

        assert_eq!(read(Some(live), Some(logged_in), op90, owner), Ok(()));
        assert_eq!(read(Some(live), Some(logged_in), op92, owner), Ok(()));
        // An IPv4 client of a dual-stack socket is the same address.
        let mapped = peer("::ffff:192.0.2.7", Some(1));
        assert_eq!(read(Some(live), Some(logged_in), op92, mapped), Ok(()));

        assert_eq!(
            read(None, None, op90, owner),
            Err(Refusal::NoSession),
            "an id no session has"
        );
        let ended = Session {
            ended: true,
            ..live
        };
        assert_eq!(
            read(Some(ended), Some(logged_in), op90, owner),
            Err(Refusal::Ended)
        );
        assert_eq!(
            read(
                Some(live),
                Some(logged_in),
                op90,
                peer("192.0.2.7", Some(2))
            ),
            Err(Refusal::Certificate),
            "the same address with another certificate"
        );
        assert_eq!(
            read(Some(live), Some(logged_in), op90, peer("192.0.2.7", None)),
            Err(Refusal::NoCertificate)
        );
        let uncertified = Session {
            owner: peer("192.0.2.7", None),
            ..live
        };
        assert_eq!(
            read(
                Some(uncertified),
                Some(logged_in),
                op90,
                peer("192.0.2.7", None)
            ),
            Err(Refusal::NoCertificate),
            "two transports without certificates are not the same client"
        );
        assert_eq!(
            read(
                Some(live),
                Some(logged_in),
                op90,
                peer("192.0.2.8", Some(1))
            ),
            Err(Refusal::Address),
            "the same certificate from another address"
        );
        assert_eq!(
            read(
                Some(live),
                Some(logged_in),
                op90,
                peer("2001:db8::7", Some(1))
            ),
            Err(Refusal::Address),
        );
        for write in [
            Opcode::Lock,
            Opcode::Login,
            Opcode::Logout,
            Opcode::Unlock,
            Opcode::HostCommit,
            Opcode::HostCreateRecord,
            Opcode::OpenFile,
            Opcode::QueryProgram,
            Opcode::DownloadFromHost,
            Opcode::PerformScriptOnServer,
        ] {
            assert_eq!(
                read(Some(live), Some(logged_in), write as u8, owner),
                Err(Refusal::Operation),
                "{write:?}"
            );
        }
        assert_eq!(
            read(Some(live), None, op90, owner),
            Err(Refusal::FileNotOpen)
        );
        assert_eq!(
            read(Some(live), Some(opened), op90, owner),
            Err(Refusal::NotLoggedIn)
        );
    }

    /// A session's joined transports are capped, a seat given back is
    /// free again, and a closed grant can be joined by no one.
    #[test]
    fn seats_are_capped_and_a_closed_grant_is_gone() {
        let joins = Joins::default();
        let owner = peer("192.0.2.7", Some(1));
        let grant = joins.open(7, owner, "192.0.2.7:5555");
        let seats: Vec<Seat> = (0..MAX_JOINED)
            .map(|_| grant.take_seat().unwrap())
            .collect();
        assert!(grant.take_seat().is_none());
        drop(seats);
        assert!(grant.take_seat().is_some());

        let lookup = joins.lookup(7, 3);
        assert!(lookup.grant.is_some() && !lookup.session.unwrap().ended);
        joins.close(&grant);
        let lookup = joins.lookup(7, 3);
        assert!(lookup.grant.is_none() && lookup.session.is_none());
        assert!(grant.facts(3).0.ended);
    }
}
