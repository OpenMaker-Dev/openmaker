//! Every order of the steps of a few actors that share a session's grant
//! (#968), run against the real code, with three invariants checked after
//! every step ([`every_order`]).
//!
//! Each actor runs on a thread of its own, one at a time. An actor runs
//! until it is about to take a lock of one of the scenario's sections
//! ([`Section`]), or reaches a point the code marks ([`point`]); then it
//! waits while the scheduler picks the actor that goes next. A step is
//! what an actor does between two such points: taking a lock, its critical
//! section and what follows until the next. Between an actor letting a
//! lock go and taking the next, any other may go. The scheduler tries
//! every choice, depth first, running each order from a fresh start.
//!
//! An actor that finds a lock taken waits until the actor that last took
//! it has taken a step. A step that begins with the lock still taken
//! changes nothing, so that order is cut there: every order that goes on
//! from it is also an order in which another actor was picked instead. A
//! lock of a section that is not one of the scenario's points is taken
//! without a pause, which loses no order only while a single actor takes
//! it; the scenario fails when two do.
//!
//! The invariants, from the events the actors and the code record
//! ([`Event`]):
//!
//! 1. No read is admitted on what a session granted once the step that
//!    takes that authority away has begun: the facts it was admitted on
//!    were read at an earlier step than [`Event::Began`].
//! 2. Once that operation has finished ([`Event::Finished`]), no joined
//!    transport keeps a page of what it took away.
//! 3. In any interval of the model's clock, the refused looks an address
//!    is granted are at most its allowance plus what refilled in it.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::IpAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{
    Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    TryLockError, Weak,
};
use std::time::{Duration, Instant};

use super::Section;
use crate::transfer::Transfers;

/// What an actor or the code records for the invariants.
pub(crate) enum Event {
    /// An operation that takes `Removal` away begins: recorded just before
    /// it, in the step of its first section.
    Began(Removal),
    /// That operation returned.
    Finished(Removal),
    /// A joined read of `file` of `session` was admitted under Login
    /// `login`, on facts read at step `facts` ([`facts_step`]).
    Admitted {
        session: u32,
        file: u32,
        login: u64,
        facts: usize,
    },
    /// A joined read kept page `cursor` of `file` of `session` in `table`,
    /// under Login `login`.
    Kept {
        session: u32,
        file: u32,
        login: u64,
        table: Weak<Mutex<Transfers>>,
        cursor: u64,
    },
    /// A request from `address` was looked at, at second `tick` of the
    /// model's clock, and refused or not.
    Looked {
        address: IpAddr,
        tick: u64,
        refused: bool,
    },
}

/// An authority taken away.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Removal {
    /// Every session's: the server closes every connection.
    All,
    /// A session's.
    Session(u32),
    /// A session's account of a file, that of Login `login`.
    File { session: u32, file: u32, login: u64 },
}

impl Removal {
    fn covers(self, session: u32, file: u32, login: u64) -> bool {
        match self {
            Removal::All => true,
            Removal::Session(id) => id == session,
            Removal::File {
                session: id,
                file: f,
                login: l,
            } => id == session && f == file && l == login,
        }
    }
}

/// What an actor does, all of it, on its thread.
pub(crate) type Body = Box<dyn FnOnce() + Send>;

/// Something kept until every actor of an order is done.
pub(crate) type Kept = Box<dyn std::any::Any + Send>;

/// One order's actors, by name, and what is dropped once they are done.
pub(crate) struct Run {
    pub actors: Vec<(&'static str, Body)>,
    pub keep: Vec<Kept>,
}

/// What [`every_order`] ran.
#[derive(Debug)]
pub(crate) struct Explored {
    pub orders: usize,
    /// The steps of the longest order.
    pub steps: usize,
}

/// Run every order of the steps of the actors `setup` gives, a fresh set
/// for each, taking the locks of `points` as steps, and check the
/// invariants after every step (the third with an allowance of
/// `per_address` refused looks, refilled one a second, when given). Panics
/// with the first violation and the order that led to it, or past
/// `max_orders`.
pub(crate) fn every_order(
    points: &[Section],
    per_address: Option<u32>,
    max_orders: usize,
    mut setup: impl FnMut() -> Run,
) -> Explored {
    let mut prefix: Vec<usize> = Vec::new();
    let mut explored = Explored {
        orders: 0,
        steps: 0,
    };
    loop {
        let order = run(points, per_address, &prefix, setup());
        explored.orders += 1;
        explored.steps = explored.steps.max(order.steps.len());
        if let Some(violation) = order.violation {
            panic!(
                "order {}: {violation}\nthe order: {}",
                explored.orders,
                order.steps.join("; ")
            );
        }
        assert!(
            explored.orders <= max_orders,
            "more than {max_orders} orders"
        );
        let mut choices = order.choices;
        loop {
            let Some((choice, count)) = choices.pop() else {
                return explored;
            };
            if choice + 1 < count {
                prefix = choices.iter().map(|&(choice, _)| choice).collect();
                prefix.push(choice + 1);
                break;
            }
        }
    }
}

/// Take a lock of `section`, as an actor: `None` on any other thread.
pub(crate) fn lock<T>(section: Section, mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    let (world, me) = actor()?;
    let address = std::ptr::from_ref(mutex).addr();
    Some(world.take(me, section, address, || match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }))
}

pub(crate) fn read<T>(section: Section, lock: &RwLock<T>) -> Option<RwLockReadGuard<'_, T>> {
    let (world, me) = actor()?;
    let address = std::ptr::from_ref(lock).addr();
    Some(world.take(me, section, address, || match lock.try_read() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }))
}

pub(crate) fn write<T>(section: Section, lock: &RwLock<T>) -> Option<RwLockWriteGuard<'_, T>> {
    let (world, me) = actor()?;
    let address = std::ptr::from_ref(lock).addr();
    Some(world.take(me, section, address, || match lock.try_write() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }))
}

/// A step's end the code marks where it does what the model holds no lock
/// for: a session's locks released in the engine, a password checked.
pub(crate) fn point(section: Section) {
    if let Some((world, me)) = actor() {
        if world.points.contains(&section) {
            world.wait(me, Status::Ready, format!("at {section:?}"));
        }
    }
}

/// The model's clock, for an actor.
pub(crate) fn now() -> Option<Instant> {
    actor().map(|(world, _)| world.base + Duration::from_secs(world.ticks.load(Ordering::Relaxed)))
}

/// The model's clock moves on a second, the refill of the scenarios'
/// allowances.
pub(crate) fn tick() {
    if let Some((world, _)) = actor() {
        world.ticks.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record `event` at the actor's step.
pub(crate) fn record(event: Event) {
    if let Some((world, _)) = actor() {
        guard(&world.log).push(Logged {
            step: STEP.get(),
            event,
        });
    }
}

/// `Joins::admit` looked at a request from `address` at `now`.
pub(crate) fn looked(address: IpAddr, now: Instant, refused: bool) {
    if let Some((world, _)) = actor() {
        let tick = now.saturating_duration_since(world.base).as_secs();
        record(Event::Looked {
            address,
            tick,
            refused,
        });
    }
}

/// A grant's facts were read for the rule, under the lock taken last: as
/// they were at the step that took it, since no one changes them while it
/// is held.
pub(crate) fn saw_facts() {
    FACTS.set(TAKEN.get());
}

/// The step at which the actor last read a grant's facts.
pub(crate) fn facts_step() -> usize {
    FACTS.get()
}

thread_local! {
    static ACTOR: RefCell<Option<(Arc<World>, usize)>> = const { RefCell::new(None) };
    static STEP: Cell<usize> = const { Cell::new(0) };
    /// The step at which the actor took its last lock.
    static TAKEN: Cell<usize> = const { Cell::new(0) };
    static FACTS: Cell<usize> = const { Cell::new(0) };
}

fn actor() -> Option<(Arc<World>, usize)> {
    ACTOR.with(|actor| actor.borrow().clone())
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Ready,
    /// Waits for a lock, last taken by that actor (if one did).
    Blocked(Option<usize>),
    Done,
}

struct Logged {
    step: usize,
    event: Event,
}

struct Turns {
    running: Option<usize>,
    status: Vec<Status>,
    /// Where each actor waits.
    at: Vec<String>,
    /// Steps given so far.
    step: usize,
    /// Each lock taken in the order, by address, and the actor that last
    /// took it.
    holders: HashMap<usize, usize>,
    /// Each lock of a section that is not a point, and the actor that took
    /// it first.
    unshared: HashMap<usize, (usize, Section)>,
    /// The order repeats one tried elsewhere from here.
    cut: bool,
    trouble: Vec<String>,
}

struct World {
    turns: Mutex<Turns>,
    turn: Condvar,
    points: Vec<Section>,
    log: Mutex<Vec<Logged>>,
    base: Instant,
    ticks: AtomicU64,
}

impl World {
    fn turns(&self) -> MutexGuard<'_, Turns> {
        guard(&self.turns)
    }

    /// Actor `me`, now `status` at `at`, waits until it is picked.
    fn wait(&self, me: usize, status: Status, at: String) {
        let mut turns = self.turns();
        turns.status[me] = status;
        turns.at[me] = at;
        turns.running = None;
        self.turn.notify_all();
        while turns.running != Some(me) {
            turns = self
                .turn
                .wait(turns)
                .unwrap_or_else(PoisonError::into_inner);
        }
        STEP.set(turns.step);
    }

    fn first_turn(&self, me: usize) {
        let mut turns = self.turns();
        while turns.running != Some(me) {
            turns = self
                .turn
                .wait(turns)
                .unwrap_or_else(PoisonError::into_inner);
        }
        STEP.set(turns.step);
    }

    fn done(&self, me: usize, panicked: Option<String>) {
        let mut turns = self.turns();
        turns.status[me] = Status::Done;
        turns.at[me] = "done".to_owned();
        if let Some(panicked) = panicked {
            turns.trouble.push(format!("an actor panicked: {panicked}"));
        }
        turns.running = None;
        self.turn.notify_all();
    }

    /// Take the lock of `section` at `address` by `try_take`, as actor
    /// `me`. If `section` is a point, a step ends just before: the next one
    /// begins by taking it, so that between letting a lock go and taking
    /// the next any other actor may go. A step also ends while the lock is
    /// taken by another; one that begins with it still taken changes
    /// nothing, and its order is cut.
    fn take<G>(
        &self,
        me: usize,
        section: Section,
        address: usize,
        mut try_take: impl FnMut() -> Option<G>,
    ) -> G {
        let point = self.points.contains(&section);
        let mut resumed = false;
        if point {
            self.wait(me, Status::Ready, format!("to take {section:?}"));
            resumed = true;
        }
        loop {
            if let Some(guard) = try_take() {
                TAKEN.set(STEP.get());
                let mut turns = self.turns();
                turns.holders.insert(address, me);
                if !point {
                    match turns.unshared.entry(address) {
                        Entry::Vacant(entry) => {
                            entry.insert((me, section));
                        }
                        Entry::Occupied(entry) if entry.get().0 != me => {
                            turns.trouble.push(format!(
                                "two actors take one lock of {section:?}, which is not a point \
                                 of the scenario"
                            ));
                        }
                        Entry::Occupied(_) => {}
                    }
                }
                return guard;
            }
            let holder = {
                let mut turns = self.turns();
                turns.cut |= resumed;
                turns.holders.get(&address).copied()
            };
            self.wait(
                me,
                Status::Blocked(holder),
                format!("waits for {section:?}"),
            );
            resumed = true;
        }
    }

    /// The three invariants, at this step.
    fn check(&self, per_address: Option<u32>) -> Result<(), String> {
        let log = guard(&self.log);
        for began in log.iter() {
            let Event::Began(removal) = &began.event else {
                continue;
            };
            for admitted in log.iter() {
                let Event::Admitted {
                    session,
                    file,
                    login,
                    facts,
                } = admitted.event
                else {
                    continue;
                };
                if facts > began.step && removal.covers(session, file, login) {
                    return Err(format!(
                        "invariant 1: a read of file {file} of session {session:#010x} under Login \
                         {login} was admitted on facts read at step {facts}, after {removal:?} \
                         began at step {}",
                        began.step
                    ));
                }
            }
        }
        for finished in log.iter() {
            let Event::Finished(removal) = &finished.event else {
                continue;
            };
            for kept in log.iter() {
                let Event::Kept {
                    session,
                    file,
                    login,
                    ref table,
                    cursor,
                } = kept.event
                else {
                    continue;
                };
                if !removal.covers(session, file, login) {
                    continue;
                }
                let Some(table) = table.upgrade() else {
                    continue;
                };
                let holds = match table.try_lock() {
                    Ok(table) => table.holds(cursor),
                    Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().holds(cursor),
                    // Its holder is in a step; the next check sees it.
                    Err(TryLockError::WouldBlock) => false,
                };
                if holds {
                    return Err(format!(
                        "invariant 2: after {removal:?} finished at step {}, a joined transport \
                         keeps page {cursor} of file {file} under Login {login}, kept at step {}",
                        finished.step, kept.step
                    ));
                }
            }
        }
        let Some(per_address) = per_address else {
            return Ok(());
        };
        let mut refused: HashMap<IpAddr, Vec<u64>> = HashMap::new();
        for logged in log.iter() {
            if let Event::Looked {
                address,
                tick,
                refused: true,
            } = logged.event
            {
                refused.entry(address).or_default().push(tick);
            }
        }
        for (address, ticks) in &refused {
            let last = ticks.iter().copied().max().unwrap_or(0);
            for from in 0..=last {
                for to in from..=last {
                    let looks = ticks
                        .iter()
                        .filter(|tick| (from..=to).contains(tick))
                        .count();
                    let looks = u64::try_from(looks).unwrap_or(u64::MAX);
                    let allowed = u64::from(per_address) + (to - from);
                    if looks > allowed {
                        return Err(format!(
                            "invariant 3: {address} was granted {looks} refused looks in seconds \
                             {from}..={to}, where its allowance gives {allowed}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

struct Order {
    /// Each choice made and how many there were.
    choices: Vec<(usize, usize)>,
    steps: Vec<String>,
    violation: Option<String>,
}

/// Run one order: the choices of `prefix`, then the first actor ready at
/// each step.
fn run(points: &[Section], per_address: Option<u32>, prefix: &[usize], run: Run) -> Order {
    let Run { actors, keep } = run;
    let count = actors.len();
    let world = Arc::new(World {
        turns: Mutex::new(Turns {
            running: None,
            status: vec![Status::Ready; count],
            at: vec![String::new(); count],
            step: 0,
            holders: HashMap::new(),
            unshared: HashMap::new(),
            cut: false,
            trouble: Vec::new(),
        }),
        turn: Condvar::new(),
        points: points.to_vec(),
        log: Mutex::new(Vec::new()),
        base: Instant::now(),
        ticks: AtomicU64::new(0),
    });
    let names: Vec<&'static str> = actors.iter().map(|(name, _)| *name).collect();
    let threads: Vec<_> = actors
        .into_iter()
        .enumerate()
        .map(|(me, (name, body))| {
            let world = Arc::clone(&world);
            std::thread::Builder::new()
                .name(format!("model {name}"))
                .spawn(move || {
                    ACTOR.with(|actor| *actor.borrow_mut() = Some((Arc::clone(&world), me)));
                    world.first_turn(me);
                    let outcome = catch_unwind(AssertUnwindSafe(body));
                    ACTOR.with(|actor| *actor.borrow_mut() = None);
                    world.done(me, outcome.err().map(|panic| message(&*panic)));
                })
                .expect("a thread for an actor")
        })
        .collect();
    let mut order = Order {
        choices: Vec::new(),
        steps: Vec::new(),
        violation: None,
    };
    let mut last: Option<usize> = None;
    let mut turns = world.turns();
    loop {
        while turns.running.is_some() {
            turns = world
                .turn
                .wait(turns)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if let Some(actor) = last {
            let step = format!("{} {}", names[actor], turns.at[actor]);
            order.steps.push(step);
            for status in &mut turns.status {
                if matches!(*status, Status::Blocked(holder) if holder.is_none_or(|holder| holder == actor))
                {
                    *status = Status::Ready;
                }
            }
        }
        if order.violation.is_none() && !turns.cut {
            if let Some(trouble) = turns.trouble.first() {
                order.violation = Some(trouble.clone());
            } else if let Err(violation) = world.check(per_address) {
                order.violation = Some(violation);
            }
        }
        let ready: Vec<usize> = (0..count)
            .filter(|&actor| turns.status[actor] == Status::Ready)
            .collect();
        if ready.is_empty() {
            if turns.status.iter().all(|status| *status == Status::Done) {
                break;
            }
            // The actors left wait for one another: their threads are left.
            order
                .violation
                .get_or_insert_with(|| "every actor left waits for a lock another holds".into());
            return order;
        }
        let exploring = order.violation.is_none() && !turns.cut;
        let choice = match prefix.get(order.choices.len()) {
            Some(&choice) if exploring => choice,
            _ => 0,
        };
        assert!(
            choice < ready.len(),
            "the order took another course than before: the scenario is not deterministic"
        );
        if exploring {
            order.choices.push((choice, ready.len()));
        }
        let actor = ready[choice];
        turns.step += 1;
        turns.running = Some(actor);
        last = Some(actor);
        world.turn.notify_all();
    }
    drop(turns);
    for thread in threads {
        // An actor's panic was caught and is the order's violation.
        let _ = thread.join();
    }
    drop(keep);
    order
}

fn message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(not a message)".to_owned())
}
