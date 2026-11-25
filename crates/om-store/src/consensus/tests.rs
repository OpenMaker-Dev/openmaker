//! The cluster simulator (`docs/server-storage.md` section 12): 3 and 5
//! members, each a store on its own simulated disk, a simulated network
//! and a simulated clock, all in one thread from one seed, so that a
//! failing seed replays exactly.
//!
//! A schedule draws, step by step, one of: a member's next action
//! ([`Member::perform`]), the delivery of a message that is due, a tick of
//! the clock, a client's entry at the serving leader, a checkpoint, a view
//! taken and held, and a fault. The network delays, drops, repeats and
//! reorders messages, and splits the members in two for a while. The
//! faults of this part are power losses between steps and in the middle
//! of a disk call, a process that stops with the cache kept, and failed
//! writes and flushes: faults after which a disk still holds what it
//! flushed, as Raft assumes. Faults that break that assumption, and the
//! repair they lead to (section 7.5), are not drawn.
//!
//! **What is committed** is the simulator's own record, taken from the
//! disks, not from any member's core: an entry of term `t` is committed
//! once a majority of the members have held it durable while `state`
//! recorded term `t` on each (Raft's acknowledgement in the entry's own
//! term), and so is every entry before it. After every step it checks
//! ([`Check`]):
//! - at most one leader per term;
//! - a leader decides an entry is committed only once the disks show it;
//! - a leader is elected only with every committed entry in its log;
//! - a leader never loses an entry of its own log while it leads;
//! - two logs that hold an entry of the same index and term hold the same
//!   entry, and every committed entry is the same wherever it is read;
//! - the committed position a member records is a committed entry;
//! - a member applies, and publishes, only committed entries, and every
//!   member's state after the same entry has the same root hashes;
//! - a view shows only what is committed, and goes on reading the same;
//! - every member starts from what the faults left, and every error a
//!   member's store returns follows a fault its disk injected during that
//!   very call: an operation the store refuses otherwise is a defect.
//!
//! At the end the faults stop and the network heals: every member must
//! start, one leader must serve, and every member must hold and publish
//! everything committed, or be one the leader reports as behind
//! ([`Core::behind`], state transfer not built).
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds, `OM_STORE_SEED=<s>`
//! seed `s` alone.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::entry::{Change, FileEvent, Group, GroupPath, Step};
use crate::page::{Hash, TreeName};
use crate::sim::{Fates, Fault, Rates, Rng, Sim};
use crate::store::{self, View};
use crate::tree::Edit;

/// Small pieces, so that schedules cross segments, checkpoint and retire
/// the log.
const STORE: store::Config = store::Config {
    log: crate::log::Config {
        segment_size: 8 << 10,
        max_payload: 4 << 10,
    },
    pages: crate::pagefile::Config {
        cache_bytes: 1 << 20,
        dirty_budget: 96,
        reserve: 32,
        growth: 32,
    },
};

/// The disk faults drawn in this part: none breaks what a flush promised.
fn disk_rates() -> Rates {
    Rates {
        crash: 4_000,
        fail_flush: 5_000,
        fail_write: 5_000,
        writeback: 400,
        ..Rates::default()
    }
}

/// The disk faults counted as a member's: those that make a call fail.
fn disk_faults(sim: &Sim) -> u64 {
    sim.fired()
        .iter()
        .filter(|(f, _)| matches!(f, Fault::Crash | Fault::FailFlush | Fault::FailWrite))
        .map(|(_, n)| n)
        .sum()
}

/// The check a failure breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Check {
    /// Two leaders in one term.
    OneLeader,
    /// A leader decided an entry is committed before the disks show it.
    EarlyCommit,
    /// A leader was elected without a committed entry.
    Completeness,
    /// A leader lost an entry of its own log while it led.
    AppendOnly,
    /// Two logs hold different entries of the same index and term, or a
    /// committed entry reads differently in two places.
    LogMatching,
    /// The committed position a member recorded is not a committed entry.
    RecordedCommitted,
    /// A member applied or published an entry that is not committed.
    Published,
    /// Two members' states after the same entry differ.
    SameState,
    /// A held view no longer reads what it read.
    View,
    /// A store refused what its core asked, with no fault in that call.
    Refused,
    /// A member did not start, with no fault.
    Start,
    /// The cluster did not settle once the faults stopped.
    Settle,
}

/// A failed check, with what the simulator saw.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Failure {
    check: Check,
    text: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.check, self.text)
    }
}

type Outcome<T = ()> = Result<T, Failure>;

/// A schedule's settings.
#[derive(Clone, Copy, Debug)]
struct Setup {
    members: usize,
    steps: u64,
    plant: Option<Plant>,
    store: store::Config,
    /// The most entries and payload bytes an append carries, when the
    /// schedule does not draw them.
    batch: Option<(u64, u64)>,
}

/// What a run exercised.
#[derive(Clone, Debug, Default)]
struct Coverage {
    schedules: u64,
    steps: u64,
    /// Faults of every kind: messages dropped and repeated, splits, power
    /// losses, stops, and disk faults that made a call fail.
    faults: u64,
    elections: u64,
    highest_term: u64,
    restarts: u64,
    power_losses: u64,
    /// Of them, right after an answer.
    after_answer: u64,
    /// Splits that isolated a leader right after it sent entries.
    after_append: u64,
    stops: u64,
    disk_faults: u64,
    failed_opens: u64,
    splits: u64,
    sent: u64,
    dropped: u64,
    repeated: u64,
    delivered: u64,
    proposed: u64,
    /// Entries committed, by the simulator's own record.
    committed: u64,
    /// Entries applied on a member and compared with every other's.
    applied: u64,
    cuts: u64,
    checkpoints: u64,
    views: u64,
    groups_opened: u64,
    groups_left_open_at_an_election: u64,
    passed_over_state: u64,
    behind: u64,
}

impl Coverage {
    fn add(&mut self, o: &Coverage) {
        self.schedules += o.schedules;
        self.steps += o.steps;
        self.faults += o.faults;
        self.elections += o.elections;
        self.highest_term = self.highest_term.max(o.highest_term);
        self.restarts += o.restarts;
        self.power_losses += o.power_losses;
        self.after_answer += o.after_answer;
        self.after_append += o.after_append;
        self.stops += o.stops;
        self.disk_faults += o.disk_faults;
        self.failed_opens += o.failed_opens;
        self.splits += o.splits;
        self.sent += o.sent;
        self.dropped += o.dropped;
        self.repeated += o.repeated;
        self.delivered += o.delivered;
        self.proposed += o.proposed;
        self.committed += o.committed;
        self.applied += o.applied;
        self.cuts += o.cuts;
        self.checkpoints += o.checkpoints;
        self.views += o.views;
        self.groups_opened += o.groups_opened;
        self.groups_left_open_at_an_election += o.groups_left_open_at_an_election;
        self.passed_over_state += o.passed_over_state;
        self.behind += o.behind;
    }
}

struct Node {
    sim: Sim,
    member: Option<Member>,
    /// When a member that is down starts again.
    down_until: u64,
    seen_committed: u64,
    seen_published: u64,
    /// The durable entry when last checked: its index and hash.
    seen_durable: (u64, [u8; 32]),
    was_leader: bool,
    /// While it leads: its term and the durable entry it held when last
    /// checked, which it must still hold.
    lead_mark: Option<(u64, u64, [u8; 32])>,
}

/// The simulator's own record (module docs): the committed entries, entry
/// `i` at `[i - 1]` with its term and hash; for each member and term, the
/// last entry it held durable while `state` recorded that term; the root
/// hashes of the state after each applied entry; the leader of each term.
#[derive(Default)]
struct Oracle {
    committed: Vec<(u64, [u8; 32])>,
    held: Vec<BTreeMap<u64, u64>>,
    roots: BTreeMap<u64, Vec<(TreeName, Hash)>>,
    leaders: BTreeMap<u64, ServerId>,
}

impl Oracle {
    fn max(&self) -> u64 {
        self.committed.len() as u64
    }

    fn hash(&self, index: u64) -> Option<[u8; 32]> {
        index
            .checked_sub(1)
            .and_then(|i| self.committed.get(i as usize))
            .map(|e| e.1)
    }

    /// The last entry of term `term` a majority held in that term.
    fn majority_held(&self, term: u64) -> u64 {
        let mut held: Vec<u64> = self
            .held
            .iter()
            .map(|h| h.get(&term).copied().unwrap_or(0))
            .collect();
        held.sort_unstable_by(|a, b| b.cmp(a));
        held[held.len() / 2]
    }
}

/// The client: entries for the serving leader, made against its published
/// state and what it gave it since it began to serve.
#[derive(Default)]
struct Client {
    leader: Option<(ServerId, u64)>,
    /// The groups it opened at that leader and has not closed.
    groups: Vec<(u64, u64)>,
    names: u64,
}

/// A phase of a schedule: the weight of a fault among the events; one
/// message in so many dropped and repeated (0: none); the most ticks a
/// message is delayed.
#[derive(Clone, Copy, Debug)]
struct Storm {
    weight: u64,
    drop: u32,
    repeat: u32,
    delay: u64,
}

const STORMS: [Storm; 4] = [
    Storm {
        weight: 0,
        drop: 0,
        repeat: 0,
        delay: 1,
    },
    Storm {
        weight: 1,
        drop: 30,
        repeat: 40,
        delay: 3,
    },
    Storm {
        weight: 2,
        drop: 12,
        repeat: 20,
        delay: 5,
    },
    Storm {
        weight: 5,
        drop: 6,
        repeat: 10,
        delay: 8,
    },
];

struct World {
    seed: u64,
    setup: Setup,
    /// The most entries and bytes an append carries, drawn for the
    /// schedule: one entry sends a new leader's entries of earlier terms on
    /// their own.
    batch: (u64, u64),
    /// The election timeout and heartbeat in ticks, drawn for the
    /// schedule: a short timeout has candidates stand in the same term.
    timing: (u64, u64),
    /// Whether the schedule churns leaders.
    churn: bool,
    /// Power losses right after a member answered a vote or an append,
    /// the window in which the answer must already be durable.
    after_answer: u64,
    /// Splits that left a leader alone right after it sent entries.
    after_append: u64,
    rng: Rng,
    now: u64,
    step: u64,
    nodes: Vec<Node>,
    ids: Vec<ServerId>,
    /// Messages in flight: when due, a number for order, the message.
    flight: Vec<(u64, u64, Message)>,
    seq: u64,
    /// The members on each side of a split, and when it heals.
    split: Option<(Vec<bool>, u64)>,
    faults: bool,
    /// How stormy the current phase is.
    storm: Storm,
    oracle: Oracle,
    client: Client,
    views: Vec<(usize, View)>,
    cov: Coverage,
}

impl World {
    fn new(seed: u64, setup: Setup) -> Outcome<World> {
        let mut sims = Vec::new();
        let mut ids = Vec::new();
        for i in 0..setup.members {
            let sim = Sim::new(seed.wrapping_mul(31).wrapping_add(i as u64 + 1));
            let fs: Arc<dyn crate::sys::Fs> = Arc::new(sim.fs());
            let id = Store::create_member(fs, &sim.random(), setup.store).map_err(|e| Failure {
                check: Check::Start,
                text: format!("member {i} was not made: {e}"),
            })?;
            sims.push(sim);
            ids.push(id);
        }
        let mut rng = Rng::stream(seed, 7);
        // One schedule in three churns leaders: short timeouts, one entry
        // an append, and new leaders often cut off (Raft's figure 8).
        let churn = rng.one_in(3);
        let (batch, timing) = if churn {
            (1, (4, 1))
        } else {
            (
                [1, 2, 3, 8][rng.below(4) as usize],
                [(4, 1), (10, 3), (16, 4)][rng.below(3) as usize],
            )
        };
        let mut w = World {
            seed,
            setup,
            batch: setup.batch.unwrap_or((batch, 16 << 10)),
            timing,
            churn,
            after_answer: 0,
            after_append: 0,
            rng,
            now: 0,
            step: 0,
            nodes: Vec::new(),
            ids: ids.clone(),
            flight: Vec::new(),
            seq: 0,
            split: None,
            faults: true,
            storm: STORMS[0],
            oracle: Oracle {
                held: vec![BTreeMap::new(); setup.members],
                ..Oracle::default()
            },
            client: Client::default(),
            views: Vec::new(),
            cov: Coverage::default(),
        };
        for (i, sim) in sims.into_iter().enumerate() {
            let fs: Arc<dyn crate::sys::Fs> = Arc::new(sim.fs());
            let (member, _) = Member::open(fs, setup.store, w.config(i))
                .map_err(|e| w.fail(Check::Start, format!("member {i} did not start: {e}")))?;
            sim.set_rates(disk_rates());
            w.nodes.push(Node {
                sim,
                member: Some(member),
                down_until: 0,
                seen_committed: 0,
                seen_published: 0,
                seen_durable: (0, [0; 32]),
                was_leader: false,
                lead_mark: None,
            });
        }
        Ok(w)
    }

    fn config(&self, i: usize) -> Config {
        Config {
            me: self.ids[i],
            members: self.ids.clone(),
            election: self.timing.0,
            heartbeat: self.timing.1,
            batch: self.batch.0,
            batch_bytes: self.batch.1,
            seed: self.seed,
            plant: self.setup.plant,
        }
    }

    fn fail(&self, check: Check, what: impl std::fmt::Display) -> Failure {
        Failure {
            check,
            text: format!(
                "seed {} ({} members), step {}, tick {}: {what}",
                self.seed, self.setup.members, self.step, self.now
            ),
        }
    }

    /// Runs `f` on member `i` with its disk's faults off: the checks read,
    /// and must neither fail nor change what the disk draws next
    /// differently from one run to the next.
    fn quiet<T>(&self, i: usize, f: impl FnOnce(&Member) -> T) -> Option<T> {
        let node = &self.nodes[i];
        let member = node.member.as_ref()?;
        node.sim.set_rates(Rates::default());
        let out = f(member);
        if self.faults {
            node.sim.set_rates(disk_rates());
        }
        Some(out)
    }

    fn up(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|i| self.nodes[*i].member.is_some())
            .collect()
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.rng.below(items.len() as u64) as usize]
    }

    fn run(&mut self) -> Outcome {
        while self.step < self.setup.steps {
            self.step += 1;
            self.one_step()?;
        }
        self.settle()
    }

    fn one_step(&mut self) -> Outcome {
        if self.step % 500 == 1 {
            // Calm phases let leaders commit; stormy ones break them.
            // Calm two phases in five.
            self.storm = STORMS[self.rng.below(5).saturating_sub(1) as usize];
        }
        let busy: Vec<usize> = self
            .up()
            .into_iter()
            .filter(|i| !self.nodes[*i].member.as_ref().unwrap().idle())
            .collect();
        let due = self.flight.iter().any(|f| f.0 <= self.now);
        // Work is done many times faster than the clock ticks: a message
        // and the actions it leads to take a fraction of a tick.
        let mut events: Vec<(u8, u64)> = vec![(b't', 50), (b'p', 100), (b'c', 5), (b'v', 10)];
        if !busy.is_empty() {
            events.push((b'a', 400));
        }
        if due {
            events.push((b'd', 300));
        }
        if self.faults && self.storm.weight > 0 {
            events.push((b'f', self.storm.weight));
        }
        let total: u64 = events.iter().map(|e| e.1).sum();
        let mut at = self.rng.below(total);
        let mut event = events[0].0;
        for (e, weight) in events {
            if at < weight {
                event = e;
                break;
            }
            at -= weight;
        }
        match event {
            b'a' => {
                let i = self.pick(&busy);
                self.act(i)
            }
            b'd' => self.deliver_one(),
            b't' => self.tick(),
            b'p' => self.propose(),
            b'c' => {
                let up = self.up();
                if up.is_empty() {
                    return Ok(());
                }
                let i = self.pick(&up);
                self.checkpoint(i)
            }
            b'v' => self.view(),
            _ => self.fault(),
        }
    }

    /// Member `i` does its next action.
    fn act(&mut self, i: usize) -> Outcome {
        let before = disk_faults(&self.nodes[i].sim);
        let member = self.nodes[i].member.as_mut().unwrap();
        let cut = matches!(member.queue.front(), Some(Action::Cut { .. }));
        let result = member.perform();
        if result.is_ok() && self.power_lost(i)? {
            return Ok(());
        }
        match result {
            Ok(Some(done)) => {
                if cut {
                    self.cov.cuts += 1;
                }
                // A power loss right after an answer: the window in which
                // the answer must already be durable. Votes are fewer than
                // acknowledgements, and drawn more often.
                let mut answered = 0;
                let mut appended = None;
                if let Some(m) = done.sent {
                    answered = match m.body {
                        Body::VoteReply { granted: true } => 6,
                        Body::AppendReply {
                            matched: Some(_), ..
                        } => 60,
                        _ => 0,
                    };
                    if let Body::Append { entries, .. } = &m.body {
                        // A new leader's first entry is `empty`.
                        let empty = crate::entry::Kind::Empty.code();
                        appended = entries
                            .iter()
                            .map(|e| e.kind == empty)
                            .reduce(|a, b| a || b);
                    }
                    self.send(m);
                }
                if done.proposed.is_some() {
                    self.cov.proposed += 1;
                }
                self.check(i)?;
                let storm = self.faults && self.storm.weight > 0;
                // A leader cut off right after it sent its first entry, or
                // right after it recorded a commit, before what follows
                // reaches the others.
                let diverged = done.committed.is_some() && self.diverged(i);
                let isolate = match (appended, done.committed, self.churn) {
                    // Another member holds entries of a later term than the
                    // leader's at their place: cut the leader off and give
                    // it its chance (Raft's figure 8).
                    _ if diverged => 2,
                    (Some(true), _, true) => 2,
                    (_, Some(_), true) => 2,
                    (Some(true), _, false) => 6,
                    (_, Some(_), false) => 40,
                    (Some(false), _, _) => 80,
                    _ => 0,
                };
                if storm && self.rng.one_in(answered) {
                    self.nodes[i].sim.crash(Fates::Drawn);
                    self.nodes[i].member = None;
                    self.nodes[i].down_until = self.now + 1 + self.rng.below(6);
                    self.cov.power_losses += 1;
                    self.cov.faults += 1;
                    self.after_answer += 1;
                } else if (storm || diverged && self.faults) && self.rng.one_in(isolate) {
                    // The leader alone, right after it sent entries, in place
                    // of any split before: what it sent may reach some and
                    // not others, and the last leader cut off may be back.
                    let mut side = vec![false; self.nodes.len()];
                    side[i] = true;
                    let until = self.now + 3 + self.rng.below(50);
                    self.split = Some((side, until));
                    self.cov.splits += 1;
                    self.cov.faults += 1;
                    self.after_append += 1;
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => self.failed(i, before, e.to_string()),
        }
    }

    /// Whether member `i`'s disk lost power in a call that returned: a
    /// failure in work the store does not report, such as freeing pages
    /// after a checkpoint. The process is gone all the same.
    fn power_lost(&mut self, i: usize) -> Outcome<bool> {
        if !self.nodes[i].sim.halted() {
            return Ok(false);
        }
        // What it applied before, it applied.
        self.check_applied(i)?;
        let down_until = self.now + 1 + self.rng.below(8);
        let node = &mut self.nodes[i];
        node.member = None;
        node.down_until = down_until;
        Ok(true)
    }

    /// Member `i`'s store returned an error in a call made when its disk
    /// had fired `before` faults: the fault's, when one fired in that very
    /// call; otherwise a defect.
    fn failed(&mut self, i: usize, before: u64, e: String) -> Outcome {
        let node = &self.nodes[i];
        if !node.sim.halted() && disk_faults(&node.sim) == before {
            return Err(self.fail(
                Check::Refused,
                format!("member {i}'s store refused what its core asked, with no fault: {e}"),
            ));
        }
        // What it applied before the error, it applied.
        self.check_applied(i)?;
        let down_until = self.now + 1 + self.rng.below(8);
        let node = &mut self.nodes[i];
        node.member = None;
        node.down_until = down_until;
        Ok(())
    }

    fn send(&mut self, m: Message) {
        self.cov.sent += 1;
        let mut copies = 1;
        let storm = if self.faults { self.storm } else { STORMS[0] };
        if self.rng.one_in(storm.drop) {
            self.cov.dropped += 1;
            self.cov.faults += 1;
            return;
        }
        if self.rng.one_in(storm.repeat) {
            self.cov.repeated += 1;
            self.cov.faults += 1;
            copies = 2;
        }
        for _ in 0..copies {
            // Never longer than an election timeout, which would leave no
            // election able to finish.
            let delay = self.rng.below(storm.delay.min(self.timing.0) + 1);
            self.seq += 1;
            self.flight.push((self.now + delay, self.seq, m.clone()));
        }
    }

    fn apart(&self, a: ServerId, b: ServerId) -> bool {
        let Some((side, _)) = &self.split else {
            return false;
        };
        let of = |id| self.ids.iter().position(|x| *x == id).map(|i| side[i]);
        of(a) != of(b)
    }

    /// Delivers one message that is due, drawn among them.
    fn deliver_one(&mut self) -> Outcome {
        let due: Vec<usize> = (0..self.flight.len())
            .filter(|k| self.flight[*k].0 <= self.now)
            .collect();
        if due.is_empty() {
            return Ok(());
        }
        let k = self.pick(&due);
        self.deliver(k)
    }

    fn deliver(&mut self, k: usize) -> Outcome {
        let (_, _, m) = self.flight.remove(k);
        let Some(i) = self.ids.iter().position(|x| *x == m.to) else {
            return Ok(());
        };
        if self.apart(m.from, m.to) || self.nodes[i].member.is_none() {
            self.cov.dropped += 1;
            return Ok(());
        }
        let member = self.nodes[i].member.as_mut().unwrap();
        if !member.idle() {
            // It takes messages only when done with the last: this one waits.
            self.seq += 1;
            self.flight.push((self.now, self.seq, m));
            return self.act(i);
        }
        member.receive(m);
        self.cov.delivered += 1;
        self.check(i)
    }

    fn tick(&mut self) -> Outcome {
        self.now += 1;
        if let Some((_, until)) = &self.split {
            if *until <= self.now {
                self.split = None;
            }
        }
        for i in 0..self.nodes.len() {
            if self.nodes[i].member.is_none() {
                if self.nodes[i].down_until <= self.now {
                    self.open(i)?;
                }
                continue;
            }
            let member = self.nodes[i].member.as_mut().unwrap();
            if member.tick() {
                self.check(i)?;
            }
        }
        Ok(())
    }

    /// Opens member `i`'s store again.
    fn open(&mut self, i: usize) -> Outcome {
        let cfg = self.config(i);
        let store = self.setup.store;
        let node = &mut self.nodes[i];
        node.sim.restart();
        let before = disk_faults(&node.sim);
        let fs: Arc<dyn crate::sys::Fs> = Arc::new(node.sim.fs());
        let opened = Member::open(fs, store, cfg).map_err(|e| e.to_string());
        let halted = node.sim.halted();
        let faulted = disk_faults(&node.sim) != before;
        match opened {
            Ok(_) if halted => {
                // The power failed in work the open does not report.
                self.cov.failed_opens += 1;
                self.nodes[i].down_until = self.now + 1 + self.rng.below(4);
                Ok(())
            }
            Ok((member, opened)) => {
                if opened.passed_over_state.is_some() {
                    self.cov.passed_over_state += 1;
                }
                let node = &mut self.nodes[i];
                node.member = Some(member);
                node.seen_committed = 0;
                node.seen_published = 0;
                node.seen_durable = (0, [0; 32]);
                node.was_leader = false;
                node.lead_mark = None;
                self.cov.restarts += 1;
                self.check(i)
            }
            Err(e) => {
                if !halted && !faulted {
                    return Err(self.fail(
                        Check::Start,
                        format!("member {i} did not start, with no fault: {e}"),
                    ));
                }
                self.cov.failed_opens += 1;
                let node = &mut self.nodes[i];
                node.down_until = self.now + 1 + self.rng.below(4);
                Ok(())
            }
        }
    }

    /// A client's entry at the serving leader, if one serves and is idle.
    fn propose(&mut self) -> Outcome {
        let Some(i) = self.up().into_iter().find(|i| {
            // A client waits for its answers: at most four entries the
            // leader has not committed.
            let m = self.nodes[*i].member.as_ref().unwrap();
            m.core.serving() && m.idle() && m.core.last().index < m.core.committed() + 4
        }) else {
            return Ok(());
        };
        let (id, term) = {
            let core = &self.nodes[i].member.as_ref().unwrap().core;
            (core.me(), core.term())
        };
        if self.client.leader != Some((id, term)) {
            // A new leader: the groups opened at the last one are its
            // `empty` entry's to discard.
            self.client.leader = Some((id, term));
            self.client.groups.clear();
        }
        let files = self
            .quiet(i, |m| -> Result<Vec<u64>, String> {
                let view = m.store.view().map_err(|e| e.to_string())?;
                let tree = view.store_tree().map_err(|e| e.to_string())?;
                let files = match tree {
                    Some(t) => t.files().map_err(|e| e.to_string())?,
                    None => Vec::new(),
                };
                Ok(files.into_iter().map(|(id, _)| id).collect())
            })
            .unwrap()
            .map_err(|e| self.fail(Check::View, format!("the leader's view: {e}")))?;
        let next = self.nodes[i].member.as_ref().unwrap().core.last().index + 1;
        let value = |rng: &mut Rng| {
            let mut v = vec![0u8; rng.below(80) as usize];
            rng.fill(&mut v);
            v
        };
        let (file, payload) = if files.is_empty() || self.rng.one_in(25) {
            self.client.names += 1;
            let name = format!("file {}", self.client.names).into_bytes();
            let added = FileEvent::Added {
                name,
                flags: 0,
                page0: vec![7; 16],
            };
            (next, Payload::File(added))
        } else if self.client.groups.len() < 2 && self.rng.one_in(12) {
            let file = self.pick(&files);
            let path = vec![b"g".to_vec(), next.to_be_bytes().to_vec()];
            let mut leaf = path.clone();
            leaf.push(vec![0]);
            let change = Change {
                group: Some(Group {
                    id: next,
                    step: Step::Open(vec![GroupPath { file, path }]),
                }),
                edits: vec![Edit::leaf(leaf, value(&mut self.rng))],
                ..Change::default()
            };
            self.client.groups.push((next, file));
            self.cov.groups_opened += 1;
            (file, Payload::Change(change))
        } else if !self.client.groups.is_empty() && self.rng.one_in(3) {
            let k = self.rng.below(self.client.groups.len() as u64) as usize;
            let (id, file) = self.client.groups[k];
            let change = if self.rng.one_in(3) {
                self.client.groups.remove(k);
                Change {
                    group: Some(Group {
                        id,
                        step: Step::Close,
                    }),
                    ..Change::default()
                }
            } else {
                let leaf = vec![
                    b"g".to_vec(),
                    id.to_be_bytes().to_vec(),
                    vec![self.rng.below(8) as u8],
                ];
                Change {
                    group: Some(Group {
                        id,
                        step: Step::Part,
                    }),
                    edits: vec![Edit::leaf(leaf, value(&mut self.rng))],
                    ..Change::default()
                }
            };
            (file, Payload::Change(change))
        } else {
            let file = self.pick(&files);
            let mut edits = Vec::new();
            for _ in 0..1 + self.rng.below(3) {
                let path = vec![b"r".to_vec(), vec![self.rng.below(24) as u8]];
                if self.rng.one_in(6) {
                    edits.push(Edit::delete(path));
                } else {
                    edits.push(Edit::leaf(path, value(&mut self.rng)));
                }
            }
            let change = Change {
                edits,
                ..Change::default()
            };
            (file, Payload::Change(change))
        };
        let member = self.nodes[i].member.as_mut().unwrap();
        if member.propose(file, payload) {
            self.check(i)?;
        }
        Ok(())
    }

    fn checkpoint(&mut self, i: usize) -> Outcome {
        let before = disk_faults(&self.nodes[i].sim);
        let member = self.nodes[i].member.as_mut().unwrap();
        let result = member.checkpoint();
        if result.is_ok() && self.power_lost(i)? {
            return Ok(());
        }
        match result {
            Ok(true) => {
                self.cov.checkpoints += 1;
                self.check(i)
            }
            Ok(false) => Ok(()),
            Err(e) => self.failed(i, before, e.to_string()),
        }
    }

    /// A view taken on a member and held; the oldest of more than four is
    /// let go, after a last check that it reads what it read.
    fn view(&mut self) -> Outcome {
        let up = self.up();
        if up.is_empty() {
            return Ok(());
        }
        let i = self.pick(&up);
        let Some(Ok(view)) = self.quiet(i, |m| m.store.view()) else {
            return Ok(());
        };
        self.cov.views += 1;
        self.check_view(&view)?;
        self.views.push((i, view));
        if self.views.len() > 4 {
            let (_, old) = self.views.remove(0);
            self.check_view(&old)?;
        }
        Ok(())
    }

    /// A view is of a committed entry, and its trees are those of the
    /// state after that entry on every member that applied it.
    fn check_view(&self, view: &View) -> Outcome {
        let at = view.position();
        if at.index == 0 {
            return Ok(());
        }
        if self.oracle.hash(at.index) != Some(at.hash.0) {
            return Err(self.fail(
                Check::Published,
                format!("a view shows entry {}, which is not committed", at.index),
            ));
        }
        let roots: BTreeMap<TreeName, Hash> =
            view.roots().into_iter().map(|(n, r)| (n, r.hash)).collect();
        if let Some(known) = self.oracle.roots.get(&at.index) {
            let known: BTreeMap<TreeName, Hash> = known
                .iter()
                .filter(|(n, _)| !is_group(n))
                .cloned()
                .collect();
            if known != roots {
                return Err(self.fail(
                    Check::View,
                    format!(
                        "a view of entry {} reads other trees than the state there",
                        at.index
                    ),
                ));
            }
        }
        if let Err(e) = view.store_tree() {
            return Err(self.fail(Check::View, format!("a held view no longer reads: {e}")));
        }
        Ok(())
    }

    /// Whether a member other than leader `i` ends its log with an entry
    /// of a later term than the leader's entry at that place.
    fn diverged(&self, i: usize) -> bool {
        let Some(lead) = self.nodes[i].member.as_ref() else {
            return false;
        };
        self.nodes.iter().enumerate().any(|(j, n)| {
            let Some(m) = n.member.as_ref().filter(|_| j != i) else {
                return false;
            };
            let last = m.core.last();
            last.index > lead.core.base().index
                && lead.core.term_at(last.index).is_none_or(|t| t < last.term)
        })
    }

    /// The member that leads, if one is up and leads.
    fn leading(&self) -> Option<usize> {
        self.up()
            .into_iter()
            .find(|i| self.nodes[*i].member.as_ref().unwrap().core.role() == Role::Leader)
    }

    /// A member that is up to fault: the leader half the time.
    fn victim(&mut self, up: &[usize]) -> usize {
        match self.leading() {
            Some(l) if self.rng.one_in(2) => l,
            _ => self.pick(up),
        }
    }

    fn fault(&mut self) -> Outcome {
        let up = self.up();
        match self.rng.below(9) {
            0..=2 if !up.is_empty() => {
                let i = self.victim(&up);
                self.nodes[i].sim.crash(Fates::Drawn);
                self.nodes[i].member = None;
                self.nodes[i].down_until = self.now + 1 + self.rng.below(25);
                self.cov.power_losses += 1;
                self.cov.faults += 1;
            }
            3 | 4 if !up.is_empty() => {
                let i = self.victim(&up);
                self.nodes[i].member = None;
                self.nodes[i].down_until = self.now + 1 + self.rng.below(25);
                self.cov.stops += 1;
                self.cov.faults += 1;
            }
            5..=7 if self.split.is_none() => {
                let n = self.nodes.len();
                let mut side = vec![false; n];
                let leader = self.leading();
                if self.rng.one_in(2) {
                    // One member alone, the leader half the time.
                    let i = match leader {
                        Some(l) if self.rng.one_in(2) => l,
                        _ => self.rng.below(n as u64) as usize,
                    };
                    side[i] = true;
                } else {
                    for s in side.iter_mut() {
                        *s = self.rng.one_in(2);
                    }
                }
                let until = self.now + 5 + self.rng.below(40);
                self.split = Some((side, until));
                self.cov.splits += 1;
                self.cov.faults += 1;
            }
            _ => self.split = None,
        }
        Ok(())
    }

    /// The simulator's record learns member `i`'s durable entry and the
    /// term its `state` records: an entry a majority held durable in its
    /// own term is committed, and so is every entry before it (module
    /// docs). What it reads it reads from the disk of the member that made
    /// the majority.
    fn observe(&mut self, i: usize) -> Outcome {
        let Some(m) = self.nodes[i].member.as_ref() else {
            return Ok(());
        };
        let (index, term, _) = m.store.durable_entry();
        let (saved, _) = m.store.saved_term();
        if index == 0 || term != saved {
            return Ok(());
        }
        let held = self.oracle.held[i].entry(term).or_insert(0);
        if index <= *held {
            return Ok(());
        }
        *held = index;
        let committed = self.oracle.majority_held(term);
        let max = self.oracle.max();
        if committed <= max || index < committed {
            return Ok(());
        }
        if max > 0 {
            self.check_entry(i, max, Check::LogMatching, "the last committed entry")?;
        }
        let read = self
            .quiet(i, |m| m.store.read_wire(max + 1, committed, u64::MAX))
            .unwrap()
            .map_err(|e| self.fail(Check::LogMatching, format!("member {i}'s log: {e}")))?;
        if read.len() as u64 != committed - max || read.first().map(|e| e.index) != Some(max + 1) {
            return Err(self.fail(
                Check::LogMatching,
                format!("member {i} holds entry {committed} without those after {max}"),
            ));
        }
        for e in read {
            self.oracle.committed.push((e.term, e.hash));
        }
        Ok(())
    }

    /// Member `i`'s log holds entry `index` as committed. An entry its log
    /// retired is covered by a checkpoint at the log's base, which must
    /// then be committed itself.
    fn check_entry(&self, i: usize, index: u64, check: Check, what: &str) -> Outcome {
        let base = self.quiet(i, |m| m.store.log_base().index).unwrap();
        if index < base {
            let at = self
                .quiet(i, |m| m.store.entry_hash(base))
                .unwrap()
                .map_err(|e| self.fail(check, format!("member {i}'s log: {e}")))?;
            if at.is_none() || at != self.oracle.hash(base) {
                return Err(self.fail(
                    Check::Published,
                    format!(
                        "member {i}'s log starts after entry {base}, which is not committed                          ({} committed)",
                        self.oracle.max()
                    ),
                ));
            }
            return Ok(());
        }
        let want = self.oracle.hash(index);
        let have = self
            .quiet(i, |m| m.store.entry_hash(index))
            .unwrap()
            .map_err(|e| self.fail(check, format!("member {i}'s log: {e}")))?;
        if want.is_none() || have != want {
            return Err(self.fail(
                check,
                format!(
                    "member {i}'s entry {index}, {what}, is not the committed entry ({} committed)",
                    self.oracle.max()
                ),
            ));
        }
        Ok(())
    }

    /// Every entry member `i` applied since the last check is committed,
    /// and its state after it has the root hashes of every other member's
    /// after the same entry.
    fn check_applied(&mut self, i: usize) -> Outcome {
        let applied = self.nodes[i].member.as_mut().unwrap().store.take_applied();
        for (index, hash, roots) in applied {
            self.cov.applied += 1;
            if self.oracle.hash(index) != Some(hash) {
                return Err(self.fail(
                    Check::Published,
                    format!(
                        "member {i} applied entry {index}, which is not committed ({} committed)",
                        self.oracle.max()
                    ),
                ));
            }
            match self.oracle.roots.get(&index) {
                Some(known) if *known != roots => {
                    return Err(self.fail(
                        Check::SameState,
                        format!(
                            "member {i}'s state after entry {index} has other root hashes than \
                             another member's"
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    self.oracle.roots.insert(index, roots);
                }
            }
        }
        Ok(())
    }

    /// The checks after a step that involved member `i` (module docs).
    fn check(&mut self, i: usize) -> Outcome {
        if self.nodes[i].member.is_none() {
            return Ok(());
        }
        self.observe(i)?;
        self.check_applied(i)?;
        let member = self.nodes[i].member.as_ref().unwrap();
        let core = &member.core;
        let (role, term, id) = (core.role(), core.term(), core.me());
        let decided = core.committed();
        let (d_index, d_term, d_hash) = member.store.durable_entry();
        self.cov.highest_term = self.cov.highest_term.max(term);
        if role == Role::Leader && decided > self.oracle.max() {
            return Err(self.fail(
                Check::EarlyCommit,
                format!(
                    "leader {i} decided entry {decided} is committed; the disks show {}",
                    self.oracle.max()
                ),
            ));
        }
        // At most one leader per term; a leader holds every committed
        // entry.
        if role == Role::Leader && !self.nodes[i].was_leader {
            self.nodes[i].was_leader = true;
            match self.oracle.leaders.get(&term) {
                Some(other) if *other != id => {
                    return Err(self.fail(Check::OneLeader, format!("two leaders in term {term}")));
                }
                Some(_) => {}
                None => {
                    self.oracle.leaders.insert(term, id);
                    self.cov.elections += 1;
                    let open = self
                        .quiet(i, |m| {
                            m.store
                                .view()
                                .ok()
                                .and_then(|v| v.store_tree().ok().flatten()?.groups().ok())
                                .map_or(0, |g| g.len() as u64)
                        })
                        .unwrap();
                    self.cov.groups_left_open_at_an_election += open;
                }
            }
            let max = self.oracle.max();
            if max > 0 {
                if d_index < max {
                    return Err(self.fail(
                        Check::Completeness,
                        format!(
                            "member {i} was elected in term {term} with its log ending at \
                             {d_index}, before committed entry {max}"
                        ),
                    ));
                }
                self.check_entry(i, max, Check::Completeness, "at its election")?;
            }
        }
        if role != Role::Leader {
            self.nodes[i].was_leader = false;
        }
        // A leader keeps every entry of its log while it leads.
        if role == Role::Leader {
            if let Some((t, index, hash)) = self.nodes[i].lead_mark {
                if t == term && (index, hash) != (d_index, d_hash) {
                    let have = self
                        .quiet(i, |m| m.store.entry_hash(index))
                        .unwrap()
                        .map_err(|e| self.fail(Check::AppendOnly, format!("its log: {e}")))?;
                    if have != Some(hash) {
                        return Err(self.fail(
                            Check::AppendOnly,
                            format!("leader {i} of term {term} lost its entry {index}"),
                        ));
                    }
                }
            }
            self.nodes[i].lead_mark = Some((term, d_index, d_hash));
        } else {
            self.nodes[i].lead_mark = None;
        }
        // Logs that hold an entry of the same index and term hold the same
        // entry.
        if (d_index, d_hash) != self.nodes[i].seen_durable {
            self.nodes[i].seen_durable = (d_index, d_hash);
            for j in self.up() {
                if j == i {
                    continue;
                }
                let theirs = self.quiet(j, |m| m.store.entry_at(d_index)).unwrap();
                if let Ok(Some((t, h))) = theirs {
                    if t == d_term && h != d_hash {
                        return Err(self.fail(
                            Check::LogMatching,
                            format!(
                                "members {i} and {j} hold different entries {d_index} of term \
                                 {d_term}"
                            ),
                        ));
                    }
                }
            }
        }
        // The committed position it recorded is committed.
        let member = self.nodes[i].member.as_ref().unwrap();
        let committed = member.store.committed_position();
        if committed > self.nodes[i].seen_committed {
            self.nodes[i].seen_committed = committed;
            self.check_entry(
                i,
                committed,
                Check::RecordedCommitted,
                "its recorded committed position",
            )?;
        }
        // It publishes only what is committed.
        let member = self.nodes[i].member.as_ref().unwrap();
        let published = member.store.published();
        if published.index != self.nodes[i].seen_published {
            self.nodes[i].seen_published = published.index;
            if published.index > 0 && self.oracle.hash(published.index) != Some(published.hash.0) {
                return Err(self.fail(
                    Check::Published,
                    format!(
                        "member {i} published entry {}, which is not committed ({} committed)",
                        published.index,
                        self.oracle.max()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The faults stop and the network heals: every member starts, one
    /// leader serves, and every member holds and publishes everything
    /// committed, or is one the leader reports as behind.
    fn settle(&mut self) -> Outcome {
        self.calm();
        for i in 0..self.nodes.len() {
            if self.nodes[i].member.is_none() {
                self.open(i)?;
                if self.nodes[i].member.is_none() {
                    return Err(self.fail(
                        Check::Start,
                        format!("member {i} did not start once faults stopped"),
                    ));
                }
            }
        }
        if self.run_until(600, |w| w.settled().is_some())? {
            self.cov.behind += self.settled().unwrap_or(0);
            return Ok(());
        }
        let state: Vec<String> = self
            .nodes
            .iter()
            .map(|n| match &n.member {
                Some(m) => format!(
                    "{:?} term {} last {} committed {} published {}",
                    m.core.role(),
                    m.core.term(),
                    m.core.last().index,
                    m.core.committed(),
                    m.core.published()
                ),
                None => "down".into(),
            })
            .collect();
        Err(self.fail(
            Check::Settle,
            format!("the cluster did not settle: {state:?}"),
        ))
    }

    /// No more faults, and the network heals.
    fn calm(&mut self) {
        self.faults = false;
        self.split = None;
        for n in &self.nodes {
            n.sim.set_rates(Rates::default());
        }
    }

    /// Runs the members with everything due delivered and done before
    /// each tick, for at most `ticks` ticks, until `done`.
    fn run_until(&mut self, ticks: u64, done: impl Fn(&World) -> bool) -> Outcome<bool> {
        for _ in 0..ticks {
            loop {
                let busy = self
                    .up()
                    .into_iter()
                    .find(|i| !self.nodes[*i].member.as_ref().unwrap().idle());
                if let Some(i) = busy {
                    self.act(i)?;
                    continue;
                }
                let due = (0..self.flight.len())
                    .filter(|k| self.flight[*k].0 <= self.now)
                    .min_by_key(|k| (self.flight[*k].0, self.flight[*k].1));
                match due {
                    Some(k) => self.deliver(k)?,
                    None => break,
                }
            }
            if done(self) {
                return Ok(true);
            }
            self.step += 1;
            self.tick()?;
        }
        Ok(false)
    }

    /// Whether the cluster settled, and how many members are behind.
    fn settled(&self) -> Option<u64> {
        let members: Vec<&Member> = self
            .nodes
            .iter()
            .filter_map(|n| n.member.as_ref())
            .collect();
        let leaders: Vec<&&Member> = members.iter().filter(|m| m.core.serving()).collect();
        let [leader] = leaders[..] else {
            return None;
        };
        let lead = &leader.core;
        if lead.committed() != lead.last().index {
            return None;
        }
        let mut behind = 0;
        for m in &members {
            if lead.behind().contains(&m.core.me()) {
                // Behind only when the leader's log no longer holds the
                // entry after the last it knows is committed.
                if m.core.committed() >= lead.base().index {
                    return None;
                }
                behind += 1;
                continue;
            }
            if m.core.committed() != lead.committed()
                || m.core.published() != lead.committed()
                || m.core.last() != lead.last()
            {
                return None;
            }
        }
        Some(behind)
    }
}

/// Whether a tree name is an open group's (`group:` and its id).
fn is_group(name: &TreeName) -> bool {
    matches!(
        crate::apply::TreeId::from_name(name.as_bytes()),
        Some(crate::apply::TreeId::Group(_))
    )
}

/// Runs one schedule.
fn run(seed: u64, setup: Setup) -> Outcome<Coverage> {
    let mut w = World::new(seed, setup)?;
    w.run()?;
    for (_, view) in &w.views {
        w.check_view(view)?;
    }
    w.cov.schedules = 1;
    w.cov.steps = w.step;
    w.cov.committed = w.oracle.max();
    w.cov.after_answer = w.after_answer;
    w.cov.after_append = w.after_append;
    w.cov.disk_faults = w.nodes.iter().map(|n| disk_faults(&n.sim)).sum();
    w.cov.faults += w.cov.disk_faults;
    Ok(w.cov)
}

/// The seeds of a run: `default` of them, scaled.
fn seeds(default: u64) -> Vec<u64> {
    let read = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(seed) = read("OM_STORE_SEED") {
        return vec![seed];
    }
    (0..default.saturating_mul(read("OM_STORE_SCALE").unwrap_or(1))).collect()
}

/// A seed's schedule: even seeds run 3 members, odd ones 5.
fn setup(seed: u64, steps: u64, plant: Option<Plant>) -> Setup {
    Setup {
        members: if seed % 2 == 0 { 3 } else { 5 },
        steps,
        plant,
        store: STORE,
        batch: None,
    }
}

const STEPS: u64 = 5_000;
const SEEDS: u64 = 60;

fn report(name: &str, cov: &Coverage, secs: f64) {
    let per = |n: u64| n as f64 / cov.schedules.max(1) as f64;
    eprintln!(
        "{name}: {} schedules in {secs:.1} s ({:.1} per second); per schedule {:.0} steps, \
         {:.1} faults; in all {cov:?}",
        cov.schedules,
        cov.schedules as f64 / secs,
        per(cov.steps),
        per(cov.faults),
    );
}

fn run_all(name: &str, seeds: Vec<u64>, steps: u64) {
    let started = Instant::now();
    let mut total = Coverage::default();
    for seed in seeds {
        match run(seed, setup(seed, steps, None)) {
            Ok(cov) => total.add(&cov),
            Err(e) => panic!("{e}"),
        }
    }
    report(name, &total, started.elapsed().as_secs_f64());
    assert!(total.elections > 0 && total.committed > 0, "{total:?}");
}

#[test]
fn clusters_keep_the_rules_of_consensus() {
    run_all("default", seeds(SEEDS), STEPS);
}

#[test]
#[ignore = "long: run in release, OM_STORE_SCALE to scale"]
fn long_clusters_keep_the_rules_of_consensus() {
    run_all("long", seeds(SEEDS * 20), STEPS * 4);
}

const PLANTS: [Plant; 5] = [
    Plant::VoteUnflushed,
    Plant::CountOlderTerm,
    Plant::CommitUnchecked,
    Plant::CutAtChecked,
    Plant::PublishDurable,
];

/// The checks a planted defect must break, and for a refusal what the
/// store must have refused.
fn expected(plant: Plant) -> (&'static [Check], &'static [&'static str]) {
    match plant {
        // A forgotten vote elects two leaders in one term, or, the term
        // forgotten too, a leader without an entry committed meanwhile.
        Plant::VoteUnflushed => (&[Check::OneLeader, Check::Completeness], &[]),
        Plant::CountOlderTerm => (&[Check::EarlyCommit], &[]),
        Plant::CommitUnchecked => (&[Check::RecordedCommitted], &[]),
        // The log refuses a cut at its committed position; past it, the cut
        // takes a matching entry and the next one no longer continues the
        // chain.
        Plant::CutAtChecked => (
            &[Check::Refused],
            &[
                "entries at or before the committed position are never cut",
                "does not continue this log's chain",
            ],
        ),
        Plant::PublishDurable => (&[Check::Published], &[]),
    }
}

fn is_expected(plant: Plant, failure: &Failure) -> bool {
    let (checks, reasons) = expected(plant);
    checks.contains(&failure.check)
        && (reasons.is_empty() || reasons.iter().any(|r| failure.text.contains(r)))
}

/// Every planted defect is found by a seed of the default run, by the
/// check it breaks, and that seed fails again the same way.
#[test]
fn planted_defects_are_found() {
    for plant in PLANTS {
        let found = (0..SEEDS).find_map(|seed| {
            run(seed, setup(seed, STEPS, Some(plant)))
                .err()
                .map(|e| (seed, e))
        });
        let Some((seed, failure)) = found else {
            panic!("{plant:?} is not found in the default run's {SEEDS} seeds");
        };
        eprintln!("{plant:?}: {failure}");
        assert!(
            is_expected(plant, &failure),
            "{plant:?} must break {:?}: {failure}",
            expected(plant)
        );
        let again = run(seed, setup(seed, STEPS, Some(plant))).err();
        assert_eq!(again.as_ref(), Some(&failure), "{plant:?} replays");
    }
}

/// How many seeds of a run find each planted defect, by which check, and
/// the first.
#[test]
#[ignore = "a measurement: run in release, OM_STORE_SCALE to scale"]
fn planted_defects_found_by_how_many_seeds() {
    for plant in PLANTS {
        let seeds = seeds(SEEDS);
        let mut kinds: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for seed in &seeds {
            if let Err(e) = run(*seed, setup(*seed, STEPS, Some(plant))) {
                let what = e.text.split_once(": ").map_or(e.text.as_str(), |(_, w)| w);
                let what: String = what
                    .chars()
                    .map(|c| if c.is_ascii_digit() { '#' } else { c })
                    .collect();
                let kind = format!(
                    "{:?} ({}): {what}",
                    e.check,
                    if is_expected(plant, &e) {
                        "expected"
                    } else {
                        "NOT EXPECTED"
                    }
                );
                kinds.entry(kind).or_insert((0, *seed)).0 += 1;
            }
        }
        let found: u64 = kinds.values().map(|k| k.0).sum();
        eprintln!("{plant:?}: found by {found} of {} seeds", seeds.len());
        for (kind, (n, first)) in kinds {
            eprintln!("  {n} seeds, the first {first}: {kind}");
        }
    }
}

/// A member's id in the core-level tests.
fn id(n: u8) -> ServerId {
    [n; 16]
}

/// The core of member `me` of three, its log holding entries of `terms`.
fn core_of(me: u8, terms: &[u64], term: u64) -> Core {
    let cfg = Config {
        me: id(me),
        members: vec![id(1), id(2), id(3)],
        election: 4,
        heartbeat: 1,
        batch: 2,
        batch_bytes: 1 << 10,
        seed: 5,
        plant: None,
    };
    let rec = Recovered {
        server: id(me),
        start_count: 1,
        term,
        vote: None,
        base: Pos::default(),
        terms: terms.to_vec(),
        committed: 0,
        published: 0,
    };
    Core::new(cfg, &rec).unwrap()
}

/// Makes `a` the leader of the next term with `voter`'s votes, its `empty`
/// entry durable, and returns the appends it sends then: to whom, the
/// request's number, `prev`, and its first and last entries.
fn elect(a: &mut Core, voter: ServerId) -> Vec<(ServerId, u64, Pos, u64, u64)> {
    for _ in 0..100 {
        if a.role() == Role::PreCandidate {
            break;
        }
        a.tick();
    }
    let ask = |a: &Core, body| Message {
        from: voter,
        to: a.me(),
        term: a.term(),
        start: 1,
        body,
    };
    let round = a.term() + 1;
    a.receive(ask(
        a,
        Body::PreVoteReply {
            round,
            granted: true,
        },
    ));
    assert_eq!(a.role(), Role::Candidate);
    a.receive(ask(a, Body::VoteReply { granted: true }));
    assert_eq!(a.role(), Role::Leader);
    appends(&a.flushed(a.last()))
}

fn appends(actions: &[Action]) -> Vec<(ServerId, u64, Pos, u64, u64)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SendAppend {
                to,
                seq,
                prev,
                first,
                last,
                ..
            } => Some((*to, *seq, *prev, *first, *last)),
            _ => None,
        })
        .collect()
}

fn reply(from: u8, term: u64, seq: u64, matched: Option<u64>, hint: u64) -> Message {
    Message {
        from: id(from),
        to: id(1),
        term,
        start: 1,
        body: Body::AppendReply { seq, matched, hint },
    }
}

/// A reply counts only against the request it answers and within what
/// that request checked; numbers a message gives never overflow; an
/// append that is not a leader's whole request changes nothing
/// (section 7.1, trust).
#[test]
fn a_core_takes_from_a_message_only_what_it_asked_for() {
    let mut a = core_of(1, &[1, 1, 1], 1);
    let sent = elect(&mut a, id(2));
    let term = a.term();
    let (_, seq, prev, first, last) = sent.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!((prev.index, first, last), (3, 4, 4));
    // An answer to no request, and a match past what was asked: passed over.
    a.receive(reply(2, term, seq + 100, Some(4), 0));
    a.receive(reply(2, term, seq, Some(9), 0));
    assert_eq!(a.committed(), 0);
    // The largest hint: no overflow, and the next try stays in the log.
    let out = appends(&a.receive(reply(2, term, seq, None, u64::MAX)));
    let (_, seq, prev, _, last) = out[0];
    assert!(prev.index < 3 && last == 4, "{out:?}");
    // An answer to that request within its range counts: the leader's own
    // entry is committed.
    a.receive(reply(2, term, seq, Some(4), 0));
    assert_eq!(a.committed(), 4);
    // A reply nobody asked for, of the largest term: passed over.
    let stray = Message {
        from: id(3),
        to: id(1),
        term: u64::MAX,
        start: 1,
        body: Body::PreVoteReply {
            round: 0,
            granted: false,
        },
    };
    assert!(a.receive(stray).is_empty());
    assert_eq!(a.term(), term);
    // A follower passes over an append whose entries pass its term, or that
    // carries more than a batch.
    let mut f = core_of(3, &[], 0);
    let wire = |index, term| Wire {
        index,
        term,
        kind: crate::entry::Kind::Empty.code(),
        file: 0,
        payload: Vec::new(),
        hash: [0; 32],
    };
    let append = |entries| Message {
        from: id(1),
        to: id(3),
        term: 2,
        start: 1,
        body: Body::Append {
            seq: 1,
            prev: Pos::default(),
            entries,
            committed: u64::MAX,
        },
    };
    assert!(f.receive(append(vec![wire(1, 3)])).is_empty());
    assert!(f
        .receive(append(vec![wire(1, 2), wire(2, 2), wire(3, 2)]))
        .is_empty());
    assert_eq!(f.term(), 0);
}

/// A rejection delivered twice, the second after the follower caught up
/// and the leader's log was retired past the rejection's hint, leaves the
/// follower where it is; a follower is called behind only after a probe
/// at the log's base is refused.
#[test]
fn a_delayed_rejection_does_not_strand_a_follower() {
    let mut a = core_of(1, &[1; 10], 1);
    let sent = elect(&mut a, id(3));
    let term = a.term();
    let (_, s1, prev, _, _) = sent.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!(prev.index, 10);
    // B's log ends at 5: it refuses the probe, and the refusal is repeated
    // in the network.
    let refusal = reply(2, term, s1, None, 5);
    let (_, s2, prev, _, last) = appends(&a.receive(refusal.clone()))[0];
    assert_eq!((prev.index, last), (5, 7));
    let (_, s3, _, _, last) = appends(&a.receive(reply(2, term, s2, Some(7), 7)))[0];
    assert_eq!(last, 9);
    let (_, _, prev, _, last) = appends(&a.receive(reply(2, term, s3, Some(9), 9)))[0];
    assert_eq!((prev.index, last), (9, 11));
    a.compacted(Pos { index: 8, term: 1 });
    // The repeated refusal answers a request already answered.
    assert!(a.receive(refusal).is_empty());
    assert!(!a.behind().contains(&id(2)));
    // A refusal that truly asks for what is retired comes only from a later
    // start of B, which makes the leader forget what B held: a probe at the
    // base first, and behind only once that is refused.
    let out = appends(&a.tick());
    let (_, s4, prev, _, _) = out.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!(prev.index, 11);
    let restarted = |seq, hint| Message {
        start: 2,
        ..reply(2, term, seq, None, hint)
    };
    // The answer to a request of B's earlier start is passed over.
    assert!(appends(&a.receive(restarted(s4, 5))).is_empty());
    let out = appends(&a.tick());
    let (_, s5, prev, _, _) = out.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!(prev.index, 11);
    let (_, s6, prev, _, _) = appends(&a.receive(restarted(s5, 5)))[0];
    assert_eq!(prev.index, 8);
    assert!(!a.behind().contains(&id(2)));
    a.receive(restarted(s6, 5));
    assert!(a.behind().contains(&id(2)));
}

/// After an action fails, a member takes nothing more: the vote queued
/// after the failed record of it is not sent (section 7.1).
#[test]
fn a_member_takes_nothing_after_a_failed_action() {
    let sims: Vec<Sim> = (0..3).map(|i| Sim::new(900 + i)).collect();
    let ids: Vec<ServerId> = sims
        .iter()
        .map(|s| Store::create_member(Arc::new(s.fs()), &s.random(), STORE).unwrap())
        .collect();
    let cfg = Config {
        me: ids[1],
        members: ids.clone(),
        election: 10,
        heartbeat: 3,
        batch: 3,
        batch_bytes: 16 << 10,
        seed: 1,
        plant: None,
    };
    let (mut b, _) = Member::open(Arc::new(sims[1].fs()), STORE, cfg).unwrap();
    let ask = Message {
        from: ids[0],
        to: ids[1],
        term: 1,
        start: 1,
        body: Body::Vote {
            last: Pos::default(),
        },
    };
    assert!(b.receive(ask.clone()));
    assert_eq!(b.waiting(), 2);
    sims[1].place(sims[1].steps() + 1, Fault::FailWrite);
    assert!(b.perform().is_err());
    assert!(matches!(b.perform(), Err(WriteError::Stopped(_))));
    assert!(b.stopped().is_some());
    assert!(!b.receive(ask));
    assert!(!b.tick());
    assert!(b.checkpoint().is_err());
}

/// A follower that is behind by entries near the largest payload, more of
/// them than its log can hold in one write, catches up (the leader's
/// appends carry up to eight).
#[test]
fn a_follower_catches_up_through_entries_larger_than_a_batch() {
    let store = store::Config {
        pages: crate::pagefile::Config {
            dirty_budget: 1 << 14,
            ..STORE.pages
        },
        ..STORE
    };
    let setup = Setup {
        members: 3,
        steps: 0,
        plant: None,
        store,
        batch: Some((8, 64 << 10)),
    };
    let mut w = World::new(11, setup).unwrap();
    w.calm();
    w.settle().unwrap();
    let lead = w.leading().unwrap();
    let b = (0..3).find(|i| *i != lead).unwrap();
    w.nodes[b].member = None;
    w.nodes[b].down_until = u64::MAX;
    // A file, then sixteen entries of about 3.9 KiB, committed by the
    // other two.
    let next = w.nodes[lead].member.as_ref().unwrap().core.last().index + 1;
    let added = FileEvent::Added {
        name: b"large".to_vec(),
        flags: 0,
        page0: vec![7; 16],
    };
    assert!(w.nodes[lead]
        .member
        .as_mut()
        .unwrap()
        .propose(next, Payload::File(added)));
    for k in 0..17u8 {
        let committed = |w: &World| {
            let c = &w.nodes[lead].member.as_ref().unwrap().core;
            c.serving() && c.committed() == c.last().index
        };
        assert!(w.run_until(100, committed).unwrap(), "entry {k}");
        if k == 16 {
            break;
        }
        let change = Change {
            edits: vec![Edit::leaf(vec![vec![k]], vec![k; 3_900])],
            ..Change::default()
        };
        let member = w.nodes[lead].member.as_mut().unwrap();
        assert!(member.propose(next, Payload::Change(change)));
    }
    w.nodes[b].down_until = 0;
    w.settle().unwrap();
    let lead = w.leading().unwrap();
    let leader = &w.nodes[lead].member.as_ref().unwrap().core;
    let follower = &w.nodes[b].member.as_ref().unwrap().core;
    assert!(!leader.behind().contains(&follower.me()));
    assert_eq!(follower.published(), leader.committed());
}
