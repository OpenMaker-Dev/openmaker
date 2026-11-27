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
//! once a majority of the members have held term `t`'s chain through it
//! durable while `state` recorded term `t` on each (Raft's acknowledgement
//! in the entry's own term), as far as each was seen to hold it, up or
//! not, and so is every entry before it. After every step it checks
//! ([`Check`]):
//! - at most one leader per term;
//! - a leader decides an entry is committed only once the disks show it,
//!   and that it is the entry they show;
//! - a leader is elected only with every committed entry in its log;
//! - a leader never loses an entry of its own log while it leads;
//! - two logs that hold an entry of the same index and term hold the same
//!   entry, over the whole range they share, every entry of it readable;
//! - the committed position a member records is a committed entry;
//! - a member applies, and publishes, only committed entries, and every
//!   member's state after the same entry has the same root hashes, while
//!   running and while an open replays;
//! - a view shows only what is committed, and goes on reading the same;
//! - what a member sends agrees with its disk: its term, a vote it grants,
//!   the last entry a vote request gives, an acknowledgement within what
//!   its append checked, on that append's chain and no further than its
//!   durable entry;
//! - a vote goes only to a candidate that asked, with a log as up to date
//!   as the voter's, as the two disks say;
//! - a serving leader's term opens with an `empty` entry of its own,
//!   committed and published, and the leader of the newest term serves
//!   only with every entry committed before its term;
//! - on a healthy network, a follower of a stable leader reaches the
//!   leader's durable entry within a bound ([`World::watch_progress`]);
//! - every member starts from what the faults left, and every error a
//!   member's store returns follows a fault its disk injected during that
//!   very call: an operation the store refuses otherwise is a defect.
//!
//! At the end the faults stop and the network heals: the client writes at
//! every tick for a while ([`World::steady`]); every member must start,
//! one leader must serve, and every member must hold and publish
//! everything committed, or be one the leader reports as behind
//! ([`Core::behind`], state transfer not built). Then a minority goes down
//! for good, and the rest must commit a new entry unless the disks show
//! one of them behind.
//!
//! `OM_STORE_SCALE=<n>` runs `n` times as many seeds, `OM_STORE_SEEDS=<n>`
//! seeds 0 to `n - 1` and `OM_STORE_SEEDS=<a>..<b>` seeds `a` to `b - 1`,
//! `OM_STORE_SEED=<s>` seed `s` alone.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
    /// A member sent what its disk does not yet record: a term `state`
    /// does not hold, a vote it does not hold, an acknowledgement past the
    /// durable entry.
    DurableAnswer,
    /// A vote was granted to a candidate whose log is not as up to date as
    /// the voter's durable log, or ends before its committed position, as
    /// the two disks say.
    Grant,
    /// A leader served a client before its term's `empty` entry was
    /// committed and published, or, leading the newest term, before its
    /// state held every entry committed before its term.
    Serving,
    /// A follower that heard from a stable leader on a healthy network
    /// while writes went on did not reach the leader's durable end within
    /// the bound.
    Progress,
    /// In the steady phase, no leader was in office within the bound of the
    /// phase's start or of the last leader's fall; or, in its opening
    /// election, a candidate's timer ran out while the answer to a vote
    /// request it sent under that election's delay was on its way, on a
    /// network fast enough for it.
    Election,
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
    /// Messages held back for several election timeouts.
    stalled: u64,
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
    /// Ends with a minority down for good in which the rest committed a new
    /// entry, and in which they could not because one of them is behind.
    minority_progress: u64,
    minority_behind: u64,
    /// Progress targets reached, and followers the disks confirmed behind
    /// or blocked while watched.
    progress_reached: u64,
    progress_excused: u64,
    /// Steady phases that closed with a verdict on every member.
    steady_judged: u64,
    /// In steady phases: the longest time without a leader in office, in
    /// ticks, and in tenths of an election attempt (the unit of
    /// [`World::election_deadline`]).
    leaderless_ticks: u64,
    leaderless_tenths: u64,
    /// Schedules in which one member ran with a smaller largest entry
    /// until the steady phase, and the size refusals sent.
    small_limit: u64,
    size_refusals: u64,
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
        self.stalled += o.stalled;
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
        self.minority_progress += o.minority_progress;
        self.minority_behind += o.minority_behind;
        self.progress_reached += o.progress_reached;
        self.progress_excused += o.progress_excused;
        self.steady_judged += o.steady_judged;
        self.leaderless_ticks = self.leaderless_ticks.max(o.leaderless_ticks);
        self.leaderless_tenths = self.leaderless_tenths.max(o.leaderless_tenths);
        self.small_limit += o.small_limit;
        self.size_refusals += o.size_refusals;
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
    /// The last vote request delivered to it: from whom, in which term, the
    /// candidate's last entry.
    asked: Option<(ServerId, u64, Pos)>,
    /// The last append delivered to it.
    appended: Option<Appended>,
    /// The tick it last opened.
    up_since: u64,
}

/// The simulator's own record (module docs): the committed entries, entry
/// `i` at `[i - 1]` with its term and hash; for each term, the one chain of
/// that term's entries seen on any disk (one leader makes them), by index;
/// for each member and term, how far it held that chain durable while
/// `state` recorded that term, which stays evidence when the member is
/// down later; the root hashes of the state after each applied entry; the
/// leader of each term.
#[derive(Default)]
struct Oracle {
    committed: Vec<(u64, [u8; 32])>,
    chains: BTreeMap<u64, BTreeMap<u64, [u8; 32]>>,
    /// Each term's first entry seen on any disk, its index and kind: the
    /// leader's opening entry, kept after logs retire it.
    opening: BTreeMap<u64, (u64, u16)>,
    /// The payload size of each entry read, by index and hash: what a
    /// refusal for size is checked against once logs retire the entry.
    sizes: BTreeMap<(u64, [u8; 32]), u64>,
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

/// An append as a member took it: from whom, what it repeats, the entry
/// it named, and its entries' indexes and hashes.
type Appended = (ServerId, Echo, Pos, Vec<(u64, [u8; 32])>);

/// A follower watched for progress: since which tick, the leader's durable
/// entry then (its target), the deadline ([`World::deadline`]), fixed when
/// the target is armed, and which leader in which term.
type Watch = (u64, u64, u64, usize, u64);

/// The history the experiment review's P1-2 needs, brought about step by
/// step in the schedules that draw it: a serving leader `l` and one
/// follower `f` cut off together, the follower's acknowledgements held
/// back; the rest elect a leader, and the split heals so that it cuts
/// `l`'s entries; then `l`, `f` and one third member are cut off from that
/// leader and the fifth member, until `l` leads again with that one real
/// follower and has sent to `f`; then the held acknowledgements arrive.
/// Each stage gives up after a while.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Return {
    Idle,
    /// `l` and `f` apart from the rest, `l` of term `term`.
    Apart {
        l: usize,
        f: usize,
        term: u64,
        since: u64,
    },
    /// The split healed; `other` leads the rest.
    Cut {
        l: usize,
        f: usize,
        other: usize,
        since: u64,
    },
    /// `l`, `f` and a third member `z` apart from the rest, so that `l`
    /// can lead with one real follower; a `z` that leads is swapped for the
    /// fifth member.
    Elect {
        l: usize,
        f: usize,
        z: usize,
        after: u64,
        since: u64,
    },
}

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
    /// Whether the network lags: one message delivered a step whatever is
    /// due, so that deliveries fall behind their time.
    lag: bool,
    /// Whether the schedule makes leaders return (five members,
    /// [`Return`]).
    ret: bool,
    /// Where the returning-leader history stands.
    back: Return,
    /// A follower's acknowledgements held back meanwhile.
    held: Vec<Message>,
    /// For each member, while the network is healthy and a leader stable
    /// ([`World::watch_progress`]).
    watch: Vec<Option<Watch>>,
    /// Whether the client writes at every tick of [`World::run_until`].
    writing: bool,
    /// At the close of the steady phase, the members whose armed progress
    /// target still awaits its verdict; and whether one was dropped
    /// without one.
    closing: Option<Vec<bool>>,
    /// In the steady phase, since when no leader has been in office.
    leaderless: Option<u64>,
    /// The steady phase's opening election is under way: every message
    /// takes [`World::electing_delay`] ticks.
    electing: bool,
    /// The vote requests sent under that rule: candidate, voter and term.
    ruled: BTreeSet<(ServerId, ServerId, u64)>,
    /// The campaigns (candidate and term) under way when the steady phase
    /// opened: one of them that wins was not elected under that rule.
    begun: BTreeSet<(ServerId, u64)>,
    /// The member that runs with a smaller largest entry until the steady
    /// phase, when it starts again with the cluster's (an operator's
    /// mistake and its correction, section 7.1).
    small: Option<usize>,
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
        // One schedule in five gives one member a smaller largest entry
        // until the steady phase. Drawn from a stream of its own, so that
        // the other schedules are the ones they would be without it.
        let small = {
            let mut r = Rng::stream(seed, 11);
            r.one_in(5).then(|| r.below(setup.members as u64) as usize)
        };
        let mut rng = Rng::stream(seed, 7);
        // One schedule in three churns leaders: short timeouts, one entry
        // an append, and new leaders often cut off (Raft's figure 8).
        let churn = rng.one_in(3);
        // One schedule in six on a network that lags on purpose.
        let lag = rng.one_in(6);
        let ret = setup.members >= 5 && rng.one_in(2);
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
            lag,
            ret,
            back: Return::Idle,
            held: Vec::new(),
            watch: vec![None; setup.members],
            writing: false,
            closing: None,
            leaderless: None,
            electing: false,
            ruled: BTreeSet::new(),
            begun: BTreeSet::new(),
            small,
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
                asked: None,
                appended: None,
                up_since: 0,
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
            max_entry: if self.small == Some(i) {
                SMALL_ENTRY
            } else {
                u64::from(self.setup.store.log.max_payload)
            },
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
        self.steady()?;
        self.settle()
    }

    /// The faults stop and every member starts, and the client goes on
    /// writing at every tick for `20 * election` ticks, on a network that
    /// drops nothing and delays every message by one or two ticks: the
    /// progress watch holds every follower to the leader's durable end
    /// while the writes go on ([`World::watch_progress`]).
    fn steady(&mut self) -> Outcome {
        self.calm();
        for i in 0..self.nodes.len() {
            if self.nodes[i].member.is_none() {
                self.open(i)?;
            }
        }
        self.writing = true;
        self.leaderless = None;
        let election = self.timing.0;
        // It opens with an election: every member that believes it leads
        // stops and starts again (one deposed that has not heard yet too),
        // and until a leader is in office whose campaign began after this,
        // every message takes the same time, a vote's round trip less than
        // the shortest timeout. A campaign under way now, its votes asked
        // on the faults' network, may still win: that leader stops and
        // starts again too ([`World::watch_election`]). The election check
        // bounds it.
        let leaders: Vec<usize> = self
            .up()
            .into_iter()
            .filter(|i| self.core_of(*i).role() == Role::Leader)
            .collect();
        for l in leaders {
            self.restart(l)?;
        }
        self.begun = self
            .up()
            .into_iter()
            .filter(|i| self.core_of(*i).role() == Role::Candidate)
            .map(|i| (self.ids[i], self.core_of(i).term()))
            .collect();
        self.electing = true;
        self.ruled.clear();
        self.run_until(1000 * election, |w| !w.electing)?;
        // The member given a smaller largest entry keeps it a while under
        // that leader, which reports it blocked, then starts again with the
        // cluster's (the operator's mistake and its correction).
        if let Some(m) = self.small {
            self.run_until(5 * election, |_| false)?;
            self.small = None;
            self.cov.small_limit += 1;
            self.restart(m)?;
        }
        self.run_until(20 * election, |_| false)?;
        // The close: the writes go on until every member has a verdict
        // under a leader in office, a target reached or the disks
        // confirming it behind or blocked. The election and progress
        // deadlines bound it; the cap only guards the loop.
        self.closing = Some(vec![true; self.nodes.len()]);
        let done = |w: &World| w.closing.as_ref().is_some_and(|p| !p.contains(&true));
        let closed = self.run_until(1000 * election, done)?;
        self.closing = None;
        if !closed {
            return Err(self.fail(
                Check::Progress,
                "the steady phase did not close: a member never had its verdict",
            ));
        }
        self.cov.steady_judged += 1;
        self.writing = false;
        self.leaderless = None;
        Ok(())
    }

    /// Member `i` stops and starts again, with no fault.
    fn restart(&mut self, i: usize) -> Outcome {
        self.nodes[i].member = None;
        self.nodes[i].down_until = self.now;
        self.open(i)
    }

    /// In the steady phase, a leader is in office (a leader of the highest
    /// term any member has) within [`World::election_deadline`] ticks of
    /// the phase's start and of the last leader's fall.
    fn watch_election(&mut self) -> Outcome {
        if !self.writing {
            self.leaderless = None;
            return Ok(());
        }
        let up = self.up();
        let top = up.iter().map(|i| self.core_of(*i).term()).max();
        let in_office = up.iter().copied().find(|i| {
            let c = self.core_of(*i);
            c.role() == Role::Leader && Some(c.term()) == top
        });
        if let Some(l) = in_office {
            if let Some(since) = self.leaderless.take() {
                let spell = self.now - since;
                self.cov.leaderless_ticks = self.cov.leaderless_ticks.max(spell);
                let tenths = spell * 10 / (self.election_deadline() / ATTEMPTS);
                self.cov.leaderless_tenths = self.cov.leaderless_tenths.max(tenths);
            }
            if self.electing && self.begun.contains(&(self.ids[l], self.core_of(l).term())) {
                // Elected by a campaign begun before the opening election,
                // not under its rule: it stops and starts again, a leader's
                // fall, and the election goes on.
                return self.restart(l);
            }
            self.electing = false;
            return Ok(());
        }
        let since = *self.leaderless.get_or_insert(self.now);
        let deadline = self.election_deadline();
        if self.now - since > deadline {
            let state: Vec<String> = up
                .iter()
                .map(|i| {
                    let c = self.core_of(*i);
                    format!("{i}: {:?} t{} last {:?}", c.role(), c.term(), c.last())
                })
                .collect();
            return Err(self.fail(
                Check::Election,
                format!(
                    "no leader in office for {} ticks on a healthy network since tick {since}, \
                     past the bound of {deadline}: {state:?}",
                    self.now - since
                ),
            ));
        }
        Ok(())
    }

    /// How long an election may take in the steady phase: [`ATTEMPTS`]
    /// attempts, each a member's timeout running out (at most `2 *
    /// election - 1` ticks) and two round trips, the pre-vote's and the
    /// vote's, each at most twice the phase's largest delay. Randomized
    /// timeouts make split votes rare but not impossible, which the
    /// attempts allow for.
    fn election_deadline(&self) -> u64 {
        let delay = self.electing_delay().max(2);
        ATTEMPTS * (2 * self.timing.0 - 1 + 2 * 2 * delay)
    }

    /// The delay of every message during the steady phase's opening
    /// election: as long as it may be for a vote's round trip to take
    /// fewer ticks than the shortest timeout (section 7.1), so that a
    /// candidate's timer never runs out before its vote's answers arrive.
    fn electing_delay(&self) -> u64 {
        ((self.timing.0 - 1) / 2).max(1)
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
        // The work keeps up with the clock: weights grow with the members
        // busy and the messages due, but on a lagging network.
        let due_count = self.flight.iter().filter(|f| f.0 <= self.now).count() as u64;
        let (work, deliveries) = if self.lag {
            (400, 300)
        } else {
            (400 * busy.len() as u64, 300 * due_count)
        };
        if !busy.is_empty() {
            events.push((b'a', work));
        }
        if due {
            events.push((b'd', deliveries));
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
        }?;
        if self.ret && self.faults {
            self.steer();
        }
        Ok(())
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
                    if matches!(m.body, Body::AppendTooLarge { .. }) {
                        self.cov.size_refusals += 1;
                    }
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
                    self.check_answer(i, &m)?;
                    let carries =
                        matches!(&m.body, Body::Append { entries, .. } if !entries.is_empty());
                    let serving = self.nodes[i].member.as_ref().unwrap().core.serving();
                    if self.ret
                        && self.faults
                        && carries
                        && serving
                        && self.back == Return::Idle
                        && self.rng.one_in(8)
                    {
                        // The leader and this follower alone; the follower's
                        // acknowledgements held back.
                        let f = self.who(m.to);
                        let mut side = vec![false; self.nodes.len()];
                        side[i] = true;
                        side[f] = true;
                        self.split = Some((side, u64::MAX));
                        let term = self.nodes[i].member.as_ref().unwrap().core.term();
                        self.back = Return::Apart {
                            l: i,
                            f,
                            term,
                            since: self.now,
                        };
                        self.cov.splits += 1;
                        self.cov.faults += 1;
                    }
                    let success = matches!(
                        m.body,
                        Body::AppendReply {
                            matched: Some(_),
                            ..
                        }
                    );
                    let storm = self.faults && self.storm.weight > 0;
                    if success && self.churn && storm && self.rng.one_in(6) {
                        // An acknowledgement held back for several election
                        // timeouts, its leader cut off at once: the leader
                        // may be deposed, cut and elected again before it
                        // arrives.
                        let leader = self.ids.iter().position(|x| *x == m.to).unwrap();
                        let delay = self.timing.0 * (3 + self.rng.below(6));
                        self.seq += 1;
                        self.cov.sent += 1;
                        self.cov.stalled += 1;
                        self.cov.faults += 1;
                        self.flight.push((self.now + delay, self.seq, m));
                        let mut side = vec![false; self.nodes.len()];
                        side[leader] = true;
                        self.split = Some((side, self.now + 3 + self.rng.below(50)));
                        self.cov.splits += 1;
                    } else {
                        self.send(m);
                    }
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
                } else if (storm || diverged && self.faults)
                    && self.back == Return::Idle
                    && self.rng.one_in(isolate)
                {
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

    /// Moves the returning-leader history on ([`Return`]), after each step.
    fn steer(&mut self) {
        let limit = 20 * self.timing.0;
        let leading = |w: &World, i: usize| {
            w.nodes[i]
                .member
                .as_ref()
                .is_some_and(|m| m.core.role() == Role::Leader)
        };
        let apart = |w: &mut World, members: &[usize]| {
            let mut side = vec![false; w.nodes.len()];
            for &i in members {
                side[i] = true;
            }
            w.split = Some((side, u64::MAX));
            w.cov.splits += 1;
            w.cov.faults += 1;
        };
        let n = self.nodes.len();
        match self.back {
            Return::Idle => {}
            Return::Apart { l, f, term, since } => {
                // The rest lead themselves: heal, so that their leader cuts
                // `l`'s entries.
                let other = (0..self.nodes.len()).find(|j| {
                    *j != l
                        && *j != f
                        && self.nodes[*j]
                            .member
                            .as_ref()
                            .is_some_and(|m| m.core.serving() && m.core.term() > term)
                });
                if let Some(other) = other {
                    self.split = None;
                    self.back = Return::Cut {
                        l,
                        f,
                        other,
                        since: self.now,
                    };
                } else if self.now > since + limit {
                    self.release();
                }
            }
            Return::Cut { l, f, other, since } => {
                let theirs = self.nodes[other].member.as_ref().map(|m| m.core.term());
                let cut = self.nodes[l].member.as_ref().is_some_and(|m| {
                    theirs.is_some_and(|t| m.core.term() >= t && m.core.last().term >= t)
                });
                if cut {
                    let after = theirs.unwrap();
                    let z = (0..n).find(|j| ![l, f, other].contains(j)).unwrap();
                    apart(self, &[l, f, z]);
                    self.back = Return::Elect {
                        l,
                        f,
                        z,
                        after,
                        since: self.now,
                    };
                } else if self.now > since + limit {
                    self.release();
                }
            }
            Return::Elect {
                l,
                f,
                z,
                after,
                since,
            } => {
                if leading(self, z) {
                    // Swap the third member for the fifth.
                    let y = (0..n)
                        .find(|j| ![l, f, z].contains(j) && !leading(self, *j))
                        .unwrap_or(z);
                    if y != z {
                        apart(self, &[l, f, y]);
                        self.back = Return::Elect {
                            l,
                            f,
                            z: y,
                            after,
                            since,
                        };
                    }
                }
                // `l` leads again and has sent `f` its `empty` entry.
                let led = self.nodes[l].member.as_ref().is_some_and(|m| {
                    m.core.role() == Role::Leader
                        && m.core.term() > after
                        && m.core
                            .progress
                            .get(&self.ids[f])
                            .is_some_and(|p| p.sent >= m.core.empty)
                });
                if led || self.now > since + limit {
                    self.release();
                }
            }
        }
    }

    /// The held acknowledgements go into the network, due now, and the
    /// history is over.
    fn release(&mut self) {
        for m in std::mem::take(&mut self.held) {
            self.seq += 1;
            self.flight.push((self.now, self.seq, m));
        }
        if self.back != Return::Idle {
            self.split = None;
        }
        self.back = Return::Idle;
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

    /// What member `i` sends agrees with what its disk records: the
    /// message's term is the term `state` holds; a granted vote is the vote
    /// it holds, given to a candidate as up to date as the voter's durable
    /// log and not ending before its committed position; an acknowledgement
    /// is at or before the durable entry.
    fn check_answer(&self, i: usize, m: &Message) -> Outcome {
        let store = &self.nodes[i].member.as_ref().unwrap().store;
        let (term, vote) = store.saved_term();
        let (durable, durable_term, _) = store.durable_entry();
        if m.term != term {
            return Err(self.fail(
                Check::DurableAnswer,
                format!(
                    "member {i} sent a message of term {}; its state records {term}",
                    m.term
                ),
            ));
        }
        let own = Pos {
            index: durable,
            term: durable_term,
        };
        match &m.body {
            // A candidate's claim to its log is its disk's.
            Body::Vote { last } | Body::PreVote { last, .. } if *last != own => {
                return Err(self.fail(
                    Check::DurableAnswer,
                    format!(
                        "member {i} asked for a vote with its log ending at {last:?}; its disk \
                         ends at {own:?}"
                    ),
                ));
            }
            Body::VoteReply { granted: true } => {
                if vote != Some(m.to) {
                    return Err(self.fail(
                        Check::DurableAnswer,
                        format!("member {i} granted a vote its state does not record"),
                    ));
                }
                // Every grant answers a request, whose candidate's log is
                // as up to date as the voter's durable log.
                let Some((from, asked_term, last)) = self.nodes[i].asked else {
                    return Err(self.fail(
                        Check::Grant,
                        format!("member {i} granted a vote nobody asked it for"),
                    ));
                };
                if from != m.to || asked_term != m.term {
                    return Err(self.fail(
                        Check::Grant,
                        format!("member {i} granted a vote other than the one it was asked for"),
                    ));
                }
                let committed = store.committed_position();
                if !last.at_least(&own) || last.index < committed {
                    return Err(self.fail(
                        Check::Grant,
                        format!(
                            "member {i} granted its vote to a log ending at {last:?}; its own \
                             ends at {own:?}, committed through {committed}"
                        ),
                    ));
                }
            }
            Body::AppendReply {
                echo,
                matched: Some(k),
                ..
            } => {
                let k = *k;
                if k > durable {
                    return Err(self.fail(
                        Check::DurableAnswer,
                        format!(
                            "member {i} acknowledged entry {k}; its durable entry is {durable}"
                        ),
                    ));
                }
                // Within what the append it answers checked, and on the
                // chain that append named.
                let Some((from, took, prev, carried)) = &self.nodes[i].appended else {
                    return Err(self.fail(
                        Check::DurableAnswer,
                        format!("member {i} acknowledged an append it did not take"),
                    ));
                };
                let last = carried.last().map_or(prev.index, |e| e.0);
                if *from != m.to || took != echo || k < prev.index || k > last {
                    return Err(self.fail(
                        Check::DurableAnswer,
                        format!(
                            "member {i} acknowledged entry {k}; the append it answers checked \
                             {} to {last}",
                            prev.index
                        ),
                    ));
                }
                // At or past the log's base, against the disk (at the base,
                // the checkpoint's entry); before it, against the record of
                // what is committed, which is all the disk keeps of it.
                let base = store.log_base().index;
                let held = if k >= base {
                    self.quiet(i, |m| m.store.entry_at(k))
                        .unwrap()
                        .map_err(|e| self.fail(Check::DurableAnswer, format!("its log: {e}")))?
                } else {
                    k.checked_sub(1)
                        .and_then(|at| self.oracle.committed.get(at as usize))
                        .copied()
                };
                let right = if k == prev.index {
                    held.is_some_and(|(t, _)| t == prev.term)
                } else {
                    let want = carried.iter().find(|e| e.0 == k).map(|e| e.1);
                    want.is_some() && held.map(|h| h.1) == want
                };
                if !right {
                    return Err(self.fail(
                        Check::DurableAnswer,
                        format!(
                            "member {i} acknowledged entry {k}, which is not the entry the \
                             append it answers carried there"
                        ),
                    ));
                }
            }
            _ => {}
        }
        Ok(())
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
            // election able to finish; but now and then, in a storm, a
            // message held back for several, as a stalled connection
            // delivers late: a reply of an earlier term arrives after a new
            // election.
            // An acknowledgement is held back more often when leaders churn:
            // a success of an earlier term arriving after the same member
            // leads again.
            let success = matches!(
                m.body,
                Body::AppendReply {
                    matched: Some(_),
                    ..
                }
            );
            let held = match self.back {
                Return::Apart { l, f, .. }
                | Return::Cut { l, f, .. }
                | Return::Elect { l, f, .. } => {
                    success && self.who(m.from) == f && self.who(m.to) == l
                }
                Return::Idle => false,
            };
            if held {
                self.cov.stalled += 1;
                self.held.push(m.clone());
                continue;
            }
            let stall = if success && self.churn { 25 } else { 300 };
            let delay = if self.faults && storm.weight > 0 && self.rng.one_in(stall) {
                self.cov.stalled += 1;
                self.cov.faults += 1;
                self.timing.0 * (2 + self.rng.below(6))
            } else if self.writing && self.electing {
                // The steady phase's opening election: every message takes
                // the same time, a round trip less than the shortest
                // timeout. The vote requests sent so are the ones its
                // timing check judges.
                if matches!(m.body, Body::Vote { .. }) {
                    self.ruled.insert((m.from, m.to, m.term));
                }
                self.electing_delay()
            } else if self.writing {
                // The rest of the steady phase: one or two ticks, so that
                // appends to a follower cross, and no answer arrives before
                // the next entry's append has gone out.
                1 + self.rng.below(2)
            } else {
                self.rng.below(storm.delay.min(self.timing.0) + 1)
            };
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
        if let Body::Vote { last } = m.body {
            self.nodes[i].asked = Some((m.from, m.term, last));
        }
        if let Body::Append { prev, entries, .. } = &m.body {
            let echo = Echo {
                term: m.term,
                start: m.start,
                prev: prev.index,
            };
            let carried = entries.iter().map(|e| (e.index, e.hash)).collect();
            self.nodes[i].appended = Some((m.from, echo, *prev, carried));
        }
        if self.electing && matches!(m.body, Body::VoteReply { granted: true }) {
            // A vote's round trip is shorter than the shortest timeout here:
            // a candidate whose request to this voter went under the opening
            // election's delay is still standing in that term when the grant
            // arrives (section 7.1). A request sent before, on the faults'
            // network, may come back late, even if the phase opened on the
            // tick it was sent.
            let core = &self.nodes[i].member.as_ref().unwrap().core;
            let asked = self.ruled.contains(&(m.to, m.from, m.term));
            if asked && core.term() == m.term && core.role() == Role::PreCandidate {
                return Err(self.fail(
                    Check::Election,
                    format!(
                        "member {i}'s timer ran out while the answers to its vote of term {} \
                         were on their way: a round trip takes {} ticks, its shortest \
                         timeout {}",
                        m.term,
                        2 * self.electing_delay(),
                        self.timing.0
                    ),
                ));
            }
        }
        let member = self.nodes[i].member.as_mut().unwrap();
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
            // In the fault phase a tick a busy member refuses is dropped, as
            // one more fault (a slow clock); `run_until` ticks only idle
            // members.
            let member = self.nodes[i].member.as_mut().unwrap();
            if member.tick() {
                self.check(i)?;
            }
        }
        self.watch_progress()?;
        self.watch_election()
    }

    /// While the network is healthy (no fault drawn, no split, every
    /// member up, delivery keeping up) and one leader leads the newest term
    /// unchanged, every follower it does not report behind or blocked
    /// comes to hold the leader's durable entry, as it was when the watch
    /// began, by a deadline ([`World::deadline`]). The watch starts again
    /// from each entry reached, so it holds while writes go on.
    fn watch_progress(&mut self) -> Outcome {
        let healthy = self.split.is_none()
            && self.back == Return::Idle
            && self.nodes.iter().all(|n| n.member.is_some())
            && (!self.faults || (self.storm.weight == 0 && !self.lag));
        let newest = self.oracle.leaders.keys().next_back().copied();
        let leader = self
            .leading()
            .filter(|l| Some(self.core_of(*l).term()) == newest);
        let Some(l) = leader.filter(|_| healthy) else {
            for f in 0..self.nodes.len() {
                self.unwatch(f);
            }
            return Ok(());
        };
        let term = self.core_of(l).term();
        let to_durable = self.core_of(l).durable().index;
        for f in 0..self.nodes.len() {
            // A watch of another leader or term ends without a verdict.
            if f == l || self.watch[f].is_some_and(|w| w.3 != l || w.4 != term) {
                self.unwatch(f);
            }
            if f == l {
                // The leader is not judged.
                self.judged(f);
                continue;
            }
            if self.excused(l, f)? {
                if self.watch[f].take().is_some() {
                    self.cov.progress_excused += 1;
                }
                self.judged(f);
                continue;
            }
            let durable = self.nodes[f]
                .member
                .as_ref()
                .unwrap()
                .store
                .durable_entry()
                .0;
            if let Some((start, target, deadline, _, _)) = self.watch[f] {
                if durable >= target && self.meet(f, l).is_some_and(|m| m >= target) {
                    self.cov.progress_reached += 1;
                    self.judged(f);
                    self.watch[f] = None;
                } else if self.now >= deadline {
                    let knows = self.core_of(l).progress.get(&self.ids[f]).copied();
                    let reported = (
                        self.core_of(l).behind().contains(&self.ids[f]),
                        self.core_of(l)
                            .blocked()
                            .into_iter()
                            .find(|b| b.0 == self.ids[f]),
                    );
                    let fc = self.core_of(f);
                    let (fr, ft, fl) = (fc.role(), fc.term(), fc.last().index);
                    return Err(self.fail(
                        Check::Progress,
                        format!(
                            "member {f} heard from leader {l} of term {term} on a healthy \
                             network since tick {start}, and is at {durable}, short of \
                             {target}; the leader knows {knows:?}; reported (behind, \
                             blocked) {reported:?}, which the disks do not confirm; the \
                             follower is {fr:?} in term {ft}, last {fl}"
                        ),
                    ));
                }
            }
            // A new target, unless the phase is closing and the member has
            // its verdict.
            let pending = self.closing.as_ref().is_none_or(|p| p[f]);
            if self.watch[f].is_none() && pending {
                let deadline = self.deadline(f, l, to_durable);
                self.watch[f] = Some((self.now, to_durable, deadline, l, term));
            }
        }
        Ok(())
    }

    /// Member `f`'s watch ends without a verdict: in the close, it is
    /// watched again under the next leader in office.
    fn unwatch(&mut self, f: usize) {
        self.watch[f] = None;
    }

    /// Member `f`'s target has its verdict.
    fn judged(&mut self, f: usize) {
        if let Some(pending) = self.closing.as_mut() {
            pending[f] = false;
        }
    }

    /// Whether leader `l` reports follower `f` behind or blocked and the
    /// disks confirm it: what `f` lacks is retired from `l`'s log (their
    /// logs agree nowhere at or after `l`'s base), or the entry it refused
    /// was in `l`'s log (or is in the record, retired) and is larger than
    /// `f`'s largest.
    fn excused(&self, l: usize, f: usize) -> Outcome<bool> {
        let core = self.core_of(l);
        // `meet` finds only a place at or after both logs' bases: none
        // means the logs agree nowhere the leader still holds, so what the
        // follower lacks is retired there.
        if core.behind().contains(&self.ids[f]) && self.meet(f, l).is_none() {
            return Ok(true);
        }
        let Some((_, oversize)) = core.blocked().into_iter().find(|b| b.0 == self.ids[f]) else {
            return Ok(false);
        };
        // The entry's size: from the leader's log, or, once it retired the
        // entry (committed), from the record's entry there.
        let at = oversize.index;
        let size = if at > self.base_of(l) {
            let read = self
                .quiet(l, |m| m.store.read_wire(at, at, u64::MAX, u64::MAX))
                .unwrap()
                .map_err(|e| self.fail(Check::Progress, format!("member {l}'s log: {e}")))?;
            read.first()
                .filter(|e| e.index == at)
                .map(|e| e.payload.len() as u64)
        } else {
            self.oracle
                .hash(at)
                .and_then(|h| self.oracle.sizes.get(&(at, h)).copied())
        };
        let limit = self.config(f).max_entry;
        Ok(size.is_some_and(|size| size > limit))
    }

    /// By when follower `f` must hold leader `l`'s entry `target`: six
    /// election timeouts and ten ticks, and four ticks (a round trip of
    /// the slowest healthy network) for each entry of its log past where
    /// the two logs meet (a refusal steps back at least one run of a
    /// term) and for each batch it lacks from there. The disks say where
    /// the logs meet.
    fn deadline(&self, f: usize, l: usize, target: u64) -> u64 {
        let (election, batch) = (self.timing.0, self.batch.0.max(1));
        let durable = self.nodes[f]
            .member
            .as_ref()
            .unwrap()
            .store
            .durable_entry()
            .0;
        let meet = self.meet(f, l).unwrap_or(0);
        let back = durable.saturating_sub(meet) + 1;
        let forward = target.saturating_sub(meet) / batch + 1;
        self.now + 6 * election + 4 * (back + forward) + 10
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
                node.up_since = self.now;
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
        // While a returning leader's acknowledgements are held back, the
        // client writes only to it: its entries the others will cut grow
        // past what the next leader commits.
        let only = match self.back {
            Return::Apart { l, .. } | Return::Cut { l, .. } | Return::Elect { l, .. } => Some(l),
            Return::Idle => None,
        };
        let Some(i) = self.up().into_iter().find(|i| {
            // A client waits for its answers: at most four entries the
            // leader has not committed.
            let m = self.nodes[*i].member.as_ref().unwrap();
            only.is_none_or(|o| o == *i)
                && m.core.serving()
                && m.idle()
                && m.core.last().index < m.core.committed() + 4
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
            if self.small.is_some() && self.rng.one_in(5) {
                // An entry past the smaller largest entry.
                let path = vec![b"big".to_vec()];
                edits.push(Edit::leaf(path, vec![1; 2 * SMALL_ENTRY as usize]));
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

    /// A leader that serves has its term's `empty` entry in its log,
    /// committed (the record's entry there) and published: what lets it
    /// serve (section 7.1). The leader of the newest term also holds every
    /// entry committed before its term; a deposed leader that has not heard
    /// yet serves too, which Raft allows, so only the newest term's is held
    /// to that.
    fn check_serving(&self, i: usize) -> Outcome {
        if self.nodes[i].sim.halted() {
            // Its power failed in this step: it is taken down next.
            return Ok(());
        }
        let member = self.nodes[i].member.as_ref().unwrap();
        let term = member.core.term();
        let empty = member.core.empty;
        let published = member.store.published().index;
        let base = member.store.log_base().index;
        // The term's first entry, as the disks showed it when it was
        // written (kept after logs retire it), is an `empty` at `empty`.
        let opening = self.oracle.opening.get(&term).copied();
        if opening != Some((empty, crate::entry::Kind::Empty.code())) {
            return Err(self.fail(
                Check::Serving,
                format!(
                    "leader {i} of term {term} serves, but its term opens with {opening:?} \
                     (index, kind), not an `empty` entry at {empty}"
                ),
            ));
        }
        let ours = if empty < base {
            // Retired by a checkpoint: the record's entry there is of its
            // term, which only it makes.
            let recorded = empty
                .checked_sub(1)
                .and_then(|k| self.oracle.committed.get(k as usize));
            recorded.is_some_and(|e| e.0 == term)
        } else {
            let entry = self
                .quiet(i, |m| m.store.entry_at(empty))
                .unwrap()
                .map_err(|e| self.fail(Check::Serving, format!("its log: {e}")))?;
            entry.is_some_and(|(t, h)| t == term && self.oracle.hash(empty) == Some(h))
        };
        if !ours || published < empty {
            return Err(self.fail(
                Check::Serving,
                format!(
                    "leader {i} of term {term} serves with its `empty` entry {empty} not its own, \
                     committed and published (published {published})"
                ),
            ));
        }
        if self.oracle.leaders.keys().next_back() != Some(&term) {
            return Ok(());
        }
        let before = self
            .oracle
            .committed
            .iter()
            .rposition(|e| e.0 < term)
            .map_or(0, |k| k as u64 + 1);
        if published < before {
            return Err(self.fail(
                Check::Serving,
                format!(
                    "leader {i} of term {term} serves with its state at entry {published}, \
                     before entry {before} committed in an earlier term"
                ),
            ));
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
            5..=7 if self.split.is_none() && self.back == Return::Idle => {
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
            _ if self.back == Return::Idle => self.split = None,
            _ => {}
        }
        Ok(())
    }

    /// The simulator's record learns member `i`'s durable entry and the
    /// term its `state` records (module docs). The term's entries it holds
    /// join the term's chain, which one leader makes, so that two different
    /// entries of one term at one index are a breach. An entry is committed
    /// when a majority held that chain through it in its own term, as far as
    /// each was seen to hold it, whether or not they are up now. What it
    /// records it reads from member `i`'s disk.
    fn observe(&mut self, i: usize) -> Outcome {
        self.observe_as(i, false)
    }

    /// [`World::observe`]; with `again`, also when member `i` holds nothing
    /// new.
    fn observe_as(&mut self, i: usize, again: bool) -> Outcome {
        let Some(m) = self.nodes[i].member.as_ref() else {
            return Ok(());
        };
        let (index, term, tip) = m.store.durable_entry();
        let (saved, _) = m.store.saved_term();
        if index == 0 || term != saved {
            return Ok(());
        }
        let known = self.oracle.held[i].get(&term).copied().unwrap_or(0);
        if index > known {
            let base = m.store.log_base().index;
            let from = known.max(base) + 1;
            let read = self
                .quiet(i, |m| m.store.read_wire(from, index, u64::MAX, u64::MAX))
                .unwrap()
                .map_err(|e| self.fail(Check::LogMatching, format!("member {i}'s log: {e}")))?;
            let at_base = if index == base {
                self.quiet(i, |m| m.store.entry_at(base))
                    .unwrap()
                    .map_err(|e| self.fail(Check::LogMatching, format!("member {i}'s log: {e}")))?
            } else {
                None
            };
            self.record_held(i, term, index, tip, &read, at_base)?;
        } else if !again {
            return Ok(());
        }
        self.check_majority(i, term)
    }

    /// The record learns that member `i` holds term `term`'s chain through
    /// its durable entry `index`, of hash `tip`, from what its disk read:
    /// the term's entries after what it held before (`read`), and the
    /// checkpoint's entry when the tip is the log's base (`at_base`). The
    /// tip is counted only when that read shows it.
    fn record_held(
        &mut self,
        i: usize,
        term: u64,
        index: u64,
        tip: [u8; 32],
        read: &[Wire],
        at_base: Option<(u64, [u8; 32])>,
    ) -> Outcome {
        let shown = match at_base {
            Some(entry) => entry == (term, tip),
            None => read
                .iter()
                .any(|e| e.index == index && e.term == term && e.hash == tip),
        };
        if !shown {
            return Err(self.fail(
                Check::LogMatching,
                format!(
                    "member {i}'s durable entry {index} of term {term} is not in what its log \
                     reads"
                ),
            ));
        }
        for e in read {
            self.oracle
                .sizes
                .insert((e.index, e.hash), e.payload.len() as u64);
        }
        // One leader makes a term's entries, so two at one index are a
        // breach.
        let entries = read
            .iter()
            .filter(|e| e.term == term)
            .map(|e| (e.index, e.hash, Some(e.kind)))
            .chain(at_base.map(|(_, h)| (index, h, None)));
        for (at, hash, kind) in entries.collect::<Vec<_>>() {
            let chain = self.oracle.chains.entry(term).or_default();
            match chain.get(&at) {
                Some(h) if *h != hash => {
                    return Err(self.fail(
                        Check::LogMatching,
                        format!(
                            "member {i} holds another entry {at} of term {term} than the term's \
                             chain"
                        ),
                    ));
                }
                Some(_) => {}
                None => {
                    chain.insert(at, hash);
                }
            }
            if let Some(kind) = kind {
                let opening = self.oracle.opening.entry(term).or_insert((at, kind));
                if at < opening.0 {
                    *opening = (at, kind);
                }
            }
        }
        self.oracle.held[i].insert(term, index);
        Ok(())
    }

    /// The position of term `term`'s chain a majority has held: inside the
    /// record it must be the recorded entry, by the hash counted; past it,
    /// the record grows from member `i`'s disk if it holds that far.
    fn check_majority(&mut self, i: usize, term: u64) -> Outcome {
        let index = self.oracle.held[i].get(&term).copied().unwrap_or(0);
        let mut levels: Vec<u64> = self
            .oracle
            .held
            .iter()
            .map(|h| h.get(&term).copied().unwrap_or(0))
            .collect();
        levels.sort_unstable_by(|a, b| b.cmp(a));
        let committed = levels[self.nodes.len() / 2];
        let max = self.oracle.max();
        if committed == 0 {
            return Ok(());
        }
        let ours = |at: u64| {
            self.oracle
                .chains
                .get(&term)
                .and_then(|c| c.get(&at))
                .copied()
        };
        if committed <= max {
            // Every held position was counted with its hash: the majority's
            // must be there, and be the recorded entry.
            let Some(hash) = ours(committed) else {
                return Err(self.fail(
                    Check::LogMatching,
                    format!(
                        "a majority held term {term}'s chain through {committed}, but the record \
                         has no entry of the term there"
                    ),
                ));
            };
            if Some(hash) != self.oracle.hash(committed) {
                return Err(self.fail(
                    Check::LogMatching,
                    format!(
                        "entry {committed} of term {term}, held by a majority, is not the \
                         committed one"
                    ),
                ));
            }
            return Ok(());
        }
        // Past the record: where the record ends, the chain's entry, if the
        // term has one there, is the recorded one.
        if let (Some(hash), Some(recorded)) = (ours(max), self.oracle.hash(max)) {
            if hash != recorded {
                return Err(self.fail(
                    Check::LogMatching,
                    format!(
                        "entry {max} of term {term}, held by a majority, is not the committed one"
                    ),
                ));
            }
        }
        if index < committed {
            return Ok(());
        }
        if max > 0 {
            self.check_entry(i, max, Check::LogMatching, "the last committed entry")?;
        }
        let read = self
            .quiet(i, |m| {
                m.store.read_wire(max + 1, committed, u64::MAX, u64::MAX)
            })
            .unwrap()
            .map_err(|e| self.fail(Check::LogMatching, format!("member {i}'s log: {e}")))?;
        if read.len() as u64 != committed - max || read.first().map(|e| e.index) != Some(max + 1) {
            return Err(self.fail(
                Check::LogMatching,
                format!("member {i} holds entry {committed} without those after {max}"),
            ));
        }
        if read.last().map(|e| e.hash) != ours(committed) {
            return Err(self.fail(
                Check::LogMatching,
                format!("member {i}'s entry {committed} is not the one a majority held"),
            ));
        }
        for e in read {
            let size = e.payload.len() as u64;
            self.oracle.sizes.insert((e.index, e.hash), size);
            self.oracle.committed.push((e.term, e.hash));
        }
        Ok(())
    }

    /// Logs `i` and `j` hold the same entry where they hold one of the same
    /// index and term: at the highest such place, which by the chain of
    /// hashes covers every place below it.
    fn check_matching(&self, i: usize, j: usize) -> Outcome {
        if self.nodes[i].sim.halted() || self.nodes[j].sim.halted() {
            // Its power failed in this step: it is taken down next.
            return Ok(());
        }
        let ends =
            |k: usize| self.quiet(k, |m| (m.store.durable_entry().0, m.store.log_base().index));
        let (Some((tip_i, base_i)), Some((tip_j, base_j))) = (ends(i), ends(j)) else {
            return Ok(());
        };
        let floor = base_i.max(base_j);
        let mut at = tip_i.min(tip_j);
        while at >= floor && at > 0 {
            // Inside the range both hold, every read must find an entry.
            let read = |k: usize| self.quiet(k, |m| m.store.entry_at(at)).unwrap();
            let (Ok(Some((t_i, h_i))), Ok(Some((t_j, h_j)))) = (read(i), read(j)) else {
                return Err(self.fail(
                    Check::LogMatching,
                    format!("members {i} and {j} both hold entry {at}, but it could not be read"),
                ));
            };
            if t_i == t_j {
                if h_i != h_j {
                    return Err(self.fail(
                        Check::LogMatching,
                        format!("members {i} and {j} hold different entries {at} of term {t_i}"),
                    ));
                }
                return Ok(());
            }
            at -= 1;
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
        let (d_index, _, d_hash) = member.store.durable_entry();
        self.cov.highest_term = self.cov.highest_term.max(term);
        if role == Role::Leader && decided > 0 {
            if decided > self.oracle.max() {
                // The record from the leader's own chain, before judging.
                self.observe_as(i, true)?;
            }
            if decided > self.oracle.max() {
                return Err(self.fail(
                    Check::EarlyCommit,
                    format!(
                        "leader {i} decided entry {decided} is committed; the disks show {}",
                        self.oracle.max()
                    ),
                ));
            }
            self.check_entry(
                i,
                decided,
                Check::EarlyCommit,
                "the committed entry it decided",
            )?;
        }
        if role == Role::Leader && self.nodes[i].member.as_ref().unwrap().core.serving() {
            self.check_serving(i)?;
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
                if j != i {
                    self.check_matching(i, j)?;
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
                    "{:?} term {} start {} last {} committed {} published {} base {}{}",
                    m.core.role(),
                    m.core.term(),
                    m.core.start(),
                    m.core.last().index,
                    m.core.committed(),
                    m.core.published(),
                    m.core.base().index,
                    if m.core.role() == Role::Leader {
                        // What the leader knows of each follower.
                        let progress: Vec<String> = m
                            .core
                            .progress
                            .values()
                            .map(|p| {
                                format!(
                                    "next {} matched {} sent {} probe {:?}",
                                    p.next, p.matched, p.sent, p.probe
                                )
                            })
                            .collect();
                        format!(", knows {progress:?}, behind {}", m.core.behind().len())
                    } else {
                        String::new()
                    }
                ),
                None => "down".into(),
            })
            .collect();
        Err(self.fail(
            Check::Settle,
            format!("the cluster did not settle: {state:?}"),
        ))
    }

    /// After the cluster settled, a minority goes down for good, the leader
    /// first: the rest must elect a leader that serves and commit a new
    /// entry. Where they cannot because one of them is behind (its log ends
    /// before another's base: state transfer, #849, is not built), the end
    /// is counted, not failed.
    fn settle_minority(&mut self) -> Outcome {
        let n = self.nodes.len();
        let mut order: Vec<usize> = (0..n).collect();
        if let Some(l) = self.leading() {
            order.retain(|i| *i != l);
            order.insert(0, l);
        }
        for &i in order.iter().take((n - 1) / 2) {
            self.nodes[i].member = None;
            self.nodes[i].down_until = u64::MAX;
        }
        let serving = |w: &World| {
            w.up()
                .into_iter()
                .find(|i| w.nodes[*i].member.as_ref().unwrap().core.serving())
        };
        let mut progress = false;
        if self.run_until(600, |w| serving(w).is_some())? {
            let leader = serving(self).unwrap();
            let proposed = self.cov.proposed;
            self.propose()?;
            let member = self.nodes[leader].member.as_ref().unwrap();
            let target = member.core.last().index;
            if self.cov.proposed > proposed || member.waiting() > 0 {
                progress = self.run_until(600, |w| {
                    serving(w).is_some_and(|l| {
                        w.nodes[l].member.as_ref().unwrap().core.committed() >= target
                    })
                })?;
            }
        }
        if progress {
            self.cov.minority_progress += 1;
            return Ok(());
        }
        // A barrier on the disks: some survivor's log last agrees with the
        // log that would have to send it what it lacks before that log's
        // base, so what it lacks is retired there (state transfer is not
        // built). That log is the leader's in office: a behind follower
        // answers its empty heartbeats, so it stays. With none in office,
        // it is each survivor's with the most up to date log, the only ones
        // that can be elected (every survivor's vote is needed).
        let ups = self.up();
        let core = |i: usize| &self.nodes[i].member.as_ref().unwrap().core;
        let tip = |i: usize| {
            let (index, term, _) = self.nodes[i].member.as_ref().unwrap().store.durable_entry();
            (term, index)
        };
        let in_office = ups
            .iter()
            .copied()
            .filter(|i| core(*i).role() == Role::Leader)
            .max_by_key(|i| core(*i).term());
        let sources: Vec<usize> = match in_office {
            Some(l) => vec![l],
            None => {
                let best = ups.iter().map(|i| tip(*i)).max().unwrap();
                ups.iter().copied().filter(|i| tip(*i) == best).collect()
            }
        };
        let barrier = ups
            .iter()
            .any(|j| !sources.contains(j) && sources.iter().all(|m| self.meet(*j, *m).is_none()));
        let ends: Vec<(u64, u64)> = ups
            .iter()
            .map(|i| {
                let store = &self.nodes[*i].member.as_ref().unwrap().store;
                (store.durable_entry().0, store.log_base().index)
            })
            .collect();
        if barrier {
            self.cov.minority_behind += 1;
            return Ok(());
        }
        let views: Vec<String> = self
            .up()
            .into_iter()
            .map(|i| {
                let c = &self.nodes[i].member.as_ref().unwrap().core;
                format!(
                    "{i}: {:?} t{} last {:?} committed {} knows {:?}",
                    c.role(),
                    c.term(),
                    c.last(),
                    c.committed(),
                    c.progress
                        .values()
                        .map(|p| (p.next, p.matched, p.sent, p.probe, p.probing))
                        .collect::<Vec<_>>()
                )
            })
            .collect();
        Err(self.fail(
            Check::Settle,
            format!("with a minority down for good the rest made no progress: {ends:?} {views:?}"),
        ))
    }

    fn base_of(&self, i: usize) -> u64 {
        self.nodes[i]
            .member
            .as_ref()
            .unwrap()
            .store
            .log_base()
            .index
    }

    /// The last entry at which the logs of `j` and `k` agree, read from the
    /// two disks; `None` when they agree nowhere both can read.
    fn meet(&self, j: usize, k: usize) -> Option<u64> {
        let store = |i: usize| &self.nodes[i].member.as_ref().unwrap().store;
        let (tip_j, tip_k) = (store(j).durable_entry().0, store(k).durable_entry().0);
        let floor = self.base_of(j).max(self.base_of(k));
        let mut at = tip_j.min(tip_k);
        loop {
            let a = self
                .quiet(j, |m| m.store.entry_hash(at))
                .unwrap()
                .ok()
                .flatten();
            let b = self
                .quiet(k, |m| m.store.entry_hash(at))
                .unwrap()
                .ok()
                .flatten();
            if a.is_some() && a == b {
                return Some(at);
            }
            if at <= floor {
                return None;
            }
            at -= 1;
        }
    }

    /// No more faults, and the network heals.
    fn calm(&mut self) {
        self.faults = false;
        self.split = None;
        self.release();
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
            // Every member is idle now, so each takes its tick: a member
            // takes nothing while busy, and a client entry before the tick
            // would keep the leader busy at every one, so that it could
            // send no heartbeat. The entry follows once the tick's work is
            // done. This order is the simulator's own, standing for the
            // duty section 7.1 gives a member's driver, not a rule of the
            // core, so no check holds it; the client's limit of four
            // uncommitted entries would leave a leader idle often enough to
            // hide it anyway.
            self.step += 1;
            self.tick()?;
            if self.writing {
                while let Some(i) = self
                    .up()
                    .into_iter()
                    .find(|i| !self.nodes[*i].member.as_ref().unwrap().idle())
                {
                    self.act(i)?;
                }
                self.propose()?;
            }
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
    w.settle_minority()?;
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
    // `OM_STORE_SEEDS=<n>` for seeds 0 to n - 1, `<a>..<b>` for a to b - 1.
    if let Ok(v) = std::env::var("OM_STORE_SEEDS") {
        let (a, b) = v.split_once("..").unwrap_or(("0", &v));
        if let (Ok(a), Ok(b)) = (a.parse::<u64>(), b.parse::<u64>()) {
            return (a..b).collect();
        }
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
    // Every steady phase ends with a verdict on every member, or the run
    // fails.
    assert_eq!(total.steady_judged, total.schedules, "{total:?}");
}

/// The largest entry of the member given a smaller one (`World::small`).
const SMALL_ENTRY: u64 = 512;

/// The election attempts the steady phase allows ([`World::election_deadline`]).
const ATTEMPTS: u64 = 8;

#[test]
fn clusters_keep_the_rules_of_consensus() {
    run_all("default", seeds(SEEDS), STEPS);
}

#[test]
#[ignore = "long: run in release, OM_STORE_SCALE to scale"]
fn long_clusters_keep_the_rules_of_consensus() {
    run_all("long", seeds(SEEDS * 20), STEPS * 4);
}

/// The planted defects a seed of the default run must find.
const PLANTS: [Plant; 19] = [
    Plant::VoteUnflushed,
    Plant::CountOlderTerm,
    Plant::CommitUnchecked,
    Plant::CutAtChecked,
    Plant::PublishDurable,
    Plant::CountStaleSuccess,
    Plant::AppendTermUnsaved,
    Plant::VoteNoUpToDate,
    Plant::VoteLastIndexOnly,
    Plant::VoteLastTermOnly,
    Plant::VoteForgotten,
    Plant::AckBeforeFlush,
    Plant::AckLastEntry,
    Plant::PublishLeaderCommitted,
    Plant::ServeBeforeEmpty,
    Plant::RefusalLatestOnly,
    Plant::OpenWithoutEmpty,
    Plant::StandWithoutRestart,
    Plant::BlockedKeptAcrossRestart,
];

/// Planted defects measured but not required, each held by a core-level
/// test instead: a success past what the leader sent comes only from a
/// member that lies, which the trust rule leaves out; and a follower that
/// follows the protocol never refuses for its size an entry it has
/// acknowledged within one start (it refuses before it takes anything, and
/// a new start makes the leader forget what it acknowledged).
const MEASURED: [Plant; 2] = [Plant::CountPastSent, Plant::SizeRefusalAtMatched];

/// The checks a planted defect must break, and for a refusal what the
/// store must have refused.
fn expected(plant: Plant) -> (&'static [Check], &'static [&'static str]) {
    match plant {
        // A vote answered before its record: the answer disagrees with the
        // disk; or, if that is missed, two leaders in one term or a leader
        // without an entry committed meanwhile.
        Plant::VoteUnflushed => (
            &[Check::DurableAnswer, Check::OneLeader, Check::Completeness],
            &[],
        ),
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
        Plant::PublishDurable | Plant::PublishLeaderCommitted => (&[Check::Published], &[]),
        // A success of an earlier term counted in this one: the leader
        // counts a copy the follower does not hold.
        // A success counted that the follower does not hold: an early
        // commit, or a follower the leader never sends what it lacks.
        Plant::CountStaleSuccess | Plant::CountPastSent => (
            &[Check::EarlyCommit, Check::Completeness, Check::Settle],
            &[],
        ),
        // An acknowledgement past what its append checked, against the
        // append's interval and chain.
        Plant::AckLastEntry => (&[Check::DurableAnswer], &[]),
        // A refusal passed over because a newer append went out: a follower
        // left behind while writes go on.
        Plant::RefusalLatestOnly => (&[Check::Progress], &[]),
        // A term opened with an ordinary entry: the leader serves without
        // its `empty`.
        Plant::OpenWithoutEmpty => (&[Check::Serving], &[]),
        // Elections that must fit two round trips in one timeout: no leader
        // for longer than an election may take.
        Plant::StandWithoutRestart => (&[Check::Election], &[]),
        // A follower that started again with the cluster's limit, still
        // sent nothing but empty heartbeats.
        Plant::BlockedKeptAcrossRestart => (&[Check::Progress], &[]),
        Plant::SizeRefusalAtMatched => (&[Check::Progress], &[]),
        Plant::AppendTermUnsaved | Plant::VoteForgotten | Plant::AckBeforeFlush => {
            (&[Check::DurableAnswer], &[])
        }
        Plant::VoteNoUpToDate | Plant::VoteLastIndexOnly | Plant::VoteLastTermOnly => {
            (&[Check::Grant], &[])
        }
        Plant::ServeBeforeEmpty => (&[Check::Serving], &[]),
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
        // A leader counting a success of an earlier term needs the
        // returning-leader history and then an acknowledgement past the
        // new `empty`: few seeds find it, and its guard is the fixed
        // returning-leader test.
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
    // `OM_PLANT=<name>` measures that defect alone.
    let only = std::env::var("OM_PLANT").ok();
    for plant in PLANTS.into_iter().chain(MEASURED) {
        if only.as_ref().is_some_and(|o| *o != format!("{plant:?}")) {
            continue;
        }
        let seeds = seeds(SEEDS);
        let mut kinds: BTreeMap<String, (u64, u64, String)> = BTreeMap::new();
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
                kinds.entry(kind).or_insert((0, *seed, e.text.clone())).0 += 1;
            }
        }
        let found: u64 = kinds.values().map(|k| k.0).sum();
        eprintln!("{plant:?}: found by {found} of {} seeds", seeds.len());
        for (kind, (n, first, text)) in kinds {
            eprintln!("  {n} seeds, the first {first}: {kind}");
            eprintln!("    in full: {text}");
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
        max_entry: 1 << 10,
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
        largest: None,
    };
    Core::new(cfg, &rec).unwrap()
}

/// Makes `a` the leader of the next term with `voter`'s votes, its `empty`
/// entry durable, and returns the appends it sends then: to whom, `prev`,
/// and the first and last entries.
fn elect(a: &mut Core, voter: ServerId) -> Vec<(ServerId, Pos, u64, u64)> {
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
    let out = a.flushed(a.last());
    sends(a, &out)
}

/// The appends a message leads to, reported sent ([`sends`]).
fn recv(a: &mut Core, m: Message) -> Vec<(ServerId, Pos, u64, u64)> {
    let out = a.receive(m);
    sends(a, &out)
}

/// The appends among `actions`, each reported sent whole as a member
/// does ([`Core::sent`]): to whom, `prev`, the first and last entries.
fn sends(a: &mut Core, actions: &[Action]) -> Vec<(ServerId, Pos, u64, u64)> {
    let out: Vec<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::SendAppend {
                to,
                prev,
                first,
                last,
                ..
            } => Some((*to, *prev, *first, *last)),
            _ => None,
        })
        .collect();
    for (to, prev, _, last) in &out {
        a.sent(*to, prev.index, (*last).max(prev.index));
    }
    out
}

/// Member `from`'s answer, in `term` and its `start`, to an append of
/// member 1 that named `prev`, in `echo_term` and start `echo_start`.
#[allow(clippy::too_many_arguments)]
fn reply_to(
    from: u8,
    term: u64,
    start: u64,
    echo_term: u64,
    echo_start: u64,
    prev: u64,
    matched: Option<u64>,
    hint: u64,
) -> Message {
    Message {
        from: id(from),
        to: id(1),
        term,
        start,
        body: Body::AppendReply {
            echo: Echo {
                term: echo_term,
                start: echo_start,
                prev,
            },
            matched,
            hint,
        },
    }
}

/// Member `from`'s answer, in the leader's term, to the append it has out
/// now.
fn reply(from: u8, term: u64, prev: u64, matched: Option<u64>, hint: u64) -> Message {
    reply_to(from, term, 1, term, 1, prev, matched, hint)
}

/// A reply counts only against the append it repeats and no further than
/// the leader sent; a refusal only of the append out now; a malformed
/// message changes nothing; a higher term is taken from any message; an
/// append that is not whole changes nothing else (section 7.1, trust).
#[test]
fn a_core_takes_from_a_message_only_what_it_asked_for() {
    let mut a = core_of(1, &[1, 1, 1], 1);
    let sent = elect(&mut a, id(2));
    let term = a.term();
    let (_, prev, first, last) = sent.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!((prev.index, first, last), (3, 4, 4));
    // A success repeating an earlier term, or another start, of the leader:
    // passed over.
    a.receive(reply_to(2, term, 1, term - 1, 1, 3, Some(4), 0));
    a.receive(reply_to(2, term, 1, term, 9, 3, Some(4), 0));
    assert_eq!((a.progress[&id(2)].matched, a.committed()), (0, 0));
    // A refusal of an append not out now: passed over. (The first append
    // to a follower is a probe: `next` stays until a success.)
    assert!(a.receive(reply(2, term, 1, None, 0)).is_empty());
    assert_eq!(a.progress[&id(2)].next, 4);
    // A success claiming past what was sent counts only as far as was sent:
    // the leader's entry 4 is committed, nothing past it.
    a.receive(reply(2, term, 3, Some(9), 9));
    assert_eq!((a.progress[&id(2)].matched, a.committed()), (4, 4));
    // A term past the bound: malformed, nothing changes.
    let past = Message {
        term: u64::MAX,
        ..reply(3, term, 0, None, 0)
    };
    assert!(a.receive(past).is_empty());
    assert_eq!((a.term(), a.role()), (term, Role::Leader));
    // A higher term, from a reply that answers nothing: taken and recorded.
    let stray = Message {
        from: id(3),
        to: id(1),
        term: term + 5,
        start: 1,
        body: Body::PreVoteReply {
            round: 0,
            granted: false,
        },
    };
    let out = a.receive(stray);
    assert!(matches!(out[..], [Action::SaveTerm { term: t, vote: None }] if t == term + 5));
    assert_eq!(a.role(), Role::Follower);
    // A malformed append changes nothing, the term and start included:
    // entries past its term, more than a batch's entries, more than a
    // batch's bytes in more than one entry. (An entry past the largest is
    // refused with its reason: the blocked follower's test.)
    let wire = |index, term, len: usize| Wire {
        index,
        term,
        kind: crate::entry::Kind::Empty.code(),
        file: 0,
        payload: vec![0; len],
        hash: [0; 32],
    };
    for entries in [
        vec![wire(1, 3, 0)],
        vec![wire(1, 2, 0), wire(2, 2, 0), wire(3, 2, 0)],
        vec![wire(1, 2, 600), wire(2, 2, 600)],
    ] {
        let mut f = core_of(3, &[], 0);
        let append = Message {
            from: id(1),
            to: id(3),
            term: 2,
            start: 1,
            body: Body::Append {
                prev: Pos::default(),
                entries,
                committed: MAX_INDEX,
            },
        };
        let out = f.receive(append);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(f.term(), 0);
        assert!(f.starts.is_empty());
    }
}

/// A refusal delivered twice, the second after the follower caught up and
/// the leader's log was retired past the refusal's hint, leaves the
/// follower where it is; a follower is called behind only when it refuses
/// a probe at the log's base as it is when the refusal arrives.
#[test]
fn a_delayed_rejection_does_not_strand_a_follower() {
    let mut a = core_of(1, &[1; 10], 1);
    let sent = elect(&mut a, id(3));
    let term = a.term();
    let (_, prev, _, _) = sent.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!(prev.index, 10);
    // B's log ends at 5: it refuses the probe, and the refusal is repeated
    // in the network.
    let refusal = reply(2, term, 10, None, 5);
    let (_, prev, _, last) = recv(&mut a, refusal.clone())[0];
    assert_eq!((prev.index, last), (5, 7));
    let (_, prev, _, last) = recv(&mut a, reply(2, term, 5, Some(7), 7))[0];
    assert_eq!((prev.index, last), (7, 9));
    let (_, prev, _, last) = recv(&mut a, reply(2, term, 7, Some(9), 9))[0];
    assert_eq!((prev.index, last), (9, 11));
    a.compacted(Pos { index: 8, term: 1 });
    // The repeated refusal names an entry B has not acknowledged: B is
    // probed again, after what it acknowledged, and is not behind.
    let (_, prev, _, _) = recv(&mut a, refusal)[0];
    assert_eq!(prev.index, 9);
    assert!(!a.behind().contains(&id(2)));
    // B's answer to that probe: it holds everything, and appends follow
    // one another again.
    assert!(recv(&mut a, reply(2, term, 9, Some(11), 11)).is_empty());
    let p = a.progress[&id(2)];
    assert_eq!((p.matched, p.next, p.probing), (11, 12, false));
    // A refusal that truly asks for what is retired comes only from a later
    // start of B, which makes the leader forget what B held: a probe at the
    // base first, and behind only once that is refused.
    let later = |prev, hint| reply_to(2, term, 2, term, 1, prev, None, hint);
    // B's first message of its new start: the leader forgets, and the
    // refusal answers no append out now.
    assert!(recv(&mut a, later(9, 5)).is_empty());
    let out = {
        let out = a.tick();
        sends(&mut a, &out)
    };
    let (_, prev, _, _) = out.iter().copied().find(|s| s.0 == id(2)).unwrap();
    assert_eq!(prev.index, 11);
    let (_, prev, _, _) = recv(&mut a, later(11, 5))[0];
    assert_eq!(prev.index, 8);
    assert!(!a.behind().contains(&id(2)));
    // The base moves on before B's answer to the probe at 8 arrives: that
    // answer is of the probe out now, but not at the base as it is now, so
    // B is probed at the new base, and is behind only once it refuses that.
    a.compacted(Pos { index: 10, term: 1 });
    let out = recv(&mut a, later(8, 5));
    assert!(!a.behind().contains(&id(2)));
    assert_eq!(out[0].1.index, 10);
    recv(&mut a, later(10, 5));
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
        max_entry: u64::from(STORE.log.max_payload),
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

/// A fixed schedule's world: no faults, and nothing moves but what the
/// script does.
impl World {
    fn scripted(members: usize, plant: Option<Plant>) -> World {
        World::scripted_seed(7, members, plant)
    }

    /// The same, with seed `seed`'s settings (timing, the regimes drawn).
    fn scripted_seed(seed: u64, members: usize, plant: Option<Plant>) -> World {
        let setup = Setup {
            members,
            steps: 0,
            plant,
            store: STORE,
            batch: Some((8, 16 << 10)),
        };
        let mut w = World::new(seed, setup).unwrap();
        w.calm();
        w
    }

    fn who(&self, id: ServerId) -> usize {
        self.ids.iter().position(|x| *x == id).unwrap()
    }

    fn core_of(&self, i: usize) -> &Core {
        &self.nodes[i].member.as_ref().unwrap().core
    }

    /// Member `i`'s actions, all of them; what it sends goes into the
    /// network.
    fn drain(&mut self, i: usize) -> Outcome {
        while self.nodes[i].member.as_ref().is_some_and(|m| !m.idle()) {
            self.act(i)?;
        }
        Ok(())
    }

    /// Delivers, oldest first, every message in the network `pick` chooses
    /// (by message, sender and receiver), and what each leads to, until
    /// none is left that it chooses.
    fn pass(&mut self, pick: impl Fn(&Message, usize, usize) -> bool) -> Outcome {
        loop {
            let chosen = (0..self.flight.len()).find(|k| {
                let m = &self.flight[*k].2;
                pick(m, self.who(m.from), self.who(m.to))
            });
            let Some(k) = chosen else {
                return Ok(());
            };
            let to = self.who(self.flight[k].2.to);
            self.drain(to)?;
            self.deliver(k)?;
            self.drain(to)?;
        }
    }

    /// Takes out of the network every message `pick` chooses.
    fn take(&mut self, pick: impl Fn(&Message, usize, usize) -> bool) -> Vec<Message> {
        let mut taken = Vec::new();
        let mut k = 0;
        while k < self.flight.len() {
            let m = &self.flight[k].2;
            if pick(m, self.who(m.from), self.who(m.to)) {
                taken.push(self.flight.remove(k).2);
            } else {
                k += 1;
            }
        }
        taken
    }

    /// Member `i` alone ticks `n` times.
    fn tick_member(&mut self, i: usize, n: u64) -> Outcome {
        for _ in 0..n {
            self.nodes[i].member.as_mut().unwrap().tick();
            self.drain(i)?;
        }
        Ok(())
    }

    /// Member `c` wins an election with the votes of `voters`, who first
    /// stop hearing from any leader; whatever else the election sends is
    /// dropped. It leads with its `empty` entry durable.
    fn win(&mut self, c: usize, voters: &[usize]) -> Outcome {
        let election = self.timing.0;
        for &v in voters {
            self.tick_member(v, election)?;
        }
        let ballot = |m: &Message| {
            matches!(
                m.body,
                Body::PreVote { .. }
                    | Body::Vote { .. }
                    | Body::PreVoteReply { .. }
                    | Body::VoteReply { .. }
            )
        };
        self.take(|m, _, _| ballot(m));
        // A round refused for a voter's higher term teaches the candidate
        // that term: it asks again on its next timeout.
        for _ in 0..3 {
            for _ in 0..4 * election {
                if self.core_of(c).role() == Role::PreCandidate {
                    break;
                }
                self.tick_member(c, 1)?;
            }
            assert_eq!(self.core_of(c).role(), Role::PreCandidate);
            self.pass(|m, f, t| {
                ballot(m) && ((f == c && voters.contains(&t)) || (t == c && voters.contains(&f)))
            })?;
            self.take(|m, _, _| ballot(m));
            if self.core_of(c).role() == Role::Leader {
                break;
            }
        }
        assert_eq!(self.core_of(c).role(), Role::Leader);
        self.take(|m, _, _| ballot(m));
        Ok(())
    }

    /// Appends and their answers between `leader` and `followers`.
    fn exchange(&mut self, leader: usize, followers: &[usize]) -> Outcome {
        self.pass(|m, f, t| {
            (f == leader && followers.contains(&t) && matches!(m.body, Body::Append { .. }))
                || (t == leader
                    && followers.contains(&f)
                    && matches!(m.body, Body::AppendReply { .. }))
        })
    }

    /// A client's entry at `leader`.
    fn write(&mut self, leader: usize, payload: Payload) -> Outcome {
        let next = self.core_of(leader).last().index + 1;
        let file = match payload {
            Payload::File(_) => next,
            _ => next - 1,
        };
        assert!(self.nodes[leader]
            .member
            .as_mut()
            .unwrap()
            .propose(file, payload));
        self.drain(leader)
    }
}

/// The schedule of the experiment review's P1-2 (five members): a leader
/// that was deposed and cut leads again, and an acknowledgement of its
/// earlier term, held back meanwhile, arrives. It must not be counted.
fn returning_leader(plant: Option<Plant>) -> Outcome {
    let (a, b, c, d, e) = (0, 1, 2, 3, 4);
    let mut w = World::scripted(5, plant);
    // A leads term 1; its `empty` reaches everyone.
    w.win(a, &[b, c, d, e])?;
    w.exchange(a, &[b, c, d, e])?;
    // Entries 2 and 3 of term 1 reach B only, and B's acknowledgements are
    // held back.
    let added = FileEvent::Added {
        name: b"f".to_vec(),
        flags: 0,
        page0: vec![7; 16],
    };
    w.write(a, Payload::File(added))?;
    w.write(
        a,
        Payload::Change(Change {
            edits: vec![Edit::leaf(vec![b"k".to_vec()], b"v".to_vec())],
            ..Change::default()
        }),
    )?;
    w.take(|m, f, t| f == a && t != b && matches!(m.body, Body::Append { .. }));
    w.pass(|m, f, t| f == a && t == b && matches!(m.body, Body::Append { .. }))?;
    let held = w.take(|_, f, t| f == b && t == a);
    assert!(!held.is_empty());
    // C leads term 2 with D and E; its `empty` at 2 reaches A, D and E: A
    // cuts its 2 and 3, and C commits 2.
    w.win(c, &[d, e])?;
    w.take(|_, f, t| f == c && t == b);
    w.exchange(c, &[a, d, e])?;
    assert_eq!(w.core_of(a).last().index, 2);
    // C stops. A leads term 3 with D and E; its `empty` at 3 reaches D only.
    w.nodes[c].member = None;
    w.nodes[c].down_until = u64::MAX;
    w.win(a, &[d, e])?;
    w.take(|m, f, t| f == a && t != d && matches!(m.body, Body::Append { .. }));
    w.exchange(a, &[d])?;
    // B's held acknowledgements of term 1 arrive.
    for m in held {
        w.seq += 1;
        w.flight.push((w.now, w.seq, m));
    }
    w.pass(|_, f, t| f == b && t == a)?;
    // A stops; C, back, stands with B and E and may lead: entry 3 was never
    // committed.
    w.nodes[a].member = None;
    w.nodes[a].down_until = u64::MAX;
    w.nodes[c].down_until = 0;
    w.open(c)?;
    w.win(c, &[b, e])?;
    w.exchange(c, &[b, e])
}

/// A leader that leads again after it was deposed and cut never counts an
/// acknowledgement of its earlier term; planted, it does, and the
/// simulator finds it.
#[test]
fn a_returning_leader_does_not_count_an_old_acknowledgement() {
    returning_leader(None).unwrap();
    let planted = returning_leader(Some(Plant::CountStaleSuccess)).unwrap_err();
    eprintln!("{planted}");
    assert!(is_expected(Plant::CountStaleSuccess, &planted), "{planted}");
}

/// Three bare cores on a network the test drives. Each does its actions as
/// a member would, its flushes and publications done at once, without a
/// store: entries carry no payload.
struct Net {
    cores: Vec<Core>,
    flight: Vec<Message>,
    /// Members whose messages are dropped, both ways.
    down: Vec<bool>,
}

impl Net {
    /// Logs holding entries of `logs`' terms, every member in `term`.
    fn with_logs(batch: u64, plant: Option<Plant>, logs: [&[u64]; 3], term: u64) -> Net {
        let cores = (1..=3u8)
            .zip(logs)
            .map(|(me, terms)| {
                let cfg = Config {
                    me: id(me),
                    members: vec![id(1), id(2), id(3)],
                    election: 4,
                    heartbeat: 1,
                    batch,
                    batch_bytes: 1 << 10,
                    max_entry: 1 << 10,
                    seed: 9,
                    plant,
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
                    largest: None,
                };
                Core::new(cfg, &rec).unwrap()
            })
            .collect();
        Net {
            cores,
            flight: Vec::new(),
            down: vec![false; 3],
        }
    }

    /// Empty logs.
    fn new(batch: u64, plant: Option<Plant>) -> Net {
        Net::with_logs(batch, plant, [&[], &[], &[]], 0)
    }

    fn at(id: ServerId) -> usize {
        usize::from(id[0]) - 1
    }

    fn post(&mut self, m: Message) {
        if !self.down[Net::at(m.from)] && !self.down[Net::at(m.to)] {
            self.flight.push(m);
        }
    }

    /// Member `i` does `actions`.
    fn act(&mut self, i: usize, actions: Vec<Action>) {
        let mut queue: VecDeque<Action> = actions.into();
        while let Some(a) = queue.pop_front() {
            let c = &mut self.cores[i];
            match a {
                Action::Flush { .. } => {
                    let last = c.last();
                    queue.extend(c.flushed(last));
                }
                Action::Publish(through) => {
                    let to = through.min(c.durable().index);
                    c.published_to(to);
                }
                Action::Send(m) => self.post(m),
                Action::SendAppend {
                    to,
                    term,
                    start,
                    prev,
                    first,
                    last,
                    committed,
                } => {
                    let entries: Vec<Wire> = (first..=last)
                        .map(|index| Wire {
                            index,
                            term: c.term_at(index).unwrap(),
                            kind: crate::entry::Kind::Empty.code(),
                            file: 0,
                            payload: Vec::new(),
                            hash: [0; 32],
                        })
                        .collect();
                    c.sent(to, prev.index, last.max(prev.index));
                    let m = Message {
                        from: c.me(),
                        to,
                        term,
                        start,
                        body: Body::Append {
                            prev,
                            entries,
                            committed,
                        },
                    };
                    self.post(m);
                }
                _ => {}
            }
        }
    }

    fn tick(&mut self, i: usize) {
        let actions = self.cores[i].tick();
        self.act(i, actions);
    }

    /// Delivers message `k` in the network.
    fn deliver(&mut self, k: usize) {
        let m = self.flight.remove(k);
        let to = Net::at(m.to);
        let actions = self.cores[to].receive(m);
        self.act(to, actions);
    }

    /// The first message in the network from `from` to `to`.
    fn find(&self, from: usize, to: usize) -> Option<usize> {
        self.flight
            .iter()
            .position(|m| Net::at(m.from) == from && Net::at(m.to) == to)
    }

    /// Delivers, oldest first, every message from `from` to `to` in the
    /// network now (not those they lead to).
    fn pass(&mut self, from: usize, to: usize) -> usize {
        let n = self
            .flight
            .iter()
            .filter(|m| Net::at(m.from) == from && Net::at(m.to) == to)
            .count();
        for _ in 0..n {
            let k = self.find(from, to).unwrap();
            self.deliver(k);
        }
        n
    }

    /// Delivers everything between `x` and `y`, what it leads to included.
    fn settle(&mut self, x: usize, y: usize) {
        for _ in 0..10_000 {
            if self.pass(x, y) + self.pass(y, x) == 0 {
                return;
            }
        }
        panic!("the network does not drain");
    }

    /// Member 1 leads with member 3's vote, member 2 heard or not.
    fn elect_first(&mut self) {
        for _ in 0..100 {
            if self.cores[0].role() == Role::PreCandidate {
                break;
            }
            self.tick(0);
        }
        self.settle(0, 2);
        self.settle(0, 1);
        assert_eq!(self.cores[0].role(), Role::Leader);
    }

    /// A client's entry at member 1.
    fn write(&mut self) {
        let actions = self.cores[0].propose(0, Payload::Empty).unwrap();
        self.act(0, actions);
    }

    fn last(&self, i: usize) -> u64 {
        self.cores[i].last().index
    }

    /// Member `i` starts again from what it holds, with `max_entry` and its
    /// next start count.
    fn restart(&mut self, i: usize, max_entry: u64) {
        let c = &self.cores[i];
        let cfg = Config {
            max_entry,
            ..c.cfg.clone()
        };
        let rec = Recovered {
            server: c.me(),
            start_count: c.start() + 1,
            term: c.term(),
            vote: c.vote,
            base: c.base(),
            terms: c.terms.clone(),
            committed: c.committed(),
            published: c.published(),
            largest: None,
        };
        self.cores[i] = Core::new(cfg, &rec).unwrap();
    }
}

/// Round 3's finding 1: member 1 leads; member 2 gets the append of entry
/// 3 before that of entry 2 and refuses it. From then on, a newer append
/// goes out before each of member 2's answers arrives, while member 3
/// keeps up and writes go on. `stop` stops member 3 after that many
/// writes. Returns member 2's last entry, the leader's, and the leader's
/// committed position.
fn overtaken(plant: Option<Plant>, stop: Option<usize>) -> (u64, u64, u64) {
    let (a, b, c) = (0, 1, 2);
    let mut net = Net::new(4, plant);
    net.elect_first();
    net.write();
    net.write();
    let k = net.find(a, b).unwrap();
    let first = net.flight.remove(k);
    let k = net.find(a, b).unwrap();
    net.deliver(k);
    net.flight.push(first);
    for round in 0..40 {
        if stop == Some(round) {
            net.down[c] = true;
            net.flight
                .retain(|m| Net::at(m.from) != c && Net::at(m.to) != c);
        }
        net.write();
        net.settle(a, c);
        net.pass(b, a);
        net.pass(a, b);
    }
    (net.last(b), net.last(a), net.cores[a].committed())
}

/// While writes go on, a follower each of whose refusals is overtaken by a
/// newer append catches up within a few rounds: a refusal of any append
/// above what it acknowledged puts it to probing at once. With round 2's
/// rule (a refusal counts only if it answers the last append sent), it
/// stays where it was for as long as the writes go on.
#[test]
fn a_follower_whose_refusals_are_overtaken_catches_up() {
    let (follower, leader, _) = overtaken(None, None);
    assert_eq!(follower, leader);
    let (follower, leader, _) = overtaken(Some(Plant::RefusalLatestOnly), None);
    assert!(
        leader - follower > 35,
        "planted: at {follower}, the leader at {leader}"
    );
}

/// The standstill: the follower that kept the commits going stops after
/// the other was left behind. The one left behind catches up and the
/// leader commits on; planted, it commits nothing more.
#[test]
fn commits_go_on_when_the_follower_that_kept_up_stops() {
    let (follower, leader, committed) = overtaken(None, Some(0));
    assert_eq!(follower, leader);
    assert!(
        committed + 1 >= leader,
        "committed {committed}, the leader at {leader}"
    );
    let (_, leader, committed) = overtaken(Some(Plant::RefusalLatestOnly), Some(0));
    assert!(
        committed <= 3,
        "planted: committed {committed}, the leader at {leader}"
    );
}

/// Round 3's finding 2: a refusal held back while the follower caught up
/// and the leader's log was retired to the entry the refusal names is old
/// news (it names an entry the follower acknowledged), so the follower is
/// not called behind.
#[test]
fn an_old_refusal_at_the_base_does_not_call_a_follower_behind() {
    let (a, b, c) = (0, 1, 2);
    let mut net = Net::new(2, None);
    net.elect_first();
    net.write();
    net.settle(a, b);
    net.settle(a, c);
    // The append of entry 3 to B is held; the next, naming 3, is refused.
    net.write();
    let k = net.find(a, b).unwrap();
    let held = net.flight.remove(k);
    net.write();
    let k = net.find(a, b).unwrap();
    net.deliver(k);
    let k = net.find(b, a).unwrap();
    let refusal = net.flight.remove(k);
    // C commits through 4, and the leader retires its log through 3.
    net.settle(a, c);
    assert_eq!(net.cores[a].committed(), 4);
    let term = net.cores[a].term_at(3).unwrap();
    net.cores[a].compacted(Pos { index: 3, term });
    assert_eq!(net.cores[a].base().index, 3);
    // The held append reaches B, and its success the leader.
    net.flight.push(held);
    net.pass(a, b);
    net.pass(b, a);
    assert_eq!(net.cores[a].progress[&id(2)].matched, 3);
    // The old refusal names the base as it is now.
    net.flight.push(refusal);
    net.pass(b, a);
    assert!(!net.cores[a].behind().contains(&id(2)));
    assert!(!net.cores[a].progress[&id(2)].probing);
}

/// Seed 190's loop (round 2): a follower with a tail of an older term, a
/// heartbeat out between each step back and its refusal. Round 2's first
/// echoes moved `next` on with every append sent, so the heartbeat probed
/// past the step back, the step back's refusal no longer answered the
/// append out now, and the leader went back and forth between two
/// entries. A probed follower's `next` stays until a success, a heartbeat
/// repeats the probe, and only the refusal of the probe out now moves it;
/// the refusals now taken at once while appends follow one another do not
/// change that, since a probed follower is never in that state.
#[test]
fn a_heartbeat_does_not_probe_past_a_step_back() {
    let (a, b) = (0, 1);
    let ours: &[u64] = &[1, 2, 2, 2, 2, 2];
    let mut net = Net::with_logs(1, None, [ours, &[1, 1, 1], ours], 2);
    net.down[b] = true;
    net.elect_first();
    net.down[b] = false;
    let mut caught = None;
    for round in 0..12 {
        net.tick(a);
        net.pass(a, b);
        net.pass(b, a);
        if net.last(b) == net.last(a) && net.cores[b].term_at(3) == Some(2) {
            caught = Some(round);
            break;
        }
    }
    assert!(caught.is_some(), "member 2 at {:?}", net.cores[b].last());
}

/// A follower given a lower largest entry refuses a larger one with its
/// reason; the leader reports it blocked, apart from behind, and sends it
/// only empty heartbeats, which read nothing from the log.
#[test]
fn a_follower_that_takes_no_entry_that_large_is_blocked() {
    let (a, b) = (0, 1);
    let mut net = Net::new(1, None);
    net.elect_first();
    net.write();
    let k = net.find(a, b).unwrap();
    let Body::Append { prev, .. } = net.flight.remove(k).body else {
        panic!("an append");
    };
    let term = net.cores[a].term();
    net.flight.push(Message {
        from: id(2),
        to: id(1),
        term,
        start: 1,
        body: Body::AppendTooLarge {
            echo: Echo {
                term,
                start: 1,
                prev: prev.index,
            },
            index: prev.index + 1,
            size: 4096,
            limit: 1024,
        },
    });
    net.pass(b, a);
    let oversize = Oversize {
        index: prev.index + 1,
        size: 4096,
        limit: 1024,
    };
    assert_eq!(net.cores[a].blocked(), vec![(id(2), oversize)]);
    assert!(net.cores[a].behind().is_empty());
    for _ in 0..3 {
        net.write();
        net.tick(a);
        for m in net.flight.iter().filter(|m| Net::at(m.to) == b) {
            assert!(matches!(&m.body, Body::Append { entries, .. } if entries.is_empty()));
        }
        net.settle(a, b);
        net.settle(a, 2);
    }
    assert_eq!(net.cores[a].blocked().len(), 1);
    // The follower's own refusal, of an entry past its limit.
    let mut f = core_of(3, &[], 0);
    let big = Message {
        from: id(1),
        to: id(3),
        term: 2,
        start: 1,
        body: Body::Append {
            prev: Pos::default(),
            entries: vec![Wire {
                index: 1,
                term: 2,
                kind: crate::entry::Kind::Empty.code(),
                file: 0,
                payload: vec![0; 2 << 10],
                hash: [0; 32],
            }],
            committed: 0,
        },
    };
    let out = f.receive(big);
    assert!(
        out.iter().any(|a| matches!(
            a,
            Action::Send(Message {
                body: Body::AppendTooLarge {
                    index: 1,
                    size: 2048,
                    limit: 1024,
                    ..
                },
                ..
            })
        )),
        "{out:?}"
    );
    assert_eq!(f.last().index, 0);
}

/// A member does not open with a largest entry below one its log holds;
/// the refusal names the entry, its size and the limit asked for. A larger
/// limit opens.
#[test]
fn a_member_does_not_open_below_the_largest_entry_it_holds() {
    let sim = Sim::new(77);
    let fs = || -> Arc<dyn crate::sys::Fs> { Arc::new(sim.fs()) };
    let me = Store::create_member(fs(), &sim.random(), STORE).unwrap();
    let cfg = |max_entry| Config {
        me,
        members: vec![me],
        election: 4,
        heartbeat: 1,
        batch: 4,
        batch_bytes: 16 << 10,
        max_entry,
        seed: 1,
        plant: None,
    };
    let (mut m, _) = Member::open(fs(), STORE, cfg(4096)).unwrap();
    let run = |m: &mut Member| while m.perform().unwrap().is_some() {};
    for _ in 0..20 {
        m.tick();
        run(&mut m);
    }
    assert!(m.core().serving());
    let next = m.core().last().index + 1;
    let added = FileEvent::Added {
        name: b"f".to_vec(),
        flags: 0,
        page0: vec![7; 16],
    };
    assert!(m.propose(next, Payload::File(added)));
    run(&mut m);
    let change = Change {
        edits: vec![Edit::leaf(vec![b"k".to_vec()], vec![1; 3_000])],
        ..Change::default()
    };
    assert!(m.propose(next, Payload::Change(change)));
    run(&mut m);
    let large = m.core().last().index;
    assert_eq!(m.core().durable().index, large);
    drop(m);
    sim.restart();
    let refused = Member::open(fs(), STORE, cfg(1_000)).err().unwrap();
    assert!(
        matches!(refused, MemberError::EntryTooLarge { index, size, limit: 1_000 }
            if index == large && size > 3_000),
        "{refused}"
    );
    sim.restart();
    assert!(Member::open(fs(), STORE, cfg(4096)).is_ok());
}

/// Round 4's findings 1 and 2. A follower that retired a large entry and
/// opened again with a smaller largest entry takes a replay of the append
/// that carried it: what is at or before its base is not refused for its
/// size. A leader takes a size refusal only for an entry it carried to that
/// follower and the follower has not acknowledged.
#[test]
fn a_size_refusal_counts_only_for_an_entry_carried_and_not_acknowledged() {
    let (a, b) = (0, 1);
    let mut net = Net::new(1, None);
    net.elect_first();
    for _ in 0..3 {
        net.write();
        net.settle(a, b);
        net.settle(a, 2);
    }
    let term = net.cores[a].term();
    assert_eq!(net.cores[a].progress[&id(2)].matched, 4);
    // A copy of the append that carried entry 3, as if 2 KiB, held back.
    let wire = |index, len| Wire {
        index,
        term,
        kind: crate::entry::Kind::Empty.code(),
        file: 0,
        payload: vec![0; len],
        hash: [0; 32],
    };
    let replay = Message {
        from: id(1),
        to: id(2),
        term,
        start: 1,
        body: Body::Append {
            prev: Pos { index: 2, term },
            entries: vec![wire(3, 2 << 10)],
            committed: 4,
        },
    };
    // B retires through 4 and opens again, its largest entry 512 bytes.
    let cfg = Config {
        me: id(2),
        members: vec![id(1), id(2), id(3)],
        election: 4,
        heartbeat: 1,
        batch: 1,
        batch_bytes: 1 << 10,
        max_entry: 512,
        seed: 9,
        plant: None,
    };
    let rec = Recovered {
        server: id(2),
        start_count: 2,
        term,
        vote: None,
        base: Pos { index: 4, term },
        terms: Vec::new(),
        committed: 4,
        published: 4,
        largest: None,
    };
    net.cores[b] = Core::new(cfg, &rec).unwrap();
    // A hears B's new start and its success through 4.
    for _ in 0..3 {
        net.tick(a);
        net.settle(a, b);
    }
    assert_eq!(net.cores[a].progress[&id(2)].matched, 4);
    // The replay reaches B: taken, not refused for its size.
    let out = net.cores[b].receive(replay);
    assert!(
        !out.iter().any(|x| matches!(
            x,
            Action::Send(Message {
                body: Body::AppendTooLarge { .. },
                ..
            })
        )),
        "{out:?}"
    );
    // A size refusal as a follower that still checked retired entries
    // would send it: of an entry B acknowledged, old news.
    let refusal = |start, prev, index| Message {
        from: id(2),
        to: id(1),
        term,
        start,
        body: Body::AppendTooLarge {
            echo: Echo {
                term,
                start: 1,
                prev,
            },
            index,
            size: 2 << 10,
            limit: 512,
        },
    };
    let heard = net.cores[a].progress[&id(2)].heard_at;
    for _ in 0..2 {
        net.tick(a);
    }
    net.flight.clear();
    assert!(net.cores[a].receive(refusal(2, 2, 3)).is_empty());
    // Of an entry past any A carried to B since its new start: the last
    // append to B named 4 and carried nothing, so `sent` is 4 and what was
    // carried less; neither 5 nor 4 was carried.
    assert!(net.cores[a].receive(refusal(2, 4, 5)).is_empty());
    assert!(net.cores[a].receive(refusal(2, 3, 4)).is_empty());
    let p = net.cores[a].progress[&id(2)];
    assert!(net.cores[a].blocked().is_empty());
    assert_eq!((p.heard_at, p.carried), (heard, 0));
}

/// Three members settled, with entries written and committed: the ground
/// for the checker's hand-built cases.
fn settled(seed: u64) -> World {
    let setup = Setup {
        members: 3,
        steps: 0,
        plant: None,
        store: STORE,
        batch: Some((8, 64 << 10)),
    };
    let mut w = World::new(seed, setup).unwrap();
    w.calm();
    w.settle().unwrap();
    w.writing = true;
    w.run_until(40, |_| false).unwrap();
    w.writing = false;
    w.run_until(20, |_| false).unwrap();
    w
}

/// Round 4's finding 6: the commit record counts a holder's position only
/// with the entry its disk shows there, and a position a majority held
/// inside the record only with the hash it counted, compared.
#[test]
fn the_commit_record_counts_a_holder_only_with_its_entry() {
    let mut w = settled(21);
    let l = w.leading().unwrap();
    let term = w.core_of(l).term();
    let f = (0..3).find(|i| *i != l).unwrap();
    let max = w.oracle.max();
    // A durable entry its read does not show: not counted.
    let short = w.record_held(f, term, max + 5, [7; 32], &[], None);
    let failure = short.unwrap_err();
    eprintln!("a tip its read does not show: {failure}");
    assert_eq!(failure.check, Check::LogMatching);
    assert!(w.oracle.held[f].get(&term).is_none_or(|k| *k <= max));
    // A majority position inside the record whose hash was not counted.
    let at = w.oracle.held[l][&term];
    assert!(at <= max);
    w.oracle.chains.get_mut(&term).unwrap().remove(&at);
    let failure = w.check_majority(l, term).unwrap_err();
    eprintln!("a majority position without its hash: {failure}");
    assert_eq!(failure.check, Check::LogMatching);
}

/// Round 4's finding 7: an acknowledgement of the entry at the follower's
/// log base is checked against the checkpoint's entry there.
#[test]
fn an_acknowledgement_at_the_base_is_the_checkpoint_entry() {
    let mut w = settled(22);
    let l = w.leading().unwrap();
    let f = (0..3).find(|i| *i != l).unwrap();
    // Write and checkpoint until the follower's log retires a segment.
    for _ in 0..20 {
        w.writing = true;
        w.run_until(40, |_| false).unwrap();
        w.writing = false;
        w.run_until(10, |_| false).unwrap();
        w.checkpoint(f).unwrap();
        if w.base_of(f) > 0 {
            break;
        }
    }
    let base = w.base_of(f);
    assert!(base > 0);
    let leader = w.ids[l];
    let (saved, _) = w.nodes[f].member.as_ref().unwrap().store.saved_term();
    let echo = Echo {
        term: saved,
        start: w.core_of(l).start(),
        prev: base - 1,
    };
    let prev = Pos {
        index: base - 1,
        term: saved,
    };
    // The append carried, at the base, an entry other than the one the
    // checkpoint keeps.
    w.nodes[f].appended = Some((leader, echo, prev, vec![(base, [9; 32])]));
    let reply = Message {
        from: w.ids[f],
        to: leader,
        term: saved,
        start: w.core_of(f).start(),
        body: Body::AppendReply {
            echo,
            matched: Some(base),
            hint: base,
        },
    };
    let failure = w.check_answer(f, &reply).unwrap_err();
    eprintln!("an acknowledgement at the base: {failure}");
    assert_eq!(failure.check, Check::DurableAnswer);
}

/// Round 4's finding 5: a leader's report that a follower is behind exempts
/// it from the progress check only when the disks confirm it.
#[test]
fn a_report_of_behind_exempts_a_follower_only_on_the_disks_word() {
    let mut w = settled(23);
    let l = w.leading().unwrap();
    let f = (0..3).find(|i| *i != l).unwrap();
    let id = w.ids[f];
    w.nodes[l].member.as_mut().unwrap().core.behind.insert(id);
    assert!(w.core_of(l).behind().contains(&id));
    assert!(!w.excused(l, f).unwrap());
}

/// The first tick at which a member leads, within `ticks`, of three bare
/// cores (election timeout 4, heartbeat 1) ticked each tick, on a network
/// that delivers every message exactly `delay` ticks after it is sent.
fn first_leader(delay: u64, ticks: u64, plant: Option<Plant>) -> Option<u64> {
    let mut net = Net::new(1, plant);
    let mut queue: Vec<(u64, Message)> = Vec::new();
    let post = |net: &mut Net, queue: &mut Vec<(u64, Message)>, now: u64| {
        queue.extend(net.flight.drain(..).map(|m| (now + delay, m)));
    };
    for now in 1..=ticks {
        while let Some(k) = queue.iter().position(|(due, _)| *due <= now) {
            let (_, m) = queue.remove(k);
            net.flight.push(m);
            let last = net.flight.len() - 1;
            net.deliver(last);
            post(&mut net, &mut queue, now);
        }
        for i in 0..3 {
            net.tick(i);
            post(&mut net, &mut queue, now);
        }
        if net.cores.iter().any(|c| c.role() == Role::Leader) {
            return Some(now);
        }
    }
    None
}

/// Round 5's finding 1 (the second review by experiment): members that
/// follow the protocol, every message delivered two ticks late, elect a
/// leader. Standing restarts the election timer, so the vote has a full
/// timeout of its own; without that (planted), the pre-vote and the vote
/// must share one, and no leader comes in 400 ticks.
#[test]
fn a_candidate_has_a_full_timeout_for_its_vote() {
    let first = first_leader(2, 400, None);
    assert!(first.is_some_and(|t| t <= 100), "{first:?}");
    assert_eq!(first_leader(2, 400, Some(Plant::StandWithoutRestart)), None);
}

/// A follower blocked for its size starts again with the cluster's limit,
/// and writes go on: it catches up (the leader forgets it was blocked with
/// the rest of what it held of it). Planted, the leader keeps it blocked.
fn blocked_restart(plant: Option<Plant>) -> (u64, u64) {
    let (a, b) = (0, 1);
    let mut net = Net::new(1, plant);
    net.elect_first();
    net.write();
    let k = net.find(a, b).unwrap();
    let Body::Append { prev, .. } = net.flight.remove(k).body else {
        panic!("an append");
    };
    let term = net.cores[a].term();
    net.flight.push(Message {
        from: id(2),
        to: id(1),
        term,
        start: 1,
        body: Body::AppendTooLarge {
            echo: Echo {
                term,
                start: 1,
                prev: prev.index,
            },
            index: prev.index + 1,
            size: 4096,
            limit: 1024,
        },
    });
    net.pass(b, a);
    assert_eq!(net.cores[a].blocked().len(), 1);
    net.restart(b, 1 << 10);
    for _ in 0..20 {
        net.write();
        net.tick(a);
        net.settle(a, b);
        net.settle(a, 2);
    }
    (net.last(b), net.last(a))
}

#[test]
fn a_blocked_follower_that_starts_again_catches_up() {
    let (follower, leader) = blocked_restart(None);
    assert_eq!(follower, leader);
    let (follower, leader) = blocked_restart(Some(Plant::BlockedKeptAcrossRestart));
    assert!(
        leader - follower > 15,
        "planted: at {follower}, the leader at {leader}"
    );
}

/// Within one start, a size refusal of an entry the follower acknowledged is
/// old news, even when the leader carried that entry to it: the `matched`
/// half of the rule on its own (round 5's finding 3). Planted, it blocks
/// the follower.
fn size_refusal_at_matched(plant: Option<Plant>) -> bool {
    let (a, b) = (0, 1);
    let mut net = Net::new(1, plant);
    net.elect_first();
    for _ in 0..3 {
        net.write();
        net.settle(a, b);
        net.settle(a, 2);
    }
    let p = net.cores[a].progress[&id(2)];
    assert!(p.matched >= 3 && p.carried >= 3);
    let term = net.cores[a].term();
    let refusal = Message {
        from: id(2),
        to: id(1),
        term,
        start: 1,
        body: Body::AppendTooLarge {
            echo: Echo {
                term,
                start: 1,
                prev: 2,
            },
            index: 3,
            size: 4096,
            limit: 1024,
        },
    };
    net.cores[a].receive(refusal);
    net.cores[a].blocked().is_empty()
}

#[test]
fn a_size_refusal_of_an_acknowledged_entry_is_old_news() {
    assert!(size_refusal_at_matched(None));
    assert!(!size_refusal_at_matched(Some(Plant::SizeRefusalAtMatched)));
}

/// Every check of a message's shape (`Core::well_formed`), each broken alone
/// by one message: the receiver answers nothing and changes nothing, not
/// even the start count it records of a sender it has not heard from, so
/// that without that one check the message would change something (round
/// 5's finding 2). Section 7.1 lists the relations.
#[test]
fn a_message_that_breaks_one_relation_changes_nothing() {
    use crate::entry::Kind;
    // A follower of term 1 holding two entries of term 1; a leader of term
    // 2 (its empty at 4); a pre-candidate asking for round 2.
    let follower = core_of(3, &[1, 1], 1);
    let mut leader = core_of(1, &[1, 1, 1], 1);
    elect(&mut leader, id(2));
    assert_eq!(leader.term(), 2);
    let mut precandidate = core_of(2, &[1], 1);
    for _ in 0..100 {
        if precandidate.role() == Role::PreCandidate {
            break;
        }
        precandidate.tick();
    }
    assert_eq!(precandidate.role(), Role::PreCandidate);
    let pos = |index, term| Pos { index, term };
    let wire = |index, term, len: usize| Wire {
        index,
        term,
        kind: Kind::Empty.code(),
        file: 0,
        payload: vec![0; len],
        hash: [0; 32],
    };
    let message = |from: u8, to: u8, term, body| Message {
        from: id(from),
        to: id(to),
        term,
        start: 1,
        body,
    };
    let append = |term, prev, entries| {
        let body = Body::Append {
            prev,
            entries,
            committed: 0,
        };
        message(1, 3, term, body)
    };
    let echo = |prev| Echo {
        term: 2,
        start: 1,
        prev,
    };
    let reply = |term, prev, matched, hint| {
        let body = Body::AppendReply {
            echo: echo(prev),
            matched,
            hint,
        };
        message(3, 1, term, body)
    };
    let too_large = |term, index, size| {
        let body = Body::AppendTooLarge {
            echo: echo(3),
            index,
            size,
            limit: 1024,
        };
        message(3, 1, term, body)
    };
    let prevote = |round, last| Body::PreVote { round, last };
    let (f, l, p) = (0, 1, 2);
    let cases = [
        (
            "the term past its bound",
            f,
            append(MAX_TERM + 1, pos(2, 1), vec![]),
        ),
        (
            "an index past its bound",
            f,
            append(2, pos(MAX_INDEX + 1, 1), vec![]),
        ),
        (
            "prevote_position",
            f,
            message(1, 3, 1, prevote(2, pos(0, 1))),
        ),
        ("prevote_round", f, message(1, 3, 1, prevote(5, pos(2, 1)))),
        (
            "prevote_last_term",
            f,
            message(1, 3, 1, prevote(2, pos(2, 3))),
        ),
        (
            "granted_round",
            p,
            message(
                1,
                2,
                2,
                Body::PreVoteReply {
                    round: 2,
                    granted: true,
                },
            ),
        ),
        (
            "vote_position",
            f,
            message(1, 3, 2, Body::Vote { last: pos(5, 0) }),
        ),
        (
            "vote_last_term",
            f,
            message(1, 3, 2, Body::Vote { last: pos(2, 3) }),
        ),
        (
            "append_count",
            f,
            append(
                2,
                pos(2, 1),
                vec![wire(3, 2, 0), wire(4, 2, 0), wire(5, 2, 0)],
            ),
        ),
        (
            "append_bytes",
            f,
            append(2, pos(2, 1), vec![wire(3, 2, 600), wire(4, 2, 600)]),
        ),
        (
            "append_follows",
            f,
            append(2, pos(2, 1), vec![wire(4, 2, 0)]),
        ),
        (
            "append_entry_terms",
            f,
            append(2, pos(2, 1), vec![wire(3, 3, 0)]),
        ),
        (
            "append_rising",
            f,
            append(2, pos(2, 1), vec![wire(3, 2, 0), wire(4, 1, 0)]),
        ),
        ("append_position", f, append(2, pos(2, 0), vec![])),
        ("append_prev_term", f, append(2, pos(2, 3), vec![])),
        ("reply_term", l, reply(1, 3, Some(4), 4)),
        ("success_reaches", l, reply(3, 3, Some(2), 3)),
        ("success_hint", l, reply(2, 3, Some(4), 3)),
        ("refusal_hint", l, reply(2, 3, None, 3)),
        ("too_large_term", l, too_large(1, 4, 2048)),
        ("too_large_index", l, too_large(2, 3, 2048)),
        ("too_large_size", l, too_large(2, 4, 1024)),
    ];
    let receivers = [follower, leader, precandidate];
    for (relation, to, m) in cases {
        let mut core = receivers[to].clone();
        let before = format!("{core:?}");
        let out = core.receive(m);
        assert!(out.is_empty(), "{relation}: {out:?}");
        assert_eq!(format!("{core:?}"), before, "{relation}");
    }
}

/// The terms whose leaders the simulator has seen.
fn led_terms(w: &World) -> Vec<u64> {
    w.oracle.leaders.keys().copied().collect()
}

/// Issue #1188, 1: the steady phase opens with an election run under its
/// rule, whatever the faults left (seed 1's settings, five members). The
/// review's schedule: a member still believes it leads term 1 while
/// another leads term 2; the phase stopped only the first leader it found
/// and took the other at once, with no election. And a campaign under way
/// when the phase opens, its votes asked on the faults' network, which
/// wins in the phase: it was taken as the phase's election.
#[test]
fn the_steady_phase_opens_with_an_election_under_its_rule() {
    let mut w = World::scripted_seed(1, 5, None);
    w.win(0, &[1, 2]).unwrap();
    // Nothing of member 0's reaches anyone again before the phase.
    w.take(|_, from, _| from == 0);
    w.win(2, &[3, 4]).unwrap();
    let led = |w: &World, i| (w.core_of(i).role(), w.core_of(i).term());
    assert_eq!(led(&w, 0), (Role::Leader, 1));
    assert_eq!(led(&w, 2), (Role::Leader, 2));
    w.steady().unwrap();
    let terms = led_terms(&w);
    assert!(terms.iter().any(|t| *t > 2), "leaders of terms {terms:?}");

    let mut w = World::scripted_seed(1, 5, None);
    for _ in 0..4 * w.timing.0 {
        if w.core_of(0).role() == Role::PreCandidate {
            break;
        }
        w.tick_member(0, 1).unwrap();
    }
    w.pass(|m, f, t| {
        matches!(m.body, Body::PreVote { .. } | Body::PreVoteReply { .. }) && (f == 0 || t == 0)
    })
    .unwrap();
    assert_eq!(led(&w, 0), (Role::Candidate, 1));
    let asked = w
        .flight
        .iter()
        .filter(|f| matches!(f.2.body, Body::Vote { .. }));
    assert_eq!(asked.count(), 4);
    w.steady().unwrap();
    let terms = led_terms(&w);
    assert!(terms.iter().any(|t| *t > 1), "leaders of terms {terms:?}");
}

/// Issue #1188, 2 (the review's schedule, seed 22's settings: a timeout of
/// 10, so a delay of 4 in the opening election): a vote asked just before
/// the steady phase opens, delayed as the faults' network may delay it,
/// comes back granted after its candidate's timer ran out in the phase.
/// That breaks no rule: the opening election's timing check judges only
/// the vote requests sent under its delay, and every check holds.
#[test]
fn a_vote_asked_before_the_opening_election_is_not_held_to_its_timing() {
    let mut w = World::scripted_seed(22, 3, None);
    let election = w.timing.0;
    assert_eq!((election, w.electing_delay()), (10, 4));
    let ballot = |m: &Message| {
        matches!(
            m.body,
            Body::PreVote { .. }
                | Body::Vote { .. }
                | Body::PreVoteReply { .. }
                | Body::VoteReply { .. }
        )
    };
    // Member 0's pre-vote is granted by member 1, and it stands.
    for _ in 0..4 * election {
        if w.core_of(0).role() == Role::PreCandidate {
            break;
        }
        w.tick_member(0, 1).unwrap();
    }
    w.pass(|m, f, t| {
        matches!(m.body, Body::PreVote { .. } | Body::PreVoteReply { .. })
            && ((f == 0 && t == 1) || (f == 1 && t == 0))
    })
    .unwrap();
    assert_eq!(w.core_of(0).role(), Role::Candidate);
    // Only its vote request to member 1 is left on the network.
    w.take(|m, f, t| ballot(m) && !(f == 0 && t == 1));
    assert_eq!(w.flight.iter().filter(|f| ballot(&f.2)).count(), 1);
    // Its timer runs out after the shortest timeout, the others' later.
    let set = |w: &mut World, i: usize, timeout| {
        let core = &mut w.nodes[i].member.as_mut().unwrap().core;
        core.elapsed = 0;
        core.timeout = timeout;
    };
    set(&mut w, 0, election);
    set(&mut w, 1, 2 * election - 1);
    set(&mut w, 2, 2 * election - 1);
    // The request takes 6 ticks, as the faults' network may delay it, and
    // the grant the opening election's 4: it arrives on the tick member 0's
    // timer runs out, after it.
    let due = w.now + election - w.electing_delay();
    for f in w.flight.iter_mut() {
        if matches!(f.2.body, Body::Vote { .. }) {
            f.0 = due;
        }
    }
    w.steady().unwrap();
}
