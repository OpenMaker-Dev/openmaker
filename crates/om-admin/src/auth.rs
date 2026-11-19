//! Signing in: the administrator's credentials and the lockout of an
//! address that keeps failing (`docs/admin-console.md`, "Security").

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Failed sign-ins from one address that lock it out...
pub(crate) const MAX_FAILURES: usize = 5;
/// ...when they fall within this window...
pub(crate) const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// ...for this long.
pub(crate) const LOCKOUT: Duration = Duration::from_secs(5 * 60);
/// Addresses tracked at once. Past it, the address heard from least recently
/// is forgotten, so many addresses cannot grow the table without bound.
const MAX_TRACKED: usize = 10_000;

/// The outcome of a sign-in, of the console or the Admin API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attempt {
    Accepted,
    /// Wrong credentials.
    Refused,
    /// The address is locked out, before this attempt or by it.
    LockedOut,
}

/// The administrator's password, kept only as its SHA-256 digest. Made from
/// the text with `From` (`"secret".into()`), which keeps nothing of the text
/// itself, so the console never holds the password in plain text.
#[derive(Clone)]
pub struct PasswordDigest([u8; 32]);

impl From<&str> for PasswordDigest {
    fn from(password: &str) -> Self {
        Self(digest(password))
    }
}

impl From<String> for PasswordDigest {
    fn from(password: String) -> Self {
        Self(digest(&password))
    }
}

impl std::fmt::Debug for PasswordDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordDigest(<redacted>)")
    }
}

/// The one administrator account, kept as digests.
pub(crate) struct Credentials {
    user: [u8; 32],
    password: PasswordDigest,
}

impl Credentials {
    pub(crate) fn new(user: &str, password: PasswordDigest) -> Self {
        Self {
            user: digest(user),
            password,
        }
    }

    /// Whether `user` and `password` are the administrator's. Both are
    /// compared in full and in constant time, so the time taken does not
    /// tell which one was wrong or how much of it matched.
    pub(crate) fn check(&self, user: &str, password: &str) -> bool {
        let user = digest(user).ct_eq(&self.user);
        let password = digest(password).ct_eq(&self.password.0);
        (user & password).into()
    }
}

fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// Failed sign-ins by address. An IPv6 address counts by its /64 network,
/// which a single client usually holds whole.
#[derive(Default)]
pub(crate) struct Throttle {
    addresses: HashMap<IpAddr, Failures>,
}

#[derive(Default)]
struct Failures {
    /// The latest failures within [`FAILURE_WINDOW`], oldest first.
    times: VecDeque<Instant>,
    locked_until: Option<Instant>,
    last_seen: Option<Instant>,
}

impl Throttle {
    /// Whether `address` is locked out at `now`.
    pub(crate) fn locked(&mut self, address: IpAddr, now: Instant) -> bool {
        let key = client_key(address);
        let Some(entry) = self.addresses.get_mut(&key) else {
            return false;
        };
        match entry.locked_until {
            Some(until) if now < until => true,
            Some(_) => {
                self.addresses.remove(&key);
                false
            }
            None => false,
        }
    }

    /// Record a failed sign-in from `address` at `now`. True when it locks
    /// the address out.
    pub(crate) fn failed(&mut self, address: IpAddr, now: Instant) -> bool {
        let key = client_key(address);
        if !self.addresses.contains_key(&key) && self.addresses.len() >= MAX_TRACKED {
            self.forget_one(now);
        }
        let entry = self.addresses.entry(key).or_default();
        entry.last_seen = Some(now);
        while entry
            .times
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= FAILURE_WINDOW)
        {
            entry.times.pop_front();
        }
        entry.times.push_back(now);
        if entry.times.len() >= MAX_FAILURES {
            entry.times.clear();
            entry.locked_until = Some(now + LOCKOUT);
            return true;
        }
        false
    }

    /// A successful sign-in from `address` clears its failures.
    pub(crate) fn succeeded(&mut self, address: IpAddr) {
        self.addresses.remove(&client_key(address));
    }

    /// Make room: drop the addresses whose failures and lockout are over, or
    /// failing that the one heard from least recently.
    fn forget_one(&mut self, now: Instant) {
        self.addresses.retain(|_, f| {
            let locked = f.locked_until.is_some_and(|until| now < until);
            let recent = f
                .times
                .back()
                .is_some_and(|&t| now.saturating_duration_since(t) < FAILURE_WINDOW);
            locked || recent
        });
        if self.addresses.len() >= MAX_TRACKED {
            if let Some(oldest) = self
                .addresses
                .iter()
                .min_by_key(|(_, f)| f.last_seen)
                .map(|(k, _)| *k)
            {
                self.addresses.remove(&oldest);
            }
        }
    }
}

/// The address a client is counted by: an IPv4 address (also when mapped
/// into IPv6), or an IPv6 address's /64 network, which one client usually
/// holds whole.
pub(crate) fn client_key(address: IpAddr) -> IpAddr {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_need_both_parts() {
        let c = Credentials::new("admin", "s3cret".into());
        assert!(c.check("admin", "s3cret"));
        assert!(!c.check("admin", "s3cret "));
        assert!(!c.check("Admin", "s3cret"));
        assert!(!c.check("", ""));
    }

    #[test]
    fn failures_lock_an_address_only_within_the_window() {
        let a: IpAddr = "192.0.2.7".parse().unwrap();
        let t0 = Instant::now();
        let mut throttle = Throttle::default();
        // Four failures, then a fifth after the first has left the window.
        for i in 0..4 {
            assert!(!throttle.failed(a, t0 + Duration::from_secs(i)));
        }
        assert!(!throttle.failed(a, t0 + FAILURE_WINDOW));
        assert!(!throttle.locked(a, t0 + FAILURE_WINDOW));
        // A fifth within the window (with the second to fifth) locks it,
        // for LOCKOUT.
        let t1 = t0 + FAILURE_WINDOW;
        assert!(throttle.failed(a, t1));
        assert!(throttle.locked(a, t1 + LOCKOUT - Duration::from_secs(1)));
        // Another address of the same IPv6 /64 is the same client; another
        // /64 is not.
        let v6: IpAddr = "2001:db8:1:2::10".parse().unwrap();
        for i in 0..5 {
            throttle.failed(v6, t0 + Duration::from_secs(i));
        }
        assert!(throttle.locked("2001:db8:1:2:ffff::1".parse().unwrap(), t0));
        assert!(!throttle.locked("2001:db8:1:3::10".parse().unwrap(), t0));
        // After the lockout the address starts again.
        assert!(!throttle.locked(a, t1 + LOCKOUT));
        assert!(!throttle.failed(a, t1 + LOCKOUT));
    }
}
