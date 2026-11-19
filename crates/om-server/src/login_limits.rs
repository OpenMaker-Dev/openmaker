//! How often Login (op80) may fail: OpenMaker Server counts failed logins
//! by client address and by account name, and answers a login past either
//! limit with FileMaker's error 214, "Too many login attempts", without
//! checking its password, so a client held back learns nothing from it.
//!
//! FileMaker Server locks accounts out (`RCLogin::LoginWithLockoutCheck`,
//! docs/re/network-protocol.md 5.4), but its thresholds, its lockout time
//! and the error a locked-out login gets are not measured. Everything here
//! is OpenMaker's choice:
//!
//! - **Per address.** An address may fail 5 logins at once, then one more
//!   a minute; its allowance is whole again after 5 minutes without a
//!   failure. An IPv4 address counts alone (also when mapped into IPv6),
//!   an IPv6 address by its /64 network, which one client usually holds
//!   whole. Clients behind one NAT share their address's allowance.
//! - **Per account**, an account name in one hosted file (each file has
//!   accounts of its own, whatever their names), from every address
//!   together, names matching in any case: 30 failures at once, then one every 30 s (whole again
//!   after 15 minutes). One address, or two, failing as fast as their own
//!   limit allows can never use it up (asserted below): it takes six
//!   addresses failing at once, or three failing for a quarter of an hour.
//!   It holds for every name, an account's or not, so it does not tell
//!   which names are accounts.
//! - **Known addresses.** An address that logged in as an account in the
//!   last 30 days is not held back by that account's limit, only by its
//!   own; logging in to one file does not make it known to another's
//!   account of the same name. An attacker with many addresses can keep an account's logins
//!   refused from new addresses (the account's limit is what bounds their
//!   guessing, to about 2,900 a day), but not from those its users logged
//!   in from lately. An attacker sharing an address with a user (one NAT,
//!   one /64) can keep that address's logins refused for as long as it
//!   goes on failing.
//!
//! What counts as a failure: a login answered 212 (a wrong password, an
//! unknown account, an account that may not log in, an unsupported login
//! type) and a login whose credentials do not decode. A right password for
//! an account the file does not admit (210, 211, 3, 9) does not count; an
//! accepted login does not clear earlier failures. A refused login counts
//! for nothing, so refusals cannot extend a limit. A login is counted when
//! it starts, before its password is checked, so many at once from one
//! address cannot all slip in under its allowance.
//!
//! **Empty passwords** are counted apart, by address only, at the
//! address's rate ([`LoginLimits::begin_empty`]), and an address out of its
//! own allowance is refused them too. A login accepted on a connection
//! takes back that connection's empty-password failures
//! ([`LoginLimits::take_back`]). FileMaker Pro's first Login after it opens
//! a hosted file is such a failure, every time, before the login the user
//! typed or one it remembered: the file's "Log in using" account with an
//! empty password (network-protocol.md 5.4). Counted as other failures, a
//! user opening files again and again was locked out, at the seventh open
//! within four minutes in #440. An empty password tries one fixed value,
//! so it gives no guesses: its count only bounds how often an address may
//! have such logins checked, and only a client that logs in gets it back.
//! Kept apart, taking it back cannot reach the counts that bound guessing,
//! which a failure that has already worn off would otherwise reduce.
//!
//! The first failure that uses up an address's or an account's allowance
//! is logged as a warning, with the peer and the account name; refusals
//! are logged at debug level, so a flood of them cannot fill the log.
//!
//! Each table keeps at most [`MAX_ENTRIES`] entries; an entry ends when
//! its allowance is whole again, and a full table forgets the entry
//! closest to that (the one least held back), so names and addresses an
//! attacker makes up cannot grow it or push out an account under attack.
//! Account names are kept as digests, not as text. Restarting the server
//! forgets every count.
//!
//! The Admin Console's sign-in (om-admin) locks an address out for 5
//! minutes after 5 failures in 5 minutes, and a sign-in clears its
//! address's failures. Login allows the same long-run rate per address, 1
//! a minute, but a user who mistypes waits a minute rather than five, the
//! per-address and per-account allowances refill steadily so that the one
//! provably bounds the other, and users logging in from an address an
//! attacker shares do not give the attacker fresh guesses.
//!
//! A private host of a local file ([`crate::LocalHost`]) has no limits:
//! only the one client that opened the file can complete its TLS
//! handshake, and that client has read the file, whose password verifiers
//! it can try offline far faster than any limit here allows. A limit
//! would only shut a user who mistypes out of their own file.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use log::warn;

/// FileMaker's error 214, "Too many login attempts" (DBEngine string
/// resource 159, `docs/re/catalogs/error-codes.csv`): the Login error for
/// a login refused by these limits. Whether FileMaker Server's lockout
/// answers with it is not measured.
pub const LOGIN_TOO_MANY_ATTEMPTS: u16 = 214;

/// How fast failures may come: `burst` at once, then one per `interval`.
#[derive(Clone, Copy)]
struct Rate {
    burst: u32,
    interval: Duration,
}

impl Rate {
    /// How far ahead of now a key's allowance may be whole again before
    /// one more failure would go over it.
    fn slack(self) -> Duration {
        self.interval * (self.burst - 1)
    }
}

/// One client address.
const ADDRESS: Rate = Rate {
    burst: 5,
    interval: Duration::from_secs(60),
};

/// One address's logins with an empty password, counted apart.
const EMPTY: Rate = ADDRESS;

/// One account name, from every address.
const ACCOUNT: Rate = Rate {
    burst: 30,
    interval: Duration::from_secs(30),
};

// Two addresses failing as fast as they may cannot use up an account's
// allowance: together they add at most 2 * 5 at once and one per
// 30 s, and the account takes 30 at once and refills one per 30 s.
const _: () = assert!(
    ACCOUNT.burst > 2 * ADDRESS.burst
        && 2 * ACCOUNT.interval.as_secs() <= ADDRESS.interval.as_secs()
);

/// How long an address an account logged in from is not held back by the
/// account's limit.
const KNOWN_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Entries each table keeps at most.
const MAX_ENTRIES: usize = 10_000;

/// An account name as counted: a digest of its lowercase form.
type AccountKey = [u8; 32];

/// The address a client is counted by: an IPv4 address (also when mapped
/// into IPv6), or an IPv6 address's /64 network.
fn client_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut octets = v6.octets();
                octets[8..].fill(0);
                IpAddr::V6(octets.into())
            }
        },
    }
}

/// The key `name` in the file `file` is counted by: both without case.
fn account_key(file: &str, name: &str) -> AccountKey {
    let key = format!("{}\0{}", file.to_lowercase(), name.to_lowercase());
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let mut key = [0; 32];
    key.copy_from_slice(digest.as_ref());
    key
}

/// A client key as the log shows it.
struct Client(IpAddr);

impl std::fmt::Display for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            IpAddr::V4(v4) => write!(f, "{v4}"),
            IpAddr::V6(v6) => write!(f, "{v6}/64"),
        }
    }
}

/// Failures by key, each kept as the time its allowance is whole again
/// (the generic cell rate algorithm): a failure moves that time one
/// interval on, and a key is held back while it is more than the rate's
/// slack ahead of now.
struct Buckets<K> {
    rate: Rate,
    entries: HashMap<K, Bucket>,
}

struct Bucket {
    full_at: Instant,
    /// Whether using up this allowance has been logged since it was last
    /// whole.
    warned: bool,
}

impl<K: Copy + Eq + Hash> Buckets<K> {
    fn new(rate: Rate) -> Self {
        Self {
            rate,
            entries: HashMap::new(),
        }
    }

    /// Whether `key` has used up its allowance at `now`.
    fn exhausted(&self, key: &K, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|b| b.full_at.saturating_duration_since(now) > self.rate.slack())
    }

    /// Count a failure of `key` at `now`.
    fn charge(&mut self, key: K, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_ENTRIES {
            self.make_room(now);
        }
        let bucket = self.entries.entry(key).or_insert(Bucket {
            full_at: now,
            warned: false,
        });
        if bucket.full_at <= now {
            *bucket = Bucket {
                full_at: now,
                warned: false,
            };
        }
        bucket.full_at += self.rate.interval;
    }

    /// Take back a failure [`Buckets::charge`] counted.
    fn refund(&mut self, key: &K, now: Instant) {
        let Some(bucket) = self.entries.get_mut(key) else {
            return;
        };
        match bucket.full_at.checked_sub(self.rate.interval) {
            Some(full_at) if full_at > now => bucket.full_at = full_at,
            _ => {
                self.entries.remove(key);
            }
        }
    }

    /// Whether `key` has used up its allowance and that is not logged yet;
    /// it counts as logged from now on.
    fn newly_exhausted(&mut self, key: &K, now: Instant) -> bool {
        let slack = self.rate.slack();
        match self.entries.get_mut(key) {
            Some(b) if !b.warned && b.full_at.saturating_duration_since(now) > slack => {
                b.warned = true;
                true
            }
            _ => false,
        }
    }

    /// Forget the entries whose allowance is whole, or failing that the one
    /// closest to it.
    fn make_room(&mut self, now: Instant) {
        self.entries.retain(|_, b| b.full_at > now);
        if self.entries.len() >= MAX_ENTRIES {
            if let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, b)| b.full_at)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&key);
            }
        }
    }
}

/// When each account last logged in from each address.
#[derive(Default)]
struct Known {
    entries: HashMap<(AccountKey, IpAddr), Instant>,
}

impl Known {
    fn contains(&self, key: &(AccountKey, IpAddr), now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|&at| now.saturating_duration_since(at) < KNOWN_FOR)
    }

    fn insert(&mut self, key: (AccountKey, IpAddr), now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_ENTRIES {
            self.entries
                .retain(|_, at| now.saturating_duration_since(*at) < KNOWN_FOR);
            if self.entries.len() >= MAX_ENTRIES {
                if let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(key, _)| *key)
                {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(key, now);
    }
}

/// The failed logins of one server.
pub(crate) struct LoginLimits {
    state: Mutex<State>,
}

struct State {
    addresses: Buckets<IpAddr>,
    /// Each address's failed logins with an empty password.
    empties: Buckets<IpAddr>,
    accounts: Buckets<AccountKey>,
    known: Known,
    /// The time the limits are measured in, if a test stopped it
    /// ([`crate::ServerHandle::stop_login_clock`]).
    stopped: Option<Instant>,
}

impl State {
    fn now(&self) -> Instant {
        self.stopped.unwrap_or_else(Instant::now)
    }
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                addresses: Buckets::new(ADDRESS),
                empties: Buckets::new(EMPTY),
                accounts: Buckets::new(ACCOUNT),
                known: Known::default(),
                stopped: None,
            }),
        }
    }
}

/// A login being checked, counted as a failure until
/// [`LoginLimits::passed`] takes it back; after [`LoginLimits::failed`],
/// a failure [`LoginLimits::take_back`] may take back.
#[must_use]
#[derive(Clone, Copy)]
pub(crate) struct Attempt {
    client: IpAddr,
    account: Option<AccountKey>,
    /// Counted apart, with an empty password ([`LoginLimits::begin_empty`]).
    empty: bool,
}

/// Why a login was refused.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    /// Its address has failed too often lately.
    Address,
    /// Its account name has, from every address, and it has not logged in
    /// from this one lately.
    Account,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Address => "too many failed logins from this address",
            Refusal::Account => "too many failed logins as this account",
        })
    }
}

/// Which allowances a failed login used up ([`LoginLimits::failed`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UsedUp {
    /// Its address's: its logins are refused for a while.
    pub address: bool,
    /// Its account name's, from all addresses.
    pub account: bool,
}

impl LoginLimits {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Before a login's password is checked: refuse it if `client`, or the
    /// `account` of `file` it names (none when its credentials do not
    /// decode), has failed too often lately; otherwise count it as a
    /// failure until it is known to be none.
    pub fn begin(
        &self,
        client: IpAddr,
        file: &str,
        account: Option<&str>,
    ) -> Result<Attempt, Refusal> {
        let client = client_key(client);
        let account = account.map(|name| account_key(file, name));
        let mut state = self.lock();
        let now = state.now();
        if state.addresses.exhausted(&client, now) {
            return Err(Refusal::Address);
        }
        if let Some(account) = account {
            if state.accounts.exhausted(&account, now)
                && !state.known.contains(&(account, client), now)
            {
                return Err(Refusal::Account);
            }
        }
        state.addresses.charge(client, now);
        if let Some(account) = account {
            state.accounts.charge(account, now);
        }
        Ok(Attempt {
            client,
            account,
            empty: false,
        })
    }

    /// [`LoginLimits::begin`] for a login with an empty password: refused
    /// when `client` is out of its allowance, or of its allowance of such
    /// logins, and counted apart, by address only (module doc).
    pub fn begin_empty(
        &self,
        client: IpAddr,
        file: &str,
        account: Option<&str>,
    ) -> Result<Attempt, Refusal> {
        let client = client_key(client);
        let account = account.map(|name| account_key(file, name));
        let mut state = self.lock();
        let now = state.now();
        if state.addresses.exhausted(&client, now) || state.empties.exhausted(&client, now) {
            return Err(Refusal::Address);
        }
        state.empties.charge(client, now);
        Ok(Attempt {
            client,
            account,
            empty: true,
        })
    }

    /// The login failed (`account` is the name it gave): it stays counted,
    /// and a warning says so when that uses up its address's or account's
    /// allowance, or its address's allowance of logins with an empty
    /// password. Returns which of the first two it used up: the last
    /// refuses only such logins, so it is no lockout of the address.
    pub fn failed(&self, attempt: Attempt, peer: &str, account: Option<&str>) -> UsedUp {
        let mut state = self.lock();
        let now = state.now();
        let empties = attempt.empty && state.empties.newly_exhausted(&attempt.client, now);
        let (address, account_used_up) = if attempt.empty {
            (false, false)
        } else {
            (
                state.addresses.newly_exhausted(&attempt.client, now),
                attempt
                    .account
                    .is_some_and(|key| state.accounts.newly_exhausted(&key, now)),
            )
        };
        drop(state);
        let named = match account {
            Some(name) => format!("as {name:?}"),
            None => "with credentials that do not decode".to_owned(),
        };
        if address {
            warn!(
                "{peer}: too many failed logins from {}, the last {named}: refusing its logins \
                 with error {LOGIN_TOO_MANY_ATTEMPTS} but one every {} s",
                Client(attempt.client),
                ADDRESS.interval.as_secs()
            );
        }
        if empties {
            warn!(
                "{peer}: too many failed logins with an empty password from {}, the last \
                 {named}: refusing those with error {LOGIN_TOO_MANY_ATTEMPTS} but one every {} \
                 s; its other logins are not held back by them",
                Client(attempt.client),
                EMPTY.interval.as_secs()
            );
        }
        if account_used_up {
            warn!(
                "{peer}: too many failed logins {named} from all addresses: refusing them with \
                 error {LOGIN_TOO_MANY_ATTEMPTS} but one every {} s, except from addresses it \
                 logged in from lately",
                ACCOUNT.interval.as_secs()
            );
        }
        UsedUp {
            address,
            account: account_used_up,
        }
    }

    /// Take back `failures`, logins with an empty password
    /// [`LoginLimits::failed`] kept counted, once a later login on their
    /// connection was accepted (module doc). Only their own count goes
    /// down: any other failure is never taken back.
    pub fn take_back(&self, failures: impl IntoIterator<Item = Attempt>) {
        let mut state = self.lock();
        let now = state.now();
        for failure in failures.into_iter().filter(|f| f.empty) {
            state.empties.refund(&failure.client, now);
        }
    }

    /// The login's password was right: it is not counted. `logged_in`:
    /// it was accepted, so its address is known for its account.
    pub fn passed(&self, attempt: Attempt, logged_in: bool) {
        let mut state = self.lock();
        let now = state.now();
        if attempt.empty {
            state.empties.refund(&attempt.client, now);
        } else {
            state.addresses.refund(&attempt.client, now);
        }
        if let Some(account) = attempt.account {
            if !attempt.empty {
                state.accounts.refund(&account, now);
            }
            if logged_in {
                state.known.insert((account, attempt.client), now);
            }
        }
    }
}

impl crate::ServerHandle {
    /// Stop the clock the login limits are measured in: from now on it
    /// moves only by [`crate::ServerHandle::advance_login_clock`]. For
    /// tests of the limits, which otherwise take minutes.
    #[doc(hidden)]
    pub fn stop_login_clock(&self) {
        let mut state = self.shared.login_limits.lock();
        let now = state.now();
        state.stopped = Some(now);
    }

    /// Move the login limits' clock `by` forward, stopping it first.
    #[doc(hidden)]
    pub fn advance_login_clock(&self, by: Duration) {
        let mut state = self.shared.login_limits.lock();
        let now = state.now();
        state.stopped = Some(now + by);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> LoginLimits {
        let limits = LoginLimits::default();
        limits.lock().stopped = Some(Instant::now());
        limits
    }

    const FILE: &str = "Tasks.fmp12";

    fn fail(limits: &LoginLimits, client: IpAddr, account: &str) -> Result<(), Refusal> {
        fail_in(limits, client, FILE, account)
    }

    fn fail_in(
        limits: &LoginLimits,
        client: IpAddr,
        file: &str,
        account: &str,
    ) -> Result<(), Refusal> {
        let attempt = limits.begin(client, file, Some(account))?;
        limits.failed(attempt, "test", Some(account));
        Ok(())
    }

    /// Each file's account is counted, and known, on its own (#732's
    /// review): an address that logged in as Admin of one file is still
    /// held back by another file's Admin, whose password differs.
    #[test]
    fn an_account_is_counted_and_known_in_its_own_file() {
        let limits = limits();
        let known = IpAddr::from([10, 0, 2, 1]);
        let attempt = limits.begin(known, "A.fmp12", Some("Admin")).unwrap();
        limits.passed(attempt, true);
        for host in 1..=6u8 {
            for _ in 0..ADDRESS.burst {
                fail_in(&limits, IpAddr::from([10, 0, 0, host]), "B.fmp12", "Admin").unwrap();
            }
        }
        assert!(matches!(
            limits.begin(known, "b.FMP12", Some("ADMIN")),
            Err(Refusal::Account)
        ));
        let fresh = IpAddr::from([10, 0, 1, 1]);
        assert!(limits.begin(fresh, "A.fmp12", Some("Admin")).is_ok());
    }

    #[test]
    fn an_ipv6_client_counts_by_its_64_and_a_mapped_ipv4_one_as_itself() {
        let limits = limits();
        for _ in 0..ADDRESS.burst {
            fail(&limits, "2001:db8:1:2::10".parse().unwrap(), "a").unwrap();
            fail(&limits, "::ffff:192.0.2.7".parse().unwrap(), "b").unwrap();
        }
        for (client, held) in [
            ("2001:db8:1:2:ffff::1", true),
            ("2001:db8:1:3::10", false),
            ("192.0.2.7", true),
            ("192.0.2.8", false),
        ] {
            let refused = limits.begin(client.parse().unwrap(), FILE, None).is_err();
            assert_eq!(refused, held, "{client}");
        }
    }

    #[test]
    fn made_up_names_and_addresses_neither_grow_the_tables_nor_push_out_an_account_under_attack() {
        let limits = limits();
        // One account used up from six addresses.
        for host in 1..=6u8 {
            for _ in 0..ADDRESS.burst {
                fail(&limits, IpAddr::from([10, 0, 0, host]), "Admin").unwrap();
            }
        }
        let fresh = IpAddr::from([10, 0, 1, 1]);
        assert!(matches!(
            limits.begin(fresh, FILE, Some("admin")),
            Err(Refusal::Account)
        ));
        // Then one failure each for many more names, from many more
        // addresses, than the tables keep.
        for i in 0..MAX_ENTRIES as u32 + 500 {
            let client = IpAddr::from((0x0b00_0000 + i).to_be_bytes());
            fail(&limits, client, &format!("made up {i}")).unwrap();
        }
        let state = limits.lock();
        assert!(state.addresses.entries.len() <= MAX_ENTRIES);
        assert!(state.accounts.entries.len() <= MAX_ENTRIES);
        drop(state);
        assert!(matches!(
            limits.begin(fresh, FILE, Some("ADMIN")),
            Err(Refusal::Account)
        ));
    }
}
