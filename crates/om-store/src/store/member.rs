//! A store as a member of a cluster (`docs/server-storage.md` section 7.1):
//! the operations the consensus core ([`crate::consensus`]) asks of its
//! log, its `state` and its page file. Entries reach the log from the
//! leader, or as the leader's own, without being applied; only what is
//! committed is applied and published, in order, as the open's replay
//! applies it (section 6.7, step 4). A member's open writes no entry of
//! its own.
//!
//! Everything here that changes the store is the crate's alone: outside
//! it, a member's store is reached through [`crate::consensus::Member`],
//! which runs only what its core asks. The single server's writes, its
//! checkpoints with restore points, dropping a restore point, freeing
//! pages on demand and acknowledging incidents are refused on a member
//! (`Store::not_member`); a member's own checkpoint frees what it can as
//! any checkpoint does.

use std::sync::Arc;

use super::{
    apply_entry, pin_version, CheckpointFailed, NotApplied, OpenError, Opened, PinOf, Purpose,
    Store, Version, WriteError,
};
use crate::apply::Position;
use crate::consensus::{Pos, Wire};
use crate::entry::{Entry, ServerId};
use crate::log::LogError;
use crate::page::{Hash, TreeName};
use crate::pagefile::SpaceError;
use crate::sys::Fs;
use crate::sys::Random;

/// The most committed entries applied from one read of the log, and the
/// most payload bytes: a publication of many entries, or of a follower
/// catching up, is applied in pieces of at most this much.
const APPLY_ENTRIES: usize = 64;
const APPLY_BYTES: u64 = 4 << 20;

/// An entry a member applied, its hash, and the hashes of the state's trees
/// right after it ([`Store::take_applied`]).
#[cfg(test)]
pub(crate) type Applied = (u64, [u8; 32], Vec<(TreeName, Hash)>);

/// The hashes of the newest applied state's trees, by name.
#[cfg(test)]
pub(crate) fn newest_root_hashes(pages: &crate::pagefile::PageFile) -> Vec<(TreeName, Hash)> {
    crate::page::PageStore::trees(pages)
        .into_iter()
        .map(|(name, root)| (name, root.hash))
        .collect()
}

/// What the consensus core starts from: the member's `state` and the terms
/// of the entries its log holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovered {
    pub server: ServerId,
    pub start_count: u64,
    pub term: u64,
    pub vote: Option<ServerId>,
    /// The entry before the first one the log holds.
    pub base: Pos,
    /// The terms of the log's entries after `base`, in order: the last is
    /// the durable entry's.
    pub terms: Vec<u64>,
    /// The committed position the marker recorded.
    pub committed: u64,
    /// The entry the recovered state is of.
    pub published: u64,
    /// The log's largest entry after `base`: its index and payload size.
    pub largest: Option<(u64, u64)>,
}

impl Store {
    /// Makes a member's store in an empty folder and says its id, which
    /// the cluster's settings name ([`crate::consensus::Config::members`]);
    /// it is opened with [`crate::consensus::Member::open`].
    pub fn create_member(
        fs: Arc<dyn Fs>,
        random: &dyn Random,
        config: super::Config,
    ) -> Result<ServerId, OpenError> {
        let (store, _) = Store::create_as(fs, random, config, true)?;
        Ok(store.server())
    }

    /// Opens the store as a member of a cluster (section 7.1): the replay
    /// applies as far as the committed position its marker recorded, the
    /// term is the one `state` recorded, and the open writes no entry.
    pub(crate) fn open_member(
        fs: Arc<dyn Fs>,
        config: super::Config,
    ) -> Result<(Store, Opened), OpenError> {
        Store::start_as(fs, config, super::How::Normal, true)
    }

    /// Whether the store is a member of a cluster.
    pub fn is_member(&self) -> bool {
        self.member
    }

    /// What the consensus core starts from (reads the log's entries after
    /// its base).
    pub(crate) fn recovered(&self) -> Result<Recovered, WriteError> {
        self.usable()?;
        let values = *self.log.state().values();
        let base = self.log.base();
        let mut terms = Vec::new();
        let mut largest: Option<(u64, u64)> = None;
        if self.log.durable().index > base.index {
            for e in self.log.read(base.index + 1).map_err(WriteError::Log)? {
                let e = e.map_err(WriteError::Log)?;
                terms.push(e.term);
                let size = e.payload.len() as u64;
                if largest.is_none_or(|(_, s)| size > s) {
                    largest = Some((e.index, size));
                }
            }
        }
        Ok(Recovered {
            server: values.server_id.0,
            start_count: values.start_count,
            term: values.term,
            vote: values.vote.map(|v| v.0),
            base: Pos {
                index: base.index,
                term: base.term,
            },
            terms,
            committed: self.log.committed(),
            published: self.published.position.index,
            largest,
        })
    }

    /// Records the term and the vote in `state`, both slots flushed
    /// (section 7.1: before a vote or an append is answered).
    pub(crate) fn save_term(
        &mut self,
        term: u64,
        vote: Option<ServerId>,
    ) -> Result<(), WriteError> {
        self.usable()?;
        let result = self.log.update_state(|v| {
            v.term = term;
            v.vote = vote.map(crate::state::ServerId);
        });
        match result {
            Ok(()) => Ok(()),
            Err(e) => Err(self.stop_log(format!("the term's record failed: {e}"), &e)),
        }
    }

    /// Appends the leader's entries to the log, unapplied, each checked to
    /// land where the leader has it with the hash it has. A batch the log
    /// cannot hold in one write is flushed in pieces: the entries are
    /// acknowledged only after the flush the core asks for next.
    pub(crate) fn append_wire(&mut self, entries: &[Wire]) -> Result<(), WriteError> {
        self.usable()?;
        for e in entries {
            let mut placed = self.log.append(e.term, e.kind, e.file, &e.payload);
            if let Err(LogError::BatchFull) = placed {
                if let Err(f) = self.log.flush_batch(None) {
                    return Err(self.stop_log(format!("the log's flush failed: {f}"), &f));
                }
                placed = self.log.append(e.term, e.kind, e.file, &e.payload);
            }
            match placed {
                Ok(p) if p.index == e.index && p.hash == e.hash => {}
                Ok(_) => {
                    return Err(self.stop(
                        "the leader's entry does not continue this log's chain where it says",
                    ))
                }
                Err(e) if self.log.is_poisoned() => {
                    return Err(self.stop_log(e.to_string(), &e));
                }
                Err(e) => return Err(WriteError::Log(e)),
            }
        }
        Ok(())
    }

    /// Appends the leader's own entry of term `term` to the log's open
    /// batch, unapplied: it is applied when committed.
    pub(crate) fn propose(
        &mut self,
        term: u64,
        file: u64,
        payload: &crate::entry::Payload,
    ) -> Result<Wire, WriteError> {
        self.usable()?;
        let bytes = payload.encode().map_err(WriteError::Payload)?;
        let kind = payload.kind().code();
        match self.log.append(term, kind, file, &bytes) {
            Ok(p) => Ok(Wire {
                index: p.index,
                term: p.term,
                kind,
                file,
                payload: bytes,
                hash: p.hash,
            }),
            Err(e) if self.log.is_poisoned() => Err(self.stop_log(e.to_string(), &e)),
            Err(e) => Err(WriteError::Log(e)),
        }
    }

    /// Flushes the open batch, recording `committed` in the marker, and
    /// returns the durable entry.
    pub(crate) fn flush_member(&mut self, committed: u64) -> Result<Pos, WriteError> {
        self.usable()?;
        match self.log.flush_batch(Some(committed)) {
            Ok(p) => Ok(Pos {
                index: p.index,
                term: p.term,
            }),
            Err(e) => Err(self.stop_log(format!("the log's flush failed: {e}"), &e)),
        }
    }

    /// Records `committed` in the marker (it never goes down), which also
    /// confirms it for a cut.
    pub(crate) fn set_committed(&mut self, committed: u64) -> Result<(), WriteError> {
        self.usable()?;
        match self.log.set_committed(committed) {
            Ok(()) => Ok(()),
            Err(e @ LogError::Refused(_)) => Err(WriteError::Log(e)),
            Err(e) => Err(self.stop_log(format!("the committed position: {e}"), &e)),
        }
    }

    /// Cuts the log from `from` (section 7.1), after recording `committed`.
    pub(crate) fn cut(&mut self, committed: u64, from: u64) -> Result<(), WriteError> {
        self.set_committed(committed)?;
        match self.log.cut(from) {
            Ok(_) => Ok(()),
            Err(e) if self.log.is_poisoned() => Err(self.stop_log(e.to_string(), &e)),
            Err(e) => Err(WriteError::Log(e)),
        }
    }

    /// Applies the committed entries after the published one, through
    /// `through`, each once, in order, and publishes them (section 7.1).
    /// Never past the committed position the marker records, whatever
    /// `through` says. The entries are read and applied in pieces
    /// ([`APPLY_ENTRIES`], [`APPLY_BYTES`]); when the pages waiting for a
    /// checkpoint pass their budget, what is applied is published and
    /// checkpointed, and the application goes on.
    pub(crate) fn publish(&mut self, through: u64) -> Result<u64, WriteError> {
        self.publish_within(through, self.log.committed())
    }

    /// [`Store::publish`] within `bound`: the committed position, but for
    /// the simulator's planted defect (`consensus::Plant::PublishDurable`).
    pub(crate) fn publish_within(&mut self, through: u64, bound: u64) -> Result<u64, WriteError> {
        self.usable()?;
        let through = through.min(bound).min(self.log.durable().index);
        while self.published.position.index < through {
            let from = self.published.position.index + 1;
            let mut bytes = 0u64;
            let mut piece = Vec::new();
            for e in self.log.read(from).map_err(WriteError::Log)? {
                let e = e.map_err(WriteError::Log)?;
                let len = e.payload.len() as u64;
                // A piece holds one entry whatever its size, and more only
                // within the budget.
                if e.index > through
                    || piece.len() >= APPLY_ENTRIES
                    || (!piece.is_empty() && bytes + len > APPLY_BYTES)
                {
                    break;
                }
                bytes += len;
                piece.push(e);
            }
            if piece.is_empty() {
                return Err(self.stop(format!("the log holds no entry {from} to apply")));
            }
            self.apply_piece(&piece)?;
        }
        Ok(self.published.position.index)
    }

    /// Applies committed entries read from the log, in order, and
    /// publishes the last.
    fn apply_piece(&mut self, piece: &[crate::log::Entry]) -> Result<(), WriteError> {
        let mut last = self.published.position;
        for e in piece {
            let entry = Entry {
                index: e.index,
                term: e.term,
                kind: e.kind,
                file: e.file,
                payload: &e.payload,
                hash: Hash(e.hash),
            };
            let mut applied = apply_entry(&self.pages, &entry, Purpose::Entry);
            if let Err(NotApplied::Space(SpaceError::CheckpointNeeded { .. })) = applied {
                // What is applied is committed and durable.
                self.publish_at(last)?;
                self.checkpoint_member()?;
                applied = apply_entry(&self.pages, &entry, Purpose::Entry);
            }
            match applied {
                Ok(_) => {}
                Err(NotApplied::Space(space)) => {
                    return Err(self.stop(format!(
                        "no space to apply committed entry {}: {space}",
                        entry.index
                    )))
                }
                Err(NotApplied::Apply(error)) => {
                    return Err(self.stop(format!(
                        "committed entry {} cannot be applied: {error}",
                        entry.index
                    )))
                }
            }
            last = Position {
                index: e.index,
                term: e.term,
                hash: Hash(e.hash),
            };
            #[cfg(test)]
            self.applied
                .push((e.index, e.hash, newest_root_hashes(&self.pages)));
        }
        self.publish_at(last)
    }

    /// Publishes the version of applied entry `last`.
    fn publish_at(&mut self, last: Position) -> Result<(), WriteError> {
        if last.index == self.published.position.index {
            return Ok(());
        }
        let pin = match pin_version(&self.pages, PinOf::Newest(last.index)) {
            Ok(pin) => pin,
            Err(e) => return Err(self.stop(format!("the version of entry {}: {e}", last.index))),
        };
        self.published = Version {
            position: last,
            pin: Arc::new(pin),
        };
        Ok(())
    }

    /// Takes a checkpoint of the published state, which retires the log as
    /// far as the checkpoints kept allow, and frees what it can. Any
    /// failure stops the store: a member that cannot checkpoint cannot
    /// apply what is committed (section 7.5, repair, is not built).
    pub(crate) fn checkpoint_member(&mut self) -> Result<(), WriteError> {
        match self.checkpoint_as(None) {
            Ok(_) => Ok(()),
            Err(CheckpointFailed::Stopped(why)) => Err(WriteError::Stopped(why)),
            Err(CheckpointFailed::NoSpace(why)) if self.stopped.is_some() => {
                Err(WriteError::NoSpace(why))
            }
            Err(e) => Err(self.stop(format!("a member's checkpoint: {e}"))),
        }
    }

    /// The hashes of the published state's trees, by name: equal on every
    /// member at the same entry (section 12).
    pub fn root_hashes(&self) -> Vec<(TreeName, Hash)> {
        self.published
            .pin
            .roots()
            .iter()
            .map(|(name, root)| (name.clone(), root.hash))
            .collect()
    }

    /// Each entry applied since the last call, with its hash and the hashes
    /// of the state's trees right after it: what the simulator compares
    /// between members at every entry (section 12).
    #[cfg(test)]
    pub(crate) fn take_applied(&mut self) -> Vec<Applied> {
        std::mem::take(&mut self.applied)
    }

    /// The durable entries from `from` through at most `to`, to send to
    /// another member: the first, then as many more as keep their payloads
    /// within `max_bytes`, and none whose payload is past `max_entry`.
    pub(crate) fn read_wire(
        &self,
        from: u64,
        to: u64,
        max_bytes: u64,
        max_entry: u64,
    ) -> Result<Vec<Wire>, WriteError> {
        self.usable()?;
        let mut out = Vec::new();
        if from > to {
            return Ok(out);
        }
        let mut bytes = 0u64;
        for e in self.log.read(from).map_err(WriteError::Log)? {
            let e = e.map_err(WriteError::Log)?;
            if e.index > to {
                break;
            }
            let len = e.payload.len() as u64;
            if len > max_entry || (!out.is_empty() && bytes + len > max_bytes) {
                break;
            }
            bytes += len;
            out.push(Wire {
                index: e.index,
                term: e.term,
                kind: e.kind,
                file: e.file,
                payload: e.payload,
                hash: e.hash,
            });
        }
        Ok(out)
    }

    /// The term and hash of the log's entry at `index`: the base's, or a
    /// durable entry's; `None` when the log holds none there.
    pub fn entry_at(&self, index: u64) -> Result<Option<(u64, [u8; 32])>, WriteError> {
        let base = self.log.base();
        if index == base.index {
            return Ok(Some((base.term, base.hash)));
        }
        if index < base.index || index > self.log.durable().index {
            return Ok(None);
        }
        let entries = self.read_wire(index, index, u64::MAX, u64::MAX)?;
        Ok(entries
            .first()
            .filter(|e| e.index == index)
            .map(|e| (e.term, e.hash)))
    }

    /// The hash of the log's entry at `index` ([`Store::entry_at`]).
    pub fn entry_hash(&self, index: u64) -> Result<Option<[u8; 32]>, WriteError> {
        Ok(self.entry_at(index)?.map(|e| e.1))
    }

    /// The committed position the marker records.
    pub fn committed_position(&self) -> u64 {
        self.log.committed()
    }

    /// The term and vote `state` records.
    pub fn saved_term(&self) -> (u64, Option<ServerId>) {
        let v = self.log.state().values();
        (v.term, v.vote.map(|v| v.0))
    }

    /// The durable entry: its index, term and hash.
    pub fn durable_entry(&self) -> (u64, u64, [u8; 32]) {
        let d = self.log.durable();
        (d.index, d.term, d.hash)
    }

    /// The entry before the first one the log holds: what a member can send
    /// from ([`crate::log::Log::base`]).
    pub fn log_base(&self) -> Pos {
        let b = self.log.base();
        Pos {
            index: b.index,
            term: b.term,
        }
    }
}
