//! The locks clients hold on a hosted file (spec `docs/re/write-path.md` 4.1,
//! `docs/re/schema-path.md` 4): per **lock user** (connection, transaction)
//! on key paths. Record editing takes write locks (type 9) on records;
//! Manage Database takes them on `[08]`, the catalogs and `[T, 05]`, and
//! upgrades a catalog's to exclusive (10) while replacing it. A write or
//! exclusive lock conflicts with any other lock user's write or exclusive
//! lock on the same path, and with another connection's on a path above or
//! below it: a table lock `[T, 05]` and a record lock `[T, 05, R]` exclude
//! each other, as FileMaker's intent locks make them. FileMaker Server never
//! makes a client wait, so neither does this table: a conflict is answered at
//! once with the holders' names.
//!
//! The table counts each connection's locks ([`LockTable::held_by`]) so
//! that a client's requests can be held to a cap (`writes::MAX_LOCKS`);
//! the table itself refuses nothing for being large.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

use om_protocol::write::{Conflict, KeyPath, LOCK_WRITE};

/// Exclusive lock type (`kDBExclusiveLock`).
const LOCK_EXCLUSIVE: u16 = 10;
/// Exclusive-reject lock type, taken on `[T]` to delete a table.
const LOCK_EXCLUSIVE_REJECT: u16 = 11;
/// The type an op62 names to release whatever type it holds
/// (`kDBAnyLockType`).
pub const LOCK_ANY: u16 = 100;

/// Who holds a lock: a connection's transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockUser {
    pub connection: u32,
    pub txn: u32,
}

#[derive(Debug)]
struct Held {
    user: LockUser,
    lock_type: u16,
    /// How many times the user took it; each op62 releases one.
    count: u32,
    /// "session name (account)", for conflict replies.
    holder: String,
}

/// The locks held on one file, by path, and each connection's paths: a
/// lock, a release and a check cost the logarithm of the locks held, so a
/// script holding the records of a large import does not make every other
/// request scan them.
#[derive(Debug, Default)]
pub struct LockTable {
    held: BTreeMap<KeyPath, Vec<Held>>,
    by_connection: HashMap<u32, BTreeSet<KeyPath>>,
    /// How many locks (entries of `held`, each with its own lock user and
    /// type) each connection holds.
    counts: HashMap<u32, usize>,
}

fn excludes(lock_type: u16) -> bool {
    matches!(
        lock_type,
        LOCK_WRITE | LOCK_EXCLUSIVE | LOCK_EXCLUSIVE_REJECT
    )
}

impl LockTable {
    /// The holders of locks that keep `user` from a lock of `lock_type` on
    /// `path`, as a conflict: another lock user's write or exclusive lock
    /// on the path, and another connection's on a path above or below it.
    /// Each holder is named once, in the order found, however many of its
    /// locks conflict: a session holding many record locks below a table's
    /// is one name, not one per record (#136).
    fn conflict(&self, path: &[Vec<u8>], user: LockUser, lock_type: u16) -> Option<Conflict> {
        if !excludes(lock_type) {
            return None;
        }
        let excluding = |h: &&Held| h.user != user && excludes(h.lock_type);
        let other_connection = |h: &&Held| h.user.connection != user.connection;
        let mut others: Vec<&Held> = self
            .held
            .get(path)
            .into_iter()
            .flatten()
            .filter(excluding)
            .collect();
        for len in 0..path.len() {
            if let Some(above) = self.held.get(&path[..len]) {
                others.extend(above.iter().filter(excluding).filter(other_connection));
            }
        }
        let below = self
            .held
            .range::<[Vec<u8>], _>((Bound::Excluded(path), Bound::Unbounded))
            .take_while(|(p, _)| p.starts_with(path));
        for (_, locks) in below {
            others.extend(locks.iter().filter(excluding).filter(other_connection));
        }
        if others.is_empty() {
            return None;
        }
        let mut named = std::collections::HashSet::new();
        Some(Conflict {
            self_conflict: others.iter().any(|h| h.user.connection == user.connection),
            users: others
                .iter()
                .filter(|h| named.insert((h.user.connection, h.holder.as_str())))
                .map(|h| h.holder.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
    }

    /// Take a lock for `user`, or report the conflicting holders.
    pub fn lock(
        &mut self,
        path: &[Vec<u8>],
        user: LockUser,
        lock_type: u16,
        holder: &str,
    ) -> Result<(), Conflict> {
        if let Some(conflict) = self.conflict(path, user, lock_type) {
            return Err(conflict);
        }
        let locks = self.held.entry(path.to_vec()).or_default();
        match locks
            .iter_mut()
            .find(|h| h.user == user && h.lock_type == lock_type)
        {
            Some(held) => held.count = held.count.saturating_add(1),
            None => {
                locks.push(Held {
                    user,
                    lock_type,
                    count: 1,
                    holder: holder.to_string(),
                });
                *self.counts.entry(user.connection).or_default() += 1;
            }
        }
        self.by_connection
            .entry(user.connection)
            .or_default()
            .insert(path.to_vec());
        Ok(())
    }

    /// Change `user`'s lock on `path` from `from` to `to` (op61,
    /// schema-path.md 4.2), or report the holders that keep it from the new
    /// type. A lock the user does not hold is taken.
    pub fn upgrade(
        &mut self,
        path: &[Vec<u8>],
        user: LockUser,
        from: u16,
        to: u16,
        holder: &str,
    ) -> Result<(), Conflict> {
        if let Some(conflict) = self.conflict(path, user, to) {
            return Err(conflict);
        }
        let held = self.held.get_mut(path).and_then(|locks| {
            locks
                .iter_mut()
                .find(|h| h.user == user && h.lock_type == from)
        });
        match held {
            Some(held) => {
                held.lock_type = to;
                Ok(())
            }
            None => self.lock(path, user, to, holder),
        }
    }

    /// Release one count of `user`'s lock of `lock_type` (or of any type,
    /// [`LOCK_ANY`]) on `path` (op62).
    pub fn unlock(&mut self, path: &[Vec<u8>], user: LockUser, lock_type: u16) -> bool {
        let Some(locks) = self.held.get_mut(path) else {
            return false;
        };
        let mut released = false;
        if let Some(i) = locks
            .iter()
            .position(|h| h.user == user && (lock_type == LOCK_ANY || h.lock_type == lock_type))
        {
            released = true;
            locks[i].count -= 1;
            if locks[i].count == 0 {
                locks.remove(i);
                self.released(user.connection, 1);
            }
        }
        self.forget(path, user.connection);
        released
    }

    /// Release every lock `user` holds on `path` (a commit releases the
    /// record it wrote).
    pub fn release_path(&mut self, path: &[Vec<u8>], user: LockUser) {
        if let Some(locks) = self.held.get_mut(path) {
            let before = locks.len();
            locks.retain(|h| h.user != user);
            let released = before - locks.len();
            self.released(user.connection, released);
            self.forget(path, user.connection);
        }
    }

    /// Release every lock of a transaction (op65 with `all` false).
    pub fn release_txn(&mut self, user: LockUser) {
        let paths: Vec<KeyPath> = self
            .by_connection
            .get(&user.connection)
            .map(|paths| paths.iter().cloned().collect())
            .unwrap_or_default();
        for path in paths {
            if let Some(locks) = self.held.get_mut(&path) {
                let before = locks.len();
                locks.retain(|h| h.user != user);
                let released = before - locks.len();
                self.released(user.connection, released);
            }
            self.forget(&path, user.connection);
        }
    }

    /// Release every lock of a connection (op65 with `all` true, disconnect).
    pub fn release_connection(&mut self, connection: u32) {
        self.counts.remove(&connection);
        for path in self.by_connection.remove(&connection).unwrap_or_default() {
            if let Some(locks) = self.held.get_mut(&path) {
                locks.retain(|h| h.user.connection != connection);
                if locks.is_empty() {
                    self.held.remove(&path);
                }
            }
        }
    }

    /// Whether another lock user holds a write or exclusive lock on
    /// `path`, or another connection one on a path above it, as
    /// [`LockTable::lock`] would find: a record under another session's
    /// table lock `[T, 05]` (a schema save checking the table holds no
    /// record, or a table being deleted) is not written either.
    pub fn held_by_other(&self, path: &[Vec<u8>], user: LockUser) -> bool {
        let excluding = |h: &Held| h.user != user && excludes(h.lock_type);
        self.held
            .get(path)
            .is_some_and(|locks| locks.iter().any(excluding))
            || (0..path.len()).any(|len| {
                self.held.get(&path[..len]).is_some_and(|locks| {
                    locks
                        .iter()
                        .any(|h| excluding(h) && h.user.connection != user.connection)
                })
            })
    }

    /// How many locks `connection` holds: one per path, lock user and type,
    /// however many times each was taken.
    pub fn held_by(&self, connection: u32) -> usize {
        self.counts.get(&connection).copied().unwrap_or(0)
    }

    /// Whether `user` holds a lock of `lock_type` on `path`: taking it again
    /// only counts it once more ([`LockTable::lock`]).
    pub fn holds(&self, path: &[Vec<u8>], user: LockUser, lock_type: u16) -> bool {
        self.held.get(path).is_some_and(|locks| {
            locks
                .iter()
                .any(|h| h.user == user && h.lock_type == lock_type)
        })
    }

    /// Every lock held, by path in key order: its user, type, count and
    /// holder (tests compare two tables with it).
    #[cfg(all(test, debug_assertions))]
    pub(crate) fn entries(&self) -> Vec<(KeyPath, LockUser, u16, u32, String)> {
        self.held
            .iter()
            .flat_map(|(path, locks)| {
                locks
                    .iter()
                    .map(|h| (path.clone(), h.user, h.lock_type, h.count, h.holder.clone()))
            })
            .collect()
    }

    /// Count `n` locks of `connection` released.
    fn released(&mut self, connection: u32, n: usize) {
        if let Some(count) = self.counts.get_mut(&connection) {
            *count = count.saturating_sub(n);
            if *count == 0 {
                self.counts.remove(&connection);
            }
        }
    }

    /// After a release on `path`: drop the path when nothing holds it, and
    /// from `connection`'s paths when it holds no lock there any more.
    fn forget(&mut self, path: &[Vec<u8>], connection: u32) {
        let Some(locks) = self.held.get(path) else {
            return;
        };
        if !locks.iter().any(|h| h.user.connection == connection) {
            if let Some(paths) = self.by_connection.get_mut(&connection) {
                paths.remove(path);
                if paths.is_empty() {
                    self.by_connection.remove(&connection);
                }
            }
        }
        if locks.is_empty() {
            self.held.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u8) -> KeyPath {
        vec![vec![0x80, 0x02], vec![0x05], vec![id]]
    }

    #[test]
    fn follows_filemaker_servers_lock_rules() {
        let mut locks = LockTable::default();
        let a = LockUser {
            connection: 1,
            txn: 10,
        };
        let a2 = LockUser { txn: 11, ..a };
        let b = LockUser {
            connection: 2,
            txn: 10,
        };
        // Granted, and re-entrant for the same lock user.
        locks.lock(&record(4), a, LOCK_WRITE, "A (admin)").unwrap();
        locks.lock(&record(4), a, LOCK_WRITE, "A (admin)").unwrap();
        // Another txn of the same connection: a self conflict.
        let conflict = locks
            .lock(&record(4), a2, LOCK_WRITE, "A (admin)")
            .unwrap_err();
        assert!(conflict.self_conflict);
        // Another connection: the holder's name.
        let conflict = locks
            .lock(&record(4), b, LOCK_WRITE, "B (admin)")
            .unwrap_err();
        assert_eq!(
            conflict,
            Conflict {
                self_conflict: false,
                users: "A (admin)".into()
            }
        );
        assert!(locks.held_by_other(&record(4), b));
        // One unlock leaves the second count held.
        locks.unlock(&record(4), a, LOCK_WRITE);
        assert!(locks.held_by_other(&record(4), b));
        locks.release_txn(a);
        locks.lock(&record(4), b, LOCK_WRITE, "B (admin)").unwrap();
        // Notify locks never conflict.
        locks.lock(&[], a, 5, "A (admin)").unwrap();
        locks.lock(&[], b, 5, "B (admin)").unwrap();
        locks.release_connection(2);
        assert!(!locks.held_by_other(&record(4), a));
    }

    /// The table is indexed by path: paths below and above a lock are
    /// found by their segments (`[T, 05]` is above `[T, 05, R]`, not above
    /// `[T, 05 00]`), a transaction's release keeps the connection's other
    /// transactions' locks, and many locks stay cheap.
    #[test]
    fn indexed_locks_keep_the_rules() {
        let mut locks = LockTable::default();
        let script = LockUser {
            connection: 9,
            txn: 1,
        };
        let other = LockUser {
            connection: 2,
            txn: 4,
        };
        for id in 0..20_000u32 {
            let path = vec![vec![0x80, 0x01], vec![0x05], id.to_be_bytes().to_vec()];
            locks.lock(&path, script, LOCK_WRITE, "S (admin)").unwrap();
        }
        // Each is counted once for the connection, however often it is
        // taken.
        let first = vec![vec![0x80, 0x01], vec![0x05], 0u32.to_be_bytes().to_vec()];
        locks.lock(&first, script, LOCK_WRITE, "S (admin)").unwrap();
        assert!(locks.holds(&first, script, LOCK_WRITE));
        assert_eq!(locks.held_by(9), 20_000);
        let table = vec![vec![0x80, 0x01], vec![0x05]];
        assert_eq!(
            locks
                .lock(&table, other, LOCK_WRITE, "B")
                .unwrap_err()
                .users,
            // One holder of 20,000 record locks is named once (#136).
            "S (admin)"
        );
        // A segment that only starts with the table's bytes is not below it.
        let sibling = vec![vec![0x80, 0x01], vec![0x05, 0x00]];
        locks.lock(&sibling, other, LOCK_WRITE, "B").unwrap();
        // The script's other transaction keeps its lock when txn 1 ends.
        let kept = vec![vec![0x80, 0x02], vec![0x05], vec![1]];
        let second = LockUser { txn: 2, ..script };
        locks.lock(&kept, second, LOCK_WRITE, "S (admin)").unwrap();
        locks.release_txn(script);
        assert_eq!(locks.held_by(9), 1);
        locks.lock(&table, other, LOCK_WRITE, "B").unwrap();
        assert!(locks.held_by_other(&kept, other));
        locks.release_connection(9);
        assert!(!locks.held_by_other(&kept, other));
        locks.lock(&kept, other, LOCK_WRITE, "B").unwrap();
        // Released twice over, the table forgets the path.
        locks.unlock(&kept, other, LOCK_ANY);
        locks.release_path(&kept, other);
        assert!(!locks.held.contains_key(&kept));
        assert_eq!(locks.held_by(2), 2);
        locks.release_connection(2);
        assert!(locks.held.is_empty() && locks.by_connection.is_empty() && locks.counts.is_empty());
    }

    #[test]
    fn follows_the_schema_lock_rules() {
        let mut locks = LockTable::default();
        let table = vec![vec![0x80, 0x02], vec![0x05]];
        let catalog = vec![vec![0x03], vec![0x10]];
        let editor = LockUser {
            connection: 1,
            txn: 0x2c,
        };
        let other = LockUser {
            connection: 2,
            txn: 7,
        };
        // Another user editing a record keeps the table lock of a save away
        // (schema-path.md 4), and the other way round.
        locks
            .lock(&record(4), other, LOCK_WRITE, "B (admin)")
            .unwrap();
        let saving = LockUser { txn: 0, ..editor };
        let conflict = locks
            .lock(&table, saving, LOCK_WRITE, "A (admin)")
            .unwrap_err();
        assert_eq!(conflict.users, "B (admin)");
        locks.release_connection(2);
        locks.lock(&table, saving, LOCK_WRITE, "A (admin)").unwrap();
        assert!(locks
            .lock(&record(5), other, LOCK_WRITE, "B (admin)")
            .is_err());
        // Nor is another connection's record committed under the table
        // lock, though it holds no lock of its own there; the saving
        // connection's other transactions may write.
        assert!(locks.held_by_other(&record(5), other));
        assert!(!locks.held_by_other(&record(5), LockUser { txn: 3, ..editor }));
        // The same connection's own transactions may nest.
        locks
            .lock(
                &record(5),
                LockUser { txn: 3, ..editor },
                LOCK_WRITE,
                "A (admin)",
            )
            .unwrap();
        // Released with "any type".
        locks.unlock(&table, saving, LOCK_ANY);
        locks
            .lock(&record(6), other, LOCK_WRITE, "B (admin)")
            .unwrap();

        // A catalog lock is upgraded to exclusive, then released as such.
        locks
            .lock(&catalog, editor, LOCK_WRITE, "A (admin)")
            .unwrap();
        locks
            .upgrade(&catalog, editor, LOCK_WRITE, LOCK_EXCLUSIVE, "A (admin)")
            .unwrap();
        assert!(locks
            .lock(&catalog, other, LOCK_WRITE, "B (admin)")
            .is_err());
        locks.unlock(&catalog, editor, LOCK_EXCLUSIVE);
        locks
            .lock(&catalog, other, LOCK_WRITE, "B (admin)")
            .unwrap();
        // An upgrade another user's lock prevents is refused.
        assert!(locks
            .upgrade(&catalog, editor, LOCK_WRITE, LOCK_EXCLUSIVE, "A (admin)")
            .is_err());
    }
}
