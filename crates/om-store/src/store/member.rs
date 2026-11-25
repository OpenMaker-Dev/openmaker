//! A store as a member of a cluster (`docs/server-storage.md` section 7.1):
//! the operations the consensus core ([`crate::consensus`]) asks of its
//! log, its `state` and its page file. Entries reach the log from the
//! leader, or as the leader's own, without being applied; only what is
//! committed is applied and published, in order, as the open's replay
//! applies it (section 6.7, step 4). A member's open writes no entry of
//! its own.

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
}

impl Store {
    /// Creates a store in an empty folder and opens it as a member of a
    /// cluster ([`Store::open_member`]).
    pub fn create_member(
        fs: Arc<dyn Fs>,
        random: &dyn Random,
        config: super::Config,
    ) -> Result<(Store, Opened), OpenError> {
        Store::create_as(fs, random, config, true)
    }

    /// Opens the store as a member of a cluster (section 7.1): the replay
    /// applies as far as the committed position its marker recorded, the
    /// term is the one `state` recorded, and the open writes no entry.
    pub fn open_member(
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
    pub fn recovered(&self) -> Result<Recovered, WriteError> {
        self.usable()?;
        let values = *self.log.state().values();
        let base = self.log.base();
        let mut terms = Vec::new();
        if self.log.durable().index > base.index {
            for e in self.log.read(base.index + 1).map_err(WriteError::Log)? {
                let e = e.map_err(WriteError::Log)?;
                terms.push(e.term);
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
        })
    }

    /// Records the term and the vote in `state`, both slots flushed
    /// (section 7.1: before a vote or an append is answered).
    pub fn save_term(&mut self, term: u64, vote: Option<ServerId>) -> Result<(), WriteError> {
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

    /// Appends the leader's entries to the log's open batch, unapplied,
    /// each checked to land where the leader has it with the hash it has.
    pub fn append_wire(&mut self, entries: &[Wire]) -> Result<(), WriteError> {
        self.usable()?;
        for e in entries {
            match self.log.append(e.term, e.kind, e.file, &e.payload) {
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
    pub fn propose(
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
    pub fn flush_member(&mut self, committed: u64) -> Result<Pos, WriteError> {
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
    pub fn set_committed(&mut self, committed: u64) -> Result<(), WriteError> {
        self.usable()?;
        match self.log.set_committed(committed) {
            Ok(()) => Ok(()),
            Err(e @ LogError::Refused(_)) => Err(WriteError::Log(e)),
            Err(e) => Err(self.stop_log(format!("the committed position: {e}"), &e)),
        }
    }

    /// Cuts the log from `from` (section 7.1), after recording `committed`.
    pub fn cut(&mut self, committed: u64, from: u64) -> Result<(), WriteError> {
        self.set_committed(committed)?;
        match self.log.cut(from) {
            Ok(_) => Ok(()),
            Err(e) if self.log.is_poisoned() => Err(self.stop_log(e.to_string(), &e)),
            Err(e) => Err(WriteError::Log(e)),
        }
    }

    /// Applies the committed entries after the published one, through
    /// `through`, each once, in order, and publishes the last (section
    /// 7.1). `through` is at or before the durable entry. When the pages
    /// waiting for a checkpoint pass their budget, what is applied is
    /// published and checkpointed, and the application goes on.
    pub fn publish(&mut self, through: u64) -> Result<u64, WriteError> {
        self.usable()?;
        let from = self.published.position.index + 1;
        let through = through.min(self.log.durable().index);
        if through < from {
            return Ok(self.published.position.index);
        }
        let entries: Vec<_> = self
            .log
            .read(from)
            .map_err(WriteError::Log)?
            .take_while(|e| e.as_ref().map_or(true, |e| e.index <= through))
            .collect::<Result<_, _>>()
            .map_err(WriteError::Log)?;
        let mut last = self.published.position;
        for e in &entries {
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
        }
        self.publish_at(last)?;
        Ok(self.published.position.index)
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
    /// far as the checkpoints kept allow ([`Store::checkpoint`]). Any
    /// failure stops the store: a member that cannot checkpoint cannot
    /// apply what is committed (section 7.5, repair, is not built).
    pub fn checkpoint_member(&mut self) -> Result<(), WriteError> {
        match self.checkpoint(None) {
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

    /// The durable entries from `from` through `to`, to send to another
    /// member.
    pub fn read_wire(&self, from: u64, to: u64) -> Result<Vec<Wire>, WriteError> {
        self.usable()?;
        if from > to {
            return Ok(Vec::new());
        }
        self.log
            .read(from)
            .map_err(WriteError::Log)?
            .take_while(|e| e.as_ref().map_or(true, |e| e.index <= to))
            .map(|e| {
                e.map(|e| Wire {
                    index: e.index,
                    term: e.term,
                    kind: e.kind,
                    file: e.file,
                    payload: e.payload,
                    hash: e.hash,
                })
                .map_err(WriteError::Log)
            })
            .collect()
    }

    /// The hash of the log's entry at `index`: the base's, or a durable
    /// entry's; `None` when the log holds none there.
    pub fn entry_hash(&self, index: u64) -> Result<Option<[u8; 32]>, WriteError> {
        let base = self.log.base();
        if index == base.index {
            return Ok(Some(base.hash));
        }
        if index < base.index || index > self.log.durable().index {
            return Ok(None);
        }
        let entries = self.read_wire(index, index)?;
        Ok(entries.first().filter(|e| e.index == index).map(|e| e.hash))
    }

    /// The committed position the marker records.
    pub fn committed_position(&self) -> u64 {
        self.log.committed()
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
