//! Admitting connections under the caps of [`ConnectionLimits`]: how many
//! the server serves at once, in all and from one client address. A
//! connection is admitted at its accept, before any TLS work or thread,
//! and holds its [`Slot`] until its thread ends.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::config::ConnectionLimits;

/// The connections being served, in all and by client address.
#[derive(Default)]
pub(crate) struct Admissions {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    total: usize,
    /// By [`client_key`]; an address with none is not kept.
    by_address: HashMap<IpAddr, usize>,
}

/// Why a connection was not admitted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The server serves its limit of connections.
    Full(usize),
    /// The client's address has its limit of connections.
    AddressFull(usize),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Full(n) => write!(f, "the server already serves {n} connections"),
            Refused::AddressFull(n) => write!(f, "{n} connections from this address already"),
        }
    }
}

/// A connection's place under the caps, given back when it is dropped:
/// held by the connection's thread, it is released however the thread
/// ends, by a panic too, or at once if no thread starts.
pub(crate) struct Slot {
    admissions: Arc<Admissions>,
    key: IpAddr,
}

impl Admissions {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock; a poisoned one is still
        // consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admit a connection from `ip` if neither cap of `limits` is reached.
    pub fn admit(self: &Arc<Self>, ip: IpAddr, limits: &ConnectionLimits) -> Result<Slot, Refused> {
        let key = client_key(ip);
        let mut state = self.lock();
        if state.total >= limits.connections {
            return Err(Refused::Full(state.total));
        }
        let from_address = state.by_address.get(&key).copied().unwrap_or(0);
        if from_address >= limits.per_address {
            return Err(Refused::AddressFull(from_address));
        }
        state.by_address.insert(key, from_address + 1);
        state.total += 1;
        Ok(Slot {
            admissions: Arc::clone(self),
            key,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = self.admissions.lock();
        state.total -= 1;
        if let Some(count) = state.by_address.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                state.by_address.remove(&self.key);
            }
        }
    }
}

/// The address a client's connections are counted under: its IP address,
/// cut to its /64 for IPv6 (the prefix one site or host is given, in which
/// it can pick any number of addresses), and an IPv4-mapped IPv6 address
/// (an IPv4 client of a dual-stack socket) as the IPv4 address it is.
pub(crate) fn client_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
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
    fn caps_count_by_address_and_release_however_a_connection_ends() {
        let limits = ConnectionLimits {
            connections: 3,
            per_address: 2,
            ..ConnectionLimits::DEFAULT
        };
        let admissions = Arc::new(Admissions::default());
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();

        // One IPv6 /64 is one address, whatever its interface ids.
        let first = admissions.admit(ip("2001:db8:1:2::1"), &limits).unwrap();
        let second = admissions
            .admit(ip("2001:db8:1:2:ffff::9"), &limits)
            .unwrap();
        assert_eq!(
            admissions.admit(ip("2001:db8:1:2::3"), &limits).err(),
            Some(Refused::AddressFull(2))
        );
        // An IPv4 client of a dual-stack socket counts as its IPv4
        // address, not as one of ::ffff:0:0/96 with every other one.
        let mapped = admissions.admit(ip("::ffff:192.0.2.7"), &limits).unwrap();
        assert_eq!(
            admissions.admit(ip("192.0.2.8"), &limits).err(),
            Some(Refused::Full(3))
        );

        // A slot held by a thread that panics is released all the same.
        let panicked = std::thread::spawn(move || {
            let _held = first;
            panic!("a connection's thread fails");
        })
        .join();
        assert!(panicked.is_err());
        let again = admissions.admit(ip("2001:db8:1:2::3"), &limits).unwrap();

        drop((second, mapped, again));
        let state = admissions.lock();
        assert_eq!((state.total, state.by_address.len()), (0, 0));
    }
}
