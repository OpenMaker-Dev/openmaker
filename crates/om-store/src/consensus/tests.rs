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
//! After every step it checks, against an oracle of what was answered as
//! committed:
//! - at most one leader per term;
//! - a leader is elected only with every entry answered as committed in
//!   its log;
//! - the committed position a member records is an entry answered as
//!   committed, with the same hash;
//! - a member publishes only entries answered as committed, and every
//!   member at the same entry has the same root hashes;
//! - a view shows only what is committed, and goes on reading the same;
//! - every member starts from what the faults left, and every error a
//!   member's store returns follows a fault its disk injected: an
//!   operation the store refuses is a defect of the core.
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

#[derive(Clone, Copy, Debug)]
struct Setup {
    members: usize,
    steps: u64,
    plant: Option<Plant>,
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
    committed: u64,
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
    /// Its disk's faults when its store last opened.
    faults_at_open: u64,
    seen_committed: u64,
    seen_published: u64,
    was_leader: bool,
}

/// What was answered as committed: entry `i` at `[i - 1]`, its term and
/// hash; the root hashes of the state at each entry a member published;
/// the leader of each term.
#[derive(Default)]
struct Oracle {
    committed: Vec<(u64, [u8; 32])>,
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
    /// The most entries an append carries, drawn for the schedule: one
    /// sends a new leader's entries of earlier terms on their own.
    batch: u64,
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

type Outcome<T = ()> = Result<T, String>;

impl World {
    fn new(seed: u64, setup: Setup) -> Outcome<World> {
        let mut nodes = Vec::new();
        for i in 0..setup.members {
            let sim = Sim::new(seed.wrapping_mul(31).wrapping_add(i as u64 + 1));
            let fs: Arc<dyn crate::sys::Fs> = Arc::new(sim.fs());
            let (store, _) = Store::create_member(fs, &sim.random(), STORE)
                .map_err(|e| format!("member {i} was not made: {e}"))?;
            nodes.push((sim, store.server(), store));
        }
        let ids: Vec<ServerId> = nodes.iter().map(|n| n.1).collect();
        let mut rng = Rng::stream(seed, 7);
        // One schedule in three churns leaders: short timeouts, small
        // appends, and new leaders often cut off (Raft's figure 8).
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
            batch,
            timing,
            churn,
            after_answer: 0,
            after_append: 0,
            rng,
            now: 0,
            step: 0,
            nodes: Vec::new(),
            ids,
            flight: Vec::new(),
            seq: 0,
            split: None,
            faults: true,
            storm: STORMS[0],
            oracle: Oracle::default(),
            client: Client::default(),
            views: Vec::new(),
            cov: Coverage::default(),
        };
        for (i, (sim, _, store)) in nodes.into_iter().enumerate() {
            let member = Member::new(store, w.config(i))
                .map_err(|e| format!("member {i} did not start: {e}"))?;
            sim.set_rates(disk_rates());
            w.nodes.push(Node {
                faults_at_open: disk_faults(&sim),
                sim,
                member: Some(member),
                down_until: 0,
                seen_committed: 0,
                seen_published: 0,
                was_leader: false,
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
            batch: self.batch,
            seed: self.seed,
            plant: self.setup.plant,
        }
    }

    fn fail(&self, what: impl std::fmt::Display) -> String {
        format!(
            "seed {} ({} members), step {}, tick {}: {what}",
            self.seed, self.setup.members, self.step, self.now
        )
    }

    /// Runs `f` on member `i`'s store with its disk's faults off: the
    /// checks read, and must neither fail nor change what the disk draws
    /// next differently from one run to the next.
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
        let member = self.nodes[i].member.as_mut().unwrap();
        let cut = matches!(member.queue.front(), Some(Action::Cut { .. }));
        let result = member.perform();
        if result.is_ok() && self.power_lost(i) {
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
                if let Some(c) = done.committed {
                    self.answered(i, c)?;
                }
                self.check(i)?;
                let storm = self.faults && self.storm.weight > 0;
                // A leader cut off right after it sent its first entry, or
                // right after it answered a commit, before what follows
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
            Err(e) => self.failed(i, e.to_string()),
        }
    }

    /// Whether member `i`'s disk lost power in a call that returned: a
    /// failure in work the store does not report, such as freeing pages
    /// after a checkpoint. The process is gone all the same.
    fn power_lost(&mut self, i: usize) -> bool {
        if !self.nodes[i].sim.halted() {
            return false;
        }
        let down_until = self.now + 1 + self.rng.below(8);
        let node = &mut self.nodes[i];
        node.member = None;
        node.down_until = down_until;
        true
    }

    /// Member `i`'s store returned an error: its disk's fault, or a defect.
    fn failed(&mut self, i: usize, e: String) -> Outcome {
        let node = &self.nodes[i];
        if !node.sim.halted() && disk_faults(&node.sim) == node.faults_at_open {
            return Err(self.fail(format!(
                "member {i}'s store refused what its core asked, with no fault: {e}"
            )));
        }
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
        let node = &mut self.nodes[i];
        node.sim.restart();
        let before = disk_faults(&node.sim);
        let fs: Arc<dyn crate::sys::Fs> = Arc::new(node.sim.fs());
        let opened = Store::open_member(fs, STORE)
            .map_err(|e| e.to_string())
            .and_then(|(store, opened)| {
                Member::new(store, cfg)
                    .map(|m| (m, opened))
                    .map_err(|e| e.to_string())
            });
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
                node.faults_at_open = disk_faults(&node.sim);
                node.seen_committed = 0;
                node.seen_published = 0;
                node.was_leader = false;
                self.cov.restarts += 1;
                self.check(i)
            }
            Err(e) => {
                if !halted && !faulted {
                    return Err(self.fail(format!("member {i} did not start, with no fault: {e}")));
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
            .map_err(|e| self.fail(format!("the leader's view: {e}")))?;
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
        let member = self.nodes[i].member.as_mut().unwrap();
        let result = member.checkpoint();
        if result.is_ok() && self.power_lost(i) {
            return Ok(());
        }
        match result {
            Ok(true) => {
                self.cov.checkpoints += 1;
                self.check(i)
            }
            Ok(false) => Ok(()),
            Err(e) => self.failed(i, e.to_string()),
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

    /// A view is of an entry answered as committed, and its trees are
    /// those of every member's state at that entry.
    fn check_view(&self, view: &View) -> Outcome {
        let at = view.position();
        if at.index == 0 {
            return Ok(());
        }
        if self.oracle.hash(at.index) != Some(at.hash.0) {
            return Err(self.fail(format!(
                "a view shows entry {} that was not answered as committed",
                at.index
            )));
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
                return Err(self.fail(format!(
                    "a view of entry {} reads other trees than the state there",
                    at.index
                )));
            }
        }
        if let Err(e) = view.store_tree() {
            return Err(self.fail(format!("a held view no longer reads: {e}")));
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

    /// The oracle learns that leader `i` decided, or recorded, that
    /// `committed` is committed: the entries up to it are answered so.
    fn answered(&mut self, i: usize, committed: u64) -> Outcome {
        let max = self.oracle.max();
        if committed <= max {
            return self.check_entry(i, committed, "a committed position it recorded");
        }
        if max > 0 {
            self.check_entry(i, max, "the last entry answered as committed")?;
        }
        let read = self
            .quiet(i, |m| m.store.read_wire(max + 1, committed))
            .unwrap()
            .map_err(|e| self.fail(format!("leader {i}'s log: {e}")))?;
        if read.len() as u64 != committed - max || read[0].index != max + 1 {
            return Err(self.fail(format!(
                "leader {i} committed entry {committed} that its log does not hold after {max}"
            )));
        }
        for e in read {
            self.oracle.committed.push((e.term, e.hash));
        }
        Ok(())
    }

    /// Member `i`'s log holds entry `index` as answered.
    fn check_entry(&self, i: usize, index: u64, what: &str) -> Outcome {
        let want = self.oracle.hash(index);
        let have = self
            .quiet(i, |m| m.store.entry_hash(index))
            .unwrap()
            .map_err(|e| self.fail(format!("member {i}'s log: {e}")))?;
        if want.is_none() || have != want {
            return Err(self.fail(format!(
                "member {i}'s entry {index}, {what}, is not the entry answered as committed \
                 ({} answered)",
                self.oracle.max()
            )));
        }
        Ok(())
    }

    /// The checks after a step that involved member `i`.
    fn check(&mut self, i: usize) -> Outcome {
        let Some(member) = self.nodes[i].member.as_ref() else {
            return Ok(());
        };
        let core = &member.core;
        let (role, term, id) = (core.role(), core.term(), core.me());
        let durable = core.durable().index;
        let decided = core.committed();
        self.cov.highest_term = self.cov.highest_term.max(term);
        // A leader's decision commits: a majority holds the entries, whether
        // or not its record of the decision lands before a fault.
        if role == Role::Leader && decided > self.oracle.max() {
            self.answered(i, decided)?;
        }
        // At most one leader per term; a leader holds every entry answered
        // as committed.
        if role == Role::Leader && !self.nodes[i].was_leader {
            self.nodes[i].was_leader = true;
            match self.oracle.leaders.get(&term) {
                Some(other) if *other != id => {
                    return Err(self.fail(format!("two leaders in term {term}")));
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
                if durable < max {
                    return Err(self.fail(format!(
                        "member {i} was elected in term {term} with its log ending at {durable}, \
                         before entry {max} answered as committed"
                    )));
                }
                self.check_entry(i, max, "at its election")?;
            }
        }
        if role != Role::Leader {
            self.nodes[i].was_leader = false;
        }
        // The committed position it recorded was answered so.
        let member = self.nodes[i].member.as_ref().unwrap();
        let committed = member.store.committed_position();
        if committed > self.nodes[i].seen_committed {
            self.nodes[i].seen_committed = committed;
            self.check_entry(i, committed, "its recorded committed position")?;
        }
        // It publishes only what was answered as committed, as every other
        // member at that entry.
        let member = self.nodes[i].member.as_ref().unwrap();
        let published = member.store.published();
        if published.index != self.nodes[i].seen_published {
            let roots = member.store.root_hashes();
            self.nodes[i].seen_published = published.index;
            if published.index > 0 {
                if self.oracle.hash(published.index) != Some(published.hash.0) {
                    return Err(self.fail(format!(
                        "member {i} published entry {}, which was not answered as committed \
                         ({} answered)",
                        published.index,
                        self.oracle.max()
                    )));
                }
                match self.oracle.roots.get(&published.index) {
                    Some(known) if *known != roots => {
                        return Err(self.fail(format!(
                            "member {i}'s state at entry {} has other root hashes than another \
                             member's",
                            published.index
                        )));
                    }
                    Some(_) => {}
                    None => {
                        self.oracle.roots.insert(published.index, roots);
                    }
                }
            }
        }
        Ok(())
    }

    /// The faults stop and the network heals: every member starts, one
    /// leader serves, and every member holds and publishes everything
    /// committed, or is one the leader reports as behind.
    fn settle(&mut self) -> Outcome {
        self.faults = false;
        self.split = None;
        for n in &self.nodes {
            n.sim.set_rates(Rates::default());
        }
        for i in 0..self.nodes.len() {
            if self.nodes[i].member.is_none() {
                self.open(i)?;
                if self.nodes[i].member.is_none() {
                    return Err(self.fail(format!("member {i} did not start once faults stopped")));
                }
            }
        }
        for _ in 0..600 {
            // Everything due is delivered and done before the next tick.
            loop {
                let busy: Vec<usize> = self
                    .up()
                    .into_iter()
                    .filter(|i| !self.nodes[*i].member.as_ref().unwrap().idle())
                    .collect();
                if let Some(&i) = busy.first() {
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
            if let Some(behind) = self.settled() {
                self.cov.behind += behind;
                return Ok(());
            }
            self.step += 1;
            self.tick()?;
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
        Err(self.fail(format!("the cluster did not settle: {state:?}")))
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

/// Every planted defect is found by a seed of the default run, and that
/// seed fails again the same way.
#[test]
fn planted_defects_are_found() {
    for plant in [
        Plant::VoteUnflushed,
        Plant::CountOlderTerm,
        Plant::CommitUnchecked,
        Plant::CutAtChecked,
        Plant::PublishDurable,
    ] {
        let found = (0..SEEDS).find_map(|seed| {
            run(seed, setup(seed, STEPS, Some(plant)))
                .err()
                .map(|e| (seed, e))
        });
        let Some((seed, failure)) = found else {
            panic!("{plant:?} is not found in the default run's {SEEDS} seeds");
        };
        eprintln!("{plant:?}: {failure}");
        let again = run(seed, setup(seed, STEPS, Some(plant))).err();
        assert_eq!(
            again.as_deref(),
            Some(failure.as_str()),
            "{plant:?} replays"
        );
    }
}

/// How many seeds of a run find each planted defect, and the first.
#[test]
#[ignore = "a measurement: run in release, OM_STORE_SCALE to scale"]
fn planted_defects_found_by_how_many_seeds() {
    for plant in [
        Plant::VoteUnflushed,
        Plant::CountOlderTerm,
        Plant::CommitUnchecked,
        Plant::CutAtChecked,
        Plant::PublishDurable,
    ] {
        let seeds = seeds(SEEDS);
        // Which check found it: the failure without its numbers, with how
        // many seeds and the first.
        let mut kinds: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for seed in &seeds {
            if let Err(e) = run(*seed, setup(*seed, STEPS, Some(plant))) {
                let what = e.split_once(": ").map_or(e.as_str(), |(_, w)| w);
                let kind: String = what
                    .chars()
                    .map(|c| if c.is_ascii_digit() { '#' } else { c })
                    .collect();
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
