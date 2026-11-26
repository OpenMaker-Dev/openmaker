//! The consensus core (`docs/server-storage.md` section 7.1): Raft with the
//! rules that section adds, as a state machine with no I/O of its own.
//!
//! The core is given ticks of a clock, messages, a leader's client entries
//! and the results of its own log operations. It gives back the operations
//! to perform and the messages to send, in the order they must happen
//! ([`Action`]): a term or a vote is recorded before the answer that
//! depends on it, and an entry is flushed before it is acknowledged or
//! counted. [`Member`] runs a member's actions one at a time, and takes
//! the next input only when all of them are done, so a simulator can stop
//! a member between any two (section 12). A member's store is reached only
//! through [`Member`]: nothing outside it can make the store do what the
//! core did not ask.
//!
//! What the core keeps of the log is the term of each entry after the
//! log's base (the entry before the first one the log holds), the durable
//! entry and the committed position, as the member's store reports them
//! when it opens ([`crate::store::Recovered`]).
//!
//! **Trust** (section 7.1, OpenMaker's rule). Members will authenticate
//! each other. The core does not defend against a member that lies within
//! the protocol, as Raft does not. On any decoded message, from anyone,
//! it never panics, never overflows, never allocates by a number the
//! message gives, and never acts outside what it asked for. Before a
//! message is matched to anything, two things of its envelope count: a
//! start count of its sender higher than the last heard makes a leader
//! forget what it held of that member, and a higher term is taken and
//! recorded (Raft's rule). Everything else of a reply counts only against
//! the request it answers, which the reply names by repeating it
//! ([`Echo`]).
//!
//! Not built here, and where it will act:
//! - repair (section 7.5) and the faults that lead to it: an unreadable
//!   end, a passed-over copy of `state` with no whole batch after its
//!   marker (handled as on a single server for now), a lost write the
//!   marker covered;
//! - the members of section 7.6: [`Config::members`] is the fixed set
//!   given when the cluster is made. The latest `members` entry in the log
//!   would replace it in [`Core::members`], which elections, votes and
//!   counting ask;
//! - state transfer to a follower that the leader's log no longer reaches
//!   back for: such a follower is reported in [`Core::behind`] and stays
//!   behind;
//! - sessions and locks in the log (section 7.3), clients (section 7.4)
//!   and checking an operation against the leader's working state before
//!   it is proposed (section 7.2). Until that check exists, proposing is
//!   not offered outside the crate: every member applies what is committed,
//!   and an entry that cannot be applied stops each member that learns it
//!   is committed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::entry::{Payload, ServerId};
use crate::log::MAX_INDEX;
use crate::store::{OpenError, Opened, Recovered, Store, View, WriteError};
use crate::sys::Fs;

/// Whether the simulator planted a defect (always false outside tests).
macro_rules! planted {
    ($core:expr, $plant:ident) => {{
        #[cfg(test)]
        let planted = $core.cfg.plant == Some(Plant::$plant);
        #[cfg(not(test))]
        let planted = false;
        planted
    }};
}

/// The highest term a message may carry, far enough below the integer's
/// end that no addition the core makes on a term can overflow. A message
/// past it, or with an index past [`MAX_INDEX`], is malformed and changes
/// nothing; a member whose term reaches it no longer stands.
pub const MAX_TERM: u64 = 1 << 62;

/// The longest election timeout the settings may give, in ticks.
pub const MAX_TICKS: u64 = 1 << 32;

/// A cluster's settings for one member, the same on every member but
/// `me`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub me: ServerId,
    /// The members, this one included: fixed when the cluster is made
    /// (module docs).
    pub members: Vec<ServerId>,
    /// Ticks without hearing from a leader before a follower asks whether
    /// it would win an election, drawn anew each time between this and
    /// twice this. A follower that heard from its leader within this many
    /// ticks refuses to say it would vote for another; a leader that has
    /// not heard from a majority within this many ticks steps down. From 2
    /// to [`MAX_TICKS`], so that members can draw different timeouts.
    pub election: u64,
    /// Ticks between a leader's appends to a follower that has nothing new.
    /// At least 1 and below `election`.
    pub heartbeat: u64,
    /// The most entries one append carries.
    pub batch: u64,
    /// The most payload bytes one append carries, unless it carries a
    /// single entry.
    pub batch_bytes: u64,
    /// The largest payload of an entry: no member proposes, sends or takes
    /// a larger one. At most the log's own limit on every member
    /// (`log::Config::max_payload`, checked when a member opens).
    pub max_entry: u64,
    /// Seeds the drawn election timeouts. A server takes it from the
    /// operating system's randomness, once at its start; the simulator
    /// from its seed.
    pub seed: u64,
    /// A defect planted for the simulator to find (section 12).
    #[cfg(test)]
    pub plant: Option<Plant>,
}

impl Config {
    /// Refused settings: members missing or repeated, this member not
    /// among them, timings that leave no room to differ or that overflow,
    /// or appends that cannot carry an entry.
    pub fn check(&self) -> Result<(), &'static str> {
        let unique: BTreeSet<&ServerId> = self.members.iter().collect();
        if self.members.is_empty() || unique.len() != self.members.len() {
            return Err("the members are missing or repeated");
        }
        if !unique.contains(&self.me) {
            return Err("this member is not among the members");
        }
        if !(2..=MAX_TICKS).contains(&self.election) {
            return Err("the election timeout must be from 2 ticks to 2^32");
        }
        if self.heartbeat == 0 || self.heartbeat >= self.election {
            return Err("the heartbeat must be at least 1 tick and below the election timeout");
        }
        if self.batch == 0 || self.batch_bytes == 0 || self.max_entry == 0 {
            return Err("an append must be able to carry an entry");
        }
        Ok(())
    }
}

/// The defects the simulator's checks must find (section 12), each the
/// breach of one rule of section 7.1.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Plant {
    /// A vote is answered before the term and vote are recorded.
    VoteUnflushed,
    /// The leader commits an entry of an earlier term by counting copies.
    CountOlderTerm,
    /// A follower records the leader's committed position bounded by its
    /// own last entry, not by the last it checked against the leader's.
    CommitUnchecked,
    /// A follower cuts from the entry before the first that differs: the
    /// entry the leader checked, which can be its committed position.
    CutAtChecked,
    /// A follower publishes what is durable, not what is committed.
    PublishDurable,
    /// A leader counts a success whatever term and start of its own the
    /// success repeats.
    CountStaleSuccess,
    /// A leader counts a success past what it sent the follower.
    CountPastSent,
    /// A follower answers an append without recording the leader's term.
    AppendTermUnsaved,
    /// A vote is granted without the up-to-date check (the pre-vote still
    /// checks).
    VoteNoUpToDate,
    /// Vote and pre-vote compare only the last index of the logs.
    VoteLastIndexOnly,
    /// Vote and pre-vote compare only the last term of the logs.
    VoteLastTermOnly,
    /// The term is recorded with a vote but not the vote: a restart
    /// forgets it.
    VoteForgotten,
    /// A follower acknowledges an append before its flush.
    AckBeforeFlush,
    /// A follower acknowledges its last entry, not the last the append
    /// checked.
    AckLastEntry,
    /// A follower publishes as far as the leader's committed position, not
    /// the one it records (and its store's bound goes too).
    PublishLeaderCommitted,
    /// A leader serves before its `empty` entry is committed and published.
    ServeBeforeEmpty,
}

/// An entry's place: its index and term. Raft compares logs by their last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub index: u64,
    pub term: u64,
}

impl Pos {
    /// Raft's rule: a log ending at `self` is at least as up to date as
    /// one ending at `other` when its last term is higher, or the same and
    /// it is at least as long.
    pub fn at_least(&self, other: &Pos) -> bool {
        (self.term, self.index) >= (other.term, other.index)
    }

    fn bounded(&self) -> bool {
        self.index <= MAX_INDEX && self.term <= MAX_TERM
    }
}

/// An entry as it travels between members, as the log holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub index: u64,
    pub term: u64,
    pub kind: u16,
    pub file: u64,
    pub payload: Vec<u8>,
    pub hash: [u8; 32],
}

/// A message between members. Every message carries its sender's term and
/// start count (section 7.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub from: ServerId,
    pub to: ServerId,
    pub term: u64,
    pub start: u64,
    pub body: Body,
}

/// What a reply repeats of the append it answers: the leader's term and
/// start count as the append gave them, and the entry its consistency
/// check named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Echo {
    pub term: u64,
    pub start: u64,
    pub prev: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Would you vote for me in term `round`, the sender's term plus one,
    /// which it has not taken? Its durable log ends at `last`.
    PreVote {
        round: u64,
        last: Pos,
    },
    PreVoteReply {
        round: u64,
        granted: bool,
    },
    /// Vote for me in this term; my durable log ends at `last`.
    Vote {
        last: Pos,
    },
    VoteReply {
        granted: bool,
    },
    /// The leader's entries after `prev`, and its committed position.
    Append {
        prev: Pos,
        entries: Vec<Wire>,
        committed: u64,
    },
    /// The answer to the append `echo` names. `matched`: the last entry
    /// that append checked, which matches the leader's log and which the
    /// sender's marker covered when it sent this. `None` when the entry at
    /// `echo.prev` did not match: the leader tries again after `hint`.
    AppendReply {
        echo: Echo,
        matched: Option<u64>,
        hint: u64,
    },
}

impl Body {
    /// Whether every term and index the body gives is within bounds.
    fn bounded(&self) -> bool {
        match self {
            Body::PreVote { round, last } => *round <= MAX_TERM && last.bounded(),
            Body::PreVoteReply { round, .. } => *round <= MAX_TERM,
            Body::Vote { last } => last.bounded(),
            Body::VoteReply { .. } => true,
            Body::Append {
                prev,
                entries,
                committed,
            } => {
                prev.bounded()
                    && *committed <= MAX_INDEX
                    && entries
                        .iter()
                        .all(|e| e.index <= MAX_INDEX && e.term <= MAX_TERM)
            }
            Body::AppendReply {
                echo,
                matched,
                hint,
            } => {
                echo.term <= MAX_TERM
                    && echo.prev <= MAX_INDEX
                    && matched.is_none_or(|m| m <= MAX_INDEX)
                    && *hint <= MAX_INDEX
            }
        }
    }
}

/// What the core asks of its member, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Record the term and the vote in `state`, both copies flushed.
    SaveTerm {
        term: u64,
        vote: Option<ServerId>,
    },
    /// Record `committed`, which confirms a committed position for this
    /// start, then cut the log from `from`.
    Cut {
        committed: u64,
        from: u64,
    },
    /// Append the leader's entries.
    Append(Vec<Wire>),
    /// Append the leader's own entry in `term`.
    Propose {
        term: u64,
        file: u64,
        payload: Payload,
    },
    /// Flush the open batch, recording `committed` in the marker; then
    /// report the durable entry ([`Core::flushed`]).
    Flush {
        committed: u64,
    },
    /// Record the committed position `committed` alone: the leader's.
    Committed(u64),
    /// Apply and publish the committed entries through this one; then
    /// report how far ([`Core::published_to`]).
    Publish(u64),
    Send(Message),
    /// Send `to` the durable entries from `first`, at most through `last`
    /// and within `batch_bytes`, none past `max_entry`, after `prev`, as
    /// [`Body::Append`] (none when `last < first`: a heartbeat); then
    /// report the last entry sent ([`Core::sent`]).
    SendAppend {
        to: ServerId,
        term: u64,
        start: u64,
        prev: Pos,
        first: u64,
        last: u64,
        committed: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Follower,
    /// Asking whether it would win an election (section 7.1), its term
    /// unchanged.
    PreCandidate,
    Candidate,
    Leader,
}

/// What a leader knows of a follower, in its term and since the start of
/// the follower it last heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    /// The next entry to send.
    next: u64,
    /// The last entry the follower acknowledged.
    matched: u64,
    /// The last entry any append sent to it checked: a success counts no
    /// further.
    sent: u64,
    /// The entry the last append sent to it named: a refusal counts only
    /// when it answers that one.
    probe: Option<u64>,
    /// Whether it is being probed: after a refusal, or before the leader
    /// heard a success from it, one append at a time without moving `next`
    /// on, each heartbeat repeating it, so that the append out now is
    /// always the one naming `next - 1`. A success ends it, and appends
    /// follow one another again.
    probing: bool,
    /// The leader's tick when the follower last answered: a new leader
    /// takes every follower as heard when it begins.
    heard_at: u64,
}

impl Progress {
    fn new(next: u64, now: u64) -> Progress {
        Progress {
            next,
            matched: 0,
            sent: 0,
            probe: None,
            probing: true,
            heard_at: now,
        }
    }
}

/// A follower's acknowledgement waiting for its flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    leader: ServerId,
    /// The leader's committed position the append gave.
    given: u64,
    /// Whether the answer went before the flush (a planted defect).
    answered: bool,
    /// The follower's term when it took the append.
    term: u64,
    echo: Echo,
    /// The last entry checked against the leader's log.
    checked: u64,
}

/// The consensus core of one member (module docs).
#[derive(Clone, Debug)]
pub struct Core {
    cfg: Config,
    start: u64,
    term: u64,
    vote: Option<ServerId>,
    /// The term and vote `state` holds, as far as the actions given say.
    saved: (u64, Option<ServerId>),
    base: Pos,
    terms: Vec<u64>,
    durable: u64,
    committed: u64,
    published: u64,
    role: Role,
    leader: Option<ServerId>,
    elapsed: u64,
    timeout: u64,
    beat: u64,
    /// The leader's ticks since it began to lead.
    ticks: u64,
    grants: BTreeSet<ServerId>,
    progress: BTreeMap<ServerId, Progress>,
    /// The highest start count heard from each member.
    starts: BTreeMap<ServerId, u64>,
    /// The leader's `empty` entry of its term.
    empty: u64,
    pending: Option<Pending>,
    behind: BTreeSet<ServerId>,
    rng: u64,
}

impl Core {
    /// The core of a member that opened with `rec`; refused when the
    /// settings are ([`Config::check`]).
    pub fn new(cfg: Config, rec: &Recovered) -> Result<Core, &'static str> {
        cfg.check()?;
        let held = u64::try_from(rec.terms.len()).map_err(|_| "the log is too long")?;
        let durable = rec
            .base
            .index
            .checked_add(held)
            .ok_or("the log is too long")?;
        // The whole id, the seed and the start count, mixed.
        let (a, b) = cfg.me.split_at(8);
        let id = u64::from_le_bytes(a.try_into().expect("8 bytes"))
            ^ mix(u64::from_le_bytes(b.try_into().expect("8 bytes")));
        let mut rng = mix(cfg.seed ^ mix(id ^ mix(rec.start_count)));
        let timeout = draw(&mut rng, cfg.election);
        Ok(Core {
            start: rec.start_count,
            term: rec.term,
            vote: rec.vote,
            saved: (rec.term, rec.vote),
            base: rec.base,
            terms: rec.terms.clone(),
            durable,
            committed: rec.committed,
            published: rec.published,
            role: Role::Follower,
            leader: None,
            elapsed: 0,
            timeout,
            beat: 0,
            ticks: 0,
            grants: BTreeSet::new(),
            progress: BTreeMap::new(),
            starts: BTreeMap::new(),
            empty: 0,
            pending: None,
            behind: BTreeSet::new(),
            rng,
            cfg,
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn term(&self) -> u64 {
        self.term
    }

    pub fn vote(&self) -> Option<ServerId> {
        self.vote
    }

    pub fn leader(&self) -> Option<ServerId> {
        self.leader
    }

    pub fn me(&self) -> ServerId {
        self.cfg.me
    }

    /// This start's count, which every message carries.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The last entry of the log, appended, durable or not.
    pub fn last(&self) -> Pos {
        Pos {
            index: self.base.index + self.terms.len() as u64,
            term: self.terms.last().copied().unwrap_or(self.base.term),
        }
    }

    /// The durable entry.
    pub fn durable(&self) -> Pos {
        Pos {
            index: self.durable,
            term: self.term_at(self.durable).unwrap_or(0),
        }
    }

    /// The entry before the first one the log holds.
    pub fn base(&self) -> Pos {
        self.base
    }

    /// The last entry known to be committed.
    pub fn committed(&self) -> u64 {
        self.committed
    }

    pub fn published(&self) -> u64 {
        self.published
    }

    /// The members elections and counting ask about. Section 7.6's
    /// `members` entries will set them; they are fixed now (module docs).
    pub fn members(&self) -> &[ServerId] {
        &self.cfg.members
    }

    /// Whether this member leads and may answer clients: its `empty`
    /// entry committed and published (section 7.1).
    pub fn serving(&self) -> bool {
        if planted!(self, ServeBeforeEmpty) {
            return self.role == Role::Leader;
        }
        self.role == Role::Leader && self.committed >= self.empty && self.published >= self.empty
    }

    /// The followers the leader's log no longer reaches back for: a probe
    /// at its base was refused, or what they need is an entry past the
    /// settings' largest. They need a state transfer, which is not built,
    /// and stay behind.
    pub fn behind(&self) -> &BTreeSet<ServerId> {
        &self.behind
    }

    /// The term of the entry at `index`, when the log knows it.
    pub fn term_at(&self, index: u64) -> Option<u64> {
        if index == self.base.index {
            return Some(self.base.term);
        }
        let k = index.checked_sub(self.base.index)?.checked_sub(1)?;
        self.terms.get(usize::try_from(k).ok()?).copied()
    }

    /// Raft's up-to-date rule against this member's durable log, and never
    /// a log that ends before its committed position.
    fn up_to_date(&self, last: &Pos) -> bool {
        let durable = self.durable();
        let ok = if planted!(self, VoteLastIndexOnly) {
            last.index >= durable.index
        } else if planted!(self, VoteLastTermOnly) {
            last.term >= durable.term
        } else {
            last.at_least(&durable)
        };
        ok && last.index >= self.committed
    }

    fn majority(&self) -> usize {
        self.members().len() / 2 + 1
    }

    fn others(&self) -> Vec<ServerId> {
        self.members()
            .iter()
            .copied()
            .filter(|m| *m != self.cfg.me)
            .collect()
    }

    fn send(&self, out: &mut Vec<Action>, to: ServerId, body: Body) {
        out.push(Action::Send(Message {
            from: self.cfg.me,
            to,
            term: self.term,
            start: self.start,
            body,
        }));
    }

    /// The term and vote recorded, before what follows (section 7.1).
    fn persist(&mut self, out: &mut Vec<Action>) {
        if self.saved != (self.term, self.vote) {
            self.saved = (self.term, self.vote);
            out.push(Action::SaveTerm {
                term: self.term,
                vote: self.vote,
            });
        }
    }

    /// A higher term seen: follow, with no vote in it yet.
    fn see_term(&mut self, term: u64) {
        if term > self.term {
            self.term = term;
            self.vote = None;
            self.follow(None);
        }
    }

    fn follow(&mut self, leader: Option<ServerId>) {
        self.role = Role::Follower;
        self.leader = leader;
        self.grants.clear();
        self.progress.clear();
        self.behind.clear();
    }

    /// A tick of the clock.
    pub fn tick(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        if self.role == Role::Leader {
            self.beat = self.beat.saturating_add(1);
            self.ticks = self.ticks.saturating_add(1);
            // A leader that has not heard from a majority within an
            // election timeout steps down.
            let (now, election) = (self.ticks, self.cfg.election);
            let heard = self
                .progress
                .values()
                .filter(|p| now - p.heard_at < election)
                .count()
                + 1;
            if heard < self.majority() {
                self.follow(None);
                self.elapsed = 0;
                return out;
            }
            if self.beat >= self.cfg.heartbeat {
                self.beat = 0;
                for to in self.others() {
                    self.replicate(&mut out, to, true);
                }
            }
            return out;
        }
        self.elapsed = self.elapsed.saturating_add(1);
        if self.elapsed >= self.timeout {
            self.elapsed = 0;
            self.timeout = draw(&mut self.rng, self.cfg.election);
            self.pre_vote(&mut out);
        }
        out
    }

    /// Asks the others whether they would vote for it (section 7.1), its
    /// term unchanged. It does not stand when its log ends before its
    /// committed position, or at the highest term.
    fn pre_vote(&mut self, out: &mut Vec<Action>) {
        if self.durable < self.committed || self.term >= MAX_TERM {
            return;
        }
        let round = self.term + 1;
        self.role = Role::PreCandidate;
        self.leader = None;
        self.grants = BTreeSet::from([self.cfg.me]);
        if self.grants.len() >= self.majority() {
            self.stand(out);
            return;
        }
        let last = self.durable();
        for to in self.others() {
            self.send(out, to, Body::PreVote { round, last });
        }
    }

    /// Stands for election in the next term, its vote for itself recorded
    /// first.
    fn stand(&mut self, out: &mut Vec<Action>) {
        if self.term >= MAX_TERM {
            return;
        }
        self.term += 1;
        self.vote = Some(self.cfg.me);
        self.role = Role::Candidate;
        self.leader = None;
        self.grants = BTreeSet::from([self.cfg.me]);
        self.persist(out);
        if self.grants.len() >= self.majority() {
            self.lead(out);
            return;
        }
        let last = self.durable();
        for to in self.others() {
            self.send(out, to, Body::Vote { last });
        }
    }

    /// Leads its term: its first entry is `empty` (section 7.1).
    fn lead(&mut self, out: &mut Vec<Action>) {
        self.role = Role::Leader;
        self.leader = Some(self.cfg.me);
        self.beat = 0;
        self.ticks = 0;
        let next = self.last().index + 1;
        self.progress = self
            .others()
            .into_iter()
            .map(|m| (m, Progress::new(next, 0)))
            .collect();
        self.behind.clear();
        self.terms.push(self.term);
        self.empty = self.last().index;
        out.push(Action::Propose {
            term: self.term,
            file: 0,
            payload: Payload::Empty,
        });
        out.push(Action::Flush {
            committed: self.committed,
        });
    }

    /// A client's entry at the leader, appended, flushed, then sent; `None`
    /// unless this member leads and serves (section 7.1). Not offered
    /// outside the crate until the leader checks an operation against its
    /// working state (section 7.2; module docs).
    // Only the simulator proposes until then.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn propose(&mut self, file: u64, payload: Payload) -> Option<Vec<Action>> {
        if !self.serving() {
            return None;
        }
        self.terms.push(self.term);
        Some(vec![
            Action::Propose {
                term: self.term,
                file,
                payload,
            },
            Action::Flush {
                committed: self.committed,
            },
        ])
    }

    /// The flush the core asked for is done: `durable` is the durable entry.
    pub fn flushed(&mut self, durable: Pos) -> Vec<Action> {
        let mut out = Vec::new();
        self.durable = durable.index;
        if self.role == Role::Leader {
            self.advance(&mut out);
            for to in self.others() {
                self.replicate(&mut out, to, false);
            }
        }
        if let Some(p) = self.pending.take() {
            if p.term == self.term && self.leader == Some(p.leader) && !p.answered {
                let matched = if planted!(self, AckLastEntry) {
                    self.last().index
                } else {
                    p.checked
                };
                let body = Body::AppendReply {
                    echo: p.echo,
                    matched: Some(matched.min(self.durable)),
                    hint: self.last().index,
                };
                self.send(&mut out, p.leader, body);
            }
            let through = if planted!(self, PublishDurable) {
                self.durable
            } else if planted!(self, PublishLeaderCommitted) {
                p.given.min(self.durable)
            } else {
                self.committed
            };
            out.push(Action::Publish(through));
        }
        out
    }

    /// The publish the core asked for is done, through `published`.
    pub fn published_to(&mut self, published: u64) {
        self.published = self.published.max(published);
    }

    /// The append to `to` that named `prev` went with the entries through
    /// `last`: as many as fit in an append's bytes. When `last` is `prev`
    /// although it was to carry entries, the next one is too large to
    /// send ([`Config::max_entry`]), and the follower is behind.
    pub fn sent(&mut self, to: ServerId, prev: u64, last: u64) {
        let Some(p) = self.progress.get_mut(&to) else {
            return;
        };
        if p.probe != Some(prev) || last < prev {
            return;
        }
        p.sent = p.sent.max(last);
        if p.next > last + 1 {
            p.next = last + 1;
        }
    }

    /// The next entry `to` needs is past the settings' largest: it can be
    /// had only by a state transfer.
    pub fn oversized(&mut self, to: ServerId) {
        if self.progress.contains_key(&to) {
            self.behind.insert(to);
        }
    }

    /// The log retired its entries through `base` (a checkpoint).
    pub fn compacted(&mut self, base: Pos) {
        if base.index <= self.base.index || base.index > self.durable {
            return;
        }
        let Ok(drop) = usize::try_from(base.index - self.base.index) else {
            return;
        };
        self.terms.drain(..drop.min(self.terms.len()));
        self.base = base;
    }

    /// The leader sends `to` what it lacks, or with `heartbeat` an append
    /// that may carry nothing. A follower whose next entry the log no
    /// longer holds is probed at the log's base first.
    fn replicate(&mut self, out: &mut Vec<Action>, to: ServerId, heartbeat: bool) {
        let base = self.base;
        let (durable, batch, term, start, committed) = (
            self.durable,
            self.cfg.batch,
            self.term,
            self.start,
            self.committed,
        );
        let Some(p) = self.progress.get(&to) else {
            return;
        };
        let next = p.next.max(base.index + 1);
        let behind = self.behind.contains(&to);
        if behind && !heartbeat {
            return;
        }
        let prev_index = next - 1;
        let Some(prev_term) = self.term_at(prev_index) else {
            return;
        };
        let last = if behind {
            prev_index
        } else {
            durable.min(next.saturating_add(batch) - 1)
        };
        if last < next && !heartbeat {
            return;
        }
        let p = self.progress.get_mut(&to).expect("looked up above");
        p.next = next;
        p.probe = Some(prev_index);
        // A probe checks its `prev`; entries count once they are sent.
        p.sent = p.sent.max(prev_index);
        if last >= next && !p.probing {
            p.next = last + 1;
        }
        out.push(Action::SendAppend {
            to,
            term,
            start,
            prev: Pos {
                index: prev_index,
                term: prev_term,
            },
            first: next,
            last,
            committed,
        });
    }

    /// The leader commits the last entry of its own term that a majority
    /// holds durable, its own copy counting as one (section 7.1).
    fn advance(&mut self, out: &mut Vec<Action>) {
        let mut held: Vec<u64> = self.progress.values().map(|p| p.matched).collect();
        held.push(self.durable);
        held.sort_unstable_by(|a, b| b.cmp(a));
        let n = held[self.majority() - 1];
        let own_term = self.term_at(n) == Some(self.term);
        if n > self.committed && (own_term || planted!(self, CountOlderTerm)) {
            self.committed = n;
            out.push(Action::Committed(n));
            out.push(Action::Publish(n));
        }
    }

    /// A message from another member.
    pub fn receive(&mut self, m: Message) -> Vec<Action> {
        let mut out = Vec::new();
        if m.to != self.cfg.me || m.from == self.cfg.me || !self.members().contains(&m.from) {
            return out;
        }
        // A term or an index past its bound: malformed, nothing changes.
        if m.term > MAX_TERM || !m.body.bounded() {
            return out;
        }
        // Every member passes over a message of an earlier start of its
        // sender. A later one makes the leader forget what it held of that
        // member, whatever else the message is; the first count heard is
        // not a rise, nothing having been held before (section 7.1).
        let seen = self.starts.entry(m.from).or_insert(0);
        if m.start < *seen {
            return out;
        }
        if m.start > *seen {
            let rose = *seen > 0;
            *seen = m.start;
            let next = self.last().index + 1;
            let now = self.ticks;
            if let Some(p) = self.progress.get_mut(&m.from).filter(|_| rose) {
                let heard_at = p.heard_at;
                *p = Progress::new(next, now);
                p.heard_at = heard_at;
            }
        }
        // Raft's rule, for every message: a higher term is taken and
        // recorded before anything else. A vote request's is recorded with
        // the vote, in one write, before the answer.
        if m.term > self.term {
            self.see_term(m.term);
            let unsaved =
                planted!(self, AppendTermUnsaved) && matches!(m.body, Body::Append { .. });
            if !matches!(m.body, Body::Vote { .. }) && !unsaved {
                self.persist(&mut out);
            }
        }
        match m.body {
            Body::PreVote { round, last } => {
                // Granted with no change of term or vote: not for a term it
                // has reached, not while it hears from a leader, and by the
                // rules of a vote.
                let hears = self.role == Role::Leader
                    || (self.leader.is_some() && self.elapsed < self.cfg.election);
                let granted = round > self.term && !hears && self.up_to_date(&last);
                self.send(&mut out, m.from, Body::PreVoteReply { round, granted });
            }
            Body::PreVoteReply { round, granted } => {
                // Only an answer to the round it asks.
                let asked = self.role == Role::PreCandidate && self.term + 1 == round;
                if asked && granted {
                    self.grants.insert(m.from);
                    if self.grants.len() >= self.majority() {
                        self.stand(&mut out);
                    }
                }
            }
            Body::Vote { last } => {
                let checked = planted!(self, VoteNoUpToDate) || self.up_to_date(&last);
                let granted = m.term == self.term
                    && matches!(self.role, Role::Follower | Role::PreCandidate)
                    && self.vote.is_none_or(|v| v == m.from)
                    && checked;
                if granted {
                    self.vote = Some(m.from);
                    self.elapsed = 0;
                    self.follow(None);
                }
                // The term and vote recorded before the answer; planted,
                // they stay in memory until something else records them, or
                // the vote is left out of the record.
                if planted!(self, VoteForgotten) && self.saved != (self.term, self.vote) {
                    self.saved = (self.term, self.vote);
                    out.push(Action::SaveTerm {
                        term: self.term,
                        vote: None,
                    });
                } else if !planted!(self, VoteUnflushed) {
                    self.persist(&mut out);
                }
                self.send(&mut out, m.from, Body::VoteReply { granted });
            }
            Body::VoteReply { granted } => {
                // Only an answer to the election it stands in.
                if self.role == Role::Candidate && m.term == self.term && granted {
                    self.grants.insert(m.from);
                    if self.grants.len() >= self.majority() {
                        self.lead(&mut out);
                    }
                }
            }
            Body::Append {
                prev,
                entries,
                committed,
            } => {
                let echo = Echo {
                    term: m.term,
                    start: m.start,
                    prev: prev.index,
                };
                self.on_append(&mut out, m.from, echo, prev, entries, committed);
            }
            Body::AppendReply {
                echo,
                matched,
                hint,
            } => self.on_reply(&mut out, m.from, echo, matched, hint),
        }
        out
    }

    /// A leader takes a follower's answer to the append it repeats: one of
    /// its own in this term and this start, a success no further than it
    /// sent and holds, a refusal only of the append it has out now.
    fn on_reply(
        &mut self,
        out: &mut Vec<Action>,
        from: ServerId,
        echo: Echo,
        matched: Option<u64>,
        hint: u64,
    ) {
        if self.role != Role::Leader {
            return;
        }
        let ours = echo.term == self.term && echo.start == self.start;
        if !ours && !(matched.is_some() && planted!(self, CountStaleSuccess)) {
            return;
        }
        let base = self.base.index;
        let durable = self.durable;
        let now = self.ticks;
        let past_sent = planted!(self, CountPastSent);
        let Some(p) = self.progress.get_mut(&from) else {
            return;
        };
        match matched {
            Some(k) => {
                if k < echo.prev {
                    return;
                }
                let bound = if past_sent {
                    durable
                } else {
                    p.sent.min(durable)
                };
                let k = k.min(bound);
                p.heard_at = now;
                p.probing = false;
                p.matched = p.matched.max(k);
                p.next = p.next.max(k + 1);
                self.behind.remove(&from);
                self.advance(out);
            }
            None => {
                if p.probe != Some(echo.prev) {
                    // Not the append out now: the next heartbeat asks again.
                    return;
                }
                p.heard_at = now;
                p.probe = None;
                p.probing = true;
                // Before the entry the append named, never before what the
                // follower acknowledged.
                let before = hint.min(echo.prev.saturating_sub(1));
                p.next = before.max(p.matched) + 1;
                if echo.prev == base && hint < base {
                    // A probe at the base, as it is now, refused: what it
                    // needs is gone.
                    self.behind.insert(from);
                }
            }
        }
        self.replicate(out, from, false);
    }

    /// A follower takes the leader's entries (section 7.1).
    fn on_append(
        &mut self,
        out: &mut Vec<Action>,
        from: ServerId,
        echo: Echo,
        prev: Pos,
        entries: Vec<Wire>,
        committed: u64,
    ) {
        let term = echo.term;
        // The append is checked whole before anything but the term
        // changes: no more entries or bytes than an append carries, none
        // past the largest, each following `prev` one by one, terms that
        // never go down and never pass the leader's.
        let count = entries.len() as u64;
        let bytes: u64 = entries.iter().map(|e| e.payload.len() as u64).sum();
        let bounded = count <= self.cfg.batch
            && (count <= 1 || bytes <= self.cfg.batch_bytes)
            && entries
                .iter()
                .all(|e| e.payload.len() as u64 <= self.cfg.max_entry);
        let follows = entries
            .iter()
            .enumerate()
            .all(|(k, e)| Some(e.index) == prev.index.checked_add(k as u64 + 1) && e.term <= term);
        let rising = entries
            .iter()
            .try_fold(prev.term, |t, e| (e.term >= t).then_some(e.term))
            .is_some();
        if !bounded || !follows || !rising || prev.term > term {
            return;
        }
        if term < self.term {
            let hint = self.last().index;
            let body = Body::AppendReply {
                echo,
                matched: None,
                hint,
            };
            self.send(out, from, body);
            return;
        }
        if self.role == Role::Leader {
            // Two leaders in one term: the simulator's check reports it.
            return;
        }
        if self.role != Role::Follower || self.leader != Some(from) {
            self.follow(Some(from));
        }
        self.elapsed = 0;
        // Its term recorded before it answers, however it came by it.
        if !planted!(self, AppendTermUnsaved) {
            self.persist(out);
        }
        let last = self.last();
        if prev.index > last.index {
            let body = Body::AppendReply {
                echo,
                matched: None,
                hint: last.index,
            };
            self.send(out, from, body);
            return;
        }
        if prev.index >= self.base.index {
            if let Some(t) = self.term_at(prev.index).filter(|t| *t != prev.term) {
                // Where to try next: before this log's run of that term,
                // never before what it knows is committed.
                let mut first = prev.index;
                while first > self.base.index + 1 && self.term_at(first - 1) == Some(t) {
                    first -= 1;
                }
                let hint = first
                    .saturating_sub(1)
                    .max(self.committed)
                    .max(self.base.index)
                    .min(prev.index.saturating_sub(1));
                let body = Body::AppendReply {
                    echo,
                    matched: None,
                    hint,
                };
                self.send(out, from, body);
                return;
            }
        }
        // The entries at or before the base are committed, and so the
        // leader's too. The first that differs from this log's is cut from.
        let mut cut = None;
        let mut new = Vec::new();
        let mut checked = prev.index;
        for e in entries {
            if e.index <= self.base.index {
                continue;
            }
            checked = checked.max(e.index);
            if cut.is_none() && new.is_empty() {
                match self.term_at(e.index) {
                    Some(t) if t == e.term => continue,
                    Some(_) => cut = Some(e.index),
                    None => {}
                }
            }
            new.push(e);
        }
        if let Some(c) = cut {
            // Every entry before `c` matches the leader's: the committed
            // position given first, which confirms one for this start, is
            // the leader's bounded by them (section 7.1).
            let from = if planted!(self, CutAtChecked) {
                (c - 1).max(self.base.index + 1)
            } else {
                c
            };
            let record = committed.min(c - 1);
            out.push(Action::Cut {
                committed: record,
                from,
            });
            self.committed = self.committed.max(record);
            let keep = usize::try_from(from - self.base.index - 1).unwrap_or(usize::MAX);
            self.terms.truncate(keep);
            self.durable = self.durable.min(from - 1);
        }
        if !new.is_empty() {
            self.terms.extend(new.iter().map(|e| e.term));
            out.push(Action::Append(new));
        }
        // The committed position recorded: the leader's, never past the
        // last entry checked against the leader's log (section 6.3).
        let bound = if planted!(self, CommitUnchecked) {
            self.last().index
        } else {
            checked
        };
        let record = committed.min(bound);
        self.committed = self.committed.max(record);
        let answered = planted!(self, AckBeforeFlush);
        if answered {
            let body = Body::AppendReply {
                echo,
                matched: Some(checked),
                hint: self.last().index,
            };
            self.send(out, from, body);
        }
        out.push(Action::Flush { committed: record });
        self.pending = Some(Pending {
            leader: from,
            given: committed,
            answered,
            term: self.term,
            echo,
            checked,
        });
    }
}

/// SplitMix64's finalizer: every bit of the input moves every bit out.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A timeout drawn between `base` and twice it (`base` at most
/// [`MAX_TICKS`]).
fn draw(rng: &mut u64, base: u64) -> u64 {
    *rng = mix(*rng);
    base + *rng % base.max(1)
}

/// What one of [`Member::perform`]'s actions did.
#[derive(Debug, Default)]
pub struct Performed {
    /// The message to send.
    pub sent: Option<Message>,
    /// The entry a leader's [`Action::Propose`] appended.
    pub proposed: Option<Wire>,
    /// The committed position a leader recorded ([`Action::Committed`]).
    pub committed: Option<u64>,
    /// How far an [`Action::Publish`] published.
    pub published: Option<u64>,
}

/// Why a member did not open.
#[derive(Debug)]
pub enum MemberError {
    /// The store did not open ([`Store::open`]'s reasons).
    Open(OpenError),
    /// The store opened but could not say what the core starts from.
    Store(WriteError),
    /// The settings are refused ([`Config::check`]), are another member's,
    /// or allow an entry larger than the store's log takes.
    Config(&'static str),
}

impl std::fmt::Display for MemberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemberError::Open(e) => write!(f, "the store did not open: {e}"),
            MemberError::Store(e) => write!(f, "the store: {e}"),
            MemberError::Config(why) => write!(f, "the settings: {why}"),
        }
    }
}

/// A member: its store and its core, and the actions waiting to be done,
/// one at a time (module docs). The store is reached only through it.
pub struct Member {
    store: Store,
    core: Core,
    queue: VecDeque<Action>,
    /// Why it stopped: after any error it takes nothing more, and is
    /// opened again.
    stopped: Option<String>,
}

impl Member {
    /// Opens the member whose store is in `fs`, made with
    /// [`Store::create_member`] (section 7.1).
    pub fn open(
        fs: Arc<dyn Fs>,
        config: crate::store::Config,
        cfg: Config,
    ) -> Result<(Member, Opened), MemberError> {
        cfg.check().map_err(MemberError::Config)?;
        if cfg.max_entry > u64::from(config.log.max_payload) {
            return Err(MemberError::Config(
                "the largest entry is past what the store's log takes",
            ));
        }
        let (store, opened) = Store::open_member(fs, config).map_err(MemberError::Open)?;
        if store.server() != cfg.me {
            return Err(MemberError::Config("the store is another member's"));
        }
        let rec = store.recovered().map_err(MemberError::Store)?;
        let core = Core::new(cfg, &rec).map_err(MemberError::Config)?;
        Ok((
            Member {
                store,
                core,
                queue: VecDeque::new(),
                stopped: None,
            },
            opened,
        ))
    }

    /// A view of the published state (section 6.6).
    pub fn view(&self) -> Result<View, WriteError> {
        self.store.view()
    }

    /// The entry the published state is of.
    pub fn published(&self) -> crate::apply::Position {
        self.store.published()
    }

    /// The member's core, to read: role, term, positions.
    pub fn core(&self) -> &Core {
        &self.core
    }

    /// Why the member stopped, if it did: it takes nothing more until it
    /// is opened again.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// Whether every action is done, so that it takes the next input.
    pub fn idle(&self) -> bool {
        self.queue.is_empty()
    }

    /// The actions waiting.
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    fn ready(&self) -> bool {
        self.stopped.is_none() && self.queue.is_empty()
    }

    /// A tick of the clock, taken when idle and not stopped.
    pub fn tick(&mut self) -> bool {
        if !self.ready() {
            return false;
        }
        let actions = self.core.tick();
        self.queue.extend(actions);
        true
    }

    /// A message, taken when idle and not stopped.
    pub fn receive(&mut self, m: Message) -> bool {
        if !self.ready() {
            return false;
        }
        let actions = self.core.receive(m);
        self.queue.extend(actions);
        true
    }

    /// A client's entry, taken when idle, not stopped, serving and no
    /// larger than the settings' largest entry. Not offered outside the
    /// crate (module docs).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn propose(&mut self, file: u64, payload: Payload) -> bool {
        if !self.ready() {
            return false;
        }
        match payload.encode() {
            Ok(bytes) if bytes.len() as u64 <= self.core.cfg.max_entry => {}
            _ => return false,
        }
        match self.core.propose(file, payload) {
            Some(actions) => {
                self.queue.extend(actions);
                true
            }
            None => false,
        }
    }

    /// Takes a checkpoint of the published state when idle, which retires
    /// the log as far as the checkpoints kept allow. An error stops the
    /// member.
    pub fn checkpoint(&mut self) -> Result<bool, WriteError> {
        if let Some(why) = &self.stopped {
            return Err(WriteError::Stopped(why.clone()));
        }
        if !self.idle() {
            return Ok(false);
        }
        match self.store.checkpoint_member() {
            Ok(()) => {
                self.core.compacted(self.store.log_base());
                Ok(true)
            }
            Err(e) => Err(self.stop(e)),
        }
    }

    /// The member stops for `e`: its queue is dropped and it takes nothing
    /// more until it is opened again.
    fn stop(&mut self, e: WriteError) -> WriteError {
        self.queue.clear();
        self.stopped = Some(e.to_string());
        e
    }

    /// Does the next action waiting. An error stops the member: its
    /// store stopped, or the action was refused, which a correct core
    /// never asks; either way what was queued after it is dropped.
    pub fn perform(&mut self) -> Result<Option<Performed>, WriteError> {
        if let Some(why) = &self.stopped {
            return Err(WriteError::Stopped(why.clone()));
        }
        let Some(action) = self.queue.pop_front() else {
            return Ok(None);
        };
        match self.run(action) {
            Ok(done) => Ok(Some(done)),
            Err(e) => Err(self.stop(e)),
        }
    }

    fn run(&mut self, action: Action) -> Result<Performed, WriteError> {
        let mut done = Performed::default();
        match action {
            Action::SaveTerm { term, vote } => self.store.save_term(term, vote)?,
            Action::Cut { committed, from } => self.store.cut(committed, from)?,
            Action::Append(entries) => self.store.append_wire(&entries)?,
            Action::Propose {
                term,
                file,
                payload,
            } => {
                let wire = self.store.propose(term, file, &payload)?;
                if self.core.term_at(wire.index) != Some(wire.term) {
                    return Err(WriteError::Rule(
                        "the log placed the leader's entry elsewhere than its core",
                    ));
                }
                done.proposed = Some(wire);
            }
            Action::Flush { committed } => {
                let durable = self.store.flush_member(committed)?;
                if durable != self.core.last() {
                    return Err(WriteError::Rule(
                        "the flush made another entry durable than its core appended",
                    ));
                }
                let next = self.core.flushed(durable);
                self.queue.extend(next);
            }
            Action::Committed(c) => {
                self.store.set_committed(c)?;
                done.committed = Some(c);
            }
            Action::Publish(through) => {
                // Planted, the store's own bound goes too.
                let lifted = planted!(self.core, PublishDurable)
                    || planted!(self.core, PublishLeaderCommitted);
                let published = if lifted {
                    self.store.publish_within(through, u64::MAX)?
                } else {
                    self.store.publish(through)?
                };
                self.core.published_to(published);
                self.core.compacted(self.store.log_base());
                done.published = Some(published);
            }
            Action::Send(m) => done.sent = Some(m),
            Action::SendAppend {
                to,
                term,
                start,
                prev,
                first,
                last,
                committed,
            } => {
                if first <= self.store.log_base().index && first <= last {
                    // A checkpoint queued before it retired what it was to
                    // carry: nothing goes, and the follower is probed at
                    // the base next time.
                    self.core.sent(to, prev.index, prev.index);
                    return Ok(done);
                }
                let max_entry = self.core.cfg.max_entry;
                let entries =
                    self.store
                        .read_wire(first, last, self.core.cfg.batch_bytes, max_entry)?;
                if entries.is_empty() && first <= last {
                    // An entry written under a larger setting: it cannot be
                    // sent, and the follower needs a state transfer.
                    self.core.oversized(to);
                    return Ok(done);
                }
                let sent = entries.last().map_or(prev.index, |e| e.index);
                self.core.sent(to, prev.index, sent);
                done.sent = Some(Message {
                    from: self.core.me(),
                    to,
                    term,
                    start,
                    body: Body::Append {
                        prev,
                        entries,
                        committed,
                    },
                });
            }
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests;
