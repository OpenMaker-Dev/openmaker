//! Reads a session's client sends on a transport that is not the session's
//! own ([#968](https://github.com/OpenMaker-Dev/openmaker/issues/968)).
//!
//! FileMaker Pro opens another TLS connection when it makes a call while its
//! first one is busy, and sends the call there with no Connect of its own,
//! naming its session's connection id; FileMaker Server answers it from that
//! session. The calls seen there were Download (op90) and Download List
//! (op92) (docs/re/network-protocol.md 3).
//!
//! OpenMaker Server serves such a transport those two reads and nothing
//! else, and only when it passes [`decide`]: the same client certificate as
//! the session's own transport (whose private key its handshake proved it
//! holds), the same address, a live session logged in to the file. The
//! transport that sent Connect owns the session and keeps it as before; it
//! publishes a [`Grant`] of what its files are and who is logged in to them,
//! which a joined transport reads at each request and never changes. A
//! request the rule refuses is answered `BAD_PARAM`, as a Perform naming an
//! unknown connection always was.
//!
//! What the rule rests on depends on the certificate. If each copy of a
//! client has a certificate of its own, joining another session needs that
//! copy's private key. If a certificate is shared (one every copy of a
//! product carries, or one a site gives all its users), it proves nothing
//! between those who share it, and what is left is the session's 32-bit id,
//! the address, and the cost of guessing: each refusal counts against the
//! transport and its address ([`crate::config::JoinRefusals`]), so a
//! neighbour at the same address gets a few dozen guesses and then about one
//! every 10 s. Which kind of certificate FileMaker Pro presents is not
//! measured yet.
//!
//! What a refusal reveals by its timing: the reply's bytes are the same for
//! every refusal, and so is what it costs the transport and its address. A
//! grant is found by (certificate, id), so a transport without the owner's
//! certificate misses whether the id is live or not and does the same work.
//! A transport with the owner's certificate (the owner's client, or one
//! sharing its certificate) finds a live id's grant and reads its state,
//! tens of nanoseconds more than a miss: too little to see within the
//! allowance's few dozen samples, not equal by construction.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use log::info;
use om_protocol::Opcode;

use crate::config::JoinRefusals;
use crate::hosting::{FileAccount, Hosted};
use crate::views::SessionWithheld;

/// The operations a joined transport may send: those FileMaker Pro was
/// seen sending on its second connection. Everything else is refused as
/// before.
pub(crate) const READS: [u8; 2] = [Opcode::DownloadWithLock as u8, Opcode::DownloadList as u8];

/// The transports one session may have joined at once: FileMaker Pro opens
/// at most 10 connections to one server (`maxGIOPConnectionPerServer`,
/// network-protocol.md 2), one of them the session's own.
pub(crate) const MAX_JOINED: usize = 9;

/// The sessions one transport may have joined at once. FileMaker Pro keeps
/// one session per server (network-protocol.md 8), and its other
/// connections named one live session at a time in the #968 capture, a
/// later one only once the earlier had ended; 4 leaves room for a session
/// it starts while another ends.
pub(crate) const MAX_SESSIONS_PER_TRANSPORT: usize = 4;

/// Lines of each kind ([`Joins::refused`], [`Joins::served`]) logged at
/// once, and how often one more is earned; those left out are counted into
/// the next line, so that a client cannot fill the disk.
const LOG_BURST: u32 = 64;
const LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Client addresses whose refusals are counted at once. Past it, those
/// whose allowance is whole again are forgotten; an address that still
/// finds no room has its joins refused, as one whose allowance is spent.
const MAX_ADDRESSES: usize = 65_536;

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
/// (`views::SessionWithheld`), which a joined read adds to as one of the
/// session's own does.
#[derive(Clone)]
pub(crate) struct FileGrant {
    pub hosted: Arc<Hosted>,
    pub account: Option<Arc<FileAccount>>,
    pub withheld: Arc<Mutex<SessionWithheld>>,
    /// Which Login the account is from ([`next_login`]): a joined
    /// transport drops what it pages of the file under an earlier one, as
    /// the session drops its own transfers at Logout, and its reads under
    /// an earlier one note no withheld records.
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

    /// The session ended: what a transport holds for it can go.
    pub fn ended(&self) -> bool {
        self.read().ended
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

/// The sessions transports may join, by the certificate their own
/// transport presented and their connection id: every session a client's
/// Connect started with a certificate, from that Connect to its end. And
/// what refused joins have cost each client address.
#[derive(Default)]
pub(crate) struct Joins {
    grants: Mutex<HashMap<(CertificateHash, u32), Arc<Grant>>>,
    limits: RwLock<JoinRefusals>,
    allowances: Mutex<HashMap<IpAddr, Allowance>>,
    refusal_log: Mutex<LogAllowance>,
    served_log: Mutex<LogAllowance>,
}

/// The refusals an address may still meet, and when it last earned one.
#[derive(Clone, Copy)]
struct Allowance {
    left: u32,
    earned: Instant,
}

impl Allowance {
    /// Earned back what `limits` gives since `earned`, up to the whole.
    fn refilled(self, limits: JoinRefusals, now: Instant) -> Allowance {
        if limits.refill.is_zero() {
            return Allowance {
                left: limits.per_address,
                earned: now,
            };
        }
        let elapsed = now.saturating_duration_since(self.earned);
        let intervals = elapsed.as_nanos() / limits.refill.as_nanos();
        if intervals == 0 {
            return self;
        }
        let left = u32::try_from(intervals)
            .unwrap_or(u32::MAX)
            .saturating_add(self.left)
            .min(limits.per_address);
        Allowance {
            left,
            earned: if left == limits.per_address {
                now
            } else {
                // Partial progress toward the next one is kept.
                self.earned + limits.refill * u32::try_from(intervals).unwrap_or(u32::MAX)
            },
        }
    }
}

/// Lines of one kind the log may still write ([`LOG_BURST`], one more
/// every [`LOG_INTERVAL`]), and what was left out since the last.
#[derive(Default)]
struct LogAllowance {
    lines: u32,
    earned: Option<Instant>,
    left_out: BTreeMap<&'static str, u64>,
}

impl LogAllowance {
    /// Whether a line may be written now, with what was left out since the
    /// last; otherwise `kind` is counted.
    fn take(&mut self, kind: &'static str) -> Option<String> {
        let now = Instant::now();
        match self.earned {
            None => {
                self.lines = LOG_BURST;
                self.earned = Some(now);
            }
            Some(earned) => {
                let intervals = now.duration_since(earned).as_secs() / LOG_INTERVAL.as_secs();
                if intervals > 0 {
                    let earned_now = u32::try_from(intervals).unwrap_or(u32::MAX);
                    self.lines = self.lines.saturating_add(earned_now).min(LOG_BURST);
                    self.earned = Some(now);
                }
            }
        }
        if self.lines == 0 {
            *self.left_out.entry(kind).or_default() += 1;
            return None;
        }
        self.lines -= 1;
        let left_out = std::mem::take(&mut self.left_out);
        Some(if left_out.is_empty() {
            String::new()
        } else {
            let counts: Vec<String> = left_out
                .iter()
                .map(|(kind, n)| format!("{kind} {n}"))
                .collect();
            format!(
                " (since the last such line, not logged: {})",
                counts.join(", ")
            )
        })
    }
}

impl Joins {
    fn grants(&self) -> MutexGuard<'_, HashMap<(CertificateHash, u32), Arc<Grant>>> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn allowances(&self) -> MutexGuard<'_, HashMap<IpAddr, Allowance>> {
        self.allowances
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// What refused joins cost ([`JoinRefusals`]).
    pub fn set_limits(&self, limits: JoinRefusals) {
        *self.limits.write().unwrap_or_else(PoisonError::into_inner) = limits;
    }

    pub fn limits(&self) -> JoinRefusals {
        *self.limits.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the grant of the session `id` whose own transport is `owner`.
    /// A session whose transport presented no certificate can be joined by
    /// none: its grant is not listed.
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
        if let Some(certificate) = owner.certificate {
            self.grants().insert((certificate, id), Arc::clone(&grant));
        }
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
        let Some(certificate) = grant.owner.certificate else {
            return;
        };
        let key = (certificate, grant.id);
        let mut grants = self.grants();
        if grants
            .get(&key)
            .is_some_and(|live| std::ptr::eq(Arc::as_ptr(live), grant))
        {
            grants.remove(&key);
        }
    }

    /// What the rule needs to decide on a request naming session `id` for
    /// file `file_id`, from a transport that presented `certificate`: the
    /// session's grant, its facts and the file as it has it open, each
    /// `None` when there is none. Only a session whose own transport
    /// presented the same certificate is found. Each lock is held only
    /// while it is read.
    pub fn lookup(&self, certificate: Option<CertificateHash>, id: u32, file_id: u32) -> Lookup {
        let grant =
            certificate.and_then(|certificate| self.grants().get(&(certificate, id)).cloned());
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

    /// Whether `address` (`admission::client_key`) may still meet a
    /// refusal: when not, a join from it is refused unexamined.
    pub fn may_try(&self, address: IpAddr) -> bool {
        let limits = self.limits();
        let mut allowances = self.allowances();
        match allowances.get_mut(&address) {
            Some(allowance) => {
                *allowance = allowance.refilled(limits, Instant::now());
                allowance.left > 0
            }
            None => limits.per_address > 0 && allowances.len() < MAX_ADDRESSES,
        }
    }

    /// Count one refusal against `address`.
    pub fn spend(&self, address: IpAddr) {
        let limits = self.limits();
        let now = Instant::now();
        let mut allowances = self.allowances();
        if !allowances.contains_key(&address) && allowances.len() >= MAX_ADDRESSES {
            allowances
                .retain(|_, allowance| allowance.refilled(limits, now).left < limits.per_address);
        }
        if !allowances.contains_key(&address) && allowances.len() >= MAX_ADDRESSES {
            return;
        }
        let allowance = allowances.entry(address).or_insert(Allowance {
            left: limits.per_address,
            earned: now,
        });
        let refilled = allowance.refilled(limits, now);
        *allowance = Allowance {
            left: refilled.left.saturating_sub(1),
            ..refilled
        };
    }

    /// Log one refused join, with the transport, the operation, the id it
    /// named and the condition that failed, within the log's allowance
    /// ([`LOG_BURST`]); the others are counted by condition into the next
    /// line.
    pub fn refused(&self, peer: &str, id: u32, opcode: u8, refusal: Refusal) {
        let mut log = self
            .refusal_log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(more) = log.take(refusal.kind()) else {
            return;
        };
        drop(log);
        info!("{peer}: refused op{opcode} naming session 0x{id:08x}: {refusal}{more}");
    }

    /// Log one read served on a joined transport, within the log's
    /// allowance; the others are counted into the next line.
    pub fn served(&self, peer: &str, line: std::fmt::Arguments<'_>) {
        let mut log = self
            .served_log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(more) = log.take("served") else {
            return;
        };
        drop(log);
        info!("{peer}: {line}{more}");
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
    /// The session the request names, `None` when no session has that id
    /// for the transport's certificate.
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
    /// No session has the id, for the transport's certificate.
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
    /// The transport has joined [`MAX_SESSIONS_PER_TRANSPORT`] sessions.
    TooManySessions,
    /// The transport's address has used up its allowance of refusals
    /// ([`JoinRefusals`]): the request was not looked at.
    Allowance,
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
            Refusal::TooManySessions => "too many sessions",
            Refusal::Allowance => "allowance spent",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NoSession => "no live session has that id for this client certificate",
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
            Refusal::TooManySessions => "the transport has joined its limit of sessions",
            Refusal::Allowance => {
                "the address has used up its allowance of refused reads; not looked at"
            }
        })
    }
}

/// The join rule: whether a transport that is not a session's own may have
/// a request answered from that session. It must present the client
/// certificate the session's own transport presented (the same, byte for
/// byte, whose private key its handshake proved it holds), come from the
/// same address, name a session that is live, and ask for one of the
/// [`READS`] of a file that session is logged in to. The connection id
/// alone proves nothing: it is 32 bits. What the rule rests on when a
/// certificate is shared is in the module's documentation.
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
    /// free again, a grant is found only with its owner's certificate, and
    /// a closed grant can be joined by no one.
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

        let lookup = joins.lookup(Some([1; 32]), 7, 3);
        assert!(lookup.grant.is_some() && !lookup.session.unwrap().ended);
        // Another certificate finds nothing, as for an id no session has.
        assert!(joins.lookup(Some([2; 32]), 7, 3).grant.is_none());
        assert!(joins.lookup(None, 7, 3).grant.is_none());
        joins.close(&grant);
        let lookup = joins.lookup(Some([1; 32]), 7, 3);
        assert!(lookup.grant.is_none() && lookup.session.is_none());
        assert!(grant.ended());
    }

    /// An address's refusals are counted: when they are spent it may try
    /// no more until it earns one back, and another address is not touched.
    #[test]
    fn an_address_that_spent_its_refusals_waits_for_one_more() {
        let joins = Joins::default();
        joins.set_limits(JoinRefusals {
            per_address: 2,
            refill: Duration::from_millis(200),
            per_connection: 16,
        });
        let (here, there): (IpAddr, IpAddr) =
            ("192.0.2.7".parse().unwrap(), "192.0.2.8".parse().unwrap());
        assert!(joins.may_try(here));
        joins.spend(here);
        assert!(joins.may_try(here));
        joins.spend(here);
        assert!(!joins.may_try(here));
        assert!(joins.may_try(there));
        std::thread::sleep(Duration::from_millis(250));
        assert!(joins.may_try(here));
        joins.spend(here);
        assert!(!joins.may_try(here));
    }
}
