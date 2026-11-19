//! The script runtime (docs/re/script-runtime.md): one per session, a stack
//! of frames, the step loop with its post-step cleanup, control flow over
//! the cached partner indexes, `Get ( LastError )` and error capture, `$`
//! and `$$` variables, script calls with parameters and results, triggers
//! and script transactions. Steps that touch windows, records, files, the
//! network or the platform go through [`ScriptHost`].
//!
//! ```text
//! host event / menu / button --perform_script, perform_step, fire_trigger--> Runtime
//! Runtime::run --step--> control flow, variables, errors (here)
//!                    \--> ScriptHost (navigation, records, fields, ...)
//! ```
//!
//! [`Runtime::run`] executes steps until nothing is left, a step waits for
//! the host ([`HostError::Pending`]), a script pauses, or an optional time
//! slice or step budget runs out. Script calls push frames on a heap stack;
//! nothing recurses on the Rust stack per script call. FileMaker has no step
//! or call-depth limit (script-runtime.md 2.6), only its stack memory guard:
//! a call fails with error 2 once the stack's estimated memory passes 1 GiB
//! (`stack`). [`Options::step_budget`] is a safety budget a host may
//! choose, unlimited by default.

mod calc;
pub mod data_files;
pub mod dynapdf;
pub mod dynapdf_text;
mod error;
mod host;
mod memory;
pub mod plugins;
mod stack;
mod steps;
mod trigger;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use om_calc::eval::{evaluate, CalcHost, Context, Evaluation, Variables};
use om_calc::{CalcError, Ctx, Value};
use om_formula::Expr;

pub use crate::catalog::Client;
pub use calc::selector;
pub use error::{confirm_alert, map_error, never_suppressed, ErrorCode};
pub use host::{
    Accounts, AlertChoice, Arg, Bar, Bounds, Clipboard, CommitOptions, CustomDialog, DialogButton,
    DialogInput, DialogResponse, Dialogs, Encoding, ErrorAlert, ErrorLogEntry, FieldRef, Fields,
    FileId, FileSource, Files, FindKind, FoundSets, HostError, HostResult, InsertFile, InsertKind,
    InsertTarget, LayoutTarget, Mode, Navigation, Network, NewWindow, OnTimer, PauseEnd, Platform,
    RecordList, RecordMove, Records, RelatedRecords, Replacement, ScriptAccess, ScriptHost,
    ServerCallbackCall, ServerScriptCall, ServerScriptResult, StepArg, StepCall,
    TransactionOptions, TransferCall, UrlRequest, UrlResponse, Visibility, WindowId, WindowOptions,
    WindowStyle, WindowTarget, Windows,
};
pub use memory::{MemoryLayout, MemoryScriptHost, MemoryWindow, SessionFunction};
pub use om_hbam::OwnedNode;
pub use stack::STACK_MEMORY_LIMIT;
pub(crate) use steps::path_list;
pub use trigger::{Fired, Suppressed, Trigger, TriggerAction, TriggerEvent, TriggerInfo};

use crate::catalog::{is_supported, OBSOLETE_STEP_IDS};
use crate::{Calculation, Script, ScriptStep};
use calc::{ScriptCalc, ScriptState};

/// Step ids the runtime uses by name.
pub(crate) mod id {
    pub const PERFORM_SCRIPT: u16 = 1;
    pub const GO_TO_RECORD: u16 = 16;
    pub const PAUSE_RESUME: u16 = 62;
    pub const IF: u16 = 68;
    pub const ELSE: u16 = 69;
    pub const END_IF: u16 = 70;
    pub const LOOP: u16 = 71;
    pub const EXIT_LOOP_IF: u16 = 72;
    pub const END_LOOP: u16 = 73;
    pub const ALLOW_USER_ABORT: u16 = 85;
    pub const SET_ERROR_CAPTURE: u16 = 86;
    pub const COMMENT: u16 = 89;
    pub const HALT_SCRIPT: u16 = 90;
    pub const GO_TO_PORTAL_ROW: u16 = 99;
    pub const EXIT_SCRIPT: u16 = 103;
    pub const ELSE_IF: u16 = 125;
    pub const SET_VARIABLE: u16 = 141;
    pub const PERFORM_SCRIPT_ON_SERVER: u16 = 164;
    pub const SET_LAYOUT_OBJECT_ANIMATION: u16 = 168;
    pub const SET_ERROR_LOGGING: u16 = 200;
    pub const OPEN_TRANSACTION: u16 = 205;
    pub const COMMIT_TRANSACTION: u16 = 206;
    pub const REVERT_TRANSACTION: u16 = 207;
    pub const SET_SESSION_IDENTIFIER: u16 = 208;
    pub const SET_REVERT_TRANSACTION_ON_ERROR: u16 = 223;
}

/// How a runtime behaves: which client it is and the host's safety limits.
#[derive(Clone, Debug)]
pub struct Options {
    /// The client the runtime stands for. Steps the client does not support
    /// (catalogs/script-step-compat.csv) fail with error 3 and the script
    /// goes on; server-type clients show no dialogs and log errors.
    pub client: Client,
    /// Halt after this many executed steps. FileMaker has no limit; `None`.
    pub step_budget: Option<u64>,
    /// Return [`RunState::Yielded`] once a call to [`Runtime::run`] has run
    /// this long (FileMaker yields to its event loop every 2000 ms).
    pub time_slice: Option<Duration>,
    /// Halt with [`Stop::Unsupported`] when the host lacks a step's
    /// capability, rather than FileMaker's error 3 and going on.
    pub stop_on_unsupported: bool,
    /// The server allows Set Error Logging (`sAllowServerScriptErrorLogging`).
    pub server_error_logging: bool,
    /// The stack memory guard's limit in estimated bytes: a script call
    /// above it fails with error 2 (script-runtime.md 2.6). FileMaker's
    /// [`STACK_MEMORY_LIMIT`] (1 GiB) unless a host lowers it.
    pub stack_memory_limit: u64,
}

impl Options {
    pub fn new(client: Client) -> Self {
        Options {
            client,
            step_budget: None,
            time_slice: None,
            stop_on_unsupported: false,
            server_error_logging: false,
            stack_memory_limit: STACK_MEMORY_LIMIT,
        }
    }

    /// Server, Data API/Custom Web and WebDirect runtimes: no dialogs,
    /// errors behave as if captured and go to the log (script-runtime.md
    /// 4.4, 4.9).
    pub fn is_server(&self) -> bool {
        matches!(
            self.client,
            Client::Server | Client::CustomWeb | Client::WebDirect
        )
    }
}

impl Default for Options {
    fn default() -> Self {
        Options::new(Client::Mac)
    }
}

/// The "current script" option of a script started while another runs
/// (script-runtime.md 3.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QueueMode {
    /// Run the new script now; the current one goes on afterwards.
    #[default]
    Continue,
    /// Halt every running script first.
    Halt,
    /// Exit the current script (returning to its caller) first.
    Exit,
    /// Resume a paused script after the new one.
    Resume,
    /// Leave the current script paused.
    Pause,
}

impl QueueMode {
    /// The stored option value (0 Continue ... 4 Pause).
    pub fn from_value(value: u16) -> Self {
        match value {
            1 => QueueMode::Halt,
            2 => QueueMode::Exit,
            3 => QueueMode::Resume,
            4 => QueueMode::Pause,
            _ => QueueMode::Continue,
        }
    }
}

/// A request to run a script.
#[derive(Clone, Debug, Default)]
pub struct ScriptCall {
    pub file: FileId,
    pub script: u64,
    /// An already-resolved owner window for an asynchronous request. It is
    /// captured before queueing, independently of the currently running frame.
    pub window: Option<WindowId>,
    /// `Get ( ScriptParameter )`.
    pub parameter: Option<Value>,
    pub mode: QueueMode,
    /// Preset `Get ( LastError )` (PSOS callbacks).
    pub preset_error: Option<i32>,
    /// Run the script after the running ones end instead of at once
    /// (`CallScript`'s queueAtBottom, script-runtime.md 3.7, 13.11).
    pub queue_at_bottom: bool,
}

impl ScriptCall {
    pub fn new(file: FileId, script: u64) -> Self {
        ScriptCall {
            file,
            script,
            ..ScriptCall::default()
        }
    }

    pub fn with_parameter(mut self, parameter: Value) -> Self {
        self.parameter = Some(parameter);
        self
    }
}

/// Why [`Runtime::run`] returned.
#[derive(Clone, Debug, PartialEq)]
pub enum RunState {
    /// No script is running or queued.
    Idle,
    /// A step returned [`HostError::Pending`]; call
    /// [`Runtime::complete_step`] and run again, or run again to re-enter
    /// the step.
    Waiting,
    /// A script is paused; [`Runtime::resume`] continues it.
    Paused { seconds: Option<f64> },
    /// The time slice ran out; run again.
    Yielded,
    /// The runtime halted every script.
    Stopped(Stop),
}

/// Why the runtime halted on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The host does not implement the step ([`Options::stop_on_unsupported`]).
    Unsupported(StepLocation),
    /// [`Options::step_budget`] ran out.
    Budget(u64),
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::Unsupported(at) => write!(
                f,
                "{}: step {} ({}) is not supported here yet; the script stopped before it changed anything.",
                at.script, at.line, at.step_name
            ),
            Stop::Budget(steps) => write!(
                f,
                "The script stopped after {steps} steps (possible infinite loop)."
            ),
        }
    }
}

/// One executed step as the script oracle traces it (`PostStepCleanup`'s
/// arguments, `tools/fms-script-harness`), for a debugger or a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepTrace {
    /// Frames on the stack when the step started, queued ones included.
    pub depth: usize,
    /// The script the step belongs to; empty for a button's single step.
    pub script: String,
    /// 1-based; 0 for a button's single step.
    pub line: usize,
    pub step: u16,
    /// What the step returned, before `MapError`.
    pub error: ErrorCode,
    /// The post-step cleanup treated it as a control step (flow steps,
    /// steps a failed transaction skips): `Get ( LastError )` untouched.
    pub control: bool,
    /// `Get ( LastError )` after the step.
    pub last_error: ErrorCode,
}

/// A step in a script: `Get ( LastErrorLocation )`'s parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepLocation {
    pub script: String,
    pub step: u16,
    pub step_name: String,
    /// 1-based.
    pub line: usize,
}

impl StepLocation {
    /// `script CR step name CR line` (script-runtime.md 4.6).
    fn text(&self) -> String {
        format!("{}\r{}\r{}", self.script, self.step_name, self.line)
    }
}

/// What a frame runs.
enum FrameKind {
    /// A script (`ScriptStackElementScript`).
    Script {
        script: Arc<Script>,
        /// The step executing; -1 before the first (script-runtime.md 1.3).
        index: isize,
        vars: Variables,
        parameter: Option<Arc<Value>>,
    },
    /// A button's single step (`ScriptStackElementStep`): no variables of
    /// its own.
    Step {
        step: Box<ScriptStep>,
        parameter: Option<Value>,
        done: bool,
    },
}

/// The trigger that started a frame.
struct TriggerFrame {
    event: TriggerEvent,
    action: Option<u64>,
    /// The step whose event the trigger holds: it runs again when the
    /// trigger lets the event happen (3000, script-runtime.md 1.7, 13.17).
    retry: Option<Box<ScriptStep>>,
}

/// A trigger action waiting for `DoTriggerAction`, with the step to run
/// again when it proceeds.
struct PendingAction {
    action: TriggerAction,
    retry: Option<Box<ScriptStep>>,
}

/// A trigger whose script is called: its frame, not pushed yet.
struct Called {
    frame: Frame,
    queued: bool,
    preset_error: Option<i32>,
}

/// A `ScriptStackElement`.
struct Frame {
    id: u64,
    kind: FrameKind,
    file: FileId,
    window: Option<WindowId>,
    /// `Get ( ScriptResult )` while this frame is current.
    result: Option<Arc<Value>>,
    /// Allow User Abort and Error Capture when the frame was created or
    /// last suspended; restored when a trigger frame above it ends.
    saved_allow_abort: bool,
    saved_capture: bool,
    trigger: Option<TriggerFrame>,
    /// The event the nearest trigger frame at or below this one handles
    /// (`Get ( TriggerKeystroke )` and friends).
    trigger_info: Option<Arc<TriggerInfo>>,
    full_access: bool,
    parent_full_access: bool,
    /// Paused by Pause/Resume Script, a pausing step or a Pause queue mode.
    paused: Option<Option<f64>>,
}

impl Frame {
    fn script(&self) -> Option<&Arc<Script>> {
        match &self.kind {
            FrameKind::Script { script, .. } => Some(script),
            FrameKind::Step { .. } => None,
        }
    }

    /// Whether the frame still has steps to run.
    fn has_steps_left(&self) -> bool {
        match &self.kind {
            FrameKind::Script { script, index, .. } => (*index + 1) < script.steps.len() as isize,
            FrameKind::Step { done, .. } => !done,
        }
    }
}

/// `ScriptTransactionMgr` (script-runtime.md 1.4, 8).
struct Transaction {
    file: FileId,
    window: Option<WindowId>,
    frame: u64,
    end_index: u16,
    error: ErrorCode,
    aborted: bool,
    skip: bool,
    options: TransactionOptions,
}

/// A step that returned [`HostError::Pending`].
struct Pending {
    step: ScriptStep,
    completion: Option<ErrorCode>,
}

/// What executing a step produced (script-runtime.md 2.3, 2.4).
#[derive(Clone, Copy, Debug, Default)]
struct Exec {
    error: ErrorCode,
    /// The handler stored and reported its error itself.
    handled: bool,
    /// A control step: never changes `Get ( LastError )`.
    control: bool,
    /// The host lacks the capability (error 3).
    unsupported: bool,
    /// The step continues asynchronously.
    pending: bool,
}

impl Exec {
    fn ok() -> Self {
        Exec::default()
    }

    fn control() -> Self {
        Exec {
            control: true,
            ..Exec::default()
        }
    }

    fn error(code: ErrorCode) -> Self {
        Exec {
            error: code,
            ..Exec::default()
        }
    }

    fn from_host(result: HostResult) -> Self {
        match result {
            Ok(()) => Exec::ok(),
            Err(HostError::Error(code)) => Exec::error(code),
            Err(HostError::Unsupported) => Exec {
                error: ErrorCode::UNAVAILABLE,
                unsupported: true,
                ..Exec::default()
            },
            Err(HostError::Pending) => Exec {
                pending: true,
                ..Exec::default()
            },
        }
    }

    /// A trigger action's outcome as its step's: an action does not wait.
    fn from_action(result: HostResult) -> Self {
        match result {
            Err(HostError::Pending) => Exec::ok(),
            other => Exec::from_host(other),
        }
    }
}

/// One session's script runtime (`ScriptRuntimeBase`).
pub struct Runtime {
    options: Options,
    /// Bottom first; queued frames are inserted at the bottom and run when
    /// the frames above them have ended (script-runtime.md 3.7).
    stack: Vec<Frame>,
    next_frame: u64,
    /// Frames that have not started yet.
    unstarted: std::collections::HashSet<u64>,
    globals: HashMap<FileId, Variables>,
    /// `$` variables outside any script (script level 0).
    level0: Variables,
    last_error: ErrorCode,
    last_error_detail: String,
    last_error_location: Option<StepLocation>,
    transaction_error_location: Option<StepLocation>,
    allow_abort: bool,
    capture: bool,
    animation: bool,
    revert_on_error: bool,
    /// `mClauseEvaluateTrue`.
    clause_taken: bool,
    /// `mLastExecutedStepID`.
    last_executed: u16,
    master_triggers: bool,
    user_triggers: bool,
    transaction: Option<Transaction>,
    /// Set Error Logging per file, with its custom debug calculation.
    error_logging: HashMap<FileId, Option<Calculation>>,
    pending: Option<Pending>,
    /// Revert Transaction's custom message, for Commit Transaction.
    transaction_detail: String,
    /// Trigger actions waiting for `DoTriggerAction`.
    trigger_actions: Vec<PendingAction>,
    /// The frame a request started on an idle runtime: its Exit Script
    /// result is the request's (`SMScriptRuntime`'s final frame, 11.3).
    final_frame: Option<u64>,
    final_result: Option<Value>,
    /// The unknown step's 4 when it aborted the top-level script (13.7).
    abort_error: Option<ErrorCode>,
    /// Executed steps, when tracing.
    trace: Option<Vec<StepTrace>>,
    /// Depth, script and line of the step executing, for its trace.
    step_context: Option<(usize, String, usize)>,
    last_message_choice: Option<i64>,
    session_identifier: Option<Value>,
    steps_run: u64,
    /// Steps of the current top-level script, for the budget.
    budget_used: u64,
    /// Calls made while the stack held enough frames for the memory guard
    /// (`mMemoryCheckCount`).
    memory_checks: u64,
    stopped: Option<Stop>,
    exprs: HashMap<Vec<u8>, Arc<Expr>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime::new(Options::default())
    }
}

/// Decoded calculations kept per runtime.
const EXPRESSION_CACHE: usize = 4096;
/// PSOS parameters and results (script-runtime.md 2.6).
const PSOS_LIMIT: usize = 1_000_000;

impl Runtime {
    pub fn new(options: Options) -> Self {
        Runtime {
            options,
            stack: Vec::new(),
            next_frame: 1,
            unstarted: Default::default(),
            globals: HashMap::new(),
            level0: Variables::new(),
            last_error: ErrorCode::NONE,
            last_error_detail: String::new(),
            last_error_location: None,
            transaction_error_location: None,
            allow_abort: true,
            capture: false,
            animation: false,
            revert_on_error: true,
            clause_taken: false,
            last_executed: 0,
            master_triggers: true,
            user_triggers: true,
            transaction: None,
            error_logging: HashMap::new(),
            pending: None,
            transaction_detail: String::new(),
            trigger_actions: Vec::new(),
            final_frame: None,
            final_result: None,
            abort_error: None,
            trace: None,
            step_context: None,
            last_message_choice: None,
            session_identifier: None,
            steps_run: 0,
            budget_used: 0,
            memory_checks: 0,
            stopped: None,
            exprs: HashMap::new(),
        }
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.options
    }

    /// Whether a script is running, paused, waiting or queued.
    pub fn is_running(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Whether closing a desktop file would interrupt an active frame or
    /// the provider owning its transaction.
    pub fn file_is_running(&self, file: FileId) -> bool {
        self.stack.iter().any(|f| f.file == file)
            || self.transaction.as_ref().is_some_and(|t| t.file == file)
    }

    /// Whether the current script is paused.
    pub fn is_paused(&self) -> bool {
        self.stack.last().is_some_and(|f| f.paused.is_some())
    }

    /// Whether a step waits for [`Self::complete_step`].
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// `Get ( LastError )`.
    pub fn last_error(&self) -> ErrorCode {
        self.last_error
    }

    /// `Get ( LastErrorLocation )`'s parts.
    pub fn last_error_location(&self) -> Option<&StepLocation> {
        self.last_error_location.as_ref()
    }

    /// `Get ( LastErrorDetail )`.
    pub fn last_error_detail(&self) -> &str {
        &self.last_error_detail
    }

    /// `Get ( ErrorCaptureState )`.
    pub fn error_capture(&self) -> bool {
        self.capture
    }

    /// `Get ( AllowAbortState )`.
    pub fn allow_abort(&self) -> bool {
        self.allow_abort
    }

    /// `Get ( RevertTransactionOnErrorState )`.
    pub fn revert_on_error(&self) -> bool {
        self.revert_on_error
    }

    /// `Get ( TransactionOpenState )`.
    pub fn transaction_open(&self) -> bool {
        self.transaction.is_some()
    }

    /// Whether triggers fire at all (the server turns them off after an
    /// unknown step aborted a script).
    pub fn triggers_enabled(&self) -> bool {
        self.master_triggers
    }

    /// The debugger's "disable script triggers": only triggers caused by a
    /// running script step fire.
    pub fn set_user_triggers(&mut self, enabled: bool) {
        self.user_triggers = enabled;
    }

    /// The Exit Script result of the script [`Self::perform_script`] last
    /// started on an idle runtime (what PSOS and a schedule return,
    /// script-runtime.md 11.3, 13.1): none when it ended without one or was
    /// halted, and never a sub-script's or a script started beside it.
    pub fn final_result(&self) -> Option<&Value> {
        self.final_result.as_ref()
    }

    /// The error a server request reports for that script
    /// (`RunScriptAndClose`, PSOS): `Get ( LastError )` at its end, or 4
    /// when an unknown step aborted it (`Get ( LastError )` then reads 1;
    /// script-runtime.md 13.7).
    pub fn final_error(&self) -> ErrorCode {
        self.abort_error.unwrap_or(self.last_error)
    }

    /// Record every executed step from now on ([`Self::take_trace`]).
    pub fn set_tracing(&mut self, on: bool) {
        self.trace = on.then(Vec::new);
    }

    /// The steps executed since the last call, when tracing.
    pub fn take_trace(&mut self) -> Vec<StepTrace> {
        self.trace.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// `Get ( SessionIdentifier )`.
    pub fn session_identifier(&self) -> Option<&Value> {
        self.session_identifier.as_ref()
    }

    /// Steps executed since the runtime was created.
    pub fn steps_run(&self) -> u64 {
        self.steps_run
    }

    /// The names of the scripts on the stack, bottom first.
    pub fn script_stack(&self) -> Vec<&str> {
        self.stack
            .iter()
            .filter_map(|f| f.script().map(|s| s.name.as_str()))
            .collect()
    }

    /// A file's `$$` variables.
    pub fn globals(&mut self, file: FileId) -> &mut Variables {
        self.globals.entry(file).or_default()
    }

    /// Drop a file's `$$` variables (the file closed).
    pub fn close_file(&mut self, file: FileId) {
        self.globals.remove(&file);
        self.error_logging.remove(&file);
    }

    /// A calculation host with the runtime's variables and script state,
    /// for evaluations outside a step (layout calculations, tooltips, a
    /// button's parameter). It never acts on files: the plug-in subset's
    /// file commands evaluate there as without the plug-in.
    pub fn calc_host<'a>(&'a mut self, host: &'a mut dyn ScriptHost) -> impl CalcHost + 'a {
        let mut calc = self.calc(host);
        calc.effects = false;
        calc
    }

    /// Evaluate a stored calculation as a step would.
    pub fn evaluate(&mut self, host: &mut dyn ScriptHost, calc: &Calculation) -> Evaluation {
        self.eval(host, calc)
    }

    // ----- starting scripts -------------------------------------------------

    /// Run a script (Scripts menu, a button's Perform Script, a PSOS request
    /// or Data API call on the server). The script starts on the next
    /// [`Self::run`].
    pub fn perform_script(
        &mut self,
        host: &mut dyn ScriptHost,
        call: ScriptCall,
    ) -> Result<(), ErrorCode> {
        let idle = self.stack.is_empty();
        if idle {
            // A new request: its result and error start over; scripts that
            // run after it ends leave them (script-runtime.md 13.14).
            self.final_frame = None;
            self.final_result = None;
            self.abort_error = None;
        }
        self.apply_queue_mode(host, call.mode);
        let preset = call.preset_error;
        let mut frame = self.script_frame(host, call.file, call.script, call.parameter, false)?;
        if call.window.is_some() {
            frame.window = call.window;
        }
        let id = frame.id;
        self.push(frame, !call.queue_at_bottom);
        if idle {
            self.final_frame = Some(id);
        }
        if let Some(code) = preset {
            self.set_last_error(ErrorCode(code), true);
        }
        Ok(())
    }

    /// Run a button's single step (a layout object's script step): a frame
    /// that executes this one step. A Perform Script step takes `parameter`
    /// when it stores no parameter calculation (the parameter of a layout
    /// button lives in the object, not the step).
    pub fn perform_step(
        &mut self,
        host: &mut dyn ScriptHost,
        step: ScriptStep,
        parameter: Option<Value>,
    ) -> Result<(), ErrorCode> {
        let file = host.current_file();
        let window = host.current_window();
        let frame = self.new_frame(
            FrameKind::Step {
                step: Box::new(step),
                parameter,
                done: false,
            },
            file,
            window,
        );
        self.push(frame, true);
        Ok(())
    }

    /// An event with a trigger script happened (script-runtime.md 7.3).
    /// Pre-event triggers run on top of any running script; delayed ones
    /// queue behind it. The event's action, if any, reaches
    /// [`ScriptHost::trigger_action`] once the script ends (at once when
    /// the trigger does not fire). The script runs on the next
    /// [`Self::run`].
    pub fn fire_trigger(&mut self, host: &mut dyn ScriptHost, trigger: Trigger) -> Fired {
        self.fire(host, trigger, true)
    }

    /// `CallScriptTrigger`: the trigger's frame runs next (or queued), and
    /// `Get ( LastError )` starts over when `reset_error`.
    fn fire(&mut self, host: &mut dyn ScriptHost, trigger: Trigger, reset_error: bool) -> Fired {
        match self.call_trigger(host, trigger) {
            Ok(called) => self.push_trigger(called, reset_error),
            Err((reason, _)) => Fired::Suppressed(reason),
        }
    }

    /// `CallScriptTrigger`'s checks and the trigger's frame. A trigger that
    /// does not fire has its action performed at once; its outcome is
    /// returned with the reason.
    fn call_trigger(
        &mut self,
        host: &mut dyn ScriptHost,
        trigger: Trigger,
    ) -> Result<Called, (Suppressed, HostResult)> {
        let suppressed = if trigger.modes & trigger.mode.trigger_bit() == 0 {
            Some(Suppressed::Mode)
        } else if !self.master_triggers {
            Some(Suppressed::MasterSwitch)
        } else if !self.user_triggers && !trigger.from_step {
            Some(Suppressed::UserTriggersDisabled)
        } else if trigger.event.is_window_event()
            && self.is_running()
            && !(trigger.from_step
                && matches!(
                    trigger.event,
                    TriggerEvent::FirstWindowOpen | TriggerEvent::WindowOpen
                ))
        {
            Some(Suppressed::ScriptRunning)
        } else {
            None
        };
        if let Some(reason) = suppressed {
            return Err((reason, self.proceed_without_trigger(host, &trigger)));
        }
        let previous_window = host.current_window();
        let called = (|| {
            // Delayed triggers may have been collected while another file's
            // step was running. Parameters belong to the event's window and
            // globals, before resolving or opening the script's target file.
            if let Some(window) = trigger.window {
                host.use_window(window).map_err(|e| match e {
                    HostError::Error(code) => code,
                    _ => ErrorCode::WINDOW_MISSING,
                })?;
            }
            let parameter = match &trigger.parameter {
                Some(calc) => Some(self.eval_in_file(host, calc, Some(trigger.file)).value),
                None => trigger.preset_parameter.clone(),
            };
            let file = match trigger.file_reference {
                Some(id) => {
                    let key = om_hbam::serial::encode(id);
                    let mut bytes = vec![key.len() as u8];
                    bytes.extend(key);
                    let reference = om_hbam::OwnedNode::leaf([32], bytes);
                    host.resolve_file(trigger.file, &reference)
                        .map_err(|e| match e {
                            HostError::Error(code) => code,
                            _ => ErrorCode::FILE_MISSING,
                        })?
                }
                None => trigger.file,
            };
            let mut frame = self.script_frame(host, file, trigger.script, parameter, false)?;
            // The event's window belongs to the originating file. An external
            // script uses its own file's window, as Perform Script does.
            if file == trigger.file && trigger.window.is_some() {
                frame.window = trigger.window;
            }
            Ok(frame)
        })();
        if let Some(window) = previous_window {
            let _ = host.use_window(window);
        }
        let mut frame = match called {
            Ok(frame) => frame,
            Err(code) => {
                self.set_last_error(code, false);
                self.set_last_error(ErrorCode::NONE, false);
                let outcome = self.proceed_without_trigger(host, &trigger);
                return Err((Suppressed::CallFailed(code.0), outcome));
            }
        };
        frame.result = trigger.preset_result.map(Arc::new);
        frame.trigger_info = Some(Arc::new(trigger.info));
        frame.trigger = Some(TriggerFrame {
            event: trigger.event,
            action: trigger.action,
            retry: None,
        });
        // A trigger a step fired runs right after that step, on top of its
        // script, delayed or not (script-runtime.md 13.12); a delayed
        // trigger of the user's own event waits for the running scripts.
        let immediate = trigger.from_step || !trigger.event.is_delayed() || self.stack.is_empty();
        Ok(Called {
            frame,
            queued: !immediate || self.pending.is_some(),
            preset_error: trigger.preset_error,
        })
    }

    fn push_trigger(&mut self, called: Called, reset_error: bool) -> Fired {
        self.push(called.frame, !called.queued);
        if reset_error {
            self.set_last_error(ErrorCode::NONE, false);
        }
        if let Some(code) = called.preset_error {
            self.set_last_error(ErrorCode(code), true);
        }
        if called.queued {
            Fired::Queued
        } else {
            Fired::Started
        }
    }

    fn proceed_without_trigger(
        &mut self,
        host: &mut dyn ScriptHost,
        trigger: &Trigger,
    ) -> HostResult {
        match trigger.action {
            Some(action) => host.trigger_action(&TriggerAction {
                event: trigger.event,
                action,
                proceed: true,
            }),
            None => Ok(()),
        }
    }

    /// Finish a step that returned [`HostError::Pending`] with its outcome
    /// (0 for success).
    pub fn complete_step(&mut self, error: ErrorCode) {
        if let Some(pending) = &mut self.pending {
            pending.completion = Some(error);
        }
    }

    /// Resume a paused script (the user continued, or the pause elapsed).
    pub fn resume(&mut self) {
        if let Some(frame) = self.stack.iter_mut().rev().find(|f| f.paused.is_some()) {
            frame.paused = None;
        }
    }

    /// Halt every script, as Halt Script does.
    pub fn halt(&mut self, host: &mut dyn ScriptHost) {
        self.halt_script(host);
    }

    /// Drop every script without calling the host (its window or
    /// connection is gone): no transaction is reverted and no trigger
    /// action is performed. The runtime states return to their defaults.
    pub fn clear(&mut self) {
        self.stack.clear();
        self.unstarted.clear();
        self.pending = None;
        self.transaction = None;
        self.trigger_actions.clear();
        self.allow_abort = true;
        self.capture = false;
        self.animation = false;
        self.revert_on_error = true;
        self.clause_taken = false;
    }

    /// The user pressed Esc / Cmd-period: halts the scripts when Allow User
    /// Abort is On. Returns whether it halted.
    pub fn user_abort(&mut self, host: &mut dyn ScriptHost) -> bool {
        if !self.is_running() || !self.allow_abort {
            return false;
        }
        self.set_last_error(ErrorCode::CANCELED, false);
        self.halt_script(host);
        true
    }

    // ----- the run loop -------------------------------------------------------

    /// Execute steps (`DoRunLoop`) until nothing is left, a step waits, a
    /// script pauses, the time slice ends or the runtime stops.
    pub fn run(&mut self, host: &mut dyn ScriptHost) -> RunState {
        self.run_loop(host, None)
    }

    /// Execute at most one step (the debugger's Step), then return
    /// [`RunState::Yielded`] while scripts remain.
    pub fn step(&mut self, host: &mut dyn ScriptHost) -> RunState {
        self.run_loop(host, Some(1))
    }

    fn run_loop(&mut self, host: &mut dyn ScriptHost, limit: Option<u64>) -> RunState {
        let start = Instant::now();
        let first = self.steps_run;
        loop {
            if let Some(stop) = self.stopped.take() {
                return RunState::Stopped(stop);
            }
            if let Some(pending) = self.pending.take() {
                match pending.completion {
                    Some(error) => {
                        let exec = match pending.step.id() {
                            id::OPEN_TRANSACTION => {
                                self.start_transaction(host, &pending.step, error);
                                Exec::error(error)
                            }
                            id::COMMIT_TRANSACTION => self.finish_commit(&pending.step, error),
                            _ => Exec {
                                error,
                                ..Exec::default()
                            },
                        };
                        self.finish_step(host, &pending.step, exec);
                    }
                    None => {
                        let exec = self.execute(host, &pending.step);
                        if exec.pending {
                            self.pending = Some(Pending {
                                step: pending.step,
                                completion: None,
                            });
                            return RunState::Waiting;
                        }
                        self.finish_step(host, &pending.step, exec);
                    }
                }
                self.after_step(host);
                continue;
            }
            let Some(top) = self.stack.last() else {
                self.do_trigger_actions(host);
                return RunState::Idle;
            };
            if let Some(seconds) = top.paused {
                return RunState::Paused { seconds };
            }
            if let Some(budget) = self.options.step_budget {
                if self.budget_used >= budget {
                    self.halt_script(host);
                    return RunState::Stopped(Stop::Budget(budget));
                }
            }
            if self
                .options
                .time_slice
                .is_some_and(|slice| start.elapsed() >= slice)
            {
                return RunState::Yielded;
            }
            if limit.is_some_and(|n| self.steps_run - first >= n) {
                return RunState::Yielded;
            }
            if host.abort_requested() && (self.allow_abort || self.options.is_server()) {
                self.set_last_error(ErrorCode::CANCELED, false);
                self.halt_script(host);
                continue;
            }
            host.before_step();
            if self.next_step(host) {
                return RunState::Waiting;
            }
            self.after_step(host);
        }
    }

    /// After every step: trigger actions, then triggers the host saw.
    fn after_step(&mut self, host: &mut dyn ScriptHost) {
        self.do_trigger_actions(host);
        for trigger in host.take_triggers() {
            self.fire(host, trigger, false);
        }
    }

    /// `DoTriggerAction`: perform or drop the actions of finished triggers.
    /// A step whose event a trigger held runs again when the trigger lets
    /// the event happen: the action's outcome is the step's (script-runtime.md
    /// 1.7, 13.17).
    fn do_trigger_actions(&mut self, host: &mut dyn ScriptHost) {
        for pending in std::mem::take(&mut self.trigger_actions) {
            let outcome = host.trigger_action(&pending.action);
            let Some(step) = pending.retry.filter(|_| pending.action.proceed) else {
                continue;
            };
            if self.stack.is_empty() {
                continue;
            }
            if self.trace.is_some() {
                self.step_context = Some(self.trace_context());
            }
            self.finish_step(host, &step, Exec::from_action(outcome));
        }
    }

    /// After a step: the triggers it caused are called, as FileMaker calls
    /// them inside the step (each call starts `Get ( LastError )` over, so
    /// the step's own error is stored after them), then `PostStepCleanup`,
    /// then their frames run. A trigger that holds the step's event (a
    /// pre-event trigger with an action) makes the step return 3000, not
    /// stored, and the step runs again when the trigger lets the event
    /// happen (script-runtime.md 1.7, 13.17).
    fn finish_step(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep, mut exec: Exec) {
        let mut called = Vec::new();
        let mut others = Vec::new();
        for trigger in host.take_triggers() {
            if !trigger.from_step {
                others.push(trigger);
                continue;
            }
            let holds_event = trigger.action.is_some() && trigger.event.can_cancel();
            match self.call_trigger(host, trigger) {
                Ok(mut c) => {
                    let first = exec.error != ErrorCode::RETRY;
                    if let (true, true, Some(t)) = (holds_event, first, c.frame.trigger.as_mut()) {
                        t.retry = Some(Box::new(step.clone()));
                        exec = Exec::error(ErrorCode::RETRY);
                    }
                    called.push(c);
                }
                // The trigger did not fire: its event happened at once.
                Err((_, outcome)) if holds_event && !exec.error.is_error() => {
                    exec = Exec::from_action(outcome);
                }
                Err(_) => {}
            }
        }
        if !called.is_empty() {
            self.set_last_error(ErrorCode::NONE, false);
        }
        self.post_step(host, step, exec);
        for c in called {
            self.push_trigger(c, false);
        }
        for trigger in others {
            self.fire(host, trigger, false);
        }
    }

    /// Fetch and execute the next step of the current frame (`DoNextStep`).
    /// Returns whether the step is pending.
    fn next_step(&mut self, host: &mut dyn ScriptHost) -> bool {
        let Some(frame) = self.stack.last_mut() else {
            return false;
        };
        if self.unstarted.remove(&frame.id) {
            if let Some(window) = frame.window {
                if let Err(failure) = host.use_window(window) {
                    // A queued owner's window can close before its first
                    // step. Never execute that frame in a different window.
                    let code = match failure {
                        HostError::Error(code) => code,
                        _ => ErrorCode::WINDOW_MISSING,
                    };
                    self.set_last_error(code, true);
                    self.exit_frame(host, false);
                    return false;
                }
            }
        }
        let (step, full_access) = match &mut frame.kind {
            FrameKind::Script { script, index, .. } => {
                *index += 1;
                let Some(step) = usize::try_from(*index)
                    .ok()
                    .and_then(|i| script.steps.get(i))
                else {
                    self.exit_frame(host, true);
                    return false;
                };
                let executable = frame.full_access
                    || frame.parent_full_access
                    || host.script_access(frame.file, script.id).executable;
                if !step.is_enabled() || step.id() == id::COMMENT || !executable {
                    return false;
                }
                (step.clone(), frame.full_access)
            }
            FrameKind::Step { step, done, .. } => {
                if *done {
                    self.exit_frame(host, true);
                    return false;
                }
                *done = true;
                ((**step).clone(), false)
            }
        };
        if host.window_mode() == Mode::Layout && host.exit_layout_mode().is_err() {
            self.exit_frame(host, true);
            return false;
        }
        let file = self.current_file();
        if full_access {
            host.set_full_access(file, true);
        }
        if self.trace.is_some() {
            self.step_context = Some(self.trace_context());
        }
        self.steps_run += 1;
        self.budget_used += 1;
        if matches!(step.id(), 34 | 121) {
            let mut files: Vec<_> = self.stack.iter().map(|f| f.file).collect();
            if let Some(transaction) = &self.transaction {
                files.push(transaction.file);
            }
            host.active_script_files(&files);
        }
        let executing = self.stack.last().map(|f| f.id);
        let exec = self.execute(host, &step);
        // A navigation step may change the caller's window. Keep that
        // window for restoration after an external script, not the one
        // captured when its frame first started.
        if let Some(frame) = self.stack.iter_mut().find(|f| Some(f.id) == executing) {
            frame.window = host.current_window();
        }
        if full_access {
            host.set_full_access(file, false);
        }
        if exec.pending {
            self.pending = Some(Pending {
                step,
                completion: None,
            });
            return true;
        }
        self.finish_step(host, &step, exec);
        false
    }

    /// Depth, script and line of the step the current frame executes.
    fn trace_context(&self) -> (usize, String, usize) {
        let (script, line) = match self.stack.last().map(|f| &f.kind) {
            Some(FrameKind::Script { script, index, .. }) => {
                (script.name.clone(), (*index).max(0) as usize + 1)
            }
            _ => (String::new(), 0),
        };
        (self.stack.len(), script, line)
    }

    /// `PostStepCleanup` (script-runtime.md 2.4), and the step's trace.
    fn post_step(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep, exec: Exec) {
        self.post_step_cleanup(host, step, exec);
        let context = self.step_context.take();
        if let (Some(trace), Some((depth, script, line))) = (&mut self.trace, context) {
            trace.push(StepTrace {
                depth,
                script,
                line,
                step: step.id(),
                error: exec.error,
                control: exec.control,
                last_error: self.last_error,
            });
        }
    }

    fn post_step_cleanup(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep, exec: Exec) {
        self.last_executed = step.id();
        if exec.unsupported && self.options.stop_on_unsupported {
            let location = self.location(step);
            self.set_last_error(ErrorCode::UNAVAILABLE, false);
            self.halt_script(host);
            self.stopped = Some(Stop::Unsupported(location));
            return;
        }
        if !exec.control && !exec.handled && self.set_last_error(exec.error, false) {
            // The step's name at the frame's index after the step: past a
            // loop that Exit after last left (13.16).
            self.last_error_location = Some(self.location(step));
            self.log_step_error(host, step);
        }
        // A control step's error is neither stored nor reported: a failed
        // Commit Transaction is logged only with a custom code (13.2).
        if !exec.error.is_error()
            || self.stack.is_empty()
            || exec.control
            || exec.error == ErrorCode::RETRY
        {
            return;
        }
        self.record_transaction_error(step, exec.error);
        if exec.error.is_internal() {
            let with_dialog = !step.has_flag(crate::flag::NO_INTERACT);
            if never_suppressed(exec.error, step.id(), with_dialog) {
                self.check_error(host, step, exec.error);
            }
            return;
        }
        let judged = if exec.handled || self.capture {
            self.last_error
        } else {
            map_error(exec.error)
        };
        if !judged.is_error() {
            return;
        }
        if judged == ErrorCode::CANCELED && !self.stack.iter().any(Frame::has_steps_left) {
            return;
        }
        self.check_error(host, step, judged);
    }

    /// `CheckError`: the client's alert, or the server's log (script-runtime.md
    /// 4.4, 4.9).
    fn check_error(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep, code: ErrorCode) {
        let location = self.location(step);
        let unknown_step = code == ErrorCode::UNKNOWN_COMMAND && self.allow_abort;
        if self.options.is_server() {
            if !code.is_internal() {
                host.log_error(&ErrorLogEntry {
                    code,
                    file: self.current_file(),
                    script: location.script.clone(),
                    step_name: location.step_name.clone(),
                    line: location.line,
                    custom: None,
                    script_error_log: false,
                });
            }
            if unknown_step {
                // The script is aborted: the request reports the 4, `Get (
                // LastError )` reads 1 (13.7).
                self.master_triggers = false;
                self.halt_script(host);
                self.abort_error = Some(code);
                self.set_last_error(ErrorCode::CANCELED, true);
            }
            return;
        }
        let with_dialog = !step.has_flag(crate::flag::NO_INTERACT);
        if self.capture && !never_suppressed(code, step.id(), with_dialog) {
            return;
        }
        let choice = host.error_alert(&ErrorAlert {
            code,
            script: location.script,
            step: step.id(),
            step_name: location.step_name,
            line: location.line,
            confirm: confirm_alert(code),
            can_cancel: self.allow_abort,
        });
        if unknown_step || (choice == AlertChoice::Cancel && self.allow_abort) {
            self.halt_script(host);
        }
    }

    /// `SetLastError` (script-runtime.md 4.2). Returns whether it stored a
    /// nonzero code.
    fn set_last_error(&mut self, code: ErrorCode, force: bool) -> bool {
        if self.stack.is_empty() && !force {
            return false;
        }
        if code.is_internal() {
            return false;
        }
        let code = map_error(code);
        if self.last_error.is_error() && code == ErrorCode::CANCELED && !force {
            return false;
        }
        self.last_error = code;
        self.last_error_detail.clear();
        self.last_error_location = if code.is_error() {
            self.current_location()
        } else {
            None
        };
        code.is_error()
    }

    /// `LogErrorState`: Set Error Logging's entry for a stored error.
    fn log_step_error(&mut self, host: &mut dyn ScriptHost, step: &ScriptStep) {
        let file = self.current_file();
        let Some(custom) = self.error_logging.get(&file).cloned() else {
            return;
        };
        let custom = custom.map(|calc| self.eval(host, &calc).value);
        let location = self.location(step);
        host.log_error(&ErrorLogEntry {
            code: self.last_error,
            file,
            script: location.script,
            step_name: location.step_name,
            line: location.line,
            custom,
            script_error_log: true,
        });
    }

    /// The location of `step` in the current frame.
    /// Whether a script runs the current step, rather than a command from
    /// the window: the frame executing it (the top one) is a script's. A
    /// command over a paused script is the window's; a trigger's script
    /// over a command is a script.
    fn in_script(&self) -> bool {
        matches!(
            self.stack.last().map(|f| &f.kind),
            Some(FrameKind::Script { .. })
        )
    }

    /// Where `step` runs: the script and line of the frame executing it; no
    /// script for a command from the window, as [`Self::in_script`] tells
    /// them apart.
    fn location(&self, step: &ScriptStep) -> StepLocation {
        let (script, line) = match self.stack.last().map(|f| &f.kind) {
            Some(FrameKind::Script { script, index, .. }) => {
                (script.name.clone(), (*index).max(0) as usize + 1)
            }
            _ => Default::default(),
        };
        StepLocation {
            script,
            step: step.id(),
            step_name: step.name(),
            line,
        }
    }

    /// The location of the step the current frame executes.
    fn current_location(&self) -> Option<StepLocation> {
        let frame = self.stack.last()?;
        let step = match &frame.kind {
            FrameKind::Script { script, index, .. } => {
                script.steps.get(usize::try_from(*index).ok()?)?.clone()
            }
            FrameKind::Step { step, .. } => (**step).clone(),
        };
        Some(self.location(&step))
    }

    // ----- frames -------------------------------------------------------------

    fn new_frame(&mut self, kind: FrameKind, file: FileId, window: Option<WindowId>) -> Frame {
        let id = self.next_frame;
        self.next_frame += 1;
        Frame {
            id,
            kind,
            file,
            window,
            result: None,
            saved_allow_abort: self.allow_abort,
            saved_capture: self.capture,
            trigger: None,
            trigger_info: None,
            full_access: false,
            parent_full_access: false,
            paused: None,
        }
    }

    /// `CallScript`'s checks and the new frame (script-runtime.md 3.7).
    fn script_frame(
        &mut self,
        host: &mut dyn ScriptHost,
        file: FileId,
        script: u64,
        parameter: Option<Value>,
        parent_full_access: bool,
    ) -> Result<Frame, ErrorCode> {
        let script_def = host
            .script(file, script)
            .filter(|s| s.kind == crate::ScriptKind::Script)
            .ok_or(ErrorCode::SCRIPT_MISSING)?;
        let access = host.script_access(file, script);
        if !access.executable && !access.full_access && !parent_full_access {
            return Err(ErrorCode::INSUFFICIENT_PRIVILEGES);
        }
        let window = match host.window_for_file(file) {
            Ok(window) => window,
            Err(_) => return Err(ErrorCode::INVALID),
        };
        self.check_stack_memory()?;
        // Parameters keep their data type (script-runtime.md 13.5).
        let parameter = parameter.map(Arc::new);
        let mut frame = self.new_frame(
            FrameKind::Script {
                script: script_def,
                index: -1,
                vars: Variables::new(),
                parameter,
            },
            file,
            window,
        );
        frame.full_access = access.full_access;
        frame.parent_full_access = parent_full_access;
        Ok(frame)
    }

    /// `OutOfStackErrorCheck` (script-runtime.md 2.6, 13.14): once the stack
    /// holds 100 frames, the first call and every 100th after it estimate
    /// the stack's memory; above the limit the call fails with error 2.
    fn check_stack_memory(&mut self) -> Result<(), ErrorCode> {
        if self.stack.len() < stack::CHECK_DEPTH {
            return Ok(());
        }
        let check = self.memory_checks % stack::CHECK_INTERVAL == 0;
        self.memory_checks += 1;
        if check && stack::estimate(&self.stack) > self.options.stack_memory_limit {
            return Err(ErrorCode::MEMORY);
        }
        Ok(())
    }

    /// Push a frame on top (`immediate`) or queue it below the running
    /// frames (`QueueScript`).
    fn push(&mut self, mut frame: Frame, immediate: bool) {
        if self.stack.is_empty() {
            self.reset_for_new_script();
        }
        self.unstarted.insert(frame.id);
        if immediate || self.stack.is_empty() {
            if let Some(top) = self.stack.last_mut() {
                top.saved_allow_abort = self.allow_abort;
                top.saved_capture = self.capture;
                if frame.trigger_info.is_none() {
                    frame.trigger_info = top.trigger_info.clone();
                }
            }
            self.stack.push(frame);
        } else {
            self.stack.insert(0, frame);
        }
    }

    /// A new top-level script: `Get ( LastError )` and its location start
    /// over (script-runtime.md 3.7).
    fn reset_for_new_script(&mut self) {
        self.last_error = ErrorCode::NONE;
        self.last_error_detail.clear();
        self.last_error_location = None;
        self.transaction_error_location = None;
        self.budget_used = 0;
    }

    /// `QueueScript`/`SetupRuntimeState` for a host-started script.
    fn apply_queue_mode(&mut self, host: &mut dyn ScriptHost, mode: QueueMode) {
        if self.stack.is_empty() {
            return;
        }
        match mode {
            QueueMode::Continue => {}
            QueueMode::Halt => self.halt_script(host),
            QueueMode::Exit => self.exit_frame(host, true),
            QueueMode::Resume => self.resume(),
            QueueMode::Pause => {
                if let Some(top) = self.stack.last_mut() {
                    top.paused.get_or_insert(None);
                }
            }
        }
    }

    /// `ExitStackFrame` (script-runtime.md 3.4): the frame ends with
    /// `result` as its Boolean return value. The bottom frame's end halts
    /// (resets the runtime states).
    fn exit_frame(&mut self, host: &mut dyn ScriptHost, result: bool) {
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        let trigger = frame
            .trigger
            .as_mut()
            .map(|t| (t.event, t.action, t.retry.take()));
        let triggered = trigger.is_some();
        if let Some((event, action, retry)) = trigger {
            // Only an event the trigger runs before can be cancelled; the
            // result of a trigger that runs after its event is ignored and
            // stores no 20 (script-runtime.md 7.1, 7.4, 13.17).
            let cancel = !result && event.can_cancel();
            if cancel {
                self.set_last_error(ErrorCode::TRIGGER_CANCELED, false);
            }
            if let Some(action) = action {
                self.trigger_actions.push(PendingAction {
                    action: TriggerAction {
                        event,
                        action,
                        proceed: !cancel,
                    },
                    retry,
                });
            }
        }
        let frame = self.stack.pop().expect("frame");
        self.unstarted.remove(&frame.id);
        if frame.full_access {
            host.set_full_access(frame.file, false);
        }
        let Some(caller) = self.stack.last() else {
            self.halt_script(host);
            return;
        };
        if triggered {
            self.allow_abort = caller.saved_allow_abort;
            self.capture = caller.saved_capture;
        }
        if let (Some(caller_window), Some(window)) = (caller.window, frame.window) {
            if caller_window != window
                && caller.paused.is_none()
                && !self.unstarted.contains(&caller.id)
            {
                let _ = host.use_window(caller_window);
            }
        }
    }

    /// `HaltScript` (script-runtime.md 3.5): reset the runtime states,
    /// revert an open transaction, clear the stack and queue.
    fn halt_script(&mut self, host: &mut dyn ScriptHost) {
        self.allow_abort = true;
        self.capture = false;
        self.animation = false;
        self.revert_on_error = true;
        self.clause_taken = false;
        if let Some(transaction) = self.transaction.take() {
            let _ = Self::revert_owned_transaction(host, &transaction);
        }
        for frame in self.stack.drain(..) {
            if frame.full_access {
                host.set_full_access(frame.file, false);
            }
            if let Some(TriggerFrame {
                event,
                action: Some(action),
                ..
            }) = frame.trigger
            {
                self.trigger_actions.push(PendingAction {
                    action: TriggerAction {
                        event,
                        action,
                        proceed: false,
                    },
                    retry: None,
                });
            }
        }
        self.unstarted.clear();
        self.pending = None;
    }

    // ----- helpers ------------------------------------------------------------

    /// A halt while a peer script is active still reverts the provider
    /// that owns the transaction. Restore the active window afterwards.
    fn revert_owned_transaction(
        host: &mut dyn ScriptHost,
        transaction: &Transaction,
    ) -> HostResult {
        let current = host.current_window();
        if let Some(window) = transaction.window {
            host.use_window(window)?;
        }
        let result = host.revert_transaction();
        if let Some(window) = current {
            let _ = host.use_window(window);
        }
        result
    }

    /// The file of the current frame (the host's current file outside a
    /// script).
    fn current_file(&self) -> FileId {
        self.stack.last().map_or(0, |f| f.file)
    }

    /// The index of the nearest script frame (a button's step frame uses
    /// its caller's variables).
    fn script_frame_index(&self) -> Option<usize> {
        self.stack
            .iter()
            .rposition(|f| matches!(f.kind, FrameKind::Script { .. }))
    }

    /// The script state `Get` reads.
    fn script_state(&self) -> ScriptState {
        let frame = self.script_frame_index().map(|i| &self.stack[i]);
        let (name, parameter) = match frame.map(|f| &f.kind) {
            Some(FrameKind::Script {
                script, parameter, ..
            }) => (Some(Arc::from(script.name.as_str())), parameter.clone()),
            _ => (None, None),
        };
        let trigger = self.stack.last().and_then(|f| f.trigger_info.clone());
        ScriptState {
            script_name: name,
            parameter,
            result: self.stack.last().and_then(|f| f.result.clone()),
            last_error: self.last_error.0,
            last_error_detail: self.last_error_detail.clone(),
            last_error_location: self
                .last_error_location
                .as_ref()
                .map(StepLocation::text)
                .unwrap_or_default(),
            capture: self.capture,
            allow_abort: self.allow_abort,
            revert_on_error: self.revert_on_error,
            animation: self.animation,
            transaction_open: self
                .transaction
                .as_ref()
                .is_some_and(|t| t.file == self.current_file()),
            last_message_choice: self.last_message_choice,
            session_identifier: self.session_identifier.clone(),
            trigger,
        }
    }

    /// The calculation host for the current frame.
    fn calc<'a>(&'a mut self, host: &'a mut dyn ScriptHost) -> ScriptCalc<'a> {
        self.calc_in_file(host, None)
    }

    fn calc_in_file<'a>(
        &'a mut self,
        host: &'a mut dyn ScriptHost,
        file: Option<FileId>,
    ) -> ScriptCalc<'a> {
        let mut state = self.script_state();
        state.transaction_open = self
            .transaction
            .as_ref()
            .is_some_and(|t| t.file == host.current_file());
        let file = file.unwrap_or_else(|| {
            self.stack
                .last()
                .map_or_else(|| host.current_file(), |f| f.file)
        });
        let index = self.script_frame_index();
        let locals = match index.map(|i| &mut self.stack[i].kind) {
            Some(FrameKind::Script { vars, .. }) => vars,
            _ => &mut self.level0,
        };
        let globals = self.globals.entry(file).or_default();
        ScriptCalc {
            host,
            locals,
            globals,
            state,
            effects: true,
        }
    }

    /// The expression of a stored calculation: its Polish stream, or its
    /// formatted stream compiled with the host's names.
    fn expression(
        &mut self,
        host: &dyn ScriptHost,
        calc: &Calculation,
    ) -> Result<Arc<Expr>, CalcError> {
        if let Some(polish) = calc.polish.as_deref() {
            if let Some(expr) = self.exprs.get(polish) {
                return Ok(Arc::clone(expr));
            }
            let expr = Arc::new(
                om_formula::polish::decode(polish).map_err(|_| CalcError::OPERAND_EXPECTED)?,
            );
            if self.exprs.len() >= EXPRESSION_CACHE {
                self.exprs.clear();
            }
            self.exprs.insert(polish.to_vec(), Arc::clone(&expr));
            return Ok(expr);
        }
        let formatted = calc
            .formatted
            .as_deref()
            .ok_or(CalcError::OPERAND_EXPECTED)?;
        let tokens =
            om_formula::formatted::decode(formatted).map_err(|_| CalcError::OPERAND_EXPECTED)?;
        let mut ctx = om_formula::Context::new(host.names());
        ctx.decimal_comma = host.locale().decimal_separator == ',';
        om_formula::compile_tokens(&tokens, &ctx)
            .map(Arc::new)
            .map_err(|e| {
                CalcError::from_code(e.code.number()).unwrap_or(CalcError::OPERAND_EXPECTED)
            })
    }

    /// Evaluate a step calculation in the script's window context.
    fn eval(&mut self, host: &mut dyn ScriptHost, calc: &Calculation) -> Evaluation {
        self.eval_in_file(host, calc, None)
    }

    fn eval_in_file(
        &mut self,
        host: &mut dyn ScriptHost,
        calc: &Calculation,
        file: Option<FileId>,
    ) -> Evaluation {
        let expr = match self.expression(host, calc) {
            Ok(expr) => expr,
            Err(error) => {
                return Evaluation {
                    value: Value::invalid(),
                    error: Some(error),
                    pending_execution: false,
                }
            }
        };
        let context: Context = host.calc_context();
        let mut calc_host = self.calc_in_file(host, file);
        evaluate(&expr, &mut calc_host, &context)
    }

    /// A step's calculation in `slot`, evaluated; `None` when not stored.
    fn eval_slot(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        slot: u16,
    ) -> Option<Evaluation> {
        let calc = step.calculation(slot)?;
        Some(self.eval(host, &calc))
    }

    /// A slot's value as text (empty when not stored).
    fn slot_text(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        slot: u16,
    ) -> Option<String> {
        let value = self.eval_slot(host, step, slot)?.value;
        Some(self.text_of(host, &value))
    }

    /// A slot's value as an integer (truncated; `?` and text without digits
    /// are `None`).
    fn slot_integer(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        slot: u16,
    ) -> Option<i64> {
        let value = self.eval_slot(host, step, slot)?.value;
        let cx = Ctx::new(host.locale(), host.today());
        value.to_number(&cx).to_i64()
    }

    /// A slot's value as a number of seconds or points.
    fn slot_float(
        &mut self,
        host: &mut dyn ScriptHost,
        step: &ScriptStep,
        slot: u16,
    ) -> Option<f64> {
        let text = self.slot_text(host, step, slot)?;
        let value = Value::text(text);
        let cx = Ctx::new(host.locale(), host.today());
        let number = value.to_number(&cx);
        if number.is_nan() || number.is_empty() {
            return None;
        }
        number.to_text_with('.').parse().ok()
    }

    fn text_of(&self, host: &dyn ScriptHost, value: &Value) -> String {
        value
            .to_text(&Ctx::new(host.locale(), host.today()))
            .into_string()
    }

    /// Perform Script on Server's parameter and result travel as text,
    /// styles kept (script-runtime.md 11.2).
    fn as_text(&self, host: &dyn ScriptHost, value: &Value) -> Value {
        Value::Text(value.to_text(&Ctx::new(host.locale(), host.today())))
    }
}

/// Whether a step id is one FileMaker refuses with error 4. The server's
/// runtimes refuse what FileMaker Server 26.0.3 does not know.
fn is_unknown_step(options: &Options, id: u16) -> bool {
    let known = if options.is_server() {
        crate::is_server_step_id(id)
    } else {
        crate::is_valid_step_id(id)
    };
    !known || OBSOLETE_STEP_IDS.contains(&id)
}

/// Whether the runtime's client has a handler for the step
/// (script-runtime.md 10.2), on the server by FileMaker Server 26.0.3's
/// table.
fn client_supports(options: &Options, id: u16) -> bool {
    if options.is_server() {
        crate::is_supported_on_server(id, options.client)
    } else {
        is_supported(id, options.client)
    }
}
