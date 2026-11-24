//! An open file and the thread its window runs on.
//!
//! The thread sleeps until input arrives or the window asks to be drawn
//! again (egui's repaint requests, also from the data worker's threads).
//! Input that arrives while a pass runs is taken together by the next one,
//! so a fast pan costs one pass per frame, not one per touch report. A
//! frame waits in a slot until the app takes it; a newer frame takes in an
//! older one the app has not taken ([`Frame::absorb`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::input::Input;
use crate::surface::Surface;
use crate::{GoError, SignIn, Source, Viewport};

/// How long a document whose app let go of it may take to close its file:
/// the close triggers and the record's commit.
const CLOSE_ON_DROP: Duration = Duration::from_secs(5);
/// How long a pass waits for the app to take the last frame before making
/// another, when only repaints or pointer moves wait: about a display
/// refresh, so that frames are not made faster than they are shown.
const PACE: Duration = Duration::from_millis(12);

/// The app's side of a document.
#[uniffi::export(with_foreign)]
pub trait DocumentObserver: Send + Sync {
    /// A frame waits in [`Document::take_frame`]. Called on the document's
    /// own thread: draw from the platform's. It is called when the slot
    /// fills, not for every frame, and the first frame can be ready before
    /// [`Document::open`] returns: the app also takes a frame whenever its
    /// canvas is drawn.
    fn frame_ready(&self);
}

#[derive(Default)]
struct Queue {
    inputs: VecDeque<Input>,
    /// When the window asked to be drawn again.
    repaint_at: Option<Instant>,
    frame: Option<Frame>,
    /// When the frame in the slot was made.
    made: Option<Instant>,
    closed: bool,
    /// The record has changes not yet saved, as of the last pass.
    unsaved: bool,
    /// The app left the foreground (or asked to close) and the save that
    /// asks for is not over: its input still waits, or the window still
    /// saves.
    saving: bool,
    /// [`Document::close`] asked to close the file.
    close: bool,
    /// The [`Document::close`] calls so far, each numbered by the count.
    close_asked: u64,
    /// The calls still waiting for their close, by number.
    close_waiting: BTreeSet<u64>,
    /// The close the window took for those calls and has not ended, by the
    /// number of the last call it took: calls taken while a close is under
    /// way (the chrome's Files, or an earlier call) join it.
    close_serving: Option<u64>,
    /// How each of those closes ended, by that number, while a call waits:
    /// its file closed, or the close was refused. A call's close is the
    /// first one taken at or after it.
    close_results: BTreeMap<u64, bool>,
    /// The app let go of the document.
    dropped: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // A panic on the document's thread leaves the queue as it was.
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn repaint(&self, delay: Duration) {
        let Some(at) = Instant::now().checked_add(delay) else {
            return;
        };
        let mut queue = self.lock();
        queue.repaint_at = Some(queue.repaint_at.map_or(at, |earlier| earlier.min(at)));
        self.wake.notify_all();
    }
}

/// An open file.
#[derive(uniffi::Object)]
pub struct Document {
    shared: Arc<Shared>,
}

#[uniffi::export]
impl Document {
    /// Open a file and start its window, blocking until the file is open or
    /// fails to open.
    #[uniffi::constructor]
    pub fn open(
        source: Source,
        sign_in: SignIn,
        viewport: Viewport,
        observer: Arc<dyn DocumentObserver>,
    ) -> Result<Arc<Self>, GoError> {
        let viewport = viewport.check()?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let (opened, open_result) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("OpenMaker document".into())
            .spawn(move || {
                let repaint_shared = Arc::downgrade(&thread_shared);
                // Any viewport's: with no native windows, the window's other
                // viewports are drawn in the root's pass.
                let surface = Surface::open(source, sign_in, viewport, move |info| {
                    if let Some(shared) = repaint_shared.upgrade() {
                        shared.repaint(info.delay);
                    }
                });
                match surface {
                    Err(error) => {
                        let _ = opened.send(Err(error));
                    }
                    Ok(surface) => {
                        if opened.send(Ok(())).is_ok() {
                            run(surface, &thread_shared, observer.as_ref());
                        }
                    }
                }
            })
            .map_err(GoError::failed)?;
        open_result
            .recv()
            .map_err(|_| GoError::failed("The document stopped while opening."))??;
        // The first frame.
        shared.repaint(Duration::ZERO);
        Ok(Arc::new(Document { shared }))
    }

    /// Queue input for the window. Never blocks; ignored once the file has
    /// closed or the document stopped.
    pub fn send(&self, input: Input) {
        let mut queue = self.shared.lock();
        // A closed or stopped document takes no more input.
        if queue.closed {
            return;
        }
        if matches!(input, Input::Active { active: false }) {
            queue.saving = true;
        }
        queue.inputs.push_back(input);
        self.shared.wake.notify_all();
    }

    /// The newest frame, with every texture change since the last one
    /// taken; `None` when there is none.
    pub fn take_frame(&self) -> Option<Frame> {
        let mut queue = self.shared.lock();
        let frame = queue.frame.take();
        if frame.is_some() {
            self.shared.wake.notify_all();
        }
        frame
    }

    /// Whether the record has changes not yet saved (or whose save is on its
    /// way), as of the window's last pass: an app leaving the foreground
    /// keeps running until they are saved.
    pub fn has_unsaved_changes(&self) -> bool {
        self.shared.lock().unsaved
    }

    /// Whether the save that leaving the foreground ([`Input::Active`]
    /// false) or [`Document::close`] asks for is still under way: from the
    /// moment the input is sent, before the window has taken it, until the
    /// save ends, saved or kept by the window (a trigger refused it, a
    /// dialog asks, or the connection is lost). An app keeping itself
    /// running in the background waits for this, then
    /// [`Document::has_unsaved_changes`] says whether changes are left.
    pub fn is_saving(&self) -> bool {
        self.shared.lock().saving
    }

    /// Close the file in front as Close File does, waiting up to
    /// `timeout_ms` for it to close. False when it is still open: a dialog
    /// the close raised (a validation failure) waits for the user, or the
    /// close was refused (a trigger, or changes only the user can let go
    /// of).
    pub fn close(&self, timeout_ms: u32) -> bool {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let mut queue = self.shared.lock();
        if queue.closed {
            return true;
        }
        queue.close = true;
        queue.close_asked += 1;
        let asked = queue.close_asked;
        queue.close_waiting.insert(asked);
        queue.saving = true;
        self.shared.wake.notify_all();
        // An earlier call's close is not this one's (this call may close the
        // file that came to the front after it), nor a later call's.
        let answered = |queue: &Queue| {
            queue
                .close_results
                .range(asked..)
                .next()
                .map(|(_, closed)| *closed)
        };
        while !queue.closed && answered(&queue).is_none() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            queue = self
                .shared
                .wake
                .wait_timeout(queue, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        // Another file a script opened may still be open: the close asked
        // for is of the file in front.
        let closed = queue.closed || answered(&queue) == Some(true);
        queue.close_waiting.remove(&asked);
        let oldest = queue.close_waiting.first().copied();
        queue
            .close_results
            .retain(|taken, _| oldest.is_some_and(|oldest| *taken >= oldest));
        closed
    }
}

impl Drop for Document {
    /// The app let go: the file closes as Close File closes it (its
    /// triggers and the record's commit), so that nothing typed is lost,
    /// and the thread ends.
    fn drop(&mut self) {
        let mut queue = self.shared.lock();
        queue.dropped = true;
        self.shared.wake.notify_all();
    }
}

/// The document's thread, until the app lets go of the document.
fn run(mut surface: Surface, shared: &Shared, observer: &dyn DocumentObserver) {
    let mut inputs = VecDeque::new();
    loop {
        {
            let mut queue = shared.lock();
            loop {
                if queue.dropped {
                    break;
                }
                let now = Instant::now();
                let due = queue.repaint_at.is_some_and(|at| at <= now) || queue.close;
                if !queue.inputs.is_empty() || due {
                    // Wait for the app to take the last frame, unless a
                    // tap, key or command waits: those go at once.
                    let urgent = queue.inputs.iter().any(|input| {
                        !matches!(input, Input::PanMoved { .. } | Input::Resize { .. })
                    });
                    let paced = queue
                        .made
                        .map(|made| made + PACE)
                        .filter(|until| *until > now);
                    match paced {
                        Some(until) if queue.frame.is_some() && !urgent => {
                            queue = shared
                                .wake
                                .wait_timeout(queue, until - now)
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .0;
                            continue;
                        }
                        _ => break,
                    }
                }
                queue = match queue.repaint_at {
                    Some(at) => {
                        shared
                            .wake
                            .wait_timeout(queue, at - now)
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .0
                    }
                    None => shared
                        .wake
                        .wait(queue)
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                };
            }
            if queue.dropped {
                let pending = std::mem::take(&mut queue.inputs);
                drop(queue);
                if surface.is_open() {
                    surface.close(pending, CLOSE_ON_DROP);
                }
                return;
            }
            inputs.extend(queue.inputs.drain(..));
            queue.repaint_at = None;
            if std::mem::take(&mut queue.close) {
                queue.close_serving = Some(queue.close_asked);
                surface.request_close();
            }
        }
        let step = match panic::catch_unwind(AssertUnwindSafe(|| surface.step(&mut inputs))) {
            Ok(step) => step,
            Err(payload) => {
                // A bug or a hostile file stopped the window mid-pass: its
                // state can no longer be trusted, so the document ends, and
                // the app is told why rather than left with a frozen layout.
                let mut queue = shared.lock();
                queue.frame = Some(Frame::stopped(surface.viewport(), panic_message(&*payload)));
                queue.closed = true;
                queue.saving = false;
                shared.wake.notify_all();
                drop(queue);
                observer.frame_ready();
                return;
            }
        };
        let mut queue = shared.lock();
        // What the pass could not take goes first next time.
        while let Some(input) = inputs.pop_back() {
            queue.inputs.push_front(input);
        }
        if let Some(delay) = step.repaint_after {
            if let Some(at) = Instant::now().checked_add(delay) {
                queue.repaint_at = Some(queue.repaint_at.map_or(at, |earlier| earlier.min(at)));
            }
        }
        queue.closed = step.closed;
        queue.unsaved = step.unsaved;
        // A close the chrome's Files asked for alone answers no call.
        if let (Some(result), Some(taken)) = (step.close_result, queue.close_serving) {
            queue.close_serving = None;
            if !queue.close_waiting.is_empty() {
                queue.close_results.insert(taken, result);
            }
        }
        // A request the window has not taken yet is still to come; a closed
        // file has nothing left to save.
        queue.saving = !step.closed
            && (step.saving
                || queue.close
                || queue
                    .inputs
                    .iter()
                    .any(|input| matches!(input, Input::Active { active: false })));
        let mut ready = false;
        if let Some(mut frame) = step.frame {
            match queue.frame.take() {
                Some(older) => frame.absorb(older),
                None => ready = true,
            }
            queue.frame = Some(frame);
            queue.made = Some(Instant::now());
        }
        shared.wake.notify_all();
        drop(queue);
        if ready {
            observer.frame_ready();
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    format!("OpenMaker stopped showing this file because of an error: {detail}")
}
