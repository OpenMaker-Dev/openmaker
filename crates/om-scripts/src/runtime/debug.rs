//! The Script Debugger's control of the run loop (script-runtime.md 6.4).
//!
//! While the debugger is open ([`Runtime::start_debugging`]), the run loop
//! stops before a step and returns
//! [`RunState::Debugger`](super::RunState::Debugger). It stops before the
//! first step of a script started while no script runs, at a step with a
//! breakpoint (`kBreakpoint`, script-steps.md), when a Step Over, Step Into
//! or Step Out has finished, and after a failed step when Pause on error is
//! on, at the script's end if that step was its last. [`Runtime::debug`]
//! continues with a command, from a stop or from a Pause/Resume Script
//! pause. [`Runtime::set_next_step`] moves the current script's pointer
//! without running the steps between, and [`Runtime::debug_stack`] lists
//! the Call Stack. Halt is [`Runtime::halt`].
//!
//! A stop belongs to its frame and step: frames that come on top of it (a
//! trigger) run under the same rules, and the stop holds again when its
//! frame is back on top there. A stop whose frame is gone is forgotten.
//!
//! The debugger never stops in, or for an error of, a frame whose script
//! the host does not let it show
//! ([`ScriptHost::script_debuggable`](super::ScriptHost::script_debuggable)):
//! FileMaker runs an "Executable Only" script whole and hides its steps. A
//! button's single step is not a script and never stops either.

use super::{id, ErrorCode, FileId, FrameKind, Runtime};
use crate::flag;

/// What the Script Debugger runs next (script-runtime.md 6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugCommand {
    /// Run: to a breakpoint or the end.
    Run,
    /// Step Over: one step. A sub-script it calls runs whole unless a
    /// breakpoint in it stops it.
    StepOver,
    /// Step Into: one step, stopping at the first step of a sub-script it
    /// calls.
    StepInto,
    /// Step Out: the rest of the current script, stopping in its caller
    /// after the Perform Script step; in the outermost script, to a
    /// breakpoint or the end.
    StepOut,
}

/// Why the Script Debugger stopped before a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugStop {
    /// A script started while the debugger is open, before its first step.
    Started,
    /// The step has a breakpoint.
    Breakpoint,
    /// A Step Over, Step Into or Step Out has finished.
    Stepped,
    /// The last step failed with this error, and Pause on error is on.
    /// When it was the script's last step, the stop is at its end: the
    /// frame's step is one past its last.
    Error(ErrorCode),
}

/// A script on the stack as the debugger's Call Stack lists it, the
/// innermost last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugFrame {
    pub file: FileId,
    pub script: u64,
    pub name: String,
    /// The step the frame is at: for the innermost script the step that
    /// runs next (one past its last at its end), for a caller its Perform
    /// Script step. `None` for a script whose steps the debugger does not
    /// show.
    pub step: Option<usize>,
}

/// The debugger's state while it is open.
#[derive(Debug, Default)]
pub(super) struct Debugging {
    pause_on_error: bool,
    /// Where the scripts are stopped: why, and the frame and the step it
    /// stopped before.
    stop: Option<(DebugStop, u64, isize)>,
    /// The command running, and the depth ([`Runtime::debug_depth`]) it
    /// was given at.
    command: Option<(DebugCommand, usize)>,
    /// The error of a failed step, with Pause on error on.
    error: Option<ErrorCode>,
    /// A frame started while no script ran: it stops before its first
    /// step that runs.
    starting: Option<u64>,
    /// The last frame it stopped for so: it does not stop for it again.
    started: Option<u64>,
    /// The frame and step the scripts continued at: nothing stops there
    /// again before a step has run.
    resumed_at: Option<(u64, isize)>,
    /// Whether the step running belongs to a script the debugger shows.
    step_shown: bool,
}

impl Runtime {
    /// Open the Script Debugger: from now on the run loop stops as the
    /// module documentation says.
    pub fn start_debugging(&mut self) {
        if self.debugger.is_none() {
            self.debugger = Some(Debugging::default());
        }
    }

    /// Close the Script Debugger. A script it stopped runs on to its end
    /// at the next [`Runtime::run`], and script triggers are enabled again
    /// (script-runtime.md 6.4). A Pause/Resume Script pause stays, as
    /// without the debugger.
    pub fn stop_debugging(&mut self) {
        self.debugger = None;
        self.set_user_triggers(true);
    }

    pub fn is_debugging(&self) -> bool {
        self.debugger.is_some()
    }

    /// The debugger's Pause on error.
    pub fn set_pause_on_error(&mut self, on: bool) {
        if let Some(debugging) = &mut self.debugger {
            debugging.pause_on_error = on;
        }
    }

    /// Where the debugger stopped the scripts, while the stopped script is
    /// on top at the step it stopped before.
    pub fn debug_stop(&self) -> Option<DebugStop> {
        let (stop, frame, at) = self.debugger.as_ref()?.stop?;
        (self.debug_position() == Some((frame, at))).then_some(stop)
    }

    /// Continue with `command` scripts the debugger stopped, or a shown
    /// script a Pause/Resume Script step paused. Returns whether it did.
    pub fn debug(&mut self, command: DebugCommand) -> bool {
        let at = self.debug_position();
        let depth = self.debug_depth();
        let stopped = self.debug_stop().is_some();
        let paused = self
            .stack
            .last()
            .is_some_and(|frame| frame.paused.is_some() && frame.debuggable);
        if self.debugger.is_none() || !(stopped || paused) {
            return false;
        }
        if paused {
            if let Some(frame) = self.stack.last_mut() {
                frame.paused = None;
            }
        }
        let Some(debugging) = &mut self.debugger else {
            return false;
        };
        debugging.stop = None;
        debugging.command = Some((command, depth));
        debugging.resumed_at = at;
        true
    }

    /// Set Next Step: while the debugger has stopped the scripts, make
    /// step `step` of the current script the next to run, without running
    /// the steps between. Refused while a script transaction is open: a
    /// jump could pass its Commit Transaction or reach another one. Returns
    /// whether the step was set.
    pub fn set_next_step(&mut self, step: usize) -> bool {
        if self.debug_stop().is_none() || self.transaction.is_some() {
            return false;
        }
        let Some(frame) = self.stack.last_mut() else {
            return false;
        };
        let id = frame.id;
        let FrameKind::Script { script, index, .. } = &mut frame.kind else {
            return false;
        };
        let Ok(target) = isize::try_from(step) else {
            return false;
        };
        if !frame.debuggable || step >= script.steps.len() {
            return false;
        }
        *index = target - 1;
        if let Some(debugging) = &mut self.debugger {
            if let Some(stop) = &mut debugging.stop {
                stop.2 = target;
            }
            debugging.resumed_at = Some((id, target));
        }
        true
    }

    /// The Call Stack: every running script frame, the innermost last;
    /// scripts queued to run after them are not listed.
    pub fn debug_stack(&self) -> Vec<DebugFrame> {
        let last = self.stack.len().saturating_sub(1);
        self.stack
            .iter()
            .enumerate()
            .filter_map(|(i, frame)| {
                let FrameKind::Script { script, index, .. } = &frame.kind else {
                    return None;
                };
                if i != last && self.unstarted.contains(&frame.id) {
                    return None;
                }
                let at = if i == last {
                    index.saturating_add(1)
                } else {
                    *index
                };
                Some(DebugFrame {
                    file: frame.file,
                    script: script.id,
                    name: script.name.clone(),
                    step: frame.debuggable.then(|| usize::try_from(at).ok()).flatten(),
                })
            })
            .collect()
    }

    /// The innermost frame, if it is a script, and the step it runs next.
    fn debug_position(&self) -> Option<(u64, isize)> {
        let frame = self.stack.last()?;
        match &frame.kind {
            FrameKind::Script { index, .. } => Some((frame.id, index.saturating_add(1))),
            FrameKind::Step { .. } => None,
        }
    }

    /// The frames Step Over and Step Out count: the innermost and those
    /// that started, not the frames queued below the running ones or
    /// waiting under the innermost.
    fn debug_depth(&self) -> usize {
        let last = self.stack.len().saturating_sub(1);
        self.stack
            .iter()
            .enumerate()
            .filter(|(i, frame)| *i == last || !self.unstarted.contains(&frame.id))
            .count()
    }

    /// Before the run loop runs the next step: whether the debugger stops
    /// the scripts there, and why.
    pub(super) fn debug_check(&mut self) -> Option<DebugStop> {
        let position = self.debug_position();
        let depth = self.debug_depth();
        let debugging = self.debugger.as_ref()?;
        if let Some((stop, frame, at)) = debugging.stop {
            if position == Some((frame, at)) {
                return Some(stop);
            }
            if !self.stack.iter().any(|f| f.id == frame) {
                self.debugger.as_mut()?.stop = None;
            }
        }
        let last = self.stack.len().checked_sub(1)?;
        let top = &self.stack[last];
        let FrameKind::Script { script, index, .. } = &top.kind else {
            return None;
        };
        let (frame, next) = (top.id, index.saturating_add(1));
        let top_level = self.stack[..last]
            .iter()
            .all(|f| f.script().is_none() || self.unstarted.contains(&f.id));
        let fresh = top_level && self.unstarted.contains(&frame);
        let in_range = usize::try_from(next)
            .ok()
            .filter(|&i| i < script.steps.len());
        let step = in_range
            .and_then(|i| script.steps.get(i))
            .filter(|step| step.is_enabled() && step.id() != id::COMMENT);
        let shown = top.debuggable;
        let breakpoint = step.is_some_and(|step| step.has_flag(flag::BREAKPOINT));
        let debugging = self.debugger.as_mut()?;
        if fresh && debugging.started != Some(frame) {
            debugging.starting = Some(frame);
        }
        // A failure of the script's last step stops at its end.
        let failed_last = in_range.is_none() && debugging.error.is_some();
        if (step.is_none() && !failed_last) || !shown || debugging.resumed_at == Some((frame, next))
        {
            return None;
        }
        let stepped = match debugging.command {
            Some((DebugCommand::StepInto, _)) => true,
            Some((DebugCommand::StepOver, at)) => depth <= at,
            Some((DebugCommand::StepOut, at)) => depth < at,
            Some((DebugCommand::Run, _)) | None => false,
        };
        let stop = if debugging.starting == Some(frame) && !failed_last {
            debugging.started = Some(frame);
            DebugStop::Started
        } else if let Some(error) = debugging.error {
            DebugStop::Error(error)
        } else if stepped {
            DebugStop::Stepped
        } else if breakpoint {
            DebugStop::Breakpoint
        } else {
            return None;
        };
        debugging.stop = Some((stop, frame, next));
        debugging.command = None;
        debugging.error = None;
        debugging.starting = None;
        Some(stop)
    }

    /// The run loop is about to run a step of a script the debugger shows
    /// (`shown`) or of one it does not, or a button's single step.
    pub(super) fn debug_running(&mut self, shown: bool) {
        if let Some(debugging) = &mut self.debugger {
            debugging.step_shown = shown;
        }
    }

    /// After the run loop ran or skipped a step.
    pub(super) fn debug_stepped(&mut self) {
        if let Some(debugging) = &mut self.debugger {
            debugging.resumed_at = None;
        }
    }

    /// A step failed with `error`, stored in `Get ( LastError )` or
    /// reported: with Pause on error, the debugger stops before the next
    /// step, unless the step belongs to a script it does not show.
    pub(super) fn debug_error(&mut self, error: ErrorCode) {
        if let Some(debugging) = self
            .debugger
            .as_mut()
            .filter(|d| d.pause_on_error && d.step_shown)
        {
            debugging.error = Some(error);
        }
    }

    /// Every script ended at once (Halt Script, a cleared runtime): the
    /// debugger keeps only its options.
    pub(super) fn debug_reset(&mut self) {
        if let Some(debugging) = &mut self.debugger {
            *debugging = Debugging {
                pause_on_error: debugging.pause_on_error,
                ..Debugging::default()
            };
        }
    }
}
