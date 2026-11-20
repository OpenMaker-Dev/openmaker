//! The Script Debugger's control of the run loop (script-runtime.md 6.4).
//!
//! While the debugger is open ([`Runtime::start_debugging`]), the run loop
//! stops before a step and returns [`RunState::Debugger`](super::RunState::Debugger). It stops before
//! the first step of a script started while no script frame runs, at a
//! step with a breakpoint (`kBreakpoint`, script-steps.md), when a Step
//! Over, Step Into or Step Out has finished, and after a failed step when
//! Pause on error is on. [`Runtime::debug`] continues with a command.
//! [`Runtime::set_next_step`] moves the current script's pointer without
//! running the steps between, and [`Runtime::debug_stack`] lists the Call
//! Stack. Halt is [`Runtime::halt`].
//!
//! The debugger never stops in a frame whose script the host does not let
//! it show ([`ScriptHost::script_debuggable`](super::ScriptHost::script_debuggable)): FileMaker runs an
//! "Executable Only" script whole and hides its steps. A button's single
//! step is not a script and never stops either.

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
    /// runs next, for a caller its Perform Script step. `None` for a script
    /// whose steps the debugger does not show.
    pub step: Option<usize>,
}

/// The debugger's state while it is open.
#[derive(Debug, Default)]
pub(super) struct Debugging {
    pause_on_error: bool,
    /// Where the scripts are stopped, if they are.
    stop: Option<DebugStop>,
    /// The command running, and the stack depth it was given at.
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
}

impl Runtime {
    /// Open the Script Debugger: from now on the run loop stops as the
    /// module documentation says.
    pub fn start_debugging(&mut self) {
        if self.debugger.is_none() {
            self.debugger = Some(Debugging::default());
        }
    }

    /// Close the Script Debugger. A stopped script runs on to its end at
    /// the next [`Runtime::run`], and script triggers are enabled again
    /// (script-runtime.md 6.4).
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

    /// Where the debugger stopped the scripts, if it did.
    pub fn debug_stop(&self) -> Option<DebugStop> {
        self.debugger.as_ref().and_then(|d| d.stop)
    }

    /// Continue scripts the debugger stopped with `command`. Returns
    /// whether they were stopped.
    pub fn debug(&mut self, command: DebugCommand) -> bool {
        let depth = self.stack.len();
        let at = self.stack.last().and_then(|frame| match &frame.kind {
            FrameKind::Script { index, .. } => Some((frame.id, index.saturating_add(1))),
            FrameKind::Step { .. } => None,
        });
        let Some(debugging) = &mut self.debugger else {
            return false;
        };
        if debugging.stop.take().is_none() {
            return false;
        }
        debugging.command = Some((command, depth));
        debugging.resumed_at = at;
        true
    }

    /// Set Next Step: while the debugger has stopped the scripts, make
    /// step `step` of the current script the next to run, without running
    /// the steps between. Returns whether the step was set.
    pub fn set_next_step(&mut self, step: usize) -> bool {
        if self.debug_stop().is_none() {
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
            debugging.resumed_at = Some((id, target));
        }
        true
    }

    /// The Call Stack: every script frame, the innermost last.
    pub fn debug_stack(&self) -> Vec<DebugFrame> {
        let last = self.stack.len().saturating_sub(1);
        self.stack
            .iter()
            .enumerate()
            .filter_map(|(i, frame)| {
                let FrameKind::Script { script, index, .. } = &frame.kind else {
                    return None;
                };
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

    /// Before the run loop runs the next step: whether the debugger stops
    /// the scripts there, and why.
    pub(super) fn debug_check(&mut self) -> Option<DebugStop> {
        let debugging = self.debugger.as_ref()?;
        if let Some(stop) = debugging.stop {
            return Some(stop);
        }
        let depth = self.stack.len();
        let top = self.stack.last()?;
        let FrameKind::Script { script, index, .. } = &top.kind else {
            return None;
        };
        let (frame, next) = (top.id, index.saturating_add(1));
        let top_level = self.stack[..depth - 1].iter().all(|f| f.script().is_none());
        let fresh = top_level && self.unstarted.contains(&frame);
        let step = usize::try_from(next)
            .ok()
            .and_then(|i| script.steps.get(i))
            .filter(|step| step.is_enabled() && step.id() != id::COMMENT);
        let shown = top.debuggable;
        let breakpoint = step.is_some_and(|step| step.has_flag(flag::BREAKPOINT));
        let debugging = self.debugger.as_mut()?;
        if fresh && debugging.started != Some(frame) {
            debugging.starting = Some(frame);
        }
        if step.is_none() || !shown || debugging.resumed_at == Some((frame, next)) {
            return None;
        }
        let stepped = match debugging.command {
            Some((DebugCommand::StepInto, _)) => true,
            Some((DebugCommand::StepOver, at)) => depth <= at,
            Some((DebugCommand::StepOut, at)) => depth < at,
            Some((DebugCommand::Run, _)) | None => false,
        };
        let stop = if debugging.starting == Some(frame) {
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
        debugging.stop = Some(stop);
        debugging.command = None;
        debugging.error = None;
        debugging.starting = None;
        Some(stop)
    }

    /// After the run loop ran or skipped a step.
    pub(super) fn debug_stepped(&mut self) {
        if let Some(debugging) = &mut self.debugger {
            debugging.resumed_at = None;
        }
    }

    /// A step failed with `error`: with Pause on error, the debugger stops
    /// before the next step.
    pub(super) fn debug_error(&mut self, error: ErrorCode) {
        if let Some(debugging) = self.debugger.as_mut().filter(|d| d.pause_on_error) {
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
