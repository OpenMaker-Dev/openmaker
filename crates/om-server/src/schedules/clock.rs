//! The time schedules follow: the server's own clock in its time zone, or
//! a clock that moves only when told to (tests, simulations).

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use jiff::Zoned;

use super::Scheduler;

/// The clock a server's schedules follow.
pub trait Clock: Send + Sync + 'static {
    /// The time now, in the time zone the schedules' dates and times are
    /// in.
    fn now(&self) -> Zoned;

    /// How long, in real time, until this clock reaches `at`; the scheduler
    /// sleeps at most that long. `None` for a clock that moves only when
    /// told to: it wakes the scheduler itself ([`Clock::watch`]).
    fn until(&self, at: &Zoned) -> Option<Duration>;

    /// Called when a scheduler starts following this clock: a clock that
    /// moves otherwise than with real time calls [`ClockWaker::wake`] each
    /// time it moves.
    fn watch(&self, waker: ClockWaker) {
        let _ = waker;
    }
}

/// Wakes a scheduler to look at its clock again.
#[derive(Clone, Debug)]
pub struct ClockWaker(pub(super) Weak<Scheduler>);

impl ClockWaker {
    /// Have the scheduler start the runs that are now due. Returns whether
    /// it still follows the clock.
    pub fn wake(&self) -> bool {
        match self.0.upgrade() {
            Some(scheduler) => {
                scheduler.ring();
                true
            }
            None => false,
        }
    }
}

/// The server's clock in its local time zone, as FileMaker Server runs its
/// schedules.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Zoned {
        Zoned::now()
    }

    fn until(&self, at: &Zoned) -> Option<Duration> {
        let left = at.timestamp().duration_since(jiff::Timestamp::now());
        Some(Duration::try_from(left).unwrap_or(Duration::ZERO))
    }
}

/// A clock that stands still until [`ManualClock::set`] or
/// [`ManualClock::advance`] moves it, waking the schedulers that follow it;
/// for tests, so that minute-level schedules run without waiting.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<Zoned>,
    wakers: Mutex<Vec<ClockWaker>>,
}

impl ManualClock {
    pub fn new(now: Zoned) -> Arc<Self> {
        Arc::new(ManualClock {
            now: Mutex::new(now),
            wakers: Mutex::new(Vec::new()),
        })
    }

    /// Move to `now` (also backwards).
    pub fn set(&self, now: Zoned) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
        self.wake();
    }

    /// Move forward by `by`.
    pub fn advance(&self, by: Duration) {
        {
            let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
            *now = now.checked_add(by).expect("a time within jiff's range");
        }
        self.wake();
    }

    /// Wake the schedulers (forgetting those that are gone), without
    /// holding the time's lock: a scheduler reads the time while it holds
    /// its own.
    fn wake(&self) {
        self.wakers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(ClockWaker::wake);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Zoned {
        self.now
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn until(&self, _at: &Zoned) -> Option<Duration> {
        None
    }

    fn watch(&self, waker: ClockWaker) {
        self.wakers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(waker);
    }
}
