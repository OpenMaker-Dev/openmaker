//! The memory held for clients' uploads, as bytes and as trees, bounded for
//! the whole server and for each client address.
//!
//! A slot list a client uploads takes tens of times its size once it is
//! read into a tree (`om_hbam::tree`), and an upload sent in parts is held
//! until its last part arrives. Each connection's own limits do not add up
//! to a bound: a client with many connections could hold uploads or stage
//! records on each until the server runs out of memory. So all of it is
//! charged to one budget on the server: the bytes of an upload held while
//! its parts arrive (`crate::transfer`), the records a client stages (op14)
//! until they are committed or dropped, and the tree of an upload while it
//! is read and used (op14 and op70 lists, op13 subtrees, op86 programs, op88
//! container data). One client address may hold a share of it
//! ([`ADDRESS_SHARE`]), so one client cannot take it all from the rest.
//!
//! Memory is reserved before a tree is read, as much as the reader may
//! take, and the reader is given that much as its limit, so no tree grows
//! past what is charged for it. What is reserved is held by a [`Charge`],
//! which gives it back when it is dropped: however a request, a staged area
//! or a session ends (a commit, a logout, a dropped connection, a panic in
//! the connection's thread), what it held goes back.
//!
//! The limit is OpenMaker's choice; FileMaker Server's is unmeasured.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// One client address may hold this fraction of the server's budget: a
/// quarter.
pub(crate) const ADDRESS_SHARE: usize = 4;

/// The most memory a tree read from slots takes, per byte of the slots:
/// `om_hbam::tree::read_owned`'s count reaches about 125 for its worst
/// input (nested keys holding one key each, every list taking a place for
/// four), so twice that.
pub(crate) const TREE_PER_BYTE: usize = 256;

/// Memory for a tree beside [`TREE_PER_BYTE`]: its root and the first
/// places of its lists.
pub(crate) const TREE_SLACK: usize = 64 << 10;

/// The most memory reading `len` bytes of slots may take
/// ([`TREE_PER_BYTE`], [`TREE_SLACK`]).
pub(crate) fn tree_bound(len: usize) -> usize {
    len.saturating_mul(TREE_PER_BYTE).saturating_add(TREE_SLACK)
}

/// The server's budget for clients' upload trees.
#[derive(Debug)]
pub(crate) struct UploadMemory {
    /// The most all clients may hold together, in bytes.
    limit: AtomicUsize,
    held: Mutex<Held>,
}

/// What is held, in all and by client address.
#[derive(Debug, Default)]
struct Held {
    total: usize,
    by_address: HashMap<IpAddr, usize>,
}

impl UploadMemory {
    /// A budget of `limit` bytes.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: AtomicUsize::new(limit),
            held: Mutex::default(),
        })
    }

    /// Change the limit. What is held already stays held.
    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit, Ordering::Relaxed);
    }

    /// What all clients hold now (`ServerHandle::upload_memory_in_use`).
    #[cfg(any(test, debug_assertions))]
    pub fn in_use(&self) -> usize {
        self.held().total
    }

    /// What `client` (a [`crate::admission::client_key`]) may still
    /// reserve: the least of what is left of the server's limit and of its
    /// share.
    pub fn available(&self, client: IpAddr) -> usize {
        let limit = self.limit.load(Ordering::Relaxed);
        let held = self.held();
        let own = held.by_address.get(&client).copied().unwrap_or(0);
        let share = (limit / ADDRESS_SHARE).saturating_sub(own);
        limit.saturating_sub(held.total).min(share)
    }

    /// Reserve `bytes` for `client`, or `None` when that would pass the
    /// server's limit or the client's share.
    pub fn reserve(self: &Arc<Self>, client: IpAddr, bytes: usize) -> Option<Charge> {
        let mut charge = Charge::none(self, client);
        charge.grow(bytes).then_some(charge)
    }

    /// The lock never left half-changed: every change under it is one
    /// arithmetic step, so a lock poisoned by a panic elsewhere is used as
    /// it is (and returning a charge while a thread unwinds never panics).
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Memory reserved for one client, given back when dropped.
#[derive(Debug)]
pub(crate) struct Charge {
    memory: Arc<UploadMemory>,
    client: IpAddr,
    bytes: usize,
}

impl Charge {
    /// A charge of nothing, to grow later.
    pub fn none(memory: &Arc<UploadMemory>, client: IpAddr) -> Self {
        Self {
            memory: memory.clone(),
            client,
            bytes: 0,
        }
    }

    /// The bytes it holds.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Reserve `more`, or change nothing and answer false when that would
    /// pass the server's limit or the client's share.
    pub fn grow(&mut self, more: usize) -> bool {
        if more == 0 {
            return true;
        }
        let limit = self.memory.limit.load(Ordering::Relaxed);
        let mut held = self.memory.held();
        let own = held.by_address.get(&self.client).copied().unwrap_or(0);
        let total = held.total.saturating_add(more);
        let own_after = own.saturating_add(more);
        if total > limit || own_after > limit / ADDRESS_SHARE {
            return false;
        }
        held.total = total;
        held.by_address.insert(self.client, own_after);
        self.bytes += more;
        true
    }

    /// Give back all but `bytes` of it.
    pub fn shrink_to(&mut self, bytes: usize) {
        if bytes < self.bytes {
            self.memory.release(self.client, self.bytes - bytes);
            self.bytes = bytes;
        }
    }

    /// Move `bytes` of it, at most all, into `other`, a charge of the same
    /// budget and client, without giving anything back in between.
    pub fn move_into(&mut self, other: &mut Charge, bytes: usize) {
        debug_assert!(Arc::ptr_eq(&self.memory, &other.memory) && self.client == other.client);
        let bytes = bytes.min(self.bytes);
        self.bytes -= bytes;
        other.bytes += bytes;
    }
}

impl UploadMemory {
    fn release(&self, client: IpAddr, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut held = self.held();
        held.total = held.total.saturating_sub(bytes);
        if let Some(own) = held.by_address.get_mut(&client) {
            *own = own.saturating_sub(bytes);
            if *own == 0 {
                held.by_address.remove(&client);
            }
        }
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.memory.release(self.client, self.bytes);
    }
}
