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
//! How a session's authority goes, in order. The step that begins taking
//! it away first sets a flag the rule reads, an atomic set before anything
//! that can wait, the grant's own lock included ([`Grant::stop`],
//! [`Grant::revoke`], the owner's disconnect flag, [`Joins::stop_all`]);
//! the session's locks are released next; what the joined transports page
//! goes last ([`Grant::drain`]), as that waits for a read under way. A read
//! keeps a page only once it has checked the grant again under its
//! transfer table's lock ([`Seat::table`]), which the drain takes after
//! the flag. An address's allowance is looked at, the rule applied and a
//! refusal charged in one section ([`Joins::admit`]). How the locks nest is
//! in [`Section`]. Under test, every order of these steps among a few
//! actors runs against this code with three invariants checked after each
//! (`model`; the scenarios are in `connection::join_orders`).
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use log::info;
use om_protocol::Opcode;

use crate::admin::Control;
use crate::config::JoinRefusals;
use crate::hosting::{FileAccount, Hosted};
use crate::transfer::Transfers;
use crate::views::SessionWithheld;

#[cfg(test)]
pub(crate) mod model;

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
    files: HashMap<u32, FileGrant>,
    /// The transfer tables of the transports that joined ([`Seat`]), at
    /// most [`MAX_JOINED`]: what they page for the session, let go when it
    /// loses a file or ends, not at their next request. None is added once
    /// the grant has ended.
    joined: Vec<Arc<Mutex<Transfers>>>,
}

/// What a session lets joined transports read. It loses an authority in
/// the step that begins taking it away, before anything that can wait, the
/// grant's own lock included: a file's account when the owning transport
/// takes it away ([`Grant::revoke`]), everything when the session ends
/// ([`Grant::stop`], the first thing `Connection::end_session` does), when
/// the administrator disconnects it (the owner's disconnect flag) and when
/// the server closes every connection ([`Joins::stop_all`]). Each is a flag
/// the rule reads, set without waiting for a read that holds the grant's
/// lock, so that no read is admitted once it is set, whatever the lock lets
/// in while a writer waits. What the joined transports page for it goes
/// after that, in a step that may wait for a read under way
/// ([`Grant::drain`]). What it gains, a Login that succeeded, it publishes
/// after the request ([`Grant::publish`]).
pub(crate) struct Grant {
    id: u32,
    /// Which grant opened this was, counting from the server's start.
    serial: u64,
    owner: Peer,
    /// The owning transport's address and port, for the log.
    owner_name: String,
    /// The owning transport's controls: once the administrator has told it
    /// to disconnect, the session's reads end, whatever its thread is doing.
    control: Arc<Control>,
    /// How many times the server had closed every connection when the
    /// grant was opened, and how many times it has now: one more ended it.
    epoch: u64,
    closes: Arc<AtomicU64>,
    /// The session ended ([`Grant::stop`]).
    stopped: AtomicBool,
    /// The Login whose account the owning transport is taking away from a
    /// file, or took away last ([`Grant::revoke`]; 0 for none). One is
    /// enough: the owner takes them away one at a time, and each is gone
    /// from `state` before the next.
    revoked: AtomicU64,
    state: RwLock<State>,
}

impl Grant {
    /// The state, also after a thread panicked holding it: each change
    /// replaces it whole, and a session's end must still end it.
    fn read(&self) -> RwLockReadGuard<'_, State> {
        read(Section::Grant, &self.state)
    }

    fn write(&self) -> RwLockWriteGuard<'_, State> {
        write(Section::Grant, &self.state)
    }

    /// The session ended, the administrator told its transport to
    /// disconnect, or the server closed every connection: no transport
    /// reads through it, and what one holds for it can go.
    pub fn ended(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
            || self.control.disconnecting()
            || self.closes.load(Ordering::Acquire) != self.epoch
    }

    /// The file `file_id` as the session has it open, logged in only while
    /// its Login is not being taken away.
    fn file(&self, state: &State, file_id: u32) -> Option<FileGrant> {
        let mut file = state.files.get(&file_id).cloned()?;
        if file.login == self.revoked.load(Ordering::Acquire) {
            file.account = None;
        }
        Some(file)
    }

    /// The files the session has open now, replacing what was published.
    pub fn publish(&self, files: impl IntoIterator<Item = (u32, FileGrant)>) {
        let files = files.into_iter().collect();
        let mut state = self.write();
        if !self.stopped.load(Ordering::Acquire) {
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
        #[cfg(test)]
        model::saw_facts();
        let session = Session {
            owner: self.owner,
            ended: self.ended(),
        };
        (session, self.file(&state, file_id))
    }

    /// Whether the session still grants reads of file `file_id` under Login
    /// `login`: it has not ended, and the account it has the file logged in
    /// with is still that Login's. A read checks it under its transfer
    /// table's lock, before it keeps anything there ([`Seat::table`]).
    fn check(&self, file_id: u32, login: u64) -> Result<(), Refusal> {
        let state = self.read();
        if self.ended() {
            return Err(Refusal::Ended);
        }
        match self.file(&state, file_id) {
            Some(file) if file.account.is_some() && file.login == login => Ok(()),
            Some(_) => Err(Refusal::NotLoggedIn),
            None => Err(Refusal::FileNotOpen),
        }
    }

    /// The session's account of file `file_id`, that of Login `login`, is
    /// going (Logout, a new Login, the file opened again or closed): from
    /// now on no transport reads the file through the session until a
    /// Login publishes one, and what joined transports page of it goes.
    /// Called before the session itself lets go of the account, so that
    /// the grant never holds it longer. The flag comes first, before the
    /// grant's lock; each table is let go of after it, under the table's
    /// own lock, so that a read either keeps a page before (and it goes
    /// here) or checks the flag after (and keeps nothing).
    pub fn revoke(&self, file_id: u32, login: u64) {
        self.revoked.store(login, Ordering::Release);
        let tables = {
            let mut state = self.write();
            if let Some(file) = state.files.get_mut(&file_id) {
                file.account = None;
            }
            state.joined.clone()
        };
        for table in tables {
            lock(Section::Table, &table).drop_file(file_id);
        }
    }

    /// The first step of the session's end, before anything that can wait:
    /// no transport reads through the grant from now on. A read already
    /// admitted finishes and keeps nothing ([`Seat::table`]); what is kept
    /// goes with [`Grant::drain`].
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.write().files.clear();
    }

    /// Let go of what the joined transports page for the grant, once it has
    /// ended ([`Grant::stop`], the administrator's flag, a bulk close): the
    /// last step of its end, which waits for a read under way.
    pub fn drain(&self) {
        let tables = self.read().joined.clone();
        for table in tables {
            lock(Section::Table, &table).clear();
        }
    }

    /// A place among the session's [`MAX_JOINED`] joined transports for a
    /// transport paging into `transfers`, kept until the [`Seat`] is
    /// dropped; refused when they are all taken or the grant ended. The
    /// flags are read under the grant's lock, which [`Grant::drain`] takes
    /// after them: a table added before the drain is drained, and none is
    /// added after.
    pub fn take_seat(self: &Arc<Self>, transfers: Arc<Mutex<Transfers>>) -> Result<Seat, Refusal> {
        let mut state = self.write();
        if self.ended() {
            return Err(Refusal::Ended);
        }
        if state.joined.len() >= MAX_JOINED {
            return Err(Refusal::Full);
        }
        state.joined.push(Arc::clone(&transfers));
        drop(state);
        Ok(Seat {
            grant: Arc::clone(self),
            transfers,
            logins: HashMap::new(),
        })
    }
}

/// The locks of joined reads, by what they guard. Each is let go before
/// any taken earlier: an address's allowance ([`Section::Address`]) is
/// held while the rule looks at [`Section::Grants`] and then at a
/// [`Section::Grant`] (`Joins::admit`), and a transfer table
/// ([`Section::Table`]) while its grant is checked ([`Seat::table`]).
/// Nothing else is taken under another: [`Section::Addresses`] and
/// [`Section::Grants`] are let go before anything else is taken, and a
/// [`Section::Grant`] before any table, so no two threads can wait on each
/// other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    /// The table of client addresses' allowances.
    Addresses,
    /// One address's allowance.
    Address,
    /// The grants, by certificate and session id.
    Grants,
    /// One session's grant.
    Grant,
    /// A joined transport's transfer table.
    Table,
    /// Not a lock: a session's locks released in the engine, a step of its
    /// end and of losing a file (`model`).
    #[cfg(test)]
    Release,
    /// Not a lock: a Login's password check (`model`).
    #[cfg(test)]
    Login,
}

/// `mutex`'s value, also after a thread panicked holding it: each user
/// replaces or removes whole entries. Under test, a step of the
/// `model`'s orders.
pub(crate) fn lock<T>(section: Section, mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[cfg(test)]
    if let Some(guard) = model::lock(section, mutex) {
        return guard;
    }
    let _ = section;
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(section: Section, lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    #[cfg(test)]
    if let Some(guard) = model::read(section, lock) {
        return guard;
    }
    let _ = section;
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(section: Section, lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    #[cfg(test)]
    if let Some(guard) = model::write(section, lock) {
        return guard;
    }
    let _ = section;
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Now, or under test the `model`'s clock.
fn now() -> Instant {
    #[cfg(test)]
    if let Some(now) = model::now() {
        return now;
    }
    Instant::now()
}

/// A joined transport's place in a session ([`Grant::take_seat`]), and the
/// table it pages the session's reads into.
pub(crate) struct Seat {
    grant: Arc<Grant>,
    transfers: Arc<Mutex<Transfers>>,
    /// Per file, the Login its transfers were started under.
    logins: HashMap<u32, u64>,
}

impl Seat {
    pub fn grant(&self) -> &Arc<Grant> {
        &self.grant
    }

    #[cfg(test)]
    pub fn transfers(&self) -> &Arc<Mutex<Transfers>> {
        &self.transfers
    }

    /// The table, locked, for a read of file `file_id` admitted under Login
    /// `login`, once the session still grants it ([`Grant::check`], under
    /// the table's lock: what the read keeps there is let go by a revoke or
    /// an end that comes after, and one that came before refuses it here).
    /// What was paged of the file under an earlier Login goes, as the
    /// session's own transfers go at Logout.
    pub fn table(
        &mut self,
        file_id: u32,
        login: u64,
    ) -> Result<MutexGuard<'_, Transfers>, Refusal> {
        let mut transfers = lock(Section::Table, &self.transfers);
        self.grant.check(file_id, login)?;
        if self.logins.insert(file_id, login) != Some(login) {
            transfers.drop_file(file_id);
        }
        Ok(transfers)
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.grant
            .write()
            .joined
            .retain(|held| !Arc::ptr_eq(held, &self.transfers));
    }
}

/// The sessions transports may join, by the certificate their own
/// transport presented and their connection id: every session a client's
/// Connect started with a certificate, from that Connect to its end. And
/// what refused joins have cost each client address.
#[derive(Default)]
pub(crate) struct Joins {
    grants: Mutex<HashMap<(CertificateHash, u32), Arc<Grant>>>,
    /// How many times the server has closed every connection
    /// ([`Joins::stop_all`]).
    closes: Arc<AtomicU64>,
    /// Grants opened so far.
    opened: AtomicU64,
    limits: RwLock<JoinRefusals>,
    allowances: Mutex<HashMap<IpAddr, Arc<Mutex<Allowance>>>>,
    refusal_log: Mutex<LogAllowance>,
    served_log: Mutex<LogAllowance>,
    /// Under test, fewer addresses than [`MAX_ADDRESSES`] (0: that many).
    #[cfg(test)]
    capacity: std::sync::atomic::AtomicUsize,
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
        lock(Section::Grants, &self.grants)
    }

    /// What refused joins cost ([`JoinRefusals`]).
    pub fn set_limits(&self, limits: JoinRefusals) {
        *self.limits.write().unwrap_or_else(PoisonError::into_inner) = limits;
    }

    pub fn limits(&self) -> JoinRefusals {
        *self.limits.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start the grant of the session `id` whose own transport is `owner`,
    /// told what the administrator asks by `control`. A session whose
    /// transport presented no certificate can be joined by none: its grant
    /// is not listed.
    pub fn open(
        &self,
        id: u32,
        owner: Peer,
        owner_name: &str,
        control: Arc<Control>,
    ) -> Arc<Grant> {
        let grant = Arc::new(Grant {
            id,
            serial: self.opened.fetch_add(1, Ordering::Relaxed),
            owner,
            owner_name: owner_name.to_owned(),
            control,
            epoch: self.closes.load(Ordering::Acquire),
            closes: Arc::clone(&self.closes),
            stopped: AtomicBool::new(false),
            revoked: AtomicU64::new(0),
            state: RwLock::new(State {
                files: HashMap::new(),
                joined: Vec::new(),
            }),
        });
        if let Some(certificate) = owner.certificate {
            self.grants().insert((certificate, id), Arc::clone(&grant));
        }
        grant
    }

    /// The last step of a session's end: `grant` is no longer listed, and
    /// what joined transports page for it goes, which waits for a read
    /// under way. The session stopped it first of all
    /// (`Connection::end_session`); a grant still live is stopped here.
    /// Never panics (a session's end runs while a panic unwinds).
    pub fn close(&self, grant: &Grant) {
        grant.stop();
        if let Some(certificate) = grant.owner.certificate {
            let key = (certificate, grant.id);
            let mut grants = self.grants();
            if grants
                .get(&key)
                .is_some_and(|live| std::ptr::eq(Arc::as_ptr(live), grant))
            {
                grants.remove(&key);
            }
        }
        grant.drain();
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

    /// Look at a request from `address` (`admission::client_key`) naming a
    /// session, by `look` (the lookup and the rule), within the address's
    /// allowance: in one section under the address's own lock, the
    /// allowance is refilled and looked at, `look` runs, and a refusal is
    /// charged. However many connections from the address try at once,
    /// they get no more refused looks than the allowance and what refilled
    /// since; an admitted one costs nothing. With nothing left the request
    /// is refused unexamined ([`Refusal::Allowance`]), as it is when the
    /// table has no room for a new address once the entries no request
    /// holds and whose allowance is whole again are forgotten.
    pub fn admit<T>(
        &self,
        address: IpAddr,
        look: impl FnOnce() -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let limits = self.limits();
        let Some(entry) = self.allowance(address, limits) else {
            return Err(Refusal::Allowance);
        };
        let mut allowance = lock(Section::Address, &entry);
        let now = now();
        *allowance = allowance.refilled(limits, now);
        if allowance.left == 0 {
            return Err(Refusal::Allowance);
        }
        let looked = look();
        if looked.is_err() {
            allowance.left -= 1;
        }
        #[cfg(test)]
        model::looked(address, now, looked.is_err());
        looked
    }

    /// `address`'s entry in the table of allowances, added whole if it has
    /// none; `None` when the table has no room for it. Making room forgets
    /// only entries no request holds (`Arc::get_mut`, exact under the
    /// table's lock, through which alone they are handed out).
    fn allowance(&self, address: IpAddr, limits: JoinRefusals) -> Option<Arc<Mutex<Allowance>>> {
        let now = now();
        let capacity = self.capacity();
        let mut allowances = lock(Section::Addresses, &self.allowances);
        if !allowances.contains_key(&address) && allowances.len() >= capacity {
            allowances.retain(|_, entry| {
                Arc::get_mut(entry).is_none_or(|entry| {
                    let allowance = entry.get_mut().unwrap_or_else(PoisonError::into_inner);
                    allowance.refilled(limits, now).left < limits.per_address
                })
            });
            if allowances.len() >= capacity {
                return None;
            }
        }
        let entry = allowances.entry(address).or_insert_with(|| {
            Arc::new(Mutex::new(Allowance {
                left: limits.per_address,
                earned: now,
            }))
        });
        Some(Arc::clone(entry))
    }

    fn capacity(&self) -> usize {
        #[cfg(test)]
        match self.capacity.load(Ordering::Relaxed) {
            0 => {}
            capacity => return capacity,
        }
        MAX_ADDRESSES
    }

    /// Under test: room for `capacity` addresses, and none counted.
    #[cfg(test)]
    pub fn reset_allowances(&self, capacity: usize) {
        self.capacity.store(capacity, Ordering::Relaxed);
        lock(Section::Addresses, &self.allowances).clear();
    }

    /// End at once the reads of session `id`, whatever its own transport is
    /// doing: the administrator disconnected it
    /// (`Shared::disconnect_session`, after the owner's disconnect flag,
    /// which ended them already). Each grant is stopped before any is
    /// drained. The session's transport ends it and lets go of its locks
    /// when it gets there.
    pub fn end(&self, id: u32) {
        let grants: Vec<Arc<Grant>> = self
            .grants()
            .iter()
            .filter(|((_, grant_id), _)| *grant_id == id)
            .map(|(_, grant)| Arc::clone(grant))
            .collect();
        for grant in &grants {
            grant.stop();
        }
        for grant in &grants {
            grant.drain();
        }
    }

    /// End at once the reads of every session, in one step: the server
    /// closes every connection (`Shared::close_connections`). Returns the
    /// grants, in the order they were opened, to [`Grant::drain`] once the
    /// sockets are shut. A grant opened after this is not ended by it.
    pub fn stop_all(&self) -> Vec<Arc<Grant>> {
        self.closes.fetch_add(1, Ordering::AcqRel);
        let mut grants: Vec<Arc<Grant>> = self.grants().values().cloned().collect();
        grants.sort_by_key(|grant| grant.serial);
        grants
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

    fn transfers() -> Arc<Mutex<Transfers>> {
        Arc::new(Mutex::new(Transfers::new(
            &crate::uploads::UploadMemory::new(usize::MAX),
            &crate::transfer::DownloadMemory::new(usize::MAX),
            "192.0.2.7".parse().unwrap(),
        )))
    }

    /// A session's joined transports are capped, a seat given back is
    /// free again, a grant is found only with its owner's certificate, and
    /// a closed grant can be joined by no one; the administrator's
    /// disconnect of the owning transport ends it at once, and so does a
    /// bulk close, for the grants opened before it alone.
    #[test]
    fn seats_are_capped_and_a_closed_grant_is_gone() {
        let joins = Joins::default();
        let owner = peer("192.0.2.7", Some(1));
        let control = Arc::new(Control::default());
        let grant = joins.open(7, owner, "192.0.2.7:5555", Arc::clone(&control));
        let seats: Vec<Seat> = (0..MAX_JOINED)
            .map(|_| grant.take_seat(transfers()).unwrap())
            .collect();
        assert_eq!(grant.take_seat(transfers()).err(), Some(Refusal::Full));
        drop(seats);
        assert!(grant.take_seat(transfers()).is_ok());

        let lookup = joins.lookup(Some([1; 32]), 7, 3);
        assert!(lookup.grant.is_some() && !lookup.session.unwrap().ended);
        // Another certificate finds nothing, as for an id no session has.
        assert!(joins.lookup(Some([2; 32]), 7, 3).grant.is_none());
        assert!(joins.lookup(None, 7, 3).grant.is_none());
        control.disconnect_now();
        assert!(joins.lookup(Some([1; 32]), 7, 3).session.unwrap().ended);
        assert_eq!(grant.take_seat(transfers()).err(), Some(Refusal::Ended));
        joins.close(&grant);
        let lookup = joins.lookup(Some([1; 32]), 7, 3);
        assert!(lookup.grant.is_none() && lookup.session.is_none());
        assert!(grant.ended());

        let before = joins.open(8, owner, "before", Arc::new(Control::default()));
        assert_eq!(joins.stop_all().len(), 1);
        let after = joins.open(9, owner, "after", Arc::new(Control::default()));
        assert!(before.ended() && !after.ended());
    }

    /// An address's refused looks are counted: when they are spent it may
    /// be looked at no more until it earns one back, an admitted look costs
    /// nothing, and another address is not touched.
    #[test]
    fn an_address_that_spent_its_attempts_waits_for_one_more() {
        let joins = Joins::default();
        joins.set_limits(JoinRefusals {
            per_address: 2,
            refill: Duration::from_millis(200),
            per_connection: 16,
        });
        let (here, there): (IpAddr, IpAddr) =
            ("192.0.2.7".parse().unwrap(), "192.0.2.8".parse().unwrap());
        let admitted = |address| joins.admit(address, || Ok(()));
        let refused = |address| joins.admit(address, || Err::<(), _>(Refusal::NoSession));
        assert_eq!(admitted(here), Ok(()));
        assert_eq!(refused(here), Err(Refusal::NoSession));
        assert_eq!(refused(here), Err(Refusal::NoSession));
        assert_eq!(refused(here), Err(Refusal::Allowance));
        assert_eq!(admitted(here), Err(Refusal::Allowance), "not looked at");
        assert_eq!(refused(there), Err(Refusal::NoSession));
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(refused(here), Err(Refusal::NoSession));
        assert_eq!(refused(here), Err(Refusal::Allowance));
    }

    /// What a joined transport pages for a session goes when the session
    /// loses the file and when it ends, not at the transport's next
    /// request: the server's download budget gets it back at once.
    #[test]
    fn joined_transfers_go_when_the_session_loses_the_file() {
        let downloads = crate::transfer::DownloadMemory::new(usize::MAX);
        let table = Arc::new(Mutex::new(Transfers::new(
            &crate::uploads::UploadMemory::new(usize::MAX),
            &downloads,
            "192.0.2.7".parse().unwrap(),
        )));
        let joins = Joins::default();
        let grant = joins.open(
            7,
            peer("192.0.2.7", Some(1)),
            "owner",
            Arc::new(Control::default()),
        );
        let _seat = grant.take_seat(Arc::clone(&table)).unwrap();
        let stream = om_hbam::OwnedNode::list(
            [1],
            (0..100u8)
                .map(|n| om_hbam::OwnedNode::leaf([n], vec![7; 50]))
                .collect(),
        )
        .to_wire()
        .unwrap();
        let mut handles = 0;
        let mut page = |file_id: u32| {
            let mut header = om_rctransaction::Header::new(Opcode::DownloadWithLock as u8);
            header.file_id = file_id;
            let owner = crate::transfer::Owner::of(&header);
            let (_, resume) =
                lock(Section::Table, &table).start(owner, stream.clone(), 512, || {
                    handles += 1;
                    handles
                });
            assert!(resume.has_more(), "held for its next page");
        };
        page(3);
        page(4);
        let one = downloads.in_use() / 2;
        grant.revoke(3, 0);
        assert_eq!(
            downloads.in_use(),
            one,
            "file 3's page went, file 4's stayed"
        );
        grant.stop();
        grant.drain();
        assert_eq!(downloads.in_use(), 0);
    }

    /// A full table takes a new address once it has forgotten the entries
    /// whose allowance refilled.
    #[test]
    fn a_full_table_reclaims_refilled_addresses_before_refusing_one() {
        let joins = Joins::default();
        joins.reset_allowances(4);
        let limits = |refill| JoinRefusals {
            per_address: 1,
            refill,
            per_connection: 16,
        };
        joins.set_limits(limits(Duration::from_secs(3600)));
        let refused = |address| joins.admit(address, || Err::<(), _>(Refusal::NoSession));
        for n in 0..4u32 {
            assert_eq!(
                refused(IpAddr::from(n.to_be_bytes())),
                Err(Refusal::NoSession)
            );
        }
        let new: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(
            refused(new),
            Err(Refusal::Allowance),
            "a full table of spent addresses"
        );
        // Every entry earns its attempt back.
        joins.set_limits(limits(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            refused(new),
            Err(Refusal::NoSession),
            "refilled entries forgotten first"
        );
    }
}
